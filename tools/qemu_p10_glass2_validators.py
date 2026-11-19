#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""DRY Glass 2.0 TrueType AA, UI Toolkit, Multi-DPI & 6-App Multi-Res E2E Suite (`--p10-only`)."""

import json
import os
import pathlib
import time
from typing import Callable

from bootstrap_qemu_gpu_modules import ensure_qemu_virtio_gpu_modules
from qemu_log_validators import _parse_kv_ints
from qemu_p2_validators import _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args
from qemu_p9_glass_validators import _capture_gpu0_png, _move_cursor_to


def run_p10a_glass2_1280x800_showcase(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p10a.img"
    _create_raw_disk(disk_path, size_mib=4)
    ppm_path = build_dir / f"{name}_gpu0.ppm"
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="512M",
        extra_qemu_args=_virtio_blk_args(disk_path)
        + ["-device", "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800"],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1)", wait_s=8.0)
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1280 h=800", wait_s=12.0)
        h.wait_for("[DRY-SYSMON] REFRESH seq=2", wait_s=10.0)
        cur_pos = [640, 400]
        time.sleep(0.15)

        shots: dict[str, dict[str, int]] = {}

        def snap(fname: str, min_colors: int = 24) -> None:
            px, col = _capture_gpu0_png(
                name, qmp, ppm_path, shots_dir / fname, 1280, 800, min_colors=min_colors
            )
            shots[fname] = {"non_bg_px": px, "colors": col}

        # 1. Default Obsidian Dark desktop (Terminal + Life Station).
        snap("glass2_1280x800_dark_desktop.png")

        # 2. Frosted App Launcher (`Alt+Space`), filter `calc`, launch & compute `6 * 7 = 42`.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "spc"])
        h.wait_for("[DRY-GLASS] LAUNCHER open=1", start_idx=idx)
        time.sleep(0.10)
        snap("glass2_1280x800_launcher.png")

        idx = len(h.serial_lines)
        qmp.type_ascii("calc")
        time.sleep(0.06)
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-GLASS] LAUNCHER_EXEC app=calc", start_idx=idx)
        h.wait_for("[DRY-CALC] Ready in CPL3 Userspace", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("6")
        qmp.send_qcodes(["shift", "8"])
        qmp.type_ascii("7")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-CALC] EVAL op=* res=42", start_idx=idx)

        # 3. Launch Editor (`Alt+5`), save scratchpad note, switch to Tiled mode (`Alt+T`).
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "5"])
        h.wait_for("[DRY-EDITOR] Ready in CPL3 Userspace", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("dry glass 2.0 truetype")
        time.sleep(0.06)
        qmp.send_qcodes(["ctrl", "s"])
        saved_ln = h.wait_for("[DRY-EDITOR] SAVED path=/persist/notes.txt", start_idx=idx)
        if _parse_kv_ints(saved_ln).get("bytes", 0) < 20:
            raise RuntimeError(f"[{name}] Unexpected Editor save byte count: {saved_ln!r}")

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)
        time.sleep(0.25)
        snap("glass2_1280x800_calc_editor.png")

        # Close Editor (`Esc`) & Calculator (`Alt+6` -> `'q'`).
        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-WM] WIN_CLOSED", start_idx=idx)
        h.wait_for("[DRY-SUP] APP_REAPED app=editor", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "6"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=calc", start_idx=idx)
        time.sleep(0.06)
        idx = len(h.serial_lines)
        qmp.type_ascii("q")
        h.wait_for("[DRY-WM] WIN_CLOSED", start_idx=idx)
        h.wait_for("[DRY-SUP] APP_REAPED app=calc", start_idx=idx)

        # 4. Launch Files (`Alt+3`) & Settings (`Alt+4`) in 4-quadrant tiled workspace.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "3"])
        h.wait_for("[DRY-FILES] Ready in CPL3 Userspace", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"])
        h.wait_for("[DRY-SETTINGS] Ready in CPL3 Userspace", start_idx=idx)
        time.sleep(0.25)
        snap("glass2_1280x800_files_settings.png")

        # 5. Open Quick Settings (`(1224, 14)`), capture, toggle Frosted Daylight (`(1108, 94)`).
        idx = len(h.serial_lines)
        _move_cursor_to(qmp, cur_pos, 1224, 14)
        qmp.click_left()
        h.wait_for("[DRY-GLASS] QS open=1", start_idx=idx)
        time.sleep(0.12)
        snap("glass2_1280x800_quick_settings.png")

        idx = len(h.serial_lines)
        _move_cursor_to(qmp, cur_pos, 1108, 94)
        qmp.click_left()
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1", start_idx=idx)
        time.sleep(0.08)
        idx_close = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] QS open=0", start_idx=idx_close)

        # 6. Focus Settings, verify 125% DPI in Frosted Daylight, then 150% -> 100% round-trip.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=settings", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=125", start_idx=idx)
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1 glass_low=0 dpi=125", start_idx=idx)
        time.sleep(0.35)
        snap("glass2_1280x800_daylight_125dpi.png", min_colors=16)

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=150", start_idx=idx)
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1 glass_low=0 dpi=150", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=100", start_idx=idx)
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1 glass_low=0 dpi=100", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("t")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=0 glass_low=0 dpi=100", start_idx=idx)

        # 7. High Contrast showcase (`'4'`), then restore standard Dark Obsidian theme.
        idx = len(h.serial_lines)
        qmp.type_ascii("4")
        h.wait_for("[DRY-WM] PREFS contrast=1 cur_large=0 light=0 glass_low=0 dpi=100", start_idx=idx)
        time.sleep(0.30)
        snap("glass2_1280x800_high_contrast.png", min_colors=8)

        idx = len(h.serial_lines)
        qmp.type_ascii("4")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=0 glass_low=0 dpi=100", start_idx=idx)
        time.sleep(0.20)

        # 8. Notification Center (`(1135, 14)`), capture, close.
        idx = len(h.serial_lines)
        _move_cursor_to(qmp, cur_pos, 1135, 14)
        qmp.click_left()
        h.wait_for("[DRY-GLASS] NOTIF open=1", start_idx=idx)
        time.sleep(0.12)
        snap("glass2_1280x800_notifications.png")

        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] NOTIF open=0", start_idx=idx)

        # 9. Lock Shield & DRY Vault unlock (`vault-passwd`, `lock`, masked passphrase input, unlock).
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("vault-passwd dryos sovereign7")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=vault-passwd ok=true gen=2", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("lock")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-WM] SESSION state=2", start_idx=idx)

        qmp.type_ascii("sovereign7")
        time.sleep(0.12)
        snap("glass2_1280x800_lock_shield.png", min_colors=12)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SUP] VAULT_UNLOCK auth=ok", start_idx=idx)
        h.wait_for("[DRY-WM] SESSION state=0", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("cat /persist/notes.txt")
        qmp.send_qcodes(["ret"])
        cat_ln = h.wait_for("[DRY-SHELL] CMD=cat path=/persist/notes.txt", start_idx=idx)
        if _parse_kv_ints(cat_ln).get("bytes", -1) != _parse_kv_ints(saved_ln).get("bytes", 0):
            raise RuntimeError(f"[{name}] /persist/notes.txt mismatch: saved={saved_ln!r} cat={cat_ln!r}")

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=True)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(
            f"[PASS] {name}: 1280x800 9-view showcase (Launcher/Calc/Editor/Files/Settings/"
            f"QS/125-150-100 DPI/HC/Notif/Lock) ({elapsed:.2f}s)."
        )
        return {
            "resolution": "1280x800",
            "back_2m": 2,
            "dpi_levels_tested": [100, 125, 150, 100],
            "calc_eval": "6*7=42",
            "leaked_frames": skv["leaked_frames"],
            "irq12": skv.get("irq12", 0),
            "screenshots": shots,
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p10b_glass2_1920x1080_tiled_and_recovery(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p10b.img"
    _create_raw_disk(disk_path, size_mib=4)
    ppm_path = build_dir / f"{name}_gpu0.ppm"
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="512M",
        extra_qemu_args=_virtio_blk_args(disk_path)
        + [
            "-global",
            "VGA.vgamem_mb=32",
            "-device",
            "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1920,yres=1080",
        ],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1)", wait_s=8.0)
        h.wait_for("[DRY-DISPLAY] Backing buffer audit: 1920x1080", wait_s=10.0)
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=4", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1920 h=1080", wait_s=12.0)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "3"])
        h.wait_for("[DRY-FILES] Ready in CPL3 Userspace", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"])
        h.wait_for("[DRY-SETTINGS] Ready in CPL3 Userspace", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)
        time.sleep(0.25)

        shots: dict[str, dict[str, int]] = {}
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass2_1920x1080_tiled_workspace.png", 1920, 1080
        )
        shots["glass2_1920x1080_tiled_workspace.png"] = {"non_bg_px": px, "colors": col}

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("gpufault")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-WM] GPU_FALLBACK_TO_GOP", start_idx=idx)
        h.wait_for("[DRY-GPU] LIVE_RECOVERY_OK gen=2 vec=6 w=1920 h=1080", start_idx=idx)
        h.wait_for("[DRY-SHELL] CMD=gpufault gen=2 vec=6", start_idx=idx)
        time.sleep(0.20)

        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass2_1920x1080_gpu_recovered_gen2.png", 1920, 1080
        )
        shots["glass2_1920x1080_gpu_recovered_gen2.png"] = {"non_bg_px": px, "colors": col}

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")
        hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        if not hlt_lns or _parse_kv_ints(hlt_lns[0]).get("pci_quiesce", 0) < 1:
            raise RuntimeError(f"[{name}] Expected pci_quiesce >= 1 in {hlt_lns!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1920x1080 4-app tiled + #UD GPU recovery gen=2 ({elapsed:.2f}s).")
        return {
            "resolution": "1920x1080",
            "back_2m": 4,
            "gpu_generations": 2,
            "pci_quiesce": _parse_kv_ints(hlt_lns[0]).get("pci_quiesce", 0),
            "leaked_frames": skv["leaked_frames"],
            "screenshots": shots,
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p10c_glass2_1024x768_all_apps(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p10c.img"
    _create_raw_disk(disk_path, size_mib=4)
    ppm_path = build_dir / f"{name}_gpu0.ppm"
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="512M",
        extra_qemu_args=_virtio_blk_args(disk_path)
        + ["-device", "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1024,yres=768"],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1)", wait_s=8.0)
        h.wait_for("[DRY-DISPLAY] Backing buffer audit: 1024x768", wait_s=10.0)
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1024 h=768", wait_s=12.0)

        for hotkey, marker in [
            (["alt", "3"], "[DRY-FILES] Ready in CPL3 Userspace"),
            (["alt", "4"], "[DRY-SETTINGS] Ready in CPL3 Userspace"),
            (["alt", "5"], "[DRY-EDITOR] Ready in CPL3 Userspace"),
            (["alt", "6"], "[DRY-CALC] Ready in CPL3 Userspace"),
        ]:
            idx = len(h.serial_lines)
            qmp.send_qcodes(hotkey)
            h.wait_for(marker, start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("8")
        qmp.send_qcodes(["shift", "8"])
        qmp.type_ascii("9")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-CALC] EVAL op=* res=72", start_idx=idx)

        # Cycle Floating (0) -> Tiled (1) -> Stacked (2) with `Alt+G`.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "g"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "g"])
        h.wait_for("[DRY-WM] LAYOUT mode=2", start_idx=idx)
        time.sleep(0.25)

        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass2_1024x768_all_apps.png", 1024, 768
        )

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("apps")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=apps running=6", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1024x768 6 concurrent apps Stacked + 8*9=72 ({elapsed:.2f}s).")
        return {
            "resolution": "1024x768",
            "back_2m": 2,
            "running_apps": 6,
            "calc_eval": "8*9=72",
            "leaked_frames": skv["leaked_frames"],
            "screenshots": {"glass2_1024x768_all_apps.png": {"non_bg_px": px, "colors": col}},
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p10_glass2_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> int:
    os.environ["QEMU_MODULE_DIR"] = str(ensure_qemu_virtio_gpu_modules())
    shots_dir = root_dir / "docs" / "screenshots"
    shots_dir.mkdir(parents=True, exist_ok=True)

    esp_1280 = build_dir / "esp_p10_1280.img"
    esp_1080 = build_dir / "esp_p10_1080.img"
    esp_1024 = build_dir / "esp_p10_1024.img"
    write_mbr_esp_image(esp_1280, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1280x800x32")
    write_mbr_esp_image(esp_1080, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1920x1080x32")
    write_mbr_esp_image(esp_1024, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1024x768x32")

    res_a = run_p10a_glass2_1280x800_showcase(
        "p10a_glass2_1280x800_showcase", esp_1280, ovmf_code, ovmf_vars, build_dir, shots_dir
    )
    res_b = run_p10b_glass2_1920x1080_tiled_and_recovery(
        "p10b_glass2_1920x1080_tiled_and_recovery",
        esp_1080,
        ovmf_code,
        ovmf_vars,
        build_dir,
        shots_dir,
    )
    res_c = run_p10c_glass2_1024x768_all_apps(
        "p10c_glass2_1024x768_all_apps", esp_1024, ovmf_code, ovmf_vars, build_dir, shots_dir
    )

    out_path = root_dir / "tools" / "compat_bench" / "results_dry_glass_2_0.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    summary = {
        "milestone": "DRY-Glass-2.0",
        "apps_total": 6,
        "screenshots_total": 12,
        "p10a": res_a,
        "p10b": res_b,
        "p10c": res_c,
    }
    out_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return 3
