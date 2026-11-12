// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `DRYFS_v1` Crash-Safe Dual-Superblock Copy-On-Write Persistent Filesystem (`Milestone 2B` —
//! `ADR-006`, `ADR-007` Rule 5, `INV-SEC-05`, `R-05`).
//!
//! Disk Layout (`4 KiB = 8 * 512 B` blocks):
//! - Block `0`: Superblock Slot A (`DryFsSuperblock`, `tx_seq`, `CRC32`, + `24` directory slots)
//! - Block `1`: Superblock Slot B (`DryFsSuperblock`, `tx_seq`, `CRC32`, + `24` directory slots)
//! - Blocks `2..total_blocks_4k`: Copy-On-Write 4 KiB file data blocks (never overwrites any block
//!   referenced by the active committed superblock!).
//!
//! Crash-Safe Commit Protocol:
//! 1. Write new/updated 4 KiB data block to a free CoW block (`>= 2`) + `VIRTIO_BLK_T_FLUSH` barrier.
//! 2. Advance `tx_seq += 1`, compute IEEE `CRC32`, write inactive Superblock (`Slot (tx_seq & 1)`)
//!    + `VIRTIO_BLK_T_FLUSH` barrier.

use core::mem::{offset_of, size_of};

use crate::ioring::IoRingClient;
use crate::vault_crypto::{crc32_ieee, crc32_update};
use crate::vfs_proto::{
    VfsDirEntry, VFS_FLAG_PRIVATE, VFS_FLAG_READONLY, VFS_MAX_NAME_LEN,
};
use crate::{
    BADGE_SUPERVISOR, BADGE_VFS_DEV_WS, BADGE_VFS_VAULT_ROOT, EEXIST, EINVAL, ENOENT, ENOSPC,
    EPERM, EROFS, SEC_FLAG_DEV_WORKSPACE, SEC_FLAG_NO_DELEGATE, SEC_FLAG_READ_ONLY_EXPORT,
};

pub const DRYFS_MAGIC: [u8; 8] = *b"DRYFS_V1";
pub const PERSIST_MAX_SLOTS: usize = 24;
pub const PERSIST_MAX_FILE_BYTES: usize = 4096;
const DATA_START_BLOCK: u32 = 2;

const PERSIST_README_TEXT: &[u8] = b"DRY OS /persist Crash-Safe VirtIO-Blk Filesystem (DRYFS_v1)\n-----------------------------------------------------------\nSTATUS: Persistent CoW Block Storage (Dual A/B Superblocks + CRC32)\nBARRIER: Every commit enforces VIRTIO_BLK_T_FLUSH durability.\nSECURITY: Mandatory ADR-007 SecLabel & INV-SEC-05 link rules active.\n";

/// Decoded caller security context derived from unforgeable endpoint badge on `dry-vfs`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VfsCallerCtx {
    pub is_supervisor: bool,
    pub sec_domain: u8, // 1 = RootSystem, 2 = Userspace
    pub sec_flags: u8,
    pub comp_id: u16,
}

impl VfsCallerCtx {
    pub const fn from_badge(badge: u64) -> Self {
        if badge == BADGE_SUPERVISOR {
            return Self {
                is_supervisor: true,
                sec_domain: 1,
                sec_flags: 0,
                comp_id: 0,
            };
        }
        if badge == BADGE_VFS_VAULT_ROOT {
            return Self {
                is_supervisor: false,
                sec_domain: 1,
                sec_flags: 0,
                comp_id: 0x0007,
            };
        }
        if badge == BADGE_VFS_DEV_WS {
            return Self {
                is_supervisor: false,
                sec_domain: 1,
                sec_flags: SEC_FLAG_DEV_WORKSPACE,
                comp_id: 0x002a,
            };
        }
        let dom_hi = ((badge >> 24) & 0xff) as u8;
        let flags_hi = ((badge >> 16) & 0xff) as u8;
        let comp = (badge & 0xffff) as u16;
        Self {
            is_supervisor: false,
            sec_domain: if dom_hi == 1 { 1 } else { 2 },
            sec_flags: flags_hi,
            comp_id: comp,
        }
    }
}

/// Fixed 64-byte persistent directory + inode entry inside `DryFsSuperblock`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DryFsEntry {
    pub used: u8,
    pub sec_domain: u8,
    pub sec_flags: u8,
    pub nlink: u8,
    pub inode: u32,
    pub size_bytes: u32,
    pub data_block_4k: u32,
    pub data_crc32: u32,
    pub vfs_flags: u16,
    pub owner_comp: u16,
    pub name: [u8; 36],
    pub _pad: u32,
}

const _: () = assert!(size_of::<DryFsEntry>() == 64);

impl DryFsEntry {
    pub const EMPTY: Self = Self {
        used: 0,
        sec_domain: 0,
        sec_flags: 0,
        nlink: 0,
        inode: 0,
        size_bytes: 0,
        data_block_4k: 0,
        data_crc32: 0,
        vfs_flags: 0,
        owner_comp: 0,
        name: [0; 36],
        _pad: 0,
    };

    pub fn name_bytes(&self) -> &[u8] {
        let len = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len());
        &self.name[..len]
    }
}

/// On-disk superblock (`64 + 24 * 64 = 1,600` bytes; stored padded inside a 4 KiB block).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DryFsSuperblock {
    pub magic: [u8; 8],
    pub version: u32,
    pub total_blocks_4k: u32,
    pub tx_seq: u64,
    pub next_inode: u32,
    pub alloc_rover: u32,
    pub active_entries: u32,
    pub crc32: u32,
    pub _reserved: [u8; 24],
    pub entries: [DryFsEntry; PERSIST_MAX_SLOTS],
}

const _: () = assert!(size_of::<DryFsSuperblock>() == 1600);

impl DryFsSuperblock {
    pub const fn empty() -> Self {
        Self {
            magic: [0; 8],
            version: 0,
            total_blocks_4k: 0,
            tx_seq: 0,
            next_inode: 1000,
            alloc_rover: DATA_START_BLOCK,
            active_entries: 0,
            crc32: 0,
            _reserved: [0; 24],
            entries: [DryFsEntry::EMPTY; PERSIST_MAX_SLOTS],
        }
    }

    pub fn compute_crc32(&self) -> u32 {
        let crc_off = offset_of!(Self, crc32);
        // SAFETY: `DryFsSuperblock` is `#[repr(C)]` POD (`1,600` bytes) with zero uninit padding.
        let bytes = unsafe {
            core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>())
        };
        let mut st = crc32_update(0xffff_ffff, &bytes[..crc_off]);
        st = crc32_update(st, &[0u8; 4]);
        st = crc32_update(st, &bytes[(crc_off + 4)..]);
        !st
    }

    pub fn is_valid(&self, max_blocks_4k: u32) -> bool {
        self.magic == DRYFS_MAGIC
            && self.version == 1
            && self.total_blocks_4k >= 16
            && self.total_blocks_4k <= max_blocks_4k
            && self.crc32 == self.compute_crc32()
    }

    pub fn encode_into_4k(&self, blk: &mut [u8; 4096]) {
        blk.fill(0);
        // SAFETY: `size_of::<Self>() == 1600 <= 4096`.
        let src = unsafe {
            core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>())
        };
        blk[..size_of::<Self>()].copy_from_slice(src);
    }

    pub fn decode_into(blk: &[u8; 4096], out: &mut Self) {
        // SAFETY: `DryFsSuperblock` is POD (`1600 <= 4096`).
        unsafe {
            core::ptr::copy_nonoverlapping(
                blk.as_ptr(),
                (out as *mut Self).cast::<u8>(),
                size_of::<Self>(),
            );
        }
    }
}

pub struct PersistMount {
    pub mounted: bool,
    pub active_slot: u8,
    pub recovered_fallback: bool,
    pub sb: DryFsSuperblock,
    pub staging_sb: DryFsSuperblock,
    pub scratch_4k: [u8; 4096],
    pub io_buf_4k: [u8; 4096],
    pub commits: u64,
}

impl PersistMount {
    pub const fn new() -> Self {
        Self {
            mounted: false,
            active_slot: 0,
            recovered_fallback: false,
            sb: DryFsSuperblock::empty(),
            staging_sb: DryFsSuperblock::empty(),
            scratch_4k: [0u8; 4096],
            io_buf_4k: [0u8; 4096],
            commits: 0,
        }
    }

    #[inline]
    fn stage_from_committed(&mut self) {
        // SAFETY: `self.sb` and `self.staging_sb` are disjoint `#[repr(C)]` POD fields in `.bss`.
        unsafe {
            core::ptr::copy_nonoverlapping(&raw const self.sb, &raw mut self.staging_sb, 1);
        }
    }

    #[inline]
    fn adopt_staged_as_committed(&mut self) {
        // SAFETY: `self.staging_sb` and `self.sb` are disjoint `#[repr(C)]` POD fields in `.bss`.
        unsafe {
            core::ptr::copy_nonoverlapping(&raw const self.staging_sb, &raw mut self.sb, 1);
        }
    }

    fn set_name(dst: &mut [u8; 36], name: &[u8]) {
        *dst = [0u8; 36];
        let n = name.len().min(VFS_MAX_NAME_LEN);
        dst[..n].copy_from_slice(&name[..n]);
    }

    /// Mount an existing `DRYFS_v1` volume (verifying both A/B superblocks + data block CRC32s)
    /// or format a fresh volume if neither slot holds `DRYFS_v1`.
    pub fn mount_or_format(&mut self, ring: &mut IoRingClient) -> Result<(bool, u64), isize> {
        let max_blocks = ((ring.total_sectors / 8) as u32).clamp(16, 4096);
        ring.read_block_4k(0, &mut self.io_buf_4k)?;
        DryFsSuperblock::decode_into(&self.io_buf_4k, &mut self.sb);
        ring.read_block_4k(1, &mut self.io_buf_4k)?;
        DryFsSuperblock::decode_into(&self.io_buf_4k, &mut self.staging_sb);

        let ok_a = self.sb.is_valid(max_blocks)
            && Self::verify_data_blocks_buf(ring, &self.sb, &mut self.scratch_4k);
        let ok_b = self.staging_sb.is_valid(max_blocks)
            && Self::verify_data_blocks_buf(ring, &self.staging_sb, &mut self.scratch_4k);

        self.recovered_fallback = false;
        match (ok_a, ok_b) {
            (true, true) => {
                if self.sb.tx_seq >= self.staging_sb.tx_seq {
                    self.active_slot = 0;
                } else {
                    self.adopt_staged_as_committed();
                    self.active_slot = 1;
                }
                self.mounted = true;
                Ok((false, self.sb.tx_seq))
            }
            (true, false) => {
                self.active_slot = 0;
                self.recovered_fallback = true;
                self.mounted = true;
                Ok((false, self.sb.tx_seq))
            }
            (false, true) => {
                self.adopt_staged_as_committed();
                self.active_slot = 1;
                self.recovered_fallback = true;
                self.mounted = true;
                Ok((false, self.sb.tx_seq))
            }
            (false, false) => {
                self.format_fresh(ring, max_blocks)?;
                Ok((true, self.sb.tx_seq))
            }
        }
    }

    fn verify_data_blocks_buf(
        ring: &mut IoRingClient,
        sb: &DryFsSuperblock,
        scratch: &mut [u8; 4096],
    ) -> bool {
        for e in &sb.entries {
            if e.used == 0 || e.size_bytes == 0 {
                continue;
            }
            if e.data_block_4k < DATA_START_BLOCK || e.data_block_4k >= sb.total_blocks_4k {
                return false;
            }
            if ring.read_block_4k(e.data_block_4k, scratch).is_err() {
                return false;
            }
            let n = (e.size_bytes as usize).min(PERSIST_MAX_FILE_BYTES);
            if crc32_ieee(&scratch[..n]) != e.data_crc32 {
                return false;
            }
        }
        true
    }

    fn format_fresh(&mut self, ring: &mut IoRingClient, max_blocks: u32) -> Result<(), isize> {
        self.sb = DryFsSuperblock::empty();
        self.sb.magic = DRYFS_MAGIC;
        self.sb.version = 1;
        self.sb.total_blocks_4k = max_blocks;
        self.sb.tx_seq = 1;
        self.sb.next_inode = 1000;
        self.sb.alloc_rover = DATA_START_BLOCK + 1;
        self.sb.active_entries = 1;

        self.scratch_4k.fill(0);
        self.scratch_4k[..PERSIST_README_TEXT.len()].copy_from_slice(PERSIST_README_TEXT);
        ring.write_block_4k(DATA_START_BLOCK, &self.scratch_4k)?;
        ring.flush()?;

        let mut readme = DryFsEntry::EMPTY;
        readme.used = 1;
        readme.sec_domain = 1;
        readme.sec_flags = SEC_FLAG_READ_ONLY_EXPORT;
        readme.nlink = 1;
        readme.inode = 1000;
        readme.size_bytes = PERSIST_README_TEXT.len() as u32;
        readme.data_block_4k = DATA_START_BLOCK;
        readme.data_crc32 = crc32_ieee(PERSIST_README_TEXT);
        readme.vfs_flags = VFS_FLAG_READONLY;
        readme.owner_comp = 0;
        Self::set_name(&mut readme.name, b"README.txt");
        self.sb.entries[0] = readme;
        self.sb.next_inode = 1001;
        self.sb.crc32 = self.sb.compute_crc32();

        self.sb.encode_into_4k(&mut self.scratch_4k);
        ring.write_block_4k(0, &self.scratch_4k)?;
        ring.write_block_4k(1, &self.scratch_4k)?;
        ring.flush()?;
        self.active_slot = 1;
        self.mounted = true;
        self.commits = 1;
        Ok(())
    }

    fn alloc_cow_block(&self) -> Result<u32, isize> {
        let usable = self.staging_sb.total_blocks_4k.saturating_sub(DATA_START_BLOCK);
        if usable == 0 {
            return Err(ENOSPC);
        }
        for step in 0..usable {
            let blk = DATA_START_BLOCK
                + ((self.staging_sb.alloc_rover.saturating_sub(DATA_START_BLOCK) + step) % usable);
            let in_old = self.sb.entries.iter().any(|e| e.used != 0 && e.data_block_4k == blk);
            let in_new = self
                .staging_sb
                .entries
                .iter()
                .any(|e| e.used != 0 && e.data_block_4k == blk);
            if !in_old && !in_new {
                return Ok(blk);
            }
        }
        Err(ENOSPC)
    }

    fn commit_staged(&mut self, ring: &mut IoRingClient) -> Result<(), isize> {
        let mut active = 0u32;
        for e in &self.staging_sb.entries {
            if e.used != 0 {
                active += 1;
            }
        }
        self.staging_sb.active_entries = active;
        self.staging_sb.tx_seq = self.sb.tx_seq.wrapping_add(1).max(2);
        self.staging_sb.crc32 = self.staging_sb.compute_crc32();

        let target_slot = (self.staging_sb.tx_seq & 1) as u32;
        self.staging_sb.encode_into_4k(&mut self.scratch_4k);
        ring.write_block_4k(target_slot, &self.scratch_4k)?;
        ring.flush()?;

        self.adopt_staged_as_committed();
        self.active_slot = target_slot as u8;
        self.commits = self.commits.wrapping_add(1);
        Ok(())
    }

    pub fn check_read_perm(&self, e: &DryFsEntry, caller: VfsCallerCtx) -> Result<(), isize> {
        if caller.is_supervisor {
            return Ok(());
        }
        if (e.sec_flags & SEC_FLAG_READ_ONLY_EXPORT) != 0 {
            return Ok(());
        }
        if caller.sec_domain == 2 {
            // `Userspace` cannot read `RootSystem` persistent files unless `READ_ONLY_EXPORT`.
            if e.sec_domain != 2 {
                return Err(EPERM);
            }
            if (e.vfs_flags & VFS_FLAG_PRIVATE) != 0
                && e.owner_comp != 0
                && caller.comp_id != e.owner_comp
            {
                return Err(EPERM);
            }
            return Ok(());
        }
        // `RootSystem` with `DEV_WORKSPACE` cannot access `NO_DELEGATE` or foreign private objects (`ADR-007` Rule 6).
        if (caller.sec_flags & SEC_FLAG_DEV_WORKSPACE) != 0
            && caller.comp_id != e.owner_comp
            && ((e.sec_flags & SEC_FLAG_NO_DELEGATE) != 0
                || (e.vfs_flags & VFS_FLAG_PRIVATE) != 0)
        {
            return Err(EPERM);
        }
        Ok(())
    }

    pub fn check_write_perm(&self, e: &DryFsEntry, caller: VfsCallerCtx) -> Result<(), isize> {
        if (e.vfs_flags & VFS_FLAG_READONLY) != 0
            || (e.sec_flags & SEC_FLAG_READ_ONLY_EXPORT) != 0
        {
            return Err(EROFS);
        }
        if caller.is_supervisor {
            return Ok(());
        }
        if caller.sec_domain != e.sec_domain {
            return Err(EPERM);
        }
        if (caller.sec_flags & SEC_FLAG_DEV_WORKSPACE) != 0 && caller.comp_id != e.owner_comp {
            return Err(EPERM);
        }
        if e.owner_comp != 0 && caller.comp_id != e.owner_comp {
            return Err(EPERM);
        }
        Ok(())
    }

    pub fn list(&self, start_idx: usize, out: &mut [VfsDirEntry]) -> (usize, usize) {
        let mut batch_n = 0usize;
        let mut total_n = 0usize;
        for e in &self.sb.entries {
            if e.used == 0 {
                continue;
            }
            if total_n >= start_idx && batch_n < out.len() {
                out[batch_n] = VfsDirEntry {
                    inode: e.inode,
                    size_bytes: e.size_bytes,
                    flags: e.vfs_flags,
                    owner_comp: e.owner_comp,
                    name: e.name,
                };
                batch_n += 1;
            }
            total_n += 1;
        }
        (batch_n, total_n)
    }

    pub fn find_idx(&self, name: &[u8]) -> Option<usize> {
        self.sb
            .entries
            .iter()
            .position(|e| e.used != 0 && e.name_bytes() == name)
    }

    pub fn read_slice(
        &mut self,
        ring: &mut IoRingClient,
        name: &[u8],
        caller: VfsCallerCtx,
        offset: usize,
        dst: &mut [u8],
    ) -> Result<(usize, usize, u16, u32), isize> {
        let idx = self.find_idx(name).ok_or(ENOENT)?;
        let entry = self.sb.entries[idx];
        self.check_read_perm(&entry, caller)?;
        let total = entry.size_bytes as usize;
        if total == 0 || offset >= total || dst.is_empty() {
            return Ok((0, total, entry.vfs_flags, entry.inode));
        }
        ring.read_block_4k(entry.data_block_4k, &mut self.scratch_4k)?;
        let end = (offset + dst.len()).min(total);
        let n = end - offset;
        dst[..n].copy_from_slice(&self.scratch_4k[offset..end]);
        Ok((n, total, entry.vfs_flags, entry.inode))
    }

    pub fn create(
        &mut self,
        ring: &mut IoRingClient,
        name: &[u8],
        caller: VfsCallerCtx,
        req_vfs_flags: u16,
    ) -> Result<u32, isize> {
        if self.find_idx(name).is_some() {
            return Err(EEXIST);
        }
        self.stage_from_committed();
        let slot_idx = self
            .staging_sb
            .entries
            .iter()
            .position(|e| e.used == 0)
            .ok_or(ENOSPC)?;
        let ino = self.staging_sb.next_inode;
        self.staging_sb.next_inode = self.staging_sb.next_inode.wrapping_add(1).max(1000);

        let is_priv = (req_vfs_flags & VFS_FLAG_PRIVATE) != 0;
        let sec_flags = if is_priv {
            caller.sec_flags | SEC_FLAG_NO_DELEGATE
        } else {
            caller.sec_flags
        };
        let mut entry = DryFsEntry::EMPTY;
        entry.used = 1;
        entry.sec_domain = caller.sec_domain;
        entry.sec_flags = sec_flags;
        entry.nlink = 1;
        entry.inode = ino;
        entry.vfs_flags = req_vfs_flags & VFS_FLAG_PRIVATE;
        entry.owner_comp = if caller.is_supervisor { 0 } else { caller.comp_id };
        Self::set_name(&mut entry.name, name);
        self.staging_sb.entries[slot_idx] = entry;

        self.commit_staged(ring)?;
        Ok(ino)
    }

    pub fn write(
        &mut self,
        ring: &mut IoRingClient,
        name: &[u8],
        caller: VfsCallerCtx,
        payload: &[u8],
        append: bool,
    ) -> Result<(usize, usize), isize> {
        let idx = self.find_idx(name).ok_or(ENOENT)?;
        let cur = self.sb.entries[idx];
        self.check_write_perm(&cur, caller)?;

        self.io_buf_4k.fill(0);
        let base_len = if append && cur.size_bytes > 0 {
            ring.read_block_4k(cur.data_block_4k, &mut self.io_buf_4k)?;
            cur.size_bytes as usize
        } else {
            0
        };
        if base_len + payload.len() > PERSIST_MAX_FILE_BYTES {
            return Err(ENOSPC);
        }
        let new_len = base_len + payload.len();
        self.io_buf_4k[base_len..new_len].copy_from_slice(payload);

        self.stage_from_committed();
        let (new_blk, new_crc) = if new_len > 0 {
            let blk = self.alloc_cow_block()?;
            ring.write_block_4k(blk, &self.io_buf_4k)?;
            ring.flush()?;
            self.staging_sb.alloc_rover = blk.wrapping_add(1);
            (blk, crc32_ieee(&self.io_buf_4k[..new_len]))
        } else {
            (0, 0)
        };

        let target_ino = cur.inode;
        for e in &mut self.staging_sb.entries {
            if e.used != 0 && e.inode == target_ino {
                e.size_bytes = new_len as u32;
                e.data_block_4k = new_blk;
                e.data_crc32 = new_crc;
            }
        }
        self.commit_staged(ring)?;
        Ok((payload.len(), new_len))
    }

    pub fn rename(
        &mut self,
        ring: &mut IoRingClient,
        old_name: &[u8],
        new_name: &[u8],
        caller: VfsCallerCtx,
    ) -> Result<(), isize> {
        let idx = self.find_idx(old_name).ok_or(ENOENT)?;
        if self.find_idx(new_name).is_some() {
            return Err(EEXIST);
        }
        let cur = self.sb.entries[idx];
        self.check_write_perm(&cur, caller)?;

        self.stage_from_committed();
        Self::set_name(&mut self.staging_sb.entries[idx].name, new_name);
        self.commit_staged(ring)
    }

    /// `INV-SEC-05`: Hardlink creation within `/persist` — requires identical domain/compartment
    /// authority and forbids `DEV_WORKSPACE` or cross-compartment aliasing (`NO_DELEGATE`).
    pub fn link(
        &mut self,
        ring: &mut IoRingClient,
        src_name: &[u8],
        dst_name: &[u8],
        caller: VfsCallerCtx,
    ) -> Result<u32, isize> {
        let src_idx = self.find_idx(src_name).ok_or(ENOENT)?;
        if self.find_idx(dst_name).is_some() {
            return Err(EEXIST);
        }
        let src = self.sb.entries[src_idx];
        self.check_write_perm(&src, caller)?;
        if (src.sec_flags & SEC_FLAG_NO_DELEGATE) != 0 && caller.comp_id != src.owner_comp {
            return Err(EPERM);
        }

        self.stage_from_committed();
        let free_idx = self
            .staging_sb
            .entries
            .iter()
            .position(|e| e.used == 0)
            .ok_or(ENOSPC)?;
        let new_nlink = src.nlink.saturating_add(1);
        for e in &mut self.staging_sb.entries {
            if e.used != 0 && e.inode == src.inode {
                e.nlink = new_nlink;
            }
        }
        let mut alias = src;
        alias.nlink = new_nlink;
        Self::set_name(&mut alias.name, dst_name);
        self.staging_sb.entries[free_idx] = alias;
        self.commit_staged(ring)?;
        Ok(src.inode)
    }

    pub fn unlink(
        &mut self,
        ring: &mut IoRingClient,
        name: &[u8],
        caller: VfsCallerCtx,
    ) -> Result<(), isize> {
        let idx = self.find_idx(name).ok_or(ENOENT)?;
        let cur = self.sb.entries[idx];
        self.check_write_perm(&cur, caller)?;

        self.stage_from_committed();
        self.staging_sb.entries[idx] = DryFsEntry::EMPTY;
        for e in &mut self.staging_sb.entries {
            if e.used != 0 && e.inode == cur.inode {
                e.nlink = e.nlink.saturating_sub(1).max(1);
            }
        }
        self.commit_staged(ring)
    }

    /// Deliberate test hook (`BADGE_SUPERVISOR` only): corrupt Superblock Slot 0 on disk to prove
    /// `DRYFS_v1` CRC32 detection and automatic fallback recovery from Slot 1 on next boot!
    pub fn corrupt_slot0_for_recovery_test(&mut self, ring: &mut IoRingClient) -> Result<(), isize> {
        // Ensure Slot 1 holds the latest valid state before corrupting Slot 0.
        if self.active_slot == 0 {
            self.stage_from_committed();
            self.commit_staged(ring)?;
        }
        if self.active_slot != 1 {
            return Err(EINVAL);
        }
        self.scratch_4k.fill(0xde);
        self.scratch_4k[..8].copy_from_slice(&DRYFS_MAGIC);
        ring.write_block_4k(0, &self.scratch_4k)?;
        ring.flush()
    }
}

impl Default for PersistMount {
    fn default() -> Self {
        Self::new()
    }
}
