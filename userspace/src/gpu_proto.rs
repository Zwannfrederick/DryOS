// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! IPC Wire Contract between `dry-display` Compositor, `dry-init` Supervisor, and `dry-gpu.elf`
//! (`Priority C` — Isolated Ring 3 VirtIO-GPU 2D Scanout Driver).

use crate::BADGE_SUPERVISOR;

pub const COMP_ID_GPU_DRV: u16 = 0x0009;
pub const BADGE_GPU_DISPLAY: u64 = 0x0901;
pub const BADGE_GPU_SUP: u64 = BADGE_SUPERVISOR;
pub const BADGE_GPU_ROGUE: u64 = 0x0999;

pub const GPU_OP_QUERY_INFO: u32 = 0x6600;
pub const GPU_OP_ATTACH_BACKBUF: u32 = 0x6601;
pub const GPU_OP_PRESENT_RECT: u32 = 0x6602;
pub const GPU_OP_BIND_PCI: u32 = 0x6603;
