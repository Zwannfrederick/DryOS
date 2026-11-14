// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Low-level PCI Configuration Space Mechanism #1, Generic/VirtIO Capability Walk (`MSI`,
//! `MSI-X`, `VNDR`), and Validated BAR/MMIO Descriptor Construction.

use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{inl, irq_restore, irq_save_cli, outl, outw};
use crate::pci_bar::{probe_function_bars, PciBarDesc};
use crate::pmm::{query_frame_state, FrameState, PAGE_SIZE};

const PCI_CONFIG_ADDR: u16 = 0x0cf8;
const PCI_CONFIG_DATA: u16 = 0x0cfc;

const VIRTIO_VENDOR_ID: u16 = 0x1af4;
const VIRTIO_DEV_BLK_LEGACY_OR_TRANS: u16 = 0x1001;
const VIRTIO_DEV_BLK_MODERN: u16 = 0x1042;
const VIRTIO_DEV_RNG_LEGACY_OR_TRANS: u16 = 0x1005;
const VIRTIO_DEV_RNG_MODERN: u16 = 0x1044;
const VIRTIO_DEV_GPU_MODERN: u16 = 0x1050;
const PCI_DEV_IVSHMEM: u16 = 0x1110;

const QEMU_EDU_VENDOR_ID: u16 = 0x1234;
const QEMU_EDU_DEVICE_ID: u16 = 0x11e8;

const QEMU_TESTDEV_VENDOR_ID: u16 = 0x1b36;
const QEMU_IOMMU_TESTDEV_ID: u16 = 0x0014;

pub const PCI_OBJ_VIRTIO_BLK: usize = 0;
pub const PCI_OBJ_VIRTIO_RNG: usize = 1;
pub const PCI_OBJ_VIRTIO_GPU: usize = 2;
pub const PCI_OBJ_QEMU_EDU: usize = 3;
pub const PCI_OBJ_IVSHMEM: usize = 4;
pub const PCI_OBJ_IOMMU_TEST: usize = 5;
pub(crate) const PCI_DEV_COUNT: usize = 6;

const PCI_CAP_ID_MSI: u8 = 0x05;
const PCI_CAP_ID_VNDR: u8 = 0x09;
const PCI_CAP_ID_MSIX: u8 = 0x11;

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

const MAX_MMIO_BAR_PAGES: usize = 32;

/// Sanitized 32-byte VirtIO 1.0 Modern PCI structure offsets copied to CPL=3 by `SYS_DEV_MAP` (`0` phys addrs).
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default)]
pub struct UserVirtioPciInfo {
    pub common_off: u32,
    pub notify_off: u32,
    pub notify_mult: u32,
    pub isr_off: u32,
    pub device_off: u32,
    pub mapped_pages: u16,
    pub bus_dev_fn: u16,
    pub vendor_id: u16,
    pub device_id: u16,
    pub _pad: u32,
}

const _: () = assert!(size_of::<UserVirtioPciInfo>() == 32);

#[derive(Clone, Copy)]
pub(crate) struct ValidatedVirtioPci {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub bar_idx: u8,
    pub is_virtio: bool,
    pub phys_base: u64,
    pub num_pages: u16,
    pub msi_cap_off: u8,
    pub msix_cap_off: u8,
    pub msix_table_bir: u8,
    pub msix_table_off: u32,
    pub msix_table_len: u16,
    pub bars: [PciBarDesc; 6],
    pub info: UserVirtioPciInfo,
}

impl ValidatedVirtioPci {
    pub(crate) const EMPTY: Self = Self {
        bus: 0,
        dev: 0,
        func: 0,
        bar_idx: 0,
        is_virtio: false,
        phys_base: 0,
        num_pages: 0,
        msi_cap_off: 0,
        msix_cap_off: 0,
        msix_table_bir: 0,
        msix_table_off: 0,
        msix_table_len: 0,
        bars: [PciBarDesc::UNUSED; 6],
        info: UserVirtioPciInfo {
            common_off: 0,
            notify_off: 0,
            notify_mult: 0,
            isr_off: 0,
            device_off: 0,
            mapped_pages: 0,
            bus_dev_fn: 0,
            vendor_id: 0,
            device_id: 0,
            _pad: 0,
        },
    };
}

static PCI_CFG_LOCK: AtomicBool = AtomicBool::new(false);

#[inline]
fn with_pci_cfg_lock<R>(f: impl FnOnce() -> R) -> R {
    let saved_if = irq_save_cli();
    while PCI_CFG_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let out = f();
    PCI_CFG_LOCK.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline]
fn pci_cfg_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000u32
        | ((bus as u32) << 16)
        | (((dev & 0x1f) as u32) << 11)
        | (((func & 0x07) as u32) << 8)
        | ((offset & 0xfc) as u32)
}

#[inline]
unsafe fn raw_read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr = pci_cfg_addr(bus, dev, func, offset);
    unsafe {
        outl(PCI_CONFIG_ADDR, addr);
        inl(PCI_CONFIG_DATA)
    }
}

#[inline]
unsafe fn raw_write_u32(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
    let addr = pci_cfg_addr(bus, dev, func, offset);
    unsafe {
        outl(PCI_CONFIG_ADDR, addr);
        outl(PCI_CONFIG_DATA, val);
    }
}

#[inline]
unsafe fn raw_write_u16(bus: u8, dev: u8, func: u8, offset: u8, val: u16) {
    let addr = pci_cfg_addr(bus, dev, func, offset);
    let data_port = PCI_CONFIG_DATA + ((offset & 2) as u16);
    unsafe {
        outl(PCI_CONFIG_ADDR, addr);
        outw(data_port, val);
    }
}

pub(crate) fn pci_read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    with_pci_cfg_lock(|| unsafe { raw_read_u32(bus, dev, func, offset) })
}

#[inline]
pub(crate) fn pci_read_u16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let shift = ((offset & 2) * 8) as u32;
    ((pci_read_u32(bus, dev, func, offset) >> shift) & 0xffff) as u16
}

#[inline]
pub(crate) fn pci_read_u8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let shift = ((offset & 3) * 8) as u32;
    ((pci_read_u32(bus, dev, func, offset) >> shift) & 0xff) as u8
}

pub(crate) fn pci_write_u16(bus: u8, dev: u8, func: u8, offset: u8, val: u16) {
    with_pci_cfg_lock(|| unsafe { raw_write_u16(bus, dev, func, offset, val) });
}

pub(crate) fn pci_write_u32(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
    with_pci_cfg_lock(|| unsafe { raw_write_u32(bus, dev, func, offset, val) });
}

/// Atomically read raw BAR0..BAR5 values and write `0xffff_ffff` size probes with Command IO/Mem
/// decode disabled, restoring exact original BAR and Command state before releasing `PCI_CFG_LOCK`.
pub(crate) fn probe_all_bars_raw(bus: u8, dev: u8, func: u8) -> ([u32; 6], [u32; 6]) {
    with_pci_cfg_lock(|| unsafe {
        let cmd_dword = raw_read_u32(bus, dev, func, 0x04);
        let orig_cmd = (cmd_dword & 0xffff) as u16;
        // Disable I/O + Memory decode + Bus Master while sizing BARs; keep INTx masked.
        raw_write_u16(bus, dev, func, 0x04, (orig_cmd & !0x0007) | 0x0400);

        let mut raw = [0u32; 6];
        let mut masks = [0u32; 6];
        for (i, slot) in raw.iter_mut().enumerate() {
            *slot = raw_read_u32(bus, dev, func, 0x10 + (i as u8) * 4);
        }

        let mut i = 0usize;
        while i < 6 {
            let reg = 0x10 + (i as u8) * 4;
            let lo = raw[i];
            let is_64 = (lo & 1) == 0 && ((lo >> 1) & 0x3) == 2 && i + 1 < 6;
            if is_64 {
                raw_write_u32(bus, dev, func, reg, 0xffff_ffff);
                raw_write_u32(bus, dev, func, reg + 4, 0xffff_ffff);
                masks[i] = raw_read_u32(bus, dev, func, reg);
                masks[i + 1] = raw_read_u32(bus, dev, func, reg + 4);
                raw_write_u32(bus, dev, func, reg, raw[i]);
                raw_write_u32(bus, dev, func, reg + 4, raw[i + 1]);
                i += 2;
            } else {
                raw_write_u32(bus, dev, func, reg, 0xffff_ffff);
                masks[i] = raw_read_u32(bus, dev, func, reg);
                raw_write_u32(bus, dev, func, reg, raw[i]);
                i += 1;
            }
        }

        raw_write_u16(bus, dev, func, 0x04, orig_cmd);
        (raw, masks)
    })
}

fn match_managed_pci_slot(vendor_id: u16, device_id: u16) -> Option<(usize, bool)> {
    match (vendor_id, device_id) {
        (VIRTIO_VENDOR_ID, VIRTIO_DEV_BLK_MODERN | VIRTIO_DEV_BLK_LEGACY_OR_TRANS) => {
            Some((PCI_OBJ_VIRTIO_BLK, true))
        }
        (VIRTIO_VENDOR_ID, VIRTIO_DEV_RNG_MODERN | VIRTIO_DEV_RNG_LEGACY_OR_TRANS) => {
            Some((PCI_OBJ_VIRTIO_RNG, true))
        }
        (VIRTIO_VENDOR_ID, VIRTIO_DEV_GPU_MODERN) => Some((PCI_OBJ_VIRTIO_GPU, true)),
        (QEMU_EDU_VENDOR_ID, QEMU_EDU_DEVICE_ID) => Some((PCI_OBJ_QEMU_EDU, false)),
        (VIRTIO_VENDOR_ID, PCI_DEV_IVSHMEM) => Some((PCI_OBJ_IVSHMEM, false)),
        (QEMU_TESTDEV_VENDOR_ID, QEMU_IOMMU_TESTDEV_ID) => Some((PCI_OBJ_IOMMU_TEST, false)),
        _ => None,
    }
}

pub(crate) fn probe_pci_function(
    bus: u8,
    dev: u8,
    func: u8,
) -> Option<(usize, ValidatedVirtioPci)> {
    let id = pci_read_u32(bus, dev, func, 0x00);
    let vendor_id = (id & 0xffff) as u16;
    let device_id = (id >> 16) as u16;
    let (slot_idx, is_virtio) = match_managed_pci_slot(vendor_id, device_id)?;

    let bars = probe_function_bars(bus, dev, func);
    let status = pci_read_u16(bus, dev, func, 0x06);

    let mut common: Option<(u8, u32, u32)> = None;
    let mut notify: Option<(u8, u32, u32, u32)> = None;
    let mut isr: Option<(u8, u32, u32)> = None;
    let mut device: Option<(u8, u32, u32)> = None;

    let mut msi_cap_off = 0u8;
    let mut msix_cap_off = 0u8;
    let mut msix_table_bir = 0u8;
    let mut msix_table_off = 0u32;
    let mut msix_table_len = 0u16;

    if (status & 0x0010) != 0 {
        let mut cap_ptr = pci_read_u8(bus, dev, func, 0x34) & 0xfc;
        let mut hops = 0usize;
        while cap_ptr >= 0x40 && hops < 48 {
            hops += 1;
            let cap_id = pci_read_u8(bus, dev, func, cap_ptr);
            let next_ptr = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(1)) & 0xfc;
            match cap_id {
                PCI_CAP_ID_MSI if msi_cap_off == 0 => {
                    let ctrl = pci_read_u16(bus, dev, func, cap_ptr.wrapping_add(2));
                    pci_write_u16(bus, dev, func, cap_ptr.wrapping_add(2), ctrl & !0x0001);
                    msi_cap_off = cap_ptr;
                }
                PCI_CAP_ID_MSIX if msix_cap_off == 0 => {
                    let ctrl = pci_read_u16(bus, dev, func, cap_ptr.wrapping_add(2));
                    pci_write_u16(
                        bus,
                        dev,
                        func,
                        cap_ptr.wrapping_add(2),
                        (ctrl & !(1 << 15)) | (1 << 14),
                    );
                    let tbl = pci_read_u32(bus, dev, func, cap_ptr.wrapping_add(4));
                    let bir = (tbl & 0x7) as u8;
                    let off = tbl & !0x7;
                    let tlen = (ctrl & 0x07ff).saturating_add(1);
                    if (bir as usize) < 6 && bars[bir as usize].is_mmio() {
                        msix_cap_off = cap_ptr;
                        msix_table_bir = bir;
                        msix_table_off = off;
                        msix_table_len = tlen;
                    }
                }
                PCI_CAP_ID_VNDR if is_virtio => {
                    let cap_len = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(2));
                    let cfg_type = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(3));
                    let bar_idx = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(4));
                    if cap_len >= 16
                        && (bar_idx as usize) < 6
                        && bars[bar_idx as usize].is_mmio()
                    {
                        let off = pci_read_u32(bus, dev, func, cap_ptr.wrapping_add(8));
                        let len = pci_read_u32(bus, dev, func, cap_ptr.wrapping_add(12));
                        match cfg_type {
                            VIRTIO_PCI_CAP_COMMON_CFG if common.is_none() => {
                                common = Some((bar_idx, off, len));
                            }
                            VIRTIO_PCI_CAP_NOTIFY_CFG if notify.is_none() && cap_len >= 20 => {
                                let mult = pci_read_u32(bus, dev, func, cap_ptr.wrapping_add(16));
                                notify = Some((bar_idx, off, len, mult));
                            }
                            VIRTIO_PCI_CAP_ISR_CFG if isr.is_none() => {
                                isr = Some((bar_idx, off, len));
                            }
                            VIRTIO_PCI_CAP_DEVICE_CFG if device.is_none() => {
                                device = Some((bar_idx, off, len));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            cap_ptr = next_ptr;
        }
    }

    let (bar_idx, phys_base, pages, c_off, n_off, n_mult, i_off, d_off) = if is_virtio {
        let (c_bar, c_off, c_len) = common?;
        let (n_bar, n_off, n_len, n_mult) = notify?;
        let (i_bar, i_off, i_len) = isr?;
        let (d_bar, d_off, d_len) = device.unwrap_or((c_bar, 0, 0));
        if c_bar != n_bar || c_bar != i_bar || c_bar != d_bar {
            return None;
        }
        let bdesc = bars[c_bar as usize];
        if !bdesc.is_mmio() || (bdesc.base_phys & ((PAGE_SIZE as u64) - 1)) != 0 {
            return None;
        }
        let max_end = c_off
            .checked_add(c_len)?
            .max(n_off.checked_add(n_len)?)
            .max(i_off.checked_add(i_len)?)
            .max(d_off.checked_add(d_len)?) as u64;
        if max_end > bdesc.size_bytes {
            return None;
        }
        let pages = max_end.div_ceil(PAGE_SIZE as u64) as usize;
        if pages == 0 || pages > MAX_MMIO_BAR_PAGES {
            return None;
        }
        for p in 0..pages {
            let pa = bdesc.base_phys + (p as u64) * (PAGE_SIZE as u64);
            if query_frame_state(pa) != FrameState::ReservedOrMmio {
                return None;
            }
        }
        (c_bar, bdesc.base_phys, pages as u16, c_off, n_off, n_mult, i_off, d_off)
    } else {
        // Generic non-VirtIO PCI device (`edu`, `ivshmem-plain`, `iommu-testdev`): must have >=1 valid MMIO BAR.
        let (first_idx, first_bar) = bars.iter().enumerate().find(|(_, b)| b.is_mmio())?;
        let pages = ((first_bar.size_bytes.div_ceil(PAGE_SIZE as u64)) as usize).clamp(1, MAX_MMIO_BAR_PAGES);
        (first_idx as u8, first_bar.base_phys, pages as u16, 0, 0, 0, 0, 0)
    };

    // Keep Bus Master disabled until an authorized Ring 3 driver calls `SYS_DEV_MAP`.
    let cmd = pci_read_u16(bus, dev, func, 0x04);
    pci_write_u16(bus, dev, func, 0x04, (cmd & !0x0004) | 0x0402);

    let bdf = ((bus as u16) << 8) | (((dev & 0x1f) as u16) << 3) | ((func & 0x07) as u16);
    Some((
        slot_idx,
        ValidatedVirtioPci {
            bus,
            dev,
            func,
            bar_idx,
            is_virtio,
            phys_base,
            num_pages: pages,
            msi_cap_off,
            msix_cap_off,
            msix_table_bir,
            msix_table_off,
            msix_table_len,
            bars,
            info: UserVirtioPciInfo {
                common_off: c_off,
                notify_off: n_off,
                notify_mult: n_mult,
                isr_off: i_off,
                device_off: d_off,
                mapped_pages: pages,
                bus_dev_fn: bdf,
                vendor_id,
                device_id,
                _pad: 0,
            },
        },
    ))
}
