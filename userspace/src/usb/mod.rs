//! Ring-3 USB / xHCI Protocol Constants, Descriptor Parsers, and Telemetry Types.

use core::mem::size_of;
use core::ptr::{read_volatile, write_volatile};

use crate::abi_ext::{sys_dev_map_window, sys_dma_alloc, sys_dma_alloc_2m};
use crate::{rdtsc_user, sys_cap_revoke, sys_yield, ENOSPC, ETIMEDOUT};

pub mod hid;
pub mod msc;
pub mod xhci;

pub const USB_CLASS_HID: u8 = 0x03;
pub const USB_SUBCLASS_BOOT: u8 = 0x01;
pub const USB_PROTO_KBD: u8 = 0x01;
pub const USB_PROTO_MOUSE: u8 = 0x02;

pub const USB_CLASS_MASS_STORAGE: u8 = 0x08;
pub const USB_SUBCLASS_SCSI: u8 = 0x06;
pub const USB_PROTO_BOT: u8 = 0x50;

pub const USB_DESC_DEVICE: u8 = 1;
pub const USB_DESC_CONFIG: u8 = 2;
pub const USB_DESC_INTERFACE: u8 = 4;
pub const USB_DESC_ENDPOINT: u8 = 5;

pub const USB_FLAG_READY: u16 = 1 << 0;
pub const USB_FLAG_MSI_BOUND: u16 = 1 << 1;
pub const USB_FLAG_BIOS_HANDOFF: u16 = 1 << 2;
pub const USB_FLAG_CSZ64: u16 = 1 << 3;
pub const USB_FLAG_KBD_READY: u16 = 1 << 4;
pub const USB_FLAG_MOUSE_READY: u16 = 1 << 5;
pub const USB_FLAG_MSC_READY: u16 = 1 << 6;
pub const USB_FLAG_DRYFS_VERIFIED: u16 = 1 << 7;
pub const USB_FLAG_RO_POLICY: u16 = 1 << 8;
pub const USB_FLAG_IOMMU_PROTECTED: u16 = 1 << 9;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UsbDeviceSummary {
    pub port: u8,
    pub slot_id: u8,
    pub speed: u8,
    pub class_code: u8,
    pub subclass: u8,
    pub proto: u8,
    pub configured: u8,
    pub iface_num: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub ep_in_addr: u8,
    pub ep_out_addr: u8,
    pub _pad: u16,
}

const _: () = assert!(size_of::<UsbDeviceSummary>() == 16);

/// Compact USB/xHCI/HID/MSC telemetry payload returned over `USB_OP_QUERY_SUMMARY` (`<= 256` B).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsbSummaryInfo {
    pub controller_vid: u16,
    pub controller_did: u16,
    pub controller_bdf: u16,
    pub state_flags: u16,
    pub max_ports: u8,
    pub connected_ports: u8,
    pub active_slots: u8,
    pub device_count: u8,
    pub msc_lun: u8,
    pub msc_part2_idx: u8,
    pub msc_ro: u8,
    pub _pad0: u8,
    pub irq_count: u64,
    pub kbd_reports: u64,
    pub kbd_events: u64,
    pub mouse_reports: u64,
    pub mouse_events: u64,
    pub wm_drops: u64,
    pub msc_total_sectors: u64,
    pub msc_part2_start: u64,
    pub msc_part2_sectors: u64,
    pub msc_reads: u64,
    pub msc_writes: u64,
    pub msc_io_errs: u64,
    pub msc_rofs_rejects: u64,
    pub devices: [UsbDeviceSummary; 4],
    pub msc_vendor_product: [u8; 24],
}

const _: () = assert!(size_of::<UsbSummaryInfo>() <= 256);

impl UsbSummaryInfo {
    pub const fn empty() -> Self {
        Self {
            controller_vid: 0,
            controller_did: 0,
            controller_bdf: 0,
            state_flags: 0,
            max_ports: 0,
            connected_ports: 0,
            active_slots: 0,
            device_count: 0,
            msc_lun: 0,
            msc_part2_idx: 0,
            msc_ro: 1,
            _pad0: 0,
            irq_count: 0,
            kbd_reports: 0,
            kbd_events: 0,
            mouse_reports: 0,
            mouse_events: 0,
            wm_drops: 0,
            msc_total_sectors: 0,
            msc_part2_start: 0,
            msc_part2_sectors: 0,
            msc_reads: 0,
            msc_writes: 0,
            msc_io_errs: 0,
            msc_rofs_rejects: 0,
            devices: [UsbDeviceSummary {
                port: 0,
                slot_id: 0,
                speed: 0,
                class_code: 0,
                subclass: 0,
                proto: 0,
                configured: 0,
                iface_num: 0,
                vendor_id: 0,
                product_id: 0,
                ep_in_addr: 0,
                ep_out_addr: 0,
                _pad: 0,
            }; 4],
            msc_vendor_product: [0; 24],
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `UsbSummaryInfo` is `#[repr(C)]` POD (`<= 256` bytes) with zeroed padding.
        unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>()) }
    }

    pub fn from_bytes(raw: &[u8]) -> Option<Self> {
        if raw.len() < size_of::<Self>() {
            return None;
        }
        let mut out = Self::empty();
        // SAFETY: `UsbSummaryInfo` is `#[repr(C)]` POD (`<= 256` bytes).
        unsafe {
            core::ptr::copy_nonoverlapping(
                raw.as_ptr(),
                (&raw mut out).cast::<u8>(),
                size_of::<Self>(),
            );
        }
        Some(out)
    }
}

#[derive(Clone, Copy)]
pub struct ParsedInterface {
    pub iface_num: u8,
    pub class_code: u8,
    pub subclass: u8,
    pub proto: u8,
    pub ep_in_addr: u8,
    pub ep_in_attr: u8,
    pub ep_in_mps: u16,
    pub ep_in_interval: u8,
    pub ep_out_addr: u8,
    pub ep_out_attr: u8,
    pub ep_out_mps: u16,
    pub ep_out_interval: u8,
}

impl ParsedInterface {
    pub const EMPTY: Self = Self {
        iface_num: 0,
        class_code: 0,
        subclass: 0,
        proto: 0,
        ep_in_addr: 0,
        ep_in_attr: 0,
        ep_in_mps: 0,
        ep_in_interval: 0,
        ep_out_addr: 0,
        ep_out_attr: 0,
        ep_out_mps: 0,
        ep_out_interval: 0,
    };
}

/// Parse up to `out.len()` USB interfaces + Bulk/Interrupt endpoints from a Config Descriptor.
pub fn parse_config_interfaces(cfg: &[u8], out: &mut [ParsedInterface]) -> (u8, usize) {
    if cfg.len() < 9 || cfg[1] != USB_DESC_CONFIG {
        return (1, 0);
    }
    let cfg_val = cfg[5].max(1);
    let total_len = (u16::from_le_bytes([cfg[2], cfg[3]]) as usize).min(cfg.len());
    let mut off = 0usize;
    let mut cur: Option<usize> = None;
    let mut count = 0usize;

    while off + 2 <= total_len {
        let len = cfg[off] as usize;
        if len < 2 || off + len > total_len {
            break;
        }
        let dtype = cfg[off + 1];
        if dtype == USB_DESC_INTERFACE && len >= 9 {
            if count < out.len() {
                out[count] = ParsedInterface {
                    iface_num: cfg[off + 2],
                    class_code: cfg[off + 5],
                    subclass: cfg[off + 6],
                    proto: cfg[off + 7],
                    ..ParsedInterface::EMPTY
                };
                cur = Some(count);
                count += 1;
            } else {
                cur = None;
            }
        } else if dtype == USB_DESC_ENDPOINT && len >= 7 {
            if let Some(idx) = cur {
                let addr = cfg[off + 2];
                let attr = cfg[off + 3] & 0x03;
                let mps = u16::from_le_bytes([cfg[off + 4], cfg[off + 5]]) & 0x07ff;
                let interval = cfg[off + 6];
                if (addr & 0x80) != 0 {
                    if out[idx].ep_in_addr == 0 {
                        out[idx].ep_in_addr = addr;
                        out[idx].ep_in_attr = attr;
                        out[idx].ep_in_mps = mps;
                        out[idx].ep_in_interval = interval;
                    }
                } else if out[idx].ep_out_addr == 0 {
                    out[idx].ep_out_addr = addr;
                    out[idx].ep_out_attr = attr;
                    out[idx].ep_out_mps = mps;
                    out[idx].ep_out_interval = interval;
                }
            }
        }
        off += len;
    }
    (cfg_val, count)
}

#[inline]
pub fn setup_packet(req_type: u8, req: u8, val: u16, idx: u16, len: u16) -> u64 {
    (req_type as u64)
        | ((req as u64) << 8)
        | ((val as u64) << 16)
        | ((idx as u64) << 32)
        | ((len as u64) << 48)
}

#[inline]
pub const fn endpoint_dci(ep_addr: u8) -> u8 {
    let ep_num = ep_addr & 0x0f;
    let dir_in = if (ep_addr & 0x80) != 0 { 1 } else { 0 };
    ep_num * 2 + dir_in
}

#[inline]
pub const fn xhci_ep_type(attr: u8, is_in: bool) -> u8 {
    match (attr & 3, is_in) {
        (2, false) => 2, // Bulk OUT
        (2, true) => 6,  // Bulk IN
        (3, false) => 3, // Interrupt OUT
        (3, true) => 7,  // Interrupt IN
        _ => 4,          // Control
    }
}

#[inline]
pub fn encode_xhci_interval(speed: u8, attr: u8, b_interval: u8) -> u8 {
    if (attr & 3) != 3 {
        return 0;
    }
    if speed >= 3 {
        b_interval.saturating_sub(1).clamp(0, 15)
    } else {
        let frames = (b_interval as u32).max(1) * 8;
        let mut pow = 3u8;
        while (1u32 << pow) < frames && pow < 10 {
            pow += 1;
        }
        pow
    }
}

pub const MMIO_WIN_BASE_VA: u64 = 0x1010_0000;
pub const MMIO_WIN_STRIDE: u64 = 0x1_0000;
pub const DMA_CTRL_VA: u64 = 0x1110_0000;
pub const DMA_RING_VA: u64 = 0x1110_1000;
pub const DMA_INCTX_VA: u64 = 0x1110_2000;
pub const DMA_DEVCTX_VA: u64 = 0x1110_3000;
pub const DMA_EP0RING_VA: u64 = 0x1110_7000;
pub const DMA_EPRING_VA: u64 = 0x1110_9000;
pub const DMA_CTRL_BOUNCE_VA: u64 = 0x1110_b000;
pub const DMA_BULK_BOUNCE_VA: u64 = 0x1110_c000;
pub const DMA_SCRATCH_4K_VA: u64 = 0x1120_0000;
pub const DMA_SCRATCH_2M_VA: u64 = 0x1200_0000;

pub const MAX_SLOTS: usize = 8;
pub const MAX_EP_RINGS: usize = 8;
const TRB_LINK: u32 = 6;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Trb {
    pub dw0: u32,
    pub dw1: u32,
    pub dw2: u32,
    pub dw3: u32,
}

#[derive(Clone, Copy)]
pub struct XhciEvent {
    pub trb_type: u8,
    pub cc: u8,
    pub slot_id: u8,
    pub ep_id: u8,
    pub residual: u32,
}

#[derive(Clone, Copy)]
pub struct TrbRing {
    pub va: u64,
    pub pa: u64,
    pub usable: usize,
    pub enq: usize,
    pub ccs: u8,
}

impl TrbRing {
    pub const EMPTY: Self = Self { va: 0, pa: 0, usable: 0, enq: 0, ccs: 1 };

    pub fn reset(&mut self) {
        if self.va != 0 {
            // SAFETY: Ring memory was mapped via `sys_dma_alloc` with `(usable + 1) * 16` bytes.
            unsafe { core::ptr::write_bytes(self.va as *mut u8, 0, (self.usable + 1) * 16) };
        }
        self.enq = 0;
        self.ccs = 1;
    }

    pub fn enqueue(&mut self, dw0: u32, dw1: u32, dw2: u32, dw3_no_c: u32) {
        let trb = Trb { dw0, dw1, dw2, dw3: (dw3_no_c & !1) | u32::from(self.ccs) };
        // SAFETY: `self.enq < self.usable` within pinned DMA ring page.
        unsafe { write_volatile((self.va as *mut Trb).add(self.enq), trb) };
        self.enq += 1;
        if self.enq == self.usable {
            let link = Trb {
                dw0: self.pa as u32,
                dw1: (self.pa >> 32) as u32,
                dw2: 0,
                dw3: (TRB_LINK << 10) | (1 << 1) | u32::from(self.ccs),
            };
            // SAFETY: Slot `self.usable` is the trailing Link TRB inside the ring allocation.
            unsafe { write_volatile((self.va as *mut Trb).add(self.usable), link) };
            self.enq = 0;
            self.ccs ^= 1;
        }
    }

    pub fn dequeue_ptr_dcs(&self) -> u64 {
        (self.pa + (self.enq as u64) * 16) | u64::from(self.ccs)
    }
}

/// # Safety
/// Caller must pass a mapped MMIO page virtual address and aligned register offset.
#[inline]
pub unsafe fn mmio_r32(base: u64, off: usize) -> u32 {
    unsafe { read_volatile((base as usize + off) as *const u32) }
}

/// # Safety
/// Caller must pass a mapped MMIO page virtual address and aligned register offset.
#[inline]
pub unsafe fn mmio_w32(base: u64, off: usize, v: u32) {
    unsafe { write_volatile((base as usize + off) as *mut u32, v) }
}

/// # Safety
/// Caller must pass a mapped MMIO page virtual address and 8-byte register offset.
#[inline]
pub unsafe fn mmio_w64(base: u64, off: usize, v: u64) {
    unsafe {
        write_volatile((base as usize + off) as *mut u32, v as u32);
        write_volatile((base as usize + off + 4) as *mut u32, (v >> 32) as u32);
    }
}

pub struct BarPageMapper {
    pub pci_cap: u64,
    pub suboff: u64,
    pub entries: [(u32, u64); 6],
    pub count: usize,
}

impl BarPageMapper {
    pub fn map_page(&mut self, page_idx: u32) -> Result<u64, isize> {
        for i in 0..self.count {
            if self.entries[i].0 == page_idx {
                return Ok(self.entries[i].1);
            }
        }
        if self.count >= self.entries.len() {
            return Err(ENOSPC);
        }
        let va = MMIO_WIN_BASE_VA + (self.count as u64) * MMIO_WIN_STRIDE;
        sys_dev_map_window(self.pci_cap, va, 0, false, 1, page_idx)?;
        let eff = va + self.suboff;
        self.entries[self.count] = (page_idx, eff);
        self.count += 1;
        Ok(eff)
    }
}

pub fn perform_bios_handoff(mapper: &mut BarPageMapper, mut off: usize, tsc_mhz: u64) -> bool {
    for _ in 0..32 {
        let Ok(page_va) = mapper.map_page((off / 4096) as u32) else {
            return false;
        };
        let loff = off & 0xfff;
        let hdr = unsafe { mmio_r32(page_va, loff) };
        if (hdr & 0xff) == 1 {
            if (hdr & (1 << 16)) != 0 {
                unsafe { mmio_w32(page_va, loff, hdr | (1 << 24)) };
                let deadline = rdtsc_user().saturating_add(500 * 1000 * tsc_mhz.max(1000));
                while rdtsc_user() < deadline {
                    let cur = unsafe { mmio_r32(page_va, loff) };
                    if (cur & (1 << 16)) == 0 && (cur & (1 << 24)) != 0 {
                        unsafe { mmio_w32(page_va, loff + 4, 0) };
                        return true;
                    }
                    let _ = sys_yield();
                }
                return false;
            }
            return true;
        }
        let next = ((hdr >> 8) & 0xff) as usize;
        if next == 0 {
            break;
        }
        off += next * 4;
    }
    true
}

pub fn wait_op_mask(
    op_va: u64,
    reg: usize,
    mask: u32,
    want: u32,
    tsc_mhz: u64,
    timeout_ms: u64,
) -> Result<(), isize> {
    let deadline = rdtsc_user().saturating_add(timeout_ms * 1000 * tsc_mhz.max(1000));
    loop {
        if (unsafe { mmio_r32(op_va, reg) } & mask) == want {
            return Ok(());
        }
        if rdtsc_user() >= deadline {
            return Err(ETIMEDOUT);
        }
        let _ = sys_yield();
    }
}

pub struct XhciDmaLayout {
    pub dma_caps: [u64; 32],
    pub dma_cap_count: usize,
    pub ctrl_pa: u64,
    pub ring_pa: u64,
    pub inctx_pa: u64,
    pub dev_ctx_pas: [u64; MAX_SLOTS],
    pub dev_ctx_vas: [u64; MAX_SLOTS],
    pub ep0_rings: [TrbRing; MAX_SLOTS],
    pub ep_rings: [TrbRing; MAX_EP_RINGS],
    pub ctrl_bounce_pa: u64,
    pub bulk_bounce_pa: u64,
}

pub fn alloc_xhci_dma_layout(pci_cap: u64, max_sp: u32) -> Result<XhciDmaLayout, isize> {
    let mut dma_caps = [0u64; 32];
    let mut dma_cnt = 0usize;
    let mut alloc_4k = |va: u64| -> Result<u64, isize> {
        let (cap_h, pa) = sys_dma_alloc(pci_cap, va)?;
        if dma_cnt >= 32 {
            let _ = sys_cap_revoke(cap_h, 1);
            return Err(ENOSPC);
        }
        dma_caps[dma_cnt] = cap_h;
        dma_cnt += 1;
        // SAFETY: Pinned 4 KiB DMA frame is mapped `USER_RW` at `va`.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, 4096) };
        Ok(pa)
    };

    let ctrl_pa = alloc_4k(DMA_CTRL_VA)?;
    let ring_pa = alloc_4k(DMA_RING_VA)?;
    let inctx_pa = alloc_4k(DMA_INCTX_VA)?;

    let mut dev_ctx_pas = [0u64; MAX_SLOTS];
    let mut dev_ctx_vas = [0u64; MAX_SLOTS];
    for p in 0..4usize {
        let pa = alloc_4k(DMA_DEVCTX_VA + (p as u64) * 4096)?;
        dev_ctx_pas[p * 2] = pa;
        dev_ctx_vas[p * 2] = DMA_DEVCTX_VA + (p as u64) * 4096;
        dev_ctx_pas[p * 2 + 1] = pa + 2048;
        dev_ctx_vas[p * 2 + 1] = DMA_DEVCTX_VA + (p as u64) * 4096 + 2048;
    }

    let mut ep0_rings = [TrbRing::EMPTY; MAX_SLOTS];
    for p in 0..2usize {
        let va_base = DMA_EP0RING_VA + (p as u64) * 4096;
        let pa_base = alloc_4k(va_base)?;
        for sub in 0..4usize {
            ep0_rings[p * 4 + sub] = TrbRing {
                va: va_base + (sub as u64) * 1024,
                pa: pa_base + (sub as u64) * 1024,
                usable: 63,
                enq: 0,
                ccs: 1,
            };
        }
    }

    let mut ep_rings = [TrbRing::EMPTY; MAX_EP_RINGS];
    for p in 0..2usize {
        let va_base = DMA_EPRING_VA + (p as u64) * 4096;
        let pa_base = alloc_4k(va_base)?;
        for sub in 0..4usize {
            ep_rings[p * 4 + sub] = TrbRing {
                va: va_base + (sub as u64) * 1024,
                pa: pa_base + (sub as u64) * 1024,
                usable: 63,
                enq: 0,
                ccs: 1,
            };
        }
    }

    let ctrl_bounce_pa = alloc_4k(DMA_CTRL_BOUNCE_VA)?;
    let bulk_bounce_pa = alloc_4k(DMA_BULK_BOUNCE_VA)?;

    if max_sp > 0 {
        let sp_arr_va = DMA_CTRL_VA + 0x880;
        let sp_arr_pa = ctrl_pa + 0x880;
        if max_sp <= 16 {
            for i in 0..max_sp as usize {
                let sp_pa = alloc_4k(DMA_SCRATCH_4K_VA + (i as u64) * 4096)?;
                unsafe { write_volatile((sp_arr_va as *mut u64).add(i), sp_pa) };
            }
        } else {
            let (cap_h, pa_2m) = sys_dma_alloc_2m(pci_cap, DMA_SCRATCH_2M_VA)?;
            dma_caps[dma_cnt] = cap_h;
            dma_cnt += 1;
            unsafe { core::ptr::write_bytes(DMA_SCRATCH_2M_VA as *mut u8, 0, 2 * 1024 * 1024) };
            for i in 0..(max_sp as usize).min(128) {
                unsafe { write_volatile((sp_arr_va as *mut u64).add(i), pa_2m + (i as u64) * 4096) };
            }
        }
        unsafe { write_volatile(DMA_CTRL_VA as *mut u64, sp_arr_pa) };
    }

    Ok(XhciDmaLayout {
        dma_caps,
        dma_cap_count: dma_cnt,
        ctrl_pa,
        ring_pa,
        inctx_pa,
        dev_ctx_pas,
        dev_ctx_vas,
        ep0_rings,
        ep_rings,
        ctrl_bounce_pa,
        bulk_bounce_pa,
    })
}
