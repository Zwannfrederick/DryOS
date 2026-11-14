// SPDX-License-Identifier: GPL-2.0-only
//! `dry-lx-rng` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0210`) Genuine Linux
//! `v6.18` `virtio-rng.c` Driver Island (`Milestone 2C-1` — `ADR-005`, `ADR-016`).
//!
//! Runs unmodified upstream `vendor/linux-drivers/v6.18/virtio_rng/virtio-rng.c` linked via
//! `liblx_virtio_rng.a` with `0` Linux kernel ABI in Ring 0, backed exclusively by
//! capability-mediated `SYS_DEV_MAP` + `SYS_DMA_ALLOC` (`vq` + `slab` pinned `DEVICE_DMA` frames)
//! and supervised via `FaultEndpoint` (`#UD` -> hardware `Command.BME = 0` quiesce -> restart).

#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use dry_userspace::abi_ext::{sys_dev_map, sys_dma_alloc, UserVirtioPciInfo};
use dry_userspace::init_legacy::log_line;
use dry_userspace::ioring::{IoCqe, IoRingSharedPage};
use dry_userspace::island_proto::{
    island_session_badge, BADGE_ISLAND_BROKER, IPC_OP_ERR_STALE_GEN, MOCK_OP_BIND_NOTIF,
    MOCK_OP_INIT_ISLAND, MOCK_OP_SET_PARK_MODE, MOCK_OP_WAKE_PROBE,
};
use dry_userspace::lx_drv_proto::{
    IORING_OP_RNG_READ, IORING_RNG_DATA_OFFSET, LX_OP_BIND_PCI, LX_OP_READ_ENTROPY,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_revoke, sys_exit,
    sys_frame_alloc, sys_ipc_recv_ext, sys_ipc_reply_inline, sys_ipc_reply_with_grant,
    sys_irq_ack, sys_irq_bind, sys_notify_bind_tcb, sys_notify_create, sys_notify_signal,
    sys_notify_unbind_tcb, sys_notify_wait, sys_vspace_map, UserIpcRecvExt, BADGE_SUPERVISOR,
    EINVAL, EPERM, EREVOKED, IPC_OP_BOUND_NOTIF, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, IRQF_ONESHOT, IRQ_ACK_UNBIND,
    IRQ_ACK_UNMASK, MSG_FLAG_NON_BLOCKING, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_REVOKE,
    RIGHT_WRITE, VSPACE_MAP_OP_4K_RW,
};

const ISLAND_RING_VADDR: u64 = 0x2400_0000;
const VIRTIO_RNG_BAR_VADDR: u64 = 0x2410_0000;
const DMA_VQ_VADDR: u64 = 0x2420_0000;
const DMA_SLAB_VADDR: u64 = 0x2420_1000;

static ACTIVE_PCI_CAP: AtomicU64 = AtomicU64::new(0);
static ACTIVE_IRQ_NOTIF: AtomicU64 = AtomicU64::new(0);
static MSIX_IRQS_HANDLED: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
pub extern "C" fn dry_lx_wait_vq_irq(timeout_tsc: u64) -> i32 {
    let notif = ACTIVE_IRQ_NOTIF.load(Ordering::Acquire);
    let pci_cap = ACTIVE_PCI_CAP.load(Ordering::Acquire);
    if notif == 0 || pci_cap == 0 {
        return 0;
    }
    let flags = if timeout_tsc == 0 {
        MSG_FLAG_NON_BLOCKING as u64
    } else {
        0
    };
    if let Ok(bits) = sys_notify_wait(notif, flags, timeout_tsc) {
        if bits != 0 {
            MSIX_IRQS_HANDLED.fetch_add(1, Ordering::Relaxed);
            let _ = sys_irq_ack(pci_cap, IRQ_ACK_UNMASK);
            return 1;
        }
    }
    0
}

#[link(name = "lx_virtio_rng", kind = "static")]
extern "C" {
    fn lx_shim_init_device(
        bar_va: u64,
        common_off: u32,
        notify_off: u32,
        notify_mult: u32,
        isr_off: u32,
        vq_va: u64,
        vq_pa: u64,
        slab_va: u64,
        slab_pa: u64,
        use_msix: u32,
    ) -> i32;
    fn lx_shim_read_entropy(out_buf: *mut u8, len: usize) -> i32;
    fn lx_shim_shutdown_device();
    fn lx_shim_irq_completions() -> u64;
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

fn execute_rng_sqe(
    sqe: dry_userspace::ioring::IoSqe,
    generation: u64,
    pci_bound: bool,
) -> IoCqe {
    if sqe.opcode != IORING_OP_RNG_READ {
        return IoCqe {
            user_tag: 0,
            status: -22,
            bytes_done: 0,
        };
    }
    if sqe._reserved != 0 && sqe._reserved != generation {
        return IoCqe {
            user_tag: 0,
            status: (-EREVOKED) as i32,
            bytes_done: 0,
        };
    }
    if !pci_bound {
        return IoCqe {
            user_tag: 0,
            status: -19,
            bytes_done: 0,
        };
    }
    let len = sqe.sector_count as usize;
    let off = sqe.buf_offset as usize;
    if len == 0 || len > 64 || off < (IORING_RNG_DATA_OFFSET as usize) || off + len > 4096 {
        return IoCqe {
            user_tag: 0,
            status: -22,
            bytes_done: 0,
        };
    }
    let dst_ptr = (ISLAND_RING_VADDR + (off as u64)) as *mut u8;
    // SAFETY: `dst_ptr..dst_ptr+len` lies inside `IoRingSharedPage` (`[2048..4096)`).
    let rc = unsafe { lx_shim_read_entropy(dst_ptr, len) };
    if rc < 0 {
        return IoCqe {
            user_tag: 0,
            status: rc,
            bytes_done: 0,
        };
    }
    let mut head = [0u8; 8];
    let n = (rc as usize).min(8);
    // SAFETY: Reading back freshly populated RNG bytes within the mapped ring page.
    unsafe {
        core::ptr::copy_nonoverlapping(dst_ptr, head.as_mut_ptr(), n);
    }
    IoCqe {
        user_tag: u64::from_le_bytes(head),
        status: 0,
        bytes_done: rc as u32,
    }
}

#[no_mangle]
pub extern "C" fn _start(drv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || drv_ep_cap == 0 {
        sys_exit(190);
    }

    // Negative self-check: kernel MUST reject zero/wrong-kind PCI caps on `SYS_DEV_MAP`/`SYS_DMA_ALLOC`.
    let mut dummy_info = UserVirtioPciInfo::default();
    let neg_rejected = sys_dev_map(0, VIRTIO_RNG_BAR_VADDR, &mut dummy_info).is_err()
        && sys_dma_alloc(drv_ep_cap, DMA_VQ_VADDR).is_err()
        && sys_dev_map(drv_ep_cap, VIRTIO_RNG_BAR_VADDR, &mut dummy_info).is_err();

    let ring_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(191),
    };
    if sys_vspace_map(ring_cap, ISLAND_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(192);
    }
    // SAFETY: `ISLAND_RING_VADDR` is a mapped 4 KiB `USER_RW` frame in our private `VSpace`.
    let ring = unsafe { IoRingSharedPage::init_at(ISLAND_RING_VADDR) };

    let mut generation = 0u64;
    let mut sq_notif = 0u64;
    let mut cq_notif = 0u64;
    let mut pci_cap = 0u64;
    let mut irq_notif = 0u64;
    let mut msix_vec = 0u64;
    let mut pci_bound = false;
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
                let cqe = execute_rng_sqe(sqe, generation, pci_bound);
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
        let expected_sess = island_session_badge(generation);
        let from_client = generation != 0 && ext.sender_badge == expected_sess;

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
                let mut pci_info = UserVirtioPciInfo::default();
                let Ok(_) = sys_dev_map(pci_cap, VIRTIO_RNG_BAR_VADDR, &mut pci_info) else {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                };
                if let Ok(ncap) = sys_notify_create(0) {
                    if let Ok(vec) = sys_irq_bind(pci_cap, ncap, IRQF_ONESHOT) {
                        irq_notif = ncap;
                        msix_vec = vec as u64;
                        ACTIVE_PCI_CAP.store(pci_cap, Ordering::Release);
                        ACTIVE_IRQ_NOTIF.store(ncap, Ordering::Release);
                    } else {
                        let _ = sys_cap_revoke(ncap, 2);
                    }
                }
                let Ok((_, vq_pa)) = sys_dma_alloc(pci_cap, DMA_VQ_VADDR) else {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                };
                let Ok((_, slab_pa)) = sys_dma_alloc(pci_cap, DMA_SLAB_VADDR) else {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                };
                // SAFETY: Mapped BAR + 2 pinned `DEVICE_DMA` frames belong exclusively to this island.
                let init_rc = unsafe {
                    lx_shim_init_device(
                        VIRTIO_RNG_BAR_VADDR,
                        pci_info.common_off,
                        pci_info.notify_off,
                        pci_info.notify_mult,
                        pci_info.isr_off,
                        DMA_VQ_VADDR,
                        vq_pa,
                        DMA_SLAB_VADDR,
                        slab_pa,
                        u32::from(irq_notif != 0),
                    )
                };
                if init_rc != 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, init_rc as isize);
                    continue;
                }
                pci_bound = true;
                let vdev_packed =
                    ((pci_info.vendor_id as u64) << 16) | (pci_info.device_id as u64);
                let compl = unsafe { lx_shim_irq_completions() };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [vdev_packed, pci_info.bus_dev_fn as u64, compl, msix_vec],
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
                lb.push_bytes(b"[DRY-LX-RNG] Ready gen=");
                lb.push_u64(generation);
                lb.push_bytes(b" virtio-rng.c probed pci_bound=");
                lb.push_u64(u64::from(pci_bound));
                lb.push_bytes(b" msix_vec=");
                lb.push_u64(msix_vec);
                log_line(&lb);
                if tok != 0 {
                    let _ = sys_ipc_reply_with_grant(
                        drv_ep_cap,
                        tok,
                        IPC_OP_PONG,
                        [generation, u64::from(neg_rejected), processed_ops, 0],
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
                let park_mode = if mrs[0] != 0 { 1 } else { 0 };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [park_mode, wake_events, 0, 0],
                    );
                }
            }
            LX_OP_READ_ENTROPY => {
                if !(from_broker || from_client) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
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
                let req_len = (mrs[0] as usize).clamp(1, 24);
                let mut buf = [0u8; 24];
                // SAFETY: `buf` is a 24-byte stack buffer valid for `req_len <= 24`.
                let rc = unsafe { lx_shim_read_entropy(buf.as_mut_ptr(), req_len) };
                if rc < 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, rc as isize);
                    continue;
                }
                processed_ops = processed_ops.wrapping_add(1);
                let w0 = u64::from_le_bytes(buf[0..8].try_into().unwrap());
                let w1 = u64::from_le_bytes(buf[8..16].try_into().unwrap());
                let w2 = u64::from_le_bytes(buf[16..24].try_into().unwrap());
                let compl = unsafe { lx_shim_irq_completions() };
                let meta = (generation & 0xffff_ffff) | ((compl & 0xffff_ffff) << 32);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [w0, w1, w2, meta],
                    );
                }
            }
            MOCK_OP_WAKE_PROBE => {
                if !(from_broker || from_client) {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let t_done = rdtsc_user();
                let compl = unsafe { lx_shim_irq_completions() };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [t_awake, t_done, compl, wake_events],
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
                    // SAFETY: Tears down `virtio-rng.c` via `virtrng_remove()` -> `virtio_reset_device()`.
                    unsafe {
                        lx_shim_shutdown_device();
                    }
                }
                if sq_notif != 0 {
                    let _ = sys_notify_unbind_tcb(sq_notif);
                    let _ = sys_cap_revoke(sq_notif, 1);
                }
                if cq_notif != 0 {
                    let _ = sys_cap_revoke(cq_notif, 1);
                }
                if irq_notif != 0 {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-LX-RNG] MSIX clean_exit gen=");
                    lb.push_u64(generation);
                    lb.push_bytes(b" msix_irqs=");
                    lb.push_u64(MSIX_IRQS_HANDLED.load(Ordering::Acquire));
                    log_line(&lb);
                    ACTIVE_IRQ_NOTIF.store(0, Ordering::Release);
                    ACTIVE_PCI_CAP.store(0, Ordering::Release);
                    let _ = sys_irq_ack(pci_cap, IRQ_ACK_UNBIND);
                    let _ = sys_cap_revoke(irq_notif, 2);
                }
                if pci_cap != 0 {
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
