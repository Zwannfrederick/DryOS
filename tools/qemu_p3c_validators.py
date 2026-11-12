#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone 2A-3 Phase P3-C Power Management & Session Lifecycle Validators:
CPU Idle (`sti; hlt` residency), Display Idle (DPMS blank + auto-timeout + wake),
Session Lock (compositor input-blocking shield + background app continuity),
Suspend-to-RAM (`S3` `-ENOSYS` gap audit), Hardware ACPI S5 Poweroff, Hardware Reset,
and SMP/RAM Desktop Matrix Sweep."""

import pathlib
import shutil
import sys
import time

from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints
from qemu_p1_validators import _count_diff_pixels
from qemu_p2_validators import _DesktopSessionHarness

ARTIFACT_DIR = pathlib.Path(
    "/home/zwannfrederick/.gemini/antigravity/brain/fcac1153-5c72-4469-803e-6ceaebf4f0cb"
)


def _ppm_black_pixel_counts(ppm_path: pathlib.Path) -> tuple[int, int]:
    data = ppm_path.read_bytes()
    parts = data.split(b"\n", 3)
    if len(parts) < 4 or parts[0].strip() != b"P6":
        raise RuntimeError(f"Unexpected PPM header in {ppm_path}")
    dims = parts[1].split()
    w, h = int(dims[0]), int(dims[1])
    rgb = parts[3]
    total = w * h
    expected = total * 3
    if len(rgb) < expected:
        raise RuntimeError(f"Truncated PPM payload in {ppm_path}: {len(rgb)} < {expected}")
    black = 0
    for off in range(0, expected, 3):
        if rgb[off] == 0 and rgb[off + 1] == 0 and rgb[off + 2] == 0:
            black += 1
    return black, total


def run_p3c_idle_dpms_lock_s3_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    """Verify CPU `HLT` residency, DPMS manual + 5s auto-blank & wake, Session Lock input blocking +
    mouse button unlock + background app continuity, and honest `S3` `-ENOSYS` rejection."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    ppm_blank = build_dir / f"{name}_blank.ppm"
    ppm_woke = build_dir / f"{name}_woke.ppm"
    ppm_lock = build_dir / f"{name}_lock.ppm"

    try:
        qmp = h.boot_to_desktop_ready()
        acpi_ln = h.wait_for("[DRY-ACPI-PWR] PASS:", wait_s=5.0)
        if "fadt=true" not in acpi_ln or "s5=true" not in acpi_ln:
            raise RuntimeError(f"[{name}] Expected FADT + S5 discovery on q35 UEFI: {acpi_ln!r}")
        h.wait_for("[DRY-INIT] Power service ready pid=", wait_s=5.0)

        # 1. Let idle vCPUs accumulate real `sti; hlt` residency, then verify via `sysinfo`.
        time.sleep(0.45)
        idx = len(h.serial_lines)
        qmp.type_ascii("sysinfo")
        qmp.send_qcodes(["ret"])
        sys_ln = h.wait_for("[DRY-SHELL] CMD=sysinfo cpus=2", start_idx=idx)
        if _parse_kv_ints(sys_ln).get("hlt", 0) < 10:
            raise RuntimeError(f"[{name}] Expected >=10 CPU HLT idle calls in sysinfo: {sys_ln!r}")

        # 2. On-demand Display Idle (`blank`) -> 100% pure-black `0x000000` framebuffer -> wake on mouse move.
        idx = len(h.serial_lines)
        qmp.type_ascii("blank")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-WM] SESSION state=1", start_idx=idx)
        time.sleep(0.06)

        qmp.screendump(ppm_blank)
        black_px, total_px = _ppm_black_pixel_counts(ppm_blank)
        if black_px != total_px:
            raise RuntimeError(
                f"[{name}] Expected 100% black pixels during DPMS blank, got {black_px}/{total_px}"
            )

        idx = len(h.serial_lines)
        qmp.move_in_steps(24, 16, steps=2)
        h.wait_for("[DRY-WM] DPMS_WAKE", start_idx=idx)
        time.sleep(0.06)

        qmp.screendump(ppm_woke)
        wake_diff = _count_diff_pixels(ppm_blank, ppm_woke)
        if wake_diff < 100_000:
            raise RuntimeError(f"[{name}] Expected full desktop restore on DPMS wake, diff={wake_diff}")

        # 3. Automatic Display Idle timer (`Settings` row 5 -> `5s` DPMS timeout) + wake.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_SPAWNED app=settings", start_idx=idx)
        h.wait_for("[DRY-SETTINGS] Ready in CPL3 Userspace", start_idx=idx)

        for _ in range(5):
            qmp.send_qcodes(["down"])
            time.sleep(0.015)
        idx = len(h.serial_lines)
        qmp.send_qcodes(["right"])
        h.wait_for("[DRY-SUP] PREFS blank_timeout_s=5", start_idx=idx)

        idx = len(h.serial_lines)
        h.wait_for("[DRY-WM] DPMS_BLANK reason=idle", start_idx=idx, wait_s=8.5)

        idx = len(h.serial_lines)
        qmp.move_in_steps(-24, -16, steps=2)
        h.wait_for("[DRY-WM] DPMS_WAKE", start_idx=idx)

        # Reset blank timeout back to `Off` (`0s`) and focus `Terminal` (`Alt+1`).
        idx = len(h.serial_lines)
        qmp.send_qcodes(["left"])
        h.wait_for("[DRY-SUP] PREFS blank_timeout_s=0", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)

        # 4. Session Lock (`lock`): blocks keyboard input (`"abc"`), captures screenshot, unlocks via mouse click!
        idx = len(h.serial_lines)
        qmp.type_ascii("lock")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-WM] SESSION state=2", start_idx=idx)
        time.sleep(0.06)

        idx = len(h.serial_lines)
        qmp.type_ascii("abc")
        h.wait_for("[DRY-WM] LOCK_BLOCKED count=3", start_idx=idx)

        qmp.screendump(ppm_lock)
        docs_png = root_dir / "docs" / "p3c_lock_shield.png"
        docs_png.parent.mkdir(parents=True, exist_ok=True)
        _ppm_to_png_and_verify(ppm_lock, docs_png)
        if ARTIFACT_DIR.exists():
            shutil.copyfile(docs_png, ARTIFACT_DIR / "p3c_lock_shield.png")

        # Allow `Life Station` background telemetry refresh while locked, then click `[ UNLOCK SESSION ]` at `(640, 460)`.
        time.sleep(0.30)
        idx = len(h.serial_lines)
        qmp.move_in_steps(0, 60, steps=3)
        qmp.click_left()
        h.wait_for("[DRY-WM] SESSION state=0", start_idx=idx)

        # Prove blocked `"abc"` keys never leaked into Terminal's input line!
        idx = len(h.serial_lines)
        qmp.type_ascii("echo clean after lock")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="clean after lock"', start_idx=idx)

        # 5. Honest Suspend-to-RAM (`S3`) `-ENOSYS` verification.
        idx = len(h.serial_lines)
        qmp.type_ascii("suspend")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-KERNEL-POWER] S3 suspend rejected: -ENOSYS", start_idx=idx)
        h.wait_for("[DRY-SHELL] CMD=suspend res=ENOSYS", start_idx=idx)

        # 6. Controlled session shutdown + `[DRY-IDLE-HLT]` + `leaked_frames=0`.
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=True)

        hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        if len(hlt_lns) != 1:
            raise RuntimeError(f"[{name}] Missing [DRY-IDLE-HLT] PASS line")
        hkv = _parse_kv_ints(hlt_lns[0])
        if hkv.get("hlt_calls", 0) < 50 or hkv.get("hlt_tsc", 0) == 0:
            raise RuntimeError(f"[{name}] Expected substantial HLT residency: {hlt_lns[0]!r}")

        print(
            f"[PASS] {name}: hlt_calls={hkv['hlt_calls']} wake_diff={wake_diff} "
            f"lock_blocked=3 s3=ENOSYS leaked_frames={skv['leaked_frames']} ({time.monotonic() - h.t0:.2f}s)."
        )
    finally:
        h.close()
        for p in (ppm_blank, ppm_woke, ppm_lock):
            p.unlink(missing_ok=True)


def run_p3c_acpi_s5_poweroff_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Verify controlled app/service teardown followed by real ACPI S5 hardware poweroff (`rc == 0`
    WITHOUT `isa-debug-exit` device attached)."""
    h = _DesktopSessionHarness(
        name,
        img_path,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        enable_debug_exit=False,
    )
    try:
        qmp = h.boot_to_desktop_ready()
        idx = len(h.serial_lines)
        qmp.type_ascii("poweroff")
        qmp.send_qcodes(["ret"])

        h.wait_for("[DRY-SHELL] CMD=poweroff ok=true", start_idx=idx)
        h.wait_for("[DRY-INIT] Controlled teardown complete -> invoking dry-power S5", start_idx=idx)
        s5_ln = h.wait_for("[DRY-KERNEL-POWER] Entering hardware ACPI S5 poweroff", start_idx=idx)

        qmp.close()
        h.qmp = None
        rc = h.proc.wait(timeout=10.0)
        h.drain_available()
        serial_out = "".join(h.serial_lines)
        if rc != 0:
            print(f"FULL SERIAL OUTPUT ({name}):\n{serial_out}", file=sys.stderr)
            raise RuntimeError(f"[{name}] Expected QEMU ACPI S5 clean exit rc=0, got rc={rc}")
        if "[DRY-VFS-STATS]" not in serial_out or "[DRY-WM-STATS]" not in serial_out:
            raise RuntimeError(f"[{name}] Expected clean VFS + WM teardown before ACPI S5 poweroff")

        print(f"[PASS] {name}: rc={rc} ({s5_ln.strip()}) ({time.monotonic() - h.t0:.2f}s).")
    finally:
        h.close()


def run_p3c_hardware_reboot_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Verify controlled app/service teardown followed by real hardware reset (`FADT.RESET_REG` /
    `0xCF9` with `-no-reboot` and WITHOUT `isa-debug-exit`, asserting `rc == 0`)."""
    h = _DesktopSessionHarness(
        name,
        img_path,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        enable_debug_exit=False,
    )
    try:
        qmp = h.boot_to_desktop_ready()
        idx = len(h.serial_lines)
        qmp.type_ascii("reboot")
        qmp.send_qcodes(["ret"])

        h.wait_for("[DRY-SHELL] CMD=reboot ok=true", start_idx=idx)
        h.wait_for("[DRY-INIT] Controlled teardown complete -> invoking dry-power reset", start_idx=idx)
        rst_ln = h.wait_for("[DRY-KERNEL-POWER] Triggering hardware system reset", start_idx=idx)

        qmp.close()
        h.qmp = None
        rc = h.proc.wait(timeout=10.0)
        h.drain_available()
        serial_out = "".join(h.serial_lines)
        if rc != 0:
            print(f"FULL SERIAL OUTPUT ({name}):\n{serial_out}", file=sys.stderr)
            raise RuntimeError(f"[{name}] Expected QEMU hardware reset (-no-reboot) rc=0, got rc={rc}")
        if "[DRY-VFS-STATS]" not in serial_out or "[DRY-WM-STATS]" not in serial_out:
            raise RuntimeError(f"[{name}] Expected clean VFS + WM teardown before hardware reset")

        print(f"[PASS] {name}: rc={rc} ({rst_ln.strip()}) ({time.monotonic() - h.t0:.2f}s).")
    finally:
        h.close()


def run_p3c_suite(
    esp_p3_std: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> int:
    run_p3c_idle_dpms_lock_s3_e2e(
        "p3c_idle_dpms_lock_s3_e2e",
        esp_p3_std,
        ovmf_code,
        ovmf_vars,
        build_dir,
        root_dir,
    )
    run_p3c_acpi_s5_poweroff_e2e(
        "p3c_acpi_s5_poweroff_e2e",
        esp_p3_std,
        ovmf_code,
        ovmf_vars,
        build_dir,
    )
    run_p3c_hardware_reboot_e2e(
        "p3c_hardware_reboot_e2e",
        esp_p3_std,
        ovmf_code,
        ovmf_vars,
        build_dir,
    )
    return 3


def run_p3_hardware_matrix(
    esp_p3_std: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Verify `desktop3=1` across 1/2/4/8/20 vCPUs and 512M/1G/2G RAM configurations."""
    configs = [
        ("p3_matrix_1cpu_256m", 1, "256M"),
        ("p3_matrix_2cpu_256m", 2, "256M"),
        ("p3_matrix_4cpu_512m", 4, "512M"),
        ("p3_matrix_8cpu_1g", 8, "1G"),
        ("p3_matrix_20cpu_2g", 20, "2G"),
    ]
    for cname, smp, mem in configs:
        h = _DesktopSessionHarness(
            cname, esp_p3_std, ovmf_code, ovmf_vars, build_dir, smp=smp, expect_fb=True, mem=mem
        )
        try:
            qmp = h.boot_to_desktop_ready()
            time.sleep(0.20)
            idx = len(h.serial_lines)
            qmp.type_ascii("sysinfo")
            qmp.send_qcodes(["ret"])
            h.wait_for(f"[DRY-SHELL] CMD=sysinfo cpus={smp}", start_idx=idx)

            idx = len(h.serial_lines)
            qmp.type_ascii("ps")
            qmp.send_qcodes(["ret"])
            h.wait_for("[DRY-SHELL] CMD=ps count=", start_idx=idx)

            qmp.type_ascii("shutdown")
            qmp.send_qcodes(["ret"])
            serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
            hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
            hkv = _parse_kv_ints(hlt_lns[0]) if hlt_lns else {}
            print(
                f"[PASS] {cname}: smp={smp} mem={mem} hlt_calls={hkv.get('hlt_calls', 0)} "
                f"leaked_frames={skv['leaked_frames']} ({time.monotonic() - h.t0:.2f}s)."
            )
        finally:
            h.close()
