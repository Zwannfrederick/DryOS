// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Explicit 15-step driver lifecycle state machine and multi-backend contract enforcement
//! (`ADR-005` / `ADR-016` / `drvctl`-compatible inspection IPC for `dry-broker`).

use crate::{EINVAL, EPERM};

/// Request flags rejected by every DRY driver backend (`ADR-005`: zero Ring-0 `.ko`, zero host iGPU assumptions).
pub const CONTRACT_FLAG_REQUIRE_RING0_KO: u64 = 1 << 8;
pub const CONTRACT_FLAG_ASSUME_IGPU_PASSTHROUGH: u64 = 1 << 9;

/// Packed capability/isolation guarantee bitflags exposed via `BROKER_OP_DRVCTL_QUERY`.
pub const DRVCTL_CONTRACT_RING0_KO_FORBIDDEN: u64 = 1 << 0;
pub const DRVCTL_CONTRACT_NO_IGPU_PASSTHROUGH: u64 = 1 << 1;
pub const DRVCTL_CONTRACT_FAULT_EP_REQUIRED: u64 = 1 << 2;
pub const DRVCTL_CONTRACT_IOMMU_ISOLATED: u64 = 1 << 3;
pub const DRVCTL_CONTRACT_VALID: u64 = 1 << 4;

/// 15-step universal driver lifecycle state machine (kept identical to `lx_driver_manager.py`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LifecycleStep {
    Detected = 0,
    Matched = 1,
    Assessed = 2,
    SourceResolved = 3,
    DepsFirmwareOk = 4,
    BuiltVerified = 5,
    IslandCreated = 6,
    CapsAssigned = 7,
    ResourcesMapped = 8,
    Initialized = 9,
    Published = 10,
    Monitoring = 11,
    FaultRecovery = 12,
    Unloaded = 13,
    RolledBack = 14,
}

impl LifecycleStep {
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    #[inline]
    pub const fn bit(self) -> u32 {
        1u32 << (self as u8)
    }

    pub const fn name(self) -> &'static [u8] {
        match self {
            Self::Detected => b"DETECTED",
            Self::Matched => b"MATCHED",
            Self::Assessed => b"ASSESSED",
            Self::SourceResolved => b"SOURCE_RESOLVED",
            Self::DepsFirmwareOk => b"DEPS_FIRMWARE_OK",
            Self::BuiltVerified => b"BUILT_VERIFIED",
            Self::IslandCreated => b"ISLAND_CREATED",
            Self::CapsAssigned => b"CAPS_ASSIGNED",
            Self::ResourcesMapped => b"RESOURCES_MAPPED",
            Self::Initialized => b"INITIALIZED",
            Self::Published => b"PUBLISHED",
            Self::Monitoring => b"MONITORING",
            Self::FaultRecovery => b"FAULT_RECOVERY",
            Self::Unloaded => b"UNLOADED",
            Self::RolledBack => b"ROLLED_BACK",
        }
    }

    /// Valid transitions mirroring `VALID_TRANSITIONS` in `tools/lx_compat/lx_driver_manager.py`.
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Detected, Self::Matched | Self::RolledBack)
                | (Self::Matched, Self::Assessed | Self::Unloaded | Self::RolledBack)
                | (Self::Assessed, Self::SourceResolved | Self::Unloaded | Self::RolledBack)
                | (Self::SourceResolved, Self::DepsFirmwareOk | Self::RolledBack)
                | (Self::DepsFirmwareOk, Self::BuiltVerified | Self::RolledBack)
                | (Self::BuiltVerified, Self::IslandCreated | Self::RolledBack)
                | (Self::IslandCreated, Self::CapsAssigned | Self::FaultRecovery | Self::Unloaded)
                | (Self::CapsAssigned, Self::ResourcesMapped | Self::FaultRecovery | Self::Unloaded)
                | (Self::ResourcesMapped, Self::Initialized | Self::FaultRecovery | Self::Unloaded)
                | (Self::Initialized, Self::Published | Self::FaultRecovery | Self::Unloaded)
                | (Self::Published, Self::Monitoring | Self::FaultRecovery | Self::Unloaded)
                | (Self::Monitoring, Self::FaultRecovery | Self::Unloaded)
                | (Self::FaultRecovery, Self::IslandCreated | Self::Unloaded | Self::RolledBack)
                | (Self::Unloaded, Self::RolledBack | Self::Detected)
                | (Self::RolledBack, Self::SourceResolved | Self::Detected)
        )
    }
}

/// Supported DRY execution backends (`native-driver-island`, `lx-compat-island`, `linux-microvm`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DriverBackendKind {
    NativeDriverIsland = 0,
    LxCompatIsland = 1,
    LinuxMicroVm = 2,
}

impl DriverBackendKind {
    pub const fn name(self) -> &'static [u8] {
        match self {
            Self::NativeDriverIsland => b"native-driver-island",
            Self::LxCompatIsland => b"lx-compat-island",
            Self::LinuxMicroVm => b"linux-microvm",
        }
    }

    #[inline]
    pub const fn contract_flags(self) -> u64 {
        DRVCTL_CONTRACT_RING0_KO_FORBIDDEN
            | DRVCTL_CONTRACT_NO_IGPU_PASSTHROUGH
            | DRVCTL_CONTRACT_FAULT_EP_REQUIRED
            | match self {
                Self::LxCompatIsland | Self::LinuxMicroVm => DRVCTL_CONTRACT_IOMMU_ISOLATED,
                Self::NativeDriverIsland => 0,
            }
            | DRVCTL_CONTRACT_VALID
    }
}

/// Validate a backend selection + policy request (`no_ring0_ko` and `no_igpu_passthrough`).
pub fn validate_backend_request(kind_raw: u64, req_flags: u64) -> Result<DriverBackendKind, isize> {
    if (req_flags & CONTRACT_FLAG_REQUIRE_RING0_KO) != 0 {
        return Err(EPERM);
    }
    if (req_flags & CONTRACT_FLAG_ASSUME_IGPU_PASSTHROUGH) != 0 {
        return Err(EINVAL);
    }
    match kind_raw {
        0 => Ok(DriverBackendKind::NativeDriverIsland),
        1 => Ok(DriverBackendKind::LxCompatIsland),
        2 => Ok(DriverBackendKind::LinuxMicroVm),
        _ => Err(EINVAL),
    }
}

/// Runtime tracker owned by `dry-broker` for the supervised driver island instance.
pub struct BrokerLifecycle {
    pub step: LifecycleStep,
    pub backend: DriverBackendKind,
    pub comp_id: u16,
    pub generation: u64,
    pub restarts: u64,
    pub visited_mask: u32,
    pub transitions: u32,
    pub pci_vendor_device: u32,
    pub pci_rev_id: u8,
}

impl BrokerLifecycle {
    pub const fn new(backend: DriverBackendKind, comp_id: u16) -> Self {
        Self {
            step: LifecycleStep::Detected,
            backend,
            comp_id,
            generation: 1,
            restarts: 0,
            visited_mask: LifecycleStep::Detected.bit(),
            transitions: 0,
            pci_vendor_device: 0,
            pci_rev_id: 0,
        }
    }

    pub fn advance(&mut self, next: LifecycleStep) -> Result<(), isize> {
        if !self.step.can_transition_to(next) {
            return Err(EINVAL);
        }
        self.step = next;
        self.visited_mask |= next.bit();
        self.transitions = self.transitions.saturating_add(1);
        Ok(())
    }

    pub fn prepare_for_spawn(
        &mut self,
        backend: DriverBackendKind,
        comp_id: u16,
        pci_rev_id: u8,
    ) {
        self.backend = backend;
        self.comp_id = comp_id;
        self.pci_rev_id = pci_rev_id;
        self.step = LifecycleStep::Detected;
        self.visited_mask = LifecycleStep::Detected.bit();
        for s in [
            LifecycleStep::Matched,
            LifecycleStep::Assessed,
            LifecycleStep::SourceResolved,
            LifecycleStep::DepsFirmwareOk,
            LifecycleStep::BuiltVerified,
        ] {
            let _ = self.advance(s);
        }
    }

    pub fn begin_fault_recovery(&mut self, next_gen: u64) {
        if self.step != LifecycleStep::FaultRecovery {
            let _ = self.advance(LifecycleStep::FaultRecovery);
        }
        self.generation = next_gen;
        self.restarts = self.restarts.saturating_add(1);
    }

    pub fn mark_spawn_failed(&mut self) {
        let _ = self.advance(LifecycleStep::RolledBack);
    }

    pub fn mark_unloaded(&mut self) {
        if self.step != LifecycleStep::Unloaded {
            if !self.step.can_transition_to(LifecycleStep::Unloaded) {
                let _ = self.advance(LifecycleStep::FaultRecovery);
            }
            let _ = self.advance(LifecycleStep::Unloaded);
        }
    }

    /// Encode state into 4 `MRS` registers for `BROKER_OP_DRVCTL_QUERY`.
    pub const fn encode_drvctl(&self, island_pid: u64) -> [u64; 4] {
        let mr0 = (self.step.as_u8() as u64)
            | ((self.backend as u64) << 8)
            | ((self.comp_id as u64) << 16)
            | ((self.pci_rev_id as u64) << 32)
            | ((self.generation & 0xff_ffff) << 40);
        let mr1 = (self.visited_mask as u64) | ((self.transitions as u64) << 32);
        let mr2 = (self.pci_vendor_device as u64) | ((island_pid & 0xffff_ffff) << 32);
        let mr3 = self.backend.contract_flags() | ((self.restarts & 0xffff_ffff) << 32);
        [mr0, mr1, mr2, mr3]
    }
}

/// Verify all 15 `LifecycleStep` transitions, illegal transition rejection, respawn & rollback.
pub fn verify_broker_lifecycle_15_stages() -> Option<(u32, u64)> {
    if validate_backend_request(1, CONTRACT_FLAG_REQUIRE_RING0_KO).is_ok()
        || validate_backend_request(2, CONTRACT_FLAG_ASSUME_IGPU_PASSTHROUGH).is_ok()
    {
        return None;
    }
    let mut lc = BrokerLifecycle::new(DriverBackendKind::LxCompatIsland, 0x0212);
    lc.pci_vendor_device = 0x1234_1111;
    lc.prepare_for_spawn(DriverBackendKind::LxCompatIsland, 0x0212, 2);
    if lc.advance(LifecycleStep::Monitoring).is_ok() {
        return None;
    }
    for s in [
        LifecycleStep::IslandCreated,
        LifecycleStep::CapsAssigned,
        LifecycleStep::ResourcesMapped,
        LifecycleStep::Initialized,
        LifecycleStep::Published,
        LifecycleStep::Monitoring,
    ] {
        lc.advance(s).ok()?;
    }
    lc.begin_fault_recovery(2);
    for s in [
        LifecycleStep::IslandCreated,
        LifecycleStep::CapsAssigned,
        LifecycleStep::ResourcesMapped,
        LifecycleStep::Initialized,
        LifecycleStep::Published,
        LifecycleStep::Monitoring,
    ] {
        lc.advance(s).ok()?;
    }
    lc.mark_unloaded();
    lc.mark_spawn_failed();
    if lc.visited_mask != 0x7fff || lc.generation != 2 || lc.restarts != 1 {
        return None;
    }
    let regs = lc.encode_drvctl(99);
    let required = DRVCTL_CONTRACT_RING0_KO_FORBIDDEN
        | DRVCTL_CONTRACT_NO_IGPU_PASSTHROUGH
        | DRVCTL_CONTRACT_VALID;
    if (regs[3] & required) != required {
        return None;
    }
    Some((lc.visited_mask, lc.restarts))
}
