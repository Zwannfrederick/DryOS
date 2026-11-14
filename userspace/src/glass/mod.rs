// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `DRY Glass` Cohesive Desktop Design System & Material Pipeline (`Project Ascension 3.0`).
//!
//! Pure `no_std`, zero heap, integer-arithmetic desktop visual layer running 100% in Ring 3
//! (`dry-display` + shared `app_kit` for pixel-surface native apps).

pub mod app_kit;
pub mod blur;
pub mod chrome;
pub mod dock_bar;
pub mod icons;
pub mod overlays;
pub mod overview;
pub mod theme;
pub mod typography;
pub mod window_strip;

use crate::action::APP_COUNT;
use crate::gfx::Rect;

pub const DOCK_H: usize = 46;
pub const DOCK_MARGIN_B: usize = 8;
pub const DOCK_CLEARANCE: usize = DOCK_H + DOCK_MARGIN_B + 4;
pub const MAX_GLASS_NOTIFS: usize = 6;
pub const GLASS_CORNER_R: usize = 8;

#[derive(Clone, Copy)]
pub struct GlassNotif {
    pub msg: [u8; 42],
    pub len: u8,
    pub kind: u8, // 0 = info, 1 = ok, 2 = warn
}

impl GlassNotif {
    pub const EMPTY: Self = Self {
        msg: [0; 42],
        len: 0,
        kind: 0,
    };
}

pub struct GlassState {
    pub launcher_open: bool,
    pub launcher_query: [u8; 20],
    pub launcher_len: u8,
    pub launcher_sel: usize,
    pub overview_open: bool,
    pub overview_query: [u8; 24],
    pub overview_len: u8,
    pub overview_sel_win_id: u32,
    pub overview_scroll: usize,
    pub qs_open: bool,
    pub notif_open: bool,
    pub notifs: [GlassNotif; MAX_GLASS_NOTIFS],
    pub notif_len: usize,
    pub notif_unread: u8,
    pub snap_preview: Option<Rect>,
}

impl GlassState {
    pub const fn new() -> Self {
        Self {
            launcher_open: false,
            launcher_query: [0; 20],
            launcher_len: 0,
            launcher_sel: 0,
            overview_open: false,
            overview_query: [0; 24],
            overview_len: 0,
            overview_sel_win_id: 0,
            overview_scroll: 0,
            qs_open: false,
            notif_open: false,
            notifs: [GlassNotif::EMPTY; MAX_GLASS_NOTIFS],
            notif_len: 0,
            notif_unread: 0,
            snap_preview: None,
        }
    }

    #[inline]
    pub const fn any_overlay_open(&self) -> bool {
        self.launcher_open || self.overview_open || self.qs_open || self.notif_open
    }

    pub fn close_overlays(&mut self) {
        self.launcher_open = false;
        self.overview_open = false;
        self.qs_open = false;
        self.notif_open = false;
        self.launcher_len = 0;
        self.launcher_sel = 0;
        self.overview_len = 0;
        self.overview_scroll = 0;
    }

    pub fn push_notif(&mut self, text: &[u8], kind: u8) {
        if self.notif_len == MAX_GLASS_NOTIFS {
            for i in 1..MAX_GLASS_NOTIFS {
                self.notifs[i - 1] = self.notifs[i];
            }
            self.notif_len = MAX_GLASS_NOTIFS - 1;
        }
        let mut entry = GlassNotif::EMPTY;
        let n = text.len().min(entry.msg.len());
        entry.msg[..n].copy_from_slice(&text[..n]);
        entry.len = n as u8;
        entry.kind = kind;
        self.notifs[self.notif_len] = entry;
        self.notif_len += 1;
        self.notif_unread = self.notif_unread.saturating_add(1).min(99);
    }

    pub fn clear_notifs(&mut self) {
        self.notif_len = 0;
        self.notif_unread = 0;
    }

    pub fn clamp_launcher_sel(&mut self, matches: usize) {
        if matches == 0 {
            self.launcher_sel = 0;
        } else if self.launcher_sel >= matches.min(APP_COUNT) {
            self.launcher_sel = matches.min(APP_COUNT) - 1;
        }
    }
}

impl Default for GlassState {
    fn default() -> Self {
        Self::new()
    }
}
