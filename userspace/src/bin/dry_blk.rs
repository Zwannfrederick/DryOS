// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-blk` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0006`) VirtIO 1.0 Modern
//! PCI Block Driver & Lock-Free `IoRing` Consumer (`Milestone 2B` — `ADR-002`, `ADR-005`, `R-03`).
//!
//! Architectural & Hardware Isolation Properties:
//! - Maps ONLY the capability-validated VirtIO-Blk MMIO BAR via `SYS_DEV_MAP` (`VmProt::USER_MMIO_UC`).
//! - Allocates 3 pinned 4 KiB `DEVICE_DMA | NO_DELEGATE` pages via `SYS_DMA_ALLOC` (`INV-CAP-08`):
//!   - Page 0 (`0x2210_0000`): Split Virtqueue `virtq_desc[256]` (`16 B * 256 = 4 KiB`).
//!   - Page 1 (`0x2210_1000`): `virtq_avail` (`+0x000`), `virtq_used` (`+0x400`), `VirtioBlkReqHdr`
//!     (`+0xE00`), and `req_status` (`+0xE40`).
//!   - Page 2 (`0x2210_2000`): 4 KiB bounce-DMA sector buffer (`dry-vfs` never holds `DEVICE_DMA`).
//! - Enforces `VIRTIO_BLK_T_FLUSH` write barriers when negotiated (`VIRTIO_BLK_F_FLUSH`).
//! - If `dry-blk` crashes (`#UD`), Ring 0 `quiesce_pci_for_cspace` clears `PCI Command.BME = 0`
//!   *before* unpinning DMA pages (`R-03`), allowing `dry-init` to restart `dry-blk` cleanly.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ptr::{ copy_nonoverlapping, read_volatile, write_bytes, write_volatile };
use core::sync::atomic::{ fence, Ordering };

use dry_userspace::ioring::{
    IoCqe, IoRingSharedPage, IoSqe, EIO_RING, IORING_MAGIC, IORING_OP_FLUSH, IORING_OP_READ_SECTORS,
    IORING_OP_WRITE_SECTORS,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_query, sys_cap_revoke,
    sys_dev_map, sys_dma_alloc, sys_exit, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_notify_signal, sys_notify_wait, sys_sysinfo, sys_vspace_map, sys_write_debug, sys_yield,
    UserCapInfo, UserIpcRecvExt, UserSysInfo, UserVirtioPciInfo, BADGE_BLK_VFS, BADGE_SUPERVISOR,
    BLK_OP_ATTACH_DATA, BLK_OP_DETACH_RING, BLK_OP_QUERY_INFO, BLK_OP_SETUP_RING, EAGAIN, EINVAL,
    EPERM, ETIMEDOUT, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN,
    IPC_OP_PONG, MSG_FLAG_NON_BLOCKING, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const VIRTIO_BAR_VADDR: u64 = 0x2200_0000;
const DMA_DESC_VADDR: u64 = 0x2210_0000;
const DMA_RINGS_VADDR: u64 = 0x2210_1000;
const DMA_DATA_VADDR: u64 = 0x2210_2000;

const CLIENT_RING_VADDR: u64 = 0x2220_0000;
const CLIENT_DATA_VADDR: u64 = 0x2220_1000;

const AVAIL_OFF: u64 = 0x000;
const USED_OFF: u64 = 0x400;
const REQ_HDR_OFF: u64 = 0xE00;
const REQ_STATUS_OFF: u64 = 0xE40;

// VirtIO 1.0 status bits
const VIRTIO_STATUS_ACK: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
const VIRTIO_STATUS_FAILED: u8 = 128;

// VirtIO feature bits
const VIRTIO_BLK_F_FLUSH_BIT: u32 = 1 << 9;
const VIRTIO_F_VERSION_1_HI: u32 = 1 << 0; // bit 32

// Virtqueue descriptor flags
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

// VirtIO-Blk request types (`VirtIO 1.0 §5.2.6`)
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioBlkReqHdr {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

struct VirtioBlkHw {
    common_base: u64,
    notify_q0_addr: u64,
    isr_addr: u64,
    rings_paddr: u64,
    data_paddr: u64,
    queue_size: u16,
    avail_idx: u16,
    last_used_idx: u16,
    flush_supported: bool,
    total_sectors: u64,
    io_ops: u64,
    flush_ops: u64,
}

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
unsafe fn mmio_read8(addr: u64) -> u8 {
    unsafe { read_volatile(addr as *const u8) }
}

#[inline]
unsafe fn mmio_write8(addr: u64, val: u8) {
    unsafe { write_volatile(addr as *mut u8, val) }
}

#[inline]
unsafe fn mmio_read16(addr: u64) -> u16 {
    unsafe { read_volatile(addr as *const u16) }
}

#[inline]
unsafe fn mmio_write16(addr: u64, val: u16) {
    unsafe { write_volatile(addr as *mut u16, val) }
}

#[inline]
unsafe fn mmio_read32(addr: u64) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

#[inline]
unsafe fn mmio_write32(addr: u64, val: u32) {
    unsafe { write_volatile(addr as *mut u32, val) }
}

#[inline]
unsafe fn mmio_write64_split(addr: u64, val: u64) {
    unsafe {
        mmio_write32(addr, val as u32);
        mmio_write32(addr + 4, (val >> 32) as u32);
    }
}

impl VirtioBlkHw {
    fn init(pci_cap: u64) -> Result<Self, i32> {
        let mut info = UserVirtioPciInfo::default();
        sys_dev_map(pci_cap, VIRTIO_BAR_VADDR, &mut info).map_err(|_| 181)?;

        let (_, desc_paddr) = sys_dma_alloc(pci_cap, DMA_DESC_VADDR).map_err(|_| 182)?;
        let (_, rings_paddr) = sys_dma_alloc(pci_cap, DMA_RINGS_VADDR).map_err(|_| 183)?;
        let (_, data_paddr) = sys_dma_alloc(pci_cap, DMA_DATA_VADDR).map_err(|_| 184)?;

        // SAFETY: The 3 DMA pages are freshly mapped `USER_RW` (`4 KiB` each).
        unsafe {
            write_bytes(DMA_DESC_VADDR as *mut u8, 0, 4096);
            write_bytes(DMA_RINGS_VADDR as *mut u8, 0, 4096);
            write_bytes(DMA_DATA_VADDR as *mut u8, 0, 4096);
        }

        let common = VIRTIO_BAR_VADDR + (info.common_off as u64);
        let isr_addr = VIRTIO_BAR_VADDR + (info.isr_off as u64);
        let device_base = VIRTIO_BAR_VADDR + (info.device_off as u64);

        // SAFETY: `SYS_DEV_MAP` validated that `common`, `notify`, `isr`, and `device` offsets fit
        // within the mapped VirtIO-Blk MMIO BAR (`VmProt::USER_MMIO_UC`).
        unsafe {
            // 1. Reset VirtIO 1.0 device (`device_status = 0`).
            mmio_write8(common + 0x14, 0);
            for _ in 0..10_000 {
                if mmio_read8(common + 0x14) == 0 {
                    break;
                }
                core::hint::spin_loop();
            }

            // 2. Set `ACKNOWLEDGE | DRIVER`.
            mmio_write8(common + 0x14, VIRTIO_STATUS_ACK);
            mmio_write8(common + 0x14, VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER);

            // 3. Negotiate `VIRTIO_F_VERSION_1` + `VIRTIO_BLK_F_FLUSH`.
            mmio_write32(common, 0);
            let dev_feat_lo = mmio_read32(common + 0x04);
            mmio_write32(common, 1);
            let dev_feat_hi = mmio_read32(common + 0x04);
            if (dev_feat_hi & VIRTIO_F_VERSION_1_HI) == 0 {
                mmio_write8(common + 0x14, VIRTIO_STATUS_FAILED);
                return Err(185);
            }
            let flush_supported = (dev_feat_lo & VIRTIO_BLK_F_FLUSH_BIT) != 0;
            let drv_feat_lo = if flush_supported { VIRTIO_BLK_F_FLUSH_BIT } else { 0 };
            mmio_write32(common + 0x08, 0);
            mmio_write32(common + 0x0c, drv_feat_lo);
            mmio_write32(common + 0x08, 1);
            mmio_write32(common + 0x0c, VIRTIO_F_VERSION_1_HI);

            let status_feat = VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
            mmio_write8(common + 0x14, status_feat);
            if (mmio_read8(common + 0x14) & VIRTIO_STATUS_FEATURES_OK) == 0 {
                mmio_write8(common + 0x14, VIRTIO_STATUS_FAILED);
                return Err(186);
            }

            // 4. Configure Virtqueue 0 (`requestq`).
            mmio_write16(common + 0x16, 0);
            let max_qsz = mmio_read16(common + 0x18);
            if max_qsz < 4 {
                mmio_write8(common + 0x14, VIRTIO_STATUS_FAILED);
                return Err(187);
            }
            let req_qsz = max_qsz.min(32);
            mmio_write16(common + 0x18, req_qsz);
            let qsz = mmio_read16(common + 0x18);
            if !(4..=256).contains(&qsz) || !qsz.is_power_of_two() {
                mmio_write8(common + 0x14, VIRTIO_STATUS_FAILED);
                return Err(188);
            }
            let q_notify_off = mmio_read16(common + 0x1e) as u64;
            let notify_q0_addr = VIRTIO_BAR_VADDR
                + (info.notify_off as u64)
                + q_notify_off.saturating_mul(info.notify_mult as u64);

            mmio_write64_split(common + 0x20, desc_paddr);
            mmio_write64_split(common + 0x28, rings_paddr + AVAIL_OFF);
            mmio_write64_split(common + 0x30, rings_paddr + USED_OFF);
            // Suppress device-to-CPU interrupts on `avail.flags = VIRTQ_AVAIL_F_NO_INTERRUPT (1)`.
            write_volatile((DMA_RINGS_VADDR + AVAIL_OFF) as *mut u16, 1u16);
            fence(Ordering::SeqCst);
            mmio_write16(common + 0x1c, 1);

            // 5. Mark `DRIVER_OK` and read `capacity` (`512`-byte sectors).
            mmio_write8(common + 0x14, status_feat | VIRTIO_STATUS_DRIVER_OK);
            let cap_lo = mmio_read32(device_base) as u64;
            let cap_hi = mmio_read32(device_base + 4) as u64;
            let total_sectors = cap_lo | (cap_hi << 32);
            if total_sectors == 0 {
                return Err(189);
            }

            Ok(Self {
                common_base: common,
                notify_q0_addr,
                isr_addr,
                rings_paddr,
                data_paddr,
                queue_size: qsz,
                avail_idx: 0,
                last_used_idx: 0,
                flush_supported,
                total_sectors,
                io_ops: 0,
                flush_ops: 0,
            })
        }
    }

    fn reset_device(&self) {
        // SAFETY: Validated `common_cfg` MMIO register.
        unsafe {
            mmio_write8(self.common_base + 0x14, 0);
            let _ = mmio_read8(self.isr_addr);
        }
    }

    fn submit_sync(&mut self, req_type: u32, sector: u64, byte_len: u32) -> Result<(), isize> {
        let hdr = VirtioBlkReqHdr {
            req_type,
            reserved: 0,
            sector,
        };
        // SAFETY: `DMA_DESC_VADDR` and `DMA_RINGS_VADDR` are pinned `USER_RW` DMA pages.
        unsafe {
            write_volatile((DMA_RINGS_VADDR + REQ_HDR_OFF) as *mut VirtioBlkReqHdr, hdr);
            write_volatile((DMA_RINGS_VADDR + REQ_STATUS_OFF) as *mut u8, 0xff);

            let dbase = DMA_DESC_VADDR as *mut VirtqDesc;
            if req_type == VIRTIO_BLK_T_FLUSH {
                // VirtIO 1.0 §5.2.6.1: `FLUSH` uses a 2-descriptor chain (`header` -> `status`) with `sector = 0`.
                write_volatile(
                    dbase.add(0),
                    VirtqDesc {
                        addr: self.rings_paddr + REQ_HDR_OFF,
                        len: 16,
                        flags: VIRTQ_DESC_F_NEXT,
                        next: 1,
                    },
                );
                write_volatile(
                    dbase.add(1),
                    VirtqDesc {
                        addr: self.rings_paddr + REQ_STATUS_OFF,
                        len: 1,
                        flags: VIRTQ_DESC_F_WRITE,
                        next: 0,
                    },
                );
            } else {
                let data_flags = if req_type == VIRTIO_BLK_T_IN {
                    VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT
                } else {
                    VIRTQ_DESC_F_NEXT
                };
                write_volatile(
                    dbase.add(0),
                    VirtqDesc {
                        addr: self.rings_paddr + REQ_HDR_OFF,
                        len: 16,
                        flags: VIRTQ_DESC_F_NEXT,
                        next: 1,
                    },
                );
                write_volatile(
                    dbase.add(1),
                    VirtqDesc {
                        addr: self.data_paddr,
                        len: byte_len,
                        flags: data_flags,
                        next: 2,
                    },
                );
                write_volatile(
                    dbase.add(2),
                    VirtqDesc {
                        addr: self.rings_paddr + REQ_STATUS_OFF,
                        len: 1,
                        flags: VIRTQ_DESC_F_WRITE,
                        next: 0,
                    },
                );
            }

            let ring_slot = (self.avail_idx % self.queue_size) as usize;
            let avail_ring_ptr = (DMA_RINGS_VADDR + AVAIL_OFF + 4) as *mut u16;
            write_volatile(avail_ring_ptr.add(ring_slot), 0u16);
            fence(Ordering::SeqCst);

            self.avail_idx = self.avail_idx.wrapping_add(1);
            write_volatile((DMA_RINGS_VADDR + AVAIL_OFF + 2) as *mut u16, self.avail_idx);
            fence(Ordering::SeqCst);

            // Ring Queue `0` MMIO notification doorbell.
            mmio_write16(self.notify_q0_addr, 0u16);

            let used_idx_ptr = (DMA_RINGS_VADDR + USED_OFF + 2) as *const u16;
            let mut completed = false;
            for _ in 0..4096u32 {
                if read_volatile(used_idx_ptr) != self.last_used_idx {
                    completed = true;
                    break;
                }
                core::hint::spin_loop();
            }
            if !completed {
                let deadline = rdtsc_user().saturating_add(15_000_000_000);
                let mut spins = 0u32;
                while rdtsc_user() < deadline {
                    if read_volatile(used_idx_ptr) != self.last_used_idx {
                        completed = true;
                        break;
                    }
                    spins = spins.wrapping_add(1);
                    if (spins & 0x1ff) == 0 {
                        let _ = sys_yield();
                    } else {
                        core::hint::spin_loop();
                    }
                }
            }
            if !completed {
                return Err(EIO_RING);
            }
            fence(Ordering::Acquire);
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            let _ = mmio_read8(self.isr_addr);

            let status = read_volatile((DMA_RINGS_VADDR + REQ_STATUS_OFF) as *const u8);
            if status != 0 {
                return Err(EIO_RING);
            }
        }
        if req_type == VIRTIO_BLK_T_FLUSH {
            self.flush_ops = self.flush_ops.wrapping_add(1);
        } else {
            self.io_ops = self.io_ops.wrapping_add(1);
        }
        Ok(())
    }

    fn execute_sqe(&mut self, sqe: IoSqe, has_client_data: bool) -> IoCqe {
        match sqe.opcode {
            IORING_OP_READ_SECTORS | IORING_OP_WRITE_SECTORS => {
                let sectors = sqe.sector_count as u64;
                let bytes = (sectors * 512) as usize;
                let off = sqe.buf_offset as usize;
                if !has_client_data
                    || sectors == 0
                    || bytes > 4096
                    || off.saturating_add(bytes) > 4096
                    || sqe.lba.saturating_add(sectors) > self.total_sectors
                {
                    return IoCqe {
                        user_tag: sqe.user_tag,
                        status: EINVAL as i32,
                        bytes_done: 0,
                    };
                }
                if sqe.opcode == IORING_OP_WRITE_SECTORS {
                    // SAFETY: `CLIENT_DATA_VADDR` and `DMA_DATA_VADDR` are mapped 4 KiB pages; `off + bytes <= 4096`.
                    unsafe {
                        copy_nonoverlapping(
                            (CLIENT_DATA_VADDR as *const u8).add(off),
                            DMA_DATA_VADDR as *mut u8,
                            bytes,
                        );
                    }
                    match self.submit_sync(VIRTIO_BLK_T_OUT, sqe.lba, bytes as u32) {
                        Ok(()) => IoCqe {
                            user_tag: sqe.user_tag,
                            status: 0,
                            bytes_done: bytes as u32,
                        },
                        Err(e) => IoCqe {
                            user_tag: sqe.user_tag,
                            status: e as i32,
                            bytes_done: 0,
                        },
                    }
                } else {
                    match self.submit_sync(VIRTIO_BLK_T_IN, sqe.lba, bytes as u32) {
                        Ok(()) => {
                            // SAFETY: `DMA_DATA_VADDR` holds `bytes` read by VirtIO-Blk; copy into `CLIENT_DATA_VADDR + off`.
                            unsafe {
                                copy_nonoverlapping(
                                    DMA_DATA_VADDR as *const u8,
                                    (CLIENT_DATA_VADDR as *mut u8).add(off),
                                    bytes,
                                );
                            }
                            IoCqe {
                                user_tag: sqe.user_tag,
                                status: 0,
                                bytes_done: bytes as u32,
                            }
                        }
                        Err(e) => IoCqe {
                            user_tag: sqe.user_tag,
                            status: e as i32,
                            bytes_done: 0,
                        },
                    }
                }
            }
            IORING_OP_FLUSH => {
                let res = if self.flush_supported {
                    self.submit_sync(VIRTIO_BLK_T_FLUSH, 0, 0)
                } else {
                    Ok(())
                };
                match res {
                    Ok(()) => IoCqe {
                        user_tag: sqe.user_tag,
                        status: 0,
                        bytes_done: 0,
                    },
                    Err(e) => IoCqe {
                        user_tag: sqe.user_tag,
                        status: e as i32,
                        bytes_done: 0,
                    },
                }
            }
            _ => IoCqe {
                user_tag: sqe.user_tag,
                status: EINVAL as i32,
                bytes_done: 0,
            },
        }
    }
}

struct ClientBinding {
    ring_cap: u64,
    data_cap: u64,
    sq_notif: u64,
    cq_notif: u64,
}

impl ClientBinding {
    const fn empty() -> Self {
        Self {
            ring_cap: 0,
            data_cap: 0,
            sq_notif: 0,
            cq_notif: 0,
        }
    }

    fn reset(&mut self) {
        if self.ring_cap != 0 {
            let _ = sys_vspace_map(self.ring_cap, CLIENT_RING_VADDR, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(self.ring_cap, 1);
            self.ring_cap = 0;
        }
        if self.data_cap != 0 {
            let _ = sys_vspace_map(self.data_cap, CLIENT_DATA_VADDR, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(self.data_cap, 1);
            self.data_cap = 0;
        }
        if self.sq_notif != 0 {
            let _ = sys_cap_revoke(self.sq_notif, 1);
            self.sq_notif = 0;
        }
        if self.cq_notif != 0 {
            let _ = sys_cap_revoke(self.cq_notif, 1);
            self.cq_notif = 0;
        }
    }
}

fn reply_err(ep: u64, tok: u64, lbl: u32, err: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(ep, tok, pack_msg_tag(lbl, 0, 1, 0), [err as u64, 0, 0, 0]);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(srv_ep_cap: u64, pci_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || srv_ep_cap == 0 || pci_cap == 0 {
        sys_exit(180);
    }

    let mut hw = match VirtioBlkHw::init(pci_cap) {
        Ok(h) => h,
        Err(code) => {
            let mut elb = LineBuilder::new();
            elb.push_bytes(b"[DRY-BLK-ERR] init failed code=");
            elb.push_i64(code as i64);
            log_line(&elb);
            sys_exit(code);
        }
    };

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let poll_tsc = 2_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-BLK] VirtIO 1.0 Modern ready sectors=");
    lb.push_u64(hw.total_sectors);
    lb.push_bytes(b" qsz=");
    lb.push_u64(hw.queue_size as u64);
    lb.push_bytes(b" flush=");
    lb.push_bytes(if hw.flush_supported { b"true" } else { b"false" });
    log_line(&lb);

    let mut cli = ClientBinding::empty();

    loop {
        // 1. Drain lock-free `IoRing` submissions when bound (verifying `ring_cap` wasn't asynchronously revoked).
        if cli.ring_cap != 0 {
            let mut cinfo = UserCapInfo::default();
            if sys_cap_query(cli.ring_cap, &mut cinfo) < 0 {
                cli.reset();
            } else {
                let data_live = cli.data_cap != 0 && sys_cap_query(cli.data_cap, &mut cinfo) >= 0;
                if !data_live && cli.data_cap != 0 {
                    cli.reset();
                } else {
                    // SAFETY: `CLIENT_RING_VADDR` is mapped `USER_RW` while `cli.ring_cap` is live.
                    let ring = unsafe { &*(CLIENT_RING_VADDR as *const IoRingSharedPage) };
                    if ring.magic == IORING_MAGIC {
                        let mut completed_any = false;
                        while let Some(sqe) = ring.pop_sqe() {
                            let cqe = hw.execute_sqe(sqe, data_live);
                            let _ = ring.push_cqe(cqe);
                            completed_any = true;
                        }
                        if completed_any && cli.cq_notif != 0 {
                            let _ = sys_notify_signal(cli.cq_notif, 1);
                        }
                    }
                }
            }
        }

        // 2. Receive control IPC (`blocking` when no `IoRing` doorbell is bound, `non-blocking` otherwise).
        let recv_flags = if cli.sq_notif != 0 {
            MSG_FLAG_NON_BLOCKING as u64
        } else {
            0
        };
        let mut ext = UserIpcRecvExt::default();
        let recv_res = sys_ipc_recv_ext(srv_ep_cap, recv_flags, 0, &mut ext);
        let (tag, tok, mrs) = match recv_res {
            Ok(v) => v,
            Err(_) => {
                if cli.sq_notif != 0 {
                    if let Err(we) = sys_notify_wait(cli.sq_notif, 0, poll_tsc) {
                        if we != ETIMEDOUT && we != EAGAIN {
                            cli.reset();
                        }
                    }
                    continue;
                }
                break;
            }
        };

        let op = msg_tag_label(tag);
        let authorized = ext.sender_badge == BADGE_SUPERVISOR || ext.sender_badge == BADGE_BLK_VFS;

        match op {
            BLK_OP_QUERY_INFO => {
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [
                            hw.total_sectors,
                            u64::from(hw.flush_supported),
                            hw.io_ops,
                            hw.flush_ops,
                        ],
                    );
                }
            }
            BLK_OP_SETUP_RING => {
                if !authorized {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if ext.transferred_cap == 0 {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                cli.reset();
                if sys_vspace_map(ext.transferred_cap, CLIENT_RING_VADDR, VSPACE_MAP_OP_4K_RW)
                    .is_err()
                {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                cli.ring_cap = ext.transferred_cap;
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [hw.total_sectors, u64::from(hw.flush_supported), 0, 0],
                    );
                }
            }
            BLK_OP_ATTACH_DATA => {
                if !authorized || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                match mrs[0] {
                    0 => {
                        if cli.data_cap != 0 {
                            let _ = sys_vspace_map(
                                cli.data_cap,
                                CLIENT_DATA_VADDR,
                                VSPACE_MAP_OP_UNMAP_4K,
                            );
                            let _ = sys_cap_revoke(cli.data_cap, 1);
                        }
                        if sys_vspace_map(
                            ext.transferred_cap,
                            CLIENT_DATA_VADDR,
                            VSPACE_MAP_OP_4K_RW,
                        )
                        .is_err()
                        {
                            let _ = sys_cap_revoke(ext.transferred_cap, 1);
                            reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                            continue;
                        }
                        cli.data_cap = ext.transferred_cap;
                    }
                    1 => {
                        if cli.sq_notif != 0 {
                            let _ = sys_cap_revoke(cli.sq_notif, 1);
                        }
                        cli.sq_notif = ext.transferred_cap;
                    }
                    2 => {
                        if cli.cq_notif != 0 {
                            let _ = sys_cap_revoke(cli.cq_notif, 1);
                        }
                        cli.cq_notif = ext.transferred_cap;
                    }
                    _ => {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                        continue;
                    }
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
            }
            BLK_OP_DETACH_RING => {
                if !authorized {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                cli.reset();
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
            }
            IPC_OP_CRASH_UD2 => {
                if ext.sender_badge == BADGE_SUPERVISOR {
                    let _ = sys_write_debug(
                        b"[DRY-BLK] Injecting CPL3 #UD fault to test R-03 PCI BME=0 quiesce\n",
                    );
                    // SAFETY: Deliberate supervisor-authorized `#UD` test (`R-03`).
                    unsafe {
                        asm!("ud2", options(nomem, nostack));
                    }
                } else {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                hw.reset_device();
                cli.reset();
                let mut slb = LineBuilder::new();
                slb.push_bytes(b"[DRY-BLK-STATS] io_ops=");
                slb.push_u64(hw.io_ops);
                slb.push_bytes(b" flush_ops=");
                slb.push_u64(hw.flush_ops);
                log_line(&slb);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [hw.io_ops, hw.flush_ops, 0, 0],
                    );
                }
                // `sys_exit(0)` triggers `reclaim_process -> quiesce_pci_for_cspace` (`BME=0` + DMA unpin + free).
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
