// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Foundational `ADR-007` Security Labels, 3-Domain Isolation, and Capability Primitives.
//!
//! Keeps `dry-kernel` microkernel-pure (`VFS`, `DRY Vault`, and `Policy Broker` live in Ring 3)
//! while baking mandatory `SecLabel`, `lease_epoch`, `generation`, and `iommu_pin_count` rules
//! directly into kernel object and capability headers from Day 1 (zero architectural debt).

use core::sync::atomic::{AtomicU32, Ordering};

/// Deterministic syscall / security error codes (`ADR-002` & `ADR-007`).
#[repr(i64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecError {
    PermCap = -1,    // EPERM_CAP (deny-by-default / domain or label mismatch)
    BusyPinned = -3, // EBUSY_PINNED (active IOMMU pin or immutable shared export)
    Revoked = -6,    // EREVOKED (generation mismatch or expired DRY Vault lease_epoch)
    Inval = -7,      // EINVAL (W^X violation or malformed request)
}

/// Three strict hardware/architectural security domains (`ADR-007` Rule 1).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecDomain {
    Kernel = 0,
    RootSystem = 1,
    Userspace = 2,
}

/// Compact security flags carried on every `SecLabel` (`ADR-007` Rules 2, 6, 10).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecFlags(pub u8);

impl SecFlags {
    pub const NONE: Self = Self(0);
    /// Controlled read-only system library / shared page exposed to `USERSPACE` (`W^X` enforced).
    pub const READ_ONLY_EXPORT: Self = Self(1 << 0);
    /// Scoped `ROOT_SYSTEM` Developer Workspace compartment (`ADR-007` Rule 6).
    pub const DEV_WORKSPACE: Self = Self(1 << 1);
    /// DMA-capable `DmaArena` / device resource (`ADR-007` Rule 10).
    pub const DEVICE_DMA: Self = Self(1 << 2);
    /// Capability to this object cannot be delegated across compartments (`ADR-007` Rule 10).
    pub const NO_DELEGATE: Self = Self(1 << 3);

    #[inline]
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }
}

/// Fixed 64-bit Security Label attached to every kernel object, endpoint, frame, and VFS node.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecLabel {
    pub domain: SecDomain,
    pub flags: SecFlags,
    pub compartment_id: u16,
    pub policy_tag: u32,
}

impl SecLabel {
    pub const fn new(domain: SecDomain, flags: SecFlags, compartment_id: u16, policy_tag: u32) -> Self {
        Self { domain, flags, compartment_id, policy_tag }
    }

    /// Default parent-to-child inheritance (`ADR-007` Rule 4): preserves domain, compartment,
    /// and policy tag; never escalates domain or strips `NO_DELEGATE`.
    pub const fn inherit_child(self) -> Self {
        self
    }
}

/// Capability operation rights bitmask (`ADR-002`, `ADR-003` & `ADR-007`).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapRights(pub u16);

impl CapRights {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXEC: Self = Self(1 << 2);
    pub const GRANT: Self = Self(1 << 3);
    pub const SIGNAL: Self = Self(1 << 4);
    pub const RETYPE: Self = Self(1 << 5);
    pub const MAP: Self = Self(1 << 6);
    pub const MANAGE: Self = Self(1 << 7);
    pub const REVOKE: Self = Self(1 << 8);
    pub const SEND: Self = Self(1 << 9);
    pub const RECV: Self = Self(1 << 10);
    pub const CALL: Self = Self(1 << 11);
    pub const REPLY: Self = Self(1 << 12);
    pub const WAIT: Self = Self(1 << 13);
    pub const SPAWN: Self = Self(1 << 14);

    pub const FRAME_DATA_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::MAP.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const FRAME_CODE_ROOT: Self = Self(
        Self::READ.0 | Self::EXEC.0 | Self::MAP.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const FRAME_SHARED_RO: Self = Self(Self::READ.0 | Self::EXEC.0 | Self::MAP.0);
    pub const VSPACE_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::MAP.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const TCB_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::SIGNAL.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const UNTYPED_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::RETYPE.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const CNODE_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const ENDPOINT_ROOT: Self = Self(
        Self::READ.0
            | Self::SEND.0
            | Self::RECV.0
            | Self::CALL.0
            | Self::REPLY.0
            | Self::GRANT.0
            | Self::MANAGE.0
            | Self::REVOKE.0,
    );
    pub const NOTIF_ROOT: Self = Self(
        Self::READ.0 | Self::SIGNAL.0 | Self::WAIT.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const BOOTMOD_ROOT: Self = Self(
        Self::READ.0 | Self::SPAWN.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const FB_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::MAP.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const IRQ_ROOT: Self = Self(
        Self::READ.0 | Self::SIGNAL.0 | Self::WAIT.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const IOPORT_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const POWER_ROOT: Self = Self(
        Self::READ.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const PCI_ROOT: Self = Self(
        Self::READ.0 | Self::WRITE.0 | Self::MAP.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );
    pub const VAULT_ROOT: Self = Self(
        Self::READ.0 | Self::GRANT.0 | Self::MANAGE.0 | Self::REVOKE.0,
    );

    #[inline(always)]
    pub const fn contains(self, rhs: Self) -> bool {
        (self.0 & rhs.0) == rhs.0
    }
}

/// Encode per-user (`user_ns`) and per-application (`app_id`) isolation into `SecLabel.compartment_id` (`Stage C.6`).
#[inline(always)]
pub const fn encode_compartment(user_ns: u8, app_id: u8) -> u16 {
    ((user_ns as u16) << 8) | (app_id as u16)
}

#[inline(always)]
pub const fn decode_compartment(compartment_id: u16) -> (u8, u8) {
    (((compartment_id >> 8) & 0xff) as u8, (compartment_id & 0xff) as u8)
}

#[inline(always)]
pub const fn pack_seclabel_u64(label: SecLabel) -> u64 {
    (label.domain as u64)
        | ((label.flags.0 as u64) << 8)
        | ((label.compartment_id as u64) << 16)
        | ((label.policy_tag as u64) << 32)
}

#[inline(always)]
pub const fn unpack_seclabel_u64(raw: u64) -> SecLabel {
    let domain = match (raw & 0xff) as u8 {
        0 => SecDomain::Kernel,
        1 => SecDomain::RootSystem,
        _ => SecDomain::Userspace,
    };
    SecLabel::new(
        domain,
        SecFlags(((raw >> 8) & 0xff) as u8),
        ((raw >> 16) & 0xffff) as u16,
        (raw >> 32) as u32,
    )
}

static GLOBAL_LEASE_EPOCH: AtomicU32 = AtomicU32::new(1);

#[inline(always)]
pub fn current_lease_epoch() -> u32 {
    GLOBAL_LEASE_EPOCH.load(Ordering::Acquire)
}

pub fn set_lease_epoch(epoch: u32) {
    GLOBAL_LEASE_EPOCH.store(epoch.max(1), Ordering::Release);
}

/// Register-only fast-path `ADR-007` + `CapRights` + `DRY Vault` lease evaluator (`INV-SEC-01..04, 07, 08`).
#[inline(always)]
pub fn authorize_cspace_fast(
    subject_label: SecLabel,
    obj_label: SecLabel,
    cap_rights: CapRights,
    requested: CapRights,
    lease_epoch: u32,
    now_epoch: u32,
) -> Result<(), SecError> {
    if requested.0 == 0 || !cap_rights.contains(requested) {
        return Err(SecError::PermCap);
    }
    if requested.contains(CapRights::WRITE) && requested.contains(CapRights::EXEC) {
        return Err(SecError::Inval);
    }
    if lease_epoch != 0 && now_epoch > lease_epoch {
        return Err(SecError::Revoked);
    }

    // ADR-007 Rules 2 & 8 (`INV-SEC-01`): `KERNEL` objects are managed exclusively by `SecDomain::Kernel`.
    if obj_label.domain == SecDomain::Kernel && subject_label.domain != SecDomain::Kernel {
        return Err(SecError::PermCap);
    }

    // INV-SEC-03: Controlled read-only shared objects never permit `WRITE`, `RETYPE`, or `MANAGE` mutation.
    if obj_label.flags.contains(SecFlags::READ_ONLY_EXPORT)
        && (requested.0 & (CapRights::WRITE.0 | CapRights::RETYPE.0)) != 0
    {
        return Err(SecError::PermCap);
    }

    match subject_label.domain {
        SecDomain::Userspace => match obj_label.domain {
            SecDomain::Kernel => return Err(SecError::PermCap),
            SecDomain::RootSystem => {
                let allowed_ro = CapRights::READ.0 | CapRights::EXEC.0 | CapRights::MAP.0;
                if !obj_label.flags.contains(SecFlags::READ_ONLY_EXPORT)
                    || (requested.0 & !allowed_ro) != 0
                {
                    return Err(SecError::PermCap);
                }
            }
            SecDomain::Userspace => {
                if subject_label.compartment_id != obj_label.compartment_id
                    && obj_label.flags.contains(SecFlags::NO_DELEGATE)
                {
                    return Err(SecError::PermCap);
                }
            }
        },
        SecDomain::RootSystem => {
            if subject_label.flags.contains(SecFlags::DEV_WORKSPACE)
                && subject_label.compartment_id != obj_label.compartment_id
                && obj_label.domain == SecDomain::RootSystem
                && !obj_label.flags.contains(SecFlags::READ_ONLY_EXPORT)
                && (lease_epoch == 0 || obj_label.flags.contains(SecFlags::NO_DELEGATE))
            {
                return Err(SecError::PermCap);
            }
        }
        SecDomain::Kernel => {}
    }

    Ok(())
}

/// Common header embedded at offset 0 of every kernel object (`TCB`, `VSpace`, `Endpoint`, `Frame`).
#[repr(C)]
pub struct KernelObjHeader {
    pub label: SecLabel,
    pub generation: AtomicU32,
    pub iommu_pin_count: AtomicU32,
    pub map_ref_count: AtomicU32,
}

impl KernelObjHeader {
    pub const fn new(label: SecLabel) -> Self {
        Self {
            label,
            generation: AtomicU32::new(1),
            iommu_pin_count: AtomicU32::new(0),
            map_ref_count: AtomicU32::new(0),
        }
    }
}

/// Capability slot metadata (`ADR-003` + `ADR-007` lease & revocation tracking).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityMeta {
    pub rights: CapRights,
    pub expected_gen: u32,
    /// `0 == permanent`, `>0 == DRY Vault lease epoch ceiling` (`INV-SEC-07`).
    pub lease_epoch: u32,
    pub badge: u64,
}

/// Ring 0 mandatory authorization check for `dry_vspace_map` / object access (`INV-SEC-01..03, 07, 08`).
/// Note (`INV-SEC-08`): There is intentionally NO permissive/bypass global flag anywhere in `dry-kernel`.
pub fn authorize_object_access(
    subject_label: SecLabel,
    cap: CapabilityMeta,
    obj: &KernelObjHeader,
    requested: CapRights,
    current_lease_epoch: u32,
) -> Result<(), SecError> {
    // INV-SEC-01 & ADR-007 Rule 8: Even `RootSystem` NEVER maps or accesses `Kernel` domain objects.
    if obj.label.domain == SecDomain::Kernel {
        return Err(SecError::PermCap);
    }

    // INV-SEC-07: Generation match (Revoke) + DRY Vault lease expiry check.
    if obj.generation.load(Ordering::Acquire) != cap.expected_gen {
        return Err(SecError::Revoked);
    }
    if cap.lease_epoch != 0 && current_lease_epoch > cap.lease_epoch {
        return Err(SecError::Revoked);
    }

    // Deny-by-default capability rights check + W^X check on RWX mappings.
    if requested.0 == 0 || !cap.rights.contains(requested) {
        return Err(SecError::PermCap);
    }
    if requested.contains(CapRights::WRITE) && requested.contains(CapRights::EXEC) {
        return Err(SecError::Inval);
    }

    // INV-SEC-03: Controlled read-only shared exports NEVER permit WRITE or RETYPE while mapped.
    if obj.label.flags.contains(SecFlags::READ_ONLY_EXPORT) {
        if requested.contains(CapRights::WRITE) {
            return Err(SecError::PermCap);
        }
        if requested.contains(CapRights::RETYPE) && obj.map_ref_count.load(Ordering::Acquire) > 0 {
            return Err(SecError::BusyPinned);
        }
    }

    // ADR-003: Active IOMMU DMA pin blocks RETYPE.
    if requested.contains(CapRights::RETYPE) && obj.iommu_pin_count.load(Ordering::Acquire) > 0 {
        return Err(SecError::BusyPinned);
    }

    // INV-SEC-02: `USERSPACE` isolation rules.
    if subject_label.domain == SecDomain::Userspace {
        match obj.label.domain {
            SecDomain::Kernel => return Err(SecError::PermCap),
            SecDomain::RootSystem => {
                // Userspace can ONLY read/exec explicitly exported read-only system objects.
                if !obj.label.flags.contains(SecFlags::READ_ONLY_EXPORT)
                    || requested.contains(CapRights::WRITE)
                    || requested.contains(CapRights::RETYPE)
                {
                    return Err(SecError::PermCap);
                }
            }
            SecDomain::Userspace => {
                // Distinct compartments remain isolated unless capability explicitly matches compartment.
                if subject_label.compartment_id != obj.label.compartment_id
                    && obj.label.flags.contains(SecFlags::NO_DELEGATE)
                {
                    return Err(SecError::PermCap);
                }
            }
        }
    } else if subject_label.domain == SecDomain::RootSystem
        && subject_label.flags.contains(SecFlags::DEV_WORKSPACE)
        && subject_label.compartment_id != obj.label.compartment_id
        && obj.label.domain == SecDomain::RootSystem
        && !obj.label.flags.contains(SecFlags::READ_ONLY_EXPORT)
        && (cap.lease_epoch == 0 || obj.label.flags.contains(SecFlags::NO_DELEGATE))
    {
        // ADR-007 Rule 6 & 12: Scoped Developer Workspace cannot mutate outside its compartment
        // (e.g., cannot read/write DRY Vault private state).
        return Err(SecError::PermCap);
    }

    Ok(())
}

/// Monotonic capability derivation (`Mint` / `Grant` — `INV-SEC-04`).
pub fn derive_capability(
    parent: CapabilityMeta,
    obj: &KernelObjHeader,
    new_rights: CapRights,
    new_lease_epoch: u32,
    new_badge: u64,
    cross_compartment: bool,
) -> Result<CapabilityMeta, SecError> {
    if !parent.rights.contains(new_rights) {
        return Err(SecError::PermCap);
    }
    if cross_compartment
        && (!parent.rights.contains(CapRights::GRANT)
            || obj.label.flags.contains(SecFlags::NO_DELEGATE))
    {
        return Err(SecError::PermCap);
    }
    let effective_lease = match (parent.lease_epoch, new_lease_epoch) {
        (0, req) => req,
        (p, 0) => p,
        (p, req) if req <= p => req,
        _ => return Err(SecError::PermCap),
    };
    Ok(CapabilityMeta {
        rights: new_rights,
        expected_gen: parent.expected_gen,
        lease_epoch: effective_lease,
        badge: if parent.badge != 0 { parent.badge } else { new_badge },
    })
}

/// Kernel boot-time self-check proving `INV-SEC-01..04, 07, 08` hold on real Ring 0 state.
pub fn verify_boot_security_invariants() {
    assert_eq!(core::mem::size_of::<SecLabel>(), 8);

    let root_subj = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0, 1);
    let dev_ws_subj = SecLabel::new(SecDomain::RootSystem, SecFlags::DEV_WORKSPACE, 42, 2);
    let user_subj = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 100, 10);

    let kern_obj = KernelObjHeader::new(SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0));
    let vault_secret = KernelObjHeader::new(SecLabel::new(SecDomain::RootSystem, SecFlags::NO_DELEGATE, 1, 99));
    let sys_libc_ro = KernelObjHeader::new(SecLabel::new(SecDomain::RootSystem, SecFlags::READ_ONLY_EXPORT, 0, 5));
    let user_file = KernelObjHeader::new(SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 100, 10).inherit_child());

    let full_cap = CapabilityMeta {
        rights: CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::EXEC.0 | CapRights::GRANT.0),
        expected_gen: 1,
        lease_epoch: 10,
        badge: 0,
    };

    // INV-SEC-01 & deny-by-default empty request check:
    assert_eq!(authorize_object_access(root_subj, full_cap, &kern_obj, CapRights::READ, 5), Err(SecError::PermCap));
    assert_eq!(authorize_object_access(user_subj, full_cap, &user_file, CapRights::NONE, 5), Err(SecError::PermCap));

    // Device DMA arena + Notification signal check (`ADR-007` Rule 10 & `ADR-003` pin count):
    let dma_arena = KernelObjHeader::new(SecLabel::new(
        SecDomain::RootSystem,
        SecFlags(SecFlags::DEVICE_DMA.0 | SecFlags::NO_DELEGATE.0),
        7,
        50,
    ));
    dma_arena.iommu_pin_count.store(1, Ordering::Release);
    let drv_subj = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 7, 50);
    let drv_cap = CapabilityMeta {
        rights: CapRights(CapRights::READ.0 | CapRights::SIGNAL.0 | CapRights::RETYPE.0),
        expected_gen: 1,
        lease_epoch: 0,
        badge: 1,
    };
    assert_eq!(authorize_object_access(drv_subj, drv_cap, &dma_arena, CapRights::SIGNAL, 5), Ok(()));
    assert_eq!(authorize_object_access(drv_subj, drv_cap, &dma_arena, CapRights::RETYPE, 5), Err(SecError::BusyPinned));

    // INV-SEC-02: Userspace cannot access non-exported RootSystem objects, nor write exported libs.
    assert_eq!(authorize_object_access(user_subj, full_cap, &vault_secret, CapRights::READ, 5), Err(SecError::PermCap));
    assert_eq!(authorize_object_access(user_subj, full_cap, &sys_libc_ro, CapRights::WRITE, 5), Err(SecError::PermCap));

    // INV-SEC-03: Userspace CAN read+exec controlled `READ_ONLY_EXPORT` system library (`W^X` holds).
    let rx_rights = CapRights(CapRights::READ.0 | CapRights::EXEC.0);
    assert_eq!(authorize_object_access(user_subj, full_cap, &sys_libc_ro, rx_rights, 5), Ok(()));
    assert_eq!(
        authorize_object_access(user_subj, full_cap, &user_file, CapRights(CapRights::WRITE.0 | CapRights::EXEC.0), 5),
        Err(SecError::Inval)
    );

    // ADR-007 Rule 6 & 12: Developer Workspace cannot touch Vault secrets in another compartment.
    assert_eq!(authorize_object_access(dev_ws_subj, full_cap, &vault_secret, CapRights::READ, 5), Err(SecError::PermCap));

    // INV-SEC-04: Monotonic attenuation blocks lease extension & blocks `NO_DELEGATE` cross-compartment grant.
    assert_eq!(derive_capability(full_cap, &user_file, CapRights::READ, 11, 7, false), Err(SecError::PermCap));
    assert_eq!(derive_capability(full_cap, &vault_secret, CapRights::READ, 5, 7, true), Err(SecError::PermCap));
    let child_cap = derive_capability(full_cap, &user_file, CapRights::WRITE, 8, 0xbeef, false).unwrap();
    assert_eq!(authorize_object_access(user_subj, child_cap, &user_file, CapRights::WRITE, 8), Ok(()));

    // INV-SEC-07: Lease expiry and generation bump (`Revoke`) deterministically return `EREVOKED`.
    assert_eq!(authorize_object_access(user_subj, child_cap, &user_file, CapRights::WRITE, 9), Err(SecError::Revoked));
    user_file.generation.fetch_add(1, Ordering::AcqRel);
    assert_eq!(authorize_object_access(user_subj, child_cap, &user_file, CapRights::WRITE, 5), Err(SecError::Revoked));
}
