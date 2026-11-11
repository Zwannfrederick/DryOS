#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Reproducible ELF undefined-symbol audit & 4-way taxonomy for Intel `igc`, `igb`, and `e1000e`."""

import json
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
OUT_PATH = ROOT / "experiments" / "h2-island" / "manifests" / "igc-kmod-symbols.json"

KMOD_618_IGC = "/lib/modules/6.18.60-1-lts/kernel/drivers/net/ethernet/intel/igc/igc.ko.zst"
KMOD_72_IGC = "/lib/modules/7.2.14-arch1-1/kernel/drivers/net/ethernet/intel/igc/igc.ko.zst"
KMOD_618_E1000E = "/lib/modules/6.18.60-1-lts/kernel/drivers/net/ethernet/intel/e1000e/e1000e.ko.zst"
KMOD_618_IGB = "/lib/modules/6.18.60-1-lts/kernel/drivers/net/ethernet/intel/igb/igb.ko.zst"


def extract_undefined_symbols(ko_zst_path: str) -> tuple[int, list[str]]:
    with tempfile.NamedTemporaryFile(suffix=".ko") as tmp:
        subprocess.run(["zstd", "-dc", ko_zst_path], stdout=tmp, check=True)
        elf_bytes = pathlib.Path(tmp.name).stat().st_size
        nm_out = subprocess.check_output(["nm", "-u", tmp.name], text=True)
        syms = sorted({line.split()[-1] for line in nm_out.splitlines() if line.strip()})
        return elf_bytes, syms


def classify_symbol(sym: str) -> tuple[str, str]:
    """Classify every Linux kernel symbol referenced by `igc` into one of 4 H2 categories:
    - `auto_map`: Direct 1-to-1 C/libc/PCI/MMIO/time/print mapping.
    - `semantic_adapter`: Stateful runtime adapter (`DmaArena`+`vmemmap`, NAPI/IoRing, MSI-X/eventfd, RT locks/timers).
    - `feature_prune`: Optional subsystem pruned in minimal Driver Island (`XDP`/`AF_XDP`, `PTP`, `LEDS`, `MMSV`, `PM`, tracing).
    - `unsupported`: Monolithic kernel internal ABI/global coupling (`softnet_data`, per-CPU `%gs`, kernel `netmem` splicing)
      that cannot be binary-shimmed and requires source-level shadow header replacement or architectural boundary redesign.
    """
    # 1. Unsupported monolithic-kernel internal globals / binary-ABI couplings (solved only by source recompilation)
    unsupported_exact = {
        "softnet_data",
        "this_cpu_off",
        "__preempt_count",
        "__SCT__preempt_schedule_notrace",
        "__SCT__might_resched",
        "vmemmap_base",
        "page_offset_base",
        "phys_base",
        "hugetlb_optimize_vmemmap_key",
        "skb_add_rx_frag_netmem",
        "system_state",
        "cpu_number",
        "numa_node",
        "__cpu_online_mask",
        "__num_online_cpus",
    }
    if sym in unsupported_exact:
        return ("unsupported", "monolithic_percpu_or_vmemmap_binary_global")

    # 2. Feature prune: XDP / AF_XDP / BPF, PTP 1588, TSN MMSV, LEDs, runtime PM, ftrace/retpoline/tracepoints
    prune_prefixes = (
        "xdp_",
        "__xdp_",
        "xp_",
        "xsk_",
        "bpf_",
        "ptp_",
        "ethtool_mmsv_",
        "led_classdev_",
        "__pm_runtime_",
        "pm_schedule_",
        "__SCK__tp_",
        "__SCT__tp_",
        "__SCT__WARN_",
        "__tracepoint_",
        "tracepoint_",
        "do_trace_",
        "static_key_",
        "__x86_indirect_thunk_",
        "__x86_return_thunk",
    )
    prune_exact = {
        "__fentry__",
        "device_set_wakeup_enable",
        "pci_enable_wake",
        "pci_wake_from_d3",
        "pci_set_power_state",
        "pci_save_state",
        "pci_restore_state",
        "pci_enable_ptm",
        "pci_disable_ptm",
        "pcie_ptm_enabled",
        "get_device_system_crosststamp",
        "ktime_get_snapshot",
        "ktime_get_snapshot_id",
        "timecounter_init",
        "skb_tstamp_tx",
        "skb_clone_tx_timestamp",
        "ethtool_puts",
        "ethtool_sprintf",
        "param_ops_int",
    }
    if sym.startswith(prune_prefixes) or sym in prune_exact:
        return ("feature_prune", "optional_xdp_ptp_tsn_pm_or_tracing")

    # 3. Semantic adapters: DMA Arena + struct page, NAPI + skb/netdev IoRing, IRQ/MSI-X, RT sync, timers/workqueues
    adapter_prefixes = (
        "dma_",
        "__dma_sync_",
        "napi_",
        "__napi_",
        "netif_",
        "__netif_",
        "netdev_",
        "__netdev_",
        "skb_",
        "__skb_",
        "pskb_",
        "__alloc_skb",
        "consume_skb",
        "dev_kfree_skb",
        "gro_receive_skb",
        "eth_",
        "__vlan_",
        "dql_",
        "_raw_spin_",
        "mutex_",
        "__mutex_",
        "rtnl_",
        "__rcu_read_",
        "kvfree_call_rcu",
        "hrtimer_",
        "timer_",
        "mod_timer",
        "round_jiffies",
        "queue_work_",
        "cancel_work_",
        "system_percpu_wq",
    )
    adapter_exact = {
        "__alloc_pages_noprof",
        "__free_pages",
        "__page_frag_cache_drain",
        "alloc_etherdev_mqs",
        "free_netdev",
        "register_netdev",
        "unregister_netdev",
        "synchronize_net",
        "dev_addr_mod",
        "__hw_addr_sync_dev",
        "dev_trans_start",
        "csum_partial",
        "request_threaded_irq",
        "free_irq",
        "synchronize_irq",
        "pci_enable_msix_range",
        "pci_disable_msix",
        "pci_enable_msi",
        "pci_disable_msi",
        "__pci_register_driver",
        "pci_unregister_driver",
    }
    if sym.startswith(adapter_prefixes) or sym in adapter_exact:
        return ("semantic_adapter", "dma_napi_skb_irq_or_rt_sync_adapter")

    # 4. Auto-mapped 1-to-1 shims: allocators, string/mem, MMIO/BAR, PCI config, clocks/sleep, logging/assertions
    auto_prefixes = (
        "kmalloc",
        "__kmalloc",
        "vmalloc",
        "vzalloc",
        "pci_",
        "pcie_",
        "_dev_",
        "ktime_",
        "mem",
        "str",
        "snprint",
        "scnprint",
        "sprint",
    )
    auto_exact = {
        "kfree",
        "vfree",
        "is_vmalloc_addr",
        "ioremap",
        "iounmap",
        "jiffies",
        "sched_clock",
        "ns_to_timespec64",
        "set_normalized_timespec64",
        "__udelay",
        "__const_udelay",
        "usleep_range_state",
        "msleep",
        "msleep_interruptible",
        "_printk",
        "__dynamic_netdev_dbg",
        "__warn_printk",
        "dump_stack",
        "print_hex_dump",
        "net_ratelimit",
        "dev_driver_string",
        "sized_strscpy",
        "refcount_warn_saturate",
        "__list_add_valid_or_report",
        "__list_del_entry_valid_or_report",
        "__fortify_panic",
        "__stack_chk_fail",
        "__ref_stack_chk_guard",
    }
    if sym.startswith(auto_prefixes) or sym in auto_exact:
        return ("auto_map", "direct_allocator_pci_mmio_time_or_libc_map")

    return ("UNCLASSIFIED", "unknown")


def main() -> None:
    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)

    bytes_618, syms_618 = extract_undefined_symbols(KMOD_618_IGC)
    bytes_72, syms_72 = extract_undefined_symbols(KMOD_72_IGC)
    bytes_e1000e, syms_e1000e = extract_undefined_symbols(KMOD_618_E1000E)
    bytes_igb, syms_igb = extract_undefined_symbols(KMOD_618_IGB)

    set_618 = set(syms_618)
    set_72 = set(syms_72)
    common_igc = sorted(set_618 & set_72)
    removed_in_72 = sorted(set_618 - set_72)
    added_in_72 = sorted(set_72 - set_618)

    union_syms = sorted(set_618 | set_72)
    classified = {}
    counts_618 = {"auto_map": 0, "semantic_adapter": 0, "feature_prune": 0, "unsupported": 0}
    counts_union = {"auto_map": 0, "semantic_adapter": 0, "feature_prune": 0, "unsupported": 0}
    unclassified = []

    for s in union_syms:
        cat, reason = classify_symbol(s)
        if cat == "UNCLASSIFIED":
            unclassified.append(s)
        else:
            counts_union[cat] += 1
            if s in set_618:
                counts_618[cat] += 1
        classified[s] = {
            "category": cat,
            "reason": reason,
            "in_6_18": s in set_618,
            "in_7_2": s in set_72,
        }

    if unclassified:
        raise RuntimeError(f"Unclassified symbols remain ({len(unclassified)}): {unclassified}")

    report = {
        "driver": "igc",
        "modules_analyzed": {
            "6.18.60-1-lts": {
                "path": KMOD_618_IGC,
                "elf_bytes": bytes_618,
                "undefined_symbol_count": len(syms_618),
                "category_counts": counts_618,
            },
            "7.2.14-arch1-1": {
                "path": KMOD_72_IGC,
                "elf_bytes": bytes_72,
                "undefined_symbol_count": len(syms_72),
            },
        },
        "version_drift_6_18_to_7_2": {
            "common_count": len(common_igc),
            "stability_ratio": round(len(common_igc) / len(syms_618), 4),
            "removed_in_7_2": removed_in_72,
            "added_in_7_2": added_in_72,
        },
        "cross_driver_overlap_6_18": {
            "e1000e": {
                "elf_bytes": bytes_e1000e,
                "symbol_count": len(syms_e1000e),
                "shared_with_igc": len(set_618 & set(syms_e1000e)),
                "shared_ratio_of_e1000e": round(len(set_618 & set(syms_e1000e)) / len(syms_e1000e), 4),
            },
            "igb": {
                "elf_bytes": bytes_igb,
                "symbol_count": len(syms_igb),
                "shared_with_igc": len(set_618 & set(syms_igb)),
                "shared_ratio_of_igb": round(len(set_618 & set(syms_igb)) / len(syms_igb), 4),
            },
        },
        "union_symbol_count": len(union_syms),
        "union_category_counts": counts_union,
        "unclassified_count": 0,
        "symbols": classified,
    }

    OUT_PATH.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(
        f"[H2-SYM] 6.18={len(syms_618)} 7.2={len(syms_72)} common={len(common_igc)} "
        f"({report['version_drift_6_18_to_7_2']['stability_ratio']*100:.1f}%) "
        f"removed={len(removed_in_72)} added={len(added_in_72)}"
    )
    print(f"[H2-SYM] 6.18 taxonomy: {counts_618}")
    print(
        f"[H2-SYM] Cross-driver (6.18): e1000e={len(syms_e1000e)} (shared {len(set_618 & set(syms_e1000e))}), "
        f"igb={len(syms_igb)} (shared {len(set_618 & set(syms_igb))})"
    )


if __name__ == "__main__":
    main()
