#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Build and QEMU-xHCI verify the bootable DRY OS Project Ascension 5.0 Live USB GPT image."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import queue
import shutil
import struct
import subprocess
import sys
import threading
import time
import uuid
import zlib
from typing import Any

TOOLS_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS_DIR))

from qemu_boot_test import (  # noqa: E402
    BUILD_DIR,
    EXIT_CODE_SUCCESS,
    KERNEL_DIR,
    LIMINE_EFI,
    OVMF_CODE,
    OVMF_VARS,
    ROOT,
    USERSPACE_DIR,
    USERSPACE_MODS,
    build_kernel,
)
from qemu_log_validators import _parse_kv_ints  # noqa: E402
from qemu_p2_validators import _DesktopQmpClient  # noqa: E402
from qemu_p13_usb_persist_validators import _format_dryfs_v1_bytes, _write_gpt_image  # noqa: E402

DEFAULT_IMG = BUILD_DIR / "dryos-ascension6-live-usb.img"
HWVAL_IMG = BUILD_DIR / "dryos-ascension8-hardware-validation.img"
OUT_JSON = ROOT / "tools" / "compat_bench" / "results_live_usb_boot.json"
HWVAL_OUT_JSON = ROOT / "tools" / "compat_bench" / "results_hwval_usb_boot.json"
SECTOR_BYTES = 512
SECTORS_PER_MIB = (1024 * 1024) // SECTOR_BYTES
PART1_START_LBA = 2048  # 1 MiB alignment


def _sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _sha256_prefix(path: pathlib.Path, byte_len: int) -> str:
    h = hashlib.sha256()
    remaining = byte_len
    with path.open("rb") as f:
        while remaining > 0:
            chunk = f.read(min(1 << 20, remaining))
            if not chunk:
                raise RuntimeError(f"Unexpected EOF hashing prefix of {path} (remaining={remaining})")
            h.update(chunk)
            remaining -= len(chunk)
    return h.hexdigest()


def _limine_conf_text() -> str:
    mod_lines = "\n".join(f"    module_path: boot():/boot/{dst}" for _, dst in USERSPACE_MODS)
    entries = (
        ("DRY OS v0.6.0 - Live Desktop (1280x800, USB Persist RW, Default)", "1280x800x32", "live_usb=1 desktop4=1 storage=rw"),
        ("DRY OS v0.6.0 - Live FHD Desktop (1920x1080, USB Persist RW)", "1920x1080x32", "live_usb=1 desktop4=1 storage=rw"),
        ("DRY OS v0.6.0 - Safe UEFI GOP Desktop (Read-Only, No GPU Island)", "1280x800x32", "live_usb=1 desktop=1 storage=ro"),
        ("DRY OS v0.6.0 - Diagnostic & Hardware Self-Test (Read-Only)", "1280x800x32", "console=com1 diag_selftest=1 desktop4=1 storage=ro"),
        ("DRY OS v0.6.0 - Linux v6.18 Driver Island Audit (Read-Only)", "1280x800x32", "console=com1 desktop4=1 lx_drm=1 storage=ro"),
    )
    # Intentionally omit `serial: yes` so Limine never polls COM1 (0x3F8) on UART-less laptops.
    blocks = ["timeout: 2", ""]
    for title, res, cmdline in entries:
        blocks.extend(
            [
                f"/{title}",
                "    protocol: limine",
                f"    resolution: {res}",
                "    path: boot():/boot/dry-kernel",
                f"    cmdline: {cmdline}",
                mod_lines,
                "",
            ]
        )
    return "\n".join(blocks)


def _limine_hwval_conf_text(disk_guid_hex: str, default_extra: str = "", default_idx: int = 0) -> str:
    mod_lines = "\n".join(f"    module_path: boot():/boot/{dst}" for _, dst in USERSPACE_MODS)
    sfx = f" {default_extra.strip()}" if default_extra.strip() else ""
    entries = [
        (
            "DRY OS v0.8.0 - SAFE AUTO CHECK (Default, Passive)",
            "1280x800x32",
            f"live_usb=1 desktop=1 storage=ro hwcheck=auto hwcheck_mode=safe usb_disk_guid={disk_guid_hex}{sfx}",
        ),
        (
            "DRY OS v0.8.0 - DESKTOP SAFE MODE (GOP Only, No GPU Driver Init)",
            "1280x800x32",
            f"live_usb=1 desktop=1 storage=ro hwcheck_mode=desktop_safe usb_disk_guid={disk_guid_hex}{sfx}",
        ),
        (
            "DRY OS v0.8.0 - EXTENDED DIAGNOSTICS (Bounded, Non-Destructive)",
            "1280x800x32",
            f"live_usb=1 desktop=1 storage=ro hwcheck=auto hwcheck_mode=extended usb_disk_guid={disk_guid_hex}{sfx}",
        ),
        (
            "DRY OS v0.8.0 - RECOVERY / LOG VIEWER (No Hardware Init Tests)",
            "1280x800x32",
            f"live_usb=1 desktop=1 storage=ro hwcheck_mode=recovery usb_disk_guid={disk_guid_hex}{sfx}",
        ),
        (
            "DRY OS v0.8.0 - NVIDIA DRIVER EXPERIMENTAL (MicroVM Level 1 Gated)",
            "1280x800x32",
            f"live_usb=1 desktop=1 storage=ro hwcheck=auto hwcheck_mode=nvidia_exp nvidia_exp=1 usb_disk_guid={disk_guid_hex}{sfx}",
        ),
    ]
    if 0 < default_idx < len(entries):
        chosen = entries.pop(default_idx)
        entries.insert(0, chosen)
    blocks = ["timeout: 2", ""]
    for title, res, cmdline in entries:
        blocks.extend(
            [
                f"/{title}",
                "    protocol: limine",
                f"    resolution: {res}",
                "    path: boot():/boot/dry-kernel",
                f"    cmdline: {cmdline}",
                mod_lines,
                "",
            ]
        )
    return "\n".join(blocks)


def _binaries_ready() -> tuple[pathlib.Path, pathlib.Path, bool]:
    kernel_elf = KERNEL_DIR / "target" / "x86_64-unknown-none" / "release" / "dry-kernel"
    u_dir = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release"
    ok = kernel_elf.exists() and all((u_dir / src).exists() for src, _ in USERSPACE_MODS)
    return kernel_elf, u_dir, ok


def _verify_esp_mdir(esp_part: pathlib.Path, mtools_env: dict[str, str]) -> None:
    def ls_dir(mpath: str) -> set[str]:
        out = subprocess.check_output(["mdir", "-b", "-i", str(esp_part), mpath], env=mtools_env, text=True)
        return {ln.strip().lower() for ln in out.splitlines() if ln.strip()}

    root_entries = ls_dir("::/")
    efi_entries = ls_dir("::/EFI/BOOT")
    limine_entries = ls_dir("::/boot/limine")
    boot_entries = ls_dir("::/boot")
    if "::/dryos_live_manifest.json" not in root_entries:
        raise RuntimeError("ESP verification failed: missing ::/DRYOS_LIVE_MANIFEST.json")
    if "::/efi/boot/bootx64.efi" not in efi_entries:
        raise RuntimeError("ESP verification failed: missing ::/EFI/BOOT/BOOTX64.EFI")
    if "::/boot/limine/limine.conf" not in limine_entries:
        raise RuntimeError("ESP verification failed: missing ::/boot/limine/limine.conf")
    if "::/boot/dry-kernel" not in boot_entries:
        raise RuntimeError("ESP verification failed: missing ::/boot/dry-kernel")
    for _, dst_name in USERSPACE_MODS:
        if f"::/boot/{dst_name.lower()}" not in boot_entries:
            raise RuntimeError(f"ESP verification failed: missing ::/boot/{dst_name}")


def _verify_gpt_hdr(hdr: bytes, expected_lba: int, expected_alt: int, expected_pe: int, entries_crc: int, label: str) -> None:
    if hdr[:8] != b"EFI PART":
        raise RuntimeError(f"{label} GPT missing EFI PART signature")
    _sig, _rev, hdr_size, orig_crc, _rsv, cur_lba, alt_lba, _fu, _lu, _guid, pe_lba, n_ent, ent_sz, pe_crc = (
        struct.unpack_from("<8sIIIIQQQQ16sQIII", hdr, 0)
    )
    if hdr_size != 92 or cur_lba != expected_lba or alt_lba != expected_alt or pe_lba != expected_pe:
        raise RuntimeError(f"{label} GPT header geometry mismatch: cur={cur_lba} alt={alt_lba} pe={pe_lba}")
    if n_ent != 128 or ent_sz != 128 or pe_crc != entries_crc:
        raise RuntimeError(f"{label} GPT entry table mismatch: n={n_ent} sz={ent_sz} crc={pe_crc:#x}")
    tmp = bytearray(hdr[:92])
    tmp[16:20] = b"\x00\x00\x00\x00"
    if (zlib.crc32(tmp) & 0xFFFF_FFFF) != orig_crc:
        raise RuntimeError(f"{label} GPT header CRC32 mismatch")


def _verify_gpt_and_dryfs(
    img_path: pathlib.Path,
    total_sectors: int,
    p1_range: tuple[int, int],
    p2_range: tuple[int, int],
) -> None:
    last_lba = total_sectors - 1
    backup_entries_lba = last_lba - 32
    with img_path.open("rb") as f:
        mbr = f.read(SECTOR_BYTES)
        if len(mbr) != SECTOR_BYTES or mbr[510:512] != b"\x55\xaa":
            raise RuntimeError("Protective MBR missing 0x55AA signature")
        _boot, _s0, _s1, _s2, ptype, _e0, _e1, _e2, pstart, plen = struct.unpack_from("<BBBBBBBBII", mbr, 446)
        if ptype != 0xEE or pstart != 1 or plen != min(0xFFFF_FFFF, last_lba):
            raise RuntimeError(f"Protective MBR mismatch: type={ptype:#x} start={pstart} len={plen}")

        f.seek(SECTOR_BYTES)
        prim_hdr = f.read(SECTOR_BYTES)
        f.seek(2 * SECTOR_BYTES)
        prim_entries = f.read(128 * 128)
        entries_crc = zlib.crc32(prim_entries) & 0xFFFF_FFFF
        _verify_gpt_hdr(prim_hdr, 1, last_lba, 2, entries_crc, "Primary")

        e1_first, e1_last = struct.unpack_from("<QQ", prim_entries, 32)
        e2_first, e2_last = struct.unpack_from("<QQ", prim_entries, 128 + 32)
        if (e1_first, e1_last) != p1_range or (e2_first, e2_last) != p2_range:
            raise RuntimeError(f"GPT partition ranges mismatch: p1={(e1_first, e1_last)} p2={(e2_first, e2_last)}")

        f.seek(backup_entries_lba * SECTOR_BYTES)
        bak_entries = f.read(128 * 128)
        if bak_entries != prim_entries:
            raise RuntimeError("Backup GPT entries do not match Primary GPT entries")
        f.seek(last_lba * SECTOR_BYTES)
        bak_hdr = f.read(SECTOR_BYTES)
        _verify_gpt_hdr(bak_hdr, last_lba, 1, backup_entries_lba, entries_crc, "Backup")

        p2_off = p2_range[0] * SECTOR_BYTES
        for blk_idx in (0, 1):
            f.seek(p2_off + blk_idx * 4096)
            sb = bytearray(f.read(1600))
            if len(sb) != 1600 or sb[:8] != b"DRYFS_V1":
                raise RuntimeError(f"Partition 2 DRYFS_V1 blk{blk_idx} missing magic")
            ver, _max_blk, tx_seq, _next_ino, _rover, active, sb_crc = struct.unpack_from("<IIQIIII", sb, 8)
            sb[36:40] = b"\x00\x00\x00\x00"
            calc_crc = zlib.crc32(sb) & 0xFFFF_FFFF
            if ver != 1 or tx_seq < 1 or active < 1 or calc_crc != sb_crc:
                raise RuntimeError(f"Partition 2 DRYFS_V1 blk{blk_idx} superblock invalid: ver={ver} crc={calc_crc:#x}!={sb_crc:#x}")


def _build_esp_partition_bytes(
    esp_part: pathlib.Path,
    esp_sectors: int,
    kernel_elf: pathlib.Path,
    u_dir: pathlib.Path,
    conf_path: pathlib.Path,
    manifest_path: pathlib.Path,
    mtools_env: dict[str, str],
) -> bytes:
    try:
        with esp_part.open("wb") as f:
            f.truncate(esp_sectors * SECTOR_BYTES)
        subprocess.check_call(["mformat", "-i", str(esp_part), "-F", "-v", "DRYOS_ESP", "::"], env=mtools_env)
        for d in ("::/EFI", "::/EFI/BOOT", "::/boot", "::/boot/limine"):
            subprocess.check_call(["mmd", "-i", str(esp_part), d], env=mtools_env)
        copies = [
            (LIMINE_EFI, "::/EFI/BOOT/BOOTX64.EFI"),
            (conf_path, "::/boot/limine/limine.conf"),
            (kernel_elf, "::/boot/dry-kernel"),
            (manifest_path, "::/DRYOS_LIVE_MANIFEST.json"),
        ] + [(u_dir / src_name, f"::/boot/{dst_name}") for src_name, dst_name in USERSPACE_MODS]
        for src, dst in copies:
            subprocess.check_call(["mcopy", "-i", str(esp_part), str(src), dst], env=mtools_env)
        _verify_esp_mdir(esp_part, mtools_env)
        return esp_part.read_bytes()
    finally:
        esp_part.unlink(missing_ok=True)


def _write_gpt_hwval_image(
    img_path: pathlib.Path,
    total_sectors: int,
    part1_range: tuple[int, int],
    part2_range: tuple[int, int],
    part3_range: tuple[int, int] | None,
    disk_guid_raw: bytes,
    seed_part1: bytes,
    seed_part3: bytes | None,
) -> None:
    """Create a 3-partition (or 2-partition if `part3_range is None`) GPT validation image."""
    assert len(disk_guid_raw) == 16 and total_sectors >= 8192
    with img_path.open("wb") as f:
        f.truncate(total_sectors * SECTOR_BYTES)

    def gpt_entry(type_guid: uuid.UUID, unique_guid: uuid.UUID, first: int, last: int, name: str) -> bytes:
        u16_name = name.encode("utf-16-le")[:72].ljust(72, b"\x00")
        return struct.pack("<16s16sQQQ72s", type_guid.bytes_le, unique_guid.bytes_le, first, last, 0, u16_name)

    esp_guid = uuid.UUID("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
    data_guid = uuid.UUID("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")
    entries = bytearray(128 * 128)
    entries[0:128] = gpt_entry(esp_guid, uuid.uuid5(uuid.NAMESPACE_OID, "dry-hwval-p1"), part1_range[0], part1_range[1], "DRYOS_ESP")
    entries[128:256] = gpt_entry(data_guid, uuid.uuid5(uuid.NAMESPACE_OID, "dry-hwval-p2"), part2_range[0], part2_range[1], "DRYOS_PERSIST")
    if part3_range is not None:
        entries[256:384] = gpt_entry(data_guid, uuid.uuid5(uuid.NAMESPACE_OID, "dry-hwval-p3"), part3_range[0], part3_range[1], "DRYOS_LOGS")
    entries_crc = zlib.crc32(entries) & 0xFFFF_FFFF

    backup_lba = total_sectors - 1
    backup_entries_lba = backup_lba - 32
    last_usable = backup_entries_lba - 1

    def make_hdr(cur_lba: int, alt_lba: int, pe_lba: int) -> bytes:
        hdr = bytearray(512)
        struct.pack_into(
            "<8sIIIIQQQQ16sQIII",
            hdr, 0, b"EFI PART", 0x0001_0000, 92, 0, 0, cur_lba, alt_lba, 34, last_usable,
            disk_guid_raw, pe_lba, 128, 128, entries_crc,
        )
        struct.pack_into("<I", hdr, 16, zlib.crc32(hdr[:92]) & 0xFFFF_FFFF)
        return bytes(hdr)

    pmbr = bytearray(512)
    struct.pack_into("<BBBBBBBBII", pmbr, 446, 0x00, 0x00, 0x02, 0x00, 0xEE, 0xFF, 0xFF, 0xFF, 1, min(0xFFFF_FFFF, total_sectors - 1))
    pmbr[510:512] = b"\x55\xaa"

    with img_path.open("r+b") as f:
        f.seek(0)
        f.write(pmbr)
        f.seek(512)
        f.write(make_hdr(1, backup_lba, 2))
        f.seek(2 * 512)
        f.write(entries)
        f.seek(backup_entries_lba * 512)
        f.write(entries)
        f.seek(backup_lba * 512)
        f.write(make_hdr(backup_lba, 1, backup_entries_lba))
        f.seek(part1_range[0] * 512)
        f.write(seed_part1)
        sb_4k, data_blk2_4k = _format_dryfs_v1_bytes(part2_range[1] - part2_range[0] + 1)
        p2_off = part2_range[0] * 512
        f.seek(p2_off)
        f.write(sb_4k)
        f.seek(p2_off + 4096)
        f.write(sb_4k)
        f.seek(p2_off + 8192)
        f.write(data_blk2_4k)
        if part3_range is not None and seed_part3:
            f.seek(part3_range[0] * 512)
            f.write(seed_part3)


def patch_img_limine_conf(img_path: pathlib.Path, part1_start_lba: int, part1_sectors: int, conf_text: str) -> None:
    """Update only `::/boot/limine/limine.conf` inside ESP for multi-boot QEMU validation tests."""
    tmp_esp = img_path.parent / f"{img_path.stem}_esp_patch.fat"
    tmp_conf = img_path.parent / f"{img_path.stem}_limine_patch.conf"
    mtools_env = {**os.environ, "LC_ALL": "C"}
    try:
        tmp_conf.write_text(conf_text, encoding="utf-8")
        with img_path.open("rb") as src, tmp_esp.open("wb") as dst:
            src.seek(part1_start_lba * SECTOR_BYTES)
            dst.write(src.read(part1_sectors * SECTOR_BYTES))
        subprocess.check_call(["mcopy", "-o", "-i", str(tmp_esp), str(tmp_conf), "::/boot/limine/limine.conf"], env=mtools_env)
        esp_bytes = tmp_esp.read_bytes()
        with img_path.open("r+b") as dst:
            dst.seek(part1_start_lba * SECTOR_BYTES)
            dst.write(esp_bytes)
    finally:
        tmp_esp.unlink(missing_ok=True)
        tmp_conf.unlink(missing_ok=True)


def _build_fat32_logs_bytes(
    part_path: pathlib.Path,
    sectors: int,
    mtools_env: dict[str, str],
    corrupt_bpb: bool = False,
    fill_full: bool = False,
) -> bytes:
    try:
        with part_path.open("wb") as f:
            f.truncate(sectors * SECTOR_BYTES)
        # Format with 4 sectors/cluster (2 KiB) so 95 MiB holds ~48k clusters cleanly.
        subprocess.check_call(["mformat", "-i", str(part_path), "-F", "-c", "4", "-v", "DRYOS_LOGS", "::"], env=mtools_env)
        if fill_full:
            raw = bytearray(part_path.read_bytes())
            spc = raw[13]
            rsvd = struct.unpack_from("<H", raw, 14)[0]
            nfats = raw[16]
            tot32 = struct.unpack_from("<I", raw, 32)[0]
            fatsz32 = struct.unpack_from("<I", raw, 36)[0]
            data_start = rsvd + nfats * fatsz32
            total_clus = (tot32 - data_start) // spc
            fat0_off = rsvd * SECTOR_BYTES
            for c in range(2, total_clus + 2):
                off = fat0_off + c * 4
                if (struct.unpack_from("<I", raw, off)[0] & 0x0FFF_FFFF) == 0:
                    struct.pack_into("<I", raw, off, 0x0FFF_FFF7)
            fat_bytes = bytes(raw[fat0_off : fat0_off + fatsz32 * SECTOR_BYTES])
            for fi in range(1, nfats):
                dst_off = (rsvd + fi * fatsz32) * SECTOR_BYTES
                raw[dst_off : dst_off + len(fat_bytes)] = fat_bytes
            if rsvd > 1 and struct.unpack_from("<I", raw, SECTOR_BYTES)[0] == 0x4161_5252:
                struct.pack_into("<I", raw, SECTOR_BYTES + 488, 0)
            part_path.write_bytes(raw)
        elif corrupt_bpb:
            raw = bytearray(part_path.read_bytes())
            raw[82:90] = b"BADFS   "
            raw[510:512] = b"\x00\x00"
            part_path.write_bytes(raw)
        else:
            subprocess.check_call(["fsck.fat", "-n", str(part_path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return part_path.read_bytes()
    finally:
        part_path.unlink(missing_ok=True)


def build_hwval_usb_image(
    img_path: pathlib.Path = HWVAL_IMG,
    force_build: bool = False,
    size_mib: int = 256,
    esp_mib: int = 96,
    persist_mib: int = 64,
    limine_extra: str = "",
    default_entry_idx: int = 0,
    guid_override: bytes | None = None,
    omit_p3: bool = False,
    corrupt_p3_bpb: bool = False,
    fill_p3_full: bool = False,
) -> dict[str, Any]:
    if size_mib < 208:
        raise ValueError(f"HW validation image requires >= 208 MiB (got {size_mib})")
    kernel_elf, u_dir, ready = _binaries_ready()
    if force_build or not ready:
        kernel_elf = build_kernel()

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    img_path.parent.mkdir(parents=True, exist_ok=True)

    disk_guid_raw = guid_override if guid_override is not None else uuid.uuid5(uuid.NAMESPACE_OID, "dry-ascension8-hwval-usb").bytes_le
    boot_guid_raw = uuid.uuid5(uuid.NAMESPACE_OID, "dry-ascension8-hwval-usb").bytes_le
    total_sectors = size_mib * SECTORS_PER_MIB
    esp_sectors = esp_mib * SECTORS_PER_MIB
    persist_sectors = persist_mib * SECTORS_PER_MIB
    p1_range = (PART1_START_LBA, PART1_START_LBA + esp_sectors - 1)
    p2_range = (p1_range[1] + 1, p1_range[1] + persist_sectors)
    if omit_p3:
        p3_range = None
        p3_sectors = 0
    else:
        p3_start = p2_range[1] + 1
        p3_end = total_sectors - 34
        p3_range = (p3_start, p3_end)
        p3_sectors = p3_end - p3_start + 1

    conf_path = BUILD_DIR / "hwval_limine.conf"
    conf_path.write_text(_limine_hwval_conf_text(boot_guid_raw.hex(), limine_extra, default_entry_idx), encoding="utf-8")

    manifest = {
        "release": "Project Ascension 8.0 (v0.8.0)",
        "edition": "Hardware Validation Edition",
        "image_size_mib": size_mib,
        "total_sectors": total_sectors,
        "partitions": {
            "esp": {"label": "DRYOS_ESP", "filesystem": "FAT32", "start_lba": p1_range[0], "end_lba": p1_range[1]},
            "persist": {"label": "DRYOS_PERSIST", "filesystem": "DRYFS_V1", "start_lba": p2_range[0], "end_lba": p2_range[1]},
            "logs": {"label": "DRYOS_LOGS", "filesystem": "FAT32", "start_lba": p3_range[0] if p3_range else 0, "sectors": p3_sectors},
        },
        "module_count": len(USERSPACE_MODS),
    }
    manifest_path = BUILD_DIR / "DRYOS_LIVE_MANIFEST.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    mtools_env = {**os.environ, "LC_ALL": "C"}
    esp_bytes = _build_esp_partition_bytes(BUILD_DIR / "hwval_esp.img", esp_sectors, kernel_elf, u_dir, conf_path, manifest_path, mtools_env)
    p3_bytes = _build_fat32_logs_bytes(BUILD_DIR / "hwval_logs.img", p3_sectors, mtools_env, corrupt_p3_bpb, fill_p3_full) if p3_range else None

    _write_gpt_hwval_image(img_path, total_sectors, p1_range, p2_range, p3_range, disk_guid_raw, esp_bytes, p3_bytes)
    _verify_gpt_and_dryfs(img_path, total_sectors, p1_range, p2_range)

    img_sha = _sha256_file(img_path)
    print(f"[HWVAL-USB] Built {img_path} ({size_mib} MiB 3-part GPT: ESP+DRYOS_PERSIST+DRYOS_LOGS={p3_sectors} secs, sha256={img_sha[:16]}...).")
    return {
        "image_path": str(img_path),
        "image_sha256": img_sha,
        "image_size_mib": size_mib,
        "module_count": len(USERSPACE_MODS),
        "part1_range": p1_range,
        "part2_start_lba": p2_range[0],
        "part2_range": p2_range,
        "part3_range": p3_range,
        "disk_guid_hex": boot_guid_raw.hex(),
    }


def verify_hwval_usb_boot(img_path: pathlib.Path, meta: dict[str, Any]) -> dict[str, Any]:
    from retrieve_dryos_logs import retrieve_and_verify  # noqa: E402

    p2_start_lba = int(meta["part2_start_lba"])
    p3_range = meta["part3_range"]
    assert p3_range is not None
    # Guard everything up to start of P3 (ESP + P2 must remain untouched under `storage=ro`!).
    ro_prefix_bytes = int(p3_range[0]) * SECTOR_BYTES
    run_img = BUILD_DIR / "hwval_usb_verify_run.img"
    vars_copy = BUILD_DIR / "OVMF_VARS_hwval.4m.fd"
    qmp_sock = pathlib.Path(f"/tmp/dryos_hwval_usb_{os.getpid()}.sock")
    bundle_dir = BUILD_DIR / "hwval_verify_bundle"

    shutil.copyfile(img_path, run_img)
    shutil.copyfile(OVMF_VARS, vars_copy)
    qmp_sock.unlink(missing_ok=True)
    pre_ro_sha = _sha256_prefix(run_img, ro_prefix_bytes)

    accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
    cmd = [
        "qemu-system-x86_64", "-machine", f"q35,accel={accel}", "-cpu", "host" if accel == "kvm" else "qemu64",
        "-smp", "2", "-m", "512M",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive", f"if=pflash,format=raw,file={vars_copy}",
        "-vga", "std", "-global", "VGA.vgamem_mb=32",
        "-device", "intel-iommu,intremap=off,caching-mode=on",
        "-device", "qemu-xhci,id=xhci0",
        "-drive", f"if=none,id=usbdisk,format=raw,file={run_img}",
        "-device", "usb-storage,id=usbmsc0,bus=xhci0.0,drive=usbdisk,bootindex=1",
        "-device", "usb-kbd,id=usbkbd0,bus=xhci0.0",
        "-device", "usb-mouse,id=usbmouse0,bus=xhci0.0",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial", "stdio", "-display", "none",
        "-qmp", f"unix:{qmp_sock},server=on,wait=off", "-no-reboot",
    ]
    t0 = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace", bufsize=1)
    line_q: queue.Queue[str | None] = queue.Queue()
    serial_lines: list[str] = []

    def _reader() -> None:
        assert proc.stdout is not None
        for ln in proc.stdout:
            line_q.put(ln)
        line_q.put(None)

    rd = threading.Thread(target=_reader, daemon=True)
    rd.start()

    def wait_for(marker: str, timeout_s: float = 40.0) -> str:
        for ex in serial_lines:
            if marker in ex:
                return ex
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                item = line_q.get(timeout=min(0.25, max(0.05, deadline - time.monotonic())))
            except queue.Empty:
                if proc.poll() is not None and line_q.empty():
                    break
                continue
            if item is None:
                break
            serial_lines.append(item)
            if marker in item:
                return item
        raise RuntimeError(f"HWVAL USB boot timeout waiting for {marker!r}:\n{''.join(serial_lines[-40:])}")

    qmp: _DesktopQmpClient | None = None
    try:
        wait_for("[DRY-HWVAL] pci_passive=1", timeout_s=35.0)
        auto_ln = wait_for("[DRY-HWDIAG] AUTO_COMPLETE", timeout_s=35.0)
        pers_ln = wait_for("[DRY-HWDIAG] PERSIST status=VERIFIED files=13 sha256_verified=1", timeout_s=35.0)
        wait_for("[DRY-SHELL] Interactive shell ready", timeout_s=25.0)
        for _ in range(50):
            if qmp_sock.exists():
                break
            time.sleep(0.1)
        qmp = _DesktopQmpClient(qmp_sock)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        sess_ln = wait_for("[DRY-P2-SESSION] PASS:", timeout_s=25.0)
        wait_for("[DRY-BOOT-OK]", timeout_s=10.0)
        qmp.close()
        qmp = None
        rc = proc.wait(timeout=15.0)
        rd.join(timeout=2.0)
        if rc != EXIT_CODE_SUCCESS or _parse_kv_ints(sess_ln).get("leaked_frames", -1) != 0:
            raise RuntimeError(f"HWVAL USB boot failed: rc={rc} sess={sess_ln}")
        if _sha256_prefix(run_img, ro_prefix_bytes) != pre_ro_sha:
            raise RuntimeError("ESP or DRYOS_PERSIST modified while booting under storage=ro!")
        if p2_start_lba <= 0:
            raise RuntimeError("Invalid part2_start_lba")
        bundle = retrieve_and_verify(run_img, None, bundle_dir, check_redacted_guid=True)
        res = {
            **meta,
            "auto_line": auto_ln.strip(),
            "persist_line": pers_ln.strip(),
            "esp_and_persist_untouched": True,
            "leaked_frames": 0,
            "retrieved_session": bundle["session_id"],
            "files_verified": bundle["files_verified"],
            "boot_to_shutdown_sec": round(time.monotonic() - t0, 3),
        }
        HWVAL_OUT_JSON.parent.mkdir(parents=True, exist_ok=True)
        HWVAL_OUT_JSON.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")
        print(f"[PASS] HWVAL USB xHCI boot + 13-file FAT32/SHA-256 + fsck.fat -n verified ({res['boot_to_shutdown_sec']}s).")
        return res
    finally:
        if qmp is not None:
            qmp.close()
        if proc.poll() is None:
            proc.kill()
        rd.join(timeout=2.0)
        qmp_sock.unlink(missing_ok=True)
        run_img.unlink(missing_ok=True)


def build_live_usb_image(
    img_path: pathlib.Path,
    force_build: bool = False,
    size_mib: int = 160,
    esp_mib: int = 96,
) -> dict[str, Any]:
    if size_mib < 128:
        raise ValueError(f"--size-mib must be >= 128 MiB (got {size_mib})")
    if esp_mib < 48 or (size_mib - esp_mib) < 18:
        raise ValueError(f"Invalid --esp-mib={esp_mib} for --size-mib={size_mib} (need >= 18 MiB for Partition 2 + GPT)")

    kernel_elf, u_dir, ready = _binaries_ready()
    if force_build or not ready:
        kernel_elf = build_kernel()

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    img_path.parent.mkdir(parents=True, exist_ok=True)

    total_sectors = size_mib * SECTORS_PER_MIB
    esp_sectors = esp_mib * SECTORS_PER_MIB
    p1_range = (PART1_START_LBA, PART1_START_LBA + esp_sectors - 1)
    part2_start_lba = p1_range[1] + 1
    part2_end_lba = total_sectors - 34
    p2_range = (part2_start_lba, part2_end_lba)
    part2_sectors = part2_end_lba - part2_start_lba + 1

    conf_path = BUILD_DIR / "live_usb_limine.conf"
    conf_path.write_text(_limine_conf_text(), encoding="utf-8")

    modules_meta = [
        {"name": dst_name, "bytes": (u_dir / src_name).stat().st_size, "sha256": _sha256_file(u_dir / src_name)}
        for src_name, dst_name in USERSPACE_MODS
    ]
    manifest = {
        "release": "Project Ascension 5.0 (v0.5.0)",
        "image_size_mib": size_mib,
        "esp_mib": esp_mib,
        "total_sectors": total_sectors,
        "part2_start_lba": part2_start_lba,
        "part2_end_lba": part2_end_lba,
        "part2_sectors": part2_sectors,
        "partitions": {
            "esp": {"label": "DRYOS_ESP", "filesystem": "FAT32", "start_lba": p1_range[0], "end_lba": p1_range[1], "sectors": esp_sectors},
            "persist": {"label": "DRYOS_PERSIST", "filesystem": "DRYFS_V1", "start_lba": part2_start_lba, "end_lba": part2_end_lba, "sectors": part2_sectors},
        },
        "bootloader": "Limine BOOTX64.EFI",
        "kernel": {"name": "dry-kernel", "bytes": kernel_elf.stat().st_size, "sha256": _sha256_file(kernel_elf)},
        "module_count": len(modules_meta),
        "modules": modules_meta,
    }
    manifest_path = BUILD_DIR / "DRYOS_LIVE_MANIFEST.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    mtools_env = {**os.environ, "LC_ALL": "C"}
    esp_bytes = _build_esp_partition_bytes(BUILD_DIR / "esp_part.img", esp_sectors, kernel_elf, u_dir, conf_path, manifest_path, mtools_env)

    _write_gpt_image(
        img_path,
        total_sectors=total_sectors,
        part1_range=p1_range,
        part2_range=p2_range,
        part2_name="DRYOS_PERSIST",
        seed_part1=esp_bytes,
        format_part2_dryfs=True,
    )
    _verify_gpt_and_dryfs(img_path, total_sectors, p1_range, p2_range)

    img_sha = _sha256_file(img_path)
    print(
        f"[LIVE-USB] Built {img_path} ({size_mib} MiB GPT: ESP={esp_mib} MiB + DRYOS_PERSIST LBA={part2_start_lba}..{part2_end_lba}, "
        f"sha256={img_sha[:16]}..., {len(USERSPACE_MODS)} modules + GPT/DRYFS_V1 verified)."
    )
    return {
        "image_path": str(img_path),
        "image_sha256": img_sha,
        "image_size_mib": size_mib,
        "esp_mib": esp_mib,
        "module_count": len(USERSPACE_MODS),
        "part1_start_lba": p1_range[0],
        "part1_end_lba": p1_range[1],
        "part2_start_lba": part2_start_lba,
        "part2_end_lba": part2_end_lba,
        "part2_sectors": part2_sectors,
        "gpt_verified": True,
        "dryfs_superblocks_verified": True,
    }


def verify_usb_xhci_boot(img_path: pathlib.Path, meta: dict[str, Any]) -> dict[str, Any]:
    part2_start_lba = int(meta["part2_start_lba"])
    esp_prefix_bytes = part2_start_lba * SECTOR_BYTES
    run_img = BUILD_DIR / "live_usb_verify_run.img"
    vars_copy = BUILD_DIR / "OVMF_VARS_live_usb.4m.fd"
    qmp_sock = pathlib.Path(f"/tmp/dryos_live_usb_{os.getpid()}.sock")

    shutil.copyfile(img_path, run_img)
    shutil.copyfile(OVMF_VARS, vars_copy)
    qmp_sock.unlink(missing_ok=True)
    pre_sha = _sha256_prefix(run_img, esp_prefix_bytes)

    accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
    cpu_arg = "host" if accel == "kvm" else "qemu64"
    cmd = [
        "qemu-system-x86_64",
        "-machine", f"q35,accel={accel}",
        "-cpu", cpu_arg,
        "-smp", "2",
        "-m", "512M",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive", f"if=pflash,format=raw,file={vars_copy}",
        "-vga", "std",
        "-global", "VGA.vgamem_mb=32",
        "-device", "intel-iommu,intremap=off,caching-mode=on",
        "-device", "qemu-xhci,id=xhci0",
        "-drive", f"if=none,id=usbdisk,format=raw,file={run_img}",
        "-device", "usb-storage,id=usbmsc0,bus=xhci0.0,drive=usbdisk,bootindex=1",
        "-device", "usb-kbd,id=usbkbd0,bus=xhci0.0",
        "-device", "usb-mouse,id=usbmouse0,bus=xhci0.0",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial", "stdio",
        "-display", "none",
        "-qmp", f"unix:{qmp_sock},server=on,wait=off",
        "-no-reboot",
    ]

    t0 = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace", bufsize=1)
    line_q: queue.Queue[str | None] = queue.Queue()
    serial_lines: list[str] = []

    def _reader() -> None:
        assert proc.stdout is not None
        for ln in proc.stdout:
            line_q.put(ln)
        line_q.put(None)

    rd = threading.Thread(target=_reader, daemon=True)
    rd.start()

    def wait_for(marker: str, timeout_s: float = 35.0) -> str:
        for ex in serial_lines:
            if marker in ex:
                return ex
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                item = line_q.get(timeout=min(0.25, max(0.05, deadline - time.monotonic())))
            except queue.Empty:
                if proc.poll() is not None and line_q.empty():
                    break
                continue
            if item is None:
                break
            serial_lines.append(item)
            if marker in item:
                return item
        raise RuntimeError(f"Live USB xHCI boot timeout waiting for {marker!r}:\n{''.join(serial_lines[-40:])}")

    qmp: _DesktopQmpClient | None = None
    try:
        wait_for("[DRY-LIVEBOOT]", timeout_s=35.0)
        wait_for("[DRY-IOMMU] VTD_ACTIVE", timeout_s=20.0)
        wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1)", timeout_s=30.0)
        wait_for("[DRY-WM-SCALABILITY] pass=1", timeout_s=30.0)
        wait_for("[DRY-SHELL] Interactive shell ready", timeout_s=20.0)
        wait_for("[DRY-SYSMON] Ready", timeout_s=15.0)
        for _ in range(50):
            if qmp_sock.exists():
                break
            time.sleep(0.1)
        qmp = _DesktopQmpClient(qmp_sock)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        sess_ln = wait_for("[DRY-P2-SESSION] PASS:", timeout_s=25.0)
        wait_for("[DRY-BOOT-OK]", timeout_s=10.0)
        qmp.close()
        qmp = None
        rc = proc.wait(timeout=15.0)
        rd.join(timeout=2.0)
        full = "".join(serial_lines)
        if rc != EXIT_CODE_SUCCESS or "[DRY-PANIC]" in full or "[DRY-EXCEPTION]" in full:
            raise RuntimeError(f"Live USB xHCI boot failed: rc={rc}\n{full}")
        skv = _parse_kv_ints(sess_ln)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"Live USB leaked_frames != 0: {sess_ln!r}")

        post_sha = _sha256_prefix(run_img, esp_prefix_bytes)
        if post_sha != pre_sha:
            raise RuntimeError(f"GPT/ESP region modified during RW Live USB boot: {pre_sha} != {post_sha}")

        res = {
            **meta,
            "boot_transport": "intel-iommu (VT-d) + qemu-xhci + usb-storage (RW) + usb-kbd + usb-mouse",
            "iommu_mode": "VTD_ACTIVE",
            "esp_pre_sha256": pre_sha,
            "esp_post_sha256": post_sha,
            "esp_untouched": True,
            "leaked_frames": skv["leaked_frames"],
            "exit_code": rc,
            "boot_to_shutdown_sec": round(time.monotonic() - t0, 3),
        }
        OUT_JSON.parent.mkdir(parents=True, exist_ok=True)
        OUT_JSON.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")
        print(
            f"[PASS] Live USB xHCI RW boot verified: leaked_frames={res['leaked_frames']} "
            f"exit={rc} modules={res['module_count']} esp_untouched=True ({res['boot_to_shutdown_sec']}s)."
        )
        return res
    finally:
        if qmp is not None:
            qmp.close()
        if proc.poll() is None:
            proc.kill()
        rd.join(timeout=2.0)
        qmp_sock.unlink(missing_ok=True)
        run_img.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--hwval", action="store_true", help="Build Ascension 8.0 Hardware Validation Edition (3-partition GPT with DRYOS_LOGS)")
    parser.add_argument("--build", action="store_true", help="Force rebuild of kernel and all userspace ELFs")
    parser.add_argument("--output", type=pathlib.Path, default=None, help="Output Live USB GPT image path")
    parser.add_argument("--size-mib", type=int, default=None, help="Total GPT disk image size in MiB")
    parser.add_argument("--esp-mib", type=int, default=96, help="Partition 1 FAT32 ESP size in MiB (default 96)")
    parser.add_argument("--persist-mib", type=int, default=64, help="Partition 2 DRYOS_PERSIST size in MiB for --hwval (default 64)")
    parser.add_argument("--verify-boot", action="store_true", help="Verify UEFI boot via QEMU qemu-xhci + usb-storage + usb-kbd + usb-mouse")
    args = parser.parse_args()

    if args.hwval:
        out_img = args.output or HWVAL_IMG
        sz = args.size_mib if args.size_mib is not None else 256
        meta = build_hwval_usb_image(out_img, force_build=args.build, size_mib=sz, esp_mib=args.esp_mib, persist_mib=args.persist_mib)
        if args.verify_boot:
            verify_hwval_usb_boot(out_img, meta)
    else:
        out_img = args.output or DEFAULT_IMG
        sz = args.size_mib if args.size_mib is not None else 160
        meta = build_live_usb_image(out_img, force_build=args.build, size_mib=sz, esp_mib=args.esp_mib)
        if args.verify_boot:
            verify_usb_xhci_boot(out_img, meta)


if __name__ == "__main__":
    main()
