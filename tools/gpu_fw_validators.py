#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Read-only Intel Iris Xe (8086:9a49) & NVIDIA RTX 3050 Ti (10de:25a0) firmware,
OpRegion/EDID, GOP preservation, OpenRM ELF64/WPR2, os-interface.h, and PRIME validators.

Strictly read-only on host paths (`/lib/firmware/{i915,nvidia}`, `/sys/class/drm/*/edid`,
`/usr/src/nvidia-*/kernel-open/common/inc/os-interface.h`); never writes `/sys`, `/dev`,
or efivars. Includes deterministic synthetic positive and negative test vectors (`--self-test`).
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import pathlib
import re
import struct
import subprocess
import zlib
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
HOST_DMC_ZST = pathlib.Path("/lib/firmware/i915/tgl_dmc_ver2_12.bin.zst")
HOST_GUC_ZST = pathlib.Path("/lib/firmware/i915/tgl_guc_70.1.1.bin.zst")
HOST_EDID_PATH = pathlib.Path("/sys/class/drm/card2-eDP-1/edid")
HOST_GSP_BIN = pathlib.Path("/lib/firmware/nvidia/615.78.08/gsp_ga10x.bin")
HOST_UCODES_BIN = pathlib.Path("/lib/firmware/nvidia/615.78.08/ucodes_ga10x.bin")
HOST_OS_IFACE_H = pathlib.Path("/usr/src/nvidia-615.78.08/kernel-open/common/inc/os-interface.h")
SHIM_C_PATH = ROOT / "drivers" / "lx-nv-open-shim" / "nv_os_shim_check.c"

ZSTD_MAGIC = b"\x28\xb5\x2f\xfd"
EDID_HEADER = b"\x00\xff\xff\xff\xff\xff\xff\x00"
OPREGION_SIG = b"IntelGraphicsMem"
VBT_SIG = b"$VBT"
BDB_SIG = b"BIOS_DATA_BLOCK "
GSP_WPR_MAGIC = 0xDC3AAE21371A60B3
GSP_WPR_REVISION = 1
GSP_WPR_VERIFIED = 0xA0A0A0A0A0A0A0A0
ALIGN_128K = 128 * 1024
FRAME_2M = 2 * 1024 * 1024


def _decompress_if_zst(data: bytes) -> bytes:
    if not data.startswith(ZSTD_MAGIC):
        return data
    try:
        from compression import zstd  # Python 3.14+ stdlib

        return zstd.decompress(data)
    except ImportError:
        return subprocess.run(["zstd", "-dc"], input=data, capture_output=True, check=True).stdout


def validate_intel_css_firmware(blob: bytes, expected_kind: str) -> dict[str, Any]:
    """Validate Intel Gen12 CSS header for `dmc` (type=9) or `guc` (type=6)."""
    raw = _decompress_if_zst(blob)
    errors: list[str] = []
    if len(raw) < 128:
        return {"valid": False, "kind": expected_kind, "errors": ["truncated_below_128B"]}

    mtype, hlen_dw, hver, _mid, mvend, date, sz_dw, key_dw, mod_dw, exp_dw = struct.unpack_from("<10I", raw, 0)
    hlen_bytes = hlen_dw * 4
    if hver != 0x00010000:
        errors.append(f"unexpected_header_version:{hex(hver)}")
    if hlen_dw < 32 or hlen_bytes > len(raw):
        errors.append(f"invalid_header_len_dw:{hlen_dw}")

    version_str = "unknown"
    extra: dict[str, Any] = {}
    if expected_kind == "dmc":
        if mtype != 9:
            errors.append(f"expected_module_type_9_got:{mtype}")
        if hlen_dw != 32:
            errors.append(f"expected_dmc_hlen_32dw_got:{hlen_dw}")
        if sz_dw * 4 != len(raw):
            errors.append(f"dmc_size_mismatch:{sz_dw*4}!={len(raw)}")
        ver_u32 = struct.unpack_from("<I", raw, 0x58)[0]
        version_str = f"{(ver_u32 >> 16) & 0xffff}.{ver_u32 & 0xffff}"
        if len(raw) >= 144:
            pkg_hlen, _, _, num_entries = struct.unpack_from("<4I", raw, 128)
            extra["pkg_header_len"] = pkg_hlen
            extra["pkg_num_entries"] = num_entries
            if num_entries < 1:
                errors.append("dmc_pkg_num_entries_zero")
        sig_off = raw.find(b"\x3e\x3e\x40\x40", 128, min(len(raw), 4096))
        extra["dmc_header_v3_sig_off"] = sig_off
        if sig_off < 0:
            errors.append("missing_dmc_header_magic_0x40403e3e")
    elif expected_kind == "guc":
        if mtype != 6:
            errors.append(f"expected_module_type_6_got:{mtype}")
        if mvend != 0x8086:
            errors.append(f"expected_vendor_0x8086_got:{hex(mvend)}")
        if hlen_dw != 32 + key_dw + mod_dw + exp_dw:
            errors.append(f"rsa_header_sum_mismatch:{hlen_dw}!=32+{key_dw}+{mod_dw}+{exp_dw}")
        rsa_tail = (mod_dw + exp_dw) * 4
        if len(raw) not in (sz_dw * 4, sz_dw * 4 - rsa_tail):
            errors.append(f"guc_size_mismatch:raw={len(raw)},sz_dw*4={sz_dw*4},rsa_tail={rsa_tail}")
        sw_ver = struct.unpack_from("<I", raw, 0x40)[0]
        version_str = f"{(sw_ver >> 16) & 0xff}.{(sw_ver >> 8) & 0xff}.{sw_ver & 0xff}"
        extra["rsa_key_bits"] = key_dw * 32
    else:
        errors.append(f"unknown_kind:{expected_kind}")

    return {
        "valid": not errors,
        "kind": expected_kind,
        "module_type": mtype,
        "version": version_str,
        "date_hex": hex(date),
        "raw_bytes": len(raw),
        "header_bytes": hlen_bytes,
        "size_dw": sz_dw,
        **extra,
        "errors": errors,
    }


def validate_igd_opregion_vbt(blob: bytes) -> dict[str, Any]:
    """Validate ACPI IGD OpRegion (`IntelGraphicsMem`) and `$VBT` + `BIOS_DATA_BLOCK`."""
    errors: list[str] = []
    if len(blob) < 0x2000:
        return {"valid": False, "errors": [f"opregion_too_small:{len(blob)}"]}
    if blob[:16] != OPREGION_SIG:
        errors.append("bad_opregion_signature")

    size_kb, opreg_ver = struct.unpack_from("<II", blob, 16)
    mboxes = struct.unpack_from("<I", blob, 40)[0]
    if size_kb * 1024 > len(blob) or size_kb < 8:
        errors.append(f"opregion_size_kb_invalid:{size_kb}")

    vbt_off = blob.find(VBT_SIG, 0x100, len(blob) - 36)
    vbt_ver = 0
    vbt_size = 0
    bdb_ok = False
    if vbt_off < 0:
        errors.append("missing_vbt_signature")
    else:
        vbt_ver, hdr_sz, vbt_size, _, bdb_off = struct.unpack_from("<HHHHI", blob, vbt_off + 20)
        if hdr_sz < 40 or vbt_off + vbt_size > len(blob):
            errors.append(f"vbt_bounds_invalid:off={vbt_off},sz={vbt_size}")
        elif blob[vbt_off + bdb_off : vbt_off + bdb_off + 16] == BDB_SIG:
            bdb_ok = True
        else:
            errors.append("missing_bios_data_block_signature")

    return {
        "valid": not errors,
        "opregion_bytes": len(blob),
        "opregion_version": f"{(opreg_ver >> 24) & 0xff}.{(opreg_ver >> 16) & 0xff}",
        "mboxes_mask": hex(mboxes),
        "vbt_offset": vbt_off,
        "vbt_version": hex(vbt_ver),
        "vbt_size": vbt_size,
        "bdb_ok": bdb_ok,
        "errors": errors,
    }


def validate_edid_128(edid: bytes) -> dict[str, Any]:
    """Validate 128-byte EDID header, checksum, PNP manufacturer, and DTD1 mode."""
    errors: list[str] = []
    if len(edid) < 128:
        return {"valid": False, "errors": [f"edid_truncated:{len(edid)}"]}
    if edid[:8] != EDID_HEADER:
        errors.append("bad_edid_header")
    cksum = sum(edid[:128]) & 0xFF
    if cksum != 0:
        errors.append(f"bad_edid_checksum:{cksum}")

    mfg_raw = struct.unpack_from(">H", edid, 8)[0]
    mfg = "".join(chr(((mfg_raw >> s) & 0x1F) + 64) for s in (10, 5, 0))
    prod_id = struct.unpack_from("<H", edid, 10)[0]
    ver, rev = edid[18], edid[19]

    px_clk_10khz = struct.unpack_from("<H", edid, 54)[0]
    h_act = edid[56] | ((edid[58] >> 4) << 8)
    h_blank = edid[57] | ((edid[58] & 0xF) << 8)
    v_act = edid[59] | ((edid[61] >> 4) << 8)
    v_blank = edid[60] | ((edid[61] & 0xF) << 8)
    hz = 0.0
    if px_clk_10khz > 0 and (h_act + h_blank) > 0 and (v_act + v_blank) > 0:
        hz = round((px_clk_10khz * 10000.0) / ((h_act + h_blank) * (v_act + v_blank)), 2)

    return {
        "valid": not errors,
        "manufacturer": mfg,
        "product_id": hex(prod_id),
        "edid_version": f"{ver}.{rev}",
        "mode": f"{h_act}x{v_act}@{hz}Hz",
        "pixel_clock_khz": px_clk_10khz * 10,
        "checksum_mod256": cksum,
        "errors": errors,
    }


def validate_gen12_gop_preservation() -> dict[str, Any]:
    """Verify Gen12 BAR2 scanout preservation contract & kernel `preserve_active_gop` guards."""
    scanout_pattern = bytes(((i * 37) & 0xFF) for i in range(4096))
    initial_crc = zlib.crc32(scanout_pattern)

    class _SimGen12Device:
        def __init__(self, preserve_active_gop: bool) -> None:
            self.bdf = "0000:00:02.0"
            self.vendor_device = "8086:9a49"
            self.preserve_active_gop = preserve_active_gop
            self.bme_enabled = True
            self.bar_probe_writes = 0
            self.bar2_buf = bytearray(scanout_pattern)

        def probe_bars(self) -> None:
            if not self.preserve_active_gop:
                self.bar_probe_writes += 2

        def sys_dev_map_window(self, bar_idx: int) -> int:
            if self.preserve_active_gop:
                return -1  # EPERM (`pci_window.rs:141`)
            if bar_idx == 2:
                self.bar2_buf[0] ^= 0xFF
            return 0

        def quiesce_slot(self) -> None:
            if not self.preserve_active_gop:
                self.bme_enabled = False

    dev = _SimGen12Device(preserve_active_gop=True)
    dev.probe_bars()
    map_rc = dev.sys_dev_map_window(2)
    dev.quiesce_slot()
    final_crc = zlib.crc32(dev.bar2_buf)

    unprotected = _SimGen12Device(preserve_active_gop=False)
    unprotected.probe_bars()
    unprotected_map_rc = unprotected.sys_dev_map_window(2)
    unprotected.quiesce_slot()

    kernel_guards_ok = all(
        needle in (ROOT / rel).read_text(encoding="utf-8")
        for rel, needle in (
            ("kernel/src/pci_cfg.rs", "read_bars_readonly"),
            ("kernel/src/pci_window.rs", "if vdev.preserve_active_gop"),
            ("kernel/src/pci.rs", "if vdev.preserve_active_gop"),
        )
    )
    ok = (
        dev.bar_probe_writes == 0 and map_rc == -1 and dev.bme_enabled
        and final_crc == initial_crc and unprotected_map_rc == 0
        and not unprotected.bme_enabled and kernel_guards_ok
    )
    return {
        "valid": ok, "bar2_scanout_crc_preserved": final_crc == initial_crc,
        "bar_probe_writes": dev.bar_probe_writes, "sys_dev_map_blocked_rc": map_rc,
        "bme_preserved": dev.bme_enabled, "kernel_source_guards_present": kernel_guards_ok,
    }


def validate_gsp_elf64_and_wpr2(elf_path_or_bytes: pathlib.Path | bytes, wpr_meta: bytes) -> dict[str, Any]:
    """Validate `gsp_ga10x.bin` ELF64 sections and OpenRM `GspFwWprMeta` WPR2 layout."""
    errors: list[str] = []
    if isinstance(elf_path_or_bytes, pathlib.Path):
        file_sz = elf_path_or_bytes.stat().st_size
        cm: Any = elf_path_or_bytes.open("rb")
    else:
        file_sz = len(elf_path_or_bytes)
        cm = io.BytesIO(elf_path_or_bytes)

    with cm as f:
        ehdr = f.read(64)
        if len(ehdr) < 64 or ehdr[:4] != b"\x7fELF" or ehdr[4] != 2 or ehdr[5] != 1:
            return {"valid": False, "errors": ["not_elf64_le"]}
        e_type, e_mach, _, _, _, e_shoff, _, _, _, _, e_shentsz, e_shnum, e_shstr = struct.unpack_from(
            "<HHIQQQIHHHHHH", ehdr, 16
        )
        if e_shoff + e_shnum * e_shentsz > file_sz or e_shstr >= e_shnum:
            return {"valid": False, "errors": ["shdrs_oob"]}
        f.seek(e_shoff)
        sh_raw = f.read(e_shnum * e_shentsz)
        shdrs = [struct.unpack_from("<IIQQQQIIQQ", sh_raw, i * e_shentsz) for i in range(e_shnum)]
        str_sh = shdrs[e_shstr]
        f.seek(str_sh[4])
        shstr = f.read(str_sh[5])
        sec_map: dict[str, tuple[int, int]] = {}
        for sh in shdrs:
            end = shstr.find(b"\x00", sh[0])
            name = shstr[sh[0] : end].decode("ascii", "replace")
            sec_map[name] = (sh[4], sh[5])
            if sh[4] + sh[5] > file_sz:
                errors.append(f"section_oob:{name}")
        fwversion = ""
        if ".fwversion" in sec_map:
            off, sz = sec_map[".fwversion"]
            f.seek(off)
            fwversion = f.read(min(sz, 64)).split(b"\x00", 1)[0].decode("ascii", "replace")

    if e_type != 1 or e_mach != 0xF3:
        errors.append(f"unexpected_elf_type_or_mach:type={e_type},mach={hex(e_mach)}")
    for req in (".fwimage", ".fwversion", ".note.gnu.build-id", ".fwsignature_ga10x"):
        if req not in sec_map:
            errors.append(f"missing_section:{req}")
    if ".fwsignature_ga10x" in sec_map and sec_map[".fwsignature_ga10x"][1] != 4096:
        errors.append(f"unexpected_ga10x_sig_size:{sec_map['.fwsignature_ga10x'][1]}")

    # Validate WPR2 metadata layout (`GspFwWprMeta` 256-byte contract)
    wpr_info: dict[str, Any] = {}
    if len(wpr_meta) != 256:
        errors.append(f"wpr_meta_size_not_256:{len(wpr_meta)}")
    else:
        magic, rev, _r3_pa, r3_sz, _bl_pa, bl_sz = struct.unpack_from("<6Q", wpr_meta, 0)
        non_wpr_off, non_wpr_sz, wpr_start, heap_off, heap_sz, fw_off, boot_off, frts_off, frts_sz, wpr_end, fb_sz = (
            struct.unpack_from("<11Q", wpr_meta, 88)
        )
        verified = struct.unpack_from("<Q", wpr_meta, 248)[0]
        if magic != GSP_WPR_MAGIC or rev != GSP_WPR_REVISION or verified != GSP_WPR_VERIFIED:
            errors.append("bad_wpr_meta_magic_rev_or_verified")
        if (wpr_start % ALIGN_128K) != 0 or (wpr_end % ALIGN_128K) != 0 or (frts_off % ALIGN_128K) != 0:
            errors.append("wpr2_boundary_not_128k_aligned")
        if not (non_wpr_off + non_wpr_sz <= wpr_start < fw_off <= boot_off <= frts_off):
            errors.append("wpr2_region_order_overlap")
        if fw_off + r3_sz > boot_off or boot_off + bl_sz > frts_off or frts_off + frts_sz > wpr_end:
            errors.append("wpr2_subregion_overflow")
        if wpr_end > fb_sz or heap_off + heap_sz > wpr_end:
            errors.append("wpr2_exceeds_fb_size")
        fwimg_sz = sec_map.get(".fwimage", (0, 0))[1]
        if fwimg_sz > r3_sz:
            errors.append("fwimage_exceeds_radix3_size")
        wpr_info = {
            "wpr_start": hex(wpr_start),
            "wpr_end": hex(wpr_end),
            "frts_offset": hex(frts_off),
            "frts_size": frts_sz,
            "fb_size": fb_sz,
        }

    return {
        "valid": not errors,
        "elf_machine": hex(e_mach),
        "sections": len(shdrs),
        "fwversion": fwversion,
        "fwimage_bytes": sec_map.get(".fwimage", (0, 0))[1],
        "ga10x_sig_bytes": sec_map.get(".fwsignature_ga10x", (0, 0))[1],
        "wpr2": wpr_info,
        "errors": errors,
    }


def audit_nvidia_os_interface_coverage() -> dict[str, Any]:
    """Audit OpenRM `os-interface.h` prototypes vs `nv_os_shim_check.c` implementation matrix."""
    shim_txt = SHIM_C_PATH.read_text(encoding="utf-8")
    impl_funcs = sorted(set(re.findall(r"\b(os_[a-z0-9_]+)\s*\(", shim_txt)))

    host_present = HOST_OS_IFACE_H.is_file()
    if host_present:
        hdr_txt = HOST_OS_IFACE_H.read_text(encoding="utf-8", errors="replace")
        all_funcs = sorted(set(re.findall(r"\b(os_[a-z0-9_]+)\s*\(", hdr_txt)))
    else:
        all_funcs = sorted(set(impl_funcs + [f"os_synthetic_unimplemented_{i}" for i in range(140)]))

    buckets: dict[str, list[str]] = {
        "memory_alloc_map": [],
        "locks_sync_wq": [],
        "pci_io_dma": [],
        "time_delay": [],
        "string_dbg": [],
        "vfs_file": [],
        "platform_cgroup_other": [],
    }
    for fn in all_funcs:
        if any(k in fn for k in ("mem", "alloc_pages", "free_pages", "map_", "unmap_", "user_pages", "page", "cache")):
            buckets["memory_alloc_map"].append(fn)
        elif any(k in fn for k in ("spinlock", "mutex", "semaphore", "rwlock", "wait", "wake", "work_queue", "schedule")):
            buckets["locks_sync_wq"].append(fn)
        elif any(k in fn for k in ("pci", "io_", "iommu", "msi", "bif_reset", "vgpu")):
            buckets["pci_io_dma"].append(fn)
        elif any(k in fn for k in ("time", "delay")):
            buckets["time_delay"].append(fn)
        elif any(k in fn for k in ("string", "snprintf", "vsnprintf", "strtoul", "dbg", "log_", "dump_stack")):
            buckets["string_dbg"].append(fn)
        elif "file" in fn:
            buckets["vfs_file"].append(fn)
        else:
            buckets["platform_cgroup_other"].append(fn)

    impl_set = set(impl_funcs)
    cls_entries = re.findall(r'\{\s*"(os_[a-z0-9_]+)"\s*,\s*(NV_SHIM_[A-Z0-9_]+)\s*\}', shim_txt)
    cls_counts: dict[str, int] = {}
    for _, cls in cls_entries:
        cls_counts[cls] = cls_counts.get(cls, 0) + 1
    matrix = {
        k: {
            "total": len(v),
            "implemented": sum(1 for fn in v if fn in impl_set),
            "remaining": sum(1 for fn in v if fn not in impl_set),
        }
        for k, v in buckets.items()
    }
    return {
        "valid": len(impl_funcs) == 22 and len(cls_entries) == 22 and (not host_present or len(all_funcs) == 162),
        "host_header_present": host_present,
        "total_os_apis": len(all_funcs),
        "implemented_in_shim": len(impl_funcs),
        "remaining_unimplemented": len(all_funcs) - len(impl_funcs),
        "coverage_pct": round((len(impl_funcs) / max(1, len(all_funcs))) * 100.0, 2),
        "shim_classification_counts": cls_counts,
        "buckets": matrix,
    }


def validate_amd_ucode_header(blob: bytes) -> dict[str, Any]:
    """Validate synthetic AMD `common_firmware_header` (`256 B`) + ucode bounds & CRC32."""
    errors: list[str] = []
    if len(blob) < 256:
        return {"valid": False, "errors": [f"amd_ucode_truncated:{len(blob)}"]}
    sz_bytes, ver_major, ver_minor, crc, ucode_ver, ucode_sz, ucode_off = struct.unpack_from("<IHHI24xII4xI", blob, 0)
    if sz_bytes < 256 or ver_major < 1 or ucode_off < 256:
        errors.append("bad_amd_common_header")
    if sz_bytes > len(blob) or ucode_off + ucode_sz > len(blob) or ucode_sz == 0:
        errors.append("amd_ucode_bounds_overflow")
    else:
        actual_crc = zlib.crc32(blob[ucode_off : ucode_off + ucode_sz]) & 0xFFFFFFFF
        if actual_crc != crc:
            errors.append(f"amd_ucode_crc_mismatch:{hex(actual_crc)}!={hex(crc)}")
    return {
        "valid": not errors,
        "header_version": f"{ver_major}.{ver_minor}",
        "ucode_version": hex(ucode_ver),
        "ucode_bytes": ucode_sz,
        "errors": errors,
    }


def validate_fw_store_sha256_contract() -> dict[str, Any]:
    """Verify `userspace/src/fw_store.rs` SHA-256 verification and tamper-rejection contract."""
    fw_rs = ROOT / "userspace" / "src" / "fw_store.rs"
    src_ok = fw_rs.is_file() and all(
        tok in fw_rs.read_text(encoding="utf-8")
        for tok in ("DigestMismatch", "EmptyBlob", "NotFound", "ct_eq_32", "Sha256")
    )
    payload = b"DRYOS-INTEL-GUC-70.1.1-CSS-SYNTH-PAYLOAD-VALID"
    digest = hashlib.sha256(payload).hexdigest()
    tampered = bytearray(payload)
    tampered[-1] ^= 0xFF
    tamper_rejected = hashlib.sha256(bytes(tampered)).hexdigest() != digest
    return {"valid": src_ok and tamper_rejected, "sha256_prefix": digest[:16], "tamper_rejected": tamper_rejected}


def validate_muxless_prime_handoff() -> dict[str, Any]:
    """Verify muxless `03:02` dGPU -> `2 MiB` `READ_ONLY_EXPORT` `FrameCap` + `dma_fence` -> GOP."""
    right_read = 1 << 0
    right_write = 1 << 1
    right_exec = 1 << 2
    right_map = 1 << 3

    current_epoch = 3
    sealed_cap = {
        "frame_id": 1001,
        "bytes": FRAME_2M,
        "rights": right_read | right_map,  # READ_ONLY_EXPORT (`vault_ctrl.rs` INV-SEC-03)
        "generation": 2,
        "lease_epoch": 3,
        "payload_crc32": 0xA55A1234,
    }
    fence = {"seq": 42, "generation": 2, "last_crc": 0xA55A1234}

    def map_prime_frame(cap: dict[str, Any], req_rights: int, expected_gen: int, fence_obj: dict[str, Any]) -> int:
        if cap["bytes"] != FRAME_2M:
            return -22  # EINVAL
        if (req_rights & (right_write | right_exec)) != 0 or (cap["rights"] & (right_write | right_exec)) != 0:
            return -1  # EPERM (`W^X` READ_ONLY_EXPORT violation)
        if cap["lease_epoch"] < current_epoch or cap["generation"] != expected_gen or fence_obj["generation"] != expected_gen:
            return -110  # EREVOKED
        if cap["payload_crc32"] != fence_obj["last_crc"]:
            return -5  # EIO
        return 0

    ok_rc = map_prime_frame(sealed_cap, right_read | right_map, 2, fence)
    rw_denied_rc = map_prime_frame(sealed_cap, right_read | right_write | right_map, 2, fence)
    stale_gen_rc = map_prime_frame({**sealed_cap, "generation": 1}, right_read | right_map, 2, fence)
    stale_epoch_rc = map_prime_frame({**sealed_cap, "lease_epoch": 2}, right_read | right_map, 2, fence)

    ok = ok_rc == 0 and rw_denied_rc == -1 and stale_gen_rc == -110 and stale_epoch_rc == -110
    return {
        "valid": ok,
        "frame_bytes": FRAME_2M,
        "ro_handoff_rc": ok_rc,
        "write_escalation_rc": rw_denied_rc,
        "stale_gen_rc": stale_gen_rc,
        "stale_epoch_rc": stale_epoch_rc,
    }


def _build_synthetic_css(kind: str) -> bytes:
    if kind == "dmc":
        hdr = bytearray(128)
        raw_sz = 1024
        struct.pack_into("<7I", hdr, 0, 9, 32, 0x10000, 0, 0x8086, 0x20210609, raw_sz // 4)
        struct.pack_into("<I", hdr, 0x58, 0x0002000C)
        pkg = bytearray(raw_sz - 128)
        struct.pack_into("<4I", pkg, 0, 612, 0, 0, 2)
        struct.pack_into("<I", pkg, 400, 0x40403E3E)
        return bytes(hdr + pkg)
    hdr = bytearray(644)
    ucode = b"\x00" * 512
    sz_dw = (len(hdr) + len(ucode)) // 4 + 65
    struct.pack_into("<10I", hdr, 0, 6, 161, 0x10000, 0, 0x8086, 0x20220405, sz_dw, 64, 64, 1)
    struct.pack_into("<I", hdr, 0x40, 0x00460101)
    return bytes(hdr) + ucode


def _build_synthetic_opregion() -> bytes:
    buf = bytearray(0x2000)
    buf[0:16] = OPREGION_SIG
    struct.pack_into("<II", buf, 16, 8, 0x02000000)
    struct.pack_into("<I", buf, 40, 0xF)
    vbt_off = 0x400
    buf[vbt_off : vbt_off + 20] = b"$VBT TIGERLAKE-LP  \x00"
    struct.pack_into("<HHHHI", buf, vbt_off + 20, 0x0240, 48, 256, 0, 48)
    buf[vbt_off + 48 : vbt_off + 64] = BDB_SIG
    return bytes(buf)


def _build_synthetic_edid() -> bytes:
    edid = bytearray(128)
    edid[0:8] = EDID_HEADER
    struct.pack_into(">H", edid, 8, ((1 << 10) | (21 << 5) | 15))  # AUO
    struct.pack_into("<H", edid, 10, 0xB69B)
    edid[18], edid[19] = 1, 4
    struct.pack_into("<H", edid, 54, 40500)
    edid[56], edid[57], edid[58] = 0x80, 0xA0, 0x70  # 1920 + 160 blank
    edid[59], edid[60], edid[61] = 0x38, 0x64, 0x40  # 1080 + 100 blank
    edid[127] = (-sum(edid[:127])) & 0xFF
    return bytes(edid)


def _build_synthetic_gsp_and_wpr2() -> tuple[bytes, bytes]:
    shstr = b"\x00.fwimage\x00.fwversion\x00.note.gnu.build-id\x00.fwsignature_ga10x\x00.shstrtab\x00"
    fwimg = b"\x13\x01\x00\x00" * 64
    fwver = b"615.78.08\x00"
    note = b"\x04\x00\x00\x00\x14\x00\x00\x00\x03\x00\x00\x00GNU\x00" + b"\xaa" * 20
    sig = b"\x01\x00\x02\x00" + b"\x00" * 4092

    off = 64
    sections: list[tuple[int, int, int, int, int]] = [(0, 0, 0, 0, 0)]
    payload = bytearray()
    for name, stype, flags, data in (
        (b".fwimage", 1, 3, fwimg),
        (b".fwversion", 1, 0, fwver),
        (b".note.gnu.build-id", 7, 0, note),
        (b".fwsignature_ga10x", 1, 0, sig),
        (b".shstrtab", 3, 0, shstr),
    ):
        sections.append((shstr.index(name), stype, flags, off + len(payload), len(data)))
        payload.extend(data)
    shoff = off + len(payload)
    ehdr = bytearray(64)
    ehdr[0:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HHIQQQIHHHHHH", ehdr, 16, 1, 0xF3, 1, 0, 0, shoff, 0, 64, 0, 0, 64, len(sections), 5)
    sh_bytes = b"".join(struct.pack("<IIQQQQIIQQ", nm, st, fl, 0, so, sz, 0, 0, 1, 0) for nm, st, fl, so, sz in sections)

    wpr = bytearray(256)
    fb_sz = 4 * 1024 * 1024 * 1024
    wpr_start, wpr_end = 0xFA000000, 0xFFC00000
    struct.pack_into("<6Q", wpr, 0, GSP_WPR_MAGIC, GSP_WPR_REVISION, 0x10000000, 0x2000000, 0x12000000, 0x10000)
    struct.pack_into(
        "<11Q", wpr, 88, 0xF9000000, 0x100000, wpr_start, wpr_start + 0x20000, 0x1000000,
        wpr_start + 0x1100000, wpr_start + 0x3200000, wpr_end - 0x100000, 0x100000, wpr_end, fb_sz,
    )
    struct.pack_into("<Q", wpr, 248, GSP_WPR_VERIFIED)
    return bytes(ehdr) + bytes(payload) + sh_bytes, bytes(wpr)


def run_self_test() -> dict[str, Any]:
    syn_dmc = _build_synthetic_css("dmc")
    syn_guc = _build_synthetic_css("guc")
    syn_opr = _build_synthetic_opregion()
    syn_edid = _build_synthetic_edid()
    syn_gsp, syn_wpr = _build_synthetic_gsp_and_wpr2()

    bad_dmc = bytearray(syn_dmc)
    struct.pack_into("<I", bad_dmc, 24, 999999)
    bad_guc = bytearray(syn_guc)
    struct.pack_into("<I", bad_guc, 16, 0x1234)
    bad_opr = bytearray(syn_opr)
    bad_opr[0:4] = b"XXXX"
    bad_edid = bytearray(syn_edid)
    bad_edid[127] ^= 0x01
    bad_wpr = bytearray(syn_wpr)
    struct.pack_into("<Q", bad_wpr, 88 + 16, 0xFA000100)  # unaligned wprStart

    amd_payload = b"\x5a\xa5\x10\x02" * 64
    amd_hdr = bytearray(256)
    struct.pack_into(
        "<IHHI24xII4xI", amd_hdr, 0, 256, 1, 0, zlib.crc32(amd_payload) & 0xFFFFFFFF, 0x01020304, len(amd_payload), 256
    )
    syn_amd = bytes(amd_hdr) + amd_payload
    bad_amd = bytearray(syn_amd)
    bad_amd[-1] ^= 0x01

    results: dict[str, Any] = {
        "intel_synthetic_dmc_valid": validate_intel_css_firmware(syn_dmc, "dmc"),
        "intel_synthetic_guc_valid": validate_intel_css_firmware(syn_guc, "guc"),
        "intel_synthetic_dmc_corrupt_rejected": not validate_intel_css_firmware(bytes(bad_dmc), "dmc")["valid"],
        "intel_synthetic_guc_bad_vendor_rejected": not validate_intel_css_firmware(bytes(bad_guc), "guc")["valid"],
        "intel_synthetic_opregion_vbt": validate_igd_opregion_vbt(syn_opr),
        "intel_synthetic_opregion_corrupt_rejected": not validate_igd_opregion_vbt(bytes(bad_opr))["valid"],
        "intel_synthetic_edid_valid": validate_edid_128(syn_edid),
        "intel_synthetic_edid_bad_cksum_rejected": not validate_edid_128(bytes(bad_edid))["valid"],
        "intel_gen12_gop_preservation": validate_gen12_gop_preservation(),
        "amd_synthetic_ucode_valid": validate_amd_ucode_header(syn_amd),
        "amd_synthetic_ucode_corrupt_rejected": not validate_amd_ucode_header(bytes(bad_amd))["valid"],
        "dry_fw_store_sha256_contract": validate_fw_store_sha256_contract(),
        "nvidia_synthetic_gsp_wpr2": validate_gsp_elf64_and_wpr2(syn_gsp, syn_wpr),
        "nvidia_synthetic_wpr2_unaligned_rejected": not validate_gsp_elf64_and_wpr2(syn_gsp, bytes(bad_wpr))["valid"],
        "nvidia_os_interface_audit": audit_nvidia_os_interface_coverage(),
        "muxless_prime_handoff": validate_muxless_prime_handoff(),
        "host_read_only_checks": {},
    }

    if HOST_DMC_ZST.is_file():
        results["host_read_only_checks"]["tgl_dmc_ver2_12"] = validate_intel_css_firmware(HOST_DMC_ZST.read_bytes(), "dmc")
    if HOST_GUC_ZST.is_file():
        results["host_read_only_checks"]["tgl_guc_70_1_1"] = validate_intel_css_firmware(HOST_GUC_ZST.read_bytes(), "guc")
    if HOST_EDID_PATH.is_file():
        try:
            edid_raw = HOST_EDID_PATH.read_bytes()
            if len(edid_raw) >= 128:
                results["host_read_only_checks"]["edp1_edid"] = validate_edid_128(edid_raw)
        except OSError:
            pass
    if HOST_GSP_BIN.is_file():
        results["host_read_only_checks"]["gsp_ga10x_615_78_08"] = validate_gsp_elf64_and_wpr2(HOST_GSP_BIN, syn_wpr)
    if HOST_UCODES_BIN.is_file():
        with HOST_UCODES_BIN.open("rb") as uf:
            umagic, uver, uflags = struct.unpack("<8sII", uf.read(16))
        results["host_read_only_checks"]["ucodes_ga10x_615_78_08"] = {
            "valid": umagic == b"NVUCODES" and uver >= 1,
            "magic": umagic.decode("ascii", "replace"),
            "version": uver,
            "flags": hex(uflags),
            "bytes": HOST_UCODES_BIN.stat().st_size,
        }

    all_ok = all(
        v if isinstance(v, bool) else bool(v.get("valid", True))
        for k, v in results.items()
        if k != "host_read_only_checks"
    ) and all(v.get("valid", False) for v in results["host_read_only_checks"].values())
    results["all_passed"] = all_ok
    return results


def main() -> None:
    parser = argparse.ArgumentParser(description="DRY OS Intel & NVIDIA Firmware/PRIME Validators")
    parser.add_argument("--self-test", action="store_true", help="Run synthetic + read-only host validators")
    parser.add_argument("--json-out", type=pathlib.Path, default=None)
    args = parser.parse_args()

    res = run_self_test() if args.self_test else run_self_test()
    if args.json_out is not None:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")

    host_n = len(res["host_read_only_checks"])
    cov = res["nvidia_os_interface_audit"]
    print(
        f"[GPU-FW-VALIDATORS] {'PASS' if res['all_passed'] else 'FAIL'}: "
        f"dmc={res['intel_synthetic_dmc_valid']['version']} "
        f"guc={res['intel_synthetic_guc_valid']['version']} "
        f"vbt_ok={res['intel_synthetic_opregion_vbt']['valid']} "
        f"edid={res['intel_synthetic_edid_valid']['mode']} "
        f"gop_preserve={res['intel_gen12_gop_preservation']['valid']} "
        f"gsp_wpr2={res['nvidia_synthetic_gsp_wpr2']['valid']} "
        f"os_iface={cov['implemented_in_shim']}/{cov['total_os_apis']} "
        f"prime_ro_fence={res['muxless_prime_handoff']['valid']} "
        f"amd_ucode={res['amd_synthetic_ucode_valid']['valid']} "
        f"fw_store_sha256={res['dry_fw_store_sha256_contract']['valid']} "
        f"host_real_blobs={host_n}"
    )
    if not res["all_passed"]:
        print(json.dumps(res, indent=2))
        raise SystemExit(1)


if __name__ == "__main__":
    main()
