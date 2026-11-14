// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-fault-test` — `FaultEndpoint` E2E probe (`Milestone 2A-4a` Contract C).
//!
//! One ELF, two roles:
//! - **Supervisor** (`_start(own_module_cap, SUPERVISOR_MAGIC)`, `RootSystem`): spawns children of
//!   itself with a bound fault endpoint and checks resume, kill policy, fault-storm bound, revoked
//!   fault capability, forged fault message, and kill-while-blocked-in-fault.
//! - **Child** (`_start(mode_cap, x)`, `Userspace`): the badge of `mode_cap` selects the fault.

#![no_std]
#![no_main]

use core::arch::asm;

use dry_userspace::init_legacy::{domain_spec, log_line};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, sys_cap_derive, sys_cap_query, sys_cap_revoke, sys_ep_create,
    sys_exit, sys_ipc_recv_ext, sys_ipc_reply_inline, sys_ipc_send_inline, sys_notify_create,
    sys_proc_kill, sys_proc_spawn, sys_proc_spawn_with_fault_ep, sys_proc_wait, sys_yield,
    UserCapInfo, UserIpcRecvExt, EAGAIN, FAULT_MSG_LABEL, FAULT_REPLY_KILL, FAULT_REPLY_RESUME,
    MSG_FLAG_NON_BLOCKING, RIGHT_CALL, RIGHT_GRANT, RIGHT_READ, RIGHT_SEND,
};

const SUPERVISOR_MAGIC: u64 = 0x4641_554c_5445_5031; // "FAULTEP1"
const CHILD_COMP: u16 = 0x0231;
const WAIT_TSC: u64 = 3_000_000_000;
const FAULT_BUDGET: u64 = 16;

const MODE_UD_THEN_EXIT: u64 = 1;
const MODE_PF_WRITE: u64 = 2;
const MODE_UD_STORM: u64 = 3;
const MODE_DELAYED_UD: u64 = 4;
const MODE_FORGE: u64 = 5;

const BADGE_FAULT: u64 = 0x00fa_0000;
const BADGE_ROGUE_CLIENT: u64 = 0x00c1_1e00;
const PF_PROBE_ADDR: u64 = 0x10;

fn fail(code: i32) -> ! {
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-FAULT-EP] FAIL code=");
    lb.push_u64(code as u64);
    log_line(&lb);
    sys_exit(code)
}

fn child_main(mode_cap: u64, forge_ep: u64) -> ! {
    let mut info = UserCapInfo::default();
    if sys_cap_query(mode_cap, &mut info) != 0 {
        sys_exit(90);
    }
    match info.badge {
        MODE_UD_THEN_EXIT => {
            // SAFETY: Deliberate `#UD`; the supervisor resumes us right after the 2-byte opcode.
            unsafe { asm!("ud2", options(nomem, nostack)) };
            sys_exit(41)
        }
        MODE_PF_WRITE => {
            // SAFETY: Deliberate write to an unmapped page (`#PF`, `CR2 = PF_PROBE_ADDR`).
            unsafe { core::ptr::write_volatile(PF_PROBE_ADDR as *mut u8, 1) };
            sys_exit(42)
        }
        MODE_UD_STORM => loop {
            // SAFETY: Deliberate `#UD`; resumed at the same `RIP` until the fault budget ends.
            unsafe { asm!("ud2", options(nomem, nostack)) };
        },
        MODE_DELAYED_UD => {
            for _ in 0..256 {
                let _ = sys_yield();
            }
            // SAFETY: Deliberate `#UD` after the supervisor revoked our fault capability.
            unsafe { asm!("ud2", options(nomem, nostack)) };
            sys_exit(44)
        }
        MODE_FORGE => {
            // An ordinary client imitating a kernel fault message (label only; badge is ours).
            let tag = pack_msg_tag(FAULT_MSG_LABEL, 0, 4, 0);
            let _ = sys_ipc_send_inline(forge_ep, tag, [6, 0x40_0000, 0, 0], WAIT_TSC);
            sys_exit(0)
        }
        _ => sys_exit(91),
    }
}

struct Sup {
    module_cap: u64,
    fault_ep: u64,
    mode_notif: u64,
}

impl Sup {
    fn spawn(&self, mode: u64, fault_cap: u64) -> u64 {
        match sys_proc_spawn_with_fault_ep(
            self.module_cap,
            0,
            CHILD_COMP,
            (self.mode_notif, RIGHT_READ, mode),
            fault_cap,
        ) {
            Ok((h, _)) => h,
            Err(e) => fail(1000 + (-e) as i32),
        }
    }

    fn fault_cap(&self, idx: u64) -> u64 {
        sys_cap_derive(self.fault_ep, RIGHT_READ | RIGHT_SEND | RIGHT_CALL, 0, BADGE_FAULT | idx)
            .unwrap_or_else(|_| fail(103))
    }

    /// Receive one message; returns `(label, badge, reply_token, mrs)`.
    fn recv(&self, timeout: u64) -> Result<(u32, u64, u64, [u64; 4]), isize> {
        let mut ext = UserIpcRecvExt::default();
        let (tag, _, mrs) = sys_ipc_recv_ext(self.fault_ep, 0, timeout, &mut ext)?;
        Ok((msg_tag_label(tag), ext.sender_badge, ext.reply_token, mrs))
    }

    fn expect_fault(&self, idx: u64, vector: u64, code: i32) -> (u64, [u64; 4]) {
        match self.recv(WAIT_TSC) {
            Ok((FAULT_MSG_LABEL, badge, tok, mrs))
                if badge == (BADGE_FAULT | idx) && (mrs[0] & 0xff) == vector && tok != 0 =>
            {
                (tok, mrs)
            }
            _ => fail(code),
        }
    }

    fn reply(&self, tok: u64, label: u32, new_rip: u64) {
        let _ = sys_ipc_reply_inline(self.fault_ep, tok, pack_msg_tag(label, 0, 1, 0), [new_rip, 0, 0, 0]);
    }

    fn no_message_pending(&self) -> bool {
        let mut ext = UserIpcRecvExt::default();
        sys_ipc_recv_ext(self.fault_ep, MSG_FLAG_NON_BLOCKING as u64, 0, &mut ext) == Err(EAGAIN)
    }
}

fn expect_wait(handle: u64, kind: isize, detail_low: u64, code: i32) {
    match sys_proc_wait(handle, 0, WAIT_TSC) {
        Ok((k, d)) if k == kind && (d & 0xffff_ffff) == detail_low => {}
        _ => fail(code),
    }
}

fn supervisor_main(module_cap: u64) -> ! {
    let obj_spec = domain_spec(0, CHILD_COMP);
    let sup = Sup {
        module_cap,
        fault_ep: sys_ep_create(obj_spec).unwrap_or_else(|_| fail(100)),
        mode_notif: sys_notify_create(obj_spec).unwrap_or_else(|_| fail(100)),
    };

    // A. Recover: `#UD` -> resume after the opcode -> child exits normally.
    let cap_a = sup.fault_cap(1);
    let h = sup.spawn(MODE_UD_THEN_EXIT, cap_a);
    let (tok, mrs) = sup.expect_fault(1, 6, 110);
    if !(0x40_0000..0x80_0000).contains(&mrs[1]) || mrs[3] == 0 {
        fail(111);
    }
    sup.reply(tok, FAULT_REPLY_RESUME, mrs[1] + 2);
    expect_wait(h, 0, 41, 112);

    // B. Terminate policy: `#PF` with exact `CR2` -> supervisor says kill -> `Faulted(#PF)`.
    let h = sup.spawn(MODE_PF_WRITE, cap_a);
    let (tok, mrs) = sup.expect_fault(1, 14, 120);
    if mrs[2] != PF_PROBE_ADDR {
        fail(121);
    }
    sup.reply(tok, FAULT_REPLY_KILL, 0);
    expect_wait(h, 1, 14, 122);

    // C. Fault storm: always "resume at the same RIP"; the kernel budget ends it.
    let h = sup.spawn(MODE_UD_STORM, cap_a);
    let mut storm = 0u64;
    while let Ok((FAULT_MSG_LABEL, badge, tok, _)) = sup.recv(WAIT_TSC / 4) {
        if badge != (BADGE_FAULT | 1) {
            fail(130);
        }
        storm += 1;
        sup.reply(tok, FAULT_REPLY_RESUME, 0);
        if storm > FAULT_BUDGET {
            fail(131);
        }
    }
    if storm != FAULT_BUDGET {
        fail(132);
    }
    expect_wait(h, 1, 6, 133);

    // D. Revoked supervisor capability: no delivery, default termination.
    let cap_d = sup.fault_cap(2);
    let h = sup.spawn(MODE_DELAYED_UD, cap_d);
    if sys_cap_revoke(cap_d, 1).is_err() {
        fail(140);
    }
    expect_wait(h, 1, 6, 141);
    if !sup.no_message_pending() {
        fail(142);
    }

    // E. Forgery: an ordinary client copies the fault label but cannot copy the badge.
    let rogue_cap = sys_cap_derive(sup.fault_ep, RIGHT_READ | RIGHT_SEND | RIGHT_GRANT, 0, BADGE_ROGUE_CLIENT)
        .unwrap_or_else(|_| fail(150));
    let (h, _) = sys_proc_spawn(
        sup.module_cap,
        0,
        CHILD_COMP,
        (sup.mode_notif, RIGHT_READ, MODE_FORGE),
        (rogue_cap, RIGHT_READ | RIGHT_SEND),
    )
    .unwrap_or_else(|_| fail(152));
    let forged_rejected = match sup.recv(WAIT_TSC) {
        Ok((FAULT_MSG_LABEL, badge, 0, _)) => u64::from(badge == BADGE_ROGUE_CLIENT),
        _ => fail(153),
    };
    expect_wait(h, 0, 0, 154);

    // F. A child blocked in its fault call stays killable and reapable.
    let h = sup.spawn(MODE_UD_THEN_EXIT, cap_a);
    for _ in 0..512 {
        let _ = sys_yield();
    }
    if sys_proc_kill(h).is_err() {
        fail(160);
    }
    expect_wait(h, 2, 0, 161);
    if !sup.no_message_pending() {
        fail(162);
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-FAULT-EP] OK resume=1 kill_policy=1 storm=");
    lb.push_u64(storm);
    lb.push_bytes(b" revoked_fallback=1 forged_rejected=");
    lb.push_u64(forged_rejected);
    lb.push_bytes(b" killed_blocked=1");
    log_line(&lb);
    let _ = sys_cap_revoke(sup.fault_ep, 2);
    let _ = sys_cap_revoke(sup.mode_notif, 2);
    sys_exit(if forged_rejected == 1 { 0 } else { 155 })
}

#[no_mangle]
pub extern "C" fn _start(arg0: u64, arg1: u64) -> ! {
    if arg1 == SUPERVISOR_MAGIC {
        supervisor_main(arg0)
    } else {
        child_main(arg0, arg1)
    }
}
