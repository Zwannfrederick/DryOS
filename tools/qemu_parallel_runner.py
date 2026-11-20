#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""QEMU runner helpers and parallel `-j` / `--jobs` regression orchestrator."""

from __future__ import annotations

import concurrent.futures
import os
import pathlib
import shutil
import subprocess
import sys
import threading
import time
from typing import Any, Callable

from bootstrap_qemu_gpu_modules import ensure_qemu_virtio_gpu_modules
from qemu_ipc_validators import capture_and_verify_gop_screendump, verify_ipc_and_display_lines
from qemu_log_validators import (
    expected_sched_metrics,
    verify_apic_and_irq_lines,
    verify_cspace_and_tlb_debt_lines,
    verify_pmm_and_untyped_lines,
    verify_preempt_line,
    verify_sched_line,
    verify_vspace_lines,
)
from qemu_user_validators import verify_userspace_lines

ROOT = pathlib.Path(__file__).resolve().parents[1]
BUILD_DIR = ROOT / "kernel" / "build"
OVMF_CODE = pathlib.Path("/usr/share/edk2/x64/OVMF_CODE.4m.fd")
OVMF_VARS = pathlib.Path("/usr/share/edk2/x64/OVMF_VARS.4m.fd")
EXIT_CODE_SUCCESS = (0x10 << 1) | 1  # 33

_PRINT_LOCK = threading.Lock()
_ESP_WRITER: Callable[..., None] | None = None


def bind_esp_writer(fn: Callable[..., None]) -> None:
    """Bind the shared ESP image builder (`write_mbr_esp_image`) from `qemu_boot_test`."""
    global _ESP_WRITER
    _ESP_WRITER = fn


def _write_blocking_safe(stream: Any, data: str) -> None:
    pos = 0
    n = len(data)
    while pos < n:
        chunk = data[pos : pos + 1024]
        try:
            written = stream.write(chunk)
            pos += written if isinstance(written, int) and written > 0 else len(chunk)
        except BlockingIOError:
            time.sleep(0.005)
    while True:
        try:
            stream.flush()
            break
        except BlockingIOError:
            time.sleep(0.005)


def _safe_print(text: str, *, err: bool = False) -> None:
    stream = sys.stderr if err else sys.stdout
    with _PRINT_LOCK:
        _write_blocking_safe(stream, text + "\n")


def run_qemu(
    name: str,
    img_path: pathlib.Path,
    smp: int,
    timeout_sec: int = 25,
    extra_args: list[str] | None = None,
    mem: str = "512M",
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
        mem,
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
        _safe_print(
            f"\n[QEMU-TIMEOUT] {name} (smp={smp}) timed out after {timeout_sec}s! Partial serial output:\n{out_str}",
            err=True,
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
    if _ESP_WRITER is None:
        raise RuntimeError("qemu_parallel_runner.bind_esp_writer() has not been initialized")
    img_path = BUILD_DIR / f"esp_{name}.img"
    _ESP_WRITER(img_path, kernel_elf, cmdline)
    rc, serial_out, elapsed = run_qemu(name, img_path, smp=smp)

    lines = [f"\n--- [{name}] smp={smp} QEMU exit={rc} (expected {expected_rc}) elapsed={elapsed:.3f}s ---"]
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Hello from DRY OS!" in line or "Limine" in line or "PANIC" in line:
            lines.append(f"  {line}")
    _safe_print("\n".join(lines))

    if rc != expected_rc:
        _safe_print("FULL SERIAL OUTPUT:\n" + serial_out, err=True)
        raise RuntimeError(f"[{name}] Unexpected QEMU exit code {rc} != {expected_rc}")
    for token in must_contain:
        if token not in serial_out:
            _safe_print("FULL SERIAL OUTPUT:\n" + serial_out, err=True)
            raise RuntimeError(f"[{name}] Missing required serial marker: {token!r}")
    for token in must_not_contain:
        if token in serial_out:
            _safe_print("FULL SERIAL OUTPUT:\n" + serial_out, err=True)
            raise RuntimeError(f"[{name}] Forbidden serial marker found: {token!r}")
    _safe_print(f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions verified in {elapsed:.3f}s).")


def assert_positive_smp_boot(
    name: str,
    kernel_elf: pathlib.Path,
    smp: int,
    cmdline: str = "console=com1",
    irq_only: bool = False,
    expected_apic_mode: str | None = None,
    expect_fb: bool = True,
    extra_qemu_args: list[str] | None = None,
    mem: str = "512M",
) -> None:
    if _ESP_WRITER is None:
        raise RuntimeError("qemu_parallel_runner.bind_esp_writer() has not been initialized")
    expected_mask = hex((1 << smp) - 1)
    expected_ops = smp * 4096
    spawned, park_rounds, work_units = expected_sched_metrics(smp)
    img_path = BUILD_DIR / f"esp_{name}.img"
    _ESP_WRITER(img_path, kernel_elf, cmdline)
    rc, serial_out, elapsed = run_qemu(
        name,
        img_path,
        smp=smp,
        timeout_sec=75 if smp > 8 else 50,
        extra_args=extra_qemu_args,
        mem=mem,
    )

    lines = [f"\n--- [{name}] smp={smp} QEMU exit={rc} (expected {EXIT_CODE_SUCCESS}) elapsed={elapsed:.3f}s ---"]
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Hello from DRY OS!" in line or "Limine" in line or "PANIC" in line:
            lines.append(f"  {line}")
    _safe_print("\n".join(lines))

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
                f"[DRY-FPU] PASS: cpus={smp}",
                f"[DRY-AUDIT-REG] PASS: cpus={smp} cases=9/9",
                f"[DRY-FAULT-EP] PASS: cpus={smp} delivered=20 resumed=17",
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
        _safe_print("FULL SERIAL OUTPUT:\n" + serial_out, err=True)
        raise RuntimeError(f"[{name}] Unexpected QEMU exit code {rc} != {EXIT_CODE_SUCCESS}")
    for token in must_contain:
        if token not in serial_out:
            _safe_print("FULL SERIAL OUTPUT:\n" + serial_out, err=True)
            raise RuntimeError(f"[{name}] Missing required serial marker: {token!r}")
    for token in must_not_contain:
        if token in serial_out:
            _safe_print("FULL SERIAL OUTPUT:\n" + serial_out, err=True)
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
        verify_p1_irq_lines_lazy(name, serial_out, smp)
    _safe_print(
        f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions + numeric sched/preempt/pmm/vspace/cspace/user/ipc/irq/display invariants verified in {elapsed:.3f}s)."
    )


def verify_p1_irq_lines_lazy(name: str, serial_out: str, smp: int) -> None:
    from qemu_p1_validators import verify_p1_irq_lines

    verify_p1_irq_lines(name, serial_out, smp)


def _run_screendump_case(kernel_elf: pathlib.Path, write_esp_fn: Callable[..., None]) -> int:
    screendump_esp = BUILD_DIR / "esp_boot_display_screendump.img"
    write_esp_fn(screendump_esp, kernel_elf, "console=com1 screendump_hold=1")
    capture_and_verify_gop_screendump(
        img_path=screendump_esp,
        ovmf_code=OVMF_CODE,
        ovmf_vars=OVMF_VARS,
        build_dir=BUILD_DIR,
        png_out=BUILD_DIR / "dry_os_first_screen.png",
    )
    return 1


def run_full_regression(
    *,
    jobs: int,
    kernel_elf: pathlib.Path,
    mem: str,
    write_esp_fn: Callable[..., None],
    suites: dict[str, Callable[[], int]],
) -> None:
    """Run full regression either sequentially (`jobs <= 1`) or across `ThreadPoolExecutor`."""
    t_all = time.monotonic()
    bind_esp_writer(write_esp_fn)

    def _run_legacy() -> dict[str, int]:
        counts: dict[str, int] = {}
        for smp in (1, 2, 4, 8, 20):
            assert_positive_smp_boot(f"boot_{smp}cpu_ok", kernel_elf, smp=smp, mem=mem)
        assert_positive_smp_boot(
            "boot_2cpu_xapic_fallback",
            kernel_elf,
            smp=2,
            cmdline="console=com1 apic_mode=xapic",
            expected_apic_mode="xapic-mmio",
            mem=mem,
        )
        assert_boot_case(
            name="boot_2cpu_fpu_fxsave_fallback",
            kernel_elf=kernel_elf,
            cmdline="console=com1 fpu_mode=fxsave",
            smp=2,
            expected_rc=EXIT_CODE_SUCCESS,
            must_contain=["[DRY-FPU] PASS: cpus=2 mode=fxsave xcr0=0x3 area_bytes=512", "[DRY-BOOT-OK]"],
            must_not_contain=["[DRY-PANIC]", "[DRY-EXCEPTION]"],
        )
        assert_positive_smp_boot(
            "boot_2cpu_headless_nofb",
            kernel_elf,
            smp=2,
            cmdline="console=com1",
            expect_fb=False,
            extra_qemu_args=["-vga", "none"],
            mem=mem,
        )
        _run_screendump_case(kernel_elf, write_esp_fn)
        counts["legacy_boot"] = 10
        return counts

    results: dict[str, int] = {}
    if jobs <= 1:
        _run_legacy()
        for key, fn in suites.items():
            results[key] = fn()
    else:
        os.environ["QEMU_MODULE_DIR"] = str(ensure_qemu_virtio_gpu_modules())
        _safe_print(f"[QEMU-TEST] Running parallel regression with jobs={jobs}...")
        tasks: list[tuple[str, Callable[[], int]]] = [
            ("neg", suites["neg"]),
            ("p13", suites["p13"]),
            ("p18", suites["p18"]),
            ("boot_20cpu", lambda: (assert_positive_smp_boot("boot_20cpu_ok", kernel_elf, smp=20, mem=mem), 1)[1]),
            ("p19", suites["p19"]),
            ("p17", suites["p17"]),
            ("p2", suites["p2"]),
            ("p15", suites["p15"]),
            ("p10", suites["p10"]),
            ("p11", suites["p11"]),
            ("p9", suites["p9"]),
            ("p4", suites["p4"]),
            ("p3", suites["p3"]),
            ("p1", suites["p1"]),
            ("p16", suites["p16"]),
            ("p12", suites["p12"]),
            ("p5", suites["p5"]),
            ("p6", suites["p6"]),
            ("p7", suites["p7"]),
            ("p8", suites["p8"]),
            ("p14", suites["p14"]),
            ("boot_8cpu", lambda: (assert_positive_smp_boot("boot_8cpu_ok", kernel_elf, smp=8, mem=mem), 1)[1]),
            ("boot_4cpu", lambda: (assert_positive_smp_boot("boot_4cpu_ok", kernel_elf, smp=4, mem=mem), 1)[1]),
            ("boot_2cpu", lambda: (assert_positive_smp_boot("boot_2cpu_ok", kernel_elf, smp=2, mem=mem), 1)[1]),
            ("boot_1cpu", lambda: (assert_positive_smp_boot("boot_1cpu_ok", kernel_elf, smp=1, mem=mem), 1)[1]),
            (
                "boot_xapic",
                lambda: (
                    assert_positive_smp_boot(
                        "boot_2cpu_xapic_fallback",
                        kernel_elf,
                        smp=2,
                        cmdline="console=com1 apic_mode=xapic",
                        expected_apic_mode="xapic-mmio",
                        mem=mem,
                    ),
                    1,
                )[1],
            ),
            (
                "boot_fxsave",
                lambda: (
                    assert_boot_case(
                        name="boot_2cpu_fpu_fxsave_fallback",
                        kernel_elf=kernel_elf,
                        cmdline="console=com1 fpu_mode=fxsave",
                        smp=2,
                        expected_rc=EXIT_CODE_SUCCESS,
                        must_contain=["[DRY-FPU] PASS: cpus=2 mode=fxsave xcr0=0x3 area_bytes=512", "[DRY-BOOT-OK]"],
                        must_not_contain=["[DRY-PANIC]", "[DRY-EXCEPTION]"],
                    ),
                    1,
                )[1],
            ),
            (
                "boot_headless",
                lambda: (
                    assert_positive_smp_boot(
                        "boot_2cpu_headless_nofb",
                        kernel_elf,
                        smp=2,
                        cmdline="console=com1",
                        expect_fb=False,
                        extra_qemu_args=["-vga", "none"],
                        mem=mem,
                    ),
                    1,
                )[1],
            ),
            ("boot_screendump", lambda: _run_screendump_case(kernel_elf, write_esp_fn)),
        ]
        with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
            futs = {pool.submit(fn): label for label, fn in tasks}
            for fut in concurrent.futures.as_completed(futs):
                label = futs[fut]
                results[label] = fut.result()

    n_neg = results["neg"]
    n_p3 = results["p3"]
    n_p4 = results["p4"]
    n_p5 = results["p5"]
    n_p6 = results["p6"]
    n_p7 = results["p7"]
    n_p8 = results["p8"]
    n_p9 = results["p9"]
    n_p10 = results["p10"]
    n_p11 = results["p11"]
    n_p12 = results["p12"]
    n_p13 = results["p13"]
    n_p14 = results["p14"]
    n_p15 = results["p15"]
    n_p16 = results["p16"]
    n_p17 = results["p17"]
    n_p18 = results["p18"]
    n_p19 = results["p19"]
    total = (
        33 + n_neg + n_p3 + n_p4 + n_p5 + n_p6 + n_p7 + n_p8 + n_p9 + n_p10 + n_p11 + n_p12
        + n_p13 + n_p14 + n_p15 + n_p16 + n_p17 + n_p18 + n_p19
    )
    elapsed_all = time.monotonic() - t_all
    _safe_print(
        f"\n[QEMU-TEST] ALL {total} DETERMINISTIC QEMU CHECKS "
        f"({10 + n_neg} LEGACY MILESTONE 0-1C+2A-4a + 9 MILESTONE 2A-1 P1 + 14 MILESTONE 2A-2 + "
        f"{n_p3} MILESTONE 2A-3 + {n_p4} MILESTONE 2B + {n_p5} MILESTONE 2C-0 + "
        f"{n_p6} MILESTONE 2C-1 + {n_p7} PRIORITY-C GPU + {n_p8} MILESTONE 2C-2 HW + "
        f"{n_p9} DRY GLASS + {n_p10} DRY GLASS 2.0 + {n_p11} DRY GLASS 2.1 + {n_p12} LX DRM + "
        f"{n_p13} M2 USB/NVME + {n_p14} M3 LX NET + {n_p15} BRAND/ICONS + {n_p16} ASCENSION 6.0 + "
        f"{n_p17} ASCENSION 7.0 + {n_p18} ASCENSION 8.0 + {n_p19} ASCENSION 9.0) PASSED in {elapsed_all:.2f}s (jobs={jobs})."
    )
