// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Intel VT-d / IOMMU subsystem (`Milestone 2C-2`).

pub mod dmar;
pub mod slpt;
pub mod tests;
pub mod vtd;

pub use dmar::{dmar_snapshot, parse_and_latch_dmar};
pub use tests::run_dmar_unit_tests;
pub use vtd::{
    attach_device_domain, detach_and_scrub_domain, init_vtd_units, is_gfx_scanout_deferred,
    map_dma_range, poll_and_clear_faults, status, unmap_dma_range, VtdStatus,
};
