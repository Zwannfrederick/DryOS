#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Negative kernel fault isolation matrix for DRY OS Milestones 1A–1C."""

import pathlib
from typing import Callable


def run_legacy_negative_matrix(
    kernel_elf: pathlib.Path,
    assert_boot_case: Callable[[str, pathlib.Path, str, int, int, list[str], list[str]], None],
    exit_code_panic: int,
) -> int:
    """Execute the deterministic 48 negative SMP/sched/fault/preempt/PMM/VSpace/CSpace/user/IPC fault tests."""
    count = 0

    smp_cases: list[tuple[str, str, list[str], list[str]]] = [
        (
            "smp_ap_timeout",
            "console=com1 smp_fault=ap_timeout",
            [
                "[DRY-SMP-FAIL] AP timeout: online=1/2 online_mask=0x1 missing_mask=0x2",
                "[DRY-PANIC] cpu=0",
            ],
            ["[DRY-BOOT-OK]"],
        ),
        (
            "smp_dup_cpu_id",
            "console=com1 smp_fault=dup_cpu_id",
            [
                "[DRY-SMP-FAIL] Duplicate CPU ID detected: cpu_idx=0",
                "[DRY-PANIC] cpu=1",
            ],
            ["[DRY-BOOT-OK]", "AP timeout"],
        ),
        (
            "smp_stack_overlap",
            "console=com1 smp_fault=stack_overlap",
            [
                "[DRY-SMP-FAIL] Stack/LAPIC conflict (stack_overlap) between cpu=1 and cpu=0",
                "[DRY-PANIC] cpu=1",
            ],
            ["[DRY-BOOT-OK]", "AP timeout"],
        ),
        (
            "smp_df_ist1_ap",
            "console=com1 smp_fault=df_ist1_ap",
            [
                "[DRY-SMP-INFO] Triggering intentional hardware #PF->#DF on AP 1 IST1...",
                "[DRY-DF-IST1] PASS: hardware #DF caught on cpu=1",
                "on private IST1 rsp=",
                "fault_rsp=0x7fffdead0008",
                "[DRY-PANIC] cpu=1",
            ],
            ["[DRY-BOOT-OK]", "[DRY-DF-IST1] FAIL"],
        ),
    ]
    for case_name, cmdline, must_contain, must_not in smp_cases:
        assert_boot_case(case_name, kernel_elf, cmdline, 2, exit_code_panic, must_contain, must_not)
        count += 1

    sched_negative_cases = [
        ("sched_stack_owner_conflict", "stack_owner", "[DRY-SCHED-FAIL] H1 Bug1 stack_owner violation"),
        ("sched_orphan_ready_detected", "orphan_ready", "[DRY-SCHED-FAIL] H1 Bug2 orphaned Ready thread detected"),
        ("sched_lost_wakeup_unarmed", "lost_wakeup", "[DRY-SCHED-FAIL] H1 Bug3 invalid park protocol state"),
        ("sched_exit_resurrection", "exit_resurrect", "[DRY-SCHED-FAIL] H1 Bug4 exiting/dead thread resurrection rejected"),
    ]
    for case_name, fault_arg, expected_fail in sched_negative_cases:
        assert_boot_case(
            case_name,
            kernel_elf,
            f"console=com1 sched_fault={fault_arg}",
            2,
            exit_code_panic,
            [expected_fail, "[DRY-PANIC]"],
            ["[DRY-BOOT-OK]"],
        )
        count += 1

    early_1a_cases: list[tuple[str, str, list[str]]] = [
        (
            "panic_handler_deterministic",
            "console=com1 panic_self_test=1",
            [
                "[DRY-BOOT] DRY Kernel v0.1.0",
                "[DRY-PANIC] cpu=0",
                "intentional panic_self_test triggered via kernel cmdline",
            ],
        ),
        (
            "exception_ud2_caught",
            "console=com1 fault_self_test=ud2",
            ["[DRY-EXCEPTION] cpu=0 vec=6 (#UD Invalid-Opcode)", "[DRY-PANIC] cpu=0"],
        ),
        (
            "exception_pf_caught",
            "console=com1 fault_self_test=pf",
            [
                "[DRY-EXCEPTION] cpu=0 vec=14 (#PF Page-Fault)",
                "cr2=0x7fffdeadbee0",
                "[DRY-PANIC] cpu=0",
            ],
        ),
    ]
    for case_name, cmdline, tokens in early_1a_cases:
        assert_boot_case(case_name, kernel_elf, cmdline, 1, exit_code_panic, tokens, ["[DRY-BOOT-OK]"])
        count += 1

    preempt_negative_cases = [
        ("preempt_irq_storm", "irq_storm", "[DRY-PREEMPT-FAIL] Timer interrupt storm / zero-progress livelock detected"),
        ("preempt_nested_irq", "nested_irq", "[DRY-PREEMPT-FAIL] Nested timer interrupt violation (irq_depth=2)"),
        ("preempt_lost_wakeup", "lost_wakeup", "[DRY-PREEMPT-FAIL] Preemptive lost-wakeup protocol violation"),
        ("preempt_double_schedule", "double_sched", "[DRY-PREEMPT-FAIL] Double scheduling / duplicate runqueue enqueue detected"),
        ("preempt_stack_reuse", "stack_reuse", "[DRY-PREEMPT-FAIL] Premature stack reuse while stack_owner active"),
    ]
    for case_name, fault_arg, expected_fail in preempt_negative_cases:
        assert_boot_case(
            case_name,
            kernel_elf,
            f"console=com1 preempt_fault={fault_arg}",
            2,
            exit_code_panic,
            [expected_fail, "[DRY-PANIC]"],
            ["[DRY-BOOT-OK]"],
        )
        count += 1

    pmm_negative_cases = [
        ("pmm_double_free", "double_free", "[DRY-PMM-FAIL] Double free detected"),
        ("pmm_use_after_free", "uaf", "[DRY-PMM-FAIL] Use-after-free / stale generation detected"),
        ("pmm_double_alloc", "double_alloc", "[DRY-PMM-FAIL] Double allocation / corrupt free-state detected"),
        ("pmm_ownership_violation", "owner_mismatch", "[DRY-PMM-FAIL] Frame ownership / domain / pin violation"),
        ("pmm_untyped_revoke", "untyped_revoke", "[DRY-PMM-FAIL] Revoked Untyped / stale capability access rejected"),
    ]
    for case_name, fault_arg, expected_fail in pmm_negative_cases:
        assert_boot_case(
            case_name,
            kernel_elf,
            f"console=com1 pmm_fault={fault_arg}",
            2,
            exit_code_panic,
            [expected_fail, "[DRY-PANIC]"],
            ["[DRY-BOOT-OK]"],
        )
        count += 1

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
        ("vspace_us_bypass", "us_bypass", ["[DRY-VSPACE-FAIL] U/S permission bypass", "[DRY-PANIC]"]),
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
        ("vspace_double_map", "double_map", ["[DRY-VSPACE-FAIL] Double map detected", "[DRY-PANIC]"]),
        ("vspace_double_unmap", "double_unmap", ["[DRY-VSPACE-FAIL] Double unmap detected", "[DRY-PANIC]"]),
        ("vspace_stale_tlb", "stale_tlb", ["[DRY-TLB-FAIL] Stale TLB hazard", "[DRY-PANIC]"]),
        ("vspace_shootdown_timeout", "shootdown_timeout", ["[DRY-TLB-FAIL] Shootdown timeout", "[DRY-PANIC]"]),
        (
            "vspace_active_frame_reuse",
            "active_frame_reuse",
            ["[DRY-PMM-FAIL] Frame ownership / domain / pin violation", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in vspace_negative_cases:
        assert_boot_case(
            case_name,
            kernel_elf,
            f"console=com1 vspace_fault={fault_arg}",
            2,
            exit_code_panic,
            expected_tokens,
            ["[DRY-BOOT-OK]"],
        )
        count += 1

    cspace_negative_cases: list[tuple[str, str, list[str]]] = [
        ("cspace_forged_handle", "forged_handle", ["[DRY-CSPACE-FAIL] Forged capability handle rejected", "[DRY-PANIC]"]),
        ("cspace_stale_aba", "stale_aba", ["[DRY-CSPACE-FAIL] Stale generation / ABA capability access rejected", "[DRY-PANIC]"]),
        ("cspace_rights_amp", "rights_amp", ["[DRY-CSPACE-FAIL] Rights amplification rejected", "[DRY-PANIC]"]),
        ("cspace_cross_domain", "cross_domain", ["[DRY-CSPACE-FAIL] Cross-domain unauthorized access rejected", "[DRY-PANIC]"]),
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
            case_name,
            kernel_elf,
            f"console=com1 cspace_fault={fault_arg}",
            2,
            exit_code_panic,
            expected_tokens,
            ["[DRY-BOOT-OK]"],
        )
        count += 1

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
            ["[DRY-USER-FAIL] W^X ELF64 segment rejected at load time: err=Some(Elf(WxViolation))", "[DRY-PANIC]"],
        ),
        (
            "user_active_proc_reclaim",
            "active_proc_reclaim",
            ["[DRY-USER-FAIL] Premature reclamation of active process rejected", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in user_negative_cases:
        assert_boot_case(
            case_name,
            kernel_elf,
            f"console=com1 user_fault={fault_arg}",
            2,
            exit_code_panic,
            expected_tokens,
            ["[DRY-BOOT-OK]"],
        )
        count += 1

    ipc_negative_cases: list[tuple[str, str, list[str]]] = [
        (
            "ipc_irq_block_rejected",
            "irq_block",
            ["[DRY-IPC-FAIL] Blocking IPC in preempt-disabled/IRQ context rejected: err=IrqForbidden", "[DRY-PANIC]"],
        ),
        ("ipc_unauth_fb_rejected", "unauth_fb", ["[DRY-IPC-FAIL] Unauthorized framebuffer mapping rejected", "[DRY-PANIC]"]),
        ("ipc_forged_ep_rejected", "forged_ep", ["[DRY-IPC-FAIL] Forged endpoint capability rejected", "[DRY-PANIC]"]),
        (
            "ipc_donation_cycle_rejected",
            "donation_cycle",
            ["[DRY-IPC-FAIL] Priority donation cycle rejected: err=Some(CycleDetected)", "[DRY-PANIC]"],
        ),
    ]
    for case_name, fault_arg, expected_tokens in ipc_negative_cases:
        assert_boot_case(
            case_name,
            kernel_elf,
            f"console=com1 ipc_fault={fault_arg}",
            2,
            exit_code_panic,
            expected_tokens,
            ["[DRY-BOOT-OK]"],
        )
        count += 1

    return count
