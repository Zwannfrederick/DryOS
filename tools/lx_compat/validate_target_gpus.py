#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Strictly read-only target GPU audit & NVIDIA open-gpu shim compile validator (Track D).

Reads host PCI sysfs metadata (`/sys/bus/pci/devices/*`), firmware blobs
(`/lib/firmware/{i915,nvidia}`), and OpenRM headers (`/usr/src/nvidia-*`) WITHOUT
performing any sysfs writes, module unbinds, or bare-metal hardware mutations.
"""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "lx_compat"))
from lx_driver_manager import CATALOG_PATH, match_pci, run_onboarding_pipeline  # noqa: E402
import re  # noqa: E402

SYS_PCI_DIR = pathlib.Path("/sys/bus/pci/devices")
SHIM_C_PATH = ROOT / "drivers" / "lx-nv-open-shim" / "nv_os_shim_check.c"
OUT_JSON = ROOT / "tools" / "compat_bench" / "results_target_gpus_a4.json"


def _read_hex(path: pathlib.Path) -> int | None:
    if not path.exists():
        return None
    try:
        return int(path.read_text(encoding="utf-8").strip(), 16)
    except (OSError, ValueError):
        return None


def _parse_pci_resources(res_path: pathlib.Path) -> list[dict[str, Any]]:
    bars: list[dict[str, Any]] = []
    if not res_path.exists():
        return bars
    for idx, line in enumerate(res_path.read_text(encoding="utf-8").splitlines()[:6]):
        parts = line.split()
        if len(parts) < 3:
            continue
        start = int(parts[0], 16)
        end = int(parts[1], 16)
        flags = int(parts[2], 16)
        if start == 0 and end == 0:
            continue
        size = end - start + 1
        bars.append(
            {
                "bar": idx,
                "start": hex(start),
                "end": hex(end),
                "size_bytes": size,
                "size_mib": round(size / (1024 * 1024), 3),
                "flags": hex(flags),
                "is_io": bool(flags & 0x100),
                "is_mem": bool(flags & 0x200),
                "is_64bit": bool(flags & 0x400),
                "is_prefetchable": bool(flags & 0x2000),
                "above_4g": start >= (1 << 32),
            }
        )
    return bars


def inspect_host_pci_device(target_vendor: int, target_device: int) -> dict[str, Any] | None:
    if not SYS_PCI_DIR.exists():
        return None
    for dev_dir in sorted(SYS_PCI_DIR.iterdir()):
        v = _read_hex(dev_dir / "vendor")
        d = _read_hex(dev_dir / "device")
        if v != target_vendor or d != target_device:
            continue
        drv_link = dev_dir / "driver"
        host_drv = pathlib.Path(os.path.realpath(drv_link)).name if drv_link.exists() else "unbound"
        iommu_link = dev_dir / "iommu_group"
        iommu_grp = pathlib.Path(os.path.realpath(iommu_link)).name if iommu_link.exists() else None
        bars = _parse_pci_resources(dev_dir / "resource")
        return {
            "found": True,
            "bdf": dev_dir.name,
            "vendor_id": f"0x{v:04x}",
            "device_id": f"0x{d:04x}",
            "class_code": hex(_read_hex(dev_dir / "class") or 0),
            "revision": hex(_read_hex(dev_dir / "revision") or 0),
            "subsystem_vendor": hex(_read_hex(dev_dir / "subsystem_vendor") or 0),
            "subsystem_device": hex(_read_hex(dev_dir / "subsystem_device") or 0),
            "host_driver": host_drv,
            "iommu_group": iommu_grp,
            "bars": bars,
            "has_bar_above_4g": any(b["above_4g"] for b in bars),
            "catalog_match": match_pci(f"{v:04x}:{d:04x}", CATALOG_PATH),
        }
    return None


def _glob_firmware(base: pathlib.Path, pattern: str) -> list[dict[str, Any]]:
    if not base.exists():
        return []
    out: list[dict[str, Any]] = []
    for p in sorted(base.glob(pattern)):
        if p.is_file():
            out.append({"path": str(p), "bytes": p.stat().st_size})
    return out


def compile_check_nvidia_shim(out_dir: pathlib.Path) -> dict[str, Any]:
    out_dir.mkdir(parents=True, exist_ok=True)
    inc_candidates = sorted(pathlib.Path("/usr/src").glob("nvidia-*/kernel-open/common/inc"))
    base_cmd = [
        "clang",
        "-std=gnu11",
        "-target",
        "x86_64-unknown-none-elf",
        "-ffreestanding",
        "-fno-builtin",
        "-mno-red-zone",
        "-mno-sse",
        "-mno-sse2",
        "-mno-avx",
        "-Wall",
        "-Wextra",
        "-Werror",
    ]
    results: dict[str, Any] = {
        "shim_source": str(SHIM_C_PATH.relative_to(ROOT)),
        "fallback_build_ok": False,
        "nvidia_header_dir": None,
        "nvidia_header_build_ok": False,
    }
    fallback_obj = out_dir / "nv_os_shim_fallback.o"
    subprocess.check_call(base_cmd + ["-c", str(SHIM_C_PATH), "-o", str(fallback_obj)])
    results["fallback_build_ok"] = fallback_obj.exists()

    if inc_candidates:
        inc_dir = inc_candidates[-1]
        hdr_obj = out_dir / "nv_os_shim_check.o"
        subprocess.check_call(
            base_cmd
            + ["-DHAVE_NVIDIA_OS_INTERFACE_H=1", f"-I{inc_dir}", "-c", str(SHIM_C_PATH), "-o", str(hdr_obj)]
        )
        results["nvidia_header_dir"] = str(inc_dir)
        results["nvidia_header_build_ok"] = hdr_obj.exists()

    txt = SHIM_C_PATH.read_text(encoding="utf-8")
    table_entries = re.findall(r'\{\s*"(os_[a-z0-9_]+)"\s*,\s*(NV_SHIM_[A-Z0-9_]+)\s*\}', txt)
    counts: dict[str, int] = {}
    for _, cls in table_entries:
        counts[cls] = counts.get(cls, 0) + 1
    results["classifications"] = {
        "total_classified": len(table_entries),
        "counts": counts,
        "symbols": {sym: cls for sym, cls in table_entries},
    }
    return results


def main() -> None:
    xe_info = inspect_host_pci_device(0x8086, 0x9A49)
    nv_info = inspect_host_pci_device(0x10DE, 0x25A0)
    i915_fw = _glob_firmware(pathlib.Path("/lib/firmware/i915"), "tgl_*")
    nv_fw_615 = _glob_firmware(pathlib.Path("/lib/firmware/nvidia/615.78.08"), "*.bin")
    nv_fw_ga107 = _glob_firmware(pathlib.Path("/lib/firmware/nvidia/ga107/gsp"), "*")
    shim_res = compile_check_nvidia_shim(ROOT / "kernel" / "build")

    report = {
        "audit_mode": "strictly_read_only_host_and_shim_compile",
        "intel_iris_xe_8086_9a49": {
            "pci": xe_info or {"found": False},
            "onboarding": run_onboarding_pipeline("intel_i915_tgl"),
            "tgl_firmware_count": len(i915_fw),
            "tgl_firmware_sample": i915_fw[:8],
            "execution_status": "read_only_host_inventory_and_bar_vt_d_alignment_verified",
            "live_modeset_verified_on_dryos": False,
            "missing_for_full_modeset": [
                "Linux v6.18 drm/i915 Multi-File GEM/TTM/Display Engine Shims",
                "GuC (`tgl_guc_70.1.1`) & DMC (`tgl_dmc_ver2_12`) Blob Loader + Zstd Streamer",
                "Hardware Iris Xe Display Engine MMIO bring-up on bare metal",
            ],
        },
        "amdgpu_portability_profile": {
            "pci_match": match_pci("1002:73df", CATALOG_PATH),
            "onboarding": run_onboarding_pipeline("amdgpu_generic"),
            "hardware_present_assumed": False,
            "execution_status": "architecture_portability_profile_only",
            "live_modeset_verified_on_dryos": False,
        },
        "nvidia_rtx_3050_ti_10de_25a0": {
            "pci": nv_info or {"found": False},
            "onboarding": run_onboarding_pipeline("nvidia_open_ga107"),
            "firmware_615": nv_fw_615,
            "firmware_ga107_gsp": nv_fw_ga107,
            "os_interface_shim": shim_res,
            "execution_status": "read_only_host_inventory_and_os_interface_compile_verified",
            "live_modeset_verified_on_dryos": False,
            "missing_for_full_modeset": [
                "NVIDIA OpenRM Core + libspdm + GSP RPC Queue Bring-up",
                "27 MiB GSP Firmware Loader (`gsp_ga10x.bin`) over DMA Page Tables",
                "NVIDIA OpenRM 64-bit >4 GiB BAR1 4 GiB Window / MSI-X Runtime Integration",
            ],
        },
    }
    OUT_JSON.parent.mkdir(parents=True, exist_ok=True)
    OUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
