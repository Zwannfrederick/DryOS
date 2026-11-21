// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Multi-block CoW extents (`VFS_FLAG_EXTENT`, up to `512 KiB`), hierarchical directories
//! (`mkdir`/`rmdir`/subdir `list`), and CoW block-allocation bitmap for `DRYFS_v1`.

use crate::abi_ext::ENOTEMPTY;
use crate::ioring::IoRingClient;
use crate::persist_fs::{
    DryFsEntry, DryFsSuperblock, PersistMount, VfsCallerCtx, DATA_START_BLOCK,
    PERSIST_MAX_EXTENT_BYTES, PERSIST_MAX_FILE_BYTES,
};
use crate::vault_crypto::crc32_update;
use crate::vfs_mem::{is_child_of, list_direct_child, parent_dir_of};
use crate::vfs_proto::{VfsDirEntry, VFS_FLAG_DIR, VFS_FLAG_EXTENT};
use crate::{EEXIST, EINVAL, ENOENT, ENOSPC};

impl PersistMount {
    #[inline]
    pub(crate) fn bm_mark(&mut self, blk: u32) {
        let idx = (blk as usize) / 64;
        if idx < self.cow_bm.len() {
            self.cow_bm[idx] |= 1u64 << ((blk as usize) & 63);
        }
    }

    #[inline]
    fn bm_is_used(&self, blk: u32) -> bool {
        let idx = (blk as usize) / 64;
        idx >= self.cow_bm.len() || (self.cow_bm[idx] & (1u64 << ((blk as usize) & 63))) != 0
    }

    fn scan_sb_used_blocks(&mut self, ring: &mut IoRingClient, use_staging: bool) {
        for slot in 0..self.sb.entries.len() {
            let e = if use_staging {
                self.staging_sb.entries[slot]
            } else {
                self.sb.entries[slot]
            };
            if e.used == 0 || e.data_block_4k == 0 {
                continue;
            }
            self.bm_mark(e.data_block_4k);
            if (e.vfs_flags & VFS_FLAG_EXTENT) != 0
                && e.size_bytes > 0
                && ring.read_block_4k(e.data_block_4k, &mut self.scratch_4k).is_ok()
            {
                let nb = (e.size_bytes as usize).div_ceil(4096).min(1024);
                for bi in 0..nb {
                    let b = u32::from_le_bytes([
                        self.scratch_4k[bi * 4],
                        self.scratch_4k[bi * 4 + 1],
                        self.scratch_4k[bi * 4 + 2],
                        self.scratch_4k[bi * 4 + 3],
                    ]);
                    if b >= DATA_START_BLOCK {
                        self.bm_mark(b);
                    }
                }
            }
        }
    }

    pub(crate) fn rebuild_cow_bitmap(&mut self, ring: &mut IoRingClient) {
        self.cow_bm.fill(0);
        for b in 0..DATA_START_BLOCK {
            self.bm_mark(b);
        }
        self.scan_sb_used_blocks(ring, false);
        self.scan_sb_used_blocks(ring, true);
    }

    pub(crate) fn alloc_from_bm(&mut self, max_blocks: u32) -> Result<u32, isize> {
        let usable = max_blocks.saturating_sub(DATA_START_BLOCK);
        if usable == 0 {
            return Err(ENOSPC);
        }
        for step in 0..usable {
            let blk = DATA_START_BLOCK
                + ((self.staging_sb.alloc_rover.saturating_sub(DATA_START_BLOCK) + step) % usable);
            if !self.bm_is_used(blk) {
                self.bm_mark(blk);
                self.staging_sb.alloc_rover = blk.wrapping_add(1);
                return Ok(blk);
            }
        }
        Err(ENOSPC)
    }

    pub(crate) fn verify_extent_entry(
        ring: &mut IoRingClient,
        sb: &DryFsSuperblock,
        entry: &DryFsEntry,
        idx_buf: &mut [u8; 4096],
        scratch: &mut [u8; 4096],
    ) -> bool {
        let total = entry.size_bytes as usize;
        if total == 0 || total > PERSIST_MAX_EXTENT_BYTES {
            return false;
        }
        if entry.data_block_4k < DATA_START_BLOCK || entry.data_block_4k >= sb.total_blocks_4k {
            return false;
        }
        if ring.read_block_4k(entry.data_block_4k, idx_buf).is_err() {
            return false;
        }
        let n_blocks = total.div_ceil(4096);
        let mut crc_st = 0xffff_ffffu32;
        let mut rem = total;
        for bi in 0..n_blocks {
            let b = u32::from_le_bytes([
                idx_buf[bi * 4],
                idx_buf[bi * 4 + 1],
                idx_buf[bi * 4 + 2],
                idx_buf[bi * 4 + 3],
            ]);
            if b < DATA_START_BLOCK || b >= sb.total_blocks_4k {
                return false;
            }
            if ring.read_block_4k(b, scratch).is_err() {
                return false;
            }
            let take = rem.min(4096);
            crc_st = crc32_update(crc_st, &scratch[..take]);
            rem -= take;
        }
        !crc_st == entry.data_crc32
    }

    pub(crate) fn read_extent_slice(
        &mut self,
        ring: &mut IoRingClient,
        entry: &DryFsEntry,
        offset: usize,
        dst: &mut [u8],
    ) -> Result<usize, isize> {
        let total = (entry.size_bytes as usize).min(PERSIST_MAX_EXTENT_BYTES);
        if total == 0 || offset >= total || dst.is_empty() {
            return Ok(0);
        }
        ring.read_block_4k(entry.data_block_4k, &mut self.idx_buf_4k)?;
        let end = (offset + dst.len()).min(total);
        let want_total = end - offset;
        let mut copied = 0usize;
        while copied < want_total {
            let cur_off = offset + copied;
            let bi = cur_off / 4096;
            let in_blk = cur_off % 4096;
            let b = u32::from_le_bytes([
                self.idx_buf_4k[bi * 4],
                self.idx_buf_4k[bi * 4 + 1],
                self.idx_buf_4k[bi * 4 + 2],
                self.idx_buf_4k[bi * 4 + 3],
            ]);
            if b < DATA_START_BLOCK || b >= self.sb.total_blocks_4k {
                return Err(crate::EIO);
            }
            ring.read_block_4k(b, &mut self.scratch_4k)?;
            let take = (want_total - copied).min(4096 - in_blk);
            dst[copied..(copied + take)].copy_from_slice(&self.scratch_4k[in_blk..(in_blk + take)]);
            copied += take;
        }
        Ok(copied)
    }

    pub fn has_dir(&self, name: &[u8]) -> bool {
        self.find_idx(name)
            .map(|i| (self.sb.entries[i].vfs_flags & VFS_FLAG_DIR) != 0)
            .unwrap_or(false)
    }

    pub fn mkdir(
        &mut self,
        ring: &mut IoRingClient,
        name: &[u8],
        caller: VfsCallerCtx,
    ) -> Result<u32, isize> {
        if self.find_idx(name).is_some() {
            return Err(EEXIST);
        }
        if let Some(parent) = parent_dir_of(name) {
            if !self.has_dir(parent) {
                return Err(ENOENT);
            }
        }
        self.create(ring, name, caller, VFS_FLAG_DIR)
    }

    pub fn rmdir(
        &mut self,
        ring: &mut IoRingClient,
        name: &[u8],
        caller: VfsCallerCtx,
    ) -> Result<(), isize> {
        let idx = self.find_idx(name).ok_or(ENOENT)?;
        let cur = self.sb.entries[idx];
        if (cur.vfs_flags & VFS_FLAG_DIR) == 0 {
            return Err(EINVAL);
        }
        self.check_write_perm(&cur, caller)?;
        for e in &self.sb.entries {
            if e.used != 0 && is_child_of(name, e.name_bytes()) {
                return Err(ENOTEMPTY);
            }
        }
        self.stage_from_committed();
        self.staging_sb.entries[idx] = DryFsEntry::EMPTY;
        self.commit_staged(ring)
    }

    pub fn list_sub(
        &self,
        dir_prefix: &[u8],
        start_idx: usize,
        out: &mut [VfsDirEntry],
    ) -> Result<(usize, usize), isize> {
        if !dir_prefix.is_empty() && !self.has_dir(dir_prefix) {
            return Err(if self.find_idx(dir_prefix).is_some() { EINVAL } else { ENOENT });
        }
        let mut batch_n = 0usize;
        let mut total_n = 0usize;
        for e in &self.sb.entries {
            if e.used == 0 {
                continue;
            }
            let Some(child_name) = list_direct_child(dir_prefix, e.name_bytes()) else {
                continue;
            };
            if total_n >= start_idx && batch_n < out.len() {
                let mut de = VfsDirEntry {
                    inode: e.inode,
                    size_bytes: e.size_bytes,
                    flags: e.vfs_flags,
                    owner_comp: e.owner_comp,
                    name: [0; 36],
                };
                Self::set_name(&mut de.name, child_name);
                out[batch_n] = de;
                batch_n += 1;
            }
            total_n += 1;
        }
        Ok((batch_n, total_n))
    }

    pub fn write_full(
        &mut self,
        ring: &mut IoRingClient,
        name: &[u8],
        caller: VfsCallerCtx,
        payload: &[u8],
        append: bool,
    ) -> Result<(usize, usize), isize> {
        let idx = self.find_idx(name).ok_or(ENOENT)?;
        let cur = self.sb.entries[idx];
        if (cur.vfs_flags & VFS_FLAG_DIR) != 0 {
            return Err(EINVAL);
        }
        self.check_write_perm(&cur, caller)?;

        let old_len = if append { cur.size_bytes as usize } else { 0 };
        let new_len = old_len.saturating_add(payload.len());
        if new_len > PERSIST_MAX_EXTENT_BYTES {
            return Err(ENOSPC);
        }
        if new_len <= PERSIST_MAX_FILE_BYTES && (!append || (cur.vfs_flags & VFS_FLAG_EXTENT) == 0) {
            return self.write(ring, name, caller, payload, append);
        }

        let mut old_blks = [0u32; 128];
        let old_n = if old_len > 0 { old_len.div_ceil(4096).min(128) } else { 0 };
        if old_n > 0 {
            if (cur.vfs_flags & VFS_FLAG_EXTENT) != 0 {
                ring.read_block_4k(cur.data_block_4k, &mut self.scratch_4k)?;
                for bi in 0..old_n {
                    old_blks[bi] = u32::from_le_bytes([
                        self.scratch_4k[bi * 4],
                        self.scratch_4k[bi * 4 + 1],
                        self.scratch_4k[bi * 4 + 2],
                        self.scratch_4k[bi * 4 + 3],
                    ]);
                }
            } else {
                old_blks[0] = cur.data_block_4k;
            }
        }

        self.stage_from_committed();
        self.rebuild_cow_bitmap(ring);
        let max_blocks = self.staging_sb.total_blocks_4k;
        let idx_blk = self.alloc_from_bm(max_blocks)?;
        self.idx_buf_4k.fill(0);

        let new_n = new_len.div_ceil(4096);
        let mut crc_st = 0xffff_ffffu32;
        for bi in 0..new_n {
            let blk = self.alloc_from_bm(max_blocks)?;
            self.io_buf_4k.fill(0);
            let blk_start = bi * 4096;
            if blk_start < old_len && bi < old_n && old_blks[bi] >= DATA_START_BLOCK {
                ring.read_block_4k(old_blks[bi], &mut self.io_buf_4k)?;
            }
            let copy_start = old_len.saturating_sub(blk_start).min(4096);
            let p_start = blk_start.saturating_sub(old_len);
            if p_start < payload.len() && copy_start < 4096 {
                let take = (payload.len() - p_start).min(4096 - copy_start);
                self.io_buf_4k[copy_start..(copy_start + take)]
                    .copy_from_slice(&payload[p_start..(p_start + take)]);
            }
            let valid_in_blk = (new_len - blk_start).min(4096);
            crc_st = crc32_update(crc_st, &self.io_buf_4k[..valid_in_blk]);
            ring.write_block_4k(blk, &self.io_buf_4k)?;
            self.idx_buf_4k[(bi * 4)..(bi * 4 + 4)].copy_from_slice(&blk.to_le_bytes());
        }
        ring.write_block_4k(idx_blk, &self.idx_buf_4k)?;
        ring.flush()?;

        let target_ino = cur.inode;
        for e in &mut self.staging_sb.entries {
            if e.used != 0 && e.inode == target_ino {
                e.size_bytes = new_len as u32;
                e.data_block_4k = idx_blk;
                e.data_crc32 = !crc_st;
                e.vfs_flags = (e.vfs_flags & !VFS_FLAG_DIR) | VFS_FLAG_EXTENT;
            }
        }
        self.commit_staged(ring)?;
        Ok((payload.len(), new_len))
    }
}
