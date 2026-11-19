// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! VirtIO-GPU (`1AF4:1050`) Backend Adapter for `DRY GPU Service` (`Milestone 3.1`).

use super::abi::{
    GpuBackend, GpuBackendCaps, GpuBackendContract, GpuBackendKind, GpuExecutionTier, GpuMode,
    GpuPresentReq, GpuPresentResult, GpuSurfaceDesc,
};
use super::fence::{FenceTimeline, VblankPacer};
use crate::gpu_proto::{GPU_OP_PRESENT_RECT, GPU_OP_QUERY_INFO};
use crate::surface_pool::{SurfaceBufferSet, SurfacePixelFormat};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_ipc_call_inline, EINVAL, EIO, IPC_OP_PONG,
};

pub struct VirtioGpuBackend {
    gpu_ep_cap: u64,
    mode: GpuMode,
    probed: bool,
    pool: SurfaceBufferSet,
    fences: FenceTimeline,
    vblank: VblankPacer,
    surface_id: u32,
}

impl VirtioGpuBackend {
    pub const fn new(gpu_ep_cap: u64, tsc_mhz: u64) -> Self {
        let mode = GpuMode::DEFAULT_1280X800;
        Self {
            gpu_ep_cap,
            mode,
            probed: false,
            pool: SurfaceBufferSet::new(mode.width, mode.height, SurfacePixelFormat::Bgrx8888, false),
            fences: FenceTimeline::new(),
            vblank: VblankPacer::new(tsc_mhz, 60),
            surface_id: 10,
        }
    }

    pub fn bind_endpoint(&mut self, ep_cap: u64) {
        self.gpu_ep_cap = ep_cap;
    }
}

impl GpuBackend for VirtioGpuBackend {
    fn kind(&self) -> GpuBackendKind {
        GpuBackendKind::VirtioGpu
    }

    fn contract(&self) -> GpuBackendContract {
        GpuBackendContract {
            kind: GpuBackendKind::VirtioGpu,
            tier: GpuExecutionTier::NativeIsland,
            flags: GpuBackendCaps(
                GpuBackendCaps::SCANOUT_2D.0
                    | GpuBackendCaps::ATOMIC_KMS.0
                    | GpuBackendCaps::DMA_BUF_IMPORT.0
                    | GpuBackendCaps::ASYNC_FENCE.0
                    | GpuBackendCaps::VBLANK_PACING.0
                    | GpuBackendCaps::IOMMU_ISOLATED.0
                    | GpuBackendCaps::GOP_HANDOVER.0,
            ),
            vendor_id: 0x1af4,
            device_id: 0x1050,
            max_width: 3840,
            max_height: 2160,
            capset_count: 1,
            broker_in_data_path: false,
        }
    }

    fn probe(&mut self) -> Result<GpuBackendContract, isize> {
        if self.gpu_ep_cap != 0 {
            let (tag, _, mrs) = sys_ipc_call_inline(
                self.gpu_ep_cap,
                pack_msg_tag(GPU_OP_QUERY_INFO, 0, 1, 0),
                [0; 4],
                0,
            )?;
            if msg_tag_label(tag) != IPC_OP_PONG {
                return Err(EIO);
            }
            let w = (mrs[0] & 0xffff_ffff) as u32;
            let h = (mrs[0] >> 32) as u32;
            if w >= 320 && h >= 240 {
                self.mode.width = w;
                self.mode.height = h;
            }
        }
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
        self.surface_id = self.surface_id.wrapping_add(1).max(10);
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
        let (px, dt) = if self.gpu_ep_cap != 0 {
            let mrs = [
                (dmg.x as u64) | ((dmg.y as u64) << 32),
                (dmg.width as u64) | ((dmg.height as u64) << 32),
                0,
                0,
            ];
            let (tag, _, out) = sys_ipc_call_inline(
                self.gpu_ep_cap,
                pack_msg_tag(GPU_OP_PRESENT_RECT, 0, 4, 0),
                mrs,
                0,
            )?;
            if msg_tag_label(tag) != IPC_OP_PONG {
                return Err(EIO);
            }
            (out[0], out[1])
        } else {
            if let Some(slot) = self.pool.acquire_draw_slot() {
                let _ = self.pool.submit_frame(slot, dmg, req.submit_tsc);
                let _ = self.pool.latch_scanout(t0);
            }
            (dmg.pixel_area(), rdtsc_user().saturating_sub(t0))
        };
        let now = rdtsc_user();
        let _ = self.fences.signal(fence_seq, now);
        let vblank_seq = self.vblank.tick_present(now);
        Ok(GpuPresentResult {
            fence_seq,
            pixels_blitted: px,
            duration_cyc: dt,
            vblank_seq,
        })
    }

    fn wait_fence(&mut self, fence_seq: u64, spin_budget: u64) -> Result<(), isize> {
        let cur_sig = self.fences.signaled_seq;
        self.fences.wait_spin_budget(fence_seq, spin_budget, |s| s <= cur_sig)
    }

    fn quiesce_recover(&mut self) -> Result<(), isize> {
        self.gpu_ep_cap = 0;
        self.fences = FenceTimeline::new();
        self.probed = false;
        Ok(())
    }
}
