// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Low-level PCI Configuration Space Mechanism #1 & VirtIO 1.0 Modern Capability Probing.
//!
//! Separated from `pci.rs` to keep each kernel module compact while enforcing read-only
//! MMIO BAR validation against `FrameState::ReservedOrMmio`.

use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{inl, irq_restore, irq_save_cli, outl, outw};
use crate::pmm::{query_frame_state, FrameState, PAGE_SIZE};

const PCI_CONFIG_ADDR: u16 = 0x0cf8;
const PCI_CONFIG_DATA: u16 = 0x0cfc;

const VIRTIO_VENDOR_ID: u16 = 0x1af4;
const VIRTIO_DEV_BLK_LEGACY_OR_TRANS: u16 = 0x1001;
const VIRTIO_DEV_BLK_MODERN: u16 = 0x1042;
const VIRTIO_DEV_RNG_LEGACY_OR_TRANS: u16 = 0x1005;
const VIRTIO_DEV_RNG_MODERN: u16 = 0x1044;
const VIRTIO_DEV_GPU_MODERN: u16 = 0x1050;

pub const PCI_OBJ_VIRTIO_BLK: usize = 0;
pub const PCI_OBJ_VIRTIO_RNG: usize = 1;
pub const PCI_OBJ_VIRTIO_GPU: usize = 2;
pub(crate) const PCI_DEV_COUNT: usize = 3;

const PCI_CAP_ID_VNDR: u8 = 0x09;
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
    pub phys_base: u64,
    pub num_pages: u16,
    pub info: UserVirtioPciInfo,
}

impl ValidatedVirtioPci {
    pub(crate) const EMPTY: Self = Self {
        bus: 0,
        dev: 0,
        func: 0,
        bar_idx: 0,
        phys_base: 0,
        num_pages: 0,
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

pub(crate) fn pci_read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr = pci_cfg_addr(bus, dev, func, offset);
    with_pci_cfg_lock(|| {
        // SAFETY: Standard PCI Mechanism #1 Configuration Space ports `0xCF8`/`0xCFC`.
        unsafe {
            outl(PCI_CONFIG_ADDR, addr);
            inl(PCI_CONFIG_DATA)
        }
    })
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
    let addr = pci_cfg_addr(bus, dev, func, offset);
    let data_port = PCI_CONFIG_DATA + ((offset & 2) as u16);
    with_pci_cfg_lock(|| {
        // SAFETY: Aligned 16-bit write within `0xCFC..=0xCFE` under `PCI_CFG_LOCK`.
        unsafe {
            outl(PCI_CONFIG_ADDR, addr);
            outw(data_port, val);
        }
    });
}

fn read_pci_mmio_bar_base(bus: u8, dev: u8, func: u8, bar_idx: u8) -> Option<u64> {
    if bar_idx >= 6 {
        return None;
    }
    let reg = 0x10u8 + bar_idx * 4;
    let lo = pci_read_u32(bus, dev, func, reg);
    if (lo & 1) != 0 {
        return None; // Reject I/O port BARs; require Memory BAR (`bit 0 == 0`).
    }
    let mem_type = (lo >> 1) & 0x3;
    let phys = if mem_type == 2 {
        if bar_idx >= 5 {
            return None;
        }
        let hi = pci_read_u32(bus, dev, func, reg + 4);
        ((hi as u64) << 32) | ((lo & !0xfu32) as u64)
    } else {
        (lo & !0xfu32) as u64
    };
    (phys != 0).then_some(phys)
}

pub(crate) fn probe_virtio_function(
    bus: u8,
    dev: u8,
    func: u8,
) -> Option<(usize, ValidatedVirtioPci)> {
    let id = pci_read_u32(bus, dev, func, 0x00);
    let vendor_id = (id & 0xffff) as u16;
    let device_id = (id >> 16) as u16;
    if vendor_id != VIRTIO_VENDOR_ID {
        return None;
    }
    let slot_idx = match device_id {
        VIRTIO_DEV_BLK_MODERN | VIRTIO_DEV_BLK_LEGACY_OR_TRANS => PCI_OBJ_VIRTIO_BLK,
        VIRTIO_DEV_RNG_MODERN | VIRTIO_DEV_RNG_LEGACY_OR_TRANS => PCI_OBJ_VIRTIO_RNG,
        VIRTIO_DEV_GPU_MODERN => PCI_OBJ_VIRTIO_GPU,
        _ => return None,
    };
    let status = pci_read_u16(bus, dev, func, 0x06);
    if (status & 0x0010) == 0 {
        return None;
    }

    let mut common: Option<(u8, u32, u32)> = None;
    let mut notify: Option<(u8, u32, u32, u32)> = None;
    let mut isr: Option<(u8, u32, u32)> = None;
    let mut device: Option<(u8, u32, u32)> = None;

    let mut cap_ptr = pci_read_u8(bus, dev, func, 0x34) & 0xfc;
    let mut hops = 0usize;
    while cap_ptr >= 0x40 && hops < 48 {
        hops += 1;
        let cap_id = pci_read_u8(bus, dev, func, cap_ptr);
        let next_ptr = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(1)) & 0xfc;
        if cap_id == PCI_CAP_ID_VNDR {
            let cap_len = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(2));
            let cfg_type = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(3));
            let bar_idx = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(4));
            if cap_len >= 16 && read_pci_mmio_bar_base(bus, dev, func, bar_idx).is_some() {
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
        cap_ptr = next_ptr;
    }

    let (c_bar, c_off, c_len) = common?;
    let (n_bar, n_off, n_len, n_mult) = notify?;
    let (i_bar, i_off, i_len) = isr?;
    // VirtIO-RNG has no `VIRTIO_PCI_CAP_DEVICE_CFG` in VirtIO 1.0; keep `device` optional.
    let (d_bar, d_off, d_len) = device.unwrap_or((c_bar, 0, 0));
    if c_bar != n_bar || c_bar != i_bar || c_bar != d_bar {
        return None;
    }
    let phys_base = read_pci_mmio_bar_base(bus, dev, func, c_bar)?;
    if (phys_base & ((PAGE_SIZE as u64) - 1)) != 0 {
        return None;
    }

    let max_end = (c_off as u64 + c_len as u64)
        .max(n_off as u64 + n_len as u64)
        .max(i_off as u64 + i_len as u64)
        .max(d_off as u64 + d_len as u64);
    let pages = max_end.div_ceil(PAGE_SIZE as u64) as usize;
    if pages == 0 || pages > MAX_MMIO_BAR_PAGES {
        return None;
    }
    for p in 0..pages {
        let pa = phys_base + (p as u64) * (PAGE_SIZE as u64);
        if query_frame_state(pa) != FrameState::ReservedOrMmio {
            return None;
        }
    }

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
            bar_idx: c_bar,
            phys_base,
            num_pages: pages as u16,
            info: UserVirtioPciInfo {
                common_off: c_off,
                notify_off: n_off,
                notify_mult: n_mult,
                isr_off: i_off,
                device_off: d_off,
                mapped_pages: pages as u16,
                bus_dev_fn: bdf,
                vendor_id,
                device_id,
                _pad: 0,
            },
        },
    ))
}
