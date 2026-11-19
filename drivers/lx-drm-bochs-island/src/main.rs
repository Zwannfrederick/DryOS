// SPDX-License-Identifier: GPL-2.0-or-later
//! `dry-lx-drm-bochs` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0212`) Genuine
//! Linux `v6.18` `drivers/gpu/drm/tiny/bochs.c` Driver Island (`Milestone A4-3`).
//!
//! Links unmodified upstream `bochs.c` + `libdry-lx-compat` ONLY inside this isolated crate
//! (`0` Linux kernel headers/symbols in Ring 0). Accesses BAR0 (`WC/UC`) and BAR2 (`UC`) strictly
//! via capability-gated `SYS_DEV_MAP` (`DEV_MAP_OP_MAP_WINDOW`), receives client framebuffers
//! read-only via `FrameCap` (`VSPACE_MAP_OP_2M_RO` PRIME import), and survives `#UD` faults via
//! `FaultEndpoint` quiesce + restart.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU64, Ordering};

use dry_userspace::abi_ext::{
    sys_dev_cfg_read, sys_dev_cfg_write, sys_dev_map_window, sys_dev_query_bars, UserFbInfo,
    UserPciDeviceInfo,
};
use dry_userspace::init_legacy::log_line;
use dry_userspace::ioring::{IoCqe, IoRingSharedPage, IoSqe};
use dry_userspace::island_proto::{
    island_session_badge, BADGE_ISLAND_BROKER, IPC_OP_ERR_STALE_GEN, MOCK_OP_BIND_NOTIF,
    MOCK_OP_INIT_ISLAND, MOCK_OP_SET_PARK_MODE, MOCK_OP_WAKE_PROBE,
};
use dry_userspace::lx_drv_proto::{
    IORING_OP_DRM_FLUSH, LX_DRM_OP_COMMIT_DAMAGE, LX_DRM_OP_IMPORT_DMABUF, LX_DRM_OP_MODESET,
    LX_DRM_OP_QUERY_MODE, LX_OP_BIND_PCI,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_revoke, sys_exit,
    sys_fb_map, sys_frame_alloc, sys_ioport_inb, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_ipc_reply_with_grant, sys_notify_bind_tcb, sys_notify_signal, sys_notify_unbind_tcb,
    sys_vspace_map, UserIpcRecvExt, BADGE_SUPERVISOR, EINVAL, EPERM, EREVOKED, IPC_OP_BOUND_NOTIF, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM,
    IPC_OP_EXIT_CLEAN, IPC_OP_PONG, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_REVOKE,
    RIGHT_WRITE, VSPACE_MAP_OP_2M_RO, VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_4K_RW,
    VSPACE_MAP_OP_UNMAP_2M,
};

const ISLAND_RING_VADDR: u64 = 0x2400_0000;
const BOCHS_BAR2_VADDR: u64 = 0x2500_0000;
const BOCHS_BAR0_VADDR: u64 = 0x2600_0000;
const ISLAND_DMABUF_VADDR: u64 = 0x2800_0000;
const ISLAND_RO_TEST_VADDR: u64 = 0x2820_0000;
const MAX_DMABUF_BYTES: usize = 2 * 1024 * 1024;
const MAX_WINDOW_PAGES: u32 = 64;

static ACTIVE_PCI_CAP: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DryLxDrmProbeInfo {
    edid_ok: u32,
    pref_w: u32,
    pref_h: u32,
    mapped_bar0_ok: u32,
    mapped_bar2_ok: u32,
}

#[derive(Clone, Copy, Default)]
struct DmabufState {
    cap: u64,
    bound: bool,
    w: u32,
    h: u32,
    pitch: u32,
}

#[link(name = "lx_drm_bochs", kind = "static")]
extern "C" {
    fn dry_lx_bochs_probe(
        vendor_id: u16,
        device_id: u16,
        subvendor: u16,
        subdevice: u16,
        bdf: u16,
        bar0_size: u64,
        bar0_flags: u8,
        bar2_size: u64,
        bar2_flags: u8,
        out: *mut DryLxDrmProbeInfo,
    ) -> i32;
    fn dry_lx_drm_modeset(w: u32, h: u32) -> i32;
    fn dry_lx_drm_commit_dmabuf(
        src_ptr: *const u8,
        w: u32,
        h: u32,
        pitch: u32,
        x: i32,
        y: i32,
        dw: i32,
        dh: i32,
    ) -> i32;
    fn dry_lx_drm_shutdown();
}

/// Bridge called by `libdry-lx-compat` `devm_ioremap[_wc]()` into capability-gated `SYS_DEV_MAP`.
#[no_mangle]
pub extern "C" fn dry_lx_rust_ioremap_bar(bar_idx: u32, cache_mode: u32, size_bytes: u64) -> *mut u8 {
    let pci_cap = ACTIVE_PCI_CAP.load(Ordering::Acquire);
    if pci_cap == 0 || size_bytes == 0 {
        return null_mut();
    }
    let (base_va, max_pages) = match bar_idx {
        0 => (BOCHS_BAR0_VADDR, 4096u32), // Up to 16 MiB BAR0 framebuffer aperture
        2 => (BOCHS_BAR2_VADDR, 16u32),   // Up to 64 KiB BAR2 MMIO register block
        _ => return null_mut(),
    };
    let total_pages = size_bytes.div_ceil(4096).clamp(1, max_pages as u64) as u32;
    let mut page_off = 0u32;
    while page_off < total_pages {
        let chunk = (total_pages - page_off).min(MAX_WINDOW_PAGES) as u16;
        let va = base_va + (page_off as u64) * 4096;
        let wc = cache_mode == 1;
        if sys_dev_map_window(pci_cap, va, bar_idx as u8, wc, chunk, page_off).is_err()
            && (!wc || sys_dev_map_window(pci_cap, va, bar_idx as u8, false, chunk, page_off).is_err())
        {
            return null_mut();
        }
        page_off += chunk as u32;
    }
    base_va as *mut u8
}

/// Capability-backed PCI configuration space bridge (`DEV_MAP_OP_CFG_RW`) for `libdry-lx-compat`.
///
/// # Safety
/// `val` must point to readable/writable initialized `u32` storage.
#[no_mangle]
pub unsafe extern "C" fn dry_lx_rust_pci_cfg_rw(
    off: u16,
    width: u8,
    is_write: u8,
    val: *mut u32,
) -> i32 {
    let pci_cap = ACTIVE_PCI_CAP.load(Ordering::Acquire);
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

/// Bounded diagnostic logger invoked by `libdry-lx-compat` (`dev_err`/`dev_info`).
#[no_mangle]
pub extern "C" fn dry_lx_rust_log(tag: *const u8, msg: *const u8) {
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-LX-DRM:");
    push_cstr(&mut lb, tag, 16);
    lb.push_bytes(b"] ");
    push_cstr(&mut lb, msg, 96);
    log_line(&lb);
}

fn push_cstr(lb: &mut LineBuilder, ptr: *const u8, max_len: usize) {
    if ptr.is_null() {
        return;
    }
    for i in 0..max_len {
        // SAFETY: `ptr` points to a NUL-terminated C string in static/rodata inside this ELF.
        let b = unsafe { core::ptr::read_volatile(ptr.add(i)) };
        if b == 0 {
            break;
        }
        if (0x20..=0x7e).contains(&b) {
            lb.push_byte(b);
        }
    }
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

#[inline]
fn reply_stale_gen(ep: u64, tok: u64, generation: u64) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(IPC_OP_ERR_STALE_GEN, 0, 2, 0),
            [(-EREVOKED) as u64, generation, 0, 0],
        );
    }
}

fn execute_drm_sqe(sqe: IoSqe, generation: u64, pci_bound: bool, db: DmabufState) -> IoCqe {
    if sqe.opcode != IORING_OP_DRM_FLUSH {
        return IoCqe { user_tag: sqe.user_tag, status: -22, bytes_done: 0 };
    }
    if sqe._reserved != 0 && sqe._reserved != generation {
        return IoCqe {
            user_tag: sqe.user_tag,
            status: (-EREVOKED) as i32,
            bytes_done: 0,
        };
    }
    if !pci_bound || !db.bound {
        return IoCqe { user_tag: sqe.user_tag, status: -19, bytes_done: 0 };
    }
    let x = ((sqe.buf_offset >> 16) & 0xffff) as i32;
    let y = (sqe.buf_offset & 0xffff) as i32;
    let dw = (sqe.sector_count as i32).max(1);
    let dh = (sqe.lba as i32).max(1);
    // SAFETY: `ISLAND_DMABUF_VADDR` is mapped `USER_RO` for `db.w x db.h` and bounds-checked in C.
    let rc = unsafe {
        dry_lx_drm_commit_dmabuf(
            ISLAND_DMABUF_VADDR as *const u8,
            db.w,
            db.h,
            db.pitch,
            x,
            y,
            dw,
            dh,
        )
    };
    let bytes_done = if rc == 0 { (dw as u32).saturating_mul(dh as u32).saturating_mul(4) } else { 0 };
    IoCqe {
        user_tag: sqe.user_tag,
        status: rc,
        bytes_done,
    }
}

#[no_mangle]
pub extern "C" fn _start(drv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || drv_ep_cap == 0 {
        sys_exit(190);
    }

    // Negative self-check: isolated Linux DRM island holds ZERO GOP FB or legacy VGA I/O port caps.
    let mut dummy_fb = UserFbInfo::default();
    let mut dummy_pci = UserPciDeviceInfo::default();
    let neg_rejected = sys_fb_map(0, 0x2900_0000, &mut dummy_fb).is_err()
        && sys_ioport_inb(0, 0x01ce).is_err()
        && sys_dev_query_bars(0, &mut dummy_pci).is_err()
        && sys_dev_query_bars(drv_ep_cap, &mut dummy_pci).is_err();

    let ring_cap = sys_frame_alloc(0).unwrap_or_else(|_| sys_exit(191));
    if sys_vspace_map(ring_cap, ISLAND_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(192);
    }
    // SAFETY: `ISLAND_RING_VADDR` is a freshly mapped 4 KiB `USER_RW` frame in our private VSpace.
    let ring = unsafe { IoRingSharedPage::init_at(ISLAND_RING_VADDR) };

    let mut generation = 0u64;
    let mut sq_notif = 0u64;
    let mut cq_notif = 0u64;
    let mut pci_cap = 0u64;
    let mut pci_bound = false;
    let mut probe_info = DryLxDrmProbeInfo::default();
    let mut cur_w = 0u64;
    let mut cur_h = 0u64;
    let mut db = DmabufState::default();
    let mut processed_ops = 0u64;
    let mut ring_ops = 0u64;
    let mut wake_events = 0u64;

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, tok, mrs, t_awake) = match sys_ipc_recv_ext(drv_ep_cap, 0, 0, &mut ext) {
            Ok((rtag, rtok, rmrs)) => (rtag, rtok, rmrs, rdtsc_user()),
            Err(_) => break,
        };
        wake_events = wake_events.wrapping_add(1);

        let op = msg_tag_label(tag);
        if op == IPC_OP_BOUND_NOTIF {
            let mut drained = false;
            while let Some(sqe) = ring.pop_sqe() {
                let cqe = execute_drm_sqe(sqe, generation, pci_bound, db);
                let _ = ring.push_cqe(cqe);
                processed_ops = processed_ops.wrapping_add(1);
                ring_ops = ring_ops.wrapping_add(1);
                drained = true;
            }
            if drained && cq_notif != 0 {
                let _ = sys_notify_signal(cq_notif, 1);
            }
            continue;
        }
        let from_broker = ext.sender_badge == BADGE_ISLAND_BROKER;
        let from_sup = ext.sender_badge == BADGE_SUPERVISOR;
        let from_client = generation != 0 && ext.sender_badge == island_session_badge(generation);

        match op {
            MOCK_OP_BIND_NOTIF => {
                if !from_broker || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if mrs[0] == 0 {
                    if sq_notif != 0 {
                        let _ = sys_notify_unbind_tcb(sq_notif);
                        let _ = sys_cap_revoke(sq_notif, 1);
                    }
                    sq_notif = ext.transferred_cap;
                    let _ = sys_notify_bind_tcb(sq_notif);
                } else {
                    if cq_notif != 0 {
                        let _ = sys_cap_revoke(cq_notif, 1);
                    }
                    cq_notif = ext.transferred_cap;
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
            }
            LX_OP_BIND_PCI => {
                if !from_broker || ext.transferred_cap == 0 || pci_bound {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                pci_cap = ext.transferred_cap;
                let mut pinfo = UserPciDeviceInfo::default();
                if sys_dev_query_bars(pci_cap, &mut pinfo).is_err() {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                ACTIVE_PCI_CAP.store(pci_cap, Ordering::Release);
                // SAFETY: `pinfo` comes from kernel `SYS_DEV_MAP(QUERY_BARS)` for our exclusive `pci_cap`.
                let prc = unsafe {
                    dry_lx_bochs_probe(
                        pinfo.vendor_id,
                        pinfo.device_id,
                        0x1af4,
                        0x1100,
                        pinfo.bus_dev_fn,
                        pinfo.bars[0].size_bytes,
                        pinfo.bars[0].flags as u8,
                        pinfo.bars[2].size_bytes,
                        pinfo.bars[2].flags as u8,
                        &mut probe_info,
                    )
                };
                if prc != 0 || probe_info.mapped_bar0_ok == 0 || probe_info.mapped_bar2_ok == 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, if prc != 0 { prc as isize } else { EINVAL });
                    continue;
                }
                pci_bound = true;
                let vdev = ((pinfo.vendor_id as u64) << 16) | (pinfo.device_id as u64);
                let pref = ((probe_info.pref_w as u64) << 32) | (probe_info.pref_h as u64);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [vdev, pinfo.bus_dev_fn as u64, probe_info.edid_ok as u64, pref],
                    );
                }
            }
            MOCK_OP_INIT_ISLAND => {
                if !from_broker || mrs[0] == 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                generation = mrs[0];
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-LX-DRM] Ready gen=");
                lb.push_u64(generation);
                lb.push_bytes(b" bochs.c probed edid=");
                lb.push_u64(probe_info.edid_ok as u64);
                lb.push_bytes(b" pref=");
                lb.push_u64(probe_info.pref_w as u64);
                lb.push_byte(b'x');
                lb.push_u64(probe_info.pref_h as u64);
                log_line(&lb);
                if tok != 0 {
                    let _ = sys_ipc_reply_with_grant(
                        drv_ep_cap,
                        tok,
                        IPC_OP_PONG,
                        [
                            generation,
                            u64::from(neg_rejected),
                            processed_ops,
                            probe_info.edid_ok as u64,
                        ],
                        ring_cap,
                        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT | RIGHT_REVOKE,
                        0,
                    );
                }
            }
            MOCK_OP_SET_PARK_MODE => {
                if !from_broker {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [u64::from(mrs[0] != 0), wake_events, 0, 0],
                    );
                }
            }
            MOCK_OP_WAKE_PROBE => {
                if !(from_broker || from_client) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let t_done = rdtsc_user();
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [t_awake, t_done, processed_ops, wake_events],
                    );
                }
            }
            LX_DRM_OP_QUERY_MODE => {
                if !(from_broker || from_client) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if mrs[3] != 0 && mrs[3] != generation {
                    reply_stale_gen(drv_ep_cap, tok, generation);
                    continue;
                }
                let pref = ((probe_info.pref_w as u64) << 32) | (probe_info.pref_h as u64);
                let flags = (probe_info.edid_ok as u64)
                    | ((probe_info.mapped_bar0_ok as u64) << 1)
                    | ((probe_info.mapped_bar2_ok as u64) << 2)
                    | (u64::from(db.bound) << 3);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [cur_w, cur_h, pref, flags],
                    );
                }
            }
            LX_DRM_OP_MODESET => {
                if !(from_broker || from_client) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if mrs[3] != 0 && mrs[3] != generation {
                    reply_stale_gen(drv_ep_cap, tok, generation);
                    continue;
                }
                if !pci_bound {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                let w = (mrs[0] as u32).clamp(320, 1920);
                let h = (mrs[1] as u32).clamp(240, 1200);
                // SAFETY: `pci_bound` confirms `g_drm_dev` is probed cleanly in `bochs.c`.
                let rc = unsafe { dry_lx_drm_modeset(w, h) };
                if rc != 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, rc as isize);
                    continue;
                }
                cur_w = w as u64;
                cur_h = h as u64;
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [cur_w, cur_h, generation, 0],
                    );
                }
            }
            LX_DRM_OP_IMPORT_DMABUF => {
                if !(from_broker || from_client) || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if mrs[3] != 0 && mrs[3] != generation {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_stale_gen(drv_ep_cap, tok, generation);
                    continue;
                }
                let w = (mrs[0] as u32).clamp(64, 1024);
                let h = (mrs[1] as u32).clamp(64, 768);
                let pitch = (mrs[2] as u32).max(w * 4);
                if (pitch as usize).saturating_mul(h as usize) > MAX_DMABUF_BYTES {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                if db.bound && db.cap != 0 {
                    let _ = sys_vspace_map(db.cap, ISLAND_DMABUF_VADDR, VSPACE_MAP_OP_UNMAP_2M);
                    let _ = sys_cap_revoke(db.cap, 1);
                    db = DmabufState::default();
                }
                let ro_enforced =
                    sys_vspace_map(ext.transferred_cap, ISLAND_RO_TEST_VADDR, VSPACE_MAP_OP_2M_RW)
                        .is_err();
                if let Err(e) =
                    sys_vspace_map(ext.transferred_cap, ISLAND_DMABUF_VADDR, VSPACE_MAP_OP_2M_RO)
                {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, e);
                    continue;
                }
                db = DmabufState {
                    cap: ext.transferred_cap,
                    bound: true,
                    w,
                    h,
                    pitch,
                };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [u64::from(ro_enforced), w as u64, h as u64, pitch as u64],
                    );
                }
            }
            LX_DRM_OP_COMMIT_DAMAGE => {
                if !(from_broker || from_client) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if mrs[3] != 0 && mrs[3] != generation {
                    reply_stale_gen(drv_ep_cap, tok, generation);
                    continue;
                }
                if !pci_bound || !db.bound || cur_w == 0 || cur_h == 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                let x = (mrs[0] & 0xffff_ffff) as i32;
                let y = (mrs[0] >> 32) as i32;
                let raw_w = (mrs[1] & 0xffff_ffff) as i32;
                let raw_h = (mrs[1] >> 32) as i32;
                let dw = if raw_w > 0 { raw_w } else { db.w as i32 };
                let dh = if raw_h > 0 { raw_h } else { db.h as i32 };
                // SAFETY: `ISLAND_DMABUF_VADDR` is mapped `USER_RO` and `dw`/`dh` are clamped in C.
                let rc = unsafe {
                    dry_lx_drm_commit_dmabuf(
                        ISLAND_DMABUF_VADDR as *const u8,
                        db.w,
                        db.h,
                        db.pitch,
                        x,
                        y,
                        dw,
                        dh,
                    )
                };
                if rc != 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, rc as isize);
                    continue;
                }
                processed_ops = processed_ops.wrapping_add(1);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [processed_ops, generation, 0, 0],
                    );
                }
            }
            IPC_OP_CRASH_UD2 => {
                if !(from_sup || from_broker) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                // SAFETY: Controlled fault-injection test triggering `#UD` (`vector = 6`) into `FaultEndpoint`.
                unsafe {
                    asm!("ud2", options(nostack, nomem));
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if !(from_sup || from_broker) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if pci_bound {
                    // SAFETY: Calls `bochs.c` `bochs_pci_shutdown()` while BARs are still mapped.
                    unsafe {
                        dry_lx_drm_shutdown();
                    }
                }
                if db.bound && db.cap != 0 {
                    let _ = sys_vspace_map(db.cap, ISLAND_DMABUF_VADDR, VSPACE_MAP_OP_UNMAP_2M);
                    let _ = sys_cap_revoke(db.cap, 1);
                }
                if sq_notif != 0 {
                    let _ = sys_notify_unbind_tcb(sq_notif);
                    let _ = sys_cap_revoke(sq_notif, 1);
                }
                if cq_notif != 0 {
                    let _ = sys_cap_revoke(cq_notif, 1);
                }
                if pci_cap != 0 {
                    ACTIVE_PCI_CAP.store(0, Ordering::Release);
                    let _ = sys_cap_revoke(pci_cap, 1);
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [1, processed_ops, ring_ops, 0],
                    );
                }
                sys_exit(0);
            }
            _ => reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL),
        }
    }
    sys_exit(0);
}
