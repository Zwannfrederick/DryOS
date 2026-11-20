#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Serial, QMP screendump, and pixel-level PNG validators for Milestone 1C IPC, Services & Display."""

import json
import os
import pathlib
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
import zlib

from qemu_log_validators import _parse_kv_ints

REQUIRED_MILESTONE_1C_TRACE = [
    "[DRY-BOOT] Kernel ready",
    "[DRY-INIT] Root service started in CPL3",
    "[DRY-IPC] Endpoint created",
    "[DRY-ECHO] Service started in CPL3",
    "[DRY-IPC] CALL: ping",
    "[DRY-ECHO] REPLY: pong",
    "[DRY-IPC] Roundtrip successful",
    "[DRY-INIT] Service lifecycle verified",
]

BENCH1_METRICS = ("ep_lookup", "send_recv", "call_reply", "same_cpu", "cross_cpu", "contended")
BENCH2_METRICS = ("notif_sig_wait", "cap_transfer", "sched_wakeup", "prio_donate", "svc_startup")


def _parse_triples(line: str) -> dict[str, tuple[int, int, int]]:
    out: dict[str, tuple[int, int, int]] = {}
    for tok in line.split():
        if "=" not in tok or ".." not in tok:
            continue
        k, v = tok.split("=", 1)
        parts = v.split("..")
        if len(parts) == 3 and all(p.isdigit() for p in parts):
            out[k] = (int(parts[0]), int(parts[1]), int(parts[2]))
    return out


def verify_ipc_and_display_lines(
    name: str, serial_out: str, smp: int, expect_fb: bool = True
) -> None:
    """Validate Milestone 1C CPL=3 ordered trace, 16/16 IPC security scenarios, 11/11 benchmarks,
    and GOP framebuffer / headless fallback invariants."""
    anchor = serial_out.find("[DRY-USER-BENCH] PASS:")
    if anchor < 0:
        raise RuntimeError(f"[{name}] Missing [DRY-USER-BENCH] PASS: anchor before Milestone 1C")

    cursor = anchor
    for marker in REQUIRED_MILESTONE_1C_TRACE:
        idx = serial_out.find(marker, cursor)
        if idx < 0:
            raise RuntimeError(
                f"[{name}] Missing or out-of-order Milestone 1C trace marker {marker!r} after byte {cursor}"
            )
        cursor = idx + len(marker)

    if expect_fb:
        if "[DRY-FB] GOP validated:" not in serial_out:
            raise RuntimeError(f"[{name}] Missing [DRY-FB] GOP validated:")
        if "[DRY-DISPLAY] Rendered Ring 3 framebuffer banner" not in serial_out[anchor:]:
            raise RuntimeError(f"[{name}] Missing [DRY-DISPLAY] Rendered Ring 3 framebuffer banner")
        if "[DRY-FB] Verified Ring 3 pixels:" not in serial_out[anchor:]:
            raise RuntimeError(f"[{name}] Missing [DRY-FB] Verified Ring 3 pixels:")
    else:
        if (
            "[DRY-FB] Limine GOP framebuffer unavailable; continuing headless" not in serial_out
            and "[DRY-FB] Disabled by cmdline (no_fb=1); continuing headless" not in serial_out
        ):
            raise RuntimeError(f"[{name}] Missing headless framebuffer fallback log")

    svc_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-SERVICES] PASS:" in ln]
    if len(svc_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-SERVICES] PASS line, got {len(svc_lines)}")
    svc_kv = _parse_kv_ints(svc_lines[0])
    exp_disp_flag = "display_ok=true" if expect_fb else "display_ok=false"
    for tok in ("init_echo_ok=true", "crash_restart_ok=true", exp_disp_flag):
        if tok not in svc_lines[0]:
            raise RuntimeError(f"[{name}] Missing {tok} in {svc_lines[0]!r}")
    if svc_kv.get("cpus") != smp or svc_kv.get("leaked_frames", 1) != 0:
        raise RuntimeError(f"[{name}] Service lifecycle invariant mismatch: {svc_lines[0]!r}")

    sec_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-IPC-SEC] PASS:" in ln]
    if len(sec_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-IPC-SEC] PASS line, got {len(sec_lines)}")
    sec_kv = _parse_kv_ints(sec_lines[0])
    if (
        "scenarios=16/16" not in sec_lines[0]
        or sec_kv.get("cpus") != smp
        or sec_kv.get("smp_calls", 0) < 256
        or sec_kv.get("leaked_frames", 1) != 0
    ):
        raise RuntimeError(f"[{name}] IPC security telemetry mismatch: {sec_lines[0]!r}")

    b1_lines = [
        ln.strip()
        for ln in serial_out.splitlines()
        if ln.strip().startswith("[DRY-IPC-BENCH] cpus=")
    ]
    b2_lines = [
        ln.strip()
        for ln in serial_out.splitlines()
        if ln.strip().startswith("[DRY-IPC-BENCH2] cpus=")
    ]
    bp_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-IPC-BENCH] PASS:" in ln]
    if len(b1_lines) != 1 or len(b2_lines) != 1 or len(bp_lines) != 1:
        raise RuntimeError(
            f"[{name}] Missing IPC benchmark lines (b1={len(b1_lines)}, b2={len(b2_lines)}, pass={len(bp_lines)})"
        )
    bp_kv = _parse_kv_ints(bp_lines[0])
    if "metrics=11/11" not in bp_lines[0] or bp_kv.get("cpus") != smp or bp_kv.get("leaked_frames", 1) != 0:
        raise RuntimeError(f"[{name}] IPC benchmark summary mismatch: {bp_lines[0]!r}")

    triples = {**_parse_triples(b1_lines[0]), **_parse_triples(b2_lines[0])}
    for m in BENCH1_METRICS + BENCH2_METRICS:
        if m not in triples:
            raise RuntimeError(f"[{name}] Missing IPC benchmark metric {m!r}")
        p50, p95, p99 = triples[m]
        if p50 == 0 or not (p50 <= p95 <= p99):
            raise RuntimeError(f"[{name}] Invalid IPC percentiles for {m}: {(p50, p95, p99)}")


def _ppm_to_png_and_verify(ppm_path: pathlib.Path, png_path: pathlib.Path) -> tuple[int, int, int, int]:
    """Parse binary P6 PPM, verify non-blank Ring 3 banner contrast & palette, and write RGB PNG."""
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
    if len(tokens) != 4 or tokens[0] != b"P6":
        raise RuntimeError(f"Unexpected PPM header in {ppm_path}: {tokens!r}")
    idx += 1  # single whitespace byte after maxval

    width = int(tokens[1])
    height = int(tokens[2])
    maxval = int(tokens[3])
    expected_rgb_bytes = width * height * 3
    rgb = raw[idx : idx + expected_rgb_bytes]
    if maxval != 255 or len(rgb) != expected_rgb_bytes or width < 640 or height < 480:
        raise RuntimeError(
            f"Invalid screendump geometry {width}x{height} maxval={maxval} bytes={len(rgb)}/{expected_rgb_bytes}"
        )

    bg_rgb = rgb[0:3]
    non_bg_pixels = 0
    distinct_colors: set[bytes] = set()
    mv = memoryview(rgb)
    for off in range(0, expected_rgb_bytes, 3):
        px = bytes(mv[off : off + 3])
        if len(distinct_colors) < 32:
            distinct_colors.add(px)
        if px != bg_rgb:
            non_bg_pixels += 1

    if non_bg_pixels < 4000 or len(distinct_colors) < 4:
        raise RuntimeError(
            f"Screendump appears blank or low-contrast: non_bg={non_bg_pixels} distinct={len(distinct_colors)}"
        )

    # Encode standard 8-bit RGB PNG (color type 2) using stdlib `zlib` + `struct`.
    row_stride = width * 3
    scanlines = bytearray((row_stride + 1) * height)
    out_pos = 0
    for y in range(height):
        scanlines[out_pos] = 0  # filter type 0 (None)
        out_pos += 1
        src_off = y * row_stride
        scanlines[out_pos : out_pos + row_stride] = mv[src_off : src_off + row_stride]
        out_pos += row_stride

    def png_chunk(tag: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(tag)
        crc = zlib.crc32(payload, crc) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + tag + payload + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    idat = zlib.compress(bytes(scanlines), level=6)
    png_bytes = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", idat)
        + png_chunk(b"IEND", b"")
    )
    png_path.write_bytes(png_bytes)
    return width, height, non_bg_pixels, len(distinct_colors)


def capture_and_verify_gop_screendump(
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    png_out: pathlib.Path,
    timeout_sec: int = 40,
) -> str:
    """Boot QEMU UEFI GOP (`-vga std`), wait for `[DRY-FB-READY]`, capture QMP `screendump`,
    convert `.ppm -> .png`, and verify non-blank Ring 3 pixel output."""
    uid = f"{png_out.stem}_{os.getpid()}_{threading.get_ident()}"
    vars_copy = build_dir / f"OVMF_VARS_screendump_{uid}.4m.fd"
    shutil.copyfile(ovmf_vars, vars_copy)
    qmp_sock = pathlib.Path(f"/tmp/dryos_qmp_{uid}.sock")
    if qmp_sock.exists():
        qmp_sock.unlink()
    ppm_path = build_dir / f"dry_os_{uid}.ppm"
    if ppm_path.exists():
        ppm_path.unlink()

    accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
    cpu_arg = "host" if accel == "kvm" else "qemu64"
    cmd = [
        "qemu-system-x86_64",
        "-machine",
        f"q35,accel={accel}",
        "-cpu",
        cpu_arg,
        "-smp",
        "2",
        "-m",
        "256M",
        "-vga",
        "std",
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

    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
    )
    serial_lines: list[str] = []
    ready = False
    try:
        assert proc.stdout is not None
        while time.monotonic() - t0 < timeout_sec:
            line = proc.stdout.readline()
            if not line:
                if proc.poll() is not None:
                    break
                continue
            serial_lines.append(line)
            if "[DRY-FB-READY] Waiting for QMP screendump..." in line:
                ready = True
                break

        if not ready:
            full = "".join(serial_lines)
            print("FULL SCREENDUMP SERIAL OUTPUT:\n" + full, file=sys.stderr)
            raise RuntimeError("QEMU exited or timed out before [DRY-FB-READY]")

        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(5.0)
            s.connect(str(qmp_sock))
            f_in = s.makefile("r", encoding="utf-8")
            _greeting = f_in.readline()
            s.sendall(b'{"execute": "qmp_capabilities"}\r\n')
            while True:
                resp = json.loads(f_in.readline())
                if "return" in resp:
                    break
            dump_cmd = json.dumps(
                {"execute": "screendump", "arguments": {"filename": str(ppm_path)}}
            ).encode("utf-8")
            s.sendall(dump_cmd + b"\r\n")
            while True:
                resp = json.loads(f_in.readline())
                if "return" in resp:
                    break
                if "error" in resp:
                    raise RuntimeError(f"QMP screendump error: {resp}")
            s.sendall(b'{"execute": "quit"}\r\n')

        proc.wait(timeout=5.0)
    finally:
        if proc.poll() is None:
            proc.kill()
        if qmp_sock.exists():
            qmp_sock.unlink()
        vars_copy.unlink(missing_ok=True)

    if not ppm_path.exists():
        raise RuntimeError(f"QMP screendump did not produce {ppm_path}")

    w, h, non_bg, n_colors = _ppm_to_png_and_verify(ppm_path, png_out)
    ppm_path.unlink(missing_ok=True)
    elapsed = time.monotonic() - t0
    serial_out = "".join(serial_lines)
    verify_ipc_and_display_lines("boot_display_screendump", serial_out, smp=2, expect_fb=True)
    print(
        f"[PASS] boot_display_screendump: QMP screendump -> {png_out} ({w}x{h}, non_bg_px={non_bg}, colors={n_colors}, {elapsed:.3f}s)."
    )
    return serial_out
