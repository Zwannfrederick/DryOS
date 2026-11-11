// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Bounded Priority Donation & Deadlock Cycle Detection (`Milestone 1C` Stage B.3, `INV-IPC-06`).
//!
//! Prevents unbounded priority inversion across synchronous `Endpoint::call` chains while bounding
//! propagation strictly to `MAX_DONATION_DEPTH = 4` hops and rejecting synchronous `call` cycles
//! (`A -> ... -> A`) deterministically with `DonateError::CycleDetected` (`SysError::Deadlock`).
//!
//! Scheduling priority convention (`PerCpuRunQueue`): smaller numeric `prio` (`0..PRIO_LEVELS`)
//! denotes higher urgency (`1` = High, `3` = Normal, `5` = Low, `7` = Idle).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::gdt_tss::MAX_CPUS;
use crate::percpu::read_gs_u64_at_8;
use crate::sched::runqueue::PRIO_LEVELS;
use crate::sched::{tcb, TOTAL_TCB_SLOTS};
use crate::vspace::tlb::poll_and_ack_pending_shootdown;

pub const MAX_DONATION_DEPTH: usize = 4;
const NO_SLOT: u16 = u16::MAX;

const _: () = assert!(TOTAL_TCB_SLOTS <= 128);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DonateError {
    InvalidSlot,
    CycleDetected,
}

#[derive(Clone, Copy)]
struct DonateNode {
    base_prio: u8,
    effective_prio: u8,
    waiting_on_slot: u16,
    donors_lo: u64,
    donors_hi: u64,
}

impl DonateNode {
    const fn new() -> Self {
        Self {
            base_prio: (PRIO_LEVELS - 1) as u8,
            effective_prio: (PRIO_LEVELS - 1) as u8,
            waiting_on_slot: NO_SLOT,
            donors_lo: 0,
            donors_hi: 0,
        }
    }

    #[inline]
    fn add_donor(&mut self, slot: usize) {
        if slot < 64 {
            self.donors_lo |= 1u64 << slot;
        } else {
            self.donors_hi |= 1u64 << (slot - 64);
        }
    }

    #[inline]
    fn remove_donor(&mut self, slot: usize) {
        if slot < 64 {
            self.donors_lo &= !(1u64 << slot);
        } else {
            self.donors_hi &= !(1u64 << (slot - 64));
        }
    }

    #[inline]
    fn has_donors(&self) -> bool {
        (self.donors_lo | self.donors_hi) != 0
    }
}

struct DonateTable {
    locked: AtomicBool,
    nodes: UnsafeCell<[DonateNode; TOTAL_TCB_SLOTS]>,
}

// SAFETY: `nodes` is accessed exclusively while holding `DONATE_TABLE.locked` with `RFLAGS.IF == 0`.
unsafe impl Sync for DonateTable {}

static DONATE_TABLE: DonateTable = DonateTable {
    locked: AtomicBool::new(false),
    nodes: UnsafeCell::new([const { DonateNode::new() }; TOTAL_TCB_SLOTS]),
};

/// Fast lock-free indicator (`true` iff `slot` currently has an active outgoing call edge).
static ACTIVE_OUTGOING_EDGE: [AtomicBool; TOTAL_TCB_SLOTS] =
    [const { AtomicBool::new(false) }; TOTAL_TCB_SLOTS];

pub static GLOBAL_DONATIONS_APPLIED: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_DONATIONS_REVERTED: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_DONATION_CYCLES: AtomicU64 = AtomicU64::new(0);

#[inline]
fn with_donate_lock<R>(f: impl FnOnce(&mut [DonateNode; TOTAL_TCB_SLOTS]) -> R) -> R {
    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;
    while DONATE_TABLE
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        poll_and_ack_pending_shootdown(cpu_idx);
        core::hint::spin_loop();
    }
    // SAFETY: Exclusive lock + `CLI` held on `cpu_idx`.
    let out = f(unsafe { &mut *DONATE_TABLE.nodes.get() });
    DONATE_TABLE.locked.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline]
fn recompute_effective_prio(nodes: &[DonateNode; TOTAL_TCB_SLOTS], slot: usize) -> u8 {
    let node = &nodes[slot];
    let mut best = node.base_prio;
    let mut lo = node.donors_lo;
    while lo != 0 {
        let d = lo.trailing_zeros() as usize;
        lo &= lo - 1;
        if d < TOTAL_TCB_SLOTS && nodes[d].effective_prio < best {
            best = nodes[d].effective_prio;
        }
    }
    let mut hi = node.donors_hi;
    while hi != 0 {
        let d = 64 + (hi.trailing_zeros() as usize);
        hi &= hi - 1;
        if d < TOTAL_TCB_SLOTS && nodes[d].effective_prio < best {
            best = nodes[d].effective_prio;
        }
    }
    best
}

/// Initialize or reset `slot`'s base and effective scheduling priority.
pub fn init_slot_priority(slot: usize, base_prio: usize) {
    if slot >= TOTAL_TCB_SLOTS {
        return;
    }
    let clamped = base_prio.min(PRIO_LEVELS - 1) as u8;
    ACTIVE_OUTGOING_EDGE[slot].store(false, Ordering::Release);
    with_donate_lock(|nodes| {
        nodes[slot] = DonateNode {
            base_prio: clamped,
            effective_prio: clamped,
            waiting_on_slot: NO_SLOT,
            donors_lo: 0,
            donors_hi: 0,
        };
    });
    tcb(slot).prio.store(clamped as usize, Ordering::Release);
}

#[inline]
pub fn inspect_slot_priorities(slot: usize) -> (usize, usize) {
    if slot >= TOTAL_TCB_SLOTS {
        return (PRIO_LEVELS - 1, PRIO_LEVELS - 1);
    }
    with_donate_lock(|nodes| {
        (
            nodes[slot].base_prio as usize,
            nodes[slot].effective_prio as usize,
        )
    })
}

/// Register synchronous `caller_slot -> server_slot` call dependency, detect cycles up to
/// `MAX_DONATION_DEPTH`, and propagate `caller_slot`'s effective priority to `server_slot`.
pub fn donate_on_call(caller_slot: usize, server_slot: usize) -> Result<usize, DonateError> {
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&caller_slot)
        || !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&server_slot)
    {
        return Err(DonateError::InvalidSlot);
    }
    if caller_slot == server_slot {
        GLOBAL_DONATION_CYCLES.fetch_add(1, Ordering::Relaxed);
        return Err(DonateError::CycleDetected);
    }

    with_donate_lock(|nodes| {
        // Detach any stale outgoing edge defensively before linking the new call edge.
        let prev_srv = nodes[caller_slot].waiting_on_slot;
        if prev_srv != NO_SLOT {
            nodes[caller_slot].waiting_on_slot = NO_SLOT;
            nodes[prev_srv as usize].remove_donor(caller_slot);
        }

        // Cycle detection across up to `MAX_DONATION_DEPTH` downstream call hops.
        let mut cursor = server_slot;
        for _ in 0..MAX_DONATION_DEPTH {
            if cursor == caller_slot {
                GLOBAL_DONATION_CYCLES.fetch_add(1, Ordering::Relaxed);
                return Err(DonateError::CycleDetected);
            }
            let nxt = nodes[cursor].waiting_on_slot;
            if nxt == NO_SLOT {
                break;
            }
            cursor = nxt as usize;
            if cursor == caller_slot {
                GLOBAL_DONATION_CYCLES.fetch_add(1, Ordering::Relaxed);
                return Err(DonateError::CycleDetected);
            }
        }

        nodes[caller_slot].waiting_on_slot = server_slot as u16;
        nodes[server_slot].add_donor(caller_slot);
        ACTIVE_OUTGOING_EDGE[caller_slot].store(true, Ordering::Release);

        let mut boosted = false;
        let mut cur = server_slot;
        for _ in 0..MAX_DONATION_DEPTH {
            let new_eff = recompute_effective_prio(nodes, cur);
            if new_eff < nodes[cur].effective_prio {
                nodes[cur].effective_prio = new_eff;
                tcb(cur).prio.store(new_eff as usize, Ordering::Release);
                boosted = true;
            } else {
                break;
            }
            let nxt = nodes[cur].waiting_on_slot;
            if nxt == NO_SLOT {
                break;
            }
            cur = nxt as usize;
        }

        if boosted {
            GLOBAL_DONATIONS_APPLIED.fetch_add(1, Ordering::Relaxed);
        }
        Ok(nodes[server_slot].effective_prio as usize)
    })
}

/// Revert an active outgoing priority donation from `caller_slot` upon `reply`, `timeout`,
/// `cancel`/`revoke`, or caller termination.
#[inline]
pub fn revert_donation_for_caller(caller_slot: usize) -> bool {
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&caller_slot) {
        return false;
    }
    if !ACTIVE_OUTGOING_EDGE[caller_slot].load(Ordering::Acquire) {
        return false;
    }

    with_donate_lock(|nodes| {
        let srv_u16 = nodes[caller_slot].waiting_on_slot;
        if srv_u16 == NO_SLOT {
            ACTIVE_OUTGOING_EDGE[caller_slot].store(false, Ordering::Release);
            return false;
        }
        nodes[caller_slot].waiting_on_slot = NO_SLOT;
        ACTIVE_OUTGOING_EDGE[caller_slot].store(false, Ordering::Release);

        let mut cur = srv_u16 as usize;
        nodes[cur].remove_donor(caller_slot);

        let mut changed = false;
        for _ in 0..MAX_DONATION_DEPTH {
            let new_eff = recompute_effective_prio(nodes, cur);
            if new_eff != nodes[cur].effective_prio {
                nodes[cur].effective_prio = new_eff;
                tcb(cur).prio.store(new_eff as usize, Ordering::Release);
                changed = true;
            } else {
                break;
            }
            let nxt = nodes[cur].waiting_on_slot;
            if nxt == NO_SLOT {
                break;
            }
            cur = nxt as usize;
        }

        if changed {
            GLOBAL_DONATIONS_REVERTED.fetch_add(1, Ordering::Relaxed);
        }
        changed
    })
}

/// Scrub both outgoing and incoming donation edges when `slot` exits or faults.
pub fn cleanup_donations_on_thread_exit(slot: usize) {
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&slot) {
        return;
    }
    let _ = revert_donation_for_caller(slot);
    with_donate_lock(|nodes| {
        if nodes[slot].has_donors() {
            let mut lo = nodes[slot].donors_lo;
            while lo != 0 {
                let d = lo.trailing_zeros() as usize;
                lo &= lo - 1;
                if d < TOTAL_TCB_SLOTS && nodes[d].waiting_on_slot == (slot as u16) {
                    nodes[d].waiting_on_slot = NO_SLOT;
                    ACTIVE_OUTGOING_EDGE[d].store(false, Ordering::Release);
                }
            }
            let mut hi = nodes[slot].donors_hi;
            while hi != 0 {
                let d = 64 + (hi.trailing_zeros() as usize);
                hi &= hi - 1;
                if d < TOTAL_TCB_SLOTS && nodes[d].waiting_on_slot == (slot as u16) {
                    nodes[d].waiting_on_slot = NO_SLOT;
                    ACTIVE_OUTGOING_EDGE[d].store(false, Ordering::Release);
                }
            }
            nodes[slot].donors_lo = 0;
            nodes[slot].donors_hi = 0;
        }
        let base = nodes[slot].base_prio;
        nodes[slot].effective_prio = base;
        tcb(slot).prio.store(base as usize, Ordering::Release);
    });
}
