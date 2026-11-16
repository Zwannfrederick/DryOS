// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Glass 2.0 native UI toolkit: canvas, vector icons, themes, layout, widgets, text editors,
//! and structural containers.

pub mod app_icons;
pub mod brand;
pub mod canvas;
pub mod containers;
pub mod icons;
pub mod layout;
pub mod text_edit;
pub mod theme;
#[allow(clippy::manual_find)]
pub mod vector_assets_apps;
#[allow(clippy::manual_find)]
pub mod vector_assets_sys;
pub mod widgets;

pub use app_icons::{draw_app_icon, draw_app_icon_for_app, AppIconKind};
pub use brand::{draw_boot_lockup, draw_brand_symbol, draw_brand_wordmark};
pub use canvas::{fit_aspect_rect, isqrt64, lerp_rgb24, rrect_cov, Canvas, VectorPalette};
pub use containers::{
    breadcrumb_bar, card, list_row, metric_card, modal_dialog_frame, popover_menu, progress_bar,
    sidebar_nav, status_badge, tab_bar, table_header,
};
pub use icons::{draw_sys_icon_id, draw_vector_icon, draw_vector_icon_themed, IconKind};
pub use layout::{
    h_cell, inset_rect, split_bot, split_left, split_right, split_top, UiCtx, UiPersist,
    WidgetResp,
};
pub use text_edit::{multiline_viewport, text_field, MultiLineParams, TextFieldState};
pub use theme::{StatusKind, UiTheme};
pub use widgets::{
    button, checkbox, icon_button, scrollbar_v, segmented_control, slider, toggle_switch,
    ButtonOpts, ButtonVariant,
};
