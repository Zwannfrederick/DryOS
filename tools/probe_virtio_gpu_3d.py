#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Read-only host & QEMU VirtIO-GPU 3D (`virgl` / `virtio-gpu-gl-pci`) capability probe (`Milestone 5`).

Inspects QEMU device help/properties and host virgl libraries WITHOUT opening `/dev/*`,
without writing sysfs, and without modifying host GPU bindings.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / "tools" / "compat_bench" / "results_virtio_gpu_3d_probe.json"


def _run_text(cmd: list[str], timeout_s: float = 10.0) -> tuple[int, str]:
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout_s, check=False)
        return proc.returncode, (proc.stdout + "\n" + proc.stderr).strip()
    except (OSError, subprocess.TimeoutExpired) as exc:
        return -1, str(exc)


def probe_virtio_gpu_3d() -> dict[str, Any]:
    qemu = shutil.which("qemu-system-x86_64")
    if not qemu:
        return {
            "status": "HOST_VIRGL_UNAVAILABLE",
            "reason": "qemu-system-x86_64 not found",
            "qemu_binary": None,
            "virtio_gpu_gl_pci_available": False,
            "virtio_gpu_pci_virgl_prop": False,
            "egl_headless_available": False,
            "virglrenderer_lib_present": False,
        }

    rc_dev, out_dev = _run_text([qemu, "-device", "help"])
    has_gpu_pci = rc_dev == 0 and "virtio-gpu-pci" in out_dev
    has_gl_pci = rc_dev == 0 and "virtio-gpu-gl-pci" in out_dev

    rc_prop, out_prop = _run_text([qemu, "-device", "virtio-gpu-pci,help"])
    has_virgl_prop = rc_prop == 0 and "virgl" in out_prop

    rc_gl_prop, out_gl_prop = _run_text([qemu, "-device", "virtio-gpu-gl-pci,help"])
    gl_props = [ln.strip() for ln in out_gl_prop.splitlines() if "virgl" in ln or "blob" in ln] if rc_gl_prop == 0 else []

    rc_disp, out_disp = _run_text([qemu, "-display", "help"])
    has_egl_headless = rc_disp == 0 and "egl-headless" in out_disp

    lib_paths = [
        pathlib.Path(p)
        for p in (
            "/usr/lib/x86_64-linux-gnu/libvirglrenderer.so.1",
            "/usr/lib/libvirglrenderer.so.1",
            "/usr/lib/x86_64-linux-gnu/libvirglrenderer.so",
        )
    ]
    virgl_lib = next((str(p) for p in lib_paths if p.exists()), None)

    gl_dryrun_ok = False
    gl_dryrun_detail = "skipped_missing_gl_or_egl"
    if has_gl_pci and has_egl_headless:
        rc_q, out_q = _run_text(
            [
                qemu,
                "-nodefaults",
                "-nographic",
                "-M",
                "q35",
                "-m",
                "64M",
                "-display",
                "egl-headless",
                "-device",
                "virtio-gpu-gl-pci",
                "-S",
                "-qmp",
                "stdio",
            ],
            timeout_s=5.0,
        )
        # Expect QEMU either prints QMP greeting before closing stdin or reports host EGL unavailable.
        gl_dryrun_ok = '"QMP"' in out_q and rc_q == 0
        gl_dryrun_detail = "qmp_instantiated" if gl_dryrun_ok else (out_q[:240] or f"rc={rc_q}")

    status = (
        "HOST_VIRGL_QEMU_GL_AVAILABLE"
        if (has_gl_pci and has_egl_headless and virgl_lib and gl_dryrun_ok)
        else "HOST_VIRGL_UNAVAILABLE"
    )
    return {
        "status": status,
        "qemu_binary": qemu,
        "virtio_gpu_pci_available": has_gpu_pci,
        "virtio_gpu_gl_pci_available": has_gl_pci,
        "virtio_gpu_pci_virgl_prop": has_virgl_prop,
        "virtio_gpu_gl_props": gl_props[:8],
        "egl_headless_available": has_egl_headless,
        "virglrenderer_lib_present": virgl_lib is not None,
        "virglrenderer_lib_path": virgl_lib,
        "qmp_gl_instantiation_ok": gl_dryrun_ok,
        "qmp_gl_detail": gl_dryrun_detail,
        "notes": "Read-only QEMU probe; no host /dev/* mutation or GPU unbind performed.",
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Probe host QEMU VirtIO-GPU 3D/VirGL readiness")
    parser.add_argument("--json-out", type=pathlib.Path, default=DEFAULT_OUT)
    args = parser.parse_args()

    res = probe_virtio_gpu_3d()
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")
    print(
        f"[VIRTIO-GPU-3D-PROBE] status={res['status']} "
        f"gl_pci={res['virtio_gpu_gl_pci_available']} "
        f"virgl_prop={res['virtio_gpu_pci_virgl_prop']} "
        f"egl_headless={res['egl_headless_available']} "
        f"virgl_lib={res['virglrenderer_lib_present']} "
        f"qmp_gl_ok={res.get('qmp_gl_instantiation_ok', False)}"
    )


if __name__ == "__main__":
    main()
