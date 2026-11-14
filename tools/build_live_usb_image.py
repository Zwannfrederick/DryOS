#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Build and QEMU-xHCI verify the bootable DRY OS Project Ascension 4.0 Live USB image."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import queue
import shutil
import struct
import subprocess
import sys
import threading
import time
from typing import Any

TOOLS_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS_DIR))

from qemu_boot_test import (  # noqa: E402
    BUILD_DIR,
    EXIT_CODE_SUCCESS,
    KERNEL_DIR,
    LIMINE_EFI,
    OVMF_CODE,
    OVMF_VARS,
    ROOT,
    USERSPACE_DIR,
    USERSPACE_MODS,
    build_kernel,
)
from qemu_log_validators import _parse_kv_ints  # noqa: E402
from qemu_p2_validators import _DesktopQmpClient  # noqa: E402

DEFAULT_IMG = BUILD_DIR / "dryos-ascension4-live-usb.img"
OUT_JSON = ROOT / "tools" / "compat_bench" / "results_live_usb_boot.json"
IMAGE_MIB = 96
PART_OFFSET = 1024 * 1024  # LBA 2048 (1 MiB)


def _sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _limine_conf_text() -> str:
    mod_lines = "".join(f"    module_path: boot():/boot/{dst}\n" for _, dst in USERSPACE_MODS)
    entries = [
        ("DRY OS - DRY Glass 2.1 Desktop (1280x800, Default)", "1280x800x32", "console=com1 desktop4=1"),
        ("DRY OS - DRY Glass 2.1 FHD Desktop (1920x1080)", "1920x1080x32", "console=com1 desktop4=1"),
        ("DRY OS - Safe UEFI GOP Desktop (No GPU Island)", "1280x800x32", "console=com1 desktop=1"),
        ("DRY OS - Linux v6.18 DRM Driver Island Audit", "1280x800x32", "console=com1 desktop4=1 lx_drm=1"),
    ]
    blocks = ["timeout: 1", "serial: yes", ""]
    for title, res, cmdline in entries:
        blocks.append(f"/{title}")
        blocks.append("    protocol: limine")
        blocks.append(f"    resolution: {res}")
        blocks.append("    path: boot():/boot/dry-kernel")
        blocks.append(f"    cmdline: {cmdline}")
        blocks.append(mod_lines.rstrip("\n"))
        blocks.append("")
    return "\n".join(blocks)


def _binaries_ready() -> tuple[pathlib.Path, pathlib.Path, bool]:
    kernel_elf = KERNEL_DIR / "target" / "x86_64-unknown-none" / "release" / "dry-kernel"
    u_dir = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release"
    ok = kernel_elf.exists() and all((u_dir / src).exists() for src, _ in USERSPACE_MODS)
    return kernel_elf, u_dir, ok


def build_live_usb_image(img_path: pathlib.Path, force_build: bool = False) -> dict[str, Any]:
    kernel_elf, u_dir, ready = _binaries_ready()
    if force_build or not ready:
        kernel_elf = build_kernel()

    img_path.parent.mkdir(parents=True, exist_ok=True)
    total_bytes = IMAGE_MIB * 1024 * 1024
    lba_start = PART_OFFSET // 512
    lba_sectors = (total_bytes - PART_OFFSET) // 512

    with img_path.open("wb") as f:
        f.truncate(total_bytes)
        entry = struct.pack(
            "<BBBBBBBBII", 0x80, 0xFE, 0xFF, 0xFF, 0xEF, 0xFE, 0xFF, 0xFF, lba_start, lba_sectors
        )
        f.seek(446)
        f.write(entry)
        f.seek(510)
        f.write(b"\x55\xaa")

    mtools_env = {**os.environ, "LC_ALL": "C"}
    part_spec = f"{img_path}@@{PART_OFFSET}"
    subprocess.check_call(["mformat", "-i", part_spec, "-F", "-v", "DRYOS_LIVE", "::"], env=mtools_env)
    for d in ("::/EFI", "::/EFI/BOOT", "::/boot", "::/boot/limine"):
        subprocess.check_call(["mmd", "-i", part_spec, d], env=mtools_env)

    conf_path = BUILD_DIR / "live_usb_limine.conf"
    conf_path.write_text(_limine_conf_text(), encoding="utf-8")

    modules_meta = []
    for src_name, dst_name in USERSPACE_MODS:
        src_p = u_dir / src_name
        modules_meta.append(
            {"name": dst_name, "bytes": src_p.stat().st_size, "sha256": _sha256(src_p)}
        )

    manifest = {
        "image_size_mib": IMAGE_MIB,
        "partition_offset_bytes": PART_OFFSET,
        "bootloader": "Limine BOOTX64.EFI",
        "kernel": {"name": "dry-kernel", "bytes": kernel_elf.stat().st_size, "sha256": _sha256(kernel_elf)},
        "module_count": len(modules_meta),
        "modules": modules_meta,
    }
    manifest_path = BUILD_DIR / "DRYOS_LIVE_MANIFEST.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    subprocess.check_call(["mcopy", "-i", part_spec, str(LIMINE_EFI), "::/EFI/BOOT/BOOTX64.EFI"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(conf_path), "::/boot/limine/limine.conf"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(kernel_elf), "::/boot/dry-kernel"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(manifest_path), "::/DRYOS_LIVE_MANIFEST.json"], env=mtools_env)
    for src_name, dst_name in USERSPACE_MODS:
        subprocess.check_call(
            ["mcopy", "-i", part_spec, str(u_dir / src_name), f"::/boot/{dst_name}"],
            env=mtools_env,
        )

    mdir_out = subprocess.check_output(["mdir", "-b", "-i", part_spec, "::/boot"], env=mtools_env, text=True)
    entries = {ln.strip().lower() for ln in mdir_out.splitlines() if ln.strip()}
    if "::/boot/dry-kernel" not in entries:
        raise RuntimeError("Live USB verification failed: missing dry-kernel in ::/boot")
    for _, dst_name in USERSPACE_MODS:
        if f"::/boot/{dst_name.lower()}" not in entries:
            raise RuntimeError(f"Live USB verification failed: missing {dst_name} in ::/boot")

    img_sha = _sha256(img_path)
    print(
        f"[LIVE-USB] Built {img_path} ({IMAGE_MIB} MiB, sha256={img_sha[:16]}..., "
        f"{len(USERSPACE_MODS)} modules verified in FAT32 ::/boot)."
    )
    return {"image_path": str(img_path), "image_sha256": img_sha, "module_count": len(USERSPACE_MODS)}


def verify_usb_xhci_boot(img_path: pathlib.Path, meta: dict[str, Any]) -> dict[str, Any]:
    vars_copy = BUILD_DIR / "OVMF_VARS_live_usb.4m.fd"
    shutil.copyfile(OVMF_VARS, vars_copy)
    qmp_sock = pathlib.Path(f"/tmp/dryos_live_usb_{os.getpid()}.sock")
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
        "512M",
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive",
        f"if=pflash,format=raw,file={vars_copy}",
        "-vga",
        "std",
        "-global",
        "VGA.vgamem_mb=32",
        "-device",
        "qemu-xhci,id=xhci0",
        "-drive",
        f"if=none,id=usbdisk,format=raw,readonly=on,file={img_path}",
        "-device",
        "usb-storage,bus=xhci0.0,drive=usbdisk,bootindex=1",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial",
        "stdio",
        "-display",
        "none",
        "-qmp",
        f"unix:{qmp_sock},server=on,wait=off",
        "-no-reboot",
    ]

    t0 = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace", bufsize=1)
    line_q: queue.Queue[str | None] = queue.Queue()
    serial_lines: list[str] = []

    def _reader() -> None:
        assert proc.stdout is not None
        for ln in proc.stdout:
            line_q.put(ln)
        line_q.put(None)

    rd = threading.Thread(target=_reader, daemon=True)
    rd.start()

    def wait_for(marker: str, timeout_s: float = 35.0) -> str:
        for ex in serial_lines:
            if marker in ex:
                return ex
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
        raise RuntimeError(f"Live USB xHCI boot timeout waiting for {marker!r}:\n{''.join(serial_lines[-40:])}")

    qmp: _DesktopQmpClient | None = None
    try:
        wait_for("[DRY-WM-SCALABILITY] pass=1", timeout_s=40.0)
        wait_for("[DRY-SHELL] Interactive shell ready in CPL3 Userspace", timeout_s=20.0)
        wait_for("[DRY-SYSMON] Ready in CPL3 Userspace", timeout_s=15.0)
        for _ in range(50):
            if qmp_sock.exists():
                break
            time.sleep(0.1)
        qmp = _DesktopQmpClient(qmp_sock)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        sess_ln = wait_for("[DRY-P2-SESSION] PASS:", timeout_s=25.0)
        wait_for("[DRY-BOOT-OK]", timeout_s=10.0)
        qmp.close()
        qmp = None
        rc = proc.wait(timeout=15.0)
        rd.join(timeout=2.0)
        full = "".join(serial_lines)
        if rc != EXIT_CODE_SUCCESS or "[DRY-PANIC]" in full or "[DRY-EXCEPTION]" in full:
            raise RuntimeError(f"Live USB xHCI boot failed: rc={rc}\n{full}")
        skv = _parse_kv_ints(sess_ln)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"Live USB leaked_frames != 0: {sess_ln!r}")
        res = {
            **meta,
            "image_size_mib": IMAGE_MIB,
            "boot_transport": "qemu-xhci + usb-storage",
            "leaked_frames": skv["leaked_frames"],
            "exit_code": rc,
            "boot_to_shutdown_sec": round(time.monotonic() - t0, 3),
        }
        OUT_JSON.parent.mkdir(parents=True, exist_ok=True)
        OUT_JSON.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")
        print(
            f"[PASS] Live USB xHCI boot verified: leaked_frames={res['leaked_frames']} "
            f"exit={rc} modules={res['module_count']} ({res['boot_to_shutdown_sec']}s)."
        )
        return res
    finally:
        if qmp is not None:
            qmp.close()
        if proc.poll() is None:
            proc.kill()
        rd.join(timeout=2.0)
        qmp_sock.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--build", action="store_true", help="Force rebuild of kernel and 24 userspace ELFs")
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_IMG, help="Output Live USB image path")
    parser.add_argument("--verify-boot", action="store_true", help="Verify UEFI boot via QEMU qemu-xhci usb-storage")
    args = parser.parse_args()

    meta = build_live_usb_image(args.output, force_build=args.build)
    if args.verify_boot:
        verify_usb_xhci_boot(args.output, meta)


if __name__ == "__main__":
    main()
