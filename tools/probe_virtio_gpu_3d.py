#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Read-only host & QEMU VirtIO-GPU 3D (`virgl` / `virtio-gpu-gl-pci`) capability probe (`Milestone 5`).

Inspects QEMU device help/properties and host virgl libraries WITHOUT opening `/dev/*`,
without writing sysfs, and without modifying host GPU bindings.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import queue
import shutil
import subprocess
import sys
import threading
import time
from typing import Any, Callable

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / "tools" / "compat_bench" / "results_virtio_gpu_3d_probe.json"
DEFAULT_LIVE_OUT = ROOT / "tools" / "compat_bench" / "results_virtio_gpu_3d_live.json"
ISOLATED_MOD_DIR = ROOT / ".toolchains" / "virgl-env" / "qemu-modules"
ISOLATED_LIB_DIR = ROOT / ".toolchains" / "virgl-env" / "lib"
FALLBACK_MOD_DIR = ROOT / ".toolchains" / "qemu-modules"


def _build_probe_env() -> tuple[dict[str, str], bool]:
    env = os.environ.copy()
    isolated_used = False
    if ISOLATED_MOD_DIR.is_dir():
        env["QEMU_MODULE_DIR"] = str(ISOLATED_MOD_DIR)
        isolated_used = True
    elif FALLBACK_MOD_DIR.is_dir() and "QEMU_MODULE_DIR" not in env:
        env["QEMU_MODULE_DIR"] = str(FALLBACK_MOD_DIR)

    if ISOLATED_LIB_DIR.is_dir():
        prev_ld = env.get("LD_LIBRARY_PATH", "")
        env["LD_LIBRARY_PATH"] = f"{ISOLATED_LIB_DIR}:{prev_ld}" if prev_ld else str(ISOLATED_LIB_DIR)
        isolated_used = True
    mesa_egl = pathlib.Path("/usr/share/glvnd/egl_vendor.d/50_mesa.json")
    if mesa_egl.is_file():
        env.setdefault("__EGL_VENDOR_LIBRARY_FILENAMES", str(mesa_egl))
    env.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
    return env, isolated_used


def _run_text(
    cmd: list[str],
    env: dict[str, str] | None = None,
    timeout_s: float = 10.0,
    input_text: str | None = None,
) -> tuple[int, str]:
    try:
        proc = subprocess.run(
            cmd,
            env=env,
            input=input_text,
            stdin=subprocess.DEVNULL if input_text is None else None,
            capture_output=True,
            text=True,
            timeout=timeout_s,
            check=False,
        )
        return proc.returncode, (proc.stdout + "\n" + proc.stderr).strip()
    except (OSError, subprocess.TimeoutExpired) as exc:
        return -1, str(exc)


def probe_virtio_gpu_3d() -> dict[str, Any]:
    qemu = shutil.which("qemu-system-x86_64")
    probe_env, isolated_used = _build_probe_env()
    if not qemu:
        return {
            "status": "HOST_VIRGL_UNAVAILABLE",
            "reason": "qemu-system-x86_64 not found",
            "isolated_virgl_env_used": isolated_used,
            "qemu_binary": None,
            "virtio_gpu_gl_pci_available": False,
            "virtio_gpu_pci_virgl_prop": False,
            "egl_headless_available": False,
            "virglrenderer_lib_present": False,
        }

    rc_dev, out_dev = _run_text([qemu, "-device", "help"], env=probe_env)
    has_gpu_pci = rc_dev == 0 and "virtio-gpu-pci" in out_dev
    has_gl_pci = rc_dev == 0 and "virtio-gpu-gl-pci" in out_dev

    rc_prop, out_prop = _run_text([qemu, "-device", "virtio-gpu-pci,help"], env=probe_env)
    has_virgl_prop = rc_prop == 0 and "virgl" in out_prop

    rc_gl_prop, out_gl_prop = _run_text([qemu, "-device", "virtio-gpu-gl-pci,help"], env=probe_env)
    gl_props = [ln.strip() for ln in out_gl_prop.splitlines() if "virgl" in ln or "blob" in ln] if rc_gl_prop == 0 else []

    rc_disp, out_disp = _run_text([qemu, "-display", "help"], env=probe_env)
    has_egl_headless = rc_disp == 0 and "egl-headless" in out_disp

    lib_paths = [
        ISOLATED_LIB_DIR / "libvirglrenderer.so.1",
        *(
            pathlib.Path(p)
            for p in (
                "/usr/lib/x86_64-linux-gnu/libvirglrenderer.so.1",
                "/usr/lib/libvirglrenderer.so.1",
                "/usr/lib/x86_64-linux-gnu/libvirglrenderer.so",
            )
        ),
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
            env=probe_env,
            timeout_s=20.0,
            input_text='{"execute": "qmp_capabilities"}\n{"execute": "quit"}\n',
        )
        # Expect QEMU either prints QMP greeting before clean quit or reports host EGL unavailable.
        gl_dryrun_ok = '"QMP"' in out_q and rc_q == 0
        gl_dryrun_detail = "qmp_instantiated" if gl_dryrun_ok else (out_q[:240] or f"rc={rc_q}")

    status = (
        "HOST_VIRGL_QEMU_GL_AVAILABLE"
        if (has_gl_pci and has_egl_headless and virgl_lib and gl_dryrun_ok)
        else "HOST_VIRGL_UNAVAILABLE"
    )
    return {
        "status": status,
        "isolated_virgl_env_used": isolated_used,
        "qemu_binary": qemu,
        "qemu_module_dir": probe_env.get("QEMU_MODULE_DIR"),
        "ld_library_path_prefix": probe_env.get("LD_LIBRARY_PATH"),
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


def _parse_kv(line: str) -> dict[str, int]:
    out: dict[str, int] = {}
    for tok in line.split():
        if "=" not in tok:
            continue
        k, v = tok.split("=", 1)
        try:
            out[k] = int(v, 16) if v.startswith("0x") else int(v)
        except ValueError:
            pass
    return out


def run_virgl_3d_live_test(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
    json_out: pathlib.Path = DEFAULT_LIVE_OUT,
) -> dict[str, Any]:
    """Boot DRY OS under QEMU `virtio-gpu-gl-pci` + `egl-headless` (software Mesa EGL)
    and verify 3D capability discovery, Virgl context/resource/submit/readback, negative
    error rejection, and zero leaked DMA/physical frames."""
    from qemu_p1_validators import EXIT_CODE_SUCCESS
    from qemu_p2_validators import _DesktopQmpClient

    probe = probe_virtio_gpu_3d()
    probe_env, isolated_used = _build_probe_env()
    if probe.get("status") != "HOST_VIRGL_QEMU_GL_AVAILABLE":
        res = {
            "status": "BLOCKED",
            "reason": probe.get("qmp_gl_detail", "HOST_VIRGL_UNAVAILABLE"),
            "isolated_virgl_env_used": isolated_used,
            "probe": probe,
        }
        json_out.parent.mkdir(parents=True, exist_ok=True)
        json_out.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")
        return res

    esp_gl = build_dir / "esp_virgl_3d.img"
    write_mbr_esp_image(
        esp_gl,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    vars_copy = build_dir / "OVMF_VARS_virgl_3d.4m.fd"
    shutil.copyfile(ovmf_vars, vars_copy)
    qmp_sock = pathlib.Path(f"/tmp/dryos_virgl3d_qmp_{os.getpid()}.sock")
    qmp_sock.unlink(missing_ok=True)

    accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
    cpu_arg = "host" if accel == "kvm" else "qemu64"
    cmd = [
        "qemu-system-x86_64",
        "-machine",
        f"q35,accel={accel}",
        "-cpu",
        cpu_arg,
        "-smp",
        "2",
        "-m",
        "384M",
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={ovmf_code}",
        "-drive",
        f"if=pflash,format=raw,file={vars_copy}",
        "-drive",
        f"format=raw,file={esp_gl}",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial",
        "stdio",
        "-display",
        "egl-headless",
        "-vga",
        "std",
        "-device",
        "virtio-gpu-gl-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800",
        "-qmp",
        f"unix:{qmp_sock},server=on,wait=off",
        "-no-reboot",
    ]

    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        env=probe_env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
    )
    line_q: queue.Queue[str | None] = queue.Queue()
    serial_lines: list[str] = []

    def _reader() -> None:
        assert proc.stdout is not None
        for ln in proc.stdout:
            line_q.put(ln)
        line_q.put(None)

    rd = threading.Thread(target=_reader, daemon=True)
    rd.start()

    def _wait_marker(marker: str, timeout_s: float = 35.0) -> str:
        for existing in serial_lines:
            if marker in existing:
                return existing
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
        tail = "".join(serial_lines)[-8192:]
        raise RuntimeError(f"[virgl_3d_live] Timed out waiting for {marker!r}\n--- TAIL ---\n{tail}")

    qmp: _DesktopQmpClient | None = None
    try:
        probe_line = _wait_marker("[DRY-GPU] 3D_PROBE ", timeout_s=90.0).strip()
        smoke_line = _wait_marker("[DRY-GPU] 3D_SMOKE ", timeout_s=20.0).strip()
        _wait_marker("[DRY-GPU] LIVE_SESSION_ACTIVE ", timeout_s=20.0)
        _wait_marker("[DRY-SHELL] Interactive shell ready in CPL3 Userspace", timeout_s=20.0)

        qmp = _DesktopQmpClient(qmp_sock)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _wait_marker("[DRY-P2-SESSION] PASS:", timeout_s=20.0)
        _wait_marker("[DRY-BOOT-OK]", timeout_s=10.0)
        qmp.close()
        qmp = None
        rc = proc.wait(timeout=15.0)
    finally:
        if qmp is not None:
            try:
                qmp.close()
            except Exception:
                pass
        if proc.poll() is None:
            proc.kill()
        rd.join(timeout=2.0)
        qmp_sock.unlink(missing_ok=True)

    while True:
        try:
            it = line_q.get_nowait()
        except queue.Empty:
            break
        if it is not None:
            serial_lines.append(it)

    serial_text = "".join(serial_lines)
    if rc != EXIT_CODE_SUCCESS:
        raise RuntimeError(f"[virgl_3d_live] Expected QEMU exit {EXIT_CODE_SUCCESS}, got {rc}\n{serial_text[-8192:]}")

    pkv = _parse_kv(probe_line)
    skv = _parse_kv(smoke_line)
    sess_lines = [ln.strip() for ln in serial_text.splitlines() if "[DRY-P2-SESSION] PASS:" in ln]
    sess_kv = _parse_kv(sess_lines[-1]) if sess_lines else {}

    elapsed = round(time.monotonic() - t0, 3)
    res = {
        "status": "VERIFIED_QEMU",
        "renderer": "QEMU virtio-gpu-gl-pci + egl-headless (Mesa software llvmpipe)",
        "isolated_virgl_env_used": isolated_used,
        "probe_line": probe_line,
        "smoke_line": smoke_line,
        "virgl_supported": pkv.get("virgl") == 1,
        "num_capsets": pkv.get("capsets", 0),
        "first_capset_id": pkv.get("id", 0),
        "unit_ok": skv.get("unit_ok") == 1,
        "executed": skv.get("exec") == 1,
        "ctx_ok": skv.get("ctx") == 1,
        "res_ok": skv.get("res") == 1,
        "submit_fenced_ok": skv.get("submit") == 1,
        "xfer_resp_kind": hex(skv.get("xfer", 0)),
        "readback_pixel": hex(skv.get("px", 0)),
        "pixel_match": skv.get("match") == 1,
        "neg_err_kind": hex(skv.get("neg_err", 0)),
        "cleaned_up": skv.get("clean") == 1,
        "leaked_frames": sess_kv.get("leaked_frames", -1),
        "wall_time_sec": elapsed,
    }
    json_out.parent.mkdir(parents=True, exist_ok=True)
    json_out.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")
    return res


def main() -> None:
    parser = argparse.ArgumentParser(description="Probe host QEMU VirtIO-GPU 3D/VirGL readiness")
    parser.add_argument("--json-out", type=pathlib.Path, default=DEFAULT_OUT)
    parser.add_argument("--live-qemu", action="store_true", help="Boot DRY OS with virtio-gpu-gl-pci and verify 3D smoke path")
    parser.add_argument("--live-json-out", type=pathlib.Path, default=DEFAULT_LIVE_OUT)
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
    if args.live_qemu:
        sys.path.insert(0, str(ROOT / "tools"))
        from qemu_boot_test import BUILD_DIR, OVMF_CODE, OVMF_VARS, build_kernel, write_mbr_esp_image

        kernel_elf = build_kernel()
        live = run_virgl_3d_live_test(
            kernel_elf, BUILD_DIR, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, json_out=args.live_json_out
        )
        print(
            f"[VIRTIO-GPU-3D-LIVE] status={live['status']} "
            f"capsets={live.get('num_capsets')} id={live.get('first_capset_id')} "
            f"ctx={live.get('ctx_ok')} res={live.get('res_ok')} submit={live.get('submit_fenced_ok')} "
            f"xfer={live.get('xfer_resp_kind')} px={live.get('readback_pixel')} match={live.get('pixel_match')} "
            f"neg_err={live.get('neg_err_kind')} clean={live.get('cleaned_up')} "
            f"leaked_frames={live.get('leaked_frames')} ({live.get('wall_time_sec')}s)"
        )
        if not (
            live.get("status") == "VERIFIED_QEMU"
            and live.get("unit_ok")
            and live.get("executed")
            and live.get("ctx_ok")
            and live.get("res_ok")
            and live.get("submit_fenced_ok")
            and live.get("pixel_match")
            and live.get("neg_err_kind", "0x0") != "0x0"
            and live.get("cleaned_up")
            and live.get("leaked_frames") == 0
        ):
            raise SystemExit(1)


if __name__ == "__main__":
    main()
