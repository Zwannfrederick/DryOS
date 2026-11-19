// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Vendor-neutral IOMMU policy, Intel VT-d (`DMAR`/`SLPT`), and AMD-Vi (`IVRS`) subsystem.

pub mod dmar;
pub mod ivrs;
pub mod ops;
pub mod slpt;
pub mod tests;
pub mod vtd;

pub use dmar::{dmar_snapshot, parse_and_latch_dmar};
pub use ivrs::parse_and_latch_ivrs;
pub use ops::IommuStatus;
pub use tests::run_dmar_unit_tests;
pub use vtd::{
    attach_device_domain, detach_and_scrub_domain, init_vtd_units, is_dma_permitted,
    is_gfx_scanout_deferred, is_hw_enforcing, map_dma_range, poll_and_clear_faults, status,
    unmap_dma_range,
};
