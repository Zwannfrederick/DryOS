// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Read-only Boot PCI Function Inventory, Active GOP Protection Coordinates & Hardware Topology
//! Summaries split from `pci.rs` to keep module size clean (`< 800` lines).

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::pci_bar::{
    PciBarDesc, PciBarKind, PCI_BAR_FLAG_64BIT, PCI_BAR_FLAG_ABOVE_4G, PCI_BAR_FLAG_IO,
    PCI_BAR_FLAG_MMIO, PCI_BAR_FLAG_PREFETCHABLE,
};

pub const PCI_OBJ_VIRTIO_BLK: usize = 0;
pub const PCI_OBJ_VIRTIO_RNG: usize = 1;
pub const PCI_OBJ_VIRTIO_GPU: usize = 2;
pub const PCI_OBJ_QEMU_EDU: usize = 3;
pub const PCI_OBJ_IVSHMEM: usize = 4;
pub const PCI_OBJ_IOMMU_TEST: usize = 5;
pub const PCI_OBJ_DISPLAY_AUX: usize = 6;
pub const PCI_OBJ_XHCI: usize = 7;
pub const PCI_OBJ_NVME: usize = 8;
pub const PCI_OBJ_NET: usize = 9;
pub const PCI_OBJ_IGPU: usize = 10;
pub(crate) const PCI_LEGACY_FIXED_SLOTS: usize = 11;
pub(crate) const MAX_MANAGED_PCI_DEVS: usize = 64;

pub const BOOT_PCI_ENUM_OFFSET: usize = 0x0800;
pub const MAX_BOOT_PCI_FUNCS: usize = 64;

pub const PCI_BOOT_EXTRA_PASSIVE: u8 = 1 << 0;
pub const PCI_BOOT_EXTRA_BAD_BAR: u8 = 1 << 1;
pub const PCI_BOOT_EXTRA_BUS_MASTER: u8 = 1 << 2;

/// Sanitized 32-byte PCI function descriptor published read-only in boot info (`0` phys addrs).
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct BootPciFuncRecord {
    pub bus_dev_fn: u16,
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsys_vendor_id: u16,
    pub subsys_id: u16,
    pub revision_id: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub managed_slot: u8,
    pub dev_flags: u8,
    pub bar_flags: [u8; 6],
    pub irq_pin: u8,
    pub extra_flags: u8,
    pub bar0_size_bytes: u64,
}

const _: () = assert!(size_of::<BootPciFuncRecord>() == 32);

struct BootPciEnumTable {
    recs: UnsafeCell<[BootPciFuncRecord; MAX_BOOT_PCI_FUNCS]>,
    count: AtomicU32,
}

// SAFETY: Populated once on BSP during `init_pci_devices()` before Ring-3 launch.
unsafe impl Sync for BootPciEnumTable {}

static BOOT_PCI_ENUM: BootPciEnumTable = BootPciEnumTable {
    recs: UnsafeCell::new([BootPciFuncRecord {
        bus_dev_fn: 0,
        vendor_id: 0,
        device_id: 0,
        subsys_vendor_id: 0,
        subsys_id: 0,
        revision_id: 0,
        prog_if: 0,
        subclass: 0,
        class_code: 0,
        managed_slot: 0xff,
        dev_flags: 0,
        bar_flags: [0; 6],
        irq_pin: 0,
        extra_flags: 0,
        bar0_size_bytes: 0,
    }; MAX_BOOT_PCI_FUNCS]),
    count: AtomicU32::new(0),
};

static ACTIVE_GOP_PRESENT: AtomicBool = AtomicBool::new(false);
static ACTIVE_GOP_BDF: AtomicU16 = AtomicU16::new(0);
static DGPU_PACKED: AtomicU64 = AtomicU64::new(0);
static VMD_PACKED: AtomicU64 = AtomicU64::new(0);
static LPSS_I2C_COUNT: AtomicU8 = AtomicU8::new(0);

#[inline]
pub(crate) fn set_active_gop_bdf(bus: u8, dev: u8, func: u8) {
    let bdf = ((bus as u16) << 8) | (((dev & 0x1f) as u16) << 3) | ((func & 0x07) as u16);
    if !ACTIVE_GOP_PRESENT.swap(true, Ordering::AcqRel) {
        ACTIVE_GOP_BDF.store(bdf, Ordering::Release);
    }
}

#[inline]
pub fn active_gop_bdf() -> Option<(u8, u8, u8)> {
    if !ACTIVE_GOP_PRESENT.load(Ordering::Acquire) {
        return None;
    }
    let bdf = ACTIVE_GOP_BDF.load(Ordering::Acquire);
    Some(((bdf >> 8) as u8, ((bdf >> 3) & 0x1f) as u8, (bdf & 0x07) as u8))
}

#[inline]
pub(crate) fn record_topology_hints(rec: &BootPciFuncRecord) {
    // 03:02:xx = Muxless 3D controller (e.g., NVIDIA RTX 3050 Ti)
    if rec.class_code == 0x03 && rec.subclass == 0x02 && DGPU_PACKED.load(Ordering::Relaxed) == 0 {
        let packed = ((rec.bus_dev_fn as u64) << 32)
            | ((rec.vendor_id as u64) << 16)
            | (rec.device_id as u64);
        DGPU_PACKED.store(packed, Ordering::Release);
    }
    // 01:04:xx = RAID / Intel VMD domain controller (e.g., 8086:9a0b)
    if rec.class_code == 0x01 && rec.subclass == 0x04 && VMD_PACKED.load(Ordering::Relaxed) == 0 {
        let packed = ((rec.bus_dev_fn as u64) << 32)
            | ((rec.vendor_id as u64) << 16)
            | (rec.device_id as u64);
        VMD_PACKED.store(packed, Ordering::Release);
    }
    // 0c:80:xx = Serial bus controller (LPSS I2C host controller on Intel laptops)
    if rec.class_code == 0x0c && rec.subclass == 0x80 {
        let _ = LPSS_I2C_COUNT.try_update(Ordering::AcqRel, Ordering::Relaxed, |v| {
            Some(v.saturating_add(1))
        });
    }
}

#[inline]
pub fn pci_topology_summary() -> (u64, u64, u8) {
    (
        DGPU_PACKED.load(Ordering::Acquire),
        VMD_PACKED.load(Ordering::Acquire),
        LPSS_I2C_COUNT.load(Ordering::Acquire),
    )
}

#[inline]
pub(crate) fn push_boot_pci_record(idx: usize, rec: BootPciFuncRecord) {
    if idx < MAX_BOOT_PCI_FUNCS {
        // SAFETY: Single-threaded BSP init before `BOOT_PCI_ENUM.count` publish.
        unsafe {
            (*BOOT_PCI_ENUM.recs.get())[idx] = rec;
        }
    }
}

#[inline]
pub(crate) fn publish_boot_pci_count(cnt: usize) {
    BOOT_PCI_ENUM
        .count
        .store((cnt.min(MAX_BOOT_PCI_FUNCS)) as u32, Ordering::Release);
}

#[inline]
pub fn latched_boot_pci_funcs() -> (&'static [BootPciFuncRecord], usize) {
    let cnt = (BOOT_PCI_ENUM.count.load(Ordering::Acquire) as usize).min(MAX_BOOT_PCI_FUNCS);
    // SAFETY: Immutable after `init_pci_devices()` on BSP.
    let recs = unsafe {
        core::slice::from_raw_parts(BOOT_PCI_ENUM.recs.get().cast::<BootPciFuncRecord>(), cnt)
    };
    (recs, cnt)
}

#[inline]
pub(crate) fn encode_bar_desc_flags(bar: PciBarDesc) -> u8 {
    match bar.kind {
        PciBarKind::IoPort => PCI_BAR_FLAG_IO as u8,
        PciBarKind::Mem32 | PciBarKind::Mem64 => {
            let mut f = PCI_BAR_FLAG_MMIO as u8;
            if bar.kind == PciBarKind::Mem64 {
                f |= PCI_BAR_FLAG_64BIT as u8;
            }
            if bar.prefetchable {
                f |= PCI_BAR_FLAG_PREFETCHABLE as u8;
            }
            if bar.above_4g {
                f |= PCI_BAR_FLAG_ABOVE_4G as u8;
            }
            f
        }
        _ => 0,
    }
}

#[inline]
pub(crate) fn slot_label(slot_idx: usize) -> &'static str {
    match slot_idx {
        PCI_OBJ_VIRTIO_BLK => "virtio-blk",
        PCI_OBJ_VIRTIO_RNG => "virtio-rng",
        PCI_OBJ_VIRTIO_GPU => "virtio-gpu",
        PCI_OBJ_QEMU_EDU => "qemu-edu",
        PCI_OBJ_IVSHMEM => "ivshmem",
        PCI_OBJ_IOMMU_TEST => "iommu-testdev",
        PCI_OBJ_DISPLAY_AUX => "display-aux",
        PCI_OBJ_XHCI => "usb-xhci",
        PCI_OBJ_NVME => "nvme-ro",
        PCI_OBJ_NET => "net-eth",
        PCI_OBJ_IGPU => "igpu-gop",
        _ => "pci",
    }
}
