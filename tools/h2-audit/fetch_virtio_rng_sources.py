#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Fetch unmodified upstream Linux v6.18 `virtio-rng.c` and record SHA-256 provenance."""

import argparse
import hashlib
import json
import pathlib
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
VENDOR_DIR = ROOT / "vendor" / "linux-drivers"
LINUX_TAG = "v6.18"
LINUX_COMMIT = "7d0a66e4bb9081d75c82ec4957c50034cb0ea449"
UPSTREAM_PATH = "drivers/char/hw_random/virtio-rng.c"
DEST_PATH = VENDOR_DIR / LINUX_TAG / "virtio_rng" / "virtio-rng.c"
MANIFEST_PATH = VENDOR_DIR / "MANIFEST.json"

URLS = [
    f"https://raw.githubusercontent.com/torvalds/linux/{LINUX_COMMIT}/{UPSTREAM_PATH}",
    f"https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/{UPSTREAM_PATH}?id={LINUX_COMMIT}",
]


def http_get(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "dryos-2c1-island/0.1"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read()


def verify_offline() -> None:
    if not DEST_PATH.exists():
        raise RuntimeError(f"Missing {DEST_PATH}")
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    rec = manifest.get("virtio_rng", {})
    digest = hashlib.sha256(DEST_PATH.read_bytes()).hexdigest()
    if digest != rec.get("sha256"):
        raise RuntimeError(f"SHA-256 mismatch for {DEST_PATH}: {digest} != {rec.get('sha256')}")
    print(f"[2C1-VERIFY] OK: {DEST_PATH.relative_to(ROOT)} sha256={digest} (0 modified)")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--verify", action="store_true", help="Offline SHA-256 check")
    args = parser.parse_args()
    if args.verify:
        verify_offline()
        return

    raw = None
    last_err = None
    for url in URLS:
        try:
            raw = http_get(url)
            break
        except Exception as exc:  # noqa: BLE001
            last_err = exc
    if raw is None:
        raise RuntimeError(f"Failed to download {UPSTREAM_PATH}@{LINUX_COMMIT}: {last_err}")

    DEST_PATH.parent.mkdir(parents=True, exist_ok=True)
    DEST_PATH.write_bytes(raw)
    digest = hashlib.sha256(raw).hexdigest()
    lines = raw.decode("utf-8", errors="replace").count("\n")

    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8")) if MANIFEST_PATH.exists() else {}
    manifest["virtio_rng"] = {
        "upstream_path": UPSTREAM_PATH,
        "tag": LINUX_TAG,
        "commit_sha": LINUX_COMMIT,
        "license": "GPL-2.0-only",
        "local_path": str(DEST_PATH.relative_to(ROOT)),
        "bytes": len(raw),
        "lines": lines,
        "sha256": digest,
    }
    MANIFEST_PATH.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"[2C1-FETCH] Saved {DEST_PATH.relative_to(ROOT)} bytes={len(raw)} lines={lines} sha256={digest}")


if __name__ == "__main__":
    main()
