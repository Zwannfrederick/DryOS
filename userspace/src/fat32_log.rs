// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal, crash-safe read-only/read-write FAT32 + VFAT LFN writer strictly scoped
//! to the verified `DRYOS_LOGS` partition on the physical boot USB medium.

use core::mem::size_of;

use crate::vault_crypto::{ct_eq_32, Sha256};
use crate::{EINVAL, ENOSPC, EROFS};

const EIO: isize = -5;

// USB log IPC opcodes (`dry-init` supervisor <-> `dry-usb`)
pub const USB_OP_LOG_CONFIG: u32 = 0x6710;
pub const USB_OP_LOG_STATUS: u32 = 0x6711;
pub const USB_OP_LOG_STAGE_RESET: u32 = 0x6712;
pub const USB_OP_LOG_STAGE_APPEND: u32 = 0x6713;
pub const USB_OP_LOG_COMMIT_FILE: u32 = 0x6714;
pub const USB_OP_LOG_LIST_SESSIONS: u32 = 0x6715;
pub const USB_OP_LOG_READ_FILE: u32 = 0x6716;

pub const FAT_ERR_CORRUPT_FS: isize = -74;
pub const FAT_ERR_UNVERIFIED_ANCESTRY: isize = -30;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UsbLogStatus {
    pub gate_mask: u32,
    pub block_reason: u32,
    pub logs_sectors: u64,
    pub free_kib: u32,
    pub total_kib: u32,
    pub writes_verified: u32,
    pub write_errors: u32,
    pub fat_mounted: u8,
    pub fat_corrupt: u8,
    pub read_only: u8,
    pub session_count: u8,
}

const _: () = assert!(size_of::<UsbLogStatus>() == 40);

impl UsbLogStatus {
    pub const fn empty() -> Self {
        Self {
            gate_mask: 0, block_reason: 0, logs_sectors: 0, free_kib: 0,
            total_kib: 0, writes_verified: 0, write_errors: 0,
            fat_mounted: 0, fat_corrupt: 0, read_only: 0, session_count: 0,
        }
    }
}

pub trait LogBlockDevice {
    fn part_sectors(&self) -> u64;
    fn read_sector(&mut self, rel_lba: u64, out: &mut [u8; 512]) -> Result<(), isize>;
    fn write_sector(&mut self, rel_lba: u64, data: &[u8; 512]) -> Result<(), isize>;
    fn flush(&mut self) -> Result<(), isize>;
}

const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;
const FAT_EOC: u32 = 0x0FFF_FFF8;

#[derive(Clone, Copy)]
pub struct Fat32LogFs {
    pub mounted: bool,
    pub corrupt: bool,
    pub sec_per_clus: u32,
    pub rsvd_secs: u32,
    pub num_fats: u32,
    pub fat_sz_32: u32,
    pub tot_secs: u32,
    pub root_clus: u32,
    pub fs_info_sec: u16,
    pub data_start: u32,
    pub total_clusters: u32,
    pub free_clusters: u32,
    pub alloc_hint: u32,
}

impl Fat32LogFs {
    pub const fn empty() -> Self {
        Self {
            mounted: false,
            corrupt: false,
            sec_per_clus: 0,
            rsvd_secs: 0,
            num_fats: 0,
            fat_sz_32: 0,
            tot_secs: 0,
            root_clus: 0,
            fs_info_sec: 0,
            data_start: 0,
            total_clusters: 0,
            free_clusters: 0,
            alloc_hint: 2,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::empty();
    }

    pub fn mount(&mut self, dev: &mut impl LogBlockDevice) -> Result<(), isize> {
        self.reset();
        let part_secs = dev.part_sectors();
        if part_secs < 128 {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        }
        let mut bpb = [0u8; 512];
        dev.read_sector(0, &mut bpb)?;
        if bpb[510] != 0x55 || bpb[511] != 0xAA {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        }
        let bps = u16::from_le_bytes([bpb[11], bpb[12]]);
        let spc = bpb[13] as u32;
        let rsvd = u16::from_le_bytes([bpb[14], bpb[15]]) as u32;
        let nfats = bpb[16] as u32;
        let root_cnt = u16::from_le_bytes([bpb[17], bpb[18]]);
        let tot16 = u16::from_le_bytes([bpb[19], bpb[20]]);
        let fat16 = u16::from_le_bytes([bpb[22], bpb[23]]);
        let tot32 = u32::from_le_bytes([bpb[32], bpb[33], bpb[34], bpb[35]]);
        let fatsz32 = u32::from_le_bytes([bpb[36], bpb[37], bpb[38], bpb[39]]);
        let root_clus = u32::from_le_bytes([bpb[44], bpb[45], bpb[46], bpb[47]]);
        let fs_info_sec = u16::from_le_bytes([bpb[48], bpb[49]]);

        if bps != 512
            || spc == 0
            || !spc.is_power_of_two()
            || rsvd == 0
            || nfats == 0
            || nfats > 4
            || root_cnt != 0
            || tot16 != 0
            || fat16 != 0
            || fatsz32 == 0
            || root_clus < 2
            || &bpb[82..90] != b"FAT32   "
            || (tot32 as u64) > part_secs
        {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        }

        let Some(fat_area) = nfats.checked_mul(fatsz32) else {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        };
        let Some(data_start) = rsvd.checked_add(fat_area) else {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        };
        if data_start >= tot32 {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        }
        let total_clusters = (tot32 - data_start) / spc;
        if total_clusters < 64 || root_clus >= total_clusters + 2 {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        }

        // Verify FAT[0] media descriptor matches FAT32 (`0x0FFF_FF00 | media`)
        let mut fat0 = [0u8; 512];
        dev.read_sector(rsvd as u64, &mut fat0)?;
        let e0 = u32::from_le_bytes([fat0[0], fat0[1], fat0[2], fat0[3]]) & 0x0FFF_FFFF;
        let e1 = u32::from_le_bytes([fat0[4], fat0[5], fat0[6], fat0[7]]) & 0x0FFF_FFFF;
        if (e0 & 0x0FFF_FF00) != 0x0FFF_FF00 || e1 < FAT_EOC {
            self.corrupt = true;
            return Err(FAT_ERR_CORRUPT_FS);
        }

        self.sec_per_clus = spc;
        self.rsvd_secs = rsvd;
        self.num_fats = nfats;
        self.fat_sz_32 = fatsz32;
        self.tot_secs = tot32;
        self.root_clus = root_clus;
        self.fs_info_sec = fs_info_sec;
        self.data_start = data_start;
        self.total_clusters = total_clusters;

        self.free_clusters = self.count_free_clusters(dev)?;
        self.mounted = true;
        Ok(())
    }

    pub fn free_kib(&self) -> u32 {
        if !self.mounted {
            return 0;
        }
        let bytes_per_clus = self.sec_per_clus as u64 * 512;
        ((self.free_clusters as u64 * bytes_per_clus) / 1024) as u32
    }

    pub fn total_kib(&self) -> u32 {
        if !self.mounted {
            return 0;
        }
        let bytes_per_clus = self.sec_per_clus as u64 * 512;
        ((self.total_clusters as u64 * bytes_per_clus) / 1024) as u32
    }

    #[inline]
    fn clus_to_lba(&self, clus: u32, sec_in_clus: u32) -> Result<u64, isize> {
        if clus < 2 || clus >= self.total_clusters + 2 || sec_in_clus >= self.sec_per_clus {
            return Err(FAT_ERR_CORRUPT_FS);
        }
        let off = (clus - 2) as u64 * (self.sec_per_clus as u64) + (sec_in_clus as u64);
        let lba = (self.data_start as u64).checked_add(off).ok_or(FAT_ERR_CORRUPT_FS)?;
        if lba >= self.tot_secs as u64 {
            return Err(FAT_ERR_CORRUPT_FS);
        }
        Ok(lba)
    }

    fn count_free_clusters(&mut self, dev: &mut impl LogBlockDevice) -> Result<u32, isize> {
        let mut free = 0u32;
        let max_clus = self.total_clusters + 2;
        let mut cur_sec = u32::MAX;
        let mut sec_buf = [0u8; 512];
        let mut first_free = 0u32;
        for c in 2..max_clus {
            let fat_sec = self.rsvd_secs + (c / 128);
            if fat_sec != cur_sec {
                dev.read_sector(fat_sec as u64, &mut sec_buf)?;
                cur_sec = fat_sec;
            }
            let off = ((c % 128) as usize) * 4;
            let val = u32::from_le_bytes([sec_buf[off], sec_buf[off + 1], sec_buf[off + 2], sec_buf[off + 3]])
                & 0x0FFF_FFFF;
            if val == 0 {
                free += 1;
                if first_free == 0 {
                    first_free = c;
                }
            }
        }
        self.alloc_hint = if first_free >= 2 { first_free } else { 2 };
        Ok(free)
    }

    fn read_fat_entry(&self, dev: &mut impl LogBlockDevice, clus: u32) -> Result<u32, isize> {
        if clus < 2 || clus >= self.total_clusters + 2 {
            return Err(FAT_ERR_CORRUPT_FS);
        }
        let fat_sec = self.rsvd_secs + (clus / 128);
        let mut sec_buf = [0u8; 512];
        dev.read_sector(fat_sec as u64, &mut sec_buf)?;
        let off = ((clus % 128) as usize) * 4;
        Ok(u32::from_le_bytes([sec_buf[off], sec_buf[off + 1], sec_buf[off + 2], sec_buf[off + 3]]) & 0x0FFF_FFFF)
    }

    fn write_fat_entry(&mut self, dev: &mut impl LogBlockDevice, clus: u32, next: u32) -> Result<(), isize> {
        if clus < 2 || clus >= self.total_clusters + 2 {
            return Err(EINVAL);
        }
        let fat_off_sec = clus / 128;
        let off = ((clus % 128) as usize) * 4;
        let mut sec_buf = [0u8; 512];
        dev.read_sector((self.rsvd_secs + fat_off_sec) as u64, &mut sec_buf)?;
        let old = u32::from_le_bytes([sec_buf[off], sec_buf[off + 1], sec_buf[off + 2], sec_buf[off + 3]]);
        let new_val = (old & 0xF000_0000) | (next & 0x0FFF_FFFF);
        sec_buf[off..off + 4].copy_from_slice(&new_val.to_le_bytes());
        for f in 0..self.num_fats {
            let sec = self.rsvd_secs + f * self.fat_sz_32 + fat_off_sec;
            dev.write_sector(sec as u64, &sec_buf)?;
        }
        Ok(())
    }

    fn update_fs_info(&self, dev: &mut impl LogBlockDevice) -> Result<(), isize> {
        if self.fs_info_sec == 0 || (self.fs_info_sec as u32) >= self.rsvd_secs {
            return Ok(());
        }
        let mut sec = [0u8; 512];
        dev.read_sector(self.fs_info_sec as u64, &mut sec)?;
        let sig1 = u32::from_le_bytes([sec[0], sec[1], sec[2], sec[3]]);
        let sig2 = u32::from_le_bytes([sec[484], sec[485], sec[486], sec[487]]);
        if sig1 == 0x4161_5252 && sig2 == 0x6141_7272 {
            sec[488..492].copy_from_slice(&self.free_clusters.to_le_bytes());
            sec[492..496].copy_from_slice(&self.alloc_hint.to_le_bytes());
            dev.write_sector(self.fs_info_sec as u64, &sec)?;
        }
        Ok(())
    }

    fn alloc_cluster(&mut self, dev: &mut impl LogBlockDevice, link_prev: Option<u32>, zero_init: bool) -> Result<u32, isize> {
        if self.free_clusters == 0 {
            return Err(ENOSPC);
        }
        let max_clus = self.total_clusters + 2;
        let start = self.alloc_hint.clamp(2, max_clus - 1);
        let mut chosen = 0u32;
        let mut cur_sec = u32::MAX;
        let mut sec_buf = [0u8; 512];
        for step in 0..self.total_clusters {
            let c = 2 + ((start - 2 + step) % self.total_clusters);
            let fat_sec = self.rsvd_secs + (c / 128);
            if fat_sec != cur_sec {
                dev.read_sector(fat_sec as u64, &mut sec_buf)?;
                cur_sec = fat_sec;
            }
            let off = ((c % 128) as usize) * 4;
            let val = u32::from_le_bytes([sec_buf[off], sec_buf[off + 1], sec_buf[off + 2], sec_buf[off + 3]])
                & 0x0FFF_FFFF;
            if val == 0 {
                chosen = c;
                break;
            }
        }
        if chosen == 0 {
            self.free_clusters = 0;
            return Err(ENOSPC);
        }
        if zero_init {
            let zeros = [0u8; 512];
            for s in 0..self.sec_per_clus {
                let lba = self.clus_to_lba(chosen, s)?;
                dev.write_sector(lba, &zeros)?;
            }
        }
        self.write_fat_entry(dev, chosen, 0x0FFF_FFFF)?;
        if let Some(prev) = link_prev {
            self.write_fat_entry(dev, prev, chosen)?;
        }
        self.free_clusters -= 1;
        self.alloc_hint = if chosen + 1 < max_clus { chosen + 1 } else { 2 };
        Ok(chosen)
    }

    /// Find a named child entry (`is_dir` optional filter) in directory `dir_clus`.
    /// Returns `(first_clus, file_size, attr)`.
    pub fn find_entry(
        &self,
        dev: &mut impl LogBlockDevice,
        dir_clus: u32,
        target: &[u8],
    ) -> Result<Option<(u32, u32, u8)>, isize> {
        if !self.mounted || target.is_empty() || target.len() > 96 {
            return Err(EINVAL);
        }
        let mut clus = dir_clus;
        let mut lfn = [0u8; 104];
        let mut lfn_len = 0usize;
        let mut sec = [0u8; 512];
        let mut steps = 0usize;

        while (2..FAT_EOC).contains(&clus) && steps < 512 {
            steps += 1;
            for s in 0..self.sec_per_clus {
                let lba = self.clus_to_lba(clus, s)?;
                dev.read_sector(lba, &mut sec)?;
                for slot in 0..16 {
                    let ent = &sec[slot * 32..slot * 32 + 32];
                    let first = ent[0];
                    if first == 0x00 {
                        return Ok(None);
                    }
                    if first == 0xE5 {
                        lfn_len = 0;
                        continue;
                    }
                    let attr = ent[11];
                    if attr == ATTR_LFN {
                        let seq = first & 0x1F;
                        if (first & 0x40) != 0 {
                            lfn.fill(0);
                            lfn_len = (seq as usize) * 13;
                        }
                        if seq >= 1 && (seq as usize) <= 8 {
                            extract_lfn_chars(ent, ((seq - 1) as usize) * 13, &mut lfn);
                        }
                        continue;
                    }
                    if (attr & ATTR_VOLUME_ID) != 0 {
                        lfn_len = 0;
                        continue;
                    }
                    let mut sfn_buf = [0u8; 12];
                    let sfn_n = format_83_name(&ent[0..11], &mut sfn_buf);
                    let sfn_matches = eq_ignore_ascii_case(&sfn_buf[..sfn_n], target);
                    let name_matches = if lfn_len > 0 {
                        eq_ignore_ascii_case(lfn_slice(&lfn, lfn_len), target) || sfn_matches
                    } else {
                        sfn_matches
                    };
                    lfn_len = 0;
                    if name_matches {
                        let hi = u16::from_le_bytes([ent[20], ent[21]]) as u32;
                        let lo = u16::from_le_bytes([ent[26], ent[27]]) as u32;
                        let sz = u32::from_le_bytes([ent[28], ent[29], ent[30], ent[31]]);
                        return Ok(Some(((hi << 16) | lo, sz, attr)));
                    }
                }
            }
            clus = self.read_fat_entry(dev, clus)?;
        }
        Ok(None)
    }

    /// Ensure subdirectory `name` exists under `parent_clus`; returns subdirectory cluster.
    pub fn ensure_dir(
        &mut self,
        dev: &mut impl LogBlockDevice,
        parent_clus: u32,
        name: &[u8],
    ) -> Result<u32, isize> {
        if !self.mounted || name.is_empty() || name.len() > 64 {
            return Err(EINVAL);
        }
        if let Some((c, _, attr)) = self.find_entry(dev, parent_clus, name)? {
            if (attr & ATTR_DIR) == 0 || c < 2 {
                return Err(EIO);
            }
            return Ok(c);
        }
        if self.free_clusters < 2 {
            return Err(ENOSPC);
        }
        let new_clus = self.alloc_cluster(dev, None, true)?;
        let parent_dotdot = if parent_clus == self.root_clus { 0 } else { parent_clus };

        // Write "." and ".." entries into sector 0 of `new_clus`
        let mut dir_sec = [0u8; 512];
        write_83_entry(&mut dir_sec[0..32], *b".          ", ATTR_DIR, new_clus, 0);
        write_83_entry(&mut dir_sec[32..64], *b"..         ", ATTR_DIR, parent_dotdot, 0);
        let dir_lba = self.clus_to_lba(new_clus, 0)?;
        dev.write_sector(dir_lba, &dir_sec)?;

        self.insert_dir_entry(dev, parent_clus, name, ATTR_DIR, new_clus, 0)?;
        self.update_fs_info(dev)?;
        dev.flush()?;
        Ok(new_clus)
    }

    pub fn write_file_verified(
        &mut self,
        dev: &mut impl LogBlockDevice,
        dir_clus: u32,
        filename: &[u8],
        data: &[u8],
    ) -> Result<[u8; 32], isize> {
        if !self.mounted || filename.is_empty() || filename.len() > 64 {
            return Err(EINVAL);
        }
        if self.find_entry(dev, dir_clus, filename)?.is_some() {
            return Err(EIO); // Session paths should be clean & unique per boot session
        }

        let bytes_per_clus = (self.sec_per_clus as usize) * 512;
        let needed_clus = data.len().div_ceil(bytes_per_clus);
        // Reserve 1 extra cluster in case inserting directory slots expands `dir_clus` chain.
        if (self.free_clusters as usize) < needed_clus + 1 {
            return Err(ENOSPC);
        }

        let mut expected_sha = Sha256::new();
        expected_sha.update(data);
        let expected_digest = expected_sha.finalize();

        let mut first_clus = 0u32;
        let mut prev_clus: Option<u32> = None;
        let mut written = 0usize;

        for _ in 0..needed_clus {
            let c = self.alloc_cluster(dev, prev_clus, false)?;
            if first_clus == 0 {
                first_clus = c;
            }
            for s in 0..self.sec_per_clus {
                let mut sec = [0u8; 512];
                if written < data.len() {
                    let take = (data.len() - written).min(512);
                    sec[..take].copy_from_slice(&data[written..written + take]);
                    written += take;
                }
                let lba = self.clus_to_lba(c, s)?;
                dev.write_sector(lba, &sec)?;
            }
            prev_clus = Some(c);
        }

        // Handle huge files (> 64 clusters, though diagnostics fit in <= 32 KiB) cleanly via FAT walk if needed.
        self.update_fs_info(dev)?;
        dev.flush()?;

        let file_clus = if data.is_empty() { 0 } else { first_clus };
        self.insert_dir_entry(dev, dir_clus, filename, ATTR_ARCHIVE, file_clus, data.len() as u32)?;
        self.update_fs_info(dev)?;
        dev.flush()?;

        // Mandatory read-back verification directly from USB sectors before returning OK!
        if !data.is_empty() {
            let mut rb_sha = Sha256::new();
            let mut rem = data.len();
            let mut cur_clus = first_clus;
            let mut steps = 0usize;
            while rem > 0 && (2..FAT_EOC).contains(&cur_clus) && steps < needed_clus + 1 {
                steps += 1;
                for s in 0..self.sec_per_clus {
                    if rem == 0 {
                        break;
                    }
                    let lba = self.clus_to_lba(cur_clus, s)?;
                    let mut sec = [0u8; 512];
                    dev.read_sector(lba, &mut sec)?;
                    let take = rem.min(512);
                    rb_sha.update(&sec[..take]);
                    rem -= take;
                }
                if rem > 0 {
                    cur_clus = self.read_fat_entry(dev, cur_clus)?;
                }
            }
            let actual = rb_sha.finalize();
            if rem != 0 || !ct_eq_32(&actual, &expected_digest) {
                return Err(EIO);
            }
        }
        Ok(expected_digest)
    }

    /// Read up to `out.len()` bytes from an existing file (`file_clus`, `file_size`).
    pub fn read_file_bytes(
        &self,
        dev: &mut impl LogBlockDevice,
        file_clus: u32,
        file_size: u32,
        out: &mut [u8],
    ) -> Result<usize, isize> {
        if !self.mounted {
            return Err(EIO);
        }
        let target_len = (file_size as usize).min(out.len());
        if target_len == 0 {
            return Ok(0);
        }
        let mut copied = 0usize;
        let mut clus = file_clus;
        let mut steps = 0usize;
        while copied < target_len && (2..FAT_EOC).contains(&clus) && steps < 256 {
            steps += 1;
            for s in 0..self.sec_per_clus {
                if copied >= target_len {
                    break;
                }
                let lba = self.clus_to_lba(clus, s)?;
                let mut sec = [0u8; 512];
                dev.read_sector(lba, &mut sec)?;
                let take = (target_len - copied).min(512);
                out[copied..copied + take].copy_from_slice(&sec[..take]);
                copied += take;
            }
            if copied < target_len {
                clus = self.read_fat_entry(dev, clus)?;
            }
        }
        Ok(copied)
    }

    /// List subdirectory names inside `dir_clus` into newline-separated ASCII buffer.
    pub fn list_subdirs(
        &self,
        dev: &mut impl LogBlockDevice,
        dir_clus: u32,
        out: &mut [u8],
    ) -> Result<(usize, u8), isize> {
        if !self.mounted {
            return Err(EROFS);
        }
        let mut written = 0usize;
        let mut count = 0u8;
        let mut clus = dir_clus;
        let mut lfn = [0u8; 104];
        let mut lfn_len = 0usize;
        let mut sec = [0u8; 512];
        let mut steps = 0usize;

        while (2..FAT_EOC).contains(&clus) && steps < 128 {
            steps += 1;
            for s in 0..self.sec_per_clus {
                let lba = self.clus_to_lba(clus, s)?;
                dev.read_sector(lba, &mut sec)?;
                for slot in 0..16 {
                    let ent = &sec[slot * 32..slot * 32 + 32];
                    let first = ent[0];
                    if first == 0x00 {
                        return Ok((written, count));
                    }
                    if first == 0xE5 {
                        lfn_len = 0;
                        continue;
                    }
                    let attr = ent[11];
                    if attr == ATTR_LFN {
                        let seq = first & 0x1F;
                        if (first & 0x40) != 0 {
                            lfn.fill(0);
                            lfn_len = (seq as usize) * 13;
                        }
                        if seq >= 1 && (seq as usize) <= 8 {
                            extract_lfn_chars(ent, ((seq - 1) as usize) * 13, &mut lfn);
                        }
                        continue;
                    }
                    let mut name_buf = [0u8; 96];
                    let name_len = if lfn_len > 0 {
                        let sl = lfn_slice(&lfn, lfn_len);
                        let n = sl.len().min(name_buf.len());
                        name_buf[..n].copy_from_slice(&sl[..n]);
                        n
                    } else {
                        format_83_name(&ent[0..11], &mut name_buf)
                    };
                    lfn_len = 0;
                    if (attr & ATTR_DIR) == 0 || name_buf[..name_len] == *b"." || name_buf[..name_len] == *b".." {
                        continue;
                    }
                    count = count.saturating_add(1);
                    if written + name_len < out.len() {
                        out[written..written + name_len].copy_from_slice(&name_buf[..name_len]);
                        out[written + name_len] = b'\n';
                        written += name_len + 1;
                    }
                }
            }
            clus = self.read_fat_entry(dev, clus)?;
        }
        Ok((written, count))
    }

    fn insert_dir_entry(
        &mut self,
        dev: &mut impl LogBlockDevice,
        dir_clus: u32,
        name: &[u8],
        attr: u8,
        first_clus: u32,
        size_bytes: u32,
    ) -> Result<(), isize> {
        let lfn_entries = name.len().div_ceil(13);
        let need_slots = lfn_entries + 1;
        let sfn = make_unique_sfn(self, dev, dir_clus, name)?;
        let csum = lfn_checksum(&sfn);

        // Locate `need_slots` contiguous free/end slots in directory cluster chain, expanding if needed.
        let mut run = 0usize;
        let mut start_idx = 0usize;
        let mut clus_list = [0u32; 32];
        let mut clus_cnt = 0usize;
        let mut clus = dir_clus;

        while (2..FAT_EOC).contains(&clus) && clus_cnt < clus_list.len() {
            clus_list[clus_cnt] = clus;
            let base_slot = clus_cnt * (self.sec_per_clus as usize) * 16;
            clus_cnt += 1;
            let mut sec = [0u8; 512];
            for s in 0..self.sec_per_clus {
                let lba = self.clus_to_lba(clus, s)?;
                dev.read_sector(lba, &mut sec)?;
                for slot in 0..16 {
                    let b0 = sec[slot * 32];
                    let idx = base_slot + (s as usize) * 16 + slot;
                    if b0 == 0x00 || b0 == 0xE5 {
                        if run == 0 {
                            start_idx = idx;
                        }
                        run += 1;
                        if b0 == 0x00 || run >= need_slots {
                            return self.write_dir_slots(
                                dev, &mut clus_list, clus_cnt, start_idx, name, lfn_entries, &sfn, csum, attr, first_clus, size_bytes,
                            );
                        }
                    } else {
                        run = 0;
                    }
                }
            }
            let next = self.read_fat_entry(dev, clus)?;
            if next >= FAT_EOC {
                if clus_cnt >= clus_list.len() {
                    return Err(ENOSPC);
                }
                let ext = self.alloc_cluster(dev, Some(clus), true)?;
                if run == 0 {
                    start_idx = clus_cnt * (self.sec_per_clus as usize) * 16;
                }
                clus_list[clus_cnt] = ext;
                clus_cnt += 1;
                return self.write_dir_slots(
                    dev, &mut clus_list, clus_cnt, start_idx, name, lfn_entries, &sfn, csum, attr, first_clus, size_bytes,
                );
            }
            clus = next;
        }
        Err(ENOSPC)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_dir_slots(
        &mut self,
        dev: &mut impl LogBlockDevice,
        clus_list: &mut [u32; 32],
        mut clus_cnt: usize,
        start_idx: usize,
        name: &[u8],
        lfn_entries: usize,
        sfn: &[u8; 11],
        csum: u8,
        attr: u8,
        first_clus: u32,
        size_bytes: u32,
    ) -> Result<(), isize> {
        let slots_per_clus = (self.sec_per_clus as usize) * 16;
        let total = lfn_entries + 1;
        for i in 0..total {
            let abs_slot = start_idx + i;
            let ci = abs_slot / slots_per_clus;
            let rem = abs_slot % slots_per_clus;
            let s = (rem / 16) as u32;
            let slot_in_sec = rem % 16;
            while ci >= clus_cnt {
                let prev = clus_list[clus_cnt - 1];
                let next = self.read_fat_entry(dev, prev)?;
                let next_c = if (2..FAT_EOC).contains(&next) {
                    next
                } else {
                    if clus_cnt >= clus_list.len() {
                        return Err(ENOSPC);
                    }
                    self.alloc_cluster(dev, Some(prev), true)?
                };
                if clus_cnt >= clus_list.len() {
                    return Err(ENOSPC);
                }
                clus_list[clus_cnt] = next_c;
                clus_cnt += 1;
            }
            let clus = clus_list[ci];
            let lba = self.clus_to_lba(clus, s)?;
            let mut sec = [0u8; 512];
            dev.read_sector(lba, &mut sec)?;
            let ent = &mut sec[slot_in_sec * 32..slot_in_sec * 32 + 32];
            if i < lfn_entries {
                let ord = lfn_entries - i;
                write_lfn_entry(ent, ord, ord == lfn_entries, csum, name);
            } else {
                write_83_entry(ent, *sfn, attr, first_clus, size_bytes);
            }
            dev.write_sector(lba, &sec)?;
        }
        Ok(())
    }
}

fn lfn_checksum(sfn: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &b in sfn {
        sum = sum.rotate_right(1).wrapping_add(b);
    }
    sum
}

fn write_83_entry(dst: &mut [u8], sfn: [u8; 11], attr: u8, first_clus: u32, size_bytes: u32) {
    dst.fill(0);
    dst[0..11].copy_from_slice(&sfn);
    dst[11] = attr;
    dst[20..22].copy_from_slice(&((first_clus >> 16) as u16).to_le_bytes());
    dst[26..28].copy_from_slice(&(first_clus as u16).to_le_bytes());
    dst[28..32].copy_from_slice(&size_bytes.to_le_bytes());
}

fn write_lfn_entry(dst: &mut [u8], seq: usize, is_last: bool, csum: u8, name: &[u8]) {
    dst.fill(0xFF);
    dst[0] = (seq as u8) | if is_last { 0x40 } else { 0 };
    dst[11] = ATTR_LFN;
    dst[12] = 0;
    dst[13] = csum;
    dst[26] = 0;
    dst[27] = 0;
    let char_offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let base = (seq - 1) * 13;
    for (j, &off) in char_offsets.iter().enumerate() {
        let idx = base + j;
        let code: u16 = if idx < name.len() {
            name[idx] as u16
        } else if idx == name.len() {
            0x0000
        } else {
            0xFFFF
        };
        dst[off..off + 2].copy_from_slice(&code.to_le_bytes());
    }
}

fn extract_lfn_chars(ent: &[u8], base: usize, out: &mut [u8]) {
    let char_offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    for (j, &off) in char_offsets.iter().enumerate() {
        if base + j >= out.len() {
            break;
        }
        let ch = u16::from_le_bytes([ent[off], ent[off + 1]]);
        if ch == 0x0000 || ch == 0xFFFF {
            out[base + j] = 0;
            break;
        }
        out[base + j] = if ch <= 0x7F { ch as u8 } else { b'_' };
    }
}

fn lfn_slice(buf: &[u8], max_len: usize) -> &[u8] {
    let lim = max_len.min(buf.len());
    let len = buf[..lim].iter().position(|&b| b == 0).unwrap_or(lim);
    &buf[..len]
}

fn format_83_name(sfn: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0usize;
    let stem_len = sfn[0..8].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    for &b in &sfn[..stem_len] {
        if n < out.len() {
            out[n] = b;
            n += 1;
        }
    }
    let ext_len = sfn[8..11].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    if ext_len > 0 && n + 1 + ext_len <= out.len() {
        out[n] = b'.';
        n += 1;
        out[n..n + ext_len].copy_from_slice(&sfn[8..8 + ext_len]);
        n += ext_len;
    }
    n
}

fn make_unique_sfn(
    fs: &Fat32LogFs,
    dev: &mut impl LogBlockDevice,
    dir_clus: u32,
    name: &[u8],
) -> Result<[u8; 11], isize> {
    let mut stem = [b' '; 8];
    let mut ext = [b' '; 3];
    let dot = name.iter().rposition(|&b| b == b'.');
    let raw_stem = dot.map_or(name, |p| &name[..p]);
    let raw_ext = dot.map_or(&[][..], |p| &name[p + 1..]);

    let clean_char = |b: u8| -> u8 {
        match b {
            b'a'..=b'z' => b - 32,
            b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => b,
            _ => b'_',
        }
    };
    let mut sl = 0usize;
    for (idx, &b) in raw_stem.iter().take(6).enumerate() {
        stem[idx] = clean_char(b);
        sl = idx + 1;
    }
    if sl == 0 {
        stem[0] = b'F';
        sl = 1;
    }
    for (el, &b) in raw_ext.iter().take(3).enumerate() {
        ext[el] = clean_char(b);
    }
    for n in 1..=99u8 {
        let mut cand = [b' '; 11];
        cand[0..8].copy_from_slice(&stem);
        let digits = if n < 10 { 2 } else { 3 };
        let base_len = sl.min(8 - digits);
        cand[base_len..8].fill(b' ');
        cand[base_len] = b'~';
        if n < 10 {
            cand[base_len + 1] = b'0' + n;
        } else {
            cand[base_len + 1] = b'0' + (n / 10);
            cand[base_len + 2] = b'0' + (n % 10);
        }
        cand[8..11].copy_from_slice(&ext);
        let mut sfn_str = [0u8; 12];
        let sfn_len = format_83_name(&cand, &mut sfn_str);
        if fs.find_entry(dev, dir_clus, &sfn_str[..sfn_len])?.is_none() {
            return Ok(cand);
        }
    }
    Err(ENOSPC)
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(&x, &y)| x.eq_ignore_ascii_case(&y))
}
