// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Narrow Capability-Gated x86 Port I/O (`CapObjectKind::IoPort` — `Milestone 2A-1` / `P1`).
//!
//! Security Invariants:
//! - **Zero `IOPL=3`:** User `RFLAGS` always maintains `IOPL == 0` (`USER_RFLAGS_ALLOWED_MASK`).
//! - **Discrete Port Allowlist:** Each `IoPort` capability authorizes at most 4 discrete 16-bit I/O
//!   ports (e.g., `[0x0060, 0x0064]` for PS/2 i8042 data + status/command, strictly excluding
//!   adjacent PIT/speaker/NMI ports `0x0061..=0x0063`).
//! - **Hard Kernel Denylist:** Critical platform ports (`0x00F4` QEMU `isa-debug-exit`, legacy 8259
//!   PIC `0x0020..=0x0021` / `0x00A0..=0x00A1`, PIT/NMI `0x0040..=0x0043` / `0x0061..=0x0063` /
//!   `0x0070..=0x0071`, COM1 `0x03F8..=0x03FF`, PCI config `0x0CF8..=0x0CFF`) can never be minted
//!   or accessed through `IoPort` capabilities.
//! - **Domain Isolation:** `SecDomain::RootSystem` driver compartments only; `SecDomain::Userspace`
//!   callers are deterministically rejected (`SysError::Perm`).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arch::{inb, irq_restore, irq_save_cli, outb};
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::{insert_root_cap, with_pinned_cap};
use crate::ipc::map_cap_err_to_sys;
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::uaccess::SysError;

pub const MAX_IOPORT_CAPS: usize = 16;
pub const MAX_DISCRETE_PORTS_PER_CAP: usize = 4;

pub const IOPORT_OP_INB: u64 = 0;
pub const IOPORT_OP_OUTB: u64 = 1;

pub const I8042_DATA_PORT: u16 = 0x0060;
pub const I8042_STATUS_CMD_PORT: u16 = 0x0064;

#[derive(Clone, Copy)]
struct IoPortInner {
    ports: [u16; MAX_DISCRETE_PORTS_PER_CAP],
    count: u8,
}

impl IoPortInner {
    const fn empty() -> Self {
        Self {
            ports: [0; MAX_DISCRETE_PORTS_PER_CAP],
            count: 0,
        }
    }

    #[inline]
    fn allows(&self, port: u16) -> bool {
        let n = self.count as usize;
        let mut i = 0;
        while i < n {
            if self.ports[i] == port {
                return true;
            }
            i += 1;
        }
        false
    }
}

struct IoPortSlot {
    allocated: AtomicBool,
    generation: AtomicU32,
    lock: AtomicBool,
    inner: UnsafeCell<IoPortInner>,
}

// SAFETY: `inner` is mutated only under IRQ-masked `lock` and read under `with_pinned_cap` while
// `allocated` + `generation` match (`ABA`-safe).
unsafe impl Sync for IoPortSlot {}

impl IoPortSlot {
    const fn new() -> Self {
        Self {
            allocated: AtomicBool::new(false),
            generation: AtomicU32::new(1),
            lock: AtomicBool::new(false),
            inner: UnsafeCell::new(IoPortInner::empty()),
        }
    }
}

static IOPORT_TABLE: [IoPortSlot; MAX_IOPORT_CAPS] = [const { IoPortSlot::new() }; MAX_IOPORT_CAPS];
static TOTAL_IOPORT_OPS: AtomicU64 = AtomicU64::new(0);
static DENIED_IOPORT_OPS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn with_ioport_lock<R>(slot: &IoPortSlot, f: impl FnOnce(&mut IoPortInner) -> R) -> R {
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
    // SAFETY: Exclusive access under `slot.lock` with local IRQs masked.
    let res = f(unsafe { &mut *slot.inner.get() });
    slot.lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    res
}

/// Hard kernel denylist of sensitive platform I/O ports that Ring 3 may never access.
#[inline]
pub const fn is_port_kernel_forbidden(port: u16) -> bool {
    matches!(
        port,
        0x0020..=0x0021 // Legacy 8259 Master PIC
        | 0x0040..=0x0043 // PIT 8254 timer
        | 0x0061..=0x0063 // Speaker / NMI control (adjacent to i8042 0x60/0x64!)
        | 0x0070..=0x0071 // CMOS / RTC / NMI mask
        | 0x0080          // DMA page / POST
        | 0x0092          // Fast A20 / reset
        | 0x00A0..=0x00A1 // Legacy 8259 Slave PIC
        | 0x00F4          // QEMU isa-debug-exit device
        | 0x03F8..=0x03FF // COM1 kernel serial console
        | 0x0CF8..=0x0CFF // PCI configuration mechanism #1
    )
}

#[inline]
pub fn is_ioport_cap_live(obj_id: u32, expected_gen: u32) -> bool {
    let idx = obj_id as usize;
    if idx >= MAX_IOPORT_CAPS || expected_gen == 0 {
        return false;
    }
    let slot = &IOPORT_TABLE[idx];
    slot.allocated.load(Ordering::Acquire) && slot.generation.load(Ordering::Acquire) == expected_gen
}

/// Mint a narrow `CapObjectKind::IoPort` root capability in `cspace_id` (`SecDomain::RootSystem`).
pub fn mint_root_ioport_cap(
    cspace_id: CSpaceId,
    owner_label: SecLabel,
    ports: &[u16],
) -> Result<CapPtr, CapError> {
    if owner_label.domain != SecDomain::RootSystem
        || owner_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    if ports.is_empty() || ports.len() > MAX_DISCRETE_PORTS_PER_CAP {
        return Err(CapError::InvalidHandle);
    }
    for &p in ports {
        if is_port_kernel_forbidden(p) {
            return Err(CapError::RightsAmplification);
        }
    }

    let mut claimed: Option<(usize, u32)> = None;
    for (idx, slot) in IOPORT_TABLE.iter().enumerate() {
        if slot
            .allocated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let gen = slot.generation.load(Ordering::Acquire);
            with_ioport_lock(slot, |inner| {
                inner.ports = [0; MAX_DISCRETE_PORTS_PER_CAP];
                for (i, &p) in ports.iter().enumerate() {
                    inner.ports[i] = p;
                }
                inner.count = ports.len() as u8;
            });
            claimed = Some((idx, gen));
            break;
        }
    }

    let (idx, gen) = claimed.ok_or(CapError::OutOfSlots)?;
    let obj_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NONE,
        owner_label.compartment_id,
        owner_label.policy_tag,
    );

    match insert_root_cap(
        cspace_id,
        CapObjectKind::IoPort,
        idx as u32,
        gen,
        CapRights::IOPORT_ROOT,
        obj_label,
        0,
        0,
    ) {
        Ok(cap) => Ok(cap),
        Err(e) => {
            destroy_ioport_cap(idx as u32, gen);
            Err(e)
        }
    }
}

pub fn destroy_ioport_cap(obj_id: u32, expected_gen: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_IOPORT_CAPS {
        return;
    }
    let slot = &IOPORT_TABLE[idx];
    with_ioport_lock(slot, |inner| {
        if slot.allocated.load(Ordering::Acquire)
            && slot.generation.load(Ordering::Acquire) == expected_gen
        {
            *inner = IoPortInner::empty();
            let next_gen = expected_gen.wrapping_add(1).max(1);
            slot.generation.store(next_gen, Ordering::Release);
            slot.allocated.store(false, Ordering::Release);
        }
    });
}

/// Execute a narrow capability-checked 8-bit port read (`IOPORT_OP_INB = 0`) or write (`IOPORT_OP_OUTB = 1`).
pub fn sys_ioport_rw(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    cap_ptr: CapPtr,
    op: u64,
    port_raw: u64,
    val_raw: u64,
) -> Result<u8, SysError> {
    if subj_label.domain != SecDomain::RootSystem {
        DENIED_IOPORT_OPS.fetch_add(1, Ordering::Relaxed);
        return Err(SysError::Perm);
    }
    if port_raw > (u16::MAX as u64) {
        return Err(SysError::Inval);
    }
    let port = port_raw as u16;
    if is_port_kernel_forbidden(port) {
        DENIED_IOPORT_OPS.fetch_add(1, Ordering::Relaxed);
        return Err(SysError::Perm);
    }
    let req_rights = match op {
        IOPORT_OP_INB => CapRights::READ,
        IOPORT_OP_OUTB => {
            if val_raw > (u8::MAX as u64) {
                return Err(SysError::Inval);
            }
            CapRights::WRITE
        }
        _ => return Err(SysError::Inval),
    };

    let now_epoch = current_lease_epoch();
    let res = with_pinned_cap(cspace_id, subj_label, cap_ptr, req_rights, now_epoch, |resolved| {
        if resolved.obj_kind != CapObjectKind::IoPort {
            return Err(CapError::WrongObjectKind);
        }
        let slot = &IOPORT_TABLE[resolved.obj_id as usize];
        let allowed = with_ioport_lock(slot, |inner| inner.allows(port));
        if !allowed {
            return Err(CapError::InsufficientRights);
        }
        let out = if op == IOPORT_OP_INB {
            // SAFETY: Port is validated against `slot.inner` discrete allowlist and kernel denylist.
            unsafe { inb(port) }
        } else {
            // SAFETY: Port is validated against `slot.inner` discrete allowlist and kernel denylist.
            unsafe { outb(port, val_raw as u8) };
            0
        };
        TOTAL_IOPORT_OPS.fetch_add(1, Ordering::Relaxed);
        Ok(out)
    });

    match res {
        Ok(b) => Ok(b),
        Err(e) => {
            DENIED_IOPORT_OPS.fetch_add(1, Ordering::Relaxed);
            Err(map_cap_err_to_sys(e))
        }
    }
}

#[inline]
pub fn ioport_telemetry() -> (u64, u64) {
    (
        TOTAL_IOPORT_OPS.load(Ordering::Relaxed),
        DENIED_IOPORT_OPS.load(Ordering::Relaxed),
    )
}
