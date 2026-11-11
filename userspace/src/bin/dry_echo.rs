// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry_echo` — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`) Capability IPC Echo Service.

#![no_std]
#![no_main]

use core::arch::asm;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_exit, sys_ipc_reply_inline,
    sys_ipc_reply_recv_inline, sys_notify_signal, sys_write_debug, IPC_OP_CRASH_UD2,
    IPC_OP_EXIT_CLEAN, IPC_OP_PING_SILENT, IPC_OP_PING_TRACE, IPC_OP_PONG,
};

const PING_WORD: u64 = u64::from_le_bytes(*b"ping\0\0\0\0");
const PONG_WORD: u64 = u64::from_le_bytes(*b"pong\0\0\0\0");

#[no_mangle]
pub extern "C" fn _start(ep_cap: u64, ready_notif_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || ep_cap == 0 {
        sys_exit(90);
    }

    if ready_notif_cap != 0 {
        let _ = sys_write_debug(b"[DRY-ECHO] Service started in CPL3\n");
        if sys_notify_signal(ready_notif_cap, 1).is_err() {
            sys_exit(91);
        }
    }

    let mut pending_reply_tok = 0u64;
    let mut pending_reply_tag = 0u64;
    let mut pending_reply_mrs = [0u64; 4];

    loop {
        let recv = sys_ipc_reply_recv_inline(
            ep_cap,
            pending_reply_tok,
            pending_reply_tag,
            pending_reply_mrs,
            0,
            0,
        );
        let (req_tag, reply_tok, req_mrs) = match recv {
            Ok(v) => v,
            Err(_) => sys_exit(92),
        };

        match msg_tag_label(req_tag) {
            IPC_OP_PING_TRACE => {
                if req_mrs[0] != PING_WORD {
                    sys_exit(93);
                }
                let _ = sys_write_debug(b"[DRY-ECHO] REPLY: pong\n");
                pending_reply_tok = reply_tok;
                pending_reply_tag = pack_msg_tag(IPC_OP_PONG, 0, 4, 0);
                pending_reply_mrs = [PONG_WORD, req_mrs[1] ^ 0x55aa, req_mrs[2], req_mrs[3]];
            }
            IPC_OP_PING_SILENT => {
                if req_mrs[0] != PING_WORD {
                    sys_exit(94);
                }
                pending_reply_tok = reply_tok;
                pending_reply_tag = pack_msg_tag(IPC_OP_PONG, 0, 4, 0);
                pending_reply_mrs = [PONG_WORD, req_mrs[1].wrapping_add(1), req_mrs[2], req_mrs[3]];
            }
            IPC_OP_CRASH_UD2 => {
                // Intentional CPL=3 `#UD` fault verifying supervisor crash isolation & `-EPIPE` recovery.
                // SAFETY: Trap is isolated to `dry_echo` by Ring 0 `handle_user_exception`.
                unsafe {
                    asm!("ud2", options(nomem, nostack));
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [PONG_WORD, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => sys_exit(95),
        }
    }
}
