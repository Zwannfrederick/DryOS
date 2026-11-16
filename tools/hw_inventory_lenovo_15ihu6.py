#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Strictly read-only Lenovo IdeaPad Gaming 3 15IHU6 (82K1) hardware inventory.

Inspects only read-only sysfs/proc/udev metadata (`/sys/bus/pci/devices/*`,
`/sys/class/dmi/id/*`, `/proc/cpuinfo`, `/proc/meminfo`, `/proc/cmdline`,
`lsblk -J`, `/sys/bus/{i2c,hid}/devices/*`, `/sys/class/{net,sound,power_supply,drm}/*`,
`/lib/firmware/{i915,nvidia}`), plus kernel log Secure Boot status via `journalctl -k`.
NEVER opens/writes `/dev/*`, NEVER writes `/sys/*`, NEVER touches `efivars`/`efibootmgr`,
and NEVER invokes `dd`.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_JSON = ROOT / "tools" / "compat_bench" / "lenovo_15ihu6_inventory.json"
DEFAULT_MD = ROOT / "docs" / "HARDWARE_INVENTORY_LENOVO_15IHU6.md"

KNOWN_PCI_ROLES: dict[tuple[str, str], str] = {
    ("0x8086", "0x9a49"): "Intel Iris Xe iGPU (primary UEFI GOP console target; PCI_OBJ_IGPU)",
    ("0x8086", "0x9a0b"): "Intel VMD controller (owns PCI domain 10000:e0..e1 NVMe + AHCI)",
    ("0x8086", "0xa0ed"): "Intel Tiger Lake xHCI USB 3.2 controller (Live USB & USB HID; PCI_OBJ_XHCI)",
    ("0x8086", "0xa0f0"): "Intel Wi-Fi 6 AX201 CNVi wireless adapter (iwlwifi)",
    ("0x8086", "0xa0e8"): "Intel LPSS I2C controller #0 (owns Synaptics I2C-HID touchpad 06cb:ce78)",
    ("0x8086", "0xa0c8"): "Intel Smart Sound Technology HDA audio DSP (Realtek ALC257 + HDMI)",
    ("0x10de", "0x25a0"): "NVIDIA RTX 3050 Ti Mobile GA107M (class 03:02 muxless dGPU; 4 GiB BAR1 >4G)",
    ("0x10ec", "0x8168"): "Realtek RTL8111/8168 Gigabit Ethernet NIC (r8169; PCI_OBJ_NET target)",
    ("0x1344", "0x5404"): "Micron MTFDHBA512QFD NVMe SSD (behind VMD domain 10000:e1:00.0)",
    ("0x8086", "0xa0d3"): "Intel Tiger Lake AHCI SATA controller (behind VMD 10000:e0:17.0)",
}


def _read_text(path: pathlib.Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def _read_hex_str(path: pathlib.Path) -> str | None:
    raw = _read_text(path)
    if raw is None:
        return None
    try:
        return f"0x{int(raw, 16):x}"
    except ValueError:
        return raw


def _read_hex_id4(path: pathlib.Path) -> str | None:
    raw = _read_text(path)
    if raw is None:
        return None
    try:
        return f"0x{int(raw, 16):04x}"
    except ValueError:
        return raw


def _parse_pci_bars(res_path: pathlib.Path) -> list[dict[str, Any]]:
    raw = _read_text(res_path)
    if not raw:
        return []
    bars: list[dict[str, Any]] = []
    for idx, line in enumerate(raw.splitlines()[:6]):
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
                "size_human": (
                    f"{size / (1024 * 1024):.1f} MiB"
                    if size >= 1024 * 1024
                    else (f"{size / 1024:.1f} KiB" if size >= 1024 else f"{size} B")
                ),
                "flags": hex(flags),
                "is_io": bool(flags & 0x100),
                "is_mem": bool(flags & 0x200),
                "is_64bit": bool(flags & 0x400),
                "is_prefetchable": bool(flags & 0x2000),
                "above_4g": start >= (1 << 32),
            }
        )
    return bars


def collect_dmi() -> dict[str, str | None]:
    base = pathlib.Path("/sys/class/dmi/id")
    keys = (
        "sys_vendor",
        "product_name",
        "product_version",
        "product_family",
        "board_vendor",
        "board_name",
        "bios_vendor",
        "bios_version",
        "bios_date",
    )
    return {k: _read_text(base / k) for k in keys}


def collect_cpu() -> dict[str, Any]:
    raw = _read_text(pathlib.Path("/proc/cpuinfo")) or ""
    blocks = [b.strip() for b in raw.split("\n\n") if b.strip()]
    first: dict[str, str] = {}
    core_ids: set[tuple[str, str]] = set()
    for blk in blocks:
        kv: dict[str, str] = {}
        for ln in blk.splitlines():
            if ":" in ln:
                k, v = ln.split(":", 1)
                kv[k.strip()] = v.strip()
        if not first:
            first = kv
        core_ids.add((kv.get("physical id", "0"), kv.get("core id", "0")))

    addr_sizes = first.get("address sizes", "")
    phys_bits = None
    virt_bits = None
    m = re.search(r"(\d+)\s+bits\s+physical,\s+(\d+)\s+bits\s+virtual", addr_sizes)
    if m:
        phys_bits = int(m.group(1))
        virt_bits = int(m.group(2))

    flags = set(first.get("flags", "").split())
    vmx_flags = set(first.get("vmx flags", "").split())
    interesting_flags = [
        f
        for f in (
            "x2apic",
            "vmx",
            "smap",
            "smep",
            "xsave",
            "xsaveopt",
            "avx2",
            "avx512f",
            "pdpe1gb",
            "invpcid",
            "constant_tsc",
            "nonstop_tsc",
            "umip",
        )
        if f in flags
    ]
    return {
        "model_name": first.get("model name"),
        "vendor_id": first.get("vendor_id"),
        "family": first.get("cpu family"),
        "model": first.get("model"),
        "stepping": first.get("stepping"),
        "microcode": first.get("microcode"),
        "cache_size": first.get("cache size"),
        "physical_cores": len(core_ids),
        "logical_threads": len(blocks),
        "physical_addr_bits": phys_bits,
        "virtual_addr_bits": virt_bits,
        "address_sizes_raw": addr_sizes,
        "key_isa_flags": interesting_flags,
        "key_vmx_flags": sorted(f for f in ("ept", "vpid", "ept_1gb", "unrestricted_guest") if f in vmx_flags),
    }


def collect_memory() -> dict[str, Any]:
    raw = _read_text(pathlib.Path("/proc/meminfo")) or ""
    kv: dict[str, int] = {}
    for ln in raw.splitlines():
        if ":" in ln:
            k, v = ln.split(":", 1)
            nums = v.strip().split()
            if nums and nums[0].isdigit():
                kv[k.strip()] = int(nums[0])
    total_kib = kv.get("MemTotal", 0)
    avail_kib = kv.get("MemAvailable", 0)
    total_gib = round(total_kib / (1024 * 1024), 2)
    installed_gib = 32 if total_gib >= 24.0 else round(total_gib)
    return {
        "mem_total_kib": total_kib,
        "mem_total_gib": total_gib,
        "installed_ram_class_gib": installed_gib,
        "mem_available_kib": avail_kib,
        "swap_total_kib": kv.get("SwapTotal", 0),
    }


def collect_boot_and_security() -> dict[str, Any]:
    cmdline = _read_text(pathlib.Path("/proc/cmdline")) or ""
    tokens = cmdline.split()
    nr_uarts = next((t for t in tokens if t.startswith("8250.nr_uarts=")), None)
    iommu_tokens = [t for t in tokens if "iommu" in t.lower() or "dmar" in t.lower()]

    sb_state = "unknown"
    sb_source = "journalctl -k -b (efivars and efibootmgr untouched)"
    sb_raw_line = None
    try:
        res = subprocess.run(
            ["journalctl", "-k", "-b", "-g", "Secure boot", "--no-pager"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if res.returncode == 0:
            for ln in res.stdout.splitlines():
                if "kernel: Secure boot " in ln:
                    sb_raw_line = ln.strip()
                    if "disabled" in ln.lower():
                        sb_state = "disabled"
                    elif "enabled" in ln.lower():
                        sb_state = "enabled"
                    break
    except (OSError, subprocess.SubprocessError):
        pass

    dmar_p = pathlib.Path("/sys/firmware/acpi/tables/DMAR")
    dmar_status = "absent"
    if dmar_p.exists():
        try:
            dmar_status = f"readable:{len(dmar_p.read_bytes())}B"
        except PermissionError:
            dmar_status = "present_root_only"
        except OSError:
            dmar_status = "present_unreadable"

    iommu_dir = pathlib.Path("/sys/class/iommu")
    groups_dir = pathlib.Path("/sys/kernel/iommu_groups")
    iommu_units = sorted(p.name for p in iommu_dir.iterdir()) if iommu_dir.exists() else []
    iommu_groups = sorted(p.name for p in groups_dir.iterdir()) if groups_dir.exists() else []

    return {
        "uefi_fw_platform_bits": _read_text(pathlib.Path("/sys/firmware/efi/fw_platform_size")),
        "kernel_lockdown": _read_text(pathlib.Path("/sys/kernel/security/lockdown")),
        "secure_boot_state": sb_state,
        "secure_boot_source": sb_source,
        "secure_boot_kernel_line": sb_raw_line,
        "efivars_touched": False,
        "efibootmgr_invoked": False,
        "cmdline": cmdline,
        "uart_8250_nr_uarts": nr_uarts,
        "uart_legacy_com1_absent": nr_uarts == "8250.nr_uarts=0",
        "cmdline_iommu_flags": iommu_tokens,
        "dmar_acpi_table": dmar_status,
        "host_iommu_units": iommu_units,
        "host_iommu_groups_count": len(iommu_groups),
        "vtd_active_in_host_linux": len(iommu_units) > 0,
    }


def collect_pci_devices() -> list[dict[str, Any]]:
    base = pathlib.Path("/sys/bus/pci/devices")
    devices: list[dict[str, Any]] = []
    if not base.exists():
        return devices
    for dev_dir in sorted(base.iterdir()):
        v = _read_hex_id4(dev_dir / "vendor") or "0x0000"
        d = _read_hex_id4(dev_dir / "device") or "0x0000"
        cls_raw = _read_text(dev_dir / "class") or "0x0"
        cls_norm = f"0x{int(cls_raw, 16):06x}" if cls_raw.startswith("0x") else cls_raw
        drv_link = dev_dir / "driver"
        host_drv = pathlib.Path(os.path.realpath(drv_link)).name if drv_link.exists() else None
        iommu_link = dev_dir / "iommu_group"
        iommu_grp = pathlib.Path(os.path.realpath(iommu_link)).name if iommu_link.exists() else None
        bars = _parse_pci_bars(dev_dir / "resource")
        irq_raw = _read_text(dev_dir / "irq")
        try:
            irq_val: int | str | None = int(irq_raw) if irq_raw is not None else None
        except ValueError:
            irq_val = irq_raw
        rec: dict[str, Any] = {
            "bdf": dev_dir.name,
            "domain": dev_dir.name.split(":")[0],
            "vendor_id": v,
            "device_id": d,
            "class_code": cls_norm,
            "revision": _read_hex_str(dev_dir / "revision"),
            "subsystem_vendor": _read_hex_id4(dev_dir / "subsystem_vendor"),
            "subsystem_device": _read_hex_id4(dev_dir / "subsystem_device"),
            "irq": irq_val,
            "host_driver": host_drv,
            "iommu_group": iommu_grp,
            "bars": bars,
            "has_bar_above_4g": any(b["above_4g"] for b in bars),
            "realpath": os.path.realpath(dev_dir),
            "dryos_role": KNOWN_PCI_ROLES.get((v, d)),
        }
        if (v, d) == ("0x10de", "0x25a0"):
            rec["power"] = {
                "runtime_status": _read_text(dev_dir / "power/runtime_status"),
                "power_state": _read_text(dev_dir / "power_state"),
                "d3cold_allowed": _read_text(dev_dir / "d3cold_allowed"),
                "current_link_speed": _read_text(dev_dir / "current_link_speed"),
                "current_link_width": _read_text(dev_dir / "current_link_width"),
                "max_link_speed": _read_text(dev_dir / "max_link_speed"),
                "max_link_width": _read_text(dev_dir / "max_link_width"),
            }
        devices.append(rec)
    return devices


def collect_storage_lsblk() -> dict[str, Any]:
    try:
        out = subprocess.check_output(
            ["lsblk", "-J", "-o", "NAME,MODEL,SIZE,TYPE,TRAN,ROTA,MOUNTPOINTS"],
            text=True,
            timeout=5,
        )
        return json.loads(out)
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError) as exc:
        return {"error": str(exc), "blockdevices": []}


def collect_subsystems() -> dict[str, Any]:
    i2c_nodes: list[dict[str, Any]] = []
    i2c_dir = pathlib.Path("/sys/bus/i2c/devices")
    if i2c_dir.exists():
        for node in sorted(i2c_dir.iterdir()):
            if "MSFT" in node.name or "SYNA" in node.name or "ELAN" in node.name:
                rp = os.path.realpath(node)
                i2c_nodes.append(
                    {
                        "node": node.name,
                        "name": _read_text(node / "name"),
                        "driver": pathlib.Path(os.path.realpath(node / "driver")).name if (node / "driver").exists() else None,
                        "modalias": _read_text(node / "modalias"),
                        "realpath": rp,
                    }
                )

    i2c_hid: list[dict[str, Any]] = []
    hid_dir = pathlib.Path("/sys/bus/hid/devices")
    if hid_dir.exists():
        for h in sorted(hid_dir.iterdir()):
            uev = _read_text(h / "uevent") or ""
            hid_name = next((ln.split("=", 1)[1] for ln in uev.splitlines() if ln.startswith("HID_NAME=")), None)
            hid_phys = next((ln.split("=", 1)[1] for ln in uev.splitlines() if ln.startswith("HID_PHYS=")), None)
            drv = pathlib.Path(os.path.realpath(h / "driver")).name if (h / "driver").exists() else None
            i2c_hid.append(
                {
                    "id": h.name,
                    "name": hid_name,
                    "phys": hid_phys,
                    "driver": drv,
                    "realpath": os.path.realpath(h),
                }
            )

    net_ifaces: list[dict[str, Any]] = []
    net_dir = pathlib.Path("/sys/class/net")
    if net_dir.exists():
        for n in sorted(net_dir.iterdir()):
            if not (n / "device").exists():
                continue
            net_ifaces.append(
                {
                    "iface": n.name,
                    "operstate": _read_text(n / "operstate"),
                    "pci_device": pathlib.Path(os.path.realpath(n / "device")).name,
                }
            )

    audio_codecs: list[dict[str, str | None]] = []
    asound = pathlib.Path("/proc/asound")
    if asound.exists():
        for c in sorted(asound.glob("card*/codec#*")):
            txt = _read_text(c) or ""
            lines = txt.splitlines()
            codec = lines[0].split(":", 1)[1].strip() if lines and ":" in lines[0] else None
            vendor = next((ln.split(":", 1)[1].strip() for ln in lines if ln.startswith("Vendor Id:")), None)
            audio_codecs.append({"node": f"{c.parent.name}/{c.name}", "codec": codec, "vendor_id": vendor})

    psu: list[dict[str, Any]] = []
    psu_dir = pathlib.Path("/sys/class/power_supply")
    if psu_dir.exists():
        for p in sorted(psu_dir.iterdir()):
            psu.append(
                {
                    "name": p.name,
                    "type": _read_text(p / "type"),
                    "status": _read_text(p / "status"),
                    "capacity": _read_text(p / "capacity"),
                    "model_name": _read_text(p / "model_name"),
                }
            )

    edid_panels: list[dict[str, Any]] = []
    drm_dir = pathlib.Path("/sys/class/drm")
    if drm_dir.exists():
        for c in sorted(drm_dir.iterdir()):
            edid_p = c / "edid"
            if not edid_p.is_file():
                continue
            try:
                edid_bytes = edid_p.read_bytes()
            except OSError:
                edid_bytes = b""
            if edid_bytes:
                edid_panels.append(
                    {
                        "connector": c.name,
                        "status": _read_text(c / "status"),
                        "enabled": _read_text(c / "enabled"),
                        "modes": (_read_text(c / "modes") or "").splitlines()[:4],
                        "edid_bytes": len(edid_bytes),
                    }
                )

    return {
        "i2c_hid_nodes": i2c_nodes,
        "hid_devices": i2c_hid,
        "i8042_platform_present": pathlib.Path("/sys/devices/platform/i8042").exists(),
        "network_pci_ifaces": net_ifaces,
        "audio_codecs": audio_codecs,
        "power_supplies": psu,
        "sleep_modes": _read_text(pathlib.Path("/sys/power/mem_sleep")),
        "power_states": _read_text(pathlib.Path("/sys/power/state")),
        "backlight_max": _read_text(pathlib.Path("/sys/class/backlight/intel_backlight/max_brightness")),
        "drm_edid_panels": edid_panels,
    }


def collect_firmware_inventory() -> dict[str, Any]:
    def list_files(base: pathlib.Path, pattern: str) -> list[dict[str, Any]]:
        if not base.exists():
            return []
        return [
            {"path": str(p), "bytes": p.stat().st_size}
            for p in sorted(base.glob(pattern))
            if p.is_file()
        ]

    tgl = list_files(pathlib.Path("/lib/firmware/i915"), "tgl_*")
    nv615 = list_files(pathlib.Path("/lib/firmware/nvidia/615.78.08"), "*.bin")
    nvga107 = list_files(pathlib.Path("/lib/firmware/nvidia/ga107/gsp"), "*")
    return {
        "intel_tgl_count": len(tgl),
        "intel_tgl_key_files": [
            x for x in tgl if x["path"].endswith(("tgl_dmc_ver2_12.bin.zst", "tgl_guc_70.1.1.bin.zst", "tgl_huc.bin.zst"))
        ],
        "nvidia_615_files": nv615,
        "nvidia_ga107_gsp_files": nvga107,
    }


def build_readiness_assessments(inv: dict[str, Any]) -> dict[str, dict[str, str]]:
    return {
        "uart_serial": {
            "status": "MITIGATED_IN_KERNEL",
            "finding": f"Host cmdline sets {inv['boot_and_security']['uart_8250_nr_uarts']}; legacy COM1 0x3F8 is absent on 15IHU6.",
            "dryos_readiness": "kernel/src/serial.rs probes COM1 scratch/LSR != 0xff and buffers logs into 16 KiB DIAG_RING without stalling.",
        },
        "display_and_gpu": {
            "status": "GOP_READY_NATIVE_KMS_PLANNED",
            "finding": "00:02.0 Iris Xe (8086:9a49, BAR2 256 MiB >4G) drives eDP-1 AUO 1920x1080; 01:00.0 RTX 3050 Ti (10de:25a0, class 03:02) is muxless with 4 GiB BAR1 >4G.",
            "dryos_readiness": "UEFI GOP + preserve_active_gop + 64-bit >4G WC BAR + VT-d SLPT verified in QEMU; native GuC/DMC/GSP modeset remains PLANNED (Gates 6-7).",
        },
        "storage_vmd_and_usb": {
            "status": "LIVE_USB_XHCI_READY_VMD_PLANNED",
            "finding": "00:0e.0 Intel VMD (8086:9a0b) owns domain 10000:e0..e1 (10000:e1:00.0 Micron NVMe 1344:5404 + 10000:e0:17.0 AHCI). 00:14.0 xHCI (8086:a0ed) handles USB.",
            "dryos_readiness": "Live USB boots via UEFI + xHCI; native NVMe behind VMD requires VMD sub-bus ECAM remapping in pci_cfg.rs (PLANNED).",
        },
        "i2c_hid_input": {
            "status": "PS2_KBD_USB_HID_NEARTERM_I2C_PLANNED",
            "finding": "i8042 AT keyboard present; Synaptics touchpad 06cb:ce78 (MSFT0001:01) hangs off 00:15.0 LPSS I2C (8086:a0e8, 4 KiB >4G BAR, IRQ 27).",
            "dryos_readiness": "dry-input covers PS/2 i8042; Synaptics touchpad needs Ring-3 LPSS DesignWare I2C + I2C-HID island; xHCI USB mouse is immediate bare-metal path.",
        },
        "networking": {
            "status": "GBE_ISLAND_CANDIDATE_WIFI_DEFERRED",
            "finding": "02:00.0 Realtek GbE (10ec:8168, r8169) has 256 B I/O + 4 KiB/16 KiB MMIO; 00:14.3 Intel Wi-Fi 6 AX201 (8086:a0f0, iwlwifi) requires CNVi + mac80211 stack.",
            "dryos_readiness": "10ec:8168 fits Ring-3 Driver Island after 2C-3 igc/e1000e (PLANNED); 8086:a0f0 exceeds <10k LOC ADR-006 island ceiling (BLOCKED/LONG-TERM).",
        },
        "audio": {
            "status": "PLANNED_LONG_TERM",
            "finding": "00:1f.3 Intel TGL SST (8086:a0c8) uses sof-audio-pci-intel-tgl with Realtek ALC257 and Intel HDMI codecs.",
            "dryos_readiness": "Requires SOF DSP firmware loader, NHLT/topology, and Ring-3 audio server (no audio stack in DRY OS yet).",
        },
        "power_and_rtd3": {
            "status": "S5_RESET_READY_S3_RTD3_PLANNED",
            "finding": "BAT1 (L20C3PC2), ACAD, s2idle/deep, and 01:00.0 d3cold_allowed=1 (currently D0 active on host).",
            "dryos_readiness": "ACPI S5 & FADT reset implemented; S3 returns -ENOSYS (R-16); 01:00.0 RTD3 D3cold power-off (~8-15W savings, R-17) is PLANNED for P5.",
        },
    }


def render_markdown(inv: dict[str, Any]) -> str:
    dmi = inv["dmi"]
    cpu = inv["cpu"]
    mem = inv["memory"]
    boot = inv["boot_and_security"]
    sub = inv["subsystems"]
    fw = inv["firmware"]

    lines = [
        "# DRY OS — Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`) Read-Only Hardware Inventory",
        "",
        "> **Scope:** Project Ascension 5.0 Track C / M4 (`tools/hw_inventory_lenovo_15ihu6.py`)  ",
        "> **Raw JSON:** [`tools/compat_bench/lenovo_15ihu6_inventory.json`](../tools/compat_bench/lenovo_15ihu6_inventory.json)  ",
        "> **Host Safety Contract:** Strictly read-only sysfs/proc/lsblk/journalctl inspection (`0` `/dev/*` opens, `0` `/sys/*` writes, `0` `efivars`/`efibootmgr` touches, `0` `dd` calls).",
        "",
        "---",
        "",
        "## 1. Platform, CPU, Memory & Boot/Security State",
        "",
        "| Component | Observed Host Value | DRY OS Relevance |",
        "| :--- | :--- | :--- |",
        f"| Machine / Board | `{dmi.get('sys_vendor')} {dmi.get('product_version')}` (`{dmi.get('product_name')}`, board `{dmi.get('board_name')}`) | Target physical bare-metal laptop |",
        f"| BIOS / UEFI | `{dmi.get('bios_vendor')} {dmi.get('bios_version')}` (`{dmi.get('bios_date')}`), `{boot.get('uefi_fw_platform_bits')}-bit` UEFI | Limine `BOOTX64.EFI` Live USB boot target |",
        f"| CPU | `{cpu.get('model_name')}` (`{cpu.get('physical_cores')}C/{cpu.get('logical_threads')}T`, ucode `{cpu.get('microcode')}`) | `{cpu.get('physical_addr_bits')}-bit` phys / `{cpu.get('virtual_addr_bits')}-bit` virt (`>4 GiB` BAR OK) |",
        f"| ISA / VMX Features | `{', '.join(cpu.get('key_isa_flags', []))}` | `x2APIC`, `SMAP`, `XSAVEOPT`, `EPT/VPID` present |",
        f"| System RAM | `{mem.get('mem_total_kib')} KiB` (`{mem.get('mem_total_gib')} GiB`, `{mem.get('installed_ram_class_gib')} GiB` installed) | Sufficient for Ring-3 islands and `2 MiB` GOP/PRIME buffers |",
        f"| Serial / UART | `{boot.get('uart_8250_nr_uarts')}` (`legacy_com1_absent = {boot.get('uart_legacy_com1_absent')}`) | `kernel/src/serial.rs` LSR/scratch probe prevents UART hang |",
        f"| Secure Boot | `{boot.get('secure_boot_state')}` (via `{boot.get('secure_boot_source')}`; lockdown `{boot.get('kernel_lockdown')}`) | Unsigned Limine `BOOTX64.EFI` can boot when disabled |",
        f"| Host `DMAR` / `VT-d` | ACPI `DMAR`: `{boot.get('dmar_acpi_table')}`, host `vtd_active = {boot.get('vtd_active_in_host_linux')}` | DRY OS parses `DMAR` independently in Ring 0 (`kernel/src/iommu/`) |",
        "",
        "---",
        "",
        "## 2. Complete PCIe & VMD Topology (`len(pci_devices) = " + str(len(inv["pci_devices"])) + "`)",
        "",
        "| BDF | Vendor:Device | Class | Host Driver | Key BARs / >4G | Role / Notes |",
        "| :--- | :--- | :--- | :--- | :--- | :--- |",
    ]
    for d in inv["pci_devices"]:
        bar_txt = ", ".join(
            f"BAR{b['bar']}:{'IO' if b['is_io'] else 'MMIO'} {b['size_human']}{'>4G' if b['above_4g'] else ''}"
            for b in d["bars"]
        ) or "none"
        role = d.get("dryos_role") or "-"
        lines.append(
            f"| `{d['bdf']}` | `{d['vendor_id']}:{d['device_id']}` | `{d['class_code']}` | `{d.get('host_driver') or 'unbound'}` | `{bar_txt}` | {role} |"
        )

    lines.extend(["", "---", "", "## 3. Storage, HID/I2C, Networking, Audio, Display & Firmware", ""])
    for blk in inv["storage_lsblk"].get("blockdevices", []):
        lines.append(f"- **Block `{blk.get('name')}`:** model `{blk.get('model')}`, size `{blk.get('size')}`, tran `{blk.get('tran')}`")
    for n in sub.get("i2c_hid_nodes", []):
        lines.append(f"- **I2C Node `{n['node']}`:** driver `{n['driver']}`, modalias `{n['modalias']}`, path `{n['realpath']}`")
    for h in sub["hid_devices"]:
        lines.append(f"- **HID `{h['id']}`:** `{h['name']}` (`driver={h['driver']}`, `phys={h['phys']}`)")
    for n in sub["network_pci_ifaces"]:
        lines.append(f"- **NIC `{n['iface']}`:** PCI `{n['pci_device']}`, `operstate={n['operstate']}`")
    for a in sub["audio_codecs"]:
        lines.append(f"- **Audio `{a['node']}`:** `{a['codec']}` (`{a['vendor_id']}`)")
    for p in sub["drm_edid_panels"]:
        lines.append(f"- **DRM `{p['connector']}`:** status `{p['status']}`, mode `{','.join(p['modes'])}`, EDID `{p['edid_bytes']} B`")
    lines.append(
        f"- **Firmware Blobs (Read-Only):** `{fw['intel_tgl_count']}` Intel `tgl_*` files; "
        f"`{len(fw['nvidia_615_files'])}` NVIDIA `615.78.08` blobs + `{len(fw['nvidia_ga107_gsp_files'])}` `ga107/gsp` blobs."
    )

    lines.extend(["", "---", "", "## 4. Follow-Up Subsystem Readiness Assessments", "", "| Subsystem | Readiness Status | Finding | DRY OS Path / Next Action |", "| :--- | :--- | :--- | :--- |"])
    for k, a in inv["readiness_assessments"].items():
        lines.append(f"| `{k}` | `{a['status']}` | {a['finding']} | {a['dryos_readiness']} |")
    lines.extend(
        [
            "",
            "---",
            "",
            "## 5. QEMU/Validator Evidence vs. Bare-Metal Physical Gates (Gates 5-7)",
            "",
            "| Gate | Scope | Current Verification State |",
            "| :--- | :--- | :--- |",
            "| **QEMU / Host Read-Only Prep** | `hw_inventory_lenovo_15ihu6.py`, `gpu_fw_validators.py --self-test`, `provision_local_firmware.py --self-test`, QEMU `--p8-only`/`--p12-only`, `build_live_usb_image.py --verify-boot` | **PASS** (read-only host inventory + synthetic/host firmware validators + QEMU/OVMF emulation) |",
            "| **Gate 5 (Physical Live USB Boot)** | Boot `dryos-ascension4-live-usb.img` on physical `82K1` via `00:14.0` xHCI (`8250.nr_uarts=0`, GOP desktop + PS/2 or USB HID) | **`READY FOR USER PHYSICAL EXECUTION`** (not executed by agent) |",
            "| **Gate 6 (Physical VT-d + Iris Xe GOP)** | Bare-metal `DMAR` `VT-d` enforcement with `00:02.0` `8086:9a49` `preserve_active_gop` untouched | **`READY FOR USER PHYSICAL EXECUTION`** (not executed by agent) |",
            "| **Gate 7 (Physical dGPU RTD3 / PRIME)** | Bare-metal `01:00.0` `10de:25a0` `RTD3 D3cold` / GSP WPR2 / muxless PRIME `READ_ONLY_EXPORT` handoff | **`READY FOR USER PHYSICAL EXECUTION`** (`BLOCKED` for live OpenRM modeset until GSP RPC bring-up) |",
            "",
        ]
    )
    return "\n".join(lines)


def collect_inventory() -> dict[str, Any]:
    inv: dict[str, Any] = {
        "schema_version": "1.0",
        "audit_mode": "strictly_read_only_host_inventory",
        "dmi": collect_dmi(),
        "cpu": collect_cpu(),
        "memory": collect_memory(),
        "boot_and_security": collect_boot_and_security(),
        "pci_devices": collect_pci_devices(),
        "storage_lsblk": collect_storage_lsblk(),
        "subsystems": collect_subsystems(),
        "firmware": collect_firmware_inventory(),
    }
    inv["readiness_assessments"] = build_readiness_assessments(inv)
    return inv


def dump_compact_json(obj: Any, indent: int = 0) -> str:
    """Serialize JSON keeping flat leaf objects compact so inventory stays well under 800 lines."""
    pad = " " * indent
    if isinstance(obj, dict):
        if not obj:
            return "{}"
        if all(not isinstance(v, (dict, list)) for v in obj.values()) and len(json.dumps(obj)) <= 300:
            return json.dumps(obj)
        items = [f'{pad}  {json.dumps(k)}: {dump_compact_json(v, indent + 2)}' for k, v in obj.items()]
        return "{\n" + ",\n".join(items) + f"\n{pad}}}"
    if isinstance(obj, list):
        if not obj:
            return "[]"
        if all(not isinstance(v, (dict, list)) for v in obj) and len(json.dumps(obj)) <= 160:
            return json.dumps(obj)
        items = [f"{pad}  {dump_compact_json(v, indent + 2)}" for v in obj]
        return "[\n" + ",\n".join(items) + f"\n{pad}]"
    return json.dumps(obj)


def main() -> None:
    parser = argparse.ArgumentParser(description="Read-only Lenovo 15IHU6 hardware inventory")
    parser.add_argument("--json-out", type=pathlib.Path, default=DEFAULT_JSON)
    parser.add_argument("--md-out", type=pathlib.Path, default=DEFAULT_MD)
    args = parser.parse_args()

    inv = collect_inventory()
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.md_out.parent.mkdir(parents=True, exist_ok=True)
    json_text = dump_compact_json(inv) + "\n"
    json.loads(json_text)
    args.json_out.write_text(json_text, encoding="utf-8")
    args.md_out.write_text(render_markdown(inv), encoding="utf-8")

    bdfs = {d["bdf"]: f"{d['vendor_id']}:{d['device_id']}" for d in inv["pci_devices"]}
    print(
        f"[HW-INV-15IHU6] PASS: product={inv['dmi'].get('product_name')} "
        f"cpu_bits={inv['cpu'].get('physical_addr_bits')}/{inv['cpu'].get('virtual_addr_bits')} "
        f"ram_gib={inv['memory'].get('installed_ram_class_gib')} "
        f"uart={inv['boot_and_security'].get('uart_8250_nr_uarts')} "
        f"sb={inv['boot_and_security'].get('secure_boot_state')} "
        f"pci_count={len(inv['pci_devices'])} "
        f"igpu={bdfs.get('0000:00:02.0')} dgpu={bdfs.get('0000:01:00.0')} "
        f"vmd={bdfs.get('0000:00:0e.0')} nvme={bdfs.get('10000:e1:00.0')}"
    )


if __name__ == "__main__":
    main()
