// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pluggable 4th Synthetic GPU Backend (`0xDEAD:0x6000`) proving `DRY GPU Service` Multi-Backend
//! Extensibility with Zero Kernel / PCI / DMA / Compositor Modifications (`Milestone 3.2`).

use super::abi::{
    GpuBackend, GpuBackendCaps, GpuBackendContract, GpuBackendKind, GpuExecutionTier, GpuMode,
    GpuPresentReq, GpuPresentResult, GpuSurfaceDesc,
};
use super::fence::{FenceTimeline, VblankPacer};
use crate::surface_pool::SurfaceBufferSet;
use crate::{rdtsc_user, EINVAL, EIO};

pub struct SyntheticGpuBackend {
    mode: GpuMode,
    probed: bool,
    pub inject_fault_on_next_present: bool,
    pub frames_checksum: u64,
    pub quiesce_count: u32,
    pool: SurfaceBufferSet,
    fences: FenceTimeline,
    vblank: VblankPacer,
    surface_id: u32,
}

impl SyntheticGpuBackend {
    pub const fn new(tsc_mhz: u64) -> Self {
        let mode = GpuMode::MODE_1080P;
        Self {
            mode,
            probed: false,
            inject_fault_on_next_present: false,
            frames_checksum: 0,
            quiesce_count: 0,
            pool: SurfaceBufferSet::new(mode.width, mode.height, mode.format, true),
            fences: FenceTimeline::new(),
            vblank: VblankPacer::new(tsc_mhz, 60),
            surface_id: 30,
        }
    }
}

impl GpuBackend for SyntheticGpuBackend {
    fn kind(&self) -> GpuBackendKind {
        GpuBackendKind::SyntheticGpu
    }

    fn contract(&self) -> GpuBackendContract {
        GpuBackendContract {
            kind: GpuBackendKind::SyntheticGpu,
            tier: GpuExecutionTier::NativeIsland,
            flags: GpuBackendCaps(
                GpuBackendCaps::SCANOUT_2D.0
                    | GpuBackendCaps::ATOMIC_KMS.0
                    | GpuBackendCaps::ASYNC_FENCE.0
                    | GpuBackendCaps::VBLANK_PACING.0
                    | GpuBackendCaps::GOP_HANDOVER.0,
            ),
            vendor_id: 0xdead,
            device_id: 0x6000,
            max_width: 3840,
            max_height: 2160,
            capset_count: 0,
            broker_in_data_path: false,
        }
    }

    fn probe(&mut self) -> Result<GpuBackendContract, isize> {
        self.probed = true;
        Ok(self.contract())
    }

    fn modeset(&mut self, mode: GpuMode) -> Result<(), isize> {
        if !self.probed || mode.width < 320 || mode.height < 240 || mode.width > 3840 || mode.height > 2160 {
            return Err(EINVAL);
        }
        self.mode = mode;
        Ok(())
    }

    fn alloc_surface(&mut self, mode: GpuMode, triple: bool) -> Result<GpuSurfaceDesc, isize> {
        if !self.probed || mode.width < 320 || mode.height < 240 {
            return Err(EINVAL);
        }
        self.pool = SurfaceBufferSet::new(mode.width, mode.height, mode.format, triple);
        self.surface_id = self.surface_id.wrapping_add(1).max(30);
        Ok(GpuSurfaceDesc {
            surface_id: self.surface_id,
            width: mode.width,
            height: mode.height,
            stride_bytes: self.pool.stride_bytes,
            buffer_count: self.pool.buffer_count as u8,
            pool_bytes: self.pool.total_pool_bytes(),
        })
    }

    fn present(&mut self, req: GpuPresentReq) -> Result<GpuPresentResult, isize> {
        if !self.probed || req.surface_id != self.surface_id {
            return Err(EINVAL);
        }
        if self.inject_fault_on_next_present {
            self.inject_fault_on_next_present = false;
            return Err(EIO);
        }
        let dmg = req.damage.clamp(self.mode.width, self.mode.height);
        let fence_seq = self.fences.alloc_seq(req.submit_tsc);
        let t0 = rdtsc_user();
        if let Some(slot) = self.pool.acquire_draw_slot() {
            let _ = self.pool.submit_frame(slot, dmg, req.submit_tsc);
            let _ = self.pool.latch_scanout(t0);
        }
        self.frames_checksum = self
            .frames_checksum
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ dmg.pixel_area()
            ^ fence_seq;
        let now = rdtsc_user();
        let _ = self.fences.signal(fence_seq, now);
        let vblank_seq = self.vblank.tick_present(now);
        Ok(GpuPresentResult {
            fence_seq,
            pixels_blitted: dmg.pixel_area(),
            duration_cyc: now.saturating_sub(t0),
            vblank_seq,
        })
    }

    fn wait_fence(&mut self, fence_seq: u64, spin_budget: u64) -> Result<(), isize> {
        self.fences.wait_spin_budget(fence_seq, spin_budget, |_| false)
    }

    fn quiesce_recover(&mut self) -> Result<(), isize> {
        self.inject_fault_on_next_present = false;
        self.quiesce_count = self.quiesce_count.saturating_add(1);
        self.fences = FenceTimeline::new();
        self.probed = true;
        Ok(())
    }
}
