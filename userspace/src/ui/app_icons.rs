// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Custom 21 DRY OS Application Vector Icons (`64x64` Q12 SDF -> AA `Canvas`).

use crate::action::AppId;
use crate::gfx::Rect;
use crate::glass::theme::ThemeMode;

use super::canvas::{
    fit_aspect_rect, Canvas, Q12Shape, VecColorSlot, VecOptical, VecShapeKind, VectorPalette,
};
use super::vector_assets_apps::{
    app_shapes, find_app_asset, ColorSlot, OpticalFlag, ShapeKind, VectorAsset, VectorShape,
};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppIconKind {
    Terminal = 0,
    Files = 1,
    Settings = 2,
    LifeStation = 3,
    Notes = 4,
    Calculator = 5,
    TextEditor = 6,
    SystemMonitor = 7,
    Launcher = 8,
    StorageManager = 9,
    NetworkManager = 10,
    AudioSettings = 11,
    DisplaySettings = 12,
    SecurityVault = 13,
    SoftwareStore = 14,
    UpdateManager = 15,
    DriverManager = 16,
    DeviceManager = 17,
    PowerManager = 18,
    SystemInfo = 19,
    HelpDiagnostics = 20,
}

impl AppIconKind {
    pub const ALL: [Self; 21] = [
        Self::Terminal,
        Self::Files,
        Self::Settings,
        Self::LifeStation,
        Self::Notes,
        Self::Calculator,
        Self::TextEditor,
        Self::SystemMonitor,
        Self::Launcher,
        Self::StorageManager,
        Self::NetworkManager,
        Self::AudioSettings,
        Self::DisplaySettings,
        Self::SecurityVault,
        Self::SoftwareStore,
        Self::UpdateManager,
        Self::DriverManager,
        Self::DeviceManager,
        Self::PowerManager,
        Self::SystemInfo,
        Self::HelpDiagnostics,
    ];

    #[inline]
    pub const fn from_app(app: AppId) -> Self {
        match app {
            AppId::Terminal => Self::Terminal,
            AppId::LifeStation => Self::LifeStation,
            AppId::Files => Self::Files,
            AppId::Settings => Self::Settings,
            AppId::Editor => Self::TextEditor,
            AppId::Calculator => Self::Calculator,
        }
    }

    #[inline]
    pub const fn id(self) -> &'static [u8] {
        match self {
            Self::Terminal => b"terminal",
            Self::Files => b"files",
            Self::Settings => b"settings",
            Self::LifeStation => b"life-station",
            Self::Notes => b"notes",
            Self::Calculator => b"calculator",
            Self::TextEditor => b"text-editor",
            Self::SystemMonitor => b"system-monitor",
            Self::Launcher => b"launcher",
            Self::StorageManager => b"storage-manager",
            Self::NetworkManager => b"network-manager",
            Self::AudioSettings => b"audio-settings",
            Self::DisplaySettings => b"display-settings",
            Self::SecurityVault => b"security-vault",
            Self::SoftwareStore => b"software-store",
            Self::UpdateManager => b"update-manager",
            Self::DriverManager => b"driver-manager",
            Self::DeviceManager => b"device-manager",
            Self::PowerManager => b"power-manager",
            Self::SystemInfo => b"system-info",
            Self::HelpDiagnostics => b"help-diagnostics",
        }
    }

    #[inline]
    pub const fn label(self) -> &'static [u8] {
        match self {
            Self::Terminal => b"Terminal",
            Self::Files => b"Files",
            Self::Settings => b"Settings",
            Self::LifeStation => b"Life Station",
            Self::Notes => b"Notes",
            Self::Calculator => b"Calculator",
            Self::TextEditor => b"Text Editor",
            Self::SystemMonitor => b"Sys Monitor",
            Self::Launcher => b"Launcher",
            Self::StorageManager => b"Storage",
            Self::NetworkManager => b"Network",
            Self::AudioSettings => b"Audio",
            Self::DisplaySettings => b"Display",
            Self::SecurityVault => b"Security",
            Self::SoftwareStore => b"Store",
            Self::UpdateManager => b"Updates",
            Self::DriverManager => b"Drivers",
            Self::DeviceManager => b"Devices",
            Self::PowerManager => b"Power",
            Self::SystemInfo => b"Sys Info",
            Self::HelpDiagnostics => b"Help/Diag",
        }
    }
}

#[inline]
pub(crate) const fn map_app_shape(s: &VectorShape) -> Q12Shape {
    Q12Shape {
        kind: match s.kind {
            ShapeKind::RRect => VecShapeKind::RRect,
            ShapeKind::VGradRRect => VecShapeKind::VGradRRect,
            ShapeKind::Circle => VecShapeKind::Circle,
            ShapeKind::Ring => VecShapeKind::Ring,
            ShapeKind::Line => VecShapeKind::Line,
        },
        optical: match s.optical {
            OpticalFlag::Always => VecOptical::Always,
            OpticalFlag::SmallOnly => VecOptical::SmallOnly,
            OpticalFlag::LargeOnly => VecOptical::LargeOnly,
        },
        color: match s.color {
            ColorSlot::Custom => VecColorSlot::Custom,
            ColorSlot::FgPrimary => VecColorSlot::FgPrimary,
            ColorSlot::FgMuted => VecColorSlot::FgMuted,
            ColorSlot::Accent => VecColorSlot::Accent,
            ColorSlot::Ok => VecColorSlot::Ok,
            ColorSlot::Warn => VecColorSlot::Warn,
            ColorSlot::Danger => VecColorSlot::Danger,
            ColorSlot::BgSurface => VecColorSlot::BgSurface,
            ColorSlot::RimHighlight => VecColorSlot::RimHighlight,
        },
        alpha: s.alpha,
        x: s.x,
        y: s.y,
        w: s.w,
        h: s.h,
        radius: s.radius,
        thick: s.thick,
        rgb_top: s.rgb_top,
        rgb_bot: s.rgb_bot,
    }
}

pub(crate) fn draw_app_asset_with_pal(
    c: &mut Canvas,
    r: Rect,
    asset: &VectorAsset,
    pal: VectorPalette,
) -> Rect {
    let fit = fit_aspect_rect(r, asset.vb_w, asset.vb_h);
    let small = fit.w.min(fit.h) <= 20;
    for s in app_shapes(asset) {
        c.draw_q12_shape(fit, map_app_shape(s), pal, small);
    }
    fit
}

pub fn draw_app_icon_id(c: &mut Canvas, r: Rect, id: &[u8], mode: ThemeMode) {
    if r.w < 8 || r.h < 8 {
        return;
    }
    if let Some(asset) = find_app_asset(id) {
        let fit = draw_app_asset_with_pal(c, r, asset, VectorPalette::from_mode(mode));
        if mode == ThemeMode::HighContrast && fit.w >= 16 {
            let pad_x = (fit.w * 256 + 2048) / 4096;
            let pad_y = (fit.h * 256 + 2048) / 4096;
            let sq_w = ((fit.w * 3584 + 2048) / 4096).max(4);
            let sq_h = ((fit.h * 3584 + 2048) / 4096).max(4);
            let rim = Rect::new(fit.x + pad_x, fit.y + pad_y, sq_w, sq_h);
            c.stroke_rrect_aa(rim, (fit.h / 5).max(3), 0x00FF_E600, 255);
        }
    }
}

#[inline]
pub fn draw_app_icon(c: &mut Canvas, r: Rect, kind: AppIconKind, mode: ThemeMode) {
    draw_app_icon_id(c, r, kind.id(), mode);
}

#[inline]
pub fn draw_app_icon_for_app(c: &mut Canvas, r: Rect, app: AppId, mode: ThemeMode) {
    draw_app_icon(c, r, AppIconKind::from_app(app), mode);
}
