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


def expected_sched_metrics(smp: int) -> tuple[int, int, int]:
    n_yield = max(8, min(24, smp * 2))
    n_pairs = max(4, min(12, smp))
    n_recycle = 48
    total_spawned = n_yield + (2 * n_pairs) + n_recycle
    park_rounds = n_pairs * 32
    work_units = (n_yield * 32) + (n_pairs * 64) + (n_recycle * 8)
    return total_spawned, park_rounds, work_units


def verify_sched_line(name: str, serial_out: str, smp: int) -> None:
    sched_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-SCHED] PASS:" in ln]
    if len(sched_lines) != 1:
        raise RuntimeError(f"[{name}] Expected exactly 1 [DRY-SCHED] PASS line, got {len(sched_lines)}")
    fields: dict[str, int] = {}
    for tok in sched_lines[0].split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if v.isdigit():
                fields[k] = int(v)
    if fields.get("recycled_slots", 0) < 28:
        raise RuntimeError(f"[{name}] Expected recycled_slots >= 28, got {fields}")
    if fields.get("ctx_switches", 0) < 400:
        raise RuntimeError(f"[{name}] Expected ctx_switches >= 400, got {fields}")
    if smp >= 2 and fields.get("migrations", 0) < 1:
        raise RuntimeError(f"[{name}] Expected cross-CPU migrations >= 1 on smp={smp}, got {fields}")


def verify_apic_and_irq_lines(
    name: str, serial_out: str, smp: int, expected_mode: str | None = None
) -> None:
    apic_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-APIC] PASS:" in ln]
    if len(apic_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-APIC] PASS line, got {len(apic_lines)}")
    if expected_mode and f"mode={expected_mode}" not in apic_lines[0]:
        raise RuntimeError(f"[{name}] Expected mode={expected_mode} in {apic_lines[0]!r}")

    apic_kv: dict[str, int] = {}
    for tok in apic_lines[0].split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if v.isdigit():
                apic_kv[k] = int(v)
    if apic_kv.get("ticks_per_ms", 0) < 100:
        raise RuntimeError(f"[{name}] Invalid calibrated ticks_per_ms in {apic_lines[0]!r}")
    if apic_kv.get("tsc_mhz", 0) < 100:
        raise RuntimeError(f"[{name}] Invalid calibrated tsc_mhz in {apic_lines[0]!r}")

    irq_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-IRQ-ONLY] PASS:" in ln]
    if len(irq_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-IRQ-ONLY] PASS line, got {len(irq_lines)}")
    irq_kv: dict[str, int] = {}
    for tok in irq_lines[0].split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if v.isdigit():
                irq_kv[k] = int(v)
    if irq_kv.get("cpus") != smp:
        raise RuntimeError(f"[{name}] IRQ-only cpus mismatch: {irq_lines[0]!r}")
    if irq_kv.get("min_cpu_irqs", 0) < 8 or irq_kv.get("total_timer_irqs", 0) < smp * 8:
        raise RuntimeError(f"[{name}] Insufficient per-CPU timer IRQs in {irq_lines[0]!r}")
    if "eoi_verified=true" not in irq_lines[0] or "trapframe_gpr_rflags_ok=true" not in irq_lines[0]:
        raise RuntimeError(f"[{name}] Missing EOI/TrapFrame verification in {irq_lines[0]!r}")


def verify_preempt_line(name: str, serial_out: str, smp: int) -> None:
    p_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-PREEMPT] PASS:" in ln]
    if len(p_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-PREEMPT] PASS line, got {len(p_lines)}")
    fields: dict[str, int] = {}
    for tok in p_lines[0].split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if v.isdigit():
                fields[k] = int(v)
    if fields.get("cpus") != smp:
        raise RuntimeError(f"[{name}] Preempt cpus mismatch: {fields}")
    if fields.get("timer_irqs", 0) < smp * 16:
        raise RuntimeError(f"[{name}] Expected timer_irqs >= {smp * 16}, got {fields}")
    if fields.get("preemptions", 0) < 4:
        raise RuntimeError(f"[{name}] Expected involuntary preemptions >= 4, got {fields}")
    if fields.get("deferred_rescheds", 0) < 1:
        raise RuntimeError(f"[{name}] Expected deferred_rescheds >= 1, got {fields}")
    if fields.get("spinner_preempts", 0) < 4:
        raise RuntimeError(f"[{name}] Expected spinner_preempts >= 4, got {fields}")
    if fields.get("race_recycled", 0) < 12:
        raise RuntimeError(f"[{name}] Expected race_recycled >= 12, got {fields}")
    if fields.get("stale_wakes_rejected") != 48 or fields.get("orphan_ready") != 0:
        raise RuntimeError(f"[{name}] Preempt invariant mismatch: {fields}")
    if smp >= 2 and fields.get("race_migrations", 0) < 1:
        raise RuntimeError(f"[{name}] Expected preemptive race_migrations >= 1 on smp={smp}, got {fields}")


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
    rc, serial_out, elapsed = run_qemu(name, img_path, smp=smp, timeout_sec=35)

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
                f"[DRY-BOOT-OK] Milestone 1A-G UEFI boot + {smp}-vCPU SMP + cooperative & preemptive H1 scheduler verified cleanly.",
            ]
        )
    must_not_contain = [
        "[DRY-PANIC]",
        "[DRY-EXCEPTION]",
        "[DRY-SMP-FAIL]",
        "[DRY-SCHED-FAIL]",
        "[DRY-PREEMPT-FAIL]",
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
    print(f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions + numeric sched/preempt invariants verified in {elapsed:.3f}s).")


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

    # Progressive SMP bring-up order (ADR-006 / Milestone 1A-E/F/G): 1 -> 2 -> 4 -> 20 vCPUs.
    assert_positive_smp_boot("boot_1cpu_ok", kernel_elf, smp=1)
    assert_positive_smp_boot("boot_2cpu_ok", kernel_elf, smp=2)
    assert_positive_smp_boot("boot_4cpu_ok", kernel_elf, smp=4)
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
    assert_boot_case(
        name="sched_stack_owner_conflict",
        kernel_elf=kernel_elf,
        cmdline="console=com1 sched_fault=stack_owner",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SCHED-FAIL] H1 Bug1 stack_owner violation",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="sched_orphan_ready_detected",
        kernel_elf=kernel_elf,
        cmdline="console=com1 sched_fault=orphan_ready",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SCHED-FAIL] H1 Bug2 orphaned Ready thread detected",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="sched_lost_wakeup_unarmed",
        kernel_elf=kernel_elf,
        cmdline="console=com1 sched_fault=lost_wakeup",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SCHED-FAIL] H1 Bug3 invalid park protocol state",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="sched_exit_resurrection",
        kernel_elf=kernel_elf,
        cmdline="console=com1 sched_fault=exit_resurrect",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-SCHED-FAIL] H1 Bug4 exiting/dead thread resurrection rejected",
            "[DRY-PANIC]",
        ],
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
    assert_boot_case(
        name="preempt_irq_storm",
        kernel_elf=kernel_elf,
        cmdline="console=com1 preempt_fault=irq_storm",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-PREEMPT-FAIL] Timer interrupt storm / zero-progress livelock detected",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="preempt_nested_irq",
        kernel_elf=kernel_elf,
        cmdline="console=com1 preempt_fault=nested_irq",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-PREEMPT-FAIL] Nested timer interrupt violation (irq_depth=2)",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="preempt_lost_wakeup",
        kernel_elf=kernel_elf,
        cmdline="console=com1 preempt_fault=lost_wakeup",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-PREEMPT-FAIL] Preemptive lost-wakeup protocol violation",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="preempt_double_schedule",
        kernel_elf=kernel_elf,
        cmdline="console=com1 preempt_fault=double_sched",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-PREEMPT-FAIL] Double scheduling / duplicate runqueue enqueue detected",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    assert_boot_case(
        name="preempt_stack_reuse",
        kernel_elf=kernel_elf,
        cmdline="console=com1 preempt_fault=stack_reuse",
        smp=2,
        expected_rc=EXIT_CODE_PANIC,
        must_contain=[
            "[DRY-PREEMPT-FAIL] Premature stack reuse while stack_owner active",
            "[DRY-PANIC]",
        ],
        must_not_contain=["[DRY-BOOT-OK]"],
    )

    print("\n[QEMU-TEST] ALL 21 MILESTONE 1A-G SMP, COOPERATIVE + PREEMPTIVE SCHEDULER & FAULT CHECKS PASSED.")


if __name__ == "__main__":
    main()
