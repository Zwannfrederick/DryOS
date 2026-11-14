#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Priority C Ring-3 VirtIO-GPU (`1AF4:1050`) 2D Scanout, GOP Fallback & Crash Recovery Validator (`--gpu-only`)."""

import json
import os
import pathlib
import time
from typing import Callable

from bootstrap_qemu_gpu_modules import ensure_qemu_virtio_gpu_modules
from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints
from qemu_p2_validators import _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args


def _parse_kv_ints_any(line: str) -> dict[str, int]:
    out = _parse_kv_ints(line)
    for tok in line.split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if v.startswith("0x"):
                try:
                    out[k] = int(v, 16)
                except ValueError:
                    pass
    return out


def _virtio_gpu_and_blk_args(disk_path: pathlib.Path) -> list[str]:
    return _virtio_blk_args(disk_path) + [
        "-device",
        "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800",
    ]


def run_p7a_virtio_gpu_e2e(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    mod_dir = ensure_qemu_virtio_gpu_modules()
    os.environ["QEMU_MODULE_DIR"] = str(mod_dir)

    disk_path = build_dir / "disk_p7_gpu.img"
    _create_raw_disk(disk_path, size_mib=4)
    ppm_path = build_dir / f"{name}_gpu0.ppm"
    png_path = root_dir / "docs" / "p7_virtio_gpu_scanout.png"
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_gpu_and_blk_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-PCI] PASS: virtio-gpu", wait_s=10.0)
        h.wait_for("[DRY-GPU] INIT dev=0x1af4:0x1050", wait_s=12.0)
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND", wait_s=12.0)
        h.wait_for("[DRY-WM] GPU_FALLBACK_TO_GOP", wait_s=15.0)
        pass_ln = h.wait_for("[DRY-GPU-PASS]", wait_s=15.0)
        bench_ln = h.wait_for("[DRY-GPU-BENCH]", wait_s=15.0)

        pkv = _parse_kv_ints_any(pass_ln)
        bkv = _parse_kv_ints_any(bench_ln)
        if (
            pkv.get("vendor_id") != 0x1AF4
            or pkv.get("device_id") != 0x1050
            or pkv.get("w") != 1280
            or pkv.get("h") != 800
            or pkv.get("gen") != 2
            or pkv.get("pres_g1", 0) < 9
            or pkv.get("pres_g2", 0) < 3
            or pkv.get("gop_fb") != 1
            or pkv.get("vec") != 6
            or pkv.get("unauth", 0) < 6
            or pkv.get("persist_ok") != 1
            or pkv.get("vault_ok") != 1
        ):
            raise RuntimeError(f"[{name}] VirtIO-GPU scanout audit failed: {pass_ln!r} {bench_ln!r}")

        # Capture the live VirtIO-GPU `gpu0` hardware scanout directly (independent of `-vga std`).
        if ppm_path.exists():
            ppm_path.unlink()
        qmp.cmd("screendump", {"filename": str(ppm_path), "device": "gpu0", "head": 0})
        shot_w, shot_h, non_bg_px, distinct_colors = _ppm_to_png_and_verify(ppm_path, png_path)
        ppm_path.unlink(missing_ok=True)
        if shot_w != 1280 or shot_h != 800:
            raise RuntimeError(f"[{name}] Unexpected VirtIO-GPU screendump geometry: {(shot_w, shot_h)!r}")

        # Confirm `/persist` and `DRY Vault` remain healthy in the live shell after `gen=2` recovery.
        idx = len(h.serial_lines)
        qmp.type_ascii("write /persist/p7-post.txt gpu-post-ok")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=write path=/persist/p7-post.txt bytes=11", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("sync")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=sync tx_seq=", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("cat /persist/p7-post.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=cat path=/persist/p7-post.txt bytes=11 text="gpu-post-ok"', start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("vault-status")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=vault-status unlocked=true", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")
        hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        if not hlt_lns:
            raise RuntimeError(f"[{name}] Missing [DRY-IDLE-HLT] PASS telemetry line")
        hkv = _parse_kv_ints(hlt_lns[0])
        if hkv.get("pci_quiesce", 0) < 1:
            raise RuntimeError(f"[{name}] Expected kernel PCI BME=0 quiesce >= 1, got {hkv!r}")
        if hkv.get("dma_denied", 0) < 3:
            raise RuntimeError(f"[{name}] Expected kernel PCI unauthorized denial >= 3, got {hkv!r}")

        tsc_mhz = max(1000, bkv.get("tsc_mhz", 3000))
        cyc_to_us = lambda cyc: round(cyc / float(tsc_mhz), 3)
        results = {
            "milestone": "Priority-C",
            "pci_device": f"{pkv['vendor_id']:04x}:{pkv['device_id']:04x}",
            "scanout_resolution": f"{pkv['w']}x{pkv['h']}",
            "scanout_format": "B8G8R8X8_UNORM",
            "generations_verified": pkv["gen"],
            "presents_gen1": pkv["pres_g1"],
            "presents_gen2": pkv["pres_g2"],
            "gop_fallback_verified": pkv["gop_fb"] == 1,
            "fault_vector": pkv["vec"],
            "unauth_ipc_rejects": pkv["unauth"],
            "kernel_pci_quiesces": hkv.get("pci_quiesce", 0),
            "kernel_dma_denied": hkv.get("dma_denied", 0),
            "leaked_frames": skv.get("leaked_frames", -1),
            "tsc_mhz": tsc_mhz,
            "present_p50_cyc": bkv.get("pres_p50", 0),
            "present_p95_cyc": bkv.get("pres_p95", 0),
            "present_p99_cyc": bkv.get("pres_p99", 0),
            "present_p50_us": cyc_to_us(bkv.get("pres_p50", 0)),
            "present_p99_us": cyc_to_us(bkv.get("pres_p99", 0)),
            "transfer_kib_gen1": bkv.get("xfer_kib", 0),
            "virtq_cmds_gen1": bkv.get("vq_cmds", 0),
            "crash_recovery_cyc": bkv.get("recov_cyc", 0),
            "crash_recovery_us": cyc_to_us(bkv.get("recov_cyc", 0)),
            "driver_rss_frames_4k": bkv.get("rss_4k", 0),
            "screendump_png": str(png_path.relative_to(root_dir)),
            "screendump_non_bg_pixels": non_bg_px,
            "screendump_distinct_colors": distinct_colors,
            "wall_time_sec": round(time.monotonic() - t0, 3),
        }
        out_path = root_dir / "tools" / "compat_bench" / "results_p7_gpu.json"
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")

        print(
            f"[PASS] {name}: dev={pkv['vendor_id']:04x}:{pkv['device_id']:04x} "
            f"res={pkv['w']}x{pkv['h']} gen=2 gop_fb=1 quiesce={hkv.get('pci_quiesce')} "
            f"dma_denied={hkv.get('dma_denied')} pres_p50={bkv.get('pres_p50')}cyc "
            f"colors={distinct_colors} ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_path.unlink(missing_ok=True)


def run_p7_gpu_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[[pathlib.Path, pathlib.Path, str], None],
) -> int:
    esp_gpu = build_dir / "esp_p7_gpu.img"
    write_mbr_esp_image(esp_gpu, kernel_elf, "console=com1 desktop4=1 gpu_test=1")
    run_p7a_virtio_gpu_e2e(
        "p7a_virtio_gpu_2d_scanout_and_gop_fallback", esp_gpu, ovmf_code, ovmf_vars, build_dir, root_dir
    )
    return 1
