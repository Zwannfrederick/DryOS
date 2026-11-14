// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-vault` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0007`) `DRY Vault`
//! Secret & Lease Broker + Scoped `DEV_WORKSPACE` Confinement Probe (`Milestone 2B` — `ADR-007`
//! Rules 1–12, `INV-SEC-01..08`).
//!
//! Security Properties:
//! - Holds zero plaintext passphrases in memory or on disk: `/persist/vault.sec` (`RootSystem |
//!   NO_DELEGATE`) stores only a 16-byte salt + 32-byte `PBKDF2-HMAC-SHA256` verifier + `CRC32`.
//! - Constant-time verifier comparison (`ct_eq_32`) + volatile buffer scrubbing (`scrub_bytes`).
//! - On Session Lock (`VAULT_OP_LOCK_REVOKE`), calls `SYS_VAULT_CTL(VAULT_CTL_ADVANCE_EPOCH)` via
//!   `CapObjectKind::VaultCtrl`, invalidating all epoch-leased capabilities in $O(1)$ (`-EREVOKED`).
//! - Exports sanitized read-only status via `VAULT_CTL_SEAL_RO_EXPORT` (`READ_ONLY_EXPORT`, `W^X`).

#![no_std]
#![no_main]

use dry_userspace::shell::LineBuilder;
use dry_userspace::vault_crypto::{
    ct_eq_32, pbkdf2_hmac_sha256_32, scrub_bytes, sha256_digest,
};
use dry_userspace::vault_proto::{
    unpack_passphrase_mrs, unpack_vault_label, VaultDiskRecord, VAULT_DEFAULT_PASSPHRASE,
    VAULT_PBKDF2_ITERS, VAULT_REC_MAGIC,
};
use dry_userspace::vfs_proto::{
    vfs_create_file, vfs_link_file, vfs_read_file, vfs_write_small, VFS_FLAG_PRIVATE,
};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_query, sys_cap_revoke,
    sys_dev_map, sys_dma_alloc, sys_exit, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_ipc_reply_with_grant, sys_ipc_reply_with_grant_lease, sys_vault_ctl, sys_vault_query_epoch,
    sys_write_debug, UserCapInfo, UserIpcRecvExt, UserVirtioPciInfo, BADGE_VAULT_SHELL,
    BADGE_VAULT_SUP, BADGE_VFS_DEV_WS, BADGE_VFS_VAULT_ROOT, EEXIST, EINVAL, ENOENT, EPERM,
    IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, RIGHT_MAP, RIGHT_READ,
    RIGHT_RECV, VAULT_CTL_ADVANCE_EPOCH, VAULT_CTL_SEAL_RO_EXPORT, VAULT_OP_LOCK_REVOKE,
    VAULT_OP_MINT_LEASE, VAULT_OP_PASSWD, VAULT_OP_SEAL_RO_STATUS, VAULT_OP_STATUS,
    VAULT_OP_UNLOCK,
};

const VAULT_SEC_PATH: &[u8] = b"/persist/vault.sec";

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

/// Branch 1: Executed when `dry-init` spawns `dry-vault.elf` inside a `DEV_WORKSPACE` (`0x002a`)
/// compartment to prove hardware/kernel confinement (`ADR-007` Rule 6).
fn run_dev_workspace_confinement_probe(vfs_devws_cap: u64) -> ! {
    let mut dummy_pci = UserVirtioPciInfo::default();
    let dma_eperm = sys_dma_alloc(0, 0x2300_0000) == Err(EPERM);
    let devmap_eperm = sys_dev_map(0, 0x2300_0000, &mut dummy_pci) == Err(EPERM);
    let vaultctl_eperm = sys_vault_ctl(0, VAULT_CTL_ADVANCE_EPOCH, 0, 0) == Err(EPERM);

    let mut sec_buf = [0u8; 80];
    let vault_sec_eperm = vfs_read_file(vfs_devws_cap, VAULT_SEC_PATH, 0, &mut sec_buf) == Err(EPERM);
    let link_eperm =
        vfs_link_file(vfs_devws_cap, VAULT_SEC_PATH, b"/persist/devws_leak") == Err(EPERM);

    let mut ro_buf = [0u8; 64];
    let ro_read_ok = match vfs_read_file(vfs_devws_cap, b"/persist/README.txt", 0, &mut ro_buf) {
        Ok((n, _)) => n > 0,
        Err(_) => false,
    };
    let _ = sys_cap_revoke(vfs_devws_cap, 1);

    if dma_eperm
        && devmap_eperm
        && vaultctl_eperm
        && vault_sec_eperm
        && link_eperm
        && ro_read_ok
    {
        let _ = sys_write_debug(
            b"[DRY-P4-DEVWS] PASS: dma_eperm=true devmap_eperm=true vault_sec_eperm=true ro_read_ok=true\n",
        );
        sys_exit(0);
    }
    sys_exit(91);
}

struct VaultBroker {
    vault_ctrl_cap: u64,
    vfs_cap: u64,
    ro_export_cap: u64,
    rec: VaultDiskRecord,
    persistent_backed: bool,
    unlocked: bool,
    auth_failures: u32,
    unlock_count: u32,
    revoke_count: u32,
}

impl VaultBroker {
    fn new_in_memory_default(vault_ctrl_cap: u64) -> Self {
        let salt = Self::derive_salt(1);
        let verifier =
            pbkdf2_hmac_sha256_32(VAULT_DEFAULT_PASSPHRASE, &salt, VAULT_PBKDF2_ITERS);
        let mut rec = VaultDiskRecord {
            magic: VAULT_REC_MAGIC,
            version: 1,
            pbkdf2_iters: VAULT_PBKDF2_ITERS,
            generation: 1,
            salt,
            verifier,
            crc32: 0,
        };
        rec.crc32 = rec.compute_crc32();
        Self {
            vault_ctrl_cap,
            vfs_cap: 0,
            ro_export_cap: 0,
            rec,
            persistent_backed: false,
            unlocked: true,
            auth_failures: 0,
            unlock_count: 1,
            revoke_count: 0,
        }
    }

    fn derive_salt(generation: u32) -> [u8; 16] {
        let mut seed = [0u8; 20];
        seed[0..8].copy_from_slice(b"DRYVLTSL");
        seed[8..16].copy_from_slice(&rdtsc_user().to_le_bytes());
        seed[16..20].copy_from_slice(&generation.to_le_bytes());
        let digest = sha256_digest(&seed);
        let mut salt = [0u8; 16];
        salt.copy_from_slice(&digest[0..16]);
        salt
    }

    fn bind_vfs_and_load(&mut self, vfs_cap: u64) {
        if self.vfs_cap != 0 {
            let _ = sys_cap_revoke(self.vfs_cap, 1);
        }
        self.vfs_cap = vfs_cap;
        let mut raw = [0u8; 96];
        if let Ok((got, _)) = vfs_read_file(vfs_cap, VAULT_SEC_PATH, 0, &mut raw) {
            if let Some(loaded) = VaultDiskRecord::from_bytes(&raw[..got]) {
                self.rec = loaded;
                self.persistent_backed = true;
                scrub_bytes(&mut raw);
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VAULT] Loaded sealed /persist/vault.sec gen=");
                lb.push_u64(self.rec.generation as u64);
                log_line(&lb);
                return;
            }
            // `AUD-U7`: never silently overwrite a corrupt on-disk vault record with defaults.
            scrub_bytes(&mut raw);
            self.unlocked = false;
            self.persistent_backed = true;
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-VAULT-ERR] Corrupt /persist/vault.sec; failing closed");
            log_line(&lb);
            return;
        }
        scrub_bytes(&mut raw);
        if let Err(e) = self.persist_record() {
            if e != ENOENT {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VAULT-WARN] persist_record failed neg_errno=");
                lb.push_u64((-e) as u64);
                log_line(&lb);
            }
        }
    }

    fn persist_record(&mut self) -> Result<(), isize> {
        if self.vfs_cap == 0 {
            return Ok(());
        }
        match vfs_create_file(self.vfs_cap, VAULT_SEC_PATH, VFS_FLAG_PRIVATE) {
            Ok(_) | Err(EEXIST) => {}
            Err(e) => return Err(e),
        }
        let mut bytes = self.rec.to_bytes();
        let res = vfs_write_small(self.vfs_cap, VAULT_SEC_PATH, &bytes, false);
        scrub_bytes(&mut bytes);
        let _ = res?;
        self.persistent_backed = true;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-VAULT] Sealed /persist/vault.sec (NO_DELEGATE) gen=");
        lb.push_u64(self.rec.generation as u64);
        log_line(&lb);
        Ok(())
    }

    fn verify_passphrase(&self, candidate: &[u8]) -> bool {
        let mut derived =
            pbkdf2_hmac_sha256_32(candidate, &self.rec.salt, self.rec.pbkdf2_iters);
        let ok = ct_eq_32(&derived, &self.rec.verifier);
        scrub_bytes(&mut derived);
        ok
    }

    fn ensure_ro_export_cap(&mut self) -> Result<u64, isize> {
        if self.ro_export_cap != 0 {
            return Ok(self.ro_export_cap);
        }
        let epoch = sys_vault_query_epoch().unwrap_or(1);
        let mut hdr = [0u8; 64];
        hdr[0..8].copy_from_slice(b"DRYVLTRO");
        hdr[8..12].copy_from_slice(&1u32.to_le_bytes());
        hdr[12..16].copy_from_slice(&epoch.to_le_bytes());
        hdr[16..20].copy_from_slice(&self.rec.generation.to_le_bytes());
        let (_, cap) = sys_vault_ctl(
            self.vault_ctrl_cap,
            VAULT_CTL_SEAL_RO_EXPORT,
            hdr.as_ptr() as u64,
            hdr.len() as u64,
        )?;
        self.ro_export_cap = cap;
        Ok(cap)
    }
}

fn reply_err(ep: u64, tok: u64, lbl: u32, err: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(ep, tok, pack_msg_tag(lbl, 0, 1, 0), [err as u64, 0, 0, 0]);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(arg0: u64, arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || arg0 == 0 {
        sys_exit(190);
    }

    let mut ci = UserCapInfo::default();
    if sys_cap_query(arg0, &mut ci) == 0
        && (ci.rights_bits & (RIGHT_RECV as u32)) == 0
        && ci.badge == BADGE_VFS_DEV_WS
    {
        run_dev_workspace_confinement_probe(arg0);
    }

    let srv_ep_cap = arg0;
    let vault_ctrl_cap = arg1;
    if vault_ctrl_cap == 0 {
        sys_exit(191);
    }

    let mut broker = VaultBroker::new_in_memory_default(vault_ctrl_cap);
    let _ = sys_write_debug(b"[DRY-VAULT] Ready in CPL3 RootSystem (PBKDF2-HMAC-SHA256 + Lease Epoch)\n");

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, tok, mrs) = match sys_ipc_recv_ext(srv_ep_cap, 0, 0, &mut ext) {
            Ok(v) => v,
            Err(_) => break,
        };
        let (op, label_meta) = unpack_vault_label(msg_tag_label(tag));

        match op {
            VAULT_OP_STATUS => {
                if ext.transferred_cap != 0 {
                    if ext.sender_badge == BADGE_VAULT_SUP
                        && mrs[0] == 1
                        && broker.vfs_cap == 0
                    {
                        let mut vci = UserCapInfo::default();
                        let qrc = sys_cap_query(ext.transferred_cap, &mut vci);
                        if qrc == 0 && vci.badge == BADGE_VFS_VAULT_ROOT {
                            broker.bind_vfs_and_load(ext.transferred_cap);
                        } else {
                            let mut lb = LineBuilder::new();
                            lb.push_bytes(b"[DRY-VAULT-WARN] vfs_cap query rejected qrc=");
                            lb.push_u64(qrc as u64);
                            lb.push_bytes(b" badge=0x");
                            lb.push_hex_u64(vci.badge);
                            log_line(&lb);
                            let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        }
                    } else {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                }
                let epoch = sys_vault_query_epoch().unwrap_or(1);
                let flags = u64::from(broker.unlocked)
                    | (u64::from(broker.persistent_backed) << 1);
                let counts = (broker.auth_failures as u64)
                    | ((broker.unlock_count as u64) << 32);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [flags, epoch as u64, counts, broker.revoke_count as u64],
                    );
                }
            }
            VAULT_OP_UNLOCK => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                let plen = label_meta;
                let mut pass_buf = [0u8; 32];
                let Ok(n) = unpack_passphrase_mrs(plen, mrs, &mut pass_buf) else {
                    scrub_bytes(&mut pass_buf);
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                };
                let ok = broker.verify_passphrase(&pass_buf[..n]);
                scrub_bytes(&mut pass_buf);

                let epoch = sys_vault_query_epoch().unwrap_or(1);
                if ok {
                    broker.unlocked = true;
                    broker.unlock_count = broker.unlock_count.wrapping_add(1);
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-VAULT] UNLOCK ok=true epoch=");
                    lb.push_u64(epoch as u64);
                    log_line(&lb);
                    if tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                            [epoch as u64, broker.unlock_count as u64, 0, 0],
                        );
                    }
                } else {
                    broker.auth_failures = broker.auth_failures.wrapping_add(1);
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-VAULT] UNLOCK ok=false failures=");
                    lb.push_u64(broker.auth_failures as u64);
                    log_line(&lb);
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                }
            }
            VAULT_OP_LOCK_REVOKE => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                if ext.sender_badge != BADGE_VAULT_SUP && ext.sender_badge != BADGE_VAULT_SHELL {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                match sys_vault_ctl(broker.vault_ctrl_cap, VAULT_CTL_ADVANCE_EPOCH, 0, 0) {
                    Ok((_, new_epoch)) => {
                        broker.unlocked = false;
                        broker.revoke_count = broker.revoke_count.wrapping_add(1);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-VAULT] LOCK_REVOKE new_epoch=");
                        lb.push_u64(new_epoch);
                        lb.push_bytes(b" revokes=");
                        lb.push_u64(broker.revoke_count as u64);
                        log_line(&lb);
                        if tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                                [new_epoch, broker.revoke_count as u64, 0, 0],
                            );
                        }
                    }
                    Err(e) => reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, e),
                }
            }
            VAULT_OP_PASSWD => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                if ext.sender_badge != BADGE_VAULT_SUP && ext.sender_badge != BADGE_VAULT_SHELL {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let old_len = label_meta & 0xff;
                let new_len = (label_meta >> 8) & 0x7f;
                if old_len == 0 || old_len > 16 || new_len == 0 || new_len > 16 {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                let mut raw = [0u8; 32];
                raw[0..8].copy_from_slice(&mrs[0].to_le_bytes());
                raw[8..16].copy_from_slice(&mrs[1].to_le_bytes());
                raw[16..24].copy_from_slice(&mrs[2].to_le_bytes());
                raw[24..32].copy_from_slice(&mrs[3].to_le_bytes());

                let old_ok = broker.verify_passphrase(&raw[0..old_len]);
                if !old_ok {
                    scrub_bytes(&mut raw);
                    broker.auth_failures = broker.auth_failures.wrapping_add(1);
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }

                let next_gen = broker.rec.generation.wrapping_add(1).max(2);
                let salt = VaultBroker::derive_salt(next_gen);
                let verifier = pbkdf2_hmac_sha256_32(
                    &raw[16..(16 + new_len)],
                    &salt,
                    VAULT_PBKDF2_ITERS,
                );
                scrub_bytes(&mut raw);

                let mut next_rec = VaultDiskRecord {
                    magic: VAULT_REC_MAGIC,
                    version: 1,
                    pbkdf2_iters: VAULT_PBKDF2_ITERS,
                    generation: next_gen,
                    salt,
                    verifier,
                    crc32: 0,
                };
                next_rec.crc32 = next_rec.compute_crc32();
                broker.rec = next_rec;
                if let Err(e) = broker.persist_record() {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, e);
                    continue;
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [next_gen as u64, 0, 0, 0],
                    );
                }
            }
            VAULT_OP_MINT_LEASE => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                if !broker.unlocked {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let epoch = sys_vault_query_epoch().unwrap_or(1);
                match broker.ensure_ro_export_cap() {
                    Ok(ro_cap) => {
                        if tok != 0 {
                            let _ = sys_ipc_reply_with_grant_lease(
                                srv_ep_cap,
                                tok,
                                IPC_OP_PONG,
                                [epoch as u64, 0, 0, 0],
                                ro_cap,
                                RIGHT_READ | RIGHT_MAP,
                                0,
                                epoch,
                            );
                        }
                    }
                    Err(e) => reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, e),
                }
            }
            VAULT_OP_SEAL_RO_STATUS => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                match broker.ensure_ro_export_cap() {
                    Ok(ro_cap) => {
                        if tok != 0 {
                            let _ = sys_ipc_reply_with_grant(
                                srv_ep_cap,
                                tok,
                                IPC_OP_PONG,
                                [1, 0, 0, 0],
                                ro_cap,
                                RIGHT_READ | RIGHT_MAP,
                                0,
                            );
                        }
                    }
                    Err(e) => reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, e),
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if ext.sender_badge != BADGE_VAULT_SUP {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if broker.ro_export_cap != 0 {
                    let _ = sys_cap_revoke(broker.ro_export_cap, 2);
                    broker.ro_export_cap = 0;
                }
                if broker.vfs_cap != 0 {
                    let _ = sys_cap_revoke(broker.vfs_cap, 1);
                    broker.vfs_cap = 0;
                }
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VAULT-STATS] unlocks=");
                lb.push_u64(broker.unlock_count as u64);
                lb.push_bytes(b" failures=");
                lb.push_u64(broker.auth_failures as u64);
                lb.push_bytes(b" revokes=");
                lb.push_u64(broker.revoke_count as u64);
                log_line(&lb);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [broker.unlock_count as u64, broker.revoke_count as u64, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
            }
        }
    }
    sys_exit(0);
}
