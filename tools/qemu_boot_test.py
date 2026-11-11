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


def run_qemu(name: str, img_path: pathlib.Path, smp: int, timeout_sec: int = 25) -> tuple[int, str, float]:
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
    print(f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions verified in {elapsed:.3f}s).")


def assert_positive_smp_boot(
    name: str,
    kernel_elf: pathlib.Path,
    smp: int,
    cmdline: str = "console=com1",
    irq_only: bool = False,
    expected_apic_mode: str | None = None,
) -> None:
    expected_mask = hex((1 << smp) - 1)
    expected_ops = smp * 4096
    spawned, park_rounds, work_units = expected_sched_metrics(smp)
    img_path = BUILD_DIR / f"esp_{name}.img"
    write_mbr_esp_image(img_path, kernel_elf, cmdline)
    rc, serial_out, elapsed = run_qemu(name, img_path, smp=smp, timeout_sec=60 if smp > 8 else 40)

    print(f"\n--- [{name}] smp={smp} QEMU exit={rc} (expected {EXIT_CODE_SUCCESS}) elapsed={elapsed:.3f}s ---")
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Limine" in line or "PANIC" in line:
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
                f"[DRY-BOOT-OK] Milestone 1B-C UEFI boot + {smp}-vCPU SMP + H1 scheduler + PMM + VSpace + CSpace & ADR-007 verified cleanly.",
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
    print(
        f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions + numeric sched/preempt/pmm/vspace/cspace invariants verified in {elapsed:.3f}s)."
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--build-only", action="store_true", help="Run clippy + release build without QEMU")
    parser.add_argument("--smp-only", type=int, default=0, help="Run only a single positive N-vCPU boot test")
    parser.add_argument("--irq-only", action="store_true", help="Sub-Stage A: verify only APIC timer IRQ + EOI + TrapFrame")
    args = parser.parse_args()

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    kernel_elf = build_kernel()
    if args.build_only:
        print(f"[QEMU-TEST] Build-only check succeeded: {kernel_elf}")
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

    print("\n[QEMU-TEST] ALL 46 MILESTONE 1B-C SMP, H1 SCHEDULER, PMM, VSPACE, CSPACE & ADR-007 CHECKS PASSED.")


if __name__ == "__main__":
    main()
