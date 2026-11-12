#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Hardware QMP `send-key`, dynamic `screendump` pixel-delta, and IRQ/IoPort validators for P1."""

import json
import os
import pathlib
import queue
import shutil
import socket
import subprocess
import sys
import threading
import time

from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints

EXIT_CODE_SUCCESS = (0x10 << 1) | 1  # 33

CHAR_TO_QCODE: dict[str, list[str]] = {
    **{chr(c): [chr(c)] for c in range(ord("a"), ord("z") + 1)},
    **{chr(c): [chr(c)] for c in range(ord("0"), ord("9") + 1)},
    " ": ["spc"],
    "-": ["minus"],
    "=": ["equal"],
    "/": ["slash"],
    ".": ["dot"],
    ",": ["comma"],
}


def verify_p1_irq_lines(name: str, serial_out: str, smp: int) -> None:
    """Verify ACPI MADT, IOAPIC pin routing, `[DRY-IRQ] PASS`, and `[DRY-IRQ-BENCH] PASS`."""
    if "[DRY-ACPI] PASS:" not in serial_out or "ioapic_found=true" not in serial_out:
        raise RuntimeError(f"[{name}] Missing [DRY-ACPI] PASS with ioapic_found=true")
    if "[DRY-IOAPIC] PASS:" not in serial_out or "irq1_vec=0x31" not in serial_out:
        raise RuntimeError(f"[{name}] Missing [DRY-IOAPIC] PASS with irq1_vec=0x31")

    irq_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-IRQ] PASS:" in ln]
    if len(irq_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-IRQ] PASS line, got {len(irq_lines)}")
    ikv = _parse_kv_ints(irq_lines[0])
    for req_tok in (
        "ioapic=true",
        "ioport_guards=ok",
        "double_ack_rejected=true",
        "revoke_race_ok=true",
    ):
        if req_tok not in irq_lines[0]:
            raise RuntimeError(f"[{name}] Missing {req_tok} in {irq_lines[0]!r}")
    if (
        ikv.get("cpus") != smp
        or ikv.get("storm_clamped", 0) < 1
        or ikv.get("lost_wakeups", 1) != 0
        or ikv.get("leaked_frames", 1) != 0
    ):
        raise RuntimeError(f"[{name}] [DRY-IRQ] invariant mismatch: {irq_lines[0]!r}")

    bench_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-IRQ-BENCH] PASS:" in ln]
    if len(bench_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-IRQ-BENCH] PASS line, got {len(bench_lines)}")
    bkv = _parse_kv_ints(bench_lines[0])
    if bkv.get("cpus") != smp:
        raise RuntimeError(f"[{name}] [DRY-IRQ-BENCH] cpu mismatch: {bench_lines[0]!r}")
    for prefix in ("ioport_inb", "irq_top", "irq_ack", "irq_wake"):
        p50 = bkv.get(f"{prefix}_p50_cyc", 0)
        p95 = bkv.get(f"{prefix}_p95_cyc", 0)
        p99 = bkv.get(f"{prefix}_p99_cyc", 0)
        if p50 == 0 or not (p50 <= p95 <= p99):
            raise RuntimeError(
                f"[{name}] Invalid {prefix} percentiles {(p50, p95, p99)} in {bench_lines[0]!r}"
            )


def _read_ppm_rgb(ppm_path: pathlib.Path) -> tuple[int, int, bytes]:
    raw = ppm_path.read_bytes()
    idx = 0
    tokens: list[bytes] = []
    while len(tokens) < 4 and idx < len(raw):
        while idx < len(raw) and raw[idx] in b" \t\r\n":
            idx += 1
        if idx < len(raw) and raw[idx] == ord("#"):
            while idx < len(raw) and raw[idx] != ord("\n"):
                idx += 1
            continue
        start = idx
        while idx < len(raw) and raw[idx] not in b" \t\r\n#":
            idx += 1
        if idx > start:
            tokens.append(raw[start:idx])
    idx += 1
    w = int(tokens[1])
    h = int(tokens[2])
    rgb = raw[idx : idx + w * h * 3]
    return w, h, rgb


def _count_diff_pixels(ppm_a: pathlib.Path, ppm_b: pathlib.Path) -> int:
    wa, ha, rgba = _read_ppm_rgb(ppm_a)
    wb, hb, rgbb = _read_ppm_rgb(ppm_b)
    if (wa, ha) != (wb, hb) or len(rgba) != len(rgbb):
        raise RuntimeError(f"PPM geometry mismatch {(wa, ha)} vs {(wb, hb)}")
    mva = memoryview(rgba)
    mvb = memoryview(rgbb)
    diff = 0
    for off in range(0, len(rgba), 3):
        if mva[off : off + 3] != mvb[off : off + 3]:
            diff += 1
    return diff


class _QmpClient:
    def __init__(self, sock_path: pathlib.Path) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(8.0)
        self.sock.connect(str(sock_path))
        self.f_in = self.sock.makefile("r", encoding="utf-8")
        _greeting = self.f_in.readline()
        self.cmd("qmp_capabilities")

    def cmd(self, execute: str, arguments: dict | None = None) -> dict:
        payload: dict = {"execute": execute}
        if arguments is not None:
            payload["arguments"] = arguments
        self.sock.sendall(json.dumps(payload).encode("utf-8") + b"\r\n")
        while True:
            line = self.f_in.readline()
            if not line:
                raise RuntimeError(f"QMP EOF waiting for {execute}")
            msg = json.loads(line)
            if "return" in msg:
                return msg
            if "error" in msg:
                raise RuntimeError(f"QMP error for {execute}: {msg}")

    def send_qcodes(self, qcodes: list[str], hold_ms: int = 10) -> None:
        keys = [{"type": "qcode", "data": q} for q in qcodes]
        self.cmd("send-key", {"keys": keys, "hold-time": hold_ms})
        time.sleep(0.016)

    def type_ascii(self, text: str) -> None:
        for ch in text:
            qcodes = CHAR_TO_QCODE.get(ch)
            if qcodes is None:
                raise ValueError(f"Unsupported test character {ch!r}")
            self.send_qcodes(qcodes)

    def screendump(self, ppm_path: pathlib.Path) -> None:
        if ppm_path.exists():
            ppm_path.unlink()
        self.cmd("screendump", {"filename": str(ppm_path)})

    def close(self) -> None:
        try:
            self.f_in.close()
        finally:
            self.sock.close()


def run_p1_interactive_qmp_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    smp: int = 2,
    expect_fb: bool = True,
    png_out: pathlib.Path | None = None,
    extra_qemu_args: list[str] | None = None,
    timeout_sec: int = 55,
) -> str:
    """Boot live Ring 3 P1 interactive stack, inject real virtual PS/2 keystrokes via QMP
    `send-key`, verify shell commands + crash/restart + window move + dynamic GOP pixel deltas,
    and shut down cleanly (`exit=33`, `leaked_frames=0`)."""
    vars_copy = build_dir / f"OVMF_VARS_{name}.4m.fd"
    shutil.copyfile(ovmf_vars, vars_copy)
    qmp_sock = pathlib.Path(f"/tmp/dryos_p1_qmp_{os.getpid()}_{name}.sock")
    if qmp_sock.exists():
        qmp_sock.unlink()

    accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
    cpu_arg = "host" if accel == "kvm" else "qemu64"
    cmd = [
        "qemu-system-x86_64",
        "-machine",
        f"q35,accel={accel}",
        "-cpu",
        cpu_arg,
        "-smp",
        str(smp),
        "-m",
        "256M",
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={ovmf_code}",
        "-drive",
        f"if=pflash,format=raw,file={vars_copy}",
        "-drive",
        f"format=raw,file={img_path}",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial",
        "stdio",
        "-display",
        "none",
        "-qmp",
        f"unix:{qmp_sock},server=on,wait=off",
        "-no-reboot",
    ]
    if expect_fb:
        cmd.extend(["-vga", "std"])
    if extra_qemu_args:
        cmd.extend(extra_qemu_args)

    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
    )

    line_q: queue.Queue[str | None] = queue.Queue()
    serial_lines: list[str] = []

    def _reader() -> None:
        assert proc.stdout is not None
        for ln in proc.stdout:
            line_q.put(ln)
        line_q.put(None)

    rd_thread = threading.Thread(target=_reader, daemon=True)
    rd_thread.start()

    def wait_for_marker(marker: str, wait_s: float = 15.0) -> str:
        deadline = time.monotonic() + wait_s
        while time.monotonic() < deadline:
            rem = max(0.05, deadline - time.monotonic())
            try:
                item = line_q.get(timeout=min(0.25, rem))
            except queue.Empty:
                if proc.poll() is not None and line_q.empty():
                    break
                continue
            if item is None:
                break
            serial_lines.append(item)
            if marker in item:
                return item
        full = "".join(serial_lines)
        print(f"FULL SERIAL OUTPUT ({name}):\n{full}", file=sys.stderr)
        raise RuntimeError(f"[{name}] Timed out waiting for serial marker {marker!r}")

    ppm_before = build_dir / f"{name}_before.ppm"
    ppm_after_cmds = build_dir / f"{name}_after_cmds.ppm"
    ppm_moved = build_dir / f"{name}_moved.ppm"
    qmp: _QmpClient | None = None
    typing_px_diff = 0
    move_px_diff = 0

    try:
        wait_for_marker("[DRY-SHELL] Interactive shell ready in CPL3 Userspace", wait_s=35.0)
        qmp = _QmpClient(qmp_sock)

        if expect_fb:
            qmp.screendump(ppm_before)

        # 1. `help` + Enter
        qmp.type_ascii("help")
        qmp.send_qcodes(["ret"])
        wait_for_marker("[DRY-SHELL] CMD=help ok=true")

        # 2. `echo dry os` + Enter
        qmp.type_ascii("echo dry os")
        qmp.send_qcodes(["ret"])
        wait_for_marker('[DRY-SHELL] CMD=echo text="dry os"')

        # 3. Backspace character deletion (`echx` + Backspace + `o edit ok` -> `echo edit ok`)
        qmp.type_ascii("echx")
        qmp.send_qcodes(["backspace"])
        qmp.type_ascii("o edit ok")
        qmp.send_qcodes(["ret"])
        wait_for_marker('[DRY-SHELL] CMD=echo text="edit ok"')

        # 4. Live `system` & `uptime` kernel telemetry commands
        qmp.type_ascii("system")
        qmp.send_qcodes(["ret"])
        sys_line = wait_for_marker("[DRY-SHELL] CMD=system ")
        sys_kv = _parse_kv_ints(sys_line)
        if (
            sys_kv.get("cpus") != smp
            or sys_kv.get("ext_irqs", 0) < 20
            or sys_kv.get("free_4k", 0) < 1000
        ):
            raise RuntimeError(f"[{name}] Unexpected live system telemetry: {sys_line!r}")

        qmp.type_ascii("uptime")
        qmp.send_qcodes(["ret"])
        up_line = wait_for_marker("[DRY-SHELL] CMD=uptime ")
        up_kv = _parse_kv_ints(up_line)
        if up_kv.get("tsc_mhz", 0) < 100:
            raise RuntimeError(f"[{name}] Unexpected uptime telemetry: {up_line!r}")

        # 5. Invalid command handling
        qmp.type_ascii("nosuchcmd")
        qmp.send_qcodes(["ret"])
        wait_for_marker('[DRY-SHELL] CMD=unknown verb="nosuchcmd"')

        # 6. Bounded 64-event input ring overflow guard
        qmp.type_ascii("overflow-test")
        qmp.send_qcodes(["ret"])
        wait_for_marker("[DRY-SHELL] CMD=overflow-test retained=64 dropped=16")

        # 7. Live `dry-input` CPL=3 `#UD` crash -> supervisor reap + `CSpace` revoke + restart + recovery
        qmp.type_ascii("crash-input")
        qmp.send_qcodes(["ret"])
        wait_for_marker("[DRY-INIT] Reaped crashed dry-input, revoked stale cap & restarted")
        wait_for_marker("[DRY-SHELL] INPUT_RECOVERED ok=true")

        qmp.type_ascii("echo recovered")
        qmp.send_qcodes(["ret"])
        wait_for_marker('[DRY-SHELL] CMD=echo text="recovered"')

        if expect_fb:
            qmp.screendump(ppm_after_cmds)
            typing_px_diff = _count_diff_pixels(ppm_before, ppm_after_cmds)
            if typing_px_diff < 2500:
                raise RuntimeError(
                    f"[{name}] Framebuffer did not dynamically update after commands (diff={typing_px_diff})"
                )

            # 8. Desktop foundation stretch proof: move floating window via real `Alt + Right` / `Alt + Down`
            qmp.send_qcodes(["alt", "right"], hold_ms=16)
            wait_for_marker("[DRY-SHELL] WIN_MOVE ")
            qmp.send_qcodes(["alt", "down"], hold_ms=16)
            wait_for_marker("[DRY-SHELL] WIN_MOVE ")

            qmp.screendump(ppm_moved)
            move_px_diff = _count_diff_pixels(ppm_after_cmds, ppm_moved)
            if move_px_diff < 10000:
                raise RuntimeError(
                    f"[{name}] Floating terminal window move did not update GOP pixels (diff={move_px_diff})"
                )
            if png_out is not None:
                png_out.parent.mkdir(parents=True, exist_ok=True)
                _ppm_to_png_and_verify(ppm_moved, png_out)

        # 9. Clean Ring 3 shutdown via real keyboard `shutdown` + Enter
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        wait_for_marker("[DRY-P1-SESSION] PASS:")
        wait_for_marker("[DRY-BOOT-OK]")

        qmp.close()
        qmp = None
        rc = proc.wait(timeout=max(5.0, timeout_sec - (time.monotonic() - t0)))
    finally:
        if qmp is not None:
            qmp.close()
        if proc.poll() is None:
            proc.kill()
        rd_thread.join(timeout=2.0)
        while not line_q.empty():
            rem_ln = line_q.get_nowait()
            if rem_ln is not None:
                serial_lines.append(rem_ln)
        qmp_sock.unlink(missing_ok=True)
        ppm_before.unlink(missing_ok=True)
        ppm_after_cmds.unlink(missing_ok=True)
        ppm_moved.unlink(missing_ok=True)

    serial_out = "".join(serial_lines)
    for line in serial_out.splitlines():
        if "[DRY-INPUT]" in line or "[DRY-SHELL]" in line or "[DRY-P1-" in line or "[DRY-INIT] P1" in line:
            print(f"  {line}")

    if rc != EXIT_CODE_SUCCESS:
        print(f"FULL SERIAL OUTPUT ({name}):\n{serial_out}", file=sys.stderr)
        raise RuntimeError(f"[{name}] Expected QEMU exit {EXIT_CODE_SUCCESS}, got {rc}")

    sess_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-P1-SESSION] PASS:" in ln]
    if len(sess_lines) != 1:
        raise RuntimeError(f"[{name}] Missing [DRY-P1-SESSION] PASS line")
    skv = _parse_kv_ints(sess_lines[0])
    if (
        skv.get("cpus") != smp
        or skv.get("ext_irqs", 0) < 40
        or skv.get("irq_acks", 0) < 40
        or skv.get("ioport_denied", 1) != 0
        or skv.get("leaked_frames", 1) != 0
    ):
        raise RuntimeError(f"[{name}] Session telemetry invariant mismatch: {sess_lines[0]!r}")

    elapsed = time.monotonic() - t0
    print(
        f"[PASS] {name}: smp={smp} exit={rc} ext_irqs={skv['ext_irqs']} "
        f"typing_px_diff={typing_px_diff} move_px_diff={move_px_diff} leaked_frames=0 ({elapsed:.3f}s)."
    )
    return serial_out


def run_p1_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_esp_fn,
    assert_boot_case_fn,
    exit_code_panic: int,
) -> None:
    """Run all 9 Milestone 2A-1 (`P1 First Interaction`) hardware QMP E2E & negative security cases."""
    p1_esp = build_dir / "esp_p1_interactive.img"
    write_esp_fn(p1_esp, kernel_elf, "console=com1 interactive=1")
    run_p1_interactive_qmp_e2e("p1_interactive_1cpu_e2e", p1_esp, ovmf_code, ovmf_vars, build_dir, smp=1)
    run_p1_interactive_qmp_e2e(
        "p1_interactive_2cpu_e2e",
        p1_esp,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        png_out=root_dir / "docs" / "p1_interactive_shell.png",
    )
    run_p1_interactive_qmp_e2e("p1_interactive_4cpu_e2e", p1_esp, ovmf_code, ovmf_vars, build_dir, smp=4)
    run_p1_interactive_qmp_e2e(
        "p1_interactive_2cpu_headless_e2e",
        p1_esp,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=False,
        extra_qemu_args=["-vga", "none"],
    )

    p1_xapic_esp = build_dir / "esp_p1_interactive_xapic.img"
    write_esp_fn(p1_xapic_esp, kernel_elf, "console=com1 apic_mode=xapic interactive=1")
    run_p1_interactive_qmp_e2e(
        "p1_interactive_2cpu_xapic_e2e", p1_xapic_esp, ovmf_code, ovmf_vars, build_dir, smp=2
    )

    irq_negative_cases: list[tuple[str, str, list[str]]] = [
        (
            "irq_unauth_ioport_rejected",
            "unauth_ioport",
            ["[DRY-IRQ-FAIL] Unauthorized port I/O blocked cleanly", "[DRY-PANIC]"],
        ),
        (
            "irq_unauth_ack_rejected",
            "unauth_irq_ack",
            ["[DRY-IRQ-FAIL] Unauthorized IRQ ack / Userspace delegation blocked", "[DRY-PANIC]"],
        ),
        (
            "irq_double_ack_rejected",
            "double_irq_ack",
            ["[DRY-IRQ-FAIL] Double SYS_IRQ_ACK deterministically rejected", "[DRY-PANIC]"],
        ),
        (
            "irq_display_cap_leak_rejected",
            "display_cap_leak",
            ["[DRY-IRQ-FAIL] Display capability leak into Userspace shell compartment rejected", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in irq_negative_cases:
        assert_boot_case_fn(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 irq_fault={fault_arg}",
            smp=2,
            expected_rc=exit_code_panic,
            must_contain=expected_tokens,
            must_not_contain=["[DRY-BOOT-OK]"],
        )
