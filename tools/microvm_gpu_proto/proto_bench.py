#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Disposable Linux MicroVM GPU Transport Prototype & IPC Benchmark (`Milestone 5`).

Measures a real 2-process shared-memory ring + pipe-doorbell copied transport
(`SHARED_MEMORY_IPC_COPIED`, `zero_copy_dmabuf=false`) and audits host KVM/QEMU
prerequisites read-only. Does NOT assume physical iGPU passthrough or zero-copy
guest DMA-BUF export to the host compositor.
"""

from __future__ import annotations

import argparse
import glob
import json
import mmap
import os
import pathlib
import platform
import shutil
import statistics
import struct
import tempfile
import time
import zlib
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_OUT = ROOT / "tools" / "compat_bench" / "results_microvm_gpu_proto.json"

HDR_FMT = "<QQII"  # seq, payload_len, expected_crc32, flags
HDR_SIZE = struct.calcsize(HDR_FMT)


def _pct(sorted_vals: list[float], pct: float) -> float:
    if not sorted_vals:
        return 0.0
    idx = min(len(sorted_vals) - 1, max(0, int((len(sorted_vals) - 1) * pct / 100.0)))
    return round(sorted_vals[idx], 2)


def probe_host_microvm_prereqs() -> dict[str, Any]:
    """Read-only probe of host KVM, QEMU, and Linux MicroVM boot readiness."""
    kvm_exists = pathlib.Path("/dev/kvm").exists()
    kvm_rw = os.access("/dev/kvm", os.R_OK | os.W_OK)
    qemu_bin = shutil.which("qemu-system-x86_64")
    kernels = sorted(glob.glob("/boot/vmlinuz-*"))
    readable_kernels = [k for k in kernels if os.access(k, os.R_OK)]
    mem_total_mib = 0
    mem_avail_mib = 0
    meminfo = pathlib.Path("/proc/meminfo")
    if meminfo.is_file():
        for ln in meminfo.read_text(encoding="utf-8", errors="replace").splitlines():
            if ln.startswith("MemTotal:"):
                mem_total_mib = int(ln.split()[1]) // 1024
            elif ln.startswith("MemAvailable:"):
                mem_avail_mib = int(ln.split()[1]) // 1024
    return {
        "host_arch": platform.machine(),
        "dev_kvm_exists": kvm_exists,
        "dev_kvm_rw": kvm_rw,
        "qemu_system_x86_64": qemu_bin,
        "host_kernels_listed": len(kernels),
        "host_kernels_readable": len(readable_kernels),
        "host_mem_total_mib": mem_total_mib,
        "host_mem_available_mib": mem_avail_mib,
        "estimated_microvm_ram_mib": 128,
        "estimated_cold_boot_ms_range": "250-900 (with minimal initramfs; host full kernel often requires dedicated microvm image)",
        "physical_igpu_passthrough_assumed": False,
        "transport_mode": "SHARED_MEMORY_IPC_COPIED",
    }


def _bench_payload(payload_bytes: int, iterations: int = 120, warmup: int = 8) -> dict[str, Any]:
    ring_bytes = ((HDR_SIZE + payload_bytes + 4095) // 4096) * 4096
    pattern = bytes((i * 31 + 7) & 0xFF for i in range(65536))
    src_frame = (pattern * ((payload_bytes // len(pattern)) + 1))[:payload_bytes]

    fd = tempfile.TemporaryFile()
    os.ftruncate(fd.fileno(), ring_bytes)
    shm = mmap.mmap(fd.fileno(), ring_bytes, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE)

    r_req, w_req = os.pipe()
    r_ack, w_ack = os.pipe()
    total_runs = warmup + iterations
    pid = os.fork()

    if pid == 0:
        # Compositor consumer child: reads copied frame bytes from shared memory ring & verifies CRC32.
        os.close(w_req)
        os.close(r_ack)
        local_fb = bytearray(payload_bytes)
        crc_errs = 0
        try:
            for _ in range(total_runs):
                tok = os.read(r_req, 1)
                if not tok:
                    break
                seq, length, expect_crc, _ = struct.unpack_from(HDR_FMT, shm, 0)
                if length != payload_bytes:
                    crc_errs += 1
                    os.write(w_ack, b"\x00")
                    continue
                shm.seek(HDR_SIZE)
                frame_view = shm.read(length)
                local_fb[:length] = frame_view
                actual_crc = zlib.crc32(local_fb[:length]) & 0xFFFFFFFF
                if actual_crc != expect_crc or seq == 0:
                    crc_errs += 1
                    os.write(w_ack, b"\x00")
                else:
                    os.write(w_ack, b"\x01")
        finally:
            shm.close()
            os.close(r_req)
            os.close(w_ack)
            os._exit(0 if crc_errs == 0 else 2)

    # MicroVM GPU producer parent: writes copied frames + CRC32 and awaits doorbell ack.
    os.close(r_req)
    os.close(w_ack)
    lat_us: list[float] = []
    ack_failures = 0
    try:
        for i in range(total_runs):
            mut_src = bytearray(src_frame)
            mut_src[0] = (mut_src[0] + i) & 0xFF
            mut_src[-1] = (mut_src[-1] ^ (i & 0xFF)) & 0xFF
            crc = zlib.crc32(mut_src) & 0xFFFFFFFF

            t0 = time.perf_counter_ns()
            struct.pack_into(HDR_FMT, shm, 0, i + 1, payload_bytes, crc, 1)
            shm.seek(HDR_SIZE)
            shm.write(mut_src)
            os.write(w_req, b"\x01")
            ack = os.read(r_ack, 1)
            dt_us = (time.perf_counter_ns() - t0) / 1000.0
            if ack != b"\x01":
                ack_failures += 1
            if i >= warmup:
                lat_us.append(dt_us)
    finally:
        os.close(w_req)
        os.close(r_ack)
        shm.close()
        fd.close()
        _, status = os.waitpid(pid, 0)

    child_ok = os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0
    sorted_us = sorted(lat_us)
    mean_us = round(statistics.mean(sorted_us), 2) if sorted_us else 0.0
    total_sec = sum(sorted_us) / 1_000_000.0
    mib_per_s = round(((payload_bytes * len(sorted_us)) / (1024.0 * 1024.0)) / max(total_sec, 1e-6), 2)
    return {
        "payload_bytes": payload_bytes,
        "iterations": iterations,
        "crc_and_ack_ok": child_ok and ack_failures == 0,
        "ack_failures": ack_failures,
        "min_us": round(sorted_us[0], 2) if sorted_us else 0.0,
        "p50_us": _pct(sorted_us, 50.0),
        "p95_us": _pct(sorted_us, 95.0),
        "p99_us": _pct(sorted_us, 99.0),
        "mean_us": mean_us,
        "throughput_mib_s": mib_per_s,
    }


def run_benchmark(iterations: int = 120) -> dict[str, Any]:
    prereqs = probe_host_microvm_prereqs()
    profiles = {
        "dirty_tile_256x256_xrgb": _bench_payload(256 * 256 * 4, iterations=iterations),
        "frame_720p_xrgb": _bench_payload(1280 * 720 * 4, iterations=iterations),
        "frame_1080p_xrgb": _bench_payload(1920 * 1080 * 4, iterations=iterations),
    }
    all_ok = all(p["crc_and_ack_ok"] for p in profiles.values())
    return {
        "prototype": "dryos-linux-microvm-gpu-ipc-proto",
        "transport_mode": "SHARED_MEMORY_IPC_COPIED",
        "zero_copy_dmabuf": False,
        "physical_igpu_passthrough_assumed": False,
        "passed": all_ok,
        "host_prereqs": prereqs,
        "benchmarks": profiles,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="DRY OS MicroVM GPU Shared-Memory IPC Benchmark")
    parser.add_argument("--iterations", type=int, default=120)
    parser.add_argument("--json-out", type=pathlib.Path, default=DEFAULT_OUT)
    args = parser.parse_args()

    res = run_benchmark(iterations=max(20, args.iterations))
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.json_out.write_text(json.dumps(res, indent=2) + "\n", encoding="utf-8")

    b_tile = res["benchmarks"]["dirty_tile_256x256_xrgb"]
    b_720 = res["benchmarks"]["frame_720p_xrgb"]
    b_1080 = res["benchmarks"]["frame_1080p_xrgb"]
    print(
        f"[MICROVM-GPU-PROTO] {'PASS' if res['passed'] else 'FAIL'}: "
        f"mode={res['transport_mode']} zero_copy={res['zero_copy_dmabuf']} "
        f"kvm_rw={res['host_prereqs']['dev_kvm_rw']} "
        f"tile_p50={b_tile['p50_us']}us({b_tile['throughput_mib_s']}MiB/s) "
        f"720p_p50={b_720['p50_us']}us({b_720['throughput_mib_s']}MiB/s) "
        f"1080p_p50={b_1080['p50_us']}us({b_1080['throughput_mib_s']}MiB/s)"
    )
    if not res["passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
