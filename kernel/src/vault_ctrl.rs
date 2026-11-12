// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated `DRY Vault` Kernel Lease Epoch & `READ_ONLY_EXPORT` Sealing Mechanism
//! (`Milestone 2B` — `ADR-007` Rules 3, 6, 12 & `INV-SEC-03`, `INV-SEC-07`).
//!
//! Keeps Ring 0 free of cryptographic policy, passphrase storage, or key hierarchies:
//! - `VAULT_CTL_QUERY_EPOCH (0)`: Unprivileged read of `GLOBAL_LEASE_EPOCH` (`(0, epoch)`).
//! - `VAULT_CTL_ADVANCE_EPOCH (1)`: Authorized `CapObjectKind::VaultCtrl` (`MANAGE | REVOKE` in
//!   non-`DEV_WORKSPACE` `RootSystem`) atomically increments `GLOBAL_LEASE_EPOCH`, invalidating
//!   all capabilities carrying `lease_epoch < new_epoch` in $O(1)$ (`SecError::Revoked`).
//! - `VAULT_CTL_SEAL_RO_EXPORT (2)`: Authorized `VaultCtrl` (`MANAGE`) seals a 4 KiB
//!   `READ_ONLY_EXPORT` frame directly from user memory (`(0, frame_cap.0)`), stripping all
//!   `WRITE` rights permanently (`INV-SEC-03`).

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::objects::cspace_create_readonly_export_from_user;
use crate::cspace::{insert_root_cap, with_pinned_cap};
use crate::ipc::map_cap_err_to_sys;
use crate::security::{
    current_lease_epoch, set_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel,
};
use crate::uaccess::SysError;

pub const VAULT_CTL_QUERY_EPOCH: u64 = 0;
pub const VAULT_CTL_ADVANCE_EPOCH: u64 = 1;
pub const VAULT_CTL_SEAL_RO_EXPORT: u64 = 2;

const VAULT_OBJ_ID: u32 = 1;
static VAULT_OBJ_GEN: AtomicU32 = AtomicU32::new(1);
static VAULT_DENIED_OPS: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn is_vault_cap_live(obj_id: u32, expected_gen: u32) -> bool {
    obj_id == VAULT_OBJ_ID
        && expected_gen != 0
        && VAULT_OBJ_GEN.load(Ordering::Acquire) == expected_gen
}

#[inline]
pub fn vault_denied_count() -> u64 {
    VAULT_DENIED_OPS.load(Ordering::Acquire)
}

/// Mint the root `CapObjectKind::VaultCtrl` capability into `dry-init`'s `RootSystem` `CSpace`.
pub fn mint_root_vault_cap(cspace_id: CSpaceId, root_subj: SecLabel) -> Result<CapPtr, CapError> {
    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    let gen = VAULT_OBJ_GEN.load(Ordering::Acquire);
    insert_root_cap(
        cspace_id,
        CapObjectKind::VaultCtrl,
        VAULT_OBJ_ID,
        gen,
        CapRights::VAULT_ROOT,
        root_subj,
        0,
        0,
    )
}

fn verify_vault_cap(
    cspace_id: CSpaceId,
    subj: SecLabel,
    cap_raw: u64,
    req: CapRights,
) -> Result<(), SysError> {
    if subj.domain != SecDomain::RootSystem || subj.flags.contains(SecFlags::DEV_WORKSPACE) {
        VAULT_DENIED_OPS.fetch_add(1, Ordering::Relaxed);
        return Err(SysError::Perm);
    }
    let now = current_lease_epoch();
    with_pinned_cap(cspace_id, subj, CapPtr(cap_raw), req, now, |resolved| {
        if resolved.obj_kind != CapObjectKind::VaultCtrl
            || !is_vault_cap_live(resolved.obj_id, resolved.expected_obj_gen)
        {
            return Err(CapError::WrongObjectKind);
        }
        Ok(())
    })
    .map_err(|e| {
        VAULT_DENIED_OPS.fetch_add(1, Ordering::Relaxed);
        map_cap_err_to_sys(e)
    })
}

/// `SYS_VAULT_CTL` (`31`) cold control-plane entry.
#[inline(never)]
pub fn sys_vault_ctl(
    cspace_id: CSpaceId,
    subj: SecLabel,
    vault_cap_raw: u64,
    op: u64,
    arg0: u64,
    arg1: u64,
) -> Result<(isize, u64), SysError> {
    match op {
        VAULT_CTL_QUERY_EPOCH => {
            if vault_cap_raw != 0 || arg0 != 0 || arg1 != 0 {
                return Err(SysError::Inval);
            }
            Ok((0, current_lease_epoch() as u64))
        }
        VAULT_CTL_ADVANCE_EPOCH => {
            if arg0 != 0 || arg1 != 0 {
                return Err(SysError::Inval);
            }
            let req = CapRights(CapRights::MANAGE.0 | CapRights::REVOKE.0);
            verify_vault_cap(cspace_id, subj, vault_cap_raw, req)?;
            let next = current_lease_epoch().saturating_add(1).max(2);
            set_lease_epoch(next);
            Ok((0, next as u64))
        }
        VAULT_CTL_SEAL_RO_EXPORT => {
            verify_vault_cap(cspace_id, subj, vault_cap_raw, CapRights::MANAGE)?;
            let cap = cspace_create_readonly_export_from_user(
                cspace_id,
                subj,
                arg0,
                arg1 as usize,
            )?;
            Ok((0, cap.0))
        }
        _ => Err(SysError::Inval),
    }
}
