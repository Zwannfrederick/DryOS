//! `dry-usb` — Ring-3 (`CPL=3`, `SecDomain::RootSystem`, `0x000a`) xHCI + USB Boot HID + BOT MSC.
//!
//! Serves:
//! - USB Boot HID Keyboard (`03:01:01`) and Boot Mouse (`03:01:02`) -> `IPC_OP_WM_INPUT_EVENT`
//! - USB Mass Storage BOT (`08:06:50`) Partition 2 (`DRYFS_V1`) -> `BLK_OP_*` + `IoRingSharedPage`
//!   with strict `INV-DISK-SAFE-02` Partition 2 + `BOOT_FLAG_STORAGE_RO` write fence
//! - `USB_OP_SET_WM_SINK` and `USB_OP_QUERY_SUMMARY` for supervisor and `lsusb`/`lsblk`

#![no_std]
#![no_main]

use core::arch::asm;

use dry_userspace::abi_ext::{
    BLK_OP_ATTACH_DATA, BLK_OP_DETACH_RING, BLK_OP_QUERY_INFO, BLK_OP_SETUP_RING,
};
use dry_userspace::input::InputRingQueue;
use dry_userspace::ioring::{IoRingSharedPage, IORING_MAGIC};
use dry_userspace::keymap::KeymapLayout;
use dry_userspace::shell::LineBuilder;
use dry_userspace::usb::hid::{init_boot_hid_protocol, UsbHidKbd, UsbHidMouse};
use dry_userspace::usb::log_svc::{UsbLogServerState, LOG_STAGE_CAP};
use dry_userspace::usb::msc::UsbMscBot;
use dry_userspace::usb::xhci::{XhciController, CC_SHORT_PKT, CC_STALL, CC_SUCCESS};
use dry_userspace::usb::{
    parse_config_interfaces, ParsedInterface, UsbDeviceSummary, UsbSummaryInfo, USB_CLASS_HID,
    USB_CLASS_MASS_STORAGE, USB_FLAG_DRYFS_VERIFIED, USB_FLAG_KBD_READY, USB_FLAG_MOUSE_READY,
    USB_FLAG_MSC_READY, USB_FLAG_RO_POLICY, USB_PROTO_BOT, USB_PROTO_KBD, USB_PROTO_MOUSE,
    USB_SUBCLASS_BOOT, USB_SUBCLASS_SCSI,
};
use dry_userspace::vfs_proto::{sys_ipc_recv_extra, sys_ipc_reply_extra};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_call_inline,
    sys_ipc_reply_inline, sys_notify_signal, sys_notify_wait, sys_vspace_map,
    sys_write_debug, UserIpcRecvExt, BADGE_BLK_VFS, BADGE_SUPERVISOR, EAGAIN, EBADCAP, EINVAL,
    EPERM, EPIPE, EREVOKED, ETIMEDOUT, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, IPC_OP_WM_INPUT_EVENT,
    MAX_IPC_EXTRA_BYTES, MSG_FLAG_NON_BLOCKING, USB_OP_QUERY_SUMMARY, USB_OP_SET_WM_SINK,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const CLIENT_RING_VADDR: u64 = 0x1800_0000;
const CLIENT_DATA_VADDR: u64 = 0x1800_1000;
static mut LOG_STAGE_BUF: [u8; LOG_STAGE_CAP] = [0u8; LOG_STAGE_CAP];

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

struct ClientBinding {
    ring_cap: u64,
    data_cap: u64,
    sq_notif: u64,
    cq_notif: u64,
}

impl ClientBinding {
    const fn empty() -> Self {
        Self { ring_cap: 0, data_cap: 0, sq_notif: 0, cq_notif: 0 }
    }

    fn reset(&mut self) {
        if self.ring_cap != 0 {
            let _ = sys_vspace_map(self.ring_cap, CLIENT_RING_VADDR, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(self.ring_cap, 1);
            self.ring_cap = 0;
        }
        if self.data_cap != 0 {
            let _ = sys_vspace_map(self.data_cap, CLIENT_DATA_VADDR, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(self.data_cap, 1);
            self.data_cap = 0;
        }
        if self.sq_notif != 0 {
            let _ = sys_cap_revoke(self.sq_notif, 1);
            self.sq_notif = 0;
        }
        if self.cq_notif != 0 {
            let _ = sys_cap_revoke(self.cq_notif, 1);
            self.cq_notif = 0;
        }
    }
}

struct UsbState {
    hc: XhciController,
    kbd: UsbHidKbd,
    mouse: UsbHidMouse,
    msc: UsbMscBot,
    devices: [UsbDeviceSummary; 4],
    device_count: u8,
    connected_ports: u8,
    active_slots: u8,
    read_only_policy: bool,
    layout: KeymapLayout,
    wm_ep_cap: u64,
    wm_drops: u64,
    next_ev_seq: u32,
    input_queue: InputRingQueue<64>,
    log_svc: UsbLogServerState,
}

impl UsbState {
    fn enumerate_root_hub(&mut self) {
        self.hc.sleep_ms(30);
        let max_p = self.hc.max_ports;
        for port in 0..max_p {
            let Some(speed) = self.hc.probe_and_reset_port(port) else {
                continue;
            };
            self.connected_ports = self.connected_ports.saturating_add(1);
            let Ok(slot_id) = self.hc.enable_slot() else {
                continue;
            };
            if self.hc.address_device(slot_id, port + 1, speed).is_err() {
                self.hc.disable_slot(slot_id);
                continue;
            }
            self.active_slots = self.active_slots.saturating_add(1);

            if self.hc.control_xfer(slot_id, 0x80, 0x06, 0x0100, 0, 8).is_err() {
                continue;
            }
            let mps0 = self.hc.ctrl_bounce_slice()[7];
            if speed == 1 && mps0 != 8 && mps0 >= 8 {
                let _ = self.hc.eval_ep0_mps(slot_id, u16::from(mps0));
            }
            if self.hc.control_xfer(slot_id, 0x80, 0x06, 0x0100, 0, 18).is_err() {
                continue;
            }
            let dev_vid = u16::from_le_bytes([self.hc.ctrl_bounce_slice()[8], self.hc.ctrl_bounce_slice()[9]]);
            let dev_pid = u16::from_le_bytes([self.hc.ctrl_bounce_slice()[10], self.hc.ctrl_bounce_slice()[11]]);

            if self.hc.control_xfer(slot_id, 0x80, 0x06, 0x0200, 0, 9).is_err() {
                continue;
            }
            let cfg_total = u16::from_le_bytes([self.hc.ctrl_bounce_slice()[2], self.hc.ctrl_bounce_slice()[3]]).clamp(9, 512);
            if self.hc.control_xfer(slot_id, 0x80, 0x06, 0x0200, 0, cfg_total).is_err() {
                continue;
            }
            let mut raw_cfg = [0u8; 512];
            raw_cfg[..cfg_total as usize].copy_from_slice(&self.hc.ctrl_bounce_slice()[..cfg_total as usize]);
            let mut ifaces = [ParsedInterface::EMPTY; 4];
            let (cfg_val, n_if) = parse_config_interfaces(&raw_cfg[..cfg_total as usize], &mut ifaces);
            if n_if == 0 {
                continue;
            }

            // Pick primary recognized interface on this slot (HID Kbd / Mouse / MSC BOT).
            let mut chosen = ifaces[0];
            for item in ifaces[..n_if].iter() {
                if (item.class_code == USB_CLASS_HID
                    && item.subclass == USB_SUBCLASS_BOOT
                    && (item.proto == USB_PROTO_KBD || item.proto == USB_PROTO_MOUSE))
                    || (item.class_code == USB_CLASS_MASS_STORAGE
                        && item.subclass == USB_SUBCLASS_SCSI
                        && item.proto == USB_PROTO_BOT)
                {
                    chosen = *item;
                    break;
                }
            }

            let mut configured = 0u8;
            if let Ok((in_ep, out_ep)) = self.hc.configure_interface_eps(slot_id, speed, &chosen) {
                if self.hc.control_xfer(slot_id, 0x00, 0x09, u16::from(cfg_val), 0, 0).is_ok() {
                    configured = 1;
                    self.bind_driver(slot_id, &chosen, in_ep, out_ep);
                }
            }

            if (self.device_count as usize) < self.devices.len() {
                self.devices[self.device_count as usize] = UsbDeviceSummary {
                    port: port + 1,
                    slot_id,
                    speed,
                    class_code: chosen.class_code,
                    subclass: chosen.subclass,
                    proto: chosen.proto,
                    configured,
                    iface_num: chosen.iface_num,
                    vendor_id: dev_vid,
                    product_id: dev_pid,
                    ep_in_addr: chosen.ep_in_addr,
                    ep_out_addr: chosen.ep_out_addr,
                    _pad: 0,
                };
                self.device_count += 1;
            }
        }
    }

    fn bind_driver(
        &mut self,
        slot_id: u8,
        iface: &ParsedInterface,
        in_ep: Option<(u8, usize)>,
        out_ep: Option<(u8, usize)>,
    ) {
        if iface.class_code == USB_CLASS_HID && iface.subclass == USB_SUBCLASS_BOOT {
            if let Some((in_dci, in_ring)) = in_ep {
                init_boot_hid_protocol(&mut self.hc, slot_id, iface.iface_num);
                if iface.proto == USB_PROTO_KBD && !self.kbd.active {
                    if let Some((va, pa)) = self.hc.alloc_hid_buf() {
                        self.kbd = UsbHidKbd {
                            active: true,
                            slot_id,
                            dci: in_dci,
                            ring_idx: in_ring,
                            ep_addr: iface.ep_in_addr,
                            mps: iface.ep_in_mps.clamp(8, 64),
                            buf_va: va,
                            buf_pa: pa,
                            ..UsbHidKbd::empty()
                        };
                        self.kbd.arm(&mut self.hc);
                    }
                } else if iface.proto == USB_PROTO_MOUSE && !self.mouse.active {
                    if let Some((va, pa)) = self.hc.alloc_hid_buf() {
                        self.mouse = UsbHidMouse {
                            active: true,
                            slot_id,
                            dci: in_dci,
                            ring_idx: in_ring,
                            ep_addr: iface.ep_in_addr,
                            mps: iface.ep_in_mps.clamp(3, 8),
                            buf_va: va,
                            buf_pa: pa,
                            ..UsbHidMouse::empty()
                        };
                        self.mouse.arm(&mut self.hc);
                    }
                }
            }
        } else if iface.class_code == USB_CLASS_MASS_STORAGE
            && iface.subclass == USB_SUBCLASS_SCSI
            && iface.proto == USB_PROTO_BOT
            && !self.msc.active
        {
            if let (Some((in_dci, in_ring)), Some((out_dci, out_ring))) = (in_ep, out_ep) {
                let mut msc = UsbMscBot::empty();
                msc.active = true;
                msc.slot_id = slot_id;
                msc.iface_num = iface.iface_num;
                msc.ep_in_addr = iface.ep_in_addr;
                msc.ep_out_addr = iface.ep_out_addr;
                msc.in_dci = in_dci;
                msc.out_dci = out_dci;
                msc.in_ring = in_ring;
                msc.out_ring = out_ring;
                self.msc = msc;
                let _ = self.msc.probe_and_verify(&mut self.hc);
            }
        }
    }

    fn drain_xhci_events(&mut self) {
        while let Some(ev) = self.hc.pop_event() {
            if ev.trb_type != 32 {
                continue;
            }
            if self.kbd.active && ev.slot_id == self.kbd.slot_id && ev.ep_id == self.kbd.dci {
                if ev.cc == CC_SUCCESS || ev.cc == CC_SHORT_PKT {
                    self.kbd.on_report(self.layout, &mut self.next_ev_seq, &mut self.input_queue);
                    self.kbd.arm(&mut self.hc);
                } else if ev.cc == CC_STALL {
                    self.hc.recover_endpoint(
                        self.kbd.slot_id,
                        self.kbd.dci,
                        self.kbd.ep_addr,
                        self.kbd.ring_idx,
                    );
                    self.kbd.arm(&mut self.hc);
                } else {
                    self.kbd.arm(&mut self.hc);
                }
            } else if self.mouse.active
                && ev.slot_id == self.mouse.slot_id
                && ev.ep_id == self.mouse.dci
            {
                if ev.cc == CC_SUCCESS || ev.cc == CC_SHORT_PKT {
                    self.mouse.on_report(ev.residual, &mut self.next_ev_seq, &mut self.input_queue);
                    self.mouse.arm(&mut self.hc);
                } else if ev.cc == CC_STALL {
                    self.hc.recover_endpoint(
                        self.mouse.slot_id,
                        self.mouse.dci,
                        self.mouse.ep_addr,
                        self.mouse.ring_idx,
                    );
                    self.mouse.arm(&mut self.hc);
                } else {
                    self.mouse.arm(&mut self.hc);
                }
            }
        }
        self.hc.ack_msi_if_bound();

        if self.wm_ep_cap != 0 {
            while let Some(ev) = self.input_queue.pop() {
                match sys_ipc_call_inline(
                    self.wm_ep_cap,
                    pack_msg_tag(IPC_OP_WM_INPUT_EVENT, 0, 4, 0),
                    ev.to_ipc_mrs(self.input_queue.overflow_count()),
                    0,
                ) {
                    Ok(_) => {}
                    Err(EPIPE | EREVOKED | EBADCAP) => {
                        self.wm_ep_cap = 0;
                        break;
                    }
                    Err(_) => {
                        self.wm_drops = self.wm_drops.wrapping_add(1);
                        break;
                    }
                }
            }
        }
    }

    fn build_summary(&self) -> UsbSummaryInfo {
        let mut s = UsbSummaryInfo::empty();
        s.controller_vid = self.hc.vid;
        s.controller_did = self.hc.did;
        s.controller_bdf = self.hc.bdf;
        s.state_flags = self.hc.summary_flags()
            | if self.kbd.active { USB_FLAG_KBD_READY } else { 0 }
            | if self.mouse.active { USB_FLAG_MOUSE_READY } else { 0 }
            | if self.msc.ready { USB_FLAG_MSC_READY } else { 0 }
            | if self.msc.dryfs_verified { USB_FLAG_DRYFS_VERIFIED } else { 0 }
            | if self.read_only_policy { USB_FLAG_RO_POLICY } else { 0 };
        s.max_ports = self.hc.max_ports;
        s.connected_ports = self.connected_ports;
        s.active_slots = self.active_slots;
        s.device_count = self.device_count;
        s.msc_lun = self.msc.max_lun;
        s.msc_part2_idx = self.msc.part2_idx;
        s.msc_ro = u8::from(self.read_only_policy);
        s.irq_count = self.hc.irq_count;
        s.kbd_reports = self.kbd.reports;
        s.kbd_events = self.kbd.events;
        s.mouse_reports = self.mouse.reports;
        s.mouse_events = self.mouse.events;
        s.wm_drops = self.wm_drops;
        s.msc_total_sectors = self.msc.total_sectors;
        s.msc_part2_start = self.msc.part2_start;
        s.msc_part2_sectors = self.msc.part2_sectors;
        s.msc_reads = self.msc.reads;
        s.msc_writes = self.msc.writes;
        s.msc_io_errs = self.msc.io_errs;
        s.msc_rofs_rejects = self.msc.rofs_rejects;
        s.devices = self.devices;
        s.msc_vendor_product = self.msc.vendor_product;
        s
    }
}

fn reply_err(ep: u64, tok: u64, lbl: u32, err: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(ep, tok, pack_msg_tag(lbl, 0, 1, 0), [err as u64, 0, 0, 0]);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(srv_ep_cap: u64, pci_xhci_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || srv_ep_cap == 0 || pci_xhci_cap == 0 {
        sys_exit(181);
    }

    let hc = match XhciController::init(pci_xhci_cap) {
        Ok(c) => c,
        Err(e) => {
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-USB] xHCI init failed neg_errno=");
            lb.push_i64(e as i64);
            log_line(&lb);
            sys_exit(182);
        }
    };

    let mut st = UsbState {
        hc,
        kbd: UsbHidKbd::empty(),
        mouse: UsbHidMouse::empty(),
        msc: UsbMscBot::empty(),
        devices: [UsbDeviceSummary::default(); 4],
        device_count: 0,
        connected_ports: 0,
        active_slots: 0,
        read_only_policy: true, // Fail-closed until supervisor confirms policy via USB_OP_SET_WM_SINK.
        layout: KeymapLayout::UsQwerty,
        wm_ep_cap: 0,
        wm_drops: 0,
        next_ev_seq: 1,
        input_queue: InputRingQueue::new(),
        log_svc: UsbLogServerState::empty(),
    };
    st.enumerate_root_hub();

    {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-USB] xHCI ready devs=");
        lb.push_u64(st.device_count as u64);
        lb.push_bytes(b" kbd=");
        lb.push_bytes(if st.kbd.active { b"1" } else { b"0" });
        lb.push_bytes(b" mouse=");
        lb.push_bytes(if st.mouse.active { b"1" } else { b"0" });
        lb.push_bytes(b" msc=");
        lb.push_bytes(if st.msc.ready { b"1" } else { b"0" });
        lb.push_bytes(b" p2_secs=");
        lb.push_u64(st.msc.part2_sectors);
        lb.push_bytes(b" dryfs=");
        lb.push_bytes(if st.msc.dryfs_verified { b"1" } else { b"0" });
        log_line(&lb);
    }

    let poll_tsc = 2_000u64.saturating_mul(st.hc.tsc_mhz.max(1000));
    let mut cli = ClientBinding::empty();

    loop {
        if cli.ring_cap != 0 && st.msc.ready && st.msc.dryfs_verified {
            let data_live = cli.data_cap != 0;
            // SAFETY: `CLIENT_RING_VADDR` is mapped 4 KiB `USER_RW` for `cli.ring_cap`.
            let ring = unsafe { &*(CLIENT_RING_VADDR as *const IoRingSharedPage) };
            if ring.magic == IORING_MAGIC {
                let mut completed = false;
                while let Some(sqe) = ring.pop_sqe() {
                    let cqe = st.msc.execute_sqe(
                        &mut st.hc,
                        sqe,
                        CLIENT_DATA_VADDR,
                        data_live,
                        st.read_only_policy,
                    );
                    let _ = ring.push_cqe(cqe);
                    completed = true;
                }
                if completed && cli.cq_notif != 0 {
                    let _ = sys_notify_signal(cli.cq_notif, 1);
                }
            }
        }

        st.drain_xhci_events();

        let (recv_flags, recv_to) = if cli.sq_notif != 0 {
            (MSG_FLAG_NON_BLOCKING as u64, 0u64)
        } else {
            (0u64, poll_tsc)
        };
        let mut ext = UserIpcRecvExt::default();
        let mut extra_rx = [0u8; MAX_IPC_EXTRA_BYTES];
        let (tag, extra_len, mrs) = match sys_ipc_recv_extra(srv_ep_cap, recv_flags, recv_to, &mut extra_rx, &mut ext) {
            Ok(v) => v,
            Err(_) => {
                if cli.sq_notif != 0 {
                    if let Err(we) = sys_notify_wait(cli.sq_notif, 0, poll_tsc) {
                        if we != ETIMEDOUT && we != EAGAIN {
                            cli.reset();
                        }
                    }
                }
                continue;
            }
        };
        let tok = ext.reply_token;

        let label = msg_tag_label(tag);
        let from_sup = ext.sender_badge == BADGE_SUPERVISOR;
        let from_blk_auth = from_sup || ext.sender_badge == BADGE_BLK_VFS;
        if !matches!(label, USB_OP_SET_WM_SINK | BLK_OP_SETUP_RING | BLK_OP_ATTACH_DATA) {
            dry_userspace::abi_ext::revoke_unused_cap(ext.transferred_cap);
        }
        let exposed_secs = if st.msc.ready && st.msc.dryfs_verified {
            st.msc.part2_sectors
        } else {
            0
        };

        match label {
            USB_OP_SET_WM_SINK => {
                if !from_sup {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if ext.transferred_cap != 0 {
                    if st.wm_ep_cap != 0 {
                        let _ = sys_cap_revoke(st.wm_ep_cap, 1);
                    }
                    st.wm_ep_cap = ext.transferred_cap;
                }
                st.read_only_policy = (mrs[0] & 1) != 0;
                if let Some(l) = KeymapLayout::from_u8(mrs[1] as u8) {
                    st.layout = l;
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [exposed_secs, u64::from(st.read_only_policy), 0, 0],
                    );
                }
            }
            USB_OP_QUERY_SUMMARY => {
                if !from_sup {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if tok != 0 {
                    let sum = st.build_summary();
                    let _ = sys_ipc_reply_extra(
                        srv_ep_cap,
                        tok,
                        IPC_OP_PONG,
                        [
                            u64::from(sum.state_flags),
                            sum.msc_part2_sectors,
                            sum.kbd_events,
                            sum.mouse_events,
                        ],
                        sum.as_bytes(),
                    );
                }
            }
            BLK_OP_QUERY_INFO => {
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [
                            exposed_secs,
                            1,
                            st.msc.reads + st.msc.writes,
                            st.msc.flushes,
                        ],
                    );
                }
            }
            BLK_OP_SETUP_RING => {
                if !from_blk_auth {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if ext.transferred_cap == 0 {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                cli.reset();
                if sys_vspace_map(ext.transferred_cap, CLIENT_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                cli.ring_cap = ext.transferred_cap;
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [exposed_secs, 1, 0, 0],
                    );
                }
            }
            BLK_OP_ATTACH_DATA => {
                if !from_blk_auth || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                match mrs[0] {
                    0 => {
                        if cli.data_cap != 0 {
                            let _ = sys_vspace_map(cli.data_cap, CLIENT_DATA_VADDR, VSPACE_MAP_OP_UNMAP_4K);
                            let _ = sys_cap_revoke(cli.data_cap, 1);
                        }
                        if sys_vspace_map(ext.transferred_cap, CLIENT_DATA_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
                            let _ = sys_cap_revoke(ext.transferred_cap, 1);
                            reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                            continue;
                        }
                        cli.data_cap = ext.transferred_cap;
                    }
                    1 => {
                        if cli.sq_notif != 0 {
                            let _ = sys_cap_revoke(cli.sq_notif, 1);
                        }
                        cli.sq_notif = ext.transferred_cap;
                    }
                    2 => {
                        if cli.cq_notif != 0 {
                            let _ = sys_cap_revoke(cli.cq_notif, 1);
                        }
                        cli.cq_notif = ext.transferred_cap;
                    }
                    _ => {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_err(srv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                        continue;
                    }
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
            }
            BLK_OP_DETACH_RING => {
                if !from_blk_auth {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                cli.reset();
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if !from_sup {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                cli.reset();
                if st.wm_ep_cap != 0 {
                    let _ = sys_cap_revoke(st.wm_ep_cap, 1);
                    st.wm_ep_cap = 0;
                }
                st.hc.quiesce();
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            IPC_OP_CRASH_UD2 => {
                if !from_sup {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                // SAFETY: Supervisor-authorized `#UD` test (`R-03`).
                unsafe {
                    asm!("ud2", options(nomem, nostack));
                }
            }
            _ => {
                // SAFETY: Single-threaded event loop owns `LOG_STAGE_BUF`.
                let stage_buf = unsafe { &mut *core::ptr::addr_of_mut!(LOG_STAGE_BUF) };
                if !st.log_svc.handle_ipc(
                    srv_ep_cap,
                    tok,
                    label,
                    from_sup,
                    mrs,
                    &extra_rx[..extra_len],
                    &mut st.hc,
                    &mut st.msc,
                    stage_buf,
                    log_line,
                ) {
                    reply_err(srv_ep_cap, tok, IPC_OP_ERR_NOSYS, EINVAL);
                }
            }
        }
    }
}
