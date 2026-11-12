#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone 2A-2 Hardware QMP PS/2 Mouse (`input-send-event`) + Keyboard (`send-key`) + Multi-Window
Compositor, Crash-Isolation, Security & Latency Benchmark Validators."""

import os
import pathlib
import queue
import shutil
import subprocess
import sys
import threading
import time

from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints
from qemu_p1_validators import CHAR_TO_QCODE, EXIT_CODE_SUCCESS, _count_diff_pixels, _QmpClient


class _DesktopQmpClient(_QmpClient):
    """Extends `_QmpClient` with real virtual PS/2 mouse `input-send-event` operations."""

    def verify_ps2_mouse_present(self) -> None:
        resp = self.cmd("query-mice")
        mice = resp.get("return", [])
        if not any("PS/2" in m.get("name", "") for m in mice):
            raise RuntimeError(f"Expected QEMU PS/2 Mouse in query-mice, got {mice!r}")

    def move_rel(self, dx: int, dy: int, settle_s: float = 0.012) -> None:
        events = []
        if dx != 0:
            events.append({"type": "rel", "data": {"axis": "x", "value": int(dx)}})
        if dy != 0:
            events.append({"type": "rel", "data": {"axis": "y", "value": int(dy)}})
        if events:
            self.cmd("input-send-event", {"events": events})
            if settle_s > 0:
                time.sleep(settle_s)

    def move_in_steps(self, total_dx: int, total_dy: int, steps: int = 6, step_s: float = 0.008) -> None:
        # Keep each QMP `rel` step <= 100 px (1 PS/2 3-byte packet) so QEMU's 16-byte host `PS2State` queue never drops bytes.
        min_steps = (max(abs(total_dx), abs(total_dy)) + 99) // 100
        steps = max(1, steps, min_steps)
        sent_x = 0
        sent_y = 0
        for i in range(1, steps + 1):
            tx = round(total_dx * i / steps)
            ty = round(total_dy * i / steps)
            self.move_rel(tx - sent_x, ty - sent_y, settle_s=step_s)
            sent_x = tx
            sent_y = ty

    def mouse_button(self, button: str, down: bool, settle_s: float = 0.014) -> None:
        self.cmd(
            "input-send-event",
            {"events": [{"type": "btn", "data": {"down": bool(down), "button": button}}]},
        )
        if settle_s > 0:
            time.sleep(settle_s)

    def click_left(self, hold_s: float = 0.016) -> None:
        self.mouse_button("left", True, settle_s=hold_s)
        self.mouse_button("left", False, settle_s=0.016)

    def drag_left(self, total_dx: int, total_dy: int, steps: int = 8) -> None:
        self.mouse_button("left", True, settle_s=0.018)
        self.move_in_steps(total_dx, total_dy, steps=steps, step_s=0.012)
        self.mouse_button("left", False, settle_s=0.018)


class _DesktopSessionHarness:
    """Manages a live QEMU/KVM UEFI desktop VM, streaming COM1 lines + QMP commands."""

    def __init__(
        self,
        name: str,
        img_path: pathlib.Path,
        ovmf_code: pathlib.Path,
        ovmf_vars: pathlib.Path,
        build_dir: pathlib.Path,
        smp: int = 2,
        expect_fb: bool = True,
        extra_qemu_args: list[str] | None = None,
        mem: str = "256M",
        enable_debug_exit: bool = True,
    ) -> None:
        self.name = name
        self.smp = smp
        self.expect_fb = expect_fb
        self.build_dir = build_dir
        self.t0 = time.monotonic()

        vars_copy = build_dir / f"OVMF_VARS_{name}.4m.fd"
        shutil.copyfile(ovmf_vars, vars_copy)
        self.qmp_sock = pathlib.Path(f"/tmp/dryos_p2_qmp_{os.getpid()}_{name}.sock")
        self.qmp_sock.unlink(missing_ok=True)

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
            mem,
            "-drive",
            f"if=pflash,format=raw,readonly=on,file={ovmf_code}",
            "-drive",
            f"if=pflash,format=raw,file={vars_copy}",
            "-drive",
            f"format=raw,file={img_path}",
            "-serial",
            "stdio",
            "-display",
            "none",
            "-qmp",
            f"unix:{self.qmp_sock},server=on,wait=off",
            "-no-reboot",
        ]
        if enable_debug_exit:
            cmd.extend(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        if expect_fb:
            cmd.extend(["-vga", "std"])
        if extra_qemu_args:
            cmd.extend(extra_qemu_args)

        self.proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            bufsize=1,
        )
        self.line_q: queue.Queue[str | None] = queue.Queue()
        self.serial_lines: list[str] = []
        self.rd_thread = threading.Thread(target=self._reader, daemon=True)
        self.rd_thread.start()
        self.qmp: _DesktopQmpClient | None = None

    def _reader(self) -> None:
        assert self.proc.stdout is not None
        for ln in self.proc.stdout:
            self.line_q.put(ln)
        self.line_q.put(None)

    def drain_available(self) -> None:
        while True:
            try:
                item = self.line_q.get_nowait()
            except queue.Empty:
                break
            if item is not None:
                self.serial_lines.append(item)

    def wait_for(self, marker: str, wait_s: float = 18.0, start_idx: int = 0) -> str:
        for existing in self.serial_lines[start_idx:]:
            if marker in existing:
                return existing
        deadline = time.monotonic() + wait_s
        while time.monotonic() < deadline:
            rem = max(0.05, deadline - time.monotonic())
            try:
                item = self.line_q.get(timeout=min(0.25, rem))
            except queue.Empty:
                if self.proc.poll() is not None and self.line_q.empty():
                    break
                continue
            if item is None:
                break
            self.serial_lines.append(item)
            if marker in item:
                return item
        full = "".join(self.serial_lines)
        print(f"FULL SERIAL OUTPUT ({self.name}):\n{full}", file=sys.stderr)
        raise RuntimeError(f"[{self.name}] Timed out waiting for serial marker {marker!r}")

    def boot_to_desktop_ready(self) -> _DesktopQmpClient:
        self.wait_for("[DRY-SHELL] Interactive shell ready in CPL3 Userspace", wait_s=35.0)
        self.wait_for("[DRY-SYSMON] Ready in CPL3 Userspace", wait_s=15.0)
        if self.expect_fb:
            self.wait_for("active=2", wait_s=15.0)
        self.qmp = _DesktopQmpClient(self.qmp_sock)
        self.qmp.verify_ps2_mouse_present()
        return self.qmp

    def finish_and_verify(self, timeout_s: float = 25.0, require_mouse_irqs: bool = True) -> tuple[str, dict[str, int]]:
        self.wait_for("[DRY-P2-SESSION] PASS:", wait_s=timeout_s)
        self.wait_for("[DRY-BOOT-OK]", wait_s=10.0)
        if self.qmp is not None:
            self.qmp.close()
            self.qmp = None
        rc = self.proc.wait(timeout=10.0)
        self.drain_available()
        serial_out = "".join(self.serial_lines)

        for line in serial_out.splitlines():
            if any(
                tok in line
                for tok in (
                    "[DRY-IOAPIC]",
                    "[DRY-DISPLAY]",
                    "[DRY-INPUT",
                    "[DRY-WM",
                    "[DRY-SYSMON]",
                    "[DRY-SHELL]",
                    "[DRY-INIT] SYSMON",
                    "[DRY-P2-SESSION]",
                )
            ):
                print(f"  {line}")

        if rc != EXIT_CODE_SUCCESS:
            print(f"FULL SERIAL OUTPUT ({self.name}):\n{serial_out}", file=sys.stderr)
            raise RuntimeError(f"[{self.name}] Expected QEMU exit {EXIT_CODE_SUCCESS}, got {rc}")

        sess = [ln.strip() for ln in serial_out.splitlines() if "[DRY-P2-SESSION] PASS:" in ln]
        if len(sess) != 1:
            raise RuntimeError(f"[{self.name}] Missing [DRY-P2-SESSION] PASS line")
        skv = _parse_kv_ints(sess[0])
        if (
            skv.get("cpus") != self.smp
            or skv.get("ioport_denied", 1) != 0
            or skv.get("leaked_frames", 1) != 0
            or (require_mouse_irqs and skv.get("irq12", 0) < 3)
        ):
            raise RuntimeError(f"[{self.name}] P2 session telemetry mismatch: {sess[0]!r}")
        return serial_out, skv

    def close(self) -> None:
        if self.qmp is not None:
            try:
                self.qmp.close()
            except Exception:
                pass
            self.qmp = None
        if self.proc.poll() is None:
            self.proc.kill()
        self.rd_thread.join(timeout=2.0)
        self.qmp_sock.unlink(missing_ok=True)


def run_p2_standard_desktop_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    smp: int = 2,
    expect_fb: bool = True,
    expected_apic: str | None = None,
    preview_png: pathlib.Path | None = None,
    artifact_dir: pathlib.Path | None = None,
    extra_qemu_args: list[str] | None = None,
) -> str:
    """Core Milestone 2A-2 QMP mouse + keyboard multi-window E2E scenario."""
    h = _DesktopSessionHarness(
        name, img_path, ovmf_code, ovmf_vars, build_dir, smp=smp, expect_fb=expect_fb, extra_qemu_args=extra_qemu_args
    )
    ppm_init = build_dir / f"{name}_00_init.ppm"
    ppm_cur = build_dir / f"{name}_01_cur.ppm"
    ppm_sysmon = build_dir / f"{name}_02_sysmon.ppm"
    ppm_drag = build_dir / f"{name}_03_drag.ppm"
    cursor_px_diff = 0
    drag_px_diff = 0

    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-SYSMON] REFRESH seq=2", wait_s=10.0)

        if not expect_fb:
            # Headless `-vga none`: verify both Userspace apps run & shell processes commands cleanly.
            qmp.type_ascii("echo headless p2")
            qmp.send_qcodes(["ret"])
            h.wait_for('[DRY-SHELL] CMD=echo text="headless p2"')
            qmp.type_ascii("system")
            qmp.send_qcodes(["ret"])
            h.wait_for("[DRY-SHELL] CMD=system ")
            qmp.type_ascii("shutdown")
            qmp.send_qcodes(["ret"])
            serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
            print(f"[PASS] {name}: smp={smp} headless=true leaked_frames=0 ({time.monotonic() - h.t0:.2f}s).")
            return serial_out

        qmp.screendump(ppm_init)

        # 1. Move software cursor slightly in empty canvas (`(640,400) -> (616,440)`) & verify dirty-rect-only update!
        qmp.move_rel(-24, 40)
        h.wait_for("[DRY-INPUT] MOUSE_FIRST ")
        qmp.screendump(ppm_cur)
        cursor_px_diff = _count_diff_pixels(ppm_init, ppm_cur)
        if not (16 <= cursor_px_diff <= 1024):
            raise RuntimeError(
                f"[{name}] Expected cursor-only dirty-rect update (16..1024 px), got diff={cursor_px_diff}"
            )
        if artifact_dir is not None:
            _ppm_to_png_and_verify(ppm_cur, artifact_dir / "p2_01_initial_two_windows.png")

        # 2. Move cursor over Window B (`dry-sysmon` at `636..1096, 52..414`) -> `(820, 220)` and click to focus!
        idx_before_focus = len(h.serial_lines)
        qmp.move_in_steps(204, -220, steps=5)
        qmp.click_left()
        h.wait_for("[DRY-WM] FOCUS id=2", start_idx=idx_before_focus)

        # Type `'r'` while `dry-sysmon` holds focus: must reach `dry-sysmon` and NEVER leak to `dry-shell`!
        qmp.type_ascii("r")
        h.wait_for("[DRY-SYSMON] KEY ascii=0x72 count=1")
        qmp.screendump(ppm_sysmon)
        if artifact_dir is not None:
            _ppm_to_png_and_verify(ppm_sysmon, artifact_dir / "p2_02_sysmon_focused.png")

        # 3. Move cursor to Window A (`dry-shell` at `24..612, 52..446`) titlebar `(260, 66)`, click & drag!
        idx_before_w1 = len(h.serial_lines)
        qmp.move_in_steps(-560, -154, steps=6)
        qmp.drag_left(96, 44, steps=6)
        h.wait_for("[DRY-WM] FOCUS id=1", start_idx=idx_before_w1)
        drag_line = h.wait_for("[DRY-WM] DRAG_END id=1", start_idx=idx_before_w1)
        dkv = _parse_kv_ints(drag_line)
        if dkv.get("x", 24) <= 24 or dkv.get("y", 52) <= 52:
            raise RuntimeError(f"[{name}] Window A did not move to new coordinates: {drag_line!r}")

        # 4. Run live Ring 3 shell commands inside dragged Window A (`help` + `system` + `echo`).
        # Notice: if `'r'` had leaked earlier into `dry-shell`, `echo desktop ok` would become `recho desktop ok`!
        qmp.type_ascii("echo desktop ok")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="desktop ok"')

        qmp.type_ascii("system")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=system ")

        qmp.screendump(ppm_drag)
        drag_px_diff = _count_diff_pixels(ppm_sysmon, ppm_drag)
        if drag_px_diff < 12000:
            raise RuntimeError(f"[{name}] Expected major GOP scene delta after drag+commands, got {drag_px_diff}")

        if preview_png is not None:
            preview_png.parent.mkdir(parents=True, exist_ok=True)
            _ppm_to_png_and_verify(ppm_drag, preview_png)
        if artifact_dir is not None:
            _ppm_to_png_and_verify(ppm_drag, artifact_dir / "p2_03_dragged_desktop_preview.png")

        # 5. Clean Ring 3 shutdown
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=True)

        if expected_apic is not None and f"mode={expected_apic}" not in serial_out:
            raise RuntimeError(f"[{name}] Expected APIC mode={expected_apic} in serial log")

        print(
            f"[PASS] {name}: smp={smp} irq1={skv['irq1']} irq12={skv['irq12']} "
            f"cursor_px_diff={cursor_px_diff} drag_px_diff={drag_px_diff} leaked_frames=0 "
            f"({time.monotonic() - h.t0:.2f}s)."
        )
        return serial_out
    finally:
        h.close()
        for p in (ppm_init, ppm_cur, ppm_sysmon, ppm_drag):
            p.unlink(missing_ok=True)


def run_p2_cursor_zorder_tiling_close_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    artifact_dir: pathlib.Path | None = None,
) -> None:
    """Verify screen-edge cursor clamping, overlapping Z-order raise/clipping, `Alt+Tab`,
    `Alt+T` + topbar pill side-by-side tiling toggle, and `[x]` mouse window close."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    ppm_overlap = build_dir / f"{name}_overlap.ppm"
    ppm_tiled = build_dir / f"{name}_tiled.ppm"
    try:
        qmp = h.boot_to_desktop_ready()

        # 1. Slam cursor into top-left `(0, 0)` and bottom-right `(1279, 799)` screen boundaries!
        qmp.move_in_steps(-700, -450)
        qmp.move_in_steps(1350, 850)
        # Return from `(1279, 799)` to Window A titlebar `(220, 66)`: `(-1059, -733)`
        qmp.move_in_steps(-1059, -733)

        # 2. Drag Window A right by `+220 px` so it overlaps Window B (`244..832` overlaps `636..1096`)!
        idx = len(h.serial_lines)
        qmp.drag_left(220, 20, steps=6)
        h.wait_for("[DRY-WM] DRAG_END id=1", start_idx=idx)

        # Click visible right portion of Window B `(960, 200)` -> raises Window B above overlapping Window A!
        idx = len(h.serial_lines)
        qmp.move_in_steps(520, 114, steps=5)
        qmp.click_left()
        h.wait_for("[DRY-WM] FOCUS id=2", start_idx=idx)
        time.sleep(0.30)  # Let `dry-sysmon` refresh while overlapping in foreground.
        qmp.screendump(ppm_overlap)
        if artifact_dir is not None:
            _ppm_to_png_and_verify(ppm_overlap, artifact_dir / "p2_04_zorder_overlap.png")

        # 3. `Alt + Tab` cycles focus & Z-order back to Window A (`id=1`)!
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "tab"], hold_ms=18)
        h.wait_for("[DRY-WM] FOCUS id=1", start_idx=idx)

        # 4. `Alt + T` snaps both windows into deterministic side-by-side tiling (`mode=1`)!
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"], hold_ms=18)
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)
        qmp.screendump(ppm_tiled)
        if artifact_dir is not None:
            _ppm_to_png_and_verify(ppm_tiled, artifact_dir / "p2_05_tiled_side_by_side.png")
        if _count_diff_pixels(ppm_overlap, ppm_tiled) < 15000:
            raise RuntimeError(f"[{name}] Side-by-side tiling did not reposition windows visually")

        # Click topbar `[MODE: TILED]` pill at `(640, 14)` to toggle back to `Floating` (`mode=0`)!
        # Current cursor is `(960, 200)` -> move `(-320, -186)` to `(640, 14)`.
        idx = len(h.serial_lines)
        qmp.move_in_steps(-320, -186, steps=5)
        qmp.click_left()
        h.wait_for("[DRY-WM] LAYOUT mode=0", start_idx=idx)

        # 5. Click `[x]` close button on Window B (`rect = (636, 52, 460, 362)` -> `close_btn = (1066, 58, 22, 20)`).
        # Move from `(640, 14)` by `(+436, +52)` -> `(1076, 66)` inside `[x]`!
        idx = len(h.serial_lines)
        qmp.move_in_steps(436, 52, steps=5)
        qmp.click_left()
        h.wait_for("[DRY-WM] CLOSE_REQ id=2", start_idx=idx)
        h.wait_for("[DRY-WM] WIN_CLOSED id=2 active=1", start_idx=idx)
        h.wait_for("[DRY-INIT] SYSMON_REAPED kind=0", start_idx=idx)

        # Focus automatically returned to Window A (`dry-shell`): type `shutdown`!
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, _ = h.finish_and_verify(require_mouse_irqs=True)

        wstats = [ln for ln in serial_out.splitlines() if "[DRY-WM-STATS]" in ln]
        if not wstats or _parse_kv_ints(wstats[0]).get("clamp_hits", 0) < 2:
            raise RuntimeError(f"[{name}] Expected >=2 screen-edge cursor clamps in {wstats!r}")

        print(f"[PASS] {name}: clamp, Z-order overlap, Alt+Tab, Alt+T/pill tiling & [x] close verified ({time.monotonic() - h.t0:.2f}s).")
    finally:
        h.close()
        ppm_overlap.unlink(missing_ok=True)
        ppm_tiled.unlink(missing_ok=True)


def run_p2_app_crash_isolation_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Crash Window B (`dry-sysmon`) with CPL=3 `#UD` (`vec=6`) and prove Window A (`dry-shell`),
    `dry-input`, and `dry-display` continue operating without memory leaks or stale surfaces."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    try:
        qmp = h.boot_to_desktop_ready()

        # Move from `(640, 400)` to Window B `(820, 220)`, click to focus, and inject `'x'` (`ud2`)!
        qmp.move_in_steps(180, -180, steps=4)
        qmp.click_left()
        h.wait_for("[DRY-WM] FOCUS id=2")

        qmp.type_ascii("x")
        h.wait_for("[DRY-SYSMON] Injecting CPL3 #UD crash in Window B")
        h.wait_for("[DRY-INIT] SYSMON_REAPED kind=1 vec=6 window_cleaned=true")
        h.wait_for("[DRY-WM] OWNER_REAPED badge=0x1030002 active=1")

        # Window A (`dry-shell`) automatically regained focus and must remain 100% interactive!
        qmp.type_ascii("echo window-a-survived")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="window-a-survived"')

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        h.finish_and_verify(require_mouse_irqs=True)
        print(f"[PASS] {name}: Window B #UD contained & reaped; Window A survived cleanly ({time.monotonic() - h.t0:.2f}s).")
    finally:
        h.close()


def run_p2_irq_interleave_flood_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Flood interleaved `IRQ1` keyboard + `IRQ12` mouse packets with zero packet desync or hang."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    try:
        qmp = h.boot_to_desktop_ready()

        # Interleave 36 rapid PS/2 mouse packets + middle/right button clicks + keyboard typing of `echo flood42`!
        phrase = "echo flood42"
        for i, ch in enumerate(phrase):
            qmp.move_rel(9 if (i & 1) == 0 else -7, -6 if (i & 1) == 0 else 8, settle_s=0.0)
            qmp.move_rel(5, -4, settle_s=0.0)
            if i % 4 == 0:
                qmp.mouse_button("right", True, settle_s=0.0)
                qmp.mouse_button("right", False, settle_s=0.0)
            qmp.send_qcodes(CHAR_TO_QCODE[ch], hold_ms=6)
        for _ in range(16):
            qmp.move_rel(-6, 5, settle_s=0.002)
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="flood42"')

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=True)

        istats = [ln for ln in serial_out.splitlines() if "[DRY-INPUT-STATS]" in ln]
        if not istats or _parse_kv_ints(istats[0]).get("desync", 1) != 0:
            raise RuntimeError(f"[{name}] PS/2 mouse packet desync detected under flood: {istats!r}")

        print(
            f"[PASS] {name}: interleaved IRQ1={skv['irq1']} IRQ12={skv['irq12']} desync=0 leaked_frames=0 "
            f"({time.monotonic() - h.t0:.2f}s)."
        )
    finally:
        h.close()


def run_p2_wm_security_rejection_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Verify Ring 3 WM badge enforcement rejects cross-window surface/input/close/forged-event attacks."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-WM-SEC] PASS:")
        qmp.move_rel(16, 16)
        qmp.move_rel(-8, 12)
        qmp.move_rel(10, -6)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, _ = h.finish_and_verify(require_mouse_irqs=True)

        wstats = [ln for ln in serial_out.splitlines() if "[DRY-WM-STATS]" in ln]
        wkv = _parse_kv_ints(wstats[0]) if wstats else {}
        if wkv.get("unauth_rej", 0) < 6 or wkv.get("inval_rej", 0) < 1:
            raise RuntimeError(f"[{name}] Expected >=6 unauth_rej & >=1 inval_rej, got {wstats!r}")

        print(
            f"[PASS] {name}: unauth_rej={wkv['unauth_rej']} inval_rej={wkv['inval_rej']} leaked_frames=0 "
            f"({time.monotonic() - h.t0:.2f}s)."
        )
    finally:
        h.close()


def run_p2_perf_bench_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Collect p50/p95/p99 cycle distributions for IRQ12 drain, input->compositor, cursor blit,
    window drag redraw, and 1-window vs 2-window surface presentation."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    try:
        qmp = h.boot_to_desktop_ready()

        # 1. 24 pure cursor movements around `(640, 400)` -> ends back at `(640, 400)`
        for i in range(24):
            qmp.move_rel(10 if (i % 2 == 0) else -10, 8 if (i % 2 == 0) else -8, settle_s=0.008)

        # 2. Move to Window A titlebar `(260, 66)` and perform 16 drag steps (`+64, +32`)
        qmp.move_in_steps(-380, -334, steps=5)
        qmp.drag_left(64, 32, steps=16)
        h.wait_for("[DRY-WM] DRAG_END id=1")

        # 3. Measure 2-window `IPC_OP_SURFACE_PRESENT` latencies (`pres2w`)
        qmp.type_ascii("echo two-win-bench")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="two-win-bench"')

        # 4. Focus Window B (`dry-sysmon`), press `'q'` to close cleanly -> measure 1-window `pres1w`!
        qmp.move_in_steps(500, 120, steps=4)
        qmp.click_left()
        h.wait_for("[DRY-WM] FOCUS id=2")
        qmp.type_ascii("q")
        h.wait_for("[DRY-SYSMON] Cleanly closed Window B")
        h.wait_for("[DRY-WM] WIN_CLOSED id=2 active=1")

        qmp.type_ascii("echo one-win-bench")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="one-win-bench"')

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, _ = h.finish_and_verify(require_mouse_irqs=True)

        bench_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-WM-BENCH]" in ln]
        if len(bench_lines) != 1:
            raise RuntimeError(f"[{name}] Missing [DRY-WM-BENCH] line in serial output")
        print(f"[PASS] {name}: {bench_lines[0]} ({time.monotonic() - h.t0:.2f}s).")
    finally:
        h.close()


def run_p2_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_esp_fn,
    assert_boot_case_fn,
    exit_code_panic: int,
    artifact_dir: pathlib.Path | None = None,
) -> None:
    """Execute all 14 Milestone 2A-2 (`DRY Desktop Preview`) QEMU/KVM E2E & negative security tests."""
    p2_esp = build_dir / "esp_p2_desktop.img"
    write_esp_fn(p2_esp, kernel_elf, "console=com1 desktop=1")

    run_p2_standard_desktop_e2e("p2_desktop_qmp_1cpu", p2_esp, ovmf_code, ovmf_vars, build_dir, smp=1)
    run_p2_standard_desktop_e2e(
        "p2_desktop_qmp_2cpu",
        p2_esp,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        preview_png=root_dir / "docs" / "p2_desktop_preview.png",
        artifact_dir=artifact_dir,
    )
    run_p2_standard_desktop_e2e("p2_desktop_qmp_4cpu", p2_esp, ovmf_code, ovmf_vars, build_dir, smp=4)
    run_p2_standard_desktop_e2e(
        "p2_desktop_headless_2cpu",
        p2_esp,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=False,
        extra_qemu_args=["-vga", "none"],
    )

    p2_xapic_esp = build_dir / "esp_p2_desktop_xapic.img"
    write_esp_fn(p2_xapic_esp, kernel_elf, "console=com1 apic_mode=xapic desktop=1")
    run_p2_standard_desktop_e2e(
        "p2_desktop_qmp_xapic_2cpu",
        p2_xapic_esp,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expected_apic="xapic-mmio",
    )

    run_p2_cursor_zorder_tiling_close_e2e(
        "p2_cursor_zorder_tiling_2cpu", p2_esp, ovmf_code, ovmf_vars, build_dir, artifact_dir=artifact_dir
    )
    run_p2_app_crash_isolation_e2e(
        "p2_app_crash_isolation_2cpu", p2_esp, ovmf_code, ovmf_vars, build_dir
    )
    run_p2_irq_interleave_flood_e2e(
        "p2_irq_interleave_flood_2cpu", p2_esp, ovmf_code, ovmf_vars, build_dir
    )

    p2_sec_esp = build_dir / "esp_p2_desktop_sec.img"
    write_esp_fn(p2_sec_esp, kernel_elf, "console=com1 desktop=1 desktop_sec=1")
    run_p2_wm_security_rejection_e2e(
        "p2_wm_security_rejection_2cpu", p2_sec_esp, ovmf_code, ovmf_vars, build_dir
    )

    p2_bench_esp = build_dir / "esp_p2_desktop_bench.img"
    write_esp_fn(p2_bench_esp, kernel_elf, "console=com1 desktop=1 desktop_bench=1")
    run_p2_perf_bench_e2e("p2_perf_bench_2cpu", p2_bench_esp, ovmf_code, ovmf_vars, build_dir)

    p2_negative_cases: list[tuple[str, str, list[str]]] = [
        (
            "irq12_unauth_bind_rejected",
            "irq12_unauth_bind",
            ["[DRY-IRQ-FAIL] Unauthorized Userspace IRQ12 mouse bind rejected", "[DRY-PANIC]"],
        ),
        (
            "irq12_split_owner_rejected",
            "irq12_split_owner",
            ["[DRY-IRQ-FAIL] Split/duplicate IRQ12 PS/2 mouse ownership rejected", "[DRY-PANIC]"],
        ),
        (
            "wm_surface_ro_escalation_rejected",
            "wm_surface_ro_escalation",
            ["[DRY-IRQ-FAIL] Cross-window RO->RW surface escalation rejected", "[DRY-PANIC]"],
        ),
        (
            "fb_backing_oob_rejected",
            "fb_backing_oob",
            ["[DRY-IRQ-FAIL] Framebuffer OOB/undersized/unaligned map bounds rejected", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in p2_negative_cases:
        assert_boot_case_fn(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 irq_fault={fault_arg}",
            smp=2,
            expected_rc=exit_code_panic,
            must_contain=expected_tokens,
            must_not_contain=["[DRY-BOOT-OK]"],
        )
