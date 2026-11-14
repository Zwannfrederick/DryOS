// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 native application window helper & redesigned app UIs.

pub mod calc_ui;
pub mod editor_ui;
pub mod files_ui;
pub mod settings_ui;
pub mod shell_ui;
pub mod sysmon_ui;

use core::slice::from_raw_parts_mut;
use core::sync::atomic::Ordering;

use crate::input::{InputEvent, InputEventKind, MOUSE_BTN_LEFT};
use crate::shell::LineBuilder;
use crate::sup_services::SupervisorPrefs;
use crate::surface::{
    clamp_pixel_dims, init_pixel_surface_rw, publish_pixel_rect_rw, publish_pixel_resize_rw,
    SharedPixelSurfaceHeader, PIXEL_SURF_HEADER_BYTES, SURF_ATTACH_FLAG_PIXEL,
};
use crate::ttf::PixelFormat;
use crate::ui::canvas::Canvas;
use crate::ui::layout::UiPersist;
use crate::glass::theme::ThemeMode;
use crate::ui::theme::UiTheme;
use crate::wm_types::WindowRole;
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_revoke, sys_frame_alloc, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_sysinfo, sys_vspace_map, sys_write_debug, UserSysInfo,
    IPC_OP_PONG, IPC_OP_SUP_QUERY_PREFS, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_PRESENT,
    IPC_OP_WIN_BIND_NOTIF, IPC_OP_WIN_CLOSE, RIGHT_MAP, RIGHT_READ, RIGHT_SIGNAL,
    VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_UNMAP_2M,
};

#[inline]
pub fn decode_theme(flags: u8) -> (ThemeMode, u16) {
    let mode = if (flags & 1) != 0 {
        ThemeMode::HighContrast
    } else if (flags & 4) != 0 {
        ThemeMode::FrostedDaylight
    } else {
        ThemeMode::ObsidianDark
    };
    let dpi = match (flags >> 4) & 3 {
        1 => 125,
        2 => 150,
        _ => 100,
    };
    (mode, dpi)
}

pub fn app_log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

pub fn app_poll_tsc(poll_us: u64) -> u64 {
    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    poll_us.saturating_mul(sys.tsc_mhz.max(1000))
}

#[derive(Clone, Copy, Default)]
pub struct PointerStep {
    pub clicked: bool,
    pub released: bool,
    pub scroll_dy: i16,
    pub redraw: bool,
}

/// Shared `Frame2M`-backed pixel window lifecycle, resize, theme/DPI sync & pointer tracking.
pub struct PixelAppWindow {
    pub vaddr: u64,
    pub surf_cap: u64,
    pub win_id: u32,
    pub w: u16,
    pub h: u16,
    pub theme_flags: u8,
    pub persist: UiPersist,
}

impl PixelAppWindow {
    pub fn query_theme_flags(sup_ep: u64) -> u8 {
        if sup_ep == 0 {
            return 0;
        }
        if let Ok((rtag, _, rmrs)) = sys_ipc_call_inline(
            sup_ep,
            pack_msg_tag(IPC_OP_SUP_QUERY_PREFS, 0, 1, 0),
            [0; 4],
            0,
        ) {
            if msg_tag_label(rtag) == IPC_OP_PONG {
                return SupervisorPrefs::unpack_mr0(rmrs[0]).theme_flags & 0x3f;
            }
        }
        0
    }

    #[inline]
    pub fn theme(&self) -> UiTheme {
        let (mode, dpi) = decode_theme(self.theme_flags);
        UiTheme::new(mode, dpi)
    }

    pub fn alloc_and_map(vaddr: u64, w: u16, h: u16, theme_flags: u8) -> Result<Self, isize> {
        let (cw, ch) = clamp_pixel_dims(w as usize, h as usize);
        let cap = sys_frame_alloc(1)?;
        if let Err(err) = sys_vspace_map(cap, vaddr, VSPACE_MAP_OP_2M_RW) {
            let _ = sys_cap_revoke(cap, 2);
            return Err(err);
        }
        // SAFETY: `vaddr` is a freshly allocated & mapped 2 MiB `USER_RW` frame with valid dimensions.
        unsafe { init_pixel_surface_rw(vaddr, cw, ch) };
        Ok(Self {
            vaddr,
            surf_cap: cap,
            win_id: 0,
            w: cw,
            h: ch,
            theme_flags: theme_flags & 0x3f,
            persist: UiPersist::new(),
        })
    }

    pub fn canvas(&mut self) -> Canvas {
        let len = (self.w as usize).saturating_mul(self.h as usize);
        // SAFETY: Pixel plane begins immediately after the 64-byte header inside the mapped 2 MiB frame.
        let pix = unsafe {
            from_raw_parts_mut((self.vaddr + PIXEL_SURF_HEADER_BYTES as u64) as *mut u32, len)
        };
        let mut c = Canvas::from_slice(pix, self.w as usize, self.h as usize);
        c.fmt = PixelFormat::RGB888;
        c
    }

    #[inline]
    pub fn publish_initial(&self) {
        // SAFETY: `self.vaddr` is mapped RW and covers `(self.w, self.h)`.
        unsafe { publish_pixel_rect_rw(self.vaddr, 0, 0, self.w, self.h) };
    }

    #[allow(clippy::too_many_arguments)]
    pub fn attach(
        &mut self,
        disp_ep: u64,
        notif_cap: u64,
        req_x: u64,
        req_y: u64,
        role: WindowRole,
        title_kind: u8,
        extra_flags: u64,
    ) -> u32 {
        if disp_ep == 0 {
            return 0;
        }
        let attach_mrs = [
            (self.w as u64) | ((self.h as u64) << 16),
            req_x | (req_y << 32),
            (role as u64)
                | ((title_kind as u64) << 8)
                | SURF_ATTACH_FLAG_PIXEL
                | extra_flags,
            0,
        ];
        if let Ok((rtag, _, rmrs)) = sys_ipc_call_with_grant(
            disp_ep,
            IPC_OP_SURFACE_ATTACH,
            attach_mrs,
            self.surf_cap,
            RIGHT_READ | RIGHT_MAP,
            0,
            0,
        ) {
            if msg_tag_label(rtag) == IPC_OP_PONG {
                self.win_id = rmrs[0] as u32;
            }
        }
        if self.win_id != 0 && notif_cap != 0 {
            let _ = sys_ipc_call_with_grant(
                disp_ep,
                IPC_OP_WIN_BIND_NOTIF,
                [self.win_id as u64, 0, 0, 0],
                notif_cap,
                RIGHT_READ | RIGHT_SIGNAL,
                0,
                0,
            );
        }
        self.win_id
    }

    pub fn handle_window_config(&mut self, ev: InputEvent) -> bool {
        if ev.kind != InputEventKind::WindowConfig {
            return false;
        }
        let (nw, nh) = clamp_pixel_dims(ev.dx.max(160) as usize, ev.dy.max(120) as usize);
        let nf = ev.modifiers & 0x3f;
        let resized = nw != self.w || nh != self.h;
        let flags_changed = nf != self.theme_flags;
        self.theme_flags = nf;
        if resized {
            self.w = nw;
            self.h = nh;
            // SAFETY: `self.vaddr` is a mapped 2 MiB `SharedPixelSurface` and `(nw, nh)` is clamped.
            unsafe { publish_pixel_resize_rw(self.vaddr, nw, nh) };
        }
        resized || flags_changed
    }

    pub fn ingest_pointer(&mut self, ev: InputEvent) -> PointerStep {
        match ev.kind {
            InputEventKind::MouseMove => {
                let was_hover = self.persist.hovered != 0;
                self.persist.mx = ev.dx as i32;
                self.persist.my = ev.dy as i32;
                let down_now = (ev.buttons & MOUSE_BTN_LEFT) != 0;
                let btn_changed = down_now != self.persist.mouse_down;
                self.persist.mouse_down = down_now;
                let left_window = ev.dx < 0 || ev.dy < 0;
                PointerStep {
                    redraw: was_hover || btn_changed || !left_window,
                    ..PointerStep::default()
                }
            }
            InputEventKind::MouseButtonDown => {
                self.persist.mx = ev.dx as i32;
                self.persist.my = ev.dy as i32;
                let is_left = (ev.scancode as u8) == MOUSE_BTN_LEFT;
                if is_left {
                    self.persist.mouse_down = true;
                }
                PointerStep {
                    clicked: is_left,
                    redraw: is_left,
                    ..PointerStep::default()
                }
            }
            InputEventKind::MouseButtonUp => {
                self.persist.mx = ev.dx as i32;
                self.persist.my = ev.dy as i32;
                let is_left = (ev.scancode as u8) == MOUSE_BTN_LEFT;
                if is_left {
                    self.persist.mouse_down = false;
                }
                PointerStep {
                    released: is_left,
                    redraw: is_left,
                    ..PointerStep::default()
                }
            }
            InputEventKind::MouseScroll => PointerStep {
                scroll_dy: ev.wheel as i16,
                redraw: ev.wheel != 0,
                ..PointerStep::default()
            },
            _ => PointerStep::default(),
        }
    }

    pub fn sync_prefs(&mut self, sup_ep: u64) -> bool {
        let nf = Self::query_theme_flags(sup_ep);
        if nf != self.theme_flags {
            self.theme_flags = nf;
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn current_seq(&self) -> u32 {
        // SAFETY: `self.vaddr` holds an initialized `SharedPixelSurfaceHeader`.
        let hdr = unsafe { &*(self.vaddr as *const SharedPixelSurfaceHeader) };
        hdr.seq.load(Ordering::Relaxed)
    }

    pub fn present_full(&self, disp_ep: u64) -> (u32, u64, u64) {
        // SAFETY: `self.vaddr` remains mapped RW until `teardown`.
        unsafe { publish_pixel_rect_rw(self.vaddr, 0, 0, self.w, self.h) };
        let seq = self.current_seq();
        if disp_ep == 0 || self.win_id == 0 {
            return (seq, 0, 0);
        }
        match sys_ipc_call_inline(
            disp_ep,
            pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
            [self.win_id as u64, 0, 0, 0],
            0,
        ) {
            Ok((_, _, mrs)) => (seq, mrs[0], mrs[1]),
            Err(_) => (seq, 0, 0),
        }
    }

    pub fn teardown(self, disp_ep: u64) {
        if disp_ep != 0 && self.win_id != 0 {
            let _ = sys_ipc_call_inline(
                disp_ep,
                pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
                [self.win_id as u64, 0, 0, 0],
                0,
            );
        }
        let _ = sys_vspace_map(self.surf_cap, self.vaddr, VSPACE_MAP_OP_UNMAP_2M);
        let _ = sys_cap_revoke(self.surf_cap, 2);
    }
}
