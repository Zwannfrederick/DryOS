// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Window Manager Supervisor Control & Telemetry IPC Handlers (`Milestone 2A-2 / 2A-3`).
//!
//! Isolates `BADGE_SUPERVISOR`-gated administrative IPC operations (`WM_BIND_SUP_NOTIF`,
//! `WM_POLL_ACTION`, `WM_FOCUS_OWNER`, `WM_TOGGLE_TILING`, `WM_QUERY_STATE`, and shutdown
//! telemetry emission) from `dry_display.rs` so each module stays well below `< 800` lines.

use crate::action::LatencyRing;
use crate::compositor::DesktopCompositor;
use crate::gfx::Rect;
use crate::shell::LineBuilder;
use crate::wm::{LayoutMode, WindowManager, TOPBAR_H};
use crate::{
    pack_msg_tag, sys_cap_revoke, sys_ipc_reply_inline, sys_write_debug, UserIpcRecvExt,
    BADGE_INPUT_SVC, BADGE_SUPERVISOR, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_PONG,
    IPC_OP_WIN_EMPTY, IPC_OP_WM_BIND_SUP_NOTIF, IPC_OP_WM_FOCUS_OWNER, IPC_OP_WM_POLL_ACTION,
    IPC_OP_WM_QUERY_STATE, IPC_OP_WM_SET_LAYOUT, IPC_OP_WM_SET_PREFS, IPC_OP_WM_TOGGLE_TILING,
};

pub fn wm_log_line(lb: &LineBuilder) {
    let mut out = [0u8; 196];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
fn reply_err(disp_ep: u64, tok: u64, code: u32) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(disp_ep, tok, pack_msg_tag(code, 0, 1, 0), [0; 4]);
    }
}

/// Handle `WM_TOGGLE_TILING`, `WM_BIND_SUP_NOTIF`, `WM_POLL_ACTION`, `WM_FOCUS_OWNER`, and `WM_QUERY_STATE`.
/// Returns `true` if `op` was handled here.
pub fn handle_wm_admin_op(
    op: u32,
    disp_ep_cap: u64,
    reply_tok: u64,
    mrs: [u64; 4],
    ext: &UserIpcRecvExt,
    wm: &mut WindowManager,
    comp: &mut DesktopCompositor,
) -> bool {
    match op {
        IPC_OP_WM_TOGGLE_TILING => {
            let allowed = ext.sender_badge == BADGE_SUPERVISOR
                || ext.sender_badge == BADGE_INPUT_SVC
                || wm.resolve_owned_slot(ext.sender_badge, 0).is_ok();
            if !allowed {
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            let m = wm.toggle_tiling(&mut comp.scene_dirty);
            let (_, cyc) = comp.flush_all(wm);
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-WM] LAYOUT mode=");
            lb.push_u64(m as u64);
            wm_log_line(&lb);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                    [m as u64, cyc, 0, 0],
                );
            }
            true
        }
        IPC_OP_WM_BIND_SUP_NOTIF => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            if mrs[2] == 1 {
                if wm.sup_wake_cap != 0 {
                    let _ = sys_cap_revoke(wm.sup_wake_cap, 1);
                }
                wm.sup_wake_cap = ext.transferred_cap;
            } else {
                if wm.sup_notif_cap != 0 {
                    let _ = sys_cap_revoke(wm.sup_notif_cap, 1);
                }
                wm.sup_notif_cap = ext.transferred_cap;
                wm.p3_mode = mrs[0] != 0;
                if wm.p3_mode {
                    wm.desktop_mode = true;
                }
                wm.running_apps_mask = (mrs[1] & 0xff) as u8;
                comp.scene_dirty.mark(Rect::new(0, 0, comp.w, TOPBAR_H));
            }
            let (_, cyc) = comp.flush_all(wm);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                    [1, cyc, 0, 0],
                );
            }
            true
        }
        IPC_OP_WM_POLL_ACTION => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            if mrs[1] != 0 {
                wm.set_running_apps_mask((mrs[0] & 0xff) as u8, &mut comp.scene_dirty);
                let _ = comp.flush_all(wm);
            }
            if let Some(act) = wm.action_queue.pop() {
                let words = act.to_words();
                let more = u64::from(!wm.action_queue.is_empty());
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [words[0], words[1], more, 0],
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
            true
        }
        IPC_OP_WM_FOCUS_OWNER => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            if mrs[2] != 0 {
                wm.set_running_apps_mask((mrs[1] & 0xff) as u8, &mut comp.scene_dirty);
            }
            let focused_wid = wm
                .focus_owner_badge(mrs[0], &mut comp.scene_dirty)
                .unwrap_or(0);
            let (_, cyc) = comp.flush_all(wm);
            if focused_wid != 0 {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-WM] FOCUS id=");
                lb.push_u64(focused_wid as u64);
                lb.push_bytes(b" z_raises=");
                lb.push_u64(wm.z_raises);
                lb.push_bytes(b" cur=");
                lb.push_u64(wm.cursor_x as u64);
                lb.push_byte(b',');
                lb.push_u64(wm.cursor_y as u64);
                wm_log_line(&lb);
            }
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                    [focused_wid as u64, wm.active_window_count() as u64, cyc, 0],
                );
            }
            true
        }
        IPC_OP_WM_SET_LAYOUT => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            let Some(target) = LayoutMode::from_u8((mrs[0] & 0xff) as u8) else {
                wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                return true;
            };
            let m = wm.set_layout_mode(target, &mut comp.scene_dirty);
            let (_, cyc) = comp.flush_all(wm);
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-WM] LAYOUT mode=");
            lb.push_u64(m as u64);
            wm_log_line(&lb);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                    [m as u64, cyc, 0, 0],
                );
            }
            true
        }
        IPC_OP_WM_SET_PREFS => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            if (mrs[0] & !0x3) != 0 {
                wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                return true;
            }
            wm.high_contrast = (mrs[0] & 1) != 0;
            wm.cursor_large = (mrs[0] & 2) != 0;
            comp.apply_display_prefs(wm);
            let (px, cyc) = comp.flush_all(wm);
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-WM] PREFS contrast=");
            lb.push_u64(u64::from(wm.high_contrast));
            lb.push_bytes(b" cur_large=");
            lb.push_u64(u64::from(wm.cursor_large));
            lb.push_bytes(b" px=");
            lb.push_u64(px);
            wm_log_line(&lb);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                    [mrs[0] & 0x3, px, cyc, 0],
                );
            }
            true
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
            true
        }
        _ => false,
    }
}

pub fn emit_wm_exit_telemetry(
    wm: &WindowManager,
    in2comp_lat: &LatencyRing,
    cursor_lat: &LatencyRing,
    drag_lat: &LatencyRing,
    pres1w_lat: &LatencyRing,
    pres2w_lat: &LatencyRing,
) {
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
    wm_log_line(&slb);

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
        wm_log_line(&blb);
    }
}
