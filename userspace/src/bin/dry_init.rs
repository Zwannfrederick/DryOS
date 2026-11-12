// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry_init` — Root Userspace Supervisor (`CPL=3`, `SecDomain::RootSystem`).
//!
//! Dispatches by `RootInitBootInfo::session_mode`:
//! - `0`: Milestone 1C Service & IPC Proof (`init_legacy::run_milestone_1c_proof`)
//! - `1`: Milestone 2A-1 P1 Interactive Session (`init_legacy::run_p1_interactive_session`)
//! - `2..=9`: Milestone 2A-2 Multi-Window Desktop Preview (`init_legacy::run_p2_desktop_session`)
//! - `>= 10`: Milestone 2A-3 Functional Desktop, Native Apps & Power Lifecycle (`supervisor::run_p3_desktop_session`)

#![no_std]
#![no_main]

use core::ptr::read_volatile;

use dry_userspace::init_legacy::{
    run_milestone_1c_proof, run_p1_interactive_session, run_p2_desktop_session,
};
use dry_userspace::supervisor::run_p3_desktop_session;
use dry_userspace::{read_cs_ss, sys_exit, RootInitBootInfo, ROOT_INIT_BOOT_MAGIC};

#[no_mangle]
pub extern "C" fn _start(boot_info_ptr: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || boot_info_ptr == 0 {
        sys_exit(70);
    }

    // SAFETY: `boot_info_ptr` is mapped `USER_RO` in `dry_init`'s `VSpace` before `_start`.
    let boot = unsafe { read_volatile(boot_info_ptr as *const RootInitBootInfo) };
    if boot.magic != ROOT_INIT_BOOT_MAGIC || boot.echo_mod_cap == 0 {
        sys_exit(71);
    }

    match boot.session_mode {
        0 => run_milestone_1c_proof(&boot),
        1 => run_p1_interactive_session(&boot),
        2..=9 => run_p2_desktop_session(&boot),
        _ => run_p3_desktop_session(boot_info_ptr, &boot),
    }
}
