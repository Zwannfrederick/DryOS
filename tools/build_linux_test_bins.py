#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Build freestanding static x86-64 Linux ELF test binaries (`tests/linux_elf/*.c`)."""

from __future__ import annotations

import pathlib
import struct
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC_C = ROOT / "tests" / "linux_elf" / "lx_hello.c"
OUT_ELF = ROOT / "kernel" / "build" / "lx-hello.elf"


def _verify_static_wx_elf64(path: pathlib.Path) -> None:
    data = path.read_bytes()
    if len(data) < 64 or data[0:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise RuntimeError(f"{path} is not a 64-bit little-endian ELF")
    e_type, e_machine = struct.unpack_from("<HH", data, 16)
    e_phoff, = struct.unpack_from("<Q", data, 32)
    e_phentsize, e_phnum = struct.unpack_from("<HH", data, 54)
    if e_type != 2 or e_machine != 62:
        raise RuntimeError(f"{path} must be static ET_EXEC EM_X86_64 (got type={e_type}, mach={e_machine})")
    for i in range(e_phnum):
        p_type, p_flags = struct.unpack_from("<II", data, e_phoff + i * e_phentsize)
        if p_type == 1 and (p_flags & 0x3) == 0x3:
            raise RuntimeError(f"{path} PT_LOAD[{i}] violates W^X (flags={p_flags:#x})")


def build_lx_hello(out_path: pathlib.Path = OUT_ELF) -> pathlib.Path:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        "clang",
        "-target",
        "x86_64-unknown-linux-gnu",
        "-ffreestanding",
        "-nostdlib",
        "-static",
        "-fno-pie",
        "-fuse-ld=lld",
        "-O2",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-Wl,--build-id=none",
        "-Wl,-z,max-page-size=4096",
        "-Wl,-z,noexecstack",
        "-Wl,-z,separate-code",
        str(SRC_C),
        "-o",
        str(out_path),
    ]
    subprocess.check_call(cmd, cwd=ROOT)
    _verify_static_wx_elf64(out_path)
    return out_path


if __name__ == "__main__":
    elf = build_lx_hello()
    print(f"[LX-ELF] Built {elf} ({elf.stat().st_size} bytes, static ET_EXEC W^X OK)")
