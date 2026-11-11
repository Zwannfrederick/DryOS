// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Kernel (`x86-64`, `no_std`) — Phase 1 Milestone 1A Entry & Deterministic Panic Handler.

#![no_std]
#![no_main]

mod arch;
mod gdt_tss;
mod idt;
mod limine;
mod mem_verify;
mod security;
mod serial;

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::read_volatile;

use crate::arch::{cli, qemu_exit, read_cr3, read_rsp, QEMU_EXIT_PANIC, QEMU_EXIT_SUCCESS};
use crate::gdt_tss::init_cpu_gdt_tss;
use crate::idt::{init_bsp_idt, verify_breakpoint_trap_recovery};
use crate::limine::{
    base_revision_supported, cstr_to_str, BOOTLOADER_INFO_REQ, CMDLINE_REQ, FIRMWARE_TYPE_REQ,
    LIMINE_FIRMWARE_EFI64,
};
use crate::mem_verify::verify_memory_and_hhdm;
use crate::security::verify_boot_security_invariants;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    cli();
    serial::init();

    serial_println!("[DRY-BOOT] DRY Kernel v0.1.0 (x86_64, no_std) starting on BSP...");

    assert!(
        base_revision_supported(),
        "Limine Base Revision 3 not supported by bootloader"
    );

    let bl_resp = BOOTLOADER_INFO_REQ.read().response;
    let (bl_name, bl_ver) = if !bl_resp.is_null() {
        // SAFETY: Valid NUL-terminated C strings supplied by Limine.
        unsafe { (cstr_to_str((*bl_resp).name), cstr_to_str((*bl_resp).version)) }
    } else {
        ("<unknown>", "<unknown>")
    };

    let fw_resp = FIRMWARE_TYPE_REQ.read().response;
    let fw_type = if !fw_resp.is_null() {
        // SAFETY: Valid response struct supplied by Limine.
        unsafe { (*fw_resp).firmware_type }
    } else {
        u64::MAX
    };
    assert_eq!(
        fw_type, LIMINE_FIRMWARE_EFI64,
        "Expected UEFI x86-64 firmware (type 2), got {}",
        fw_type
    );

    let cmdline = {
        let cmd_resp = CMDLINE_REQ.read().response;
        if !cmd_resp.is_null() {
            // SAFETY: Valid NUL-terminated command line string supplied by Limine.
            unsafe { cstr_to_str((*cmd_resp).cmdline) }
        } else {
            ""
        }
    };

    serial_println!(
        "[DRY-BOOT] Bootloader: {} {} | Firmware: EFI64 | CR3={:#x} RSP={:#x} | cmdline=\"{}\"",
        bl_name,
        bl_ver,
        read_cr3(),
        read_rsp(),
        cmdline
    );

    let (df_ist1_top, rsp0_top) = init_cpu_gdt_tss(0);
    init_bsp_idt();
    let (bp_hits, bp_rip) = verify_breakpoint_trap_recovery();
    serial_println!(
        "[DRY-CPU] PASS: GDT(CS=0x08,TR=0x28) TSS(IST1={:#x},RSP0={:#x}) IDT(256) #BP(hits={},rip={:#x})",
        df_ist1_top,
        rsp0_top,
        bp_hits,
        bp_rip
    );

    if cmdline.contains("panic_self_test=1") {
        panic!("intentional panic_self_test triggered via kernel cmdline");
    }
    if cmdline.contains("fault_self_test=ud2") {
        // SAFETY: Intentional invalid opcode test verified by automated QEMU harness.
        unsafe {
            asm!("ud2", options(nomem, nostack));
        }
    }
    if cmdline.contains("fault_self_test=pf") {
        // SAFETY: Intentional non-present canonical address read to verify `#PF` + `CR2` capture.
        unsafe {
            let _ = read_volatile(0x0000_7fff_dead_bee0usize as *const u64);
        }
    }

    let mem = verify_memory_and_hhdm();
    serial_println!(
        "[DRY-HHDM] PASS: offset={:#x} kernel_phys={:#x} kernel_virt={:#x}",
        mem.hhdm_offset,
        mem.kernel_phys_base,
        mem.kernel_virt_base
    );
    serial_println!(
        "[DRY-MEMMAP] PASS: entries={} usable_regions={} usable_mib={} reclaimable_kib={}",
        mem.total_entries,
        mem.usable_regions,
        mem.usable_bytes / (1024 * 1024),
        mem.reclaimable_bytes / 1024
    );

    verify_boot_security_invariants();
    serial_println!(
        "[DRY-SEC] PASS: ADR-007 SecLabel (8B) + 3-Domain + Lease/Revoke invariants verified"
    );

    serial_println!("[DRY-BOOT-OK] Milestone 1A single-CPU UEFI boot verified cleanly.");
    qemu_exit(QEMU_EXIT_SUCCESS);
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    cli();
    if let Some(loc) = info.location() {
        serial::emergency_print(format_args!(
            "\r\n[DRY-PANIC] {}:{}:{} — {}\r\n",
            loc.file(),
            loc.line(),
            loc.column(),
            info.message()
        ));
    } else {
        serial::emergency_print(format_args!("\r\n[DRY-PANIC] <no-location> — {}\r\n", info.message()));
    }
    qemu_exit(QEMU_EXIT_PANIC);
}
