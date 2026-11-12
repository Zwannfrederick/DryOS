// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Kernel (`x86-64`, `no_std`) — Phase 1 Milestone 1A Entry & Deterministic Panic Handler.

#![no_std]
#![no_main]

mod acpi;
mod apic;
mod arch;
mod cspace;
mod display;
mod gdt_tss;
mod idt;
mod ipc;
mod irq;
mod limine;
mod mem_verify;
mod pci;
mod percpu;
mod pmm;
mod power;
mod proc;
mod sched;
mod security;
mod serial;
mod smp;
mod syscall;
mod uaccess;
mod vault_ctrl;
mod vspace;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::{cli, halt_forever, qemu_exit, read_cr3, read_rsp, QEMU_EXIT_PANIC, QEMU_EXIT_SUCCESS};
use crate::gdt_tss::{init_cpu_gdt_tss, prepare_cpu_boot_stack};
use crate::idt::{init_bsp_idt, verify_breakpoint_trap_recovery};
use crate::limine::{
    base_revision_supported, cstr_to_str, latch_boot_modules_to_static_bss,
    latch_cmdline_to_static_bss, latch_framebuffer_to_static_bss, BOOTLOADER_INFO_REQ,
    FIRMWARE_TYPE_REQ, LIMINE_FIRMWARE_EFI64, MP_REQ,
};
use crate::mem_verify::verify_memory_and_hhdm;
use crate::percpu::{init_cpu_percpu_and_gs, CpuRegionInfo};
use crate::security::verify_boot_security_invariants;
use crate::smp::{init_and_verify_smp, SmpFaultTest};

// Switch BSP (`cpu_idx = 0`) immediately from Limine's temporary `BOOTLOADER_RECLAIMABLE` stack
// onto its permanent 16 KiB `'static` `CPU_DESCS[0].boot_stack` so BSP and all APs share identical
// stack lifetime & canary invariants.
global_asm!(
    ".global _start",
    ".balign 16",
    "_start:",
    "    cli",
    "    cld",
    "    and rsp, -16",
    "    call {prep_bsp}",
    "    mov rsp, rax",
    "    xor ebp, ebp",
    "    call {bsp_main}",
    prep_bsp = sym prepare_bsp_stack_top,
    bsp_main = sym bsp_kmain,
);

extern "C" fn prepare_bsp_stack_top() -> u64 {
    let (_, top) = prepare_cpu_boot_stack(0);
    top
}

extern "C" fn bsp_kmain() -> ! {
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

    let cmdline = latch_cmdline_to_static_bss();
    let boot_mods = latch_boot_modules_to_static_bss();
    let _fb_latched = latch_framebuffer_to_static_bss();

    let bsp_lapic_id = {
        let mp_resp = MP_REQ.read().response;
        assert!(!mp_resp.is_null(), "Limine MP response is null");
        // SAFETY: Valid response struct supplied by Limine.
        unsafe { (*mp_resp).bsp_lapic_id }
    };

    serial_println!(
        "[DRY-BOOT] Bootloader: {} {} | Firmware: EFI64 | CR3={:#x} RSP={:#x} | cmdline=\"{}\"",
        bl_name,
        bl_ver,
        read_cr3(),
        read_rsp(),
        cmdline
    );

    let layout = init_cpu_gdt_tss(0);
    init_bsp_idt();
    let bsp_pcpu = init_cpu_percpu_and_gs(CpuRegionInfo {
        cpu_idx: 0,
        lapic_id: bsp_lapic_id,
        boot_stack_base: layout.boot_stack_base,
        boot_stack_top: layout.boot_stack_top,
        df_ist1_base: layout.df_ist1_base,
        df_ist1_top: layout.df_ist1_top,
        tss_base: layout.tss_base,
        gdt_base: layout.gdt_base,
    });
    let (bp_hits, bp_rip) = verify_breakpoint_trap_recovery(bsp_pcpu);
    serial_println!(
        "[DRY-CPU] PASS: BSP(cpu=0,lapic={}) GDT(CS=0x08,TR=0x28,Busy=0x8b) TSS(IST1={:#x},RSP0={:#x}) GS={:#x} IDT(256) #BP(hits={},rip={:#x})",
        bsp_lapic_id,
        layout.df_ist1_top,
        layout.boot_stack_top,
        bsp_pcpu.self_ptr,
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
    let _pmm_inv = pmm::init_pmm(&mem);
    let _fb_valid = display::init_framebuffer(&mem, cmdline);
    let _pci_blk = pci::init_pci_devices();
    let acpi_madt = acpi::discover_acpi_madt(
        mem.hhdm_offset,
        mem.kernel_phys_base,
        mem.kernel_virt_base,
    );
    serial_println!(
        "[DRY-ACPI] PASS: rsdp_rev={} xsdt={} ioapic_found={} ioapic_id={} ioapic_phys={:#x} gsi_base={} iso_overrides={}",
        acpi_madt.rsdp_revision,
        acpi_madt.used_xsdt,
        acpi_madt.ioapic_found,
        acpi_madt.ioapic_id,
        acpi_madt.ioapic_phys,
        acpi_madt.ioapic_gsi_base,
        acpi_madt.iso_override_count
    );
    let acpi_pwr = acpi::acpi_power_info();
    serial_println!(
        "[DRY-ACPI-PWR] PASS: fadt={} pm1a={:#x} s5={} slp_typ_a={} reset_port={:#x} s3_aml={} facs={:#x}",
        acpi_pwr.fadt_found,
        acpi_pwr.pm1a_cnt_port,
        acpi_pwr.s5_valid,
        acpi_pwr.slp_typ_s5_a,
        acpi_pwr.reset_reg_io_port,
        acpi_pwr.s3_aml_present,
        acpi_pwr.facs_phys
    );

    let force_xapic = cmdline.contains("apic_mode=xapic");
    let apic_cal = apic::detect_and_init_bsp_apic(
        mem.hhdm_offset,
        mem.kernel_phys_base,
        mem.kernel_virt_base,
        force_xapic,
    );
    serial_println!(
        "[DRY-APIC] PASS: mode={} div=16 samples_ticks_ms=[{},{},{}] ticks_per_ms={} quantum_ticks={} quantum_us={} tsc_mhz={} cpuid_crystal_hz={} cpuid_base_mhz={} arat={} inv_tsc={} tsc_dl={}",
        apic_cal.mode.as_str(),
        apic_cal.samples_ticks_ms[0],
        apic_cal.samples_ticks_ms[1],
        apic_cal.samples_ticks_ms[2],
        apic_cal.ticks_per_ms,
        apic_cal.quantum_ticks,
        apic_cal.quantum_us,
        apic_cal.tsc_mhz,
        apic_cal.cpuid_crystal_hz,
        apic_cal.cpuid_base_mhz,
        apic_cal.has_arat as u8,
        apic_cal.has_inv_tsc as u8,
        apic_cal.has_tsc_deadline as u8,
    );

    let smp_fault = SmpFaultTest::from_cmdline(cmdline);
    let smp = init_and_verify_smp(bsp_pcpu, smp_fault);
    serial_println!(
        "[DRY-SMP] PASS: cpus_online={} mask={:#x} total_bp_traps={} symmetric_ops={} stacks_disjoint=true",
        smp.cpu_count,
        smp.online_mask,
        smp.total_bp_traps,
        smp.total_work_ops
    );

    let sched_fault = sched::tests::SchedFaultTest::from_cmdline(cmdline);
    let sched_summary = sched::tests::verify_cooperative_scheduler(smp.cpu_count, sched_fault);
    serial_println!(
        "[DRY-SCHED] PASS: cpus={} spawned={} exited={} recycled_slots={} ctx_switches={} migrations={} park_rounds={} work_units={} stale_wakes_rejected={} orphan_ready=0",
        sched_summary.cpus,
        sched_summary.spawned,
        sched_summary.exited,
        sched_summary.recycled_slots,
        sched_summary.ctx_switches,
        sched_summary.migrations,
        sched_summary.park_rounds,
        sched_summary.work_units,
        sched_summary.stale_wakes_rejected,
    );

    let irq_only_stage = cmdline.contains("preempt_stage=irq_only");
    let preempt_fault = sched::preempt_tests::PreemptFaultTest::from_cmdline(cmdline);
    sched::preempt_tests::verify_preemptive_scheduler(smp.cpu_count, irq_only_stage, preempt_fault);
    if !irq_only_stage {
        let pmm_fault = pmm::tests::PmmFaultTest::from_cmdline(cmdline);
        pmm::tests::verify_pmm_and_untyped(smp.cpu_count, apic_cal.tsc_mhz, pmm_fault);
        let vspace_summary =
            vspace::init_and_activate_kernel_vspace(&mem, smp.cpu_count, apic_cal.tsc_mhz);
        let _ioapic_ok = irq::init_ioapic_and_irq_subsystem(mem.hhdm_offset, bsp_lapic_id);
        if cmdline.contains("irq_fault=") {
            syscall::init_all_cpus_syscall_and_smap(smp.cpu_count);
            irq::tests::maybe_run_irq_fault_test(cmdline, smp.cpu_count);
        }
        if cmdline.contains("ipc_fault=") {
            syscall::init_all_cpus_syscall_and_smap(smp.cpu_count);
            ipc::tests::maybe_run_ipc_fault_test(cmdline, smp.cpu_count);
        }
        if cmdline.contains("user_fault=") {
            syscall::init_all_cpus_syscall_and_smap(smp.cpu_count);
            proc::tests::verify_userspace_and_syscalls(smp.cpu_count, apic_cal.tsc_mhz, cmdline);
        }
        if cmdline.contains("cspace_fault=") {
            cspace::tests::verify_cspace_and_security(smp.cpu_count, apic_cal.tsc_mhz, cmdline);
        }
        if cmdline.contains("interactive=1")
            || cmdline.contains("desktop=1")
            || cmdline.contains("desktop3=1")
            || cmdline.contains("desktop4=1")
        {
            syscall::init_all_cpus_syscall_and_smap(smp.cpu_count);
            irq::session::maybe_run_p1_interactive_session(cmdline, smp.cpu_count);
            sched::shutdown_ap_scheduler_service();
            if cmdline.contains("desktop4=1") {
                serial_println!(
                    "[DRY-BOOT-OK] Milestone 2B (Persistent Storage, Labeled VFS & DRY Vault) {}-vCPU session verified cleanly.",
                    smp.cpu_count
                );
            } else if cmdline.contains("desktop3=1") {
                serial_println!(
                    "[DRY-BOOT-OK] Milestone 2A-3 (Functional Desktop & Native Apps) {}-vCPU session verified cleanly.",
                    smp.cpu_count
                );
            } else if cmdline.contains("desktop=1") {
                serial_println!(
                    "[DRY-BOOT-OK] Milestone 2A-2 (DRY Desktop Preview) {}-vCPU multi-window desktop session verified cleanly.",
                    smp.cpu_count
                );
            } else {
                serial_println!(
                    "[DRY-BOOT-OK] Milestone 2A-1 (P1 First Interaction) {}-vCPU interactive session verified cleanly.",
                    smp.cpu_count
                );
            }
            qemu_exit(QEMU_EXIT_SUCCESS);
        }
        vspace::tests::verify_vspace_and_tlb(
            &vspace_summary,
            smp.cpu_count,
            apic_cal.tsc_mhz,
            cmdline,
        );
        cspace::tests::verify_cspace_and_security(smp.cpu_count, apic_cal.tsc_mhz, cmdline);
        assert!(boot_mods >= 5, "Expected >=5 Limine userspace ELF64 boot modules, got {}", boot_mods);
        syscall::init_all_cpus_syscall_and_smap(smp.cpu_count);
        proc::tests::verify_userspace_and_syscalls(smp.cpu_count, apic_cal.tsc_mhz, cmdline);
        ipc::tests::verify_ipc_services_and_display(cmdline, smp.cpu_count);
        irq::tests::verify_irq_and_ioport_security(cmdline, smp.cpu_count);
    }
    sched::shutdown_ap_scheduler_service();

    if !irq_only_stage {
        serial_println!(
            "[DRY-BOOT-OK] Milestone 1C UEFI boot + {}-vCPU SMP + H1 scheduler + PMM + VSpace + CSpace + Ring 3 Services, IPC & GOP Display verified cleanly.",
            smp.cpu_count
        );
        if cmdline.contains("screendump_hold=1") {
            serial_println!("[DRY-FB-READY] Waiting for QMP screendump...");
            halt_forever();
        }
    }
    qemu_exit(QEMU_EXIT_SUCCESS);
}

const NO_PANIC_OWNER: u64 = u64::MAX;
const EARLY_CPU_TOKEN: u64 = u64::MAX - 1;
static PANIC_OWNER: AtomicU64 = AtomicU64::new(NO_PANIC_OWNER);

/// Claim exclusive ownership of fatal serial reporting; if another CPU is already panicking, halt.
pub fn enter_fatal_or_halt() {
    cli();
    let token = percpu::try_current_percpu().map_or(EARLY_CPU_TOKEN, |c| c.cpu_idx);
    if matches!(
        PANIC_OWNER.compare_exchange(
            NO_PANIC_OWNER,
            token,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Err(owner) if owner != token
    ) {
        halt_forever();
    }
}

pub fn is_panicking() -> bool {
    PANIC_OWNER.load(Ordering::Acquire) != NO_PANIC_OWNER
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    enter_fatal_or_halt();
    let cpu_id = percpu::try_current_percpu().map_or(u64::MAX, |c| c.cpu_idx);
    if let Some(loc) = info.location() {
        serial::emergency_print(format_args!(
            "\r\n[DRY-PANIC] cpu={} {}:{}:{} — {}\r\n",
            cpu_id,
            loc.file(),
            loc.line(),
            loc.column(),
            info.message()
        ));
    } else {
        serial::emergency_print(format_args!(
            "\r\n[DRY-PANIC] cpu={} <no-location> — {}\r\n",
            cpu_id,
            info.message()
        ));
    }
    qemu_exit(QEMU_EXIT_PANIC);
}
