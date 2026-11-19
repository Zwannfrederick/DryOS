#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Ascension 7.0 Milestone F GPU IPC Multi-Tier Benchmark Suite (`1920x1080x4` vs `165 Hz`)."""

from __future__ import annotations

import argparse
import json
import pathlib
import resource
import subprocess
import sys
import time
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
PROTO_DIR = pathlib.Path(__file__).resolve().parent
C_SRC = PROTO_DIR / "ipc_bench_a7.c"
BIN_PATH = ROOT / "kernel" / "build" / "ipc_bench_a7"
DEFAULT_OUT = ROOT / "tools" / "compat_bench" / "results_gpu_ipc_a7.json"

FRAME_BUDGET_165HZ_US = 1_000_000.0 / 165.0  # 6060.61 us


def _compile_native_bench() -> None:
    BIN_PATH.parent.mkdir(parents=True, exist_ok=True)
    subprocess.check_call(
        [
            "gcc",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-std=c11",
            str(C_SRC),
            "-o",
            str(BIN_PATH),
        ]
    )


def _run_c_tier(label: str, args: list[str]) -> dict[str, Any]:
    out = subprocess.check_output([str(BIN_PATH), *args], text=True, timeout=30).strip()
    data = json.loads(out)
    if not data.get("integrity_ok"):
        raise RuntimeError(f"[{label}] SHM integrity verification failed: {data!r}")
    data["label"] = label
    data["budget_165hz_us"] = round(FRAME_BUDGET_165HZ_US, 2)
    data["p50_within_165hz"] = data["p50_us"] <= FRAME_BUDGET_165HZ_US
    data["p95_within_165hz"] = data["p95_us"] <= FRAME_BUDGET_165HZ_US
    data["p99_within_165hz"] = data["p99_us"] <= FRAME_BUDGET_165HZ_US
    return data


def _run_t0_python_baseline(iterations: int) -> dict[str, Any]:
    proto_json = ROOT / "tools" / "compat_bench" / "results_microvm_gpu_proto.json"
    t0 = time.monotonic()
    ru_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    subprocess.check_call(
        [
            sys.executable,
            str(PROTO_DIR / "proto_bench.py"),
            "--iterations",
            str(iterations),
            "--json-out",
            str(proto_json),
        ],
        stdout=subprocess.DEVNULL,
    )
    wall = max(1e-6, time.monotonic() - t0)
    ru_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu_s = (
        (ru_after.ru_utime - ru_before.ru_utime) + (ru_after.ru_stime - ru_before.ru_stime)
    )
    proto_data = json.loads(proto_json.read_text(encoding="utf-8"))
    b1080 = proto_data["benchmarks"]["frame_1080p_xrgb"]
    return {
        "label": "T0_python_copied_shm_1080p",
        "mode": "python_copied_shm",
        "iterations": b1080["iterations"],
        "integrity_ok": b1080["crc_and_ack_ok"],
        "logical_frame_bytes": b1080["payload_bytes"],
        "copied_payload_bytes": b1080["payload_bytes"] * 2,
        "min_us": b1080["min_us"],
        "p50_us": b1080["p50_us"],
        "p95_us": b1080["p95_us"],
        "p99_us": b1080["p99_us"],
        "mean_us": b1080["mean_us"],
        "copy_throughput_mib_s": b1080["throughput_mib_s"],
        "effective_1080p_mib_s": b1080["throughput_mib_s"],
        "cpu_util_pct": round((cpu_s / wall) * 100.0, 1),
        "max_rss_kib": ru_after.ru_maxrss,
        "budget_165hz_us": round(FRAME_BUDGET_165HZ_US, 2),
        "p50_within_165hz": b1080["p50_us"] <= FRAME_BUDGET_165HZ_US,
        "p95_within_165hz": b1080["p95_us"] <= FRAME_BUDGET_165HZ_US,
        "p99_within_165hz": b1080["p99_us"] <= FRAME_BUDGET_165HZ_US,
    }


def run_suite(c_iters: int = 120, py_iters: int = 40) -> dict[str, Any]:
    _compile_native_bench()
    tiers: list[dict[str, Any]] = [
        _run_t0_python_baseline(py_iters),
        _run_c_tier("T1_native_double_eventfd_1080p", ["double_eventfd", str(c_iters)]),
        _run_c_tier("T2_native_triple_async_ring_1080p", ["triple_async", str(c_iters)]),
        _run_c_tier("T3_dirty_rect_5pct_1080p", ["dirty_rect", str(max(1200, c_iters)), "5"]),
        _run_c_tier("T3_dirty_rect_15pct_1080p", ["dirty_rect", str(max(600, c_iters)), "15"]),
        _run_c_tier("T3_dirty_rect_25pct_1080p", ["dirty_rect", str(max(400, c_iters)), "25"]),
        _run_c_tier("T4_spin_futex_recycle_1080p", ["spin_futex_full", str(c_iters)]),
        _run_c_tier("T4b_spin_futex_dirty_15pct", ["spin_futex_d15", str(max(600, c_iters))]),
        _run_c_tier("T5_host_shm_zerocopy_idx_1080p", ["zerocopy_idx", str(max(50000, c_iters))]),
    ]
    return {
        "suite": "ascension-7.0-gpu-ipc-benchmark",
        "resolution": "1920x1080x4",
        "frame_bytes": 1920 * 1080 * 4,
        "target_refresh_hz": 165,
        "frame_budget_us": round(FRAME_BUDGET_165HZ_US, 2),
        "honest_boundaries": {
            "cross_vm_guest_dmabuf_zero_copy": False,
            "t5_zero_copy_scope": "host_process_posix_shm_slot_index_only",
            "physical_gpu_hardware_present": False,
        },
        "all_integrity_ok": all(t["integrity_ok"] for t in tiers),
        "tiers": tiers,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Run Ascension 7.0 Milestone F GPU IPC benchmark suite")
    parser.add_argument("--c-iters", type=int, default=120)
    parser.add_argument("--py-iters", type=int, default=40)
    parser.add_argument("--json-out", type=pathlib.Path, default=DEFAULT_OUT)
    args = parser.parse_args()

    res = run_suite(c_iters=args.c_iters, py_iters=args.py_iters)
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")

    for t in res["tiers"]:
        print(
            f"[GPU-IPC-A7] {t['label']:34s} p50={t['p50_us']:8.2f}us p95={t['p95_us']:8.2f}us "
            f"p99={t['p99_us']:8.2f}us eff={t['effective_1080p_mib_s']:9.1f}MiB/s "
            f"cpu={t['cpu_util_pct']:5.1f}% rss={t['max_rss_kib']}KiB 165Hz_p95={t['p95_within_165hz']}"
        )
    if not res["all_integrity_ok"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
