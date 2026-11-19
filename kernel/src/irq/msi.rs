// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated PCI `MSI` (`0x05`) & `MSI-X` (`0x11`) Interrupt Subsystem (`Milestone 2C-2`).
//!
//! Security & Hardware Contracts:
//! 1. Ring-3 drivers NEVER write raw LAPIC destination addresses or IDT vectors (`msg_addr`, `msg_data`),
//!    and `sys_dev_map` / `sys_dev_map_window` forbid mapping `MSI-X` table pages into CPL=3.
//! 2. `MSI-X` (`0x11`) is preferred over `MSI` (`0x05`) when present; hardware `Vector Control.Mask`
//!    (or `MSI MaskBits` / `MSI Enable`) is masked under `IRQF_ONESHOT` strictly *before* `lapic_eoi()`.
//! 3. Revoking `PciDev` / `Notification` capabilities or exiting the driver process masks all entries
//!    and disables `MSI-X`/`MSI` before unbinding vectors or releasing DMA pins.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::apic::{
    bsp_apic_msi_dest_id, lapic_eoi, PCI_MSI_VECTOR_COUNT, VEC_PCI_MSI_BASE, VEC_PCI_MSI_LAST,
};
use crate::arch::{irq_restore, irq_save_cli, rdtsc};
use crate::cspace::cap::{CapError, CapObjectKind, CapPtr, CDT_NONE};
use crate::cspace::{with_pinned_cap, CSpaceId};
use crate::ipc::map_cap_err_to_sys;
use crate::ipc::notif::notif_signal_from_irq;
use crate::pci::{
    claim_pci_slot_for_irq, is_pci_cap_live, pci_read_u16, pci_read_u32, pci_write_u16,
    pci_write_u32, release_claimed_pci_slot, validated_pci_slot,
};
use crate::percpu::current_percpu;
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::uaccess::SysError;
use crate::vspace::pte::pci_msix_table_vaddr;

use super::{
    IRQF_ONESHOT, IRQ_ACK_MASK, IRQ_ACK_UNBIND, IRQ_ACK_UNMASK, STORM_BURST_LIMIT,
    STORM_WINDOW_TSC,
};

const PCI_CMD_OFF: u8 = 0x04;
const PCI_CMD_BUS_MASTER: u16 = 1 << 2;
const PCI_CMD_MEM_SPACE: u16 = 1 << 1;
const PCI_CMD_INTX_DISABLE: u16 = 1 << 10;

const MSIX_CTRL_ENABLE: u16 = 1 << 15;
const MSIX_CTRL_FN_MASK: u16 = 1 << 14;
const MSIX_VEC_MASK_BIT: u32 = 1 << 0;

const MSI_CTRL_ENABLE: u16 = 1 << 0;
const MSI_CTRL_MME_MASK: u16 = 0x7 << 4;
const MSI_CTRL_64BIT: u16 = 1 << 7;
const MSI_CTRL_PVM: u16 = 1 << 8;

static TOTAL_MSI_IRQS: AtomicU64 = AtomicU64::new(0);
static TOTAL_MSI_ACKS: AtomicU64 = AtomicU64::new(0);
static TOTAL_MSI_SPURIOUS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct MsiInner {
    bound: bool,
    dev_idx: usize,
    entry_idx: u16,
    is_msix: bool,
    bus: u8,
    dev: u8,
    func: u8,
    cap_off: u8,
    msi_64bit: bool,
    msi_pvm: bool,
    table_entry_va: u64,
    oneshot: bool,
    masked_in_hw: bool,
    oneshot_pending_ack: bool,
    notif_obj_id: u32,
    notif_obj_gen: u32,
    notif_global_slot: u32,
    signal_bits: u64,
    delivered_count: u64,
    storm_window_start_tsc: u64,
    storm_window_count: u32,
}

impl MsiInner {
    const fn empty() -> Self {
        Self {
            bound: false,
            dev_idx: 0,
            entry_idx: 0,
            is_msix: false,
            bus: 0,
            dev: 0,
            func: 0,
            cap_off: 0,
            msi_64bit: false,
            msi_pvm: false,
            table_entry_va: 0,
            oneshot: true,
            masked_in_hw: true,
            oneshot_pending_ack: false,
            notif_obj_id: 0,
            notif_obj_gen: 0,
            notif_global_slot: CDT_NONE,
            signal_bits: 0,
            delivered_count: 0,
            storm_window_start_tsc: 0,
            storm_window_count: 0,
        }
    }
}

struct MsiSlot {
    allocated: AtomicBool,
    lock: AtomicBool,
    inner: UnsafeCell<MsiInner>,
}

// SAFETY: `inner` is mutated and read strictly while holding `lock` under `irq_save_cli()`.
unsafe impl Sync for MsiSlot {}

impl MsiSlot {
    const fn new() -> Self {
        Self {
            allocated: AtomicBool::new(false),
            lock: AtomicBool::new(false),
            inner: UnsafeCell::new(MsiInner::empty()),
        }
    }
}

static MSI_SLOTS: [MsiSlot; PCI_MSI_VECTOR_COUNT] = [const { MsiSlot::new() }; PCI_MSI_VECTOR_COUNT];

#[inline]
fn with_msi_slot_lock<R>(slot: &MsiSlot, f: impl FnOnce(&mut MsiInner) -> R) -> R {
    let saved_if = irq_save_cli();
    while slot
        .lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: Exclusive `slot.lock` + `CLI` held.
    let out = f(unsafe { &mut *slot.inner.get() });
    slot.lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline]
pub fn msi_telemetry() -> (u64, u64, u64) {
    (
        TOTAL_MSI_IRQS.load(Ordering::Relaxed),
        TOTAL_MSI_ACKS.load(Ordering::Relaxed),
        TOTAL_MSI_SPURIOUS.load(Ordering::Relaxed),
    )
}

#[inline]
fn msix_entry_vaddr(dev_idx: usize, table_off: u32, entry_idx: u16) -> u64 {
    pci_msix_table_vaddr(dev_idx) + ((table_off as u64) & 0xfff) + (entry_idx as u64) * 16
}

fn hw_mask_msi_entry(m: &MsiInner, mask: bool) {
    if m.is_msix && m.table_entry_va != 0 {
        let ctrl_ptr = (m.table_entry_va + 12) as *mut u32;
        // SAFETY: `table_entry_va` is in pre-mapped UC Ring-0 `PML4[385]` MSI-X window.
        unsafe {
            let cur = read_volatile(ctrl_ptr);
            let next = if mask {
                cur | MSIX_VEC_MASK_BIT
            } else {
                cur & !MSIX_VEC_MASK_BIT
            };
            write_volatile(ctrl_ptr, next);
            let _ = read_volatile(ctrl_ptr);
        }
    } else if m.cap_off != 0 {
        if m.msi_pvm {
            let mask_off = m.cap_off.wrapping_add(if m.msi_64bit { 0x10 } else { 0x0c });
            let cur = pci_read_u32(m.bus, m.dev, m.func, mask_off);
            let next = if mask { cur | 1 } else { cur & !1 };
            pci_write_u32(m.bus, m.dev, m.func, mask_off, next);
            let _ = pci_read_u32(m.bus, m.dev, m.func, mask_off);
        } else {
            let ctrl_off = m.cap_off.wrapping_add(0x02);
            let cur = pci_read_u16(m.bus, m.dev, m.func, ctrl_off);
            let next = if mask {
                cur & !MSI_CTRL_ENABLE
            } else {
                cur | MSI_CTRL_ENABLE
            };
            pci_write_u16(m.bus, m.dev, m.func, ctrl_off, next);
            let _ = pci_read_u16(m.bus, m.dev, m.func, ctrl_off);
        }
    }
}

fn program_msix_entry(m: &MsiInner, vector: u8) {
    let dest = bsp_apic_msi_dest_id() & 0xff;
    let addr_lo = 0xfee0_0000u32 | (dest << 12);
    let base = m.table_entry_va;
    // SAFETY: Valid Ring-0 UC MMIO address inside `PML4[385]`.
    unsafe {
        write_volatile((base + 12) as *mut u32, MSIX_VEC_MASK_BIT);
        write_volatile(base as *mut u32, addr_lo);
        write_volatile((base + 4) as *mut u32, 0);
        write_volatile((base + 8) as *mut u32, vector as u32);
        write_volatile((base + 12) as *mut u32, 0);
        let _ = read_volatile((base + 12) as *const u32);
    }
    let ctrl_off = m.cap_off.wrapping_add(0x02);
    let ctrl = pci_read_u16(m.bus, m.dev, m.func, ctrl_off);
    pci_write_u16(
        m.bus,
        m.dev,
        m.func,
        ctrl_off,
        (ctrl | MSIX_CTRL_ENABLE) & !MSIX_CTRL_FN_MASK,
    );
    let cmd = pci_read_u16(m.bus, m.dev, m.func, PCI_CMD_OFF);
    pci_write_u16(
        m.bus,
        m.dev,
        m.func,
        PCI_CMD_OFF,
        cmd | PCI_CMD_INTX_DISABLE | PCI_CMD_MEM_SPACE | PCI_CMD_BUS_MASTER,
    );
}

fn program_msi_cap(m: &MsiInner, vector: u8) {
    let dest = bsp_apic_msi_dest_id() & 0xff;
    let addr_lo = 0xfee0_0000u32 | (dest << 12);
    pci_write_u32(m.bus, m.dev, m.func, m.cap_off.wrapping_add(0x04), addr_lo);
    if m.msi_64bit {
        pci_write_u32(m.bus, m.dev, m.func, m.cap_off.wrapping_add(0x08), 0);
        pci_write_u16(
            m.bus,
            m.dev,
            m.func,
            m.cap_off.wrapping_add(0x0c),
            vector as u16,
        );
        if m.msi_pvm {
            pci_write_u32(m.bus, m.dev, m.func, m.cap_off.wrapping_add(0x10), 0);
        }
    } else {
        pci_write_u16(
            m.bus,
            m.dev,
            m.func,
            m.cap_off.wrapping_add(0x08),
            vector as u16,
        );
        if m.msi_pvm {
            pci_write_u32(m.bus, m.dev, m.func, m.cap_off.wrapping_add(0x0c), 0);
        }
    }
    let ctrl_off = m.cap_off.wrapping_add(0x02);
    let ctrl = pci_read_u16(m.bus, m.dev, m.func, ctrl_off);
    pci_write_u16(
        m.bus,
        m.dev,
        m.func,
        ctrl_off,
        (ctrl & !MSI_CTRL_MME_MASK) | MSI_CTRL_ENABLE,
    );
    let cmd = pci_read_u16(m.bus, m.dev, m.func, PCI_CMD_OFF);
    pci_write_u16(
        m.bus,
        m.dev,
        m.func,
        PCI_CMD_OFF,
        cmd | PCI_CMD_INTX_DISABLE | PCI_CMD_MEM_SPACE | PCI_CMD_BUS_MASTER,
    );
}

fn unbind_slot_hw(m: &mut MsiInner) -> Option<MsiInner> {
    if !m.bound {
        return None;
    }
    let snap = *m;
    *m = MsiInner::empty();
    Some(snap)
}

fn find_or_alloc_slot_for(dev_idx: usize, entry_idx: u16) -> Option<usize> {
    for (idx, slot) in MSI_SLOTS.iter().enumerate() {
        if slot.allocated.load(Ordering::Acquire) {
            let matched = with_msi_slot_lock(slot, |m| {
                m.bound && m.dev_idx == dev_idx && m.entry_idx == entry_idx
            });
            if matched {
                return Some(idx);
            }
        }
    }
    for (idx, slot) in MSI_SLOTS.iter().enumerate() {
        if slot
            .allocated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some(idx);
        }
    }
    None
}

/// Bind a `CapObjectKind::PciDev` `MSI-X` (preferred) or `MSI` entry (`entry_idx` in `arg2[47:32]`)
/// to a Ring-3 `Notification` capability (`IRQF_ONESHOT` supported via `arg2[0]`).
pub fn bind_pci_msi(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap: CapPtr,
    notif_cap: CapPtr,
    arg2: u64,
) -> Result<u8, SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(SysError::Perm);
    }
    let flags = arg2 & 0xffff_ffff;
    if flags != 0 && flags != IRQF_ONESHOT {
        return Err(SysError::Inval);
    }
    if (arg2 >> 48) != 0 {
        return Err(SysError::Inval);
    }
    let entry_idx = ((arg2 >> 32) & 0xffff) as u16;

    let now_epoch = current_lease_epoch();
    let pci_req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let notif_req = CapRights::SIGNAL;
    let mut sub_err: Option<SysError> = None;

    let res = with_pinned_cap(cspace_id, subj_label, pci_cap, pci_req, now_epoch, |p_res| {
        if p_res.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(p_res.obj_id, p_res.expected_obj_gen)
        {
            return Err(CapError::WrongObjectKind);
        }
        let dev_idx = p_res.obj_id as usize;
        let Some(vdev) = validated_pci_slot(dev_idx) else {
            return Err(CapError::InvalidHandle);
        };
        if vdev.preserve_active_gop {
            sub_err = Some(SysError::Perm);
            return Err(CapError::InsufficientRights);
        }
        if vdev.msix_cap_off == 0 && vdev.msi_cap_off == 0 {
            sub_err = Some(SysError::Inval);
            return Err(CapError::InvalidHandle);
        }
        let use_msix = vdev.msix_cap_off != 0;
        if use_msix {
            if entry_idx >= vdev.msix_table_len
                || (((vdev.msix_table_off & 0xfff) as usize + (entry_idx as usize + 1) * 16)
                    > (crate::vspace::pte::KERNEL_MSIX_MAX_PAGES * 4096))
            {
                sub_err = Some(SysError::Inval);
                return Err(CapError::InvalidHandle);
            }
        } else if entry_idx != 0 {
            sub_err = Some(SysError::Inval);
            return Err(CapError::InvalidHandle);
        }

        let claimed_new = claim_pci_slot_for_irq(dev_idx, cspace_id, &vdev)?;

        let bind_res =
            with_pinned_cap(cspace_id, subj_label, notif_cap, notif_req, now_epoch, |n_res| {
            if n_res.obj_kind != CapObjectKind::Notification {
                return Err(CapError::WrongObjectKind);
            }
            let Some(slot_idx) = find_or_alloc_slot_for(dev_idx, entry_idx) else {
                sub_err = Some(SysError::NoMem);
                return Err(CapError::InvalidHandle);
            };
            let vec = VEC_PCI_MSI_BASE + (slot_idx as u8);
            let sig = if p_res.badge != 0 {
                p_res.badge
            } else if n_res.badge != 0 {
                n_res.badge
            } else {
                1u64 << (entry_idx.min(63) as u32)
            };

            let (msi_64bit, msi_pvm, cap_off, table_va) = if use_msix {
                (
                    false,
                    true,
                    vdev.msix_cap_off,
                    msix_entry_vaddr(dev_idx, vdev.msix_table_off, entry_idx),
                )
            } else {
                let ctrl = pci_read_u16(vdev.bus, vdev.dev, vdev.func, vdev.msi_cap_off.wrapping_add(0x02));
                (
                    (ctrl & MSI_CTRL_64BIT) != 0,
                    (ctrl & MSI_CTRL_PVM) != 0,
                    vdev.msi_cap_off,
                    0u64,
                )
            };

            let hw_cfg = with_msi_slot_lock(&MSI_SLOTS[slot_idx], |m| {
                *m = MsiInner {
                    bound: true,
                    dev_idx,
                    entry_idx,
                    is_msix: use_msix,
                    bus: vdev.bus,
                    dev: vdev.dev,
                    func: vdev.func,
                    cap_off,
                    msi_64bit,
                    msi_pvm,
                    table_entry_va: table_va,
                    oneshot: (flags & IRQF_ONESHOT) != 0,
                    masked_in_hw: false,
                    oneshot_pending_ack: false,
                    notif_obj_id: n_res.obj_id,
                    notif_obj_gen: n_res.expected_obj_gen,
                    notif_global_slot: n_res.global_slot,
                    signal_bits: sig,
                    delivered_count: 0,
                    storm_window_start_tsc: rdtsc(),
                    storm_window_count: 0,
                };
                *m
            });

            if use_msix {
                program_msix_entry(&hw_cfg, vec);
            } else {
                program_msi_cap(&hw_cfg, vec);
            }
            Ok(vec)
        });
        if bind_res.is_err() {
            release_claimed_pci_slot(dev_idx, claimed_new);
        }
        bind_res
    });

    if let Some(se) = sub_err {
        return Err(se);
    }
    res.map_err(map_cap_err_to_sys)
}

/// Acknowledge / unmask / mask / unbind a `PciDev` `MSI` / `MSI-X` entry.
pub fn ack_pci_msi(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap: CapPtr,
    action_and_entry: u64,
) -> Result<u64, SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(SysError::Perm);
    }
    let action = action_and_entry & 0xffff_ffff;
    let entry_idx = ((action_and_entry >> 32) & 0xffff) as u16;
    if action > IRQ_ACK_UNBIND || (action_and_entry >> 48) != 0 {
        return Err(SysError::Inval);
    }

    let now_epoch = current_lease_epoch();
    let req = CapRights(CapRights::WRITE.0 | CapRights::MAP.0);
    let mut op_err: Option<SysError> = None;
    let mut delivered_total = 0u64;

    let res = with_pinned_cap(cspace_id, subj_label, pci_cap, req, now_epoch, |p_res| {
        if p_res.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(p_res.obj_id, p_res.expected_obj_gen)
        {
            return Err(CapError::WrongObjectKind);
        }
        let dev_idx = p_res.obj_id as usize;
        let mut found = false;

        for slot in MSI_SLOTS.iter() {
            if !slot.allocated.load(Ordering::Acquire) {
                continue;
            }
            let mut hw_mask_op: Option<(MsiInner, bool)> = None;
            let mut freed = false;
            with_msi_slot_lock(slot, |m| {
                if !m.bound || m.dev_idx != dev_idx || m.entry_idx != entry_idx {
                    return;
                }
                found = true;
                match action {
                    IRQ_ACK_UNMASK => {
                        if m.oneshot && !m.oneshot_pending_ack {
                            op_err = Some(SysError::Already);
                            return;
                        }
                        m.oneshot_pending_ack = false;
                        m.masked_in_hw = false;
                        delivered_total = m.delivered_count;
                        hw_mask_op = Some((*m, false));
                    }
                    IRQ_ACK_MASK => {
                        m.oneshot_pending_ack = false;
                        m.masked_in_hw = true;
                        delivered_total = m.delivered_count;
                        hw_mask_op = Some((*m, true));
                    }
                    IRQ_ACK_UNBIND => {
                        delivered_total = m.delivered_count;
                        if let Some(snap) = unbind_slot_hw(m) {
                            hw_mask_op = Some((snap, true));
                            freed = true;
                        }
                    }
                    _ => unreachable!(),
                }
            });
            if let Some((snap, mask)) = hw_mask_op {
                hw_mask_msi_entry(&snap, mask);
                if freed {
                    slot.allocated.store(false, Ordering::Release);
                }
            }
            if found {
                break;
            }
        }
        if !found && op_err.is_none() {
            op_err = Some(SysError::Inval);
        }
        Ok(())
    });

    if let Err(ce) = res {
        return Err(map_cap_err_to_sys(ce));
    }
    if let Some(se) = op_err {
        return Err(se);
    }
    if action == IRQ_ACK_UNMASK {
        TOTAL_MSI_ACKS.fetch_add(1, Ordering::Relaxed);
    }
    Ok(delivered_total)
}

/// Top-half PCI `MSI`/`MSI-X` dispatcher for IDT vectors `0x40..=0x5F`.
pub fn on_pci_msi_interrupt(vector: u8) {
    if !(VEC_PCI_MSI_BASE..=VEC_PCI_MSI_LAST).contains(&vector) {
        lapic_eoi();
        return;
    }

    let pcpu = current_percpu();
    pcpu.irq_depth.fetch_add(1, Ordering::Acquire);
    TOTAL_MSI_IRQS.fetch_add(1, Ordering::Relaxed);

    let idx = (vector - VEC_PCI_MSI_BASE) as usize;
    let slot = &MSI_SLOTS[idx];
    let now_tsc = rdtsc();
    let mut hw_mask_target: Option<MsiInner> = None;
    let mut signal_target: Option<(u32, u32, u64)> = None;

    if slot.allocated.load(Ordering::Acquire) {
        with_msi_slot_lock(slot, |m| {
            if !m.bound || (m.oneshot && (m.masked_in_hw || m.oneshot_pending_ack)) {
                m.masked_in_hw = true;
                hw_mask_target = Some(*m);
                TOTAL_MSI_SPURIOUS.fetch_add(1, Ordering::Relaxed);
                return;
            }

            if now_tsc.wrapping_sub(m.storm_window_start_tsc) < STORM_WINDOW_TSC {
                m.storm_window_count = m.storm_window_count.saturating_add(1);
            } else {
                m.storm_window_start_tsc = now_tsc;
                m.storm_window_count = 1;
            }

            if m.oneshot {
                m.masked_in_hw = true;
                hw_mask_target = Some(*m);
            }

            if m.storm_window_count > STORM_BURST_LIMIT {
                m.masked_in_hw = true;
                m.oneshot_pending_ack = false;
                hw_mask_target = Some(*m);
                return;
            }

            if m.oneshot {
                m.oneshot_pending_ack = true;
            }
            m.delivered_count = m.delivered_count.saturating_add(1);
            signal_target = Some((m.notif_obj_id, m.notif_obj_gen, m.signal_bits));
        });
    } else {
        TOTAL_MSI_SPURIOUS.fetch_add(1, Ordering::Relaxed);
    }

    if let Some(ref target) = hw_mask_target {
        hw_mask_msi_entry(target, true);
    }

    // Issue Local APIC EOI strictly once AFTER masking the one-shot vector.
    lapic_eoi();

    if let Some((nid, ngen, bits)) = signal_target {
        let _ = notif_signal_from_irq(nid, ngen, bits);
    }

    pcpu.irq_depth.fetch_sub(1, Ordering::Release);
}

/// Mask and unbind any `MSI`/`MSI-X` slot attached to a revoked/destroyed `Notification`.
pub(super) fn on_notif_revoked_for_msi(notif_id: u32, notif_gen: u32, notif_global_slot: u32) {
    for slot in MSI_SLOTS.iter() {
        if !slot.allocated.load(Ordering::Acquire) {
            continue;
        }
        let mut unbind_snap: Option<MsiInner> = None;
        with_msi_slot_lock(slot, |m| {
            if m.bound
                && m.notif_obj_id == notif_id
                && m.notif_obj_gen == notif_gen
                && (m.notif_global_slot == notif_global_slot || notif_global_slot == CDT_NONE)
            {
                unbind_snap = unbind_slot_hw(m);
            }
        });
        if let Some(snap) = unbind_snap {
            hw_mask_msi_entry(&snap, true);
            slot.allocated.store(false, Ordering::Release);
        }
    }
}

/// Mask all bound `MSI`/`MSI-X` vectors and disable hardware `MSI`/`MSI-X` capability before PCI quiesce.
pub fn quiesce_pci_msi_for_dev(dev_idx: usize) {
    for slot in MSI_SLOTS.iter() {
        if !slot.allocated.load(Ordering::Acquire) {
            continue;
        }
        let mut unbind_snap: Option<MsiInner> = None;
        with_msi_slot_lock(slot, |m| {
            if m.bound && m.dev_idx == dev_idx {
                unbind_snap = unbind_slot_hw(m);
            }
        });
        if let Some(snap) = unbind_snap {
            hw_mask_msi_entry(&snap, true);
            slot.allocated.store(false, Ordering::Release);
        }
    }
    if let Some(vdev) = validated_pci_slot(dev_idx) {
        if vdev.msix_cap_off != 0 {
            let ctrl_off = vdev.msix_cap_off.wrapping_add(0x02);
            let ctrl = pci_read_u16(vdev.bus, vdev.dev, vdev.func, ctrl_off);
            pci_write_u16(
                vdev.bus,
                vdev.dev,
                vdev.func,
                ctrl_off,
                (ctrl & !MSIX_CTRL_ENABLE) | MSIX_CTRL_FN_MASK,
            );
        }
        if vdev.msi_cap_off != 0 {
            let ctrl_off = vdev.msi_cap_off.wrapping_add(0x02);
            let ctrl = pci_read_u16(vdev.bus, vdev.dev, vdev.func, ctrl_off);
            pci_write_u16(vdev.bus, vdev.dev, vdev.func, ctrl_off, ctrl & !MSI_CTRL_ENABLE);
        }
    }
}
