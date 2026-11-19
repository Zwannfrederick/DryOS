// SPDX-License-Identifier: Apache-2.0 OR MIT
//! ACPI `IVRS` (`I/O Virtualization Reporting Structure`) parser and validation (`Milestone 1.2`).
//!
//! Safely extracts `IVHD` AMD-Vi IOMMU hardware units and `IVMD` reserved memory blocks with full
//! length/checksum/alignment validation. Never assumes AMD-Vi translation works merely because an
//! AMD CPU or `IVRS` table exists.

use core::arch::x86_64::__cpuid;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

pub const MAX_IVHD_UNITS: usize = 8;
pub const MAX_IVMD_RANGES: usize = 16;

#[derive(Clone, Copy)]
pub struct IvhdUnit {
    pub valid: bool,
    pub entry_type: u8,
    pub flags: u8,
    pub iommu_bdf: u16,
    pub cap_offset: u16,
    pub mmio_base: u64,
    pub segment: u16,
    pub iommu_info: u16,
    pub efr_sup: bool,
}

impl IvhdUnit {
    pub const fn empty() -> Self {
        Self {
            valid: false,
            entry_type: 0,
            flags: 0,
            iommu_bdf: 0,
            cap_offset: 0,
            mmio_base: 0,
            segment: 0,
            iommu_info: 0,
            efr_sup: false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct IvmdRange {
    pub valid: bool,
    pub entry_type: u8,
    pub flags: u8,
    pub device_id: u16,
    pub start_addr: u64,
    pub mem_len: u64,
}

impl IvmdRange {
    pub const fn empty() -> Self {
        Self {
            valid: false,
            entry_type: 0,
            flags: 0,
            device_id: 0,
            start_addr: 0,
            mem_len: 0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct IvrsSummary {
    pub present: bool,
    pub malformed: bool,
    pub checksum_ok: bool,
    pub iv_info: u32,
    pub pa_bits: u8,
    pub va_bits: u8,
    pub efr_sup: bool,
    pub amd_svm_cpu: bool,
    pub ivhd_count: usize,
    pub ivhds: [IvhdUnit; MAX_IVHD_UNITS],
    pub ivmd_count: usize,
    pub ivmds: [IvmdRange; MAX_IVMD_RANGES],
}

impl IvrsSummary {
    pub const fn empty() -> Self {
        Self {
            present: false,
            malformed: false,
            checksum_ok: false,
            iv_info: 0,
            pa_bits: 0,
            va_bits: 0,
            efr_sup: false,
            amd_svm_cpu: false,
            ivhd_count: 0,
            ivhds: [IvhdUnit::empty(); MAX_IVHD_UNITS],
            ivmd_count: 0,
            ivmds: [IvmdRange::empty(); MAX_IVMD_RANGES],
        }
    }
}

struct SyncIvrsCell(UnsafeCell<IvrsSummary>);
unsafe impl Sync for SyncIvrsCell {}

static IVRS_LATCHED: SyncIvrsCell = SyncIvrsCell(UnsafeCell::new(IvrsSummary::empty()));
static IVRS_READY: AtomicBool = AtomicBool::new(false);
static IVRS_MALFORMED: AtomicBool = AtomicBool::new(false);

#[inline]
fn read_u16_le(bytes: &[u8], off: usize) -> Option<u16> {
    let sl = bytes.get(off..off + 2)?;
    Some(u16::from_le_bytes([sl[0], sl[1]]))
}

#[inline]
fn read_u32_le(bytes: &[u8], off: usize) -> Option<u32> {
    let sl = bytes.get(off..off + 4)?;
    Some(u32::from_le_bytes([sl[0], sl[1], sl[2], sl[3]]))
}

#[inline]
fn read_u64_le(bytes: &[u8], off: usize) -> Option<u64> {
    let sl = bytes.get(off..off + 8)?;
    Some(u64::from_le_bytes([
        sl[0], sl[1], sl[2], sl[3], sl[4], sl[5], sl[6], sl[7],
    ]))
}

/// Safely probe `CPUID` to see whether this CPU reports AMD vendor + SVM/ext features.
pub fn cpu_has_amd_svm() -> bool {
    let l0 = __cpuid(0);
    if l0.ebx != 0x6874_7541 || l0.edx != 0x6974_6e65 || l0.ecx != 0x444d_4163 {
        return false; // Not "AuthenticAMD"
    }
    let ext0 = __cpuid(0x8000_0000);
    if ext0.eax < 0x8000_0001 {
        return false;
    }
    let ext1 = __cpuid(0x8000_0001);
    (ext1.ecx & (1 << 2)) != 0 // SVM bit
}

/// Pure, allocation-free ACPI `IVRS` parser suitable for both firmware tables and synthetic tests.
pub fn parse_ivrs_bytes(bytes: &[u8]) -> Result<IvrsSummary, ()> {
    if bytes.len() < 48 || bytes.get(0..4) != Some(b"IVRS") {
        return Err(());
    }
    let total_len = read_u32_le(bytes, 4).ok_or(())? as usize;
    if total_len < 48 || total_len > bytes.len() {
        return Err(());
    }
    let table = &bytes[..total_len];
    let sum = table.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if sum != 0 {
        return Err(());
    }

    let iv_info = read_u32_le(table, 36).ok_or(())?;
    let efr_sup = (iv_info & 0x1) != 0;
    let pa_raw = ((iv_info >> 8) & 0x7f) as u8;
    let va_raw = ((iv_info >> 15) & 0x7f) as u8;

    let mut summary = IvrsSummary::empty();
    summary.present = true;
    summary.checksum_ok = true;
    summary.iv_info = iv_info;
    summary.efr_sup = efr_sup;
    summary.pa_bits = pa_raw;
    summary.va_bits = va_raw;
    summary.amd_svm_cpu = cpu_has_amd_svm();

    let mut off = 48usize;
    while off + 4 <= total_len {
        let entry_type = table[off];
        let flags = table[off + 1];
        let entry_len = read_u16_le(table, off + 2).ok_or(())? as usize;
        if entry_len < 4 || off + entry_len > total_len {
            return Err(());
        }
        match entry_type {
            0x10 | 0x11 | 0x40 => {
                let min_len = if entry_type == 0x10 { 24 } else { 40 };
                if entry_len < min_len {
                    return Err(());
                }
                let iommu_bdf = read_u16_le(table, off + 4).ok_or(())?;
                let cap_offset = read_u16_le(table, off + 6).ok_or(())?;
                let mmio_base = read_u64_le(table, off + 8).ok_or(())?;
                let segment = read_u16_le(table, off + 16).ok_or(())?;
                let iommu_info = read_u16_le(table, off + 18).ok_or(())?;
                if mmio_base == 0 || (mmio_base & 0xfff) != 0 {
                    return Err(());
                }
                if summary.ivhd_count < MAX_IVHD_UNITS {
                    summary.ivhds[summary.ivhd_count] = IvhdUnit {
                        valid: true,
                        entry_type,
                        flags,
                        iommu_bdf,
                        cap_offset,
                        mmio_base,
                        segment,
                        iommu_info,
                        efr_sup: efr_sup && entry_type != 0x10,
                    };
                    summary.ivhd_count += 1;
                }
            }
            0x20..=0x22 => {
                if entry_len < 32 {
                    return Err(());
                }
                let device_id = read_u16_le(table, off + 4).ok_or(())?;
                let start_addr = read_u64_le(table, off + 16).ok_or(())?;
                let mem_len = read_u64_le(table, off + 24).ok_or(())?;
                if mem_len == 0 || start_addr.checked_add(mem_len).is_none() {
                    return Err(());
                }
                if summary.ivmd_count < MAX_IVMD_RANGES {
                    summary.ivmds[summary.ivmd_count] = IvmdRange {
                        valid: true,
                        entry_type,
                        flags,
                        device_id,
                        start_addr,
                        mem_len,
                    };
                    summary.ivmd_count += 1;
                }
            }
            _ => {}
        }
        off += entry_len;
    }

    if off != total_len || summary.ivhd_count == 0 {
        return Err(());
    }
    Ok(summary)
}

/// Validate and latch ACPI `IVRS` table discovered during early boot.
pub fn parse_and_latch_ivrs(bytes: &[u8]) -> Result<(), ()> {
    match parse_ivrs_bytes(bytes) {
        Ok(sum) => {
            // SAFETY: Single-threaded BSP ACPI discovery prior to SMP startup.
            unsafe { *IVRS_LATCHED.0.get() = sum };
            IVRS_MALFORMED.store(false, Ordering::Release);
            IVRS_READY.store(true, Ordering::Release);
            Ok(())
        }
        Err(()) => {
            let mut bad = IvrsSummary::empty();
            bad.present = true;
            bad.malformed = true;
            // SAFETY: Single-threaded BSP ACPI discovery prior to SMP startup.
            unsafe { *IVRS_LATCHED.0.get() = bad };
            IVRS_MALFORMED.store(true, Ordering::Release);
            IVRS_READY.store(false, Ordering::Release);
            Err(())
        }
    }
}

pub fn ivrs_snapshot() -> Option<&'static IvrsSummary> {
    if !IVRS_READY.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: Latched immutably on BSP during early boot prior to `IVRS_READY = true`.
    Some(unsafe { &*IVRS_LATCHED.0.get() })
}

pub fn is_ivrs_present_or_malformed() -> (bool, bool) {
    if IVRS_MALFORMED.load(Ordering::Acquire) {
        return (true, true);
    }
    if IVRS_READY.load(Ordering::Acquire) {
        return (true, false);
    }
    (false, false)
}
