#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Read-only NVIDIA RTX 3050 Ti (`10de:25a0`, GA107M) OpenRM `os-interface.h` & GSP firmware audit.

Enumerates every `NV_API_CALL` prototype from `/usr/src/nvidia-*/kernel-open/common/inc/os-interface.h`,
joins with `drivers/lx-nv-open-shim/nv_os_shim_check.c` (`IMPLEMENTED`, `PARTIAL`, `ENOSYS`,
`COMPILE_ONLY`, remaining `UNSUPPORTED`), and performs streaming SHA-256 + ELF64/WPR2 inspection on
host NVIDIA firmware blobs without opening `/dev/*` or touching host GPUs.
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import pathlib
import re
import struct
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
SHIM_CHECK_C = ROOT / "drivers" / "lx-nv-open-shim" / "nv_os_shim_check.c"
DEFAULT_OUT = ROOT / "tools" / "compat_bench" / "results_nvidia_ga107_audit.json"

SHIM_ENUM_MAP = {
    "NV_SHIM_IMPLEMENTED": "IMPLEMENTED",
    "NV_SHIM_PARTIAL": "PARTIAL",
    "NV_SHIM_STUB_ENOSYS": "ENOSYS",
    "NV_SHIM_COMPILE_ONLY": "COMPILE_ONLY",
}


def _find_os_interface_h() -> pathlib.Path | None:
    for pat in (
        "/usr/src/nvidia-*/kernel-open/common/inc/os-interface.h",
        "/usr/src/nvidia-*/common/inc/os-interface.h",
    ):
        matches = sorted(glob.glob(pat))
        if matches:
            return pathlib.Path(matches[0])
    return None


def _parse_os_interface_protos(header_path: pathlib.Path) -> list[str]:
    text = header_path.read_text(encoding="utf-8", errors="replace")
    # Collapse multi-line prototypes so regex captures single and multi-line NV_API_CALL entries.
    joined = re.sub(r"\s+", " ", text)
    protos: list[str] = []
    seen: set[str] = set()
    for m in re.finditer(r"NV_API_CALL\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", joined):
        sym = m.group(1)
        if sym not in seen:
            seen.add(sym)
            protos.append(sym)
    return protos


def _parse_shim_table(c_path: pathlib.Path) -> dict[str, str]:
    text = c_path.read_text(encoding="utf-8", errors="replace")
    out: dict[str, str] = {}
    for sym, raw_status in re.findall(
        r'\{\s*"([A-Za-z0-9_]+)"\s*,\s*(NV_SHIM_[A-Z0-9_]+)\s*\}', text
    ):
        out[sym] = SHIM_ENUM_MAP.get(raw_status, "UNSUPPORTED")
    return out


def _categorize_api(sym: str) -> tuple[str, str]:
    s = sym.lower()
    if any(k in s for k in ("mem", "page", "map_", "unmap_", "cache", "write_combine", "numa", "offline")):
        return "memory_vm", "Provide via SYS_DMA_CTL / SYS_VSPACE_MAP (ring-3 SLPT); no kernel PFN exposure"
    if any(k in s for k in ("pci", "io_read", "io_write", "flr", "iommu_sva", "dma_buf", "tdisp")):
        return "pci_mmio_io", "Provide via SYS_DEV_MAP(CFG_RW) + SYS_IOPORT_RW (SVA/PASID unimplemented)"
    if any(k in s for k in ("spinlock", "mutex", "semaphore", "rwlock", "wait_queue", "schedule")):
        return "sync_locking", "Provide in Ring-3 via Notification + futex/atomic primitives"
    if any(k in s for k in ("time", "delay", "cpu_frequency")):
        return "time_clocks", "Provide via rdtsc + calibrated SYS_SYSINFO tsc_mhz + SYS_NOTIFY_WAIT"
    if any(k in s for k in ("queue_work", "is_isr", "vgx_msi", "suspend", "cpu_count", "cpu_number", "string", "snprintf", "strtoul", "printf", "out_string")):
        return "threads_strings_irq", "String/IRQ-thread wrappers trivial in C runtime; VFIO/xen-dom0/suspend unsupported"
    if any(k in s for k in ("file", "cgroup", "dmem", "pid", "nv_cap", "imex", "tegra", "grid", "xen", "cert", "smbios", "acpi", "crashlog", "vgpu", "registry")):
        return "linux_vfs_cgroup_sys", "Linux kernel VFS/cgroup/SMBIOS/ACPI-eval specific; omit in MicroVM or UNSUPPORTED natively"
    return "misc_runtime", "Audit case-by-case; return honest -ENOSYS natively"


def _inspect_nvidia_firmware_streaming() -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    paths: list[str] = []
    for pat in (
        "/lib/firmware/nvidia/*/gsp/gsp_ga10x.bin*",
        "/lib/firmware/nvidia/*/ucodes_ga10x.bin*",
    ):
        paths.extend(sorted(glob.glob(pat)))
    for sp in paths:
        p = pathlib.Path(sp)
        h = hashlib.sha256()
        total = 0
        head = b""
        with p.open("rb") as f:
            while True:
                chunk = f.read(1 << 20)
                if not chunk:
                    break
                if not head:
                    head = chunk[:64]
                total += len(chunk)
                h.update(chunk)
        is_elf = head[:4] == b"\x7fELF"
        elf_class = head[4] if is_elf and len(head) >= 5 else 0
        elf_machine = struct.unpack_from("<H", head, 18)[0] if is_elf and len(head) >= 20 else None
        results.append(
            {
                "path": str(p),
                "bytes": total,
                "mib": round(total / (1024.0 * 1024.0), 2),
                "exceeds_legacy_16mib_cap": total > 16 * 1024 * 1024,
                "fw_stream_verifier_required": True,
                "sha256": h.hexdigest(),
                "is_elf": is_elf,
                "elf_class": elf_class,
                "elf_machine": elf_machine,
                "magic_hex": head[:8].hex(),
            }
        )
    return results


def _inspect_ga107_pci_sysfs() -> dict[str, Any]:
    for d in sorted(pathlib.Path("/sys/bus/pci/devices").glob("*")):
        try:
            ven = (d / "vendor").read_text(encoding="utf-8").strip()
            dev = (d / "device").read_text(encoding="utf-8").strip()
            if ven == "0x10de" and dev == "0x25a0":
                cls = (d / "class").read_text(encoding="utf-8").strip()
                res = (d / "resource").read_text(encoding="utf-8").splitlines()
                bars = []
                for idx, ln in enumerate(res[:6]):
                    parts = ln.split()
                    if len(parts) == 3 and parts[0] != "0x0000000000000000":
                        start = int(parts[0], 16)
                        end = int(parts[1], 16)
                        bars.append(
                            {
                                "bar": idx,
                                "start": hex(start),
                                "size_bytes": end - start + 1,
                                "flags": parts[2],
                            }
                        )
                return {"present": True, "bdf": d.name, "class": cls, "bars": bars}
        except OSError:
            continue
    return {"present": False}


def run_audit() -> dict[str, Any]:
    hdr = _find_os_interface_h()
    if hdr is None:
        raise RuntimeError("Missing NVIDIA os-interface.h under /usr/src/nvidia-*")
    all_apis = _parse_os_interface_protos(hdr)
    shim_status = _parse_shim_table(SHIM_CHECK_C)

    classified: list[dict[str, str]] = []
    counts = {"IMPLEMENTED": 0, "PARTIAL": 0, "ENOSYS": 0, "COMPILE_ONLY": 0, "UNSUPPORTED": 0}
    by_subsys: dict[str, dict[str, int]] = {}

    for sym in all_apis:
        st = shim_status.get(sym, "UNSUPPORTED")
        counts[st] += 1
        subsys, feasibility = _categorize_api(sym)
        by_subsys.setdefault(subsys, {"total": 0, "IMPLEMENTED": 0, "PARTIAL": 0, "ENOSYS": 0, "COMPILE_ONLY": 0, "UNSUPPORTED": 0})
        by_subsys[subsys]["total"] += 1
        by_subsys[subsys][st] += 1
        classified.append(
            {
                "symbol": sym,
                "status": st,
                "subsystem": subsys,
                "dryos_feasibility": feasibility,
            }
        )

    return {
        "target_gpu": "NVIDIA GeForce RTX 3050 Ti Laptop GPU (10de:25a0, GA107M, muxless Optimus)",
        "milestone_d_status": "PARTIAL / UNSUPPORTED (Read-only hardware/firmware/ABI audit only; zero NVIDIA GPU or CUDA execution)",
        "os_interface_header": str(hdr),
        "total_os_interface_apis": len(all_apis),
        "status_counts": counts,
        "by_subsystem": by_subsys,
        "pci_sysfs": _inspect_ga107_pci_sysfs(),
        "firmware_streaming_sha256": _inspect_nvidia_firmware_streaming(),
        "apis": classified,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Audit NVIDIA OpenRM os-interface.h and GA107 firmware read-only")
    parser.add_argument("--json-out", type=pathlib.Path, default=DEFAULT_OUT)
    args = parser.parse_args()

    res = run_audit()
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")
    c = res["status_counts"]
    print(
        f"[NV-GA107-AUDIT] total_apis={res['total_os_interface_apis']} "
        f"impl={c['IMPLEMENTED']} partial={c['PARTIAL']} enosys={c['ENOSYS']} "
        f"compile_only={c['COMPILE_ONLY']} unsupported={c['UNSUPPORTED']} "
        f"fw_blobs={len(res['firmware_streaming_sha256'])}"
    )


if __name__ == "__main__":
    main()
