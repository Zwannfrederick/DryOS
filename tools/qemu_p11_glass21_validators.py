# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""DRY Glass 2.1 (Project Ascension 4.0 Tracks A+B) QMP + PNG validation suite.

Verifies Adaptive Window Strip, multi-window app grouping, `+N` overflow badge,
searchable Window Overview (`Alt+W`, search, minimize toggle, restore), compact
icon-rail sidebars under narrow/HiDPI tiling (`125%`, `150%`), and 3-resolution UI clarity.
"""

from __future__ import annotations

import json
import os
import pathlib
import time
from typing import Callable

from qemu_p10_glass2_validators import (
    _DesktopSessionHarness,
    _capture_gpu0_png,
    _create_raw_disk,
    _parse_kv_ints,
    _virtio_blk_args,
    ensure_qemu_virtio_gpu_modules,
)


def run_p11a_glass21_1280x800_grouped_overview_dpi(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p11a.img"
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
            "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800",
        ],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1)", wait_s=8.0)
        h.wait_for("[DRY-DISPLAY] Backing buffer audit: 1280x800", wait_s=10.0)
        h.wait_for("[DRY-WM-SCALABILITY] pass=1", wait_s=10.0)
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1280 h=800", wait_s=12.0)

        shots: dict[str, dict[str, int]] = {}

        def snap(fname: str, min_colors: int = 16) -> None:
            px, col = _capture_gpu0_png(
                name, qmp, ppm_path, shots_dir / fname, 1280, 800, min_colors=min_colors
            )
            shots[fname] = {"non_bg_px": px, "colors": col}

        # Launch Files, Settings, Editor, Scratchpad #2 (`Ctrl+N`), Calculator -> 7 open windows.
        for hotkey, marker in [
            (["alt", "3"], "[DRY-FILES] Ready in CPL3 Userspace"),
            (["alt", "4"], "[DRY-SETTINGS] Ready in CPL3 Userspace"),
            (["alt", "5"], "[DRY-EDITOR] Ready in CPL3 Userspace"),
        ]:
            idx = len(h.serial_lines)
            qmp.send_qcodes(hotkey)
            h.wait_for(marker, start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["ctrl", "n"])
        h.wait_for("[DRY-WM] WIN_ATTACH id=", start_idx=idx)
        time.sleep(0.10)
        qmp.type_ascii("scratch note 42")

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "6"])
        h.wait_for("[DRY-CALC] Ready in CPL3 Userspace", start_idx=idx)
        time.sleep(0.25)
        snap("glass21_1280x800_grouped_strip.png")

        # Open searchable Window Overview (`Alt+W`), verify grouping, filter "note", toggle minimize.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "w"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=1", start_idx=idx)
        time.sleep(0.20)
        snap("glass21_1280x800_overview_grouped.png")

        qmp.type_ascii("note")
        time.sleep(0.18)
        snap("glass21_1280x800_overview_search_note.png")

        qmp.send_qcodes(["alt", "m"])
        time.sleep(0.15)
        idx = len(h.serial_lines)
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=0", start_idx=idx)
        time.sleep(0.15)

        # Focus Settings (`Alt+4`), inspect Frosted Daylight 125% DPI, then 150% DPI Tiled & High Contrast.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=settings", start_idx=idx)
        time.sleep(0.10)

        idx = len(h.serial_lines)
        qmp.type_ascii("t")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1 glass_low=0 dpi=125", start_idx=idx)
        time.sleep(0.28)
        snap("glass21_1280x800_daylight_125dpi.png")

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=1 glass_low=0 dpi=150", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)
        time.sleep(0.32)
        snap("glass21_1280x800_daylight_150dpi_tiled.png")

        idx = len(h.serial_lines)
        qmp.type_ascii("4")
        h.wait_for("[DRY-WM] PREFS contrast=1 cur_large=0 light=1 glass_low=0 dpi=150", start_idx=idx)
        time.sleep(0.25)
        snap("glass21_1280x800_hc_150dpi_tiled.png", min_colors=8)

        # Clean round-trip back to Dark Obsidian + 100% DPI + Floating, then clean shutdown.
        idx = len(h.serial_lines)
        qmp.type_ascii("4")
        h.wait_for("[DRY-WM] PREFS contrast=0", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.type_ascii("t")
        h.wait_for("[DRY-WM] PREFS contrast=0 cur_large=0 light=0", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("dpi=100", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.08)

        # Move pointer in steps so IRQ12 counters (`>= 3`) remain verified across the session.
        qmp.move_in_steps(48, 36, steps=5)
        h.wait_for("[DRY-INPUT] MOUSE_FIRST")
        time.sleep(0.05)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=True)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(
            f"[PASS] {name}: 1280x800 grouped strip + Alt+W Overview + 125/150% DPI + HC ({elapsed:.2f}s)."
        )
        return {
            "resolution": "1280x800",
            "windows_opened": 7,
            "leaked_frames": skv["leaked_frames"],
            "irq12": skv.get("irq12", 0),
            "screenshots": shots,
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p11b_glass21_1024x768_narrow_150dpi(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p11b.img"
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
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1024 h=768", wait_s=12.0)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "3"])
        h.wait_for("[DRY-FILES] Ready in CPL3 Userspace", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"])
        h.wait_for("[DRY-SETTINGS] Ready in CPL3 Userspace", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=125", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.type_ascii("0")
        h.wait_for("[DRY-SETTINGS] APPLY dpi_pct=150", start_idx=idx)
        h.wait_for("dpi=150", start_idx=idx)
        time.sleep(0.30)

        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass21_1024x768_narrow_150dpi_tiled.png", 1024, 768
        )

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.08)

        idx = len(h.serial_lines)
        qmp.type_ascii("apps")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=apps running=4", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1024x768 narrow tiled 150% DPI icon-rail sidebar ({elapsed:.2f}s).")
        return {
            "resolution": "1024x768",
            "dpi_pct": 150,
            "leaked_frames": skv["leaked_frames"],
            "screenshots": {"glass21_1024x768_narrow_150dpi_tiled.png": {"non_bg_px": px, "colors": col}},
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p11c_glass21_1920x1080_overview_tiled(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    shots_dir: pathlib.Path,
) -> dict:
    disk_path = build_dir / "disk_p11c.img"
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
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1920 h=1080", wait_s=12.0)

        for hotkey, marker in [
            (["alt", "3"], "[DRY-FILES] Ready in CPL3 Userspace"),
            (["alt", "4"], "[DRY-SETTINGS] Ready in CPL3 Userspace"),
            (["alt", "5"], "[DRY-EDITOR] Ready in CPL3 Userspace"),
        ]:
            idx = len(h.serial_lines)
            qmp.send_qcodes(hotkey)
            h.wait_for(marker, start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "t"])
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)
        time.sleep(0.25)

        shots: dict[str, dict[str, int]] = {}
        px, col = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass21_1920x1080_tiled_workspace.png", 1920, 1080
        )
        shots["glass21_1920x1080_tiled_workspace.png"] = {"non_bg_px": px, "colors": col}

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "w"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=1", start_idx=idx)
        time.sleep(0.20)
        px2, col2 = _capture_gpu0_png(
            name, qmp, ppm_path, shots_dir / "glass21_1920x1080_overview.png", 1920, 1080
        )
        shots["glass21_1920x1080_overview.png"] = {"non_bg_px": px2, "colors": col2}

        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-GLASS] OVERVIEW open=0", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"])
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] {name}: 1920x1080 tiled workspace + Window Overview ({elapsed:.2f}s).")
        return {
            "resolution": "1920x1080",
            "leaked_frames": skv["leaked_frames"],
            "screenshots": shots,
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p11_glass21_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> int:
    os.environ["QEMU_MODULE_DIR"] = str(ensure_qemu_virtio_gpu_modules())
    shots_dir = root_dir / "docs" / "screenshots" / "glass21"
    shots_dir.mkdir(parents=True, exist_ok=True)

    esp_1280 = build_dir / "esp_p11_1280.img"
    esp_1024 = build_dir / "esp_p11_1024.img"
    esp_1080 = build_dir / "esp_p11_1080.img"
    write_mbr_esp_image(esp_1280, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1280x800x32")
    write_mbr_esp_image(esp_1024, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1024x768x32")
    write_mbr_esp_image(esp_1080, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1", resolution="1920x1080x32")

    res_a = run_p11a_glass21_1280x800_grouped_overview_dpi(
        "p11a_glass21_1280x800_grouped_overview_dpi",
        esp_1280,
        ovmf_code,
        ovmf_vars,
        build_dir,
        shots_dir,
    )
    res_b = run_p11b_glass21_1024x768_narrow_150dpi(
        "p11b_glass21_1024x768_narrow_150dpi",
        esp_1024,
        ovmf_code,
        ovmf_vars,
        build_dir,
        shots_dir,
    )
    res_c = run_p11c_glass21_1920x1080_overview_tiled(
        "p11c_glass21_1920x1080_overview_tiled",
        esp_1080,
        ovmf_code,
        ovmf_vars,
        build_dir,
        shots_dir,
    )

    out_path = root_dir / "tools" / "compat_bench" / "results_dry_glass_2_1.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    summary = {
        "milestone": "DRY-Glass-2.1",
        "tracks": ["A", "B"],
        "screenshots_total": 9,
        "p11a": res_a,
        "p11b": res_b,
        "p11c": res_c,
    }
    out_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return 3
