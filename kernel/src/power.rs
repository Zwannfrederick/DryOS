// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated System Power Lifecycle & Idle Residency Telemetry (`Milestone 2A-3` `P3-C`).
//!
//! Separates unprivileged read-only power/idle telemetry (`POWER_OP_QUERY`) from privileged
//! hardware state transitions (`POWER_OP_SHUTDOWN_S5`, `POWER_OP_REBOOT`, `POWER_OP_SUSPEND_S3`),
//! which strictly require a live `CapObjectKind::PowerCtrl` capability in `SecDomain::RootSystem`
//! with `CapRights::MANAGE`.
//!
//! Epistemic Honesty Contract (`Suspend-to-RAM S3`):
//! Even when QEMU/OVMF exposes `\_S3_` in `DSDT` or a `FACS` physical pointer in `FADT`, `DRY OS`
//! runs under 64-bit UEFI without a `< 1 MiB` 16-bit real-mode wakeup trampoline, SMP AP `SIPI`
//! re-initialization path, IOAPIC redirection restore, or UEFI GOP re-POST. `POWER_OP_SUSPEND_S3`
//! therefore audits the capability gate, logs exact firmware table presence, and returns `-ENOSYS`
//! rather than faking sleep via screen blanking or VM pause.

use core::mem::size_of;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::acpi::acpi_power_info;
use crate::apic::uptime_telemetry;
use crate::arch::{cli, halt_forever, inw, outb, outw};
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::{insert_root_cap, with_pinned_cap};
use crate::ipc::map_cap_err_to_sys;
use crate::sched::{idle_hlt_telemetry, ONLINE_SCHED_CPUS};
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::{copy_to_user, SysError};

pub const POWER_OP_QUERY: u64 = 0;
pub const POWER_OP_SHUTDOWN_S5: u64 = 1;
pub const POWER_OP_REBOOT: u64 = 2;
pub const POWER_OP_SUSPEND_S3: u64 = 3;

const POWER_OBJ_ID: u32 = 1;
static POWER_OBJ_GEN: AtomicU32 = AtomicU32::new(1);
static POWER_DENIED_OPS: AtomicU64 = AtomicU64::new(0);

/// Sanitized 40-byte power & idle residency snapshot copied to CPL=3 by `SYS_POWER_CTL(QUERY)`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserPowerInfo {
    pub idle_hlt_calls: u64,
    pub idle_hlt_tsc: u64,
    pub elapsed_tsc: u64,
    pub fadt_present: u8,
    pub s5_supported: u8,
    pub reset_supported: u8,
    pub s3_aml_present: u8,
    pub s3_supported: u8,
    pub _pad: [u8; 3],
    pub denied_ops: u64,
}

const _: () = assert!(size_of::<UserPowerInfo>() == 40);

#[inline]
pub fn is_power_cap_live(obj_id: u32, expected_gen: u32) -> bool {
    obj_id == POWER_OBJ_ID
        && expected_gen != 0
        && POWER_OBJ_GEN.load(Ordering::Acquire) == expected_gen
}

pub fn power_denied_count() -> u64 {
    POWER_DENIED_OPS.load(Ordering::Acquire)
}

/// Mint the root `CapObjectKind::PowerCtrl` capability into `dry-init`'s `SecDomain::RootSystem` CSpace.
pub fn mint_root_power_cap(cspace_id: CSpaceId, root_subj: SecLabel) -> Result<CapPtr, CapError> {
    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    let gen = POWER_OBJ_GEN.load(Ordering::Acquire);
    insert_root_cap(
        cspace_id,
        CapObjectKind::PowerCtrl,
        POWER_OBJ_ID,
        gen,
        CapRights::POWER_ROOT,
        root_subj,
        0,
        0,
    )
}

fn verify_power_manage_cap(cspace_id: CSpaceId, subj: SecLabel, cap_raw: u64) -> Result<(), SysError> {
    if subj.domain != SecDomain::RootSystem || subj.flags.contains(SecFlags::DEV_WORKSPACE) {
        POWER_DENIED_OPS.fetch_add(1, Ordering::Relaxed);
        return Err(SysError::Perm);
    }
    let now = current_lease_epoch();
    let res = with_pinned_cap(
        cspace_id,
        subj,
        CapPtr(cap_raw),
        CapRights::MANAGE,
        now,
        |resolved| {
            if resolved.obj_kind != CapObjectKind::PowerCtrl {
                return Err(CapError::WrongObjectKind);
            }
            Ok(())
        },
    );
    if let Err(e) = res {
        POWER_DENIED_OPS.fetch_add(1, Ordering::Relaxed);
        return Err(map_cap_err_to_sys(e));
    }
    Ok(())
}

#[inline(never)]
pub fn sys_power_ctl(
    cspace_id: CSpaceId,
    subj: SecLabel,
    op: u64,
    arg: u64,
) -> Result<isize, SysError> {
    match op {
        POWER_OP_QUERY => {
            if arg == 0 || (arg & 7) != 0 {
                return Err(SysError::Fault);
            }
            let online = ONLINE_SCHED_CPUS.load(Ordering::Acquire).max(1);
            let (hlt_calls, hlt_tsc) = idle_hlt_telemetry(online);
            let (_, _, elapsed_tsc) = uptime_telemetry();
            let pwr = acpi_power_info();
            let info = UserPowerInfo {
                idle_hlt_calls: hlt_calls,
                idle_hlt_tsc: hlt_tsc,
                elapsed_tsc,
                fadt_present: u8::from(pwr.fadt_found),
                s5_supported: u8::from(pwr.s5_valid || pwr.pm1a_cnt_port != 0),
                reset_supported: 1, // FADT RESET_REG + architectural 0xCF9 / i8042 reset chain
                s3_aml_present: u8::from(pwr.s3_aml_present),
                s3_supported: 0, // Honest: UEFI real-mode wake vector + SMP/IOAPIC/GOP resume not wired
                _pad: [0; 3],
                denied_ops: power_denied_count(),
            };
            // SAFETY: `UserPowerInfo` is `#[repr(C)]` POD (`40` bytes) with zeroed padding.
            let bytes = unsafe {
                core::slice::from_raw_parts((&raw const info).cast::<u8>(), size_of::<UserPowerInfo>())
            };
            copy_to_user(arg, bytes)?;
            Ok(0)
        }
        POWER_OP_SUSPEND_S3 => {
            verify_power_manage_cap(cspace_id, subj, arg)?;
            let pwr = acpi_power_info();
            serial_println!(
                "[DRY-KERNEL-POWER] S3 suspend rejected: -ENOSYS (s3_aml={} facs_present={} gap=missing_1mib_realmode_trampoline_smp_sipi_ioapic_gop_resume)",
                pwr.s3_aml_present,
                pwr.facs_phys != 0
            );
            Err(SysError::NoSys)
        }
        POWER_OP_SHUTDOWN_S5 => {
            verify_power_manage_cap(cspace_id, subj, arg)?;
            let pwr = acpi_power_info();
            serial_println!(
                "[DRY-KERNEL-POWER] Entering hardware ACPI S5 poweroff (fadt={} pm1a={:#x} slp_typ_a={})",
                pwr.fadt_found,
                pwr.pm1a_cnt_port,
                pwr.slp_typ_s5_a
            );
            cli();
            // SAFETY: Authorized `RootSystem` S5 transition after controlled userspace teardown.
            unsafe {
                if pwr.pm1a_cnt_port != 0 {
                    if pwr.smi_cmd_port != 0
                        && pwr.acpi_enable_val != 0
                        && (inw(pwr.pm1a_cnt_port) & 1) == 0
                    {
                        outb(pwr.smi_cmd_port, pwr.acpi_enable_val);
                    }
                    let slp_en = 1u16 << 13;
                    let val_a = ((pwr.slp_typ_s5_a as u16) << 10) | slp_en;
                    outw(pwr.pm1a_cnt_port, val_a);
                    if pwr.pm1b_cnt_port != 0 {
                        let val_b = ((pwr.slp_typ_s5_b as u16) << 10) | slp_en;
                        outw(pwr.pm1b_cnt_port, val_b);
                    }
                }
                // Architectural QEMU PIIX4 (`0x0604`) & Bochs (`0xB004`) soft-off fallback if FADT was absent.
                outw(0x0604, 0x2000);
                outw(0xb004, 0x2000);
            }
            halt_forever();
        }
        POWER_OP_REBOOT => {
            verify_power_manage_cap(cspace_id, subj, arg)?;
            let pwr = acpi_power_info();
            serial_println!(
                "[DRY-KERNEL-POWER] Triggering hardware system reset (fadt_port={:#x} reset_val={:#x})",
                pwr.reset_reg_io_port,
                pwr.reset_value
            );
            cli();
            // SAFETY: Authorized `RootSystem` hardware reset sequence (`FADT.RESET_REG` -> `0xCF9` -> `0x64`).
            unsafe {
                if pwr.reset_supported && pwr.reset_reg_io_port != 0 {
                    outb(pwr.reset_reg_io_port, pwr.reset_value);
                }
                outb(0x0cf9, 0x02);
                outb(0x0cf9, 0x06);
                outb(0x0064, 0xfe);
            }
            halt_forever();
        }
        _ => Err(SysError::Inval),
    }
}
