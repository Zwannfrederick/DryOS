#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Serial output validators for Milestone 1B-D Ring 3 Userspace, Syscall ABI & Benchmarks."""

from qemu_log_validators import _parse_kv_ints


REQUIRED_STAGE_D_ORDERED_TRACE = [
    "[DRY-BOOT] Kernel ready",
    "[DRY-PROC] PID=1 VSpace=1 CSpace=1",
    "[DRY-USER] Entering CPL3",
    "Hello from DRY OS!",
    "[DRY-SYSCALL] write_debug: OK",
    "[DRY-SYSCALL] yield: OK",
    "[DRY-USER] exit(0)",
    "[DRY-PROC] PID=1 terminated",
    "[DRY-PMM] leaked_frames=0",
]


def verify_userspace_lines(name: str, serial_out: str, smp: int) -> None:
    """Validate Stage D real-hardware ordered CPL=3 trace, Stage F 16/16 security invariants,
    and Stage G 11-metric `p50/p95/p99` benchmark percentiles."""
    cursor = 0
    for marker in REQUIRED_STAGE_D_ORDERED_TRACE:
        idx = serial_out.find(marker, cursor)
        if idx < 0:
            raise RuntimeError(
                f"[{name}] Missing or out-of-order Stage D Ring 3 trace marker {marker!r} after byte {cursor}"
            )
        cursor = idx + len(marker)

    sec_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-USER-SEC] PASS:" in ln]
    if len(sec_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-USER-SEC] PASS line, got {len(sec_lines)}")
    sec_kv = _parse_kv_ints(sec_lines[0])
    for flag in (
        "scenarios=16/16",
        "cpl3_isolation_ok=true",
        "syscall_hygiene_ok=true",
        "nmi_gs_safe=true",
        "sysret_canon_ok=true",
    ):
        if flag not in sec_lines[0]:
            raise RuntimeError(f"[{name}] Missing {flag} in {sec_lines[0]!r}")
    if (
        sec_kv.get("cpus") != smp
        or sec_kv.get("uaccess_calls", 0) < 6
        or sec_kv.get("uaccess_fixups", 0) < 6
        or sec_kv.get("leaked_frames", 1) != 0
    ):
        raise RuntimeError(f"[{name}] Stage F security telemetry mismatch: {sec_lines[0]!r}")
    if sec_kv.get("smap_hw") == 1 and sec_kv.get("smap_active") != 1:
        raise RuntimeError(f"[{name}] Hardware supports SMAP but smap_active != 1: {sec_lines[0]!r}")

    ub_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-USER-BENCH] PASS:" in ln]
    if len(ub_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-USER-BENCH] PASS line, got {len(ub_lines)}")
    ub_kv = _parse_kv_ints(ub_lines[0])
    if (
        ub_kv.get("cpus") != smp
        or ub_kv.get("same_vs_cr3_reloads", 1) != 0
        or ub_kv.get("leaked_frames", 1) != 0
        or ub_kv.get("uaccess_mib_s", 0) == 0
        or ub_kv.get("null_iretq_p50_cyc", 0) == 0
        or ub_kv.get("uaccess_4k_from_p50_cyc", 0) == 0
        or ub_kv.get("uaccess_4k_to_p50_cyc", 0) == 0
        or ub_kv.get("smp_workers") != smp
        or ub_kv.get("smp_syscalls", 0) < smp * 256
        or ub_kv.get("smp_mops_x100", 0) == 0
    ):
        raise RuntimeError(f"[{name}] Stage G user benchmark header/throughput mismatch: {ub_lines[0]!r}")

    for metric in (
        "r0_to_r3",
        "r3_to_r0",
        "null_sys",
        "yield_sys",
        "k2k_sw",
        "u2u_same_vs",
        "u2u_cross_vs",
        "cr3_sw",
        "uaccess_64b",
        "proc_life",
    ):
        p50 = ub_kv.get(f"{metric}_p50_cyc", 0)
        p95 = ub_kv.get(f"{metric}_p95_cyc", 0)
        p99 = ub_kv.get(f"{metric}_p99_cyc", 0)
        if p50 == 0 or not (p50 <= p95 <= p99):
            raise RuntimeError(
                f"[{name}] Invalid {metric} percentiles (p50={p50}, p95={p95}, p99={p99}): {ub_lines[0]!r}"
            )
