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

use dry_userspace::action::{DesktopAction, APP_COUNT};
use dry_userspace::apps::shell_ui::{
    render_shell_pixel, viewport_rows, ShellBackend, SHELL_PIXEL_VADDR, SHELL_WIN_H, SHELL_WIN_W,
};
use dry_userspace::apps::PixelAppWindow;
use dry_userspace::input::{InputEvent, InputEventKind, MOD_ALT, MOD_CTRL, MOD_SHIFT};
use dry_userspace::keymap::{
    KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END, KEY_ENTER, KEY_HOME, KEY_LEFT, KEY_PAGEDOWN,
    KEY_PAGEUP, KEY_RIGHT, KEY_UP,
};
use dry_userspace::shell::{parse_command, LineBuilder, ShellCommand, ShellEngine, MAX_INPUT_LEN};
use dry_userspace::shell_fs_cmds::{call_sup_action, debug_line, try_exec_ext_cmd};
use dry_userspace::surface::{
    SharedWindowSurface, STYLE_DEFAULT, STYLE_MUTED, STYLE_OK, STYLE_WARN, TERM_COLS, TERM_ROWS,
};
use dry_userspace::wm::{WindowRole, TITLE_KIND_TERMINAL};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_send_inline, sys_power_query,
    sys_proc_list, sys_sysinfo, sys_vspace_map, sys_write_debug, UserPowerInfo, UserProcInfo,
    UserSysInfo, EBADCAP, EPIPE, EREVOKED, IPC_OP_CRASH_UD2, IPC_OP_INPUT_EVENT,
    IPC_OP_INPUT_WAIT, IPC_OP_PONG, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO,
    IPC_OP_SUP_QUERY_APPS, IPC_OP_SUP_RECONNECT_INPUT, IPC_OP_SUP_SHUTDOWN,
    IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_MOVE, IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_CLOSE,
    IPC_OP_WIN_CLOSED, IPC_OP_WIN_WAIT_EVENT, RIGHT_MAP, RIGHT_READ, VSPACE_MAP_OP_4K_RW,
    VSPACE_MAP_OP_UNMAP_2M, VSPACE_MAP_OP_UNMAP_4K,
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

fn vis_rows_backend(backend: &ShellBackend) -> usize {
    match backend {
        ShellBackend::Legacy4K { rows, .. } => *rows,
        ShellBackend::Pixel2M { win } => viewport_rows(win.theme(), win.h as usize),
    }
}

fn render_and_present_backend(
    backend: &mut ShellBackend,
    engine: &mut ShellEngine,
    disp_ep_cap: u64,
    win_id: u32,
    sys: Option<&UserSysInfo>,
) -> (u64, u64) {
    match backend {
        ShellBackend::Legacy4K { surf, cols, rows, .. } => {
            engine.project_to_surface(surf, *cols, *rows, sys);
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
        ShellBackend::Pixel2M { win } => {
            render_shell_pixel(win, engine, sys);
            let (_, d_px, d_cyc) = win.present_full(disp_ep_cap);
            (d_px, d_cyc)
        }
    }
}

fn reconnect_input_service(
    sup_ep_cap: u64,
    disp_ep_cap: u64,
    win_id: u32,
    input_ep_cap: &mut u64,
    engine: &mut ShellEngine,
    backend: &mut ShellBackend,
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
            let _ = render_and_present_backend(backend, engine, disp_ep_cap, win_id, Some(sys));
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

    // SAFETY: Single-threaded process owns `ENGINE`.
    let engine = unsafe { &mut *ENGINE.0.get() };
    engine.init_banner();

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);

    // 2. Allocate & initialize window surface (`Frame2M` pixel surface in Glass 2.0 mode, `Frame4K` legacy).
    let (mut backend, mut win_id) = if p3_mode && desktop_wm_mode {
        let flags = PixelAppWindow::query_theme_flags(sup_ep_cap);
        let mut win = PixelAppWindow::alloc_and_map(
            SHELL_PIXEL_VADDR,
            SHELL_WIN_W,
            SHELL_WIN_H,
            flags,
        )
        .unwrap_or_else(|_| sys_exit(132));
        render_shell_pixel(&mut win, engine, Some(&sys));
        win.publish_initial();
        let wid = win.attach(
            disp_ep_cap,
            0,
            DESKTOP_WIN_X,
            DESKTOP_WIN_Y,
            WindowRole::Normal,
            TITLE_KIND_TERMINAL,
            0,
        );
        (ShellBackend::Pixel2M { win }, wid)
    } else {
        let surf_cap = match sys_frame_alloc(0) {
            Ok(c) => c,
            Err(_) => sys_exit(132),
        };
        if sys_vspace_map(surf_cap, SHELL_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
            sys_exit(133);
        }
        // SAFETY: `SHELL_SURF_VADDR` is mapped `USER_RW` in `dry-shell`'s `VSpace`; `init_at` uses atomics only.
        let surf =
            unsafe { SharedWindowSurface::init_at(SHELL_SURF_VADDR, win_cols, win_rows, true) };
        engine.project_to_surface(surf, win_cols as usize, win_rows as usize, Some(&sys));

        let mut wid: u32 = 0;
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
                    wid = rmrs[0] as u32;
                }
            }
        }
        (
            ShellBackend::Legacy4K {
                vaddr: SHELL_SURF_VADDR,
                cap: surf_cap,
                surf,
                cols: win_cols as usize,
                rows: win_rows as usize,
            },
            wid,
        )
    };
    let _ = &mut win_id;

    let mut input_ep_cap = init_input_ep_cap;
    let mut cached_vfs_cap: u64 = 0;
    let mut cached_vault_cap: u64 = 0;
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
                    &mut backend,
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

        let (ev, _) = InputEvent::from_ipc_mrs(mrs);
        if ev.kind == InputEventKind::WindowConfig {
            if let ShellBackend::Pixel2M { ref mut win } = backend {
                if win.handle_window_config(ev) {
                    let _ = sys_sysinfo(&mut sys);
                    let _ = render_and_present_backend(
                        &mut backend,
                        engine,
                        disp_ep_cap,
                        win_id,
                        Some(&sys),
                    );
                }
            }
            continue;
        }

        let vrows = vis_rows_backend(&backend);

        if ev.kind == InputEventKind::MouseScroll {
            if ev.wheel != 0 && engine.scroll_history_rows((ev.wheel as isize) * 3, vrows) {
                let _ = render_and_present_backend(&mut backend, engine, disp_ep_cap, win_id, None);
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
            if engine.scroll_history_rows(delta, vrows) {
                let _ = render_and_present_backend(&mut backend, engine, disp_ep_cap, win_id, None);
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
                let _ = render_and_present_backend(&mut backend, engine, disp_ep_cap, win_id, None);
            }
            continue;
        }

        let redraw = match ev.keycode {
            KEY_PAGEUP => engine.scroll_history_rows(8, vrows),
            KEY_PAGEDOWN => engine.scroll_history_rows(-8, vrows),
            KEY_UP => engine.history_prev(),
            KEY_DOWN => engine.history_next(),
            KEY_BACKSPACE => engine.editor.backspace(),
            KEY_DELETE => engine.editor.delete_at_cursor(),
            KEY_LEFT => engine.editor.move_left(),
            KEY_RIGHT => engine.editor.move_right(),
            KEY_HOME => engine.editor.move_home(),
            KEY_END => engine.editor.move_end(),
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
                let mut post_present_action: Option<DesktopAction> = None;
                let mut exec_wants_break = false;
                let pipe_parsed = dry_userspace::shell_pipe::parse_pipe_or_redir(cmd_slice);
                let mark_pushed = if pipe_parsed.is_some() {
                    engine.mark_output().0
                } else {
                    0
                };

                {
                    let mut exec_one = |one_cmd: &[u8]| {
                if dry_userspace::shell_diag_cmds::try_exec_dry_cmd(
                    one_cmd,
                    sup_ep_cap,
                    engine,
                    debug_line,
                ) {
                    // Handled by shared `hwdiag` diagnostic command dispatcher.
                } else if dry_userspace::shell_gpu_cmds::try_exec_gpu_cmd(
                    one_cmd,
                    sup_ep_cap,
                    engine,
                    debug_line,
                ) {
                    // Handled by Mandatory NVIDIA / GPU CLI bridge.
                } else {
                match parse_command(one_cmd) {
                    ShellCommand::Empty => {}
                    ShellCommand::Clip(rest) => {
                        let mut s = 0usize;
                        while s < rest.len() && rest[s] == b' ' {
                            s += 1;
                        }
                        let trimmed = &rest[s..];
                        if let Some(text) = trimmed.strip_prefix(b"set ") {
                            match dry_userspace::wm_ipc::wm_clipboard_set(disp_ep_cap, text) {
                                Ok(n) => {
                                    let mut lb = LineBuilder::new();
                                    lb.push_bytes(b"Clipboard set (");
                                    lb.push_u64(n as u64);
                                    lb.push_bytes(b" bytes)");
                                    engine.push_line(lb.as_bytes(), dry_userspace::surface::STYLE_OK);
                                    let mut slb = LineBuilder::new();
                                    slb.push_bytes(b"[DRY-SHELL] CMD=clip op=set len=");
                                    slb.push_u64(n as u64);
                                    slb.push_bytes(b" text=\"");
                                    for &b in text.iter().take(48) {
                                        slb.push_byte(if (0x20..=0x7e).contains(&b) { b } else { b'.' });
                                    }
                                    slb.push_byte(b'"');
                                    debug_line(&slb);
                                }
                                Err(e) => {
                                    let mut lb = LineBuilder::new();
                                    lb.push_bytes(b"clip set error: ");
                                    lb.push_i64(e as i64);
                                    engine.push_line(lb.as_bytes(), dry_userspace::surface::STYLE_WARN);
                                }
                            }
                        } else if trimmed == b"get" || trimmed.is_empty() {
                            let mut cbuf = [0u8; dry_userspace::MAX_IPC_EXTRA_BYTES];
                            match dry_userspace::wm_ipc::wm_clipboard_get(disp_ep_cap, &mut cbuf) {
                                Ok(n) => {
                                    engine.push_line(&cbuf[..n], dry_userspace::surface::STYLE_OK);
                                    let mut slb = LineBuilder::new();
                                    slb.push_bytes(b"[DRY-SHELL] CMD=clip op=get len=");
                                    slb.push_u64(n as u64);
                                    slb.push_bytes(b" text=\"");
                                    for &b in cbuf[..n].iter().take(48) {
                                        slb.push_byte(if (0x20..=0x7e).contains(&b) { b } else { b'.' });
                                    }
                                    slb.push_byte(b'"');
                                    debug_line(&slb);
                                }
                                Err(e) => {
                                    let mut lb = LineBuilder::new();
                                    lb.push_bytes(b"clip get error: ");
                                    lb.push_i64(e as i64);
                                    engine.push_line(lb.as_bytes(), dry_userspace::surface::STYLE_WARN);
                                }
                            }
                        } else {
                            engine.push_line(
                                b"Usage: clip set <text> | clip get",
                                dry_userspace::surface::STYLE_WARN,
                            );
                        }
                    }
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
                        let mut pwr = UserPowerInfo::default();
                        if p3_mode && sys_power_query(&mut pwr).is_ok() {
                            engine.format_power_telemetry(&pwr);
                        }
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
                        if p3_mode {
                            lb.push_bytes(b" hlt=");
                            lb.push_u64(pwr.idle_hlt_calls);
                        }
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
                        let p0 = sys_ipc_call_inline(
                            sup_ep_cap,
                            pack_msg_tag(IPC_OP_SUP_QUERY_APPS, 0, 1, 0),
                            [0; 4],
                            0,
                        );
                        let p1 = sys_ipc_call_inline(
                            sup_ep_cap,
                            pack_msg_tag(IPC_OP_SUP_QUERY_APPS, 0, 1, 0),
                            [1, 0, 0, 0],
                            0,
                        );
                        match (p0, p1) {
                            (Ok((t0, _, m0)), Ok((t1, _, m1)))
                                if msg_tag_label(t0) == IPC_OP_PONG
                                    && msg_tag_label(t1) == IPC_OP_PONG =>
                            {
                                let all: [u64; APP_COUNT] =
                                    [m0[0], m0[1], m0[2], m0[3], m1[0], m1[1]];
                                engine.format_apps(all);
                                let mut running = 0u64;
                                for w in all {
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
                    ShellCommand::Echo(text) => {
                        engine.push_line(text, STYLE_DEFAULT);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-SHELL] CMD=echo text=\"");
                        lb.push_bytes(text);
                        lb.push_bytes(b"\"");
                        debug_line(&lb);
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
                    other => {
                        if let Some(out) = try_exec_ext_cmd(
                            &other,
                            sup_ep_cap,
                            disp_ep_cap,
                            input_ep_cap,
                            session_mode,
                            &mut cached_vfs_cap,
                            &mut cached_vault_cap,
                            engine,
                        ) {
                            post_present_action = out.post_present_action;
                            if out.wants_self_exit {
                                wants_self_exit = true;
                            }
                            if out.trigger_crash_after_present {
                                trigger_crash_after_present = true;
                            }
                            if out.wants_break {
                                exec_wants_break = true;
                            }
                        }
                    }
                }
                }
                };
                if let Some(ref parsed) = pipe_parsed {
                    exec_one(parsed.lhs());
                } else {
                    exec_one(cmd_slice);
                }
                }
                if exec_wants_break {
                    break;
                }
                if let Some(parsed) = pipe_parsed {
                    let mut cap_lines = [[0u8; dry_userspace::surface::TERM_COLS]; 32];
                    let mut cap_lens = [0u8; 32];
                    let n = engine.take_lines_since(mark_pushed, &mut cap_lines, &mut cap_lens);
                    dry_userspace::shell_pipe::finish_pipe_or_redir(
                        &parsed,
                        sup_ep_cap,
                        &mut cached_vfs_cap,
                        &cap_lines[..n],
                        &cap_lens[..n],
                        engine,
                    );
                }

                let _ = sys_sysinfo(&mut sys);
                let (d_px, d_cyc) = render_and_present_backend(
                    &mut backend,
                    engine,
                    disp_ep_cap,
                    win_id,
                    Some(&sys),
                );
                if disp_ep_cap != 0 {
                    let mut plb = LineBuilder::new();
                    plb.push_bytes(b"[DRY-SHELL] PRESENT seq=");
                    plb.push_u64(backend.current_seq() as u64);
                    plb.push_bytes(b" dirty_px=");
                    plb.push_u64(d_px);
                    plb.push_bytes(b" blit_cyc=");
                    plb.push_u64(d_cyc);
                    debug_line(&plb);
                }
                if let Some(act) = post_present_action {
                    let _ = call_sup_action(sup_ep_cap, act);
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
                            &mut backend,
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
                false
            }
            _ => {
                if ev.ascii != 0 && engine.editor.insert_ascii(ev.ascii) {
                    let (d_px, d_cyc) =
                        render_and_present_backend(&mut backend, engine, disp_ep_cap, win_id, None);
                    if !logged_single_row && disp_ep_cap != 0 {
                        logged_single_row = true;
                        let mut rlb = LineBuilder::new();
                        rlb.push_bytes(b"[DRY-SHELL] ROW_PRESENT seq=");
                        rlb.push_u64(backend.current_seq() as u64);
                        rlb.push_bytes(b" dirty_px=");
                        rlb.push_u64(d_px);
                        rlb.push_bytes(b" blit_cyc=");
                        rlb.push_u64(d_cyc);
                        debug_line(&rlb);
                    }
                }
                false
            }
        };
        if redraw {
            let _ = render_and_present_backend(&mut backend, engine, disp_ep_cap, win_id, None);
        }
    }

    if cached_vfs_cap != 0 {
        let _ = sys_cap_revoke(cached_vfs_cap, 1);
    }
    if cached_vault_cap != 0 {
        let _ = sys_cap_revoke(cached_vault_cap, 1);
    }
    match backend {
        ShellBackend::Legacy4K { vaddr, cap, .. } => {
            let _ = sys_vspace_map(cap, vaddr, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(cap, 2);
        }
        ShellBackend::Pixel2M { win } => {
            let _ = sys_vspace_map(win.surf_cap, win.vaddr, VSPACE_MAP_OP_UNMAP_2M);
            let _ = sys_cap_revoke(win.surf_cap, 2);
        }
    }
    sys_exit(0);
}
