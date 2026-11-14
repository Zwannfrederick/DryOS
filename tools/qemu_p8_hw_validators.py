#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone 2C-2 Hardware Foundations Validator (`--p8-only`).

Boots DRY OS on Q35 with Intel VT-d (`intel-iommu,caching-mode=on`), QEMU `edu` (`PCI MSI`, DMA),
and `ivshmem-plain` (`BAR64 > 4 GiB`, prefetchable WC/UC), asserting clean Ring-3 hardware access,
bound-notification MSI wakeup, VT-d rogue-DMA fault containment, `#UD` quiesce/recovery, and 0 leaks.
"""

import json
import pathlib
import time
from typing import Callable

from qemu_log_validators import _parse_kv_ints

EXIT_CODE_SUCCESS = 33


def _find_prefixed_line(log: str, prefix: str) -> str | None:
    for line in log.splitlines():
        s = line.strip()
        if s.startswith(prefix):
            return s
    return None


def _parse_kv_any(line: str) -> dict[str, int]:
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


def run_p8a_hw_foundations_e2e(
    name: str,
    esp_img: pathlib.Path,
    root_dir: pathlib.Path,
    run_qemu: Callable[..., tuple[int, str, float]],
) -> None:
    t0 = time.monotonic()
    extra_args = [
        "-fw_cfg",
        "name=opt/ovmf/X-PciMmio64Mb,string=4096",
        "-device",
        "intel-iommu,intremap=off,caching-mode=on",
        "-device",
        "edu,dma_mask=0xffffffffffffffff",
        "-object",
        "memory-backend-ram,size=256M,id=iv0",
        "-device",
        "ivshmem-plain,memdev=iv0",
    ]
    rc, log, _elapsed = run_qemu(
        name,
        esp_img,
        smp=2,
        timeout_sec=35,
        extra_args=extra_args,
    )
    if rc != EXIT_CODE_SUCCESS:
        raise AssertionError(
            f"{name}: expected exit code {EXIT_CODE_SUCCESS}, got {rc}.\nLog tail:\n{log[-3000:]}"
        )

    vtd_line = _find_prefixed_line(log, "[DRY-IOMMU] VTD_ACTIVE ")
    hw_pass_line = _find_prefixed_line(log, "[DRY-HW-PASS] ")
    hw_bench_line = _find_prefixed_line(log, "[DRY-HW-BENCH] ")
    hw_kernel_line = _find_prefixed_line(log, "[DRY-HW-KERNEL] PASS: ")
    boot_ok_line = _find_prefixed_line(log, "[DRY-BOOT-OK] Milestone 2C-2 ")

    for label, line in (
        ("VTD_ACTIVE", vtd_line),
        ("DRY-HW-PASS", hw_pass_line),
        ("DRY-HW-BENCH", hw_bench_line),
        ("DRY-HW-KERNEL", hw_kernel_line),
        ("DRY-BOOT-OK", boot_ok_line),
    ):
        if line is None:
            raise AssertionError(f"{name}: missing {label} marker.\nLog tail:\n{log[-3000:]}")

    pkv = _parse_kv_any(hw_pass_line)
    bkv = _parse_kv_any(hw_bench_line)
    kkv = _parse_kv_any(hw_kernel_line)

    assert pkv.get("bar64_above4g") == 1, f"{name}: BAR64 above4g failed: {hw_pass_line}"
    assert pkv.get("wc_ok") == 1, f"{name}: WC coherence failed: {hw_pass_line}"
    assert pkv.get("bounds_rej", 0) >= 3, f"{name}: bounds rejections < 3: {hw_pass_line}"
    assert 0x40 <= pkv.get("msi_vec", 0) <= 0x5F, f"{name}: bad MSI vector: {hw_pass_line}"
    assert pkv.get("msi_irqs", 0) >= 32, f"{name}: msi_irqs < 32: {hw_pass_line}"
    assert pkv.get("bound_notif_ok") == 1, f"{name}: bound notif wait failed: {hw_pass_line}"
    assert pkv.get("vtd_dma_ok") == 1, f"{name}: VT-d SLPT DMA failed: {hw_pass_line}"
    assert pkv.get("vtd_rogue_faults", 0) >= 1, f"{name}: VT-d rogue fault not observed: {hw_pass_line}"
    assert pkv.get("crash_vec") == 6, f"{name}: unexpected child fault vec: {hw_pass_line}"
    assert pkv.get("recovery_ok") == 1, f"{name}: post-crash recovery failed: {hw_pass_line}"

    assert kkv.get("vtd_enabled") == 1, f"{name}: kernel VT-d not enabled: {hw_kernel_line}"
    assert kkv.get("vtd_present") == 1, f"{name}: kernel VT-d not present: {hw_kernel_line}"
    assert kkv.get("dmar_drhd", 0) >= 1, f"{name}: DMAR DRHD count < 1: {hw_kernel_line}"
    assert kkv.get("quiesces", 0) >= 1, f"{name}: PCI crash quiesce count < 1: {hw_kernel_line}"
    assert kkv.get("vtd_faults", 0) >= 1, f"{name}: kernel VT-d fault count < 1: {hw_kernel_line}"
    assert kkv.get("leaked_frames") == 0, f"{name}: frame leak detected: {hw_kernel_line}"

    tsc_mhz = max(1, bkv.get("tsc_mhz", 2000))
    msi_p50_us = round(bkv.get("msi_bound_p50_cyc", 0) / tsc_mhz, 2)
    results = {
        "milestone": "2C-2-hw-foundations",
        "tsc_mhz": tsc_mhz,
        "bar64_above4g": pkv["bar64_above4g"] == 1,
        "wc_64k_cyc": bkv.get("wc_64k_cyc", 0),
        "uc_64k_cyc": bkv.get("uc_64k_cyc", 0),
        "msi_vec": hex(pkv["msi_vec"]),
        "msi_irqs": pkv["msi_irqs"],
        "msi_bound_p50_cyc": bkv.get("msi_bound_p50_cyc", 0),
        "msi_bound_p50_us": msi_p50_us,
        "dma_rt_cyc": bkv.get("dma_rt_cyc", 0),
        "vtd_rogue_faults": pkv["vtd_rogue_faults"],
        "kernel_pci_quiesces": kkv["quiesces"],
        "kernel_msi_irqs": kkv.get("msi_irqs", 0),
        "kernel_msi_acks": kkv.get("msi_acks", 0),
        "leaked_frames": kkv["leaked_frames"],
        "wall_time_sec": round(time.monotonic() - t0, 3),
    }
    out_path = root_dir / "tools" / "compat_bench" / "p8_hw_foundations_latest.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")

    print(
        f"[PASS] {name}: bar64=1 wc={results['wc_64k_cyc']}cyc uc={results['uc_64k_cyc']}cyc "
        f"msi_vec={results['msi_vec']} msi_p50={msi_p50_us}us vtd_faults={results['vtd_rogue_faults']} "
        f"quiesce={results['kernel_pci_quiesces']} leaked_frames=0 ({time.monotonic() - t0:.2f}s)."
    )


def run_p8_hw_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    write_mbr_esp_image: Callable[[pathlib.Path, pathlib.Path, str], None],
    run_qemu: Callable[..., tuple[int, str, float]],
) -> int:
    esp_hw = build_dir / "esp_p8_hw.img"
    write_mbr_esp_image(esp_hw, kernel_elf, "console=com1 hw_test=1")
    run_p8a_hw_foundations_e2e("p8a_hw_foundations_vtd_msi_bar64_e2e", esp_hw, root_dir, run_qemu)
    return 1
