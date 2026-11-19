// SPDX-License-Identifier: Apache-2.0 OR MIT
//! ACPI `MCFG`, PCIe `ECAM` (4 KiB extended config space), and extended capability walker
//! (`Milestone 1.3`).

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::pmm::{query_frame_state, FrameState};
use crate::serial_println;
use crate::vspace::pte::pci_ecam_vaddr;

pub const MAX_MCFG_ALLOCS: usize = 8;

#[derive(Clone, Copy)]
pub struct McfgAlloc {
    pub valid: bool,
    pub base_addr: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

impl McfgAlloc {
    pub const fn empty() -> Self {
        Self {
            valid: false,
            base_addr: 0,
            segment: 0,
            start_bus: 0,
            end_bus: 0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct McfgSummary {
    pub present: bool,
    pub alloc_count: usize,
    pub allocs: [McfgAlloc; MAX_MCFG_ALLOCS],
    pub max_bus: u8,
}

impl McfgSummary {
    pub const fn empty() -> Self {
        Self {
            present: false,
            alloc_count: 0,
            allocs: [McfgAlloc::empty(); MAX_MCFG_ALLOCS],
            max_bus: 0,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct PcieExtCaps {
    pub pcie_cap_off: u8,
    pub has_flr: bool,
    pub aer_cap_off: u16,
    pub acs_cap_off: u16,
    pub sriov_cap_off: u16,
    pub rebar_cap_off: u16,
    pub rebar_bar_mask: u8,
    pub ecam_phys: u64,
}

impl PcieExtCaps {
    pub const EMPTY: Self = Self {
        pcie_cap_off: 0,
        has_flr: false,
        aer_cap_off: 0,
        acs_cap_off: 0,
        sriov_cap_off: 0,
        rebar_cap_off: 0,
        rebar_bar_mask: 0,
        ecam_phys: 0,
    };
}

struct SyncMcfgCell(UnsafeCell<McfgSummary>);
unsafe impl Sync for SyncMcfgCell {}

static MCFG_LATCHED: SyncMcfgCell = SyncMcfgCell(UnsafeCell::new(McfgSummary::empty()));
static MCFG_READY: AtomicBool = AtomicBool::new(false);
static KERNEL_ECAM_READY: AtomicBool = AtomicBool::new(false);

#[inline]
fn read_u16_le(bytes: &[u8], off: usize) -> Option<u16> {
    let s = bytes.get(off..off + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

#[inline]
fn read_u32_le(bytes: &[u8], off: usize) -> Option<u32> {
    let s = bytes.get(off..off + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[inline]
fn read_u64_le(bytes: &[u8], off: usize) -> Option<u64> {
    let s = bytes.get(off..off + 8)?;
    Some(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

/// Pure, allocation-free ACPI `MCFG` parser with checksum, length, and alignment checks.
pub fn parse_mcfg_bytes(bytes: &[u8]) -> Result<McfgSummary, ()> {
    if bytes.len() < 44 || bytes.get(0..4) != Some(b"MCFG") {
        return Err(());
    }
    let total_len = read_u32_le(bytes, 4).ok_or(())? as usize;
    if total_len < 60 || total_len > bytes.len() || !(total_len - 44).is_multiple_of(16) {
        return Err(());
    }
    let table = &bytes[..total_len];
    let sum = table.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if sum != 0 {
        return Err(());
    }

    let mut summary = McfgSummary::empty();
    summary.present = true;
    let mut off = 44usize;
    while off + 16 <= total_len {
        let base_addr = read_u64_le(table, off).ok_or(())?;
        let segment = read_u16_le(table, off + 8).ok_or(())?;
        let start_bus = table[off + 10];
        let end_bus = table[off + 11];
        if base_addr == 0 || (base_addr & 0xfff) != 0 || start_bus > end_bus {
            return Err(());
        }
        if summary.alloc_count < MAX_MCFG_ALLOCS {
            summary.allocs[summary.alloc_count] = McfgAlloc {
                valid: true,
                base_addr,
                segment,
                start_bus,
                end_bus,
            };
            summary.alloc_count += 1;
            if segment == 0 && end_bus > summary.max_bus {
                summary.max_bus = end_bus;
            }
        }
        off += 16;
    }
    if summary.alloc_count == 0 {
        return Err(());
    }
    Ok(summary)
}

pub fn parse_and_latch_mcfg(bytes: &[u8]) -> Result<(), ()> {
    let summary = parse_mcfg_bytes(bytes)?;
    // SAFETY: Single-threaded BSP ACPI discovery prior to SMP bring-up and PCI scan.
    unsafe { *MCFG_LATCHED.0.get() = summary };
    MCFG_READY.store(true, Ordering::Release);
    Ok(())
}

pub fn mcfg_max_bus() -> u8 {
    if !MCFG_READY.load(Ordering::Acquire) {
        return 15;
    }
    // SAFETY: `MCFG_LATCHED` is immutable once `MCFG_READY = true`.
    let sum = unsafe { &*MCFG_LATCHED.0.get() };
    sum.max_bus.max(15)
}

pub fn mcfg_alloc_count() -> usize {
    if !MCFG_READY.load(Ordering::Acquire) {
        return 0;
    }
    // SAFETY: `MCFG_LATCHED` is immutable once `MCFG_READY = true`.
    unsafe { (&*MCFG_LATCHED.0.get()).alloc_count }
}

/// Resolve the `4 KiB` physical ECAM config page for `(seg, bus, dev, func)` if covered by `MCFG`.
pub fn ecam_page_phys(seg: u16, bus: u8, dev: u8, func: u8) -> Option<u64> {
    if !MCFG_READY.load(Ordering::Acquire) || dev >= 32 || func >= 8 {
        return None;
    }
    // SAFETY: `MCFG_LATCHED` is immutable once `MCFG_READY = true`.
    let sum = unsafe { &*MCFG_LATCHED.0.get() };
    for a in sum.allocs.iter().take(sum.alloc_count) {
        if a.valid && a.segment == seg && bus >= a.start_bus && bus <= a.end_bus {
            let bus_off = ((bus - a.start_bus) as u64) << 20;
            let dev_off = ((dev & 0x1f) as u64) << 15;
            let fn_off = ((func & 0x07) as u64) << 12;
            let pa = a.base_addr.checked_add(bus_off | dev_off | fn_off)?;
            if query_frame_state(pa) == FrameState::ReservedOrMmio {
                return Some(pa);
            }
            return None;
        }
    }
    None
}

pub fn mark_kernel_ecam_ready() {
    KERNEL_ECAM_READY.store(true, Ordering::Release);
}

#[inline]
pub fn is_kernel_ecam_ready() -> bool {
    KERNEL_ECAM_READY.load(Ordering::Acquire)
}

#[inline]
unsafe fn mmio_read_width(base: *const u8, off: u16, width: u8) -> u32 {
    // SAFETY: Caller verified natural alignment & 4 KiB ECAM bounds for `base + off`.
    unsafe {
        let p = base.add(off as usize);
        match width {
            1 => read_volatile(p) as u32,
            2 => read_volatile(p.cast::<u16>()) as u32,
            4 => read_volatile(p.cast::<u32>()),
            _ => 0xffff_ffff,
        }
    }
}

#[inline]
unsafe fn mmio_write_width(base: *mut u8, off: u16, width: u8, val: u32) {
    // SAFETY: Caller verified natural alignment & 4 KiB ECAM bounds for `base + off`.
    unsafe {
        let p = base.add(off as usize);
        match width {
            1 => write_volatile(p, val as u8),
            2 => write_volatile(p.cast::<u16>(), val as u16),
            4 => write_volatile(p.cast::<u32>(), val),
            _ => {}
        }
    }
}

/// Read `1`, `2`, or `4` bytes from ECAM during single-threaded early boot before `KERNEL_VSPACE`.
pub(crate) unsafe fn early_ecam_read(bus: u8, dev: u8, func: u8, off: u16, width: u8) -> Option<u32> {
    if is_kernel_ecam_ready() || (off as usize) + (width as usize) > 4096 {
        return None;
    }
    let pa = ecam_page_phys(0, bus, dev, func)?;
    let va = unsafe { crate::acpi::map_acpi_scratch_uc(pa) }?;
    Some(unsafe { mmio_read_width(va as *const u8, off, width) })
}

/// Write `1`, `2`, or `4` bytes to ECAM during single-threaded early boot before `KERNEL_VSPACE`.
pub(crate) unsafe fn early_ecam_write(
    bus: u8,
    dev: u8,
    func: u8,
    off: u16,
    width: u8,
    val: u32,
) -> bool {
    if is_kernel_ecam_ready() || (off as usize) + (width as usize) > 4096 {
        return false;
    }
    let Some(pa) = ecam_page_phys(0, bus, dev, func) else {
        return false;
    };
    let Some(va) = (unsafe { crate::acpi::map_acpi_scratch_uc(pa) }) else {
        return false;
    };
    unsafe { mmio_write_width(va, off, width, val) };
    true
}

/// Runtime access via `KERNEL_VSPACE` mapped `PML4[385]` ECAM page for `slot_idx`.
pub(crate) fn runtime_slot_ecam_rw(
    slot_idx: usize,
    has_ecam: bool,
    off: u16,
    width: u8,
    is_write: bool,
    write_val: u32,
) -> Option<u32> {
    if !is_kernel_ecam_ready() || !has_ecam || (off as usize) + (width as usize) > 4096 {
        return None;
    }
    let base = pci_ecam_vaddr(slot_idx);
    // SAFETY: `KERNEL_VSPACE` mapped this slot's 4 KiB UC ECAM page when `has_ecam` is true.
    unsafe {
        if is_write {
            mmio_write_width(base as *mut u8, off, width, write_val);
            Some(0)
        } else {
            Some(mmio_read_width(base as *const u8, off, width))
        }
    }
}

/// Walk standard PCIe cap (`0x10`, FLR bit 28) and `0x100..0xFFF` ECAM extended capabilities
/// (`AER`, `ACS`, `SR-IOV`, `Resizable BAR`) on BSP early boot.
pub(crate) fn probe_pcie_and_ext_caps(
    bus: u8,
    dev: u8,
    func: u8,
    pcie_cap_off: u8,
) -> PcieExtCaps {
    let mut caps = PcieExtCaps {
        pcie_cap_off,
        ecam_phys: ecam_page_phys(0, bus, dev, func).unwrap_or(0),
        ..PcieExtCaps::default()
    };

    if pcie_cap_off >= 0x40 && (pcie_cap_off as usize) + 8 <= 256 {
        let dev_cap = crate::pci::pci_read_u32(bus, dev, func, pcie_cap_off.wrapping_add(4));
        caps.has_flr = (dev_cap & (1 << 28)) != 0;
    }

    if caps.ecam_phys == 0 {
        return caps;
    }

    // Walk PCIe Extended Capability linked list (`0x100..0xFFC`).
    let mut off = 0x100u16;
    let mut visited = [0u16; 48];
    let mut vis_len = 0usize;
    while (0x100..=0xffc).contains(&off) && (off & 0x3) == 0 && vis_len < visited.len() {
        if visited[..vis_len].contains(&off) {
            break;
        }
        visited[vis_len] = off;
        vis_len += 1;

        let Some(hdr) = crate::pci::pci_ecam_read_u32(bus, dev, func, off) else {
            break;
        };
        if hdr == 0 || hdr == 0xffff_ffff {
            break;
        }
        let ext_id = (hdr & 0xffff) as u16;
        let next = ((hdr >> 20) & 0xfff) as u16;
        match ext_id {
            0x0001 if caps.aer_cap_off == 0 => caps.aer_cap_off = off,
            0x000d if caps.acs_cap_off == 0 => caps.acs_cap_off = off,
            0x0010 if caps.sriov_cap_off == 0 => caps.sriov_cap_off = off,
            0x0015 if caps.rebar_cap_off == 0 => {
                caps.rebar_cap_off = off;
                if let Some(ctrl) = crate::pci::pci_ecam_read_u32(bus, dev, func, off + 8) {
                    let nbars = ((ctrl >> 5) & 0x7).min(6) as u8;
                    let mut mask = 0u8;
                    for b in 0..nbars {
                        let bar_reg = off + 4 + (b as u16) * 8;
                        if let Some(cap_dw) = crate::pci::pci_ecam_read_u32(bus, dev, func, bar_reg) {
                            if (cap_dw >> 4) != 0 {
                                mask |= 1 << b;
                            }
                        }
                    }
                    caps.rebar_bar_mask = mask;
                }
            }
            _ => {}
        }
        if next < 0x100 || next == off {
            break;
        }
        off = next;
    }

    caps
}

/// Deterministic kernel unit tests for ACPI `MCFG` parsing and validation bounds.
#[inline(never)]
pub fn run_mcfg_unit_tests() {
    let mut valid = [0u8; 60];
    valid[0..4].copy_from_slice(b"MCFG");
    valid[4..8].copy_from_slice(&60u32.to_le_bytes());
    valid[8] = 1;
    valid[44..52].copy_from_slice(&0x0000_0000_b000_0000u64.to_le_bytes());
    valid[52..54].copy_from_slice(&0u16.to_le_bytes());
    valid[54] = 0; // start_bus
    valid[55] = 255; // end_bus
    let sum = valid.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    valid[9] = 0u8.wrapping_sub(sum);

    let ok = parse_mcfg_bytes(&valid).expect("valid MCFG must parse");
    assert!(ok.present && ok.alloc_count == 1 && ok.max_bus == 255);
    assert_eq!(ok.allocs[0].base_addr, 0x0000_0000_b000_0000);
    assert_eq!(ok.allocs[0].segment, 0);
    assert_eq!(ok.allocs[0].start_bus, 0);
    assert_eq!(ok.allocs[0].end_bus, 255);

    let mut bad_csum = valid;
    bad_csum[9] ^= 0xff;
    assert!(parse_mcfg_bytes(&bad_csum).is_err());

    let mut bad_len = valid;
    bad_len[4..8].copy_from_slice(&52u32.to_le_bytes());
    let s2 = bad_len[..52].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    bad_len[9] = bad_len[9].wrapping_sub(s2);
    assert!(parse_mcfg_bytes(&bad_len).is_err());

    let mut bad_align = valid;
    bad_align[44..52].copy_from_slice(&0x0000_0000_b000_0200u64.to_le_bytes());
    bad_align[9] = 0;
    let s3 = bad_align.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    bad_align[9] = 0u8.wrapping_sub(s3);
    assert!(parse_mcfg_bytes(&bad_align).is_err());

    let mut bad_bus = valid;
    bad_bus[54] = 32;
    bad_bus[55] = 16;
    bad_bus[9] = 0;
    let s4 = bad_bus.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    bad_bus[9] = 0u8.wrapping_sub(s4);
    assert!(parse_mcfg_bytes(&bad_bus).is_err());

    serial_println!("[DRY-PCI] PASS: mcfg_unit=5 allocs={} max_bus={}", mcfg_alloc_count(), mcfg_max_bus());
}
