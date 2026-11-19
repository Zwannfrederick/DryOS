#!/usr/bin/env python3
"""Provision an isolated local QEMU VirtIO-GPU 3D (Virgl + GL + EGL) test environment.

Downloads Arch `virglrenderer`, `qemu-hw-display-virtio-gpu-gl`,
`qemu-hw-display-virtio-gpu-pci-gl`, `qemu-ui-opengl`, and `qemu-ui-egl-headless`
packages (matching installed `qemu-common`) into `.toolchains/virgl-env/pkg/`
WITHOUT modifying host system packages, extracts shared objects into
`.toolchains/virgl-env/{qemu-modules,lib}/`, checks transitive `ldd` deps,
and verifies `virtio-gpu-gl-pci` and `egl-headless` availability.
"""

from __future__ import annotations

import io
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request

from bootstrap_qemu_gpu_modules import ensure_qemu_virtio_gpu_modules

ROOT = pathlib.Path(__file__).resolve().parent.parent
VIRGL_DIR = ROOT / ".toolchains" / "virgl-env"
PKG_DIR = VIRGL_DIR / "pkg"
MOD_DIR = VIRGL_DIR / "qemu-modules"
LIB_DIR = VIRGL_DIR / "lib"

BASE_2D_SOS = ("hw-display-virtio-gpu.so", "hw-display-virtio-gpu-pci.so")
REQUIRED_GL_SOS = (
    "hw-display-virtio-gpu-gl.so",
    "hw-display-virtio-gpu-pci-gl.so",
    "ui-opengl.so",
    "ui-egl-headless.so",
)
PKGS = (
    "virglrenderer",
    "qemu-hw-display-virtio-gpu-gl",
    "qemu-hw-display-virtio-gpu-pci-gl",
    "qemu-ui-opengl",
    "qemu-ui-egl-headless",
)


def virgl_env(base: dict[str, str] | None = None) -> dict[str, str]:
    env = (base or os.environ).copy()
    env["QEMU_MODULE_DIR"] = str(MOD_DIR)
    prev_ld = env.get("LD_LIBRARY_PATH", "")
    env["LD_LIBRARY_PATH"] = f"{LIB_DIR}:{prev_ld}" if prev_ld else str(LIB_DIR)
    mesa_egl = pathlib.Path("/usr/share/glvnd/egl_vendor.d/50_mesa.json")
    if mesa_egl.is_file():
        env.setdefault("__EGL_VENDOR_LIBRARY_FILENAMES", str(mesa_egl))
    env.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
    return env


def verify_virgl_qemu_exposed() -> tuple[bool, bool]:
    env = virgl_env()
    dev = subprocess.run(
        ["qemu-system-x86_64", "-device", "help"],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    disp = subprocess.run(
        ["qemu-system-x86_64", "-display", "help"],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    has_gl_pci = dev.returncode == 0 and "virtio-gpu-gl-pci" in dev.stdout
    has_egl_headless = disp.returncode == 0 and "egl-headless" in disp.stdout
    return has_gl_pci, has_egl_headless


def _resolve_urls_for_pkgs(pkgs: tuple[str, ...] | list[str]) -> list[str]:
    proc = subprocess.run(
        ["pacman", "-Sp", "--noconfirm", *pkgs],
        capture_output=True,
        text=True,
        check=False,
    )
    urls = [ln.strip() for ln in proc.stdout.splitlines() if ln.strip().startswith("http")]
    if proc.returncode == 0 and len(urls) >= len(pkgs):
        return urls

    qver = subprocess.run(
        ["pacman", "-Q", "qemu-common"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()[1].strip()
    mirror = None
    mirrorlist = pathlib.Path("/etc/pacman.d/mirrorlist")
    if mirrorlist.exists():
        for raw in mirrorlist.read_text(encoding="utf-8", errors="ignore").splitlines():
            m = re.match(r"^\s*Server\s*=\s*(\S+)", raw)
            if m:
                mirror = m.group(1)
                break
    if not mirror:
        raise RuntimeError(f"Could not resolve pacman mirror URL for packages: {pkgs!r}")

    arch = os.uname().machine
    base = mirror.replace("$repo", "extra").replace("$arch", arch).rstrip("/")
    resolved: list[str] = []
    for pkg in pkgs:
        if pkg.startswith("qemu-"):
            resolved.append(f"{base}/{pkg}-{qver}-{arch}.pkg.tar.zst")
        else:
            raise RuntimeError(f"pacman -Sp failed for non-qemu package {pkg!r}: {proc.stderr.strip()}")
    return resolved


def _download_and_extract_pkg(url: str) -> None:
    fname = url.rsplit("/", 1)[-1]
    dst_pkg = PKG_DIR / fname
    if not dst_pkg.exists():
        print(f"[VIRGL-ENV] Downloading {url}...")
        req = urllib.request.Request(url, headers={"User-Agent": "dryos-virgl-bootstrap/1.0"})
        with urllib.request.urlopen(req, timeout=60) as resp:
            dst_pkg.write_bytes(resp.read())
    else:
        print(f"[VIRGL-ENV] Cached archive {dst_pkg.name}")

    zstd = subprocess.run(["zstd", "-dc", str(dst_pkg)], capture_output=True, check=True)
    with tarfile.open(fileobj=io.BytesIO(zstd.stdout), mode="r:") as tf:
        members = tf.getmembers()
        # Extract regular files first, then relative symlinks so targets exist cleanly.
        for pass_sym in (False, True):
            for member in members:
                rel = member.name.lstrip("./")
                if rel.startswith("usr/lib/qemu/") and ".so" in pathlib.Path(rel).name:
                    out_dir = MOD_DIR
                elif rel.startswith("usr/lib/") and ".so" in pathlib.Path(rel).name and "/" not in rel[len("usr/lib/"):]:
                    out_dir = LIB_DIR
                else:
                    continue

                base = pathlib.Path(rel).name
                out_path = out_dir / base
                if not pass_sym and member.isfile():
                    f = tf.extractfile(member)
                    assert f is not None
                    if out_path.is_symlink() or out_path.exists():
                        out_path.unlink()
                    out_path.write_bytes(f.read())
                    os.chmod(out_path, 0o755)
                    print(f"[VIRGL-ENV] Extracted {out_path} ({out_path.stat().st_size} B)")
                elif pass_sym and member.issym():
                    if out_path.is_symlink() or out_path.exists():
                        out_path.unlink()
                    link_target = pathlib.Path(member.linkname).name
                    out_path.symlink_to(link_target)
                    print(f"[VIRGL-ENV] Linked {out_path} -> {link_target}")


def _find_missing_shared_libs() -> set[str]:
    missing: set[str] = set()
    env = virgl_env()
    targets = list(MOD_DIR.glob("*.so")) + [p for p in LIB_DIR.glob("*.so*") if not p.is_symlink()]
    for so in targets:
        proc = subprocess.run(["ldd", str(so)], env=env, capture_output=True, text=True, check=False)
        for line in proc.stdout.splitlines():
            if "=> not found" in line:
                soname = line.strip().split("=>", 1)[0].strip()
                if soname:
                    missing.add(soname)
    return missing


def _resolve_missing_pkg_for_soname(soname: str) -> str | None:
    proc = subprocess.run(
        ["pacman", "-Fq", f"usr/lib/{soname}"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode == 0:
        for line in proc.stdout.splitlines():
            pkg = line.strip().split("/", 1)[-1]
            if pkg:
                return pkg
    # Conservative fallback stem without triggering extra guesswork on host state.
    stem = soname.split(".so", 1)[0]
    if stem.startswith("lib"):
        return stem
    return None


def ensure_virgl_env() -> tuple[pathlib.Path, pathlib.Path]:
    MOD_DIR.mkdir(parents=True, exist_ok=True)
    LIB_DIR.mkdir(parents=True, exist_ok=True)
    PKG_DIR.mkdir(parents=True, exist_ok=True)

    # Always keep base 2D VirtIO-GPU modules present alongside GL modules in MOD_DIR.
    base_mod_dir = ensure_qemu_virtio_gpu_modules()
    for base_so in BASE_2D_SOS:
        src = base_mod_dir / base_so
        dst = MOD_DIR / base_so
        if not dst.exists() or dst.stat().st_size != src.stat().st_size:
            shutil.copy2(src, dst)

    have_mods = all((MOD_DIR / so).exists() for so in (*BASE_2D_SOS, *REQUIRED_GL_SOS))
    have_lib = (LIB_DIR / "libvirglrenderer.so.1").exists()
    if have_mods and have_lib and not _find_missing_shared_libs():
        ok_gl, ok_egl = verify_virgl_qemu_exposed()
        if ok_gl and ok_egl:
            return MOD_DIR, LIB_DIR

    for url in _resolve_urls_for_pkgs(PKGS):
        _download_and_extract_pkg(url)

    # If transitive shared libraries (e.g., epoxy/gbm helpers) are missing, pull them into LIB_DIR only.
    for _ in range(3):
        missing = _find_missing_shared_libs()
        if not missing:
            break
        pkgs_needed: list[str] = []
        for soname in sorted(missing):
            pkg = _resolve_missing_pkg_for_soname(soname)
            if pkg and pkg not in pkgs_needed:
                pkgs_needed.append(pkg)
        if not pkgs_needed:
            break
        for url in _resolve_urls_for_pkgs(pkgs_needed):
            _download_and_extract_pkg(url)

    ok_gl, ok_egl = verify_virgl_qemu_exposed()
    if not (ok_gl and ok_egl):
        rem = sorted(_find_missing_shared_libs())
        raise RuntimeError(
            f"Virgl isolated env incomplete: gl_pci={ok_gl} egl_headless={ok_egl} missing_libs={rem!r}"
        )
    return MOD_DIR, LIB_DIR


def main() -> int:
    mod_dir, lib_dir = ensure_virgl_env()
    print(f"[VIRGL-ENV] OK: QEMU_MODULE_DIR={mod_dir} LD_LIBRARY_PATH={lib_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
