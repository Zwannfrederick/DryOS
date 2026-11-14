// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Glass Visual System — Theme Tokens & Color Helpers (`Obsidian Dark`, `Frosted Daylight`, `High Contrast`).

use crate::abi_ext::UserFbInfo;
use crate::compositor::pack_rgb;
use crate::ui::theme::UiTheme;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ThemeMode {
    ObsidianDark = 0,
    FrostedDaylight = 1,
    HighContrast = 2,
}

impl ThemeMode {
    #[inline]
    pub const fn from_wm(high_contrast: bool, light_mode: bool) -> Self {
        if high_contrast {
            Self::HighContrast
        } else if light_mode {
            Self::FrostedDaylight
        } else {
            Self::ObsidianDark
        }
    }
}

#[derive(Copy, Clone)]
pub struct GlassTokens {
    pub mode: ThemeMode,
    pub wall_top_rgb: (u8, u8, u8),
    pub wall_bot_rgb: (u8, u8, u8),
    pub wall_glow_rgb: (u8, u8, u8),
    pub dot_grid: u32,
    pub panel_tint_rgb: (u8, u8, u8),
    pub panel_alpha: u8,
    pub dock_tint_rgb: (u8, u8, u8),
    pub dock_alpha: u8,
    pub overlay_tint_rgb: (u8, u8, u8),
    pub overlay_alpha: u8,
    pub glass_rim: u32,
    pub glass_rim_soft: u32,
    pub title_focus_rgb: (u8, u8, u8),
    pub title_idle_rgb: (u8, u8, u8),
    pub pill_bg: u32,
    pub pill_active: u32,
    pub card_bg: u32,
    pub card_alt: u32,
    pub border_focus: u32,
    pub border_idle: u32,
    pub fg_primary: u32,
    pub fg_muted: u32,
    pub accent: u32,
    pub ok: u32,
    pub warn: u32,
    pub close_bg: u32,
    pub close_fg: u32,
    pub shadow_rgb: (u8, u8, u8),
}

impl GlassTokens {
    pub const fn zeroed() -> Self {
        Self {
            mode: ThemeMode::ObsidianDark,
            wall_top_rgb: (0, 0, 0),
            wall_bot_rgb: (0, 0, 0),
            wall_glow_rgb: (0, 0, 0),
            dot_grid: 0,
            panel_tint_rgb: (0, 0, 0),
            panel_alpha: 0,
            dock_tint_rgb: (0, 0, 0),
            dock_alpha: 0,
            overlay_tint_rgb: (0, 0, 0),
            overlay_alpha: 0,
            glass_rim: 0,
            glass_rim_soft: 0,
            title_focus_rgb: (0, 0, 0),
            title_idle_rgb: (0, 0, 0),
            pill_bg: 0,
            pill_active: 0,
            card_bg: 0,
            card_alt: 0,
            border_focus: 0,
            border_idle: 0,
            fg_primary: 0,
            fg_muted: 0,
            accent: 0,
            ok: 0,
            warn: 0,
            close_bg: 0,
            close_fg: 0,
            shadow_rgb: (0, 0, 0),
        }
    }

    #[inline]
    pub fn ui_theme(&self, dpi_pct: u16) -> UiTheme {
        UiTheme::new(self.mode, dpi_pct)
    }

    pub fn from_fb(info: &UserFbInfo, mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::ObsidianDark => Self {
                mode,
                wall_top_rgb: (11, 15, 24),
                wall_bot_rgb: (19, 28, 43),
                wall_glow_rgb: (34, 72, 118),
                dot_grid: pack_rgb(info, 28, 36, 52),
                panel_tint_rgb: (17, 22, 32),
                panel_alpha: 204,
                dock_tint_rgb: (20, 26, 38),
                dock_alpha: 212,
                overlay_tint_rgb: (18, 24, 35),
                overlay_alpha: 226,
                glass_rim: pack_rgb(info, 70, 84, 110),
                glass_rim_soft: pack_rgb(info, 44, 54, 72),
                title_focus_rgb: (27, 34, 48),
                title_idle_rgb: (20, 25, 35),
                pill_bg: pack_rgb(info, 32, 40, 56),
                pill_active: pack_rgb(info, 44, 96, 176),
                card_bg: pack_rgb(info, 24, 30, 42),
                card_alt: pack_rgb(info, 31, 39, 54),
                border_focus: pack_rgb(info, 76, 134, 214),
                border_idle: pack_rgb(info, 42, 51, 67),
                fg_primary: pack_rgb(info, 238, 242, 248),
                fg_muted: pack_rgb(info, 152, 163, 182),
                accent: pack_rgb(info, 96, 165, 250),
                ok: pack_rgb(info, 52, 211, 153),
                warn: pack_rgb(info, 245, 158, 11),
                close_bg: pack_rgb(info, 200, 62, 68),
                close_fg: pack_rgb(info, 255, 255, 255),
                shadow_rgb: (4, 7, 13),
            },
            ThemeMode::FrostedDaylight => Self {
                mode,
                wall_top_rgb: (224, 233, 244),
                wall_bot_rgb: (202, 216, 235),
                wall_glow_rgb: (176, 202, 236),
                dot_grid: pack_rgb(info, 192, 204, 220),
                panel_tint_rgb: (244, 247, 252),
                panel_alpha: 218,
                dock_tint_rgb: (240, 244, 250),
                dock_alpha: 224,
                overlay_tint_rgb: (246, 249, 253),
                overlay_alpha: 234,
                glass_rim: pack_rgb(info, 255, 255, 255),
                glass_rim_soft: pack_rgb(info, 198, 209, 224),
                title_focus_rgb: (234, 240, 248),
                title_idle_rgb: (222, 229, 239),
                pill_bg: pack_rgb(info, 225, 232, 242),
                pill_active: pack_rgb(info, 37, 99, 235),
                card_bg: pack_rgb(info, 248, 250, 253),
                card_alt: pack_rgb(info, 234, 240, 248),
                border_focus: pack_rgb(info, 59, 130, 246),
                border_idle: pack_rgb(info, 196, 206, 220),
                fg_primary: pack_rgb(info, 15, 23, 42),
                fg_muted: pack_rgb(info, 71, 85, 105),
                accent: pack_rgb(info, 29, 78, 216),
                ok: pack_rgb(info, 5, 150, 105),
                warn: pack_rgb(info, 180, 83, 9),
                close_bg: pack_rgb(info, 220, 38, 38),
                close_fg: pack_rgb(info, 255, 255, 255),
                shadow_rgb: (15, 23, 42),
            },
            ThemeMode::HighContrast => Self {
                mode,
                wall_top_rgb: (8, 10, 14),
                wall_bot_rgb: (14, 18, 26),
                wall_glow_rgb: (22, 42, 68),
                dot_grid: pack_rgb(info, 38, 46, 62),
                panel_tint_rgb: (10, 12, 18),
                panel_alpha: 242,
                dock_tint_rgb: (12, 15, 22),
                dock_alpha: 244,
                overlay_tint_rgb: (12, 15, 22),
                overlay_alpha: 248,
                glass_rim: pack_rgb(info, 250, 204, 21),
                glass_rim_soft: pack_rgb(info, 148, 163, 184),
                title_focus_rgb: (22, 28, 40),
                title_idle_rgb: (14, 18, 26),
                pill_bg: pack_rgb(info, 28, 34, 48),
                pill_active: pack_rgb(info, 202, 138, 4),
                card_bg: pack_rgb(info, 18, 22, 30),
                card_alt: pack_rgb(info, 30, 36, 48),
                border_focus: pack_rgb(info, 250, 204, 21),
                border_idle: pack_rgb(info, 148, 163, 184),
                fg_primary: pack_rgb(info, 255, 255, 255),
                fg_muted: pack_rgb(info, 226, 232, 240),
                accent: pack_rgb(info, 250, 204, 21),
                ok: pack_rgb(info, 74, 222, 128),
                warn: pack_rgb(info, 251, 146, 60),
                close_bg: pack_rgb(info, 220, 38, 38),
                close_fg: pack_rgb(info, 255, 255, 255),
                shadow_rgb: (0, 0, 0),
            },
        }
    }
}

#[inline]
pub fn unpack_rgb(info: &UserFbInfo, px: u32) -> (u8, u8, u8) {
    (
        ((px >> info.red_shift) & 0xff) as u8,
        ((px >> info.green_shift) & 0xff) as u8,
        ((px >> info.blue_shift) & 0xff) as u8,
    )
}

#[inline]
pub fn blend_rgb(dst: (u8, u8, u8), src: (u8, u8, u8), alpha: u8) -> (u8, u8, u8) {
    let a = alpha as u16;
    let inv = 255u16 - a;
    (
        ((dst.0 as u16 * inv + src.0 as u16 * a) / 255) as u8,
        ((dst.1 as u16 * inv + src.1 as u16 * a) / 255) as u8,
        ((dst.2 as u16 * inv + src.2 as u16 * a) / 255) as u8,
    )
}

#[inline]
pub fn blend_packed(info: &UserFbInfo, dst_px: u32, src: (u8, u8, u8), alpha: u8) -> u32 {
    if alpha == 0 {
        return dst_px;
    }
    if alpha == 255 {
        return pack_rgb(info, src.0, src.1, src.2);
    }
    let dst = unpack_rgb(info, dst_px);
    let out = blend_rgb(dst, src, alpha);
    pack_rgb(info, out.0, out.1, out.2)
}
