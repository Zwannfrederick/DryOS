#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Fetch unmodified upstream Linux v6.18 `8139cp.c` + `mii.c` and record SHA-256 provenance."""

import argparse
import hashlib
import json
import pathlib
import urllib.request
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
VENDOR_DIR = ROOT / "vendor" / "linux-drivers"
LINUX_TAG = "v6.18"
LINUX_COMMIT = "7d0a66e4bb9081d75c82ec4957c50034cb0ea449"
DEST_DIR = VENDOR_DIR / LINUX_TAG / "8139cp"
MANIFEST_PATH = VENDOR_DIR / "MANIFEST.json"

TARGETS = (
    {
        "key": "net_8139cp",
        "upstream_path": "drivers/net/ethernet/realtek/8139cp.c",
        "filename": "8139cp.c",
        "license": "GPL-2.0-only",
    },
    {
        "key": "net_mii",
        "upstream_path": "drivers/net/mii.c",
        "filename": "mii.c",
        "license": "GPL-2.0-only",
    },
)


def _urls(up_path: str) -> list[str]:
    return [
        f"https://raw.githubusercontent.com/torvalds/linux/{LINUX_COMMIT}/{up_path}",
        f"https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/{up_path}?id={LINUX_COMMIT}",
    ]


def http_get(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "dryos-m3-lx-net/0.1"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read()


def verify_offline() -> list[dict[str, Any]]:
    if not MANIFEST_PATH.exists():
        raise RuntimeError(f"Missing {MANIFEST_PATH}")
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    verified = []
    for tgt in TARGETS:
        dest = DEST_DIR / tgt["filename"]
        if not dest.exists():
            raise RuntimeError(f"Missing {dest}")
        rec = manifest.get(tgt["key"], {})
        expected = rec.get("sha256", "")
        actual = hashlib.sha256(dest.read_bytes()).hexdigest()
        if not expected or actual != expected:
            raise RuntimeError(f"SHA-256 mismatch for {dest}: {actual} != {expected}")
        verified.append({"key": tgt["key"], "path": str(dest.relative_to(ROOT)), "sha256": actual})
        print(f"[M3-VERIFY] OK: {dest.relative_to(ROOT)} sha256={actual} (0 modified)")
    return verified


def fetch_one(tgt: dict[str, str], force: bool = False) -> dict[str, Any]:
    dest = DEST_DIR / tgt["filename"]
    if dest.exists() and not force:
        raw = dest.read_bytes()
    else:
        raw = None
        last_err = None
        for url in _urls(tgt["upstream_path"]):
            try:
                raw = http_get(url)
                break
            except Exception as exc:  # noqa: BLE001
                last_err = exc
        if raw is None:
            raise RuntimeError(f"Failed to download {tgt['upstream_path']}@{LINUX_COMMIT}: {last_err}")
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(raw)

    digest = hashlib.sha256(raw).hexdigest()
    lines = raw.decode("utf-8", errors="replace").count("\n")
    return {
        "upstream_path": tgt["upstream_path"],
        "tag": LINUX_TAG,
        "commit_sha": LINUX_COMMIT,
        "license": tgt["license"],
        "local_path": str(dest.relative_to(ROOT)),
        "bytes": len(raw),
        "lines": lines,
        "sha256": digest,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--verify", action="store_true", help="Offline SHA-256 check")
    parser.add_argument("--force", action="store_true", help="Re-download even if present locally")
    args = parser.parse_args()
    if args.verify:
        verify_offline()
        return

    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8")) if MANIFEST_PATH.exists() else {}
    entries = {}
    for tgt in TARGETS:
        rec = fetch_one(tgt, force=args.force)
        entries[tgt["key"]] = rec
        manifest[tgt["key"]] = rec
        print(
            f"[M3-FETCH] Saved {rec['local_path']} bytes={rec['bytes']} "
            f"lines={rec['lines']} sha256={rec['sha256']}"
        )

    manifest["net_8139cp"]["companion_sources"] = [entries["net_mii"]]
    MANIFEST_PATH.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
