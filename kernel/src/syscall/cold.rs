// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Cold Control-Plane Syscall Dispatcher (`SYS_CAP_*`, `SYS_PROC_*`, `SYS_FB_MAP`, `SYS_IRQ_*`,
//! `SYS_IOPORT_RW`, `SYS_SYSINFO`, `SYS_FRAME_ALLOC`, `SYS_VSPACE_MAP`).
//!
//! Marked `#[inline(never)]` so `rust_syscall_dispatch` preserves a compact hot-path stack frame
//! inside the 16 KiB kernel stack budget.

use core::mem::size_of;
use core::sync::atomic::Ordering;

use crate::apic::{active_apic_mode, uptime_telemetry};
use crate::cspace::cap::{CapError, CapPtr};
use crate::cspace::objects::{
    cspace_alloc_frame, cspace_vspace_map_2m, cspace_vspace_map_4k, cspace_vspace_unmap_2m,
    cspace_vspace_unmap_4k,
};
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object, cspace_revoke_children};
use crate::cspace::{cspace_derive, with_pinned_cap};
use crate::display::{is_framebuffer_available, sys_fb_map};
use crate::ipc::endpoint::ep_create;
use crate::ipc::{map_cap_err_to_sys, total_ipc_msgs};
use crate::ipc::notif::notif_create;
use crate::irq::ioport::sys_ioport_rw;
use crate::irq::{
    ioapic_present, irq_line_delivered, irq_telemetry, sys_irq_ack, sys_irq_bind,
    IRQ_KEYBOARD_PS2, IRQ_MOUSE_PS2,
};
use crate::percpu::get_percpu_by_idx;
use crate::pmm::{free_global_frames_snapshot, inventory, PageOrder};
use crate::proc::process_vspace_cap;
use crate::proc::supervisor::{sys_proc_kill, sys_proc_list, sys_proc_spawn, sys_proc_wait};
use crate::sched::{tcb, total_ctx_switches, GLOBAL_TIMER_IRQS, ONLINE_SCHED_CPUS};
use crate::security::{
    current_lease_epoch, unpack_seclabel_u64, CapRights, SecDomain, SecFlags, SecLabel,
};
use crate::syscall::{
    UserCapInfo, DRY_SYSCALL_ABI_VERSION, SYS_CAP_DERIVE, SYS_CAP_QUERY, SYS_CAP_REVOKE,
    SYS_DEV_MAP, SYS_DMA_ALLOC, SYS_EP_CREATE, SYS_FB_MAP, SYS_FRAME_ALLOC, SYS_IOPORT_RW,
    SYS_IRQ_ACK, SYS_IRQ_BIND, SYS_NOTIFY_CREATE, SYS_POWER_CTL, SYS_PROC_KILL, SYS_PROC_LIST,
    SYS_PROC_SPAWN, SYS_PROC_WAIT, SYS_SYSINFO, SYS_VAULT_CTL, SYS_VSPACE_MAP,
};
use crate::uaccess::{copy_to_user, SysError};
use crate::vspace::pte::VmProt;

pub const VSPACE_MAP_OP_4K_RO: u64 = 0;
pub const VSPACE_MAP_OP_4K_RW: u64 = 1;
pub const VSPACE_MAP_OP_2M_RO: u64 = 2;
pub const VSPACE_MAP_OP_2M_RW: u64 = 3;
pub const VSPACE_MAP_OP_UNMAP_4K: u64 = 4;
pub const VSPACE_MAP_OP_UNMAP_2M: u64 = 5;

/// Sanitized 104-byte system telemetry POD copied to CPL=3 by `SYS_SYSINFO` (zero kernel ptrs/PFNs).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserSysInfo {
    pub abi_version: u64,
    pub online_cpus: u32,
    pub apic_mode: u8,
    pub ioapic_present: u8,
    pub fb_present: u8,
    pub _pad0: u8,
    pub tsc_mhz: u64,
    pub uptime_ms: u64,
    pub elapsed_tsc: u64,
    pub timer_irqs: u64,
    pub total_ext_irqs: u64,
    pub total_syscalls: u64,
    pub total_ctx_switches: u64,
    pub usable_frames_4k: u32,
    pub free_frames_4k: u32,
    pub irq1_delivered: u64,
    pub irq12_delivered: u64,
    pub total_ipc_msgs: u64,
}

const _: () = assert!(size_of::<UserSysInfo>() == 104);

fn decode_target_obj_label(subj_label: SecLabel, spec: u64) -> Result<SecLabel, SysError> {
    if spec == 0 {
        return Ok(subj_label);
    }
    let dom = match (spec & 0xff) as u8 {
        0 => SecDomain::Userspace,
        1 => SecDomain::RootSystem,
        _ => return Err(SysError::Perm),
    };
    let comp = ((spec >> 16) & 0xffff) as u16;
    Ok(SecLabel::new(dom, SecFlags::NONE, comp, 1))
}

fn sys_cap_query_impl(
    cspace_id: u16,
    subj_raw: u64,
    cap_raw: u64,
    user_out_ptr: u64,
) -> Result<isize, SysError> {
    if (user_out_ptr & 7) != 0 {
        return Err(SysError::Inval);
    }
    let subj_label = unpack_seclabel_u64(subj_raw);
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(cap_raw);

    let info = with_pinned_cap(cspace_id, subj_label, ptr, CapRights::READ, now_epoch, |res| {
        Ok(UserCapInfo {
            obj_kind: res.obj_kind as u8,
            domain: res.obj_label.domain as u8,
            flags: res.obj_label.flags.0 as u16,
            rights_bits: res.rights.0 as u32,
            compartment_id: res.obj_label.compartment_id,
            _pad: 0,
            lease_epoch: res.lease_epoch,
            badge: res.badge,
            abi_reserved: 0,
        })
    })
    .map_err(map_cap_err_to_sys)?;

    // SAFETY: `UserCapInfo` is `#[repr(C)]` POD (`32` bytes) with `_pad = 0`.
    let bytes = unsafe {
        core::slice::from_raw_parts((&raw const info).cast::<u8>(), size_of::<UserCapInfo>())
    };
    copy_to_user(user_out_ptr, bytes)?;
    Ok(0)
}

#[inline(never)]
fn sys_sysinfo_impl(user_out_ptr: u64) -> Result<isize, SysError> {
    if user_out_ptr == 0 || (user_out_ptr & 7) != 0 {
        return Err(SysError::Fault);
    }
    let online = ONLINE_SCHED_CPUS.load(Ordering::Acquire);
    let mut syscalls = 0u64;
    for cpu in 0..online {
        syscalls = syscalls.wrapping_add(get_percpu_by_idx(cpu).syscall_count.load(Ordering::Relaxed));
    }
    let (tsc_mhz, uptime_ms, elapsed_tsc) = uptime_telemetry();
    let (ext_irqs, _, _, _) = irq_telemetry();
    let inv = inventory();
    let free_frames = free_global_frames_snapshot(online);

    let info = UserSysInfo {
        abi_version: DRY_SYSCALL_ABI_VERSION,
        online_cpus: online as u32,
        apic_mode: active_apic_mode() as u8,
        ioapic_present: u8::from(ioapic_present()),
        fb_present: u8::from(is_framebuffer_available()),
        _pad0: 0,
        tsc_mhz,
        uptime_ms,
        elapsed_tsc,
        timer_irqs: GLOBAL_TIMER_IRQS.load(Ordering::Relaxed),
        total_ext_irqs: ext_irqs,
        total_syscalls: syscalls,
        total_ctx_switches: total_ctx_switches(),
        usable_frames_4k: (inv.usable_frames + inv.reclaimable_frames).min(u32::MAX as u64) as u32,
        free_frames_4k: free_frames.min(u32::MAX as u64) as u32,
        irq1_delivered: irq_line_delivered(IRQ_KEYBOARD_PS2),
        irq12_delivered: irq_line_delivered(IRQ_MOUSE_PS2),
        total_ipc_msgs: total_ipc_msgs(online),
    };

    // SAFETY: `UserSysInfo` is `#[repr(C)]` POD (`104` bytes) with `_pad0 = 0`.
    let bytes = unsafe {
        core::slice::from_raw_parts((&raw const info).cast::<u8>(), size_of::<UserSysInfo>())
    };
    copy_to_user(user_out_ptr, bytes)?;
    Ok(0)
}

#[inline(never)]
fn sys_frame_alloc_impl(
    cspace_id: u16,
    subj: SecLabel,
    order_raw: u64,
    flags: u64,
) -> Result<u64, SysError> {
    if flags != 0 {
        return Err(SysError::Inval);
    }
    let order = match order_raw {
        0 => PageOrder::Order4KiB,
        1 => PageOrder::Order2MiB,
        _ => return Err(SysError::Inval),
    };
    // Root-allocated process frames carry `lease_epoch = 0` (`permanent`); callers attenuate into
    // time/epoch-bounded leases via `SYS_CAP_DERIVE` (`INV-SEC-04`, `INV-SEC-07`).
    let (cap, _fh) =
        cspace_alloc_frame(cspace_id, subj, subj, order, false, 0).map_err(map_cap_err_to_sys)?;
    Ok(cap.0)
}

#[inline(never)]
fn sys_vspace_map_impl(
    pid: u32,
    cspace_id: u16,
    subj: SecLabel,
    frame_cap_raw: u64,
    vaddr: u64,
    op: u64,
) -> Result<(), SysError> {
    let vs_cap = process_vspace_cap(pid).ok_or(SysError::Perm)?;
    let frame_cap = CapPtr(frame_cap_raw);
    let now = current_lease_epoch();
    let res = match op {
        VSPACE_MAP_OP_4K_RO => {
            cspace_vspace_map_4k(cspace_id, subj, vs_cap, frame_cap, vaddr, VmProt::USER_RO, now)
        }
        VSPACE_MAP_OP_4K_RW => {
            cspace_vspace_map_4k(cspace_id, subj, vs_cap, frame_cap, vaddr, VmProt::USER_RW, now)
        }
        VSPACE_MAP_OP_2M_RO => {
            cspace_vspace_map_2m(cspace_id, subj, vs_cap, frame_cap, vaddr, VmProt::USER_RO, now)
        }
        VSPACE_MAP_OP_2M_RW => {
            cspace_vspace_map_2m(cspace_id, subj, vs_cap, frame_cap, vaddr, VmProt::USER_RW, now)
        }
        VSPACE_MAP_OP_UNMAP_4K => {
            cspace_vspace_unmap_4k(cspace_id, subj, vs_cap, frame_cap, vaddr, now)
        }
        VSPACE_MAP_OP_UNMAP_2M => {
            cspace_vspace_unmap_2m(cspace_id, subj, vs_cap, frame_cap, vaddr, now)
        }
        _ => return Err(SysError::Inval),
    };
    res.map_err(map_cap_err_to_sys)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn sys_cold_dispatch(
    nr: u64,
    pid: u32,
    cur_slot: usize,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> (isize, u64) {
    let cur_tcb = tcb(cur_slot);
    match nr {
        SYS_CAP_QUERY => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj_raw = cur_tcb.subj_label_raw.load(Ordering::Acquire);
            match sys_cap_query_impl(cs_id, subj_raw, a0, a1) {
                Ok(v) => (v, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_EP_CREATE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match decode_target_obj_label(subj, a0)
                .and_then(|obj_lbl| ep_create(cs_id, subj, obj_lbl, 0).map_err(map_cap_err_to_sys))
            {
                Ok((cap, _)) => (0, cap.0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_NOTIFY_CREATE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match decode_target_obj_label(subj, a0)
                .and_then(|obj_lbl| notif_create(cs_id, subj, obj_lbl, 0).map_err(map_cap_err_to_sys))
            {
                Ok((cap, _)) => (0, cap.0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_CAP_DERIVE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let now = current_lease_epoch();
            match cspace_derive(
                cs_id,
                subj,
                CapPtr(a0),
                CapRights((a1 & 0xffff) as u16),
                a2 as u32,
                a3,
                now,
            )
            .map_err(map_cap_err_to_sys)
            {
                Ok(cap) => (0, cap.0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_CAP_REVOKE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let now = current_lease_epoch();
            let res: Result<usize, CapError> = match a1 {
                0 => cspace_revoke_children(cs_id, subj, CapPtr(a0), now),
                1 => cspace_delete_cap(cs_id, subj, CapPtr(a0), now).map(|()| 1),
                2 => cspace_destroy_object(cs_id, subj, CapPtr(a0), now).map(|()| 1),
                _ => Err(CapError::InvalidHandle),
            };
            match res.map_err(map_cap_err_to_sys) {
                Ok(n) => (n as isize, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_PROC_SPAWN => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_proc_spawn(pid, cs_id, subj, a0, a1, a2, a3, a4, a5) {
                Ok(pair) => pair,
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_PROC_WAIT => match sys_proc_wait(pid, a0, a1, a2) {
            Ok(pair) => pair,
            Err(e) => (e.as_raw(), 0),
        },
        SYS_FB_MAP => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let vs_id = cur_tcb.vspace_id.load(Ordering::Acquire);
            match sys_fb_map(cs_id, subj, vs_id, a0, a1, a2) {
                Ok(pages) => (pages, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IRQ_BIND => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_irq_bind(cs_id, subj, CapPtr(a0), CapPtr(a1), a2) {
                Ok(vec) => (vec as isize, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IRQ_ACK => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_irq_ack(cs_id, subj, CapPtr(a0), a1) {
                Ok(cnt) => (0, cnt),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IOPORT_RW => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_ioport_rw(cs_id, subj, CapPtr(a0), a1, a2, a3) {
                Ok(val) => (val as isize, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_SYSINFO => match sys_sysinfo_impl(a0) {
            Ok(v) => (v, 0),
            Err(e) => (e.as_raw(), 0),
        },
        SYS_FRAME_ALLOC => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_frame_alloc_impl(cs_id, subj, a0, a1) {
                Ok(cap_raw) => (0, cap_raw),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_VSPACE_MAP => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_vspace_map_impl(pid, cs_id, subj, a0, a1, a2) {
                Ok(()) => (0, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_PROC_LIST => match sys_proc_list(a0, a1) {
            Ok(pair) => pair,
            Err(e) => (e.as_raw(), 0),
        },
        SYS_PROC_KILL => {
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_proc_kill(pid, subj, a0, a1) {
                Ok(()) => (0, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_POWER_CTL => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match crate::power::sys_power_ctl(cs_id, subj, a0, a1) {
                Ok(v) => (v, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_DEV_MAP => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let vs_id = cur_tcb.vspace_id.load(Ordering::Acquire);
            match crate::pci::sys_dev_map(cs_id, subj, vs_id, a0, a1, a2) {
                Ok(pages) => (pages, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_DMA_ALLOC => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match crate::pci::sys_dma_alloc(pid, cs_id, subj, a0, a1, a2) {
                Ok(pair) => pair,
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_VAULT_CTL => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match crate::vault_ctrl::sys_vault_ctl(cs_id, subj, a0, a1, a2, a3) {
                Ok(pair) => pair,
                Err(e) => (e.as_raw(), 0),
            }
        }
        _ => (SysError::NoSys.as_raw(), 0),
    }
}
