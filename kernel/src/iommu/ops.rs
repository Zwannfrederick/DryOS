// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Vendor-neutral IOMMU abstraction (`IommuOps`), explicit enforcement status, and fail-closed
//! DMA policy evaluation (`Milestone 1.1` / `1.2`).

use core::arch::x86_64::__cpuid;

/// Physical IOMMU hardware / test backend kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum IommuBackendKind {
    None = 0,
    IntelVtd = 1,
    AmdVi = 2,
    QemuTestBypass = 3,
}

impl IommuBackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::IntelVtd => "intel-vtd",
            Self::AmdVi => "amd-vi",
            Self::QemuTestBypass => "qemu-test-no-iommu",
        }
    }
}

/// Explicit Ring-0 IOMMU enforcement state machine.
///
/// Only `Enforcing` and (solely under triple-gated QEMU test conditions) `TestUnsafeNoIommu`
/// authorize Ring-3 driver claiming (`SYS_DEV_MAP`), Bus Master Enable (`BME = 1`), and DMA
/// allocation/mapping. `Uninitialized`, `UnsupportedFailClosed`, and `FailedClosed` reject DMA.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum IommuStatus {
    Uninitialized = 0,
    Enforcing = 1,
    TestUnsafeNoIommu = 2,
    UnsupportedFailClosed = 3,
    FailedClosed = 4,
}

impl IommuStatus {
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Enforcing,
            2 => Self::TestUnsafeNoIommu,
            3 => Self::UnsupportedFailClosed,
            4 => Self::FailedClosed,
            _ => Self::Uninitialized,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Enforcing => "enforcing",
            Self::TestUnsafeNoIommu => "test-unsafe-no-iommu",
            Self::UnsupportedFailClosed => "unsupported-fail-closed",
            Self::FailedClosed => "failed-closed",
        }
    }
}

/// Page access permissions for IOMMU IOVA mappings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IommuPerms(pub u8);

impl IommuPerms {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const RW: Self = Self(Self::READ.0 | Self::WRITE.0);

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Vendor-neutral IOMMU hardware capabilities & address-width descriptor.
#[derive(Clone, Copy, Debug)]
pub struct IommuCaps {
    pub backend: IommuBackendKind,
    pub max_aw_bits: u8,
    pub page_4k: bool,
    pub page_2m: bool,
    pub coherent: bool,
    pub int_remap: bool,
    pub fault_reporting: bool,
    pub reserved_regions: bool,
}

impl IommuCaps {
    pub const fn none() -> Self {
        Self {
            backend: IommuBackendKind::None,
            max_aw_bits: 0,
            page_4k: false,
            page_2m: false,
            coherent: false,
            int_remap: false,
            fault_reporting: false,
            reserved_regions: false,
        }
    }
}

/// Capability-oriented vendor-neutral IOMMU operations interface.
pub trait IommuOps: Sync {
    fn kind(&self) -> IommuBackendKind;
    fn caps(&self) -> IommuCaps;
    fn attach_device(&self, dev_idx: usize, bus: u8, dev: u8, func: u8) -> Result<(), ()>;
    fn detach_device(&self, dev_idx: usize);
    fn map_iova(
        &self,
        dev_idx: usize,
        iova: u64,
        hpa: u64,
        len_bytes: u64,
        perms: IommuPerms,
    ) -> Result<(), ()>;
    fn unmap_iova(&self, dev_idx: usize, iova: u64, len_bytes: u64);
    fn poll_faults(&self) -> u64;

    /// Maximum physical/IOVA address mask supported by this IOMMU backend.
    fn max_dma_mask(&self) -> u64 {
        let bits = self.caps().max_aw_bits;
        if bits == 0 {
            0
        } else if bits >= 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        }
    }

    /// Apply any firmware reserved memory regions (`RMRR`/`IVMD`) required for `dev_idx`.
    fn apply_reserved_regions(&self, _dev_idx: usize) -> Result<(), ()> {
        Ok(())
    }
}

/// Snapshot of fail-closed IOMMU policy inputs evaluated at boot.
#[derive(Clone, Copy, Debug)]
pub struct IommuPolicyDecision {
    pub hypervisor_cpuid: bool,
    pub qemu_test_opt_in: bool,
    pub live_usb: bool,
    pub allow_qemu_test_bypass: bool,
}

/// Return `true` when `CPUID.01H:ECX[31]` indicates execution under a hypervisor (never true on bare metal).
#[inline]
pub fn is_hypervisor_cpuid() -> bool {
    let res = __cpuid(1);
    (res.ecx & (1 << 31)) != 0
}

/// Exact whitespace-delimited kernel cmdline token match.
#[inline]
pub fn cmdline_has_token(cmdline: &str, token: &str) -> bool {
    cmdline.split_ascii_whitespace().any(|tok| tok == token)
}

/// Pure policy evaluation: QEMU non-IOMMU test mode requires ALL 3 invariants simultaneously:
/// `hypervisor_cpuid && qemu_test_opt_in && !live_usb`.
#[inline]
pub const fn evaluate_iommu_policy_flags(
    hypervisor_cpuid: bool,
    qemu_test_opt_in: bool,
    live_usb: bool,
) -> bool {
    hypervisor_cpuid && qemu_test_opt_in && !live_usb
}

/// Evaluate boot cmdline and CPUID hypervisor bit to decide whether `TestUnsafeNoIommu` may activate
/// when no supported hardware IOMMU is present.
pub fn evaluate_iommu_policy(cmdline: &str) -> IommuPolicyDecision {
    let hypervisor_cpuid = is_hypervisor_cpuid();
    let qemu_test_opt_in = cmdline_has_token(cmdline, "dry_qemu_test_no_iommu=1");
    let live_usb = cmdline_has_token(cmdline, "live_usb=1");
    let allow_qemu_test_bypass =
        evaluate_iommu_policy_flags(hypervisor_cpuid, qemu_test_opt_in, live_usb);
    IommuPolicyDecision {
        hypervisor_cpuid,
        qemu_test_opt_in,
        live_usb,
        allow_qemu_test_bypass,
    }
}

/// Return `true` iff `st` permits DMA claiming / Bus Master Enable.
#[inline]
pub const fn is_dma_allowed_status(st: IommuStatus) -> bool {
    matches!(st, IommuStatus::Enforcing | IommuStatus::TestUnsafeNoIommu)
}

/// Return `true` iff `st` provides real hardware IOMMU DMA remapping isolation.
#[inline]
pub const fn is_hw_enforcing_status(st: IommuStatus) -> bool {
    matches!(st, IommuStatus::Enforcing)
}
