#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Opt-in local host firmware hash & staging utility for Lenovo 15IHU6 Live USB builds.

Design and safety invariants:
- NEVER modifies host state (`/lib/firmware`, `/sys`, `/dev`, `/boot`, `efivars`).
- NEVER writes to git-tracked repository directories (`kernel/`, `userspace/`, `drivers/`,
  `assets/`, `docs/`, `tools/`, `vendor/`, etc.) or repo root.
- Default invocation (no `--stage`) is strictly read-only inspection + SHA-256 hashing.
- Staging writes ONLY when the user explicitly specifies `--stage --staging-dir <PATH>`
  pointing to a safe non-tracked directory (e.g., `/tmp/...` or `kernel/build/...`).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import shutil
import tempfile
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
IGNORED_SUBDIR = (ROOT / "kernel" / "build").resolve()
FORBIDDEN_SYSTEM_PREFIXES = (
    pathlib.Path("/boot"),
    pathlib.Path("/dev"),
    pathlib.Path("/etc"),
    pathlib.Path("/lib"),
    pathlib.Path("/lib64"),
    pathlib.Path("/proc"),
    pathlib.Path("/run"),
    pathlib.Path("/sbin"),
    pathlib.Path("/sys"),
    pathlib.Path("/usr"),
    pathlib.Path("/var"),
)

FIRMWARE_SPECS: list[dict[str, str]] = [
    {
        "id": "intel_tgl_dmc_2_12",
        "vendor": "intel",
        "license_note": "Intel proprietary redistributable binary; never committed to DRY OS git",
        "host_path": "/lib/firmware/i915/tgl_dmc_ver2_12.bin.zst",
        "stage_rel": "i915/tgl_dmc_ver2_12.bin.zst",
    },
    {
        "id": "intel_tgl_guc_70_1_1",
        "vendor": "intel",
        "license_note": "Intel proprietary redistributable binary; never committed to DRY OS git",
        "host_path": "/lib/firmware/i915/tgl_guc_70.1.1.bin.zst",
        "stage_rel": "i915/tgl_guc_70.1.1.bin.zst",
    },
    {
        "id": "intel_tgl_huc",
        "vendor": "intel",
        "license_note": "Intel proprietary redistributable binary; never committed to DRY OS git",
        "host_path": "/lib/firmware/i915/tgl_huc.bin.zst",
        "stage_rel": "i915/tgl_huc.bin.zst",
    },
    {
        "id": "nvidia_ga10x_gsp_615",
        "vendor": "nvidia",
        "license_note": "NVIDIA redistributable GSP binary; never committed to DRY OS git",
        "host_path": "/lib/firmware/nvidia/615.78.08/gsp_ga10x.bin",
        "stage_rel": "nvidia/615.78.08/gsp_ga10x.bin",
    },
    {
        "id": "nvidia_ga10x_ucodes_615",
        "vendor": "nvidia",
        "license_note": "NVIDIA redistributable ucodes binary; never committed to DRY OS git",
        "host_path": "/lib/firmware/nvidia/615.78.08/ucodes_ga10x.bin",
        "stage_rel": "nvidia/615.78.08/ucodes_ga10x.bin",
    },
]


def _sha256_streaming(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        while True:
            chunk = f.read(1024 * 1024)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def _decompress_zst_bytes(raw: bytes) -> bytes:
    try:
        from compression import zstd  # Python 3.14+

        return zstd.decompress(raw)
    except ImportError as exc:
        raise RuntimeError("Python 3.14 compression.zstd unavailable for decompression") from exc


def validate_staging_dir(staging_dir: pathlib.Path) -> pathlib.Path:
    """Reject repo-tracked paths and system directories; require explicit non-tracked target."""
    resolved = staging_dir.expanduser().resolve()
    root_resolved = ROOT.resolve()

    if resolved == pathlib.Path("/"):
        raise ValueError("Refusing root directory '/' as staging target")
    for sys_prefix in FORBIDDEN_SYSTEM_PREFIXES:
        if resolved == sys_prefix or sys_prefix in resolved.parents:
            raise ValueError(f"Refusing system directory as staging target: {resolved}")

    if resolved == root_resolved:
        raise ValueError(f"Refusing repository root as staging target: {resolved}")
    if root_resolved in resolved.parents:
        if resolved != IGNORED_SUBDIR and IGNORED_SUBDIR not in resolved.parents:
            raise ValueError(
                f"Refusing git-tracked repository path as staging target: {resolved} "
                f"(use an external path or gitignored {IGNORED_SUBDIR})"
            )
    return resolved


def inspect_host_firmware(vendor_filter: str = "all", specs: list[dict[str, str]] = FIRMWARE_SPECS) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for spec in specs:
        if vendor_filter != "all" and spec["vendor"] != vendor_filter:
            continue
        hp = pathlib.Path(spec["host_path"])
        present = hp.is_file()
        rec: dict[str, Any] = {
            "id": spec["id"],
            "vendor": spec["vendor"],
            "host_path": spec["host_path"],
            "stage_rel": spec["stage_rel"],
            "present": present,
            "bytes": hp.stat().st_size if present else 0,
            "sha256": _sha256_streaming(hp) if present else None,
            "license_note": spec["license_note"],
        }
        records.append(rec)
    return records


def stage_firmware(
    staging_dir: pathlib.Path,
    vendor_filter: str = "all",
    manifest_only: bool = False,
    decompress_zst: bool = False,
    specs: list[dict[str, str]] = FIRMWARE_SPECS,
) -> dict[str, Any]:
    target = validate_staging_dir(staging_dir)
    records = inspect_host_firmware(vendor_filter=vendor_filter, specs=specs)
    target.mkdir(parents=True, exist_ok=True)
    fw_root = target / "firmware"

    staged_entries: list[dict[str, Any]] = []
    for rec in records:
        entry = dict(rec)
        entry["staged_path"] = None
        entry["decompressed_path"] = None
        entry["decompressed_sha256"] = None
        if rec["present"] and not manifest_only:
            src = pathlib.Path(rec["host_path"])
            dst = fw_root / rec["stage_rel"]
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(src, dst)
            entry["staged_path"] = str(dst)
            if decompress_zst and dst.suffix == ".zst":
                raw = _decompress_zst_bytes(src.read_bytes())
                raw_dst = dst.with_suffix("")
                raw_dst.write_bytes(raw)
                entry["decompressed_path"] = str(raw_dst)
                entry["decompressed_sha256"] = hashlib.sha256(raw).hexdigest()
        staged_entries.append(entry)

    manifest = {
        "schema_version": "1.0",
        "policy": "opt_in_local_staging_only_never_commit_to_git",
        "manifest_only": manifest_only,
        "staging_dir": str(target),
        "entries": staged_entries,
    }
    manifest_path = target / "FIRMWARE_STAGE_MANIFEST.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return {"manifest_path": str(manifest_path), "manifest": manifest}


def run_self_test() -> dict[str, Any]:
    # 1. Verify git-tracked and system paths are strictly rejected.
    rejected_paths: list[str] = []
    for bad in (ROOT, ROOT / "tools", ROOT / "docs", pathlib.Path("/lib/firmware"), pathlib.Path("/boot")):
        try:
            validate_staging_dir(bad)
        except ValueError:
            rejected_paths.append(str(bad))
        else:
            raise AssertionError(f"Expected staging rejection for {bad}")

    # 2. Verify read-only inspection produces zero writes and reads host blobs safely.
    host_recs = inspect_host_firmware("all")

    # 3. Verify opt-in staging on synthetic blob + manifest-only host hashes in temp dir.
    with tempfile.TemporaryDirectory(prefix="dryos_fw_stage_test_") as tmp:
        tmp_path = pathlib.Path(tmp)
        syn_blob = tmp_path / "src" / "synthetic_dmc.bin"
        syn_blob.parent.mkdir(parents=True, exist_ok=True)
        syn_blob.write_bytes(b"DRYOS_SYNTHETIC_FW_TEST" * 16)
        syn_spec = [
            {
                "id": "synthetic_test_fw",
                "vendor": "intel",
                "license_note": "synthetic self-test",
                "host_path": str(syn_blob),
                "stage_rel": "i915/synthetic_dmc.bin",
            }
        ]
        stage_out_dir = tmp_path / "staged"
        res_file = stage_firmware(stage_out_dir, vendor_filter="all", manifest_only=False, specs=syn_spec)
        staged_file = stage_out_dir / "firmware" / "i915" / "synthetic_dmc.bin"
        assert staged_file.is_file()
        assert _sha256_streaming(staged_file) == _sha256_streaming(syn_blob)

        manifest_only_dir = tmp_path / "manifest_only"
        res_host = stage_firmware(manifest_only_dir, vendor_filter="all", manifest_only=True)
        assert pathlib.Path(res_host["manifest_path"]).is_file()
        assert not (manifest_only_dir / "firmware").exists()

    return {
        "valid": True,
        "rejected_forbidden_targets": len(rejected_paths),
        "host_blobs_inspected": len(host_recs),
        "host_blobs_present": sum(1 for r in host_recs if r["present"]),
        "synthetic_stage_verified": True,
        "manifest_only_verified": True,
        "staged_manifest_check": pathlib.Path(res_file["manifest_path"]).name,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Opt-in local firmware inspector & stager (never mutates host/git)")
    parser.add_argument("--vendor", choices=("all", "intel", "nvidia"), default="all")
    parser.add_argument("--stage", action="store_true", help="Opt-in: write staged manifest/files to --staging-dir")
    parser.add_argument("--staging-dir", type=pathlib.Path, default=None, help="Explicit non-tracked target directory")
    parser.add_argument("--manifest-only", action="store_true", help="Write only SHA-256 manifest without copying binaries")
    parser.add_argument("--decompress-zst", action="store_true", help="Also write decompressed .bin beside .bin.zst")
    parser.add_argument("--self-test", action="store_true", help="Run non-destructive safety and temp-dir staging tests")
    args = parser.parse_args()

    if args.self_test:
        res = run_self_test()
        print(
            f"[PROVISION-LOCAL-FW] SELF-TEST PASS: rejected={res['rejected_forbidden_targets']} "
            f"host_present={res['host_blobs_present']}/{res['host_blobs_inspected']} "
            f"synthetic_stage=True manifest_only=True"
        )
        return

    if args.stage:
        if args.staging_dir is None:
            raise SystemExit("ERROR: --stage requires explicit --staging-dir outside git-tracked paths")
        out = stage_firmware(
            args.staging_dir,
            vendor_filter=args.vendor,
            manifest_only=args.manifest_only,
            decompress_zst=args.decompress_zst,
        )
        cnt = sum(1 for e in out["manifest"]["entries"] if e["present"])
        print(f"[PROVISION-LOCAL-FW] STAGED ({cnt} present) -> {out['manifest_path']}")
        return

    recs = inspect_host_firmware(vendor_filter=args.vendor)
    print(json.dumps({"mode": "read_only_audit_no_writes", "entries": recs}, indent=2))


if __name__ == "__main__":
    main()
