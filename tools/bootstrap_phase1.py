#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Bootstrap repo-local, SHA-256-locked Rust no_std sysroot + Limine UEFI binary.

Does NOT modify any host system packages (`pacman` / `/usr`).
"""

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import tarfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
TC_DIR = ROOT / ".toolchains"
SYSROOT_DIR = TC_DIR / "sysroot"
LIMINE_DIR = TC_DIR / "limine"
LOCK_PATH = ROOT / "tools" / "phase1-toolchain-lock.json"

LIMINE_BIN_REPO = "https://github.com/limine-bootloader/limine.git"
LIMINE_PROTO_REPO = "https://github.com/limine-bootloader/limine-protocol.git"
LIMINE_BRANCHES = ["v10.x-binary", "v9.x-binary", "v8.x-binary"]


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def http_get(url: str) -> bytes:
    req = urllib.request.Request(
        url, headers={"User-Agent": "dryos-phase1-bootstrap/0.1"}
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return resp.read()


def detect_host_rustc() -> dict[str, str]:
    out = subprocess.check_output(["rustc", "-vV"], text=True)
    info: dict[str, str] = {"raw": out.splitlines()[0]}
    for line in out.splitlines()[1:]:
        if ":" in line:
            k, v = line.split(":", 1)
            info[k.strip()] = v.strip()
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    info["host_sysroot"] = sysroot
    return info


def setup_rust_src(rustc_info: dict[str, str], refresh: bool = False) -> dict[str, str]:
    release = rustc_info["release"]
    commit = rustc_info["commit-hash"]
    host_triple = rustc_info["host"]
    host_sysroot = pathlib.Path(rustc_info["host_sysroot"])

    archive_dir = TC_DIR / "downloads"
    archive_dir.mkdir(parents=True, exist_ok=True)
    tar_path = archive_dir / f"rust-src-{release}.tar.xz"

    url = f"https://static.rust-lang.org/dist/rust-src-{release}.tar.xz"
    if not tar_path.exists():
        print(f"[BOOT-TC] Fetching {url} ...")
        tar_path.write_bytes(http_get(url))
    digest = sha256_file(tar_path)

    rustlib_dir = SYSROOT_DIR / "lib" / "rustlib"
    src_dst = rustlib_dir / "src" / "rust"
    stamp_path = SYSROOT_DIR / ".rust-src-release"
    cargo_home = TC_DIR / "cargo-home"
    cur_stamp = stamp_path.read_text(encoding="utf-8").strip() if stamp_path.exists() else ""
    src_stale = refresh or cur_stamp != release or not (src_dst / "library" / "Cargo.toml").exists()

    if src_stale:
        if rustlib_dir.exists():
            shutil.rmtree(rustlib_dir)
        rustlib_dir.mkdir(parents=True, exist_ok=True)
        os.symlink(host_sysroot / "lib" / "rustlib" / host_triple, rustlib_dir / host_triple)

        extract_tmp = TC_DIR / "extract_tmp"
        if extract_tmp.exists():
            shutil.rmtree(extract_tmp)
        extract_tmp.mkdir(parents=True)
        with tarfile.open(tar_path, mode="r:xz") as tf:
            tf.extractall(extract_tmp, filter="data")

        candidates = list(extract_tmp.glob("*/rust-src/lib/rustlib/src/rust"))
        if not candidates:
            raise RuntimeError("Could not locate rust-src/lib/rustlib/src/rust in archive")
        src_dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(candidates[0]), str(src_dst))
        shutil.rmtree(extract_tmp)
        SYSROOT_DIR.mkdir(parents=True, exist_ok=True)
        stamp_path.write_text(release + "\n", encoding="utf-8")

    if src_stale or not (cargo_home / "registry").exists():
        cargo_home.mkdir(parents=True, exist_ok=True)
        subprocess.check_call(
            ["cargo", "fetch", "--manifest-path", str(src_dst / "library" / "Cargo.toml")],
            env={**os.environ, "RUSTC_BOOTSTRAP": "1", "CARGO_HOME": str(cargo_home)},
        )

    return {
        "release": release,
        "commit_hash": commit,
        "url": url,
        "archive": str(tar_path.relative_to(ROOT)),
        "sha256": digest,
        "library_path": str((src_dst / "library").relative_to(ROOT)),
    }


def resolve_git_ref(repo: str, refs: list[str]) -> tuple[str, str]:
    out = subprocess.check_output(["git", "ls-remote", repo, *refs], text=True, timeout=30)
    found: dict[str, str] = {}
    for line in out.splitlines():
        parts = line.strip().split()
        if len(parts) == 2:
            found[parts[1]] = parts[0]
    for r in refs:
        if r in found:
            return r, found[r]
    raise RuntimeError(f"Could not resolve any of {refs} in {repo}")


def fetch_cached(dst: pathlib.Path, urls: list[str]) -> tuple[bytes, str]:
    if dst.exists() and dst.stat().st_size > 0:
        return dst.read_bytes(), urls[0]
    last_err = None
    for url in urls:
        try:
            raw = http_get(url)
            dst.write_bytes(raw)
            return raw, url
        except Exception as exc:  # noqa: BLE001
            last_err = exc
    raise RuntimeError(f"Failed to download {dst.name}: {last_err}")


def setup_limine() -> dict[str, object]:
    LIMINE_DIR.mkdir(parents=True, exist_ok=True)
    bin_ref, bin_commit = resolve_git_ref(
        LIMINE_BIN_REPO, [f"refs/heads/{b}" for b in LIMINE_BRANCHES]
    )
    _, proto_commit = resolve_git_ref(LIMINE_PROTO_REPO, ["HEAD"])

    files_meta = []
    efi_raw, efi_url = fetch_cached(
        LIMINE_DIR / "BOOTX64.EFI",
        [f"https://raw.githubusercontent.com/limine-bootloader/limine/{bin_commit}/BOOTX64.EFI"],
    )
    files_meta.append(
        {"name": "BOOTX64.EFI", "bytes": len(efi_raw), "sha256": sha256_bytes(efi_raw), "url": efi_url}
    )

    lic_raw, lic_url = fetch_cached(
        LIMINE_DIR / "LICENSE",
        [
            f"https://raw.githubusercontent.com/limine-bootloader/limine/{bin_commit}/LICENSE.md",
            f"https://raw.githubusercontent.com/limine-bootloader/limine/{bin_commit}/COPYING",
            f"https://raw.githubusercontent.com/limine-bootloader/limine-protocol/{proto_commit}/LICENSE",
        ],
    )
    files_meta.append(
        {"name": "LICENSE", "bytes": len(lic_raw), "sha256": sha256_bytes(lic_raw), "url": lic_url}
    )

    hdr_raw, hdr_url = fetch_cached(
        LIMINE_DIR / "limine.h",
        [f"https://raw.githubusercontent.com/limine-bootloader/limine-protocol/{proto_commit}/include/limine.h"],
    )
    files_meta.append(
        {"name": "limine.h", "bytes": len(hdr_raw), "sha256": sha256_bytes(hdr_raw), "url": hdr_url}
    )

    limine_h = hdr_raw.decode("utf-8", errors="replace")
    return {
        "binary_repo": LIMINE_BIN_REPO,
        "binary_ref": bin_ref,
        "binary_commit_sha": bin_commit,
        "protocol_repo": LIMINE_PROTO_REPO,
        "protocol_commit_sha": proto_commit,
        "license": "BSD-2-Clause / 0BSD",
        "supports_mp_request": "LIMINE_MP_REQUEST" in limine_h,
        "files": files_meta,
    }


def verify_lock() -> None:
    if not LOCK_PATH.exists():
        raise RuntimeError(f"Missing lockfile {LOCK_PATH}")
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    rs = lock["rust_src"]
    tar_path = ROOT / rs["archive"]
    if not tar_path.exists() or sha256_file(tar_path) != rs["sha256"]:
        raise RuntimeError(f"rust-src archive missing or SHA-256 mismatch: {tar_path}")
    if not (ROOT / rs["library_path"] / "Cargo.toml").exists():
        raise RuntimeError("Extracted rust-src library missing")

    lim = lock["limine"]
    for entry in lim["files"]:
        fp = LIMINE_DIR / entry["name"]
        if not fp.exists() or sha256_file(fp) != entry["sha256"]:
            raise RuntimeError(f"Limine artifact missing or SHA-256 mismatch: {fp}")

    print(
        f"[BOOT-TC] VERIFIED OK: rust-src {rs['release']} + Limine "
        f"{lim['binary_commit_sha'][:12]} / proto {lim['protocol_commit_sha'][:12]} "
        f"(0 host packages modified)"
    )


def _existing_valid_limine_lock() -> dict[str, object] | None:
    if not LOCK_PATH.exists():
        return None
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    lim = lock.get("limine")
    if not isinstance(lim, dict) or not isinstance(lim.get("files"), list):
        return None
    for entry in lim["files"]:
        fp = LIMINE_DIR / entry["name"]
        if not fp.exists() or sha256_file(fp) != entry["sha256"]:
            return None
    return lim


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--verify", action="store_true", help="Offline SHA-256 check")
    parser.add_argument(
        "--refresh-rust-src",
        action="store_true",
        help="Re-sync repo-local rust-src and cargo-home to current host rustc while keeping pinned Limine artifacts",
    )
    args = parser.parse_args()
    if args.verify:
        verify_lock()
        return

    rustc_info = detect_host_rustc()
    rs_meta = setup_rust_src(rustc_info, refresh=args.refresh_rust_src)
    lim_meta = _existing_valid_limine_lock() if args.refresh_rust_src else None
    if lim_meta is None:
        lim_meta = setup_limine()
    lock = {"host_rustc": rustc_info["raw"], "rust_src": rs_meta, "limine": lim_meta}
    LOCK_PATH.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
    verify_lock()


if __name__ == "__main__":
    main()
