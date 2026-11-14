// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry_display` — Isolated Ring 3 (`CPL=3`) Framebuffer Banner, Window Manager & Desktop Compositor
//! (`Milestone 1C` + `Milestone 2A-1 P1` + `Milestone 2A-2 DRY Desktop Preview`).
//!
//! Operates in two modes selected by initial endpoint capability rights:
//! 1. **Milestone 1C Banner Mode (`SEND | CALL`):** Renders the Stage E diagnostic card once and
//!    reports `IPC_OP_DISPLAY_DONE` to `dry_init`.
//! 2. **Interactive / Multi-Window Compositor Mode (`RECV | REPLY`):** Audits GOP geometry &
//!    allocates 2 MiB WB RAM backing frames (`MAX_BACKBUF_2M = 8`), enforces capability-badge
//!    window ownership (`WindowManager`), snapshots atomic `USER_RO` client surfaces, renders
//!    Z-ordered windows + 12x16 software pointer, and blits only dirty rectangles to GOP MMIO.

#![no_std]
#![no_main]

use core::cell::UnsafeCell;

use dry_userspace::action::LatencyRing;
use dry_userspace::compositor::DesktopCompositor;
use dry_userspace::display_banner::run_legacy_banner_mode;
use dry_userspace::gfx::Rect;
use dry_userspace::input::InputEvent;
use dry_userspace::shell::LineBuilder;
use dry_userspace::surface::{
    capture_pixel_dirty_from_ro, PIXEL_SURF_MAX_H, PIXEL_SURF_MAX_PIXELS, PIXEL_SURF_MAX_W,
    SURF_ATTACH_FLAG_PIXEL, TERM_COLS, TERM_ROWS,
};
use dry_userspace::wm::{
    SessionVisualState, WindowManager, WindowRole, WmInputAction, MAX_WINDOWS, SUP_BIT_WM_ACTION,
};
use dry_userspace::wm_ipc::{
    emit_wm_exit_telemetry, handle_wm_admin_op, wake_pending_pixel_clients, wake_window_client,
    wm_log_line as log_line,
};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_query, sys_cap_revoke, sys_exit,
    sys_fb_map, sys_frame_alloc, sys_ipc_recv_ext, sys_ipc_reply_inline, sys_ipc_send_inline,
    sys_notify_signal, sys_vspace_map, sys_write_debug, UserCapInfo, UserFbInfo, UserIpcRecvExt,
    BADGE_INPUT_SVC, BADGE_SUPERVISOR, ETIMEDOUT, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM,
    IPC_OP_EXIT_CLEAN, IPC_OP_INPUT_EVENT, IPC_OP_PONG, IPC_OP_SURFACE_ATTACH,
    IPC_OP_SURFACE_FOCUS, IPC_OP_SURFACE_MOVE, IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_BIND_NOTIF,
    IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED, IPC_OP_WIN_EMPTY, IPC_OP_WIN_POLL_EVENT,
    IPC_OP_WIN_WAIT_EVENT, IPC_OP_WM_INPUT_EVENT, IPC_OP_WM_OWNER_GONE, IPC_OP_WM_POLL_ACTION,
    MSG_FLAG_NON_BLOCKING, RIGHT_RECV, VSPACE_MAP_OP_2M_RO, VSPACE_MAP_OP_2M_RW,
    VSPACE_MAP_OP_4K_RO, VSPACE_MAP_OP_UNMAP_2M, VSPACE_MAP_OP_UNMAP_4K,
};

const FB_MAP_VADDR: u64 = 0x1000_0000;
const SURFACE_BASE_VADDR: u64 = 0x1800_0000;
const PIXEL_SURF_BASE_VADDR: u64 = 0x1A00_0000;
const PIXEL_SNAP_BASE_VADDR: u64 = 0x1B00_0000;
const BACKBUF_VADDR: u64 = 0x2000_0000;
const WALLPAPER_VADDR: u64 = 0x3000_0000;
const TWO_MIB: usize = 2 * 1024 * 1024;
const MAX_BACKBUF_2M: usize = 8; // Supports up to 16 MiB (e.g., 2560x1600x32) with explicit bounds check.

struct SingleThreadCell<T>(UnsafeCell<T>);
// SAFETY: `dry-display` executes strictly on a single CPL=3 thread.
unsafe impl<T> Sync for SingleThreadCell<T> {}

static WM_STATE: SingleThreadCell<WindowManager> = SingleThreadCell(UnsafeCell::new(WindowManager::new()));
static COMP_STATE: SingleThreadCell<DesktopCompositor> =
    SingleThreadCell(UnsafeCell::new(DesktopCompositor::uninit()));

#[inline]
fn reply_err(disp_ep: u64, tok: u64, code: u32) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(disp_ep, tok, pack_msg_tag(code, 0, 1, 0), [0; 4]);
    }
}

fn teardown_window_slot(
    wm: &mut WindowManager,
    comp: &mut DesktopCompositor,
    disp_ep: u64,
    slot_idx: usize,
) {
    if let Some(rm) = wm.remove_window_slot(slot_idx, &mut comp.scene_dirty) {
        if rm.waiter_tok != 0 {
            let _ = sys_ipc_reply_inline(
                disp_ep,
                rm.waiter_tok,
                pack_msg_tag(IPC_OP_WIN_CLOSED, 0, 1, 0),
                [0; 4],
            );
        }
        if rm.notif_cap != 0 {
            let _ = sys_notify_signal(rm.notif_cap, 1);
            let _ = sys_cap_revoke(rm.notif_cap, 1);
        }
        if rm.surf_cap != 0 {
            let unmap_op = if rm.is_pixel {
                VSPACE_MAP_OP_UNMAP_2M
            } else {
                VSPACE_MAP_OP_UNMAP_4K
            };
            let _ = sys_vspace_map(rm.surf_cap, rm.surf_vaddr, unmap_op);
            let _ = sys_cap_revoke(rm.surf_cap, 1);
        }
        if rm.snap_cap != 0 {
            let _ = sys_vspace_map(rm.snap_cap, rm.snap_vaddr, VSPACE_MAP_OP_UNMAP_2M);
            let _ = sys_cap_revoke(rm.snap_cap, 1);
        }
    }
}

#[no_mangle]
pub extern "C" fn _start(disp_ep_cap: u64, fb_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || disp_ep_cap == 0 || fb_cap == 0 {
        sys_exit(110);
    }

    let mut info = UserFbInfo::default();
    if sys_fb_map(fb_cap, FB_MAP_VADDR, &mut info).is_err() {
        sys_exit(111);
    }

    // Strict Section 2 & 7 GOP + backing-buffer geometry validation (zero implicit OOB assumptions).
    let w = info.width as usize;
    let h = info.height as usize;
    let stride_px = (info.pitch as usize) / 4;
    let gop_bytes_needed = (info.pitch as u64).checked_mul(info.height as u64);
    if w < 320
        || h < 240
        || info.bpp != 32
        || (info.pitch & 3) != 0
        || stride_px < w
        || gop_bytes_needed.is_none_or(|b| b == 0 || b > info.mapped_bytes)
    {
        sys_exit(112);
    }

    let mut ep_info = UserCapInfo::default();
    if sys_cap_query(disp_ep_cap, &mut ep_info) < 0 {
        sys_exit(113);
    }
    if (ep_info.rights_bits & (RIGHT_RECV as u32)) == 0 {
        run_legacy_banner_mode(FB_MAP_VADDR, disp_ep_cap, &info);
    }

    let back_bytes_needed = match w.checked_mul(h).and_then(|px| px.checked_mul(4)) {
        Some(b) if b > 0 => b,
        _ => sys_exit(112),
    };
    let num_2m = back_bytes_needed.div_ceil(TWO_MIB);
    if num_2m == 0 || num_2m > MAX_BACKBUF_2M {
        sys_exit(112);
    }

    let mut back_caps = [0u64; MAX_BACKBUF_2M];
    for (i, slot) in back_caps.iter_mut().take(num_2m).enumerate() {
        let cap = match sys_frame_alloc(1) {
            Ok(c) => c,
            Err(_) => sys_exit(114),
        };
        let va = BACKBUF_VADDR + (i * TWO_MIB) as u64;
        if sys_vspace_map(cap, va, VSPACE_MAP_OP_2M_RW).is_err() {
            sys_exit(115);
        }
        *slot = cap;
    }

    let mut wall_caps = [0u64; MAX_BACKBUF_2M];
    for (i, slot) in wall_caps.iter_mut().take(num_2m).enumerate() {
        let cap = match sys_frame_alloc(1) {
            Ok(c) => c,
            Err(_) => sys_exit(114),
        };
        let va = WALLPAPER_VADDR + (i * TWO_MIB) as u64;
        if sys_vspace_map(cap, va, VSPACE_MAP_OP_2M_RW).is_err() {
            sys_exit(115);
        }
        *slot = cap;
    }

    // SAFETY: Single-threaded CPL=3 compositor owns `WM_STATE` and `COMP_STATE`.
    let wm = unsafe { &mut *WM_STATE.0.get() };
    let comp = unsafe { &mut *COMP_STATE.0.get() };
    wm.init_screen(w, h);
    wm.back_caps = back_caps;
    comp.init(
        FB_MAP_VADDR as *mut u32,
        BACKBUF_VADDR as *mut u32,
        WALLPAPER_VADDR as *mut u32,
        &info,
    );

    comp.scene_dirty.mark(Rect::new(0, 0, w, h));
    let _ = comp.flush_all(wm);

    let mut alb = LineBuilder::new();
    alb.push_bytes(b"[DRY-DISPLAY] Backing buffer audit: ");
    alb.push_u64(w as u64);
    alb.push_byte(b'x');
    alb.push_u64(h as u64);
    alb.push_bytes(b" pitch=");
    alb.push_u64(info.pitch as u64);
    alb.push_bytes(b" gop_mapped=");
    alb.push_u64(info.mapped_bytes);
    alb.push_bytes(b" back_2m=");
    alb.push_u64(num_2m as u64);
    alb.push_bytes(b" back_cap=");
    alb.push_u64((num_2m * TWO_MIB) as u64);
    log_line(&alb);
    let _ = sys_write_debug(b"[DRY-DISPLAY] P1 Ring 3 desktop compositor active\n");

    let mut in2comp_lat = LatencyRing::new();
    let mut cursor_lat = LatencyRing::new();
    let mut drag_lat = LatencyRing::new();
    let mut pres1w_lat = LatencyRing::new();
    let mut pres2w_lat = LatencyRing::new();

    loop {
        let recv_timeout = wm.next_dpms_recv_timeout_tsc(rdtsc_user());
        let mut ext = UserIpcRecvExt::default();
        let (tag, reply_tok, mrs) = match sys_ipc_recv_ext(disp_ep_cap, 0, recv_timeout, &mut ext) {
            Ok(v) => v,
            Err(ETIMEDOUT) => {
                let now_tsc = rdtsc_user();
                if wm.session_state == SessionVisualState::Active && wm.blank_timeout_tsc > 0 {
                    wm.set_session_state(
                        SessionVisualState::DisplayBlanked,
                        now_tsc,
                        &mut comp.scene_dirty,
                    );
                    let (px, _) = comp.flush_all(wm);
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-WM] DPMS_BLANK reason=idle px=");
                    lb.push_u64(px);
                    log_line(&lb);
                }
                continue;
            }
            Err(_) => break,
        };
        let op = msg_tag_label(tag);
        match op {
            IPC_OP_SURFACE_ATTACH => {
                if ext.sender_badge == 0 || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                }
                let is_pixel = (mrs[2] & SURF_ATTACH_FLAG_PIXEL) != 0;
                let w_or_cols = if mrs[0] == 0 && !is_pixel {
                    TERM_COLS as u16
                } else {
                    (mrs[0] & 0xffff) as u16
                };
                let h_or_rows = if mrs[0] == 0 && !is_pixel {
                    TERM_ROWS as u16
                } else {
                    ((mrs[0] >> 16) & 0xffff) as u16
                };
                let geom_ok = if is_pixel {
                    let pw = w_or_cols as usize;
                    let ph = h_or_rows as usize;
                    pw >= 160
                        && ph >= 120
                        && pw <= PIXEL_SURF_MAX_W
                        && ph <= PIXEL_SURF_MAX_H
                        && pw * ph <= PIXEL_SURF_MAX_PIXELS
                } else {
                    w_or_cols != 0
                        && h_or_rows != 0
                        && (w_or_cols as usize) <= TERM_COLS
                        && (h_or_rows as usize) <= TERM_ROWS
                };
                if !geom_ok {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                    continue;
                }

                let req_x = (mrs[1] & 0xffff_ffff) as usize;
                let req_y = (mrs[1] >> 32) as usize;
                let role = WindowRole::from_u8((mrs[2] & 0xff) as u8);
                let title_kind = ((mrs[2] >> 8) & 0xff) as u8;
                let focus_on_open = ((mrs[2] >> 16) & 1) == 0;
                let allow_multi = ((mrs[2] >> 17) & 1) != 0;

                if !allow_multi {
                    if let Some(existing) = (0..MAX_WINDOWS)
                        .find(|&i| wm.windows[i].allocated && wm.windows[i].owner_badge == ext.sender_badge)
                    {
                        teardown_window_slot(wm, comp, disp_ep_cap, existing);
                    }
                }

                let Some(free_slot) = (0..MAX_WINDOWS).find(|&i| !wm.windows[i].allocated) else {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                    continue;
                };

                let (slot_idx, snap_ok) = if is_pixel {
                    let surf_vaddr = PIXEL_SURF_BASE_VADDR + (free_slot as u64) * (TWO_MIB as u64);
                    let snap_vaddr = PIXEL_SNAP_BASE_VADDR + (free_slot as u64) * (TWO_MIB as u64);
                    if sys_vspace_map(ext.transferred_cap, surf_vaddr, VSPACE_MAP_OP_2M_RO).is_err() {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                        reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                        continue;
                    }
                    let Ok(snap_cap) = sys_frame_alloc(1) else {
                        let _ = sys_vspace_map(ext.transferred_cap, surf_vaddr, VSPACE_MAP_OP_UNMAP_2M);
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                        continue;
                    };
                    if sys_vspace_map(snap_cap, snap_vaddr, VSPACE_MAP_OP_2M_RW).is_err() {
                        let _ = sys_cap_revoke(snap_cap, 1);
                        let _ = sys_vspace_map(ext.transferred_cap, surf_vaddr, VSPACE_MAP_OP_UNMAP_2M);
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                        continue;
                    }
                    let Ok(si) = wm.register_pixel_window(
                        ext.sender_badge,
                        w_or_cols,
                        h_or_rows,
                        req_x,
                        req_y,
                        role,
                        title_kind,
                        ext.transferred_cap,
                        surf_vaddr,
                        snap_cap,
                        snap_vaddr,
                        focus_on_open,
                        &mut comp.scene_dirty,
                    ) else {
                        let _ = sys_vspace_map(snap_cap, snap_vaddr, VSPACE_MAP_OP_UNMAP_2M);
                        let _ = sys_cap_revoke(snap_cap, 1);
                        let _ = sys_vspace_map(ext.transferred_cap, surf_vaddr, VSPACE_MAP_OP_UNMAP_2M);
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                        continue;
                    };
                    // SAFETY: `surf_vaddr` (`USER_RO`) & `snap_vaddr` (`USER_RW`) are mapped `2 MiB` frames.
                    let mut init_pw = w_or_cols as usize;
                    let mut init_ph = h_or_rows as usize;
                    let ok = unsafe {
                        capture_pixel_dirty_from_ro(
                            surf_vaddr,
                            snap_vaddr,
                            &mut init_pw,
                            &mut init_ph,
                            true,
                        )
                        .is_ok_and(|opt| opt.is_some())
                    };
                    (si, ok)
                } else {
                    let surf_vaddr = SURFACE_BASE_VADDR + (free_slot as u64) * 0x1000;
                    if sys_vspace_map(ext.transferred_cap, surf_vaddr, VSPACE_MAP_OP_4K_RO).is_err() {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                        reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                        continue;
                    }
                    let Ok(si) = wm.register_window(
                        ext.sender_badge,
                        w_or_cols,
                        h_or_rows,
                        req_x,
                        req_y,
                        role,
                        title_kind,
                        ext.transferred_cap,
                        surf_vaddr,
                        focus_on_open,
                        &mut comp.scene_dirty,
                    ) else {
                        let _ = sys_vspace_map(ext.transferred_cap, surf_vaddr, VSPACE_MAP_OP_UNMAP_4K);
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                        continue;
                    };
                    // SAFETY: `surf_vaddr` was just mapped `USER_RO` in `dry-display`'s `VSpace`.
                    let ok = unsafe {
                        wm.windows[si].snapshot.capture_from_ro(
                            surf_vaddr,
                            w_or_cols,
                            h_or_rows,
                            &mut comp.scratch_cells,
                        )
                        .is_ok()
                    };
                    (si, ok)
                };
                if !snap_ok {
                    teardown_window_slot(wm, comp, disp_ep_cap, slot_idx);
                    wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                    continue;
                }

                comp.scene_dirty.mark(wm.windows[slot_idx].outer_rect());
                let (_, cyc) = comp.flush_all(wm);
                wake_pending_pixel_clients(disp_ep_cap, wm);
                let wid = wm.windows[slot_idx].id;
                let wx = wm.windows[slot_idx].rect.x;
                let wy = wm.windows[slot_idx].rect.y;
                let cols = w_or_cols;
                let rows = h_or_rows;

                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-WM] WIN_ATTACH id=");
                lb.push_u64(wid as u64);
                lb.push_bytes(b" badge=0x");
                lb.push_hex_u64(ext.sender_badge);
                lb.push_bytes(b" geom=");
                lb.push_u64(cols as u64);
                lb.push_byte(b'x');
                lb.push_u64(rows as u64);
                lb.push_bytes(b" pos=");
                lb.push_u64(wx as u64);
                lb.push_byte(b',');
                lb.push_u64(wy as u64);
                lb.push_bytes(b" active=");
                lb.push_u64(wm.active_window_count() as u64);
                log_line(&lb);

                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [wid as u64, wx as u64, wy as u64, cyc],
                    );
                }
            }
            IPC_OP_SURFACE_PRESENT => {
                let Ok(slot_idx) = wm.resolve_owned_slot(ext.sender_badge, mrs[0] as u32) else {
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                };
                let ro_va = wm.windows[slot_idx].surf_vaddr;
                let ok = if wm.windows[slot_idx].is_pixel {
                    let snap_va = wm.windows[slot_idx].snap_vaddr;
                    let mut pw = wm.windows[slot_idx].pixel_w as usize;
                    let mut ph = wm.windows[slot_idx].pixel_h as usize;
                    let (old_pw, old_ph) = (pw, ph);
                    // SAFETY: `ro_va` and `snap_va` remain mapped for the lifetime of `slot_idx`.
                    let res = unsafe {
                        capture_pixel_dirty_from_ro(ro_va, snap_va, &mut pw, &mut ph, false)
                    };
                    match res {
                        Ok(Some((dx, dy, dw, dh))) => {
                            if pw != old_pw || ph != old_ph {
                                wm.windows[slot_idx].pixel_w = pw as u16;
                                wm.windows[slot_idx].pixel_h = ph as u16;
                                if wm.session_state == SessionVisualState::Active {
                                    comp.scene_dirty.mark(wm.windows[slot_idx].outer_rect());
                                }
                            } else if wm.session_state == SessionVisualState::Active
                                && dw > 0
                                && dh > 0
                            {
                                let (gx, gy) = wm.windows[slot_idx].grid_origin();
                                comp.scene_dirty.mark(Rect::new(gx + dx, gy + dy, dw, dh));
                                comp.scene_dirty.mark(wm.windows[slot_idx].titlebar_rect());
                            }
                            true
                        }
                        Ok(None) => true,
                        Err(_) => false,
                    }
                } else {
                    let cols = wm.windows[slot_idx].cols;
                    let rows = wm.windows[slot_idx].rows;
                    // SAFETY: `ro_va` is mapped `USER_RO` for the lifetime of `wm.windows[slot_idx]`.
                    let snap = unsafe {
                        wm.windows[slot_idx].snapshot.capture_from_ro(
                            ro_va,
                            cols,
                            rows,
                            &mut comp.scratch_cells,
                        )
                    };
                    match snap {
                        Ok((rmin, rmax, changed)) => {
                            if changed && wm.session_state == SessionVisualState::Active {
                                let (gx, gy) = wm.windows[slot_idx].grid_origin();
                                comp.scene_dirty.mark(Rect::new(
                                    gx,
                                    gy + rmin * 16,
                                    (cols as usize) * 8,
                                    (rmax - rmin + 1) * 16,
                                ));
                                comp.scene_dirty.mark(wm.windows[slot_idx].titlebar_rect());
                            }
                            true
                        }
                        Err(_) => false,
                    }
                };
                if !ok {
                    wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                    continue;
                }
                let (px, cyc) = comp.flush_all(wm);
                if px > 0 {
                    if wm.active_window_count() >= 2 {
                        pres2w_lat.record(cyc);
                    } else {
                        pres1w_lat.record(cyc);
                    }
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [px, cyc, wm.windows[slot_idx].id as u64, 0],
                    );
                }
            }
            IPC_OP_SURFACE_MOVE => {
                let Ok(slot_idx) = wm.resolve_owned_slot(ext.sender_badge, mrs[2] as u32) else {
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                };
                let (nx, ny) = wm.move_window_by(
                    slot_idx,
                    (mrs[0] as i64) as isize,
                    (mrs[1] as i64) as isize,
                    &mut comp.scene_dirty,
                );
                let (_, cyc) = comp.flush_all(wm);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [nx as u64, ny as u64, cyc, 0],
                    );
                }
            }
            IPC_OP_SURFACE_FOCUS => {
                let Ok(slot_idx) = wm.resolve_owned_slot(ext.sender_badge, mrs[1] as u32) else {
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                };
                if mrs[0] != 0 {
                    let _ = wm.focus_and_raise(slot_idx, &mut comp.scene_dirty);
                } else if wm.windows[slot_idx].focused {
                    wm.windows[slot_idx].focused = false;
                    comp.scene_dirty.mark(wm.windows[slot_idx].outer_rect());
                }
                let (px, cyc) = comp.flush_all(wm);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [px, cyc, 0, 0],
                    );
                }
            }
            IPC_OP_WIN_BIND_NOTIF => {
                let Ok(slot_idx) = wm.resolve_owned_slot(ext.sender_badge, mrs[0] as u32) else {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                };
                if wm.windows[slot_idx].notif_cap != 0 {
                    let _ = sys_cap_revoke(wm.windows[slot_idx].notif_cap, 1);
                }
                wm.windows[slot_idx].notif_cap = ext.transferred_cap;
                wake_window_client(disp_ep_cap, wm, slot_idx);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [u64::from(ext.transferred_cap != 0), 0, 0, 0],
                    );
                }
            }
            IPC_OP_WIN_WAIT_EVENT => {
                let Ok(slot_idx) = wm.resolve_owned_slot(ext.sender_badge, mrs[0] as u32) else {
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                };
                if wm.windows[slot_idx].close_requested {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            disp_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_WIN_CLOSED, 0, 1, 0),
                            [wm.windows[slot_idx].id as u64, 0, 0, 0],
                        );
                    }
                } else if let Some(ev) = wm.windows[slot_idx].ev_queue.pop() {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            disp_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_INPUT_EVENT, 0, 4, 0),
                            ev.to_ipc_mrs(wm.windows[slot_idx].ev_queue.overflow_count()),
                        );
                    }
                } else if reply_tok != 0 {
                    wm.windows[slot_idx].waiter_reply_tok = reply_tok;
                }
            }
            IPC_OP_WIN_POLL_EVENT => {
                let Ok(slot_idx) = wm.resolve_owned_slot(ext.sender_badge, mrs[0] as u32) else {
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                };
                if wm.windows[slot_idx].close_requested {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            disp_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_WIN_CLOSED, 0, 1, 0),
                            [wm.windows[slot_idx].id as u64, 0, 0, 0],
                        );
                    }
                } else if let Some(ev) = wm.windows[slot_idx].ev_queue.pop() {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            disp_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_INPUT_EVENT, 0, 4, 0),
                            ev.to_ipc_mrs(wm.windows[slot_idx].ev_queue.overflow_count()),
                        );
                    }
                } else if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_WIN_EMPTY, 0, 1, 0),
                        [0; 4],
                    );
                }
            }
            IPC_OP_WIN_CLOSE => {
                let Ok(slot_idx) = wm.resolve_owned_slot(ext.sender_badge, mrs[0] as u32) else {
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                };
                let wid = wm.windows[slot_idx].id;
                teardown_window_slot(wm, comp, disp_ep_cap, slot_idx);
                let (_, cyc) = comp.flush_all(wm);
                wake_pending_pixel_clients(disp_ep_cap, wm);
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-WM] WIN_CLOSED id=");
                lb.push_u64(wid as u64);
                lb.push_bytes(b" active=");
                lb.push_u64(wm.active_window_count() as u64);
                log_line(&lb);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [wid as u64, wm.active_window_count() as u64, cyc, 0],
                    );
                }
            }
            IPC_OP_WM_OWNER_GONE => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                }
                let dead_badge = mrs[0];
                let mut reaped = 0u64;
                for slot_idx in 0..MAX_WINDOWS {
                    if wm.windows[slot_idx].allocated && wm.windows[slot_idx].owner_badge == dead_badge {
                        teardown_window_slot(wm, comp, disp_ep_cap, slot_idx);
                        reaped += 1;
                    }
                }
                let (_, cyc) = comp.flush_all(wm);
                wake_pending_pixel_clients(disp_ep_cap, wm);
                if reaped > 0 {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-WM] OWNER_REAPED badge=0x");
                    lb.push_hex_u64(dead_badge);
                    lb.push_bytes(b" active=");
                    lb.push_u64(wm.active_window_count() as u64);
                    log_line(&lb);
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [reaped, wm.active_window_count() as u64, cyc, 0],
                    );
                }
            }
            IPC_OP_WM_INPUT_EVENT => {
                if ext.sender_badge != BADGE_INPUT_SVC {
                    wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                }
                let (ev, _) = InputEvent::from_ipc_mrs(mrs);
                in2comp_lat.record(ev.latency_cycles(rdtsc_user()));

                let action = wm.handle_input_event(ev, &mut comp.scene_dirty, &mut comp.mmio_dirty);
                match action {
                    WmInputAction::KeyRouted(wid)
                    | WmInputAction::PointerRouted(wid)
                    | WmInputAction::WheelRouted(wid)
                    | WmInputAction::CloseRequested(wid) => {
                        if let Some(s) = (0..MAX_WINDOWS)
                            .find(|&i| wm.windows[i].allocated && wm.windows[i].id == wid)
                        {
                            wake_window_client(disp_ep_cap, wm, s);
                        }
                    }
                    WmInputAction::DesktopActionQueued(act) => {
                        let words = act.to_words();
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] ACTION_QUEUED kind=");
                        lb.push_u64(words[0]);
                        lb.push_bytes(b" arg=");
                        lb.push_u64(words[1]);
                        log_line(&lb);
                        if wm.sup_notif_cap != 0 {
                            let _ = sys_notify_signal(wm.sup_notif_cap, SUP_BIT_WM_ACTION);
                        }
                        if wm.sup_wake_cap != 0 {
                            let _ = sys_ipc_send_inline(
                                wm.sup_wake_cap,
                                pack_msg_tag(IPC_OP_WM_POLL_ACTION, MSG_FLAG_NON_BLOCKING, 0, 0),
                                [0; 4],
                                0,
                            );
                        }
                    }
                    _ => {}
                }
                wake_pending_pixel_clients(disp_ep_cap, wm);

                let (px, cyc) = comp.flush_all(wm);
                match action {
                    WmInputAction::CursorMoved if px > 0 => cursor_lat.record(cyc),
                    WmInputAction::WindowDragged(_) if px > 0 => drag_lat.record(cyc),
                    WmInputAction::FocusChanged(wid) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] FOCUS id=");
                        lb.push_u64(wid as u64);
                        lb.push_bytes(b" z_raises=");
                        lb.push_u64(wm.z_raises);
                        lb.push_bytes(b" cur=");
                        lb.push_u64(wm.cursor_x as u64);
                        lb.push_byte(b',');
                        lb.push_u64(wm.cursor_y as u64);
                        log_line(&lb);
                    }
                    WmInputAction::DragEnded(wid, x, y) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] DRAG_END id=");
                        lb.push_u64(wid as u64);
                        lb.push_bytes(b" x=");
                        lb.push_u64(x as u64);
                        lb.push_bytes(b" y=");
                        lb.push_u64(y as u64);
                        lb.push_bytes(b" moves=");
                        lb.push_u64(wm.drag_moves);
                        log_line(&lb);
                    }
                    WmInputAction::LayoutToggled(mode) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] LAYOUT mode=");
                        lb.push_u64(mode as u64);
                        log_line(&lb);
                    }
                    WmInputAction::CloseRequested(wid) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] CLOSE_REQ id=");
                        lb.push_u64(wid as u64);
                        log_line(&lb);
                    }
                    WmInputAction::KeyMoved(wid, x, y) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] KEY_MOVE id=");
                        lb.push_u64(wid as u64);
                        lb.push_bytes(b" x=");
                        lb.push_u64(x as u64);
                        lb.push_bytes(b" y=");
                        lb.push_u64(y as u64);
                        log_line(&lb);
                    }
                    WmInputAction::DpmsWoke => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] DPMS_WAKE wakes=");
                        lb.push_u64(wm.dpms_wake_count);
                        log_line(&lb);
                    }
                    WmInputAction::LockBlocked => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-WM] LOCK_BLOCKED count=");
                        lb.push_u64(wm.locked_blocked_events);
                        log_line(&lb);
                    }
                    _ => {}
                }

                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [px, cyc, wm.cursor_x as u64, wm.cursor_y as u64],
                    );
                }
            }
            IPC_OP_EXIT_CLEAN => {
                emit_wm_exit_telemetry(
                    wm,
                    &in2comp_lat,
                    &cursor_lat,
                    &drag_lat,
                    &pres1w_lat,
                    &pres2w_lat,
                );

                if wm.sup_notif_cap != 0 {
                    let _ = sys_cap_revoke(wm.sup_notif_cap, 1);
                    wm.sup_notif_cap = 0;
                }
                if wm.sup_wake_cap != 0 {
                    let _ = sys_cap_revoke(wm.sup_wake_cap, 1);
                    wm.sup_wake_cap = 0;
                }
                if wm.gpu_ep_cap != 0 {
                    let _ = sys_cap_revoke(wm.gpu_ep_cap, 1);
                    wm.gpu_ep_cap = 0;
                }
                for slot_idx in 0..MAX_WINDOWS {
                    if wm.windows[slot_idx].allocated {
                        teardown_window_slot(wm, comp, disp_ep_cap, slot_idx);
                    }
                }
                for (i, &cap) in back_caps.iter().take(num_2m).enumerate() {
                    if cap != 0 {
                        let va = BACKBUF_VADDR + (i * TWO_MIB) as u64;
                        let _ = sys_vspace_map(cap, va, VSPACE_MAP_OP_UNMAP_2M);
                        let _ = sys_cap_revoke(cap, 2);
                    }
                }
                for (i, &cap) in wall_caps.iter().take(num_2m).enumerate() {
                    if cap != 0 {
                        let va = WALLPAPER_VADDR + (i * TWO_MIB) as u64;
                        let _ = sys_vspace_map(cap, va, VSPACE_MAP_OP_UNMAP_2M);
                        let _ = sys_cap_revoke(cap, 2);
                    }
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => {
                if handle_wm_admin_op(op, disp_ep_cap, reply_tok, mrs, &ext, wm, comp) {
                    continue;
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [0; 4],
                    );
                }
            }
        }
    }
    sys_exit(0);
}
