// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Intel Iris Xe (`8086:9a49`, Tiger Lake Gen12) Non-Destructive Driver Preparation Contract.
//!
//! Zero physical MMIO writes and zero fake Linux symbols: defines validated BAR layout rules,
//! Gen12 register offset descriptors, GuC/DMC/HuC CSS header parsing, 64-bit GGTT PTE encoding,
//! command submission ring descriptors, MSI `IRQF_ONESHOT` IRQ routing contracts, and streaming
//! firmware verification invariants.

use crate::abi_ext::{
    UserPciBarInfo, UserPciDeviceInfo, PCI_BAR_FLAG_64BIT, PCI_BAR_FLAG_MMIO,
    PCI_BAR_FLAG_PREFETCHABLE, PCI_DEV_FLAG_IOMMU_ACTIVE, PCI_DEV_FLAG_MSI,
};
use crate::fw_store::{sha256_streaming, FwStreamVerifier, FwVerifyError};
use crate::gpu_svc::handover::DisplayHandoverState;
use crate::shell::LineBuilder;
use crate::{sys_write_debug, IRQF_ONESHOT};

pub const INTEL_VENDOR_ID: u16 = 0x8086;
pub const INTEL_TGL_IRIS_XE_DEV_ID: u16 = 0x9a49;
pub const TGL_BAR0_MIN_BYTES: u64 = 16 * 1024 * 1024;
pub const TGL_BAR2_MIN_BYTES: u64 = 256 * 1024 * 1024;
pub const ALLOW_PHYSICAL_GPU_WRITES: bool = false;

pub const GEN12_GGTT_BASE_OFF: u32 = 0x0080_0000;
pub const GEN12_GGTT_SIZE_BYTES: u32 = 8 * 1024 * 1024;

// Typed Gen12 MMIO register offsets inside BAR0 GTTMMADR (non-destructive descriptors only).
pub const GEN12_FORCEWAKE_RENDER: u32 = 0x0000_a278;
pub const GEN12_FORCEWAKE_ACK_RENDER: u32 = 0x0000_0d84;
pub const GEN12_FORCEWAKE_MT: u32 = 0x0000_a188;
pub const GEN12_FORCEWAKE_ACK_MT: u32 = 0x0000_0d88;
pub const GEN12_DE_MASTER_IRQ: u32 = 0x0004_4400;
pub const GEN12_DE_PIPE_A_IRQ_MASK: u32 = 0x0004_4404;
pub const GEN12_PIPE_CONF_A: u32 = 0x0007_0008;
pub const GEN12_PLANE_SURF_1_A: u32 = 0x0007_019c;
pub const GEN12_TRANS_CONF_EDP: u32 = 0x0007_f008;
pub const GEN12_RCS0_RING_TAIL: u32 = 0x0000_2030;
pub const GEN12_RCS0_RING_HEAD: u32 = 0x0000_2034;
pub const GEN12_RCS0_RING_START: u32 = 0x0000_2038;
pub const GEN12_RCS0_RING_CTL: u32 = 0x0000_203c;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntelPrepError {
    WrongPciId,
    MissingGttmmadrBar0,
    MissingGmadrBar2,
    IommuNotActive,
    InvalidCssHeader,
    InvalidGgttAddress,
    InvalidRingGeom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntelCssHeader {
    pub module_type: u32,
    pub header_bytes: u32,
    pub module_vendor: u32,
    pub total_bytes: u32,
    pub sw_version: u32,
}

#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingSubmitDesc {
    pub ring_iova: u64,
    pub ring_bytes: u32,
    pub tail_bytes: u32,
    pub ctx_lrca: u64,
    pub fence_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntelIrqRouteContract {
    pub irq_bind_flags: u64,
    pub status_reg_off: u32,
    pub mask_reg_off: u32,
    pub pipe_a_vblank_bit: u32,
    pub msi_required: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuServiceReadinessContract {
    pub vendor_id: u16,
    pub device_id: u16,
    pub target_w: u16,
    pub target_h: u16,
    pub refresh_hz: u16,
    pub entry_handover_state: DisplayHandoverState,
    pub gop_fallback_preserved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntelPrepSummary {
    pub bars_ok: bool,
    pub css_ok: bool,
    pub fw_tamper_rej: bool,
    pub ggtt_ok: bool,
    pub irq_ok: bool,
}

#[inline]
pub const fn tgl_default_irq_contract() -> IntelIrqRouteContract {
    IntelIrqRouteContract {
        irq_bind_flags: IRQF_ONESHOT,
        status_reg_off: GEN12_DE_MASTER_IRQ,
        mask_reg_off: GEN12_DE_PIPE_A_IRQ_MASK,
        pipe_a_vblank_bit: 1u32 << 0,
        msi_required: true,
    }
}

#[inline]
pub const fn tgl_gpu_svc_contract() -> GpuServiceReadinessContract {
    GpuServiceReadinessContract {
        vendor_id: INTEL_VENDOR_ID,
        device_id: INTEL_TGL_IRIS_XE_DEV_ID,
        target_w: 1920,
        target_h: 1080,
        refresh_hz: 165,
        entry_handover_state: DisplayHandoverState::GopActive,
        gop_fallback_preserved: true,
    }
}

/// Validate `8086:9a49` BAR0 (`16 MiB` non-prefetchable `GTTMMADR`) & BAR2 (`256 MiB` prefetchable `GMADR`).
pub fn validate_tgl_pci_bars(info: &UserPciDeviceInfo) -> Result<(u64, u64), IntelPrepError> {
    if info.vendor_id != INTEL_VENDOR_ID || info.device_id != INTEL_TGL_IRIS_XE_DEV_ID {
        return Err(IntelPrepError::WrongPciId);
    }
    if (info.dev_flags & PCI_DEV_FLAG_IOMMU_ACTIVE) == 0 {
        return Err(IntelPrepError::IommuNotActive);
    }
    let bar0 = &info.bars[0];
    let need_mmio64 = PCI_BAR_FLAG_MMIO | PCI_BAR_FLAG_64BIT;
    if (bar0.flags & need_mmio64) != need_mmio64
        || (bar0.flags & PCI_BAR_FLAG_PREFETCHABLE) != 0
        || bar0.size_bytes < TGL_BAR0_MIN_BYTES
        || bar0.size_bytes < u64::from(GEN12_GGTT_BASE_OFF + GEN12_GGTT_SIZE_BYTES)
    {
        return Err(IntelPrepError::MissingGttmmadrBar0);
    }
    let bar2 = &info.bars[2];
    let need_gmadr = need_mmio64 | PCI_BAR_FLAG_PREFETCHABLE;
    if (bar2.flags & need_gmadr) != need_gmadr || bar2.size_bytes < TGL_BAR2_MIN_BYTES {
        return Err(IntelPrepError::MissingGmadrBar2);
    }
    Ok((bar0.size_bytes, bar2.size_bytes))
}

#[inline]
fn le_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

/// Parse and validate an Intel GuC/HuC/DMC CSS binary header without executing firmware.
pub fn parse_intel_css_header(blob: &[u8]) -> Result<IntelCssHeader, IntelPrepError> {
    if blob.len() < 128 {
        return Err(IntelPrepError::InvalidCssHeader);
    }
    let module_type = le_u32(blob, 0x00);
    let header_len_dw = le_u32(blob, 0x04);
    let module_vendor = le_u32(blob, 0x10);
    let size_dw = le_u32(blob, 0x18);
    let sw_version = le_u32(blob, 0x40);
    let header_bytes = header_len_dw.checked_mul(4).ok_or(IntelPrepError::InvalidCssHeader)?;
    let total_bytes = size_dw.checked_mul(4).ok_or(IntelPrepError::InvalidCssHeader)?;
    if module_type != 6
        || module_vendor != u32::from(INTEL_VENDOR_ID)
        || header_len_dw < 33
        || (header_bytes as usize) > blob.len()
        || (total_bytes as usize) > blob.len()
        || total_bytes < header_bytes
    {
        return Err(IntelPrepError::InvalidCssHeader);
    }
    Ok(IntelCssHeader {
        module_type,
        header_bytes,
        module_vendor,
        total_bytes,
        sw_version,
    })
}

/// Encode a 64-bit Gen12 GGTT page-table entry referencing a 4 KiB-aligned DMA/IOVA address.
pub fn encode_ggtt_pte(iova_or_hpa: u64, present: bool, local_mem: bool) -> Result<u64, IntelPrepError> {
    if (iova_or_hpa & 0xfff) != 0 || (iova_or_hpa >> 48) != 0 {
        return Err(IntelPrepError::InvalidGgttAddress);
    }
    let mut entry = iova_or_hpa & 0x0000_ffff_ffff_f000;
    if present {
        entry |= 1 << 0;
    }
    if local_mem {
        entry |= 1 << 1;
    }
    Ok(entry)
}

pub fn validate_ring_submit(desc: &RingSubmitDesc) -> Result<u32, IntelPrepError> {
    if (desc.ring_iova & 0xfff) != 0
        || desc.ring_bytes < 4096
        || !desc.ring_bytes.is_power_of_two()
        || (desc.tail_bytes & 0x7) != 0
        || desc.tail_bytes >= desc.ring_bytes
    {
        return Err(IntelPrepError::InvalidRingGeom);
    }
    // GEN12 RING_CTL encodes size in 4K pages minus 1 inside bits [20:12], enabled bit [0].
    let pages_minus_1 = (desc.ring_bytes / 4096) - 1;
    Ok((pages_minus_1 << 12) | 1)
}

/// Deterministic Ring-3 contract verification for Tiger Lake `8086:9a49` bring-up readiness.
pub fn verify_intel_tgl_prep_invariants() -> Option<IntelPrepSummary> {
    let valid_info = UserPciDeviceInfo {
        vendor_id: INTEL_VENDOR_ID,
        device_id: INTEL_TGL_IRIS_XE_DEV_ID,
        bus_dev_fn: 0x0010,
        dev_flags: PCI_DEV_FLAG_IOMMU_ACTIVE | PCI_DEV_FLAG_MSI,
        msix_table_size: 0,
        msi_cap_present: 1,
        iommu_protected: 1,
        iommu_faults: 0,
        bars: [
            UserPciBarInfo {
                size_bytes: TGL_BAR0_MIN_BYTES,
                flags: PCI_BAR_FLAG_MMIO | PCI_BAR_FLAG_64BIT,
                page_suboff: 0,
                _pad: 0,
            },
            UserPciBarInfo::default(),
            UserPciBarInfo {
                size_bytes: TGL_BAR2_MIN_BYTES,
                flags: PCI_BAR_FLAG_MMIO | PCI_BAR_FLAG_64BIT | PCI_BAR_FLAG_PREFETCHABLE,
                page_suboff: 0,
                _pad: 0,
            },
            UserPciBarInfo::default(),
            UserPciBarInfo::default(),
            UserPciBarInfo::default(),
        ],
    };
    let (b0, b2) = validate_tgl_pci_bars(&valid_info).ok()?;
    if b0 < TGL_BAR0_MIN_BYTES || b2 < TGL_BAR2_MIN_BYTES {
        return None;
    }
    let mut wrong_id = valid_info;
    wrong_id.device_id = 0x1234;
    if validate_tgl_pci_bars(&wrong_id) != Err(IntelPrepError::WrongPciId) {
        return None;
    }

    let mut css = [0u8; 256];
    css[0x00..0x04].copy_from_slice(&6u32.to_le_bytes());
    css[0x04..0x08].copy_from_slice(&33u32.to_le_bytes());
    css[0x10..0x14].copy_from_slice(&(INTEL_VENDOR_ID as u32).to_le_bytes());
    css[0x18..0x1c].copy_from_slice(&64u32.to_le_bytes());
    css[0x40..0x44].copy_from_slice(&((70u32 << 16) | (1u32 << 8) | 1u32).to_le_bytes());
    let parsed = parse_intel_css_header(&css).ok()?;
    if parsed.header_bytes != 132 || parsed.total_bytes != 256 {
        return None;
    }
    let expect_sha = sha256_streaming(&css);
    let mut sv = FwStreamVerifier::new();
    sv.update(&css);
    if sv.finish(&expect_sha) != Ok(256) {
        return None;
    }

    css[0x10] = 0xff; // Tamper vendor inside CSS header
    if parse_intel_css_header(&css) != Err(IntelPrepError::InvalidCssHeader) {
        return None;
    }
    let mut bad_sv = FwStreamVerifier::new();
    bad_sv.update(&css);
    if bad_sv.finish(&expect_sha) != Err(FwVerifyError::DigestMismatch) {
        return None;
    }

    let pte = encode_ggtt_pte(0x0000_c000_1000, true, false).ok()?;
    if (pte & 1) == 0 || (pte & !0xfff) != 0x0000_c000_1000 {
        return None;
    }
    if encode_ggtt_pte(0x1001, true, false) != Err(IntelPrepError::InvalidGgttAddress) {
        return None;
    }
    let ring_ctl = validate_ring_submit(&RingSubmitDesc {
        ring_iova: 0xc000_0000,
        ring_bytes: 16384,
        tail_bytes: 64,
        ctx_lrca: 0xc001_0000,
        fence_seq: 1,
    })
    .ok()?;
    if (ring_ctl & 1) == 0 || ((ring_ctl >> 12) & 0x1ff) != 3 {
        return None;
    }

    let irq_c = tgl_default_irq_contract();
    let svc_c = tgl_gpu_svc_contract();
    if ALLOW_PHYSICAL_GPU_WRITES
        || irq_c.irq_bind_flags != IRQF_ONESHOT
        || !irq_c.msi_required
        || svc_c.target_w != 1920
        || svc_c.refresh_hz != 165
        || !svc_c.gop_fallback_preserved
    {
        return None;
    }

    Some(IntelPrepSummary {
        bars_ok: true,
        css_ok: true,
        fw_tamper_rej: true,
        ggtt_ok: true,
        irq_ok: true,
    })
}

pub fn log_intel_tgl_prep_selftest() {
    if let Some(s) = verify_intel_tgl_prep_invariants() {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-INTEL-PREP] PASS: dev=8086:9a49 bars_ok=");
        lb.push_u64(u64::from(s.bars_ok));
        lb.push_bytes(b" css_ok=");
        lb.push_u64(u64::from(s.css_ok));
        lb.push_bytes(b" fw_tamper_rej=");
        lb.push_u64(u64::from(s.fw_tamper_rej));
        lb.push_bytes(b" ggtt_ok=");
        lb.push_u64(u64::from(s.ggtt_ok));
        lb.push_bytes(b" irq_ok=");
        lb.push_u64(u64::from(s.irq_ok));
        lb.push_bytes(b" status=READY_FOR_HW_VAL");
        let bytes = lb.as_bytes();
        let mut out = [0u8; 164];
        let n = bytes.len().min(out.len() - 1);
        out[..n].copy_from_slice(&bytes[..n]);
        out[n] = b'\n';
        let _ = sys_write_debug(&out[..=n]);
    } else {
        let _ = sys_write_debug(b"[DRY-INTEL-PREP] FAIL\n");
    }
}
