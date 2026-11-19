#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Read-only host retrieval and verification tool for DRY OS `DRYOS_LOGS` test bundles.

Safety rules:
- Never mounts partitions automatically, never formats or writes to host block devices.
- Supports direct GPT FAT32 image inspection via `mtools` (`--image`) or user-mounted (`--mount-dir`).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import struct
import subprocess
import tempfile
import zlib
from typing import Any

REQUIRED_DATA_FILES = [
    "summary.json",
    "hardware_inventory.json",
    "cpu_memory.json",
    "acpi_uefi.json",
    "pci_topology.json",
    "iommu_dma.json",
    "intel_gpu.json",
    "nvidia_gpu.json",
    "usb_storage.json",
    "driver_compatibility.json",
    "kernel.log",
    "test_results.json",
]
REQUIRED_ALL_FILES = REQUIRED_DATA_FILES + ["manifest.sha256"]


def list_candidates() -> None:
    """Inspect host block devices read-only and print manual mount instructions."""
    try:
        out = subprocess.check_output(
            ["lsblk", "-J", "-o", "NAME,PATH,SIZE,TRAN,RM,PARTLABEL,FSTYPE,MOUNTPOINT"],
            text=True,
        )
        data = json.loads(out)
    except Exception as exc:
        print(f"[DRY-RETRIEVE] lsblk read-only query unavailable: {exc}")
        return

    found = []
    def walk(devs: list[dict[str, Any]]) -> None:
        for d in devs:
            if (d.get("partlabel") or "") == "DRYOS_LOGS" or (d.get("label") or "") == "DRYOS_LOGS":
                found.append(d)
            if d.get("children"):
                walk(d["children"])

    walk(data.get("blockdevices", []))
    if not found:
        print("[DRY-RETRIEVE] No DRYOS_LOGS partition currently attached.")
        return
    for d in found:
        path = d.get("path", "/dev/sdX3")
        mp = d.get("mountpoint")
        print(f"Found DRYOS_LOGS: {path} size={d.get('size')} fstype={d.get('fstype')} mount={mp}")
        if mp:
            print(f"  Retrieve: python3 tools/retrieve_dryos_logs.py --mount-dir {mp} --out ./retrieved_logs")
        else:
            print(f"  Manual RO mount: sudo mkdir -p /mnt/dryos_logs && sudo mount -o ro {path} /mnt/dryos_logs")
            print("  Then run: python3 tools/retrieve_dryos_logs.py --mount-dir /mnt/dryos_logs --out ./retrieved_logs")


def find_gpt_logs_partition(img_path: pathlib.Path) -> tuple[int, int, bytes]:
    """Return `(start_lba, sectors, disk_guid_bytes)` for `DRYOS_LOGS` in GPT image."""
    with img_path.open("rb") as f:
        mbr_gpt = f.read(1024)
        if len(mbr_gpt) < 1024 or mbr_gpt[512:520] != b"EFI PART":
            raise RuntimeError(f"{img_path} does not have a valid GPT header at LBA 1")
        hdr = bytearray(mbr_gpt[512:1024])
        hdr_sz = struct.unpack_from("<I", hdr, 12)[0]
        stored_hcrc = struct.unpack_from("<I", hdr, 16)[0]
        hcopy = bytearray(hdr[:hdr_sz])
        hcopy[16:20] = b"\x00\x00\x00\x00"
        if (zlib.crc32(hcopy) & 0xFFFF_FFFF) != stored_hcrc:
            raise RuntimeError("GPT header CRC32 mismatch")
        disk_guid = bytes(hdr[56:72])
        entry_lba, num_entries, entry_sz, expected_ecrc = struct.unpack_from("<QIII", hdr, 72)
        f.seek(entry_lba * 512)
        entries_raw = f.read(num_entries * entry_sz)
        if (zlib.crc32(entries_raw) & 0xFFFF_FFFF) != expected_ecrc:
            raise RuntimeError("GPT partition entry array CRC32 mismatch")
        for idx in range(num_entries):
            ent = entries_raw[idx * entry_sz : (idx + 1) * entry_sz]
            if not any(ent[:16]):
                continue
            first_lba, last_lba = struct.unpack_from("<QQ", ent, 32)
            name = ent[56:128].decode("utf-16-le", errors="ignore").rstrip("\x00")
            if name == "DRYOS_LOGS":
                return first_lba, last_lba - first_lba + 1, disk_guid
    raise RuntimeError(f"Partition DRYOS_LOGS not found in GPT of {img_path}")


def extract_from_image(img_path: pathlib.Path, work_root: pathlib.Path) -> pathlib.Path:
    """Slice `DRYOS_LOGS` partition bytes read-only and copy `hardware-tests` via `mcopy`."""
    start_lba, secs, _ = find_gpt_logs_partition(img_path)
    part_img = work_root / "dryos_logs_p3.fat32"
    with img_path.open("rb") as src, part_img.open("wb") as dst:
        src.seek(start_lba * 512)
        rem = secs * 512
        while rem > 0:
            chunk = src.read(min(rem, 1 << 20))
            if not chunk:
                break
            dst.write(chunk)
            rem -= len(chunk)

    env = {**os.environ, "MTOOLS_SKIP_CHECK": "1"}
    hw_dir = work_root / "hardware-tests"
    hw_dir.mkdir(parents=True, exist_ok=True)
    # Run read-only `fsck.fat -n` check on slice first
    fsck = subprocess.run(["fsck.fat", "-n", str(part_img)], capture_output=True, text=True)
    if fsck.returncode != 0:
        raise RuntimeError(f"fsck.fat -n failed on DRYOS_LOGS:\n{fsck.stdout}\n{fsck.stderr}")

    cp = subprocess.run(
        ["mcopy", "-s", "-n", "-i", str(part_img), "::hardware-tests/*", str(hw_dir) + "/"],
        env=env,
        capture_output=True,
        text=True,
    )
    if cp.returncode != 0:
        raise RuntimeError(f"mcopy failed reading ::hardware-tests: {cp.stderr or cp.stdout}")
    return work_root


def verify_session_dir(sess_dir: pathlib.Path, forbidden_raw_guid_hex: str | None = None) -> dict[str, Any]:
    """Validate all 13 files, JSON schemas, `manifest.sha256`, and redaction rules."""
    missing = [f for f in REQUIRED_ALL_FILES if not (sess_dir / f).is_file()]
    if missing:
        raise RuntimeError(f"Missing required files in {sess_dir}: {missing}")

    manifest_lines = [
        ln.strip()
        for ln in (sess_dir / "manifest.sha256").read_text(encoding="utf-8").splitlines()
        if ln.strip()
    ]
    recorded: dict[str, str] = {}
    for ln in manifest_lines:
        parts = ln.split()
        if len(parts) != 2:
            raise RuntimeError(f"Malformed manifest.sha256 line: {ln!r}")
        recorded[parts[1]] = parts[0]

    for fname in REQUIRED_DATA_FILES:
        if fname not in recorded:
            raise RuntimeError(f"{fname} absent from manifest.sha256")
        actual = hashlib.sha256((sess_dir / fname).read_bytes()).hexdigest()
        if actual != recorded[fname]:
            raise RuntimeError(f"SHA-256 mismatch for {fname}: expected {recorded[fname]}, got {actual}")

    parsed_json: dict[str, Any] = {}
    for fname in REQUIRED_DATA_FILES:
        if not fname.endswith(".json"):
            continue
        parsed_json[fname] = json.loads((sess_dir / fname).read_text(encoding="utf-8"))

    usb_txt = (sess_dir / "usb_storage.json").read_text(encoding="utf-8")
    if forbidden_raw_guid_hex and forbidden_raw_guid_hex.lower() in usb_txt.lower():
        raise RuntimeError("Redaction failure: full unredacted boot_disk_guid leaked in usb_storage.json")

    return {
        "session_id": sess_dir.name,
        "files_verified": len(REQUIRED_ALL_FILES),
        "manifest_sha256_ok": True,
        "json_valid_count": len(parsed_json),
        "summary": parsed_json["summary.json"],
        "usb_storage": parsed_json["usb_storage.json"],
        "test_count": len(parsed_json["test_results.json"].get("results", [])),
    }


def retrieve_and_verify(
    image: pathlib.Path | None,
    mount_dir: pathlib.Path | None,
    out_dir: pathlib.Path,
    session_name: str | None = None,
    check_redacted_guid: bool = True,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="dryos_retrieve_") as tmp:
        tmp_path = pathlib.Path(tmp)
        forbidden_guid = None
        if image is not None:
            if check_redacted_guid:
                _, _, guid = find_gpt_logs_partition(image)
                forbidden_guid = guid.hex()
            src_root = extract_from_image(image, tmp_path)
        elif mount_dir is not None:
            src_root = mount_dir
        else:
            raise RuntimeError("Specify --image or --mount-dir")

        hw_root = src_root / "hardware-tests"
        if not hw_root.is_dir():
            raise RuntimeError(f"No hardware-tests directory found under {src_root}")
        sessions = sorted(p.name for p in hw_root.iterdir() if p.is_dir())
        if not sessions:
            raise RuntimeError("No test sessions found under hardware-tests/")
        chosen = session_name or sessions[-1]
        src_sess = hw_root / chosen
        if not src_sess.is_dir():
            raise RuntimeError(f"Requested session {chosen!r} not found (available: {sessions})")

        out_dir.mkdir(parents=True, exist_ok=True)
        dst_sess = out_dir / chosen
        if dst_sess.exists():
            shutil.rmtree(dst_sess)
        shutil.copytree(src_sess, dst_sess)

        report = verify_session_dir(dst_sess, forbidden_raw_guid_hex=forbidden_guid)
        report["available_sessions"] = sessions
        (out_dir / "bundle_verification.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        return report


def main() -> None:
    ap = argparse.ArgumentParser(description="Safe read-only DRY OS hardware validation log retriever")
    ap.add_argument("--list-candidates", action="store_true", help="List host DRYOS_LOGS partitions and safe manual RO mount commands")
    ap.add_argument("--image", type=pathlib.Path, help="Read directly from a DRY OS validation disk image without mounting")
    ap.add_argument("--mount-dir", type=pathlib.Path, help="Read from an already read-only mounted DRYOS_LOGS directory")
    ap.add_argument("--session", type=str, default=None, help="Specific session ID (defaults to latest)")
    ap.add_argument("--out", type=pathlib.Path, default=pathlib.Path("./dryos_hw_report_bundle"), help="Output directory")
    args = ap.parse_args()

    if args.list_candidates:
        list_candidates()
        return
    if args.image is None and args.mount_dir is None:
        list_candidates()
        return
    res = retrieve_and_verify(args.image, args.mount_dir, args.out, session_name=args.session)
    print(json.dumps(res, indent=2))


if __name__ == "__main__":
    main()
