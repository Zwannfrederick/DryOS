#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Ascension 9.0 Stabilization, VFS Exec, Net/Linux-ABI & Ergonomics Validators (`--p19-only`)."""

from __future__ import annotations

import json
import pathlib
import socket
import threading
import time
from typing import Any, Callable

from qemu_p2_validators import _DesktopSessionHarness
from qemu_p4_validators import _create_raw_disk, _virtio_blk_args


def _run_p19a_soak_and_stability(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    disk_path = build_dir / "disk_p19a_soak.img"
    _create_raw_disk(disk_path, size_mib=4)
    esp_img = build_dir / "esp_p19a_soak.img"
    write_mbr_esp_image(
        esp_img,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        "p19a_soak_and_stability",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()

        # 1. Rapid multi-cycle app launch/close soak across apps.
        ready_markers = {
            "editor": "[DRY-EDITOR] Ready in CPL3 Userspace",
            "calc": "[DRY-CALC] Ready in CPL3 Userspace",
            "files": "[DRY-FILES] Ready in CPL3 Userspace",
        }
        app_cycles = 0
        for app in ("editor", "calc", "files", "editor", "calc"):
            idx = len(h.serial_lines)
            qmp.type_ascii(f"open {app}")
            qmp.send_qcodes(["ret"])
            h.wait_for(f"[DRY-SHELL] CMD=open app={app}", start_idx=idx, wait_s=12.0)
            h.wait_for(ready_markers[app], start_idx=idx, wait_s=12.0)

            # `open <app>` focuses the launched app window; Alt+1 returns focus to terminal.
            idx_focus = len(h.serial_lines)
            qmp.send_qcodes(["alt", "1"])
            h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx_focus, wait_s=12.0)

            idx = len(h.serial_lines)
            qmp.type_ascii(f"close {app}")
            qmp.send_qcodes(["ret"])
            h.wait_for(f"[DRY-SHELL] CMD=close app={app}", start_idx=idx, wait_s=12.0)
            app_cycles += 1

        # 2. Rapid /ram + /persist CoW write/sync/cat/rm/overwrite cycles.
        for cmd, marker in (
            ("write /ram/soak1.txt r1", "[DRY-SHELL] CMD=write path=/ram/soak1.txt bytes=2"),
            ("write /persist/soak1.txt p1", "[DRY-SHELL] CMD=write path=/persist/soak1.txt bytes=2"),
            ("write /persist/soak2.txt p2-val", "[DRY-SHELL] CMD=write path=/persist/soak2.txt bytes=6"),
            ("sync", "[DRY-SHELL] CMD=sync tx_seq="),
            ("cat /persist/soak2.txt", '[DRY-SHELL] CMD=cat path=/persist/soak2.txt bytes=6 text="p2-val"'),
            ("rm /ram/soak1.txt", "[DRY-SHELL] CMD=rm path=/ram/soak1.txt ok=true"),
            ("rm /persist/soak2.txt", "[DRY-SHELL] CMD=rm path=/persist/soak2.txt ok=true"),
            ("write /persist/soak1.txt p1-updated", "[DRY-SHELL] CMD=write path=/persist/soak1.txt bytes=10"),
            ("sync", "[DRY-SHELL] CMD=sync tx_seq="),
        ):
            idx = len(h.serial_lines)
            qmp.type_ascii(cmd)
            qmp.send_qcodes(["ret"])
            h.wait_for(marker, start_idx=idx, wait_s=12.0)

        # 3. Repeated input server #UD crash/recovery soak.
        crash_cycles = 0
        for tag in ("after-crash-1", "after-crash-2"):
            idx = len(h.serial_lines)
            qmp.type_ascii("crash-input")
            qmp.send_qcodes(["ret"])
            h.wait_for("[DRY-SHELL] INPUT_RECOVERED ok=true", start_idx=idx, wait_s=15.0)

            idx = len(h.serial_lines)
            qmp.type_ascii(f"echo {tag}")
            qmp.send_qcodes(["ret"])
            h.wait_for(f'[DRY-SHELL] CMD=echo text="{tag}"', start_idx=idx, wait_s=12.0)
            crash_cycles += 1

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if "[DRY-PANIC]" in serial_out or skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p19a] Panic or frame leak detected: {skv!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(
            f"[PASS] p19a_soak_and_stability: app_cycles={app_cycles} crash_cycles={crash_cycles} "
            f"leaked_frames=0 ({elapsed:.2f}s)."
        )
        return {
            "app_cycles": app_cycles,
            "crash_cycles": crash_cycles,
            "leaked_frames": skv["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)


def _run_p19b_vfs_tree_and_dynamic_exec(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    disk_path = build_dir / "disk_p19b_vfs_exec.img"
    _create_raw_disk(disk_path, size_mib=8)
    esp_img = build_dir / "esp_p19b_vfs_exec.img"
    write_mbr_esp_image(
        esp_img,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    t0 = time.monotonic()
    probe_bytes = 0

    def run_cmd(h: _DesktopSessionHarness, qmp: Any, cmd: str, marker: str, wait_s: float = 15.0) -> str:
        idx = len(h.serial_lines)
        qmp.type_ascii(cmd)
        qmp.send_qcodes(["ret"])
        return h.wait_for(marker, start_idx=idx, wait_s=wait_s)

    h1 = _DesktopSessionHarness(
        "p19b_boot1",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp = h1.boot_to_desktop_ready()
        h1.wait_for("[DRY-INIT] Mounted /bin USTAR bytes=", wait_s=12.0)

        run_cmd(h1, qmp, "mkdir /ram/a", "[DRY-SHELL] CMD=mkdir path=/ram/a ok=true")
        run_cmd(h1, qmp, "mkdir /ram/a/b", "[DRY-SHELL] CMD=mkdir path=/ram/a/b ok=true")
        run_cmd(h1, qmp, "cd /ram/a/b", "[DRY-SHELL] CMD=cd cwd=/ram/a/b")
        run_cmd(h1, qmp, "pwd", "[DRY-SHELL] CMD=pwd cwd=/ram/a/b")
        run_cmd(h1, qmp, "write note.txt hi", "[DRY-SHELL] CMD=write path=/ram/a/b/note.txt bytes=2")
        run_cmd(
            h1,
            qmp,
            "cat /ram/a/b/note.txt",
            '[DRY-SHELL] CMD=cat path=/ram/a/b/note.txt bytes=2 text="hi"',
        )
        run_cmd(h1, qmp, "cd /", "[DRY-SHELL] CMD=cd cwd=/")
        run_cmd(h1, qmp, "ls /bin", "[DRY-SHELL] CMD=ls path=/bin count=")
        run_cmd(h1, qmp, "mkdir /persist/sub", "[DRY-SHELL] CMD=mkdir path=/persist/sub ok=true")
        cp_ln = run_cmd(
            h1,
            qmp,
            "cp /bin/dry-probe.elf /persist/sub/probe.elf",
            "[DRY-SHELL] CMD=cp src=/bin/dry-probe.elf dst=/persist/sub/probe.elf bytes=",
            wait_s=20.0,
        )
        probe_bytes = int(cp_ln.split("bytes=")[1].split()[0])

        idx_exec = len(h1.serial_lines)
        qmp.type_ascii("exec /persist/sub/probe.elf")
        qmp.send_qcodes(["ret"])
        h1.wait_for("[DRY-PROBE] PASS: dyn_exec=1", start_idx=idx_exec, wait_s=15.0)
        h1.wait_for("[DRY-SHELL] CMD=exec path=/persist/sub/probe.elf rc=0 pid=", start_idx=idx_exec, wait_s=15.0)

        run_cmd(h1, qmp, "exec /ram/welcome.txt", "[DRY-SHELL] CMD=exec path=/ram/welcome.txt err=")
        run_cmd(h1, qmp, "sync", "[DRY-SHELL] CMD=sync tx_seq=")

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial1, skv1 = h1.finish_and_verify(require_mouse_irqs=False)
        if "[DRY-PANIC]" in serial1 or skv1.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p19b boot1] Panic or frame leak detected: {skv1!r}")
    finally:
        h1.close()

    h2 = _DesktopSessionHarness(
        "p19b_boot2",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp2 = h2.boot_to_desktop_ready()
        idx_exec2 = len(h2.serial_lines)
        qmp2.type_ascii("exec /persist/sub/probe.elf")
        qmp2.send_qcodes(["ret"])
        h2.wait_for("[DRY-PROBE] PASS: dyn_exec=1", start_idx=idx_exec2, wait_s=15.0)
        h2.wait_for("[DRY-SHELL] CMD=exec path=/persist/sub/probe.elf rc=0 pid=", start_idx=idx_exec2, wait_s=15.0)

        qmp2.type_ascii("shutdown")
        qmp2.send_qcodes(["ret"])
        serial2, skv2 = h2.finish_and_verify(require_mouse_irqs=False)
        if "[DRY-PANIC]" in serial2 or skv2.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p19b boot2] Panic or frame leak detected: {skv2!r}")

        elapsed = round(time.monotonic() - t0, 3)
        print(
            f"[PASS] p19b_vfs_tree_and_dynamic_exec: probe_bytes={probe_bytes} persist_exec_ok=true "
            f"leaked_frames=0 ({elapsed:.2f}s)."
        )
        return {
            "probe_bytes": probe_bytes,
            "persist_exec_ok": True,
            "leaked_frames": skv2["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        h2.close()
        disk_path.unlink(missing_ok=True)


def _start_local_dns_and_http_servers() -> tuple[int, int, threading.Event, list[threading.Thread]]:
    stop_evt = threading.Event()
    udp_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp_sock.bind(("127.0.0.1", 0))
    udp_sock.settimeout(0.15)
    dns_port = int(udp_sock.getsockname()[1])

    tcp_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    tcp_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    tcp_sock.bind(("127.0.0.1", 0))
    tcp_sock.listen(4)
    tcp_sock.settimeout(0.15)
    http_port = int(tcp_sock.getsockname()[1])

    def dns_worker() -> None:
        with udp_sock:
            while not stop_evt.is_set():
                try:
                    pkt, addr = udp_sock.recvfrom(512)
                except TimeoutError:
                    continue
                except OSError:
                    break
                if len(pkt) >= 12:
                    reply = (
                        pkt[0:2]
                        + b"\x81\x80\x00\x01\x00\x01\x00\x00\x00\x00"
                        + pkt[12:]
                        + b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04\x0a\x00\x02\x0f"
                    )
                    udp_sock.sendto(reply, addr)

    def http_worker() -> None:
        body = b"HELLO_DRY_NET"
        resp = (
            f"HTTP/1.0 200 OK\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode("ascii")
            + body
        )
        with tcp_sock:
            while not stop_evt.is_set():
                try:
                    conn, _ = tcp_sock.accept()
                except TimeoutError:
                    continue
                except OSError:
                    break
                with conn:
                    conn.settimeout(2.0)
                    try:
                        _ = conn.recv(1024)
                    except OSError:
                        pass
                    conn.sendall(resp)

    t_dns = threading.Thread(target=dns_worker, daemon=True)
    t_http = threading.Thread(target=http_worker, daemon=True)
    t_dns.start()
    t_http.start()
    return dns_port, http_port, stop_evt, [t_dns, t_http]


def _run_p19c_net_tcp_http_dns(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    disk_path = build_dir / "disk_p19c_net.img"
    _create_raw_disk(disk_path, size_mib=4)
    esp_img = build_dir / "esp_p19c_net.img"
    write_mbr_esp_image(
        esp_img,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    dns_port, http_port, stop_evt, threads = _start_local_dns_and_http_servers()
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        "p19c_net_tcp_http_dns",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path) + ["-device", "rtl8139,netdev=n0", "-netdev", "user,id=n0"],
    )
    try:
        qmp = h.boot_to_desktop_ready()

        idx = len(h.serial_lines)
        qmp.type_ascii("ifconfig")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-NET] Ready in CPL3 Userspace", start_idx=idx, wait_s=20.0)
        h.wait_for("[DRY-SHELL] CMD=ifconfig ip=10.0.2.15 gw=10.0.2.2", start_idx=idx, wait_s=20.0)

        idx = len(h.serial_lines)
        qmp.type_ascii("ping 10.0.2.2")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=ping ip=10.0.2.2 ok=2 rtt_us=", start_idx=idx, wait_s=15.0)

        idx = len(h.serial_lines)
        qmp.type_ascii(f"nslookup dryos.local 10.0.2.2:{dns_port}")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=nslookup host=dryos.local ip=10.0.2.15", start_idx=idx, wait_s=15.0)

        idx = len(h.serial_lines)
        qmp.type_ascii(f"httpget 10.0.2.2:{http_port}/hello")
        qmp.send_qcodes(["ret"])
        h.wait_for(
            '[DRY-SHELL] CMD=httpget status=200 bytes=13 body="HELLO_DRY_NET"',
            start_idx=idx,
            wait_s=15.0,
        )

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if "[DRY-PANIC]" in serial_out or skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p19c] Panic or frame leak: {skv!r}")
        elapsed = round(time.monotonic() - t0, 3)
        print(f"[PASS] p19c_net_tcp_http_dns: dhcp+icmp+dns+http200=HELLO_DRY_NET leaked_frames=0 ({elapsed:.2f}s).")
        return {
            "dns_port": dns_port,
            "http_port": http_port,
            "leaked_frames": skv["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        stop_evt.set()
        for t in threads:
            t.join(timeout=1.0)
        h.close()
        disk_path.unlink(missing_ok=True)


def _run_p19d_linux_abi_static_elf(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> dict[str, Any]:
    disk_path = build_dir / "disk_p19d_lxabi.img"
    _create_raw_disk(disk_path, size_mib=4)
    esp_img = build_dir / "esp_p19d_lxabi.img"
    write_mbr_esp_image(
        esp_img,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1",
        resolution="1280x800x32",
    )
    t0 = time.monotonic()
    h = _DesktopSessionHarness(
        "p19d_linux_abi_static_elf",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        mem="384M",
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-INIT] Mounted /bin USTAR bytes=", wait_s=12.0)

        idx = len(h.serial_lines)
        qmp.type_ascii("linux-run /bin/lx-hello.elf")
        qmp.send_qcodes(["ret"])
        done_ln = h.wait_for("[DRY-LXABI] Done pid=", start_idx=idx, wait_s=20.0)
        shell_ln = h.wait_for("[DRY-SHELL] CMD=linux-run path=/bin/lx-hello.elf rc=42 ", start_idx=idx, wait_s=20.0)
        if "LX_HELLO_PASS tls=1 brk=1 fd=1 os=DRY-OS" not in shell_ln:
            raise RuntimeError(f"[p19d] Unexpected linux-run output: {shell_ln!r}")

        idx_cat = len(h.serial_lines)
        qmp.type_ascii("cat /ram/lx_out.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for(
            '[DRY-SHELL] CMD=cat path=/ram/lx_out.txt bytes=35 text="HELLO_FROM_LINUX_ELF brk=OK tls=OK',
            start_idx=idx_cat,
            wait_s=15.0,
        )

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        if "[DRY-PANIC]" in serial_out or skv.get("leaked_frames", -1) != 0:
            raise RuntimeError(f"[p19d] Panic or frame leak: {skv!r}")
        syscalls_cnt = int(done_ln.split("syscalls=")[1].split()[0])
        elapsed = round(time.monotonic() - t0, 3)
        print(
            f"[PASS] p19d_linux_abi_static_elf: rc=42 tls=1 brk=1 syscalls={syscalls_cnt} "
            f"leaked_frames=0 ({elapsed:.2f}s)."
        )
        return {
            "rc": 42,
            "syscalls": syscalls_cnt,
            "leaked_frames": skv["leaked_frames"],
            "wall_time_sec": elapsed,
        }
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)


def run_p19_ascension9_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> int:
    print("\n=== Running Ascension 9.0 Stabilization & Maturity Suite (P19) ===")
    res_a = _run_p19a_soak_and_stability(kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)
    res_b = _run_p19b_vfs_tree_and_dynamic_exec(kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)
    res_c = _run_p19c_net_tcp_http_dns(kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)
    res_d = _run_p19d_linux_abi_static_elf(kernel_elf, build_dir, ovmf_code, ovmf_vars, write_mbr_esp_image)
    out_path = root_dir / "tools" / "compat_bench" / "results_p19_ascension9.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(
            {"milestone": "Ascension-9.0", "p19a": res_a, "p19b": res_b, "p19c": res_c, "p19d": res_d},
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return 4
