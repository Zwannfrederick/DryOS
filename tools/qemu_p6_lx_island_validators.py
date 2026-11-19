#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone 2C-1 Genuine Linux v6.18 `virtio-rng.c` Driver Island E2E Validator (`--lx-island-only`)."""

import json
import pathlib
import time
from typing import Callable

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


def _virtio_rng_and_blk_args(disk_path: pathlib.Path) -> list[str]:
    return _virtio_blk_args(disk_path) + [
        "-object",
        "rng-random,filename=/dev/urandom,id=rng0",
        "-device",
        "virtio-rng-pci-non-transitional,rng=rng0",
    ]


def run_p6a_linux_virtio_rng_island_e2e(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    disk_path = build_dir / "disk_p6_lx_island.img"
    _create_raw_disk(disk_path, size_mib=4)
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_rng_and_blk_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-PCI] PASS: virtio-rng", wait_s=10.0)
        h.wait_for("[DRY-LX-RNG] Ready gen=1 virtio-rng.c probed pci_bound=1", wait_s=12.0)
        pass_ln = h.wait_for("[DRY-LX-RNG-PASS]", wait_s=15.0)
        pass2_ln = h.wait_for("[DRY-LX-RNG-PASS2]", wait_s=15.0)
        bench_ln = h.wait_for("[DRY-LX-RNG-BENCH]", wait_s=15.0)

        pkv = _parse_kv_ints_any(pass_ln)
        pkv.update(_parse_kv_ints_any(pass2_ln))
        bkv = _parse_kv_ints_any(bench_ln)

        if (
            pkv.get("vendor_id") != 0x1AF4
            or pkv.get("device_id") not in (0x1044, 0x1005)
            or pkv.get("mac_neg") != 1
            or pkv.get("unauth", 0) < 5
            or pkv.get("vq_irqs", 0) < 28
            or pkv.get("bytes", 0) < 1792
            or not (40 <= pkv.get("ones_pct", 0) <= 60)
            or pkv.get("uniq", 0) < 30
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
            raise RuntimeError(f"[{name}] Genuine Linux Driver Island verification failed: {pass_ln!r} {pass2_ln!r}")

        # Confirm `/persist` and `DRY Vault` remain healthy in the live desktop shell after `#UD` recovery.
        idx = len(h.serial_lines)
        qmp.type_ascii("write /persist/p6-post.txt lx-post-ok")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=write path=/persist/p6-post.txt bytes=10", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("sync")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=sync tx_seq=", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("cat /persist/p6-post.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=cat path=/persist/p6-post.txt bytes=10 text="lx-post-ok"', start_idx=idx)

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
        if hkv.get("dma_denied", 0) < 2:
            raise RuntimeError(f"[{name}] Expected kernel PCI unauthorized denial >= 2, got {hkv!r}")
        if "[DRY-BROKER-STATS]" not in serial_out:
            raise RuntimeError(f"[{name}] Missing [DRY-BROKER-STATS] teardown marker")

        tsc_mhz = max(1000, bkv.get("tsc_mhz", 3000))
        cyc_to_us = lambda cyc: round(cyc / float(tsc_mhz), 3)
        results = {
            "milestone": "2C-1",
            "upstream_linux_revision": "v6.18 (7d0a66e4bb9081d75c82ec4957c50034cb0ea449)",
            "upstream_driver": "drivers/char/hw_random/virtio-rng.c",
            "upstream_sha256": "720433a85ce0ce6f84e85842b54d23c7c8e9bad792c66b1066accbaa7fb95a75",
            "pci_device": f"{pkv['vendor_id']:04x}:{pkv['device_id']:04x}",
            "vq_irqs": pkv["vq_irqs"],
            "entropy_bytes": pkv["bytes"],
            "ones_pct": pkv["ones_pct"],
            "unique_u64_of_32": pkv["uniq"],
            "fault_vector": pkv["vec"],
            "restarts": pkv["restarts"],
            "kernel_pci_quiesces": hkv.get("pci_quiesce", 0),
            "kernel_dma_denied": hkv.get("dma_denied", 0),
            "leaked_frames": skv.get("leaked_frames", -1),
            "tsc_mhz": tsc_mhz,
            "direct_1hop_p50_cyc": bkv.get("dir_p50", 0),
            "direct_1hop_p95_cyc": bkv.get("dir_p95", 0),
            "direct_1hop_p99_cyc": bkv.get("dir_p99", 0),
            "direct_1hop_p50_us": cyc_to_us(bkv.get("dir_p50", 0)),
            "broker_2hop_p50_cyc": bkv.get("brk_p50", 0),
            "broker_2hop_p95_cyc": bkv.get("brk_p95", 0),
            "broker_2hop_p99_cyc": bkv.get("brk_p99", 0),
            "broker_2hop_p50_us": cyc_to_us(bkv.get("brk_p50", 0)),
            "ioring_batch16_total_cyc": bkv.get("ring16_cyc", 0),
            "ioring_batch16_per_op_cyc": round(bkv.get("ring16_cyc", 0) / 16.0, 1),
            "crash_recovery_cyc": bkv.get("recov_cyc", 0),
            "crash_recovery_us": cyc_to_us(bkv.get("recov_cyc", 0)),
            "island_rss_frames_4k": bkv.get("rss_4k", 0),
            "wall_time_sec": round(time.monotonic() - t0, 3),
        }
        out_path = root_dir / "tools" / "compat_bench" / "results_p6_lx_rng.json"
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")

        print(
            f"[PASS] {name}: linux=v6.18_virtio-rng.c dev={pkv['vendor_id']:04x}:{pkv['device_id']:04x} "
            f"bytes={pkv['bytes']} vq_irqs={pkv['vq_irqs']} ones={pkv['ones_pct']}% vec=6 "
            f"quiesce={hkv.get('pci_quiesce')} dir_p50={bkv.get('dir_p50')}cyc "
            f"ring16_per_op={results['ioring_batch16_per_op_cyc']}cyc ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)


def run_p6_lx_island_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[[pathlib.Path, pathlib.Path, str], None],
) -> int:
    esp_lx = build_dir / "esp_p6_lx_island.img"
    write_mbr_esp_image(esp_lx, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1 lx_island=1")
    run_p6a_linux_virtio_rng_island_e2e(
        "p6a_linux_v618_virtio_rng_island", esp_lx, ovmf_code, ovmf_vars, build_dir, root_dir
    )
    return 1
