// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Guarded `CapPtr`, 64-Byte Cacheline-Aligned `CapSlot`, Intrusive CDT Links, and Reverse Mapping
//! Records (`ADR-002`, `ADR-003` Phase 1–2 Radix-Ready Contract & `ADR-007`).

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::percpu::read_gs_u64_at_8;
use crate::security::{CapRights, SecLabel};
use crate::vspace::tlb::poll_and_ack_pending_shootdown;
use crate::vspace::VSpaceId;

pub type CSpaceId = u16;
pub const KERNEL_RESERVED_CSPACE_ID: CSpaceId = 0;
pub const INVALID_CSPACE_ID: CSpaceId = u16::MAX;
pub const MAX_CSPACES: usize = 32;
pub const CNODE_RADIX_BITS: u8 = 8;
pub const CNODE_SLOTS: usize = 1 << (CNODE_RADIX_BITS as usize); // 256 slots / CNode
pub const CDT_NONE: u32 = u32::MAX;
pub const MAP_REC_NONE: u16 = u16::MAX;
pub const MAX_CAP_MAPPINGS: usize = 512;

/// `ADR-003` Radix-Upgradable 64-bit Capability Handle:
/// `[63:56] guard: u8 | [55:48] radix_bits: u8 | [47:16] slot_gen: u32 | [15:0] slot_idx: u16`.
/// Slot `0` is permanently `CAP_NULL`; Phase 3 multi-level radix `CSpace` resolves nested `CNode`
/// levels using the identical 64-bit `CapPtr` representation without breaking callers.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapPtr(pub u64);

impl CapPtr {
    pub const NULL: Self = Self(0);

    #[inline(always)]
    pub const fn pack(guard: u8, radix_bits: u8, slot_gen: u32, slot_idx: u16) -> Self {
        Self(
            ((guard as u64) << 56)
                | ((radix_bits as u64) << 48)
                | ((slot_gen as u64) << 16)
                | (slot_idx as u64),
        )
    }

    #[inline(always)]
    pub const fn guard(self) -> u8 {
        ((self.0 >> 56) & 0xff) as u8
    }

    #[inline(always)]
    pub const fn radix_bits(self) -> u8 {
        ((self.0 >> 48) & 0xff) as u8
    }

    #[inline(always)]
    pub const fn slot_gen(self) -> u32 {
        ((self.0 >> 16) & 0xffff_ffff) as u32
    }

    #[inline(always)]
    pub const fn slot_idx(self) -> u16 {
        (self.0 & 0xffff) as u16
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapObjectKind {
    None = 0,
    Untyped = 1,
    Frame4K = 2,
    Frame2M = 3,
    VSpace = 4,
    Tcb = 5,
    CNode = 6,
    Notification = 7,
    Endpoint = 8,
    BootModule = 9,
    Framebuffer = 10,
    IrqLine = 11,
    IoPort = 12,
}

impl CapObjectKind {
    #[inline(always)]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Untyped,
            2 => Self::Frame4K,
            3 => Self::Frame2M,
            4 => Self::VSpace,
            5 => Self::Tcb,
            6 => Self::CNode,
            7 => Self::Notification,
            8 => Self::Endpoint,
            9 => Self::BootModule,
            10 => Self::Framebuffer,
            11 => Self::IrqLine,
            12 => Self::IoPort,
            _ => Self::None,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Empty = 0,
    Building = 1,
    Live = 2,
    Revoking = 3,
    EmptyRevoked = 4,
}

impl SlotState {
    #[inline(always)]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Building,
            2 => Self::Live,
            3 => Self::Revoking,
            4 => Self::EmptyRevoked,
            _ => Self::Empty,
        }
    }
}

#[repr(i64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    InsufficientRights = -1,
    OutOfSlots = -2,
    BusyActiveObject = -3,
    StaleGenerationAba = -6,
    WrongObjectKind = -7,
    InvalidHandle = -10,
    Revoked = -11,
    LeaseExpired = -12,
    RightsAmplification = -13,
    DomainViolation = -14,
    TransferEscalation = -15,
    PagingError = -16,
}

/// Unforgeable kernel capability-transfer receipt ready for `ADR-002` IPC `Grant` delivery (`INV-SEC-06`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpcCapTransferReceipt {
    pub dst_cspace: CSpaceId,
    pub dst_cap: CapPtr,
    pub badge: u64,
    pub sender_label: SecLabel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedCap {
    pub global_slot: u32,
    pub obj_kind: CapObjectKind,
    pub obj_id: u32,
    pub expected_obj_gen: u32,
    pub rights: CapRights,
    pub obj_label: SecLabel,
    pub lease_epoch: u32,
    pub badge: u64,
}

#[inline(always)]
pub const fn next_slot_gen(cur: u32) -> u32 {
    if cur >= u32::MAX - 1 {
        2
    } else {
        cur + 1
    }
}

#[inline(always)]
pub const fn pack_state_gen(
    state: SlotState,
    kind: CapObjectKind,
    rights: CapRights,
    slot_gen: u32,
) -> u64 {
    (state as u64)
        | ((kind as u64) << 8)
        | ((rights.0 as u64) << 16)
        | ((slot_gen as u64) << 32)
}

#[inline(always)]
pub const fn unpack_state_gen(raw: u64) -> (SlotState, CapObjectKind, CapRights, u32) {
    (
        SlotState::from_u8((raw & 0xff) as u8),
        CapObjectKind::from_u8(((raw >> 8) & 0xff) as u8),
        CapRights(((raw >> 16) & 0xffff) as u16),
        (raw >> 32) as u32,
    )
}

#[inline(always)]
pub const fn pack_obj_meta(obj_id: u32, expected_obj_gen: u32) -> u64 {
    (obj_id as u64) | ((expected_obj_gen as u64) << 32)
}

#[inline(always)]
pub const fn unpack_obj_meta(raw: u64) -> (u32, u32) {
    (raw as u32, (raw >> 32) as u32)
}

/// 64-byte (`1` L1 cache line) capability slot supporting lock-free $O(1)$ fast-path `lookup`
/// and intrusive $O(k)$ parent-child CDT revocation across `CSpace`s.
#[repr(C, align(64))]
pub struct CapSlot {
    pub state_gen: AtomicU64,
    pub obj_meta: AtomicU64,
    pub sec_label_raw: AtomicU64,
    pub badge: AtomicU64,
    pub lease_epoch: AtomicU32,
    pub in_flight_ops: AtomicU32,
    pub cdt_parent: AtomicU32,
    pub cdt_first_child: AtomicU32,
    pub cdt_prev_sib: AtomicU32,
    pub cdt_next_sib: AtomicU32,
    pub first_map_idx: AtomicU16,
    pub origin_cspace: AtomicU16,
    pub reserved_pad: AtomicU32,
}

const _: () = assert!(size_of::<CapSlot>() == 64);

impl CapSlot {
    pub const fn new_empty() -> Self {
        Self {
            state_gen: AtomicU64::new(pack_state_gen(
                SlotState::Empty,
                CapObjectKind::None,
                CapRights::NONE,
                1,
            )),
            obj_meta: AtomicU64::new(0),
            sec_label_raw: AtomicU64::new(0),
            badge: AtomicU64::new(0),
            lease_epoch: AtomicU32::new(0),
            in_flight_ops: AtomicU32::new(0),
            cdt_parent: AtomicU32::new(CDT_NONE),
            cdt_first_child: AtomicU32::new(CDT_NONE),
            cdt_prev_sib: AtomicU32::new(CDT_NONE),
            cdt_next_sib: AtomicU32::new(CDT_NONE),
            first_map_idx: AtomicU16::new(MAP_REC_NONE),
            origin_cspace: AtomicU16::new(INVALID_CSPACE_ID),
            reserved_pad: AtomicU32::new(0),
        }
    }
}

#[derive(Clone, Copy)]
pub struct CapMapRecord {
    pub active: bool,
    pub is_huge_2m: bool,
    pub vspace_id: VSpaceId,
    pub vspace_gen: u32,
    pub vaddr: u64,
    pub next_idx: u16,
    pub num_pages: u16,
}

impl CapMapRecord {
    const fn empty() -> Self {
        Self {
            active: false,
            is_huge_2m: false,
            vspace_id: 0,
            vspace_gen: 0,
            vaddr: 0,
            next_idx: MAP_REC_NONE,
            num_pages: 0,
        }
    }
}

struct CdtAndMapPool {
    locked: AtomicBool,
    maps: UnsafeCell<[CapMapRecord; MAX_CAP_MAPPINGS]>,
}

// SAFETY: `maps` and intrusive CDT links are mutated strictly under `CDT_POOL.locked` with `CLI`.
unsafe impl Sync for CdtAndMapPool {}

static CDT_POOL: CdtAndMapPool = CdtAndMapPool {
    locked: AtomicBool::new(false),
    maps: UnsafeCell::new([const { CapMapRecord::empty() }; MAX_CAP_MAPPINGS]),
};

/// Execute `f` under the leaf `CDT_POOL` lock (`CLI` + cooperative shootdown poll).
/// Lock order contract: NEVER call `VSpace`, `PMM`, or `Untyped` functions inside `with_cdt_lock`!
#[inline]
pub(crate) fn with_cdt_lock<R>(f: impl FnOnce(&mut [CapMapRecord; MAX_CAP_MAPPINGS]) -> R) -> R {
    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;
    while CDT_POOL
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        poll_and_ack_pending_shootdown(cpu_idx);
        core::hint::spin_loop();
    }
    // SAFETY: Exclusive lock + `CLI` held on `cpu_idx`.
    let out = f(unsafe { &mut *CDT_POOL.maps.get() });
    CDT_POOL.locked.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}
