#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Read-only Host GPU Stack, VT-d/IOMMU, Xe/i915/NVIDIA Symbol & PRIME Probe Audit."""

import glob
import json
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BUILD = ROOT / "build"
PCI_DEVICES = {
    "intel_iris_xe": "0000:00:02.0",
    "nvidia_rtx3050ti": "0000:01:00.0",
}
NVIDIA_OPEN_ROOT = Path("/usr/src/nvidia-615.78.08/kernel-open")


def read_text_safe(p: Path) -> str | None:
    try:
        return p.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def inspect_pci_device(bdf: str) -> dict:
    base = Path("/sys/bus/pci/devices") / bdf
    if not base.exists():
        return {"bdf": bdf, "present": False}
    uevent = read_text_safe(base / "uevent") or ""
    drv_match = re.search(r"^DRIVER=(.+)$", uevent, re.MULTILINE)
    bars = []
    res_txt = read_text_safe(base / "resource") or ""
    for idx, line in enumerate(res_txt.splitlines()[:6]):
        parts = line.split()
        if len(parts) == 3:
            start = int(parts[0], 16)
            end = int(parts[1], 16)
            flags = int(parts[2], 16)
            if end > start and start != 0:
                bars.append(
                    {
                        "bar": idx,
                        "size_bytes": end - start + 1,
                        "kind": "ioport" if (flags & 0x100) else "mmio",
                        "prefetchable": bool(flags & 0x2000),
                    }
                )
    return {
        "bdf": bdf,
        "present": True,
        "vendor": read_text_safe(base / "vendor"),
        "device": read_text_safe(base / "device"),
        "class": read_text_safe(base / "class"),
        "irq": read_text_safe(base / "irq"),
        "linux_driver": drv_match.group(1) if drv_match else None,
        "bars": bars,
    }


def inspect_iommu() -> dict:
    iommu_dir = Path("/sys/class/iommu")
    groups_dir = Path("/sys/kernel/iommu_groups")
    entries = sorted(p.name for p in iommu_dir.iterdir()) if iommu_dir.exists() else []
    groups = sorted(p.name for p in groups_dir.iterdir()) if groups_dir.exists() else []
    cmdline = read_text_safe(Path("/proc/cmdline")) or ""
    dmar_path = Path("/sys/firmware/acpi/tables/DMAR")
    dmar_status = "absent"
    if dmar_path.exists():
        try:
            dmar_status = f"readable:{len(dmar_path.read_bytes())}B"
        except PermissionError:
            dmar_status = "present_root_only"
        except OSError:
            dmar_status = "present_unreadable"
    return {
        "iommu_sysfs_units": entries,
        "iommu_groups_count": len(groups),
        "vtd_active_in_host_linux": len(entries) > 0,
        "dmar_acpi_table": dmar_status,
        "cmdline_iommu_flags": [
            tok for tok in cmdline.split() if "iommu" in tok.lower() or "dmar" in tok.lower()
        ],
    }


def classify_symbol(sym: str) -> str:
    if any(k in sym for k in ("drm_", "ttm_", "drm_gem", "drm_kms", "drmm_")):
        return "drm_kms_gem_ttm"
    if any(k in sym for k in ("dma_buf", "dma_resv", "dma_fence")):
        return "dma_buf_resv_fence"
    if any(k in sym for k in ("pci_", "iommu_", "dma_", "ioremap", "mtrr", "acpi_")):
        return "pci_dma_iommu_acpi"
    if any(k in sym for k in ("alloc_pages", "__free_pages", "vmalloc", "vfree", "folio", "remap_pfn", "get_user_pages", "pin_user_pages")):
        return "mm_pages_vmalloc_pin"
    if any(k in sym for k in ("mutex", "spin", "rcu", "queue_work", "workqueue", "schedule_", "wait_event", "wake_up", "kthread", "irq")):
        return "sync_wq_rcu_irq"
    if any(k in sym for k in ("request_firmware", "release_firmware")):
        return "firmware"
    return "other"


def inspect_ko_zst(ko_zst: Path) -> dict:
    if not ko_zst.is_file():
        return {"path": str(ko_zst), "present": False}
    BUILD.mkdir(parents=True, exist_ok=True)
    tmp_ko = BUILD / (ko_zst.stem + ".tmp")
    try:
        dec = subprocess.run(["zstd", "-dc", str(ko_zst)], capture_output=True, check=True)
        tmp_ko.write_bytes(dec.stdout)
        nm = subprocess.run(["nm", "-u", str(tmp_ko)], capture_output=True, text=True, check=True)
    finally:
        tmp_ko.unlink(missing_ok=True)

    buckets: dict[str, int] = {}
    syms = []
    for raw in nm.stdout.splitlines():
        parts = raw.strip().split()
        if parts and parts[0] == "U":
            sym = parts[-1].split("@")[0]
            syms.append(sym)
            b = classify_symbol(sym)
            buckets[b] = buckets.get(b, 0) + 1
    return {
        "path": str(ko_zst),
        "present": True,
        "zst_bytes": ko_zst.stat().st_size,
        "ko_bytes": len(dec.stdout),
        "undefined_symbol_count": len(syms),
        "buckets": buckets,
        "sample_symbols": syms[:10],
    }


def resolve_module_root() -> Path:
    preferred = Path("/usr/lib/modules/6.18.62-1-lts")
    if preferred.is_dir():
        return preferred
    uname = os.uname().release
    cand = Path("/usr/lib/modules") / uname
    if cand.is_dir():
        return cand
    roots = sorted(Path("/usr/lib/modules").iterdir())
    return roots[0] if roots else preferred


def inspect_nvidia_open_src() -> dict:
    hdr = NVIDIA_OPEN_ROOT / "common" / "inc" / "os-interface.h"
    impl = NVIDIA_OPEN_ROOT / "nvidia" / "os-interface.c"
    if not hdr.is_file():
        return {"present": False, "path": str(NVIDIA_OPEN_ROOT)}
    hdr_txt = hdr.read_text(encoding="utf-8", errors="replace")
    os_funcs = sorted(set(re.findall(r"\b(os_[a-z0-9_]+)\s*\(", hdr_txt)))
    c_lines = len(impl.read_text(encoding="utf-8", errors="replace").splitlines()) if impl.is_file() else 0
    return {
        "present": True,
        "source_root": str(NVIDIA_OPEN_ROOT),
        "os_interface_h_os_api_count": len(os_funcs),
        "os_interface_c_lines": c_lines,
        "sample_os_apis": os_funcs[:16],
    }


def inspect_firmware() -> dict:
    guc = Path("/lib/firmware/i915/tgl_guc_70.1.1.bin.zst")
    nv_gsp = Path("/lib/firmware/nvidia/615.78.08/gsp_ga10x.bin")
    nv_ucode = Path("/lib/firmware/nvidia/615.78.08/ucodes_ga10x.bin")
    return {
        "intel_tgl_guc_70_1_1_zst": guc.stat().st_size if guc.is_file() else None,
        "intel_tgl_firmware_files": len(glob.glob("/lib/firmware/i915/tgl_*")),
        "nvidia_615_gsp_ga10x_bytes": nv_gsp.stat().st_size if nv_gsp.is_file() else None,
        "nvidia_615_ucodes_ga10x_bytes": nv_ucode.stat().st_size if nv_ucode.is_file() else None,
    }


def main() -> None:
    probe_out = subprocess.run(
        [str(BUILD / "prime_fence_probe")],
        capture_output=True,
        text=True,
        check=True,
    )
    prime_probe = json.loads(probe_out.stdout.strip())

    mod_root = resolve_module_root()
    modules = {
        "xe": inspect_ko_zst(mod_root / "kernel/drivers/gpu/drm/xe/xe.ko.zst"),
        "i915": inspect_ko_zst(mod_root / "kernel/drivers/gpu/drm/i915/i915.ko.zst"),
        "nvidia": inspect_ko_zst(mod_root / "updates/dkms/nvidia.ko.zst"),
        "nvidia_modeset": inspect_ko_zst(mod_root / "updates/dkms/nvidia-modeset.ko.zst"),
        "nvidia_drm": inspect_ko_zst(mod_root / "updates/dkms/nvidia-drm.ko.zst"),
        "nvidia_uvm": inspect_ko_zst(mod_root / "updates/dkms/nvidia-uvm.ko.zst"),
    }

    report = {
        "milestone": "Priority-C Host GPU Compatibility Audit",
        "kernel_module_tree": str(mod_root),
        "pci_devices": {k: inspect_pci_device(v) for k, v in PCI_DEVICES.items()},
        "iommu": inspect_iommu(),
        "kernel_modules_und_audit": modules,
        "nvidia_kernel_open_os_shim": inspect_nvidia_open_src(),
        "firmware": inspect_firmware(),
        "prime_zero_copy_fence_probe": prime_probe,
    }
    out_file = ROOT / "results_h3_gpu.json"
    out_file.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    xe_u = modules["xe"].get("undefined_symbol_count")
    i915_u = modules["i915"].get("undefined_symbol_count")
    nv_u = sum(
        modules[k].get("undefined_symbol_count", 0)
        for k in ("nvidia", "nvidia_modeset", "nvidia_drm", "nvidia_uvm")
        if modules[k].get("present")
    )
    print(
        f"[H3-GPU] PASS: xe_UND={xe_u} i915_UND={i915_u} nvidia_stack_UND={nv_u} "
        f"os_apis={report['nvidia_kernel_open_os_shim'].get('os_interface_h_os_api_count')} "
        f"prime_copy_p50={prime_probe['full_copy_p50_ns']}ns "
        f"prime_fence_p50={prime_probe['zero_copy_fence_p50_ns']}ns "
        f"speedup={prime_probe['speedup_x']}x vtd_active={report['iommu']['vtd_active_in_host_linux']}"
    )


if __name__ == "__main__":
    main()
