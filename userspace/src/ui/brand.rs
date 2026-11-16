// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY OS Visual Identity: Iso-Kernel Prism Symbol, Wordmark & Boot/Lock Lockup.

use crate::gfx::Rect;
use crate::glass::theme::ThemeMode;

use super::app_icons::{draw_app_asset_with_pal, map_app_shape};
use super::canvas::{fit_aspect_rect, Canvas, VectorPalette};
use super::vector_assets_apps::{app_shapes, find_app_asset};

#[inline]
const fn remap_symbol_rgb(rgb: u32, mode: ThemeMode, on_accent: bool) -> u32 {
    if on_accent {
        if matches!(mode, ThemeMode::HighContrast) {
            return match rgb {
                0x00FF_FFFF => 0x0000_0000,
                0x00E0_F2FE => 0x0000_0000,
                0x007D_D3FC => 0x000F_172A,
                0x0060_A5FA => 0x001E_293B,
                _ => 0x0000_0000,
            };
        }
        return match rgb {
            0x00FF_FFFF | 0x00E0_F2FE => 0x00FF_FFFF,
            0x007D_D3FC | 0x0060_A5FA => 0x00E0_F2FE,
            0x0038_BDF8 => 0x00BF_DBFE,
            0x0002_84C7 | 0x0025_63EB => 0x001E_3A8A,
            _ => rgb,
        };
    }
    match mode {
        ThemeMode::ObsidianDark => rgb,
        ThemeMode::FrostedDaylight => match rgb {
            0x00FF_FFFF => 0x000F_172A,
            0x00E0_F2FE => 0x001E_40AF,
            0x007D_D3FC => 0x0002_84C7,
            0x0060_A5FA => 0x0025_63EB,
            0x0038_BDF8 => 0x001D_4ED8,
            _ => rgb,
        },
        ThemeMode::HighContrast => match rgb {
            0x00FF_FFFF => 0x00FF_FFFF,
            0x00E0_F2FE => 0x00FF_FFFF,
            0x007D_D3FC => 0x00FF_E600,
            0x0060_A5FA => 0x00FF_E600,
            0x0038_BDF8 => 0x00FF_E600,
            0x0002_84C7 => 0x0038_BDF8,
            _ => rgb,
        },
    }
}

/// Render the DRY OS Iso-Kernel Prism mark with automatic 16px optical switching.
pub fn draw_brand_symbol(c: &mut Canvas, r: Rect, mode: ThemeMode, on_accent: bool) {
    if r.w < 8 || r.h < 8 {
        return;
    }
    let id: &[u8] = if r.w.min(r.h) <= 18 {
        b"dryos-symbol-small-16"
    } else {
        b"dryos-symbol"
    };
    let Some(asset) = find_app_asset(id) else {
        return;
    };
    let fit = fit_aspect_rect(r, asset.vb_w, asset.vb_h);
    let small = fit.w.min(fit.h) <= 20;
    let pal = VectorPalette::from_mode(mode);
    for raw in app_shapes(asset) {
        let mut s = map_app_shape(raw);
        s.rgb_top = remap_symbol_rgb(s.rgb_top, mode, on_accent);
        s.rgb_bot = remap_symbol_rgb(s.rgb_bot, mode, on_accent);
        c.draw_q12_shape(fit, s, pal, small);
    }
}

/// Render the theme-matched `DRY OS` vector wordmark (`240x48` aspect preserved).
pub fn draw_brand_wordmark(c: &mut Canvas, r: Rect, mode: ThemeMode) {
    if r.w < 20 || r.h < 6 {
        return;
    }
    let id: &[u8] = match mode {
        ThemeMode::ObsidianDark => b"dryos-wordmark-dark",
        ThemeMode::FrostedDaylight => b"dryos-wordmark-light",
        ThemeMode::HighContrast => b"dryos-wordmark-hc",
    };
    if let Some(asset) = find_app_asset(id) {
        let _ = draw_app_asset_with_pal(c, r, asset, VectorPalette::from_mode(mode));
    }
}

/// Render the full DRY OS Iso-Kernel Prism + wordmark lockup badge (`320x80` aspect preserved).
pub fn draw_boot_lockup(c: &mut Canvas, r: Rect, mode: ThemeMode) {
    if r.w < 40 || r.h < 12 {
        return;
    }
    if let Some(asset) = find_app_asset(b"dryos-lockup-boot") {
        let fit = draw_app_asset_with_pal(c, r, asset, VectorPalette::from_mode(mode));
        if mode == ThemeMode::HighContrast && fit.w >= 48 {
            let rx = fit.x + (fit.w * 26 + 2048) / 4096;
            let ry = fit.y + (fit.h * 102 + 2048) / 4096;
            let rw = ((fit.w * 4045 + 2048) / 4096).max(4);
            let rh = ((fit.h * 3891 + 2048) / 4096).max(4);
            let rad = ((fit.h * 819 + 2048) / 4096).max(4);
            c.stroke_rrect_aa(Rect::new(rx, ry, rw, rh), rad, 0x00FF_E600, 255);
        }
    }
}
