// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Kernel bootstrap harness for Milestone 2C-2 (`hw_test=1`):
//! spawns Ring-3 `dry-hw-test.elf` with live `QEMU edu` & `ivshmem-plain` `PciDev` caps under
//! Intel `VT-d`, verifies clean process teardown (`leaked_frames == 0`), and emits telemetry.

use core::ptr::copy_nonoverlapping;

use crate::cspace::cap::CapError;
use crate::cspace::objects::cspace_alloc_frame;
use crate::cspace::revoke::attach_cap_mapping_record;
use crate::cspace::cspace_lookup_fast;
use crate::iommu::{dmar_snapshot, poll_and_clear_faults, status as vtd_status, VtdStatus};
use crate::irq::msi::msi_telemetry;
use crate::limine::get_boot_module;
use crate::pci::{
    is_pci_dev_present, mint_root_pci_dev_cap, pci_dma_telemetry, PCI_OBJ_IVSHMEM,
    PCI_OBJ_QEMU_EDU,
};
use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation, PageOrder};
use crate::proc::supervisor::mint_boot_module_cap;
use crate::proc::{
    reclaim_process, spawn_elf_process_with, ProcessOutcome, ReclaimStatus, SpawnOptions,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{init_scheduler, set_preemptive_mode_active};
use crate::security::{CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::map_4k;
use crate::vspace::pte::VmProt;

const BOOT_INFO_VADDR: u64 = 0x0070_0000;
const HW_TEST_BOOT_MAGIC: u64 = 0x4452_595f_4857_5431;

#[repr(C)]
#[derive(Clone, Copy)]
struct HwTestBootInfo {
    magic: u64,
    hw_mod_cap: u64,
    edu_pci_cap: u64,
    ivshmem_pci_cap: u64,
}

/// Run the Milestone 2C-2 Ring-3 PCI BAR64/WC, MSI, VT-d DMA, and Bound-Notification suite.
pub fn verify_hw_foundations(online_cpus: usize) {
    assert!(
        is_pci_dev_present(PCI_OBJ_QEMU_EDU),
        "[DRY-HW-KERNEL] Missing QEMU edu device (1234:11e8)"
    );
    assert!(
        is_pci_dev_present(PCI_OBJ_IVSHMEM),
        "[DRY-HW-KERNEL] Missing ivshmem-plain device (1af4:1110)"
    );

    let elf = get_boot_module("dry-hw-test.elf").expect("Missing Limine module dry-hw-test.elf");
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);
    let (denied0, quiesce0) = pci_dma_telemetry();
    let (msi_irqs0, msi_acks0, msi_spur0) = msi_telemetry();
    let _ = poll_and_clear_faults();

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf8, 1);

    let lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0x0038, 1);
    let info = spawn_elf_process_with(
        elf,
        SpawnOptions {
            subj_label: lbl,
            arg0: BOOT_INFO_VADDR,
            arg1: HW_TEST_BOOT_MAGIC,
            target_cpu: 0,
            trace_lifecycle: false,
            user_fs_base: 0,
            user_gs_base: 0,
        },
        |cs, l, vs_cap, vs_h| {
            let hw_mod_cap = mint_boot_module_cap(cs, l, "dry-hw-test.elf")?;
            let edu_pci_cap = mint_root_pci_dev_cap(cs, l, PCI_OBJ_QEMU_EDU)?;
            let ivshmem_pci_cap = mint_root_pci_dev_cap(cs, l, PCI_OBJ_IVSHMEM)?;

            let (info_fr_cap, info_fh) =
                cspace_alloc_frame(cs, l, l, PageOrder::Order4KiB, false, 0)?;
            let boot_info = HwTestBootInfo {
                magic: HW_TEST_BOOT_MAGIC,
                hw_mod_cap: hw_mod_cap.0,
                edu_pci_cap: edu_pci_cap.0,
                ivshmem_pci_cap: ivshmem_pci_cap.0,
            };
            // SAFETY: `info_fh` is a freshly allocated, zeroed 4 KiB frame for `BOOT_INFO_VADDR`.
            unsafe {
                copy_nonoverlapping(
                    (&raw const boot_info).cast::<u8>(),
                    info_fh.hhdm_ptr(),
                    core::mem::size_of::<HwTestBootInfo>(),
                );
            }
            map_4k(vs_h, BOOT_INFO_VADDR, info_fh, VmProt::USER_RO)
                .map_err(|_| CapError::PagingError)?;
            attach_cap_mapping_record(
                cspace_lookup_fast(cs, l, info_fr_cap, CapRights::READ, 0)?.global_slot,
                info_fr_cap.slot_gen(),
                vs_h.id,
                vs_h.generation,
                BOOT_INFO_VADDR,
                false,
            )?;
            let _ = vs_cap;
            Ok((BOOT_INFO_VADDR, HW_TEST_BOOT_MAGIC))
        },
    )
    .expect("spawn dry-hw-test failed");

    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert_eq!(
        reclaim_process(info.handle),
        Ok(ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0))),
        "dry-hw-test reported a hardware foundations failure"
    );

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before.free_global_frames.saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak after hw_test=1: {}", leaked);

    let vtd_st = vtd_status();
    let vtd_present = vtd_st != VtdStatus::NotPresent && vtd_st != VtdStatus::Uninitialized;
    let vtd_enabled = vtd_st == VtdStatus::Enforcing;
    let drhd = dmar_snapshot().map_or(0, |d| d.drhd_count);
    let vtd_faults = poll_and_clear_faults();
    let (denied1, quiesce1) = pci_dma_telemetry();
    let (msi_irqs1, msi_acks1, msi_spur1) = msi_telemetry();

    serial_println!(
        "[DRY-HW-KERNEL] PASS: cpus={} vtd_present={} vtd_enabled={} dmar_drhd={} quiesces={} dma_denied={} vtd_faults={} msi_irqs={} msi_acks={} msi_spurious={} leaked_frames={}",
        online_cpus,
        vtd_present as u8,
        vtd_enabled as u8,
        drhd,
        quiesce1.saturating_sub(quiesce0),
        denied1.saturating_sub(denied0),
        vtd_faults,
        msi_irqs1.saturating_sub(msi_irqs0),
        msi_acks1.saturating_sub(msi_acks0),
        msi_spur1.saturating_sub(msi_spur0),
        leaked,
    );
}
