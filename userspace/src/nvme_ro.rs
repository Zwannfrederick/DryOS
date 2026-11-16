//! Strictly Read-Only Ring-3 NVMe Driver Library (`NVMe 1.4` subset, `INV-DISK-SAFE-01`).
//!
//! Safety Invariant (`INV-DISK-SAFE-01`):
//! Compiles ONLY Admin `IDENTIFY (0x06)`, `CREATE_IO_CQ (0x05)`, `CREATE_IO_SQ (0x01)`, and
//! NVM `READ (0x02)`. Zero mutating storage opcodes exist anywhere in this module.

use core::mem::size_of;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::abi_ext::{sys_dev_map_window, sys_dev_query_bars, sys_dma_alloc, UserPciDeviceInfo};
use crate::{
    rdtsc_user, sys_cap_revoke, sys_irq_ack, sys_irq_bind, sys_notify_create, sys_sysinfo,
    sys_yield, EBUSY, ETIMEDOUT, IRQF_ONESHOT, IRQ_ACK_UNBIND, IRQ_ACK_UNMASK, UserSysInfo,
};

const EIO: isize = -5;
const ENODEV: isize = -19;

// INV-DISK-SAFE-01: strictly read-only command opcode set.
const NVME_ADMIN_CREATE_IO_SQ: u8 = 0x01;
const NVME_ADMIN_CREATE_IO_CQ: u8 = 0x05;
const NVME_ADMIN_IDENTIFY: u8 = 0x06;
const NVME_NVM_READ: u8 = 0x02;

pub const NVME_FLAG_READY: u16 = 1 << 0;
pub const NVME_FLAG_IO_READ_OK: u16 = 1 << 1;
pub const NVME_FLAG_GPT_OK: u16 = 1 << 2;
pub const NVME_FLAG_MSI_BOUND: u16 = 1 << 3;
pub const NVME_FLAG_VMD_GATED: u16 = 1 << 4;
pub const NVME_FLAG_IOMMU_PROTECTED: u16 = 1 << 5;

const NVME_BAR_P0_VA: u64 = 0x1000_0000;
const NVME_BAR_DB_VA: u64 = 0x1001_0000;
const NVME_BAR_DB2_VA: u64 = 0x1002_0000;

const NVME_DMA_ASQ_VA: u64 = 0x1100_0000;
const NVME_DMA_ACQ_VA: u64 = 0x1100_1000;
const NVME_DMA_IOSQ_VA: u64 = 0x1100_2000;
const NVME_DMA_IOCQ_VA: u64 = 0x1100_3000;
const NVME_DMA_DATA_VA: u64 = 0x1100_4000;

const ADMIN_Q_DEPTH: usize = 64;
const IO_Q_DEPTH: usize = 16;

/// Compact read-only NVMe telemetry snapshot transferred over `NVME_OP_QUERY_INFO` (`<= 256` B).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvmeInfo {
    pub vendor_id: u16,
    pub device_id: u16,
    pub bus_dev_fn: u16,
    pub state_flags: u16,
    pub nsze_sectors: u64,
    pub lba_bytes: u32,
    pub nn: u32,
    pub vs: u32,
    pub cqes_read: u32,
    pub gpt_first_usable: u64,
    pub gpt_last_usable: u64,
    pub part1_start: u64,
    pub part1_sectors: u64,
    pub part2_start: u64,
    pub part2_sectors: u64,
    pub serial: [u8; 20],
    pub model: [u8; 40],
    pub fw_rev: [u8; 8],
}

const _: () = assert!(size_of::<NvmeInfo>() <= 256);

impl NvmeInfo {
    pub const fn empty() -> Self {
        Self {
            vendor_id: 0,
            device_id: 0,
            bus_dev_fn: 0,
            state_flags: 0,
            nsze_sectors: 0,
            lba_bytes: 512,
            nn: 0,
            vs: 0,
            cqes_read: 0,
            gpt_first_usable: 0,
            gpt_last_usable: 0,
            part1_start: 0,
            part1_sectors: 0,
            part2_start: 0,
            part2_sectors: 0,
            serial: [0; 20],
            model: [0; 40],
            fw_rev: [0; 8],
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `NvmeInfo` is `#[repr(C)]` POD with explicit fields and no padding traps.
        unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>()) }
    }

    pub fn from_bytes(raw: &[u8]) -> Option<Self> {
        if raw.len() < size_of::<Self>() {
            return None;
        }
        let mut out = Self::empty();
        // SAFETY: `NvmeInfo` is `#[repr(C)]` POD (`<= 256` bytes).
        unsafe {
            core::ptr::copy_nonoverlapping(
                raw.as_ptr(),
                (&raw mut out).cast::<u8>(),
                size_of::<Self>(),
            );
        }
        Some(out)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NvmeSqe {
    cdw0: u32,
    nsid: u32,
    rsvd: u64,
    mptr: u64,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NvmeCqe {
    dw0: u32,
    dw1: u32,
    sq_head_sq_id: u32,
    dw3_status_cid: u32,
}

const _: () = assert!(size_of::<NvmeSqe>() == 64);
const _: () = assert!(size_of::<NvmeCqe>() == 16);

struct NvmeQueuePair {
    sq_va: u64,
    cq_va: u64,
    sq_db_va: u64,
    cq_db_va: u64,
    depth: usize,
    sq_tail: usize,
    cq_head: usize,
    cq_phase: u32,
    next_cid: u16,
}

impl NvmeQueuePair {
    fn submit_and_wait(
        &mut self,
        cmd: NvmeSqe,
        tsc_mhz: u64,
        timeout_ms: u64,
        cqes_done: &mut u32,
    ) -> Result<u32, isize> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1).max(1);
        let mut entry = cmd;
        entry.cdw0 = (entry.cdw0 & 0x0000_ffff) | ((cid as u32) << 16);

        // SAFETY: `sq_va` is a pinned 4 KiB DMA page with `depth <= 64`.
        unsafe {
            let sq_ptr = (self.sq_va as *mut NvmeSqe).add(self.sq_tail);
            write_volatile(sq_ptr, entry);
        }
        self.sq_tail = (self.sq_tail + 1) % self.depth;
        fence(Ordering::SeqCst);
        // SAFETY: `sq_db_va` points into the mapped NVMe BAR0 doorbell window.
        unsafe { write_volatile(self.sq_db_va as *mut u32, self.sq_tail as u32) };

        let deadline = rdtsc_user().saturating_add(timeout_ms * 1000 * tsc_mhz.max(1000));
        loop {
            fence(Ordering::Acquire);
            // SAFETY: `cq_va` is a pinned 4 KiB DMA page with `depth <= 64`.
            let cqe = unsafe { read_volatile((self.cq_va as *const NvmeCqe).add(self.cq_head)) };
            let phase = (cqe.dw3_status_cid >> 16) & 1;
            if phase == self.cq_phase {
                self.cq_head += 1;
                if self.cq_head == self.depth {
                    self.cq_head = 0;
                    self.cq_phase ^= 1;
                }
                fence(Ordering::SeqCst);
                // SAFETY: `cq_db_va` points into mapped NVMe BAR0 doorbell window.
                unsafe { write_volatile(self.cq_db_va as *mut u32, self.cq_head as u32) };
                *cqes_done = cqes_done.saturating_add(1);
                let status_field = (cqe.dw3_status_cid >> 17) & 0x7fff;
                let sct = (status_field >> 8) & 0x7;
                let sc = status_field & 0xff;
                if sct != 0 || sc != 0 {
                    return Err(EIO);
                }
                return Ok(cqe.dw0);
            }
            if rdtsc_user() >= deadline {
                return Err(ETIMEDOUT);
            }
            let _ = sys_yield();
        }
    }
}

pub struct NvmeReadOnlyCtrl {
    pci_cap: u64,
    bar0_p0: u64,
    notif_cap: u64,
    msi_bound: bool,
    dma_caps: [u64; 5],
    data_paddr: u64,
    tsc_mhz: u64,
    timeout_ms: u64,
    admin_q: NvmeQueuePair,
    io_q: NvmeQueuePair,
    pub info: NvmeInfo,
}

#[inline]
unsafe fn mmio_r32(base: u64, off: usize) -> u32 {
    unsafe { read_volatile((base as usize + off) as *const u32) }
}

#[inline]
unsafe fn mmio_w32(base: u64, off: usize, v: u32) {
    unsafe { write_volatile((base as usize + off) as *mut u32, v) }
}

#[inline]
unsafe fn mmio_r64(base: u64, off: usize) -> u64 {
    let lo = unsafe { read_volatile((base as usize + off) as *const u32) } as u64;
    let hi = unsafe { read_volatile((base as usize + off + 4) as *const u32) } as u64;
    lo | (hi << 32)
}

#[inline]
unsafe fn mmio_w64(base: u64, off: usize, v: u64) {
    unsafe {
        write_volatile((base as usize + off) as *mut u32, v as u32);
        write_volatile((base as usize + off + 4) as *mut u32, (v >> 32) as u32);
    }
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

impl NvmeReadOnlyCtrl {
    pub fn init(pci_cap: u64) -> Result<Self, isize> {
        let mut sys = UserSysInfo::default();
        let _ = sys_sysinfo(&mut sys);
        let tsc_mhz = sys.tsc_mhz.max(1000);

        let mut pci_info = UserPciDeviceInfo::default();
        sys_dev_query_bars(pci_cap, &mut pci_info)?;
        if pci_info.bars[0].size_bytes < 0x2000 {
            return Err(ENODEV);
        }

        // Map page 0 (0x0000..0x0fff) cleanly without overlapping BAR0 MSI-X pages.
        sys_dev_map_window(pci_cap, NVME_BAR_P0_VA, 0, false, 1, 0)?;
        let suboff = pci_info.bars[0].page_suboff as u64;
        let bar0_p0 = NVME_BAR_P0_VA + suboff;

        // Map page 1 (SQ0TDBL / CQ0HDBL at +0x1000) and check DSTRD for IO queue doorbells.
        sys_dev_map_window(pci_cap, NVME_BAR_DB_VA, 0, false, 1, 1)?;
        let cap = unsafe { mmio_r64(bar0_p0, 0x00) };
        let vs = unsafe { mmio_r32(bar0_p0, 0x08) };
        let dstrd = ((cap >> 32) & 0x0f) as usize;
        let db_stride = 4usize << dstrd;
        let to_units = ((cap >> 24) & 0xff).max(4);
        let timeout_ms = (to_units * 500).clamp(2000, 10000);

        let cq1_off = 0x1000usize + 3 * db_stride;
        let db2_page = cq1_off / 4096;
        let io_db_page_va = if db2_page == 1 {
            NVME_BAR_DB_VA + suboff
        } else {
            sys_dev_map_window(pci_cap, NVME_BAR_DB2_VA, 0, false, 1, db2_page as u32)?;
            NVME_BAR_DB2_VA + suboff
        };
        let sq0_db_va = NVME_BAR_DB_VA + suboff;
        let cq0_db_va = sq0_db_va + db_stride as u64;
        let sq1_db_va = if (0x1000 + 2 * db_stride) / 4096 == 1 {
            NVME_BAR_DB_VA + suboff + (2 * db_stride) as u64
        } else {
            io_db_page_va + ((0x1000 + 2 * db_stride) & 0xfff) as u64
        };
        let cq1_db_va = io_db_page_va + (cq1_off & 0xfff) as u64;

        // Disable controller if left enabled by firmware before writing AQA/ASQ/ACQ.
        let cc_cur = unsafe { mmio_r32(bar0_p0, 0x14) };
        if (cc_cur & 1) != 0 {
            unsafe { mmio_w32(bar0_p0, 0x14, cc_cur & !1) };
            Self::wait_csts_ready(bar0_p0, false, tsc_mhz, timeout_ms)?;
        }

        // Allocate 5 pinned 4 KiB DMA pages (ASQ, ACQ, IOSQ, IOCQ, DATA bounce).
        let vas = [
            NVME_DMA_ASQ_VA,
            NVME_DMA_ACQ_VA,
            NVME_DMA_IOSQ_VA,
            NVME_DMA_IOCQ_VA,
            NVME_DMA_DATA_VA,
        ];
        let mut dma_caps = [0u64; 5];
        let mut paddrs = [0u64; 5];
        for i in 0..5 {
            match sys_dma_alloc(pci_cap, vas[i]) {
                Ok((cap_h, pa)) => {
                    dma_caps[i] = cap_h;
                    paddrs[i] = pa;
                    // SAFETY: `sys_dma_alloc` mapped a fresh 4 KiB `USER_RW` page at `vas[i]`.
                    unsafe { core::ptr::write_bytes(vas[i] as *mut u8, 0, 4096) };
                }
                Err(e) => {
                    for c in dma_caps {
                        if c != 0 {
                            let _ = sys_cap_revoke(c, 1);
                        }
                    }
                    return Err(e);
                }
            }
        }

        let aqa = (((ADMIN_Q_DEPTH as u32) - 1) << 16) | ((ADMIN_Q_DEPTH as u32) - 1);
        unsafe {
            mmio_w32(bar0_p0, 0x24, aqa);
            mmio_w64(bar0_p0, 0x28, paddrs[0]);
            mmio_w64(bar0_p0, 0x30, paddrs[1]);
            // CC: IOCQES=4 (16B), IOSQES=6 (64B), MPS=0 (4 KiB), CSS=0 (NVM), EN=1.
            mmio_w32(bar0_p0, 0x14, (4 << 20) | (6 << 16) | 1);
        }
        Self::wait_csts_ready(bar0_p0, true, tsc_mhz, timeout_ms)?;

        let notif_cap = sys_notify_create(0).unwrap_or(0);
        let msi_bound = if notif_cap != 0 {
            sys_irq_bind(pci_cap, notif_cap, IRQF_ONESHOT).is_ok()
        } else {
            false
        };

        let mut info = NvmeInfo::empty();
        info.vendor_id = pci_info.vendor_id;
        info.device_id = pci_info.device_id;
        info.bus_dev_fn = pci_info.bus_dev_fn;
        info.vs = vs;
        info.state_flags = NVME_FLAG_READY
            | if msi_bound { NVME_FLAG_MSI_BOUND } else { 0 }
            | if pci_info.iommu_protected != 0 {
                NVME_FLAG_IOMMU_PROTECTED
            } else {
                0
            };

        let mut ctrl = Self {
            pci_cap,
            bar0_p0,
            notif_cap,
            msi_bound,
            dma_caps,
            data_paddr: paddrs[4],
            tsc_mhz,
            timeout_ms,
            admin_q: NvmeQueuePair {
                sq_va: NVME_DMA_ASQ_VA,
                cq_va: NVME_DMA_ACQ_VA,
                sq_db_va: sq0_db_va,
                cq_db_va: cq0_db_va,
                depth: ADMIN_Q_DEPTH,
                sq_tail: 0,
                cq_head: 0,
                cq_phase: 1,
                next_cid: 1,
            },
            io_q: NvmeQueuePair {
                sq_va: NVME_DMA_IOSQ_VA,
                cq_va: NVME_DMA_IOCQ_VA,
                sq_db_va: sq1_db_va,
                cq_db_va: cq1_db_va,
                depth: IO_Q_DEPTH,
                sq_tail: 0,
                cq_head: 0,
                cq_phase: 1,
                next_cid: 1,
            },
            info,
        };

        ctrl.probe_controller_and_ns1(paddrs[2], paddrs[3])?;
        Ok(ctrl)
    }

    fn wait_csts_ready(bar0: u64, want_ready: bool, tsc_mhz: u64, timeout_ms: u64) -> Result<(), isize> {
        let deadline = rdtsc_user().saturating_add(timeout_ms * 1000 * tsc_mhz.max(1000));
        loop {
            let csts = unsafe { mmio_r32(bar0, 0x1c) };
            if (csts & 2) != 0 {
                return Err(EIO);
            }
            let rdy = (csts & 1) != 0;
            if rdy == want_ready {
                return Ok(());
            }
            if rdtsc_user() >= deadline {
                return Err(ETIMEDOUT);
            }
            let _ = sys_yield();
        }
    }

    fn data_slice(&self) -> &'static [u8] {
        // SAFETY: `NVME_DMA_DATA_VA` is a valid pinned 4 KiB DMA page.
        unsafe { core::slice::from_raw_parts(NVME_DMA_DATA_VA as *const u8, 4096) }
    }

    fn clear_data_page(&self) {
        // SAFETY: `NVME_DMA_DATA_VA` is a valid pinned 4 KiB `USER_RW` page.
        unsafe { core::ptr::write_bytes(NVME_DMA_DATA_VA as *mut u8, 0, 4096) };
    }

    fn probe_controller_and_ns1(&mut self, iosq_pa: u64, iocq_pa: u64) -> Result<(), isize> {
        // 1. Admin Identify Controller (`CNS = 1`).
        self.clear_data_page();
        let id_ctrl = NvmeSqe {
            cdw0: NVME_ADMIN_IDENTIFY as u32,
            nsid: 0,
            rsvd: 0,
            mptr: 0,
            prp1: self.data_paddr,
            prp2: 0,
            cdw10: 1,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let _ = self.admin_q.submit_and_wait(
            id_ctrl,
            self.tsc_mhz,
            self.timeout_ms,
            &mut self.info.cqes_read,
        )?;
        {
            let d = self.data_slice();
            self.info.serial.copy_from_slice(&d[4..24]);
            self.info.model.copy_from_slice(&d[24..64]);
            self.info.fw_rev.copy_from_slice(&d[64..72]);
            self.info.nn = read_u32_le(d, 516);
        }

        // 2. Admin Identify Namespace 1 (`NSID = 1, CNS = 0`).
        self.clear_data_page();
        let id_ns = NvmeSqe {
            cdw0: NVME_ADMIN_IDENTIFY as u32,
            nsid: 1,
            rsvd: 0,
            mptr: 0,
            prp1: self.data_paddr,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        if self
            .admin_q
            .submit_and_wait(id_ns, self.tsc_mhz, self.timeout_ms, &mut self.info.cqes_read)
            .is_err()
        {
            return Ok(());
        }
        {
            let d = self.data_slice();
            self.info.nsze_sectors = read_u64_le(d, 0);
            let flbas = (d[26] & 0x0f) as usize;
            let lbaf = read_u32_le(d, 128 + flbas * 4);
            let lbads = ((lbaf >> 16) & 0xff).clamp(9, 12);
            self.info.lba_bytes = 1u32 << lbads;
        }
        if self.info.nsze_sectors == 0 {
            return Ok(());
        }

        // 3. Create IO CQ #1 (PC=1, IEN=0 -> polling via phase tag) + IO SQ #1 (`CQID=1, PC=1`).
        let qsize_m1 = (IO_Q_DEPTH as u32) - 1;
        let create_cq = NvmeSqe {
            cdw0: NVME_ADMIN_CREATE_IO_CQ as u32,
            nsid: 0,
            rsvd: 0,
            mptr: 0,
            prp1: iocq_pa,
            prp2: 0,
            cdw10: (qsize_m1 << 16) | 1,
            cdw11: 1,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let _ = self.admin_q.submit_and_wait(
            create_cq,
            self.tsc_mhz,
            self.timeout_ms,
            &mut self.info.cqes_read,
        )?;

        let create_sq = NvmeSqe {
            cdw0: NVME_ADMIN_CREATE_IO_SQ as u32,
            nsid: 0,
            rsvd: 0,
            mptr: 0,
            prp1: iosq_pa,
            prp2: 0,
            cdw10: (qsize_m1 << 16) | 1,
            cdw11: (1 << 16) | 1,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let _ = self.admin_q.submit_and_wait(
            create_sq,
            self.tsc_mhz,
            self.timeout_ms,
            &mut self.info.cqes_read,
        )?;

        // 4. Perform strictly read-only NVM READ(0x02) on LBA 0..8 (up to 4 KiB) for GPT/MBR inspection.
        let blocks = (4096 / self.info.lba_bytes).clamp(1, 8).min(self.info.nsze_sectors as u32);
        if self.read_lbas(0, blocks as u16).is_ok() {
            self.info.state_flags |= NVME_FLAG_IO_READ_OK;
            let hdr_off = if self.info.lba_bytes == 512 && blocks >= 2 {
                512usize
            } else {
                0usize
            };
            if self.info.lba_bytes >= 4096 && self.info.nsze_sectors >= 2 {
                let _ = self.read_lbas(1, 1);
            }
            let d = self.data_slice();
            if d.len() >= hdr_off + 92 && &d[hdr_off..hdr_off + 8] == b"EFI PART" {
                self.info.state_flags |= NVME_FLAG_GPT_OK;
                self.info.gpt_first_usable = read_u64_le(d, hdr_off + 40);
                self.info.gpt_last_usable = read_u64_le(d, hdr_off + 48);
                if self.info.lba_bytes == 512 && blocks >= 3 {
                    self.parse_gpt_entries_512(1024, (blocks as usize - 2) * 4);
                } else if self.info.lba_bytes >= 4096 && self.read_lbas(2, 1).is_ok() {
                    self.parse_gpt_entries_512(0, 8);
                }
            } else if hdr_off == 512 && d[510] == 0x55 && d[511] == 0xaa {
                // Fallback plain MBR partition table inspection (entry 0 & entry 1).
                let p1s = read_u32_le(d, 446 + 8) as u64;
                let p1n = read_u32_le(d, 446 + 12) as u64;
                let p2s = read_u32_le(d, 462 + 8) as u64;
                let p2n = read_u32_le(d, 462 + 12) as u64;
                if d[446 + 4] != 0 && p1n > 0 {
                    self.info.part1_start = p1s;
                    self.info.part1_sectors = p1n;
                }
                if d[462 + 4] != 0 && p2n > 0 {
                    self.info.part2_start = p2s;
                    self.info.part2_sectors = p2n;
                }
            }
        }
        if self.msi_bound {
            let _ = sys_irq_ack(self.pci_cap, IRQ_ACK_UNMASK);
        }
        Ok(())
    }

    fn read_lbas(&mut self, slba: u64, count: u16) -> Result<(), isize> {
        if count == 0 {
            return Err(EBUSY);
        }
        self.clear_data_page();
        let cmd = NvmeSqe {
            cdw0: NVME_NVM_READ as u32,
            nsid: 1,
            rsvd: 0,
            mptr: 0,
            prp1: self.data_paddr,
            prp2: 0,
            cdw10: slba as u32,
            cdw11: (slba >> 32) as u32,
            cdw12: (count - 1) as u32,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let _ = self.io_q.submit_and_wait(
            cmd,
            self.tsc_mhz,
            self.timeout_ms,
            &mut self.info.cqes_read,
        )?;
        Ok(())
    }

    fn parse_gpt_entries_512(&mut self, base_off: usize, max_entries: usize) {
        let d = self.data_slice();
        for idx in 0..max_entries.min(4) {
            let off = base_off + idx * 128;
            if off + 128 > d.len() {
                break;
            }
            let nonzero_type = d[off..off + 16].iter().any(|&b| b != 0);
            if !nonzero_type {
                continue;
            }
            let first = read_u64_le(d, off + 32);
            let last = read_u64_le(d, off + 40);
            if last >= first {
                let len = last - first + 1;
                if idx == 0 {
                    self.info.part1_start = first;
                    self.info.part1_sectors = len;
                } else if idx == 1 {
                    self.info.part2_start = first;
                    self.info.part2_sectors = len;
                }
            }
        }
    }

    pub fn quiesce(&mut self) {
        if self.msi_bound {
            let _ = sys_irq_ack(self.pci_cap, IRQ_ACK_UNBIND);
            self.msi_bound = false;
        }
        let cc = unsafe { mmio_r32(self.bar0_p0, 0x14) };
        if (cc & 1) != 0 {
            unsafe { mmio_w32(self.bar0_p0, 0x14, cc & !1) };
            let _ = Self::wait_csts_ready(self.bar0_p0, false, self.tsc_mhz, 500);
        }
        for cap in self.dma_caps.iter_mut() {
            if *cap != 0 {
                let _ = sys_cap_revoke(*cap, 1);
                *cap = 0;
            }
        }
        if self.notif_cap != 0 {
            let _ = sys_cap_revoke(self.notif_cap, 1);
            self.notif_cap = 0;
        }
    }
}
