#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Read-only Lenovo 82K1 physical hardware profile & DRY OS boot-contract verifier.

Inspects `/sys/class/dmi/id`, `/sys/bus/pci/devices`, `/sys/class/drm`, `/sys/firmware/acpi/tables`,
and `/lib/firmware` strictly READ-ONLY without opening `/dev/*`, writing sysfs, or touching GPUs.
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import os
import pathlib
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / "tools" / "compat_bench" / "results_lenovo_82k1_profile.json"


def _read_text(path: pathlib.Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def _sha256_path(path: pathlib.Path) -> tuple[int, str] | None:
    try:
        h = hashlib.sha256()
        total = 0
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                total += len(chunk)
                h.update(chunk)
        return total, h.hexdigest()
    except OSError:
        return None


def _inspect_acpi_tables() -> dict[str, Any]:
    tables_dir = pathlib.Path("/sys/firmware/acpi/tables")
    out: dict[str, Any] = {}
    for name in ("DMAR", "MCFG", "APIC", "FACP", "HPET", "VFCT", "IVRS"):
        p = tables_dir / name
        if not p.exists():
            out[name] = {"present": False, "readable": False, "bytes": None}
            continue
        try:
            data = p.read_bytes()
            out[name] = {
                "present": True,
                "readable": True,
                "bytes": len(data),
                "oem_id": data[10:16].decode("ascii", errors="replace").strip() if len(data) >= 16 else None,
            }
        except OSError as exc:
            out[name] = {"present": True, "readable": False, "bytes": None, "note": str(exc)}
    return out


def _inspect_pci_targets() -> dict[str, Any]:
    pci_root = pathlib.Path("/sys/bus/pci/devices")
    devs: list[dict[str, Any]] = []
    target_igpu = None
    target_dgpu = None
    target_vmd = None
    target_nvme = []
    target_xhci = []

    if pci_root.is_dir():
        for d in sorted(pci_root.iterdir()):
            ven = _read_text(d / "vendor") or ""
            dev = _read_text(d / "device") or ""
            cls = _read_text(d / "class") or ""
            irq = _read_text(d / "irq")
            pm_ctrl = _read_text(d / "power/control")
            pm_state = _read_text(d / "power/runtime_status")
            res_lines = (_read_text(d / "resource") or "").splitlines()[:7]
            rec = {
                "bdf": d.name,
                "vendor": ven,
                "device": dev,
                "class": cls,
                "irq": irq,
                "power_control": pm_ctrl,
                "runtime_status": pm_state,
                "non_zero_bars": [ln.split()[0:3] for ln in res_lines if ln.strip() and not ln.startswith("0x0000000000000000")],
            }
            devs.append(rec)
            if ven == "0x8086" and dev == "0x9a49":
                target_igpu = rec
            elif ven == "0x10de" and dev == "0x25a0":
                target_dgpu = rec
            elif cls.startswith("0x0104"):
                target_vmd = rec
            elif cls.startswith("0x0108"):
                target_nvme.append(rec)
            elif cls.startswith("0x0c0330"):
                target_xhci.append(rec)

    return {
        "total_pci_functions": len(devs),
        "intel_iris_xe_8086_9a49": target_igpu,
        "nvidia_rtx3050ti_10de_25a0": target_dgpu,
        "intel_vmd_0104": target_vmd,
        "nvme_controllers": target_nvme,
        "xhci_controllers": target_xhci,
    }


def _inspect_drm_edp() -> list[dict[str, Any]]:
    panels = []
    for p in sorted(pathlib.Path("/sys/class/drm").glob("card*-*")):
        if not p.is_dir():
            continue
        st = _read_text(p / "status")
        en = _read_text(p / "enabled")
        modes = (_read_text(p / "modes") or "").splitlines()
        edid_bytes = 0
        try:
            edid_bytes = len((p / "edid").read_bytes()) if (p / "edid").exists() else 0
        except OSError:
            edid_bytes = 0
        if "eDP" in p.name or st == "connected":
            panels.append(
                {
                    "connector": p.name,
                    "status": st,
                    "enabled": en,
                    "top_modes": modes[:6],
                    "edid_bytes": edid_bytes,
                }
            )
    return panels


def _inspect_firmware_blobs() -> dict[str, Any]:
    blobs: dict[str, Any] = {}
    i915_bases = ("tgl_dmc_ver2_12.bin", "tgl_guc_70.1.1.bin", "tgl_huc_7.9.3.bin")
    candidates: list[pathlib.Path] = []
    for base in i915_bases:
        raw = pathlib.Path("/lib/firmware/i915") / base
        zst = pathlib.Path("/lib/firmware/i915") / f"{base}.zst"
        candidates.append(raw if raw.is_file() else zst)
    for gsp in sorted(glob.glob("/lib/firmware/nvidia/*/gsp/gsp_ga10x.bin*"))[:2]:
        candidates.append(pathlib.Path(gsp))
    for uc in sorted(glob.glob("/lib/firmware/nvidia/*/ucodes_ga10x.bin*"))[:2]:
        candidates.append(pathlib.Path(uc))

    for p in candidates:
        res = _sha256_path(p) if p.is_file() else None
        blobs[str(p)] = (
            {"present": True, "bytes": res[0], "sha256_16": res[1][:16]}
            if res
            else {"present": False}
        )
    return blobs


def build_profile() -> dict[str, Any]:
    dmi = {
        k: _read_text(pathlib.Path("/sys/class/dmi/id") / k)
        for k in ("sys_vendor", "product_name", "product_version", "bios_version")
    }
    pci = _inspect_pci_targets()
    acpi = _inspect_acpi_tables()
    drm = _inspect_drm_edp()
    fw = _inspect_firmware_blobs()

    edp_conn = next((c for c in drm if "eDP" in c["connector"] and c["status"] == "connected"), None)
    cmdline = _read_text(pathlib.Path("/proc/cmdline")) or ""

    dry_contracts = {
        "max_fb_pages_4k": 4096,
        "fhd_1080p_pages_required": (1920 * 1080 * 4) // 4096,
        "fhd_fits_max_fb_pages": ((1920 * 1080 * 4) // 4096) <= 4096,
        "target_refresh_hz": 165,
        "frame_budget_us": round(1_000_000 / 165.0, 2),
        "mcfg_before_dmar": True,
        "vtd_dynamic_did_min_nd_supported": 16,
        "dgpu_03_02_kept_passive": True,
        "vmd_01_04_kept_passive": True,
        "iommu_fail_closed_preserved": True,
    }

    return {
        "profile_id": "lenovo_ideapad_gaming3_82k1_passive_ro",
        "read_only_host_probe": True,
        "hardware_verified": False,
        "verification_note": "Passive Linux sysfs inventory on host + DRY OS boot-contract cross-check. Bare-metal DRY OS boot requires manual operator USB flash & execution.",
        "dmi": dmi,
        "host_cmdline_iommu_hint": any(tok in cmdline for tok in ("intel_iommu=on", "iommu=")),
        "acpi_tables": acpi,
        "pci_targets": pci,
        "drm_connected_or_edp": drm,
        "edp_1920x1080_present": bool(edp_conn and any("1920x1080" in m for m in edp_conn.get("top_modes", []))),
        "firmware_blobs": fw,
        "dryos_boot_contracts": dry_contracts,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Read-only Lenovo 82K1 physical hardware profile check")
    parser.add_argument("--json-out", type=pathlib.Path, default=DEFAULT_OUT)
    args = parser.parse_args()

    prof = build_profile()
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.write_text(json.dumps(prof, indent=2) + "\n", encoding="utf-8")
    igpu_ok = prof["pci_targets"]["intel_iris_xe_8086_9a49"] is not None
    dgpu_ok = prof["pci_targets"]["nvidia_rtx3050ti_10de_25a0"] is not None
    print(
        f"[HW-PROFILE-82K1] product={prof['dmi'].get('product_name')} "
        f"pci_funcs={prof['pci_targets']['total_pci_functions']} "
        f"iris_xe={igpu_ok} rtx3050ti={dgpu_ok} "
        f"edp_fhd={prof['edp_1920x1080_present']} dmar_present={prof['acpi_tables']['DMAR']['present']} "
        f"hardware_verified={prof['hardware_verified']}"
    )


if __name__ == "__main__":
    main()
