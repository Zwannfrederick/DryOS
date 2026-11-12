// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry_init` — Root Userspace Supervisor (`CPL=3`, `SecDomain::RootSystem`).
//!
//! Operates in two modes selected by `RootInitBootInfo::session_mode`:
//! - `0` (**Milestone 1C Service & IPC Proof**): `dry_echo` round-trip, `#UD` crash + stale-handle
//!   revocation + restart, and one-shot `dry_display` banner.
//! - `>= 1` (**Milestone 2A-1 / P1 Interactive Session**): launches `dry-display` compositor,
//!   `dry-input` (`IRQ1` + narrow `i8042` `IoPortCap`), and isolated `dry-shell` (`Userspace`),
//!   supervises live `dry-input` `#UD` crash recovery + `CSpace` revocation + `IRQ1` re-binding,
//!   and orchestrates zero-leak shutdown.

#![no_std]
#![no_main]

use core::ptr::read_volatile;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_exit, sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_recv_inline,
    sys_ipc_reply_inline, sys_ipc_reply_with_grant, sys_ipc_send_inline, sys_notify_create,
    sys_notify_wait, sys_proc_spawn, sys_proc_wait, sys_write_debug, RootInitBootInfo, EBADCAP,
    EPIPE, EREVOKED, IPC_OP_CRASH_UD2, IPC_OP_DISPLAY_DONE, IPC_OP_EXIT_CLEAN,
    IPC_OP_INPUT_INIT_PORT, IPC_OP_INPUT_WAIT, IPC_OP_PING_SILENT, IPC_OP_PING_TRACE, IPC_OP_PONG,
    IPC_OP_SUP_HELLO, IPC_OP_SUP_RECONNECT_INPUT, IPC_OP_SUP_SHUTDOWN, RIGHT_CALL, RIGHT_GRANT,
    RIGHT_MAP, RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_SIGNAL, RIGHT_WAIT,
    RIGHT_WRITE, ROOT_INIT_BOOT_MAGIC,
};

const PING_WORD: u64 = u64::from_le_bytes(*b"ping\0\0\0\0");
const PONG_WORD: u64 = u64::from_le_bytes(*b"pong\0\0\0\0");

const ECHO_COMP_ID: u16 = 0x0101;
const SHELL_COMP_ID: u16 = 0x0102;
const DISPLAY_COMP_ID: u16 = 0x0002;
const INPUT_COMP_ID: u16 = 0x0003;

#[inline]
const fn domain_spec(domain_code: u8, comp_id: u16) -> u64 {
    (domain_code as u64) | (1u64 << 8) | ((comp_id as u64) << 16)
}

fn spawn_and_init_input_service(boot: &RootInitBootInfo) -> Result<(u64, u64), i32> {
    let shell_spec = domain_spec(0, SHELL_COMP_ID);
    let input_ep = sys_ep_create(shell_spec).map_err(|_| 140)?;

    let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;
    let irq_rights = RIGHT_READ | RIGHT_SIGNAL | RIGHT_WAIT;
    let (input_handle, _) = sys_proc_spawn(
        boot.input_mod_cap,
        1,
        INPUT_COMP_ID,
        (input_ep, srv_rights, 0x1040),
        (boot.irq1_cap, irq_rights),
    )
    .map_err(|_| 141)?;

    let (rtag, _, _) = sys_ipc_call_with_grant(
        input_ep,
        IPC_OP_INPUT_INIT_PORT,
        [0; 4],
        boot.i8042_port_cap,
        RIGHT_READ | RIGHT_WRITE,
        0,
        0,
    )
    .map_err(|_| 142)?;
    if msg_tag_label(rtag) != IPC_OP_PONG {
        return Err(143);
    }

    Ok((input_handle, input_ep))
}

fn run_p1_interactive_session(boot: &RootInitBootInfo) -> ! {
    if boot.input_mod_cap == 0
        || boot.shell_mod_cap == 0
        || boot.irq1_cap == 0
        || boot.i8042_port_cap == 0
    {
        sys_exit(139);
    }

    let _ = sys_write_debug(b"[DRY-INIT] Starting P1 interactive session in CPL3\n");
    let shell_spec = domain_spec(0, SHELL_COMP_ID);

    // 1. Optional `dry-display` Ring 3 desktop compositor (`fb_cap != 0` unless `-vga none`).
    let mut disp_handle = 0u64;
    let mut disp_ep = 0u64;
    if boot.fb_cap != 0 && boot.display_mod_cap != 0 {
        disp_ep = match sys_ep_create(shell_spec) {
            Ok(c) => c,
            Err(_) => sys_exit(144),
        };
        let fb_delegatable = match sys_cap_derive(
            boot.fb_cap,
            RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT,
            0,
            0,
        ) {
            Ok(c) => c,
            Err(_) => sys_exit(145),
        };
        let (dh, _) = match sys_proc_spawn(
            boot.display_mod_cap,
            1,
            DISPLAY_COMP_ID,
            (disp_ep, RIGHT_READ | RIGHT_RECV | RIGHT_REPLY, 0xd151),
            (fb_delegatable, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP),
        ) {
            Ok(v) => v,
            Err(_) => sys_exit(146),
        };
        disp_handle = dh;
    }

    // 2. Spawn `dry-input` (`RootSystem`, `0x0003`) & bind `IRQ1` + narrow `i8042` `IoPortCap`.
    let (mut input_handle, mut input_ep) = match spawn_and_init_input_service(boot) {
        Ok(v) => v,
        Err(code) => sys_exit(code),
    };

    // 3. Spawn unprivileged `dry-shell` (`Userspace`, `0x0102`) with `(sup_ep, input_ep)`.
    let sup_ep = match sys_ep_create(shell_spec) {
        Ok(c) => c,
        Err(_) => sys_exit(147),
    };
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let (shell_handle, _) = match sys_proc_spawn(
        boot.shell_mod_cap,
        0,
        SHELL_COMP_ID,
        (sup_ep, cli_rights, 0x5500),
        (input_ep, cli_rights),
    ) {
        Ok(v) => v,
        Err(_) => sys_exit(148),
    };

    // 4. Supervise `dry-shell` & `dry-input` lifecycle until `IPC_OP_SUP_SHUTDOWN`.
    let mut restarts = 0u64;
    while let Ok((tag, reply_tok, _mrs)) = sys_ipc_recv_inline(sup_ep, 0, 0) {
        match msg_tag_label(tag) {
            IPC_OP_SUP_HELLO => {
                if disp_ep != 0 {
                    if sys_ipc_reply_with_grant(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [1, 0, 0, 0],
                        disp_ep,
                        cli_rights,
                        0x54e2,
                    )
                    .is_err()
                    {
                        sys_exit(149);
                    }
                } else {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [0; 4],
                    );
                }
            }
            IPC_OP_SUP_RECONNECT_INPUT => {
                // Reap crashed `dry-input`, verify `-EPIPE` + post-revoke rejection, and respawn!
                let (w_kind, w_detail) = match sys_proc_wait(input_handle, 0, 0) {
                    Ok(v) => v,
                    Err(_) => sys_exit(150),
                };
                if w_kind != 1 || (w_detail & 0xff) != 6 {
                    sys_exit(151);
                }
                let probe_cli = match sys_cap_derive(input_ep, cli_rights, 0, 0x99) {
                    Ok(c) => c,
                    Err(_) => sys_exit(152),
                };
                if sys_ipc_call_inline(
                    probe_cli,
                    pack_msg_tag(IPC_OP_INPUT_WAIT, 0, 1, 0),
                    [0; 4],
                    0,
                ) != Err(EPIPE)
                {
                    sys_exit(153);
                }
                if sys_cap_revoke(input_ep, 2).is_err() {
                    sys_exit(154);
                }
                let post_rev = sys_ipc_call_inline(
                    probe_cli,
                    pack_msg_tag(IPC_OP_INPUT_WAIT, 0, 1, 0),
                    [0; 4],
                    0,
                );
                if post_rev != Err(EREVOKED) && post_rev != Err(EBADCAP) {
                    sys_exit(155);
                }

                let (new_handle, new_ep) = match spawn_and_init_input_service(boot) {
                    Ok(v) => v,
                    Err(code) => sys_exit(code),
                };
                input_handle = new_handle;
                input_ep = new_ep;
                restarts += 1;
                let _ = sys_write_debug(
                    b"[DRY-INIT] Reaped crashed dry-input, revoked stale cap & restarted\n",
                );

                if sys_ipc_reply_with_grant(
                    sup_ep,
                    reply_tok,
                    IPC_OP_PONG,
                    [restarts, 0, 0, 0],
                    input_ep,
                    cli_rights,
                    0x54e1,
                )
                .is_err()
                {
                    sys_exit(156);
                }
            }
            IPC_OP_SUP_SHUTDOWN => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                break;
            }
            _ => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [0; 4],
                    );
                }
            }
        }
    }

    // 5. Deterministic zero-leak teardown of `dry-shell`, `dry-input`, and `dry-display`.
    if sys_proc_wait(shell_handle, 0, 0) != Ok((0, 0)) {
        sys_exit(157);
    }

    let _ = sys_ipc_call_inline(
        input_ep,
        pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
        [0; 4],
        0,
    );
    if sys_proc_wait(input_handle, 0, 0) != Ok((0, 0)) {
        sys_exit(158);
    }
    let _ = sys_cap_revoke(input_ep, 2);

    if disp_ep != 0 {
        let _ = sys_ipc_call_inline(
            disp_ep,
            pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
            [0; 4],
            0,
        );
        if sys_proc_wait(disp_handle, 0, 0) != Ok((0, 0)) {
            sys_exit(159);
        }
        let _ = sys_cap_revoke(disp_ep, 2);
    }

    let _ = sys_cap_revoke(sup_ep, 2);
    let _ = sys_write_debug(b"[DRY-INIT] P1 interactive session verified & reaped cleanly\n");
    sys_exit(0);
}

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

    if boot.session_mode >= 1 {
        run_p1_interactive_session(&boot);
    }

    let _ = sys_write_debug(b"[DRY-INIT] Root service started in CPL3\n");

    // 1. Create `Endpoint` + startup `Notification` scoped to `dry_echo`'s `Userspace` compartment.
    let echo_spec = domain_spec(0, ECHO_COMP_ID);
    let ep1_root = match sys_ep_create(echo_spec) {
        Ok(c) => c,
        Err(_) => sys_exit(72),
    };
    let _ = sys_write_debug(b"[DRY-IPC] Endpoint created\n");

    let ready_notif = match sys_notify_create(echo_spec) {
        Ok(c) => c,
        Err(_) => sys_exit(73),
    };

    // 2. Spawn `dry_echo` in `SecDomain::Userspace` with narrowed `(RECV|REPLY, SIGNAL)` caps.
    let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;
    let sig_rights = RIGHT_READ | RIGHT_SIGNAL;
    let (echo1_handle, _) = match sys_proc_spawn(
        boot.echo_mod_cap,
        0,
        ECHO_COMP_ID,
        (ep1_root, srv_rights, 0xec40),
        (ready_notif, sig_rights),
    ) {
        Ok(v) => v,
        Err(_) => sys_exit(74),
    };

    if sys_notify_wait(ready_notif, 0, 0) != Ok(1) {
        sys_exit(75);
    }

    // 3. Derive narrowed client capability (`SEND | CALL`, badged) & execute `ping -> pong` round-trip.
    let cli_ep1 = match sys_cap_derive(ep1_root, RIGHT_READ | RIGHT_SEND | RIGHT_CALL, 0, 0x00c1_1e01) {
        Ok(c) => c,
        Err(_) => sys_exit(76),
    };

    let _ = sys_write_debug(b"[DRY-IPC] CALL: ping\n");
    let (rep_tag, _, rep_mrs) = match sys_ipc_call_inline(
        cli_ep1,
        pack_msg_tag(IPC_OP_PING_TRACE, 0, 4, 0),
        [PING_WORD, 0x1234, 0x77, 0x88],
        0,
    ) {
        Ok(v) => v,
        Err(_) => sys_exit(77),
    };
    if msg_tag_label(rep_tag) != IPC_OP_PONG || rep_mrs[0] != PONG_WORD || rep_mrs[1] != (0x1234 ^ 0x55aa) {
        sys_exit(78);
    }
    let _ = sys_write_debug(b"[DRY-IPC] Roundtrip successful\n");

    // 4. Crash Supervision & Restart Verification (`Stage C.4, C.5, D.4, F.13`).
    let _ = sys_ipc_send_inline(cli_ep1, pack_msg_tag(IPC_OP_CRASH_UD2, 0, 1, 0), [0; 4], 0);
    let (wait_kind1, wait_detail1) = match sys_proc_wait(echo1_handle, 0, 0) {
        Ok(v) => v,
        Err(_) => sys_exit(79),
    };
    if wait_kind1 != 1 || (wait_detail1 & 0xff) != 6 {
        sys_exit(80);
    }

    let dead_call = sys_ipc_call_inline(cli_ep1, pack_msg_tag(IPC_OP_PING_SILENT, 0, 1, 0), [PING_WORD, 0, 0, 0], 0);
    if dead_call != Err(EPIPE) {
        sys_exit(81);
    }
    if sys_cap_revoke(ep1_root, 2).is_err() {
        sys_exit(82);
    }
    let stale_call = sys_ipc_call_inline(cli_ep1, pack_msg_tag(IPC_OP_PING_SILENT, 0, 1, 0), [PING_WORD, 0, 0, 0], 0);
    if stale_call != Err(EREVOKED) && stale_call != Err(EBADCAP) {
        sys_exit(83);
    }
    let _ = sys_cap_revoke(ready_notif, 2);

    let ep2_root = match sys_ep_create(echo_spec) {
        Ok(c) => c,
        Err(_) => sys_exit(84),
    };
    let (echo2_handle, _) = match sys_proc_spawn(
        boot.echo_mod_cap,
        0,
        ECHO_COMP_ID,
        (ep2_root, srv_rights, 0xec41),
        (0, 0),
    ) {
        Ok(v) => v,
        Err(_) => sys_exit(85),
    };
    let cli_ep2 = match sys_cap_derive(ep2_root, RIGHT_READ | RIGHT_SEND | RIGHT_CALL, 0, 0x00c1_1e02) {
        Ok(c) => c,
        Err(_) => sys_exit(86),
    };
    let (rtag2, _, rmrs2) = match sys_ipc_call_inline(
        cli_ep2,
        pack_msg_tag(IPC_OP_PING_SILENT, 0, 2, 0),
        [PING_WORD, 41, 0, 0],
        0,
    ) {
        Ok(v) => v,
        Err(_) => sys_exit(87),
    };
    if msg_tag_label(rtag2) != IPC_OP_PONG || rmrs2[0] != PONG_WORD || rmrs2[1] != 42 {
        sys_exit(88);
    }
    let _ = sys_ipc_call_inline(cli_ep2, pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0), [0; 4], 0);
    if sys_proc_wait(echo2_handle, 0, 0) != Ok((0, 0)) {
        sys_exit(89);
    }
    let _ = sys_cap_revoke(ep2_root, 2);

    // 5. Optional Ring 3 Display Service (`Stage E`): spawn `dry_display` if GOP `fb_cap` present.
    if boot.fb_cap != 0 && boot.display_mod_cap != 0 {
        let disp_spec = domain_spec(1, DISPLAY_COMP_ID);
        let disp_ep = match sys_ep_create(disp_spec) {
            Ok(c) => c,
            Err(_) => sys_exit(96),
        };
        let fb_delegatable = match sys_cap_derive(
            boot.fb_cap,
            RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT,
            0,
            0,
        ) {
            Ok(c) => c,
            Err(_) => sys_exit(97),
        };
        let (disp_handle, _) = match sys_proc_spawn(
            boot.display_mod_cap,
            1,
            DISPLAY_COMP_ID,
            (disp_ep, RIGHT_READ | RIGHT_SEND | RIGHT_CALL, 0xd150),
            (fb_delegatable, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP),
        ) {
            Ok(v) => v,
            Err(_) => sys_exit(98),
        };
        let (d_tag, d_reply_tok, d_mrs) = match sys_ipc_recv_inline(disp_ep, 0, 0) {
            Ok(v) => v,
            Err(_) => sys_exit(99),
        };
        if msg_tag_label(d_tag) != IPC_OP_DISPLAY_DONE || d_mrs[0] == 0 || d_mrs[2] == 0 {
            sys_exit(100);
        }
        let _ = sys_ipc_reply_inline(disp_ep, d_reply_tok, pack_msg_tag(IPC_OP_PONG, 0, 1, 0), [1, 0, 0, 0]);
        if sys_proc_wait(disp_handle, 0, 0) != Ok((0, 0)) {
            sys_exit(102);
        }
        let _ = sys_cap_revoke(disp_ep, 2);
    }

    let _ = sys_write_debug(b"[DRY-INIT] Service lifecycle verified\n");
    sys_exit(0);
}
