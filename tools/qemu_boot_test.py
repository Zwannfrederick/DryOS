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
import time

from qemu_log_validators import (
    expected_sched_metrics,
    verify_apic_and_irq_lines,
    verify_cspace_and_tlb_debt_lines,
    verify_pmm_and_untyped_lines,
    verify_preempt_line,
    verify_sched_line,
    verify_vspace_lines,
)
from qemu_ipc_validators import capture_and_verify_gop_screendump, verify_ipc_and_display_lines
from qemu_p1_validators import run_p1_suite, verify_p1_irq_lines
from qemu_p2_validators import run_p2_suite
from qemu_p3_validators import run_p3_suite
from qemu_p3c_validators import run_p3_hardware_matrix
from qemu_p4_validators import run_p4_suite
from qemu_p5_island_validators import run_p5_island_suite
from qemu_user_validators import verify_userspace_lines

ROOT = pathlib.Path(__file__).resolve().parents[1]
KERNEL_DIR = ROOT / "kernel"
USERSPACE_DIR = ROOT / "userspace"
BUILD_DIR = KERNEL_DIR / "build"
TC_DIR = ROOT / ".toolchains"
SYSROOT = TC_DIR / "sysroot"
CARGO_HOME = TC_DIR / "cargo-home"
LIMINE_EFI = TC_DIR / "limine" / "BOOTX64.EFI"

OVMF_CODE = pathlib.Path("/usr/share/edk2/x64/OVMF_CODE.4m.fd")
OVMF_VARS = pathlib.Path("/usr/share/edk2/x64/OVMF_VARS.4m.fd")

EXIT_CODE_SUCCESS = (0x10 << 1) | 1  # 33
EXIT_CODE_PANIC = (0x11 << 1) | 1    # 35

USERSPACE_MODS = (
    ("user-hello", "user-hello.elf"),
    ("user-peer", "user-peer.elf"),
    ("dry-init", "dry-init.elf"),
    ("dry-echo", "dry-echo.elf"),
    ("dry-display", "dry-display.elf"),
    ("dry-input", "dry-input.elf"),
    ("dry-shell", "dry-shell.elf"),
    ("dry-sysmon", "dry-sysmon.elf"),
    ("dry-vfs", "dry-vfs.elf"),
    ("dry-files", "dry-files.elf"),
    ("dry-settings", "dry-settings.elf"),
    ("dry-power", "dry-power.elf"),
    ("dry-blk", "dry-blk.elf"),
    ("dry-vault", "dry-vault.elf"),
    ("dry-broker", "dry-broker.elf"),
    ("dry-mock-drv", "dry-mock-drv.elf"),
)


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


def userspace_cargo_env() -> dict[str, str]:
    linker_script = USERSPACE_DIR / "linker_user.ld"
    rustflags = " ".join(
        [
            f"--sysroot={SYSROOT}",
            "-C",
            "relocation-model=static",
            "-C",
            "target-feature=-sse,-sse2,-avx",
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


def build_userspace() -> list[pathlib.Path]:
    env = userspace_cargo_env()
    print("[QEMU-TEST] Running cargo clippy (-D warnings) on dry-userspace...")
    subprocess.check_call(
        [
            "cargo",
            "clippy",
            "--offline",
            "--manifest-path",
            str(USERSPACE_DIR / "Cargo.toml"),
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
    print(
        "[QEMU-TEST] Building release ELF64 userspace binaries "
        "(user-hello, user-peer, dry-init, dry-echo, dry-display, dry-input, dry-shell, dry-sysmon)..."
    )
    subprocess.check_call(
        [
            "cargo",
            "build",
            "--offline",
            "--manifest-path",
            str(USERSPACE_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--release",
        ],
        env=env,
    )
    out_dir = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release"
    bins = [out_dir / src_name for src_name, _ in USERSPACE_MODS]
    for b in bins:
        if not b.exists():
            raise RuntimeError(f"Missing userspace ELF {b}")
    return bins


def build_kernel() -> pathlib.Path:
    subprocess.check_call([sys.executable, str(ROOT / "tools" / "bootstrap_phase1.py"), "--verify"])
    build_userspace()
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

    u_out_dir = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release"
    mods = USERSPACE_MODS

    conf_path = BUILD_DIR / "limine.conf"
    mod_lines = "".join(f"    module_path: boot():/boot/{dst}\n" for _, dst in mods)
    conf_text = (
        "timeout: 0\n"
        "serial: yes\n"
        "\n"
        "/DRY OS (Phase 1)\n"
        "    protocol: limine\n"
        "    path: boot():/boot/dry-kernel\n"
        f"    cmdline: {cmdline}\n"
        f"{mod_lines}"
    )
    conf_path.write_text(conf_text, encoding="utf-8")

    subprocess.check_call(["mcopy", "-i", part_spec, str(LIMINE_EFI), "::/EFI/BOOT/BOOTX64.EFI"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(conf_path), "::/boot/limine/limine.conf"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(kernel_elf), "::/boot/dry-kernel"], env=mtools_env)
    for src_name, dst_name in mods:
        subprocess.check_call(
            ["mcopy", "-i", part_spec, str(u_out_dir / src_name), f"::/boot/{dst_name}"],
            env=mtools_env,
        )


def run_qemu(
    name: str,
    img_path: pathlib.Path,
    smp: int,
    timeout_sec: int = 25,
    extra_args: list[str] | None = None,
) -> tuple[int, str, float]:
    vars_copy = BUILD_DIR / f"OVMF_VARS_{name}.4m.fd"
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
    if extra_args:
        cmd.extend(extra_args)
    t0 = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            timeout=timeout_sec,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        out_str = (
            exc.stdout.decode("utf-8", errors="replace")
            if isinstance(exc.stdout, bytes)
            else (exc.stdout or "")
        )
        print(
            f"\n[QEMU-TIMEOUT] {name} (smp={smp}) timed out after {timeout_sec}s! Partial serial output:\n{out_str}",
            file=sys.stderr,
        )
        raise
    elapsed = time.monotonic() - t0
    return proc.returncode, proc.stdout, elapsed


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
    rc, serial_out, elapsed = run_qemu(name, img_path, smp=smp)

    print(f"\n--- [{name}] smp={smp} QEMU exit={rc} (expected {expected_rc}) elapsed={elapsed:.3f}s ---")
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Hello from DRY OS!" in line or "Limine" in line or "PANIC" in line:
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
    print(f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions verified in {elapsed:.3f}s).")


def assert_positive_smp_boot(
    name: str,
    kernel_elf: pathlib.Path,
    smp: int,
    cmdline: str = "console=com1",
    irq_only: bool = False,
    expected_apic_mode: str | None = None,
    expect_fb: bool = True,
    extra_qemu_args: list[str] | None = None,
) -> None:
    expected_mask = hex((1 << smp) - 1)
    expected_ops = smp * 4096
    spawned, park_rounds, work_units = expected_sched_metrics(smp)
    img_path = BUILD_DIR / f"esp_{name}.img"
    write_mbr_esp_image(img_path, kernel_elf, cmdline)
    rc, serial_out, elapsed = run_qemu(
        name,
        img_path,
        smp=smp,
        timeout_sec=75 if smp > 8 else 50,
        extra_args=extra_qemu_args,
    )

    print(f"\n--- [{name}] smp={smp} QEMU exit={rc} (expected {EXIT_CODE_SUCCESS}) elapsed={elapsed:.3f}s ---")
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Hello from DRY OS!" in line or "Limine" in line or "PANIC" in line:
            print(f"  {line}")

    must_contain = [
        "[DRY-BOOT] DRY Kernel v0.1.0",
        "Firmware: EFI64",
        "[DRY-CPU] PASS: BSP(cpu=0,",
        "GDT(CS=0x08,TR=0x28,Busy=0x8b)",
        "#BP(hits=1,",
        "[DRY-HHDM] PASS:",
        "[DRY-MEMMAP] PASS:",
        "[DRY-SEC] PASS:",
        f"[DRY-SMP] PASS: cpus_online={smp} mask={expected_mask} total_bp_traps={smp} symmetric_ops={expected_ops} stacks_disjoint=true",
        f"[DRY-SCHED] PASS: cpus={smp} spawned={spawned} exited={spawned}",
        f"park_rounds={park_rounds} work_units={work_units} stale_wakes_rejected=48 orphan_ready=0",
        "[DRY-APIC] PASS:",
        f"[DRY-IRQ-ONLY] PASS: cpus={smp}",
    ]
    if not irq_only:
        must_contain.extend(
            [
                f"[DRY-PREEMPT] PASS: cpus={smp}",
                "[DRY-PMM] PASS:",
                "[DRY-UNTYPED] PASS:",
                f"[DRY-PMM-BENCH] PASS: cpus={smp}",
                f"[DRY-LIVENESS-REG] PASS: cpus={smp}",
                "[DRY-VSPACE] PASS:",
                "[DRY-HUGEPAGE] PASS:",
                f"[DRY-VSPACE-BENCH] PASS: cpus={smp}",
                f"[DRY-VSPACE-SMP] PASS: cpus={smp}",
                f"[DRY-TLB-DEBT] PASS: cpus={smp}",
                f"[DRY-CSPACE] PASS: cpus={smp}",
                f"[DRY-CSPACE-BENCH] PASS: cpus={smp}",
                f"[DRY-CSPACE-SMP] PASS: cpus={smp}",
                "Hello from DRY OS!",
                f"[DRY-USER-SEC] PASS: cpus={smp}",
                f"[DRY-USER-BENCH] PASS: cpus={smp}",
                f"[DRY-SERVICES] PASS: cpus={smp}",
                f"[DRY-IPC-SEC] PASS: cpus={smp}",
                f"[DRY-IPC-BENCH] PASS: cpus={smp}",
                f"[DRY-IRQ] PASS: cpus={smp}",
                f"[DRY-IRQ-BENCH] PASS: cpus={smp}",
                f"[DRY-BOOT-OK] Milestone 1C UEFI boot + {smp}-vCPU SMP + H1 scheduler + PMM + VSpace + CSpace + Ring 3 Services, IPC & GOP Display verified cleanly.",
            ]
        )
    must_not_contain = [
        "[DRY-PANIC]",
        "[DRY-EXCEPTION]",
        "[DRY-SMP-FAIL]",
        "[DRY-SCHED-FAIL]",
        "[DRY-PREEMPT-FAIL]",
        "[DRY-PMM-FAIL]",
        "[DRY-VSPACE-FAIL]",
        "[DRY-TLB-FAIL]",
        "[DRY-CSPACE-FAIL]",
        "[DRY-USER-FAIL]",
        "[DRY-IPC-FAIL]",
        "[DRY-IRQ-FAIL]",
        "[DRY-APIC-FAIL]",
        "[DRY-DF-IST1] FAIL",
    ]

    if rc != EXIT_CODE_SUCCESS:
        print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
        raise RuntimeError(f"[{name}] Unexpected QEMU exit code {rc} != {EXIT_CODE_SUCCESS}")
    for token in must_contain:
        if token not in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Missing required serial marker: {token!r}")
    for token in must_not_contain:
        if token in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Forbidden serial marker found: {token!r}")
    verify_sched_line(name, serial_out, smp)
    verify_apic_and_irq_lines(name, serial_out, smp, expected_mode=expected_apic_mode)
    if not irq_only:
        verify_preempt_line(name, serial_out, smp)
        verify_pmm_and_untyped_lines(name, serial_out, smp)
        verify_vspace_lines(name, serial_out, smp)
        verify_cspace_and_tlb_debt_lines(name, serial_out, smp)
        verify_userspace_lines(name, serial_out, smp)
        verify_ipc_and_display_lines(name, serial_out, smp, expect_fb=expect_fb)
        verify_p1_irq_lines(name, serial_out, smp)
    print(
        f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions + numeric sched/preempt/pmm/vspace/cspace/user/ipc/irq/display invariants verified in {elapsed:.3f}s)."
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--build-only", action="store_true", help="Run clippy + release build without QEMU")
    parser.add_argument("--smp-only", type=int, default=0, help="Run only a single positive N-vCPU boot test")
    parser.add_argument("--irq-only", action="store_true", help="Sub-Stage A: verify only APIC timer IRQ + EOI + TrapFrame")
    parser.add_argument("--p1-only", action="store_true", help="Run only Milestone 2A-1 (P1) interactive & negative security suite")
    parser.add_argument("--p3-only", action="store_true", help="Run only Milestone 2A-3 native desktop & lifecycle suite")
    parser.add_argument("--p3-matrix", action="store_true", help="Run Milestone 2A-3 SMP (1..20 vCPU) & RAM (256M..2G) desktop matrix")
    parser.add_argument("--p4-only", action="store_true", help="Run only Milestone 2B persistent storage & DRY Vault suite")
    parser.add_argument("--island-only", action="store_true", help="Run only Milestone 2C-0 Adaptive Driver Islands PoC suite")
    args = parser.parse_args()

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    kernel_elf = build_kernel()
    if args.build_only:
        print(f"[QEMU-TEST] Build-only check succeeded: {kernel_elf}")
        return

    if args.p1_only:
        run_p1_suite(
            kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, assert_boot_case, EXIT_CODE_PANIC
        )
        print("\n[QEMU-TEST] ALL 9 MILESTONE 2A-1 (P1) INTERACTIVE HARDWARE E2E & SECURITY CHECKS PASSED.")
        return

    if args.p3_only:
        n_p3 = run_p3_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p3} MILESTONE 2A-3 NATIVE DESKTOP & SECURITY CHECKS PASSED.")
        return

    if args.p4_only:
        n_p4 = run_p4_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p4} MILESTONE 2B PERSISTENT STORAGE & DRY VAULT CHECKS PASSED.")
        return

    if args.island_only:
        n_p5 = run_p5_island_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p5} MILESTONE 2C-0 ADAPTIVE DRIVER ISLANDS POC CHECKS PASSED.")
        return

    if args.p3_matrix:
        esp_p3_std = BUILD_DIR / "esp_p3_desktop_std.img"
        write_mbr_esp_image(esp_p3_std, kernel_elf, "console=com1 desktop3=1")
        run_p3_hardware_matrix(esp_p3_std, OVMF_CODE, OVMF_VARS, BUILD_DIR)
        print("\n[QEMU-TEST] ALL 5 MILESTONE 2A-3 SMP/RAM MATRIX DESKTOP CONFIGURATIONS PASSED.")
        return

    if args.smp_only > 0:
        cmdline = "console=com1 preempt_stage=irq_only" if args.irq_only else "console=com1"
        assert_positive_smp_boot(
            f"boot_{args.smp_only}cpu_ok",
            kernel_elf,
            args.smp_only,
            cmdline=cmdline,
            irq_only=args.irq_only,
        )
        return

    # Progressive SMP bring-up order (ADR-006 / Milestone 1A-E..1B-C): 1 -> 2 -> 4 -> 8 -> 20 vCPUs.
    assert_positive_smp_boot("boot_1cpu_ok", kernel_elf, smp=1)
    assert_positive_smp_boot("boot_2cpu_ok", kernel_elf, smp=2)
    assert_positive_smp_boot("boot_4cpu_ok", kernel_elf, smp=4)
    assert_positive_smp_boot("boot_8cpu_ok", kernel_elf, smp=8)
    assert_positive_smp_boot("boot_20cpu_ok", kernel_elf, smp=20)

    # Explicit hardware fallback verification: force xAPIC MMIO fallback (`0xFEE0_0000` UC via HHDM) on 2 vCPUs.
    assert_positive_smp_boot(
        "boot_2cpu_xapic_fallback",
        kernel_elf,
        smp=2,
        cmdline="console=com1 apic_mode=xapic",
        expected_apic_mode="xapic-mmio",
    )

    # Headless fallback verification (`Stage E.4`): boot without GOP (`-vga none`) and confirm graceful serial-only boot.
    assert_positive_smp_boot(
        "boot_2cpu_headless_nofb",
        kernel_elf,
        smp=2,
        cmdline="console=com1",
        expect_fb=False,
        extra_qemu_args=["-vga", "none"],
    )

    # Ring 3 GOP visual proof (`Stage E.2 & E.3`): QMP screendump -> PNG + pixel-level contrast verification.
    screendump_esp = BUILD_DIR / "esp_boot_display_screendump.img"
    write_mbr_esp_image(screendump_esp, kernel_elf, "console=com1 screendump_hold=1")
    capture_and_verify_gop_screendump(
        img_path=screendump_esp,
        ovmf_code=OVMF_CODE,
        ovmf_vars=OVMF_VARS,
        build_dir=BUILD_DIR,
        png_out=BUILD_DIR / "dry_os_first_screen.png",
    )

    # Negative SMP & hardware fault isolation tests (must fail deterministically with exit=35).
    assert_boot_case(
        name="smp_ap_timeout",
        kernel_elf=kernel_elf,
        cmdline="console=com1 smp_fault=ap_timeout",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SMP-FAIL] AP timeout: online=1/2 online_mask=0x1 missing_mask=0x2",
            "[DRY-PANIC] cpu=0",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="smp_dup_cpu_id",
        kernel_elf=kernel_elf,
        cmdline="console=com1 smp_fault=dup_cpu_id",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SMP-FAIL] Duplicate CPU ID detected: cpu_idx=0",
            "[DRY-PANIC] cpu=1",
        ],
        must_not_contain=["[DRY-BOOT-OK]", "AP timeout"],
    )

    assert_boot_case(
        name="smp_stack_overlap",
        kernel_elf=kernel_elf,
        cmdline="console=com1 smp_fault=stack_overlap",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SMP-FAIL] Stack/LAPIC conflict (stack_overlap) between cpu=1 and cpu=0",
            "[DRY-PANIC] cpu=1",
        ],
        must_not_contain=["[DRY-BOOT-OK]", "AP timeout"],
    )

    assert_boot_case(
        name="smp_df_ist1_ap",
        kernel_elf=kernel_elf,
        cmdline="console=com1 smp_fault=df_ist1_ap",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SMP-INFO] Triggering intentional hardware #PF->#DF on AP 1 IST1...",
            "[DRY-DF-IST1] PASS: hardware #DF caught on cpu=1",
            "on private IST1 rsp=",
            "fault_rsp=0x7fffdead0008",
            "[DRY-PANIC] cpu=1",
        ],
        must_not_contain=["[DRY-BOOT-OK]", "[DRY-DF-IST1] FAIL"],
    )

    # Negative H1 Scheduler Bug-Class Detection Tests (must fail deterministically with exit=35).
    sched_negative_cases = [
        ("sched_stack_owner_conflict", "stack_owner", "[DRY-SCHED-FAIL] H1 Bug1 stack_owner violation"),
        ("sched_orphan_ready_detected", "orphan_ready", "[DRY-SCHED-FAIL] H1 Bug2 orphaned Ready thread detected"),
        ("sched_lost_wakeup_unarmed", "lost_wakeup", "[DRY-SCHED-FAIL] H1 Bug3 invalid park protocol state"),
        ("sched_exit_resurrection", "exit_resurrect", "[DRY-SCHED-FAIL] H1 Bug4 exiting/dead thread resurrection rejected"),
    ]
    for case_name, fault_arg, expected_fail in sched_negative_cases:
        assert_boot_case(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 sched_fault={fault_arg}",
            smp=2,
            expected_rc=EXIT_CODE_PANIC,
            must_contain=[expected_fail, "[DRY-PANIC]"],
            must_not_contain=["[DRY-BOOT-OK]"],
        )

    # Milestone 1A-D 1-CPU exception & panic regression suite.
    assert_boot_case(
        name="panic_handler_deterministic",
        kernel_elf=kernel_elf,
        cmdline="console=com1 panic_self_test=1",
        smp=1,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-BOOT] DRY Kernel v0.1.0",
            "[DRY-PANIC] cpu=0",
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
            "[DRY-EXCEPTION] cpu=0 vec=6 (#UD Invalid-Opcode)",
            "[DRY-PANIC] cpu=0",
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
            "[DRY-EXCEPTION] cpu=0 vec=14 (#PF Page-Fault)",
            "cr2=0x7fffdeadbee0",
            "[DRY-PANIC] cpu=0",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    # Milestone 1A-G Negative Preemptive Scheduler & APIC Timer Fault Isolation Tests (exit=35).
    preempt_negative_cases = [
        ("preempt_irq_storm", "irq_storm", "[DRY-PREEMPT-FAIL] Timer interrupt storm / zero-progress livelock detected"),
        ("preempt_nested_irq", "nested_irq", "[DRY-PREEMPT-FAIL] Nested timer interrupt violation (irq_depth=2)"),
        ("preempt_lost_wakeup", "lost_wakeup", "[DRY-PREEMPT-FAIL] Preemptive lost-wakeup protocol violation"),
        ("preempt_double_schedule", "double_sched", "[DRY-PREEMPT-FAIL] Double scheduling / duplicate runqueue enqueue detected"),
        ("preempt_stack_reuse", "stack_reuse", "[DRY-PREEMPT-FAIL] Premature stack reuse while stack_owner active"),
    ]
    for case_name, fault_arg, expected_fail in preempt_negative_cases:
        assert_boot_case(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 preempt_fault={fault_arg}",
            smp=2,
            expected_rc=EXIT_CODE_PANIC,
            must_contain=[expected_fail, "[DRY-PANIC]"],
            must_not_contain=["[DRY-BOOT-OK]"],
        )

    # Milestone 1B-A Negative Physical Memory Manager & Untyped Capability Fault Tests (exit=35).
    pmm_negative_cases = [
        ("pmm_double_free", "double_free", "[DRY-PMM-FAIL] Double free detected"),
        ("pmm_use_after_free", "uaf", "[DRY-PMM-FAIL] Use-after-free / stale generation detected"),
        ("pmm_double_alloc", "double_alloc", "[DRY-PMM-FAIL] Double allocation / corrupt free-state detected"),
        ("pmm_ownership_violation", "owner_mismatch", "[DRY-PMM-FAIL] Frame ownership / domain / pin violation"),
        ("pmm_untyped_revoke", "untyped_revoke", "[DRY-PMM-FAIL] Revoked Untyped / stale capability access rejected"),
    ]
    for case_name, fault_arg, expected_fail in pmm_negative_cases:
        assert_boot_case(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 pmm_fault={fault_arg}",
            smp=2,
            expected_rc=EXIT_CODE_PANIC,
            must_contain=[expected_fail, "[DRY-PANIC]"],
            must_not_contain=["[DRY-BOOT-OK]"],
        )

    # Milestone 1B-B Negative VSpace, Page Table, Hardware Permission & TLB Shootdown Fault Tests (exit=35).
    vspace_negative_cases: list[tuple[str, str, list[str]]] = [
        (
            "vspace_wrong_aspace",
            "wrong_aspace",
            [
                "[DRY-VSPACE-INFO] Testing wrong address-space hardware #PF",
                "[DRY-EXCEPTION]",
                "vec=14 (#PF Page-Fault)",
                "cr2=0x400000",
                "[DRY-PANIC]",
            ],
        ),
        (
            "vspace_us_bypass",
            "us_bypass",
            ["[DRY-VSPACE-FAIL] U/S permission bypass", "[DRY-PANIC]"],
        ),
        (
            "vspace_write_ro",
            "write_ro",
            [
                "[DRY-VSPACE-INFO] Testing write-to-readonly hardware #PF",
                "[DRY-EXCEPTION]",
                "vec=14 (#PF Page-Fault)",
                "cr2=0x500000",
                "[DRY-PANIC]",
            ],
        ),
        (
            "vspace_exec_nx",
            "exec_nx",
            [
                "[DRY-VSPACE-INFO] Testing NX instruction-fetch hardware #PF",
                "[DRY-EXCEPTION]",
                "vec=14 (#PF Page-Fault)",
                "cr2=0x501000",
                "[DRY-PANIC]",
            ],
        ),
        (
            "vspace_double_map",
            "double_map",
            ["[DRY-VSPACE-FAIL] Double map detected", "[DRY-PANIC]"],
        ),
        (
            "vspace_double_unmap",
            "double_unmap",
            ["[DRY-VSPACE-FAIL] Double unmap detected", "[DRY-PANIC]"],
        ),
        (
            "vspace_stale_tlb",
            "stale_tlb",
            ["[DRY-TLB-FAIL] Stale TLB hazard", "[DRY-PANIC]"],
        ),
        (
            "vspace_shootdown_timeout",
            "shootdown_timeout",
            ["[DRY-TLB-FAIL] Shootdown timeout", "[DRY-PANIC]"],
        ),
        (
            "vspace_active_frame_reuse",
            "active_frame_reuse",
            ["[DRY-PMM-FAIL] Frame ownership / domain / pin violation", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in vspace_negative_cases:
        assert_boot_case(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 vspace_fault={fault_arg}",
            smp=2,
            expected_rc=EXIT_CODE_PANIC,
            must_contain=expected_tokens,
            must_not_contain=["[DRY-BOOT-OK]"],
        )

    # Milestone 1B-C Negative CSpace, Capability Lifecycle & ADR-007 Security Fault Tests (exit=35).
    cspace_negative_cases: list[tuple[str, str, list[str]]] = [
        (
            "cspace_forged_handle",
            "forged_handle",
            ["[DRY-CSPACE-FAIL] Forged capability handle rejected", "[DRY-PANIC]"],
        ),
        (
            "cspace_stale_aba",
            "stale_aba",
            ["[DRY-CSPACE-FAIL] Stale generation / ABA capability access rejected", "[DRY-PANIC]"],
        ),
        (
            "cspace_rights_amp",
            "rights_amp",
            ["[DRY-CSPACE-FAIL] Rights amplification rejected", "[DRY-PANIC]"],
        ),
        (
            "cspace_cross_domain",
            "cross_domain",
            ["[DRY-CSPACE-FAIL] Cross-domain unauthorized access rejected", "[DRY-PANIC]"],
        ),
        (
            "cspace_concurrent_revoke",
            "concurrent_revoke",
            ["[DRY-CSPACE-FAIL] Concurrent lookup on revoked capability rejected", "[DRY-PANIC]"],
        ),
        (
            "cspace_stale_mapping_after_revoke",
            "stale_mapping",
            [
                "[DRY-CSPACE-INFO] Testing post-revoke stale VSpace mapping hardware #PF",
                "[DRY-EXCEPTION]",
                "vec=14 (#PF Page-Fault)",
                "cr2=0x700000",
                "[DRY-PANIC]",
            ],
        ),
        (
            "cspace_parent_child_lifetime",
            "parent_child_lifetime",
            ["[DRY-CSPACE-FAIL] Parent-child capability lifetime violation rejected", "[DRY-PANIC]"],
        ),
        (
            "cspace_transfer_escalation",
            "transfer_escalation",
            ["[DRY-CSPACE-FAIL] Capability transfer privilege escalation rejected", "[DRY-PANIC]"],
        ),
        (
            "cspace_double_destroy_revoke",
            "double_destroy",
            ["[DRY-CSPACE-FAIL] Double destroy / double revoke detected", "[DRY-PANIC]"],
        ),
        (
            "cspace_premature_active_reclaim",
            "active_reclaim",
            ["[DRY-CSPACE-FAIL] Premature reclamation of active TCB/Frame blocked", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in cspace_negative_cases:
        assert_boot_case(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 cspace_fault={fault_arg}",
            smp=2,
            expected_rc=EXIT_CODE_PANIC,
            must_contain=expected_tokens,
            must_not_contain=["[DRY-BOOT-OK]"],
        )

    # Milestone 1B-D Negative Ring 3 / SMAP / ELF64 W^X / Active Process Reclaim Fault Tests (exit=35).
    user_negative_cases: list[tuple[str, str, list[str]]] = [
        (
            "user_kernel_pf_not_masked",
            "kernel_pf_not_masked",
            [
                "[DRY-USER-INFO] Triggering unmapped Ring 0 #PF outside uaccess extable",
                "[DRY-EXCEPTION]",
                "vec=14 (#PF Page-Fault)",
                "cs=0x8",
                "[DRY-PANIC]",
            ],
        ),
        (
            "user_smap_ring0_bypass",
            "smap_ring0_bypass",
            [
                "[DRY-USER-INFO] Attempting raw Ring 0 read of USER_RW 0x600000 with CR4.SMAP=1 (AC=0)",
                "[DRY-EXCEPTION]",
                "vec=14 (#PF Page-Fault)",
                "cr2=0x600000",
                "[DRY-PANIC]",
            ],
        ),
        (
            "user_wx_elf_segment",
            "wx_elf_segment",
            [
                "[DRY-USER-FAIL] W^X ELF64 segment rejected at load time: err=Some(Elf(WxViolation))",
                "[DRY-PANIC]",
            ],
        ),
        (
            "user_active_proc_reclaim",
            "active_proc_reclaim",
            [
                "[DRY-USER-FAIL] Premature reclamation of active process rejected",
                "[DRY-PANIC]",
            ],
        ),
    ]
    for case_name, fault_arg, expected_tokens in user_negative_cases:
        assert_boot_case(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 user_fault={fault_arg}",
            smp=2,
            expected_rc=EXIT_CODE_PANIC,
            must_contain=expected_tokens,
            must_not_contain=["[DRY-BOOT-OK]"],
        )

    # Milestone 1C Negative Capability IPC, Priority Donation & Framebuffer Isolation Tests (exit=35).
    ipc_negative_cases: list[tuple[str, str, list[str]]] = [
        (
            "ipc_irq_block_rejected",
            "irq_block",
            ["[DRY-IPC-FAIL] Blocking IPC in preempt-disabled/IRQ context rejected: err=IrqForbidden", "[DRY-PANIC]"],
        ),
        (
            "ipc_unauth_fb_rejected",
            "unauth_fb",
            ["[DRY-IPC-FAIL] Unauthorized framebuffer mapping rejected", "[DRY-PANIC]"],
        ),
        (
            "ipc_forged_ep_rejected",
            "forged_ep",
            ["[DRY-IPC-FAIL] Forged endpoint capability rejected", "[DRY-PANIC]"],
        ),
        (
            "ipc_donation_cycle_rejected",
            "donation_cycle",
            ["[DRY-IPC-FAIL] Priority donation cycle rejected: err=Some(CycleDetected)", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in ipc_negative_cases:
        assert_boot_case(
            name=case_name,
            kernel_elf=kernel_elf,
            cmdline=f"console=com1 ipc_fault={fault_arg}",
            smp=2,
            expected_rc=EXIT_CODE_PANIC,
            must_contain=expected_tokens,
            must_not_contain=["[DRY-BOOT-OK]"],
        )

    run_p1_suite(
        kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, assert_boot_case, EXIT_CODE_PANIC
    )
    run_p2_suite(
        kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, assert_boot_case, EXIT_CODE_PANIC
    )
    n_p3 = run_p3_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p4 = run_p4_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p5 = run_p5_island_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)

    print(
        f"\n[QEMU-TEST] ALL {79 + n_p3 + n_p4 + n_p5} DETERMINISTIC QEMU CHECKS "
        f"(56 LEGACY MILESTONE 0-1C + 9 MILESTONE 2A-1 P1 + 14 MILESTONE 2A-2 + "
        f"{n_p3} MILESTONE 2A-3 + {n_p4} MILESTONE 2B + {n_p5} MILESTONE 2C-0) PASSED."
    )


if __name__ == "__main__":
    main()
