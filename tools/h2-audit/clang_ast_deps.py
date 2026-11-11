#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Clang AST-based dependency, struct-field, callback, and v6.12->v6.18 drift analyzer for `igc`."""

import json
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
EXTRACTOR = ROOT / "tools" / "h2-audit" / "build" / "clang_ast_extractor"
VENDOR = ROOT / "vendor" / "linux-drivers"
OUT_JSON = ROOT / "experiments" / "h2-island" / "manifests" / "igc-clang-ast-deps.json"
KDIR = pathlib.Path("/lib/modules/6.18.60-1-lts/build")

CORE_TUS = {
    "igc_main.c",
    "igc_mac.c",
    "igc_i225.c",
    "igc_base.c",
    "igc_nvm.c",
    "igc_phy.c",
}


def classify_ast_call(fn: str, header: str) -> str:
    if fn.startswith(("__builtin_", "__compiletime_")):
        return "auto_map"

    # Architecturally unsupported monolithic-kernel behaviors in an isolated userspace C Driver Island:
    # 1) In-kernel eBPF/XDP/AF_XDP zero-copy kernel UMEM & JIT pointer execution (`bpf_*`, `xdp_*`, `xsk_*`, `xp_*`)
    # 2) Direct cross-address-space userspace pointer dereference (`copy_from_user`, `copy_to_user`)
    # 3) Monolithic kernel global RCU / self-modifying jump-label / %gs per-CPU coupling
    unsupported_prefixes = (
        "xdp_",
        "__xdp_",
        "xp_",
        "xsk_",
        "bpf_",
    )
    unsupported = {
        "copy_from_user",
        "copy_to_user",
        "skb_add_rx_frag_netmem",
        "netmem_to_page",
        "page_pool_put_full_page",
        "synchronize_rcu",
        "rcu_barrier",
        "____wrong_branch_error",
        "__bad_size_call_parameter",
        "static_key_slow_inc",
        "static_key_slow_dec",
    }
    if (
        fn.startswith(unsupported_prefixes)
        or fn in unsupported
        or "/bpf" in header
        or "/xdp" in header
    ):
        return "unsupported"

    # Optional NIC subsystems cleanly pruned in minimal Driver Island (PTP, Ethtool, LEDs, PM, ftrace)
    prune_prefixes = (
        "ptp_",
        "ethtool_",
        "led_",
        "pm_runtime_",
        "__pm_runtime_",
        "pm_schedule_",
        "trace_",
        "do_trace_",
        "static_branch_",
    )
    prune_exact = {
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
        "timecounter_init",
        "timecounter_read",
        "timecounter_cyc2time",
        "skb_tstamp_tx",
        "skb_tx_timestamp",
        "skb_clone_tx_timestamp",
    }
    if fn.startswith(prune_prefixes) or fn in prune_exact or "/bpf" in header or "/xdp" in header or "ptp_clock" in header:
        return "feature_prune"

    # Stateful semantic adapters (DMA/IOMMU arena, NAPI/skb/netdev IoRing, IRQ/MSI-X, RT sync/timers/work)
    adapter_prefixes = (
        "dma_",
        "napi_",
        "__napi_",
        "netif_",
        "__netif_",
        "netdev_",
        "__netdev_",
        "skb_",
        "__skb_",
        "pskb_",
        "eth_",
        "vlan_",
        "__vlan_",
        "ipv6_",
        "ip_",
        "tcp_",
        "udp_",
        "csum_",
        "spin_lock",
        "spin_unlock",
        "spin_trylock",
        "raw_spin_",
        "_raw_spin_",
        "mutex_",
        "__mutex_",
        "rtnl_",
        "rcu_read_",
        "kfree_rcu",
        "kvfree_rcu",
        "hrtimer_",
        "timer_",
        "mod_timer",
        "del_timer",
        "schedule_work",
        "queue_work",
        "cancel_work",
        "INIT_WORK",
        "alloc_pages",
        "__alloc_pages",
        "__free_pages",
        "free_page",
        "get_page",
        "put_page",
        "page_address",
        "virt_to_page",
        "page_to_phys",
        "dql_",
    )
    adapter_exact = {
        "alloc_etherdev_mqs",
        "free_netdev",
        "register_netdev",
        "unregister_netdev",
        "synchronize_net",
        "consume_skb",
        "dev_kfree_skb_any",
        "dev_kfree_skb_irq",
        "dev_consume_skb_any",
        "build_skb",
        "gro_receive_skb",
        "dev_addr_mod",
        "dev_uc_sync",
        "dev_uc_unsync",
        "dev_mc_sync",
        "dev_mc_unsync",
        "request_irq",
        "request_threaded_irq",
        "free_irq",
        "synchronize_irq",
        "enable_irq",
        "disable_irq",
        "disable_irq_nosync",
        "pci_alloc_irq_vectors",
        "pci_free_irq_vectors",
        "pci_enable_msix_range",
        "pci_disable_msix",
        "pci_enable_msi",
        "pci_disable_msi",
        "pci_irq_vector",
        "__pci_register_driver",
        "pci_unregister_driver",
    }
    if fn.startswith(adapter_prefixes) or fn in adapter_exact:
        return "semantic_adapter"

    # Direct 1-to-1 hardware MMIO, PCI config, memory alloc, bitops, atomics, time, logging
    return "auto_map"


def clang_kernel_args(clang_inc: str, src_dir: pathlib.Path, basename: str) -> list[str]:
    return [
        "-nostdinc",
        "-isystem",
        clang_inc,
        f"-I{src_dir}",
        f"-I{KDIR}/arch/x86/include",
        f"-I{KDIR}/arch/x86/include/generated",
        f"-I{KDIR}/include",
        f"-I{KDIR}/arch/x86/include/uapi",
        f"-I{KDIR}/arch/x86/include/generated/uapi",
        f"-I{KDIR}/include/uapi",
        f"-I{KDIR}/include/generated/uapi",
        "-include",
        f"{KDIR}/include/linux/compiler-version.h",
        "-include",
        f"{KDIR}/include/linux/kconfig.h",
        "-include",
        f"{KDIR}/include/linux/compiler_types.h",
        "-D__KERNEL__",
        "-DMODULE",
        '-DKBUILD_MODNAME="igc"',
        f'-DKBUILD_BASENAME="{basename}"',
        "-fcf-protection=branch",
        "-O2",
        "-fno-strict-aliasing",
        "-Wno-everything",
    ]


def analyze_version(tag: str, clang_inc: str) -> dict:
    src_dir = VENDOR / tag / "igc"
    c_files = sorted(p.name for p in src_dir.glob("*.c"))

    all_calls: dict[str, dict] = {}
    core_calls: set[str] = set()
    ext_fields: dict[str, set[str]] = {}
    ops_tables: list[dict] = []
    tu_errors: dict[str, int] = {}

    for cname in c_files:
        cpath = src_dir / cname
        is_core = cname in CORE_TUS
        args = clang_kernel_args(clang_inc, src_dir, cname[:-2])
        proc = subprocess.run(
            [str(EXTRACTOR), str(src_dir), str(cpath), *args],
            capture_output=True,
            text=True,
            check=True,
        )
        for line in proc.stdout.splitlines():
            if not line.strip():
                continue
            item = json.loads(line)
            kind = item["kind"]
            if kind == "tu_summary":
                tu_errors[cname] = item["errors"]
            elif kind == "ext_call":
                fn = item["name"]
                if not fn:
                    continue
                cat = classify_ast_call(fn, item["header"])
                entry = all_calls.setdefault(
                    fn,
                    {
                        "sig": item["sig"],
                        "header": item["header"],
                        "inlined": item["inlined"],
                        "category": cat,
                        "used_in_core": False,
                        "files": set(),
                    },
                )
                entry["files"].add(cname)
                if is_core:
                    entry["used_in_core"] = True
                    core_calls.add(fn)
            elif kind == "ext_field":
                rec = item["record"]
                fld = item["field"]
                if fld:
                    ext_fields.setdefault(rec, set()).add(fld)
            elif kind == "ops_table":
                ops_tables.append({"file": cname, "var": item["var"], "type": item["type"]})

    all_cat_counts = {"auto_map": 0, "semantic_adapter": 0, "feature_prune": 0, "unsupported": 0}
    core_cat_counts = {"auto_map": 0, "semantic_adapter": 0, "feature_prune": 0, "unsupported": 0}
    inlined_count = 0
    serialized_calls = {}
    for fn, meta in sorted(all_calls.items()):
        cat = meta["category"]
        all_cat_counts[cat] += 1
        if meta["used_in_core"]:
            core_cat_counts[cat] += 1
        if meta["inlined"]:
            inlined_count += 1
        serialized_calls[fn] = {
            "category": cat,
            "inlined": meta["inlined"],
            "used_in_core": meta["used_in_core"],
            "sig": meta["sig"],
            "header": meta["header"],
            "files": sorted(meta["files"]),
        }

    key_structs = {}
    for rec, fields in sorted(ext_fields.items()):
        if any(
            k in rec
            for k in (
                "struct net_device",
                "struct sk_buff",
                "struct pci_dev",
                "struct napi_struct",
                "struct device",
                "struct page",
                "struct netdev_queue",
            )
        ):
            key_structs[rec] = sorted(fields)

    return {
        "c_file_count": len(c_files),
        "tu_parse_errors_against_6_18_headers": tu_errors,
        "total_external_functions": len(serialized_calls),
        "inlined_kernel_helpers_hidden_from_nm": inlined_count,
        "core_6_files_external_functions": len(core_calls),
        "all_13_files_category_counts": all_cat_counts,
        "core_6_files_category_counts": core_cat_counts,
        "ops_tables": ops_tables,
        "key_kernel_struct_fields_accessed": key_structs,
        "external_calls": serialized_calls,
    }


def compute_source_diff_stats() -> dict:
    dir_612 = VENDOR / "v6.12" / "igc"
    dir_618 = VENDOR / "v6.18" / "igc"
    proc = subprocess.run(
        ["diff", "-u", "-r", str(dir_612), str(dir_618)],
        capture_output=True,
        text=True,
        check=False,
    )
    added = 0
    removed = 0
    changed_files = []
    for line in proc.stdout.splitlines():
        if line.startswith("+++ "):
            changed_files.append(pathlib.Path(line.split()[1]).name)
        elif line.startswith("+") and not line.startswith("+++"):
            added += 1
        elif line.startswith("-") and not line.startswith("---"):
            removed += 1
    return {
        "changed_file_count": len(changed_files),
        "changed_files": sorted(changed_files),
        "lines_added": added,
        "lines_removed": removed,
    }


def main() -> None:
    OUT_JSON.parent.mkdir(parents=True, exist_ok=True)
    clang_inc = subprocess.check_output(
        ["clang", "-print-file-name=include"], text=True
    ).strip()

    res_618 = analyze_version("v6.18", clang_inc)
    res_612 = analyze_version("v6.12", clang_inc)
    diff_stats = compute_source_diff_stats()

    calls_618 = set(res_618["external_calls"].keys())
    calls_612 = set(res_612["external_calls"].keys())

    report = {
        "tool": "libclang AST extractor (clang 23.1.4)",
        "kernel_headers_reference": str(KDIR),
        "source_diff_v6_12_to_v6_18": diff_stats,
        "ast_api_drift_v6_12_to_v6_18": {
            "calls_only_in_v6_12": sorted(calls_612 - calls_618),
            "calls_only_in_v6_18": sorted(calls_618 - calls_612),
            "common_calls": len(calls_618 & calls_612),
        },
        "v6.18": res_618,
        "v6.12": res_612,
    }

    OUT_JSON.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(
        f"[H2-AST] v6.18: TUs={res_618['c_file_count']} parse_errs={sum(res_618['tu_parse_errors_against_6_18_headers'].values())} "
        f"ext_calls={res_618['total_external_functions']} (inlined_hidden_from_nm={res_618['inlined_kernel_helpers_hidden_from_nm']}) "
        f"core6_calls={res_618['core_6_files_external_functions']}"
    )
    print(f"[H2-AST] v6.18 all-13 taxonomy: {res_618['all_13_files_category_counts']}")
    print(f"[H2-AST] v6.18 core-6 taxonomy: {res_618['core_6_files_category_counts']}")
    print(
        f"[H2-AST] v6.12 parse_errs_against_6.18_headers={res_612['tu_parse_errors_against_6_18_headers']}"
    )
    print(f"[H2-AST] v6.12->v6.18 source diff: +{diff_stats['lines_added']} / -{diff_stats['lines_removed']} across {diff_stats['changed_file_count']} files")
    print(f"[H2-AST] AST call drift: removed={report['ast_api_drift_v6_12_to_v6_18']['calls_only_in_v6_12']} added={report['ast_api_drift_v6_12_to_v6_18']['calls_only_in_v6_18']}")


if __name__ == "__main__":
    main()
