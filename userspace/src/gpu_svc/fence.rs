// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Monotonic Asynchronous GPU Fence Timeline & VBlank Pacing Tracker (`Milestone 3.3`).

use crate::{rdtsc_user, ETIMEDOUT};

#[derive(Clone, Copy, Debug)]
pub struct FenceTimeline {
    pub next_seq: u64,
    pub signaled_seq: u64,
    pub last_submit_tsc: u64,
    pub last_signal_tsc: u64,
    pub total_waits: u64,
    pub timeouts: u64,
}

impl FenceTimeline {
    pub const fn new() -> Self {
        Self {
            next_seq: 1,
            signaled_seq: 0,
            last_submit_tsc: 0,
            last_signal_tsc: 0,
            total_waits: 0,
            timeouts: 0,
        }
    }

    #[inline]
    pub fn alloc_seq(&mut self, now_tsc: u64) -> u64 {
        let s = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.last_submit_tsc = now_tsc;
        s
    }

    #[inline]
    pub fn signal(&mut self, seq: u64, now_tsc: u64) -> bool {
        if seq == 0 || seq >= self.next_seq || seq <= self.signaled_seq {
            return false;
        }
        self.signaled_seq = seq;
        self.last_signal_tsc = now_tsc;
        true
    }

    #[inline]
    pub const fn is_signaled(&self, seq: u64) -> bool {
        seq > 0 && seq <= self.signaled_seq
    }

    #[inline]
    pub const fn pending_count(&self) -> u64 {
        self.next_seq.saturating_sub(1).saturating_sub(self.signaled_seq)
    }

    /// Poll a hardware/backend completion closure up to `spin_budget` TSC cycles before returning `-ETIMEDOUT`.
    pub fn wait_spin_budget(
        &mut self,
        target_seq: u64,
        spin_budget: u64,
        mut poll_hw: impl FnMut(u64) -> bool,
    ) -> Result<(), isize> {
        self.total_waits = self.total_waits.saturating_add(1);
        if self.is_signaled(target_seq) {
            return Ok(());
        }
        let t0 = rdtsc_user();
        loop {
            let now = rdtsc_user();
            if poll_hw(target_seq) {
                let _ = self.signal(target_seq, now);
                return Ok(());
            }
            if now.saturating_sub(t0) >= spin_budget {
                self.timeouts = self.timeouts.saturating_add(1);
                return Err(ETIMEDOUT);
            }
            core::hint::spin_loop();
        }
    }
}

impl Default for FenceTimeline {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VblankPacer {
    pub period_cyc: u64,
    pub vblank_seq: u64,
    pub last_vblank_tsc: u64,
    pub missed_deadlines: u64,
}

impl VblankPacer {
    pub const fn new(tsc_mhz: u64, refresh_hz: u64) -> Self {
        let hz = if refresh_hz == 0 { 60 } else { refresh_hz };
        let mhz = if tsc_mhz < 1000 { 1000 } else { tsc_mhz };
        let period = (mhz * 1_000_000) / hz;
        Self {
            period_cyc: period,
            vblank_seq: 0,
            last_vblank_tsc: 0,
            missed_deadlines: 0,
        }
    }

    pub fn tick_present(&mut self, now_tsc: u64) -> u64 {
        if self.last_vblank_tsc == 0 {
            self.last_vblank_tsc = now_tsc;
            self.vblank_seq = 1;
            return 1;
        }
        let dt = now_tsc.saturating_sub(self.last_vblank_tsc);
        let intervals = dt.checked_div(self.period_cyc).unwrap_or(1);
        if intervals > 1 {
            self.missed_deadlines = self.missed_deadlines.saturating_add(intervals - 1);
        }
        self.vblank_seq = self.vblank_seq.saturating_add(1);
        self.last_vblank_tsc = now_tsc;
        self.vblank_seq
    }
}
