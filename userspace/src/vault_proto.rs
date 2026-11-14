// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `DRY Vault` Wire Protocol, Persistent Sealed Record (`vault.sec`), & Client Wrappers
//! (`Milestone 2B` — `ADR-007` Rules 3, 5, 12 & `INV-SEC-01..08`).

use core::mem::size_of;

use crate::vault_crypto::{crc32_ieee, scrub_bytes};
use crate::{
    msg_tag_label, pack_msg_tag, sys_ipc_call_inline, sys_ipc_call_with_grant, EINVAL, EPERM,
    IPC_OP_ERR_PERM, IPC_OP_PONG, VAULT_OP_LOCK_REVOKE, VAULT_OP_MINT_LEASE, VAULT_OP_PASSWD,
    VAULT_OP_SEAL_RO_STATUS, VAULT_OP_STATUS, VAULT_OP_UNLOCK,
};

pub const VAULT_REC_MAGIC: [u8; 8] = *b"DRYVLT01";
pub const VAULT_PBKDF2_ITERS: u32 = 1024;
pub const VAULT_DEFAULT_PASSPHRASE: &[u8] = b"dryos";
pub const VAULT_MAX_PASS_LEN: usize = 32;

/// Persistent `72`-byte record stored in `/persist/vault.sec` (`RootSystem | NO_DELEGATE`).
/// Never stores plaintext passphrases — only `16`-byte salt + `PBKDF2-HMAC-SHA256` verifier + `CRC32`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VaultDiskRecord {
    pub magic: [u8; 8],
    pub version: u32,
    pub pbkdf2_iters: u32,
    pub generation: u32,
    pub salt: [u8; 16],
    pub verifier: [u8; 32],
    pub crc32: u32,
}

const _: () = assert!(size_of::<VaultDiskRecord>() == 72);

impl VaultDiskRecord {
    pub fn compute_crc32(&self) -> u32 {
        let mut copy = *self;
        copy.crc32 = 0;
        // SAFETY: `VaultDiskRecord` is `#[repr(C)]` POD (`72` bytes) with zero uninitialized padding.
        let bytes = unsafe {
            core::slice::from_raw_parts((&raw const copy).cast::<u8>(), size_of::<Self>())
        };
        crc32_ieee(bytes)
    }

    pub fn is_valid(&self) -> bool {
        self.magic == VAULT_REC_MAGIC
            && self.version == 1
            && (256..=65_536).contains(&self.pbkdf2_iters)
            && self.crc32 == self.compute_crc32()
    }

    pub fn to_bytes(&self) -> [u8; 72] {
        // SAFETY: `size_of::<Self>() == 72` POD.
        unsafe { core::mem::transmute::<Self, [u8; 72]>(*self) }
    }

    pub fn from_bytes(raw: &[u8]) -> Option<Self> {
        if raw.len() < size_of::<Self>() {
            return None;
        }
        // SAFETY: `raw` has at least `72` bytes; `read_unaligned` into POD struct.
        let rec = unsafe { core::ptr::read_unaligned(raw.as_ptr().cast::<Self>()) };
        if rec.is_valid() {
            Some(rec)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct VaultStatusInfo {
    pub unlocked: bool,
    pub persistent_backed: bool,
    pub lease_epoch: u32,
    pub auth_failures: u32,
    pub unlock_count: u32,
    pub revoke_count: u32,
}

#[inline(always)]
pub const fn pack_vault_label(op: u32, meta15: u16) -> u32 {
    (op & 0xffff) | (((meta15 & 0x7fff) as u32) << 16)
}

#[inline(always)]
pub const fn unpack_vault_label(raw_label: u32) -> (u32, usize) {
    (raw_label & 0xffff, ((raw_label >> 16) & 0x7fff) as usize)
}

pub fn pack_passphrase_mrs(pass: &[u8]) -> Result<(usize, [u64; 4]), isize> {
    if pass.is_empty() || pass.len() > VAULT_MAX_PASS_LEN {
        return Err(EINVAL);
    }
    let mut raw = [0u8; 32];
    raw[..pass.len()].copy_from_slice(pass);
    let mrs = [
        u64::from_le_bytes([raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7]]),
        u64::from_le_bytes([
            raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
        ]),
        u64::from_le_bytes([
            raw[16], raw[17], raw[18], raw[19], raw[20], raw[21], raw[22], raw[23],
        ]),
        u64::from_le_bytes([
            raw[24], raw[25], raw[26], raw[27], raw[28], raw[29], raw[30], raw[31],
        ]),
    ];
    scrub_bytes(&mut raw);
    Ok((pass.len(), mrs))
}

pub fn unpack_passphrase_mrs(len: usize, mrs: [u64; 4], out: &mut [u8; 32]) -> Result<usize, isize> {
    if len == 0 || len > VAULT_MAX_PASS_LEN {
        return Err(EINVAL);
    }
    out[0..8].copy_from_slice(&mrs[0].to_le_bytes());
    out[8..16].copy_from_slice(&mrs[1].to_le_bytes());
    out[16..24].copy_from_slice(&mrs[2].to_le_bytes());
    out[24..32].copy_from_slice(&mrs[3].to_le_bytes());
    for b in &mut out[len..] {
        *b = 0;
    }
    Ok(len)
}

pub fn vault_query_status(vault_ep: u64) -> Result<VaultStatusInfo, isize> {
    let (rtag, _, rmrs) =
        sys_ipc_call_inline(vault_ep, pack_msg_tag(VAULT_OP_STATUS, 0, 0, 0), [0; 4], 0)?;
    if msg_tag_label(rtag) == IPC_OP_ERR_PERM {
        return Err(EPERM);
    }
    if msg_tag_label(rtag) != IPC_OP_PONG {
        return Err(EINVAL);
    }
    Ok(VaultStatusInfo {
        unlocked: (rmrs[0] & 1) != 0,
        persistent_backed: (rmrs[0] & 2) != 0,
        lease_epoch: rmrs[1] as u32,
        auth_failures: (rmrs[2] & 0xffff_ffff) as u32,
        unlock_count: (rmrs[2] >> 32) as u32,
        revoke_count: rmrs[3] as u32,
    })
}

pub fn vault_unlock(vault_ep: u64, pass: &[u8]) -> Result<u32, isize> {
    let (plen, mut mrs) = pack_passphrase_mrs(pass)?;
    let res = sys_ipc_call_inline(
        vault_ep,
        pack_msg_tag(pack_vault_label(VAULT_OP_UNLOCK, plen as u16), 0, 4, 0),
        mrs,
        0,
    );
    mrs.fill(0);
    let (rtag, _, rmrs) = res?;
    if msg_tag_label(rtag) == IPC_OP_ERR_PERM {
        return Err(EPERM);
    }
    if msg_tag_label(rtag) != IPC_OP_PONG {
        return Err(EINVAL);
    }
    Ok(rmrs[0] as u32)
}

pub fn vault_lock_and_revoke(vault_ep: u64) -> Result<u32, isize> {
    let (rtag, _, rmrs) = sys_ipc_call_inline(
        vault_ep,
        pack_msg_tag(VAULT_OP_LOCK_REVOKE, 0, 0, 0),
        [0; 4],
        0,
    )?;
    if msg_tag_label(rtag) == IPC_OP_ERR_PERM {
        return Err(EPERM);
    }
    if msg_tag_label(rtag) != IPC_OP_PONG {
        return Err(EINVAL);
    }
    Ok(rmrs[0] as u32)
}

/// Change the Vault passphrase (`old_pass` <= 16 bytes in `mrs[0..2]`, `new_pass` <= 16 bytes in `mrs[2..4]`).
pub fn vault_change_passphrase(
    vault_ep: u64,
    old_pass: &[u8],
    new_pass: &[u8],
) -> Result<u32, isize> {
    if old_pass.is_empty() || old_pass.len() > 16 || new_pass.is_empty() || new_pass.len() > 16 {
        return Err(EINVAL);
    }
    let mut buf = [0u8; 32];
    buf[..old_pass.len()].copy_from_slice(old_pass);
    buf[16..16 + new_pass.len()].copy_from_slice(new_pass);
    let mut mrs = [
        u64::from_le_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]]),
        u64::from_le_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
        ]),
        u64::from_le_bytes([
            buf[16], buf[17], buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
        ]),
        u64::from_le_bytes([
            buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
        ]),
    ];
    scrub_bytes(&mut buf);
    let lens = ((new_pass.len() as u16) << 8) | (old_pass.len() as u16);
    let res = sys_ipc_call_inline(
        vault_ep,
        pack_msg_tag(pack_vault_label(VAULT_OP_PASSWD, lens), 0, 4, 0),
        mrs,
        0,
    );
    mrs.fill(0);
    let (rtag, _, rmrs) = res?;
    if msg_tag_label(rtag) == IPC_OP_ERR_PERM {
        return Err(EPERM);
    }
    if msg_tag_label(rtag) != IPC_OP_PONG {
        return Err(EINVAL);
    }
    Ok(rmrs[0] as u32)
}

/// Request an epoch-leased capability from `dry-vault` (`grant_lease == current_lease_epoch`).
/// Once `VAULT_OP_LOCK_REVOKE` advances `GLOBAL_LEASE_EPOCH`, Ring 0 rejects the capability with `-EREVOKED`.
pub fn vault_mint_leased_cap(vault_ep: u64) -> Result<(u64, u32), isize> {
    let (rtag, cap, rmrs) =
        sys_ipc_call_with_grant(vault_ep, VAULT_OP_MINT_LEASE, [0; 4], 0, 0, 0, 0)?;
    if msg_tag_label(rtag) == IPC_OP_ERR_PERM {
        return Err(EPERM);
    }
    if msg_tag_label(rtag) != IPC_OP_PONG || cap == 0 {
        return Err(EINVAL);
    }
    Ok((cap, rmrs[0] as u32))
}

/// Request a sealed `READ_ONLY_EXPORT` 4 KiB frame capability (`INV-SEC-03`) holding sanitized Vault status.
pub fn vault_get_sealed_ro_status_cap(vault_ep: u64) -> Result<u64, isize> {
    let (rtag, cap, _) =
        sys_ipc_call_with_grant(vault_ep, VAULT_OP_SEAL_RO_STATUS, [0; 4], 0, 0, 0, 0)?;
    if msg_tag_label(rtag) == IPC_OP_ERR_PERM {
        return Err(EPERM);
    }
    if msg_tag_label(rtag) != IPC_OP_PONG || cap == 0 {
        return Err(EINVAL);
    }
    Ok(cap)
}
