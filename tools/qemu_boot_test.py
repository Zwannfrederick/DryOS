#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Build x86-64 `dry-kernel` offline and verify QEMU UEFI boot via serial + exit code."""

import argparse
import json
import os
import pathlib
import shutil
import struct
import subprocess
import sys
import time

from qemu_log_validators import (
    expected_sched_metrics,
    verify_apic_and_irq_lines,
    verify_cspace_and_tlb_debt_lines,
    verify_pmm_and_untyped_lines,
    verify_preempt_line,
    verify_sched_line,
    verify_vspace_lines,
)
from qemu_ipc_validators import capture_and_verify_gop_screendump, verify_ipc_and_display_lines
from qemu_neg_validators import run_legacy_negative_matrix
from qemu_p1_validators import run_p1_suite, verify_p1_irq_lines
from qemu_p2_validators import run_p2_suite
from qemu_p3_validators import run_p3_suite
from qemu_p3c_validators import run_p3_hardware_matrix
from qemu_p4_validators import run_p4_suite
from qemu_p5_island_validators import run_p5_island_suite
from qemu_p6_lx_island_validators import run_p6_lx_island_suite
from qemu_p7_gpu_validators import run_p7_gpu_suite
from qemu_p8_hw_validators import run_p8_hw_suite
from qemu_p9_glass_validators import run_p9_glass_suite
from qemu_p10_glass2_validators import run_p10_glass2_suite
from qemu_p11_glass21_validators import run_p11_glass21_suite
from qemu_p12_lx_compat_validators import run_p12_lx_compat_suite
from qemu_p13_usb_persist_validators import run_p13_usb_persist_suite
from qemu_p14_lx_net_validators import run_p14_lx_net_suite
from qemu_p15_brand_icons_validators import run_p15_brand_icons_suite
from qemu_p16_ascension6_validators import run_p16_ascension6_suite
from qemu_p17_ascension7_validators import run_p17_ascension7_suite
from qemu_p18_hwval_validators import run_p18_hwval_suite
from qemu_user_validators import verify_userspace_lines

ROOT = pathlib.Path(__file__).resolve().parents[1]
KERNEL_DIR = ROOT / "kernel"
USERSPACE_DIR = ROOT / "userspace"
LX_ISLAND_DIR = ROOT / "drivers" / "lx-virtio-rng-island"
LX_DRM_ISLAND_DIR = ROOT / "drivers" / "lx-drm-bochs-island"
LX_NET_ISLAND_DIR = ROOT / "drivers" / "lx-net-8139cp-island"
BUILD_DIR = KERNEL_DIR / "build"
TC_DIR = ROOT / ".toolchains"
SYSROOT = TC_DIR / "sysroot"
CARGO_HOME = TC_DIR / "cargo-home"
LIMINE_EFI = TC_DIR / "limine" / "BOOTX64.EFI"

OVMF_CODE = pathlib.Path("/usr/share/edk2/x64/OVMF_CODE.4m.fd")
OVMF_VARS = pathlib.Path("/usr/share/edk2/x64/OVMF_VARS.4m.fd")

EXIT_CODE_SUCCESS = (0x10 << 1) | 1  # 33
EXIT_CODE_PANIC = (0x11 << 1) | 1    # 35

USERSPACE_MODS = (
    ("user-hello", "user-hello.elf"),
    ("user-peer", "user-peer.elf"),
    ("dry-init", "dry-init.elf"),
    ("dry-echo", "dry-echo.elf"),
    ("dry-display", "dry-display.elf"),
    ("dry-input", "dry-input.elf"),
    ("dry-shell", "dry-shell.elf"),
    ("dry-sysmon", "dry-sysmon.elf"),
    ("dry-vfs", "dry-vfs.elf"),
    ("dry-files", "dry-files.elf"),
    ("dry-settings", "dry-settings.elf"),
    ("dry-power", "dry-power.elf"),
    ("dry-blk", "dry-blk.elf"),
    ("dry-vault", "dry-vault.elf"),
    ("dry-broker", "dry-broker.elf"),
    ("dry-mock-drv", "dry-mock-drv.elf"),
    ("dry-lx-rng", "dry-lx-rng.elf"),
    ("dry-fpu-test", "dry-fpu-test.elf"),
    ("dry-fault-test", "dry-fault-test.elf"),
    ("dry-gpu", "dry-gpu.elf"),
    ("dry-hw-test", "dry-hw-test.elf"),
    ("dry-editor", "dry-editor.elf"),
    ("dry-calc", "dry-calc.elf"),
    ("dry-lx-drm-bochs", "dry-lx-drm-bochs.elf"),
    ("dry-usb", "dry-usb.elf"),
    ("dry-nvme", "dry-nvme.elf"),
    ("dry-lx-net", "dry-lx-net.elf"),
    ("dry-sdk-demo", "dry-sdk-demo.elf"),
)


def kernel_cargo_env() -> dict[str, str]:
    linker_script = KERNEL_DIR / "linker.ld"
    rustflags = " ".join(
        [
            f"--sysroot={SYSROOT}",
            "-C",
            "relocation-model=static",
            "-C",
            f"link-arg=-T{linker_script}",
            "-C",
            "link-arg=-zmax-page-size=0x1000",
        ]
    )
    return {
        **os.environ,
        "RUSTC_BOOTSTRAP": "1",
        "CARGO_HOME": str(CARGO_HOME),
        "__CARGO_TESTS_ONLY_SRC_ROOT": str(SYSROOT / "lib" / "rustlib" / "src" / "rust" / "library"),
        "RUSTFLAGS": rustflags,
    }


def userspace_cargo_env() -> dict[str, str]:
    linker_script = USERSPACE_DIR / "linker_user.ld"
    rustflags = " ".join(
        [
            f"--sysroot={SYSROOT}",
            "-C",
            "relocation-model=static",
            "-C",
            "target-feature=-sse,-sse2,-avx",
            "-C",
            f"link-arg=-T{linker_script}",
            "-C",
            "link-arg=-zmax-page-size=0x1000",
        ]
    )
    return {
        **os.environ,
        "RUSTC_BOOTSTRAP": "1",
        "CARGO_HOME": str(CARGO_HOME),
        "__CARGO_TESTS_ONLY_SRC_ROOT": str(SYSROOT / "lib" / "rustlib" / "src" / "rust" / "library"),
        "RUSTFLAGS": rustflags,
    }


def build_lx_rng_island() -> pathlib.Path:
    """Compile unmodified Linux v6.18 `virtio-rng.c` + minimal C shim and link `dry-lx-rng`."""
    subprocess.check_call(
        [sys.executable, str(ROOT / "tools" / "h2-audit" / "fetch_virtio_rng_sources.py"), "--verify"]
    )
    obj_dir = BUILD_DIR / "lx_rng"
    obj_dir.mkdir(parents=True, exist_ok=True)
    inc_dir = LX_ISLAND_DIR / "cshim" / "include"
    cflags = [
        "clang",
        "-std=gnu11",
        "-target",
        "x86_64-unknown-none-elf",
        "-ffreestanding",
        "-fno-builtin",
        "-fno-stack-protector",
        "-fno-pic",
        "-fno-pie",
        "-mno-red-zone",
        "-mno-sse",
        "-mno-sse2",
        "-mno-avx",
        "-mcmodel=small",
        "-O2",
        "-Wall",
        "-Wextra",
        "-Wno-unused-parameter",
        "-Werror",
        '-DKBUILD_MODNAME="virtio_rng"',
        f"-I{inc_dir}",
    ]
    drv_src = ROOT / "vendor" / "linux-drivers" / "v6.18" / "virtio_rng" / "virtio-rng.c"
    shim_src = LX_ISLAND_DIR / "cshim" / "lx_virtio_shim.c"
    drv_obj = obj_dir / "virtio-rng.o"
    shim_obj = obj_dir / "lx_virtio_shim.o"
    lib_a = obj_dir / "liblx_virtio_rng.a"

    subprocess.check_call(cflags + ["-c", str(drv_src), "-o", str(drv_obj)])
    subprocess.check_call(cflags + ["-c", str(shim_src), "-o", str(shim_obj)])
    subprocess.check_call(["llvm-ar", "rcs", str(lib_a), str(drv_obj), str(shim_obj)])

    linker_script = LX_ISLAND_DIR / "linker.ld"
    lx_env = {
        **os.environ,
        "RUSTC_BOOTSTRAP": "1",
        "CARGO_HOME": str(CARGO_HOME),
        "__CARGO_TESTS_ONLY_SRC_ROOT": str(SYSROOT / "lib" / "rustlib" / "src" / "rust" / "library"),
        "RUSTFLAGS": " ".join(
            [
                f"--sysroot={SYSROOT}",
                "-C",
                "relocation-model=static",
                "-C",
                "target-feature=-sse,-sse2,-avx",
                "-C",
                f"link-arg=-T{linker_script}",
                "-C",
                "link-arg=-zmax-page-size=0x1000",
                "-L",
                f"native={obj_dir}",
            ]
        ),
    }
    print("[QEMU-TEST] Running cargo clippy (-D warnings) on dry-lx-rng...")
    subprocess.check_call(
        [
            "cargo",
            "clippy",
            "--offline",
            "--manifest-path",
            str(LX_ISLAND_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--",
            "-D",
            "warnings",
        ],
        env=lx_env,
    )
    print("[QEMU-TEST] Building release ELF64 dry-lx-rng (GPL-2.0-only isolated crate)...")
    subprocess.check_call(
        [
            "cargo",
            "build",
            "--offline",
            "--manifest-path",
            str(LX_ISLAND_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--release",
        ],
        env=lx_env,
    )
    lx_bin = LX_ISLAND_DIR / "target" / "x86_64-unknown-none" / "release" / "dry-lx-rng"
    dst_bin = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release" / "dry-lx-rng"
    dst_bin.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(lx_bin, dst_bin)
    return dst_bin


def build_lx_compat_island(island_dir: pathlib.Path, driver_key: str, bin_name: str) -> pathlib.Path:
    """Verify upstream SHA-256, build cached static archive, and link standalone GPL island ELF."""
    mgr = ROOT / "tools" / "lx_compat" / "lx_driver_manager.py"
    subprocess.check_call([sys.executable, str(mgr), "verify", "--all"])
    meta_json = subprocess.check_output(
        [sys.executable, str(mgr), "build-cache", "--driver", driver_key],
        text=True,
    )
    archive_dir = pathlib.Path(json.loads(meta_json)["archive_path"]).parent
    linker_script = island_dir / "linker.ld"
    lx_env = {
        **os.environ,
        "RUSTC_BOOTSTRAP": "1",
        "CARGO_HOME": str(CARGO_HOME),
        "__CARGO_TESTS_ONLY_SRC_ROOT": str(SYSROOT / "lib" / "rustlib" / "src" / "rust" / "library"),
        "RUSTFLAGS": " ".join(
            [
                f"--sysroot={SYSROOT}",
                "-C",
                "relocation-model=static",
                "-C",
                "target-feature=-sse,-sse2,-avx",
                "-C",
                f"link-arg=-T{linker_script}",
                "-C",
                "link-arg=-zmax-page-size=0x1000",
                "-L",
                f"native={archive_dir}",
            ]
        ),
    }
    print(f"[QEMU-TEST] Running cargo clippy (-D warnings) on {bin_name}...")
    subprocess.check_call(
        [
            "cargo",
            "clippy",
            "--offline",
            "--manifest-path",
            str(island_dir / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--",
            "-D",
            "warnings",
        ],
        env=lx_env,
    )
    print(f"[QEMU-TEST] Building release ELF64 {bin_name} (GPL-2.0 isolated crate)...")
    subprocess.check_call(
        [
            "cargo",
            "build",
            "--offline",
            "--manifest-path",
            str(island_dir / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--release",
        ],
        env=lx_env,
    )
    lx_bin = island_dir / "target" / "x86_64-unknown-none" / "release" / bin_name
    dst_bin = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release" / bin_name
    dst_bin.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(lx_bin, dst_bin)
    return dst_bin


def build_sdk_demo_island() -> pathlib.Path:
    """Compile standalone MIT/Apache-2.0 `dry-sdk-demo` Driver Island (`dry-driver-sdk`)."""
    sdk_dir = ROOT / "drivers" / "sdk-example-pci"
    base_env = userspace_cargo_env()
    sdk_env = {
        **base_env,
        "RUSTFLAGS": base_env["RUSTFLAGS"].replace(
            str(USERSPACE_DIR / "linker_user.ld"), str(sdk_dir / "linker.ld")
        ),
    }
    print("[QEMU-TEST] Running cargo clippy (-D warnings) on dry-sdk-demo...")
    subprocess.check_call(
        [
            "cargo", "clippy", "--offline", "--manifest-path", str(sdk_dir / "Cargo.toml"),
            "-Z", "build-std=core,compiler_builtins", "--target", "x86_64-unknown-none",
            "--", "-D", "warnings",
        ],
        env=sdk_env,
    )
    print("[QEMU-TEST] Building release ELF64 dry-sdk-demo (dry-driver-sdk ABI v1)...")
    subprocess.check_call(
        [
            "cargo", "build", "--offline", "--manifest-path", str(sdk_dir / "Cargo.toml"),
            "-Z", "build-std=core,compiler_builtins", "--target", "x86_64-unknown-none",
            "--release",
        ],
        env=sdk_env,
    )
    src_bin = sdk_dir / "target" / "x86_64-unknown-none" / "release" / "dry-sdk-demo"
    dst_bin = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release" / "dry-sdk-demo"
    dst_bin.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src_bin, dst_bin)
    return dst_bin


def build_userspace() -> list[pathlib.Path]:
    env = userspace_cargo_env()
    print("[QEMU-TEST] Running cargo clippy (-D warnings) on dry-userspace...")
    subprocess.check_call(
        [
            "cargo",
            "clippy",
            "--offline",
            "--manifest-path",
            str(USERSPACE_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--",
            "-D",
            "warnings",
        ],
        env=env,
    )
    print(
        "[QEMU-TEST] Building release ELF64 userspace binaries "
        "(user-hello, user-peer, dry-init, dry-echo, dry-display, dry-input, dry-shell, dry-sysmon)..."
    )
    subprocess.check_call(
        [
            "cargo",
            "build",
            "--offline",
            "--manifest-path",
            str(USERSPACE_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--release",
        ],
        env=env,
    )
    out_dir = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release"
    build_lx_rng_island()
    build_lx_compat_island(LX_DRM_ISLAND_DIR, "drm_bochs", "dry-lx-drm-bochs")
    build_lx_compat_island(LX_NET_ISLAND_DIR, "net_8139cp", "dry-lx-net")
    build_sdk_demo_island()
    bins = [out_dir / src_name for src_name, _ in USERSPACE_MODS]
    for b in bins:
        if not b.exists():
            raise RuntimeError(f"Missing userspace ELF {b}")
    return bins


def build_kernel() -> pathlib.Path:
    subprocess.check_call([sys.executable, str(ROOT / "tools" / "bootstrap_phase1.py"), "--verify"])
    build_userspace()
    env = kernel_cargo_env()

    print("[QEMU-TEST] Running cargo clippy (-D warnings) on dry-kernel...")
    subprocess.check_call(
        [
            "cargo",
            "clippy",
            "--offline",
            "--manifest-path",
            str(KERNEL_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--",
            "-D",
            "warnings",
        ],
        env=env,
    )

    print("[QEMU-TEST] Building release ELF64 dry-kernel...")
    subprocess.check_call(
        [
            "cargo",
            "build",
            "--offline",
            "--manifest-path",
            str(KERNEL_DIR / "Cargo.toml"),
            "-Z",
            "build-std=core,compiler_builtins",
            "--target",
            "x86_64-unknown-none",
            "--release",
        ],
        env=env,
    )

    elf = KERNEL_DIR / "target" / "x86_64-unknown-none" / "release" / "dry-kernel"
    if not elf.exists():
        raise RuntimeError(f"Expected kernel ELF not found at {elf}")
    return elf


def write_mbr_esp_image(
    img_path: pathlib.Path,
    kernel_elf: pathlib.Path,
    cmdline: str,
    resolution: str | None = None,
) -> None:
    """Create a 64 MiB MBR-partitioned FAT32 EFI System Partition image using `mtools`."""
    total_mib = 64
    total_bytes = total_mib * 1024 * 1024
    part_offset = 1024 * 1024  # LBA 2048 (1 MiB)
    lba_start = part_offset // 512
    lba_sectors = (total_bytes - part_offset) // 512

    with img_path.open("wb") as f:
        f.truncate(total_bytes)
        # MBR Partition Entry #1 at offset 446: bootable=0x80, type=0xEF (EFI System Partition)
        entry = struct.pack("<BBBBBBBBII", 0x80, 0xFE, 0xFF, 0xFF, 0xEF, 0xFE, 0xFF, 0xFF, lba_start, lba_sectors)
        f.seek(446)
        f.write(entry)
        f.seek(510)
        f.write(b"\x55\xaa")

    mtools_env = {**os.environ, "LC_ALL": "C"}
    part_spec = f"{img_path}@@{part_offset}"
    subprocess.check_call(["mformat", "-i", part_spec, "-F", "-v", "DRYOS_ESP", "::"], env=mtools_env)
    for d in ["::/EFI", "::/EFI/BOOT", "::/boot", "::/boot/limine"]:
        subprocess.check_call(["mmd", "-i", part_spec, d], env=mtools_env)

    u_out_dir = USERSPACE_DIR / "target" / "x86_64-unknown-none" / "release"
    mods = USERSPACE_MODS

    conf_path = BUILD_DIR / "limine.conf"
    mod_lines = "".join(f"    module_path: boot():/boot/{dst}\n" for _, dst in mods)
    conf_text = (
        "timeout: 0\n"
        "serial: yes\n"
        "\n"
        "/DRY OS (Phase 1)\n"
        "    protocol: limine\n"
        f"{f'    resolution: {resolution}\n' if resolution else ''}"
        "    path: boot():/boot/dry-kernel\n"
        f"    cmdline: {cmdline}\n"
        f"{mod_lines}"
    )
    conf_path.write_text(conf_text, encoding="utf-8")

    subprocess.check_call(["mcopy", "-i", part_spec, str(LIMINE_EFI), "::/EFI/BOOT/BOOTX64.EFI"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(conf_path), "::/boot/limine/limine.conf"], env=mtools_env)
    subprocess.check_call(["mcopy", "-i", part_spec, str(kernel_elf), "::/boot/dry-kernel"], env=mtools_env)
    for src_name, dst_name in mods:
        subprocess.check_call(
            ["mcopy", "-i", part_spec, str(u_out_dir / src_name), f"::/boot/{dst_name}"],
            env=mtools_env,
        )


def run_qemu(
    name: str,
    img_path: pathlib.Path,
    smp: int,
    timeout_sec: int = 25,
    extra_args: list[str] | None = None,
    mem: str = "512M",
) -> tuple[int, str, float]:
    vars_copy = BUILD_DIR / f"OVMF_VARS_{name}.4m.fd"
    shutil.copyfile(OVMF_VARS, vars_copy)

    accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
    cpu_arg = "host" if accel == "kvm" else "qemu64"

    cmd = [
        "qemu-system-x86_64",
        "-machine",
        f"q35,accel={accel}",
        "-cpu",
        cpu_arg,
        "-smp",
        str(smp),
        "-m",
        mem,
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive",
        f"if=pflash,format=raw,file={vars_copy}",
        "-drive",
        f"format=raw,file={img_path}",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial",
        "stdio",
        "-display",
        "none",
        "-no-reboot",
    ]
    if extra_args:
        cmd.extend(extra_args)
    t0 = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            timeout=timeout_sec,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        out_str = (
            exc.stdout.decode("utf-8", errors="replace")
            if isinstance(exc.stdout, bytes)
            else (exc.stdout or "")
        )
        print(
            f"\n[QEMU-TIMEOUT] {name} (smp={smp}) timed out after {timeout_sec}s! Partial serial output:\n{out_str}",
            file=sys.stderr,
        )
        raise
    elapsed = time.monotonic() - t0
    return proc.returncode, proc.stdout, elapsed


def assert_boot_case(
    name: str,
    kernel_elf: pathlib.Path,
    cmdline: str,
    smp: int,
    expected_rc: int,
    must_contain: list[str],
    must_not_contain: list[str],
) -> None:
    img_path = BUILD_DIR / f"esp_{name}.img"
    write_mbr_esp_image(img_path, kernel_elf, cmdline)
    rc, serial_out, elapsed = run_qemu(name, img_path, smp=smp)

    print(f"\n--- [{name}] smp={smp} QEMU exit={rc} (expected {expected_rc}) elapsed={elapsed:.3f}s ---")
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Hello from DRY OS!" in line or "Limine" in line or "PANIC" in line:
            print(f"  {line}")

    if rc != expected_rc:
        print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
        raise RuntimeError(f"[{name}] Unexpected QEMU exit code {rc} != {expected_rc}")
    for token in must_contain:
        if token not in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Missing required serial marker: {token!r}")
    for token in must_not_contain:
        if token in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Forbidden serial marker found: {token!r}")
    print(f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions verified in {elapsed:.3f}s).")


def assert_positive_smp_boot(
    name: str,
    kernel_elf: pathlib.Path,
    smp: int,
    cmdline: str = "console=com1",
    irq_only: bool = False,
    expected_apic_mode: str | None = None,
    expect_fb: bool = True,
    extra_qemu_args: list[str] | None = None,
    mem: str = "512M",
) -> None:
    expected_mask = hex((1 << smp) - 1)
    expected_ops = smp * 4096
    spawned, park_rounds, work_units = expected_sched_metrics(smp)
    img_path = BUILD_DIR / f"esp_{name}.img"
    write_mbr_esp_image(img_path, kernel_elf, cmdline)
    rc, serial_out, elapsed = run_qemu(
        name,
        img_path,
        smp=smp,
        timeout_sec=75 if smp > 8 else 50,
        extra_args=extra_qemu_args,
        mem=mem,
    )

    print(f"\n--- [{name}] smp={smp} QEMU exit={rc} (expected {EXIT_CODE_SUCCESS}) elapsed={elapsed:.3f}s ---")
    for line in serial_out.splitlines():
        if "[DRY-" in line or "Hello from DRY OS!" in line or "Limine" in line or "PANIC" in line:
            print(f"  {line}")

    must_contain = [
        "[DRY-BOOT] DRY Kernel v0.1.0",
        "Firmware: EFI64",
        "[DRY-CPU] PASS: BSP(cpu=0,",
        "GDT(CS=0x08,TR=0x28,Busy=0x8b)",
        "#BP(hits=1,",
        "[DRY-HHDM] PASS:",
        "[DRY-MEMMAP] PASS:",
        "[DRY-SEC] PASS:",
        f"[DRY-SMP] PASS: cpus_online={smp} mask={expected_mask} total_bp_traps={smp} symmetric_ops={expected_ops} stacks_disjoint=true",
        f"[DRY-SCHED] PASS: cpus={smp} spawned={spawned} exited={spawned}",
        f"park_rounds={park_rounds} work_units={work_units} stale_wakes_rejected=48 orphan_ready=0",
        "[DRY-APIC] PASS:",
        f"[DRY-IRQ-ONLY] PASS: cpus={smp}",
    ]
    if not irq_only:
        must_contain.extend(
            [
                f"[DRY-PREEMPT] PASS: cpus={smp}",
                "[DRY-PMM] PASS:",
                "[DRY-UNTYPED] PASS:",
                f"[DRY-PMM-BENCH] PASS: cpus={smp}",
                f"[DRY-LIVENESS-REG] PASS: cpus={smp}",
                "[DRY-VSPACE] PASS:",
                "[DRY-HUGEPAGE] PASS:",
                f"[DRY-VSPACE-BENCH] PASS: cpus={smp}",
                f"[DRY-VSPACE-SMP] PASS: cpus={smp}",
                f"[DRY-TLB-DEBT] PASS: cpus={smp}",
                f"[DRY-CSPACE] PASS: cpus={smp}",
                f"[DRY-CSPACE-BENCH] PASS: cpus={smp}",
                f"[DRY-CSPACE-SMP] PASS: cpus={smp}",
                "Hello from DRY OS!",
                f"[DRY-USER-SEC] PASS: cpus={smp}",
                f"[DRY-USER-BENCH] PASS: cpus={smp}",
                f"[DRY-FPU] PASS: cpus={smp}",
                f"[DRY-AUDIT-REG] PASS: cpus={smp} cases=9/9",
                f"[DRY-FAULT-EP] PASS: cpus={smp} delivered=20 resumed=17",
                f"[DRY-SERVICES] PASS: cpus={smp}",
                f"[DRY-IPC-SEC] PASS: cpus={smp}",
                f"[DRY-IPC-BENCH] PASS: cpus={smp}",
                f"[DRY-IRQ] PASS: cpus={smp}",
                f"[DRY-IRQ-BENCH] PASS: cpus={smp}",
                f"[DRY-BOOT-OK] Milestone 1C UEFI boot + {smp}-vCPU SMP + H1 scheduler + PMM + VSpace + CSpace + Ring 3 Services, IPC & GOP Display verified cleanly.",
            ]
        )
    must_not_contain = [
        "[DRY-PANIC]",
        "[DRY-EXCEPTION]",
        "[DRY-SMP-FAIL]",
        "[DRY-SCHED-FAIL]",
        "[DRY-PREEMPT-FAIL]",
        "[DRY-PMM-FAIL]",
        "[DRY-VSPACE-FAIL]",
        "[DRY-TLB-FAIL]",
        "[DRY-CSPACE-FAIL]",
        "[DRY-USER-FAIL]",
        "[DRY-IPC-FAIL]",
        "[DRY-IRQ-FAIL]",
        "[DRY-APIC-FAIL]",
        "[DRY-DF-IST1] FAIL",
    ]

    if rc != EXIT_CODE_SUCCESS:
        print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
        raise RuntimeError(f"[{name}] Unexpected QEMU exit code {rc} != {EXIT_CODE_SUCCESS}")
    for token in must_contain:
        if token not in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Missing required serial marker: {token!r}")
    for token in must_not_contain:
        if token in serial_out:
            print("FULL SERIAL OUTPUT:\n" + serial_out, file=sys.stderr)
            raise RuntimeError(f"[{name}] Forbidden serial marker found: {token!r}")
    verify_sched_line(name, serial_out, smp)
    verify_apic_and_irq_lines(name, serial_out, smp, expected_mode=expected_apic_mode)
    if not irq_only:
        verify_preempt_line(name, serial_out, smp)
        verify_pmm_and_untyped_lines(name, serial_out, smp)
        verify_vspace_lines(name, serial_out, smp)
        verify_cspace_and_tlb_debt_lines(name, serial_out, smp)
        verify_userspace_lines(name, serial_out, smp)
        verify_ipc_and_display_lines(name, serial_out, smp, expect_fb=expect_fb)
        verify_p1_irq_lines(name, serial_out, smp)
    print(
        f"[PASS] {name}: smp={smp} exit={rc} ({len(must_contain)} assertions + numeric sched/preempt/pmm/vspace/cspace/user/ipc/irq/display invariants verified in {elapsed:.3f}s)."
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--build-only", action="store_true", help="Run clippy + release build without QEMU")
    parser.add_argument("--smp-only", type=int, default=0, help="Run only a single positive N-vCPU boot test")
    parser.add_argument("--irq-only", action="store_true", help="Sub-Stage A: verify only APIC timer IRQ + EOI + TrapFrame")
    parser.add_argument("--p1-only", action="store_true", help="Run only Milestone 2A-1 (P1) interactive & negative security suite")
    parser.add_argument("--p3-only", action="store_true", help="Run only Milestone 2A-3 native desktop & lifecycle suite")
    parser.add_argument("--p3-matrix", action="store_true", help="Run Milestone 2A-3 SMP (1..20 vCPU) & RAM (256M..2G) desktop matrix")
    parser.add_argument("--p4-only", action="store_true", help="Run only Milestone 2B persistent storage & DRY Vault suite")
    parser.add_argument("--island-only", action="store_true", help="Run only Milestone 2C-0 Adaptive Driver Islands PoC suite")
    parser.add_argument("--lx-island-only", action="store_true", help="Run only Milestone 2C-1 Genuine Linux virtio-rng Driver Island suite")
    parser.add_argument("--gpu-only", action="store_true", help="Run only Priority C VirtIO-GPU 2D scanout & GOP fallback suite")
    parser.add_argument("--p8-only", action="store_true", help="Run only Milestone 2C-2 VT-d, MSI/MSI-X, BAR64 & bound-notification hardware suite")
    parser.add_argument("--p9-only", action="store_true", help="Run only DRY Glass Desktop, Multi-Res VirtIO-GPU & Native Apps suite")
    parser.add_argument("--p10-only", action="store_true", help="Run only DRY Glass 2.0 TrueType UI, 6 Native Apps, Multi-DPI & Multi-Res suite")
    parser.add_argument("--p11-only", action="store_true", help="Run only DRY Glass 2.1 Adaptive Window Strip, Overview & HiDPI Sidebar suite")
    parser.add_argument("--p12-only", action="store_true", help="Run only Ascension 4.0 Linux DRM (bochs.c) Runtime Compatibility suite")
    parser.add_argument("--p13-only", action="store_true", help="Run only M2 xHCI USB HID/BOT Persist & Read-Only NVMe suite")
    parser.add_argument("--p14-only", action="store_true", help="Run only M3 Universal Linux Network (8139cp.c+mii.c) SLIRP suite")
    parser.add_argument("--p15-only", action="store_true", help="Run only DRY OS Brand Lockup, 21 App Icons & 200%% DPI suite")
    parser.add_argument("--p16-only", action="store_true", help="Run only Ascension 6.0 Universal GPU & Hardening suite")
    parser.add_argument("--p17-only", action="store_true", help="Run only Ascension 7.0 Reliability, Daily-Use & Virgl 3D suite")
    parser.add_argument("--p18-only", action="store_true", help="Run only Ascension 8.0 Hardware Validation & Persistence suite")
    parser.add_argument("--mem", default="512M", help="QEMU guest RAM size passed to positive SMP/boot runs (e.g. 512M, 4G)")
    args = parser.parse_args()

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    kernel_elf = build_kernel()
    if args.build_only:
        print(f"[QEMU-TEST] Build-only check succeeded: {kernel_elf}")
        return

    if args.p1_only:
        run_p1_suite(
            kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, assert_boot_case, EXIT_CODE_PANIC
        )
        print("\n[QEMU-TEST] ALL 9 MILESTONE 2A-1 (P1) INTERACTIVE HARDWARE E2E & SECURITY CHECKS PASSED.")
        return

    if args.p3_only:
        n_p3 = run_p3_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p3} MILESTONE 2A-3 NATIVE DESKTOP & SECURITY CHECKS PASSED.")
        return

    if args.p4_only:
        n_p4 = run_p4_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p4} MILESTONE 2B PERSISTENT STORAGE & DRY VAULT CHECKS PASSED.")
        return

    if args.island_only:
        n_p5 = run_p5_island_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p5} MILESTONE 2C-0 ADAPTIVE DRIVER ISLANDS POC CHECKS PASSED.")
        return

    if args.lx_island_only:
        n_p6 = run_p6_lx_island_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p6} MILESTONE 2C-1 GENUINE LINUX DRIVER ISLAND CHECKS PASSED.")
        return

    if args.gpu_only:
        n_p7 = run_p7_gpu_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p7} PRIORITY C VIRTIO-GPU SCANOUT & GOP FALLBACK CHECKS PASSED.")
        return

    if args.p8_only:
        n_p8 = run_p8_hw_suite(kernel_elf, BUILD_DIR, ROOT, write_mbr_esp_image, run_qemu)
        print(f"\n[QEMU-TEST] ALL {n_p8} MILESTONE 2C-2 HARDWARE FOUNDATIONS CHECKS PASSED.")
        return

    if args.p9_only:
        n_p9 = run_p9_glass_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p9} DRY GLASS DESKTOP CHECKS PASSED.")
        return

    if args.p10_only:
        n_p10 = run_p10_glass2_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p10} DRY GLASS 2.0 CHECKS PASSED.")
        return

    if args.p11_only:
        n_p11 = run_p11_glass21_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p11} DRY GLASS 2.1 CHECKS PASSED.")
        return

    if args.p12_only:
        n_p12 = run_p12_lx_compat_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p12} LINUX DRM COMPATIBILITY CHECKS PASSED.")
        return

    if args.p13_only:
        n_p13 = run_p13_usb_persist_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p13} M2 USB/PERSIST/NVME-RO CHECKS PASSED.")
        return

    if args.p14_only:
        n_p14 = run_p14_lx_net_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p14} M3 UNIVERSAL LINUX NET DRIVER ISLAND CHECKS PASSED.")
        return

    if args.p15_only:
        n_p15 = run_p15_brand_icons_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p15} BRAND + ICONS VISUAL IDENTITY CHECKS PASSED.")
        return

    if args.p16_only:
        n_p16 = run_p16_ascension6_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, run_qemu)
        print(f"\n[QEMU-TEST] ALL {n_p16} ASCENSION 6.0 UNIVERSAL GPU & HARDENING CHECKS PASSED.")
        return

    if args.p17_only:
        n_p17 = run_p17_ascension7_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, run_qemu)
        print(f"\n[QEMU-TEST] ALL {n_p17} ASCENSION 7.0 RELIABILITY & VIRGL 3D CHECKS PASSED.")
        return

    if args.p18_only:
        n_p18 = run_p18_hwval_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
        print(f"\n[QEMU-TEST] ALL {n_p18} ASCENSION 8.0 HARDWARE VALIDATION & PERSISTENCE CHECKS PASSED.")
        return

    if args.p3_matrix:
        esp_p3_std = BUILD_DIR / "esp_p3_desktop_std.img"
        write_mbr_esp_image(esp_p3_std, kernel_elf, "console=com1 desktop3=1")
        run_p3_hardware_matrix(esp_p3_std, OVMF_CODE, OVMF_VARS, BUILD_DIR)
        print("\n[QEMU-TEST] ALL 5 MILESTONE 2A-3 SMP/RAM MATRIX DESKTOP CONFIGURATIONS PASSED.")
        return

    if args.smp_only > 0:
        cmdline = "console=com1 preempt_stage=irq_only" if args.irq_only else "console=com1"
        assert_positive_smp_boot(
            f"boot_{args.smp_only}cpu_ok",
            kernel_elf,
            args.smp_only,
            cmdline=cmdline,
            irq_only=args.irq_only,
            mem=args.mem,
        )
        return

    # Progressive SMP bring-up order (ADR-006 / Milestone 1A-E..1B-C): 1 -> 2 -> 4 -> 8 -> 20 vCPUs.
    assert_positive_smp_boot("boot_1cpu_ok", kernel_elf, smp=1, mem=args.mem)
    assert_positive_smp_boot("boot_2cpu_ok", kernel_elf, smp=2, mem=args.mem)
    assert_positive_smp_boot("boot_4cpu_ok", kernel_elf, smp=4, mem=args.mem)
    assert_positive_smp_boot("boot_8cpu_ok", kernel_elf, smp=8, mem=args.mem)
    assert_positive_smp_boot("boot_20cpu_ok", kernel_elf, smp=20, mem=args.mem)

    # Explicit hardware fallback verification: force xAPIC MMIO fallback (`0xFEE0_0000` UC via HHDM) on 2 vCPUs.
    assert_positive_smp_boot(
        "boot_2cpu_xapic_fallback",
        kernel_elf,
        smp=2,
        cmdline="console=com1 apic_mode=xapic",
        expected_apic_mode="xapic-mmio",
        mem=args.mem,
    )

    # FPU fallback verification (`Milestone 2A-4a`): force `FXSAVE64` (no `XSAVE`/AVX) on 2 vCPUs.
    assert_boot_case(
        name="boot_2cpu_fpu_fxsave_fallback",
        kernel_elf=kernel_elf,
        cmdline="console=com1 fpu_mode=fxsave",
        smp=2,
        expected_rc=EXIT_CODE_SUCCESS,
        must_contain=["[DRY-FPU] PASS: cpus=2 mode=fxsave xcr0=0x3 area_bytes=512", "[DRY-BOOT-OK]"],
        must_not_contain=["[DRY-PANIC]", "[DRY-EXCEPTION]"],
    )

    # Headless fallback verification (`Stage E.4`): boot without GOP (`-vga none`) and confirm graceful serial-only boot.
    assert_positive_smp_boot(
        "boot_2cpu_headless_nofb",
        kernel_elf,
        smp=2,
        cmdline="console=com1",
        expect_fb=False,
        extra_qemu_args=["-vga", "none"],
        mem=args.mem,
    )

    # Ring 3 GOP visual proof (`Stage E.2 & E.3`): QMP screendump -> PNG + pixel-level contrast verification.
    screendump_esp = BUILD_DIR / "esp_boot_display_screendump.img"
    write_mbr_esp_image(screendump_esp, kernel_elf, "console=com1 screendump_hold=1")
    capture_and_verify_gop_screendump(
        img_path=screendump_esp,
        ovmf_code=OVMF_CODE,
        ovmf_vars=OVMF_VARS,
        build_dir=BUILD_DIR,
        png_out=BUILD_DIR / "dry_os_first_screen.png",
    )

    n_neg = run_legacy_negative_matrix(kernel_elf, assert_boot_case, EXIT_CODE_PANIC)

    run_p1_suite(
        kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, assert_boot_case, EXIT_CODE_PANIC
    )
    run_p2_suite(
        kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, assert_boot_case, EXIT_CODE_PANIC
    )
    n_p3 = run_p3_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p4 = run_p4_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p5 = run_p5_island_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p6 = run_p6_lx_island_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p7 = run_p7_gpu_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p8 = run_p8_hw_suite(kernel_elf, BUILD_DIR, ROOT, write_mbr_esp_image, run_qemu)
    n_p9 = run_p9_glass_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p10 = run_p10_glass2_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p11 = run_p11_glass21_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p12 = run_p12_lx_compat_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p13 = run_p13_usb_persist_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p14 = run_p14_lx_net_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p15 = run_p15_brand_icons_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)
    n_p16 = run_p16_ascension6_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, run_qemu)
    n_p17 = run_p17_ascension7_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image, run_qemu)
    n_p18 = run_p18_hwval_suite(kernel_elf, BUILD_DIR, ROOT, OVMF_CODE, OVMF_VARS, write_mbr_esp_image)

    print(
        f"\n[QEMU-TEST] ALL {33 + n_neg + n_p3 + n_p4 + n_p5 + n_p6 + n_p7 + n_p8 + n_p9 + n_p10 + n_p11 + n_p12 + n_p13 + n_p14 + n_p15 + n_p16 + n_p17 + n_p18} DETERMINISTIC QEMU CHECKS "
        f"({10 + n_neg} LEGACY MILESTONE 0-1C+2A-4a + 9 MILESTONE 2A-1 P1 + 14 MILESTONE 2A-2 + "
        f"{n_p3} MILESTONE 2A-3 + {n_p4} MILESTONE 2B + {n_p5} MILESTONE 2C-0 + "
        f"{n_p6} MILESTONE 2C-1 + {n_p7} PRIORITY-C GPU + {n_p8} MILESTONE 2C-2 HW + "
        f"{n_p9} DRY GLASS + {n_p10} DRY GLASS 2.0 + {n_p11} DRY GLASS 2.1 + {n_p12} LX DRM + "
        f"{n_p13} M2 USB/NVME + {n_p14} M3 LX NET + {n_p15} BRAND/ICONS + {n_p16} ASCENSION 6.0 + "
        f"{n_p17} ASCENSION 7.0 + {n_p18} ASCENSION 8.0) PASSED."
    )


if __name__ == "__main__":
    main()
