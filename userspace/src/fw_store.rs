// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Read-only SHA-256 Firmware Store (`dry-fw-store`) for Ring-3 Driver & MicroVM Qualification
//! (`Milestone 4`). Zero dynamic allocation (`no_std`); verifies every lookup via streaming SHA-256.

use crate::broker_lifecycle::verify_broker_lifecycle_15_stages;
use crate::shell::LineBuilder;
use crate::vault_crypto::{ct_eq_32, Sha256};
use crate::{sys_write_debug, EINVAL, ENOSPC};

pub const MAX_FW_ENTRIES: usize = 8;
pub const MAX_FW_BLOB_BYTES: usize = 16 * 1024 * 1024;
const STREAM_CHUNK_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FwVerifyError {
    NotFound,
    EmptyBlob,
    SizeExceeded,
    DigestMismatch,
}

#[derive(Clone, Copy)]
pub struct FwEntry<'a> {
    pub name: &'a [u8],
    pub sha256: [u8; 32],
    pub blob: &'a [u8],
}

pub struct FwStore<'a> {
    entries: [Option<FwEntry<'a>>; MAX_FW_ENTRIES],
    count: usize,
}

impl<'a> FwStore<'a> {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_FW_ENTRIES],
            count: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn register(
        &mut self,
        name: &'a [u8],
        expected_sha256: [u8; 32],
        blob: &'a [u8],
    ) -> Result<(), isize> {
        if name.is_empty() || blob.is_empty() || blob.len() > MAX_FW_BLOB_BYTES {
            return Err(EINVAL);
        }
        if self.count >= MAX_FW_ENTRIES {
            return Err(ENOSPC);
        }
        self.entries[self.count] = Some(FwEntry {
            name,
            sha256: expected_sha256,
            blob,
        });
        self.count += 1;
        Ok(())
    }

    /// Look up `name` read-only and verify its entire payload in bounded streaming chunks via SHA-256.
    pub fn lookup_verified(&self, name: &[u8]) -> Result<&'a [u8], FwVerifyError> {
        for slot in self.entries.iter().take(self.count) {
            let Some(entry) = slot else {
                continue;
            };
            if entry.name != name {
                continue;
            }
            if entry.blob.is_empty() {
                return Err(FwVerifyError::EmptyBlob);
            }
            if entry.blob.len() > MAX_FW_BLOB_BYTES {
                return Err(FwVerifyError::SizeExceeded);
            }
            let mut hasher = Sha256::new();
            for chunk in entry.blob.chunks(STREAM_CHUNK_BYTES) {
                hasher.update(chunk);
            }
            let digest = hasher.finalize();
            if !ct_eq_32(&digest, &entry.sha256) {
                return Err(FwVerifyError::DigestMismatch);
            }
            return Ok(entry.blob);
        }
        Err(FwVerifyError::NotFound)
    }
}

impl Default for FwStore<'_> {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
pub fn sha256_streaming(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    for chunk in bytes.chunks(STREAM_CHUNK_BYTES) {
        h.update(chunk);
    }
    h.finalize()
}

/// Verify read-only SHA-256 firmware verification + tamper rejection + ENOENT handling.
pub fn verify_fw_store_invariants() -> Option<(usize, usize)> {
    const GOOD_GUC: &[u8] = b"DRYOS-INTEL-GUC-70.1.1-CSS-SYNTH-PAYLOAD-VALID";
    const GOOD_GSP: &[u8] = b"DRYOS-NVIDIA-GA10X-GSP-ELF64-SYNTH-PAYLOAD-VALID";
    const TAMPERED_GUC: &[u8] = b"DRYOS-INTEL-GUC-70.1.1-CSS-SYNTH-PAYLOAD-TAMPR";

    let guc_sha = sha256_streaming(GOOD_GUC);
    let gsp_sha = sha256_streaming(GOOD_GSP);

    let mut store = FwStore::new();
    if store.register(b"bad_empty.bin", guc_sha, &[]).is_ok() {
        return None;
    }
    store.register(b"tgl_guc_70.1.1.bin", guc_sha, GOOD_GUC).ok()?;
    store.register(b"gsp_ga10x.bin", gsp_sha, GOOD_GSP).ok()?;
    store.register(b"tgl_guc_tampered.bin", guc_sha, TAMPERED_GUC).ok()?;

    let got_guc = store.lookup_verified(b"tgl_guc_70.1.1.bin").ok()?;
    if got_guc != GOOD_GUC {
        return None;
    }
    let got_gsp = store.lookup_verified(b"gsp_ga10x.bin").ok()?;
    if got_gsp != GOOD_GSP {
        return None;
    }
    if store.lookup_verified(b"tgl_guc_tampered.bin") != Err(FwVerifyError::DigestMismatch) {
        return None;
    }
    if store.lookup_verified(b"nonexistent_ucode.bin") != Err(FwVerifyError::NotFound) {
        return None;
    }
    Some((store.len(), 2))
}

fn emit_line(lb: &LineBuilder) {
    let bytes = lb.as_bytes();
    let mut line = [0u8; 164];
    let n = bytes.len().min(line.len() - 1);
    line[..n].copy_from_slice(&bytes[..n]);
    line[n] = b'\n';
    let _ = sys_write_debug(&line[..=n]);
}

/// Emit compact `<140 B` verification lines for both `dry-fw-store` and 15-stage `BrokerLifecycle`.
pub fn log_fw_and_lifecycle_selftest() {
    if let Some((entries, verified)) = verify_fw_store_invariants() {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-FW-STORE] PASS: entries=");
        lb.push_u64(entries as u64);
        lb.push_bytes(b" verified=");
        lb.push_u64(verified as u64);
        lb.push_bytes(b" tamper_rej=1 enoent_rej=1");
        emit_line(&lb);
    } else {
        let _ = sys_write_debug(b"[DRY-FW-STORE] FAIL\n");
    }

    if let Some((mask, respawns)) = verify_broker_lifecycle_15_stages() {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-LIFECYCLE] PASS: steps=15 mask=0x");
        lb.push_hex_u64(u64::from(mask));
        lb.push_bytes(b" illegal_rej=1 respawns=");
        lb.push_u64(respawns);
        emit_line(&lb);
    } else {
        let _ = sys_write_debug(b"[DRY-LIFECYCLE] FAIL\n");
    }
}
