#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Fetch upstream Linux v6.12 and v6.18 Intel `igc` driver sources and record commit/file SHAs."""

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
VENDOR_DIR = ROOT / "vendor" / "linux-drivers"
TAGS = ["v6.12", "v6.18"]
SUBDIR = "drivers/net/ethernet/intel/igc"

GIT_REMOTES = [
    "https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git",
    "https://github.com/torvalds/linux.git",
]


def resolve_tag_commits() -> dict[str, dict[str, str]]:
    refs = []
    for tag in TAGS:
        refs.extend([f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"])

    last_err = None
    for remote in GIT_REMOTES:
        try:
            out = subprocess.check_output(
                ["git", "ls-remote", remote, *refs], text=True, timeout=20
            )
            mapping: dict[str, dict[str, str]] = {
                t: {"remote": remote, "tag_object": "", "commit": ""} for t in TAGS
            }
            for line in out.splitlines():
                sha, ref = line.strip().split()
                for t in TAGS:
                    if ref == f"refs/tags/{t}":
                        mapping[t]["tag_object"] = sha
                    elif ref == f"refs/tags/{t}^{{}}":
                        mapping[t]["commit"] = sha
            for t in TAGS:
                if not mapping[t]["commit"]:
                    mapping[t]["commit"] = mapping[t]["tag_object"]
                if not mapping[t]["commit"]:
                    raise RuntimeError(f"Missing commit SHA for {t} from {remote}")
            return mapping
        except Exception as exc:  # noqa: BLE001
            last_err = exc
    raise RuntimeError(f"Failed to resolve tags from remotes: {last_err}")


def http_get(url: str) -> bytes:
    req = urllib.request.Request(
        url, headers={"User-Agent": "dryos-h2-audit/0.1 (research)"}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read()


def list_igc_files(commit: str) -> list[str]:
    api_url = f"https://api.github.com/repos/torvalds/linux/contents/{SUBDIR}?ref={commit}"
    try:
        items = json.loads(http_get(api_url).decode("utf-8"))
        return sorted(item["name"] for item in items if item.get("type") == "file")
    except Exception:  # noqa: BLE001
        tree_url = f"https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/{SUBDIR}?id={commit}"
        html = http_get(tree_url).decode("utf-8", errors="replace")
        names = set(re.findall(rf"/{SUBDIR}/([A-Za-z0-9_.-]+)\?id=", html))
        if not names:
            raise RuntimeError(f"Could not list {SUBDIR} at {commit}")
        return sorted(names)


def download_file(commit: str, filename: str) -> bytes:
    urls = [
        f"https://raw.githubusercontent.com/torvalds/linux/{commit}/{SUBDIR}/{filename}",
        f"https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/{SUBDIR}/{filename}?id={commit}",
    ]
    last_err = None
    for url in urls:
        try:
            return http_get(url)
        except Exception as exc:  # noqa: BLE001
            last_err = exc
    raise RuntimeError(f"Failed to download {filename}@{commit}: {last_err}")


def verify_manifest(manifest_path: pathlib.Path) -> None:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    modified = []
    total_files = 0
    for tag, info in manifest["versions"].items():
        out_dir = VENDOR_DIR / tag / "igc"
        for entry in info["files"]:
            total_files += 1
            fpath = out_dir / entry["name"]
            if not fpath.exists():
                modified.append(f"{tag}/{entry['name']} (missing)")
                continue
            digest = hashlib.sha256(fpath.read_bytes()).hexdigest()
            if digest != entry["sha256"]:
                modified.append(f"{tag}/{entry['name']} (sha256 mismatch)")
        print(
            f"[H2-VERIFY] {tag}: commit={info['commit_sha']} files={info['file_count']} "
            f"lines={info['total_lines']}"
        )
    if modified:
        raise RuntimeError(f"Upstream files modified or missing: {modified}")
    print(f"[H2-VERIFY] OK: {total_files}/{total_files} upstream files match SHA-256 (0 modified)")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--verify",
        action="store_true",
        help="Offline SHA-256 verification against vendor/linux-drivers/MANIFEST.json",
    )
    args = parser.parse_args()
    manifest_path = VENDOR_DIR / "MANIFEST.json"
    if args.verify:
        verify_manifest(manifest_path)
        return

    VENDOR_DIR.mkdir(parents=True, exist_ok=True)
    resolved = resolve_tag_commits()
    manifest: dict[str, object] = {
        "subsystem": SUBDIR,
        "license": "GPL-2.0-only",
        "versions": {},
    }

    for tag in TAGS:
        info = resolved[tag]
        commit = info["commit"]
        out_dir = VENDOR_DIR / tag / "igc"
        out_dir.mkdir(parents=True, exist_ok=True)

        filenames = list_igc_files(commit)
        files_meta = []
        total_lines = 0
        for name in filenames:
            raw = download_file(commit, name)
            (out_dir / name).write_bytes(raw)
            digest = hashlib.sha256(raw).hexdigest()
            lines = raw.decode("utf-8", errors="replace").count("\n")
            total_lines += lines
            files_meta.append(
                {
                    "name": name,
                    "bytes": len(raw),
                    "lines": lines,
                    "sha256": digest,
                }
            )

        # Verify every local `#include "..."` inside igc resolves within downloaded files
        downloaded_set = set(filenames)
        missing_local = []
        for name in filenames:
            if name.endswith((".c", ".h")):
                txt = (out_dir / name).read_text(encoding="utf-8", errors="replace")
                for inc in re.findall(r'^\s*#\s*include\s*"([^"]+)"', txt, flags=re.M):
                    if inc not in downloaded_set:
                        missing_local.append(f"{name} -> {inc}")
        if missing_local:
            raise RuntimeError(f"Missing local includes for {tag}: {missing_local}")

        manifest["versions"][tag] = {
            "git_remote": info["remote"],
            "tag_object_sha": info["tag_object"],
            "commit_sha": commit,
            "file_count": len(files_meta),
            "total_lines": total_lines,
            "files": files_meta,
        }
        print(
            f"[H2-FETCH] {tag}: commit={commit} files={len(files_meta)} total_lines={total_lines}"
        )

    manifest_path = VENDOR_DIR / "MANIFEST.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"[H2-FETCH] Wrote {manifest_path}")


if __name__ == "__main__":
    main()

