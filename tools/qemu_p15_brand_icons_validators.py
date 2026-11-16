#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""DRY OS Visual Identity, Prism Brand Lockup, 21-App Icon Showcase & 200% DPI E2E Suite."""

from __future__ import annotations

import json
import os
import pathlib
import sys
import time
from typing import Callable

TOOLS_DIR = pathlib.Path(__file__).resolve().parent
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

from qemu_ipc_validators import capture_and_verify_gop_screendump
from qemu_p10_glass2_validators import (
    _DesktopSessionHarness,
    _capture_gpu0_png,
    _create_raw_disk,
    _virtio_blk_args,
    ensure_qemu_virtio_gpu_modules,
)


def _boot_gpu_desktop(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    disk_path: pathlib.Path,
    res_w: int,
    res_h: int,
):
    _create_raw_disk(disk_path, size_mib=4)
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
        + ["-device", f"virtio-gpu-pci,id=gpu0,disable-legacy=on,xres={res_w},yres={res_h}"],
    )
    qmp = h.boot_to_desktop_ready()
    h.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1)", wait_s=8.0)
    h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
    h.wait_for(f"[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w={res_w} h={res_h}", wait_s=12.0)
    h.wait_for("[DRY-SYSMON] REFRESH seq=2", wait_s=10.0)
    return h, qmp


def _shutdown_verify(name: str, h: _DesktopSessionHarness, qmp) -> int:
    idx = len(h.serial_lines)
    qmp.send_qcodes(["alt", "1"])
    h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
    time.sleep(0.08)
    qmp.type_ascii("shutdown")
    qmp.send_qcodes(["ret"])
    _, skv = h.finish_and_verify(require_mouse_irqs=False)
    leaked = skv.get("leaked_frames", -1)
    if leaked != 0:
        raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")
    return leaked


def run_p15a_1280x800_brand_themes_dpi(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p15a.img"
    ppm_path = build_dir / f"{name}_gpu0.ppm"
    t0 = time.monotonic()
    h, qmp = _boot_gpu_desktop(name, esp_img, ovmf_code, ovmf_vars, build_dir, disk_path, 1280, 800)
    try:
        shots: dict[str, dict[str, int]] = {}

        def snap(fname: str, min_colors: int = 16) -> None:
            px, col = _capture_gpu0_png(
                name, qmp, ppm_path, shots_dir / fname, 1280, 800, min_colors=min_colors
            )
            shots[fname] = {"non_bg_px": px, "colors": col}

        time.sleep(0.15)
        snap("p15_1280x800_dark_desktop.png", min_colors=24)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "spc"])
        h.wait_for("[DRY-GLASS] LAUNCHER open=1", start_idx=idx)
        time.sleep(0.12)
        snap("p15_1280x800_launcher.png", min_colors=24)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] LAUNCHER open=0", start_idx=idx)

        for hotkey, marker in [
            (["alt", "3"], "[DRY-FILES] Ready in CPL3 Userspace"),
            (["alt", "5"], "[DRY-EDITOR] Ready in CPL3 Userspace"),
            (["alt", "6"], "[DRY-CALC] Ready in CPL3 Userspace"),
            (["alt", "4"], "[DRY-SETTINGS] Ready in CPL3 Userspace"),
        ]:
            idx = len(h.serial_lines)
            qmp.send_qcodes(hotkey)
            h.wait_for(marker, start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "3"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=files", start_idx=idx)
        time.sleep(0.15)
        snap("p15_1280x800_files.png", min_colors=20)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=settings", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("v")
        h.wait_for("[DRY-SETTINGS] VIEW showcase=1", start_idx=idx)
        time.sleep(0.20)
        snap("p15_1280x800_gallery_dark.png", min_colors=24)

        idx = len(h.serial_lines)
        qmp.type_ascii("t")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1", start_idx=idx)
        time.sleep(0.20)
        snap("p15_1280x800_gallery_light.png", min_colors=20)

        idx = len(h.serial_lines)
        qmp.type_ascii("t")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=0", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.type_ascii("4")
        h.wait_for("[DRY-WM] PREFS contrast=1 cur_large=0 light=0", start_idx=idx)
        time.sleep(0.20)
        snap("p15_1280x800_gallery_hc.png", min_colors=8)

        idx = len(h.serial_lines)
        qmp.type_ascii("4")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=0", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("9")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=200", start_idx=idx)
        time.sleep(0.20)
        snap("p15_1280x800_gallery_200dpi.png", min_colors=24)

        idx = len(h.serial_lines)
        qmp.type_ascii("v")
        h.wait_for("[DRY-SETTINGS] VIEW showcase=0", start_idx=idx)
        time.sleep(0.18)
        snap("p15_1280x800_settings_200dpi.png", min_colors=16)

        idx = len(h.serial_lines)
        qmp.type_ascii("9")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=100", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "w"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=1", start_idx=idx)
        time.sleep(0.20)
        snap("p15_1280x800_overview.png", min_colors=24)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=0", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.08)
        idx = len(h.serial_lines)
        qmp.type_ascii("lock")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-WM] SESSION_LOCK locked=1", start_idx=idx)
        time.sleep(0.20)
        snap("p15_1280x800_lock_screen.png", min_colors=20)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-WM] SESSION_LOCK locked=0", start_idx=idx)

        leaked = _shutdown_verify(name, h, qmp)
        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: themes + 200% DPI + gallery + overview + lock ({elapsed:.2f}s).")
        return {"resolution": "1280x800", "leaked_frames": leaked, "screenshots": shots, "wall_time_sec": elapsed}
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p15b_1024x768_125_150dpi(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p15b.img"
    ppm_path = build_dir / f"{name}_gpu0.ppm"
    t0 = time.monotonic()
    h, qmp = _boot_gpu_desktop(name, esp_img, ovmf_code, ovmf_vars, build_dir, disk_path, 1024, 768)
    try:
        shots: dict[str, dict[str, int]] = {}
        for hotkey, marker in [
            (["alt", "3"], "[DRY-FILES] Ready in CPL3 Userspace"),
            (["alt", "4"], "[DRY-SETTINGS] Ready in CPL3 Userspace"),
        ]:
            idx = len(h.serial_lines)
            qmp.send_qcodes(hotkey)
            h.wait_for(marker, start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("v")
        h.wait_for("[DRY-SETTINGS] VIEW showcase=1", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=125", start_idx=idx)
        time.sleep(0.20)
        px1, col1 = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "p15_1024x768_tiled_gallery_125dpi.png", 1024, 768, min_colors=20
        )
        shots["p15_1024x768_tiled_gallery_125dpi.png"] = {"non_bg_px": px1, "colors": col1}

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=150", start_idx=idx)
        time.sleep(0.20)
        px2, col2 = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "p15_1024x768_tiled_gallery_150dpi.png", 1024, 768, min_colors=20
        )
        shots["p15_1024x768_tiled_gallery_150dpi.png"] = {"non_bg_px": px2, "colors": col2}

        leaked = _shutdown_verify(name, h, qmp)
        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1024x768 tiled gallery 125%+150% DPI ({elapsed:.2f}s).")
        return {"resolution": "1024x768", "leaked_frames": leaked, "screenshots": shots, "wall_time_sec": elapsed}
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p15c_1920x1080_showcase(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p15c.img"
    ppm_path = build_dir / f"{name}_gpu0.ppm"
    t0 = time.monotonic()
    h, qmp = _boot_gpu_desktop(name, esp_img, ovmf_code, ovmf_vars, build_dir, disk_path, 1920, 1080)
    try:
        shots: dict[str, dict[str, int]] = {}
        for hotkey, marker in [
            (["alt", "3"], "[DRY-FILES] Ready in CPL3 Userspace"),
            (["alt", "5"], "[DRY-EDITOR] Ready in CPL3 Userspace"),
            (["alt", "4"], "[DRY-SETTINGS] Ready in CPL3 Userspace"),
        ]:
            idx = len(h.serial_lines)
            qmp.send_qcodes(hotkey)
            h.wait_for(marker, start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.type_ascii("v")
        h.wait_for("[DRY-SETTINGS] VIEW showcase=1", start_idx=idx)
        time.sleep(0.25)
        px1, col1 = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "p15_1920x1080_tiled_gallery.png", 1920, 1080, min_colors=24
        )
        shots["p15_1920x1080_tiled_gallery.png"] = {"non_bg_px": px1, "colors": col1}

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "w"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=1", start_idx=idx)
        time.sleep(0.20)
        px2, col2 = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "p15_1920x1080_overview.png", 1920, 1080, min_colors=24
        )
        shots["p15_1920x1080_overview.png"] = {"non_bg_px": px2, "colors": col2}

        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=0", start_idx=idx)

        leaked = _shutdown_verify(name, h, qmp)
        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1920x1080 tiled gallery + overview ({elapsed:.2f}s).")
        return {"resolution": "1920x1080", "leaked_frames": leaked, "screenshots": shots, "wall_time_sec": elapsed}
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p15_brand_icons_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> int:
    os.environ["QEMU_MODULE_DIR"] = str(ensure_qemu_virtio_gpu_modules())
    shots_dir = root_dir / "docs" / "screenshots" / "brand_icons"
    shots_dir.mkdir(parents=True, exist_ok=True)

    esp_banner = build_dir / "esp_p15_banner.img"
    esp_1280 = build_dir / "esp_p15_1280.img"
    esp_1024 = build_dir / "esp_p15_1024.img"
    esp_1080 = build_dir / "esp_p15_1080.img"
    write_mbr_esp_image(esp_banner, kernel_elf, "console=com1 screendump_hold=1")
    write_mbr_esp_image(esp_1280, kernel_elf, "console=com1 desktop4=1", resolution="1280x800x32")
    write_mbr_esp_image(esp_1024, kernel_elf, "console=com1 desktop4=1", resolution="1024x768x32")
    write_mbr_esp_image(esp_1080, kernel_elf, "console=com1 desktop4=1", resolution="1920x1080x32")

    capture_and_verify_gop_screendump(
        img_path=esp_banner,
        ovmf_code=ovmf_code,
        ovmf_vars=ovmf_vars,
        build_dir=build_dir,
        png_out=shots_dir / "p15_boot_banner.png",
    )
    res_a = run_p15a_1280x800_brand_themes_dpi(
        "p15a_1280x800_brand_themes_dpi", esp_1280, ovmf_code, ovmf_vars, build_dir, shots_dir
    )
    res_b = run_p15b_1024x768_125_150dpi(
        "p15b_1024x768_125_150dpi", esp_1024, ovmf_code, ovmf_vars, build_dir, shots_dir
    )
    res_c = run_p15c_1920x1080_showcase(
        "p15c_1920x1080_showcase", esp_1080, ovmf_code, ovmf_vars, build_dir, shots_dir
    )

    out_path = root_dir / "tools" / "compat_bench" / "results_p15_brand_icons.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    summary = {
        "milestone": "DRY-Brand-Icons-P15",
        "p15a": res_a,
        "p15b": res_b,
        "p15c": res_c,
    }
    out_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return 4


def main() -> int:
    import qemu_boot_test as qbt

    qbt.BUILD_DIR.mkdir(parents=True, exist_ok=True)
    kernel_elf = qbt.build_kernel()
    n = run_p15_brand_icons_suite(
        kernel_elf, qbt.BUILD_DIR, qbt.ROOT, qbt.OVMF_CODE, qbt.OVMF_VARS, qbt.write_mbr_esp_image
    )
    print(f"\n[QEMU-TEST] ALL {n} BRAND + ICONS VISUAL IDENTITY CHECKS PASSED.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
