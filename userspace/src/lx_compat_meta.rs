// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Read-only Boot PCI Enumeration & Compatibility Metadata Helpers (`Milestone A4-3`).
//!
//! Reads the sanitized array of `BootPciFuncRecord` structs latched by the kernel at
//! `boot_info_addr + BOOT_PCI_ENUM_OFFSET` without issuing privileged PCI configuration I/O.

use core::ptr::read_volatile;

use crate::{BootPciFuncRecord, BOOT_PCI_ENUM_OFFSET, MAX_BOOT_PCI_FUNCS};

/// Copy up to `out.len()` sanitized `BootPciFuncRecord` entries from the read-only boot info page.
pub fn read_boot_pci_funcs(
    boot_info_addr: u64,
    count: u64,
    out: &mut [BootPciFuncRecord],
) -> usize {
    if boot_info_addr == 0 || out.is_empty() {
        return 0;
    }
    let total = (count as usize).min(MAX_BOOT_PCI_FUNCS).min(out.len());
    let base = (boot_info_addr + BOOT_PCI_ENUM_OFFSET) as *const BootPciFuncRecord;
    for (idx, dst) in out[..total].iter_mut().enumerate() {
        // SAFETY: Kernel maps `boot_info_addr` `USER_RO` and populates `count <= MAX_BOOT_PCI_FUNCS`
        // aligned 32-byte `BootPciFuncRecord` entries starting at `+BOOT_PCI_ENUM_OFFSET`.
        *dst = unsafe { read_volatile(base.add(idx)) };
    }
    total
}

/// Find the first latched PCI function matching `vendor_id:device_id` (`0xffff` acts as wildcard)
/// and optional `managed_slot` filter (`0xff` matches any slot).
pub fn find_boot_pci_managed(
    boot_info_addr: u64,
    count: u64,
    vendor_id: u16,
    device_id: u16,
    managed_slot: u8,
) -> Option<BootPciFuncRecord> {
    if boot_info_addr == 0 {
        return None;
    }
    let total = (count as usize).min(MAX_BOOT_PCI_FUNCS);
    let base = (boot_info_addr + BOOT_PCI_ENUM_OFFSET) as *const BootPciFuncRecord;
    for idx in 0..total {
        // SAFETY: Bounded by `total <= MAX_BOOT_PCI_FUNCS` inside the mapped 4 KiB boot info page.
        let rec = unsafe { read_volatile(base.add(idx)) };
        let v_ok = vendor_id == 0xffff || rec.vendor_id == vendor_id;
        let d_ok = device_id == 0xffff || rec.device_id == device_id;
        let s_ok = managed_slot == 0xff || rec.managed_slot == managed_slot;
        if v_ok && d_ok && s_ok {
            return Some(rec);
        }
    }
    None
}

/// Find the first latched PCI function matching `vendor_id:device_id` (`0xffff` acts as wildcard).
pub fn find_boot_pci_func(
    boot_info_addr: u64,
    count: u64,
    vendor_id: u16,
    device_id: u16,
) -> Option<BootPciFuncRecord> {
    find_boot_pci_managed(boot_info_addr, count, vendor_id, device_id, 0xff)
}

/// Count latched PCI functions whose base class code equals `class_code` (e.g. `0x03` Display).
pub fn count_boot_pci_class(boot_info_addr: u64, count: u64, class_code: u8) -> usize {
    if boot_info_addr == 0 {
        return 0;
    }
    let total = (count as usize).min(MAX_BOOT_PCI_FUNCS);
    let base = (boot_info_addr + BOOT_PCI_ENUM_OFFSET) as *const BootPciFuncRecord;
    let mut hits = 0usize;
    for idx in 0..total {
        // SAFETY: Bounded by `total <= MAX_BOOT_PCI_FUNCS` inside the mapped 4 KiB boot info page.
        let rec = unsafe { read_volatile(base.add(idx)) };
        if rec.class_code == class_code {
            hits += 1;
        }
    }
    hits
}

/// Return `true` when `BootPciFuncRecord::bar_flags[i]` marks an active MMIO BAR.
#[inline]
pub const fn bar_is_mmio(bar_flag: u8) -> bool {
    (bar_flag & 1) != 0
}

/// Return `true` when `BootPciFuncRecord::bar_flags[i]` marks a prefetchable MMIO BAR.
#[inline]
pub const fn bar_is_prefetchable(bar_flag: u8) -> bool {
    (bar_flag & 4) != 0
}
