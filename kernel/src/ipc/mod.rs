// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Based Synchronous & Asynchronous IPC Subsystem (`Milestone 1C` Stages A & B, `ADR-002`).
//!
//! Provides:
//! - `Endpoint` (`endpoint.rs`): Synchronous `send`, `recv`, `call`, `reply`, and combined
//!   `reply_recv` with 4 inline 64-bit message registers (`mr0..mr3` in `R12..R15` = 32 B zero-copy),
//!   bounded extra user slice (`<= 256 B`), forward-compatible `SharedPayloadDesc`, `CSpace`
//!   capability grant transfer, generation-safe single-shot `ReplyToken`, and 4-hop Priority Donation.
//! - `Notification` (`notif.rs`): Lock-free non-blocking `signal` (`fetch_or` + `SeqCst` fence)
//!   and H1 `wait` with unforgeable capability badge accumulation.

pub mod bench;
pub mod bound_notif;
pub mod endpoint;
pub mod endpoint_state;
pub mod notif;
pub mod tests;

use core::mem::size_of;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::cspace::cap::{ unpack_obj_meta, unpack_state_gen, CSpaceId, CapError, CapObjectKind, CapPtr, SlotState };
use crate::cspace::{CNODE_SLOTS, CSPACE_TABLE};
use crate::gdt_tss::MAX_CPUS;
use crate::percpu::current_percpu;
use crate::security::CapRights;
use crate::uaccess::SysError;

#[repr(C, align(64))]
struct CpuIpcCounter(AtomicU64);

impl CpuIpcCounter {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }
}

static CPU_IPC_COUNTERS: [CpuIpcCounter; MAX_CPUS] = [const { CpuIpcCounter::new() }; MAX_CPUS];

#[inline(always)]
pub fn note_cpu_ipc_msg() {
    let idx = (current_percpu().cpu_idx as usize) & (MAX_CPUS - 1);
    CPU_IPC_COUNTERS[idx].0.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn total_ipc_msgs(online_cpus: usize) -> u64 {
    let mut sum = 0u64;
    for slot in CPU_IPC_COUNTERS.iter().take(online_cpus.min(MAX_CPUS)) {
        sum = sum.wrapping_add(slot.0.load(Ordering::Relaxed));
    }
    sum
}

pub const MAX_INLINE_WORDS: usize = 4;
pub const MAX_IPC_EXTRA_BYTES: usize = 256;

pub const MSG_FLAG_EXTRA_BUF: u8 = 1 << 0;
pub const MSG_FLAG_GRANT_CAP: u8 = 1 << 1;
pub const MSG_FLAG_NON_BLOCKING: u8 = 1 << 2;
pub const MSG_FLAG_SHARED_DESC: u8 = 1 << 3;
const MSG_KNOWN_FLAGS_MASK: u8 =
    MSG_FLAG_EXTRA_BUF | MSG_FLAG_GRANT_CAP | MSG_FLAG_NON_BLOCKING | MSG_FLAG_SHARED_DESC;

pub const NOTIFY_FLAG_BIND_TCB: u64 = 1 << 4;
pub const NOTIFY_FLAG_UNBIND_TCB: u64 = 1 << 5;
pub const IPC_OP_BOUND_NOTIF: u32 = 0x7fff_0001;

/// 64-bit IPC Message Descriptor (`MsgTag`), always keeping bit `63 == 0` so valid tags returned in
/// `RAX` are strictly non-negative (`>= 0`) and never alias `-errno` (`[-4095..-1]`).
///
/// Layout: `[62:32] label (31b) | [31:16] extra_len (16b) | [15:8] flags (8b) | [7:0] words (8b)`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsgTag(pub u64);

impl MsgTag {
    pub const EMPTY: Self = Self(0);

    #[inline(always)]
    pub const fn new(label: u32, flags: u8, words: u8, extra_len: u16) -> Self {
        let clean_label = (label & 0x7fff_ffff) as u64;
        Self(
            (clean_label << 32)
                | ((extra_len as u64) << 16)
                | ((flags as u64) << 8)
                | (words as u64),
        )
    }

    #[inline(always)]
    pub const fn words(self) -> usize {
        (self.0 & 0xff) as usize
    }

    #[inline(always)]
    pub const fn flags(self) -> u8 {
        ((self.0 >> 8) & 0xff) as u8
    }

    #[inline(always)]
    pub const fn extra_len(self) -> usize {
        ((self.0 >> 16) & 0xffff) as usize
    }

    #[inline(always)]
    pub const fn label(self) -> u32 {
        ((self.0 >> 32) & 0x7fff_ffff) as u32
    }

    /// Validate `MsgTag` word count, flags, and bounded inline extra payload length (`Stage F.10`).
    #[inline]
    pub fn validate(self) -> Result<(), SysError> {
        if (self.0 >> 63) != 0 || (self.flags() & !MSG_KNOWN_FLAGS_MASK) != 0 {
            return Err(SysError::Inval);
        }
        if self.words() > MAX_INLINE_WORDS {
            return Err(SysError::MsgSize);
        }
        let extra = self.extra_len();
        if (self.flags() & MSG_FLAG_EXTRA_BUF) != 0 {
            if extra > MAX_IPC_EXTRA_BYTES {
                return Err(SysError::MsgSize);
            }
        } else if extra != 0 {
            return Err(SysError::Inval);
        }
        Ok(())
    }
}

/// Optional userspace-supplied extended send/grant/extra-buffer descriptor (`48` bytes POD).
/// Not touched (`meta_ptr == 0`) on the zero-copy register-only IPC fast path!
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UserIpcSendExt {
    pub extra_buf_ptr: u64,
    pub grant_cap: u64,
    pub grant_rights: u16,
    pub _pad: u16,
    pub grant_lease: u32,
    pub grant_badge: u64,
    pub shared_region_cap: u64,
    pub shared_offset_and_len: u64,
}

const _: () = assert!(size_of::<UserIpcSendExt>() == 48);

/// Optional userspace-supplied extended receive/call completion descriptor (`40` bytes POD).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UserIpcRecvExt {
    pub extra_buf_ptr: u64,
    pub extra_buf_cap: u64,
    pub sender_badge: u64,
    pub reply_token: u64,
    pub transferred_cap: u64,
}

const _: () = assert!(size_of::<UserIpcRecvExt>() == 40);

/// Forward-compatible zero-copy shared-memory payload descriptor (`Milestone 2` bulk ring contract).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedPayloadDesc {
    pub region_cap: CapPtr,
    pub byte_offset: u32,
    pub byte_len: u32,
}

#[inline]
pub fn map_cap_err_to_sys(err: CapError) -> SysError {
    match err {
        CapError::InvalidHandle | CapError::StaleGenerationAba => SysError::BadCap,
        CapError::Revoked | CapError::LeaseExpired => SysError::Revoked,
        CapError::BusyActiveObject => SysError::Busy,
        CapError::OutOfSlots => SysError::NoMem,
        CapError::PagingError => SysError::Inval,
        CapError::InsufficientRights
        | CapError::RightsAmplification
        | CapError::DomainViolation
        | CapError::TransferEscalation
        | CapError::WrongObjectKind => SysError::Perm,
    }
}

/// Enforce `INV-IPC-05` (`Stage A.9`): blocking IPC is strictly forbidden inside IRQ/NMI handlers
/// or preemption-disabled critical sections.
#[inline]
pub fn check_blocking_ipc_allowed() -> Result<(), SysError> {
    let pcpu = current_percpu();
    if pcpu.irq_depth.load(Ordering::Relaxed) != 0
        || pcpu.preempt_count.load(Ordering::Relaxed) != 0
    {
        return Err(SysError::IrqForbidden);
    }
    Ok(())
}

/// Invoked from `CSpace` revocation (`revoke.rs`) after `slot.in_flight_ops` drains to `0` so any
/// thread blocked on `global_slot` wakes deterministically with `SysError::Revoked`.
pub fn on_cap_slot_revoked(
    kind: CapObjectKind,
    obj_id: u32,
    expected_obj_gen: u32,
    global_slot: u32,
) {
    match kind {
        CapObjectKind::Endpoint => {
            endpoint::on_endpoint_cap_revoked(obj_id, expected_obj_gen, global_slot);
        }
        CapObjectKind::Notification => {
            notif::on_notif_cap_revoked(obj_id, expected_obj_gen, global_slot);
            crate::irq::on_notif_cap_revoked_for_irq(obj_id, expected_obj_gen, global_slot);
        }
        CapObjectKind::IrqLine => {
            crate::irq::on_irq_cap_revoked(obj_id, expected_obj_gen, global_slot);
        }
        CapObjectKind::PciDev => {
            crate::pci::on_pci_cap_revoked(obj_id, expected_obj_gen, global_slot);
        }
        _ => {}
    }
}

/// Invoked when a process transitions into `Exiting | Faulting | Killing` (`proc/mod.rs`) so any
/// `Endpoint` for which this process held a `RECV`/`REPLY` server capability immediately wakes
/// blocked clients with `SysError::Pipe` (`Stage D.4` & `Stage F.4`).
pub fn on_process_terminating(pid: u32, cspace_id: CSpaceId) {
    if cspace_id == 0 || (cspace_id as usize) >= crate::cspace::MAX_CSPACES {
        return;
    }
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    if !cnode.allocated.load(Ordering::Acquire) {
        return;
    }
    let srv_mask = CapRights::RECV.0 | CapRights::REPLY.0;
    for idx in 1..CNODE_SLOTS {
        let slot = &cnode.slots[idx];
        let (st, kind, rights, _) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
        if st == SlotState::Live
            && kind == CapObjectKind::Endpoint
            && (rights.0 & srv_mask) != 0
            && !rights.contains(CapRights::MANAGE)
        {
            // Delegated server endpoint (`!MANAGE` distinguishes `dry_echo`'s server cap from
            // `dry_init`'s root supervisor cap!).
            let (ep_id, ep_gen) = unpack_obj_meta(slot.obj_meta.load(Ordering::Acquire));
            endpoint::on_delegated_server_exit(ep_id, ep_gen, pid);
        } else if st == SlotState::Live && kind == CapObjectKind::IrqLine {
            let (irq_id, irq_gen) = unpack_obj_meta(slot.obj_meta.load(Ordering::Acquire));
            let gslot = crate::cspace::global_slot_id(cspace_id, idx as u16);
            crate::irq::on_irq_cap_revoked(irq_id, irq_gen, gslot);
        } else if st == SlotState::Live && kind == CapObjectKind::PciDev {
            crate::pci::quiesce_pci_for_cspace(cspace_id);
        }
    }
}

/// Invoked on every worker/user thread exit (`sched::exit_current_thread`) to cancel pending waits
/// and fail any `ReplyToken`s held by the dying thread (`SysError::Pipe`).
pub fn on_thread_terminating(slot: usize, pid: u32) {
    bound_notif::on_thread_exit_cleanup(slot);
    endpoint::on_thread_exit_cleanup(slot, pid);
    notif::on_thread_exit_cleanup(slot);
}
