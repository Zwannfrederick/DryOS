#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Build x86-64 `dry-kernel` offline and verify QEMU UEFI boot via serial + exit code."""

import argparse
import os
import pathlib
import shutil
import struct
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
KERNEL_DIR = ROOT / "kernel"
BUILD_DIR = KERNEL_DIR / "build"
TC_DIR = ROOT / ".toolchains"
SYSROOT = TC_DIR / "sysroot"
CARGO_HOME = TC_DIR / "cargo-home"
LIMINE_EFI = TC_DIR / "limine" / "BOOTX64.EFI"

OVMF_CODE = pathlib.Path("/usr/share/edk2/x64/OVMF_CODE.4m.fd")
OVMF_VARS = pathlib.Path("/usr/share/edk2/x64/OVMF_VARS.4m.fd")

EXIT_CODE_SUCCESS = (0x10 << 1) | 1  # 33
EXIT_CODE_PANIC = (0x11 << 1) | 1    # 35


def kernel_cargo_env() -> dict[str, str]:
    linker_script = KERNEL_DIR / "linker.ld"
    rustflags = " ".join(
        [
            f"--sysroot={SYSROOT}",
            "-C",
            "relocation-model=static",
            "-C",
            f"link-arg=-T{linker_script}",
            "-C",
            "link-arg=-zmax-page-size=0x1000",
        ]
    )
    return {
        **os.environ,
        "RUSTC_BOOTSTRAP": "1",
        "CARGO_HOME": str(CARGO_HOME),
        "__CARGO_TESTS_ONLY_SRC_ROOT": str(SYSROOT / "lib" / "rustlib" / "src" / "rust" / "library"),
        "RUSTFLAGS": rustflags,
    }


def build_kernel() -> pathlib.Path:
    subprocess.check_call([sys.executable, str(ROOT / "tools" / "bootstrap_phase1.py"), "--verify"])
    env = kernel_cargo_env()

    print("[QEMU-TEST] Running cargo clippy (-D warnings) on dry-kernel...")
    subprocess.check_call(
        [
            "cargo",
            "clippy",
            "--offline",
            "--manifest-path",
            str(KERNEL_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--",
            "-D",
            "warnings",
        ],
        env=env,
    )

    print("[QEMU-TEST] Building release ELF64 dry-kernel...")
    subprocess.check_call(
        [
            "cargo",
            "build",
            "--offline",
            "--manifest-path",
            str(KERNEL_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--release",
        ],
        env=env,
    )

    elf = KERNEL_DIR / "target" / "x86_64-unknown-none" / "release" / "dry-kernel"
    if not elf.exists():
        raise RuntimeError(f"Expected kernel ELF not found at {elf}")
    return elf


def write_mbr_esp_image(img_path: pathlib.Path, kernel_elf: pathlib.Path, cmdline: str) -> None:
    """Create a 64 MiB MBR-partitioned FAT32 EFI System Partition image using `mtools`."""
    total_mib = 64
    total_bytes = total_mib * 1024 * 1024
    part_offset = 1024 * 1024  # LBA 2048 (1 MiB)
    lba_start = part_offset // 512
    lba_sectors = (total_bytes - part_offset) // 512

    with img_path.open("wb") as f:
        f.truncate(total_bytes)
        # MBR Partition Entry #1 at offset 446: bootable=0x80, type=0xEF (EFI System Partition)
        entry = struct.pack("<BBBBBBBBII", 0x80, 0xFE, 0xFF, 0xFF, 0xEF, 0xFE, 0xFF, 0xFF, lba_start, lba_sectors)
        f.seek(446)
        f.write(entry)
        f.seek(510)
        f.write(b"\x55\xaa")

    mtools_env = {**os.environ, "LC_ALL": "C"}
    part_spec = f"{img_path}@@{part_offset}"
    subprocess.check_call(["mformat", "-i", part_spec, "-F", "-v", "DRYOS_ESP", "::"], env=mtools_env)
    for d in ["::/EFI", "::/EFI/BOOT", "::/boot", "::/boot/limine"]:
        subprocess.check_call(["mmd", "-i", part_spec, d], env=mtools_env)

    conf_path = BUILD_DIR / "limine.conf"
    conf_text = (
        "timeout: 0\n"
        "serial: yes\n"
        "\n"
        "/DRY OS (Phase 1)\n"
        "    protocol: limine\n"
        "    path: boot():/boot/dry-kernel\n"
        f"    cmdline: {cmdline}\n"
    )
    conf_path.write_text(conf_text, encoding="utf-8")

    subprocess.check_call(["mcopy", "-i", part_spec, str(LIMINE_EFI), "::/EFI/BOOT/BOOTX64.EFI"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(conf_path), "::/boot/limine/limine.conf"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(kernel_elf), "::/boot/dry-kernel"], env=mtools_env)


def run_qemu(img_path: pathlib.Path, smp: int, timeout_sec: int = 20) -> tuple[int, str]:
    vars_copy = BUILD_DIR / "OVMF_VARS.4m.fd"
    shutil.copyfile(OVMF_VARS, vars_copy)

    accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
    cpu_arg = "host" if accel == "kvm" else "qemu64"

    cmd = [
        "qemu-system-x86_64",
        "-machine",
        f"q35,accel={accel}",
        "-cpu",
        cpu_arg,
        "-smp",
        str(smp),
        "-m",
        "256M",
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive",
        f"if=pflash,format=raw,file={vars_copy}",
        "-drive",
        f"format=raw,file={img_path}",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial",
        "stdio",
        "-display",
        "none",
        "-no-reboot",
    ]
    proc = subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        timeout=timeout_sec,
        check=False,
    )
    return proc.returncode, proc.stdout


def assert_boot_case(
    name: str,
    kernel_elf: pathlib.Path,
    cmdline: str,
    smp: int,
    expected_rc: int,
    must_contain: list[str],
    must_not_contain: list[str],
) -> None:
    img_path = BUILD_DIR / f"esp_{name}.img"
    write_mbr_esp_image(img_path, kernel_elf, cmdline)
    rc, serial_out = run_qemu(img_path, smp=smp)

    print(f"\n--- [{name}] QEMU exit={rc} (expected {expected_rc}) ---")
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Limine" in line or "PANIC" in line:
            print(f"  {line}")

    if rc != expected_rc:
        print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
        raise RuntimeError(f"[{name}] Unexpected QEMU exit code {rc} != {expected_rc}")
    for token in must_contain:
        if token not in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Missing required serial marker: {token!r}")
    for token in must_not_contain:
        if token in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Forbidden serial marker found: {token!r}")
    print(f"[PASS] {name}: exit={rc} and all {len(must_contain)} serial assertions verified.")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--smp", type=int, default=1, help="vCPU count for normal boot check")
    args = parser.parse_args()

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    kernel_elf = build_kernel()

    assert_boot_case(
        name="boot_1cpu_ok",
        kernel_elf=kernel_elf,
        cmdline="console=com1",
        smp=args.smp,
        expected_rc=EXIT_CODE_SUCCESS,
        must_contain=[
            "[DRY-BOOT] DRY Kernel v0.1.0",
            "Firmware: EFI64",
            "[DRY-CPU] PASS: GDT(CS=0x08,TR=0x28)",
            "#BP(hits=1,",
            "[DRY-HHDM] PASS:",
            "[DRY-MEMMAP] PASS:",
            "[DRY-SEC] PASS:",
            "[DRY-BOOT-OK]",
        ],
        must_not_contain=["[DRY-PANIC]", "[DRY-EXCEPTION]"],
    )

    assert_boot_case(
        name="panic_handler_deterministic",
        kernel_elf=kernel_elf,
        cmdline="console=com1 panic_self_test=1",
        smp=1,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-BOOT] DRY Kernel v0.1.0",
            "[DRY-PANIC]",
            "intentional panic_self_test triggered via kernel cmdline",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="exception_ud2_caught",
        kernel_elf=kernel_elf,
        cmdline="console=com1 fault_self_test=ud2",
        smp=1,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-EXCEPTION] vec=6 (#UD Invalid-Opcode)",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="exception_pf_caught",
        kernel_elf=kernel_elf,
        cmdline="console=com1 fault_self_test=pf",
        smp=1,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-EXCEPTION] vec=14 (#PF Page-Fault)",
            "cr2=0x7fffdeadbee0",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    print("\n[QEMU-TEST] ALL MILESTONE 1A BOOT CHECKS PASSED.")


if __name__ == "__main__":
    main()
