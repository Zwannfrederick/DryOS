// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure `#![no_std]` scalable TrueType typography engine (`DrySans` + `DryMono`, SIL OFL 1.1).

pub mod atlas;
pub mod layout;
pub mod raster;
pub mod sfnt;

pub use atlas::{atlas_stats, font_metrics, glyph_advance_24_8, FontId, FontMetrics};
pub use layout::{
    byte_index_at_x, div255, draw_text_baseline_raw, draw_text_ellipsis_raw,
    draw_text_selected_raw, draw_text_top_raw, measure_text, x_at_byte_index, PixelFormat,
    Utf8BytesIter,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextStyle {
    pub font: FontId,
    pub size_px: u8,
    pub color_rgb: u32,
}

impl TextStyle {
    pub const fn new(font: FontId, size_px: u8, color_rgb: u32) -> Self {
        Self {
            font,
            size_px,
            color_rgb,
        }
    }
}

/// Scale nominal UI font size cleanly for High-DPI percentage (`100`, `125`, `150`).
#[inline]
pub const fn scale_px(base_px: u8, dpi_pct: u16) -> u8 {
    let scaled = ((base_px as u16) * dpi_pct + 50) / 100;
    if scaled < 8 {
        8
    } else if scaled > 64 {
        64
    } else {
        scaled as u8
    }
}
