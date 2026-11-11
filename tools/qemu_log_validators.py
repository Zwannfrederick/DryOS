#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Serial log invariant validators for DRY Kernel QEMU boot & SMP test harness."""


def expected_sched_metrics(smp: int) -> tuple[int, int, int]:
    n_yield = max(8, min(24, smp * 2))
    n_pairs = max(4, min(12, smp))
    n_recycle = 48
    total_spawned = n_yield + (2 * n_pairs) + n_recycle
    park_rounds = n_pairs * 32
    work_units = (n_yield * 32) + (n_pairs * 64) + (n_recycle * 8)
    return total_spawned, park_rounds, work_units


def _parse_kv_ints(line: str) -> dict[str, int]:
    out: dict[str, int] = {}
    for tok in line.split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if v.isdigit():
                out[k] = int(v)
    return out


def verify_sched_line(name: str, serial_out: str, smp: int) -> None:
    sched_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-SCHED] PASS:" in ln]
    if len(sched_lines) != 1:
        raise RuntimeError(f"[{name}] Expected exactly 1 [DRY-SCHED] PASS line, got {len(sched_lines)}")
    fields = _parse_kv_ints(sched_lines[0])
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

    apic_kv = _parse_kv_ints(apic_lines[0])
    if apic_kv.get("ticks_per_ms", 0) < 100:
        raise RuntimeError(f"[{name}] Invalid calibrated ticks_per_ms in {apic_lines[0]!r}")
    if apic_kv.get("tsc_mhz", 0) < 100:
        raise RuntimeError(f"[{name}] Invalid calibrated tsc_mhz in {apic_lines[0]!r}")

    irq_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-IRQ-ONLY] PASS:" in ln]
    if len(irq_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-IRQ-ONLY] PASS line, got {len(irq_lines)}")
    irq_kv = _parse_kv_ints(irq_lines[0])
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
    fields = _parse_kv_ints(p_lines[0])
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


def verify_pmm_and_untyped_lines(name: str, serial_out: str, smp: int) -> None:
    pmm_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-PMM] PASS:" in ln]
    if len(pmm_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-PMM] PASS line, got {len(pmm_lines)}")
    pmm_kv = _parse_kv_ints(pmm_lines[0])
    if pmm_kv.get("usable_frames", 0) < 32768 or pmm_kv.get("free_init", 0) < 32768:
        raise RuntimeError(f"[{name}] Unexpectedly low usable/free frames: {pmm_lines[0]!r}")
    if pmm_kv.get("reclaimable_frames", 0) < 1 or pmm_kv.get("kernel_meta_frames", 0) < 256:
        raise RuntimeError(f"[{name}] Missing reclaimable/kernel_meta accounting: {pmm_lines[0]!r}")
    if pmm_kv.get("meta_overhead_bps", 9999) > 50:
        raise RuntimeError(f"[{name}] FrameMeta overhead > 0.50% (50 bps): {pmm_lines[0]!r}")
    if pmm_kv.get("leaked_frames", 1) != 0:
        raise RuntimeError(f"[{name}] Frame leak detected: {pmm_lines[0]!r}")
    for flag in ("bootstrap_sealed=true", "cross_domain_zero_ok=true", "oom_reclaim_conserved=true"):
        if flag not in pmm_lines[0]:
            raise RuntimeError(f"[{name}] Missing {flag} in {pmm_lines[0]!r}")

    ut_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-UNTYPED] PASS:" in ln]
    if len(ut_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-UNTYPED] PASS line, got {len(ut_lines)}")
    for flag in (
        "split_ok=true",
        "retype_ok=true",
        "pin_guard_ok=true",
        "revoke_scrub_ok=true",
        "conserved=true",
    ):
        if flag not in ut_lines[0]:
            raise RuntimeError(f"[{name}] Missing {flag} in {ut_lines[0]!r}")

    bench_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-PMM-BENCH] PASS:" in ln]
    if len(bench_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-PMM-BENCH] PASS line, got {len(bench_lines)}")
    b_kv = _parse_kv_ints(bench_lines[0])
    if b_kv.get("cpus") != smp:
        raise RuntimeError(f"[{name}] PMM bench cpus mismatch: {bench_lines[0]!r}")
    if b_kv.get("samples", 0) < 2048:
        raise RuntimeError(f"[{name}] Expected PMM bench samples >= 2048, got: {bench_lines[0]!r}")
    if b_kv.get("alloc_p50_cyc", 0) == 0 or b_kv.get("alloc_p99_cyc", 0) < b_kv.get("alloc_p50_cyc", 0):
        raise RuntimeError(f"[{name}] Invalid alloc latency percentiles: {bench_lines[0]!r}")
    if b_kv.get("free_p50_cyc", 0) == 0 or b_kv.get("free_p99_cyc", 0) < b_kv.get("free_p50_cyc", 0):
        raise RuntimeError(f"[{name}] Invalid free latency percentiles: {bench_lines[0]!r}")
    if b_kv.get("zero_4k_p50_cyc", 0) == 0 or b_kv.get("throughput_mops_x100", 0) == 0:
        raise RuntimeError(f"[{name}] Missing zeroing/throughput metrics: {bench_lines[0]!r}")
    if b_kv.get("mag_hit_pct", 0) < 90:
        raise RuntimeError(f"[{name}] Expected magazine hit rate >= 90%, got: {bench_lines[0]!r}")
    if (
        b_kv.get("contig_2m_before", 0) < 16
        or b_kv.get("contig_2m_mid", 99999) >= b_kv.get("contig_2m_before", 0)
        or b_kv.get("contig_2m_after") != b_kv.get("contig_2m_before")
    ):
        raise RuntimeError(f"[{name}] 2MiB contiguous conservation mismatch: {bench_lines[0]!r}")

    live_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-LIVENESS-REG] PASS:" in ln]
    if len(live_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-LIVENESS-REG] PASS line, got {len(live_lines)}")
    l_kv = _parse_kv_ints(live_lines[0])
    if (
        l_kv.get("cpus") != smp
        or l_kv.get("unpinned_spawner_waves") != 4
        or l_kv.get("spawned") != 48
        or l_kv.get("exited") != 48
        or l_kv.get("starved") != 0
    ):
        raise RuntimeError(f"[{name}] Spawner liveness regression mismatch: {live_lines[0]!r}")


def verify_vspace_lines(name: str, serial_out: str, smp: int) -> None:
    vs_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-VSPACE] PASS:" in ln]
    if len(vs_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-VSPACE] PASS line, got {len(vs_lines)}")
    v_kv = _parse_kv_ints(vs_lines[0])
    for flag in (
        "boot_reclaim_blocked_while_active=true",
        "kernel_wx_enforced=true",
        "wp_nxe_verified=true",
        "pcid_assumed=false",
        "pcid_enabled=false",
        "isolated_vspaces_ok=true",
        "pt_reclaim_ok=true",
    ):
        if flag not in vs_lines[0]:
            raise RuntimeError(f"[{name}] Missing {flag} in {vs_lines[0]!r}")
    if v_kv.get("cpus") != smp:
        raise RuntimeError(f"[{name}] VSpace cpus mismatch: {vs_lines[0]!r}")
    if v_kv.get("reclaimed_boot_frames", 0) < 1000:
        raise RuntimeError(f"[{name}] Expected reclaimed_boot_frames >= 1000, got: {vs_lines[0]!r}")
    if v_kv.get("leaked_frames", 1) != 0:
        raise RuntimeError(f"[{name}] VSpace leaked_frames != 0: {vs_lines[0]!r}")

    hp_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-HUGEPAGE] PASS:" in ln]
    if len(hp_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-HUGEPAGE] PASS line, got {len(hp_lines)}")
    hp_kv = _parse_kv_ints(hp_lines[0])
    for flag in ("order2m_ok=true", "wx_ok=true", "conserved=true"):
        if flag not in hp_lines[0]:
            raise RuntimeError(f"[{name}] Missing {flag} in {hp_lines[0]!r}")
    if hp_kv.get("pt_frames_saved", 0) < 1:
        raise RuntimeError(f"[{name}] Expected pt_frames_saved >= 1, got: {hp_lines[0]!r}")
    if hp_kv.get("map_2m_cyc", 0) == 0 or hp_kv.get("map_512x4k_cyc", 0) <= hp_kv.get("map_2m_cyc", 0):
        raise RuntimeError(f"[{name}] Invalid 2MiB vs 512x4K mapping cost comparison: {hp_lines[0]!r}")
    if hp_kv.get("unmap_2m_cyc", 0) == 0:
        raise RuntimeError(f"[{name}] Missing unmap_2m_cyc in {hp_lines[0]!r}")

    vb_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-VSPACE-BENCH] PASS:" in ln]
    if len(vb_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-VSPACE-BENCH] PASS line, got {len(vb_lines)}")
    vb_kv = _parse_kv_ints(vb_lines[0])
    if vb_kv.get("cpus") != smp or vb_kv.get("samples", 0) < 512:
        raise RuntimeError(f"[{name}] Invalid VSpace bench header: {vb_lines[0]!r}")
    if (
        vb_kv.get("cr3_switch_p50_cyc", 0) == 0
        or vb_kv.get("cr3_switch_p99_cyc", 0) < vb_kv.get("cr3_switch_p50_cyc", 0)
        or vb_kv.get("cr3_noop_p50_cyc", 999999) >= vb_kv.get("cr3_switch_p50_cyc", 0)
    ):
        raise RuntimeError(f"[{name}] Invalid CR3 switch/noop percentiles: {vb_lines[0]!r}")
    for metric in ("map_4k", "protect_4k", "unmap_local"):
        p50 = vb_kv.get(f"{metric}_p50_cyc", 0)
        p99 = vb_kv.get(f"{metric}_p99_cyc", 0)
        if p50 == 0 or p99 < p50:
            raise RuntimeError(f"[{name}] Invalid {metric} percentiles: {vb_lines[0]!r}")
    if smp >= 2:
        if vb_kv.get("shootdown_1peer_p50_cyc", 0) == 0 or vb_kv.get("shootdown_all_p50_cyc", 0) == 0:
            raise RuntimeError(f"[{name}] Missing SMP shootdown percentiles on smp={smp}: {vb_lines[0]!r}")

    smp_lines = [ln.strip() for ln in serial_out.splitlines() if "[DRY-VSPACE-SMP] PASS:" in ln]
    if len(smp_lines) != 1:
        raise RuntimeError(f"[{name}] Expected 1 [DRY-VSPACE-SMP] PASS line, got {len(smp_lines)}")
    vs_kv = _parse_kv_ints(smp_lines[0])
    if (
        vs_kv.get("cpus") != smp
        or vs_kv.get("workers", 0) < 4
        or vs_kv.get("cr3_switches", 0) < 32
        or vs_kv.get("map_unmap_ops", 0) < 64
        or vs_kv.get("protect_ops", 0) < 64
        or vs_kv.get("stale_tlb_reads", 1) != 0
        or vs_kv.get("leaked_frames", 1) != 0
    ):
        raise RuntimeError(f"[{name}] VSpace SMP stress invariant mismatch: {smp_lines[0]!r}")
    if smp >= 2 and vs_kv.get("shootdowns", 0) < 16:
        raise RuntimeError(f"[{name}] Expected shootdowns >= 16 on smp={smp}: {smp_lines[0]!r}")
