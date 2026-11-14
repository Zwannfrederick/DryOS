// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Glass 2.0 native UI foundations (`Canvas`, AA geometry, and scalable vector icons).

pub mod canvas;
pub mod icons;

pub use canvas::{isqrt64, lerp_rgb24, rrect_cov, Canvas};
pub use icons::{draw_vector_icon, IconKind};
