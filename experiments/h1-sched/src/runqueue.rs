//! Lock-free per-CPU runqueues (`RunQueue` and `PerCpuRunQueue`) for `EXEC_READY` tasks.

use crate::sync::{yield_now, AtomicUsize, Ordering, UnsafeCell};

struct RingCell {
    seq: AtomicUsize,
    slot: UnsafeCell<usize>,
}

/// Lock-free bounded MPMC ring queue (Vyukov algorithm) for per-CPU `READY` task IDs.
/// Uses pure atomic sequence numbers without `Mutex`, so Loom verifies true weak-memory visibility.
pub struct RunQueue {
    mask: usize,
    cells: Box<[RingCell]>,
    enqueue_pos: AtomicUsize,
    dequeue_pos: AtomicUsize,
}

// SAFETY: Slot access is strictly ordered by per-cell `Acquire`/`Release` sequence transitions.
unsafe impl Send for RunQueue {}
unsafe impl Sync for RunQueue {}

impl RunQueue {
    /// Creates a bounded lock-free runqueue; `capacity` must be a power of two `>= 2`.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 2 && capacity.is_power_of_two());
        let cells = (0..capacity)
            .map(|i| RingCell {
                seq: AtomicUsize::new(i),
                slot: UnsafeCell::new(0),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            mask: capacity - 1,
            cells,
            enqueue_pos: AtomicUsize::new(0),
            dequeue_pos: AtomicUsize::new(0),
        }
    }

    pub fn push(&self, task_id: usize) -> bool {
        let mut pos = self.enqueue_pos.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & self.mask];
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
                        cell.slot.with_mut(|p| unsafe { *p = task_id });
                        cell.seq.store(pos.wrapping_add(1), Ordering::Release);
                        return true;
                    }
                    Err(actual) => pos = actual,
                }
            } else if diff < 0 {
                return false;
            } else {
                pos = self.enqueue_pos.load(Ordering::Relaxed);
                yield_now();
            }
        }
    }

    pub fn pop(&self) -> Option<usize> {
        let mut pos = self.dequeue_pos.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & self.mask];
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
                        let id = cell.slot.with(|p| unsafe { *p });
                        cell.seq
                            .store(pos.wrapping_add(self.mask).wrapping_add(1), Ordering::Release);
                        return Some(id);
                    }
                    Err(actual) => pos = actual,
                }
            } else if diff < 0 {
                return None;
            } else {
                pos = self.dequeue_pos.load(Ordering::Relaxed);
                yield_now();
            }
        }
    }

    #[inline]
    pub fn steal(&self) -> Option<usize> {
        self.pop()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        let deq = self.dequeue_pos.load(Ordering::Acquire);
        let enq = self.enqueue_pos.load(Ordering::Acquire);
        deq == enq
    }
}

/// Multi-priority per-CPU runqueue (`0` = highest priority .. `PRIOS - 1` = lowest).
pub struct PerCpuRunQueue<const PRIOS: usize = 4> {
    rings: [RunQueue; PRIOS],
}

impl<const PRIOS: usize> PerCpuRunQueue<PRIOS> {
    pub fn new(capacity_per_prio: usize) -> Self {
        Self {
            rings: std::array::from_fn(|_| RunQueue::new(capacity_per_prio)),
        }
    }

    #[inline]
    pub fn ring(&self, prio: usize) -> &RunQueue {
        &self.rings[prio]
    }

    #[inline]
    pub fn push(&self, prio: usize, task_id: usize) -> bool {
        self.rings[prio].push(task_id)
    }

    #[inline]
    pub fn pop_local(&self) -> Option<usize> {
        for ring in &self.rings {
            if let Some(id) = ring.pop() {
                return Some(id);
            }
        }
        None
    }

    #[inline]
    pub fn steal(&self) -> Option<usize> {
        self.pop_local()
    }
}
