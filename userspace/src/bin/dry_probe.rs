// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal `#![no_std]` dynamic VFS executable probe (`/bin/dry-probe.elf`).

#![no_std]
#![no_main]

use dry_userspace::shell::LineBuilder;
use dry_userspace::{read_cs_ss, sys_exit, sys_write_debug};

#[unsafe(no_mangle)]
pub extern "C" fn _start(arg0: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 {
        sys_exit(200);
    }
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-PROBE] PASS: dyn_exec=1 arg0=");
    lb.push_u64(arg0);
    lb.push_byte(b'\n');
    let _ = sys_write_debug(lb.as_bytes());
    sys_exit(0);
}
