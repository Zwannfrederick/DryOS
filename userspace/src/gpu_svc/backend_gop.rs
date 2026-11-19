// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! UEFI GOP Fail-Safe Scanout Backend (`dry-gpu-svc`, `Milestone 3.1`).

use super::abi::{
    GpuBackend, GpuBackendCaps, GpuBackendContract, GpuBackendKind, GpuExecutionTier, GpuMode,
    GpuPresentReq, GpuPresentResult, GpuSurfaceDesc,
};
use super::fence::{FenceTimeline, VblankPacer};
use crate::surface_pool::{SurfaceBufferSet, SurfacePixelFormat};
use crate::{rdtsc_user, EINVAL};

pub struct UefiGopBackend {
    mode: GpuMode,
    probed: bool,
    pool: SurfaceBufferSet,
    fences: FenceTimeline,
    vblank: VblankPacer,
    surface_id: u32,
}

impl UefiGopBackend {
    pub const fn new(width: u32, height: u32, tsc_mhz: u64) -> Self {
        let mode = GpuMode {
            width,
            height,
            refresh_mhz: 60_000,
            format: SurfacePixelFormat::Xrgb8888,
        };
        Self {
            mode,
            probed: false,
            pool: SurfaceBufferSet::new(width, height, SurfacePixelFormat::Xrgb8888, false),
            fences: FenceTimeline::new(),
            vblank: VblankPacer::new(tsc_mhz, 60),
            surface_id: 1,
        }
    }
}

impl GpuBackend for UefiGopBackend {
    fn kind(&self) -> GpuBackendKind {
        GpuBackendKind::UefiGop
    }

    fn contract(&self) -> GpuBackendContract {
        GpuBackendContract {
            kind: GpuBackendKind::UefiGop,
            tier: GpuExecutionTier::NativeIsland,
            flags: GpuBackendCaps(
                GpuBackendCaps::SCANOUT_2D.0
                    | GpuBackendCaps::VBLANK_PACING.0
                    | GpuBackendCaps::GOP_HANDOVER.0,
            ),
            vendor_id: 0x0000,
            device_id: 0x0001,
            max_width: self.mode.width,
            max_height: self.mode.height,
            capset_count: 0,
            broker_in_data_path: false,
        }
    }

    fn probe(&mut self) -> Result<GpuBackendContract, isize> {
        if self.mode.width < 320 || self.mode.height < 240 {
            return Err(EINVAL);
        }
        self.probed = true;
        Ok(self.contract())
    }

    fn modeset(&mut self, mode: GpuMode) -> Result<(), isize> {
        // UEFI GOP scanout geometry is fixed by firmware; dynamic modeset only permits matching size.
        if !self.probed || mode.width != self.mode.width || mode.height != self.mode.height {
            return Err(EINVAL);
        }
        Ok(())
    }

    fn alloc_surface(&mut self, mode: GpuMode, triple: bool) -> Result<GpuSurfaceDesc, isize> {
        if mode.width != self.mode.width || mode.height != self.mode.height {
            return Err(EINVAL);
        }
        self.pool = SurfaceBufferSet::new(mode.width, mode.height, mode.format, triple);
        self.surface_id = self.surface_id.wrapping_add(1).max(1);
        Ok(GpuSurfaceDesc {
            surface_id: self.surface_id,
            width: self.mode.width,
            height: self.mode.height,
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
        let t0 = rdtsc_user();
        let fence_seq = self.fences.alloc_seq(req.submit_tsc);
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
        let cur_sig = self.fences.signaled_seq;
        self.fences.wait_spin_budget(fence_seq, spin_budget, |s| s <= cur_sig)
    }

    fn quiesce_recover(&mut self) -> Result<(), isize> {
        self.probed = true;
        Ok(())
    }
}
