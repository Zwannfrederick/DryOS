#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Ascension 8.0 Track A/B Hardware Validation & Persistence Fault Matrix (`--p18-only`)."""

from __future__ import annotations

import hashlib
import os
import pathlib
import queue
import shutil
import subprocess
import sys
import threading
import time
from typing import Any

from qemu_log_validators import _parse_kv_ints
from qemu_p1_validators import EXIT_CODE_SUCCESS
from qemu_p2_validators import _DesktopQmpClient
from retrieve_dryos_logs import retrieve_and_verify


def _sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _sha256_slice(path: pathlib.Path, start_byte: int, length: int) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        f.seek(start_byte)
        rem = length
        while rem > 0:
            chunk = f.read(min(1 << 20, rem))
            if not chunk:
                raise RuntimeError(f"Unexpected EOF slicing {path}")
            h.update(chunk)
            rem -= len(chunk)
    return h.hexdigest()


def _create_canary_disk(path: pathlib.Path, seed: bytes, size_mib: int = 4) -> str:
    blk = (seed * ((4096 // len(seed)) + 1))[:4096]
    with path.open("wb") as f:
        for _ in range((size_mib * 1024 * 1024) // 4096):
            f.write(blk)
    return _sha256_file(path)


class _HwvalSession:
    """QEMU xHCI + UEFI runner for Ascension 8.0 multi-partition validation images."""

    def __init__(
        self,
        name: str,
        usb_img: pathlib.Path,
        ovmf_code: pathlib.Path,
        ovmf_vars: pathlib.Path,
        build_dir: pathlib.Path,
        iommu: bool = True,
        usb_hid: bool = True,
        extra_args: list[str] | None = None,
        mem: str = "512M",
    ) -> None:
        self.name = name
        vars_copy = build_dir / f"OVMF_VARS_{name}.4m.fd"
        shutil.copyfile(ovmf_vars, vars_copy)
        self.qmp_sock = pathlib.Path(f"/tmp/dryos_p18_{os.getpid()}_{name}.sock")
        self.qmp_sock.unlink(missing_ok=True)
        accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
        cmd = [
            "qemu-system-x86_64",
            "-machine",
            f"q35,accel={accel}",
            "-cpu",
            "host" if accel == "kvm" else "qemu64",
            "-smp",
            "2",
            "-m",
            mem,
            "-drive",
            f"if=pflash,format=raw,readonly=on,file={ovmf_code}",
            "-drive",
            f"if=pflash,format=raw,file={vars_copy}",
            "-vga",
            "std",
            "-global",
            "VGA.vgamem_mb=32",
        ]
        if iommu:
            cmd.extend(["-device", "intel-iommu,intremap=off,caching-mode=on"])
        cmd.extend(
            [
                "-device",
                "qemu-xhci,id=xhci0",
                "-drive",
                f"if=none,id=usbdisk,format=raw,file={usb_img}",
                "-device",
                "usb-storage,id=usbmsc0,bus=xhci0.0,drive=usbdisk,bootindex=1",
                "-device",
                "isa-debug-exit,iobase=0xf4,iosize=0x04",
                "-serial",
                "stdio",
                "-display",
                "none",
                "-qmp",
                f"unix:{self.qmp_sock},server=on,wait=off",
                "-no-reboot",
            ]
        )
        if usb_hid:
            cmd.extend(["-device", "usb-kbd,id=usbkbd0,bus=xhci0.0", "-device", "usb-mouse,id=usbmouse0,bus=xhci0.0"])
        if extra_args:
            cmd.extend(extra_args)

        self.proc = subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace", bufsize=1
        )
        self.line_q: queue.Queue[str | None] = queue.Queue()
        self.serial_lines: list[str] = []
        self.qmp: _DesktopQmpClient | None = None

        def _reader() -> None:
            assert self.proc.stdout is not None
            for ln in self.proc.stdout:
                self.line_q.put(ln)
            self.line_q.put(None)

        self.rd = threading.Thread(target=_reader, daemon=True)
        self.rd.start()

    def wait_for(self, marker: str, start_idx: int = 0, timeout_s: float = 40.0) -> str:
        for ex in self.serial_lines[start_idx:]:
            if marker in ex:
                return ex
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                item = self.line_q.get(timeout=min(0.25, max(0.05, deadline - time.monotonic())))
            except queue.Empty:
                if self.proc.poll() is not None and self.line_q.empty():
                    break
                continue
            if item is None:
                break
            self.serial_lines.append(item)
            if len(self.serial_lines) > start_idx and marker in item:
                return item
        tail = "".join(self.serial_lines[-40:])
        raise RuntimeError(f"[{self.name}] Timeout waiting for {marker!r}:\n{tail}")

    def connect_qmp(self) -> _DesktopQmpClient:
        if self.qmp is not None:
            return self.qmp
        for _ in range(50):
            if self.qmp_sock.exists():
                break
            time.sleep(0.1)
        self.qmp = _DesktopQmpClient(self.qmp_sock)
        time.sleep(0.08)
        return self.qmp

    def run_cmd(self, cmd_text: str, expect_marker: str, wait_s: float = 12.0) -> str:
        qmp = self.connect_qmp()
        idx = len(self.serial_lines)
        qmp.type_ascii(cmd_text)
        qmp.send_qcodes(["ret"])
        return self.wait_for(expect_marker, start_idx=idx, timeout_s=wait_s)

    def shutdown_clean(self) -> tuple[int, str, str]:
        sess_ln = self.run_cmd("shutdown", "[DRY-P2-SESSION] PASS:", wait_s=25.0)
        self.wait_for("[DRY-BOOT-OK]", timeout_s=10.0)
        rc = self.proc.wait(timeout=15.0)
        self.rd.join(timeout=2.0)
        full = "".join(self.serial_lines)
        if rc != EXIT_CODE_SUCCESS:
            raise RuntimeError(f"[{self.name}] Unexpected exit rc={rc}\n{full[-2000:]}")
        if "[DRY-PANIC]" in full or "[DRY-EXCEPTION]" in full:
            raise RuntimeError(f"[{self.name}] Kernel panic or unhandled exception occurred\n{full[-2000:]}")
        if _parse_kv_ints(sess_ln).get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{self.name}] leaked_frames != 0: {sess_ln!r}")
        return rc, sess_ln, full

    def close(self) -> None:
        if self.qmp is not None:
            self.qmp.close()
            self.qmp = None
        if self.proc.poll() is None:
            self.proc.kill()
        self.rd.join(timeout=2.0)
        self.qmp_sock.unlink(missing_ok=True)


def _run_p18a_safe_auto_and_canary(
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    persisted_img_out: pathlib.Path,
) -> dict[str, Any]:
    from build_live_usb_image import _sha256_prefix, build_hwval_usb_image

    t0 = time.monotonic()
    src_img = build_dir / "p18a_hwval.img"
    meta = build_hwval_usb_image(src_img, force_build=False)
    p3_start_lba = int(meta["part3_range"][0])
    ro_prefix_bytes = p3_start_lba * 512
    pre_ro_sha = _sha256_prefix(src_img, ro_prefix_bytes)

    nvme_img = build_dir / "p18a_canary_nvme.img"
    vblk_img = build_dir / "p18a_canary_vblk.img"
    nvme_pre = _create_canary_disk(nvme_img, b"DRYOS_CANARY_NVME_")
    vblk_pre = _create_canary_disk(vblk_img, b"DRYOS_CANARY_VBLK_")

    extra_disks = [
        "-drive", f"if=none,id=nvmedisk,format=raw,file={nvme_img}",
        "-device", "nvme,serial=DRYP18CANARY,drive=nvmedisk",
        "-drive", f"if=none,id=vblkdisk,format=raw,file={vblk_img}",
        "-device", "virtio-blk-pci,drive=vblkdisk,disable-legacy=on",
    ]
    shutil.copyfile(src_img, persisted_img_out)
    s = _HwvalSession("p18a_safe", persisted_img_out, ovmf_code, ovmf_vars, build_dir, iommu=True, extra_args=extra_disks)
    try:
        s.wait_for("[DRY-HWVAL] pci_passive=1")
        s.wait_for("[DRY-USB] LOGS_ANCESTRY gates=0x1f")
        auto_ln = s.wait_for("[DRY-HWDIAG] AUTO_COMPLETE mode=safe")
        s.wait_for("[DRY-HWDIAG] PERSIST status=VERIFIED files=13 sha256_verified=1")
        s.wait_for("[DRY-SHELL] Interactive shell ready")
        s.wait_for("[DRY-SYSMON] Ready")

        cmds = [
            ("dry help", "[DRY-SHELL] CMD=dry help ok=true"),
            ("dry doctor", "[DRY-SHELL] CMD=dry doctor"),
            ("dry check --safe", "[DRY-SHELL] CMD=dry check_safe"),
            ("dry check --all", "[DRY-SHELL] CMD=dry check_all"),
            ("dry check --cpu", "[DRY-SHELL] CMD=dry check_cpu"),
            ("dry check --memory", "[DRY-SHELL] CMD=dry check_memory"),
            ("dry check --acpi", "[DRY-SHELL] CMD=dry check_acpi"),
            ("dry check --pci", "[DRY-SHELL] CMD=dry check_pci"),
            ("dry check --iommu", "[DRY-SHELL] CMD=dry check_iommu"),
            ("dry check --gpu", "[DRY-SHELL] CMD=dry check_gpu"),
            ("dry check --usb", "[DRY-SHELL] CMD=dry check_usb"),
            ("dry check --storage", "[DRY-SHELL] CMD=dry check_storage"),
            ("dry drivers", "[DRY-SHELL] CMD=dry drivers"),
            ("dry compat", "[DRY-SHELL] CMD=dry compat"),
            ("dry report", "[DRY-SHELL] CMD=dry report"),
            ("dry report --export-usb", "[DRY-SHELL] CMD=dry report_export"),
        ]
        for c, expect in cmds:
            ln = s.run_cmd(c, expect)
            if c != "dry help" and "persist=VERIFIED gates=0x1f" not in ln:
                raise RuntimeError(f"[p18a] Command {c!r} unexpected line: {ln!r}")

        _, _, full = s.shutdown_clean()
        if "fail=0" not in auto_ln:
            raise RuntimeError(f"[p18a] Safe auto check reported unexpected failures: {auto_ln!r}")
    finally:
        s.close()

    if _sha256_file(nvme_img) != nvme_pre:
        raise RuntimeError("[p18a] Canary NVMe disk was modified during safe hardware validation!")
    if _sha256_file(vblk_img) != vblk_pre:
        raise RuntimeError("[p18a] Canary VirtIO-Blk disk was modified during safe hardware validation!")
    if _sha256_prefix(persisted_img_out, ro_prefix_bytes) != pre_ro_sha:
        raise RuntimeError("[p18a] ESP or DRYOS_PERSIST modified under storage=ro!")

    b1 = retrieve_and_verify(persisted_img_out, None, build_dir / "p18a_bundle_s1", session_name="sess-0001")
    b2 = retrieve_and_verify(persisted_img_out, None, build_dir / "p18a_bundle_s2", session_name="sess-0002")
    if b1["files_verified"] != 13 or b2["files_verified"] != 13 or b2["available_sessions"] != ["sess-0001", "sess-0002"]:
        raise RuntimeError(f"[p18a] Unexpected retrieved sessions: {b2!r}")

    print(f"[PASS] p18a_hwval_safe_auto_and_canary: 15 CLI cmds, 2 sessions verified, 2 canary disks untouched ({round(time.monotonic() - t0, 2)}s).")
    return {"meta": meta, "sessions": b2["available_sessions"], "elapsed_s": round(time.monotonic() - t0, 2)}


def _run_p18b_recovery_and_multisession(
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    persisted_img: pathlib.Path,
    meta: dict[str, Any],
) -> dict[str, Any]:
    from build_live_usb_image import _limine_hwval_conf_text, patch_img_limine_conf

    t0 = time.monotonic()
    p1_start, p1_end = meta["part1_range"]
    p3_start, p3_end = meta["part3_range"]
    p3_off = p3_start * 512
    p3_len = (p3_end - p3_start + 1) * 512

    # Reboot 1: RECOVERY / LOG VIEWER — read-only inspection, zero AUTO_COMPLETE, P3 untouched.
    patch_img_limine_conf(
        persisted_img, p1_start, p1_end - p1_start + 1,
        _limine_hwval_conf_text(meta["disk_guid_hex"], default_idx=3),
    )
    p3_pre_sha = _sha256_slice(persisted_img, p3_off, p3_len)
    s = _HwvalSession("p18b_recovery", persisted_img, ovmf_code, ovmf_vars, build_dir)
    try:
        rec_ln = s.wait_for("[DRY-HWDIAG] RECOVERY_SESSIONS count=2")
        s.wait_for("[DRY-SHELL] Interactive shell ready")
        rep_ln = s.run_cmd("dry report", "[DRY-SHELL] CMD=dry report")
        if "persist=RECOVERY" not in rep_ln:
            raise RuntimeError(f"[p18b] Recovery report status wrong: {rep_ln!r}")
        _, _, full = s.shutdown_clean()
        if "[DRY-HWDIAG] AUTO_COMPLETE" in full:
            raise RuntimeError(f"[p18b] Recovery mode unexpectedly executed AUTO_COMPLETE:\n{rec_ln}")
    finally:
        s.close()

    if _sha256_slice(persisted_img, p3_off, p3_len) != p3_pre_sha:
        raise RuntimeError("[p18b] DRYOS_LOGS partition mutated during RECOVERY / LOG VIEWER boot!")

    # Reboot 2: SAFE AUTO CHECK again — must append sess-0003 cleanly without corrupting P3.
    patch_img_limine_conf(
        persisted_img, p1_start, p1_end - p1_start + 1,
        _limine_hwval_conf_text(meta["disk_guid_hex"], default_idx=0),
    )
    s2 = _HwvalSession("p18b_multisess", persisted_img, ovmf_code, ovmf_vars, build_dir)
    try:
        s2.wait_for("[DRY-HWDIAG] AUTO_COMPLETE mode=safe pass=")
        s2.wait_for("session=sess-0003")
        s2.wait_for("[DRY-HWDIAG] PERSIST status=VERIFIED files=13 sha256_verified=1")
        s2.wait_for("[DRY-SHELL] Interactive shell ready")
        s2.shutdown_clean()
    finally:
        s2.close()

    b3 = retrieve_and_verify(persisted_img, None, build_dir / "p18b_bundle_s3")
    if b3["session_id"] != "sess-0003" or b3["available_sessions"] != ["sess-0001", "sess-0002", "sess-0003"]:
        raise RuntimeError(f"[p18b] Multi-session append mismatch: {b3!r}")

    print(f"[PASS] p18b_reboot_recovery_and_multisession: recovery RO verified + sess-0003 appended cleanly ({round(time.monotonic() - t0, 2)}s).")
    return {"sessions": b3["available_sessions"], "elapsed_s": round(time.monotonic() - t0, 2)}


def _run_p18c_negative_fault_matrix(
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
) -> int:
    from build_live_usb_image import build_hwval_usb_image

    t0 = time.monotonic()
    nvme_img = build_dir / "p18c_canary_nvme.img"
    nvme_pre = _create_canary_disk(nvme_img, b"DRYOS_NEG_NVME_")
    nvme_args = ["-drive", f"if=none,id=nvmedisk,format=raw,file={nvme_img}", "-device", "nvme,serial=DRYP18NEG,drive=nvmedisk"]

    cases = [
        ("neg1_missing_p3", {"omit_p3": True}, True, "[DRY-HWDIAG] PERSIST status=BLOCKED", "fail=0"),
        ("neg2_write_fail", {"limine_extra": "dry_test_usb_write_fail=1"}, True, "[DRY-HWDIAG] PERSIST status=FAILED", None),
        ("neg3_wrong_guid", {"guid_override": b"\x99" * 16}, True, "[DRY-HWDIAG] PERSIST status=BLOCKED", None),
        ("neg4_corrupt_bpb", {"corrupt_p3_bpb": True}, True, "[DRY-HWDIAG] PERSIST status=FAILED", None),
        ("neg5_full_fat32", {"fill_p3_full": True}, True, "[DRY-HWDIAG] PERSIST status=FAILED", "reason=-28"),
        ("neg6_usb_disconn", {"limine_extra": "dry_test_usb_disconnect=1"}, True, "[DRY-HWDIAG] PERSIST status=FAILED", None),
        ("neg7_bad_acpi", {"limine_extra": "dry_test_malformed_acpi=1"}, True, "[DRY-HWDIAG] AUTO_COMPLETE", "fail=1"),
        ("neg8_bad_pci_bar", {"limine_extra": "dry_test_bad_pci_bar=1"}, True, "[DRY-HWDIAG] AUTO_COMPLETE", "fail=1"),
        ("neg9_no_iommu", {"limine_extra": "dry_qemu_test_no_iommu=1"}, False, "[DRY-HWDIAG] AUTO_COMPLETE", "blocked="),
    ]

    for cname, build_kw, iommu_on, expect1, expect2 in cases:
        img = build_dir / f"p18c_{cname}.img"
        build_hwval_usb_image(img, force_build=False, **build_kw)
        sess = _HwvalSession(f"p18c_{cname}", img, ovmf_code, ovmf_vars, build_dir, iommu=iommu_on, usb_hid=iommu_on, extra_args=nvme_args)
        try:
            sess.wait_for("[DRY-HWVAL] pci_passive=1")
            sess.wait_for("[DRY-HWDIAG] AUTO_COMPLETE")
            ln1 = sess.wait_for(expect1)
            if expect2 and expect2 not in ln1:
                # Re-scan all serial lines for secondary token (e.g. `reason=-28` or `fail=1`).
                joined = "".join(sess.serial_lines)
                if expect2 not in joined:
                    raise RuntimeError(f"[{cname}] Missing {expect2!r} in:\n{ln1}")
            if cname == "neg9_no_iommu":
                kv = _parse_kv_ints(ln1)
                if kv.get("blocked", 0) < 1:
                    raise RuntimeError(f"[{cname}] Expected blocked>=1 in {ln1!r}")
            sess.wait_for("[DRY-SHELL] Interactive shell ready")
            if cname in ("neg1_missing_p3", "neg3_wrong_guid"):
                rln = sess.run_cmd("dry report", "[DRY-SHELL] CMD=dry report")
                if "persist=BLOCKED" not in rln:
                    raise RuntimeError(f"[{cname}] RAM report lost BLOCKED status: {rln!r}")
            elif cname in ("neg2_write_fail", "neg4_corrupt_bpb", "neg5_full_fat32", "neg6_usb_disconn"):
                rln = sess.run_cmd("dry report", "[DRY-SHELL] CMD=dry report")
                if "persist=FAILED" not in rln:
                    raise RuntimeError(f"[{cname}] RAM report lost FAILED status: {rln!r}")
            sess.shutdown_clean()
        finally:
            sess.close()
            img.unlink(missing_ok=True)
        if _sha256_file(nvme_img) != nvme_pre:
            raise RuntimeError(f"[{cname}] Canary NVMe modified during negative fault case!")
        print(f"  [PASS] p18c/{cname}")

    print(f"[PASS] p18c_negative_fault_matrix: all {len(cases)}/9 negative persistence/hardware fault tests passed ({round(time.monotonic() - t0, 2)}s).")
    return len(cases)


def _run_p18d_security_hardening(
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
) -> dict[str, Any]:
    from build_live_usb_image import build_hwval_usb_image

    t0 = time.monotonic()
    nvme_img = build_dir / "p18d_canary_nvme.img"
    nvme_pre = _create_canary_disk(nvme_img, b"DRYOS_SEC_M2_")
    nvme_args = [
        "-drive",
        f"if=none,id=nvmedisk,format=raw,file={nvme_img}",
        "-device",
        "nvme,serial=DRYP18SEC,drive=nvmedisk",
    ]
    img = build_dir / "p18d_security.img"
    build_hwval_usb_image(img, force_build=False, limine_extra="dry_test_sec_probe=1")
    sess = _HwvalSession(
        "p18d_security",
        img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        iommu=True,
        usb_hid=True,
        extra_args=nvme_args,
    )
    try:
        sess.wait_for("[DRY-SEC-M2] PASS: usb_unauth_ud2_eperm=1 usb_alive=1 cap_flood_reclaimed=1")
        sess.wait_for("[DRY-SHELL] Interactive shell ready")
        sess.shutdown_clean()
    finally:
        sess.close()
        img.unlink(missing_ok=True)
    if _sha256_file(nvme_img) != nvme_pre:
        nvme_img.unlink(missing_ok=True)
        raise RuntimeError("[p18d] Canary NVMe modified during security probe!")
    nvme_img.unlink(missing_ok=True)
    elapsed = round(time.monotonic() - t0, 2)
    print(f"[PASS] p18d_security_hardening: M2 IPC/cap hardening + leaked_frames=0 + NVMe untouched ({elapsed}s).")
    return {"elapsed_s": elapsed}


def run_p18_hwval_suite(
    _kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    _write_mbr_esp_image: Any,
) -> int:
    """Execute Ascension 8.0 Hardware Validation & Persistence suite (`--p18-only`)."""
    print("\n=== Running Ascension 8.0 Hardware Validation & Persistence Suite (P18) ===")
    persisted = build_dir / "p18_shared_persisted.img"
    res_a = _run_p18a_safe_auto_and_canary(build_dir, ovmf_code, ovmf_vars, persisted)
    res_b = _run_p18b_recovery_and_multisession(build_dir, ovmf_code, ovmf_vars, persisted, res_a["meta"])
    n_neg = _run_p18c_negative_fault_matrix(build_dir, ovmf_code, ovmf_vars)
    res_d = _run_p18d_security_hardening(build_dir, ovmf_code, ovmf_vars)
    persisted.unlink(missing_ok=True)
    out = root / "tools" / "compat_bench" / "results_p18_hwval.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    import json
    out.write_text(json.dumps({"p18a": res_a, "p18b": res_b, "p18c_cases": n_neg, "p18d": res_d}, indent=2) + "\n", encoding="utf-8")
    return 3 + n_neg
