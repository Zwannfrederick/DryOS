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
use core::ptr::write_volatile;

use dry_userspace::compositor::{
    flush_video_stores, pack_rgb, DesktopCompositor, Palette,
};
use dry_userspace::gfx::{glyph_8x16, Rect, BOX_HORIZ};
use dry_userspace::input::InputEvent;
use dry_userspace::shell::LineBuilder;
use dry_userspace::surface::{TERM_COLS, TERM_ROWS};
use dry_userspace::wm::{WindowManager, WindowRole, WmInputAction, MAX_WINDOWS};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_query, sys_cap_revoke, sys_exit,
    sys_fb_map, sys_frame_alloc, sys_ipc_call_inline, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_notify_signal, sys_vspace_map, sys_write_debug, UserCapInfo, UserFbInfo, UserIpcRecvExt,
    BADGE_INPUT_SVC, BADGE_SUPERVISOR, IPC_OP_DISPLAY_DONE, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM,
    IPC_OP_EXIT_CLEAN, IPC_OP_INPUT_EVENT, IPC_OP_PONG, IPC_OP_SURFACE_ATTACH,
    IPC_OP_SURFACE_FOCUS, IPC_OP_SURFACE_MOVE, IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_BIND_NOTIF,
    IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED, IPC_OP_WIN_EMPTY, IPC_OP_WIN_POLL_EVENT,
    IPC_OP_WIN_WAIT_EVENT, IPC_OP_WM_INPUT_EVENT, IPC_OP_WM_OWNER_GONE, IPC_OP_WM_QUERY_STATE,
    IPC_OP_WM_TOGGLE_TILING, RIGHT_RECV, VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_4K_RO,
    VSPACE_MAP_OP_UNMAP_2M, VSPACE_MAP_OP_UNMAP_4K,
};

const FB_MAP_VADDR: u64 = 0x1000_0000;
const SURFACE_BASE_VADDR: u64 = 0x1800_0000;
const BACKBUF_VADDR: u64 = 0x2000_0000;
const TWO_MIB: usize = 2 * 1024 * 1024;
const MAX_BACKBUF_2M: usize = 8; // Supports up to 16 MiB (e.g., 2560x1600x32) with explicit bounds check.

struct SingleThreadCell<T>(UnsafeCell<T>);
// SAFETY: `dry-display` executes strictly on a single CPL=3 thread.
unsafe impl<T> Sync for SingleThreadCell<T> {}

static WM_STATE: SingleThreadCell<WindowManager> = SingleThreadCell(UnsafeCell::new(WindowManager::new()));
static COMP_STATE: SingleThreadCell<DesktopCompositor> =
    SingleThreadCell(UnsafeCell::new(DesktopCompositor::uninit()));

struct LatencyRing {
    samples: [u32; 64],
    count: usize,
}

impl LatencyRing {
    const fn new() -> Self {
        Self {
            samples: [0; 64],
            count: 0,
        }
    }

    #[inline]
    fn record(&mut self, cyc: u64) {
        let idx = self.count & 63;
        self.samples[idx] = cyc.min(u32::MAX as u64) as u32;
        self.count = self.count.wrapping_add(1);
    }

    fn percentiles(&self) -> (u64, u64, u64) {
        let n = self.count.min(64);
        if n == 0 {
            return (0, 0, 0);
        }
        let mut tmp = [0u32; 64];
        tmp[..n].copy_from_slice(&self.samples[..n]);
        for i in 1..n {
            let key = tmp[i];
            let mut j = i;
            while j > 0 && tmp[j - 1] > key {
                tmp[j] = tmp[j - 1];
                j -= 1;
            }
            tmp[j] = key;
        }
        (
            tmp[(n - 1) * 50 / 100] as u64,
            tmp[(n - 1) * 95 / 100] as u64,
            tmp[(n - 1) * 99 / 100] as u64,
        )
    }
}

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 196];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
fn put_px_mmio(base: *mut u32, stride_px: usize, w: usize, h: usize, x: usize, y: usize, color: u32) {
    if x < w && y < h {
        // SAFETY: `(x, y)` lies strictly inside mapped GOP framebuffer.
        unsafe {
            write_volatile(base.add(y * stride_px + x), color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect_mmio(
    base: *mut u32,
    stride_px: usize,
    fb_w: usize,
    fb_h: usize,
    rx: usize,
    ry: usize,
    rw: usize,
    rh: usize,
    color: u32,
) {
    let y_end = (ry + rh).min(fb_h);
    let x_end = (rx + rw).min(fb_w);
    for y in ry..y_end {
        let row_off = y * stride_px;
        for x in rx..x_end {
            // SAFETY: Bounds clamped to `(fb_w, fb_h)`.
            unsafe {
                write_volatile(base.add(row_off + x), color);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bytes_mmio(
    base: *mut u32,
    stride_px: usize,
    fb_w: usize,
    fb_h: usize,
    start_x: usize,
    start_y: usize,
    scale: usize,
    text: &[u8],
    color: u32,
) -> u64 {
    let mut fg_count = 0u64;
    let adv_x = 9 * scale;
    for (idx, &ch) in text.iter().enumerate() {
        let g = glyph_8x16(ch);
        let gx = start_x + idx * adv_x;
        for (row, &bits) in g.iter().enumerate() {
            if bits == 0 {
                continue;
            }
            for col in 0..8usize {
                if (bits & (0x80 >> col)) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            put_px_mmio(
                                base,
                                stride_px,
                                fb_w,
                                fb_h,
                                gx + col * scale + sx,
                                start_y + row * scale + sy,
                                color,
                            );
                            fg_count += 1;
                        }
                    }
                }
            }
        }
    }
    fg_count
}

/// Legacy Milestone 1C one-shot visual proof (`Stage E.2`). Kept 100% intact for regression parity.
fn run_legacy_banner_mode(disp_ep_cap: u64, info: &UserFbInfo) -> ! {
    let w = info.width as usize;
    let h = info.height as usize;
    let stride_px = (info.pitch as usize) / 4;
    let fb_ptr = FB_MAP_VADDR as *mut u32;
    let pal = Palette::from_fb(info);
    let bg_card = pack_rgb(info, 0x11, 0x18, 0x27);
    let border_col = pack_rgb(info, 0x28, 0x37, 0x4D);

    fill_rect_mmio(fb_ptr, stride_px, w, h, 0, 0, w, h, pal.bg_canvas);

    let scale = if w >= 800 && h >= 520 { 2usize } else { 1usize };
    let card_w = (320 * scale).min(w.saturating_sub(32));
    let card_h = (220 * scale).min(h.saturating_sub(32));
    let card_x = (w - card_w) / 2;
    let card_y = (h - card_h) / 2;

    fill_rect_mmio(fb_ptr, stride_px, w, h, card_x, card_y, card_w, card_h, border_col);
    fill_rect_mmio(fb_ptr, stride_px, w, h, card_x + 2, card_y + 2, card_w - 4, card_h - 4, bg_card);
    fill_rect_mmio(fb_ptr, stride_px, w, h, card_x + 2, card_y + 2, card_w - 4, 4 * scale, pal.fg_accent);

    let tx = card_x + 24 * scale;
    let mut ty = card_y + 22 * scale;
    let line_step = 22 * scale;
    let mut fg_pixels = 0u64;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"DRY OS", pal.fg_primary);
    ty += line_step;

    let divider = [BOX_HORIZ; 26];
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, &divider, pal.fg_accent);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"System: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 8 * 9 * scale, ty, scale, b"ONLINE", pal.fg_ok);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Kernel: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 8 * 9 * scale, ty, scale, b"Microkernel", pal.fg_primary);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Userspace: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 11 * 9 * scale, ty, scale, b"Ring 3", pal.fg_primary);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"IPC: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 5 * 9 * scale, ty, scale, b"ACTIVE", pal.fg_ok);
    ty += line_step + 10 * scale;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Hello from DRY OS!", pal.fg_accent);

    flush_video_stores();
    let _ = sys_write_debug(b"[DRY-DISPLAY] Rendered Ring 3 framebuffer banner\n");
    let _ = sys_ipc_call_inline(
        disp_ep_cap,
        pack_msg_tag(IPC_OP_DISPLAY_DONE, 0, 3, 0),
        [w as u64, h as u64, fg_pixels, 0],
        0,
    );
    sys_exit(0);
}

#[inline]
fn reply_err(disp_ep: u64, tok: u64, code: u32) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(disp_ep, tok, pack_msg_tag(code, 0, 1, 0), [0; 4]);
    }
}

fn wake_window_client(disp_ep: u64, wm: &mut WindowManager, slot_idx: usize) {
    let win = &mut wm.windows[slot_idx];
    if !win.allocated {
        return;
    }
    if win.close_requested {
        if win.waiter_reply_tok != 0 {
            let _ = sys_ipc_reply_inline(
                disp_ep,
                win.waiter_reply_tok,
                pack_msg_tag(IPC_OP_WIN_CLOSED, 0, 1, 0),
                [win.id as u64, 0, 0, 0],
            );
            win.waiter_reply_tok = 0;
        } else if win.notif_cap != 0 {
            let _ = sys_notify_signal(win.notif_cap, 1);
        }
        return;
    }
    if win.waiter_reply_tok != 0 {
        if let Some(ev) = win.ev_queue.pop() {
            let _ = sys_ipc_reply_inline(
                disp_ep,
                win.waiter_reply_tok,
                pack_msg_tag(IPC_OP_INPUT_EVENT, 0, 4, 0),
                ev.to_ipc_mrs(win.ev_queue.overflow_count()),
            );
            win.waiter_reply_tok = 0;
        }
    } else if win.notif_cap != 0 && !win.ev_queue.is_empty() {
        let _ = sys_notify_signal(win.notif_cap, 1);
    }
}

fn teardown_window_slot(
    wm: &mut WindowManager,
    comp: &mut DesktopCompositor,
    disp_ep: u64,
    slot_idx: usize,
) {
    if let Some((surf_cap, surf_vaddr, notif_cap, waiter_tok)) =
        wm.remove_window_slot(slot_idx, &mut comp.scene_dirty)
    {
        if waiter_tok != 0 {
            let _ = sys_ipc_reply_inline(
                disp_ep,
                waiter_tok,
                pack_msg_tag(IPC_OP_WIN_CLOSED, 0, 1, 0),
                [0; 4],
            );
        }
        if notif_cap != 0 {
            let _ = sys_notify_signal(notif_cap, 1);
            let _ = sys_cap_revoke(notif_cap, 1);
        }
        if surf_cap != 0 {
            let _ = sys_vspace_map(surf_cap, surf_vaddr, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(surf_cap, 1);
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
        run_legacy_banner_mode(disp_ep_cap, &info);
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

    // SAFETY: Single-threaded CPL=3 compositor owns `WM_STATE` and `COMP_STATE`.
    let wm = unsafe { &mut *WM_STATE.0.get() };
    let comp = unsafe { &mut *COMP_STATE.0.get() };
    wm.init_screen(w, h);
    comp.init(FB_MAP_VADDR as *mut u32, BACKBUF_VADDR as *mut u32, &info);

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
        let mut ext = UserIpcRecvExt::default();
        let (tag, reply_tok, mrs) = match sys_ipc_recv_ext(disp_ep_cap, 0, 0, &mut ext) {
            Ok(v) => v,
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
                let cols = if mrs[0] == 0 {
                    TERM_COLS as u16
                } else {
                    (mrs[0] & 0xffff) as u16
                };
                let rows = if mrs[0] == 0 {
                    TERM_ROWS as u16
                } else {
                    ((mrs[0] >> 16) & 0xffff) as u16
                };
                if cols == 0
                    || rows == 0
                    || (cols as usize) > TERM_COLS
                    || (rows as usize) > TERM_ROWS
                {
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
                let surf_vaddr = SURFACE_BASE_VADDR + (free_slot as u64) * 0x1000;
                if sys_vspace_map(ext.transferred_cap, surf_vaddr, VSPACE_MAP_OP_4K_RO).is_err() {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                }

                let Ok(slot_idx) = wm.register_window(
                    ext.sender_badge,
                    cols,
                    rows,
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
                let snap_res = unsafe {
                    wm.windows[slot_idx].snapshot.capture_from_ro(
                        surf_vaddr,
                        cols,
                        rows,
                        &mut comp.scratch_cells,
                    )
                };
                if snap_res.is_err() {
                    teardown_window_slot(wm, comp, disp_ep_cap, slot_idx);
                    wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                    continue;
                }

                comp.scene_dirty.mark(wm.windows[slot_idx].outer_rect());
                let (_, cyc) = comp.flush_all(wm);
                let wid = wm.windows[slot_idx].id;
                let wx = wm.windows[slot_idx].rect.x;
                let wy = wm.windows[slot_idx].rect.y;

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
                let cols = wm.windows[slot_idx].cols;
                let rows = wm.windows[slot_idx].rows;
                // SAFETY: `ro_va` is mapped `USER_RO` for the lifetime of `wm.windows[slot_idx]`.
                let snap = unsafe {
                    wm.windows[slot_idx]
                        .snapshot
                        .capture_from_ro(ro_va, cols, rows, &mut comp.scratch_cells)
                };
                match snap {
                    Ok((rmin, rmax, changed)) => {
                        if changed {
                            let (gx, gy) = wm.windows[slot_idx].grid_origin();
                            comp.scene_dirty.mark(Rect::new(
                                gx,
                                gy + rmin * 16,
                                (cols as usize) * 8,
                                (rmax - rmin + 1) * 16,
                            ));
                            comp.scene_dirty.mark(wm.windows[slot_idx].titlebar_rect());
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
                    Err(_) => {
                        wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                        reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                    }
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
                    WmInputAction::KeyRouted(wid) | WmInputAction::CloseRequested(wid) => {
                        if let Some(s) = (0..MAX_WINDOWS)
                            .find(|&i| wm.windows[i].allocated && wm.windows[i].id == wid)
                        {
                            wake_window_client(disp_ep_cap, wm, s);
                        }
                    }
                    _ => {}
                }

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
            IPC_OP_WM_TOGGLE_TILING => {
                let allowed = ext.sender_badge == BADGE_SUPERVISOR
                    || ext.sender_badge == BADGE_INPUT_SVC
                    || wm.resolve_owned_slot(ext.sender_badge, 0).is_ok();
                if !allowed {
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                    continue;
                }
                let m = wm.toggle_tiling(&mut comp.scene_dirty);
                let (_, cyc) = comp.flush_all(wm);
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-WM] LAYOUT mode=");
                lb.push_u64(m as u64);
                log_line(&lb);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [m as u64, cyc, 0, 0],
                    );
                }
            }
            IPC_OP_WM_QUERY_STATE => {
                let fid = wm.focused_slot().map_or(0, |s| wm.windows[s].id);
                let mr0 = (wm.cursor_x as u64)
                    | ((wm.cursor_y as u64) << 16)
                    | ((fid as u64) << 32)
                    | ((wm.layout as u64) << 48);
                let mr1 = (wm.unauthorized_rejects & 0xffff_ffff)
                    | ((wm.invalid_req_rejects & 0xffff_ffff) << 32);
                let mr2 = (wm.focus_switches & 0xffff)
                    | ((wm.z_raises & 0xffff) << 16)
                    | ((wm.drag_moves & 0xffff) << 32)
                    | ((wm.cursor_clamp_hits & 0xffff) << 48);
                let mr3 = wm.active_window_count() as u64;
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [mr0, mr1, mr2, mr3],
                    );
                }
            }
            IPC_OP_EXIT_CLEAN => {
                let mut slb = LineBuilder::new();
                slb.push_bytes(b"[DRY-WM-STATS] wins=");
                slb.push_u64(wm.active_window_count() as u64);
                slb.push_bytes(b" focus_sw=");
                slb.push_u64(wm.focus_switches);
                slb.push_bytes(b" z_raises=");
                slb.push_u64(wm.z_raises);
                slb.push_bytes(b" drag_moves=");
                slb.push_u64(wm.drag_moves);
                slb.push_bytes(b" clamp_hits=");
                slb.push_u64(wm.cursor_clamp_hits);
                slb.push_bytes(b" unauth_rej=");
                slb.push_u64(wm.unauthorized_rejects);
                slb.push_bytes(b" inval_rej=");
                slb.push_u64(wm.invalid_req_rejects);
                log_line(&slb);

                let (i50, i95, i99) = in2comp_lat.percentiles();
                let (c50, c95, c99) = cursor_lat.percentiles();
                let (d50, d95, d99) = drag_lat.percentiles();
                let (p1_50, p1_95, p1_99) = pres1w_lat.percentiles();
                let (p2_50, p2_95, p2_99) = pres2w_lat.percentiles();
                if i50 > 0 || c50 > 0 || p2_50 > 0 {
                    let mut blb = LineBuilder::new();
                    blb.push_bytes(b"[DRY-WM-BENCH] in2comp=");
                    blb.push_u64(i50);
                    blb.push_byte(b'/');
                    blb.push_u64(i95);
                    blb.push_byte(b'/');
                    blb.push_u64(i99);
                    blb.push_bytes(b" cur=");
                    blb.push_u64(c50);
                    blb.push_byte(b'/');
                    blb.push_u64(c95);
                    blb.push_byte(b'/');
                    blb.push_u64(c99);
                    blb.push_bytes(b" drag=");
                    blb.push_u64(d50);
                    blb.push_byte(b'/');
                    blb.push_u64(d95);
                    blb.push_byte(b'/');
                    blb.push_u64(d99);
                    blb.push_bytes(b" pres1w=");
                    blb.push_u64(p1_50);
                    blb.push_byte(b'/');
                    blb.push_u64(p1_95);
                    blb.push_byte(b'/');
                    blb.push_u64(p1_99);
                    blb.push_bytes(b" pres2w=");
                    blb.push_u64(p2_50);
                    blb.push_byte(b'/');
                    blb.push_u64(p2_95);
                    blb.push_byte(b'/');
                    blb.push_u64(p2_99);
                    log_line(&blb);
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
