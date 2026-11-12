// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-sysmon` (`Life Station`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0103`)
//! Live System & Compartment Monitor (`Milestone 2A-2` Window B & `Milestone 2A-3` Life Station).
//!
//! Architectural & Security Properties:
//! - Runs in a separate `Userspace` compartment (`0x0103`) with its own `VSpace` and `CSpace`.
//! - Holds ZERO hardware capabilities (`0` `IrqCap`, `0` `IoPortCap`, `0` `Framebuffer` cap).
//! - Renders live `SYS_SYSINFO` + `SYS_PROC_LIST` kernel telemetry into an atomic
//!   `SharedWindowSurface` and sleeps via `sys_notify_wait` (`~250 ms` H1 timer deadline + instant
//!   wakeup on input/close; `0` busy-polling, `0` fabricated/unmeasured metrics).

#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering;

use dry_userspace::action::{compartment_short_name, DesktopAction};
use dry_userspace::gfx::BOX_HORIZ;
use dry_userspace::input::{InputEvent, InputEventKind};
use dry_userspace::shell::LineBuilder;
use dry_userspace::surface::{
    SharedWindowSurface, SurfaceCell, STYLE_ACCENT, STYLE_DEFAULT, STYLE_HEADER, STYLE_MUTED,
    STYLE_OK, STYLE_WARN,
};
use dry_userspace::wm::{WindowRole, TITLE_KIND_SYSMON};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait,
    sys_proc_list, sys_sysinfo, sys_vspace_map, sys_write_debug, UserProcInfo, UserSysInfo,
    IPC_OP_INPUT_EVENT, IPC_OP_PONG, IPC_OP_SUP_ACTION, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO,
    IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_BIND_NOTIF, IPC_OP_WIN_CLOSE,
    IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT, RIGHT_MAP, RIGHT_READ, RIGHT_SIGNAL,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const SYSMON_SURF_VADDR: u64 = 0x1800_0000;
const LEGACY_COLS: usize = 54;
const LEGACY_ROWS: usize = 20;
const LEGACY_INIT_X: u64 = 636;
const LEGACY_INIT_Y: u64 = 52;

const LIFE_COLS: usize = 56;
const LIFE_ROWS: usize = 22;
const LIFE_INIT_X: u64 = 620;
const LIFE_INIT_Y: u64 = 52;
const MAX_COLS: usize = 56;

const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 120];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn write_row_cols(surf: &SharedWindowSurface, cols: usize, row: usize, text: &[u8], style: u8) {
    let mut cells = [SurfaceCell::BLANK; MAX_COLS];
    for (i, &ch) in text.iter().take(cols).enumerate() {
        cells[i] = SurfaceCell { ch, style };
    }
    surf.store_row_slice(cols, row, &cells[..cols]);
}

fn push_padded(lb: &mut LineBuilder, s: &[u8], width: usize) {
    let take = s.len().min(width);
    lb.push_bytes(&s[..take]);
    for _ in take..width {
        lb.push_byte(b' ');
    }
}

fn push_u64_rjust(lb: &mut LineBuilder, val: u64, width: usize) {
    let mut digits = [0u8; 20];
    let mut v = val;
    let mut len = 0usize;
    if v == 0 {
        digits[0] = b'0';
        len = 1;
    } else {
        while v > 0 && len < digits.len() {
            digits[len] = b'0' + (v % 10) as u8;
            v /= 10;
            len += 1;
        }
    }
    for _ in len..width {
        lb.push_byte(b' ');
    }
    while len > 0 {
        len -= 1;
        lb.push_byte(digits[len]);
    }
}

fn render_legacy_telemetry_frame(
    surf: &SharedWindowSurface,
    sys: &UserSysInfo,
    refresh_seq: u64,
    keys_rcvd: u64,
    last_ascii: u8,
) {
    let odd = surf.begin_write();

    write_row_cols(
        surf,
        LEGACY_COLS,
        0,
        b"DRY SYSTEM MONITOR // CPL=3 COMPARTMENT 0x0103",
        STYLE_HEADER,
    );
    let div = [BOX_HORIZ; LEGACY_COLS];
    write_row_cols(surf, LEGACY_COLS, 1, &div, STYLE_MUTED);
    write_row_cols(surf, LEGACY_COLS, 2, b"CPU & INTERRUPT FABRIC", STYLE_ACCENT);

    let apic_str: &[u8] = match sys.apic_mode {
        1 => b"x2APIC-MSR",
        2 => b"xAPIC-MMIO",
        _ => b"Disabled",
    };
    let mut l3 = LineBuilder::new();
    l3.push_bytes(b"  Online vCPUs : ");
    l3.push_u64(sys.online_cpus as u64);
    l3.push_bytes(b"       APIC : ");
    l3.push_bytes(apic_str);
    write_row_cols(surf, LEGACY_COLS, 3, l3.as_bytes(), STYLE_OK);

    let mut l4 = LineBuilder::new();
    l4.push_bytes(b"  IOAPIC Route : ");
    l4.push_bytes(if sys.ioapic_present != 0 {
        b"ACTIVE"
    } else {
        b"OFF   "
    });
    l4.push_bytes(b"   TSC  : ");
    l4.push_u64(sys.tsc_mhz);
    l4.push_bytes(b" MHz");
    write_row_cols(surf, LEGACY_COLS, 4, l4.as_bytes(), STYLE_DEFAULT);

    let mut l5 = LineBuilder::new();
    l5.push_bytes(b"  Uptime       : ");
    l5.push_u64(sys.uptime_ms / 1000);
    l5.push_byte(b'.');
    l5.push_u64_pad3(sys.uptime_ms % 1000);
    l5.push_bytes(b" s   Frame: #");
    l5.push_u64(refresh_seq);
    write_row_cols(surf, LEGACY_COLS, 5, l5.as_bytes(), STYLE_OK);

    write_row_cols(surf, LEGACY_COLS, 6, b"", STYLE_DEFAULT);
    write_row_cols(
        surf,
        LEGACY_COLS,
        7,
        b"PHYSICAL MEMORY (4K BUDDY PMM)",
        STYLE_ACCENT,
    );

    let usable = sys.usable_frames_4k.max(1) as u64;
    let free = (sys.free_frames_4k as u64).min(usable);
    let used = usable.saturating_sub(free);
    let used_pct = (used * 100) / usable;
    let free_mib = (free * 4) / 1024;
    let usable_mib = (usable * 4) / 1024;

    let mut l8 = LineBuilder::new();
    l8.push_bytes(b"  Free / Usable: ");
    l8.push_u64(free_mib);
    l8.push_bytes(b" / ");
    l8.push_u64(usable_mib);
    l8.push_bytes(b" MiB (");
    l8.push_u64(free);
    l8.push_bytes(b" 4K)");
    write_row_cols(surf, LEGACY_COLS, 8, l8.as_bytes(), STYLE_DEFAULT);

    let filled = ((used * 20) / usable).min(20) as usize;
    let mut l9 = LineBuilder::new();
    l9.push_bytes(b"  Used : [");
    for i in 0..20 {
        l9.push_byte(if i < filled { b'#' } else { b'.' });
    }
    l9.push_bytes(b"] ");
    l9.push_u64(used_pct);
    l9.push_byte(b'%');
    write_row_cols(surf, LEGACY_COLS, 9, l9.as_bytes(), STYLE_OK);

    write_row_cols(surf, LEGACY_COLS, 10, b"", STYLE_DEFAULT);
    write_row_cols(
        surf,
        LEGACY_COLS,
        11,
        b"MICROKERNEL & HARDWARE COUNTERS",
        STYLE_ACCENT,
    );

    let mut l12 = LineBuilder::new();
    l12.push_bytes(b"  Syscalls     : ");
    l12.push_u64(sys.total_syscalls);
    write_row_cols(surf, LEGACY_COLS, 12, l12.as_bytes(), STYLE_DEFAULT);

    let mut l13 = LineBuilder::new();
    l13.push_bytes(b"  IPC Messages : ");
    l13.push_u64(sys.total_ipc_msgs);
    write_row_cols(surf, LEGACY_COLS, 13, l13.as_bytes(), STYLE_OK);

    let mut l14 = LineBuilder::new();
    l14.push_bytes(b"  Ctx Switches : ");
    l14.push_u64(sys.total_ctx_switches);
    write_row_cols(surf, LEGACY_COLS, 14, l14.as_bytes(), STYLE_DEFAULT);

    let mut l15 = LineBuilder::new();
    l15.push_bytes(b"  LAPIC Timers : ");
    l15.push_u64(sys.timer_irqs);
    write_row_cols(surf, LEGACY_COLS, 15, l15.as_bytes(), STYLE_DEFAULT);

    let mut l16 = LineBuilder::new();
    l16.push_bytes(b"  IRQ1 (KBD)   : ");
    l16.push_u64(sys.irq1_delivered);
    l16.push_bytes(b"   IRQ12 (MOUSE): ");
    l16.push_u64(sys.irq12_delivered);
    write_row_cols(surf, LEGACY_COLS, 16, l16.as_bytes(), STYLE_ACCENT);

    write_row_cols(surf, LEGACY_COLS, 17, &div, STYLE_MUTED);

    let mut l18 = LineBuilder::new();
    l18.push_bytes(b"  Focused Keys : ");
    l18.push_u64(keys_rcvd);
    l18.push_bytes(b"   Last Key: ");
    if (0x21..=0x7E).contains(&last_ascii) {
        l18.push_byte(b'\'');
        l18.push_byte(last_ascii);
        l18.push_byte(b'\'');
    } else {
        l18.push_bytes(b"none");
    }
    write_row_cols(
        surf,
        LEGACY_COLS,
        18,
        l18.as_bytes(),
        if keys_rcvd > 0 { STYLE_WARN } else { STYLE_MUTED },
    );

    write_row_cols(
        surf,
        LEGACY_COLS,
        19,
        b"[R] Refresh  [Q] Close Window  [X] Fault-Isolation",
        STYLE_MUTED,
    );

    surf.dirty_row_min.store(0, Ordering::Relaxed);
    surf.dirty_row_max
        .store((LEGACY_ROWS - 1) as u16, Ordering::Relaxed);
    surf.uptime_sec_snap.store(
        (sys.uptime_ms / 1000).min(u32::MAX as u64) as u32,
        Ordering::Relaxed,
    );
    surf.ext_irqs_snap.store(
        sys.total_ext_irqs.min(u32::MAX as u64) as u32,
        Ordering::Relaxed,
    );
    surf.end_write(odd);
}

#[allow(clippy::too_many_arguments)]
fn render_life_station_frame(
    surf: &SharedWindowSurface,
    sys: &UserSysInfo,
    prev_sys: &UserSysInfo,
    procs: &[UserProcInfo],
    refresh_seq: u64,
    keys_rcvd: u64,
    last_ascii: u8,
) {
    let odd = surf.begin_write();
    let div = [BOX_HORIZ; LIFE_COLS];

    write_row_cols(
        surf,
        LIFE_COLS,
        0,
        b"DRY LIFE STATION // LIVE KERNEL & COMPARTMENT TELEMETRY",
        STYLE_HEADER,
    );
    write_row_cols(surf, LIFE_COLS, 1, &div, STYLE_MUTED);

    let apic_str: &[u8] = match sys.apic_mode {
        1 => b"x2APIC",
        2 => b"xAPIC ",
        _ => b"Off   ",
    };
    let mut r2 = LineBuilder::new();
    r2.push_bytes(b"vCPUs: ");
    r2.push_u64(sys.online_cpus as u64);
    r2.push_bytes(b"  APIC: ");
    r2.push_bytes(apic_str);
    r2.push_bytes(b"  TSC: ");
    r2.push_u64(sys.tsc_mhz);
    r2.push_bytes(b"MHz  Up: ");
    r2.push_u64(sys.uptime_ms / 1000);
    r2.push_byte(b'.');
    r2.push_u64_pad3(sys.uptime_ms % 1000);
    r2.push_byte(b's');
    write_row_cols(surf, LIFE_COLS, 2, r2.as_bytes(), STYLE_OK);

    let usable = sys.usable_frames_4k.max(1) as u64;
    let free = (sys.free_frames_4k as u64).min(usable);
    let used = usable.saturating_sub(free);
    let used_pct = (used * 100) / usable;
    let used_mib = (used * 4) / 1024;
    let usable_mib = (usable * 4) / 1024;
    let filled = ((used * 16) / usable).min(16) as usize;

    let mut r3 = LineBuilder::new();
    r3.push_bytes(b"PMM  : [");
    for i in 0..16 {
        r3.push_byte(if i < filled { b'#' } else { b'.' });
    }
    r3.push_bytes(b"] ");
    r3.push_u64(used_pct);
    r3.push_bytes(b"%  ");
    r3.push_u64(used_mib);
    r3.push_byte(b'/');
    r3.push_u64(usable_mib);
    r3.push_bytes(b" MiB (");
    r3.push_u64(free);
    r3.push_bytes(b" free)");
    write_row_cols(surf, LIFE_COLS, 3, r3.as_bytes(), STYLE_DEFAULT);

    let d_sys = sys.total_syscalls.saturating_sub(prev_sys.total_syscalls);
    let d_ipc = sys.total_ipc_msgs.saturating_sub(prev_sys.total_ipc_msgs);
    let mut r4 = LineBuilder::new();
    r4.push_bytes(b"Sys  : ");
    r4.push_u64(sys.total_syscalls);
    r4.push_bytes(b" (+");
    r4.push_u64(d_sys);
    r4.push_bytes(b")  IPC: ");
    r4.push_u64(sys.total_ipc_msgs);
    r4.push_bytes(b" (+");
    r4.push_u64(d_ipc);
    r4.push_bytes(b")");
    write_row_cols(surf, LIFE_COLS, 4, r4.as_bytes(), STYLE_OK);

    let mut r5 = LineBuilder::new();
    r5.push_bytes(b"CtxSw: ");
    r5.push_u64(sys.total_ctx_switches);
    r5.push_bytes(b"  IRQ1: ");
    r5.push_u64(sys.irq1_delivered);
    r5.push_bytes(b"  IRQ12: ");
    r5.push_u64(sys.irq12_delivered);
    r5.push_bytes(b"  Tmr: ");
    r5.push_u64(sys.timer_irqs);
    write_row_cols(surf, LIFE_COLS, 5, r5.as_bytes(), STYLE_ACCENT);

    write_row_cols(surf, LIFE_COLS, 6, &div, STYLE_MUTED);

    let mut r7 = LineBuilder::new();
    r7.push_bytes(b"ACTIVE COMPARTMENTS (");
    r7.push_u64(procs.len() as u64);
    r7.push_bytes(b" LIVE) // SYS_PROC_LIST");
    write_row_cols(surf, LIFE_COLS, 7, r7.as_bytes(), STYLE_ACCENT);

    write_row_cols(
        surf,
        LIFE_COLS,
        8,
        b" PID DOM  COMPARTMENT   ST    4K_PG     SYSCALLS",
        STYLE_MUTED,
    );

    for slot in 0..10usize {
        let row = 9 + slot;
        if slot < procs.len() {
            let p = &procs[slot];
            let dom = if p.domain == 1 { b"ROOT" } else { b"USER" };
            let (st_lbl, st_style): (&[u8], u8) = match p.state {
                2 => (b"RUN ", if p.domain == 1 { STYLE_ACCENT } else { STYLE_OK }),
                3 => (b"EXIT", STYLE_WARN),
                4 => (b"FLT!", STYLE_WARN),
                5 => (b"KILL", STYLE_WARN),
                _ => (b"INIT", STYLE_MUTED),
            };
            let cname = compartment_short_name(p.compartment_id, p.domain);
            let mut pl = LineBuilder::new();
            push_u64_rjust(&mut pl, p.pid as u64, 4);
            pl.push_byte(b' ');
            pl.push_bytes(dom);
            pl.push_byte(b' ');
            push_padded(&mut pl, cname, 13);
            pl.push_byte(b' ');
            pl.push_bytes(st_lbl);
            pl.push_byte(b' ');
            push_u64_rjust(&mut pl, (p.mapped_pages_4k + p.pt_frames_4k) as u64, 6);
            pl.push_byte(b' ');
            push_u64_rjust(&mut pl, p.syscall_entries, 12);
            write_row_cols(surf, LIFE_COLS, row, pl.as_bytes(), st_style);
        } else {
            write_row_cols(surf, LIFE_COLS, row, b"", STYLE_DEFAULT);
        }
    }

    write_row_cols(surf, LIFE_COLS, 19, &div, STYLE_MUTED);

    let mut r20 = LineBuilder::new();
    r20.push_bytes(b"Seq #");
    r20.push_u64(refresh_seq);
    r20.push_bytes(b"  Keys: ");
    r20.push_u64(keys_rcvd);
    r20.push_bytes(b"  Last: ");
    if (0x21..=0x7E).contains(&last_ascii) {
        r20.push_byte(b'\'');
        r20.push_byte(last_ascii);
        r20.push_byte(b'\'');
    } else {
        r20.push_bytes(b"none");
    }
    write_row_cols(
        surf,
        LIFE_COLS,
        20,
        r20.as_bytes(),
        if keys_rcvd > 0 { STYLE_WARN } else { STYLE_MUTED },
    );

    write_row_cols(
        surf,
        LIFE_COLS,
        21,
        b"[R] Refresh  [T] Tile  [Q] Close  [X] #UD Fault Test",
        STYLE_MUTED,
    );

    surf.dirty_row_min.store(0, Ordering::Relaxed);
    surf.dirty_row_max
        .store((LIFE_ROWS - 1) as u16, Ordering::Relaxed);
    surf.uptime_sec_snap.store(
        (sys.uptime_ms / 1000).min(u32::MAX as u64) as u32,
        Ordering::Relaxed,
    );
    surf.ext_irqs_snap.store(
        sys.total_ext_irqs.min(u32::MAX as u64) as u32,
        Ordering::Relaxed,
    );
    surf.end_write(odd);
}

#[no_mangle]
pub extern "C" fn _start(sup_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || sup_ep_cap == 0 {
        sys_exit(160);
    }

    let notif_cap = match sys_notify_create(0) {
        Ok(c) => c,
        Err(_) => sys_exit(161),
    };

    // Handshake with `dry-init` (`0x0103` = `Life Station`), granting `SIGNAL` on `notif_cap`.
    let (disp_ep_cap, session_mode, launches) = match sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0x0103, 0, 0, 0],
        notif_cap,
        RIGHT_READ | RIGHT_SIGNAL,
        0,
        0,
    ) {
        Ok((tag, cap, rmrs)) if msg_tag_label(tag) == IPC_OP_PONG => (cap, rmrs[0], rmrs[1]),
        _ => sys_exit(162),
    };

    let p3_mode = session_mode >= 10;
    let cols = if p3_mode { LIFE_COLS } else { LEGACY_COLS };
    let rows = if p3_mode { LIFE_ROWS } else { LEGACY_ROWS };
    let init_x = if p3_mode { LIFE_INIT_X } else { LEGACY_INIT_X };
    let init_y = if p3_mode { LIFE_INIT_Y } else { LEGACY_INIT_Y };

    let surf_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(163),
    };
    if sys_vspace_map(surf_cap, SYSMON_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(164);
    }
    // SAFETY: `SYSMON_SURF_VADDR` is mapped `USER_RW` in `dry-sysmon`'s isolated `VSpace`.
    let surf = unsafe {
        SharedWindowSurface::init_at(SYSMON_SURF_VADDR, cols as u16, rows as u16, false)
    };

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let mut prev_sys = sys;
    let mut procs = [UserProcInfo::default(); 24];
    let mut proc_count = if p3_mode {
        sys_proc_list(&mut procs).unwrap_or((0, 0)).0
    } else {
        0
    };
    let mut refresh_seq: u64 = 1;
    let mut keys_rcvd: u64 = 0;
    let mut last_ascii: u8 = 0;

    if p3_mode {
        render_life_station_frame(
            surf,
            &sys,
            &prev_sys,
            &procs[..proc_count.min(procs.len())],
            refresh_seq,
            keys_rcvd,
            last_ascii,
        );
    } else {
        render_legacy_telemetry_frame(surf, &sys, refresh_seq, keys_rcvd, last_ascii);
    }

    let mut win_id: u32 = 0;
    if disp_ep_cap != 0 {
        // Set `NO_FOCUS_ON_OPEN` (`1 << 16`) only on initial boot (`launches <= 1`) so Terminal keeps initial focus.
        let no_focus_flag = if launches <= 1 { 1u64 << 16 } else { 0 };
        let attach_mrs = [
            (cols as u64) | ((rows as u64) << 16),
            init_x | (init_y << 32),
            (WindowRole::Normal as u64) | ((TITLE_KIND_SYSMON as u64) << 8) | no_focus_flag,
            0,
        ];
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
        if win_id != 0 {
            let _ = sys_ipc_call_with_grant(
                disp_ep_cap,
                IPC_OP_WIN_BIND_NOTIF,
                [win_id as u64, 0, 0, 0],
                notif_cap,
                RIGHT_READ | RIGHT_SIGNAL,
                0,
                0,
            );
        }
    }

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-SYSMON] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    if p3_mode {
        rlb.push_bytes(b" mode=life_station procs=");
        rlb.push_u64(proc_count as u64);
    }
    log_line(&rlb);

    let refresh_tsc = 250_000u64.saturating_mul(sys.tsc_mhz.max(1000));
    let mut should_exit_clean = false;

    loop {
        let mut force_log = refresh_seq <= 2;

        if disp_ep_cap != 0 && win_id != 0 {
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    should_exit_clean = true;
                    break;
                }
                if plbl == IPC_OP_INPUT_EVENT {
                    let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                    if ev.kind == InputEventKind::KeyPress {
                        keys_rcvd = keys_rcvd.wrapping_add(1);
                        last_ascii = ev.ascii;
                        force_log = true;

                        let mut klb = LineBuilder::new();
                        klb.push_bytes(b"[DRY-SYSMON] KEY ascii=0x");
                        klb.push_hex_u64(ev.ascii as u64);
                        klb.push_bytes(b" count=");
                        klb.push_u64(keys_rcvd);
                        log_line(&klb);

                        if ev.ascii == b'q' || ev.ascii == b'Q' {
                            should_exit_clean = true;
                            break;
                        }
                        if p3_mode && (ev.ascii == b't' || ev.ascii == b'T') {
                            let [w0, w1] = DesktopAction::ToggleTiling.to_words();
                            let _ = sys_ipc_call_inline(
                                sup_ep_cap,
                                pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
                                [w0, w1, 0, 0],
                                0,
                            );
                        }
                        if ev.ascii == b'x' || ev.ascii == b'X' {
                            let _ = sys_write_debug(
                                b"[DRY-SYSMON] Injecting CPL3 #UD crash in Window B\n",
                            );
                            // SAFETY: Intentional CPL=3 `#UD` to verify per-window crash isolation.
                            unsafe {
                                asm!("ud2", options(nomem, nostack));
                            }
                        }
                    }
                } else {
                    break;
                }
            }
        }

        if should_exit_clean {
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
                [0x0103, 0, 0, 0],
                0,
            );
            let _ = sys_write_debug(b"[DRY-SYSMON] Cleanly closed Window B\n");
            break;
        }

        prev_sys = sys;
        let _ = sys_sysinfo(&mut sys);
        if p3_mode {
            proc_count = sys_proc_list(&mut procs).unwrap_or((0, 0)).0;
            render_life_station_frame(
                surf,
                &sys,
                &prev_sys,
                &procs[..proc_count.min(procs.len())],
                refresh_seq,
                keys_rcvd,
                last_ascii,
            );
        } else {
            render_legacy_telemetry_frame(surf, &sys, refresh_seq, keys_rcvd, last_ascii);
        }
        if disp_ep_cap != 0 && win_id != 0 {
            let _ = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            );
        }

        if force_log {
            let mut slb = LineBuilder::new();
            slb.push_bytes(b"[DRY-SYSMON] REFRESH seq=");
            slb.push_u64(refresh_seq);
            slb.push_bytes(b" up_ms=");
            slb.push_u64(sys.uptime_ms);
            slb.push_bytes(b" syscalls=");
            slb.push_u64(sys.total_syscalls);
            slb.push_bytes(b" ipc=");
            slb.push_u64(sys.total_ipc_msgs);
            slb.push_bytes(b" irq1=");
            slb.push_u64(sys.irq1_delivered);
            slb.push_bytes(b" irq12=");
            slb.push_u64(sys.irq12_delivered);
            if p3_mode {
                slb.push_bytes(b" procs=");
                slb.push_u64(proc_count as u64);
            }
            log_line(&slb);
        }
        refresh_seq = refresh_seq.wrapping_add(1);

        if let Ok(bits) = sys_notify_wait(notif_cap, 0, refresh_tsc) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                if disp_ep_cap != 0 && win_id != 0 {
                    let _ = sys_ipc_call_inline(
                        disp_ep_cap,
                        pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
                        [win_id as u64, 0, 0, 0],
                        0,
                    );
                }
                break;
            }
        }
    }

    let _ = sys_vspace_map(surf_cap, SYSMON_SURF_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let _ = sys_cap_revoke(surf_cap, 2);
    let _ = sys_cap_revoke(notif_cap, 2);
    sys_exit(0);
}
