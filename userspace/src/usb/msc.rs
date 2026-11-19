//! Ring-3 USB Mass Storage Bulk-Only Transport (`08:06:50`) Block Driver (`INV-DISK-SAFE-02`).
//!
//! Safety Invariant (`INV-DISK-SAFE-02`):
//! - Scans GPT / MBR to isolate Partition 2 (`DRYOS_PERSIST`) and verifies `DRYFS_V1` magic on
//!   Partition 2 block 0 or 1.
//! - Rejects every `IORING_OP_WRITE_SECTORS` unless `dryfs_verified == true`,
//!   `read_only_policy == false`, and `rel_sector + count <= part2_sectors`.
//! - Never writes to LBA 0 (GPT) or Partition 1 (ESP).

use core::ptr::{read_volatile, write_volatile};

use crate::ioring::{
    IoCqe, IoSqe, EIO_RING, IORING_OP_FLUSH, IORING_OP_READ_SECTORS, IORING_OP_WRITE_SECTORS,
};
use crate::persist_fs::DRYFS_MAGIC;
use crate::usb::xhci::XhciController;
use crate::{EINVAL, EPIPE, EROFS};

const EIO: isize = -5;

const CBW_SIG: u32 = 0x4342_5355; // "USBC"
const CSW_SIG: u32 = 0x5342_5355; // "USBS"

pub struct UsbMscBot {
    pub active: bool,
    pub ready: bool,
    pub dryfs_verified: bool,
    pub slot_id: u8,
    pub iface_num: u8,
    pub ep_in_addr: u8,
    pub ep_out_addr: u8,
    pub in_dci: u8,
    pub out_dci: u8,
    pub in_ring: usize,
    pub out_ring: usize,
    pub max_lun: u8,
    pub total_sectors: u64,
    pub part2_start: u64,
    pub part2_sectors: u64,
    pub part2_idx: u8,
    pub reads: u64,
    pub writes: u64,
    pub flushes: u64,
    pub io_errs: u64,
    pub rofs_rejects: u64,
    pub vendor_product: [u8; 24],
    next_tag: u32,
}

impl UsbMscBot {
    pub const fn empty() -> Self {
        Self {
            active: false,
            ready: false,
            dryfs_verified: false,
            slot_id: 0,
            iface_num: 0,
            ep_in_addr: 0,
            ep_out_addr: 0,
            in_dci: 0,
            out_dci: 0,
            in_ring: 0,
            out_ring: 0,
            max_lun: 0,
            total_sectors: 0,
            part2_start: 0,
            part2_sectors: 0,
            part2_idx: 0,
            reads: 0,
            writes: 0,
            flushes: 0,
            io_errs: 0,
            rofs_rejects: 0,
            vendor_product: [0; 24],
            next_tag: 1,
        }
    }

    pub fn probe_and_verify(&mut self, hc: &mut XhciController) -> Result<(), isize> {
        if !self.active {
            return Err(EIO);
        }
        // Optional BOT GET_MAX_LUN; devices may STALL if single-LUN.
        if hc
            .control_xfer(self.slot_id, 0xa1, 0xfe, 0, u16::from(self.iface_num), 1)
            .is_ok()
        {
            self.max_lun = hc.ctrl_bounce_slice()[0].min(7);
        }

        // Wait briefly if QEMU storage reports initial Unit Attention.
        for _ in 0..3 {
            let mut tur = [0u8; 6];
            tur[0] = 0x00; // TEST UNIT READY
            if self.bot_cmd(hc, &tur, false, 0).is_ok() {
                break;
            }
            let mut req_sense = [0u8; 6];
            req_sense[0] = 0x03; // REQUEST SENSE
            req_sense[4] = 18;
            let _ = self.bot_cmd(hc, &req_sense, true, 18);
            hc.sleep_ms(10);
        }

        // INQUIRY (36 B).
        let mut inq = [0u8; 6];
        inq[0] = 0x12;
        inq[4] = 36;
        if self.bot_cmd(hc, &inq, true, 36).is_ok() {
            let b = hc.bulk_bounce_slice();
            self.vendor_product.copy_from_slice(&b[8..32]);
        }

        // READ CAPACITY(10) (8 B).
        let mut rcap = [0u8; 10];
        rcap[0] = 0x25;
        self.bot_cmd(hc, &rcap, true, 8)?;
        let b = hc.bulk_bounce_slice();
        let last_lba = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
        let blk_sz = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
        if blk_sz != 512 || last_lba == 0 {
            return Err(EIO);
        }
        self.total_sectors = last_lba + 1;
        self.ready = true;

        self.discover_partition2_and_dryfs(hc)?;
        Ok(())
    }

    fn bot_cmd(
        &mut self,
        hc: &mut XhciController,
        cdb: &[u8],
        dir_in: bool,
        data_len: u32,
    ) -> Result<(), isize> {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1).max(1);

        let (cbw_va, cbw_pa) = hc.cbw_ptr_pa();
        let (csw_va, csw_pa) = hc.csw_ptr_pa();
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&CBW_SIG.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&data_len.to_le_bytes());
        cbw[12] = if dir_in { 0x80 } else { 0x00 };
        cbw[13] = 0; // LUN 0
        cbw[14] = cdb.len().min(16) as u8;
        cbw[15..15 + cdb.len().min(16)].copy_from_slice(&cdb[..cdb.len().min(16)]);
        for (i, &byte) in cbw.iter().enumerate() {
            unsafe { write_volatile((cbw_va + i as u64) as *mut u8, byte) };
        }

        let r_cbw = hc.sync_normal_trb(self.slot_id, self.out_dci, self.out_ring, cbw_pa, 31, 1000);
        if let Err(e) = r_cbw {
            if e == EPIPE {
                hc.recover_endpoint(self.slot_id, self.out_dci, self.ep_out_addr, self.out_ring);
            } else {
                self.bot_mass_storage_reset(hc);
            }
            return Err(EIO);
        }

        let mut data_err: Option<isize> = None;
        if data_len > 0 {
            let (dci, ring, addr) = if dir_in {
                (self.in_dci, self.in_ring, self.ep_in_addr)
            } else {
                (self.out_dci, self.out_ring, self.ep_out_addr)
            };
            if let Err(e) = hc.sync_normal_trb(
                self.slot_id,
                dci,
                ring,
                hc.bulk_bounce_pa,
                data_len,
                1500,
            ) {
                if e == EPIPE {
                    hc.recover_endpoint(self.slot_id, dci, addr, ring);
                    data_err = Some(e);
                } else {
                    self.bot_mass_storage_reset(hc);
                    return Err(EIO);
                }
            }
        }

        let mut csw_res =
            hc.sync_normal_trb(self.slot_id, self.in_dci, self.in_ring, csw_pa, 13, 1000);
        if csw_res == Err(EPIPE) {
            hc.recover_endpoint(self.slot_id, self.in_dci, self.ep_in_addr, self.in_ring);
            csw_res =
                hc.sync_normal_trb(self.slot_id, self.in_dci, self.in_ring, csw_pa, 13, 1000);
        }
        if csw_res.is_err() {
            self.bot_mass_storage_reset(hc);
            return Err(EIO);
        }

        let mut csw = [0u8; 13];
        for (i, dst) in csw.iter_mut().enumerate() {
            *dst = unsafe { read_volatile((csw_va + i as u64) as *const u8) };
        }
        let sig = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
        let csw_tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
        let status = csw[12];
        if sig != CSW_SIG || csw_tag != tag || status == 2 {
            self.bot_mass_storage_reset(hc);
            return Err(EIO);
        }
        if let Some(e) = data_err {
            return Err(e);
        }
        if status != 0 {
            return Err(EIO);
        }
        Ok(())
    }

    fn bot_mass_storage_reset(&self, hc: &mut XhciController) {
        let _ = hc.control_xfer(self.slot_id, 0x21, 0xff, 0, u16::from(self.iface_num), 0);
        hc.recover_endpoint(self.slot_id, self.in_dci, self.ep_in_addr, self.in_ring);
        hc.recover_endpoint(self.slot_id, self.out_dci, self.ep_out_addr, self.out_ring);
    }

    fn read_sectors_raw(&mut self, hc: &mut XhciController, lba: u64, count: u16) -> Result<(), isize> {
        if count == 0 || count > 8 || lba.saturating_add(count as u64) > self.total_sectors {
            return Err(EINVAL);
        }
        let mut cdb = [0u8; 10];
        cdb[0] = 0x28; // READ(10)
        cdb[2..6].copy_from_slice(&(lba as u32).to_be_bytes());
        cdb[7..9].copy_from_slice(&count.to_be_bytes());
        self.bot_cmd(hc, &cdb, true, (count as u32) * 512)
    }

    fn write_sectors_raw(&mut self, hc: &mut XhciController, lba: u64, count: u16) -> Result<(), isize> {
        if count == 0 || count > 8 || lba.saturating_add(count as u64) > self.total_sectors {
            return Err(EINVAL);
        }
        let mut cdb = [0u8; 10];
        cdb[0] = 0x2a; // WRITE(10)
        cdb[2..6].copy_from_slice(&(lba as u32).to_be_bytes());
        cdb[7..9].copy_from_slice(&count.to_be_bytes());
        self.bot_cmd(hc, &cdb, false, (count as u32) * 512)
    }

    fn discover_partition2_and_dryfs(&mut self, hc: &mut XhciController) -> Result<(), isize> {
        // Read LBA 0..8 first (4 KiB): covers MBR (0), GPT header (1), and first entries (2..7).
        let probe_n = self.total_sectors.min(8) as u16;
        if probe_n < 2 {
            return Ok(());
        }
        self.read_sectors_raw(hc, 0, probe_n)?;
        let hdr = hc.bulk_bounce_slice();

        let mut found: Option<(u64, u64, u8)> = None;
        if probe_n >= 3 && &hdr[512..520] == b"EFI PART" {
            let entry_lba = u64::from_le_bytes([
                hdr[512 + 72],
                hdr[512 + 73],
                hdr[512 + 74],
                hdr[512 + 75],
                hdr[512 + 76],
                hdr[512 + 77],
                hdr[512 + 78],
                hdr[512 + 79],
            ]);
            let num_entries = u32::from_le_bytes([
                hdr[512 + 80],
                hdr[512 + 81],
                hdr[512 + 82],
                hdr[512 + 83],
            ])
            .min(128);
            let entry_sz = u32::from_le_bytes([
                hdr[512 + 84],
                hdr[512 + 85],
                hdr[512 + 86],
                hdr[512 + 87],
            ]);
            if entry_sz == 128 && entry_lba >= 2 {
                let total_entry_secs = ((num_entries * 128).div_ceil(512)).min(32);
                let mut fallback_p2: Option<(u64, u64, u8)> = None;
                let mut sec_off = 0u32;
                while sec_off < total_entry_secs {
                    let batch = (total_entry_secs - sec_off).min(8) as u16;
                    self.read_sectors_raw(hc, entry_lba + sec_off as u64, batch)?;
                    let buf = hc.bulk_bounce_slice();
                    for idx_in_batch in 0..(batch as usize * 4) {
                        let global_idx = (sec_off as usize) * 4 + idx_in_batch;
                        if global_idx >= num_entries as usize {
                            break;
                        }
                        let off = idx_in_batch * 128;
                        if !buf[off..off + 16].iter().any(|&x| x != 0) {
                            continue;
                        }
                        let first = u64::from_le_bytes([
                            buf[off + 32],
                            buf[off + 33],
                            buf[off + 34],
                            buf[off + 35],
                            buf[off + 36],
                            buf[off + 37],
                            buf[off + 38],
                            buf[off + 39],
                        ]);
                        let last = u64::from_le_bytes([
                            buf[off + 40],
                            buf[off + 41],
                            buf[off + 42],
                            buf[off + 43],
                            buf[off + 44],
                            buf[off + 45],
                            buf[off + 46],
                            buf[off + 47],
                        ]);
                        if last < first || first < 34 || last >= self.total_sectors {
                            continue;
                        }
                        let len = last - first + 1;
                        if utf16_starts_with_ascii(&buf[off + 56..off + 128], b"DRYOS_PERSIST") {
                            found = Some((first, len, (global_idx + 1) as u8));
                            break;
                        }
                        if global_idx == 1 && fallback_p2.is_none() {
                            fallback_p2 = Some((first, len, 2));
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                    sec_off += batch as u32;
                }
                if found.is_none() {
                    found = fallback_p2;
                }
            }
        } else if hdr[510] == 0x55 && hdr[511] == 0xaa {
            // DOS MBR Partition #2 (`offset 446 + 16`).
            let p_type = hdr[462 + 4];
            let p_start = u32::from_le_bytes([hdr[462 + 8], hdr[462 + 9], hdr[462 + 10], hdr[462 + 11]]) as u64;
            let p_secs = u32::from_le_bytes([hdr[462 + 12], hdr[462 + 13], hdr[462 + 14], hdr[462 + 15]]) as u64;
            if p_type != 0 && p_start >= 2 && p_secs >= 32 && p_start + p_secs <= self.total_sectors {
                found = Some((p_start, p_secs, 2));
            }
        }

        let Some((p2_start, p2_secs, p2_idx)) = found else {
            return Ok(());
        };
        self.part2_start = p2_start;
        self.part2_sectors = p2_secs;
        self.part2_idx = p2_idx;

        // Verify `*b"DRYFS_V1"` on Partition 2 Block 0 (p2_start) or Block 1 (p2_start + 8).
        let mut magic_ok = false;
        if self.part2_sectors >= 16 {
            let ok0 = self.read_sectors_raw(hc, self.part2_start, 8).is_ok()
                && hc.bulk_bounce_slice()[0..8] == DRYFS_MAGIC;
            magic_ok = ok0
                || (self.read_sectors_raw(hc, self.part2_start + 8, 8).is_ok()
                    && hc.bulk_bounce_slice()[0..8] == DRYFS_MAGIC);
        }
        self.dryfs_verified = magic_ok;
        Ok(())
    }

    pub fn execute_sqe(
        &mut self,
        hc: &mut XhciController,
        sqe: IoSqe,
        data_vaddr: u64,
        data_live: bool,
        read_only_policy: bool,
    ) -> IoCqe {
        let err = |status: isize| IoCqe { user_tag: sqe.user_tag, status: status as i32, bytes_done: 0 };

        if !self.ready || !self.dryfs_verified {
            self.io_errs = self.io_errs.wrapping_add(1);
            return err(EIO_RING);
        }

        match sqe.opcode {
            IORING_OP_READ_SECTORS => {
                let count = sqe.sector_count;
                let byte_len = (count as usize) * 512;
                if !data_live || count == 0 || count > 8 || (sqe.buf_offset as usize).saturating_add(byte_len) > 4096 {
                    return err(EINVAL);
                }
                let Some(end_rel) = sqe.lba.checked_add(count as u64) else {
                    self.io_errs = self.io_errs.wrapping_add(1);
                    return err(EIO_RING);
                };
                if end_rel > self.part2_sectors {
                    self.io_errs = self.io_errs.wrapping_add(1);
                    return err(EIO_RING);
                }
                let abs_lba = self.part2_start + sqe.lba;
                if self.read_sectors_raw(hc, abs_lba, count).is_err() {
                    self.io_errs = self.io_errs.wrapping_add(1);
                    return err(EIO_RING);
                }
                // SAFETY: Validated `buf_offset + byte_len <= 4096` within mapped `CLIENT_DATA_VADDR`.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        hc.bulk_bounce_slice().as_ptr(),
                        (data_vaddr + sqe.buf_offset as u64) as *mut u8,
                        byte_len,
                    );
                }
                self.reads = self.reads.wrapping_add(1);
                IoCqe { user_tag: sqe.user_tag, status: 0, bytes_done: byte_len as u32 }
            }
            IORING_OP_WRITE_SECTORS => {
                // INV-DISK-SAFE-02 strict write fence:
                if read_only_policy {
                    self.rofs_rejects = self.rofs_rejects.wrapping_add(1);
                    return err(EROFS);
                }
                let count = sqe.sector_count;
                let byte_len = (count as usize) * 512;
                if !data_live || count == 0 || count > 8 || (sqe.buf_offset as usize).saturating_add(byte_len) > 4096 {
                    return err(EINVAL);
                }
                let Some(end_rel) = sqe.lba.checked_add(count as u64) else {
                    self.io_errs = self.io_errs.wrapping_add(1);
                    return err(EIO_RING);
                };
                if !self.dryfs_verified || end_rel > self.part2_sectors {
                    self.io_errs = self.io_errs.wrapping_add(1);
                    return err(EIO_RING);
                }
                let abs_lba = self.part2_start + sqe.lba;
                if abs_lba < self.part2_start || abs_lba + (count as u64) > self.part2_start + self.part2_sectors {
                    self.io_errs = self.io_errs.wrapping_add(1);
                    return err(EIO_RING);
                }
                // SAFETY: Validated `buf_offset + byte_len <= 4096` within mapped `CLIENT_DATA_VADDR`.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        (data_vaddr + sqe.buf_offset as u64) as *const u8,
                        hc.bulk_bounce_mut().as_mut_ptr(),
                        byte_len,
                    );
                }
                if self.write_sectors_raw(hc, abs_lba, count).is_err() {
                    self.io_errs = self.io_errs.wrapping_add(1);
                    return err(EIO_RING);
                }
                self.writes = self.writes.wrapping_add(1);
                IoCqe { user_tag: sqe.user_tag, status: 0, bytes_done: byte_len as u32 }
            }
            IORING_OP_FLUSH => {
                if !read_only_policy {
                    let mut sync_cdb = [0u8; 10];
                    sync_cdb[0] = 0x35; // SYNCHRONIZE CACHE(10)
                    let _ = self.bot_cmd(hc, &sync_cdb, false, 0);
                }
                self.flushes = self.flushes.wrapping_add(1);
                IoCqe { user_tag: sqe.user_tag, status: 0, bytes_done: 0 }
            }
            _ => err(EINVAL),
        }
    }
}

fn utf16_starts_with_ascii(u16le: &[u8], ascii: &[u8]) -> bool {
    if u16le.len() < ascii.len() * 2 {
        return false;
    }
    for (i, &ch) in ascii.iter().enumerate() {
        if u16le[i * 2] != ch || u16le[i * 2 + 1] != 0 {
            return false;
        }
    }
    true
}
