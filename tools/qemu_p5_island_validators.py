#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone 2C-0 Adaptive Driver Islands Lifecycle PoC (`ADR-016`):
`Device Broker` (`0x0008`), Isolated `Mock Driver Island` (`0x0209`, `SecDomain::Userspace`),
Capability Isolation, `#UD` Crash/Restart Recovery, Stale Capability Rejection (`-EBADCAP` /
`-EREVOKED`), Non-Interference with `dry-blk` / `dry-vfs` / `dry-vault`, and Honest QEMU/KVM
Latency / Wakeup / Recovery / Idle / Memory / `IoRing` Batching Measurements."""

import json
import pathlib
import time
from typing import Callable

from qemu_log_validators import _parse_kv_ints
from qemu_p2_validators import _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args


def _deterministic_mock_transform_ref(seed: int, word0: int, word1: int, gen: int) -> int:
    """Reference 64-bit SplitMix64 + IEEE CRC32 fold matching `userspace/src/island_proto.rs`."""
    mask64 = 0xFFFFFFFFFFFFFFFF
    x = (seed ^ ((gen & mask64) * 0x9E3779B97F4A7C15)) & mask64
    for w in (word0 & mask64, word1 & mask64):
        z = (x + w + 0x9E3779B97F4A7C15) & mask64
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & mask64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & mask64
        x = (z ^ (z >> 31)) & mask64

    crc = 0xFFFFFFFF
    payload = (
        seed.to_bytes(8, "little")
        + word0.to_bytes(8, "little")
        + word1.to_bytes(8, "little")
        + (gen & mask64).to_bytes(8, "little")
    )
    for b in payload:
        crc ^= b
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xEDB88320
            else:
                crc >>= 1
    crc ^= 0xFFFFFFFF
    return ((x & 0xFFFFFFFF00000000) | (crc & 0xFFFFFFFF)) & mask64


def run_p5a_driver_island_broker_lifecycle_and_stale_cap_e2e(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Milestone 2C-0 E2E Lifecycle & Isolation Proof:
    1. Boots `desktop4=1 island_poc=1` with live VirtIO-Blk (`/persist`) + `DRY Vault`.
    2. Verifies `Device Broker` (`0x0008`) + `Mock Driver Island` (`0x0209`, `SecDomain::Userspace`)
       spawn (`gen=1`), MAC `SYS_PROC_KILL -> -EPERM`, deterministic `Endpoint` + `IoRingSharedPage`
       digest match against Python reference `_deterministic_mock_transform_ref`.
    3. Verifies rogue badge (`BADGE_BROKER_ROGUE`) and unbadged requests are rejected (`-EPERM`).
    4. Injects controlled `#UD` (`vec=6`) into `dry-mock-drv`, verifies `dry-broker` reaps and
       restarts `gen=2`, and proves stale `gen=1` `drv_ep`, `sq_notif`, and `ring_cap` fail with
       `-EBADCAP` (`-9`) while stale generation tokens fail with `-EREVOKED` (`-110`).
    5. Verifies `/persist` (`dry-vfs` + `dry-blk`) and `dry-vault` remain 100% healthy after island
       crash/recovery, then shuts down cleanly with `leaked_frames == 0`."""
    disk_path = build_dir / "disk_p5a_island.img"
    _create_raw_disk(disk_path, size_mib=4)
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()
        dig_ln = h.wait_for("[DRY-ISLAND-POC-DIGEST]", wait_s=12.0)
        poc_ln = h.wait_for("[DRY-ISLAND-POC] PASS:", wait_s=12.0)
        dkv = _parse_kv_ints(dig_ln)
        pkv = _parse_kv_ints(poc_ln)

        exp_gen1 = _deterministic_mock_transform_ref(0x4452593243303031, 0x1122334455667788, 0x99AABBCCDDEEFF00, 1)
        exp_gen2 = _deterministic_mock_transform_ref(0x4452593243303031, 0x1122334455667788, 0x99AABBCCDDEEFF00, 2)

        if dkv.get("gen1_digest", -1) != exp_gen1 or dkv.get("gen2_digest", -1) != exp_gen2:
            raise RuntimeError(
                f"[{name}] Deterministic digest mismatch: got ({dkv.get('gen1_digest')}, {dkv.get('gen2_digest')}) "
                f"!= expected ({exp_gen1}, {exp_gen2}) in {dig_ln!r}"
            )
        if (
            pkv.get("mac") != 1
            or pkv.get("unauth", 0) < 4
            or pkv.get("vec") != 6
            or pkv.get("restarts", 0) < 1
            or pkv.get("ep_rev") != 1
            or pkv.get("ntf_rev") != 1
            or pkv.get("cq_rev") != 1
            or pkv.get("rng_rev") != 1
            or pkv.get("gen_rev") != 1
            or pkv.get("batch") != 1
            or pkv.get("sv_ok") != 1
        ):
            raise RuntimeError(f"[{name}] Incomplete lifecycle/isolation proof fields: {poc_ln!r}")

        # Verify interactive `Terminal` + live VirtIO-Blk `/persist` write/flush/readback + `dry-vault` after PoC!
        idx = len(h.serial_lines)
        qmp.type_ascii("write /persist/shell-post.txt post-island-ok")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=write path=/persist/shell-post.txt bytes=14", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("sync")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=sync tx_seq=", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("cat /persist/shell-post.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=cat path=/persist/shell-post.txt bytes=14 text="post-island-ok"', start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("vault-status")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=vault-status unlocked=true", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")
        if "[DRY-BROKER-STATS]" not in serial_out:
            raise RuntimeError(f"[{name}] Missing [DRY-BROKER-STATS] clean teardown telemetry")

        print(
            f"[PASS] {name}: gen1_ok=1 crash_vec=6 restarts={pkv['restarts']} "
            f"unauth_denied={pkv['unauth']} stale_revoked=5 leaked_frames=0 ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)


def run_p5b_driver_island_latency_wakeup_batch_bench_e2e(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    """Milestone 2C-0 Experimental Benchmark Suite (`AŞAMA 2.3`):
    Captures real QEMU/KVM (`2 vCPU`) measurements for:
    - Direct capability-leased `Endpoint` request/reply `p50 / p95 / p99` cycles
    - Broker-mediated `Endpoint` request/reply `p50 / p95 / p99` cycles
    - Idle-to-active wakeup (`HLT` -> island wake) `p50 / p95 / p99` cycles
    - `#UD` service crash-to-recovery latency (`4` consecutive restarts)
    - Quiescent idle CPU/syscall activity (`broker_idle_syscalls == 0`, `island_idle_syscalls == 0`, `hlt_calls > 0`)
    - Extra memory/frame footprint (`broker_pages_4k`, `island_pages_4k`, `pmm_delta_4k`, `free_leak_4k == 0`)
    - Unbatched (`batch=1`) vs batched (`batch=16`, `batch=32`) `IoRingSharedPage` `cyc/op`."""
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
    )
    try:
        qmp = h.boot_to_desktop_ready()
        ipc_ln = h.wait_for("[DRY-ISLAND-BENCH-IPC]", wait_s=15.0)
        wake_ln = h.wait_for("[DRY-ISLAND-BENCH-WAKE]", wait_s=15.0)
        rec_ln = h.wait_for("[DRY-ISLAND-BENCH-RECOVER]", wait_s=15.0)
        idle_ln = h.wait_for("[DRY-ISLAND-BENCH-IDLE]", wait_s=15.0)
        mem_ln = h.wait_for("[DRY-ISLAND-BENCH-MEM]", wait_s=15.0)
        ring_ln = h.wait_for("[DRY-ISLAND-BENCH-RING]", wait_s=15.0)

        ikv = _parse_kv_ints(ipc_ln)
        wkv = _parse_kv_ints(wake_ln)
        rkv = _parse_kv_ints(rec_ln)
        dkv = _parse_kv_ints(idle_ln)
        mkv = _parse_kv_ints(mem_ln)
        bkv = _parse_kv_ints(ring_ln)

        if ikv.get("d_p50", 0) <= 0 or ikv.get("b_p50", 0) <= 0:
            raise RuntimeError(f"[{name}] Invalid IPC latency telemetry: {ipc_ln!r}")
        if wkv.get("w1_p50", 0) <= 0 or wkv.get("wr_p50", 0) <= 0 or wkv.get("hlt_ok", 0) < 1:
            raise RuntimeError(f"[{name}] Invalid idle-wakeup telemetry: {wake_ln!r}")
        if rkv.get("p50_cyc", 0) <= 0 or rkv.get("runs", 0) < 4:
            raise RuntimeError(f"[{name}] Invalid crash-recovery telemetry: {rec_ln!r}")
        if (
            dkv.get("brk_sys", -1) != 0
            or dkv.get("isl_sys", -1) != 0
            or dkv.get("hlt_calls", 0) <= 0
        ):
            raise RuntimeError(f"[{name}] Expected zero idle syscalls & positive HLT residency: {idle_ln!r}")
        if mkv.get("pmm_leak", -1) != 0 or mkv.get("pmm_act", 0) <= 0:
            raise RuntimeError(f"[{name}] Invalid memory/frame cost telemetry: {mem_ln!r}")
        if bkv.get("b32_p50", 0) <= 0 or bkv.get("b32_p50", 999999) >= bkv.get("b1_p50", 0):
            raise RuntimeError(f"[{name}] Expected batch=32 cyc/op < batch=1 cyc/op: {ring_ln!r}")

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        _, skv = h.finish_and_verify(require_mouse_irqs=False)
        if skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[{name}] Expected leaked_frames=0, got {skv!r}")

        out_json = root_dir / "tools" / "compat_bench" / "results_poc_2c0_island.json"
        out_json.parent.mkdir(parents=True, exist_ok=True)
        out_json.write_text(
            json.dumps(
                {
                    "experiment": "POC-2C0",
                    "environment": "DRY OS Ring 3 QEMU/KVM q35 2 vCPU (Milestone 2C-0)",
                    "ipc": ikv,
                    "wakeup": wkv,
                    "recovery": rkv,
                    "idle": dkv,
                    "memory": mkv,
                    "ioring_batch": bkv,
                },
                indent=2,
            )
            + "\n"
        )

        print(
            f"[PASS] {name}: direct_p50={ikv['d_p50']}cyc broker_p50={ikv['b_p50']}cyc "
            f"wake1w_p50={wkv['w1_p50']}cyc recover_p50={rkv['p50_us']}us "
            f"b1={bkv['b1_p50']}->b32={bkv['b32_p50']}cycop pmm_delta={mkv['pmm_act']}fr "
            f"leaked_frames=0 ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h.close()


def run_p5_island_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[[pathlib.Path, pathlib.Path, str], None],
) -> int:
    """Execute Milestone 2C-0 Adaptive Driver Islands PoC lifecycle & benchmark scenarios."""
    esp_poc = build_dir / "esp_p5_island_poc.img"
    write_mbr_esp_image(esp_poc, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1 island_poc=1")

    esp_bench = build_dir / "esp_p5_island_bench.img"
    write_mbr_esp_image(esp_bench, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1 island_bench=1")

    run_p5a_driver_island_broker_lifecycle_and_stale_cap_e2e(
        "p5a_driver_island_broker_lifecycle_and_stale_cap_e2e",
        esp_poc,
        ovmf_code,
        ovmf_vars,
        build_dir,
    )
    run_p5b_driver_island_latency_wakeup_batch_bench_e2e(
        "p5b_driver_island_latency_wakeup_batch_bench_e2e",
        esp_bench,
        ovmf_code,
        ovmf_vars,
        build_dir,
        root_dir,
    )
    return 2
