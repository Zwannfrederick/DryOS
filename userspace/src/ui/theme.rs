// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Glass 2.0 UI Toolkit Theme Tokens (Obsidian Dark, Alabaster Light, High Contrast).

use crate::glass::theme::ThemeMode;
use crate::ttf::scale_px;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Neutral = 0,
    Accent = 1,
    Ok = 2,
    Warn = 3,
    Danger = 4,
}

#[derive(Clone, Copy)]
pub struct UiTheme {
    pub mode: ThemeMode,
    pub dpi_pct: u16,
    // Surface elevation hierarchy (canonical 0x00RRGGBB)
    pub bg_app: u32,
    pub bg_sidebar: u32,
    pub bg_card: u32,
    pub bg_card_alt: u32,
    pub bg_input: u32,
    pub bg_hover: u32,
    pub bg_pressed: u32,
    pub bg_selected: u32,
    // Borders & hairlines
    pub border_subtle: u32,
    pub border_strong: u32,
    pub focus_ring: u32,
    // Typography foregrounds (WCAG-AA >= 4.5:1)
    pub fg_primary: u32,
    pub fg_secondary: u32,
    pub fg_muted: u32,
    pub fg_on_accent: u32,
    // Semantic accents & status pills
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_subtle: u32,
    pub accent_fg: u32,
    pub ok: u32,
    pub ok_bg: u32,
    pub ok_fg: u32,
    pub warn: u32,
    pub warn_bg: u32,
    pub warn_fg: u32,
    pub danger: u32,
    pub danger_bg: u32,
    pub danger_fg: u32,
    // DPI-scaled typography & rhythm
    pub title_px: u8,
    pub heading_px: u8,
    pub body_px: u8,
    pub mono_px: u8,
    pub caption_px: u8,
    pub r_sm: usize,
    pub r_md: usize,
    pub r_lg: usize,
    pub sp_xs: usize,
    pub sp_sm: usize,
    pub sp_md: usize,
    pub sp_lg: usize,
    pub ctrl_h: usize,
}

impl UiTheme {
    pub const fn new(mode: ThemeMode, dpi_pct: u16) -> Self {
        let dpi = if dpi_pct >= 185 {
            200
        } else if dpi_pct >= 140 {
            150
        } else if dpi_pct >= 115 {
            125
        } else {
            100
        };
        let title_px = scale_px(18, dpi);
        let heading_px = scale_px(14, dpi);
        let body_px = scale_px(13, dpi);
        let mono_px = scale_px(13, dpi);
        let caption_px = scale_px(11, dpi);
        let sp_xs = ((4 * dpi) / 100) as usize;
        let sp_sm = ((8 * dpi) / 100) as usize;
        let sp_md = ((12 * dpi) / 100) as usize;
        let sp_lg = ((16 * dpi) / 100) as usize;
        let ctrl_h = ((28 * dpi) / 100) as usize;

        match mode {
            ThemeMode::ObsidianDark => Self {
                mode,
                dpi_pct: dpi,
                bg_app: 0x000F_141E,
                bg_sidebar: 0x0013_1926,
                bg_card: 0x0017_1E2E,
                bg_card_alt: 0x001C_2436,
                bg_input: 0x000B_1018,
                bg_hover: 0x0023_2E44,
                bg_pressed: 0x002B_3852,
                bg_selected: 0x001E_3456,
                border_subtle: 0x0026_3147,
                border_strong: 0x003B_4B6B,
                focus_ring: 0x0060_A5FA,
                fg_primary: 0x00F1_F5F9,
                fg_secondary: 0x00CB_D5E1,
                fg_muted: 0x008E_9CB0,
                fg_on_accent: 0x00FF_FFFF,
                accent: 0x0025_63EB,
                accent_hover: 0x001D_4ED8,
                accent_subtle: 0x001A_2E4D,
                accent_fg: 0x0060_A5FA,
                ok: 0x0010_B981,
                ok_bg: 0x0011_2B26,
                ok_fg: 0x0034_D399,
                warn: 0x00D9_7706,
                warn_bg: 0x002D_2111,
                warn_fg: 0x00FB_BF24,
                danger: 0x00DC_2626,
                danger_bg: 0x0033_161A,
                danger_fg: 0x00F8_7171,
                title_px,
                heading_px,
                body_px,
                mono_px,
                caption_px,
                r_sm: 6,
                r_md: 8,
                r_lg: 12,
                sp_xs,
                sp_sm,
                sp_md,
                sp_lg,
                ctrl_h,
            },
            ThemeMode::FrostedDaylight => Self {
                mode,
                dpi_pct: dpi,
                bg_app: 0x00F3_F6FA,
                bg_sidebar: 0x00E9_EEF6,
                bg_card: 0x00FF_FFFF,
                bg_card_alt: 0x00F0_F4FA,
                bg_input: 0x00FF_FFFF,
                bg_hover: 0x00E2_E8F2,
                bg_pressed: 0x00D2_DCED,
                bg_selected: 0x00DB_EAFE,
                border_subtle: 0x00DA_E2EE,
                border_strong: 0x00B0_BECE,
                focus_ring: 0x0025_63EB,
                fg_primary: 0x000F_172A,
                fg_secondary: 0x0033_4155,
                fg_muted: 0x0052_6075,
                fg_on_accent: 0x00FF_FFFF,
                accent: 0x001D_4ED8,
                accent_hover: 0x001E_40AF,
                accent_subtle: 0x00DB_EAFE,
                accent_fg: 0x001D_4ED8,
                ok: 0x0005_9669,
                ok_bg: 0x00D1_FAE5,
                ok_fg: 0x0004_7857,
                warn: 0x00B4_5309,
                warn_bg: 0x00FE_F3C7,
                warn_fg: 0x0092_400E,
                danger: 0x00B9_1C1C,
                danger_bg: 0x00FE_E2E2,
                danger_fg: 0x0099_1B1B,
                title_px,
                heading_px,
                body_px,
                mono_px,
                caption_px,
                r_sm: 6,
                r_md: 8,
                r_lg: 12,
                sp_xs,
                sp_sm,
                sp_md,
                sp_lg,
                ctrl_h,
            },
            ThemeMode::HighContrast => Self {
                mode,
                dpi_pct: dpi,
                bg_app: 0x0000_0000,
                bg_sidebar: 0x0008_0A0F,
                bg_card: 0x000C_1018,
                bg_card_alt: 0x0016_1B26,
                bg_input: 0x0000_0000,
                bg_hover: 0x001F_2937,
                bg_pressed: 0x0037_4151,
                bg_selected: 0x001E_3A8A,
                border_subtle: 0x0064_748B,
                border_strong: 0x00FF_FFFF,
                focus_ring: 0x00FF_E600,
                fg_primary: 0x00FF_FFFF,
                fg_secondary: 0x00F1_F5F9,
                fg_muted: 0x00CB_D5E1,
                fg_on_accent: 0x0000_0000,
                accent: 0x00FF_E600,
                accent_hover: 0x00FE_F08A,
                accent_subtle: 0x002A_2500,
                accent_fg: 0x00FF_E600,
                ok: 0x0000_FF88,
                ok_bg: 0x0000_2E18,
                ok_fg: 0x004A_DE80,
                warn: 0x00FF_B703,
                warn_bg: 0x0033_2200,
                warn_fg: 0x00FD_E047,
                danger: 0x00FF_4D4D,
                danger_bg: 0x003B_0D0D,
                danger_fg: 0x00FC_A5A5,
                title_px,
                heading_px,
                body_px,
                mono_px,
                caption_px,
                r_sm: 6,
                r_md: 8,
                r_lg: 10,
                sp_xs,
                sp_sm,
                sp_md,
                sp_lg,
                ctrl_h,
            },
        }
    }

    #[inline]
    pub const fn status_colors(&self, kind: StatusKind) -> (u32, u32) {
        match kind {
            StatusKind::Neutral => (self.bg_card_alt, self.fg_secondary),
            StatusKind::Accent => (self.accent_subtle, self.accent_fg),
            StatusKind::Ok => (self.ok_bg, self.ok_fg),
            StatusKind::Warn => (self.warn_bg, self.warn_fg),
            StatusKind::Danger => (self.danger_bg, self.danger_fg),
        }
    }
}
