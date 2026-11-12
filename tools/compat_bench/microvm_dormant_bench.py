#!/usr/bin/env python3
"""
Exp-B2: Minimal Event-Driven Linux Guest MicroVM Benchmark on Host KVM.

Boots `/lib/modules/6.18.62-1-lts/vmlinuz` under `qemu-system-x86_64 -accel kvm`
with a custom ~20-line static C `/init` inside an ephemeral cpio initramfs over
`virtio-console` (`hvc0`) and `isa-serial` (`ttyS0`).

Measures:
  1. Cold start latency (`QEMU spawn -> guest /init READY` banner, 5 runs).
  2. Warm `HLT`-parked wakeup round-trip latency (`host write 1B -> KVM irqfd/virtqueue ->
     guest /init wake from poll/read -> 1B reply -> host read`, 100 iterations spaced
     by 15 ms so guest vCPU exits KVM halt-poll and genuinely enters `HLT` sleep).
  3. Quiescent idle host CPU consumption over a 3.0 s silent window (`/proc/<pid>/stat`).
  4. Host memory footprint (`VmRSS`, `RssAnon` from `/proc/<pid>/status`) across:
     - `128M` demand-paged (`virtio-only`, no IOMMU pin)
     - `256M` demand-paged (`virtio-only`, no IOMMU pin)
     - `256M -mem-prealloc` (models mandatory VFIO/IOMMU pinned guest RAM ceiling)
"""

import json
import os
import select
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

VMLINUZ = Path("/lib/modules/6.18.62-1-lts/vmlinuz")
OUT_JSON = Path(__file__).resolve().parent / "results_exp_b2_microvm.json"

INIT_C_SRC = r"""
#include <fcntl.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>

static void set_raw(int fd) {
    struct termios t;
    if (tcgetattr(fd, &t) == 0) {
        cfmakeraw(&t);
        tcsetattr(fd, TCSANOW, &t);
    }
}

int main(void) {
    mkdir("/dev", 0755);
    mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
    int fd = open("/dev/hvc0", O_RDWR | O_NOCTTY);
    if (fd < 0) fd = 0;
    set_raw(fd);

    const char ready[] = "UVM_READY\n";
    (void)write(fd, ready, sizeof(ready) - 1);

    unsigned char ch = 0;
    while (read(fd, &ch, 1) == 1) {
        if (ch == 'P') {
            const char ack = 'K';
            (void)write(fd, &ack, 1);
        } else if (ch == 'Q') {
            const char bye = 'Z';
            (void)write(fd, &bye, 1);
            reboot(RB_POWER_OFF);
            return 0;
        }
    }
    return 0;
}
"""


def build_initramfs(tmpdir: Path) -> Path:
    c_path = tmpdir / "init.c"
    root_dir = tmpdir / "rootfs"
    root_dir.mkdir()
    init_bin = root_dir / "init"
    cpio_path = tmpdir / "initrd.cpio"

    c_path.write_text(INIT_C_SRC)
    subprocess.run(
        ["clang", "-O2", "-static", "-s", str(c_path), "-o", str(init_bin)],
        check=True,
    )
    init_bin.chmod(0o755)
    proc = subprocess.Popen(
        ["cpio", "-o", "-H", "newc", "--quiet"],
        cwd=str(root_dir),
        stdin=subprocess.PIPE,
        stdout=open(cpio_path, "wb"),
    )
    assert proc.stdin is not None
    proc.stdin.write(b".\ninit\n")
    proc.stdin.close()
    proc.wait()
    assert proc.returncode == 0
    return cpio_path


def read_proc_cpu_ticks(pid: int) -> int:
    stat = Path(f"/proc/{pid}/stat").read_text()
    rparen = stat.rfind(")")
    fields = stat[rparen + 2 :].split()
    utime = int(fields[11])
    stime = int(fields[12])
    return utime + stime


def read_proc_rss_kib(pid: int) -> dict[str, int]:
    out = {"VmRSS_kib": 0, "RssAnon_kib": 0}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            out["VmRSS_kib"] = int(line.split()[1])
        elif line.startswith("RssAnon:"):
            out["RssAnon_kib"] = int(line.split()[1])
    return out


def percentile(sorted_vals: list[float], pct: float) -> float:
    idx = min(len(sorted_vals) - 1, max(0, int(round((pct / 100.0) * (len(sorted_vals) - 1)))))
    return round(sorted_vals[idx], 2)


def run_profile(label: str, mem_arg: str, prealloc: bool, initrd: Path, cold_runs: int = 5) -> dict:
    clk_tck = os.sysconf("SC_CLK_TCK")
    cold_ms_samples: list[float] = []
    wake_us_samples: list[float] = []
    idle_cpu_pct = 0.0
    rss_snapshot = {"VmRSS_kib": 0, "RssAnon_kib": 0}

    for run_idx in range(cold_runs):
        h_in_r, h_in_w = os.pipe()
        h_out_r, h_out_w = os.pipe()

        cmd = [
            "qemu-system-x86_64",
            "-machine", "q35,accel=kvm,usb=off,vmport=off",
            "-cpu", "host",
            "-smp", "1",
            "-m", mem_arg,
            "-nodefaults",
            "-display", "none",
            "-no-reboot",
            "-kernel", str(VMLINUZ),
            "-initrd", str(initrd),
            "-append", "console=hvc0 quiet loglevel=0 nokaslr no_timer_check mitigations=off rdinit=/init",
            "-chardev", f"pipe,id=vc0,path=/dev/null",
        ]
        # Replace pipe chardev with explicit fd inheritance via stdio!
        cmd = cmd[:-2] + [
            "-chardev", "stdio,id=vc0,signal=off",
            "-device", "virtio-serial-pci,id=vser0",
            "-device", "virtconsole,chardev=vc0,nr=0",
        ]
        if prealloc:
            cmd.append("-mem-prealloc")

        t0 = time.perf_counter()
        proc = subprocess.Popen(
            cmd,
            stdin=h_in_r,
            stdout=h_out_w,
            stderr=subprocess.DEVNULL,
        )
        os.close(h_in_r)
        os.close(h_out_w)

        buf = b""
        deadline = t0 + 15.0
        while b"UVM_READY" not in buf:
            rem = deadline - time.perf_counter()
            if rem <= 0:
                proc.kill()
                raise RuntimeError(f"{label}: timeout waiting for UVM_READY (buf={buf!r})")
            rdy, _, _ = select.select([h_out_r], [], [], rem)
            if rdy:
                chunk = os.read(h_out_r, 256)
                if not chunk:
                    break
                buf += chunk
        t_ready = time.perf_counter()
        cold_ms_samples.append((t_ready - t0) * 1000.0)

        # On the final run, measure quiescent idle CPU, RSS, and 100 warm HLT wakeups
        if run_idx == cold_runs - 1:
            # Drain any trailing newline after UVM_READY
            while select.select([h_out_r], [], [], 0.05)[0]:
                os.read(h_out_r, 256)

            # 1. 3.0s quiescent idle window (guest /init blocked in read() -> Linux NO_HZ HLT)
            time.sleep(0.25)
            ticks_a = read_proc_cpu_ticks(proc.pid)
            wall_a = time.perf_counter()
            time.sleep(3.0)
            ticks_b = read_proc_cpu_ticks(proc.pid)
            wall_b = time.perf_counter()
            cpu_sec = (ticks_b - ticks_a) / float(clk_tck)
            idle_cpu_pct = round((cpu_sec / (wall_b - wall_a)) * 100.0, 3)

            # 2. 100 warm wakeups from true HLT (15 ms sleep > KVM 200 us halt_poll_ns)
            for i in range(105):
                time.sleep(0.015)
                tw0 = time.perf_counter_ns()
                os.write(h_in_w, b"P")
                rdy, _, _ = select.select([h_out_r], [], [], 2.0)
                if not rdy:
                    raise RuntimeError(f"{label}: timeout on ping {i}")
                ack = os.read(h_out_r, 1)
                tw1 = time.perf_counter_ns()
                assert ack == b"K", f"unexpected ack {ack!r}"
                if i >= 5:  # 5 warmup pings
                    wake_us_samples.append((tw1 - tw0) / 1000.0)

            rss_snapshot = read_proc_rss_kib(proc.pid)

        os.write(h_in_w, b"Q")
        try:
            proc.wait(timeout=3.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        os.close(h_in_w)
        os.close(h_out_r)

    cold_sorted = sorted(cold_ms_samples)
    wake_sorted = sorted(wake_us_samples)
    return {
        "profile": label,
        "guest_mem": mem_arg,
        "mem_prealloc_vfio_model": prealloc,
        "cold_start_ms": {
            "runs": cold_runs,
            "min": round(cold_sorted[0], 2),
            "p50": round(statistics.median(cold_sorted), 2),
            "max": round(cold_sorted[-1], 2),
        },
        "warm_hlt_wakeup_rtt_us": {
            "samples": len(wake_sorted),
            "inter_ping_sleep_ms": 15,
            "min": round(wake_sorted[0], 2),
            "p50": percentile(wake_sorted, 50),
            "p95": percentile(wake_sorted, 95),
            "p99": percentile(wake_sorted, 99),
            "max": round(wake_sorted[-1], 2),
        },
        "quiescent_3s_host_cpu_pct": idle_cpu_pct,
        "host_memory": {
            "VmRSS_mib": round(rss_snapshot["VmRSS_kib"] / 1024.0, 2),
            "RssAnon_mib": round(rss_snapshot["RssAnon_kib"] / 1024.0, 2),
        },
    }


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="dry_exp_b2_") as tmp:
        tmpdir = Path(tmp)
        initrd = build_initramfs(tmpdir)
        initrd_kib = round(initrd.stat().st_size / 1024.0, 1)

        profiles = [
            run_profile("uvm_128m_demand", "128M", False, initrd),
            run_profile("uvm_256m_demand", "256M", False, initrd),
            run_profile("uvm_256m_vfio_pinned", "256M", True, initrd),
        ]

    payload = {
        "experiment": "Exp-B2",
        "host_kernel": os.uname().release,
        "guest_vmlinuz": str(VMLINUZ),
        "initramfs_kib": initrd_kib,
        "profiles": profiles,
    }
    OUT_JSON.write_text(json.dumps(payload, indent=2) + "\n")
    print(json.dumps(payload, indent=2))


if __name__ == "__main__":
    main()
