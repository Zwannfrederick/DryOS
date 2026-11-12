#!/usr/bin/env python3
"""
Exp-B3: Cross-Subsystem Linux Kernel Driver ELF Symbol & Dependency Surface Audit.

Decompresses real compiled Linux 6.18.62-1-lts and DKMS 615.78.08 `.ko.zst` kernel
modules into `/tmp`, inspects ELF `.text`/total sizes and undefined (`UND`) kernel
symbols via `nm -u` and `size`, classifies external kernel dependencies by subsystem,
and compares each driver class against DRY OS `experiments/h2-island` (`igc` runtime:
3,468 LOC implementing ~95 kernel symbols).
"""

import json
import os
import subprocess
import tempfile
from pathlib import Path

KVER = "6.18.62-1-lts"
MOD_ROOT = Path(f"/lib/modules/{KVER}")
OUT_JSON = Path(__file__).resolve().parent / "results_exp_b3_drivers.json"

TARGETS = [
    ("virtio-net", "Paravirt NIC", ["kernel/drivers/net/virtio_net.ko.zst"]),
    ("virtio-gpu", "Paravirt GPU (DRM/VirGL)", ["kernel/drivers/gpu/drm/virtio/virtio-gpu.ko.zst"]),
    ("igc", "Discrete PCIe 2.5GbE NIC (H2 Proven)", ["kernel/drivers/net/ethernet/intel/igc/igc.ko.zst"]),
    ("e1000e", "Discrete PCIe 1GbE NIC (H2 Proven)", ["kernel/drivers/net/ethernet/intel/e1000e/e1000e.ko.zst"]),
    ("nvme", "PCIe NVMe Storage (core+pci)", [
        "kernel/drivers/nvme/host/nvme-core.ko.zst",
        "kernel/drivers/nvme/host/nvme.ko.zst",
    ]),
    ("i915", "Intel Iris Xe Legacy KMS/GEM GPU", ["kernel/drivers/gpu/drm/i915/i915.ko.zst"]),
    ("xe", "Intel Xe Modern VM_BIND GPU", ["kernel/drivers/gpu/drm/xe/xe.ko.zst"]),
    ("amdgpu", "AMD Radeon KMS/TTM GPU", ["kernel/drivers/gpu/drm/amd/amdgpu/amdgpu.ko.zst"]),
    ("nvidia-core", "NVIDIA Open RM (GSP)", ["updates/dkms/nvidia.ko.zst"]),
    ("nvidia-uvm", "NVIDIA CUDA Unified Memory (HMM)", ["updates/dkms/nvidia-uvm.ko.zst"]),
    ("nvidia-full", "NVIDIA Full Stack (RM+KMS+DRM+UVM)", [
        "updates/dkms/nvidia.ko.zst",
        "updates/dkms/nvidia-modeset.ko.zst",
        "updates/dkms/nvidia-drm.ko.zst",
        "updates/dkms/nvidia-uvm.ko.zst",
    ]),
]

BUCKETS = [
    ("mm_hmm", (
        "hmm_", "mmu_notifier", "pin_user_pages", "get_user_pages", "vm_insert",
        "remap_pfn", "vmap", "vunmap", "vmalloc", "vfree", "kvfree", "kvmalloc",
        "kmalloc", "__kmalloc", "kfree", "kmem_cache", "folio_", "__folio",
        "__alloc_pages", "alloc_pages", "__free_pages", "free_pages", "shmem_",
        "unmap_mapping_range", "__mmput", "mmput", "mmap_", "vmf_", "page_to_",
        "pfn_to_", "set_memory_", "set_pages_", "sg_alloc", "sg_free", "sg_next",
    )),
    ("drm_dmabuf", (
        "drm_", "__drm_", "ttm_", "dma_buf_", "dma_resv_", "dma_fence_",
        "sync_file_", "aperture_", "fb_", "backlight_", "cec_", "hdmi_",
        "mipi_", "drmm_",
    )),
    ("pci_dma_irq", (
        "pci_", "pcim_", "pcie_", "dma_map", "dma_unmap", "dma_alloc", "dma_free",
        "dma_set", "dma_sync", "dmam_", "iommu_", "request_threaded_irq",
        "request_irq", "free_irq", "enable_irq", "disable_irq", "irq_",
        "ioremap", "iounmap", "arch_io_",
    )),
    ("sched_sync_rcu", (
        "mutex_", "__mutex_", "ww_mutex", "down_", "up_", "_raw_spin", "_raw_read",
        "_raw_write", "rcu_", "call_rcu", "synchronize_rcu", "kvfree_rcu",
        "srcu_", "schedule", "io_schedule", "wake_up", "__wake_up", "init_wait",
        "prepare_to_wait", "finish_wait", "autoremove_wake", "wait_for_completion",
        "complete", "init_completion", "queue_work", "queue_delayed_work",
        "mod_delayed_work", "flush_work", "cancel_work", "cancel_delayed_work",
        "alloc_workqueue", "destroy_workqueue", "kthread_", "hrtimer_",
        "mod_timer", "timer_", "del_timer", "msleep", "usleep_range", "fsleep",
        "__msecs_to_jiffies", "jiffies", "ktime_",
    )),
    ("net_blk_virtio", (
        "netif_", "napi_", "eth_", "skb_", "__skb_", "consume_skb", "kfree_skb",
        "__netdev", "netdev_", "alloc_etherdev", "register_netdev", "unregister_netdev",
        "xdp_", "page_pool_", "ptp_", "phylink_", "blk_mq_", "blk_", "bio_",
        " disk_", "nvme_", "virtqueue_", "virtio_", "register_virtio",
    )),
    ("acpi_pm_fw_i2c", (
        "acpi_", "pm_runtime_", "__pm_runtime_", "dev_pm_", "request_firmware",
        "firmware_request", "release_firmware", "i2c_", "component_", "mei_",
        "dmi_", "efi", "vga_", "apple_gmux",
    )),
]


def classify_symbol(sym: str) -> str:
    for bucket, prefixes in BUCKETS:
        if any(sym.startswith(p) for p in prefixes):
            return bucket
    return "core_kobj_misc"


def audit_target(name: str, desc: str, rel_paths: list[str], tmpdir: Path) -> dict:
    total_ko_bytes = 0
    total_text_bytes = 0
    total_data_bss_bytes = 0
    defined_syms: set[str] = set()
    undef_syms: set[str] = set()

    for idx, rel in enumerate(rel_paths):
        src = MOD_ROOT / rel
        ko_path = tmpdir / f"{name}_{idx}.ko"
        with open(ko_path, "wb") as out_f:
            subprocess.run(["zstd", "-d", "-q", "-c", str(src)], stdout=out_f, check=True)
        total_ko_bytes += ko_path.stat().st_size

        sz_out = subprocess.check_output(["size", str(ko_path)], text=True).splitlines()
        if len(sz_out) >= 2:
            parts = sz_out[1].split()
            total_text_bytes += int(parts[0])
            total_data_bss_bytes += int(parts[1]) + int(parts[2])

        nm_out = subprocess.check_output(["nm", str(ko_path)], text=True).splitlines()
        for line in nm_out:
            cols = line.split()
            if not cols:
                continue
            sym_type = cols[-2] if len(cols) >= 2 else ""
            sym_name = cols[-1]
            if sym_type == "U":
                undef_syms.add(sym_name)
            elif sym_type in ("T", "D", "R", "B", "W"):
                defined_syms.add(sym_name)

    # Subtract intra-bundle symbols exported by sibling modules (e.g. nvme-core -> nvme)
    external_undef = sorted(undef_syms - defined_syms)
    counts = {b[0]: 0 for b in BUCKETS}
    counts["core_kobj_misc"] = 0
    sample_mm_drm: list[str] = []

    for sym in external_undef:
        b = classify_symbol(sym)
        counts[b] += 1
        if b in ("mm_hmm", "drm_dmabuf") and len(sample_mm_drm) < 12:
            sample_mm_drm.append(sym)

    return {
        "driver": name,
        "description": desc,
        "modules": rel_paths,
        "elf_ko_kib": round(total_ko_bytes / 1024.0, 1),
        "text_kib": round(total_text_bytes / 1024.0, 1),
        "data_bss_kib": round(total_data_bss_bytes / 1024.0, 1),
        "external_undef_total": len(external_undef),
        "buckets": counts,
        "dde_shim_multiplier_vs_igc": round(len(external_undef) / 141.0, 2),
        "sample_mm_drm_symbols": sample_mm_drm,
    }


def main() -> None:
    results = []
    with tempfile.TemporaryDirectory(prefix="dry_exp_b3_") as tmp:
        tmpdir = Path(tmp)
        for name, desc, rel_paths in TARGETS:
            rec = audit_target(name, desc, rel_paths, tmpdir)
            results.append(rec)

    igc_undef = next(r["external_undef_total"] for r in results if r["driver"] == "igc")
    for r in results:
        r["dde_shim_multiplier_vs_igc"] = round(r["external_undef_total"] / max(igc_undef, 1), 2)

    payload = {
        "experiment": "Exp-B3",
        "kernel_version": KVER,
        "nvidia_dkms_version": "615.78.08",
        "h2_igc_baseline_shim_loc": 3468,
        "drivers": results,
    }
    OUT_JSON.write_text(json.dumps(payload, indent=2) + "\n")

    hdr = (
        f"{'Driver':<13} | {'Text KiB':>8} | {'Total UND':>9} | "
        f"{'MM/HMM':>6} | {'DRM/DMA':>7} | {'PCI/IRQ':>7} | {'Sync/WQ':>7} | {'vs igc':>6}"
    )
    print(hdr)
    print("-" * len(hdr))
    for r in results:
        b = r["buckets"]
        print(
            f"{r['driver']:<13} | {r['text_kib']:>8.1f} | {r['external_undef_total']:>9} | "
            f"{b['mm_hmm']:>6} | {b['drm_dmabuf']:>7} | {b['pci_dma_irq']:>7} | "
            f"{b['sched_sync_rcu']:>7} | {r['dde_shim_multiplier_vs_igc']:>5.2f}x"
        )


if __name__ == "__main__":
    main()
