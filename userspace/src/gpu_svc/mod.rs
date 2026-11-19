// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Unified `DRY GPU Service` (`dry-gpu-svc`): Multi-Backend Routing, 7-State GOP Handover,
//! Asynchronous Fences, and Off-Broker Direct Surface Presentation (`Milestone 3`).

pub mod abi;
pub mod backend_bochs;
pub mod backend_gop;
pub mod backend_synth;
pub mod backend_virtio;
pub mod fence;
pub mod handover;

pub use abi::{
    GpuBackend, GpuBackendCaps, GpuBackendContract, GpuBackendKind, GpuExecutionTier, GpuMode,
    GpuPresentReq, GpuPresentResult, GpuSurfaceDesc,
};
pub use backend_bochs::LinuxDrmBochsBackend;
pub use backend_gop::UefiGopBackend;
pub use backend_synth::SyntheticGpuBackend;
pub use backend_virtio::VirtioGpuBackend;
pub use fence::{FenceTimeline, VblankPacer};
pub use handover::{verify_handover_invariants, DisplayHandoverMachine, DisplayHandoverState};

use crate::surface_pool::DamageRect;
use crate::{rdtsc_user, EIO};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSvcVerifySummary {
    pub backends_verified: u8,
    pub handover_states_mask: u8,
    pub synth_presents: u64,
    pub rollbacks_to_gop: u32,
    pub broker_in_data_path: bool,
}

fn exercise_backend(
    backend: &mut dyn GpuBackend,
    handover: &mut DisplayHandoverMachine,
    mode: GpuMode,
    triple: bool,
) -> Result<GpuPresentResult, isize> {
    let c = backend.probe()?;
    if c.broker_in_data_path {
        return Err(EIO);
    }
    backend.modeset(mode)?;
    let surf = backend.alloc_surface(mode, triple)?;
    if handover.state == DisplayHandoverState::KmsActive {
        handover.enter_fault_and_fallback();
    }
    handover.promote_to_kms(backend.kind())?;
    let res = backend.present(GpuPresentReq {
        surface_id: surf.surface_id,
        damage: DamageRect::full(mode.width, mode.height),
        submit_tsc: rdtsc_user(),
    })?;
    backend.wait_fence(res.fence_seq, 50_000)?;
    Ok(res)
}

/// Deterministic Ring-3 verification of all 4 backends (`GOP`, `VirtIO`, `Bochs DRM`, `SyntheticGpu`),
/// 7-state GOP-to-KMS handover + fallback + recovery, monotonic async fences, and off-broker data path.
pub fn verify_gpu_svc_e2e_invariants(tsc_mhz: u64) -> Option<GpuSvcVerifySummary> {
    if !verify_handover_invariants() {
        return None;
    }
    let mut hm = DisplayHandoverMachine::new();
    let mut gop = UefiGopBackend::new(1280, 800, tsc_mhz);
    let mut virtio = VirtioGpuBackend::new(0, tsc_mhz);
    let mut bochs = LinuxDrmBochsBackend::new(tsc_mhz);
    let mut synth = SyntheticGpuBackend::new(tsc_mhz);

    let mut verified = 0u8;
    let mut gop_surf = {
        let c = gop.probe().ok()?;
        if c.broker_in_data_path {
            return None;
        }
        gop.modeset(GpuMode::DEFAULT_1280X800).ok()?;
        gop.alloc_surface(GpuMode::DEFAULT_1280X800, false).ok()?
    };
    let r_gop = gop
        .present(GpuPresentReq {
            surface_id: gop_surf.surface_id,
            damage: DamageRect::full(1280, 800),
            submit_tsc: rdtsc_user(),
        })
        .ok()?;
    if r_gop.fence_seq == 0 || gop.wait_fence(r_gop.fence_seq, 50_000).is_err() {
        return None;
    }
    verified += 1;

    let r_virt = exercise_backend(&mut virtio, &mut hm, GpuMode::DEFAULT_1280X800, false).ok()?;
    if r_virt.fence_seq != 1 {
        return None;
    }
    verified += 1;

    let mode_800 = GpuMode {
        width: 800,
        height: 600,
        ..GpuMode::DEFAULT_1280X800
    };
    let r_bochs = exercise_backend(&mut bochs, &mut hm, mode_800, false).ok()?;
    if r_bochs.fence_seq != 1 {
        return None;
    }
    verified += 1;

    // 4th backend: SyntheticGpu 1080p triple-buffered + injected fault -> GOP fallback -> recovery -> re-present.
    synth.probe().ok()?;
    synth.modeset(GpuMode::MODE_1080P).ok()?;
    let synth_surf = synth.alloc_surface(GpuMode::MODE_1080P, true).ok()?;
    hm.enter_fault_and_fallback();
    hm.promote_to_kms(GpuBackendKind::SyntheticGpu).ok()?;
    let r1 = synth
        .present(GpuPresentReq {
            surface_id: synth_surf.surface_id,
            damage: DamageRect::full(1920, 1080),
            submit_tsc: rdtsc_user(),
        })
        .ok()?;
    synth.wait_fence(r1.fence_seq, 50_000).ok()?;

    // Fault present on SyntheticGpu -> must fail and trigger fail-safe GOP rollback.
    synth.inject_fault_on_next_present = true;
    if synth
        .present(GpuPresentReq {
            surface_id: synth_surf.surface_id,
            damage: DamageRect::full(1920, 1080),
            submit_tsc: rdtsc_user(),
        })
        .is_ok()
    {
        return None;
    }
    hm.enter_fault_and_fallback();
    if hm.state != DisplayHandoverState::FallbackGop || hm.active_backend != GpuBackendKind::UefiGop {
        return None;
    }
    // Backup GOP scanout remains healthy during fallback.
    gop_surf = gop.alloc_surface(GpuMode::DEFAULT_1280X800, false).ok()?;
    let _ = gop
        .present(GpuPresentReq {
            surface_id: gop_surf.surface_id,
            damage: DamageRect::full(1280, 800),
            submit_tsc: rdtsc_user(),
        })
        .ok()?;

    // Recover SyntheticGpu cleanly and promote back to KmsActive.
    synth.quiesce_recover().ok()?;
    let synth_surf2 = synth.alloc_surface(GpuMode::MODE_1080P, true).ok()?;
    hm.promote_to_kms(GpuBackendKind::SyntheticGpu).ok()?;
    let r2 = synth
        .present(GpuPresentReq {
            surface_id: synth_surf2.surface_id,
            damage: DamageRect::full(1920, 1080),
            submit_tsc: rdtsc_user(),
        })
        .ok()?;
    synth.wait_fence(r2.fence_seq, 50_000).ok()?;
    if synth.quiesce_count != 1 || r2.fence_seq != 1 {
        return None;
    }
    verified += 1;

    let any_broker_in_data = gop.contract().broker_in_data_path
        || virtio.contract().broker_in_data_path
        || bochs.contract().broker_in_data_path
        || synth.contract().broker_in_data_path;

    Some(GpuSvcVerifySummary {
        backends_verified: verified,
        handover_states_mask: hm.visited_mask,
        synth_presents: 2,
        rollbacks_to_gop: hm.rollbacks_to_gop,
        broker_in_data_path: any_broker_in_data,
    })
}
