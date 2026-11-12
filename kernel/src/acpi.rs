// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal Early-Boot ACPI `RSDP -> XSDT/RSDT -> MADT` (`APIC`) Discovery (`Milestone 2A-1` / `P1`).
//!
//! Parses firmware ACPI tables once on the BSP while Limine's HHDM is active (before
//! `BootloaderReclaimable` reclaim and `KERNEL_VSPACE` handoff), verifying all byte checksums and
//! latching the primary I/O APIC physical address, GSI base, and ISA IRQ `0..16` Interrupt Source
//! Overrides (`ISO`, polarity & trigger mode) into permanent `.bss`.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr::{read_unaligned, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::read_cr3;
use crate::limine::get_rsdp_raw_addr;

pub const ISA_IRQ_COUNT: usize = 16;

const ACPI_BOOT_WIN_BASE: u64 = 0xffff_c000_0000_0000; // PML4[384], disjoint from HHDM & kernel image
const PHYS_PAGE_MASK: u64 = 0x000f_ffff_ffff_f000;
const SLOT_RSDP_START: usize = 0; // 2 pages (8 KiB)
const SLOT_ROOT_START: usize = 2; // 16 pages (64 KiB)
const SLOT_CHILD_START: usize = 18; // 64 pages (256 KiB, MADT)
const SLOT_FADT_START: usize = 82; // 16 pages (64 KiB, FADT)
const SLOT_DSDT_START: usize = 98; // 256 pages (1 MiB, DSDT)

#[repr(C, align(4096))]
struct AcpiPtPage([u64; 512]);
struct SyncAcpiPtPage(UnsafeCell<AcpiPtPage>);
// SAFETY: Used exclusively on BSP (`CLI`) during early `discover_acpi_madt` before SMP bring-up.
unsafe impl Sync for SyncAcpiPtPage {}

static ACPI_WIN_PDPT: SyncAcpiPtPage = SyncAcpiPtPage(UnsafeCell::new(AcpiPtPage([0; 512])));
static ACPI_WIN_PD: SyncAcpiPtPage = SyncAcpiPtPage(UnsafeCell::new(AcpiPtPage([0; 512])));
static ACPI_WIN_PT: SyncAcpiPtPage = SyncAcpiPtPage(UnsafeCell::new(AcpiPtPage([0; 512])));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsaIrqRoute {
    pub gsi: u32,
    pub active_low: bool,
    pub level_triggered: bool,
    pub overridden: bool,
}

impl IsaIrqRoute {
    pub const fn identity(irq: u8) -> Self {
        Self {
            gsi: irq as u32,
            active_low: false,
            level_triggered: false,
            overridden: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AcpiMadtSummary {
    pub rsdp_revision: u8,
    pub used_xsdt: bool,
    pub ioapic_found: bool,
    pub ioapic_id: u8,
    pub ioapic_phys: u64,
    pub ioapic_gsi_base: u32,
    pub ioapic_count: u8,
    pub iso_override_count: u8,
    pub isa_routes: [IsaIrqRoute; ISA_IRQ_COUNT],
}

impl AcpiMadtSummary {
    const fn empty() -> Self {
        let mut routes = [IsaIrqRoute::identity(0); ISA_IRQ_COUNT];
        let mut i = 0usize;
        while i < ISA_IRQ_COUNT {
            routes[i] = IsaIrqRoute::identity(i as u8);
            i += 1;
        }
        Self {
            rsdp_revision: 0,
            used_xsdt: false,
            ioapic_found: false,
            ioapic_id: 0,
            ioapic_phys: 0,
            ioapic_gsi_base: 0,
            ioapic_count: 0,
            iso_override_count: 0,
            isa_routes: routes,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AcpiPowerInfo {
    pub fadt_found: bool,
    pub pm1a_cnt_port: u16,
    pub pm1b_cnt_port: u16,
    pub smi_cmd_port: u16,
    pub acpi_enable_val: u8,
    pub s5_valid: bool,
    pub slp_typ_s5_a: u8,
    pub slp_typ_s5_b: u8,
    pub reset_supported: bool,
    pub reset_reg_io_port: u16,
    pub reset_value: u8,
    pub facs_phys: u64,
    pub s3_aml_present: bool,
}

impl AcpiPowerInfo {
    pub const fn empty() -> Self {
        Self {
            fadt_found: false,
            pm1a_cnt_port: 0,
            pm1b_cnt_port: 0,
            smi_cmd_port: 0,
            acpi_enable_val: 0,
            s5_valid: false,
            slp_typ_s5_a: 0,
            slp_typ_s5_b: 0,
            reset_supported: false,
            reset_reg_io_port: 0,
            reset_value: 0,
            facs_phys: 0,
            s3_aml_present: false,
        }
    }
}

struct StaticAcpi(UnsafeCell<AcpiMadtSummary>);
// SAFETY: Written once on BSP (`CLI`) before SMP bring-up and `KERNEL_VSPACE` activation; read-only thereafter.
unsafe impl Sync for StaticAcpi {}

struct StaticAcpiPower(UnsafeCell<AcpiPowerInfo>);
// SAFETY: Written once on BSP (`CLI`) before SMP bring-up and `KERNEL_VSPACE` activation; read-only thereafter.
unsafe impl Sync for StaticAcpiPower {}

static ACPI_READY: AtomicBool = AtomicBool::new(false);
static ACPI_SUMMARY: StaticAcpi = StaticAcpi(UnsafeCell::new(AcpiMadtSummary::empty()));
static ACPI_POWER: StaticAcpiPower = StaticAcpiPower(UnsafeCell::new(AcpiPowerInfo::empty()));

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpV1 {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_phys: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpV2 {
    v1: RsdpV1,
    length: u32,
    xsdt_phys: u64,
    ext_checksum: u8,
    _reserved: [u8; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct AcpiSdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[inline]
fn to_phys_addr(hhdm_offset: u64, phys_or_virt: u64) -> u64 {
    if phys_or_virt >= hhdm_offset {
        phys_or_virt - hhdm_offset
    } else {
        phys_or_virt
    }
}

unsafe fn install_acpi_boot_window(hhdm_offset: u64, kernel_phys_base: u64, kernel_virt_base: u64) {
    let cr3_phys = read_cr3() & PHYS_PAGE_MASK;
    let i4 = ((ACPI_BOOT_WIN_BASE >> 39) & 0x1ff) as usize;
    let kphys_of = |ptr: *const AcpiPtPage| -> u64 {
        let kv = ptr as u64;
        assert!(kv >= kernel_virt_base);
        kernel_phys_base + (kv - kernel_virt_base)
    };
    // SAFETY: Single-threaded BSP (`CLI`) installing `PML4[384] -> ACPI_WIN_PDPT -> ACPI_WIN_PD -> ACPI_WIN_PT`.
    unsafe {
        let saved_cr0: u64;
        asm!("mov {}, cr0", out(reg) saved_cr0, options(nomem, nostack, preserves_flags));
        asm!("mov cr0, {}", in(reg) (saved_cr0 & !(1u64 << 16)), options(nomem, nostack, preserves_flags));

        let pml4 = (hhdm_offset + cr3_phys) as *mut u64;
        let pdpt_ptr = (*ACPI_WIN_PDPT.0.get()).0.as_mut_ptr();
        let pd_ptr = (*ACPI_WIN_PD.0.get()).0.as_mut_ptr();
        write_volatile(pdpt_ptr, kphys_of(ACPI_WIN_PD.0.get()) | 0x03);
        write_volatile(pd_ptr, kphys_of(ACPI_WIN_PT.0.get()) | 0x03);
        write_volatile(pml4.add(i4), kphys_of(ACPI_WIN_PDPT.0.get()) | 0x03);

        asm!("mov cr0, {}", in(reg) saved_cr0, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn map_acpi_window_ro(slot_base: usize, max_slots: usize, phys_addr: u64, byte_len: usize) -> Option<*const u8> {
    if phys_addr == 0 || byte_len == 0 {
        return None;
    }
    let page_off = (phys_addr & 0xfff) as usize;
    let total = page_off.checked_add(byte_len)?;
    let pages = total.div_ceil(4096);
    if pages > max_slots || slot_base + pages > 512 {
        return None;
    }
    let first_page_phys = phys_addr & PHYS_PAGE_MASK;
    // SAFETY: Mutating BSP-local `ACPI_WIN_PT` slots and invalidating local TLB entries via `invlpg`.
    unsafe {
        let pt_ptr = (*ACPI_WIN_PT.0.get()).0.as_mut_ptr();
        for p in 0..pages {
            let slot = slot_base + p;
            let paddr = first_page_phys + (p as u64) * 4096;
            // Supervisor RO + NX + Present (`(1 << 63) | 0x01`).
            write_volatile(pt_ptr.add(slot), paddr | (1u64 << 63) | 0x01);
            let vaddr = ACPI_BOOT_WIN_BASE + (slot as u64) * 4096;
            asm!("invlpg [{}]", in(reg) vaddr, options(nostack, preserves_flags));
        }
    }
    Some((ACPI_BOOT_WIN_BASE + (slot_base as u64) * 4096 + (page_off as u64)) as *const u8)
}

/// Validate standard 8-bit ACPI table byte checksum (`sum(bytes[0..len]) & 0xFF == 0`).
unsafe fn verify_checksum(ptr: *const u8, len: usize) -> bool {
    if ptr.is_null() || len == 0 || len > (1024 * 1024) {
        return false;
    }
    let mut sum = 0u8;
    let mut i = 0usize;
    while i < len {
        // SAFETY: Caller ensures `[ptr .. ptr + len)` is mapped in the read-only ACPI boot window.
        sum = sum.wrapping_add(unsafe { *ptr.add(i) });
        i += 1;
    }
    sum == 0
}

unsafe fn find_tables_in_root_sdt(
    hhdm_offset: u64,
    root_raw: u64,
    is_xsdt: bool,
) -> (Option<*const u8>, Option<*const u8>) {
    let root_phys = to_phys_addr(hhdm_offset, root_raw);
    if root_phys == 0 {
        return (None, None);
    }
    // Map initial 36-byte SDT header first, then remap over full `hdr.length`.
    let Some(hdr_probe) = (unsafe {
        map_acpi_window_ro(SLOT_ROOT_START, 16, root_phys, core::mem::size_of::<AcpiSdtHeader>())
    }) else {
        return (None, None);
    };
    let hdr: AcpiSdtHeader = unsafe { read_unaligned(hdr_probe.cast()) };
    let expected_sig = if is_xsdt { *b"XSDT" } else { *b"RSDT" };
    let total_len = hdr.length as usize;
    if hdr.signature != expected_sig || total_len < core::mem::size_of::<AcpiSdtHeader>() {
        return (None, None);
    }
    let Some(hdr_ptr) = (unsafe { map_acpi_window_ro(SLOT_ROOT_START, 16, root_phys, total_len) }) else {
        return (None, None);
    };
    if !unsafe { verify_checksum(hdr_ptr, total_len) } {
        return (None, None);
    }

    let payload_off = core::mem::size_of::<AcpiSdtHeader>();
    let entry_size = if is_xsdt { 8usize } else { 4usize };
    let entries = (total_len - payload_off) / entry_size;
    let mut madt_out: Option<*const u8> = None;
    let mut fadt_out: Option<*const u8> = None;

    for idx in 0..entries {
        let e_ptr = unsafe { hdr_ptr.add(payload_off + idx * entry_size) };
        let child_raw = if is_xsdt {
            unsafe { read_unaligned(e_ptr.cast::<u64>()) }
        } else {
            unsafe { read_unaligned(e_ptr.cast::<u32>()) as u64 }
        };
        let child_phys = to_phys_addr(hhdm_offset, child_raw);
        if child_phys == 0 {
            continue;
        }
        if madt_out.is_none() {
            if let Some(c_probe) = unsafe {
                map_acpi_window_ro(SLOT_CHILD_START, 64, child_phys, core::mem::size_of::<AcpiSdtHeader>())
            } {
                let c_hdr: AcpiSdtHeader = unsafe { read_unaligned(c_probe.cast()) };
                let c_len = c_hdr.length as usize;
                if c_hdr.signature == *b"APIC" && c_len >= 44 {
                    if let Some(c_virt) = unsafe { map_acpi_window_ro(SLOT_CHILD_START, 64, child_phys, c_len) } {
                        if unsafe { verify_checksum(c_virt, c_len) } {
                            madt_out = Some(c_virt);
                            continue;
                        }
                    }
                }
            }
        }
        if fadt_out.is_none() {
            if let Some(f_probe) = unsafe {
                map_acpi_window_ro(SLOT_FADT_START, 16, child_phys, core::mem::size_of::<AcpiSdtHeader>())
            } {
                let f_hdr: AcpiSdtHeader = unsafe { read_unaligned(f_probe.cast()) };
                let f_len = f_hdr.length as usize;
                if f_hdr.signature == *b"FACP" && f_len >= 116 {
                    if let Some(f_virt) = unsafe { map_acpi_window_ro(SLOT_FADT_START, 16, child_phys, f_len) } {
                        if unsafe { verify_checksum(f_virt, f_len) } {
                            fadt_out = Some(f_virt);
                        }
                    }
                }
            }
        }
    }
    (madt_out, fadt_out)
}

unsafe fn read_aml_int_u8(dsdt: *const u8, total_len: usize, cur: &mut usize) -> Option<u8> {
    if *cur >= total_len {
        return None;
    }
    let op = unsafe { *dsdt.add(*cur) };
    match op {
        0x00 => {
            *cur += 1;
            Some(0)
        }
        0x01 => {
            *cur += 1;
            Some(1)
        }
        0xff => {
            *cur += 1;
            Some(0xff)
        }
        0x0a if *cur + 1 < total_len => {
            let v = unsafe { *dsdt.add(*cur + 1) };
            *cur += 2;
            Some(v)
        }
        0x0b if *cur + 2 < total_len => {
            let v = unsafe { *dsdt.add(*cur + 1) };
            *cur += 3;
            Some(v)
        }
        0x0c if *cur + 4 < total_len => {
            let v = unsafe { *dsdt.add(*cur + 1) };
            *cur += 5;
            Some(v)
        }
        _ => None,
    }
}

unsafe fn scan_dsdt_sleep_states(dsdt: *const u8, total_len: usize) -> (Option<(u8, u8)>, bool) {
    let mut s5_pair: Option<(u8, u8)> = None;
    let mut s3_found = false;
    if total_len <= core::mem::size_of::<AcpiSdtHeader>() + 6 {
        return (None, false);
    }
    let mut off = core::mem::size_of::<AcpiSdtHeader>();
    while off + 4 <= total_len {
        let b0 = unsafe { *dsdt.add(off) };
        let b1 = unsafe { *dsdt.add(off + 1) };
        let b2 = unsafe { *dsdt.add(off + 2) };
        let b3 = unsafe { *dsdt.add(off + 3) };
        if b0 == b'_' && b1 == b'S' && b3 == b'_' && (b2 == b'3' || b2 == b'5') {
            let name_op_before = (off >= 1 && unsafe { *dsdt.add(off - 1) } == 0x08)
                || (off >= 2
                    && unsafe { *dsdt.add(off - 2) } == 0x08
                    && unsafe { *dsdt.add(off - 1) } == b'\\');
            if name_op_before && off + 6 < total_len && unsafe { *dsdt.add(off + 4) } == 0x12 {
                if b2 == b'3' {
                    s3_found = true;
                } else if b2 == b'5' && s5_pair.is_none() {
                    let pkg_lead = unsafe { *dsdt.add(off + 5) };
                    let pkg_len_bytes = match pkg_lead >> 6 {
                        0 => 1usize,
                        n => 1usize + (n as usize),
                    };
                    let mut cur = off + 5 + pkg_len_bytes + 1; // Skip `PkgLength` + `NumElements`
                    if let Some(ta) = unsafe { read_aml_int_u8(dsdt, total_len, &mut cur) } {
                        let tb = unsafe { read_aml_int_u8(dsdt, total_len, &mut cur) }.unwrap_or(ta);
                        s5_pair = Some((ta & 0x07, tb & 0x07));
                    }
                }
            }
        }
        off += 1;
    }
    (s5_pair, s3_found)
}

unsafe fn parse_fadt_and_dsdt(hhdm_offset: u64, fadt: *const u8) -> AcpiPowerInfo {
    let mut pwr = AcpiPowerInfo::empty();
    let f_hdr: AcpiSdtHeader = unsafe { read_unaligned(fadt.cast()) };
    let len = f_hdr.length as usize;
    pwr.fadt_found = true;

    let facs32 = unsafe { read_unaligned(fadt.add(36).cast::<u32>()) } as u64;
    let dsdt32 = unsafe { read_unaligned(fadt.add(40).cast::<u32>()) } as u64;
    let smi_cmd = unsafe { read_unaligned(fadt.add(48).cast::<u32>()) };
    if smi_cmd <= u16::MAX as u32 {
        pwr.smi_cmd_port = smi_cmd as u16;
        pwr.acpi_enable_val = unsafe { *fadt.add(52) };
    }

    let mut pm1a = unsafe { read_unaligned(fadt.add(64).cast::<u32>()) } as u64;
    let mut pm1b = unsafe { read_unaligned(fadt.add(68).cast::<u32>()) } as u64;
    let flags = unsafe { read_unaligned(fadt.add(112).cast::<u32>()) };

    if len >= 129 && (flags & (1 << 10)) != 0 {
        let space_id = unsafe { *fadt.add(116) };
        let addr = unsafe { read_unaligned(fadt.add(120).cast::<u64>()) };
        let rval = unsafe { *fadt.add(128) };
        if space_id == 1 && addr > 0 && addr <= u16::MAX as u64 {
            pwr.reset_supported = true;
            pwr.reset_reg_io_port = addr as u16;
            pwr.reset_value = rval;
        }
    }

    let mut facs_raw = facs32;
    if len >= 140 {
        let x_facs = unsafe { read_unaligned(fadt.add(132).cast::<u64>()) };
        if x_facs != 0 {
            facs_raw = x_facs;
        }
    }
    pwr.facs_phys = to_phys_addr(hhdm_offset, facs_raw);

    let mut dsdt_raw = dsdt32;
    if len >= 148 {
        let x_dsdt = unsafe { read_unaligned(fadt.add(140).cast::<u64>()) };
        if x_dsdt != 0 {
            dsdt_raw = x_dsdt;
        }
    }
    if len >= 184 && unsafe { *fadt.add(172) } == 1 {
        let x_pm1a = unsafe { read_unaligned(fadt.add(176).cast::<u64>()) };
        if x_pm1a != 0 {
            pm1a = x_pm1a;
        }
    }
    if len >= 196 && unsafe { *fadt.add(184) } == 1 {
        let x_pm1b = unsafe { read_unaligned(fadt.add(188).cast::<u64>()) };
        if x_pm1b != 0 {
            pm1b = x_pm1b;
        }
    }
    if pm1a <= u16::MAX as u64 {
        pwr.pm1a_cnt_port = pm1a as u16;
    }
    if pm1b <= u16::MAX as u64 {
        pwr.pm1b_cnt_port = pm1b as u16;
    }

    let dsdt_phys = to_phys_addr(hhdm_offset, dsdt_raw);
    if dsdt_phys != 0 {
        if let Some(d_probe) = unsafe {
            map_acpi_window_ro(SLOT_DSDT_START, 256, dsdt_phys, core::mem::size_of::<AcpiSdtHeader>())
        } {
            let d_hdr: AcpiSdtHeader = unsafe { read_unaligned(d_probe.cast()) };
            let d_len = d_hdr.length as usize;
            if d_hdr.signature == *b"DSDT" && d_len >= core::mem::size_of::<AcpiSdtHeader>() {
                if let Some(d_virt) = unsafe { map_acpi_window_ro(SLOT_DSDT_START, 256, dsdt_phys, d_len) } {
                    if unsafe { verify_checksum(d_virt, d_len) } {
                        let (s5_opt, s3_found) = unsafe { scan_dsdt_sleep_states(d_virt, d_len) };
                        pwr.s3_aml_present = s3_found;
                        if let Some((ta, tb)) = s5_opt {
                            pwr.s5_valid = pwr.pm1a_cnt_port != 0;
                            pwr.slp_typ_s5_a = ta;
                            pwr.slp_typ_s5_b = tb;
                        }
                    }
                }
            }
        }
    }
    pwr
}

/// Discover ACPI `RSDP -> XSDT/RSDT -> MADT` on BSP and latch IOAPIC + ISA IRQ routing metadata.
pub fn discover_acpi_madt(
    hhdm_offset: u64,
    kernel_phys_base: u64,
    kernel_virt_base: u64,
) -> AcpiMadtSummary {
    let mut summary = AcpiMadtSummary::empty();
    let Some(rsdp_raw) = get_rsdp_raw_addr() else {
        // SAFETY: Single-threaded BSP initialization.
        unsafe { *ACPI_SUMMARY.0.get() = summary };
        ACPI_READY.store(true, Ordering::Release);
        return summary;
    };

    let rsdp_phys = to_phys_addr(hhdm_offset, rsdp_raw);
    // SAFETY: Single-threaded BSP (`CLI`) reading firmware RSDP/XSDT/MADT via temporary RO window.
    unsafe {
        install_acpi_boot_window(hhdm_offset, kernel_phys_base, kernel_virt_base);
        let Some(rsdp_ptr) = map_acpi_window_ro(SLOT_RSDP_START, 2, rsdp_phys, 64) else {
            *ACPI_SUMMARY.0.get() = summary;
            ACPI_READY.store(true, Ordering::Release);
            return summary;
        };
        let v1: RsdpV1 = read_unaligned(rsdp_ptr.cast());
        if v1.signature != *b"RSD PTR " || !verify_checksum(rsdp_ptr, core::mem::size_of::<RsdpV1>()) {
            *ACPI_SUMMARY.0.get() = summary;
            ACPI_READY.store(true, Ordering::Release);
            return summary;
        }
        summary.rsdp_revision = v1.revision;

        let mut madt_ptr: Option<*const u8> = None;
        let mut fadt_ptr: Option<*const u8> = None;
        if v1.revision >= 2 {
            let v2: RsdpV2 = read_unaligned(rsdp_ptr.cast());
            let v2_len = (v2.length as usize).clamp(core::mem::size_of::<RsdpV2>(), 4096);
            if let Some(rsdp_full) = map_acpi_window_ro(SLOT_RSDP_START, 2, rsdp_phys, v2_len) {
                if verify_checksum(rsdp_full, v2_len) && v2.xsdt_phys != 0 {
                    let (m, f) = find_tables_in_root_sdt(hhdm_offset, v2.xsdt_phys, true);
                    if m.is_some() || f.is_some() {
                        summary.used_xsdt = true;
                        madt_ptr = m;
                        fadt_ptr = f;
                    }
                }
            }
        }
        if (madt_ptr.is_none() || fadt_ptr.is_none()) && v1.rsdt_phys != 0 {
            let (m, f) = find_tables_in_root_sdt(hhdm_offset, v1.rsdt_phys as u64, false);
            if madt_ptr.is_none() {
                madt_ptr = m;
            }
            if fadt_ptr.is_none() {
                fadt_ptr = f;
            }
        }

        if let Some(fadt) = fadt_ptr {
            *ACPI_POWER.0.get() = parse_fadt_and_dsdt(hhdm_offset, fadt);
        }

        if let Some(madt) = madt_ptr {
            let m_hdr: AcpiSdtHeader = read_unaligned(madt.cast());
            let total_len = m_hdr.length as usize;
            // MADT header (`36 B`) + Local APIC Addr (`4 B`) + Flags (`4 B`) = `44 B`.
            let mut off = 44usize;
            while off + 2 <= total_len {
                let entry_type = *madt.add(off);
                let entry_len = *madt.add(off + 1) as usize;
                if entry_len < 2 || off + entry_len > total_len {
                    break;
                }
                match entry_type {
                    // Type 1: I/O APIC (`12` bytes)
                    1 if entry_len >= 12 => {
                        let io_id = *madt.add(off + 2);
                        let io_phys = read_unaligned(madt.add(off + 4).cast::<u32>()) as u64;
                        let gsi_base = read_unaligned(madt.add(off + 8).cast::<u32>());
                        summary.ioapic_count = summary.ioapic_count.saturating_add(1);
                        if !summary.ioapic_found || gsi_base == 0 {
                            summary.ioapic_found = true;
                            summary.ioapic_id = io_id;
                            summary.ioapic_phys = io_phys;
                            summary.ioapic_gsi_base = gsi_base;
                        }
                    }
                    // Type 2: Interrupt Source Override (`10` bytes)
                    2 if entry_len >= 10 => {
                        let bus = *madt.add(off + 2);
                        let src_irq = *madt.add(off + 3);
                        let gsi = read_unaligned(madt.add(off + 4).cast::<u32>());
                        let flags = read_unaligned(madt.add(off + 8).cast::<u16>());
                        if bus == 0 && (src_irq as usize) < ISA_IRQ_COUNT {
                            // Polarity (`bits[1:0]`): `0b11` = active-low; ISA default = active-high (`false`).
                            let pol = flags & 0x3;
                            let active_low = pol == 0x3;
                            // Trigger (`bits[3:2]`): `0b11` = level-triggered; ISA default = edge (`false`).
                            let trig = (flags >> 2) & 0x3;
                            let level_triggered = trig == 0x3;
                            summary.isa_routes[src_irq as usize] = IsaIrqRoute {
                                gsi,
                                active_low,
                                level_triggered,
                                overridden: true,
                            };
                            summary.iso_override_count = summary.iso_override_count.saturating_add(1);
                        }
                    }
                    _ => {}
                }
                off += entry_len;
            }
        }

        *ACPI_SUMMARY.0.get() = summary;
        ACPI_READY.store(true, Ordering::Release);
    }
    summary
}

#[inline]
pub fn acpi_madt_summary() -> AcpiMadtSummary {
    assert!(ACPI_READY.load(Ordering::Acquire), "ACPI MADT queried before discover_acpi_madt");
    // SAFETY: Immutable after `discover_acpi_madt` on BSP.
    unsafe { *ACPI_SUMMARY.0.get() }
}

#[inline]
pub fn acpi_power_info() -> AcpiPowerInfo {
    if !ACPI_READY.load(Ordering::Acquire) {
        return AcpiPowerInfo::empty();
    }
    // SAFETY: Immutable after `discover_acpi_madt` on BSP.
    unsafe { *ACPI_POWER.0.get() }
}

#[inline]
pub fn ioapic_phys_base() -> Option<u64> {
    if !ACPI_READY.load(Ordering::Acquire) {
        return None;
    }
    let s = acpi_madt_summary();
    (s.ioapic_found && s.ioapic_phys != 0).then_some(s.ioapic_phys)
}

#[inline]
pub fn isa_irq_route(irq: u8) -> IsaIrqRoute {
    if !ACPI_READY.load(Ordering::Acquire) || (irq as usize) >= ISA_IRQ_COUNT {
        return IsaIrqRoute::identity(irq);
    }
    acpi_madt_summary().isa_routes[irq as usize]
}
