#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Ascension 8.0 M4 Mandatory NVIDIA Driver Island & GPU Compatibility Matrix (`--p18-only`)."""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time
from typing import Any

from retrieve_dryos_logs import find_gpt_logs_partition, retrieve_and_verify

NV_EXP_DATA_FILES = [
    "nvidia_island_status.json",
    "nvidia_prerequisites.json",
    "nvidia_pci_inventory.json",
    "nvidia_smi_output.txt",
    "nvidia_smi_list.txt",
    "nvidia_smi_query.txt",
    "nvidia_ipc_audit.json",
]


def _verify_nvidia_exp_partition(img_path: pathlib.Path, sess_id: str) -> dict[str, Any]:
    start_lba, secs, _ = find_gpt_logs_partition(img_path)
    with tempfile.TemporaryDirectory() as td:
        tmp = pathlib.Path(td)
        p3 = tmp / "p3_nv.fat32"
        with img_path.open("rb") as src, p3.open("wb") as dst:
            src.seek(start_lba * 512)
            rem = secs * 512
            while rem > 0:
                chunk = src.read(min(rem, 1 << 20))
                if not chunk:
                    break
                dst.write(chunk)
                rem -= len(chunk)

        subprocess.check_call(["fsck.fat", "-n", str(p3)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        ext = tmp / "extracted"
        ext.mkdir()
        env = {**os.environ, "MTOOLS_SKIP_CHECK": "1"}
        subprocess.check_call(
            ["mcopy", "-s", "-i", str(p3), f"::/hardware-tests/{sess_id}/nvidia_exp", str(ext)],
            env=env,
        )
        nv_dir = ext / "nvidia_exp"
        man_path = nv_dir / "manifest.sha256"
        if not man_path.is_file():
            raise RuntimeError(f"Missing {sess_id}/nvidia_exp/manifest.sha256")
        recorded: dict[str, str] = {}
        for line in man_path.read_text(encoding="utf-8").splitlines():
            parts = line.strip().split()
            if len(parts) == 2:
                recorded[parts[1]] = parts[0]
        if set(recorded.keys()) != set(NV_EXP_DATA_FILES):
            raise RuntimeError(f"nvidia_exp/manifest.sha256 mismatch: {recorded.keys()}")
        for fname in NV_EXP_DATA_FILES:
            fpath = nv_dir / fname
            if not fpath.is_file():
                raise RuntimeError(f"Missing {sess_id}/nvidia_exp/{fname}")
            got = hashlib.sha256(fpath.read_bytes()).hexdigest()
            if got != recorded[fname]:
                raise RuntimeError(f"SHA-256 mismatch for {fname}: {got} != {recorded[fname]}")

        st = json.loads((nv_dir / "nvidia_island_status.json").read_text(encoding="utf-8"))
        pr = json.loads((nv_dir / "nvidia_prerequisites.json").read_text(encoding="utf-8"))
        smi = (nv_dir / "nvidia_smi_output.txt").read_text(encoding="utf-8")
        if st.get("level") != 1 or st.get("status") != "BLOCKED" or st.get("hardware_verified") is not False:
            raise RuntimeError(f"Dishonest nvidia_island_status.json: {st!r}")
        if st.get("nvml_telemetry") != "N/A (BLOCKED)":
            raise RuntimeError(f"Fabricated NVML telemetry flag: {st!r}")
        if pr.get("total") != 14 or len(pr.get("prerequisites", [])) != 14:
            raise RuntimeError(f"Expected 14 prerequisites in JSON, got {pr!r}")
        if "BLOCKED" not in smi or "Temp: N/A" not in smi:
            raise RuntimeError(f"nvidia_smi_output.txt missing honest BLOCKED/N/A fields:\n{smi}")
        return {"status": st, "prereqs": pr}


def _nvme_canary_args(nvme_img: pathlib.Path, serial: str) -> list[str]:
    return [
        "-drive", f"if=none,id=nvmedisk,format=raw,file={nvme_img}",
        "-device", f"nvme,serial={serial},drive=nvmedisk",
    ]


def _run_nv_safe_gating(build_dir: pathlib.Path, ovmf_code: pathlib.Path, ovmf_vars: pathlib.Path) -> None:
    from build_live_usb_image import build_hwval_usb_image
    from qemu_p18_hwval_validators import _HwvalSession

    img = build_dir / "p18nv_safe_gate.img"
    build_hwval_usb_image(img, force_build=False, default_entry_idx=0)
    sess = _HwvalSession("p18nv_safe_gate", img, ovmf_code, ovmf_vars, build_dir, iommu=True, usb_hid=True)
    try:
        sess.wait_for("[DRY-SHELL] Interactive shell ready")
        ln = sess.run_cmd("dry gpu status", "[DRY-SHELL] CMD=dry gpu status")
        if "level=1" not in ln or "spawned=0" not in ln or "status=BLOCKED" not in ln:
            raise RuntimeError(f"[p18nv_safe] Expected passive non-spawned Level 1 status: {ln!r}")
        _rc, _sl, full = sess.shutdown_clean()
        if "[DRY-NV-ISLAND] Ready" in full:
            raise RuntimeError("[p18nv_safe] dry-nv-island spawned unexpectedly in safe boot profile!")
    finally:
        sess.close()
        img.unlink(missing_ok=True)
    print("  [PASS] p18nv/safe_gating (dry-nv-island stays unspawned in safe mode)")


def _run_nv_exp_level1_e2e(build_dir: pathlib.Path, ovmf_code: pathlib.Path, ovmf_vars: pathlib.Path) -> dict[str, Any]:
    from build_live_usb_image import build_hwval_usb_image
    from qemu_p18_hwval_validators import _HwvalSession, _create_canary_disk, _sha256_file

    t0 = time.monotonic()
    nvme_img = build_dir / "p18f_canary_nvme.img"
    nvme_pre = _create_canary_disk(nvme_img, b"DRYOS_NV_EXP_")
    img = build_dir / "p18f_nvidia_exp.img"
    build_hwval_usb_image(img, force_build=False, default_entry_idx=4)
    sess = _HwvalSession(
        "p18f_nvidia_exp", img, ovmf_code, ovmf_vars, build_dir,
        iommu=True, usb_hid=True, extra_args=_nvme_canary_args(nvme_img, "DRYP18NV"),
    )
    try:
        sess.wait_for("[DRY-NV-ISLAND] Ready in CPL3 Userspace [0x0218]")
        sess.wait_for("[DRY-NV-ISLAND] AUDIT level=1 status=BLOCKED")
        sess.wait_for("[DRY-HWDIAG] PERSIST status=VERIFIED files=13 sha256_verified=1")
        sess.wait_for("[DRY-NV-EXP] EXPORT status=VERIFIED files=8")
        sess.wait_for("[DRY-SHELL] Interactive shell ready")

        for cmd in (
            "nvidia-smi",
            "nvidia-smi -L",
            "nvidia-smi -q",
            "dry gpu status",
            "dry gpu nvidia status",
            "dry gpu nvidia prereqs",
            "dry gpu nvidia export",
        ):
            ln = sess.run_cmd(cmd, f"[DRY-SHELL] CMD={cmd}")
            if "level=1 status=BLOCKED" not in ln or "spawned=1" not in ln:
                raise RuntimeError(f"[p18f] Unexpected output for {cmd!r}: {ln!r}")
            if cmd == "dry gpu nvidia export" and "export_ok=1 files=8" not in ln:
                raise RuntimeError(f"[p18f] Export command failed: {ln!r}")

        sess.shutdown_clean()
        if _sha256_file(nvme_img) != nvme_pre:
            raise RuntimeError("[p18f] Canary NVMe modified during nvidia_exp session!")
        bundle = retrieve_and_verify(img, None, build_dir / "p18f_retrieved")
        nv_audit = _verify_nvidia_exp_partition(img, bundle["session_id"])
    finally:
        sess.close()
        img.unlink(missing_ok=True)
        nvme_img.unlink(missing_ok=True)
        shutil.rmtree(build_dir / "p18f_retrieved", ignore_errors=True)
    elapsed = round(time.monotonic() - t0, 2)
    print(f"[PASS] p18f_nvidia_exp_level1_e2e: 7 cmds + 7 nvidia_exp files + fsck.fat + 14 prereqs verified ({elapsed}s).")
    return {"session_id": bundle["session_id"], "met": nv_audit["status"]["met_count"], "elapsed_s": elapsed}


def _run_nv_negative_faults(build_dir: pathlib.Path, ovmf_code: pathlib.Path, ovmf_vars: pathlib.Path) -> int:
    from build_live_usb_image import build_hwval_usb_image
    from qemu_p18_hwval_validators import _HwvalSession, _create_canary_disk, _sha256_file

    nvme_img = build_dir / "p18g_canary_nvme.img"
    nvme_pre = _create_canary_disk(nvme_img, b"DRYOS_NV_NEG_")
    cases = [
        ("nv1_no_gpu", "dry_test_nv_no_gpu=1", "nvidia_island_status.json", ("reason", "no_10de_gpu_level1_blocked"), None),
        ("nv2_bad_bar", "dry_test_nv_bad_bar=1", "nvidia_island_status.json", ("reason", "bad_pci_bar_injected_blocked"), None),
        ("nv3_vm_crash", "dry_test_nv_vm_crash=1", None, None, "restarts=1"),
        ("nv4_timeout", "dry_test_nv_timeout=1", None, None, "timeout_ok=1"),
        ("nv5_corrupt_ipc", "dry_test_nv_corrupt_ipc=1", None, None, "corrupt_rej=1"),
        ("nv6_unauth_cap", "dry_test_nv_unauth_cap=1", None, None, "unauth_rej=1"),
    ]
    for cname, karg, _jfile, kv_check, serial_tok in cases:
        img = build_dir / f"p18g_{cname}.img"
        build_hwval_usb_image(img, force_build=False, default_entry_idx=4, limine_extra=karg)
        sess = _HwvalSession(
            f"p18g_{cname}", img, ovmf_code, ovmf_vars, build_dir,
            iommu=True, usb_hid=True, extra_args=_nvme_canary_args(nvme_img, "DRYP18NG"),
        )
        try:
            aln = sess.wait_for("[DRY-NV-ISLAND] AUDIT level=1 status=BLOCKED")
            if serial_tok and serial_tok not in aln:
                raise RuntimeError(f"[{cname}] Expected {serial_tok!r} in {aln!r}")
            sess.wait_for("[DRY-NV-EXP] EXPORT status=VERIFIED files=8")
            sess.wait_for("[DRY-SHELL] Interactive shell ready")
            sess.shutdown_clean()
            nv_res = _verify_nvidia_exp_partition(img, "sess-0001")
            if kv_check:
                k, v = kv_check
                if nv_res["status"].get(k) != v:
                    raise RuntimeError(f"[{cname}] Expected {k}={v!r}, got {nv_res['status']!r}")
                if cname == "nv2_bad_bar" and not nv_res["prereqs"]["prerequisites"][1]["injected_fail"]:
                    raise RuntimeError(f"[{cname}] Expected prereq #2 injected_fail=true")
        finally:
            sess.close()
            img.unlink(missing_ok=True)
        if _sha256_file(nvme_img) != nvme_pre:
            nvme_img.unlink(missing_ok=True)
            raise RuntimeError(f"[{cname}] Canary NVMe modified during negative NVIDIA test!")
        print(f"  [PASS] p18g/{cname}")
    nvme_img.unlink(missing_ok=True)
    return len(cases)


def run_p18_nvidia_suite(build_dir: pathlib.Path, ovmf_code: pathlib.Path, ovmf_vars: pathlib.Path) -> tuple[int, dict[str, Any]]:
    """Run safe-gating + NVIDIA Level-1 E2E + 6 negative fault cases (returns check count = 8)."""
    t0 = time.monotonic()
    _run_nv_safe_gating(build_dir, ovmf_code, ovmf_vars)
    res_f = _run_nv_exp_level1_e2e(build_dir, ovmf_code, ovmf_vars)
    n_neg = _run_nv_negative_faults(build_dir, ovmf_code, ovmf_vars)
    return 2 + n_neg, {"p18nv_e2e": res_f, "p18nv_neg_cases": n_neg, "elapsed_s": round(time.monotonic() - t0, 2)}
