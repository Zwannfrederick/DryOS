// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Legacy `dry_init` Session Runners (`Milestone 1C` `mode=0`, `Milestone 2A-1` `mode=1`,
//! and `Milestone 2A-2` `mode=2..4`).
//! Extracted from `bin/dry_init.rs` so each module remains well below `< 800` lines while
//! preserving 100% behavioral parity across all 79 historical regression scenarios.

use crate::shell::LineBuilder;
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_derive, sys_cap_revoke, sys_ep_create, sys_exit,
    sys_frame_alloc, sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_recv_ext,
    sys_ipc_recv_inline, sys_ipc_reply_inline, sys_ipc_reply_with_grant, sys_ipc_send_inline,
    sys_notify_create, sys_notify_signal, sys_notify_wait, sys_proc_spawn, sys_proc_wait,
    sys_sysinfo, sys_write_debug, RootInitBootInfo, UserIpcRecvExt, UserSysInfo, BADGE_INPUT_SVC,
    BADGE_SUPERVISOR, BADGE_WIN_ROGUE_PROBE, BADGE_WIN_SHELL, BADGE_WIN_SYSMON, EAGAIN, EBADCAP,
    EPIPE, EREVOKED, ETIMEDOUT, IPC_OP_CRASH_UD2, IPC_OP_DISPLAY_DONE, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_INPUT_INIT_MOUSE, IPC_OP_INPUT_INIT_PORT,
    IPC_OP_INPUT_SET_WM_SINK, IPC_OP_INPUT_WAIT, IPC_OP_PING_SILENT, IPC_OP_PING_TRACE,
    IPC_OP_PONG, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO, IPC_OP_SUP_RECONNECT_INPUT,
    IPC_OP_SUP_SHUTDOWN, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_CLOSE,
    IPC_OP_WIN_POLL_EVENT, IPC_OP_WM_INPUT_EVENT, IPC_OP_WM_OWNER_GONE, IPC_OP_WM_QUERY_STATE,
    RIGHT_CALL, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND,
    RIGHT_SIGNAL, RIGHT_WAIT, RIGHT_WRITE,
};

const PING_WORD: u64 = u64::from_le_bytes(*b"ping\0\0\0\0");
const PONG_WORD: u64 = u64::from_le_bytes(*b"pong\0\0\0\0");

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;
const ECHO_COMP_ID: u16 = 0x0101;
const SHELL_COMP_ID: u16 = 0x0102;
const SYSMON_COMP_ID: u16 = 0x0103;
const DISPLAY_COMP_ID: u16 = 0x0002;
const INPUT_COMP_ID: u16 = 0x0003;
const SYSMON_SHUTDOWN_BIT: u64 = 1 << 1;

#[inline]
pub const fn domain_spec(domain_code: u8, comp_id: u16) -> u64 {
    (domain_code as u64) | (1u64 << 8) | ((comp_id as u64) << 16)
}

pub fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
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

pub fn spawn_and_init_desktop_input(
    boot: &RootInitBootInfo,
    disp_ep: u64,
) -> Result<(u64, u64), i32> {
    let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let input_ep = sys_ep_create(ipc_spec).map_err(|_| 140)?;

    let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;
    let irq_rights = RIGHT_READ | RIGHT_SIGNAL | RIGHT_WAIT;
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
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

    if boot.irq12_cap != 0 {
        let _ = sys_ipc_call_with_grant(
            input_ep,
            IPC_OP_INPUT_INIT_MOUSE,
            [0; 4],
            boot.irq12_cap,
            irq_rights,
            0,
            0,
        );
    }
    if disp_ep != 0 {
        let _ = sys_ipc_call_with_grant(
            input_ep,
            IPC_OP_INPUT_SET_WM_SINK,
            [0; 4],
            disp_ep,
            cli_rights,
            BADGE_INPUT_SVC,
            0,
        );
    }
    Ok((input_handle, input_ep))
}

pub fn run_wm_security_audit(disp_ep: u64) -> Result<(), i32> {
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let rogue_ep = sys_cap_derive(disp_ep, cli_rights, 0, BADGE_WIN_ROGUE_PROBE).map_err(|_| 170)?;

    for target_wid in [1u64, 2u64] {
        let (t, _, _) = sys_ipc_call_inline(
            rogue_ep,
            pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
            [target_wid, 0, 0, 0],
            0,
        )
        .map_err(|_| 171)?;
        if msg_tag_label(t) != IPC_OP_ERR_PERM {
            return Err(172);
        }
    }

    let (t_poll, _, _) = sys_ipc_call_inline(
        rogue_ep,
        pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    )
    .map_err(|_| 173)?;
    if msg_tag_label(t_poll) != IPC_OP_ERR_PERM {
        return Err(174);
    }

    let (t_close, _, _) = sys_ipc_call_inline(
        rogue_ep,
        pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    )
    .map_err(|_| 175)?;
    if msg_tag_label(t_close) != IPC_OP_ERR_PERM {
        return Err(176);
    }

    let (t_ev, _, _) = sys_ipc_call_inline(
        rogue_ep,
        pack_msg_tag(IPC_OP_WM_INPUT_EVENT, 0, 4, 0),
        [0; 4],
        0,
    )
    .map_err(|_| 177)?;
    if msg_tag_label(t_ev) != IPC_OP_ERR_PERM {
        return Err(178);
    }

    let (t_reap, _, _) = sys_ipc_call_inline(
        rogue_ep,
        pack_msg_tag(IPC_OP_WM_OWNER_GONE, 0, 1, 0),
        [BADGE_WIN_SHELL, 0, 0, 0],
        0,
    )
    .map_err(|_| 179)?;
    if msg_tag_label(t_reap) != IPC_OP_ERR_PERM {
        return Err(180);
    }

    let tmp_frame = sys_frame_alloc(0).map_err(|_| 181)?;
    let (t_geom, _, _) = sys_ipc_call_with_grant(
        rogue_ep,
        IPC_OP_SURFACE_ATTACH,
        [999 | (999 << 16), 0, 0, 0],
        tmp_frame,
        RIGHT_READ | RIGHT_MAP,
        0,
        0,
    )
    .map_err(|_| 182)?;
    let _ = sys_cap_revoke(tmp_frame, 2);
    let _ = sys_cap_revoke(rogue_ep, 1);
    if msg_tag_label(t_geom) != IPC_OP_ERR_INVAL {
        return Err(183);
    }

    let _ = sys_write_debug(
        b"[DRY-WM-SEC] PASS: cross_present_denied=true cross_input_denied=true close_hijack_denied=true forged_ev_denied=true forged_reap_denied=true bad_geom_denied=true\n",
    );
    Ok(())
}

fn reap_sysmon_if_exited(
    sysmon_alive: &mut bool,
    sysmon_handle: u64,
    sysmon_notif_cap: &mut u64,
    disp_sup_cap: u64,
    blocking: bool,
) {
    if !*sysmon_alive {
        return;
    }
    let flags = if blocking { 0 } else { 1 };
    match sys_proc_wait(sysmon_handle, flags, 0) {
        Ok((w_kind, w_detail)) => {
            *sysmon_alive = false;
            if *sysmon_notif_cap != 0 {
                let _ = sys_cap_revoke(*sysmon_notif_cap, 1);
                *sysmon_notif_cap = 0;
            }
            if disp_sup_cap != 0 {
                let _ = sys_ipc_call_inline(
                    disp_sup_cap,
                    pack_msg_tag(IPC_OP_WM_OWNER_GONE, 0, 1, 0),
                    [BADGE_WIN_SYSMON, 0, 0, 0],
                    0,
                );
            }
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-INIT] SYSMON_REAPED kind=");
            lb.push_u64(w_kind as u64);
            lb.push_bytes(b" vec=");
            lb.push_u64(w_detail & 0xff);
            lb.push_bytes(b" window_cleaned=true");
            log_line(&lb);
        }
        Err(EAGAIN) => {}
        Err(_) => {
            *sysmon_alive = false;
        }
    }
}

pub fn run_p2_desktop_session(boot: &RootInitBootInfo) -> ! {
    if boot.input_mod_cap == 0
        || boot.shell_mod_cap == 0
        || boot.sysmon_mod_cap == 0
        || boot.irq1_cap == 0
        || boot.i8042_port_cap == 0
    {
        sys_exit(139);
    }

    let _ = sys_write_debug(b"[DRY-INIT] Starting Milestone 2A-2 multi-window desktop session in CPL3\n");
    let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let poll_timeout_tsc = 40_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    let mut disp_handle = 0u64;
    let mut disp_ep = 0u64;
    let mut disp_sup_cap = 0u64;
    if boot.fb_cap != 0 && boot.display_mod_cap != 0 {
        disp_ep = match sys_ep_create(ipc_spec) {
            Ok(c) => c,
            Err(_) => sys_exit(144),
        };
        disp_sup_cap = match sys_cap_derive(disp_ep, cli_rights, 0, BADGE_SUPERVISOR) {
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
            (disp_ep, RIGHT_READ | RIGHT_RECV | RIGHT_REPLY, 0xd152),
            (fb_delegatable, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP),
        ) {
            Ok(v) => v,
            Err(_) => sys_exit(146),
        };
        disp_handle = dh;
    }

    let (mut input_handle, mut input_ep) = match spawn_and_init_desktop_input(boot, disp_ep) {
        Ok(v) => v,
        Err(code) => sys_exit(code),
    };

    let sup_ep = match sys_ep_create(ipc_spec) {
        Ok(c) => c,
        Err(_) => sys_exit(147),
    };
    let (shell_handle, _) = match sys_proc_spawn(
        boot.shell_mod_cap,
        0,
        SHELL_COMP_ID,
        (sup_ep, cli_rights, 0x5501),
        (input_ep, cli_rights),
    ) {
        Ok(v) => v,
        Err(_) => sys_exit(148),
    };
    let (sysmon_handle, _) = match sys_proc_spawn(
        boot.sysmon_mod_cap,
        0,
        SYSMON_COMP_ID,
        (sup_ep, cli_rights, 0x5502),
        (0, 0),
    ) {
        Ok(v) => v,
        Err(_) => sys_exit(165),
    };

    let mut sysmon_alive = true;
    let mut sysmon_notif_cap = 0u64;
    let mut shell_hello = false;
    let mut sysmon_hello = false;
    let mut sec_audit_done = boot.session_mode != 3 || disp_ep == 0;
    let mut restarts = 0u64;

    loop {
        reap_sysmon_if_exited(
            &mut sysmon_alive,
            sysmon_handle,
            &mut sysmon_notif_cap,
            disp_sup_cap,
            false,
        );

        if !sec_audit_done && shell_hello && sysmon_hello {
            if let Ok((_, _, qmrs)) = sys_ipc_call_inline(
                disp_sup_cap,
                pack_msg_tag(IPC_OP_WM_QUERY_STATE, 0, 1, 0),
                [0; 4],
                0,
            ) {
                if qmrs[3] >= 2 {
                    if let Err(code) = run_wm_security_audit(disp_ep) {
                        sys_exit(code);
                    }
                    sec_audit_done = true;
                }
            }
        }

        let mut ext = UserIpcRecvExt::default();
        let (tag, reply_tok, mrs) = match sys_ipc_recv_ext(sup_ep, 0, poll_timeout_tsc, &mut ext) {
            Ok(v) => v,
            Err(ETIMEDOUT | EAGAIN) => continue,
            Err(_) => break,
        };

        match msg_tag_label(tag) {
            IPC_OP_SUP_HELLO => {
                let caller_comp = mrs[0];
                let badge = if caller_comp == SYSMON_COMP_ID as u64 {
                    if ext.transferred_cap != 0 {
                        sysmon_notif_cap = ext.transferred_cap;
                    }
                    sysmon_hello = true;
                    BADGE_WIN_SYSMON
                } else {
                    shell_hello = true;
                    BADGE_WIN_SHELL
                };

                if disp_ep != 0 {
                    if sys_ipc_reply_with_grant(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [boot.session_mode, 0, 0, 0],
                        disp_ep,
                        cli_rights,
                        badge,
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
                        [boot.session_mode, 0, 0, 0],
                    );
                }
            }
            IPC_OP_SUP_APP_EXITED => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                if mrs[0] == SYSMON_COMP_ID as u64 {
                    reap_sysmon_if_exited(
                        &mut sysmon_alive,
                        sysmon_handle,
                        &mut sysmon_notif_cap,
                        disp_sup_cap,
                        true,
                    );
                }
            }
            IPC_OP_SUP_RECONNECT_INPUT => {
                let (w_kind, w_detail) = match sys_proc_wait(input_handle, 0, 0) {
                    Ok(v) => v,
                    Err(_) => sys_exit(150),
                };
                if w_kind != 1 || (w_detail & 0xff) != 6 {
                    sys_exit(151);
                }
                if sys_cap_revoke(input_ep, 2).is_err() {
                    sys_exit(154);
                }
                let (new_handle, new_ep) = match spawn_and_init_desktop_input(boot, disp_ep) {
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
                if !sec_audit_done {
                    if let Err(code) = run_wm_security_audit(disp_ep) {
                        sys_exit(code);
                    }
                }
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

    if sys_proc_wait(shell_handle, 0, 0) != Ok((0, 0)) {
        sys_exit(157);
    }

    if sysmon_alive {
        if sysmon_notif_cap != 0 {
            let _ = sys_notify_signal(sysmon_notif_cap, SYSMON_SHUTDOWN_BIT);
        }
        reap_sysmon_if_exited(
            &mut sysmon_alive,
            sysmon_handle,
            &mut sysmon_notif_cap,
            disp_sup_cap,
            true,
        );
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
            disp_sup_cap,
            pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
            [0; 4],
            0,
        );
        if sys_proc_wait(disp_handle, 0, 0) != Ok((0, 0)) {
            sys_exit(159);
        }
        let _ = sys_cap_revoke(disp_sup_cap, 1);
        let _ = sys_cap_revoke(disp_ep, 2);
    }

    let _ = sys_cap_revoke(sup_ep, 2);
    let _ = sys_write_debug(b"[DRY-INIT] P2 desktop session verified & reaped cleanly\n");
    sys_exit(0);
}

pub fn run_p1_interactive_session(boot: &RootInitBootInfo) -> ! {
    if boot.input_mod_cap == 0
        || boot.shell_mod_cap == 0
        || boot.irq1_cap == 0
        || boot.i8042_port_cap == 0
    {
        sys_exit(139);
    }

    let _ = sys_write_debug(b"[DRY-INIT] Starting P1 interactive session in CPL3\n");
    let shell_spec = domain_spec(0, SHELL_COMP_ID);

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

    let (mut input_handle, mut input_ep) = match spawn_and_init_input_service(boot) {
        Ok(v) => v,
        Err(code) => sys_exit(code),
    };

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

pub fn run_milestone_1c_proof(boot: &RootInitBootInfo) -> ! {
    let _ = sys_write_debug(b"[DRY-INIT] Root service started in CPL3\n");

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
