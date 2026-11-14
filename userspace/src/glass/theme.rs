// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `DRY Glass` Color Tokens & WCAG-AA Dark / Frosted Daylight / High-Contrast Palettes.

use crate::compositor::pack_rgb;
use crate::UserFbInfo;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy)]
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
            panel_alpha: 200,
            dock_tint_rgb: (0, 0, 0),
            dock_alpha: 200,
            overlay_tint_rgb: (0, 0, 0),
            overlay_alpha: 220,
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

    pub fn from_fb(info: &UserFbInfo, mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::HighContrast => Self {
                mode,
                wall_top_rgb: (0x00, 0x00, 0x00),
                wall_bot_rgb: (0x00, 0x00, 0x00),
                wall_glow_rgb: (0x00, 0x10, 0x14),
                dot_grid: pack_rgb(info, 0x26, 0x26, 0x26),
                panel_tint_rgb: (0x00, 0x00, 0x00),
                panel_alpha: 255,
                dock_tint_rgb: (0x08, 0x08, 0x08),
                dock_alpha: 255,
                overlay_tint_rgb: (0x04, 0x06, 0x0A),
                overlay_alpha: 255,
                glass_rim: pack_rgb(info, 0x00, 0xFF, 0xEE),
                glass_rim_soft: pack_rgb(info, 0x94, 0xA3, 0xB8),
                title_focus_rgb: (0x00, 0x3B, 0x4D),
                title_idle_rgb: (0x14, 0x14, 0x14),
                pill_bg: pack_rgb(info, 0x12, 0x24, 0x2E),
                pill_active: pack_rgb(info, 0x00, 0x52, 0x66),
                card_bg: pack_rgb(info, 0x08, 0x0C, 0x12),
                card_alt: pack_rgb(info, 0x14, 0x1E, 0x28),
                border_focus: pack_rgb(info, 0x00, 0xFF, 0xEE),
                border_idle: pack_rgb(info, 0x94, 0xA3, 0xB8),
                fg_primary: pack_rgb(info, 0xFF, 0xFF, 0xFF),
                fg_muted: pack_rgb(info, 0xD8, 0xE2, 0xEC),
                accent: pack_rgb(info, 0x00, 0xFF, 0xEE),
                ok: pack_rgb(info, 0x39, 0xFF, 0x88),
                warn: pack_rgb(info, 0xFF, 0xEA, 0x00),
                close_bg: pack_rgb(info, 0x88, 0x00, 0x1E),
                close_fg: pack_rgb(info, 0xFF, 0xFF, 0xFF),
                shadow_rgb: (0x00, 0x00, 0x00),
            },
            ThemeMode::FrostedDaylight => Self {
                mode,
                wall_top_rgb: (0xDC, 0xE9, 0xF7),
                wall_bot_rgb: (0xBF, 0xD4, 0xEC),
                wall_glow_rgb: (0x8F, 0xC9, 0xF2),
                dot_grid: pack_rgb(info, 0xA4, 0xBC, 0xD6),
                panel_tint_rgb: (0xF6, 0xFA, 0xFE),
                panel_alpha: 214,
                dock_tint_rgb: (0xF8, 0xFA, 0xFC),
                dock_alpha: 218,
                overlay_tint_rgb: (0xF4, 0xF8, 0xFD),
                overlay_alpha: 228,
                glass_rim: pack_rgb(info, 0xFF, 0xFF, 0xFF),
                glass_rim_soft: pack_rgb(info, 0xA8, 0xBD, 0xD4),
                title_focus_rgb: (0xE0, 0xEF, 0xFD),
                title_idle_rgb: (0xEC, 0xF1, 0xF7),
                pill_bg: pack_rgb(info, 0xE2, 0xEC, 0xF7),
                pill_active: pack_rgb(info, 0xBA, 0xE0, 0xFD),
                card_bg: pack_rgb(info, 0xE9, 0xF1, 0xFA),
                card_alt: pack_rgb(info, 0xD8, 0xE7, 0xF6),
                border_focus: pack_rgb(info, 0x02, 0x84, 0xC7),
                border_idle: pack_rgb(info, 0x94, 0xA8, 0xC0),
                fg_primary: pack_rgb(info, 0x0F, 0x17, 0x2A),
                fg_muted: pack_rgb(info, 0x47, 0x55, 0x69),
                accent: pack_rgb(info, 0x03, 0x69, 0xA1),
                ok: pack_rgb(info, 0x04, 0x78, 0x57),
                warn: pack_rgb(info, 0xB4, 0x53, 0x09),
                close_bg: pack_rgb(info, 0xFE, 0xE2, 0xE2),
                close_fg: pack_rgb(info, 0xB9, 0x1C, 0x1C),
                shadow_rgb: (0x1E, 0x29, 0x3B),
            },
            ThemeMode::ObsidianDark => Self {
                mode,
                wall_top_rgb: (0x08, 0x0D, 0x19),
                wall_bot_rgb: (0x12, 0x1F, 0x34),
                wall_glow_rgb: (0x13, 0x3D, 0x5E),
                dot_grid: pack_rgb(info, 0x1D, 0x2E, 0x49),
                panel_tint_rgb: (0x0D, 0x15, 0x26),
                panel_alpha: 206,
                dock_tint_rgb: (0x10, 0x1B, 0x30),
                dock_alpha: 200,
                overlay_tint_rgb: (0x0F, 0x18, 0x2B),
                overlay_alpha: 222,
                glass_rim: pack_rgb(info, 0x48, 0x68, 0x8E),
                glass_rim_soft: pack_rgb(info, 0x27, 0x39, 0x54),
                title_focus_rgb: (0x16, 0x26, 0x40),
                title_idle_rgb: (0x11, 0x1A, 0x2C),
                pill_bg: pack_rgb(info, 0x19, 0x28, 0x42),
                pill_active: pack_rgb(info, 0x1B, 0x38, 0x58),
                card_bg: pack_rgb(info, 0x13, 0x1E, 0x32),
                card_alt: pack_rgb(info, 0x1B, 0x2B, 0x46),
                border_focus: pack_rgb(info, 0x22, 0xD3, 0xEE),
                border_idle: pack_rgb(info, 0x30, 0x42, 0x5C),
                fg_primary: pack_rgb(info, 0xF8, 0xFA, 0xFC),
                fg_muted: pack_rgb(info, 0x94, 0xA3, 0xB8),
                accent: pack_rgb(info, 0x22, 0xD3, 0xEE),
                ok: pack_rgb(info, 0x34, 0xD3, 0x99),
                warn: pack_rgb(info, 0xFB, 0xBF, 0x24),
                close_bg: pack_rgb(info, 0x52, 0x1B, 0x2B),
                close_fg: pack_rgb(info, 0xFD, 0xA4, 0xAF),
                shadow_rgb: (0x02, 0x05, 0x0C),
            },
        }
    }
}

#[inline(always)]
pub fn unpack_rgb(info: &UserFbInfo, px: u32) -> (u8, u8, u8) {
    let r = ((px >> (info.red_shift & 31)) & 0xff) as u8;
    let g = ((px >> (info.green_shift & 31)) & 0xff) as u8;
    let b = ((px >> (info.blue_shift & 31)) & 0xff) as u8;
    (r, g, b)
}

#[inline(always)]
pub fn blend_rgb(dst: (u8, u8, u8), src: (u8, u8, u8), alpha: u8) -> (u8, u8, u8) {
    let a = alpha as u16;
    let inv = 255u16 - a;
    let r = (((src.0 as u16) * a + (dst.0 as u16) * inv + 127) / 255) as u8;
    let g = (((src.1 as u16) * a + (dst.1 as u16) * inv + 127) / 255) as u8;
    let b = (((src.2 as u16) * a + (dst.2 as u16) * inv + 127) / 255) as u8;
    (r, g, b)
}

#[inline(always)]
pub fn blend_packed(info: &UserFbInfo, dst_px: u32, src: (u8, u8, u8), alpha: u8) -> u32 {
    if alpha == 255 {
        return pack_rgb(info, src.0, src.1, src.2);
    }
    if alpha == 0 {
        return dst_px;
    }
    let d = unpack_rgb(info, dst_px);
    let m = blend_rgb(d, src, alpha);
    pack_rgb(info, m.0, m.1, m.2)
}
