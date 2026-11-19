// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Vendor-neutral IOMMU policy, Intel VT-d (`DMAR`/`SLPT`), and AMD-Vi (`IVRS`) subsystem.

pub mod dmar;
pub mod ivrs;
pub mod ops;
pub mod slpt;
pub mod tests;
pub mod vtd;
pub mod vtd_domain;

pub use dmar::{dmar_snapshot, overlaps_rmrr, parse_and_latch_dmar};
pub use ivrs::parse_and_latch_ivrs;
pub use ops::{IommuBackendKind, IommuCaps, IommuOps, IommuPerms, IommuStatus};
pub use tests::run_dmar_unit_tests;
pub use vtd::{backend_kind, caps, init_vtd_units, is_dma_permitted, is_gfx_scanout_deferred, is_hw_enforcing, poll_and_clear_faults, status};
pub use vtd_domain::{
    attach_device_domain, detach_and_scrub_domain, is_device_coherent, map_dma_range,
    map_dma_range_iova, unmap_dma_range,
};

struct VtdIommuOps;
struct QemuBypassIommuOps;
struct FailClosedIommuOps;

static VTD_OPS: VtdIommuOps = VtdIommuOps;
static QEMU_BYPASS_OPS: QemuBypassIommuOps = QemuBypassIommuOps;
static FAIL_CLOSED_OPS: FailClosedIommuOps = FailClosedIommuOps;

impl IommuOps for VtdIommuOps {
    fn kind(&self) -> IommuBackendKind {
        IommuBackendKind::IntelVtd
    }
    fn caps(&self) -> IommuCaps {
        caps()
    }
    fn attach_device(&self, dev_idx: usize, bus: u8, dev: u8, func: u8) -> Result<(), ()> {
        attach_device_domain(dev_idx, bus, dev, func)
    }
    fn detach_device(&self, dev_idx: usize) {
        detach_and_scrub_domain(dev_idx);
    }
    fn map_iova(
        &self,
        dev_idx: usize,
        iova: u64,
        hpa: u64,
        len_bytes: u64,
        perms: IommuPerms,
    ) -> Result<(), ()> {
        map_dma_range_iova(dev_idx, iova, hpa, len_bytes, perms)
    }
    fn unmap_iova(&self, dev_idx: usize, iova: u64, len_bytes: u64) {
        unmap_dma_range(dev_idx, iova, len_bytes);
    }
    fn poll_faults(&self) -> u64 {
        poll_and_clear_faults()
    }
}

impl IommuOps for QemuBypassIommuOps {
    fn kind(&self) -> IommuBackendKind {
        IommuBackendKind::QemuTestBypass
    }
    fn caps(&self) -> IommuCaps {
        caps()
    }
    fn attach_device(&self, dev_idx: usize, bus: u8, dev: u8, func: u8) -> Result<(), ()> {
        attach_device_domain(dev_idx, bus, dev, func)
    }
    fn detach_device(&self, _dev_idx: usize) {}
    fn map_iova(
        &self,
        dev_idx: usize,
        iova: u64,
        hpa: u64,
        len_bytes: u64,
        perms: IommuPerms,
    ) -> Result<(), ()> {
        map_dma_range_iova(dev_idx, iova, hpa, len_bytes, perms)
    }
    fn unmap_iova(&self, _dev_idx: usize, _iova: u64, _len_bytes: u64) {}
    fn poll_faults(&self) -> u64 {
        0
    }
}

impl IommuOps for FailClosedIommuOps {
    fn kind(&self) -> IommuBackendKind {
        backend_kind()
    }
    fn caps(&self) -> IommuCaps {
        caps()
    }
    fn attach_device(&self, _dev_idx: usize, _bus: u8, _dev: u8, _func: u8) -> Result<(), ()> {
        Err(())
    }
    fn detach_device(&self, _dev_idx: usize) {}
    fn map_iova(
        &self,
        _dev_idx: usize,
        _iova: u64,
        _hpa: u64,
        _len_bytes: u64,
        _perms: IommuPerms,
    ) -> Result<(), ()> {
        Err(())
    }
    fn unmap_iova(&self, _dev_idx: usize, _iova: u64, _len_bytes: u64) {}
    fn poll_faults(&self) -> u64 {
        vtd::latched_fault_count()
    }
}

pub fn active_iommu_ops() -> &'static dyn IommuOps {
    match status() {
        IommuStatus::Enforcing => &VTD_OPS,
        IommuStatus::TestUnsafeNoIommu => &QEMU_BYPASS_OPS,
        IommuStatus::Uninitialized
        | IommuStatus::UnsupportedFailClosed
        | IommuStatus::FailedClosed => &FAIL_CLOSED_OPS,
    }
}

pub(crate) fn fail_closed_ops_for_test() -> &'static dyn IommuOps {
    &FAIL_CLOSED_OPS
}
