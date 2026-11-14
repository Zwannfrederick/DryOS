// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Lock-Free Shared-Memory `IoRing` (`SQ`/`CQ` + `Notification` Doorbells) (`Milestone 2B` —
//! `ADR-002`, `ADR-005`).
//!
//! Connects `dry-vfs` (`0x0004`, SQ producer / CQ consumer) and `dry-blk` (`0x0006`, SQ consumer /
//! CQ producer) over two capability-delegated 4 KiB frames (`IoRingSharedPage` + 4 KiB data buffer)
//! and two `Notification` objects (`sq_notif`, `cq_notif`) with `Acquire`/`Release` ring indices.

use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_revoke, sys_frame_alloc, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_notify_create, sys_notify_signal, sys_notify_wait, sys_vspace_map,
    BLK_OP_ATTACH_DATA, BLK_OP_DETACH_RING, BLK_OP_QUERY_INFO, BLK_OP_SETUP_RING, EBADCAP,
    EINVAL, EPIPE, EREVOKED, ETIMEDOUT, IPC_OP_PONG, RIGHT_MAP, RIGHT_READ, RIGHT_SIGNAL,
    RIGHT_WAIT, RIGHT_WRITE, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

pub const EIO_RING: isize = -5;
pub const IORING_MAGIC: u64 = 0x4452_595f_494f_5247; // "DRY_IORG"
pub const IORING_RING_CAP: usize = 32;
pub const IORING_RING_MASK: u32 = (IORING_RING_CAP as u32) - 1;

pub const IORING_OP_READ_SECTORS: u8 = 1;
pub const IORING_OP_WRITE_SECTORS: u8 = 2;
pub const IORING_OP_FLUSH: u8 = 3;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoSqe {
    pub opcode: u8,
    pub flags: u8,
    pub sector_count: u16,
    pub buf_offset: u32,
    pub lba: u64,
    pub user_tag: u64,
    pub _reserved: u64,
}

const _: () = assert!(size_of::<IoSqe>() == 32);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoCqe {
    pub user_tag: u64,
    pub status: i32,
    pub bytes_done: u32,
}

const _: () = assert!(size_of::<IoCqe>() == 16);

/// Shared 4 KiB `IoRing` control page (`1,600` bytes used <= `4,096` bytes).
#[repr(C)]
pub struct IoRingSharedPage {
    pub magic: u64,
    pub sq_head: AtomicU32,
    pub sq_tail: AtomicU32,
    pub cq_head: AtomicU32,
    pub cq_tail: AtomicU32,
    pub ring_mask: u32,
    pub _pad: [u32; 9],
    pub sqes: [IoSqe; IORING_RING_CAP],
    pub cqes: [IoCqe; IORING_RING_CAP],
}

const _: () = assert!(size_of::<IoRingSharedPage>() <= 4096);

impl IoRingSharedPage {
    /// Initialize a freshly zeroed 4 KiB frame mapped `USER_RW` at `vaddr`.
    ///
    /// # Safety
    /// `vaddr` must point to a valid 4 KiB `USER_RW` page.
    pub unsafe fn init_at(vaddr: u64) -> &'static Self {
        // SAFETY: Caller guarantees `vaddr` is a mapped 4 KiB `USER_RW` page.
        unsafe {
            core::ptr::write_bytes(vaddr as *mut u8, 0, 4096);
            let p = vaddr as *mut Self;
            (*p).magic = IORING_MAGIC;
            (*p).ring_mask = IORING_RING_MASK;
            &*p
        }
    }

    /// Producer (`dry-vfs`): enqueue an `IoSqe` into `sqes` with `Release` ordering on `sq_tail`.
    pub fn push_sqe(&self, sqe: IoSqe) -> bool {
        let tail = self.sq_tail.load(Ordering::Relaxed);
        let head = self.sq_head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= (IORING_RING_CAP as u32) {
            return false;
        }
        let idx = (tail & IORING_RING_MASK) as usize;
        // SAFETY: SPSC ownership of `sqes[idx]` belongs to producer while `tail - head < CAP`.
        unsafe {
            let slot = (&raw const self.sqes[idx]).cast_mut();
            core::ptr::write_volatile(slot, sqe);
        }
        self.sq_tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer (`dry-blk`): dequeue the next `IoSqe` with `Acquire` ordering on `sq_tail`.
    pub fn pop_sqe(&self) -> Option<IoSqe> {
        let head = self.sq_head.load(Ordering::Relaxed);
        let tail = self.sq_tail.load(Ordering::Acquire);
        // `AUD-U5`: a hostile producer may publish `tail - head > CAP`; treat it as empty rather
        // than letting the consumer spin over up to `2^32` bogus entries.
        if head == tail || tail.wrapping_sub(head) > (IORING_RING_CAP as u32) {
            return None;
        }
        let idx = (head & IORING_RING_MASK) as usize;
        // SAFETY: Producer published `sqes[idx]` before `Release`-storing `sq_tail`.
        let sqe = unsafe { core::ptr::read_volatile(&raw const self.sqes[idx]) };
        self.sq_head.store(head.wrapping_add(1), Ordering::Release);
        Some(sqe)
    }

    /// Producer (`dry-blk`): publish a completion `IoCqe` with `Release` ordering on `cq_tail`.
    pub fn push_cqe(&self, cqe: IoCqe) -> bool {
        let tail = self.cq_tail.load(Ordering::Relaxed);
        let head = self.cq_head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= (IORING_RING_CAP as u32) {
            return false;
        }
        let idx = (tail & IORING_RING_MASK) as usize;
        // SAFETY: SPSC ownership of `cqes[idx]` belongs to `dry-blk` while `tail - head < CAP`.
        unsafe {
            let slot = (&raw const self.cqes[idx]).cast_mut();
            core::ptr::write_volatile(slot, cqe);
        }
        self.cq_tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer (`dry-vfs`): pop the next `IoCqe` with `Acquire` ordering on `cq_tail`.
    pub fn pop_cqe(&self) -> Option<IoCqe> {
        let head = self.cq_head.load(Ordering::Relaxed);
        let tail = self.cq_tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let idx = (head & IORING_RING_MASK) as usize;
        // SAFETY: `dry-blk` published `cqes[idx]` before `Release`-storing `cq_tail`.
        let cqe = unsafe { core::ptr::read_volatile(&raw const self.cqes[idx]) };
        self.cq_head.store(head.wrapping_add(1), Ordering::Release);
        Some(cqe)
    }
}

/// Client-side `IoRing` handle owned by `dry-vfs`.
pub struct IoRingClient {
    pub blk_ep: u64,
    pub ring_cap: u64,
    pub data_cap: u64,
    pub sq_notif: u64,
    pub cq_notif: u64,
    pub ring_vaddr: u64,
    pub data_vaddr: u64,
    pub total_sectors: u64,
    pub next_tag: u64,
}

impl IoRingClient {
    pub const fn empty() -> Self {
        Self {
            blk_ep: 0,
            ring_cap: 0,
            data_cap: 0,
            sq_notif: 0,
            cq_notif: 0,
            ring_vaddr: 0,
            data_vaddr: 0,
            total_sectors: 0,
            next_tag: 1,
        }
    }

    #[inline]
    pub const fn is_active(&self) -> bool {
        self.blk_ep != 0 && self.total_sectors >= 128
    }

    pub fn connect(blk_ep: u64, ring_vaddr: u64, data_vaddr: u64) -> Result<Self, isize> {
        let ring_cap = sys_frame_alloc(0)?;
        let data_cap = match sys_frame_alloc(0) {
            Ok(c) => c,
            Err(e) => {
                let _ = sys_cap_revoke(ring_cap, 2);
                return Err(e);
            }
        };
        let sq_notif = sys_notify_create(0)?;
        let cq_notif = sys_notify_create(0)?;

        sys_vspace_map(ring_cap, ring_vaddr, VSPACE_MAP_OP_4K_RW)?;
        sys_vspace_map(data_cap, data_vaddr, VSPACE_MAP_OP_4K_RW)?;
        // SAFETY: Both `ring_vaddr` and `data_vaddr` are mapped `USER_RW` 4 KiB frames.
        unsafe {
            let _ = IoRingSharedPage::init_at(ring_vaddr);
            core::ptr::write_bytes(data_vaddr as *mut u8, 0, 4096);
        }

        let rw_map = RIGHT_READ | RIGHT_WRITE | RIGHT_MAP;
        let (rtag, _, rmrs) = sys_ipc_call_with_grant(
            blk_ep,
            BLK_OP_SETUP_RING,
            [0; 4],
            ring_cap,
            rw_map,
            0,
            0,
        )?;
        if msg_tag_label(rtag) != IPC_OP_PONG || rmrs[0] == 0 {
            return Err(EIO_RING);
        }
        let total_sectors = rmrs[0];

        let (d0, _, _) = sys_ipc_call_with_grant(
            blk_ep,
            BLK_OP_ATTACH_DATA,
            [0, 0, 0, 0],
            data_cap,
            rw_map,
            0,
            0,
        )?;
        let (d1, _, _) = sys_ipc_call_with_grant(
            blk_ep,
            BLK_OP_ATTACH_DATA,
            [1, 0, 0, 0],
            sq_notif,
            RIGHT_READ | RIGHT_WAIT,
            0,
            0,
        )?;
        let (d2, _, _) = sys_ipc_call_with_grant(
            blk_ep,
            BLK_OP_ATTACH_DATA,
            [2, 0, 0, 0],
            cq_notif,
            RIGHT_READ | RIGHT_SIGNAL,
            0,
            0,
        )?;
        if msg_tag_label(d0) != IPC_OP_PONG
            || msg_tag_label(d1) != IPC_OP_PONG
            || msg_tag_label(d2) != IPC_OP_PONG
        {
            return Err(EIO_RING);
        }

        Ok(Self {
            blk_ep,
            ring_cap,
            data_cap,
            sq_notif,
            cq_notif,
            ring_vaddr,
            data_vaddr,
            total_sectors,
            next_tag: 1,
        })
    }

    fn exec_sqe(&mut self, mut sqe: IoSqe) -> Result<IoCqe, isize> {
        if !self.is_active() {
            return Err(EINVAL);
        }
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1).max(1);
        sqe.user_tag = tag;

        // SAFETY: `self.ring_vaddr` remains mapped `USER_RW` for the lifetime of `IoRingClient`.
        let ring = unsafe { &*(self.ring_vaddr as *const IoRingSharedPage) };
        if !ring.push_sqe(sqe) {
            return Err(EIO_RING);
        }
        let _ = sys_notify_signal(self.sq_notif, 1)?;
        for _slice in 0..20u32 {
            if let Some(cqe) = ring.pop_cqe() {
                if cqe.user_tag != tag {
                    return Err(EIO_RING);
                }
                if cqe.status < 0 {
                    return Err(cqe.status as isize);
                }
                return Ok(cqe);
            }
            match sys_notify_wait(self.cq_notif, 0, 250_000_000) {
                Ok(_) => {}
                Err(ETIMEDOUT) => {
                    if let Err(pe) = sys_ipc_call_inline(
                        self.blk_ep,
                        pack_msg_tag(BLK_OP_QUERY_INFO, 0, 0, 0),
                        [0; 4],
                        250_000_000,
                    ) {
                        if matches!(pe, EPIPE | EREVOKED | EBADCAP) {
                            return Err(EIO_RING);
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(ETIMEDOUT)
    }

    pub fn read_block_4k(&mut self, block_4k: u32, out: &mut [u8; 4096]) -> Result<(), isize> {
        let lba = (block_4k as u64) * 8;
        if lba + 8 > self.total_sectors {
            return Err(EINVAL);
        }
        let _ = self.exec_sqe(IoSqe {
            opcode: IORING_OP_READ_SECTORS,
            flags: 0,
            sector_count: 8,
            buf_offset: 0,
            lba,
            user_tag: 0,
            _reserved: 0,
        })?;
        // SAFETY: `data_vaddr` is a mapped 4 KiB page written by `dry-blk` before `cq_tail` release.
        unsafe {
            core::ptr::copy_nonoverlapping(self.data_vaddr as *const u8, out.as_mut_ptr(), 4096);
        }
        Ok(())
    }

    pub fn write_block_4k(&mut self, block_4k: u32, src: &[u8; 4096]) -> Result<(), isize> {
        let lba = (block_4k as u64) * 8;
        if lba + 8 > self.total_sectors {
            return Err(EINVAL);
        }
        // SAFETY: `data_vaddr` is mapped `USER_RW`; written before `sq_tail` release.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.data_vaddr as *mut u8, 4096);
        }
        let _ = self.exec_sqe(IoSqe {
            opcode: IORING_OP_WRITE_SECTORS,
            flags: 0,
            sector_count: 8,
            buf_offset: 0,
            lba,
            user_tag: 0,
            _reserved: 0,
        })?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), isize> {
        let _ = self.exec_sqe(IoSqe {
            opcode: IORING_OP_FLUSH,
            flags: 0,
            sector_count: 0,
            buf_offset: 0,
            lba: 0,
            user_tag: 0,
            _reserved: 0,
        })?;
        Ok(())
    }

    pub fn teardown(&mut self) {
        if self.blk_ep != 0 {
            if self.sq_notif != 0 {
                let _ = sys_notify_signal(self.sq_notif, 1);
            }
            let _ = sys_ipc_call_inline(
                self.blk_ep,
                pack_msg_tag(BLK_OP_DETACH_RING, 0, 1, 0),
                [0; 4],
                0,
            );
        }
        if self.ring_cap != 0 {
            let _ = sys_vspace_map(self.ring_cap, self.ring_vaddr, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(self.ring_cap, 2);
            self.ring_cap = 0;
        }
        if self.data_cap != 0 {
            let _ = sys_vspace_map(self.data_cap, self.data_vaddr, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(self.data_cap, 2);
            self.data_cap = 0;
        }
        if self.sq_notif != 0 {
            let _ = sys_cap_revoke(self.sq_notif, 2);
            self.sq_notif = 0;
        }
        if self.cq_notif != 0 {
            let _ = sys_cap_revoke(self.cq_notif, 2);
            self.cq_notif = 0;
        }
        if self.blk_ep != 0 {
            let _ = sys_cap_revoke(self.blk_ep, 1);
            self.blk_ep = 0;
        }
        self.total_sectors = 0;
    }
}
