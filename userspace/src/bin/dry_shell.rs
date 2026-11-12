// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-shell` — Unprivileged Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0102`) Native Terminal.
//!
//! Holds ZERO hardware capabilities (`0` `IrqCap`, `0` `IoPortCap`, `0` `Framebuffer` cap):
//! - Connects to real microkernel (`SYS_SYSINFO`, `SYS_PROC_LIST`) and supervisor (`IPC_OP_SUP_ACTION`,
//!   `IPC_OP_SUP_QUERY_APPS`) APIs.
//! - Supports line editing, 16-entry command history (`Up`/`Down`), 128-line scrollback (`PgUp`/`PgDn`,
//!   `Shift+Up/Down`, mouse wheel), and explicit rejection of unsupported POSIX/bash metacharacters.

#![no_std]
#![no_main]

use core::cell::UnsafeCell;

use dry_userspace::action::{AppId, DesktopAction};
use dry_userspace::input::{InputEvent, InputEventKind, MOD_ALT, MOD_CTRL, MOD_SHIFT};
use dry_userspace::keymap::{
    KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END, KEY_ENTER, KEY_HOME, KEY_LEFT, KEY_PAGEDOWN,
    KEY_PAGEUP, KEY_RIGHT, KEY_UP,
};
use dry_userspace::shell::{parse_command, LineBuilder, ShellCommand, ShellEngine, MAX_INPUT_LEN};
use dry_userspace::surface::{
    SharedWindowSurface, STYLE_DEFAULT, STYLE_MUTED, STYLE_OK, STYLE_WARN, TERM_COLS, TERM_ROWS,
};
use dry_userspace::vfs_proto::{vfs_create_file, vfs_read_file, vfs_unlink_file};
use dry_userspace::wm::{WindowRole, TITLE_KIND_TERMINAL};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_send_inline, sys_proc_kill,
    sys_proc_list, sys_sysinfo, sys_vspace_map, sys_write_debug, UserProcInfo, UserSysInfo,
    EBADCAP, EPERM, EPIPE, EREVOKED, EROFS, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS,
    IPC_OP_ERR_PERM, IPC_OP_INPUT_EVENT, IPC_OP_INPUT_OVERFLOW_TEST, IPC_OP_INPUT_WAIT,
    IPC_OP_PONG, IPC_OP_SUP_ACTION, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_GET_VFS_CAP, IPC_OP_SUP_HELLO,
    IPC_OP_SUP_MINT_ROGUE_TEST, IPC_OP_SUP_QUERY_APPS, IPC_OP_SUP_RECONNECT_INPUT,
    IPC_OP_SUP_SHUTDOWN, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_MOVE, IPC_OP_SURFACE_PRESENT,
    IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED, IPC_OP_WIN_WAIT_EVENT, IPC_OP_WM_TOGGLE_TILING, RIGHT_MAP,
    RIGHT_READ, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const SHELL_SURF_VADDR: u64 = 0x1800_0000;
const DESKTOP_WIN_COLS: u16 = 70;
const DESKTOP_WIN_ROWS: u16 = 22;
const DESKTOP_WIN_X: u64 = 24;
const DESKTOP_WIN_Y: u64 = 52;

struct SingleThreadCell(UnsafeCell<ShellEngine>);
// SAFETY: `dry-shell` executes strictly on a single CPL=3 thread.
unsafe impl Sync for SingleThreadCell {}

static ENGINE: SingleThreadCell = SingleThreadCell(UnsafeCell::new(ShellEngine::new()));

fn debug_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn present_surface(disp_ep_cap: u64, win_id: u32) -> (u64, u64) {
    if disp_ep_cap == 0 {
        return (0, 0);
    }
    match sys_ipc_call_inline(
        disp_ep_cap,
        pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
        [win_id as u64, 0, 0, 0],
        0,
    ) {
        Ok((_, _, mrs)) => (mrs[0], mrs[1]),
        Err(_) => (0, 0),
    }
}

fn move_floating_window(disp_ep_cap: u64, win_id: u32, dx: i64, dy: i64) {
    if disp_ep_cap == 0 {
        return;
    }
    if let Ok((_, _, mrs)) = sys_ipc_call_inline(
        disp_ep_cap,
        pack_msg_tag(IPC_OP_SURFACE_MOVE, 0, 3, 0),
        [dx as u64, dy as u64, win_id as u64, 0],
        0,
    ) {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-SHELL] WIN_MOVE x=");
        lb.push_u64(mrs[0]);
        lb.push_bytes(b" y=");
        lb.push_u64(mrs[1]);
        lb.push_bytes(b" blit_cyc=");
        lb.push_u64(mrs[2]);
        debug_line(&lb);
    }
}

fn call_sup_action(sup_ep_cap: u64, act: DesktopAction) -> Result<[u64; 4], u32> {
    let words = act.to_words();
    let (rtag, _, rmrs) = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
        [words[0], words[1], 0, 0],
        0,
    )
    .map_err(|_| IPC_OP_ERR_INVAL)?;
    let lbl = msg_tag_label(rtag);
    if lbl == IPC_OP_PONG {
        Ok(rmrs)
    } else {
        Err(lbl)
    }
}

fn run_live_p3_sec_probes(sup_ep_cap: u64, engine: &mut ShellEngine) {
    let kill_eperm = sys_proc_kill(1) == Err(EPERM);

    let mut unauth_perm = false;
    if let Ok((rtag, rogue_cap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_MINT_ROGUE_TEST, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(rtag) == IPC_OP_PONG && rogue_cap != 0 {
            let r1 = call_sup_action(rogue_cap, DesktopAction::LaunchOrFocusApp(AppId::LifeStation));
            let r2 = call_sup_action(rogue_cap, DesktopAction::CloseApp(AppId::Terminal));
            let r3 = call_sup_action(rogue_cap, DesktopAction::RequestShutdown);
            let r4 = call_sup_action(rogue_cap, DesktopAction::RequestReboot);
            let _ = sys_cap_revoke(rogue_cap, 1);
            unauth_perm = r1 == Err(IPC_OP_ERR_PERM)
                && r2 == Err(IPC_OP_ERR_PERM)
                && r3 == Err(IPC_OP_ERR_PERM)
                && r4 == Err(IPC_OP_ERR_PERM);
        }
    }

    let bad1 = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
        [0xdead_beef, 999, 0, 0],
        0,
    )
    .map(|(t, _, _)| msg_tag_label(t));
    let bad2 = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
        [1, 99, 0, 0],
        0,
    )
    .map(|(t, _, _)| msg_tag_label(t));
    let bad_ipc_inval = bad1 == Ok(IPC_OP_ERR_INVAL) && bad2 == Ok(IPC_OP_ERR_INVAL);

    let mut vfs_rofs = false;
    let mut vfs_eperm = false;
    if let Ok((vtag, vcap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_GET_VFS_CAP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(vtag) == IPC_OP_PONG && vcap != 0 {
            let mut scratch = [0u8; 32];
            vfs_rofs = vfs_create_file(vcap, b"/boot/pwn.txt", 0) == Err(EROFS);
            vfs_eperm = vfs_read_file(vcap, b"/ram/vault_key.priv", 0, &mut scratch) == Err(EPERM)
                && vfs_unlink_file(vcap, b"/ram/vault_key.priv") == Err(EPERM);
            let _ = sys_cap_revoke(vcap, 1);
        }
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"Security audit: kill_eperm=");
    lb.push_bytes(if kill_eperm { b"true" } else { b"false" });
    lb.push_bytes(b" unauth_perm=");
    lb.push_bytes(if unauth_perm { b"true" } else { b"false" });
    lb.push_bytes(b" vfs_eperm=");
    lb.push_bytes(if vfs_eperm { b"true" } else { b"false" });
    engine.push_line(
        lb.as_bytes(),
        if kill_eperm && unauth_perm && bad_ipc_inval && vfs_rofs && vfs_eperm {
            STYLE_OK
        } else {
            STYLE_WARN
        },
    );

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=sec-audit kill_eperm=");
    slb.push_bytes(if kill_eperm { b"true" } else { b"false" });
    slb.push_bytes(b" unauth_perm=");
    slb.push_bytes(if unauth_perm { b"true" } else { b"false" });
    slb.push_bytes(b" bad_ipc_inval=");
    slb.push_bytes(if bad_ipc_inval { b"true" } else { b"false" });
    slb.push_bytes(b" vfs_rofs=");
    slb.push_bytes(if vfs_rofs { b"true" } else { b"false" });
    slb.push_bytes(b" vfs_eperm=");
    slb.push_bytes(if vfs_eperm { b"true" } else { b"false" });
    debug_line(&slb);
}

#[allow(clippy::too_many_arguments)]
fn reconnect_input_service(
    sup_ep_cap: u64,
    disp_ep_cap: u64,
    win_id: u32,
    input_ep_cap: &mut u64,
    engine: &mut ShellEngine,
    surf: &SharedWindowSurface,
    win_cols: usize,
    win_rows: usize,
    sys: &mut UserSysInfo,
) -> bool {
    match sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_RECONNECT_INPUT, [0; 4], 0, 0, 0, 0) {
        Ok((rtag, new_ep, rmrs)) if msg_tag_label(rtag) == IPC_OP_PONG && new_ep != 0 => {
            let _ = sys_cap_revoke(*input_ep_cap, 1);
            *input_ep_cap = new_ep;
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[supervisor] dry-input restarted (#");
            lb.push_u64(rmrs[0]);
            lb.push_bytes(b"), stale cap revoked, IRQ1 re-bound");
            engine.push_line(lb.as_bytes(), STYLE_OK);
            let _ = sys_sysinfo(sys);
            engine.project_to_surface(surf, win_cols, win_rows, Some(sys));
            let _ = present_surface(disp_ep_cap, win_id);
            let _ = sys_write_debug(b"[DRY-SHELL] INPUT_RECOVERED ok=true\n");
            true
        }
        _ => false,
    }
}

#[no_mangle]
pub extern "C" fn _start(sup_ep_cap: u64, init_input_ep_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || sup_ep_cap == 0 || init_input_ep_cap == 0 {
        sys_exit(130);
    }

    // 1. Handshake with `dry-init` supervisor (`0x0102` = `dry-shell`) to obtain badged `disp_ep_cap`.
    let (disp_ep_cap, session_mode) = match sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0x0102, 0, 0, 0],
        0,
        0,
        0,
        0,
    ) {
        Ok((tag, cap, mrs)) if msg_tag_label(tag) == IPC_OP_PONG => (cap, mrs[0]),
        _ => sys_exit(131),
    };

    let desktop_wm_mode = session_mode >= 2 && disp_ep_cap != 0;
    let p3_mode = session_mode >= 10;
    let win_cols = if desktop_wm_mode {
        DESKTOP_WIN_COLS
    } else {
        TERM_COLS as u16
    };
    let win_rows = if desktop_wm_mode {
        DESKTOP_WIN_ROWS
    } else {
        TERM_ROWS as u16
    };

    // 2. Allocate & initialize atomic 4 KiB `SharedWindowSurface` (`USER_RW`).
    let surf_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(132),
    };
    if sys_vspace_map(surf_cap, SHELL_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(133);
    }
    // SAFETY: `SHELL_SURF_VADDR` is mapped `USER_RW` in `dry-shell`'s `VSpace`; `init_at` uses atomics only.
    let surf = unsafe { SharedWindowSurface::init_at(SHELL_SURF_VADDR, win_cols, win_rows, true) };

    // SAFETY: Single-threaded process owns `ENGINE`.
    let engine = unsafe { &mut *ENGINE.0.get() };
    engine.init_banner();

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    engine.project_to_surface(surf, win_cols as usize, win_rows as usize, Some(&sys));

    let mut win_id: u32 = 0;
    if disp_ep_cap != 0 {
        let attach_mrs = if desktop_wm_mode {
            [
                (win_cols as u64) | ((win_rows as u64) << 16),
                DESKTOP_WIN_X | (DESKTOP_WIN_Y << 32),
                (WindowRole::Normal as u64) | ((TITLE_KIND_TERMINAL as u64) << 8),
                0,
            ]
        } else {
            [0; 4]
        };
        if let Ok((rtag, _, rmrs)) = sys_ipc_call_with_grant(
            disp_ep_cap,
            IPC_OP_SURFACE_ATTACH,
            attach_mrs,
            surf_cap,
            RIGHT_READ | RIGHT_MAP,
            0,
            0,
        ) {
            if msg_tag_label(rtag) == IPC_OP_PONG {
                win_id = rmrs[0] as u32;
            }
        }
    }

    let mut input_ep_cap = init_input_ep_cap;
    let mut logged_single_row = false;
    let _ = sys_write_debug(b"[DRY-SHELL] Interactive shell ready in CPL3 Userspace\n");

    loop {
        let wait_res = if desktop_wm_mode {
            sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_WAIT_EVENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            )
        } else {
            sys_ipc_call_inline(
                input_ep_cap,
                pack_msg_tag(IPC_OP_INPUT_WAIT, 0, 1, 0),
                [0; 4],
                0,
            )
        };

        let (tag, _, mrs) = match wait_res {
            Ok(v) => v,
            Err(EPIPE | EREVOKED | EBADCAP) if !desktop_wm_mode => {
                if reconnect_input_service(
                    sup_ep_cap,
                    disp_ep_cap,
                    win_id,
                    &mut input_ep_cap,
                    engine,
                    surf,
                    win_cols as usize,
                    win_rows as usize,
                    &mut sys,
                ) {
                    continue;
                }
                break;
            }
            Err(_) => break,
        };

        let lbl = msg_tag_label(tag);
        if lbl == IPC_OP_WIN_CLOSED {
            let _ = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            );
            if p3_mode {
                let _ = sys_ipc_call_inline(
                    sup_ep_cap,
                    pack_msg_tag(IPC_OP_SUP_APP_EXITED, 0, 1, 0),
                    [0x0102, 0, 0, 0],
                    0,
                );
            } else {
                let _ = sys_ipc_call_inline(
                    sup_ep_cap,
                    pack_msg_tag(IPC_OP_SUP_SHUTDOWN, 0, 1, 0),
                    [engine.cmd_count as u64, 0, 0, 0],
                    0,
                );
            }
            break;
        }
        if lbl != IPC_OP_INPUT_EVENT {
            continue;
        }

        let wcols = win_cols as usize;
        let wrows = win_rows as usize;
        let (ev, _) = InputEvent::from_ipc_mrs(mrs);

        if ev.kind == InputEventKind::MouseScroll {
            if ev.wheel != 0 && engine.scroll_history_rows((ev.wheel as isize) * 3, wrows) {
                engine.project_to_surface(surf, wcols, wrows, None);
                let _ = present_surface(disp_ep_cap, win_id);
            }
            continue;
        }
        if ev.kind != InputEventKind::KeyPress {
            continue;
        }

        if (ev.modifiers & MOD_ALT) != 0 {
            match ev.keycode {
                KEY_LEFT => move_floating_window(disp_ep_cap, win_id, -32, 0),
                KEY_RIGHT => move_floating_window(disp_ep_cap, win_id, 32, 0),
                KEY_UP => move_floating_window(disp_ep_cap, win_id, 0, -24),
                KEY_DOWN => move_floating_window(disp_ep_cap, win_id, 0, 24),
                _ => {}
            }
            continue;
        }

        if (ev.modifiers & MOD_SHIFT) != 0 && (ev.keycode == KEY_UP || ev.keycode == KEY_DOWN) {
            let delta = if ev.keycode == KEY_UP { 3 } else { -3 };
            if engine.scroll_history_rows(delta, wrows) {
                engine.project_to_surface(surf, wcols, wrows, None);
                let _ = present_surface(disp_ep_cap, win_id);
            }
            continue;
        }

        if (ev.modifiers & MOD_CTRL) != 0 {
            let changed = match ev.ascii {
                b'l' | b'L' => {
                    engine.clear_history();
                    true
                }
                b'u' | b'U' => {
                    engine.editor.clear();
                    true
                }
                b'a' | b'A' => engine.editor.move_home(),
                b'e' | b'E' => engine.editor.move_end(),
                b'c' | b'C' => {
                    engine.push_line(b"^C", STYLE_MUTED);
                    engine.editor.clear();
                    true
                }
                _ => false,
            };
            if changed {
                engine.project_to_surface(surf, wcols, wrows, None);
                let _ = present_surface(disp_ep_cap, win_id);
            }
            continue;
        }

        match ev.keycode {
            KEY_PAGEUP => {
                if engine.scroll_history_rows(8, wrows) {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_PAGEDOWN => {
                if engine.scroll_history_rows(-8, wrows) {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_UP => {
                if engine.history_prev() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_DOWN => {
                if engine.history_next() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_BACKSPACE => {
                if engine.editor.backspace() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_DELETE => {
                if engine.editor.delete_at_cursor() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_LEFT => {
                if engine.editor.move_left() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_RIGHT => {
                if engine.editor.move_right() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_HOME => {
                if engine.editor.move_home() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_END => {
                if engine.editor.move_end() {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let _ = present_surface(disp_ep_cap, win_id);
                }
            }
            KEY_ENTER => {
                let mut line_copy = [0u8; MAX_INPUT_LEN];
                let line_len = engine.editor.as_bytes().len();
                line_copy[..line_len].copy_from_slice(engine.editor.as_bytes());
                let cmd_slice = &line_copy[..line_len];
                engine.editor.clear();
                engine.record_command_history(cmd_slice);
                engine.push_echoed_prompt(cmd_slice);
                engine.cmd_count = engine.cmd_count.wrapping_add(1);

                let mut wants_shutdown = false;
                let mut wants_self_exit = false;
                let mut trigger_crash_after_present = false;

                match parse_command(cmd_slice) {
                    ShellCommand::Empty => {}
                    ShellCommand::Help => {
                        engine.format_help();
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=help ok=true\n");
                    }
                    ShellCommand::Clear => {
                        engine.clear_history();
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=clear ok=true\n");
                    }
                    ShellCommand::History => {
                        engine.format_history();
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=history ok=true\n");
                    }
                    cmd @ (ShellCommand::System | ShellCommand::SysInfo) => {
                        let _ = sys_sysinfo(&mut sys);
                        engine.format_system(&sys);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(if cmd == ShellCommand::System {
                            b"[DRY-SHELL] CMD=system cpus="
                        } else {
                            b"[DRY-SHELL] CMD=sysinfo cpus="
                        });
                        lb.push_u64(sys.online_cpus as u64);
                        lb.push_bytes(b" syscalls=");
                        lb.push_u64(sys.total_syscalls);
                        lb.push_bytes(b" ext_irqs=");
                        lb.push_u64(sys.total_ext_irqs);
                        lb.push_bytes(b" free_4k=");
                        lb.push_u64(sys.free_frames_4k as u64);
                        debug_line(&lb);
                    }
                    ShellCommand::Uptime => {
                        let _ = sys_sysinfo(&mut sys);
                        engine.format_uptime(&sys);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-SHELL] CMD=uptime ms=");
                        lb.push_u64(sys.uptime_ms);
                        lb.push_bytes(b" tsc_mhz=");
                        lb.push_u64(sys.tsc_mhz);
                        debug_line(&lb);
                    }
                    ShellCommand::Ps => {
                        let mut procs = [UserProcInfo::default(); 32];
                        match sys_proc_list(&mut procs) {
                            Ok((written, total)) => {
                                engine.format_ps(&procs[..written], total);
                                let mut lb = LineBuilder::new();
                                lb.push_bytes(b"[DRY-SHELL] CMD=ps count=");
                                lb.push_u64(written as u64);
                                lb.push_bytes(b" total=");
                                lb.push_u64(total as u64);
                                debug_line(&lb);
                            }
                            Err(_) => {
                                engine.push_line(b"ps failed", STYLE_WARN);
                            }
                        }
                    }
                    ShellCommand::Apps => {
                        match sys_ipc_call_inline(
                            sup_ep_cap,
                            pack_msg_tag(IPC_OP_SUP_QUERY_APPS, 0, 1, 0),
                            [0; 4],
                            0,
                        ) {
                            Ok((rtag, _, rmrs)) if msg_tag_label(rtag) == IPC_OP_PONG => {
                                engine.format_apps(rmrs);
                                let mut running = 0u64;
                                for w in rmrs {
                                    if ((w >> 32) & 0xff) == 1 {
                                        running += 1;
                                    }
                                }
                                let mut lb = LineBuilder::new();
                                lb.push_bytes(b"[DRY-SHELL] CMD=apps running=");
                                lb.push_u64(running);
                                debug_line(&lb);
                            }
                            _ => {
                                engine.push_line(
                                    b"Application registry requires Milestone 2A-3 supervisor.",
                                    STYLE_WARN,
                                );
                            }
                        }
                    }
                    ShellCommand::OpenApp(arg) => match AppId::from_name(arg) {
                        Some(app) => match call_sup_action(sup_ep_cap, DesktopAction::LaunchOrFocusApp(app)) {
                            Ok(rmrs) => {
                                let mut lb = LineBuilder::new();
                                lb.push_bytes(if rmrs[1] != 0 {
                                    b"Launched app '"
                                } else {
                                    b"Focused running app '"
                                });
                                lb.push_bytes(app.slug());
                                lb.push_bytes(b"' (pid=");
                                lb.push_u64(rmrs[0]);
                                lb.push_byte(b')');
                                engine.push_line(lb.as_bytes(), STYLE_OK);

                                let mut slb = LineBuilder::new();
                                slb.push_bytes(b"[DRY-SHELL] CMD=open app=");
                                slb.push_bytes(app.slug());
                                slb.push_bytes(b" pid=");
                                slb.push_u64(rmrs[0]);
                                slb.push_bytes(b" spawned=");
                                slb.push_u64(rmrs[1]);
                                debug_line(&slb);
                            }
                            Err(IPC_OP_ERR_NOSYS) => {
                                engine.push_line(
                                    b"App module not loaded in current boot image (-ENOSYS).",
                                    STYLE_WARN,
                                );
                            }
                            Err(_) => {
                                engine.push_line(b"Failed to launch/focus app.", STYLE_WARN);
                            }
                        },
                        None => engine.push_line(
                            b"Usage: open <terminal|lifestation|files|settings>",
                            STYLE_WARN,
                        ),
                    },
                    ShellCommand::CloseApp(arg) => match AppId::from_name(arg) {
                        Some(app) => {
                            if app == AppId::Terminal {
                                wants_self_exit = true;
                            } else {
                                match call_sup_action(sup_ep_cap, DesktopAction::CloseApp(app)) {
                                    Ok(rmrs) => {
                                        let mut lb = LineBuilder::new();
                                        lb.push_bytes(b"Closed app '");
                                        lb.push_bytes(app.slug());
                                        lb.push_bytes(b"' (reaped_pid=");
                                        lb.push_u64(rmrs[0]);
                                        lb.push_byte(b')');
                                        engine.push_line(lb.as_bytes(), STYLE_OK);

                                        let mut slb = LineBuilder::new();
                                        slb.push_bytes(b"[DRY-SHELL] CMD=close app=");
                                        slb.push_bytes(app.slug());
                                        slb.push_bytes(b" pid=");
                                        slb.push_u64(rmrs[0]);
                                        debug_line(&slb);
                                    }
                                    Err(_) => {
                                        engine.push_line(b"Failed to close app.", STYLE_WARN);
                                    }
                                }
                            }
                        }
                        None => engine.push_line(
                            b"Usage: close <terminal|lifestation|files|settings>",
                            STYLE_WARN,
                        ),
                    },
                    ShellCommand::RestartApp(arg) => match AppId::from_name(arg) {
                        Some(app) => match call_sup_action(sup_ep_cap, DesktopAction::RestartApp(app)) {
                            Ok(rmrs) => {
                                let mut lb = LineBuilder::new();
                                lb.push_bytes(b"Restarted app '");
                                lb.push_bytes(app.slug());
                                lb.push_bytes(b"' -> pid=");
                                lb.push_u64(rmrs[0]);
                                lb.push_bytes(b" launches=");
                                lb.push_u64(rmrs[1]);
                                engine.push_line(lb.as_bytes(), STYLE_OK);

                                let mut slb = LineBuilder::new();
                                slb.push_bytes(b"[DRY-SHELL] CMD=restart app=");
                                slb.push_bytes(app.slug());
                                slb.push_bytes(b" pid=");
                                slb.push_u64(rmrs[0]);
                                debug_line(&slb);
                            }
                            Err(_) => engine.push_line(b"Failed to restart app.", STYLE_WARN),
                        },
                        None => engine.push_line(
                            b"Usage: restart <terminal|lifestation|files|settings>",
                            STYLE_WARN,
                        ),
                    },
                    ShellCommand::Tile => {
                        if disp_ep_cap != 0 {
                            if let Ok((_, _, rmrs)) = sys_ipc_call_inline(
                                disp_ep_cap,
                                pack_msg_tag(IPC_OP_WM_TOGGLE_TILING, 0, 1, 0),
                                [0; 4],
                                0,
                            ) {
                                engine.push_line(
                                    if rmrs[0] != 0 {
                                        b"Window layout -> TILED"
                                    } else {
                                        b"Window layout -> FLOATING"
                                    },
                                    STYLE_OK,
                                );
                                let mut slb = LineBuilder::new();
                                slb.push_bytes(b"[DRY-SHELL] CMD=tile mode=");
                                slb.push_u64(rmrs[0]);
                                debug_line(&slb);
                            }
                        }
                    }
                    ShellCommand::Blank => {
                        let _ = call_sup_action(sup_ep_cap, DesktopAction::BlankDisplayNow);
                        engine.push_line(b"Display blank requested (any input wakes).", STYLE_OK);
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=blank ok=true\n");
                    }
                    ShellCommand::Lock => {
                        let _ = call_sup_action(sup_ep_cap, DesktopAction::LockSession);
                        engine.push_line(
                            b"Session lock engaged (unauthenticated until DRY Vault auth).",
                            STYLE_OK,
                        );
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=lock ok=true\n");
                    }
                    ShellCommand::Reboot => {
                        engine.push_line(b"Requesting controlled system reboot...", STYLE_WARN);
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=reboot ok=true\n");
                        let _ = call_sup_action(sup_ep_cap, DesktopAction::RequestReboot);
                        break;
                    }
                    ShellCommand::Suspend => {
                        match call_sup_action(sup_ep_cap, DesktopAction::RequestSuspendS3) {
                            Err(IPC_OP_ERR_NOSYS) => {
                                engine.push_line(
                                    b"Suspend-to-RAM (S3) unsupported: -ENOSYS (UEFI wake vector planned).",
                                    STYLE_WARN,
                                );
                                let _ = sys_write_debug(b"[DRY-SHELL] CMD=suspend res=ENOSYS\n");
                            }
                            _ => engine.push_line(b"Suspend requested.", STYLE_OK),
                        }
                    }
                    ShellCommand::Echo(text) => {
                        engine.push_line(text, STYLE_DEFAULT);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-SHELL] CMD=echo text=\"");
                        lb.push_bytes(text);
                        lb.push_bytes(b"\"");
                        debug_line(&lb);
                    }
                    ShellCommand::OverflowTest => {
                        if let Ok((_, _, omrs)) = sys_ipc_call_inline(
                            input_ep_cap,
                            pack_msg_tag(IPC_OP_INPUT_OVERFLOW_TEST, 0, 1, 0),
                            [0; 4],
                            0,
                        ) {
                            let mut lb = LineBuilder::new();
                            lb.push_bytes(b"Input queue guard: retained=");
                            lb.push_u64(omrs[0]);
                            lb.push_bytes(b" dropped=");
                            lb.push_u64(omrs[1]);
                            lb.push_bytes(b" total_ovf=");
                            lb.push_u64(omrs[2]);
                            engine.push_line(lb.as_bytes(), STYLE_OK);

                            let mut slb = LineBuilder::new();
                            slb.push_bytes(b"[DRY-SHELL] CMD=overflow-test retained=");
                            slb.push_u64(omrs[0]);
                            slb.push_bytes(b" dropped=");
                            slb.push_u64(omrs[1]);
                            debug_line(&slb);
                        }
                    }
                    ShellCommand::CrashInput => {
                        engine.push_line(
                            b"Injecting #UD fault into dry-input service...",
                            STYLE_WARN,
                        );
                        trigger_crash_after_present = true;
                    }
                    ShellCommand::SecAudit => {
                        run_live_p3_sec_probes(sup_ep_cap, engine);
                    }
                    ShellCommand::Shutdown => {
                        engine.push_line(b"Shutting down DRY OS session...", STYLE_OK);
                        wants_shutdown = true;
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=shutdown ok=true\n");
                    }
                    ShellCommand::UnsupportedPosix(op_ch) => {
                        engine.format_unsupported_posix(op_ch);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-SHELL] CMD=unsupported_posix op=0x");
                        lb.push_hex_u64(op_ch as u64);
                        debug_line(&lb);
                    }
                    ShellCommand::Unknown(verb) => {
                        engine.format_unknown(verb);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-SHELL] CMD=unknown verb=\"");
                        lb.push_bytes(verb);
                        lb.push_bytes(b"\"");
                        debug_line(&lb);
                    }
                }

                let _ = sys_sysinfo(&mut sys);
                engine.project_to_surface(surf, wcols, wrows, Some(&sys));
                let (d_px, d_cyc) = present_surface(disp_ep_cap, win_id);
                if disp_ep_cap != 0 {
                    let mut plb = LineBuilder::new();
                    plb.push_bytes(b"[DRY-SHELL] PRESENT seq=");
                    plb.push_u64(surf.current_seq() as u64);
                    plb.push_bytes(b" dirty_px=");
                    plb.push_u64(d_px);
                    plb.push_bytes(b" blit_cyc=");
                    plb.push_u64(d_cyc);
                    debug_line(&plb);
                }

                if trigger_crash_after_present {
                    let _ = sys_ipc_send_inline(
                        input_ep_cap,
                        pack_msg_tag(IPC_OP_CRASH_UD2, 0, 1, 0),
                        [0; 4],
                        0,
                    );
                    if desktop_wm_mode
                        && !reconnect_input_service(
                            sup_ep_cap,
                            disp_ep_cap,
                            win_id,
                            &mut input_ep_cap,
                            engine,
                            surf,
                            wcols,
                            wrows,
                            &mut sys,
                        )
                    {
                        break;
                    }
                }

                if wants_self_exit {
                    if disp_ep_cap != 0 && win_id != 0 {
                        let _ = sys_ipc_call_inline(
                            disp_ep_cap,
                            pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
                            [win_id as u64, 0, 0, 0],
                            0,
                        );
                    }
                    let _ = sys_ipc_call_inline(
                        sup_ep_cap,
                        pack_msg_tag(IPC_OP_SUP_APP_EXITED, 0, 1, 0),
                        [0x0102, 0, 0, 0],
                        0,
                    );
                    break;
                }

                if wants_shutdown {
                    let _ = sys_ipc_call_inline(
                        sup_ep_cap,
                        pack_msg_tag(IPC_OP_SUP_SHUTDOWN, 0, 1, 0),
                        [engine.cmd_count as u64, 0, 0, 0],
                        0,
                    );
                    break;
                }
            }
            _ => {
                if ev.ascii != 0 && engine.editor.insert_ascii(ev.ascii) {
                    engine.project_to_surface(surf, wcols, wrows, None);
                    let (d_px, d_cyc) = present_surface(disp_ep_cap, win_id);
                    if !logged_single_row && disp_ep_cap != 0 {
                        logged_single_row = true;
                        let mut rlb = LineBuilder::new();
                        rlb.push_bytes(b"[DRY-SHELL] ROW_PRESENT seq=");
                        rlb.push_u64(surf.current_seq() as u64);
                        rlb.push_bytes(b" dirty_px=");
                        rlb.push_u64(d_px);
                        rlb.push_bytes(b" blit_cyc=");
                        rlb.push_u64(d_cyc);
                        debug_line(&rlb);
                    }
                }
            }
        }
    }

    let _ = sys_vspace_map(surf_cap, SHELL_SURF_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let _ = sys_cap_revoke(surf_cap, 2);
    sys_exit(0);
}
