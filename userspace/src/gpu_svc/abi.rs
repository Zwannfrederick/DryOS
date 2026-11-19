// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Vendor-Neutral `DRY GPU Service` Trait, Capabilities, and Request Contracts (`Milestone 3`).

use crate::surface_pool::{DamageRect, SurfacePixelFormat};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuBackendKind {
    UefiGop = 0,
    VirtioGpu = 1,
    LinuxDrmBochs = 2,
    SyntheticGpu = 3,
    LinuxMicroVm = 4,
}

impl GpuBackendKind {
    pub const fn as_str(self) -> &'static [u8] {
        match self {
            Self::UefiGop => b"uefi-gop",
            Self::VirtioGpu => b"virtio-gpu",
            Self::LinuxDrmBochs => b"lx-drm-bochs",
            Self::SyntheticGpu => b"synth-gpu",
            Self::LinuxMicroVm => b"linux-microvm",
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuExecutionTier {
    NativeIsland = 0,
    LxCompatIsland = 1,
    MicroVmCopied = 2,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuBackendCaps(pub u32);

impl GpuBackendCaps {
    pub const NONE: Self = Self(0);
    pub const SCANOUT_2D: Self = Self(1 << 0);
    pub const ATOMIC_KMS: Self = Self(1 << 1);
    pub const VIRGL_3D: Self = Self(1 << 2);
    pub const DMA_BUF_IMPORT: Self = Self(1 << 3);
    pub const ASYNC_FENCE: Self = Self(1 << 4);
    pub const VBLANK_PACING: Self = Self(1 << 5);
    pub const IOMMU_ISOLATED: Self = Self(1 << 6);
    pub const GOP_HANDOVER: Self = Self(1 << 7);
    pub const MICROVM_COPIED: Self = Self(1 << 8);

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuBackendContract {
    pub kind: GpuBackendKind,
    pub tier: GpuExecutionTier,
    pub flags: GpuBackendCaps,
    pub vendor_id: u16,
    pub device_id: u16,
    pub max_width: u32,
    pub max_height: u32,
    pub capset_count: u8,
    pub broker_in_data_path: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuMode {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub format: SurfacePixelFormat,
}

impl GpuMode {
    pub const DEFAULT_1280X800: Self = Self {
        width: 1280,
        height: 800,
        refresh_mhz: 60_000,
        format: SurfacePixelFormat::Xrgb8888,
    };

    pub const MODE_1080P: Self = Self {
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        format: SurfacePixelFormat::Xrgb8888,
    };
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSurfaceDesc {
    pub surface_id: u32,
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub buffer_count: u8,
    pub pool_bytes: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPresentReq {
    pub surface_id: u32,
    pub damage: DamageRect,
    pub submit_tsc: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPresentResult {
    pub fence_seq: u64,
    pub pixels_blitted: u64,
    pub duration_cyc: u64,
    pub vblank_seq: u64,
}

/// Unified GPU backend interface shared across UEFI GOP, VirtIO-GPU, Linux DRM islands, and synthetic GPUs.
pub trait GpuBackend {
    fn kind(&self) -> GpuBackendKind;
    fn contract(&self) -> GpuBackendContract;
    fn probe(&mut self) -> Result<GpuBackendContract, isize>;
    fn modeset(&mut self, mode: GpuMode) -> Result<(), isize>;
    fn alloc_surface(&mut self, mode: GpuMode, triple: bool) -> Result<GpuSurfaceDesc, isize>;
    fn present(&mut self, req: GpuPresentReq) -> Result<GpuPresentResult, isize>;
    fn wait_fence(&mut self, fence_seq: u64, spin_budget: u64) -> Result<(), isize>;
    fn quiesce_recover(&mut self) -> Result<(), isize>;
}
