#!/usr/bin/env python3
"""Provision QEMU VirtIO-GPU module plugins locally without modifying host packages.

Downloads Arch `qemu-hw-display-virtio-gpu` and `qemu-hw-display-virtio-gpu-pci`
packages matching the installed `qemu-common` version into `.toolchains/qemu-modules/pkg/`,
extracts only the two shared objects into `.toolchains/qemu-modules/`, and verifies
`QEMU_MODULE_DIR=... qemu-system-x86_64 -device help` exposes `virtio-gpu-pci`.
"""

import io
import os
import re
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MOD_DIR = ROOT / ".toolchains" / "qemu-modules"
PKG_DIR = MOD_DIR / "pkg"
REQUIRED_SOS = ("hw-display-virtio-gpu.so", "hw-display-virtio-gpu-pci.so")
PKGS = ("qemu-hw-display-virtio-gpu", "qemu-hw-display-virtio-gpu-pci")


def verify_virtio_gpu_available(mod_dir: Path) -> bool:
    env = os.environ.copy()
    env["QEMU_MODULE_DIR"] = str(mod_dir)
    proc = subprocess.run(
        ["qemu-system-x86_64", "-device", "help"],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"qemu-system-x86_64 -device help failed: {proc.stderr.strip()}")
    return "virtio-gpu-pci" in proc.stdout


def resolve_package_urls() -> list[str]:
    proc = subprocess.run(
        ["pacman", "-Sp", *PKGS],
        capture_output=True,
        text=True,
        check=False,
    )
    urls = [line.strip() for line in proc.stdout.splitlines() if line.strip().startswith("http")]
    if proc.returncode == 0 and len(urls) == len(PKGS):
        return urls

    qver = subprocess.run(
        ["pacman", "-Q", "qemu-common"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()[1].strip()
    mirror = None
    mirrorlist = Path("/etc/pacman.d/mirrorlist")
    if mirrorlist.exists():
        for raw in mirrorlist.read_text(encoding="utf-8", errors="ignore").splitlines():
            m = re.match(r"^\s*Server\s*=\s*(\S+)", raw)
            if m:
                mirror = m.group(1)
                break
    if not mirror:
        raise RuntimeError("Could not resolve pacman mirror URL for qemu virtio-gpu packages")

    arch = os.uname().machine
    resolved = []
    for pkg in PKGS:
        base = mirror.replace("$repo", "extra").replace("$arch", arch)
        resolved.append(f"{base.rstrip('/')}/{pkg}-{qver}-{arch}.pkg.tar.zst")
    return resolved


def download_and_extract() -> None:
    MOD_DIR.mkdir(parents=True, exist_ok=True)
    PKG_DIR.mkdir(parents=True, exist_ok=True)
    urls = resolve_package_urls()
    for url in urls:
        fname = url.rsplit("/", 1)[-1]
        dst_pkg = PKG_DIR / fname
        if not dst_pkg.exists():
            print(f"[GPU-MOD] Downloading {url}...")
            req = urllib.request.Request(url, headers={"User-Agent": "dryos-bootstrap/1.0"})
            with urllib.request.urlopen(req, timeout=60) as resp:
                data = resp.read()
            dst_pkg.write_bytes(data)
        else:
            print(f"[GPU-MOD] Cached archive {dst_pkg.name}")

        zstd = subprocess.run(
            ["zstd", "-dc", str(dst_pkg)],
            capture_output=True,
            check=True,
        )
        with tarfile.open(fileobj=io.BytesIO(zstd.stdout), mode="r:") as tf:
            for member in tf.getmembers():
                base = Path(member.name).name
                if base in REQUIRED_SOS and member.isfile():
                    f = tf.extractfile(member)
                    assert f is not None
                    out_so = MOD_DIR / base
                    out_so.write_bytes(f.read())
                    os.chmod(out_so, 0o755)
                    print(f"[GPU-MOD] Extracted {out_so} ({out_so.stat().st_size} B)")


def ensure_qemu_virtio_gpu_modules() -> Path:
    if all((MOD_DIR / so).is_file() for so in REQUIRED_SOS) and verify_virtio_gpu_available(MOD_DIR):
        return MOD_DIR
    download_and_extract()
    for so in REQUIRED_SOS:
        if not (MOD_DIR / so).is_file():
            raise RuntimeError(f"Missing extracted QEMU module: {MOD_DIR / so}")
    if not verify_virtio_gpu_available(MOD_DIR):
        raise RuntimeError(f"virtio-gpu-pci unavailable even with QEMU_MODULE_DIR={MOD_DIR}")
    return MOD_DIR


def main() -> int:
    mod_dir = ensure_qemu_virtio_gpu_modules()
    print(f"[GPU-MOD] OK: virtio-gpu-pci available via QEMU_MODULE_DIR={mod_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
