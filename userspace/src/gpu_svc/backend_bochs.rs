// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Linux v6.18 DRM `bochs` (`1234:1111`) Island Backend Adapter (`Milestone 3.1`).

use super::abi::{
    GpuBackend, GpuBackendCaps, GpuBackendContract, GpuBackendKind, GpuExecutionTier, GpuMode,
    GpuPresentReq, GpuPresentResult, GpuSurfaceDesc,
};
use super::fence::{FenceTimeline, VblankPacer};
use crate::surface_pool::{SurfaceBufferSet, SurfacePixelFormat};
use crate::{rdtsc_user, EINVAL};

pub struct LinuxDrmBochsBackend {
    mode: GpuMode,
    probed: bool,
    pool: SurfaceBufferSet,
    fences: FenceTimeline,
    vblank: VblankPacer,
    surface_id: u32,
}

impl LinuxDrmBochsBackend {
    pub const fn new(tsc_mhz: u64) -> Self {
        let mode = GpuMode {
            width: 800,
            height: 600,
            refresh_mhz: 60_000,
            format: SurfacePixelFormat::Xrgb8888,
        };
        Self {
            mode,
            probed: false,
            pool: SurfaceBufferSet::new(mode.width, mode.height, SurfacePixelFormat::Xrgb8888, false),
            fences: FenceTimeline::new(),
            vblank: VblankPacer::new(tsc_mhz, 60),
            surface_id: 20,
        }
    }
}

impl GpuBackend for LinuxDrmBochsBackend {
    fn kind(&self) -> GpuBackendKind {
        GpuBackendKind::LinuxDrmBochs
    }

    fn contract(&self) -> GpuBackendContract {
        GpuBackendContract {
            kind: GpuBackendKind::LinuxDrmBochs,
            tier: GpuExecutionTier::LxCompatIsland,
            flags: GpuBackendCaps(
                GpuBackendCaps::SCANOUT_2D.0
                    | GpuBackendCaps::ATOMIC_KMS.0
                    | GpuBackendCaps::DMA_BUF_IMPORT.0
                    | GpuBackendCaps::ASYNC_FENCE.0
                    | GpuBackendCaps::VBLANK_PACING.0
                    | GpuBackendCaps::IOMMU_ISOLATED.0
                    | GpuBackendCaps::GOP_HANDOVER.0,
            ),
            vendor_id: 0x1234,
            device_id: 0x1111,
            max_width: 1920,
            max_height: 1200,
            capset_count: 0,
            broker_in_data_path: false,
        }
    }

    fn probe(&mut self) -> Result<GpuBackendContract, isize> {
        self.probed = true;
        Ok(self.contract())
    }

    fn modeset(&mut self, mode: GpuMode) -> Result<(), isize> {
        if !self.probed || mode.width < 320 || mode.height < 240 || mode.width > 1920 || mode.height > 1200 {
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
        self.surface_id = self.surface_id.wrapping_add(1).max(20);
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
        let dmg = req.damage.clamp(self.mode.width, self.mode.height);
        let fence_seq = self.fences.alloc_seq(req.submit_tsc);
        let t0 = rdtsc_user();
        if let Some(slot) = self.pool.acquire_draw_slot() {
            let _ = self.pool.submit_frame(slot, dmg, req.submit_tsc);
            let _ = self.pool.latch_scanout(t0);
        }
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
        self.fences = FenceTimeline::new();
        self.probed = true;
        Ok(())
    }
}
