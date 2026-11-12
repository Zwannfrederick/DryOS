// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-sysmon` — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0103`) Live System Monitor
//! (`Milestone 2A-2` Window B).
//!
//! Architectural & Security Properties:
//! - Runs in a separate `Userspace` compartment (`0x0103`) with its own `VSpace` and `CSpace`.
//! - Holds ZERO hardware capabilities (`0` `IrqCap`, `0` `IoPortCap`, `0` `Framebuffer` cap).
//! - Renders live `SYS_SYSINFO` telemetry into an atomic `SharedWindowSurface` (`54x20` at `(636, 52)`)
//!   and sleeps via `sys_notify_wait` (`~250 ms` H1 timer deadline + instant wakeup on input/close;
//!   `0` busy-polling!).

#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering;

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
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait, sys_sysinfo,
    sys_vspace_map, sys_write_debug, UserSysInfo, IPC_OP_INPUT_EVENT, IPC_OP_PONG,
    IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_PRESENT,
    IPC_OP_WIN_BIND_NOTIF, IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT, RIGHT_MAP,
    RIGHT_READ, RIGHT_SIGNAL, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const SYSMON_SURF_VADDR: u64 = 0x1800_0000;
const MON_COLS: usize = 54;
const MON_ROWS: usize = 20;
const MON_INIT_X: u64 = 636;
const MON_INIT_Y: u64 = 52;
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 120];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn write_row(surf: &SharedWindowSurface, row: usize, text: &[u8], style: u8) {
    let mut cells = [SurfaceCell::BLANK; MON_COLS];
    for (i, &ch) in text.iter().take(MON_COLS).enumerate() {
        cells[i] = SurfaceCell { ch, style };
    }
    surf.store_row_slice(MON_COLS, row, &cells);
}

fn render_telemetry_frame(
    surf: &SharedWindowSurface,
    sys: &UserSysInfo,
    refresh_seq: u64,
    keys_rcvd: u64,
    last_ascii: u8,
) {
    let odd = surf.begin_write();

    write_row(
        surf,
        0,
        b"DRY SYSTEM MONITOR // CPL=3 COMPARTMENT 0x0103",
        STYLE_HEADER,
    );
    let div = [BOX_HORIZ; MON_COLS];
    write_row(surf, 1, &div, STYLE_MUTED);
    write_row(surf, 2, b"CPU & INTERRUPT FABRIC", STYLE_ACCENT);

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
    write_row(surf, 3, l3.as_bytes(), STYLE_OK);

    let mut l4 = LineBuilder::new();
    l4.push_bytes(b"  IOAPIC Route : ");
    l4.push_bytes(if sys.ioapic_present != 0 { b"ACTIVE" } else { b"OFF   " });
    l4.push_bytes(b"   TSC  : ");
    l4.push_u64(sys.tsc_mhz);
    l4.push_bytes(b" MHz");
    write_row(surf, 4, l4.as_bytes(), STYLE_DEFAULT);

    let mut l5 = LineBuilder::new();
    l5.push_bytes(b"  Uptime       : ");
    l5.push_u64(sys.uptime_ms / 1000);
    l5.push_byte(b'.');
    l5.push_u64_pad3(sys.uptime_ms % 1000);
    l5.push_bytes(b" s   Frame: #");
    l5.push_u64(refresh_seq);
    write_row(surf, 5, l5.as_bytes(), STYLE_OK);

    write_row(surf, 6, b"", STYLE_DEFAULT);
    write_row(surf, 7, b"PHYSICAL MEMORY (4K BUDDY PMM)", STYLE_ACCENT);

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
    write_row(surf, 8, l8.as_bytes(), STYLE_DEFAULT);

    let filled = ((used * 20) / usable).min(20) as usize;
    let mut l9 = LineBuilder::new();
    l9.push_bytes(b"  Used : [");
    for i in 0..20 {
        l9.push_byte(if i < filled { b'#' } else { b'.' });
    }
    l9.push_bytes(b"] ");
    l9.push_u64(used_pct);
    l9.push_byte(b'%');
    write_row(surf, 9, l9.as_bytes(), STYLE_OK);

    write_row(surf, 10, b"", STYLE_DEFAULT);
    write_row(surf, 11, b"MICROKERNEL & HARDWARE COUNTERS", STYLE_ACCENT);

    let mut l12 = LineBuilder::new();
    l12.push_bytes(b"  Syscalls     : ");
    l12.push_u64(sys.total_syscalls);
    write_row(surf, 12, l12.as_bytes(), STYLE_DEFAULT);

    let mut l13 = LineBuilder::new();
    l13.push_bytes(b"  IPC Messages : ");
    l13.push_u64(sys.total_ipc_msgs);
    write_row(surf, 13, l13.as_bytes(), STYLE_OK);

    let mut l14 = LineBuilder::new();
    l14.push_bytes(b"  Ctx Switches : ");
    l14.push_u64(sys.total_ctx_switches);
    write_row(surf, 14, l14.as_bytes(), STYLE_DEFAULT);

    let mut l15 = LineBuilder::new();
    l15.push_bytes(b"  LAPIC Timers : ");
    l15.push_u64(sys.timer_irqs);
    write_row(surf, 15, l15.as_bytes(), STYLE_DEFAULT);

    let mut l16 = LineBuilder::new();
    l16.push_bytes(b"  IRQ1 (KBD)   : ");
    l16.push_u64(sys.irq1_delivered);
    l16.push_bytes(b"   IRQ12 (MOUSE): ");
    l16.push_u64(sys.irq12_delivered);
    write_row(surf, 16, l16.as_bytes(), STYLE_ACCENT);

    write_row(surf, 17, &div, STYLE_MUTED);

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
    write_row(
        surf,
        18,
        l18.as_bytes(),
        if keys_rcvd > 0 { STYLE_WARN } else { STYLE_MUTED },
    );

    write_row(
        surf,
        19,
        b"[R] Refresh  [Q] Close Window  [X] Fault-Isolation",
        STYLE_MUTED,
    );

    surf.dirty_row_min.store(0, Ordering::Relaxed);
    surf.dirty_row_max.store((MON_ROWS - 1) as u16, Ordering::Relaxed);
    surf.uptime_sec_snap
        .store((sys.uptime_ms / 1000).min(u32::MAX as u64) as u32, Ordering::Relaxed);
    surf.ext_irqs_snap
        .store(sys.total_ext_irqs.min(u32::MAX as u64) as u32, Ordering::Relaxed);
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

    // Handshake with `dry-init` (`0x0103` = `dry-sysmon`), granting `SIGNAL` on `notif_cap` for clean shutdown wakeup.
    let disp_ep_cap = match sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0x0103, 0, 0, 0],
        notif_cap,
        RIGHT_READ | RIGHT_SIGNAL,
        0,
        0,
    ) {
        Ok((tag, cap, _)) if msg_tag_label(tag) == IPC_OP_PONG => cap,
        _ => sys_exit(162),
    };

    let surf_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(163),
    };
    if sys_vspace_map(surf_cap, SYSMON_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(164);
    }
    // SAFETY: `SYSMON_SURF_VADDR` is mapped `USER_RW` in `dry-sysmon`'s isolated `VSpace`.
    let surf = unsafe {
        SharedWindowSurface::init_at(SYSMON_SURF_VADDR, MON_COLS as u16, MON_ROWS as u16, false)
    };

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let mut refresh_seq: u64 = 1;
    let mut keys_rcvd: u64 = 0;
    let mut last_ascii: u8 = 0;
    render_telemetry_frame(surf, &sys, refresh_seq, keys_rcvd, last_ascii);

    let mut win_id: u32 = 0;
    if disp_ep_cap != 0 {
        // `1 << 16` = `NO_FOCUS_ON_OPEN` so Window A (`dry-shell`) starts with keyboard focus!
        let attach_mrs = [
            (MON_COLS as u64) | ((MON_ROWS as u64) << 16),
            MON_INIT_X | (MON_INIT_Y << 32),
            (WindowRole::Normal as u64) | ((TITLE_KIND_SYSMON as u64) << 8) | (1u64 << 16),
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

        let _ = sys_sysinfo(&mut sys);
        render_telemetry_frame(surf, &sys, refresh_seq, keys_rcvd, last_ascii);
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
