// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Window Manager Supervisor Control & Telemetry IPC Handlers (`Milestone 2A-2 / 2A-3`).
//!
//! Isolates `BADGE_SUPERVISOR`-gated administrative IPC operations (`WM_BIND_SUP_NOTIF`,
//! `WM_POLL_ACTION`, `WM_FOCUS_OWNER`, `WM_TOGGLE_TILING`, `WM_QUERY_STATE`, and shutdown
//! telemetry emission) from `dry_display.rs` so each module stays well below `< 800` lines.

use crate::action::LatencyRing;
use crate::compositor::DesktopCompositor;
use crate::gfx::Rect;
use crate::gpu_proto::{GPU_OP_ATTACH_BACKBUF, GPU_OP_PRESENT_RECT};
use crate::shell::LineBuilder;
use crate::vault_crypto::scrub_bytes;
use crate::vault_proto::{pack_passphrase_mrs, pack_vault_label};
use crate::wm::{LayoutMode, SessionVisualState, WindowManager, TOPBAR_H};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_revoke, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_ipc_reply_inline, sys_write_debug, UserIpcRecvExt,
    BADGE_INPUT_SVC, BADGE_SUPERVISOR, EBADCAP, EREVOKED, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM,
    IPC_OP_PONG, IPC_OP_WIN_EMPTY, IPC_OP_WM_BIND_SUP_NOTIF, IPC_OP_WM_FOCUS_OWNER,
    IPC_OP_WM_POLL_ACTION, IPC_OP_WM_QUERY_STATE, IPC_OP_WM_SET_AUTH_LOCK, IPC_OP_WM_SET_LAYOUT,
    IPC_OP_WM_SET_PREFS, IPC_OP_WM_SET_SESSION_STATE, IPC_OP_WM_TAKE_LOCK_PIN,
    IPC_OP_WM_TOGGLE_TILING, RIGHT_MAP, RIGHT_READ,
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

/// Present dirty compositor bounding box via bound `dry-gpu` endpoint, falling back
/// transparently to direct UEFI GOP blitting if the GPU server faulted or revoked its endpoint.
pub fn try_gpu_present_or_fallback(wm: &mut WindowManager, bbox: Rect) -> Option<(u64, u64)> {
    let gpu_ep = wm.gpu_ep_cap;
    if gpu_ep == 0 {
        return None;
    }
    let cur = (wm.cursor_x as u64 & 0xffff)
        | (((wm.cursor_y as u64) & 0xffff) << 16)
        | (u64::from(wm.cursor_visible) << 32)
        | (u64::from(wm.cursor_large) << 33);
    let flags = u64::from(wm.session_state == SessionVisualState::DisplayBlanked)
        | (u64::from(wm.high_contrast) << 1);
    let mrs = [
        (bbox.x as u64) | ((bbox.y as u64) << 32),
        (bbox.w as u64) | ((bbox.h as u64) << 32),
        cur,
        flags,
    ];
    let reason: &[u8] = match sys_ipc_call_inline(
        gpu_ep,
        pack_msg_tag(GPU_OP_PRESENT_RECT, 0, 4, 0),
        mrs,
        0,
    ) {
        Ok((tag, _, out)) if msg_tag_label(tag) == IPC_OP_PONG => {
            wm.gpu_presents = wm.gpu_presents.wrapping_add(1);
            return Some((out[0], out[1]));
        }
        Ok(_) => b"proto_err",
        Err(err) if err == EREVOKED || err == EBADCAP => b"ep_revoked",
        Err(_) => b"ipc_err",
    };
    let _ = sys_cap_revoke(gpu_ep, 1);
    wm.gpu_ep_cap = 0;
    wm.gpu_fallbacks = wm.gpu_fallbacks.wrapping_add(1);
    let mut flb = LineBuilder::new();
    flb.push_bytes(b"[DRY-WM] GPU_FALLBACK_TO_GOP reason=");
    flb.push_bytes(reason);
    wm_log_line(&flb);
    None
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
            } else if mrs[2] == 2 {
                if wm.gpu_ep_cap != 0 {
                    let _ = sys_cap_revoke(wm.gpu_ep_cap, 1);
                }
                wm.gpu_ep_cap = ext.transferred_cap;
                if wm.gpu_ep_cap != 0 {
                    let need = (comp.w * comp.h * 4).div_ceil(2 * 1024 * 1024).min(2);
                    let mut attach_ok = true;
                    for i in 0..need {
                        let cap = wm.back_caps[i];
                        let ok = cap != 0
                            && sys_ipc_call_with_grant(
                                wm.gpu_ep_cap,
                                GPU_OP_ATTACH_BACKBUF,
                                [i as u64, 0, 0, 0],
                                cap,
                                RIGHT_READ | RIGHT_MAP,
                                0,
                                0,
                            )
                            .is_ok_and(|(t, _, _)| msg_tag_label(t) == IPC_OP_PONG);
                        if !ok {
                            attach_ok = false;
                            break;
                        }
                    }
                    if attach_ok {
                        let mut glb = LineBuilder::new();
                        glb.push_bytes(b"[DRY-WM] GPU_SCANOUT_BOUND back_2m=");
                        glb.push_u64(need as u64);
                        wm_log_line(&glb);
                        comp.scene_dirty.mark(Rect::new(0, 0, comp.w, comp.h));
                    } else {
                        let _ = sys_cap_revoke(wm.gpu_ep_cap, 1);
                        wm.gpu_ep_cap = 0;
                    }
                }
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
        IPC_OP_WM_SET_SESSION_STATE => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            let now_tsc = rdtsc_user();
            // `mrs[0] == u64::MAX` updates `blank_timeout_tsc` (`mrs[1]`) without changing visual state.
            if mrs[0] != u64::MAX {
                let Some(next) = SessionVisualState::from_u8((mrs[0] & 0xff) as u8) else {
                    wm.invalid_req_rejects = wm.invalid_req_rejects.wrapping_add(1);
                    reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                    return true;
                };
                wm.set_session_state(next, now_tsc, &mut comp.scene_dirty);
            } else {
                wm.last_input_tsc = now_tsc;
            }
            if mrs[2] != 0 {
                wm.blank_timeout_tsc = mrs[1];
            }
            let (px, cyc) = comp.flush_all(wm);
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-WM] SESSION state=");
            lb.push_u64(wm.session_state as u64);
            lb.push_bytes(b" blank_tsc=");
            lb.push_u64(wm.blank_timeout_tsc);
            lb.push_bytes(b" px=");
            lb.push_u64(px);
            wm_log_line(&lb);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                    [
                        wm.session_state as u64,
                        px,
                        cyc,
                        wm.locked_blocked_events,
                    ],
                );
            }
            true
        }
        IPC_OP_WM_SET_AUTH_LOCK => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            wm.vault_auth_required = (mrs[0] & 1) != 0;
            wm.vault_auth_failures = mrs[1] as u32;
            if mrs[2] != 0 {
                wm.clear_lock_pin();
            }
            comp.scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
            let (px, cyc) = comp.flush_all(wm);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                    [px, cyc, 0, 0],
                );
            }
            true
        }
        IPC_OP_WM_TAKE_LOCK_PIN => {
            if ext.sender_badge != BADGE_SUPERVISOR {
                wm.unauthorized_rejects = wm.unauthorized_rejects.wrapping_add(1);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_PERM);
                return true;
            }
            let (mut raw_pin, plen) = wm.take_lock_pin();
            comp.scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
            let _ = comp.flush_all(wm);
            if plen == 0 {
                scrub_bytes(&mut raw_pin);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                return true;
            }
            let Ok((_, mut out_mrs)) = pack_passphrase_mrs(&raw_pin[..plen]) else {
                scrub_bytes(&mut raw_pin);
                reply_err(disp_ep_cap, reply_tok, IPC_OP_ERR_INVAL);
                return true;
            };
            scrub_bytes(&mut raw_pin);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    disp_ep_cap,
                    reply_tok,
                    pack_msg_tag(pack_vault_label(IPC_OP_PONG, plen as u16), 0, 4, 0),
                    out_mrs,
                );
            }
            out_mrs.fill(0);
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
