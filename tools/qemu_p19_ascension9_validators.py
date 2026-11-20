#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Ascension 9.0 Stabilization, VFS Exec, Net/Linux-ABI & Ergonomics Validators (`--p19-only`)."""

from __future__ import annotations

import json
import pathlib
import time
from typing import Any, Callable

from qemu_p2_validators import _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args


def _run_p19a_soak_and_stability(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    disk_path = build_dir / "disk_p19a_soak.img"
    _create_raw_disk(disk_path, size_mib=4)
    esp_img = build_dir / "esp_p19a_soak.img"
    write_mbr_esp_image(
        esp_img,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        "p19a_soak_and_stability",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()

        # 1. Rapid multi-cycle app launch/close soak across apps.
        ready_markers = {
            "editor": "[DRY-EDITOR] Ready in CPL3 Userspace",
            "calc": "[DRY-CALC] Ready in CPL3 Userspace",
            "files": "[DRY-FILES] Ready in CPL3 Userspace",
        }
        app_cycles = 0
        for app in ("editor", "calc", "files", "editor", "calc"):
            idx = len(h.serial_lines)
            qmp.type_ascii(f"open {app}")
            qmp.send_qcodes(["ret"])
            h.wait_for(f"[DRY-SHELL] CMD=open app={app}", start_idx=idx, wait_s=12.0)
            h.wait_for(ready_markers[app], start_idx=idx, wait_s=12.0)

            # `open <app>` focuses the launched app window; Alt+1 returns focus to terminal.
            idx_focus = len(h.serial_lines)
            qmp.send_qcodes(["alt", "1"])
            h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx_focus, wait_s=12.0)

            idx = len(h.serial_lines)
            qmp.type_ascii(f"close {app}")
            qmp.send_qcodes(["ret"])
            h.wait_for(f"[DRY-SHELL] CMD=close app={app}", start_idx=idx, wait_s=12.0)
            app_cycles += 1

        # 2. Rapid /ram + /persist CoW write/sync/cat/rm/overwrite cycles.
        for cmd, marker in (
            ("write /ram/soak1.txt r1", "[DRY-SHELL] CMD=write path=/ram/soak1.txt bytes=2"),
            ("write /persist/soak1.txt p1", "[DRY-SHELL] CMD=write path=/persist/soak1.txt bytes=2"),
            ("write /persist/soak2.txt p2-val", "[DRY-SHELL] CMD=write path=/persist/soak2.txt bytes=6"),
            ("sync", "[DRY-SHELL] CMD=sync tx_seq="),
            ("cat /persist/soak2.txt", '[DRY-SHELL] CMD=cat path=/persist/soak2.txt bytes=6 text="p2-val"'),
            ("rm /ram/soak1.txt", "[DRY-SHELL] CMD=rm path=/ram/soak1.txt ok=true"),
            ("rm /persist/soak2.txt", "[DRY-SHELL] CMD=rm path=/persist/soak2.txt ok=true"),
            ("write /persist/soak1.txt p1-updated", "[DRY-SHELL] CMD=write path=/persist/soak1.txt bytes=10"),
            ("sync", "[DRY-SHELL] CMD=sync tx_seq="),
        ):
            idx = len(h.serial_lines)
            qmp.type_ascii(cmd)
            qmp.send_qcodes(["ret"])
            h.wait_for(marker, start_idx=idx, wait_s=12.0)

        # 3. Repeated input server #UD crash/recovery soak.
        crash_cycles = 0
        for tag in ("after-crash-1", "after-crash-2"):
            idx = len(h.serial_lines)
            qmp.type_ascii("crash-input")
            qmp.send_qcodes(["ret"])
            h.wait_for("[DRY-SHELL] INPUT_RECOVERED ok=true", start_idx=idx, wait_s=15.0)

            idx = len(h.serial_lines)
            qmp.type_ascii(f"echo {tag}")
            qmp.send_qcodes(["ret"])
            h.wait_for(f'[DRY-SHELL] CMD=echo text="{tag}"', start_idx=idx, wait_s=12.0)
            crash_cycles += 1

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if "[DRY-PANIC]" in serial_out or skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p19a] Panic or frame leak detected: {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(
            f"[PASS] p19a_soak_and_stability: app_cycles={app_cycles} crash_cycles={crash_cycles} "
            f"leaked_frames=0 ({elapsed:.2f}s)."
        )
        return {
            "app_cycles": app_cycles,
            "crash_cycles": crash_cycles,
            "leaked_frames": skv["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)


def run_p19_ascension9_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> int:
    print("\n=== Running Ascension 9.0 Stabilization & Maturity Suite (P19) ===")
    res_a = _run_p19a_soak_and_stability(kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)
    out_path = root_dir / "tools" / "compat_bench" / "results_p19_ascension9.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps({"milestone": "Ascension-9.0", "p19a": res_a}, indent=2) + "\n",
        encoding="utf-8",
    )
    return 1
