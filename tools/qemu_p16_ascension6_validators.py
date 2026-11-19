#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Ascension 6.0 End-to-End Architecture & Verification Validators (`--p16-only`)."""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys
import time
from typing import Any, Callable

from bootstrap_qemu_gpu_modules import ensure_qemu_virtio_gpu_modules
from qemu_p2_validators import _DesktopSessionHarness

EXIT_CODE_SUCCESS = (0x10 << 1) | 1  # 33
EXIT_CODE_PANIC = (0x11 << 1) | 1    # 35


def _assert_tokens(name: str, text: str, tokens: list[str]) -> None:
    for tok in tokens:
        if tok not in text:
            tail = text[-8192:] if len(text) > 8192 else text
            raise RuntimeError(f"[{name}] Missing serial token {tok!r}\n--- TAIL ---\n{tail}")


def _run_p16a_iommu_fail_closed_and_ivrs(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
    run_qemu: Callable[..., tuple[int, str, float]],
) -> dict[str, Any]:
    t0 = time.monotonic()
    esp_hw = build_dir / "esp_p16a_no_iommu_hw.img"
    write_mbr_esp_image(esp_hw, kernel_elf, "console=com1 hw_test=1")
    rc_hw, out_hw, dt_hw = run_qemu(
        "p16a_no_iommu_fail_closed",
        esp_hw,
        smp=2,
        timeout_sec=25,
        extra_args=[
            "-fw_cfg",
            "name=opt/ovmf/X-PciMmio64Mb,string=4096",
            "-device",
            "edu,dma_mask=0xffffffffffffffff",
            "-object",
            "memory-backend-ram,size=256M,id=iv0",
            "-device",
            "ivshmem-plain,memdev=iv0",
        ],
    )
    if rc_hw != EXIT_CODE_PANIC:
        raise RuntimeError(f"[p16a_no_iommu_fail_closed] Expected exit {EXIT_CODE_PANIC}, got {rc_hw}")
    _assert_tokens(
        "p16a_no_iommu_fail_closed",
        out_hw,
        ["[DRY-IOMMU] FAIL_CLOSED", "[DRY-HW-FAIL] code=106", "[DRY-FB]"],
    )

    esp_ivrs = build_dir / "esp_p16a_amd_ivrs.img"
    write_mbr_esp_image(esp_ivrs, kernel_elf, "console=com1")
    rc_iv, out_iv, dt_iv = run_qemu(
        "p16a_amd_ivrs_fail_closed",
        esp_ivrs,
        smp=2,
        timeout_sec=25,
        extra_args=["-device", "amd-iommu,intremap=off"],
    )
    if rc_iv != EXIT_CODE_SUCCESS:
        raise RuntimeError(f"[p16a_amd_ivrs_fail_closed] Expected exit {EXIT_CODE_SUCCESS}, got {rc_iv}")
    _assert_tokens(
        "p16a_amd_ivrs_fail_closed",
        out_iv,
        ["[DRY-IOMMU] IVRS detected", "[DRY-IOMMU] FAIL_CLOSED", "[DRY-BOOT-OK]"],
    )
    elapsed = round(time.monotonic() - t0, 3)
    print(f"[PASS] p16a_iommu_fail_closed_and_ivrs: no_iommu_rc={rc_hw} code=106 ivrs=fail_closed ({elapsed:.2f}s).")
    return {
        "no_iommu_rc": rc_hw,
        "no_iommu_wall_s": round(dt_hw, 3),
        "ivrs_rc": rc_iv,
        "ivrs_wall_s": round(dt_iv, 3),
        "wall_time_sec": elapsed,
    }


def _run_p16b_gpu_svc_4backends_handover_fences(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    os.environ["QEMU_MODULE_DIR"] = str(ensure_qemu_virtio_gpu_modules())
    esp_gpu = build_dir / "esp_p16b_gpu_svc.img"
    write_mbr_esp_image(
        esp_gpu,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        "p16b_gpu_svc_4backends_handover_fences",
        esp_gpu,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=["-device", "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800"],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1280 h=800", wait_s=12.0)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        _assert_tokens(
            "p16b_gpu_svc_4backends_handover_fences",
            serial_out,
            [
                "[DRY-GPU-SVC] PASS: backends=4 handover=0x7f synth=2 rollbacks=3 broker_data_path=0",
                "[DRY-WM-GPU-SVC] state=KMS_ACTIVE",
                "broker_data_path=0",
                "[DRY-GPU] 3D_PROBE virgl=",
                "[DRY-FW-STORE] PASS: entries=3 verified=2 tamper_rej=1 enoent_rej=1",
                "[DRY-LIFECYCLE] PASS: steps=15 mask=0x7fff illegal_rej=1 respawns=1",
            ],
        )
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p16b] leaked_frames != 0: {skv!r}")
        elapsed = round(time.monotonic() - t0, 3)
        print(
            f"[PASS] p16b_gpu_svc_4backends_handover_fences: backends=4 handover=0x7f "
            f"fw_store=1 lifecycle=15 leaked_frames=0 ({elapsed:.2f}s)."
        )
        return {
            "backends_verified": 4,
            "handover_mask": "0x7f",
            "broker_in_data_path": False,
            "leaked_frames": skv["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()


def _run_p16c_lx_neutrality_and_onboarding(root_dir: pathlib.Path) -> dict[str, Any]:
    t0 = time.monotonic()
    libdry = root_dir / "drivers" / "libdry-lx-compat"
    bochs_hits = [
        str(p.relative_to(root_dir))
        for p in libdry.rglob("*")
        if p.is_file() and "bochs" in p.read_text(encoding="utf-8", errors="ignore").lower()
    ]
    if bochs_hits:
        raise RuntimeError(f"[p16c] Vendor-neutral libdry-lx-compat still mentions bochs: {bochs_hits!r}")

    subprocess.check_call([sys.executable, "-m", "unittest", str(root_dir / "tools" / "lx_compat" / "test_lx_driver_manager.py")])
    subprocess.check_call([sys.executable, str(root_dir / "tools" / "lx_compat" / "validate_target_gpus.py")], stdout=subprocess.DEVNULL)
    subprocess.check_call([sys.executable, str(root_dir / "tools" / "gpu_fw_validators.py"), "--self-test"])

    mgr = root_dir / "tools" / "lx_compat" / "lx_driver_manager.py"
    onboard_summary: dict[str, Any] = {}
    for drv in ("drm_bochs", "virtio_gpu_native", "synthetic_gpu"):
        data = json.loads(subprocess.check_output([sys.executable, str(mgr), "onboard", "--driver", drv], text=True))
        if not data.get("fully_onboarded") or data.get("stages_passed") != 11:
            raise RuntimeError(f"[p16c] Expected 11/11 onboarding for {drv}: {data!r}")
        onboard_summary[drv] = f"{data['stages_passed']}/{data['stages_total']}"
    for prof in ("intel_i915_tgl", "amdgpu_generic", "nvidia_open_ga107"):
        data = json.loads(subprocess.check_output([sys.executable, str(mgr), "onboard", "--driver", prof], text=True))
        if data.get("fully_onboarded") or data.get("hardware_present_assumed"):
            raise RuntimeError(f"[p16c] Profile-only target illegally marked active: {prof} -> {data!r}")
        onboard_summary[prof] = f"{data['stages_passed']}/{data['stages_total']} ({data['catalog_status']})"

    elapsed = round(time.monotonic() - t0, 3)
    print(f"[PASS] p16c_lx_neutrality_and_onboarding: bochs_in_libdry=0 profiles=6 fw_validators=ok ({elapsed:.2f}s).")
    return {"bochs_hits_in_libdry": 0, "onboarding": onboard_summary, "wall_time_sec": elapsed}


def _run_p16d_microvm_proto_and_virtio3d_probe(root_dir: pathlib.Path) -> dict[str, Any]:
    t0 = time.monotonic()
    proto_script = root_dir / "tools" / "microvm_gpu_proto" / "proto_bench.py"
    proto_json = root_dir / "tools" / "compat_bench" / "results_microvm_gpu_proto.json"
    subprocess.check_call([sys.executable, str(proto_script), "--iterations", "60", "--json-out", str(proto_json)])
    proto_data = json.loads(proto_json.read_text(encoding="utf-8"))
    if (
        not proto_data.get("passed")
        or proto_data.get("transport_mode") != "SHARED_MEMORY_IPC_COPIED"
        or proto_data.get("zero_copy_dmabuf") is not False
    ):
        raise RuntimeError(f"[p16d] Unexpected MicroVM prototype result: {proto_data!r}")

    probe_script = root_dir / "tools" / "probe_virtio_gpu_3d.py"
    probe_json = root_dir / "tools" / "compat_bench" / "results_virtio_gpu_3d_probe.json"
    subprocess.check_call([sys.executable, str(probe_script), "--json-out", str(probe_json)])
    probe_data = json.loads(probe_json.read_text(encoding="utf-8"))
    if probe_data.get("status") not in ("HOST_VIRGL_UNAVAILABLE", "HOST_VIRGL_QEMU_GL_AVAILABLE"):
        raise RuntimeError(f"[p16d] Invalid VirtIO-GPU 3D probe status: {probe_data!r}")

    elapsed = round(time.monotonic() - t0, 3)
    print(
        f"[PASS] p16d_microvm_proto_and_virtio3d_probe: mode={proto_data['transport_mode']} "
        f"virgl_status={probe_data['status']} ({elapsed:.2f}s)."
    )
    return {
        "microvm_transport_mode": proto_data["transport_mode"],
        "microvm_zero_copy_dmabuf": proto_data["zero_copy_dmabuf"],
        "microvm_tile_p50_us": proto_data["benchmarks"]["dirty_tile_256x256_xrgb"]["p50_us"],
        "virtio_3d_host_status": probe_data["status"],
        "wall_time_sec": elapsed,
    }


def run_p16_ascension6_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
    run_qemu: Callable[..., tuple[int, str, float]],
) -> int:
    res_a = _run_p16a_iommu_fail_closed_and_ivrs(kernel_elf, build_dir, write_mbr_esp_image, run_qemu)
    res_b = _run_p16b_gpu_svc_4backends_handover_fences(
        kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image
    )
    res_c = _run_p16c_lx_neutrality_and_onboarding(root_dir)
    res_d = _run_p16d_microvm_proto_and_virtio3d_probe(root_dir)

    out_path = root_dir / "tools" / "compat_bench" / "results_p16_ascension6.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    summary = {
        "milestone": "Ascension-6.0",
        "p16a": res_a,
        "p16b": res_b,
        "p16c": res_c,
        "p16d": res_d,
    }
    out_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return 4
