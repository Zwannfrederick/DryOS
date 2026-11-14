// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! ACPI `DMAR` (`DMA Remapping Reporting`) parser and bounded validation (`Milestone 2C-2`).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::pmm::{query_frame_state, FrameState, PAGE_SIZE};
use crate::serial_println;
use crate::vspace::pte::cpu_max_phys_addr_limit;

pub const MAX_DRHD_UNITS: usize = 4;
pub const MAX_RMRR_RANGES: usize = 8;
pub const MAX_DEV_SCOPES: usize = 16;

const DMAR_HEADER_LEN: usize = 48;
const DMAR_STRUCT_DRHD: u16 = 0;
const DMAR_STRUCT_RMRR: u16 = 1;
const DRHD_FLAG_INCLUDE_PCI_ALL: u8 = 1 << 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmarError {
    TooShort,
    BadSignature,
    BadLength,
    BadChecksum,
    InvalidHaw,
    MalformedStructure,
    InvalidScope,
    InvalidRange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DmarDevScope {
    pub valid: bool,
    pub scope_type: u8,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl DmarDevScope {
    pub const fn empty() -> Self {
        Self {
            valid: false,
            scope_type: 0,
            bus: 0,
            dev: 0,
            func: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DrhdUnit {
    pub valid: bool,
    pub include_all: bool,
    pub flags: u8,
    pub segment: u16,
    pub register_base_phys: u64,
    pub mapped_pages: u8,
    pub scope_count: usize,
    pub scopes: [DmarDevScope; MAX_DEV_SCOPES],
}

impl DrhdUnit {
    pub const fn empty() -> Self {
        Self {
            valid: false,
            include_all: false,
            flags: 0,
            segment: 0,
            register_base_phys: 0,
            mapped_pages: 0,
            scope_count: 0,
            scopes: [DmarDevScope::empty(); MAX_DEV_SCOPES],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RmrrRegion {
    pub valid: bool,
    pub segment: u16,
    pub base_addr: u64,
    pub limit_addr: u64,
    pub scope_count: usize,
    pub scopes: [DmarDevScope; MAX_DEV_SCOPES],
}

impl RmrrRegion {
    pub const fn empty() -> Self {
        Self {
            valid: false,
            segment: 0,
            base_addr: 0,
            limit_addr: 0,
            scope_count: 0,
            scopes: [DmarDevScope::empty(); MAX_DEV_SCOPES],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ParsedDmar {
    pub haw: u8,
    pub flags: u8,
    pub drhd_count: usize,
    pub drhds: [DrhdUnit; MAX_DRHD_UNITS],
    pub rmrr_count: usize,
    pub rmrrs: [RmrrRegion; MAX_RMRR_RANGES],
}

impl ParsedDmar {
    pub const fn empty() -> Self {
        Self {
            haw: 0,
            flags: 0,
            drhd_count: 0,
            drhds: [DrhdUnit::empty(); MAX_DRHD_UNITS],
            rmrr_count: 0,
            rmrrs: [RmrrRegion::empty(); MAX_RMRR_RANGES],
        }
    }
}

struct DmarLatchBox(UnsafeCell<ParsedDmar>);
// SAFETY: Written once during single-threaded BSP boot before APs start or `DMAR_LATCHED` is released.
unsafe impl Sync for DmarLatchBox {}

static DMAR_LATCHED: AtomicBool = AtomicBool::new(false);
static DMAR_MALFORMED: AtomicBool = AtomicBool::new(false);
static DMAR_DATA: DmarLatchBox = DmarLatchBox(UnsafeCell::new(ParsedDmar::empty()));

#[inline]
fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline]
fn read_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

fn parse_device_scopes(
    bytes: &[u8],
    resolve_live_bridges: bool,
    out: &mut [DmarDevScope; MAX_DEV_SCOPES],
) -> Result<usize, DmarError> {
    let mut off = 0usize;
    let mut count = 0usize;
    while off < bytes.len() {
        if off + 6 > bytes.len() {
            return Err(DmarError::InvalidScope);
        }
        let scope_type = bytes[off];
        let scope_len = bytes[off + 1] as usize;
        if scope_len < 8 || (scope_len & 1) != 0 || off + scope_len > bytes.len() {
            return Err(DmarError::InvalidScope);
        }
        let start_bus = bytes[off + 5];
        let mut cur_bus = start_bus;
        let path_bytes = &bytes[off + 6..off + scope_len];
        let hops = path_bytes.len() / 2;
        for h in 0..(hops - 1) {
            let b_dev = path_bytes[h * 2] & 0x1f;
            let b_func = path_bytes[h * 2 + 1] & 0x07;
            if resolve_live_bridges {
                cur_bus = crate::pci::pci_read_u8(cur_bus, b_dev, b_func, 0x19);
            }
        }
        let leaf_dev = path_bytes[(hops - 1) * 2];
        let leaf_func = path_bytes[(hops - 1) * 2 + 1];
        if leaf_dev > 31 || leaf_func > 7 {
            return Err(DmarError::InvalidScope);
        }
        if count < MAX_DEV_SCOPES {
            out[count] = DmarDevScope {
                valid: true,
                scope_type,
                bus: cur_bus,
                dev: leaf_dev,
                func: leaf_func,
            };
            count += 1;
        }
        off += scope_len;
    }
    Ok(count)
}

/// Parse an ACPI `DMAR` table slice with full length, checksum, structure, and scope checks.
#[inline(never)]
pub fn parse_dmar_bytes(bytes: &[u8], resolve_live_bridges: bool) -> Result<ParsedDmar, DmarError> {
    if bytes.len() < DMAR_HEADER_LEN {
        return Err(DmarError::TooShort);
    }
    if &bytes[0..4] != b"DMAR" {
        return Err(DmarError::BadSignature);
    }
    let length = read_u32(bytes, 4) as usize;
    if length < DMAR_HEADER_LEN || length > bytes.len() {
        return Err(DmarError::BadLength);
    }
    let csum = bytes[..length].iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
    if csum != 0 {
        return Err(DmarError::BadChecksum);
    }
    let haw = bytes[36].saturating_add(1);
    if !(32..=57).contains(&haw) {
        return Err(DmarError::InvalidHaw);
    }

    let mut parsed = ParsedDmar::empty();
    parsed.haw = haw;
    parsed.flags = bytes[37];

    let mut off = DMAR_HEADER_LEN;
    while off < length {
        if off + 4 > length {
            return Err(DmarError::MalformedStructure);
        }
        let stype = read_u16(bytes, off);
        let slen = read_u16(bytes, off + 2) as usize;
        if slen < 4 || off + slen > length {
            return Err(DmarError::MalformedStructure);
        }
        let body = &bytes[off..off + slen];
        match stype {
            DMAR_STRUCT_DRHD => {
                if slen < 16 {
                    return Err(DmarError::MalformedStructure);
                }
                let reg_base = read_u64(body, 8);
                if reg_base == 0 || (reg_base & ((PAGE_SIZE as u64) - 1)) != 0 {
                    return Err(DmarError::InvalidRange);
                }
                if parsed.drhd_count < MAX_DRHD_UNITS {
                    let mut unit = DrhdUnit::empty();
                    unit.valid = true;
                    unit.flags = body[4];
                    unit.include_all = (unit.flags & DRHD_FLAG_INCLUDE_PCI_ALL) != 0;
                    unit.segment = read_u16(body, 6);
                    unit.register_base_phys = reg_base;
                    unit.scope_count =
                        parse_device_scopes(&body[16..], resolve_live_bridges, &mut unit.scopes)?;
                    parsed.drhds[parsed.drhd_count] = unit;
                    parsed.drhd_count += 1;
                }
            }
            DMAR_STRUCT_RMRR => {
                if slen < 24 {
                    return Err(DmarError::MalformedStructure);
                }
                let base_addr = read_u64(body, 8);
                let limit_addr = read_u64(body, 16);
                if limit_addr < base_addr {
                    return Err(DmarError::InvalidRange);
                }
                if parsed.rmrr_count < MAX_RMRR_RANGES {
                    let mut rmrr = RmrrRegion::empty();
                    rmrr.valid = true;
                    rmrr.segment = read_u16(body, 6);
                    rmrr.base_addr = base_addr;
                    rmrr.limit_addr = limit_addr;
                    rmrr.scope_count =
                        parse_device_scopes(&body[24..], resolve_live_bridges, &mut rmrr.scopes)?;
                    parsed.rmrrs[parsed.rmrr_count] = rmrr;
                    parsed.rmrr_count += 1;
                }
            }
            _ => {}
        }
        off += slen;
    }

    Ok(parsed)
}

/// Validate and latch the hardware ACPI `DMAR` table discovered during early boot.
#[inline(never)]
pub fn parse_and_latch_dmar(bytes: &[u8]) -> bool {
    let parsed = match parse_dmar_bytes(bytes, true) {
        Ok(p) => p,
        Err(e) => {
            DMAR_MALFORMED.store(true, Ordering::Release);
            serial_println!("[DRY-IOMMU] DMAR_MALFORMED err={:?}", e);
            return false;
        }
    };
    if parsed.drhd_count == 0 {
        DMAR_MALFORMED.store(true, Ordering::Release);
        serial_println!("[DRY-IOMMU] DMAR_MALFORMED err=NoDrhdUnits");
        return false;
    }

    let max_pa = cpu_max_phys_addr_limit();
    for u in 0..parsed.drhd_count {
        let base = parsed.drhds[u].register_base_phys;
        if base >= max_pa || query_frame_state(base) != FrameState::ReservedOrMmio {
            DMAR_MALFORMED.store(true, Ordering::Release);
            serial_println!(
                "[DRY-IOMMU] DMAR_MALFORMED err=DrhdNotMmio unit={} base={:#x}",
                u,
                base
            );
            return false;
        }
    }

    // SAFETY: Single-threaded BSP boot before AP startup; published via `DMAR_LATCHED`.
    unsafe {
        *DMAR_DATA.0.get() = parsed;
    }
    DMAR_LATCHED.store(true, Ordering::Release);
    let first_base = parsed.drhds[0].register_base_phys;
    serial_println!(
        "[DRY-ACPI] DMAR latched: haw={} flags={:#x} drhds={} rmrrs={} drhd0={:#x}",
        parsed.haw,
        parsed.flags,
        parsed.drhd_count,
        parsed.rmrr_count,
        first_base
    );
    true
}

#[inline]
pub fn is_dmar_present_or_malformed() -> (bool, bool) {
    (
        DMAR_LATCHED.load(Ordering::Acquire),
        DMAR_MALFORMED.load(Ordering::Acquire),
    )
}

pub fn dmar_snapshot() -> Option<&'static ParsedDmar> {
    if !DMAR_LATCHED.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: Immutable after `DMAR_LATCHED` is published with `Release` and `set_drhd_mapped_pages` completes on BSP.
    Some(unsafe { &*DMAR_DATA.0.get() })
}

/// Update `mapped_pages` on a latched `DRHD` unit during `init_and_activate_kernel_vspace`.
pub(crate) fn set_drhd_mapped_pages(unit_idx: usize, pages: u8) {
    if !DMAR_LATCHED.load(Ordering::Acquire) || unit_idx >= MAX_DRHD_UNITS {
        return;
    }
    // SAFETY: Called only on BSP during single-threaded `init_and_activate_kernel_vspace`.
    unsafe {
        (*DMAR_DATA.0.get()).drhds[unit_idx].mapped_pages = pages;
    }
}
