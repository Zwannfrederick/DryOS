// SPDX-License-Identifier: GPL-2.0-only
//! `dry-lx-net` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0214`) Network Driver
//! Island linking unmodified Linux `v6.18` `drivers/net/ethernet/realtek/8139cp.c` +
//! `drivers/net/mii.c` against `libdry-lx-compat` (`ADR-005` GPL isolation boundary).

#![no_std]
#![no_main]

use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

use dry_userspace::abi_ext::{sys_dev_cfg_read, sys_dev_cfg_write, sys_dma_alloc_sg};
use dry_userspace::init_legacy::log_line;
use dry_userspace::ioring::{IoCqe, IoRingSharedPage};
use dry_userspace::island_proto::{
    island_session_badge, BADGE_ISLAND_BROKER, IPC_OP_ERR_STALE_GEN, MOCK_OP_BIND_NOTIF,
    MOCK_OP_INIT_ISLAND, MOCK_OP_SET_PARK_MODE,
};
use dry_userspace::lx_compat_meta::bar_is_mmio;
use dry_userspace::lx_drv_proto::{
    IORING_OP_NET_POLL, LX_NET_OP_POLL_RX_TX, LX_NET_OP_QUERY_INFO, LX_NET_OP_SEND_ARP,
    LX_OP_BIND_PCI,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_dev_map_window,
    sys_dev_query_bars, sys_dma_alloc_2m, sys_exit, sys_frame_alloc, sys_get_tid,
    sys_ipc_recv_ext, sys_ipc_reply_inline, sys_ipc_reply_with_grant, sys_irq_ack, sys_irq_bind,
    sys_notify_bind_tcb, sys_notify_create, sys_notify_signal, sys_notify_unbind_tcb,
    sys_proc_kill, sys_sysinfo, sys_vspace_map, UserIpcRecvExt, UserPciBarInfo, UserPciDeviceInfo,
    UserSysInfo, BADGE_SUPERVISOR, EBADCAP, EINVAL, EPERM, EREVOKED, IPC_OP_BOUND_NOTIF,
    IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG,
    IRQF_ONESHOT, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_REVOKE, RIGHT_WRITE,
    VSPACE_MAP_OP_4K_RW,
};

const ZERO_DEV_INFO: UserPciDeviceInfo = UserPciDeviceInfo {
    vendor_id: 0,
    device_id: 0,
    bus_dev_fn: 0,
    dev_flags: 0,
    msix_table_size: 0,
    msi_cap_present: 0,
    iommu_protected: 0,
    iommu_faults: 0,
    bars: [UserPciBarInfo { size_bytes: 0, flags: 0, page_suboff: 0, _pad: 0 }; 6],
};

const BAR_MAP_BASE: u64 = 0x2100_0000;
const BAR_MAP_STRIDE: u64 = 0x0100_0000;
const ISLAND_RING_VADDR: u64 = 0x2400_0000;
const DMA_ARENA_VADDR: u64 = 0x2800_0000;
const TWO_MIB: u64 = 2 * 1024 * 1024;

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct DryLxPciBarInit {
    size_bytes: u64,
    flags: u32,
    page_suboff: u16,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct DryLxNetProbeInfo {
    mac: [u8; 6],
    link_up: u8,
    bar1_mapped: u8,
    bar1_suboff: u16,
    irq_is_msi: u16,
    mtu: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct DryLxNetStats {
    tx_packets: u64,
    rx_packets: u64,
    tx_bytes: u64,
    rx_bytes: u64,
    arp_replies: u64,
    udp_echo_pkts: u64,
    irqs_handled: u64,
    mac: [u8; 6],
    gw_mac_known: u8,
    link_up: u8,
}

#[link(name = "lx_net_8139cp", kind = "static")]
extern "C" {
    fn dry_lx_net_probe(
        vendor_id: u16,
        device_id: u16,
        subvendor: u16,
        subdevice: u16,
        bdf: u16,
        revision: u8,
        dev_flags: u16,
        bars: *const DryLxPciBarInit,
        out: *mut DryLxNetProbeInfo,
    ) -> i32;
    fn dry_lx_net_send_arp_who_has(target_ip_be: u32) -> i32;
    fn dry_lx_net_poll_step(budget: i32, out: *mut DryLxNetStats) -> i32;
    fn dry_lx_net_get_stats(out: *mut DryLxNetStats);
    fn dry_lx_net_shutdown();
}

struct SyncCell<T>(UnsafeCell<T>);
// SAFETY: Driver island runs single-threaded inside its isolated process.
unsafe impl<T> Sync for SyncCell<T> {}

static PCI_CAP: AtomicU64 = AtomicU64::new(0);
static DMA_CAP: AtomicU64 = AtomicU64::new(0);
static DMA_PA: AtomicU64 = AtomicU64::new(0);
static IRQ_NOTIF: AtomicU64 = AtomicU64::new(0);
static TSC_KHZ: AtomicU64 = AtomicU64::new(2_000_000);
static DEV_INFO: SyncCell<UserPciDeviceInfo> = SyncCell(UnsafeCell::new(ZERO_DEV_INFO));

#[inline]
unsafe fn cstr_prefix<'a>(ptr: *const u8, max_len: usize) -> &'a [u8] {
    if ptr.is_null() {
        return b"";
    }
    let mut len = 0usize;
    while len < max_len {
        // SAFETY: Caller passes valid NUL-terminated C string from static C compat runtime.
        if unsafe { *ptr.add(len) } == 0 {
            break;
        }
        len += 1;
    }
    // SAFETY: `[ptr..ptr+len)` precedes NUL byte within `max_len`.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

#[no_mangle]
pub extern "C" fn dry_lx_rust_ioremap_bar(bar_idx: u32, cache_mode: u32, _size_bytes: u64) -> *mut u8 {
    let pci_cap = PCI_CAP.load(Ordering::Acquire);
    if bar_idx >= 6 || pci_cap == 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: Single-threaded driver runtime; `DEV_INFO` initialized before `dry_lx_net_probe`.
    let info = unsafe { &*DEV_INFO.0.get() };
    let bar = info.bars[bar_idx as usize];
    if bar.size_bytes == 0 || !bar_is_mmio(bar.flags as u8) {
        return core::ptr::null_mut();
    }
    let total = u64::from(bar.page_suboff).saturating_add(bar.size_bytes);
    let pages = total.div_ceil(4096).clamp(1, 256) as u16;
    let va = BAR_MAP_BASE + u64::from(bar_idx) * BAR_MAP_STRIDE;
    if sys_dev_map_window(pci_cap, va, bar_idx as u8, cache_mode != 0, pages, 0).is_err() {
        return core::ptr::null_mut();
    }
    va as *mut u8
}

/// # Safety
/// `tag` and `msg` must be null or valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn dry_lx_rust_log(tag: *const u8, msg: *const u8) {
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-LX-NET:");
    // SAFETY: Caller guarantees NUL-terminated C log tag passed by `libdry-lx-compat`.
    lb.push_bytes(unsafe { cstr_prefix(tag, 24) });
    lb.push_bytes(b"] ");
    // SAFETY: Caller guarantees NUL-terminated C log message passed by `libdry-lx-compat`.
    lb.push_bytes(unsafe { cstr_prefix(msg, 128) });
    log_line(&lb);
}

/// # Safety
/// `va_out`, `pa_out`, and `bytes_out` must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn dry_lx_rust_dma_arena_init(
    va_out: *mut u64,
    pa_out: *mut u64,
    bytes_out: *mut u64,
) -> i32 {
    if va_out.is_null() || pa_out.is_null() || bytes_out.is_null() {
        return -(EINVAL as i32);
    }
    let existing = DMA_CAP.load(Ordering::Acquire);
    if existing != 0 {
        // SAFETY: Out pointers validated non-null above.
        unsafe {
            *va_out = DMA_ARENA_VADDR;
            *pa_out = DMA_PA.load(Ordering::Acquire);
            *bytes_out = TWO_MIB;
        }
        return 0;
    }
    let pci_cap = PCI_CAP.load(Ordering::Acquire);
    if pci_cap == 0 {
        return -(EINVAL as i32);
    }
    match sys_dma_alloc_2m(pci_cap, DMA_ARENA_VADDR) {
        Ok((cap, pa)) => {
            DMA_CAP.store(cap, Ordering::Release);
            DMA_PA.store(pa, Ordering::Release);
            // SAFETY: Out pointers validated non-null above.
            unsafe {
                *va_out = DMA_ARENA_VADDR;
                *pa_out = pa;
                *bytes_out = TWO_MIB;
            }
            0
        }
        Err(e) => -(e as i32),
    }
}

/// # Safety
/// `vec_out` must be a valid writable pointer.
#[no_mangle]
pub unsafe extern "C" fn dry_lx_rust_irq_bind(vec_out: *mut u32) -> i32 {
    let pci_cap = PCI_CAP.load(Ordering::Acquire);
    if pci_cap == 0 || vec_out.is_null() {
        return -(EINVAL as i32);
    }
    let ncap = match sys_notify_create(0) {
        Ok(c) => c,
        Err(e) => return -(e as i32),
    };
    match sys_irq_bind(pci_cap, ncap, IRQF_ONESHOT) {
        Ok(vec) => {
            IRQ_NOTIF.store(ncap, Ordering::Release);
            // SAFETY: `vec_out` checked non-null above.
            unsafe { *vec_out = u32::from(vec) };
            0
        }
        Err(e) => {
            let _ = sys_cap_revoke(ncap, 2);
            -(e as i32)
        }
    }
}

#[no_mangle]
pub extern "C" fn dry_lx_rust_irq_ack(action: u32) -> i32 {
    let pci_cap = PCI_CAP.load(Ordering::Acquire);
    if pci_cap == 0 {
        return -(EINVAL as i32);
    }
    match sys_irq_ack(pci_cap, u64::from(action)) {
        Ok(_) => 0,
        Err(e) => -(e as i32),
    }
}

#[no_mangle]
pub extern "C" fn dry_lx_rust_tsc_khz() -> u64 {
    TSC_KHZ.load(Ordering::Acquire)
}

/// Capability-backed PCI configuration space bridge (`DEV_MAP_OP_CFG_RW`).
///
/// # Safety
/// `val` must be a valid non-null pointer to `u32` storage.
#[no_mangle]
pub unsafe extern "C" fn dry_lx_rust_pci_cfg_rw(
    off: u16,
    width: u8,
    is_write: u8,
    val: *mut u32,
) -> i32 {
    let pci_cap = PCI_CAP.load(Ordering::Acquire);
    if pci_cap == 0 || val.is_null() {
        return EINVAL as i32;
    }
    if is_write == 0 {
        match sys_dev_cfg_read(pci_cap, off, width) {
            Ok(v) => {
                // SAFETY: Caller guarantees `val` is valid writable `u32` storage.
                unsafe { *val = v };
                0
            }
            Err(e) => e as i32,
        }
    } else {
        // SAFETY: Caller guarantees `val` is valid readable `u32` storage.
        let v = unsafe { *val };
        match sys_dev_cfg_write(pci_cap, off, width, v) {
            Ok(()) => 0,
            Err(e) => e as i32,
        }
    }
}

static SG_VA_NEXT: AtomicU64 = AtomicU64::new(0x2900_0000);

/// Capability-backed scatter-gather DMA bridge (`SYS_DMA_CTL` / `DMA_OP_ALLOC_SG`).
///
/// # Safety
/// `va_out` and `iova_out` must be valid non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn dry_lx_rust_dma_sg_alloc(
    n_pages: u32,
    dir_flags: u32,
    va_out: *mut u64,
    iova_out: *mut u64,
) -> i32 {
    let pci_cap = PCI_CAP.load(Ordering::Acquire);
    if pci_cap == 0 || va_out.is_null() || iova_out.is_null() || n_pages == 0 || n_pages > 16 {
        return EINVAL as i32;
    }
    let bytes = u64::from(n_pages) * 4096;
    let base_va = SG_VA_NEXT.fetch_add(bytes, Ordering::Relaxed);
    for i in 0..n_pages {
        let Ok(fcap) = sys_frame_alloc(0) else {
            return dry_userspace::ENOMEM as i32;
        };
        let va = base_va + u64::from(i) * 4096;
        if let Err(e) = sys_vspace_map(fcap, va, VSPACE_MAP_OP_4K_RW) {
            let _ = sys_cap_revoke(fcap, 1);
            return e as i32;
        }
    }
    match sys_dma_alloc_sg(pci_cap, base_va, n_pages as u16, dir_flags as u8, None) {
        Ok(desc) => {
            // SAFETY: Out pointers checked non-null above.
            unsafe {
                *va_out = base_va;
                *iova_out = desc.iova_base;
            }
            0
        }
        Err(e) => e as i32,
    }
}

#[inline]
fn pack_mac(mac: &[u8; 6]) -> u64 {
    u64::from(mac[0])
        | (u64::from(mac[1]) << 8)
        | (u64::from(mac[2]) << 16)
        | (u64::from(mac[3]) << 24)
        | (u64::from(mac[4]) << 32)
        | (u64::from(mac[5]) << 40)
}

#[inline]
fn reply_err(ep: u64, tok: u64, op: u32, err_code: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(op, 0, 1, 0),
            [(-err_code) as u64, 0, 0, 0],
        );
    }
}

#[no_mangle]
pub extern "C" fn _start(drv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || drv_ep_cap == 0 {
        sys_exit(180);
    }
    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    TSC_KHZ.store(sys.tsc_mhz.max(1000).saturating_mul(1000), Ordering::Release);

    let mut dummy_pci = ZERO_DEV_INFO;
    let mac_isolated = sys_proc_kill(1).is_err() && sys_dev_query_bars(drv_ep_cap, &mut dummy_pci).is_err();

    let ring_cap = sys_frame_alloc(0).unwrap_or_else(|_| sys_exit(181));
    if sys_vspace_map(ring_cap, ISLAND_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(182);
    }
    // SAFETY: `ISLAND_RING_VADDR` is mapped 4 KiB `USER_RW` for this island's `IoRing`.
    let ring = unsafe { IoRingSharedPage::init_at(ISLAND_RING_VADDR) };

    let mut generation = 0u64;
    let mut sq_notif = 0u64;
    let mut cq_notif = 0u64;
    let mut pci_bound = false;
    let mut probe_info = DryLxNetProbeInfo::default();

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, tok, mrs) = match sys_ipc_recv_ext(drv_ep_cap, 0, 0, &mut ext) {
            Ok(v) => v,
            Err(_) => break,
        };
        let op = msg_tag_label(tag);

        if op == IPC_OP_BOUND_NOTIF {
            let mut drained = false;
            while let Some(sqe) = ring.pop_sqe() {
                let cqe = if sqe.opcode != IORING_OP_NET_POLL || !pci_bound {
                    IoCqe { user_tag: sqe.user_tag, status: -(EINVAL as i32), bytes_done: 0 }
                } else if sqe._reserved != 0 && sqe._reserved != generation {
                    IoCqe {
                        user_tag: sqe.user_tag,
                        status: -(EREVOKED as i32),
                        bytes_done: generation as u32,
                    }
                } else {
                    if sqe.buf_offset != 0 {
                        // SAFETY: Single-threaded driver runtime; probed via `dry_lx_net_probe`.
                        let _ = unsafe { dry_lx_net_send_arp_who_has(sqe.buf_offset) };
                    }
                    let mut st = DryLxNetStats::default();
                    // SAFETY: Single-threaded driver runtime; `st` is valid output storage.
                    let rc = unsafe { dry_lx_net_poll_step(i32::from(sqe.sector_count.max(16)), &mut st) };
                    IoCqe {
                        user_tag: sqe.user_tag,
                        status: if rc >= 0 { 0 } else { rc },
                        bytes_done: st.tx_packets.wrapping_add(st.rx_packets) as u32,
                    }
                };
                let _ = ring.push_cqe(cqe);
                drained = true;
            }
            if drained && cq_notif != 0 {
                let _ = sys_notify_signal(cq_notif, 1);
            }
            continue;
        }

        let is_sup = ext.sender_badge == BADGE_SUPERVISOR;
        let is_broker = ext.sender_badge == BADGE_ISLAND_BROKER || is_sup;
        let is_session = generation != 0 && ext.sender_badge == island_session_badge(generation);

        match op {
            MOCK_OP_BIND_NOTIF => {
                if !is_broker || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                match mrs[0] {
                    0 => {
                        if sq_notif != 0 {
                            let _ = sys_notify_unbind_tcb(sq_notif);
                            let _ = sys_cap_revoke(sq_notif, 1);
                        }
                        sq_notif = ext.transferred_cap;
                        let _ = sys_notify_bind_tcb(sq_notif);
                    }
                    1 => {
                        if cq_notif != 0 {
                            let _ = sys_cap_revoke(cq_notif, 1);
                        }
                        cq_notif = ext.transferred_cap;
                    }
                    _ => {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                        continue;
                    }
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap, tok, pack_msg_tag(IPC_OP_PONG, 0, 1, 0), [1, 0, 0, 0],
                    );
                }
            }
            LX_OP_BIND_PCI => {
                if !is_broker || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let pci_cap = ext.transferred_cap;
                let rev_id = (mrs[0] & 0xff) as u8;
                let mut dev_info = ZERO_DEV_INFO;
                if sys_dev_query_bars(pci_cap, &mut dev_info).is_err() {
                    let _ = sys_cap_revoke(pci_cap, 1);
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                PCI_CAP.store(pci_cap, Ordering::Release);
                // SAFETY: Single-threaded driver runtime before `dry_lx_net_probe` reads `DEV_INFO`.
                unsafe { *DEV_INFO.0.get() = dev_info };

                let mut bars = [DryLxPciBarInit::default(); 6];
                for (dst, src) in bars.iter_mut().zip(dev_info.bars.iter()) {
                    *dst = DryLxPciBarInit {
                        size_bytes: src.size_bytes,
                        flags: src.flags,
                        page_suboff: src.page_suboff,
                    };
                }
                // SAFETY: Valid pointers and single-threaded driver init calling into `8139cp.c` + `mii.c`.
                let rc = unsafe {
                    dry_lx_net_probe(
                        dev_info.vendor_id,
                        dev_info.device_id,
                        0,
                        0,
                        dev_info.bus_dev_fn,
                        rev_id,
                        dev_info.dev_flags,
                        bars.as_ptr(),
                        &mut probe_info,
                    )
                };
                if rc != 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                pci_bound = true;
                let vdev = (u64::from(dev_info.vendor_id) << 16) | u64::from(dev_info.device_id);
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-LX-NET] 8139cp.c+mii.c probed vdev=0x");
                lb.push_hex_u64(vdev);
                lb.push_bytes(b" rev=0x");
                lb.push_hex_u64(u64::from(rev_id));
                lb.push_bytes(b" bar1_suboff=");
                lb.push_u64(u64::from(probe_info.bar1_suboff));
                lb.push_bytes(b" link=");
                lb.push_u64(u64::from(probe_info.link_up));
                log_line(&lb);

                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [
                            vdev,
                            u64::from(probe_info.bar1_suboff),
                            pack_mac(&probe_info.mac),
                            u64::from(probe_info.link_up),
                        ],
                    );
                }
            }
            MOCK_OP_INIT_ISLAND => {
                if !is_broker || !pci_bound || mrs[0] == 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EBADCAP);
                    continue;
                }
                generation = mrs[0];
                let (tid, _) = sys_get_tid();
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-LX-NET] Ready in CPL3 RootSystem [0x0214] gen=");
                lb.push_u64(generation);
                lb.push_bytes(b" tid=");
                lb.push_u64(tid);
                log_line(&lb);

                if tok != 0 {
                    let rights = RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT | RIGHT_REVOKE;
                    if sys_ipc_reply_with_grant(
                        drv_ep_cap,
                        tok,
                        IPC_OP_PONG,
                        [generation, u64::from(mac_isolated), tid, 1],
                        ring_cap,
                        rights,
                        0,
                    )
                    .is_err()
                    {
                        sys_exit(183);
                    }
                }
            }
            LX_NET_OP_QUERY_INFO | LX_NET_OP_SEND_ARP | LX_NET_OP_POLL_RX_TX => {
                if !is_broker && !is_session {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if !pci_bound {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EBADCAP);
                    continue;
                }
                if mrs[3] != 0 && mrs[3] != generation {
                    if tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            drv_ep_cap,
                            tok,
                            pack_msg_tag(IPC_OP_ERR_STALE_GEN, 0, 2, 0),
                            [(-EREVOKED) as u64, generation, 0, 0],
                        );
                    }
                    continue;
                }
                let mut st = DryLxNetStats::default();
                let out_mrs = match op {
                    LX_NET_OP_QUERY_INFO => {
                        // SAFETY: Single-threaded driver runtime; `st` is valid output storage.
                        unsafe { dry_lx_net_get_stats(&mut st) };
                        let flags = u64::from(st.link_up)
                            | (u64::from(probe_info.bar1_mapped) << 8)
                            | (u64::from(probe_info.bar1_suboff) << 16)
                            | (u64::from(probe_info.irq_is_msi) << 32)
                            | (u64::from(probe_info.mtu) << 40);
                        [pack_mac(&st.mac), flags, st.tx_packets, st.rx_packets]
                    }
                    LX_NET_OP_SEND_ARP => {
                        // SAFETY: Single-threaded driver runtime; transmits ARP via `cp_start_xmit`.
                        let rc = unsafe { dry_lx_net_send_arp_who_has(mrs[0] as u32) };
                        if rc != 0 {
                            reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                            continue;
                        }
                        // SAFETY: Single-threaded driver runtime; pumps IRQ + NAPI poll once.
                        unsafe {
                            let _ = dry_lx_net_poll_step(64, &mut st);
                        }
                        let aux = (st.udp_echo_pkts & 0xffff_ffff) | (u64::from(st.gw_mac_known) << 32);
                        [st.tx_packets, st.rx_packets, st.arp_replies, aux]
                    }
                    _ => {
                        // SAFETY: Single-threaded driver runtime; pumps IRQ + NAPI poll.
                        unsafe {
                            let _ = dry_lx_net_poll_step(64, &mut st);
                        }
                        let aux = (st.udp_echo_pkts & 0xffff_ffff) | ((st.irqs_handled & 0xffff_ffff) << 32);
                        [st.tx_packets, st.rx_packets, st.arp_replies, aux]
                    }
                };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap, tok, pack_msg_tag(IPC_OP_PONG, 0, 4, 0), out_mrs,
                    );
                }
            }
            MOCK_OP_SET_PARK_MODE => {
                if !is_broker {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let park_mode = if mrs[0] == 1 && sq_notif != 0 { 1 } else { 0 };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap, tok, pack_msg_tag(IPC_OP_PONG, 0, 2, 0), [park_mode, generation, 0, 0],
                    );
                }
            }
            IPC_OP_CRASH_UD2 => {
                if !is_sup {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let mut clb = LineBuilder::new();
                clb.push_bytes(b"[DRY-LX-NET] Injecting controlled CPL3 #UD fault gen=");
                clb.push_u64(generation);
                log_line(&clb);
                // SAFETY: Deliberate supervisor-authorized `#UD` fault to test Driver Island recovery.
                unsafe { asm!("ud2", options(nomem, nostack)) };
            }
            IPC_OP_EXIT_CLEAN => {
                if !is_sup {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                // SAFETY: Clean shutdown of `8139cp.c` + `mii.c` before revoking capabilities.
                unsafe { dry_lx_net_shutdown() };
                for c in [DMA_CAP.swap(0, Ordering::AcqRel), IRQ_NOTIF.swap(0, Ordering::AcqRel)] {
                    if c != 0 {
                        let _ = sys_cap_revoke(c, 1);
                    }
                }
                if sq_notif != 0 {
                    let _ = sys_notify_unbind_tcb(sq_notif);
                    let _ = sys_cap_revoke(sq_notif, 1);
                }
                if cq_notif != 0 {
                    let _ = sys_cap_revoke(cq_notif, 1);
                }
                let pci = PCI_CAP.swap(0, Ordering::AcqRel);
                if pci != 0 {
                    let _ = sys_cap_revoke(pci, 1);
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap, tok, pack_msg_tag(IPC_OP_PONG, 0, 1, 0), [generation, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
            }
        }
    }
    sys_exit(0);
}
