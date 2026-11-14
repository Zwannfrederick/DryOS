#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone A4-3 Linux `v6.18` `bochs.c` DRM Driver Island Validator (`--p12-only`).

Boots DRY OS on Q35 with Intel VT-d (`intel-iommu`) and a secondary QEMU
`bochs-display` PCI GPU (`1234:1111`, `id=bochs0`) alongside primary `-vga std`
GOP, verifying `libdry-lx-compat` BAR0/BAR2 mapping, EDID detection, RO PRIME
`FrameCap` DMA-BUF import, `#UD` fault recovery (`gen=2`), QMP `bochs0` scanout,
and `leaked_frames=0`.
"""

import json
import pathlib
import time
from typing import Callable

from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints
from qemu_p1_validators import EXIT_CODE_SUCCESS
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


def _bochs_drm_qemu_args(disk_path: pathlib.Path) -> list[str]:
    return _virtio_blk_args(disk_path) + [
        "-device",
        "intel-iommu,intremap=off,caching-mode=on",
        "-device",
        "bochs-display,id=bochs0,vgamem=4M",
    ]


def run_p12a_linux_bochs_drm_island_e2e(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    disk_path = build_dir / "disk_p12_lx_drm.img"
    _create_raw_disk(disk_path, size_mib=4)
    shots_dir = root_dir / "docs" / "screenshots" / "glass21"
    shots_dir.mkdir(parents=True, exist_ok=True)
    ppm_bochs = build_dir / f"{name}_bochs0.ppm"
    ppm_gop = build_dir / f"{name}_gop.ppm"
    png_bochs = shots_dir / "p12_bochs_drm_scanout.png"
    png_gop = shots_dir / "p12_gop_desktop_coexist.png"
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
        extra_qemu_args=_bochs_drm_qemu_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-PCI] PASS: display-aux", wait_s=10.0)
        h.wait_for("[DRY-LX-DRM] Ready gen=1 bochs.c probed", wait_s=12.0)
        pass_ln = h.wait_for("[DRY-LX-DRM] PASS:", wait_s=15.0)
        recov_ln = h.wait_for("[DRY-LX-DRM-RECOV]", wait_s=15.0)
        bench_ln = h.wait_for("[DRY-LX-DRM-BENCH]", wait_s=15.0)
        h.wait_for("[DRY-LX-DRM] SCANOUT_READY w=800 h=600 gen=2", wait_s=15.0)

        pkv = {**_parse_kv_ints_any(pass_ln), **_parse_kv_ints_any(recov_ln)}
        bkv = _parse_kv_ints_any(bench_ln)
        if (
            pkv.get("vendor_id") != 0x1234
            or pkv.get("device_id") != 0x1111
            or pkv.get("slot") != 6
            or pkv.get("disp_pci", 0) < 2
            or pkv.get("mac_neg") != 1
            or pkv.get("ro_dma") != 1
            or pkv.get("bar0_ok") != 1
            or pkv.get("bar2_ok") != 1
            or pkv.get("unauth", 0) < 5
            or pkv.get("vec") != 6
            or pkv.get("restarts", 0) < 1
            or pkv.get("ep_rev") != 1
            or pkv.get("ntf_rev") != 1
            or pkv.get("cq_rev") != 1
            or pkv.get("ring_rev") != 1
            or pkv.get("stale_gen") != 1
            or pkv.get("persist_ok") != 1
            or pkv.get("vault_ok") != 1
        ):
            raise RuntimeError(f"[{name}] Linux bochs DRM island audit failed: {pass_ln!r}")

        ppm_bochs.unlink(missing_ok=True)
        qmp.cmd("screendump", {"filename": str(ppm_bochs), "device": "bochs0", "head": 0})
        bw, bh, b_px, b_col = _ppm_to_png_and_verify(ppm_bochs, png_bochs)
        ppm_bochs.unlink(missing_ok=True)
        if bw != 800 or bh != 600 or b_px < 50000 or b_col < 6:
            raise RuntimeError(
                f"[{name}] Unexpected bochs0 scanout PNG geometry/contrast: {(bw, bh, b_px, b_col)!r}"
            )

        ppm_gop.unlink(missing_ok=True)
        qmp.cmd("screendump", {"filename": str(ppm_gop)})
        gw, gh, g_px, g_col = _ppm_to_png_and_verify(ppm_gop, png_gop)
        ppm_gop.unlink(missing_ok=True)
        if gw < 800 or gh < 600 or g_px < 10000 or g_col < 4:
            raise RuntimeError(
                f"[{name}] Unexpected primary GOP coexist PNG: {(gw, gh, g_px, g_col)!r}"
            )

        idx = len(h.serial_lines)
        qmp.type_ascii("write /persist/p12-post.txt drm-post-ok")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=write path=/persist/p12-post.txt bytes=11", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("sync")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=sync tx_seq=", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        sess_ln = h.wait_for("[DRY-P2-SESSION] PASS:", wait_s=25.0)
        h.wait_for("[DRY-BOOT-OK]", wait_s=10.0)
        qmp.close()
        h.qmp = None
        rc = h.proc.wait(timeout=10.0)
        h.drain_available()
        serial_out = "".join(h.serial_lines)
        skv = _parse_kv_ints(sess_ln)
        if (
            rc != EXIT_CODE_SUCCESS
            or skv.get("cpus") != 2
            or skv.get("ioport_denied") != 2
            or skv.get("leaked_frames", -1) != 0
        ):
            raise RuntimeError(f"[{name}] Expected rc={EXIT_CODE_SUCCESS}, ioport_denied=2, leaked=0, got rc={rc} {skv!r}")
        hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        if not hlt_lns:
            raise RuntimeError(f"[{name}] Missing [DRY-IDLE-HLT] PASS telemetry line")
        hkv = _parse_kv_ints(hlt_lns[0])
        if hkv.get("pci_quiesce", 0) < 1:
            raise RuntimeError(f"[{name}] Expected kernel PCI BME=0 quiesce >= 1, got {hkv!r}")

        tsc_mhz = max(1000, bkv.get("tsc_mhz", 3000))
        cyc_to_us = lambda cyc: round(cyc / float(tsc_mhz), 3)
        results = {
            "milestone": "Ascension-4.0-A4-3",
            "upstream_linux_revision": "v6.18 (7d0a66e4bb9081d75c82ec4957c50034cb0ea449)",
            "upstream_driver": "drivers/gpu/drm/tiny/bochs.c",
            "upstream_sha256": "93f5c4bc2811ad1e0961a4e3d5c0410ac4b2336b2bdc96cc2c40802cc439ffe3",
            "pci_device": f"{pkv['vendor_id']:04x}:{pkv['device_id']:04x}",
            "bdf": pkv.get("bdf", 0),
            "managed_slot": pkv["slot"],
            "display_pci_funcs_visible": pkv["disp_pci"],
            "ro_dmabuf_enforced": pkv["ro_dma"] == 1,
            "edid_valid": pkv.get("edid_ok", 0) == 1,
            "bar0_mapped": pkv["bar0_ok"] == 1,
            "bar2_mapped": pkv["bar2_ok"] == 1,
            "fault_vector": pkv["vec"],
            "restarts": pkv["restarts"],
            "kernel_pci_quiesces": hkv.get("pci_quiesce", 0),
            "kernel_dma_denied": hkv.get("dma_denied", 0),
            "leaked_frames": skv.get("leaked_frames", -1),
            "tsc_mhz": tsc_mhz,
            "direct_damage_p50_cyc": bkv.get("dir_p50", 0),
            "direct_damage_p95_cyc": bkv.get("dir_p95", 0),
            "direct_damage_p99_cyc": bkv.get("dir_p99", 0),
            "direct_damage_p50_us": cyc_to_us(bkv.get("dir_p50", 0)),
            "ioring_32_flushes_total_cyc": bkv.get("ring32_cyc", 0),
            "ioring_per_flush_cyc": round(bkv.get("ring32_cyc", 0) / 32.0, 1),
            "crash_recovery_cyc": bkv.get("recov_cyc", 0),
            "crash_recovery_us": cyc_to_us(bkv.get("recov_cyc", 0)),
            "island_rss_frames_4k": bkv.get("rss_4k", 0),
            "bochs_scanout": {"w": bw, "h": bh, "non_bg_px": b_px, "distinct_colors": b_col, "png": str(png_bochs.relative_to(root_dir))},
            "gop_coexist": {"w": gw, "h": gh, "non_bg_px": g_px, "distinct_colors": g_col, "png": str(png_gop.relative_to(root_dir))},
            "wall_time_sec": round(time.monotonic() - t0, 3),
        }
        out_path = root_dir / "tools" / "compat_bench" / "results_p12_lx_compat.json"
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
        print(
            f"[PASS] {name}: linux=v6.18_bochs.c dev={pkv['vendor_id']:04x}:{pkv['device_id']:04x} "
            f"res={bw}x{bh} gen=2 ro_dma=1 vec=6 quiesce={hkv.get('pci_quiesce')} "
            f"dir_p50={bkv.get('dir_p50')}cyc ring_per_flush={results['ioring_per_flush_cyc']}cyc "
            f"colors={b_col} ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)
        ppm_bochs.unlink(missing_ok=True)
        ppm_gop.unlink(missing_ok=True)


def run_p12_lx_compat_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> int:
    esp_drm = build_dir / "esp_p12_lx_drm.img"
    write_mbr_esp_image(esp_drm, kernel_elf, "console=com1 desktop4=1 lx_drm=1")
    run_p12a_linux_bochs_drm_island_e2e(
        "p12a_linux_v618_bochs_drm_island", esp_drm, ovmf_code, ovmf_vars, build_dir, root_dir
    )
    return 1
