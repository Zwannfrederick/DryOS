// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-shell` — Unprivileged Ring 3 (`CPL=3`, `SecDomain::Userspace`) Interactive Shell.
//!
//! Holds ZERO hardware capabilities (`0` `IrqCap`, `0` `IoPortCap`, `0` `Framebuffer` cap):
//! receives normalized `InputEvent`s from `dry-input` over Capability IPC, executes commands
//! backed by live `SYS_SYSINFO` kernel telemetry, writes character cells into its private 4 KiB
//! `SharedTerminalSurface`, and requests dirty-rectangle presentation from `dry-display`.

#![no_std]
#![no_main]

use dry_userspace::input::{InputEvent, InputEventKind, MOD_ALT, MOD_CTRL, MOD_SHIFT};
use dry_userspace::keymap::{
    KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END, KEY_ENTER, KEY_HOME, KEY_LEFT, KEY_RIGHT, KEY_UP,
};
use dry_userspace::shell::{parse_command, LineBuilder, ShellCommand, ShellEngine, MAX_INPUT_LEN};
use dry_userspace::surface::{
    SharedTerminalSurface, STYLE_DEFAULT, STYLE_OK, STYLE_WARN,
};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_send_inline, sys_sysinfo, sys_vspace_map,
    sys_write_debug, UserSysInfo, EBADCAP, EPIPE, EREVOKED, IPC_OP_CRASH_UD2, IPC_OP_INPUT_EVENT,
    IPC_OP_INPUT_OVERFLOW_TEST, IPC_OP_INPUT_WAIT, IPC_OP_PONG, IPC_OP_SUP_HELLO,
    IPC_OP_SUP_RECONNECT_INPUT, IPC_OP_SUP_SHUTDOWN, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_MOVE,
    IPC_OP_SURFACE_PRESENT, RIGHT_MAP, RIGHT_READ, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

use core::cell::UnsafeCell;

const SHELL_SURF_VADDR: u64 = 0x1800_0000;

struct SingleThreadCell(UnsafeCell<ShellEngine>);
// SAFETY: `dry-shell` executes strictly on a single CPL=3 thread.
unsafe impl Sync for SingleThreadCell {}

static ENGINE: SingleThreadCell = SingleThreadCell(UnsafeCell::new(ShellEngine::new()));

fn debug_line(lb: &LineBuilder) {
    let mut out = [0u8; 96];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn present_surface(disp_ep_cap: u64) -> (u64, u64) {
    if disp_ep_cap == 0 {
        return (0, 0);
    }
    match sys_ipc_call_inline(
        disp_ep_cap,
        pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
        [0; 4],
        0,
    ) {
        Ok((_, _, mrs)) => (mrs[0], mrs[1]),
        Err(_) => (0, 0),
    }
}

fn move_floating_window(disp_ep_cap: u64, dx: i64, dy: i64) {
    if disp_ep_cap == 0 {
        return;
    }
    if let Ok((_, _, mrs)) = sys_ipc_call_inline(
        disp_ep_cap,
        pack_msg_tag(IPC_OP_SURFACE_MOVE, 0, 2, 0),
        [dx as u64, dy as u64, 0, 0],
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

#[no_mangle]
pub extern "C" fn _start(sup_ep_cap: u64, init_input_ep_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || sup_ep_cap == 0 || init_input_ep_cap == 0 {
        sys_exit(130);
    }

    // 1. Handshake with `dry-init` supervisor to receive optional `disp_ep_cap` (`0` if headless).
    let disp_ep_cap = match sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0; 4],
        0,
        0,
        0,
        0,
    ) {
        Ok((tag, cap, _)) if msg_tag_label(tag) == IPC_OP_PONG => cap,
        _ => sys_exit(131),
    };

    // 2. Allocate & map 4 KiB `SharedTerminalSurface` (`USER_RW`), then grant `READ|MAP` to `dry-display`.
    let surf_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(132),
    };
    if sys_vspace_map(surf_cap, SHELL_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(133);
    }
    // SAFETY: `SHELL_SURF_VADDR` is exclusively mapped `USER_RW` in single-threaded `dry-shell`.
    let surf = unsafe { &mut *(SHELL_SURF_VADDR as *mut SharedTerminalSurface) };
    surf.init();

    // SAFETY: Single-threaded process owns `ENGINE`.
    let engine = unsafe { &mut *ENGINE.0.get() };
    engine.init_banner();

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    engine.project_to_surface(surf, Some(&sys));

    if disp_ep_cap != 0 {
        let _ = sys_ipc_call_with_grant(
            disp_ep_cap,
            IPC_OP_SURFACE_ATTACH,
            [0; 4],
            surf_cap,
            RIGHT_READ | RIGHT_MAP,
            0,
            0,
        );
    }

    let mut input_ep_cap = init_input_ep_cap;
    let mut logged_single_row = false;
    let _ = sys_write_debug(b"[DRY-SHELL] Interactive shell ready in CPL3 Userspace\n");

    loop {
        let wait_res = sys_ipc_call_inline(
            input_ep_cap,
            pack_msg_tag(IPC_OP_INPUT_WAIT, 0, 1, 0),
            [0; 4],
            0,
        );

        let (tag, _, mrs) = match wait_res {
            Ok(v) => v,
            Err(EPIPE | EREVOKED | EBADCAP) => {
                // `dry-input` crashed or was revoked; request supervised restart & fresh capability!
                match sys_ipc_call_with_grant(
                    sup_ep_cap,
                    IPC_OP_SUP_RECONNECT_INPUT,
                    [0; 4],
                    0,
                    0,
                    0,
                    0,
                ) {
                    Ok((rtag, new_ep, rmrs)) if msg_tag_label(rtag) == IPC_OP_PONG && new_ep != 0 => {
                        let _ = sys_cap_revoke(input_ep_cap, 1);
                        input_ep_cap = new_ep;
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[supervisor] dry-input restarted (#");
                        lb.push_u64(rmrs[0]);
                        lb.push_bytes(b"), stale cap revoked, IRQ1 re-bound");
                        engine.push_line(lb.as_bytes(), STYLE_OK);
                        let _ = sys_sysinfo(&mut sys);
                        engine.project_to_surface(surf, Some(&sys));
                        let _ = present_surface(disp_ep_cap);
                        let _ = sys_write_debug(b"[DRY-SHELL] INPUT_RECOVERED ok=true\n");
                        continue;
                    }
                    _ => break,
                }
            }
            Err(_) => break,
        };

        if msg_tag_label(tag) != IPC_OP_INPUT_EVENT {
            continue;
        }
        let (ev, _) = InputEvent::from_ipc_mrs(mrs);
        if ev.kind != InputEventKind::KeyPress {
            continue;
        }

        // Desktop foundation shortcut: `Alt + Arrow` moves the floating terminal window.
        if (ev.modifiers & MOD_ALT) != 0 {
            match ev.keycode {
                KEY_LEFT => move_floating_window(disp_ep_cap, -32, 0),
                KEY_RIGHT => move_floating_window(disp_ep_cap, 32, 0),
                KEY_UP => move_floating_window(disp_ep_cap, 0, -24),
                KEY_DOWN => move_floating_window(disp_ep_cap, 0, 24),
                _ => {}
            }
            continue;
        }

        // Scrollback navigation: `Shift + Up/Down`.
        if (ev.modifiers & MOD_SHIFT) != 0 && (ev.keycode == KEY_UP || ev.keycode == KEY_DOWN) {
            let delta = if ev.keycode == KEY_UP { 3 } else { -3 };
            if engine.scroll_history(delta) {
                engine.project_to_surface(surf, None);
                let _ = present_surface(disp_ep_cap);
            }
            continue;
        }

        if (ev.modifiers & MOD_CTRL) != 0 {
            if ev.ascii == b'l' || ev.ascii == b'L' {
                engine.clear_history();
                engine.project_to_surface(surf, None);
                let _ = present_surface(disp_ep_cap);
            } else if ev.ascii == b'u' || ev.ascii == b'U' {
                engine.editor.clear();
                engine.project_to_surface(surf, None);
                let _ = present_surface(disp_ep_cap);
            }
            continue;
        }

        match ev.keycode {
            KEY_BACKSPACE => {
                if engine.editor.backspace() {
                    engine.project_to_surface(surf, None);
                    let _ = present_surface(disp_ep_cap);
                }
            }
            KEY_DELETE => {
                if engine.editor.delete_at_cursor() {
                    engine.project_to_surface(surf, None);
                    let _ = present_surface(disp_ep_cap);
                }
            }
            KEY_LEFT => {
                if engine.editor.move_left() {
                    engine.project_to_surface(surf, None);
                    let _ = present_surface(disp_ep_cap);
                }
            }
            KEY_RIGHT => {
                if engine.editor.move_right() {
                    engine.project_to_surface(surf, None);
                    let _ = present_surface(disp_ep_cap);
                }
            }
            KEY_HOME => {
                if engine.editor.move_home() {
                    engine.project_to_surface(surf, None);
                    let _ = present_surface(disp_ep_cap);
                }
            }
            KEY_END => {
                if engine.editor.move_end() {
                    engine.project_to_surface(surf, None);
                    let _ = present_surface(disp_ep_cap);
                }
            }
            KEY_ENTER => {
                let mut line_copy = [0u8; MAX_INPUT_LEN];
                let line_len = engine.editor.as_bytes().len();
                line_copy[..line_len].copy_from_slice(engine.editor.as_bytes());
                let cmd_slice = &line_copy[..line_len];
                engine.editor.clear();
                engine.push_echoed_prompt(cmd_slice);
                engine.cmd_count = engine.cmd_count.wrapping_add(1);

                let mut wants_shutdown = false;
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
                    ShellCommand::System => {
                        let _ = sys_sysinfo(&mut sys);
                        engine.format_system(&sys);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-SHELL] CMD=system cpus=");
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
                    ShellCommand::Shutdown => {
                        engine.push_line(b"Shutting down DRY OS interactive session...", STYLE_OK);
                        wants_shutdown = true;
                        let _ = sys_write_debug(b"[DRY-SHELL] CMD=shutdown ok=true\n");
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
                engine.project_to_surface(surf, Some(&sys));
                let (d_px, d_cyc) = present_surface(disp_ep_cap);
                if disp_ep_cap != 0 {
                    let mut plb = LineBuilder::new();
                    plb.push_bytes(b"[DRY-SHELL] PRESENT seq=");
                    plb.push_u64(surf.seq);
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
                    engine.project_to_surface(surf, None);
                    let (d_px, d_cyc) = present_surface(disp_ep_cap);
                    if !logged_single_row && disp_ep_cap != 0 {
                        logged_single_row = true;
                        let mut rlb = LineBuilder::new();
                        rlb.push_bytes(b"[DRY-SHELL] ROW_PRESENT seq=");
                        rlb.push_u64(surf.seq);
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
