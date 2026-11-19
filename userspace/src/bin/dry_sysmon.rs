// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-sysmon` (`Life Station`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0103`)
//! Live DRY Glass 2.0 System & Compartment Monitor (`Milestone 2A-2` Window B & DRY Glass 2.0).

#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering;

use dry_userspace::action::{DesktopAction, APP_COUNT};
use dry_userspace::apps::sysmon_ui::{
    render_sysmon_pixel, SysmonView, SYSMON_PIXEL_VADDR, SYSMON_WIN_H, SYSMON_WIN_W,
};
use dry_userspace::apps::{app_log_line, PixelAppWindow};
use dry_userspace::gfx::BOX_HORIZ;
use dry_userspace::input::{InputEvent, InputEventKind};
use dry_userspace::keymap::KEY_ESC;
use dry_userspace::shell::LineBuilder;
use dry_userspace::surface::{
    SharedWindowSurface, SurfaceCell, STYLE_ACCENT, STYLE_DEFAULT, STYLE_HEADER, STYLE_MUTED,
    STYLE_OK, STYLE_WARN,
};
use dry_userspace::wm_types::{WindowRole, TITLE_KIND_SYSMON};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait,
    sys_power_query, sys_proc_list, sys_sysinfo, sys_vault_query_epoch, sys_vspace_map,
    sys_write_debug, UserPowerInfo, UserProcInfo, UserSysInfo, IPC_OP_INPUT_EVENT, IPC_OP_PONG,
    IPC_OP_SUP_ACTION, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO, IPC_OP_SUP_QUERY_APPS,
    IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_BIND_NOTIF, IPC_OP_WIN_CLOSE,
    IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT, RIGHT_MAP, RIGHT_READ, RIGHT_SIGNAL,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_2M, VSPACE_MAP_OP_UNMAP_4K,
};

const SYSMON_SURF_VADDR: u64 = 0x1800_0000;
const LEGACY_COLS: usize = 54;
const LEGACY_ROWS: usize = 20;
const LEGACY_INIT_X: u64 = 636;
const LEGACY_INIT_Y: u64 = 52;

const LIFE_INIT_X: u64 = 620;
const LIFE_INIT_Y: u64 = 52;
const SPARK_CAP: usize = 32;
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

enum SysmonBackend {
    Legacy4K {
        cap: u64,
        surf: &'static SharedWindowSurface,
    },
    Pixel2M {
        win: PixelAppWindow,
    },
}

fn query_running_apps(sup_ep_cap: u64) -> u64 {
    let mut running = 0u64;
    for page in 0u64..=1 {
        if let Ok((rtag, _, rmrs)) = sys_ipc_call_inline(
            sup_ep_cap,
            pack_msg_tag(IPC_OP_SUP_QUERY_APPS, 0, 1, 0),
            [page, 0, 0, 0],
            0,
        ) {
            if msg_tag_label(rtag) == IPC_OP_PONG {
                let lim = if page == 1 { APP_COUNT.saturating_sub(4) } else { 4 };
                for &w in rmrs.iter().take(lim) {
                    if ((w >> 32) & 0xff) == 1 {
                        running += 1;
                    }
                }
            }
        }
    }
    running
}

fn query_diag_summary(sup_ep_cap: u64) -> dry_userspace::hwdiag::DiagSummaryWire {
    let mut rx = [0u8; dry_userspace::MAX_IPC_EXTRA_BYTES];
    let mut out = dry_userspace::hwdiag::DiagSummaryWire::empty();
    if let Ok((tag, elen, _)) = dry_userspace::vfs_proto::sys_ipc_call_extra(
        sup_ep_cap,
        dry_userspace::hwdiag::IPC_OP_SUP_DIAG_QUERY,
        [0; 4],
        &[],
        &mut rx,
        0,
    ) {
        if msg_tag_label(tag) == IPC_OP_PONG
            && elen >= core::mem::size_of::<dry_userspace::hwdiag::DiagSummaryWire>()
        {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    rx.as_ptr(),
                    (&raw mut out).cast::<u8>(),
                    core::mem::size_of::<dry_userspace::hwdiag::DiagSummaryWire>(),
                );
            }
        }
    }
    out
}

fn write_row_cols(surf: &SharedWindowSurface, cols: usize, row: usize, text: &[u8], style: u8) {
    let mut cells = [SurfaceCell::BLANK; LEGACY_COLS];
    for (i, &ch) in text.iter().take(cols).enumerate() {
        cells[i] = SurfaceCell { ch, style };
    }
    surf.store_row_slice(cols, row, &cells[..cols]);
}

fn render_legacy_telemetry_frame(
    surf: &SharedWindowSurface,
    sys: &UserSysInfo,
    refresh_seq: u64,
    keys_rcvd: u64,
    last_ascii: u8,
) {
    let odd = surf.begin_write();
    let div = [BOX_HORIZ; LEGACY_COLS];

    write_row_cols(surf, LEGACY_COLS, 0, b"DRY SYSMON // KERNEL TELEMETRY", STYLE_HEADER);
    write_row_cols(surf, LEGACY_COLS, 1, &div, STYLE_MUTED);

    let mut l2 = LineBuilder::new();
    l2.push_bytes(b"Status : ACTIVE  Window B (CPL=3)  Refresh #");
    l2.push_u64(refresh_seq);
    write_row_cols(surf, LEGACY_COLS, 2, l2.as_bytes(), STYLE_OK);

    let apic_str: &[u8] = match sys.apic_mode {
        1 => b"x2APIC",
        2 => b"xAPIC",
        _ => b"Disabled",
    };
    let mut l3 = LineBuilder::new();
    l3.push_bytes(b"APIC   : ");
    l3.push_bytes(apic_str);
    l3.push_bytes(b"    Online vCPUs : ");
    l3.push_u64(sys.online_cpus as u64);
    write_row_cols(surf, LEGACY_COLS, 3, l3.as_bytes(), STYLE_DEFAULT);

    let mut l4 = LineBuilder::new();
    l4.push_bytes(b"TSC    : ");
    l4.push_u64(sys.tsc_mhz);
    l4.push_bytes(b" MHz  Uptime : ");
    l4.push_u64(sys.uptime_ms / 1000);
    l4.push_byte(b'.');
    l4.push_u64_pad3(sys.uptime_ms % 1000);
    l4.push_bytes(b" s");
    write_row_cols(surf, LEGACY_COLS, 4, l4.as_bytes(), STYLE_DEFAULT);
    write_row_cols(surf, LEGACY_COLS, 5, b"", STYLE_DEFAULT);

    write_row_cols(surf, LEGACY_COLS, 6, b"PHYSICAL MEMORY MANAGER (4 KiB FRAMES)", STYLE_ACCENT);
    let usable = sys.usable_frames_4k.max(1) as u64;
    let free = (sys.free_frames_4k as u64).min(usable);
    let used = usable.saturating_sub(free);
    let used_pct = (used * 100) / usable;

    let mut l7 = LineBuilder::new();
    l7.push_bytes(b"  Usable : ");
    l7.push_u64(usable);
    l7.push_bytes(b" frames (");
    l7.push_u64((usable * 4) / 1024);
    l7.push_bytes(b" MiB)");
    write_row_cols(surf, LEGACY_COLS, 7, l7.as_bytes(), STYLE_DEFAULT);

    let mut l8 = LineBuilder::new();
    l8.push_bytes(b"  Free   : ");
    l8.push_u64(free);
    l8.push_bytes(b" frames (");
    l8.push_u64((free * 4) / 1024);
    l8.push_bytes(b" MiB)");
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
    write_row_cols(surf, LEGACY_COLS, 11, b"MICROKERNEL & HARDWARE COUNTERS", STYLE_ACCENT);

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
    surf.dirty_row_max.store((LEGACY_ROWS - 1) as u16, Ordering::Relaxed);
    surf.uptime_sec_snap.store((sys.uptime_ms / 1000).min(u32::MAX as u64) as u32, Ordering::Relaxed);
    surf.ext_irqs_snap.store(sys.total_ext_irqs.min(u32::MAX as u64) as u32, Ordering::Relaxed);
    surf.end_write(odd);
}

#[allow(clippy::too_many_arguments)]
fn push_spark_sample(ring: &mut [u16; SPARK_CAP], len: &mut usize, d_sys: u64) -> u16 {
    let val = d_sys.min(u16::MAX as u64) as u16;
    if *len < SPARK_CAP {
        ring[*len] = val;
        *len += 1;
    } else {
        for i in 1..SPARK_CAP {
            ring[i - 1] = ring[i];
        }
        ring[SPARK_CAP - 1] = val;
    }
    let mut mx = 1u16;
    for &s in &ring[..*len] {
        if s > mx {
            mx = s;
        }
    }
    mx
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
    let no_focus_flag = if launches <= 1 { 1u64 << 16 } else { 0 };

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let mut prev_sys;
    let mut pwr = UserPowerInfo::default();
    if p3_mode {
        let _ = sys_power_query(&mut pwr);
    }
    let mut procs = [UserProcInfo::default(); 24];
    let mut proc_count = if p3_mode {
        sys_proc_list(&mut procs).unwrap_or((0, 0)).0
    } else {
        0
    };
    let mut refresh_seq: u64 = 1;
    let mut keys_rcvd: u64 = 0;
    let mut last_ascii: u8 = 0;
    let mut spark_ring = [0u16; SPARK_CAP];
    let mut spark_len = 0usize;

    let (mut backend, win_id) = if p3_mode {
        let init_flags = PixelAppWindow::query_theme_flags(sup_ep_cap);
        let mut win = PixelAppWindow::alloc_and_map(
            SYSMON_PIXEL_VADDR,
            SYSMON_WIN_W,
            SYSMON_WIN_H,
            init_flags,
        )
        .unwrap_or_else(|_| sys_exit(163));
        let running_apps = query_running_apps(sup_ep_cap);
        let spark_max = push_spark_sample(&mut spark_ring, &mut spark_len, 0);
        let view = SysmonView {
            sys: &sys,
            d_sys: 0,
            d_ipc: 0,
            pwr: &pwr,
            procs: &procs[..proc_count.min(procs.len())],
            running_apps,
            refresh_seq,
            lease_epoch: sys_vault_query_epoch().unwrap_or(1) as u64,
            keys_rcvd,
            last_ascii,
            spark_samples: &spark_ring[..spark_len],
            spark_max,
            diag: query_diag_summary(sup_ep_cap),
        };
        render_sysmon_pixel(&mut win, &view);
        win.publish_initial();
        let wid = win.attach(
            disp_ep_cap,
            notif_cap,
            LIFE_INIT_X,
            LIFE_INIT_Y,
            WindowRole::Normal,
            TITLE_KIND_SYSMON,
            no_focus_flag,
        );
        (SysmonBackend::Pixel2M { win }, wid)
    } else {
        let surf_cap = match sys_frame_alloc(0) {
            Ok(c) => c,
            Err(_) => sys_exit(163),
        };
        if sys_vspace_map(surf_cap, SYSMON_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
            sys_exit(164);
        }
        // SAFETY: `SYSMON_SURF_VADDR` is mapped `USER_RW` in `dry-sysmon`'s isolated `VSpace`.
        let surf = unsafe {
            SharedWindowSurface::init_at(SYSMON_SURF_VADDR, LEGACY_COLS as u16, LEGACY_ROWS as u16, false)
        };
        render_legacy_telemetry_frame(surf, &sys, refresh_seq, keys_rcvd, last_ascii);
        let mut wid: u32 = 0;
        if disp_ep_cap != 0 {
            let attach_mrs = [
                (LEGACY_COLS as u64) | ((LEGACY_ROWS as u64) << 16),
                LEGACY_INIT_X | (LEGACY_INIT_Y << 32),
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
                    wid = rmrs[0] as u32;
                }
            }
            if wid != 0 {
                let _ = sys_ipc_call_with_grant(
                    disp_ep_cap,
                    IPC_OP_WIN_BIND_NOTIF,
                    [wid as u64, 0, 0, 0],
                    notif_cap,
                    RIGHT_READ | RIGHT_SIGNAL,
                    0,
                    0,
                );
            }
        }
        (SysmonBackend::Legacy4K { cap: surf_cap, surf }, wid)
    };

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-SYSMON] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    if p3_mode {
        rlb.push_bytes(b" mode=life_station procs=");
        rlb.push_u64(proc_count as u64);
    }
    app_log_line(&rlb);

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
                    if let SysmonBackend::Pixel2M { ref mut win } = backend {
                        if win.handle_window_config(ev) {
                            continue;
                        }
                    }
                    if ev.kind == InputEventKind::KeyPress {
                        keys_rcvd = keys_rcvd.wrapping_add(1);
                        last_ascii = ev.ascii;
                        force_log = true;

                        let mut klb = LineBuilder::new();
                        klb.push_bytes(b"[DRY-SYSMON] KEY ascii=0x");
                        klb.push_hex_u64(ev.ascii as u64);
                        klb.push_bytes(b" count=");
                        klb.push_u64(keys_rcvd);
                        app_log_line(&klb);

                        if ev.keycode == KEY_ESC || ev.ascii == b'q' || ev.ascii == b'Q' {
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
        match backend {
            SysmonBackend::Pixel2M { ref mut win } => {
                let _ = win.sync_prefs(sup_ep_cap);
                let _ = sys_power_query(&mut pwr);
                proc_count = sys_proc_list(&mut procs).unwrap_or((0, 0)).0;
                let running_apps = query_running_apps(sup_ep_cap);
                let d_sys = sys.total_syscalls.saturating_sub(prev_sys.total_syscalls);
                let d_ipc = sys.total_ipc_msgs.saturating_sub(prev_sys.total_ipc_msgs);
                let spark_max = push_spark_sample(&mut spark_ring, &mut spark_len, d_sys);
                let view = SysmonView {
                    sys: &sys,
                    d_sys,
                    d_ipc,
                    pwr: &pwr,
                    procs: &procs[..proc_count.min(procs.len())],
                    running_apps,
                    refresh_seq,
                    lease_epoch: sys_vault_query_epoch().unwrap_or(1) as u64,
                    keys_rcvd,
                    last_ascii,
                    spark_samples: &spark_ring[..spark_len],
                    spark_max,
                    diag: query_diag_summary(sup_ep_cap),
                };
                render_sysmon_pixel(win, &view);
                if disp_ep_cap != 0 && win_id != 0 {
                    let _ = win.present_full(disp_ep_cap);
                }
            }
            SysmonBackend::Legacy4K { surf, .. } => {
                render_legacy_telemetry_frame(surf, &sys, refresh_seq, keys_rcvd, last_ascii);
                if disp_ep_cap != 0 && win_id != 0 {
                    let _ = sys_ipc_call_inline(
                        disp_ep_cap,
                        pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
                        [win_id as u64, 0, 0, 0],
                        0,
                    );
                }
            }
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
            app_log_line(&slb);
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

    match backend {
        SysmonBackend::Legacy4K { cap, .. } => {
            let _ = sys_vspace_map(cap, SYSMON_SURF_VADDR, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(cap, 2);
        }
        SysmonBackend::Pixel2M { win } => {
            let _ = sys_vspace_map(win.surf_cap, win.vaddr, VSPACE_MAP_OP_UNMAP_2M);
            let _ = sys_cap_revoke(win.surf_cap, 2);
        }
    }
    let _ = sys_cap_revoke(notif_cap, 2);
    sys_exit(0);
}
