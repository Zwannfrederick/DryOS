#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""M3 Universal Linux Network Driver Island E2E Validator (`--p14-only`, `8139cp.c` + `mii.c`)."""

from __future__ import annotations

import pathlib
import socket
import time
from typing import Callable

from qemu_p2_validators import _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args


def _alloc_free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _parse_kv_any(line: str) -> dict[str, int]:
    out: dict[str, int] = {}
    for tok in line.split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            try:
                out[k] = int(v, 0)
            except ValueError:
                pass
    return out


def _probe_slirp_udp_echo(host_port: int, payload: bytes = b"DRYOS-LX-NET-PING", timeout_s: float = 6.0) -> bytes:
    deadline = time.monotonic() + timeout_s
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(0.20)
        while time.monotonic() < deadline:
            sock.sendto(payload, ("127.0.0.1", host_port))
            try:
                data, _addr = sock.recvfrom(1024)
                if data:
                    return data
            except TimeoutError:
                time.sleep(0.05)
    raise RuntimeError(f"Timed out waiting for SLIRP UDP echo reply from 10.0.2.15:7777 via 127.0.0.1:{host_port}")


def run_p14a_lx_net_8139cp_slirp_recovery(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> dict[str, int]:
    disk_path = build_dir / "disk_p14_lx_net.img"
    _create_raw_disk(disk_path, size_mib=4)
    host_udp_port = _alloc_free_udp_port()
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_blk_args(disk_path)
        + [
            "-device",
            "rtl8139,netdev=n0",
            "-netdev",
            f"user,id=n0,hostfwd=udp:127.0.0.1:{host_udp_port}-10.0.2.15:7777",
        ],
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-LX-NET] UDP_LISTEN_READY port=7777 gen=1", wait_s=20.0)
        echo_reply = _probe_slirp_udp_echo(host_udp_port)
        if echo_reply != b"DRYOS-LX-NET-PING":
            raise RuntimeError(f"[{name}] Unexpected UDP echo payload: {echo_reply!r}")

        pass1 = h.wait_for("[DRY-LX-NET-PASS]", wait_s=15.0)
        pass2 = h.wait_for("[DRY-LX-NET-PASS2]", wait_s=10.0)
        h.wait_for("[DRY-BROKER-LIFECYCLE] step=UNLOADED", wait_s=10.0)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
    finally:
        h.close()

    for required in (
        "[DRY-LX-NET] 8139cp.c+mii.c probed vdev=0x10ec8139",
        "[DRY-LX-NET] Ready in CPL3 RootSystem [0x0214] gen=1",
        "[DRY-LX-NET] Injecting controlled CPL3 #UD fault gen=1",
        "[DRY-LX-NET] Ready in CPL3 RootSystem [0x0214] gen=2",
        "[DRY-BROKER-LIFECYCLE] step=UNLOADED backend=lx-compat-island",
    ):
        if required not in serial_out:
            raise RuntimeError(f"[{name}] Missing required serial marker {required!r}")
    lc_lines = [ln for ln in serial_out.splitlines() if "[DRY-BROKER-LIFECYCLE]" in ln]
    if not lc_lines or _parse_kv_any(lc_lines[-1]).get("comp") != 0x0214:
        raise RuntimeError(f"[{name}] Missing/unexpected comp_id in [DRY-BROKER-LIFECYCLE]: {lc_lines!r}")

    kv1 = _parse_kv_any(pass1)
    kv2 = _parse_kv_any(pass2)
    if (
        kv1.get("vendor_id") != 0x10EC
        or kv1.get("device_id") != 0x8139
        or kv1.get("rev", 0) < 0x20
        or kv1.get("mac", 0) == 0
        or kv1.get("tx_pkts", 0) <= 0
        or kv1.get("rx_pkts", 0) <= 0
        or kv1.get("udp_echo", 0) < 1
        or kv1.get("vec") != 6
        or kv1.get("restarts", 0) < 1
    ):
        raise RuntimeError(f"[{name}] Unexpected [DRY-LX-NET-PASS] metrics: {kv1}")

    if (
        (kv2.get("drvctl_visited", 0) & 0x1FFF) != 0x1FFF
        or kv2.get("ep_rev") != 1
        or kv2.get("ring_rev") != 1
        or kv2.get("stale_gen") != 1
        or kv2.get("persist_ok") != 1
        or kv2.get("vault_ok") != 1
        or skv.get("leaked_frames", 1) != 0
    ):
        raise RuntimeError(f"[{name}] Unexpected recovery/containment metrics: kv2={kv2} skv={skv}")

    dt = time.monotonic() - t0
    print(
        f"[PASS] {name}: 8139cp+mii mac=0x{kv1['mac']:012x} bar1_suboff={kv1.get('bar1_suboff', 0)} "
        f"tx={kv1['tx_pkts']} rx={kv1['rx_pkts']} udp_echo={kv1['udp_echo']} "
        f"arp={kv1.get('arp', 0)} vec={kv1['vec']} restarts={kv1['restarts']} "
        f"drvctl=0x{kv2['drvctl_visited']:x} leaked_frames=0 ({dt:.2f}s)."
    )
    return {**kv1, **kv2, "leaked_frames": skv["leaked_frames"]}


def run_p14_lx_net_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    _root: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[[pathlib.Path, pathlib.Path, str], None],
) -> int:
    esp_p14 = build_dir / "esp_p14_lx_net.img"
    write_mbr_esp_image(esp_p14, kernel_elf, "console=com1 desktop4=1 lx_net_test=1")
    run_p14a_lx_net_8139cp_slirp_recovery("p14a_lx_net_8139cp", esp_p14, ovmf_code, ovmf_vars, build_dir)
    return 1
