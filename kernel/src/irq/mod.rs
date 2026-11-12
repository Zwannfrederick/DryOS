// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated External Interrupt Subsystem (`IOAPIC` + `CapObjectKind::IrqLine` — `Milestone 2A-1` / `P1`).
//!
//! Architecture & Safety Contracts:
//! 1. **Hardware Discovery (`acpi.rs` -> `irq/mod.rs`):** IOAPIC physical MMIO base and ISA
//!    Interrupt Source Overrides (`GSI`, polarity, trigger mode) come from the validated ACPI MADT,
//!    never hardcoded guesses.
//! 2. **`xAPIC` & `x2APIC` Compatibility:** IOAPIC redirection entries target BSP `lapic_id` in
//!    physical destination mode (`bit 11 = 0`), while `EOI` is issued through `apic::lapic_eoi()`
//!    (MMIO `0xFEE0_00B0` in `xAPIC`, `IA32_X2APIC_EOI` MSR `0x80B` in `x2APIC`).
//! 3. **One-Shot Mask-Until-Ack (`IRQF_ONESHOT`):** `on_external_irq` masks the IOAPIC pin in
//!    hardware *before* `lapic_eoi()`, records `oneshot_pending_ack = true`, and signals a bounded
//!    `Notification` (`notif_signal_from_irq`) with zero blocking and zero nested locks.
//! 4. **Zero Double-EOI / Double-Ack:** Hardware `lapic_eoi()` is exclusively Ring 0; duplicate
//!    `SYS_IRQ_ACK(IRQ_ACK_UNMASK)` calls from Ring 3 deterministically fail with `SysError::Already`.
//! 5. **Revocation & Process Exit Race Safety:** Revoking an `IrqLine` cap, revoking its bound
//!    `Notification` cap, or terminating the driver process immediately masks the IOAPIC pin and
//!    unbinds the line; any in-flight vector arriving on another CPU sees `!bound` and is safely
//!    dropped after masking + `lapic_eoi()`.

pub mod bench;
pub mod ioport;
pub mod tests;

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::acpi::{acpi_madt_summary, isa_irq_route};
use crate::apic::{lapic_eoi, EXT_IRQ_VECTOR_COUNT, VEC_EXT_IRQ_BASE, VEC_EXT_IRQ_LAST};
use crate::arch::{irq_restore, irq_save_cli, rdtsc};
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr, CDT_NONE};
use crate::cspace::{insert_root_cap, with_pinned_cap};
use crate::ipc::map_cap_err_to_sys;
use crate::ipc::notif::notif_signal_from_irq;
use crate::percpu::current_percpu;
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::SysError;

pub const MAX_IRQ_LINES: usize = EXT_IRQ_VECTOR_COUNT;
pub const IRQ_KEYBOARD_PS2: u8 = 1;
pub const IRQ_TEST_SW_LINE: u8 = 15;

pub const IRQF_ONESHOT: u64 = 1 << 0;

pub const IRQ_ACK_UNMASK: u64 = 0;
pub const IRQ_ACK_MASK: u64 = 1;
pub const IRQ_ACK_UNBIND: u64 = 2;

pub const STORM_WINDOW_TSC: u64 = 64_000_000;
pub const STORM_BURST_LIMIT: u32 = 256;

const IOAPIC_IOREGSEL_OFF: usize = 0x00;
const IOAPIC_IOWIN_OFF: usize = 0x10;
const IOAPIC_REG_ID: u32 = 0x00;
const IOAPIC_REG_VER: u32 = 0x01;
const IOAPIC_REG_REDTBL_BASE: u32 = 0x10;

const IOAPIC_REDTBL_MASKED: u32 = 1 << 16;
const IOAPIC_REDTBL_LEVEL: u32 = 1 << 15;
const IOAPIC_REDTBL_ACTIVE_LOW: u32 = 1 << 13;

const MAX_SHADOW_REDIR_PINS: usize = 64;

struct IoApicHwCache {
    redir_lo: [u32; MAX_SHADOW_REDIR_PINS],
    last_reg_sel: u32,
}

impl IoApicHwCache {
    const fn new() -> Self {
        Self {
            redir_lo: [IOAPIC_REDTBL_MASKED; MAX_SHADOW_REDIR_PINS],
            last_reg_sel: u32::MAX,
        }
    }
}

struct IoApicController {
    present: AtomicBool,
    virt_base: AtomicU64,
    gsi_base: AtomicU32,
    max_redir: AtomicU8,
    dest_lapic_id: AtomicU8,
    lock: AtomicBool,
    cache: UnsafeCell<IoApicHwCache>,
}

// SAFETY: `cache` is mutated and read strictly while holding `IOAPIC.lock` (`irq_save_cli()`).
unsafe impl Sync for IoApicController {}

impl IoApicController {
    const fn new() -> Self {
        Self {
            present: AtomicBool::new(false),
            virt_base: AtomicU64::new(0),
            gsi_base: AtomicU32::new(0),
            max_redir: AtomicU8::new(0),
            dest_lapic_id: AtomicU8::new(0),
            lock: AtomicBool::new(false),
            cache: UnsafeCell::new(IoApicHwCache::new()),
        }
    }
}

static IOAPIC: IoApicController = IoApicController::new();

#[inline]
unsafe fn ioapic_read_reg_unlocked(virt_base: u64, reg: u32) -> u32 {
    let sel = (virt_base as usize + IOAPIC_IOREGSEL_OFF) as *mut u32;
    let win = (virt_base as usize + IOAPIC_IOWIN_OFF) as *const u32;
    // SAFETY: `virt_base` is supervisor-only `KERNEL_MMIO_UC` mapped in `KERNEL_VSPACE` HHDM.
    unsafe {
        write_volatile(sel, reg);
        read_volatile(win)
    }
}

#[inline]
unsafe fn ioapic_write_reg_cached(virt_base: u64, cache: &mut IoApicHwCache, reg: u32, val: u32) {
    let sel = (virt_base as usize + IOAPIC_IOREGSEL_OFF) as *mut u32;
    let win = (virt_base as usize + IOAPIC_IOWIN_OFF) as *mut u32;
    // SAFETY: `virt_base` is supervisor-only `KERNEL_MMIO_UC`; `IOREGSEL` latches across `IOWIN` writes.
    unsafe {
        if cache.last_reg_sel != reg {
            write_volatile(sel, reg);
            cache.last_reg_sel = reg;
        }
        write_volatile(win, val);
    }
}

fn with_ioapic_hw_lock<R>(
    f: impl FnOnce(u64, u32, u8, u8, &mut IoApicHwCache) -> R,
) -> Option<R> {
    if !IOAPIC.present.load(Ordering::Acquire) {
        return None;
    }
    let saved_if = irq_save_cli();
    while IOAPIC
        .lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        core::hint::spin_loop();
    }
    let virt = IOAPIC.virt_base.load(Ordering::Relaxed);
    let gsi_base = IOAPIC.gsi_base.load(Ordering::Relaxed);
    let max_redir = IOAPIC.max_redir.load(Ordering::Relaxed);
    let dest = IOAPIC.dest_lapic_id.load(Ordering::Relaxed);
    // SAFETY: Exclusive access under `IOAPIC.lock`.
    let out = f(virt, gsi_base, max_redir, dest, unsafe { &mut *IOAPIC.cache.get() });
    IOAPIC.lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    Some(out)
}

fn ioapic_program_route(gsi: u32, vector: u8, active_low: bool, level: bool, masked: bool) -> bool {
    with_ioapic_hw_lock(|virt, gsi_base, max_redir, dest, cache| {
        if gsi < gsi_base {
            return false;
        }
        let pin = gsi - gsi_base;
        if pin > (max_redir as u32) || (pin as usize) >= MAX_SHADOW_REDIR_PINS {
            return false;
        }
        let mut lo = vector as u32;
        if active_low {
            lo |= IOAPIC_REDTBL_ACTIVE_LOW;
        }
        if level {
            lo |= IOAPIC_REDTBL_LEVEL;
        }
        if masked {
            lo |= IOAPIC_REDTBL_MASKED;
        }
        let hi = (dest as u32) << 24;
        let reg_lo = IOAPIC_REG_REDTBL_BASE + pin * 2;
        let reg_hi = reg_lo + 1;
        let masked_prev = cache.redir_lo[pin as usize] | IOAPIC_REDTBL_MASKED;
        // SAFETY: serialized by `IOAPIC.lock`; always mask pin first before updating `hi`/`lo`.
        unsafe {
            ioapic_write_reg_cached(virt, cache, reg_lo, masked_prev);
            ioapic_write_reg_cached(virt, cache, reg_hi, hi);
            ioapic_write_reg_cached(virt, cache, reg_lo, lo);
        }
        cache.redir_lo[pin as usize] = lo;
        true
    })
    .unwrap_or(false)
}

fn ioapic_set_pin_masked(gsi: u32, masked: bool) {
    let _ = with_ioapic_hw_lock(|virt, gsi_base, max_redir, _, cache| {
        if gsi < gsi_base {
            return;
        }
        let pin = gsi - gsi_base;
        if pin > (max_redir as u32) || (pin as usize) >= MAX_SHADOW_REDIR_PINS {
            return;
        }
        let cur = cache.redir_lo[pin as usize];
        let next = if masked {
            cur | IOAPIC_REDTBL_MASKED
        } else {
            cur & !IOAPIC_REDTBL_MASKED
        };
        if next == cur {
            return;
        }
        let reg_lo = IOAPIC_REG_REDTBL_BASE + pin * 2;
        // SAFETY: serialized by `IOAPIC.lock`.
        unsafe {
            ioapic_write_reg_cached(virt, cache, reg_lo, next);
        }
        cache.redir_lo[pin as usize] = next;
    });
}

#[derive(Clone, Copy)]
struct IrqLineInner {
    bound: bool,
    oneshot: bool,
    masked_in_hw: bool,
    oneshot_pending_ack: bool,
    active_low: bool,
    level_triggered: bool,
    gsi: u32,
    bound_irq_global_slot: u32,
    notif_obj_id: u32,
    notif_obj_gen: u32,
    notif_global_slot: u32,
    signal_bits: u64,
    storm_window_start_tsc: u64,
    storm_window_count: u32,
    storm_throttled_count: u64,
    delivered_count: u64,
}

impl IrqLineInner {
    const fn empty(default_gsi: u32) -> Self {
        Self {
            bound: false,
            oneshot: true,
            masked_in_hw: true,
            oneshot_pending_ack: false,
            active_low: false,
            level_triggered: false,
            gsi: default_gsi,
            bound_irq_global_slot: CDT_NONE,
            notif_obj_id: 0,
            notif_obj_gen: 0,
            notif_global_slot: CDT_NONE,
            signal_bits: 0,
            storm_window_start_tsc: 0,
            storm_window_count: 0,
            storm_throttled_count: 0,
            delivered_count: 0,
        }
    }
}

struct IrqLineSlot {
    allocated: AtomicBool,
    generation: AtomicU32,
    lock: AtomicBool,
    inner: UnsafeCell<IrqLineInner>,
}

// SAFETY: `inner` is accessed exclusively under IRQ-masked leaf `lock`.
unsafe impl Sync for IrqLineSlot {}

impl IrqLineSlot {
    const fn new(idx: u32) -> Self {
        Self {
            allocated: AtomicBool::new(false),
            generation: AtomicU32::new(1),
            lock: AtomicBool::new(false),
            inner: UnsafeCell::new(IrqLineInner::empty(idx)),
        }
    }
}

static IRQ_LINES: [IrqLineSlot; MAX_IRQ_LINES] = [
    IrqLineSlot::new(0),
    IrqLineSlot::new(1),
    IrqLineSlot::new(2),
    IrqLineSlot::new(3),
    IrqLineSlot::new(4),
    IrqLineSlot::new(5),
    IrqLineSlot::new(6),
    IrqLineSlot::new(7),
    IrqLineSlot::new(8),
    IrqLineSlot::new(9),
    IrqLineSlot::new(10),
    IrqLineSlot::new(11),
    IrqLineSlot::new(12),
    IrqLineSlot::new(13),
    IrqLineSlot::new(14),
    IrqLineSlot::new(15),
];

static TOTAL_EXT_IRQS: AtomicU64 = AtomicU64::new(0);
static SPURIOUS_OR_MASKED_IRQS: AtomicU64 = AtomicU64::new(0);
static TOTAL_STORM_THROTTLES: AtomicU64 = AtomicU64::new(0);
static TOTAL_IRQ_ACKS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn with_irq_slot_lock<R>(slot: &IrqLineSlot, f: impl FnOnce(&mut IrqLineInner) -> R) -> R {
    let saved_if = irq_save_cli();
    while slot
        .lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        core::hint::spin_loop();
    }
    // SAFETY: Protected by `slot.lock` with local maskable interrupts disabled.
    let res = f(unsafe { &mut *slot.inner.get() });
    slot.lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    res
}

/// Initialize the primary IOAPIC from the ACPI MADT summary (`KERNEL_VSPACE` must already be active)
/// and mask all redirection entries cleanly before any Ring 3 driver binds an IRQ capability.
pub fn init_ioapic_and_irq_subsystem(hhdm_offset: u64, bsp_lapic_id: u32) -> bool {
    let madt = acpi_madt_summary();
    if !madt.ioapic_found || madt.ioapic_phys == 0 {
        serial_println!("[DRY-IOAPIC] INFO: no ACPI MADT IOAPIC present; external pins disabled");
        return false;
    }
    assert!(
        bsp_lapic_id <= 0xff,
        "BSP LAPIC ID {:#x} exceeds 8-bit IOAPIC physical destination field",
        bsp_lapic_id
    );

    let virt = hhdm_offset + madt.ioapic_phys;
    // SAFETY: `madt.ioapic_phys` was explicitly mapped `KERNEL_MMIO_UC` in `init_and_activate_kernel_vspace`.
    let (hw_id, ver_raw) = unsafe {
        let id_reg = ioapic_read_reg_unlocked(virt, IOAPIC_REG_ID);
        let ver_reg = ioapic_read_reg_unlocked(virt, IOAPIC_REG_VER);
        (((id_reg >> 24) & 0x0f) as u8, ver_reg)
    };
    let ver = (ver_raw & 0xff) as u8;
    let max_redir = ((ver_raw >> 16) & 0xff) as u8;

    IOAPIC.virt_base.store(virt, Ordering::Relaxed);
    IOAPIC.gsi_base.store(madt.ioapic_gsi_base, Ordering::Relaxed);
    IOAPIC.max_redir.store(max_redir, Ordering::Relaxed);
    IOAPIC
        .dest_lapic_id
        .store(bsp_lapic_id as u8, Ordering::Relaxed);
    IOAPIC.present.store(true, Ordering::Release);

    // Mask every IOAPIC redirection pin before enabling user IRQ bindings.
    let _ = with_ioapic_hw_lock(|v, _, max_r, dest, cache| {
        cache.last_reg_sel = u32::MAX;
        for pin in 0..=(max_r as u32) {
            let vec = VEC_EXT_IRQ_BASE + (pin.min((MAX_IRQ_LINES - 1) as u32) as u8);
            let reg_lo = IOAPIC_REG_REDTBL_BASE + pin * 2;
            let reg_hi = reg_lo + 1;
            let lo = (vec as u32) | IOAPIC_REDTBL_MASKED;
            // SAFETY: serialized under `IOAPIC.lock`.
            unsafe {
                ioapic_write_reg_cached(v, cache, reg_hi, (dest as u32) << 24);
                ioapic_write_reg_cached(v, cache, reg_lo, lo);
            }
            if (pin as usize) < MAX_SHADOW_REDIR_PINS {
                cache.redir_lo[pin as usize] = lo;
            }
        }
    });

    for line in 0..(MAX_IRQ_LINES as u8) {
        let route = isa_irq_route(line);
        with_irq_slot_lock(&IRQ_LINES[line as usize], |inner| {
            *inner = IrqLineInner::empty(route.gsi);
            inner.active_low = route.active_low;
            inner.level_triggered = route.level_triggered;
        });
    }

    let r1 = isa_irq_route(IRQ_KEYBOARD_PS2);
    serial_println!(
        "[DRY-IOAPIC] PASS: acpi_id={} hw_id={} ver={:#x} phys={:#x} gsi_base={} max_redir={} dest_lapic={} irq1_gsi={} irq1_vec={:#x}",
        madt.ioapic_id,
        hw_id,
        ver,
        madt.ioapic_phys,
        madt.ioapic_gsi_base,
        max_redir,
        bsp_lapic_id,
        r1.gsi,
        VEC_EXT_IRQ_BASE + IRQ_KEYBOARD_PS2
    );
    true
}

#[inline]
pub fn ioapic_present() -> bool {
    IOAPIC.present.load(Ordering::Acquire)
}

#[inline]
pub fn irq_telemetry() -> (u64, u64, u64, u64) {
    (
        TOTAL_EXT_IRQS.load(Ordering::Relaxed),
        TOTAL_IRQ_ACKS.load(Ordering::Relaxed),
        SPURIOUS_OR_MASKED_IRQS.load(Ordering::Relaxed),
        TOTAL_STORM_THROTTLES.load(Ordering::Relaxed),
    )
}

#[inline]
pub fn is_irq_line_live(obj_id: u32, expected_gen: u32) -> bool {
    let idx = obj_id as usize;
    if idx >= MAX_IRQ_LINES || expected_gen == 0 {
        return false;
    }
    let slot = &IRQ_LINES[idx];
    slot.allocated.load(Ordering::Acquire) && slot.generation.load(Ordering::Acquire) == expected_gen
}

/// Mint a root `CapObjectKind::IrqLine` capability for ISA `irq_line` (`0..16`) into `cspace_id`.
pub fn mint_root_irq_cap(
    cspace_id: CSpaceId,
    owner_label: SecLabel,
    irq_line: u8,
    signal_badge: u64,
) -> Result<CapPtr, CapError> {
    if owner_label.domain != SecDomain::RootSystem
        || owner_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    let idx = irq_line as usize;
    if idx >= MAX_IRQ_LINES {
        return Err(CapError::InvalidHandle);
    }
    let slot = &IRQ_LINES[idx];
    if slot
        .allocated
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(CapError::BusyActiveObject);
    }

    let gen = slot.generation.load(Ordering::Acquire);
    let route = isa_irq_route(irq_line);
    with_irq_slot_lock(slot, |inner| {
        *inner = IrqLineInner::empty(route.gsi);
        inner.active_low = route.active_low;
        inner.level_triggered = route.level_triggered;
    });

    let obj_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NONE,
        owner_label.compartment_id,
        owner_label.policy_tag,
    );
    let badge = if signal_badge != 0 {
        signal_badge
    } else {
        1u64 << irq_line
    };

    match insert_root_cap(
        cspace_id,
        CapObjectKind::IrqLine,
        irq_line as u32,
        gen,
        CapRights::IRQ_ROOT,
        obj_label,
        0,
        badge,
    ) {
        Ok(cap) => Ok(cap),
        Err(e) => {
            destroy_irq_line(irq_line as u32, gen);
            Err(e)
        }
    }
}

fn unbind_and_mask_line_locked(inner: &mut IrqLineInner) -> u32 {
    inner.bound = false;
    inner.masked_in_hw = true;
    inner.oneshot_pending_ack = false;
    inner.bound_irq_global_slot = CDT_NONE;
    inner.notif_obj_id = 0;
    inner.notif_obj_gen = 0;
    inner.notif_global_slot = CDT_NONE;
    inner.signal_bits = 0;
    inner.gsi
}

pub fn destroy_irq_line(obj_id: u32, expected_gen: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_IRQ_LINES {
        return;
    }
    let slot = &IRQ_LINES[idx];
    let mut mask_gsi = None;
    with_irq_slot_lock(slot, |inner| {
        if slot.allocated.load(Ordering::Acquire)
            && slot.generation.load(Ordering::Acquire) == expected_gen
        {
            mask_gsi = Some(unbind_and_mask_line_locked(inner));
            let next_gen = expected_gen.wrapping_add(1).max(1);
            slot.generation.store(next_gen, Ordering::Release);
            slot.allocated.store(false, Ordering::Release);
        }
    });
    if let Some(gsi) = mask_gsi {
        ioapic_set_pin_masked(gsi, true);
    }
}

pub fn on_irq_cap_revoked(obj_id: u32, expected_obj_gen: u32, revoked_global_slot: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_IRQ_LINES {
        return;
    }
    let slot = &IRQ_LINES[idx];
    if !is_irq_line_live(obj_id, expected_obj_gen) {
        return;
    }
    let mut mask_gsi = None;
    with_irq_slot_lock(slot, |inner| {
        if inner.bound
            && (inner.bound_irq_global_slot == revoked_global_slot
                || inner.bound_irq_global_slot == CDT_NONE)
        {
            mask_gsi = Some(unbind_and_mask_line_locked(inner));
        }
    });
    if let Some(gsi) = mask_gsi {
        ioapic_set_pin_masked(gsi, true);
    }
}

pub fn on_notif_cap_revoked_for_irq(notif_id: u32, notif_gen: u32, notif_global_slot: u32) {
    for slot in &IRQ_LINES {
        if !slot.allocated.load(Ordering::Acquire) {
            continue;
        }
        let mut mask_gsi = None;
        with_irq_slot_lock(slot, |inner| {
            if inner.bound
                && inner.notif_obj_id == notif_id
                && inner.notif_obj_gen == notif_gen
                && (inner.notif_global_slot == notif_global_slot || notif_global_slot == CDT_NONE)
            {
                mask_gsi = Some(unbind_and_mask_line_locked(inner));
            }
        });
        if let Some(gsi) = mask_gsi {
            ioapic_set_pin_masked(gsi, true);
        }
    }
}

/// Bind an `IrqLine` capability to a `Notification` capability in one-shot mode and unmask the pin.
pub fn sys_irq_bind(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    irq_cap: CapPtr,
    notif_cap: CapPtr,
    flags: u64,
) -> Result<u8, SysError> {
    if subj_label.domain != SecDomain::RootSystem {
        return Err(SysError::Perm);
    }
    if flags != 0 && flags != IRQF_ONESHOT {
        return Err(SysError::Inval);
    }

    let now_epoch = current_lease_epoch();
    let irq_req = CapRights(CapRights::READ.0 | CapRights::WAIT.0 | CapRights::SIGNAL.0);
    let notif_req = CapRights::SIGNAL;

    let bind_res = with_pinned_cap(cspace_id, subj_label, irq_cap, irq_req, now_epoch, |irq_res| {
        if irq_res.obj_kind != CapObjectKind::IrqLine {
            return Err(CapError::WrongObjectKind);
        }
        with_pinned_cap(cspace_id, subj_label, notif_cap, notif_req, now_epoch, |ntf_res| {
            if ntf_res.obj_kind != CapObjectKind::Notification {
                return Err(CapError::WrongObjectKind);
            }
            let irq_line = irq_res.obj_id as u8;
            let vec = VEC_EXT_IRQ_BASE + irq_line;
            let slot = &IRQ_LINES[irq_line as usize];
            let sig = if irq_res.badge != 0 {
                irq_res.badge
            } else if ntf_res.badge != 0 {
                ntf_res.badge
            } else {
                1u64 << irq_line
            };

            let (gsi, act_low, lvl) = with_irq_slot_lock(slot, |inner| {
                inner.bound = true;
                inner.oneshot = true;
                inner.masked_in_hw = false;
                inner.oneshot_pending_ack = false;
                inner.bound_irq_global_slot = irq_res.global_slot;
                inner.notif_obj_id = ntf_res.obj_id;
                inner.notif_obj_gen = ntf_res.expected_obj_gen;
                inner.notif_global_slot = ntf_res.global_slot;
                inner.signal_bits = sig;
                inner.storm_window_start_tsc = rdtsc();
                inner.storm_window_count = 0;
                (inner.gsi, inner.active_low, inner.level_triggered)
            });

            let _ = ioapic_program_route(gsi, vec, act_low, lvl, false);
            Ok(vec)
        })
    });

    bind_res.map_err(map_cap_err_to_sys)
}

/// Acknowledge (`IRQ_ACK_UNMASK = 0`), mask (`IRQ_ACK_MASK = 1`), or unbind (`IRQ_ACK_UNBIND = 2`)
/// a capability-owned external interrupt line.
pub fn sys_irq_ack(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    irq_cap: CapPtr,
    action: u64,
) -> Result<u64, SysError> {
    if subj_label.domain != SecDomain::RootSystem {
        return Err(SysError::Perm);
    }
    if action > IRQ_ACK_UNBIND {
        return Err(SysError::Inval);
    }

    let now_epoch = current_lease_epoch();
    let mut op_err: Option<SysError> = None;
    let mut delivered_total = 0u64;

    let cap_res = with_pinned_cap(
        cspace_id,
        subj_label,
        irq_cap,
        CapRights::SIGNAL,
        now_epoch,
        |irq_res| {
            if irq_res.obj_kind != CapObjectKind::IrqLine {
                return Err(CapError::WrongObjectKind);
            }
            let slot = &IRQ_LINES[irq_res.obj_id as usize];
            let mut hw_mask_op: Option<(u32, bool)> = None;

            with_irq_slot_lock(slot, |inner| {
                match action {
                    IRQ_ACK_UNMASK => {
                        if !inner.bound {
                            op_err = Some(SysError::Inval);
                            return;
                        }
                        if !inner.oneshot_pending_ack {
                            op_err = Some(SysError::Already);
                            return;
                        }
                        inner.oneshot_pending_ack = false;
                        inner.masked_in_hw = false;
                        delivered_total = inner.delivered_count;
                        hw_mask_op = Some((inner.gsi, false));
                    }
                    IRQ_ACK_MASK => {
                        if !inner.bound {
                            op_err = Some(SysError::Inval);
                            return;
                        }
                        inner.oneshot_pending_ack = false;
                        inner.masked_in_hw = true;
                        delivered_total = inner.delivered_count;
                        hw_mask_op = Some((inner.gsi, true));
                    }
                    IRQ_ACK_UNBIND => {
                        delivered_total = inner.delivered_count;
                        let gsi = unbind_and_mask_line_locked(inner);
                        hw_mask_op = Some((gsi, true));
                    }
                    _ => unreachable!(),
                }
            });

            if let Some((gsi, mask)) = hw_mask_op {
                ioapic_set_pin_masked(gsi, mask);
            }
            Ok(())
        },
    );

    if let Err(ce) = cap_res {
        return Err(map_cap_err_to_sys(ce));
    }
    if let Some(se) = op_err {
        return Err(se);
    }
    if action == IRQ_ACK_UNMASK {
        TOTAL_IRQ_ACKS.fetch_add(1, Ordering::Relaxed);
    }
    Ok(delivered_total)
}

/// Top-half external hardware interrupt dispatcher invoked from `idt::rust_exception_dispatch`
/// for IDT vectors `0x30..=0x3F` (`RFLAGS.IF == 0`, `RFLAGS.AC == 0`, `RFLAGS.DF == 0`).
pub fn on_external_irq(vector: u8) {
    if !(VEC_EXT_IRQ_BASE..=VEC_EXT_IRQ_LAST).contains(&vector) {
        lapic_eoi();
        return;
    }

    let pcpu = current_percpu();
    pcpu.irq_depth.fetch_add(1, Ordering::Acquire);
    TOTAL_EXT_IRQS.fetch_add(1, Ordering::Relaxed);

    let line = (vector - VEC_EXT_IRQ_BASE) as usize;
    let slot = &IRQ_LINES[line];
    let now_tsc = rdtsc();

    let mut hw_mask_gsi: Option<u32> = None;
    let mut signal_target: Option<(u32, u32, u64)> = None;

    if slot.allocated.load(Ordering::Acquire) {
        with_irq_slot_lock(slot, |inner| {
            if !inner.bound || inner.masked_in_hw || inner.oneshot_pending_ack {
                inner.masked_in_hw = true;
                hw_mask_gsi = Some(inner.gsi);
                SPURIOUS_OR_MASKED_IRQS.fetch_add(1, Ordering::Relaxed);
                return;
            }

            if now_tsc.wrapping_sub(inner.storm_window_start_tsc) < STORM_WINDOW_TSC {
                inner.storm_window_count = inner.storm_window_count.saturating_add(1);
            } else {
                inner.storm_window_start_tsc = now_tsc;
                inner.storm_window_count = 1;
            }

            // One-shot contract: mask the pin in IOAPIC immediately until Ring 3 calls `SYS_IRQ_ACK`.
            inner.masked_in_hw = true;
            hw_mask_gsi = Some(inner.gsi);

            if inner.storm_window_count > STORM_BURST_LIMIT {
                inner.storm_throttled_count = inner.storm_throttled_count.saturating_add(1);
                inner.oneshot_pending_ack = false;
                TOTAL_STORM_THROTTLES.fetch_add(1, Ordering::Relaxed);
                return;
            }

            inner.oneshot_pending_ack = true;
            inner.delivered_count = inner.delivered_count.saturating_add(1);
            signal_target = Some((inner.notif_obj_id, inner.notif_obj_gen, inner.signal_bits));
        });
    } else {
        let r = isa_irq_route(line as u8);
        hw_mask_gsi = Some(r.gsi);
        SPURIOUS_OR_MASKED_IRQS.fetch_add(1, Ordering::Relaxed);
    }

    if let Some(gsi) = hw_mask_gsi {
        ioapic_set_pin_masked(gsi, true);
    }

    // Issue Local APIC EOI strictly once after masking the IOAPIC pin.
    lapic_eoi();

    // Signal bound Ring 3 `Notification` outside `IrqLineSlot::lock` (zero nested locks, non-blocking).
    if let Some((nid, ngen, bits)) = signal_target {
        let _ = notif_signal_from_irq(nid, ngen, bits);
    }

    pcpu.irq_depth.fetch_sub(1, Ordering::Release);
}
