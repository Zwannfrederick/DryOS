//! Ring-3 xHCI 1.x Host Controller Driver (`dry-usb`, `SecDomain::RootSystem`).
//!
//! Covers BAR0 sparse page mapping (MSI-X safe), BIOS->OS handoff (`USBLEGSUP`), DCBAA + Scratchpad
//! setup, Command/Event/Transfer TRB rings with Link wrapping, slot/endpoint bring-up, bounded-TSC
//! command & transfer execution, and clean `Reset Endpoint` stall recovery.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::abi_ext::{sys_dev_query_bars, UserPciDeviceInfo};
use crate::usb::{
    alloc_xhci_dma_layout, encode_xhci_interval, endpoint_dci, mmio_r32 as r32, mmio_w32 as w32,
    mmio_w64 as w64, perform_bios_handoff, setup_packet, wait_op_mask, xhci_ep_type, BarPageMapper,
    ParsedInterface, Trb, TrbRing, XhciDmaLayout, XhciEvent, DMA_BULK_BOUNCE_VA,
    DMA_CTRL_BOUNCE_VA, DMA_CTRL_VA, DMA_INCTX_VA, DMA_RING_VA, MAX_EP_RINGS, MAX_SLOTS,
    USB_FLAG_BIOS_HANDOFF, USB_FLAG_CSZ64, USB_FLAG_IOMMU_PROTECTED, USB_FLAG_MSI_BOUND,
    USB_FLAG_READY,
};
use crate::{
    rdtsc_user, sys_cap_revoke, sys_irq_ack, sys_irq_bind, sys_notify_create, sys_notify_wait,
    sys_sysinfo, sys_yield, ENOSPC, EPIPE, ETIMEDOUT, IRQF_ONESHOT, IRQ_ACK_UNBIND,
    IRQ_ACK_UNMASK, UserSysInfo,
};

const EIO: isize = -5;
const ENODEV: isize = -19;

pub type ConfiguredEps = (Option<(u8, usize)>, Option<(u8, usize)>);

const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEV: u32 = 11;
const TRB_CONFIG_EP: u32 = 12;
const TRB_EVAL_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_STOP_EP: u32 = 15;
const TRB_SET_TR_DEQ: u32 = 16;

const EVT_TRANSFER: u8 = 32;
const EVT_CMD_COMP: u8 = 33;
const EVT_PORT_CHG: u8 = 34;

pub const CC_SUCCESS: u8 = 1;
pub const CC_STALL: u8 = 6;
pub const CC_SHORT_PKT: u8 = 13;

pub struct XhciController {
    pci_cap: u64,
    pub tsc_mhz: u64,
    mapper: BarPageMapper,
    op_va: u64,
    op_off: usize,
    db_va: u64,
    rt_va: u64,
    pub max_ports: u8,
    pub max_slots: u8,
    ctx_sz: usize,
    pub csz64: bool,
    pub bios_handoff_ok: bool,
    pub msi_bound: bool,
    pub iommu_protected: bool,
    pub vid: u16,
    pub did: u16,
    pub bdf: u16,
    notif_cap: u64,
    pub irq_count: u64,
    dma_caps: [u64; 32],
    dma_cap_count: usize,
    dcbaa_va: u64,
    inctx_pa: u64,
    dev_ctx_pas: [u64; MAX_SLOTS],
    dev_ctx_vas: [u64; MAX_SLOTS],
    evt_va: u64,
    evt_pa: u64,
    evt_deq: usize,
    evt_ccs: u8,
    cmd_ring: TrbRing,
    ep0_rings: [TrbRing; MAX_SLOTS],
    ep_rings: [TrbRing; MAX_EP_RINGS],
    ep_ring_used: usize,
    pub ctrl_bounce_pa: u64,
    pub bulk_bounce_pa: u64,
    async_buf_used: usize,
    queued_evts: [XhciEvent; 16],
    queued_cnt: usize,
}

impl XhciController {
    pub fn init(pci_cap: u64) -> Result<Self, isize> {
        let mut sys = UserSysInfo::default();
        let _ = sys_sysinfo(&mut sys);
        let tsc_mhz = sys.tsc_mhz.max(1000);

        let mut pci_info = UserPciDeviceInfo::default();
        sys_dev_query_bars(pci_cap, &mut pci_info)?;
        if pci_info.bars[0].size_bytes < 0x1000 {
            return Err(ENODEV);
        }

        let mut mapper = BarPageMapper {
            pci_cap,
            suboff: pci_info.bars[0].page_suboff as u64,
            entries: [(0, 0); 6],
            count: 0,
        };
        let p0 = mapper.map_page(0)?;
        let caplen = (unsafe { r32(p0, 0x00) } & 0xff) as usize;
        let hcsparams1 = unsafe { r32(p0, 0x04) };
        let hcsparams2 = unsafe { r32(p0, 0x08) };
        let hccparams1 = unsafe { r32(p0, 0x10) };
        let dboff = (unsafe { r32(p0, 0x14) } & !0x3) as usize;
        let rtsoff = (unsafe { r32(p0, 0x18) } & !0x1f) as usize;

        let max_slots = ((hcsparams1 & 0xff) as usize).clamp(1, MAX_SLOTS) as u8;
        let max_ports = ((hcsparams1 >> 24) & 0xff) as u8;
        let csz64 = (hccparams1 & (1 << 2)) != 0;
        let ctx_sz = if csz64 { 64 } else { 32 };
        let max_sp = (((hcsparams2 >> 27) & 0x1f) << 5) | ((hcsparams2 >> 21) & 0x1f);

        let op_page = (caplen / 4096) as u32;
        let op_va = mapper.map_page(op_page)? + (caplen & 0xfff) as u64;
        let db_va = mapper.map_page((dboff / 4096) as u32)? + (dboff & 0xfff) as u64;
        let rt_va = mapper.map_page((rtsoff / 4096) as u32)? + (rtsoff & 0xfff) as u64;

        let mut bios_handoff_ok = true;
        let xecp = ((hccparams1 >> 16) as usize) * 4;
        if xecp != 0 {
            bios_handoff_ok = perform_bios_handoff(&mut mapper, xecp, tsc_mhz);
        }

        // Halt and reset controller cleanly before programming DCBAAP/CRCR/ERSTBA.
        let cmd = unsafe { r32(op_va, 0x00) };
        if (cmd & 1) != 0 {
            unsafe { w32(op_va, 0x00, cmd & !1) };
            let _ = wait_op_mask(op_va, 0x04, 1, 1, tsc_mhz, 100);
        }
        unsafe { w32(op_va, 0x00, 1 << 1) }; // HCRST
        wait_op_mask(op_va, 0x00, 1 << 1, 0, tsc_mhz, 500)?;
        wait_op_mask(op_va, 0x04, 1 << 11, 0, tsc_mhz, 500)?; // CNR == 0

        let XhciDmaLayout {
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
        } = alloc_xhci_dma_layout(pci_cap, max_sp)?;

        let cmd_ring_pa = ring_pa;
        let evt_ring_va = DMA_RING_VA + 0x800;
        let evt_ring_pa = ring_pa + 0x800;
        let erst_va = DMA_CTRL_VA + 0x800;
        let erst_pa = ctrl_pa + 0x800;
        unsafe {
            w64(erst_va, 0, evt_ring_pa);
            w32(erst_va, 8, 128);
            w32(erst_va, 12, 0);
        }

        unsafe {
            w32(op_va, 0x38, max_slots as u32); // CONFIG.MaxSlotsEn
            w64(op_va, 0x30, ctrl_pa); // DCBAAP
            w64(op_va, 0x18, cmd_ring_pa | 1); // CRCR (RCS=1)
            w32(rt_va, 0x28, 1); // ERSTSZ
            w64(rt_va, 0x38, evt_ring_pa | (1 << 3)); // ERDP + EHB
            w64(rt_va, 0x30, erst_pa); // ERSTBA
            w32(rt_va, 0x24, 0x00fa); // IMOD
            w32(rt_va, 0x20, 0x3); // IMAN = IE | IP(W1C)
        }

        let notif_cap = sys_notify_create(0).unwrap_or(0);
        let msi_bound = if notif_cap != 0 {
            sys_irq_bind(pci_cap, notif_cap, IRQF_ONESHOT).is_ok()
        } else {
            false
        };

        fence(Ordering::SeqCst);
        unsafe { w32(op_va, 0x00, 1 | (1 << 2)) }; // USBCMD: RS=1, INTE=1
        wait_op_mask(op_va, 0x04, 1, 0, tsc_mhz, 250)?; // wait HCH=0

        Ok(Self {
            pci_cap,
            tsc_mhz,
            mapper,
            op_va,
            op_off: caplen,
            db_va,
            rt_va,
            max_ports,
            max_slots,
            ctx_sz,
            csz64,
            bios_handoff_ok,
            msi_bound,
            iommu_protected: pci_info.iommu_protected != 0,
            vid: pci_info.vendor_id,
            did: pci_info.device_id,
            bdf: pci_info.bus_dev_fn,
            notif_cap,
            irq_count: 0,
            dma_caps,
            dma_cap_count: dma_cnt,
            dcbaa_va: DMA_CTRL_VA,
            inctx_pa,
            dev_ctx_pas,
            dev_ctx_vas,
            evt_va: evt_ring_va,
            evt_pa: evt_ring_pa,
            evt_deq: 0,
            evt_ccs: 1,
            cmd_ring: TrbRing { va: DMA_RING_VA, pa: cmd_ring_pa, usable: 127, enq: 0, ccs: 1 },
            ep0_rings,
            ep_rings,
            ep_ring_used: 0,
            ctrl_bounce_pa,
            bulk_bounce_pa,
            async_buf_used: 0,
            queued_evts: [XhciEvent { trb_type: 0, cc: 0, slot_id: 0, ep_id: 0, residual: 0 }; 16],
            queued_cnt: 0,
        })
    }

    pub fn summary_flags(&self) -> u16 {
        USB_FLAG_READY
            | if self.msi_bound { USB_FLAG_MSI_BOUND } else { 0 }
            | if self.bios_handoff_ok { USB_FLAG_BIOS_HANDOFF } else { 0 }
            | if self.csz64 { USB_FLAG_CSZ64 } else { 0 }
            | if self.iommu_protected { USB_FLAG_IOMMU_PROTECTED } else { 0 }
    }

    pub fn sleep_ms(&self, ms: u64) {
        let deadline = rdtsc_user().saturating_add(ms * 1000 * self.tsc_mhz.max(1000));
        while rdtsc_user() < deadline {
            let _ = sys_yield();
        }
    }

    fn portsc_va(&mut self, port_idx: u8) -> Result<(u64, usize), isize> {
        let abs = self.op_off + 0x400 + (port_idx as usize) * 0x10;
        let va = self.mapper.map_page((abs / 4096) as u32)?;
        Ok((va, abs & 0xfff))
    }

    pub fn probe_and_reset_port(&mut self, port_idx: u8) -> Option<u8> {
        let Ok((va, off)) = self.portsc_va(port_idx) else {
            return None;
        };
        let mut sc = unsafe { r32(va, off) };
        if (sc & 1) == 0 {
            return None; // CCS == 0: no device connected.
        }
        // Clear pending W1C port change bits while keeping PP(bit 9) and NEVER setting PED(bit 1).
        unsafe { w32(va, off, (sc & (1 << 9)) | 0x00fe_0000) };
        sc = unsafe { r32(va, off) };
        if (sc & (1 << 1)) == 0 {
            unsafe { w32(va, off, (sc & (1 << 9)) | (1 << 4)) }; // PR=1
            let deadline = rdtsc_user().saturating_add(250 * 1000 * self.tsc_mhz.max(1000));
            while rdtsc_user() < deadline {
                let cur = unsafe { r32(va, off) };
                if (cur & (1 << 4)) == 0 && ((cur & (1 << 21)) != 0 || (cur & (1 << 1)) != 0) {
                    unsafe { w32(va, off, (cur & (1 << 9)) | 0x00fe_0000) };
                    break;
                }
                let _ = sys_yield();
            }
            self.sleep_ms(12);
        }
        let final_sc = unsafe { r32(va, off) };
        if (final_sc & 0x3) != 0x3 {
            return None;
        }
        let speed = ((final_sc >> 10) & 0x0f) as u8;
        Some(speed.max(1))
    }

    pub fn ctrl_bounce_slice(&self) -> &[u8] {
        // SAFETY: First 2048 B of `DMA_CTRL_BOUNCE_VA` is owned by synchronous EP0 control xfers.
        unsafe { core::slice::from_raw_parts(DMA_CTRL_BOUNCE_VA as *const u8, 2048) }
    }

    pub fn bulk_bounce_slice(&self) -> &[u8] {
        // SAFETY: `DMA_BULK_BOUNCE_VA` is a dedicated 4 KiB page for BOT bulk transfers.
        unsafe { core::slice::from_raw_parts(DMA_BULK_BOUNCE_VA as *const u8, 4096) }
    }

    pub fn bulk_bounce_mut(&mut self) -> &mut [u8] {
        // SAFETY: `DMA_BULK_BOUNCE_VA` is a dedicated 4 KiB page for BOT bulk transfers.
        unsafe { core::slice::from_raw_parts_mut(DMA_BULK_BOUNCE_VA as *mut u8, 4096) }
    }

    pub fn cbw_ptr_pa(&self) -> (u64, u64) {
        (DMA_CTRL_BOUNCE_VA + 0x900, self.ctrl_bounce_pa + 0x900)
    }

    pub fn csw_ptr_pa(&self) -> (u64, u64) {
        (DMA_CTRL_BOUNCE_VA + 0x940, self.ctrl_bounce_pa + 0x940)
    }

    pub fn alloc_hid_buf(&mut self) -> Option<(u64, u64)> {
        if self.async_buf_used >= 4 {
            return None;
        }
        let off = 0x800 + (self.async_buf_used as u64) * 64;
        self.async_buf_used += 1;
        Some((DMA_CTRL_BOUNCE_VA + off, self.ctrl_bounce_pa + off))
    }

    pub fn alloc_ep_ring(&mut self) -> Option<usize> {
        if self.ep_ring_used >= MAX_EP_RINGS {
            return None;
        }
        let idx = self.ep_ring_used;
        self.ep_ring_used += 1;
        self.ep_rings[idx].reset();
        Some(idx)
    }

    #[inline]
    fn ring_db(&self, target: u8, dci: u8) {
        fence(Ordering::SeqCst);
        // SAFETY: `db_va` maps Doorbell Array within BAR0.
        unsafe { w32(self.db_va, (target as usize) * 4, dci as u32) };
    }

    pub fn poll_raw_event(&mut self) -> Option<XhciEvent> {
        fence(Ordering::Acquire);
        // SAFETY: `evt_deq < 128` inside Event Ring DMA half-page.
        let trb = unsafe { read_volatile((self.evt_va as *const Trb).add(self.evt_deq)) };
        if ((trb.dw3 & 1) as u8) != self.evt_ccs {
            return None;
        }
        self.evt_deq += 1;
        if self.evt_deq == 128 {
            self.evt_deq = 0;
            self.evt_ccs ^= 1;
        }
        let erdp = self.evt_pa + (self.evt_deq as u64) * 16;
        unsafe {
            w64(self.rt_va, 0x38, erdp | (1 << 3)); // ERDP + clear EHB
            w32(self.op_va, 0x04, 1 << 3); // Clear USBSTS.EINT
            w32(self.rt_va, 0x20, 0x3); // IMAN: keep IE=1, clear IP
        }
        self.irq_count = self.irq_count.wrapping_add(1);
        Some(XhciEvent {
            trb_type: ((trb.dw3 >> 10) & 0x3f) as u8,
            cc: ((trb.dw2 >> 24) & 0xff) as u8,
            slot_id: ((trb.dw3 >> 24) & 0xff) as u8,
            ep_id: ((trb.dw3 >> 16) & 0x1f) as u8,
            residual: trb.dw2 & 0x00ff_ffff,
        })
    }

    pub fn pop_event(&mut self) -> Option<XhciEvent> {
        if self.queued_cnt > 0 {
            let ev = self.queued_evts[0];
            for i in 1..self.queued_cnt {
                self.queued_evts[i - 1] = self.queued_evts[i];
            }
            self.queued_cnt -= 1;
            return Some(ev);
        }
        self.poll_raw_event()
    }

    fn take_matching_event(
        &mut self,
        want_type: u8,
        want_slot: u8,
        want_dci: u8,
    ) -> Option<XhciEvent> {
        while let Some(ev) = self.poll_raw_event() {
            if ev.trb_type == want_type
                && (want_type == EVT_CMD_COMP
                    || (ev.slot_id == want_slot && ev.ep_id == want_dci))
            {
                return Some(ev);
            }
            if ev.trb_type != EVT_PORT_CHG && self.queued_cnt < self.queued_evts.len() {
                self.queued_evts[self.queued_cnt] = ev;
                self.queued_cnt += 1;
            }
        }
        None
    }

    fn wait_event(
        &mut self,
        want_type: u8,
        want_slot: u8,
        want_dci: u8,
        timeout_ms: u64,
    ) -> Result<XhciEvent, isize> {
        let tsc_mhz = self.tsc_mhz.max(1000);
        let deadline = rdtsc_user().saturating_add(timeout_ms * 1000 * tsc_mhz);
        let wait_slice = tsc_mhz.saturating_mul(250);
        loop {
            for _ in 0..64 {
                if let Some(ev) = self.take_matching_event(want_type, want_slot, want_dci) {
                    return Ok(ev);
                }
                core::hint::spin_loop();
            }
            if rdtsc_user() >= deadline {
                return Err(ETIMEDOUT);
            }
            if self.msi_bound && self.notif_cap != 0 {
                self.ack_msi_if_bound();
                if let Some(ev) = self.take_matching_event(want_type, want_slot, want_dci) {
                    return Ok(ev);
                }
                let _ = sys_notify_wait(self.notif_cap, 0, wait_slice);
            } else {
                let _ = sys_yield();
            }
        }
    }

    fn submit_cmd(&mut self, dw0: u32, dw1: u32, dw3: u32, timeout_ms: u64) -> Result<(u8, u8), isize> {
        self.cmd_ring.enqueue(dw0, dw1, 0, dw3);
        self.ring_db(0, 0);
        let ev = self.wait_event(EVT_CMD_COMP, 0, 0, timeout_ms)?;
        Ok((ev.cc, ev.slot_id))
    }

    pub fn enable_slot(&mut self) -> Result<u8, isize> {
        let (cc, slot_id) = self.submit_cmd(0, 0, TRB_ENABLE_SLOT << 10, 1000)?;
        if cc != CC_SUCCESS || slot_id == 0 || (slot_id as usize) > MAX_SLOTS {
            return Err(EIO);
        }
        Ok(slot_id)
    }

    pub fn disable_slot(&mut self, slot_id: u8) {
        let _ = self.submit_cmd(0, 0, ((slot_id as u32) << 24) | (TRB_DISABLE_SLOT << 10), 500);
    }

    #[inline]
    pub fn address_device(&mut self, slot_id: u8, port_1based: u8, speed: u8) -> Result<(), isize> {
        self.address_device_routed(slot_id, port_1based, 0, speed, 0, 0)
    }

    pub fn address_device_routed(
        &mut self,
        slot_id: u8,
        port_1based: u8,
        route_string: u32,
        speed: u8,
        tt_hub_slot: u8,
        tt_port: u8,
    ) -> Result<(), isize> {
        let idx = (slot_id as usize) - 1;
        unsafe {
            core::ptr::write_bytes(self.dev_ctx_vas[idx] as *mut u8, 0, 2048);
            write_volatile((self.dcbaa_va as *mut u64).add(slot_id as usize), self.dev_ctx_pas[idx]);
            core::ptr::write_bytes(DMA_INCTX_VA as *mut u8, 0, 4096);
        }
        self.ep0_rings[idx].reset();

        let mps0: u32 = match speed {
            2 | 1 => 8, // LS / FS initial (updated via Evaluate Context if != 8)
            3 => 64,    // HS
            4 => 512,   // SS
            _ => 8,
        };
        let ep0_pa = self.ep0_rings[idx].pa;
        let slot_ctx = DMA_INCTX_VA + self.ctx_sz as u64;
        let ep0_ctx = DMA_INCTX_VA + (self.ctx_sz as u64) * 2;
        let tt_dw2 = if tt_hub_slot != 0 && speed <= 2 {
            u32::from(tt_hub_slot) | (u32::from(tt_port) << 8)
        } else {
            0
        };
        unsafe {
            w32(DMA_INCTX_VA, 0x04, 0x3); // Input Control Add Flags: A0 + A1
            w32(
                slot_ctx,
                0x00,
                (1 << 27) | ((speed as u32) << 20) | (route_string & 0x000f_ffff),
            ); // Context Entries=1
            w32(slot_ctx, 0x04, (port_1based as u32) << 16);
            if tt_dw2 != 0 {
                w32(slot_ctx, 0x08, tt_dw2);
            }
            w32(ep0_ctx, 0x04, (mps0 << 16) | (4 << 3) | (3 << 1)); // Control EP, CErr=3
            w64(ep0_ctx, 0x08, ep0_pa | 1); // TR Deq + DCS=1
            w32(ep0_ctx, 0x10, 8);
        }
        let pa = self.inctx_pa;
        let (cc, _) = self.submit_cmd(
            pa as u32,
            (pa >> 32) as u32,
            ((slot_id as u32) << 24) | (TRB_ADDRESS_DEV << 10),
            1000,
        )?;
        if cc != CC_SUCCESS { Err(EIO) } else { Ok(()) }
    }

    pub fn configure_hub_slot(
        &mut self,
        slot_id: u8,
        nbr_ports: u8,
        speed: u8,
        mtt: bool,
    ) -> Result<(), isize> {
        let slot_src = self.dev_ctx_vas[(slot_id as usize) - 1];
        let dw0_old = unsafe { r32(slot_src, 0x00) };
        let dw1_old = unsafe { r32(slot_src, 0x04) };
        let dw2_old = unsafe { r32(slot_src, 0x08) };
        let mtt_bit = if mtt && speed == 3 { 1u32 << 25 } else { 0 };
        let entries = (dw0_old >> 27).max(1);
        let new_dw0 = (dw0_old & !(0x1f << 27) & !(0x3 << 25))
            | (entries << 27)
            | (1u32 << 26)
            | mtt_bit;
        let new_dw1 = (dw1_old & 0x00ff_ffff) | (u32::from(nbr_ports) << 24);

        unsafe { core::ptr::write_bytes(DMA_INCTX_VA as *mut u8, 0, 4096) };
        let slot_dst = DMA_INCTX_VA + self.ctx_sz as u64;
        unsafe {
            w32(DMA_INCTX_VA, 0x04, 1); // A0: Slot Context
            w32(slot_dst, 0x00, new_dw0);
            w32(slot_dst, 0x04, new_dw1);
            w32(slot_dst, 0x08, dw2_old);
        }
        let pa = self.inctx_pa;
        let (cc, _) = self.submit_cmd(
            pa as u32,
            (pa >> 32) as u32,
            ((slot_id as u32) << 24) | (TRB_CONFIG_EP << 10),
            1000,
        )?;
        if cc == CC_SUCCESS {
            return Ok(());
        }
        let (cc2, _) = self.submit_cmd(
            pa as u32,
            (pa >> 32) as u32,
            ((slot_id as u32) << 24) | (TRB_EVAL_CTX << 10),
            1000,
        )?;
        if cc2 != CC_SUCCESS { Err(EIO) } else { Ok(()) }
    }

    pub fn eval_ep0_mps(&mut self, slot_id: u8, mps0: u16) -> Result<(), isize> {
        unsafe { core::ptr::write_bytes(DMA_INCTX_VA as *mut u8, 0, 4096) };
        let ep0_ctx = DMA_INCTX_VA + (self.ctx_sz as u64) * 2;
        unsafe {
            w32(DMA_INCTX_VA, 0x04, 1 << 1); // A1 only
            w32(ep0_ctx, 0x04, ((mps0 as u32) << 16) | (4 << 3) | (3 << 1));
        }
        let pa = self.inctx_pa;
        let (cc, _) = self.submit_cmd(
            pa as u32,
            (pa >> 32) as u32,
            ((slot_id as u32) << 24) | (TRB_EVAL_CTX << 10),
            1000,
        )?;
        if cc != CC_SUCCESS { Err(EIO) } else { Ok(()) }
    }

    pub fn configure_interface_eps(
        &mut self,
        slot_id: u8,
        speed: u8,
        iface: &ParsedInterface,
    ) -> Result<ConfiguredEps, isize> {
        unsafe { core::ptr::write_bytes(DMA_INCTX_VA as *mut u8, 0, 4096) };
        let mut add_flags: u32 = 1; // Slot Context
        let mut max_dci: u8 = 1;
        let mut in_pair = None;
        let mut out_pair = None;

        for is_in in [true, false] {
            let (addr, attr, mps, b_int) = if is_in {
                (iface.ep_in_addr, iface.ep_in_attr, iface.ep_in_mps, iface.ep_in_interval)
            } else {
                (iface.ep_out_addr, iface.ep_out_attr, iface.ep_out_mps, iface.ep_out_interval)
            };
            if addr == 0 {
                continue;
            }
            let dci = endpoint_dci(addr);
            if dci <= 1 || dci >= 32 {
                continue;
            }
            let ring_idx = self.alloc_ep_ring().ok_or(ENOSPC)?;
            let ring_pa = self.ep_rings[ring_idx].pa;
            let ep_ctx = DMA_INCTX_VA + ((dci as u64) + 1) * (self.ctx_sz as u64);
            let ep_type = xhci_ep_type(attr, is_in) as u32;
            let interval = encode_xhci_interval(speed, attr, b_int);
            let avg_len = if (attr & 3) == 3 { mps.min(64) as u32 } else { 512 };
            let max_esit = if (attr & 3) == 3 { mps as u32 } else { 0 };

            unsafe {
                w32(ep_ctx, 0x00, (interval as u32) << 16);
                w32(ep_ctx, 0x04, ((mps.max(8) as u32) << 16) | (ep_type << 3) | (3 << 1));
                w64(ep_ctx, 0x08, ring_pa | 1);
                w32(ep_ctx, 0x10, (max_esit << 16) | avg_len);
            }
            add_flags |= 1 << dci;
            max_dci = max_dci.max(dci);
            if is_in {
                in_pair = Some((dci, ring_idx));
            } else {
                out_pair = Some((dci, ring_idx));
            }
        }

        if add_flags == 1 {
            return Err(ENODEV);
        }
        let slot_src = self.dev_ctx_vas[(slot_id as usize) - 1];
        let dw0_old = unsafe { r32(slot_src, 0x00) } & 0x07ff_ffff;
        let dw1_old = unsafe { r32(slot_src, 0x04) };
        let dw2_old = unsafe { r32(slot_src, 0x08) };
        let slot_dst = DMA_INCTX_VA + self.ctx_sz as u64;
        unsafe {
            w32(DMA_INCTX_VA, 0x04, add_flags);
            w32(slot_dst, 0x00, dw0_old | ((max_dci as u32) << 27));
            w32(slot_dst, 0x04, dw1_old);
            if dw2_old != 0 {
                w32(slot_dst, 0x08, dw2_old);
            }
        }

        let pa = self.inctx_pa;
        let (cc, _) = self.submit_cmd(
            pa as u32,
            (pa >> 32) as u32,
            ((slot_id as u32) << 24) | (TRB_CONFIG_EP << 10),
            1000,
        )?;
        if cc != CC_SUCCESS { Err(EIO) } else { Ok((in_pair, out_pair)) }
    }

    pub fn control_xfer(
        &mut self,
        slot_id: u8,
        req_type: u8,
        req: u8,
        val: u16,
        idx: u16,
        len: u16,
    ) -> Result<usize, isize> {
        let sidx = (slot_id as usize) - 1;
        let setup = setup_packet(req_type, req, val, idx, len);
        let is_in = (req_type & 0x80) != 0;
        let trt: u32 = if len == 0 {
            0
        } else if is_in {
            3
        } else {
            2
        };
        if len > 0 {
            // SAFETY: Synchronous control bounce slice `0..2048`.
            unsafe { core::ptr::write_bytes(DMA_CTRL_BOUNCE_VA as *mut u8, 0, len.min(2048) as usize) };
        }
        let ring = &mut self.ep0_rings[sidx];
        ring.enqueue(
            setup as u32,
            (setup >> 32) as u32,
            8,
            (trt << 16) | (TRB_SETUP << 10) | (1 << 6), // IDT=1
        );
        if len > 0 {
            let pa = self.ctrl_bounce_pa;
            ring.enqueue(
                pa as u32,
                (pa >> 32) as u32,
                len as u32,
                (u32::from(is_in) << 16) | (TRB_DATA << 10),
            );
        }
        let status_dir_in = len == 0 || !is_in;
        ring.enqueue(
            0,
            0,
            0,
            (u32::from(status_dir_in) << 16) | (TRB_STATUS << 10) | (1 << 5), // IOC=1
        );
        self.ring_db(slot_id, 1);
        let ev = self.wait_event(EVT_TRANSFER, slot_id, 1, 1500)?;
        if ev.cc == CC_STALL {
            return Err(EPIPE);
        }
        if ev.cc != CC_SUCCESS && ev.cc != CC_SHORT_PKT {
            return Err(EIO);
        }
        Ok((len as usize).saturating_sub(ev.residual as usize))
    }

    pub fn arm_normal_trb(
        &mut self,
        slot_id: u8,
        dci: u8,
        ring_idx: usize,
        buf_pa: u64,
        len: u32,
    ) {
        self.ep_rings[ring_idx].enqueue(
            buf_pa as u32,
            (buf_pa >> 32) as u32,
            len,
            (TRB_NORMAL << 10) | (1 << 5) | (1 << 2), // IOC | ISP
        );
        self.ring_db(slot_id, dci);
    }

    pub fn sync_normal_trb(
        &mut self,
        slot_id: u8,
        dci: u8,
        ring_idx: usize,
        buf_pa: u64,
        len: u32,
        timeout_ms: u64,
    ) -> Result<usize, isize> {
        self.arm_normal_trb(slot_id, dci, ring_idx, buf_pa, len);
        let ev = self.wait_event(EVT_TRANSFER, slot_id, dci, timeout_ms)?;
        if ev.cc == CC_STALL {
            return Err(EPIPE);
        }
        if ev.cc != CC_SUCCESS && ev.cc != CC_SHORT_PKT {
            return Err(EIO);
        }
        Ok((len as usize).saturating_sub(ev.residual as usize))
    }

    pub fn recover_endpoint(&mut self, slot_id: u8, dci: u8, ep_addr: u8, ring_idx: usize) {
        let _ = self.submit_cmd(
            0,
            0,
            ((slot_id as u32) << 24) | ((dci as u32) << 16) | (TRB_STOP_EP << 10),
            500,
        );
        let _ = self.submit_cmd(
            0,
            0,
            ((slot_id as u32) << 24) | ((dci as u32) << 16) | (TRB_RESET_EP << 10),
            500,
        );
        let _ = self.control_xfer(slot_id, 0x02, 0x01, 0, ep_addr as u16, 0); // CLEAR_FEATURE(HALT)
        let deq_pa = self.ep_rings[ring_idx].dequeue_ptr_dcs();
        let _ = self.submit_cmd(
            deq_pa as u32,
            (deq_pa >> 32) as u32,
            ((slot_id as u32) << 24) | ((dci as u32) << 16) | (TRB_SET_TR_DEQ << 10),
            500,
        );
    }

    pub fn ack_msi_if_bound(&self) {
        if self.msi_bound {
            let _ = sys_irq_ack(self.pci_cap, IRQ_ACK_UNMASK);
        }
    }

    pub fn quiesce(&mut self) {
        if self.msi_bound {
            let _ = sys_irq_ack(self.pci_cap, IRQ_ACK_UNBIND);
            self.msi_bound = false;
        }
        let cmd = unsafe { r32(self.op_va, 0x00) };
        if (cmd & 1) != 0 {
            unsafe { w32(self.op_va, 0x00, cmd & !1) };
            let _ = wait_op_mask(self.op_va, 0x04, 1, 1, self.tsc_mhz, 100);
        }
        for i in 0..self.dma_cap_count {
            if self.dma_caps[i] != 0 {
                let _ = sys_cap_revoke(self.dma_caps[i], 1);
                self.dma_caps[i] = 0;
            }
        }
        self.dma_cap_count = 0;
        if self.notif_cap != 0 {
            let _ = sys_cap_revoke(self.notif_cap, 1);
            self.notif_cap = 0;
        }
    }
}
