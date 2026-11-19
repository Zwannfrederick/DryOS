#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""DRY Glass Desktop, SharedPixelSurface, 6 Native Apps & Multi-Res VirtIO-GPU E2E Suite (`--p9-only`)."""

import json
import os
import pathlib
import time
from typing import Callable

from bootstrap_qemu_gpu_modules import ensure_qemu_virtio_gpu_modules
from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints
from qemu_p2_validators import _DesktopQmpClient, _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args


def _capture_gpu0_png(
    name: str,
    qmp: _DesktopQmpClient,
    ppm_path: pathlib.Path,
    png_path: pathlib.Path,
    expect_w: int,
    expect_h: int,
    min_colors: int = 24,
) -> tuple[int, int]:
    ppm_path.unlink(missing_ok=True)
    qmp.cmd("screendump", {"filename": str(ppm_path), "device": "gpu0", "head": 0})
    shot_w, shot_h, non_bg_px, distinct_colors = _ppm_to_png_and_verify(ppm_path, png_path)
    ppm_path.unlink(missing_ok=True)
    if (shot_w, shot_h) != (expect_w, expect_h):
        raise RuntimeError(
            f"[{name}] Unexpected gpu0 screendump geometry {(shot_w, shot_h)!r}, expected {(expect_w, expect_h)!r}"
        )
    if distinct_colors < min_colors or non_bg_px < 10_000:
        raise RuntimeError(
            f"[{name}] Low visual diversity in {png_path.name}: colors={distinct_colors} non_bg={non_bg_px}"
        )
    return non_bg_px, distinct_colors


def _move_cursor_to(qmp: _DesktopQmpClient, cur_pos: list[int], tx: int, ty: int) -> None:
    dx = tx - cur_pos[0]
    dy = ty - cur_pos[1]
    if dx != 0 or dy != 0:
        qmp.move_in_steps(dx, dy, steps=8, step_s=0.010)
        cur_pos[0] = tx
        cur_pos[1] = ty
    time.sleep(0.05)


def run_p9a_glass_1280x800(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p9a.img"
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
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path)
        + ["-device", "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800"],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1280 h=800", wait_s=12.0)
        cur_pos = [640, 400]
        time.sleep(0.15)

        shots: dict[str, dict[str, int]] = {}
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1280x800_dark_home.png", 1280, 800
        )
        shots["glass_1280x800_dark_home.png"] = {"non_bg_px": px, "colors": col}

        # 1. Open Searchable Launcher (`Alt+Space`), filter `calc`, launch Calculator.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "spc"])
        h.wait_for("[DRY-GLASS] LAUNCHER open=1", start_idx=idx)
        time.sleep(0.10)
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1280x800_launcher.png", 1280, 800
        )
        shots["glass_1280x800_launcher.png"] = {"non_bg_px": px, "colors": col}

        idx = len(h.serial_lines)
        qmp.type_ascii("calc")
        time.sleep(0.06)
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-GLASS] LAUNCHER_EXEC app=calc", start_idx=idx)
        h.wait_for("[DRY-CALC] Ready in CPL3 Userspace", start_idx=idx)

        # Evaluate `6 * 7 = 42` in Calculator.
        idx = len(h.serial_lines)
        qmp.type_ascii("6")
        qmp.send_qcodes(["shift", "8"])
        qmp.type_ascii("7")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-CALC] EVAL op=* res=42", start_idx=idx)
        time.sleep(0.10)
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1280x800_calc.png", 1280, 800
        )
        shots["glass_1280x800_calc.png"] = {"non_bg_px": px, "colors": col}

        # 2. Launch Editor (`Alt+5`), type note, save via `Ctrl+S` to `/persist/notes.txt`.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "5"])
        h.wait_for("[DRY-EDITOR] Ready in CPL3 Userspace", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("dry glass persist ok")
        time.sleep(0.06)
        qmp.send_qcodes(["ctrl", "s"])
        saved_ln = h.wait_for("[DRY-EDITOR] SAVED path=/persist/notes.txt", start_idx=idx)
        if _parse_kv_ints(saved_ln).get("bytes", 0) < 20:
            raise RuntimeError(f"[{name}] Unexpected Editor save byte count: {saved_ln!r}")
        time.sleep(0.10)
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1280x800_editor.png", 1280, 800
        )
        shots["glass_1280x800_editor.png"] = {"non_bg_px": px, "colors": col}

        # 3. Open Quick Settings (`(1224, 14)`), capture, toggle Frosted Daylight (`(1108, 94)`).
        idx = len(h.serial_lines)
        _move_cursor_to(qmp, cur_pos, 1224, 14)
        qmp.click_left()
        h.wait_for("[DRY-GLASS] QS open=1", start_idx=idx)
        time.sleep(0.10)
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1280x800_quick_settings.png", 1280, 800
        )
        shots["glass_1280x800_quick_settings.png"] = {"non_bg_px": px, "colors": col}

        _move_cursor_to(qmp, cur_pos, 1108, 94)
        qmp.click_left()
        time.sleep(0.08)
        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] QS open=0", start_idx=idx)
        time.sleep(0.45)
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1280x800_frosted_daylight.png", 1280, 800
        )
        shots["glass_1280x800_frosted_daylight.png"] = {"non_bg_px": px, "colors": col}

        # 4. Open Notification Center (`(1135, 14)`), capture, close.
        idx = len(h.serial_lines)
        _move_cursor_to(qmp, cur_pos, 1135, 14)
        qmp.click_left()
        h.wait_for("[DRY-GLASS] NOTIF open=1", start_idx=idx)
        time.sleep(0.10)
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1280x800_notifications.png", 1280, 800
        )
        shots["glass_1280x800_notifications.png"] = {"non_bg_px": px, "colors": col}

        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] NOTIF open=0", start_idx=idx)

        # 5. Focus Terminal (`Alt+1`), verify 4 running apps and persistent `/persist/notes.txt`, shutdown.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("apps")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=apps running=4", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("cat /persist/notes.txt")
        qmp.send_qcodes(["ret"])
        cat_ln = h.wait_for("[DRY-SHELL] CMD=cat path=/persist/notes.txt", start_idx=idx)
        saved_bytes = _parse_kv_ints(saved_ln).get("bytes", 0)
        cat_bytes = _parse_kv_ints(cat_ln).get("bytes", -1)
        if cat_bytes != saved_bytes or saved_bytes < 110:
            raise RuntimeError(f"[{name}] /persist/notes.txt size mismatch: saved={saved_ln!r} cat={cat_ln!r}")

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=True)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1280x800 4-app Frosted Glass + Daylight + /persist/notes.txt ({elapsed:.2f}s).")
        return {
            "resolution": "1280x800",
            "back_2m": 2,
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


def run_p9b_glass_1920x1080(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p9b.img"
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
        h.wait_for("[DRY-DISPLAY] Backing buffer audit: 1920x1080", wait_s=10.0)
        h.wait_for("[DRY-GPU] INIT dev=0x1af4:0x1050 scanout=1920x1080 fmt=B8G8R8X8 dma_2m=4", wait_s=12.0)
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
        time.sleep(0.15)

        shots: dict[str, dict[str, int]] = {}
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1920x1080_tiled_gpu.png", 1920, 1080
        )
        shots["glass_1920x1080_tiled_gpu.png"] = {"non_bg_px": px, "colors": col}

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
        time.sleep(0.15)

        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1920x1080_recovered_gen2.png", 1920, 1080
        )
        shots["glass_1920x1080_recovered_gen2.png"] = {"non_bg_px": px, "colors": col}

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")
        hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        if not hlt_lns or _parse_kv_ints(hlt_lns[0]).get("pci_quiesce", 0) < 1:
            raise RuntimeError(f"[{name}] Expected pci_quiesce >= 1 in {hlt_lns!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1920x1080 4-app tiled + live #UD GPU recovery gen=2 ({elapsed:.2f}s).")
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


def run_p9c_glass_1024x768(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
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
        mem="384M",
        extra_qemu_args=["-device", "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1024,yres=768"],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-DISPLAY] Backing buffer audit: 1024x768", wait_s=10.0)
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1024 h=768", wait_s=12.0)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "6"])
        h.wait_for("[DRY-CALC] Ready in CPL3 Userspace", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.15)

        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass_1024x768_compact.png", 1024, 768
        )

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1024x768 compact DRY Glass topbar + Dock + Calculator ({elapsed:.2f}s).")
        return {
            "resolution": "1024x768",
            "back_2m": 2,
            "leaked_frames": skv["leaked_frames"],
            "screenshots": {"glass_1024x768_compact.png": {"non_bg_px": px, "colors": col}},
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        ppm_path.unlink(missing_ok=True)


def run_p9_glass_suite(
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

    esp_1280 = build_dir / "esp_p9_1280.img"
    esp_1080 = build_dir / "esp_p9_1080.img"
    esp_1024 = build_dir / "esp_p9_1024.img"
    write_mbr_esp_image(esp_1280, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1280x800x32")
    write_mbr_esp_image(esp_1080, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1920x1080x32")
    write_mbr_esp_image(esp_1024, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1024x768x32")

    res_a = run_p9a_glass_1280x800(
        "p9a_glass_1280x800_apps_and_themes", esp_1280, ovmf_code, ovmf_vars, build_dir, shots_dir
    )
    res_b = run_p9b_glass_1920x1080(
        "p9b_glass_1920x1080_gpu_fault_recovery", esp_1080, ovmf_code, ovmf_vars, build_dir, shots_dir
    )
    res_c = run_p9c_glass_1024x768(
        "p9c_glass_1024x768_compact", esp_1024, ovmf_code, ovmf_vars, build_dir, shots_dir
    )

    out_path = root_dir / "tools" / "compat_bench" / "results_p9_glass.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    summary = {
        "milestone": "Ascension-3.0-Track-B",
        "apps_total": 6,
        "p9a": res_a,
        "p9b": res_b,
        "p9c": res_c,
    }
    out_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return 3
