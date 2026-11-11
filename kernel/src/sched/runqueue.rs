// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Static `no_std` Lock-Free Per-CPU Priority RunQueues (`ADR-004`, Milestone 1A-F).
//!
//! Uses a bounded MPMC Vyukov sequence ring in `'static` kernel `.bss` (`0` heap allocations) so
//! local enqueue/dequeue, cross-CPU wakeup push, and cross-CPU `EXEC_READY` work-stealing operate
//! without any global scheduler lock (`BKL`-free).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::gdt_tss::MAX_CPUS;

pub const PRIO_LEVELS: usize = 4;
pub const RQ_RING_CAP: usize = 128;
const RQ_RING_MASK: usize = RQ_RING_CAP - 1;

const _: () = assert!(RQ_RING_CAP.is_power_of_two());

struct RingCell {
    seq: AtomicUsize,
    slot: UnsafeCell<u32>,
}

#[repr(C, align(64))]
pub struct StaticRunQueue {
    enqueue_pos: AtomicUsize,
    dequeue_pos: AtomicUsize,
    cells: [RingCell; RQ_RING_CAP],
}

// SAFETY: Every `cells[i].slot` write/read is serialized by `cells[i].seq` `Acquire`/`Release` transitions.
unsafe impl Sync for StaticRunQueue {}
unsafe impl Send for StaticRunQueue {}

impl StaticRunQueue {
    const fn uninit() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO_CELL: RingCell = RingCell {
            seq: AtomicUsize::new(0),
            slot: UnsafeCell::new(0),
        };
        Self {
            enqueue_pos: AtomicUsize::new(0),
            dequeue_pos: AtomicUsize::new(0),
            cells: [ZERO_CELL; RQ_RING_CAP],
        }
    }

    pub fn reset(&self) {
        self.enqueue_pos.store(0, Ordering::Release);
        self.dequeue_pos.store(0, Ordering::Release);
        for (i, cell) in self.cells.iter().enumerate() {
            cell.seq.store(i, Ordering::Release);
        }
    }

    pub fn push(&self, ticket: u32) -> bool {
        let mut pos = self.enqueue_pos.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & RQ_RING_MASK];
            let seq = cell.seq.load(Ordering::Acquire);
            let diff = (seq as isize).wrapping_sub(pos as isize);
            if diff == 0 {
                match self.enqueue_pos.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: Calling CPU won exclusive ownership of `pos` via `enqueue_pos` CAS.
                        unsafe {
                            *cell.slot.get() = ticket;
                        }
                        cell.seq.store(pos.wrapping_add(1), Ordering::Release);
                        return true;
                    }
                    Err(actual) => pos = actual,
                }
            } else if diff < 0 {
                return false;
            } else {
                pos = self.enqueue_pos.load(Ordering::Relaxed);
                core::hint::spin_loop();
            }
        }
    }

    pub fn pop(&self) -> Option<u32> {
        let mut pos = self.dequeue_pos.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & RQ_RING_MASK];
            let seq = cell.seq.load(Ordering::Acquire);
            let diff = (seq as isize).wrapping_sub(pos.wrapping_add(1) as isize);
            if diff == 0 {
                match self.dequeue_pos.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: Calling CPU won exclusive consumer access to `pos`.
                        let val = unsafe { *cell.slot.get() };
                        cell.seq
                            .store(pos.wrapping_add(RQ_RING_MASK).wrapping_add(1), Ordering::Release);
                        return Some(val);
                    }
                    Err(actual) => pos = actual,
                }
            } else if diff < 0 {
                return None;
            } else {
                pos = self.dequeue_pos.load(Ordering::Relaxed);
                core::hint::spin_loop();
            }
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.dequeue_pos.load(Ordering::Acquire) == self.enqueue_pos.load(Ordering::Acquire)
    }
}

#[repr(C, align(64))]
pub struct PerCpuRunQueue {
    rings: [StaticRunQueue; PRIO_LEVELS],
}

impl PerCpuRunQueue {
    const fn uninit() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const EMPTY_RING: StaticRunQueue = StaticRunQueue::uninit();
        Self {
            rings: [EMPTY_RING; PRIO_LEVELS],
        }
    }

    pub fn reset(&self) {
        for r in &self.rings {
            r.reset();
        }
    }

    #[inline]
    pub fn push(&self, prio: usize, ticket: u32) -> bool {
        self.rings[prio.min(PRIO_LEVELS - 1)].push(ticket)
    }

    #[inline]
    pub fn pop_highest(&self) -> Option<u32> {
        for r in &self.rings {
            if let Some(t) = r.pop() {
                return Some(t);
            }
        }
        None
    }

    /// Steal highest-priority migratable ticket (`prio 1..PRIO_LEVELS`; `prio 0` is CPU-pinned).
    #[inline]
    pub fn steal_migratable(&self) -> Option<u32> {
        for r in self.rings.iter().skip(1) {
            if let Some(t) = r.pop() {
                return Some(t);
            }
        }
        None
    }

    pub fn is_all_empty(&self) -> bool {
        self.rings.iter().all(StaticRunQueue::is_empty)
    }
}

#[allow(clippy::declare_interior_mutable_const)]
const UNINIT_CPU_RQ: PerCpuRunQueue = PerCpuRunQueue::uninit();
static CPU_RUNQUEUES: [PerCpuRunQueue; MAX_CPUS] = [UNINIT_CPU_RQ; MAX_CPUS];

#[inline]
pub fn cpu_rq(cpu_idx: usize) -> &'static PerCpuRunQueue {
    &CPU_RUNQUEUES[cpu_idx]
}

/// Pop highest-priority local `EXEC_READY` ticket, or steal migratable work across peer CPUs.
pub fn pop_local_or_steal(local_cpu: usize, online_cpus: usize) -> Option<u32> {
    if let Some(ticket) = CPU_RUNQUEUES[local_cpu].pop_highest() {
        return Some(ticket);
    }
    for offset in 1..online_cpus {
        let victim = (local_cpu + offset) % online_cpus;
        if let Some(ticket) = CPU_RUNQUEUES[victim].steal_migratable() {
            return Some(ticket);
        }
    }
    None
}
