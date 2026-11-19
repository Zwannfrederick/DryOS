#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Ascension 7.0 Reliability, Daily-Use Hardening & Virgl 3D Validators (`--p17-only`)."""

from __future__ import annotations

import json
import os
import pathlib
import time
from typing import Any, Callable

from bootstrap_qemu_gpu_modules import ensure_qemu_virtio_gpu_modules
from probe_virtio_gpu_3d import run_virgl_3d_live_test
from qemu_log_validators import _parse_kv_ints
from qemu_p2_validators import _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args


def _parse_kv_any(line: str) -> dict[str, int]:
    out = _parse_kv_ints(line)
    for tok in line.split():
        if "=" not in tok:
            continue
        k, v = tok.split("=", 1)
        if v.startswith("0x"):
            try:
                out[k] = int(v, 16)
            except ValueError:
                pass
    return out


def _find_prefixed_line(log: str, prefix: str) -> str | None:
    for ln in log.splitlines():
        s = ln.strip()
        if s.startswith(prefix):
            return s
    return None


def _run_p17a_multi_crash_gpu_and_revoke(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    os.environ["QEMU_MODULE_DIR"] = str(ensure_qemu_virtio_gpu_modules())
    disk_path = build_dir / "disk_p17a.img"
    _create_raw_disk(disk_path, size_mib=4)
    esp_img = build_dir / "esp_p17a_multi_crash.img"
    write_mbr_esp_image(
        esp_img,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        "p17a_multi_crash_gpu_and_revoke",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path)
        + ["-device", "virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800"],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-INTEL-PREP] PASS: dev=8086:9a49", wait_s=12.0)
        h.wait_for("[DRY-GPU] 3D_SMOKE unit_ok=1", wait_s=12.0)
        h.wait_for("[DRY-WM] GPU_SCANOUT_BOUND back_2m=2", wait_s=12.0)
        h.wait_for("[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=1280 h=800", wait_s=12.0)

        for expected_gen in (2, 3):
            idx = len(h.serial_lines)
            qmp.type_ascii("gpufault")
            qmp.send_qcodes(["ret"])
            h.wait_for("[DRY-WM] GPU_FALLBACK_TO_GOP", start_idx=idx, wait_s=15.0)
            h.wait_for(f"[DRY-GPU] LIVE_RECOVERY_OK gen={expected_gen} vec=6", start_idx=idx, wait_s=15.0)
            h.wait_for(f"[DRY-SHELL] CMD=gpufault gen={expected_gen} vec=6", start_idx=idx, wait_s=15.0)

        idx = len(h.serial_lines)
        qmp.type_ascii("sec-audit")
        qmp.send_qcodes(["ret"])
        h.wait_for(
            "[DRY-SHELL] CMD=sec-audit kill_eperm=true unauth_perm=false bad_ipc_inval=true "
            "vfs_rofs=true vfs_eperm=true pwr_eperm=true",
            start_idx=idx,
            wait_s=12.0,
        )

        idx = len(h.serial_lines)
        qmp.type_ascii("corrupt-sb0-test")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=corrupt-sb0-test ok=true", start_idx=idx, wait_s=12.0)

        idx = len(h.serial_lines)
        qmp.type_ascii("write /persist/p17-multi.txt p17-gen3ok")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=write path=/persist/p17-multi.txt bytes=10", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("sync")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=sync tx_seq=", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("cat /persist/p17-multi.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=cat path=/persist/p17-multi.txt bytes=10 text="p17-gen3ok"', start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        hkv = _parse_kv_any(hlt_lns[0]) if hlt_lns else {}
        quiesce = hkv.get("pci_quiesce", 0)
        if skv.get("leaked_frames", -1) != 0 or quiesce < 2:
            raise RuntimeError(f"[p17a] quiesce={quiesce} skv={skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] p17a_multi_crash_gpu_and_revoke: gen=1->2->3 quiesce={quiesce} sec_audit=1 leaked_frames=0 ({elapsed:.2f}s).")
        return {
            "gpu_generations": 3,
            "pci_quiesce": quiesce,
            "sec_audit_ok": True,
            "leaked_frames": skv["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)


def _run_p17b_missing_storage_degradation(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    esp_img = build_dir / "esp_p17b_no_storage.img"
    write_mbr_esp_image(
        esp_img,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        "p17b_missing_storage_degradation",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="256M",
        extra_qemu_args=None,
    )
    try:
        qmp = h.boot_to_desktop_ready()
        idx = len(h.serial_lines)
        qmp.type_ascii("mounts")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=mounts persist=0 backend=none ro=0", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("uname")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=uname sys=DryOS arch=x86_64 smp=2", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if "[DRY-PANIC]" in serial_out or skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p17b] Unexpected panic or frame leak: {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] p17b_missing_storage_degradation: persist=0 volatile_ok=1 leaked_frames=0 ({elapsed:.2f}s).")
        return {
            "persist_mounted": False,
            "volatile_desktop_ok": True,
            "leaked_frames": skv["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()


def _run_p17c_smp4_vtd_telemetry(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
    run_qemu: Callable[..., tuple[int, str, float]],
) -> dict[str, Any]:
    t0 = time.monotonic()
    esp_hw = build_dir / "esp_p17c_smp4_hw.img"
    write_mbr_esp_image(esp_hw, kernel_elf, "console=com1 hw_test=1")
    rc, log, _ = run_qemu(
        "p17c_smp4_vtd_isolation",
        esp_hw,
        smp=4,
        timeout_sec=35,
        extra_args=[
            "-fw_cfg",
            "name=opt/ovmf/X-PciMmio64Mb,string=4096",
            "-device",
            "intel-iommu,intremap=off,caching-mode=on",
            "-device",
            "edu,dma_mask=0xffffffffffffffff",
            "-object",
            "memory-backend-ram,size=256M,id=iv0",
            "-device",
            "ivshmem-plain,memdev=iv0",
        ],
    )
    if rc != 33:
        raise RuntimeError(f"[p17c] Expected exit 33, got {rc}\n{log[-3000:]}")

    for prefix in (
        "[DRY-IOMMU] VTD_ACTIVE ",
        "[DRY-IOMMU] UNIT idx=0 ",
        "[DRY-PCI-TOPO] ",
        "[DRY-HW-PASS] ",
        "[DRY-HW-A6-PASS] ",
        "[DRY-HW-KERNEL] PASS: ",
    ):
        if _find_prefixed_line(log, prefix) is None:
            raise RuntimeError(f"[p17c] Missing {prefix!r} in log\n{log[-3000:]}")

    kline = _find_prefixed_line(log, "[DRY-HW-KERNEL] PASS: ") or ""
    kkv = _parse_kv_any(kline)
    if kkv.get("vtd_enabled") != 1 or kkv.get("vtd_faults", 0) < 2 or kkv.get("leaked_frames") != 0:
        raise RuntimeError(f"[p17c] Kernel telemetry check failed: {kline!r}")

    elapsed = round(time.monotonic() - t0, 3)
    print(
        f"[PASS] p17c_smp4_vtd_isolation: smp=4 vtd_enabled=1 vtd_faults={kkv.get('vtd_faults')} "
        f"quiesces={kkv.get('quiesces')} leaked_frames=0 ({elapsed:.2f}s)."
    )
    return {
        "smp": 4,
        "vtd_enabled": True,
        "vtd_faults": kkv.get("vtd_faults", 0),
        "quiesces": kkv.get("quiesces", 0),
        "leaked_frames": kkv["leaked_frames"],
        "wall_time_sec": elapsed,
    }


def _run_p17d_virtio_3d_virgl_gate(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    live_json = root_dir / "tools" / "compat_bench" / "results_virtio_gpu_3d_live.json"
    res = run_virgl_3d_live_test(
        kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image, json_out=live_json
    )
    if res.get("status") != "VERIFIED_QEMU" or not (
        res.get("unit_ok")
        and res.get("executed")
        and res.get("ctx_ok")
        and res.get("res_ok")
        and res.get("submit_fenced_ok")
        and res.get("pixel_match")
        and res.get("neg_err_kind", "0x0") != "0x0"
        and res.get("cleaned_up")
        and res.get("leaked_frames") == 0
    ):
        raise RuntimeError(f"[p17d] Virgl 3D gate failed: {res!r}")

    print(
        f"[PASS] p17d_virtio_3d_virgl_gate: status={res['status']} capsets={res['num_capsets']} "
        f"px={res['readback_pixel']} neg_err={res['neg_err_kind']} leaked_frames=0 ({res['wall_time_sec']:.2f}s)."
    )
    return res


def run_p17_ascension7_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
    run_qemu: Callable[..., tuple[int, str, float]],
) -> int:
    res_a = _run_p17a_multi_crash_gpu_and_revoke(kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)
    res_b = _run_p17b_missing_storage_degradation(kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)
    res_c = _run_p17c_smp4_vtd_telemetry(kernel_elf, build_dir, write_mbr_esp_image, run_qemu)
    res_d = _run_p17d_virtio_3d_virgl_gate(kernel_elf, build_dir, root_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)

    out_path = root_dir / "tools" / "compat_bench" / "results_p17_ascension7.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(
            {
                "milestone": "Ascension-7.0",
                "p17a": res_a,
                "p17b": res_b,
                "p17c": res_c,
                "p17d": res_d,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return 4
