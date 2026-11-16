// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Vector system iconography rendered from compiled SVG Q12 SDF tables via `Canvas`.

use crate::action::AppId;
use crate::gfx::Rect;
use crate::glass::theme::ThemeMode;

use super::app_icons::{draw_app_icon, AppIconKind};
use super::canvas::{
    fit_aspect_rect, Canvas, Q12Shape, VecColorSlot, VecOptical, VecShapeKind, VectorPalette,
};
use super::theme::UiTheme;
use super::vector_assets_sys::{
    find_sys_asset, sys_shapes, ColorSlot, OpticalFlag, ShapeKind, VectorShape,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconKind {
    // App shortcuts (source-compatible with existing callers)
    Terminal,
    LifeStation,
    Files,
    Settings,
    Editor,
    Calculator,
    // Core desktop & existing system variants
    Search,
    Bell,
    Power,
    Lock,
    Close,
    Check,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Sun,
    Moon,
    Sliders,
    Folder,
    File,
    Save,
    Plus,
    Trash,
    Shield,
    Cpu,
    Memory,
    Refresh,
    LayoutTiled,
    LayoutFloat,
    LayoutTabbed,
    Warning,
    Info,
    // Expanded navigation, edit & window controls
    Back,
    Forward,
    ChevronUp,
    Menu,
    Options,
    Filter,
    Sort,
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    Open,
    Download,
    Upload,
    Edit,
    Expand,
    Collapse,
    Home,
    WinClose,
    WinMin,
    WinMax,
    WinRestore,
    WorkspaceGrid,
    DockDot,
    // Files & storage
    FolderOpen,
    FileDoc,
    FileCode,
    FileExec,
    FileImage,
    FileArchive,
    FileAudio,
    FileVideo,
    Disk,
    Partition,
    DriveNvme,
    DriveUsb,
    StorageCtrl,
    // Hardware, network & devices
    Gpu,
    Display,
    AudioOut,
    Keyboard,
    Mouse,
    Touchpad,
    Battery,
    NetworkEth,
    NetworkWifi,
    DriverIsland,
    UsbPort,
    UsbHid,
    PermissionCap,
    // Status, progress, notifications & services
    Unlocked,
    NotifUnread,
    QuickSettings,
    Accessibility,
    Online,
    Offline,
    Sync,
    Updating,
    Unavailable,
    StatusSuccess,
    StatusWarning,
    StatusFailure,
    StatusInfo,
    DeviceOk,
    SvcRunning,
    SvcStopped,
    SvcCrashed,
    ProgressRing,
    SpinnerArc,
}

impl IconKind {
    #[inline]
    pub const fn from_app(app: AppId) -> Self {
        match app {
            AppId::Terminal => Self::Terminal,
            AppId::LifeStation => Self::LifeStation,
            AppId::Files => Self::Files,
            AppId::Settings => Self::Settings,
            AppId::Editor => Self::Editor,
            AppId::Calculator => Self::Calculator,
        }
    }

    #[inline]
    pub const fn sys_id(self) -> Option<&'static [u8]> {
        match self {
            Self::Search => Some(b"search"),
            Self::Bell => Some(b"notif-bell"),
            Self::Power => Some(b"power"),
            Self::Lock => Some(b"locked"),
            Self::Close | Self::WinClose => Some(b"close"),
            Self::ChevronLeft | Self::Back => Some(b"back"),
            Self::ChevronRight | Self::Forward => Some(b"forward"),
            Self::ChevronDown => Some(b"down"),
            Self::ChevronUp => Some(b"up"),
            Self::Sliders | Self::QuickSettings => Some(b"quick-settings"),
            Self::Folder => Some(b"folder"),
            Self::File | Self::FileDoc => Some(b"file-doc"),
            Self::Save => Some(b"save"),
            Self::Plus => Some(b"add"),
            Self::Trash => Some(b"trash"),
            Self::Shield | Self::PermissionCap => Some(b"permission-cap"),
            Self::Cpu => Some(b"cpu"),
            Self::Memory => Some(b"ram"),
            Self::Refresh => Some(b"refresh"),
            Self::LayoutTiled => Some(b"tile-h"),
            Self::LayoutFloat => Some(b"tile-float"),
            Self::LayoutTabbed => Some(b"tile-stack"),
            Self::Warning | Self::StatusWarning => Some(b"status-warning"),
            Self::Info | Self::StatusInfo => Some(b"status-info"),
            Self::Menu => Some(b"menu"),
            Self::Options => Some(b"options"),
            Self::Filter => Some(b"filter"),
            Self::Sort => Some(b"sort"),
            Self::Undo => Some(b"undo"),
            Self::Redo => Some(b"redo"),
            Self::Copy => Some(b"copy"),
            Self::Cut => Some(b"cut"),
            Self::Paste => Some(b"paste"),
            Self::Open => Some(b"open"),
            Self::Download => Some(b"download"),
            Self::Upload => Some(b"upload"),
            Self::Edit => Some(b"edit"),
            Self::Expand => Some(b"expand"),
            Self::Collapse => Some(b"collapse"),
            Self::Home => Some(b"home"),
            Self::WinMin => Some(b"win-min"),
            Self::WinMax => Some(b"win-max"),
            Self::WinRestore => Some(b"win-restore"),
            Self::WorkspaceGrid => Some(b"workspace-grid"),
            Self::DockDot => Some(b"dock-dot"),
            Self::FolderOpen => Some(b"folder-open"),
            Self::FileCode => Some(b"file-code"),
            Self::FileExec => Some(b"file-exec"),
            Self::FileImage => Some(b"file-image"),
            Self::FileArchive => Some(b"file-archive"),
            Self::FileAudio => Some(b"file-audio"),
            Self::FileVideo => Some(b"file-video"),
            Self::Disk => Some(b"disk"),
            Self::Partition => Some(b"partition"),
            Self::DriveNvme => Some(b"drive-nvme"),
            Self::DriveUsb => Some(b"drive-usb"),
            Self::StorageCtrl => Some(b"storage-ctrl"),
            Self::Gpu => Some(b"gpu"),
            Self::Display => Some(b"display"),
            Self::AudioOut => Some(b"audio-out"),
            Self::Keyboard => Some(b"keyboard"),
            Self::Mouse => Some(b"mouse"),
            Self::Touchpad => Some(b"touchpad"),
            Self::Battery => Some(b"battery"),
            Self::NetworkEth => Some(b"network-eth"),
            Self::NetworkWifi => Some(b"network-wifi"),
            Self::DriverIsland => Some(b"driver-island"),
            Self::UsbPort => Some(b"usb-port"),
            Self::UsbHid => Some(b"usb-hid"),
            Self::Unlocked => Some(b"unlocked"),
            Self::NotifUnread => Some(b"notif-unread"),
            Self::Accessibility => Some(b"accessibility"),
            Self::Online => Some(b"online"),
            Self::Offline => Some(b"offline"),
            Self::Sync => Some(b"sync"),
            Self::Updating => Some(b"updating"),
            Self::Unavailable => Some(b"unavailable"),
            Self::StatusSuccess => Some(b"status-success"),
            Self::StatusFailure => Some(b"status-failure"),
            Self::DeviceOk => Some(b"device-ok"),
            Self::SvcRunning => Some(b"svc-running"),
            Self::SvcStopped => Some(b"svc-stopped"),
            Self::SvcCrashed => Some(b"svc-crashed"),
            Self::ProgressRing => Some(b"progress-ring"),
            Self::SpinnerArc => Some(b"spinner-arc"),
            _ => None,
        }
    }
}

#[inline]
const fn map_sys_shape(s: &VectorShape) -> Q12Shape {
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

#[inline(always)]
fn gx(r: Rect, norm_x10: i32) -> i32 {
    (r.x as i32) * 16 + (norm_x10 * r.w as i32) / 10
}

#[inline(always)]
fn gy(r: Rect, norm_x10: i32) -> i32 {
    (r.y as i32) * 16 + (norm_x10 * r.h as i32) / 10
}

#[inline]
fn infer_mode(fg: u32, accent: u32) -> ThemeMode {
    let luma = ((fg >> 16) & 0xFF) * 299 + ((fg >> 8) & 0xFF) * 587 + (fg & 0xFF) * 114;
    if accent == 0x00FF_E600 || fg == 0x00FF_E600 {
        ThemeMode::HighContrast
    } else if luma < 128_000 {
        ThemeMode::FrostedDaylight
    } else {
        ThemeMode::ObsidianDark
    }
}

/// Render any `vector_assets_sys` icon by exact ID using `pal`.
pub fn draw_sys_icon_id(c: &mut Canvas, r: Rect, id: &[u8], pal: VectorPalette) {
    if r.w < 8 || r.h < 8 {
        return;
    }
    let Some(asset) = find_sys_asset(id) else {
        return;
    };
    let fit = fit_aspect_rect(r, asset.vb_w, asset.vb_h);
    let small = fit.w.min(fit.h) <= 20;
    for s in sys_shapes(asset) {
        c.draw_q12_shape(fit, map_sys_shape(s), pal, small);
    }
}

#[inline]
const fn app_kind_for(kind: IconKind) -> Option<AppIconKind> {
    match kind {
        IconKind::Terminal => Some(AppIconKind::Terminal),
        IconKind::LifeStation => Some(AppIconKind::LifeStation),
        IconKind::Files => Some(AppIconKind::Files),
        IconKind::Settings => Some(AppIconKind::Settings),
        IconKind::Editor => Some(AppIconKind::TextEditor),
        IconKind::Calculator => Some(AppIconKind::Calculator),
        _ => None,
    }
}

pub fn draw_vector_icon(c: &mut Canvas, r: Rect, kind: IconKind, fg: u32, accent: u32) {
    if r.w < 8 || r.h < 8 {
        return;
    }
    if let Some(app_kind) = app_kind_for(kind) {
        draw_app_icon(c, r, app_kind, infer_mode(fg, accent));
        return;
    }
    if let Some(id) = kind.sys_id() {
        draw_sys_icon_id(c, r, id, VectorPalette::from_fg_accent(fg, accent));
        return;
    }
    let sw = ((15 * r.w.min(r.h) as i32) / 16).max(16);
    let thin = ((12 * r.w.min(r.h) as i32) / 16).max(14);
    match kind {
        IconKind::Check => {
            c.stroke_line_aa(gx(r, 34), gy(r, 86), gx(r, 66), gy(r, 118), sw, fg, 255);
            c.stroke_line_aa(gx(r, 66), gy(r, 118), gx(r, 128), gy(r, 48), sw, fg, 255);
        }
        IconKind::Sun => {
            c.fill_circle_aa(gx(r, 80), gy(r, 80), (28 * r.w as i32) / 16, accent, 255);
            for (x0, y0, x1, y1) in [
                (80, 18, 80, 34),
                (80, 126, 80, 142),
                (18, 80, 34, 80),
                (126, 80, 142, 80),
            ] {
                c.stroke_line_aa(gx(r, x0), gy(r, y0), gx(r, x1), gy(r, y1), thin, fg, 235);
            }
        }
        IconKind::Moon => {
            c.fill_circle_aa(gx(r, 76), gy(r, 82), (46 * r.w as i32) / 16, accent, 245);
            c.stroke_circle_aa(gx(r, 98), gy(r, 64), (36 * r.w as i32) / 16, sw, fg, 190);
        }
        _ => {}
    }
}

/// Render an icon resolved against a full `UiTheme`.
pub fn draw_vector_icon_themed(c: &mut Canvas, r: Rect, kind: IconKind, t: UiTheme) {
    if r.w < 8 || r.h < 8 {
        return;
    }
    if let Some(app_kind) = app_kind_for(kind) {
        draw_app_icon(c, r, app_kind, t.mode);
        return;
    }
    if let Some(id) = kind.sys_id() {
        draw_sys_icon_id(c, r, id, VectorPalette::from_mode(t.mode));
        return;
    }
    draw_vector_icon(c, r, kind, t.fg_primary, t.accent_fg);
}
