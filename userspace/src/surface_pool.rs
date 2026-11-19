// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Multi-buffered Display Scanout/Render `SurfaceBufferSet` (`1080p` & `4K` Double/Triple Buffering)
//! with Damage Rectangles and Frame Presentation Timing (`Milestone 2.3`).

pub const MAX_SURFACE_BUFFERS: usize = 3;
pub const HUGE_2M_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SurfacePixelFormat {
    Xrgb8888 = 1,
    Argb8888 = 2,
    Bgrx8888 = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SurfaceSlotState {
    Free = 0,
    Drawing = 1,
    Queued = 2,
    ScanningOut = 3,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl DamageRect {
    pub const EMPTY: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    #[inline]
    pub const fn full(w: u32, h: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Clamp rectangle cleanly to `[0..surf_w, 0..surf_h]`.
    pub fn clamp(self, surf_w: u32, surf_h: u32) -> Self {
        if self.x >= surf_w || self.y >= surf_h || self.is_empty() {
            return Self::EMPTY;
        }
        let max_w = surf_w - self.x;
        let max_h = surf_h - self.y;
        Self {
            x: self.x,
            y: self.y,
            width: if self.width > max_w { max_w } else { self.width },
            height: if self.height > max_h { max_h } else { self.height },
        }
    }

    /// Bounding-box union of two damage regions clamped to `(surf_w, surf_h)`.
    pub fn union_clamped(self, other: Self, surf_w: u32, surf_h: u32) -> Self {
        let a = self.clamp(surf_w, surf_h);
        let b = other.clamp(surf_w, surf_h);
        if a.is_empty() {
            return b;
        }
        if b.is_empty() {
            return a;
        }
        let x0 = if a.x < b.x { a.x } else { b.x };
        let y0 = if a.y < b.y { a.y } else { b.y };
        let a_x1 = a.x.saturating_add(a.width);
        let a_y1 = a.y.saturating_add(a.height);
        let b_x1 = b.x.saturating_add(b.width);
        let b_y1 = b.y.saturating_add(b.height);
        let x1 = if a_x1 > b_x1 { a_x1 } else { b_x1 };
        let y1 = if a_y1 > b_y1 { a_y1 } else { b_y1 };
        Self {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        }
    }

    #[inline]
    pub const fn pixel_area(self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }
}

#[derive(Clone, Copy)]
pub struct SurfaceBufferSlot {
    pub state: SurfaceSlotState,
    pub fb_id: u32,
    pub frame_seq: u64,
    pub vaddr: u64,
    pub iova_or_phys: u64,
    pub damage: DamageRect,
    pub submit_tsc: u64,
    pub scanout_tsc: u64,
}

impl SurfaceBufferSlot {
    pub const EMPTY: Self = Self {
        state: SurfaceSlotState::Free,
        fb_id: 0,
        frame_seq: 0,
        vaddr: 0,
        iova_or_phys: 0,
        damage: DamageRect::EMPTY,
        submit_tsc: 0,
        scanout_tsc: 0,
    };
}

pub struct SurfaceBufferSet {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub format: SurfacePixelFormat,
    pub buffer_count: usize,
    pub bytes_per_buffer: usize,
    pub pages_2m_per_buffer: usize,
    pub front_idx: Option<usize>,
    pub queued_idx: Option<usize>,
    pub next_seq: u64,
    pub presents_completed: u64,
    pub triple_drop_overrides: u64,
    pub last_present_cycles: u64,
    pub total_damage_pixels: u64,
    pub slots: [SurfaceBufferSlot; MAX_SURFACE_BUFFERS],
}

impl SurfaceBufferSet {
    pub const fn new(
        width: u32,
        height: u32,
        format: SurfacePixelFormat,
        triple_buffered: bool,
    ) -> Self {
        let count = if triple_buffered { 3 } else { 2 };
        let stride_bytes = width.saturating_mul(4);
        let bytes_per_buffer = (stride_bytes as usize).saturating_mul(height as usize);
        let pages_2m_per_buffer = bytes_per_buffer.div_ceil(HUGE_2M_BYTES);
        Self {
            width,
            height,
            stride_bytes,
            format,
            buffer_count: count,
            bytes_per_buffer,
            pages_2m_per_buffer,
            front_idx: None,
            queued_idx: None,
            next_seq: 1,
            presents_completed: 0,
            triple_drop_overrides: 0,
            last_present_cycles: 0,
            total_damage_pixels: 0,
            slots: [SurfaceBufferSlot::EMPTY; MAX_SURFACE_BUFFERS],
        }
    }

    pub const fn new_1080p(triple_buffered: bool) -> Self {
        Self::new(1920, 1080, SurfacePixelFormat::Xrgb8888, triple_buffered)
    }

    pub const fn new_4k(triple_buffered: bool) -> Self {
        Self::new(3840, 2160, SurfacePixelFormat::Xrgb8888, triple_buffered)
    }

    pub fn bind_slot_backing(&mut self, idx: usize, fb_id: u32, vaddr: u64, iova_or_phys: u64) -> bool {
        if idx >= self.buffer_count {
            return false;
        }
        self.slots[idx].fb_id = fb_id;
        self.slots[idx].vaddr = vaddr;
        self.slots[idx].iova_or_phys = iova_or_phys;
        true
    }

    /// Acquire a `Free` back buffer for rendering, or recycle superseded `Queued` mailbox buffer in triple-buffer mode.
    pub fn acquire_draw_slot(&mut self) -> Option<usize> {
        if let Some(idx) = (0..self.buffer_count).find(|&i| self.slots[i].state == SurfaceSlotState::Free) {
            self.slots[idx].state = SurfaceSlotState::Drawing;
            self.slots[idx].damage = DamageRect::EMPTY;
            return Some(idx);
        }
        if self.buffer_count == 3 {
            if let Some(q_idx) = self.queued_idx.take() {
                self.slots[q_idx].state = SurfaceSlotState::Drawing;
                self.slots[q_idx].damage = DamageRect::EMPTY;
                self.triple_drop_overrides = self.triple_drop_overrides.saturating_add(1);
                return Some(q_idx);
            }
        }
        None
    }

    /// Commit a rendered back buffer + damage rect for scanout presentation.
    pub fn submit_frame(&mut self, idx: usize, raw_damage: DamageRect, now_tsc: u64) -> Option<u64> {
        if idx >= self.buffer_count || self.slots[idx].state != SurfaceSlotState::Drawing {
            return None;
        }
        if let Some(prev_q) = self.queued_idx.replace(idx) {
            self.slots[prev_q].state = SurfaceSlotState::Free;
            self.triple_drop_overrides = self.triple_drop_overrides.saturating_add(1);
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let dmg = raw_damage.clamp(self.width, self.height);
        self.slots[idx].state = SurfaceSlotState::Queued;
        self.slots[idx].frame_seq = seq;
        self.slots[idx].damage = dmg;
        self.slots[idx].submit_tsc = now_tsc;
        self.total_damage_pixels = self.total_damage_pixels.saturating_add(dmg.pixel_area());
        Some(seq)
    }

    /// Promote the latest `Queued` buffer to active scanout (`ScanningOut`) and retire previous `front_idx` to `Free`.
    pub fn latch_scanout(&mut self, now_tsc: u64) -> Option<(usize, u32, u64)> {
        let next_idx = self.queued_idx.take()?;
        if let Some(old_front) = self.front_idx.replace(next_idx) {
            self.slots[old_front].state = SurfaceSlotState::Free;
        }
        let slot = &mut self.slots[next_idx];
        slot.state = SurfaceSlotState::ScanningOut;
        slot.scanout_tsc = now_tsc;
        self.last_present_cycles = now_tsc.saturating_sub(slot.submit_tsc);
        self.presents_completed = self.presents_completed.saturating_add(1);
        Some((next_idx, slot.fb_id, slot.frame_seq))
    }

    #[inline]
    pub fn total_pool_bytes(&self) -> usize {
        self.bytes_per_buffer.saturating_mul(self.buffer_count)
    }
}

/// Deterministic userspace verification of `1080p` & `4K` double/triple buffer state transitions.
pub fn verify_surface_pool_invariants() -> bool {
    fn run() -> Option<bool> {
        let mut s1080 = SurfaceBufferSet::new_1080p(false);
        if s1080.buffer_count != 2
            || s1080.bytes_per_buffer != 1920 * 1080 * 4
            || s1080.pages_2m_per_buffer != 4
        {
            return Some(false);
        }
        let _ = s1080.bind_slot_backing(0, 10, 0x3000_0000, 0x1_0000_0000);
        let _ = s1080.bind_slot_backing(1, 11, 0x3100_0000, 0x1_0100_0000);
        let b0 = s1080.acquire_draw_slot()?;
        let b1 = s1080.acquire_draw_slot()?;
        if b0 == b1 || s1080.acquire_draw_slot().is_some() {
            return Some(false);
        }
        let u = DamageRect { x: 100, y: 100, width: 200, height: 150 }.union_clamped(
            DamageRect { x: 1850, y: 1000, width: 300, height: 200 },
            1920,
            1080,
        );
        if u.width != 1820 || u.height != 980 {
            return Some(false);
        }
        s1080.submit_frame(b0, u, 1000)?;
        let (_, fb0, seq0) = s1080.latch_scanout(1450)?;
        if fb0 != 10 || seq0 != 1 || s1080.last_present_cycles != 450 {
            return Some(false);
        }
        s1080.submit_frame(b1, DamageRect::full(1920, 1080), 2000)?;
        let (_, fb1, seq1) = s1080.latch_scanout(2500)?;
        if fb1 != 11 || seq1 != 2 || s1080.slots[b0].state != SurfaceSlotState::Free {
            return Some(false);
        }

        let mut s4k = SurfaceBufferSet::new_4k(true);
        if s4k.buffer_count != 3
            || s4k.bytes_per_buffer != 3840 * 2160 * 4
            || s4k.pages_2m_per_buffer != 16
            || s4k.total_pool_bytes() != 3840 * 2160 * 4 * 3
        {
            return Some(false);
        }
        let _ = s4k.bind_slot_backing(0, 20, 0x4000_0000, 0x2_0000_0000);
        let _ = s4k.bind_slot_backing(1, 21, 0x4400_0000, 0x2_0400_0000);
        let _ = s4k.bind_slot_backing(2, 22, 0x4800_0000, 0x2_0800_0000);
        let t0 = s4k.acquire_draw_slot()?;
        s4k.submit_frame(t0, DamageRect::full(3840, 2160), 10)?;
        s4k.latch_scanout(20)?;
        let t1 = s4k.acquire_draw_slot()?;
        s4k.submit_frame(t1, DamageRect::full(3840, 2160), 30)?;
        let t2 = s4k.acquire_draw_slot()?;
        s4k.submit_frame(t2, DamageRect::full(3840, 2160), 40)?;
        let t3 = s4k.acquire_draw_slot()?;
        Some(t3 == t1 && s4k.triple_drop_overrides == 1 && s4k.queued_idx == Some(t2))
    }
    run() == Some(true)
}
