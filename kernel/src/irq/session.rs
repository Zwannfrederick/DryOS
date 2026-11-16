// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Interactive & Desktop Session Bootstrap (`P1`, `Milestone 2A-2`, `Milestone 2A-3` & `Milestone 2B`).
//!
//! Mints root boot-module and hardware capabilities into `dry-init.elf`'s `SecDomain::RootSystem`
//! `CSpace`, maps the sanitized read-only `P1InitBootInfo` + `BootModManifestEntry` page at
//! `0x0070_0000`, runs preemptive SMP scheduling until graceful session teardown, and audits zero
//! leaked frames.

use core::mem::size_of;
use core::ptr::copy_nonoverlapping;
use core::sync::atomic::Ordering;

use crate::arch::{cli, sti};
use crate::cspace::cap::{CapError, CapPtr};
use crate::cspace::objects::cspace_alloc_frame;
use crate::cspace::revoke::attach_cap_mapping_record;
use crate::cspace::cspace_lookup_fast;
use crate::display::{is_framebuffer_available, mint_root_framebuffer_cap};
use crate::irq::ioport::{mint_root_ioport_cap, I8042_DATA_PORT, I8042_STATUS_CMD_PORT};
use crate::irq::{
    ioapic_present, irq_line_delivered, irq_telemetry, mint_root_irq_cap, IRQ_KEYBOARD_PS2,
    IRQ_MOUSE_PS2,
};
use crate::limine::{
    get_boot_module, latched_boot_module_count, latched_boot_module_meta, MAX_LATCHED_MODULES,
};
use crate::percpu::get_percpu_by_idx;
use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation, PageOrder};
use crate::proc::supervisor::mint_boot_module_cap;
use crate::proc::{
    reclaim_process, spawn_elf_process_with, ProcessOutcome, ReclaimStatus, SpawnOptions,
};
use crate::sched::{
    disarm_epoch_watchdog, idle_dispatch_once, init_scheduler, set_ap_epoch_kind,
    set_preemptive_mode_active, start_cpu_timer_for_phase, stop_cpu_timer_for_phase, ApEpochKind,
    AP_PHASE_EPOCH, PHASE_PENDING_THREADS,
};
use crate::security::{set_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::map_4k;
use crate::vspace::pte::VmProt;

const BOOT_INFO_VADDR: u64 = 0x0070_0000;
const BOOT_MANIFEST_OFFSET: usize = 0x0100;
const BOOT_EXT_INFO_OFFSET: usize = 0x0700;
const ROOT_INIT_BOOT_MAGIC: u64 = 0x4452_595f_494e_4954;
const ROOT_BOOT_EXT_MAGIC: u64 = 0x4452_595f_4245_5854;

const BOOT_FLAG_LIVE_USB: u64 = 1 << 0;
const BOOT_FLAG_STORAGE_RO: u64 = 1 << 1;
const BOOT_FLAG_RECOVERY: u64 = 1 << 2;
const BOOT_FLAG_UART_PRESENT: u64 = 1 << 3;
const BOOT_FLAG_VTD_ENFORCING: u64 = 1 << 4;
const BOOT_FLAG_VTD_GFX_DEFERRED: u64 = 1 << 5;
const BOOT_FLAG_SAFE_GOP: u64 = 1 << 6;
const BOOT_FLAG_USB_PERSIST_PREFER: u64 = 1 << 7;
const BOOT_FLAG_DIAG_SELFTEST: u64 = 1 << 8;

#[repr(C)]
#[derive(Clone, Copy)]
struct P1InitBootInfo {
    magic: u64,
    echo_mod_cap: u64,
    display_mod_cap: u64,
    fb_cap: u64,
    input_mod_cap: u64,
    shell_mod_cap: u64,
    irq1_cap: u64,
    i8042_port_cap: u64,
    session_mode: u64,
    irq12_cap: u64,
    sysmon_mod_cap: u64,
    vfs_mod_cap: u64,
    files_mod_cap: u64,
    settings_mod_cap: u64,
    power_mod_cap: u64,
    power_ctrl_cap: u64,
    boot_manifest_count: u64,
    blk_mod_cap: u64,
    vault_mod_cap: u64,
    pci_blk_cap: u64,
    vault_ctrl_cap: u64,
    broker_mod_cap: u64,
    mock_drv_mod_cap: u64,
    lx_rng_mod_cap: u64,
    pci_rng_cap: u64,
    gpu_mod_cap: u64,
    pci_gpu_cap: u64,
    editor_mod_cap: u64,
    calc_mod_cap: u64,
    lx_drm_mod_cap: u64,
    pci_disp_aux_cap: u64,
    boot_pci_func_count: u64,
}

const _: () = assert!(size_of::<P1InitBootInfo>() == BOOT_MANIFEST_OFFSET);

#[repr(C)]
#[derive(Clone, Copy)]
struct BootModManifestEntry {
    name: [u8; 40],
    size_bytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RootInitBootExtInfo {
    magic: u64,
    usb_mod_cap: u64,
    pci_xhci_cap: u64,
    nvme_mod_cap: u64,
    pci_nvme_cap: u64,
    lx_net_mod_cap: u64,
    pci_net_cap: u64,
    pci_igpu_cap: u64,
    boot_flags: u64,
    dgpu_packed: u64,
    vmd_packed: u64,
    lpss_i2c_count: u64,
}

const _: () = assert!(
    BOOT_MANIFEST_OFFSET + MAX_LATCHED_MODULES * size_of::<BootModManifestEntry>()
        <= BOOT_EXT_INFO_OFFSET
);
const _: () = assert!(
    BOOT_EXT_INFO_OFFSET + size_of::<RootInitBootExtInfo>() <= crate::pci::BOOT_PCI_ENUM_OFFSET
);
const _: () = assert!(
    crate::pci::BOOT_PCI_ENUM_OFFSET
        + crate::pci::MAX_BOOT_PCI_FUNCS * size_of::<crate::pci::BootPciFuncRecord>()
        <= crate::pmm::PAGE_SIZE
);

/// Launch the live Ring 3 session (`interactive=1`, `desktop=1`, `desktop3=1`, or `desktop4=1`).
#[inline(never)]
pub fn maybe_run_p1_interactive_session(cmdline: &str, online_cpus: usize) {
    let is_desktop4 = cmdline.contains("desktop4=1");
    let is_desktop3 = cmdline.contains("desktop3=1") || is_desktop4;
    let is_desktop = cmdline.contains("desktop=1") || is_desktop3;
    if !cmdline.contains("interactive=1") && !is_desktop {
        return;
    }

    let init_elf = get_boot_module("dry-init.elf").expect("missing dry-init.elf");
    let _ = get_boot_module("dry-input.elf").expect("missing dry-input.elf");
    let _ = get_boot_module("dry-shell.elf").expect("missing dry-shell.elf");
    if is_desktop {
        let _ = get_boot_module("dry-sysmon.elf").expect("missing dry-sysmon.elf");
    }

    let session_mode = if is_desktop4 {
        if cmdline.contains("p4_sec=1") {
            21u64
        } else if cmdline.contains("p4_bench=1") {
            22u64
        } else if cmdline.contains("p4_shutdown=1") {
            23u64
        } else if cmdline.contains("island_poc=1") {
            25u64
        } else if cmdline.contains("island_bench=1") {
            26u64
        } else if cmdline.contains("lx_island=1") {
            27u64
        } else if cmdline.contains("gpu_test=1") {
            28u64
        } else if cmdline.contains("lx_drm=1") {
            29u64
        } else if cmdline.contains("usb_persist_test=1") {
            30u64
        } else if cmdline.contains("lx_net_test=1") {
            31u64
        } else {
            20u64
        }
    } else if is_desktop3 {
        if cmdline.contains("p3_sec=1") {
            11u64
        } else if cmdline.contains("p3_bench=1") {
            12u64
        } else if cmdline.contains("p3_shutdown=1") {
            13u64
        } else if cmdline.contains("p3_reboot=1") {
            14u64
        } else {
            10u64
        }
    } else if is_desktop {
        if cmdline.contains("desktop_sec=1") {
            3u64
        } else if cmdline.contains("desktop_bench=1") {
            4u64
        } else {
            2u64
        }
    } else {
        1u64
    };

    // Reset lease epoch baseline to `1` at session launch.
    set_lease_epoch(1);
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    disarm_epoch_watchdog();
    PHASE_PENDING_THREADS.store(1, Ordering::Release);

    let root_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0, 1);
    let init_proc = spawn_elf_process_with(
        init_elf,
        SpawnOptions {
            subj_label: root_lbl,
            target_cpu: 0,
            arg0: BOOT_INFO_VADDR,
            arg1: session_mode,
            user_fs_base: 0,
            user_gs_base: 0,
            trace_lifecycle: false,
        },
        |cs, lbl, vs_cap, vs_h| {
            let echo_cap = mint_boot_module_cap(cs, lbl, "dry-echo.elf")?;
            let disp_cap = mint_boot_module_cap(cs, lbl, "dry-display.elf").unwrap_or(CapPtr::NULL);
            let input_cap = mint_boot_module_cap(cs, lbl, "dry-input.elf")?;
            let shell_cap = mint_boot_module_cap(cs, lbl, "dry-shell.elf")?;
            let sysmon_cap =
                mint_boot_module_cap(cs, lbl, "dry-sysmon.elf").unwrap_or(CapPtr::NULL);
            let vfs_cap = mint_boot_module_cap(cs, lbl, "dry-vfs.elf").unwrap_or(CapPtr::NULL);
            let files_cap = mint_boot_module_cap(cs, lbl, "dry-files.elf").unwrap_or(CapPtr::NULL);
            let settings_cap =
                mint_boot_module_cap(cs, lbl, "dry-settings.elf").unwrap_or(CapPtr::NULL);
            let power_cap = mint_boot_module_cap(cs, lbl, "dry-power.elf").unwrap_or(CapPtr::NULL);
            let blk_cap = mint_boot_module_cap(cs, lbl, "dry-blk.elf").unwrap_or(CapPtr::NULL);
            let vault_cap = mint_boot_module_cap(cs, lbl, "dry-vault.elf").unwrap_or(CapPtr::NULL);
            let broker_cap =
                mint_boot_module_cap(cs, lbl, "dry-broker.elf").unwrap_or(CapPtr::NULL);
            let mock_drv_cap =
                mint_boot_module_cap(cs, lbl, "dry-mock-drv.elf").unwrap_or(CapPtr::NULL);
            let lx_rng_cap =
                mint_boot_module_cap(cs, lbl, "dry-lx-rng.elf").unwrap_or(CapPtr::NULL);
            let gpu_mod_cap =
                mint_boot_module_cap(cs, lbl, "dry-gpu.elf").unwrap_or(CapPtr::NULL);
            let editor_mod_cap =
                mint_boot_module_cap(cs, lbl, "dry-editor.elf").unwrap_or(CapPtr::NULL);
            let calc_mod_cap =
                mint_boot_module_cap(cs, lbl, "dry-calc.elf").unwrap_or(CapPtr::NULL);
            let lx_drm_mod_cap =
                mint_boot_module_cap(cs, lbl, "dry-lx-drm-bochs.elf").unwrap_or(CapPtr::NULL);
            let usb_mod_cap =
                mint_boot_module_cap(cs, lbl, "dry-usb.elf").unwrap_or(CapPtr::NULL);
            let nvme_mod_cap =
                mint_boot_module_cap(cs, lbl, "dry-nvme.elf").unwrap_or(CapPtr::NULL);
            let lx_net_mod_cap =
                mint_boot_module_cap(cs, lbl, "dry-lx-net.elf").unwrap_or(CapPtr::NULL);
            let safe_gop = cmdline.contains("safe_gop=1");
            let fb_cap = if is_framebuffer_available() {
                mint_root_framebuffer_cap(cs, lbl)?
            } else {
                CapPtr::NULL
            };
            let irq1_cap = mint_root_irq_cap(cs, lbl, IRQ_KEYBOARD_PS2, 1u64 << IRQ_KEYBOARD_PS2)?;
            let irq12_cap = if session_mode >= 2 {
                mint_root_irq_cap(cs, lbl, IRQ_MOUSE_PS2, 1u64 << IRQ_MOUSE_PS2)?
            } else {
                CapPtr::NULL
            };
            let i8042_cap =
                mint_root_ioport_cap(cs, lbl, &[I8042_DATA_PORT, I8042_STATUS_CMD_PORT])?;
            let pwr_ctrl_cap = if session_mode >= 10 {
                crate::power::mint_root_power_cap(cs, lbl)?
            } else {
                CapPtr::NULL
            };
            let pci_blk_cap = if session_mode >= 20 && crate::pci::is_pci_blk_present() {
                crate::pci::mint_root_pci_blk_cap(cs, lbl)?
            } else {
                CapPtr::NULL
            };
            let pci_rng_cap = if session_mode >= 20
                && crate::pci::is_pci_dev_present(crate::pci::PCI_OBJ_VIRTIO_RNG)
            {
                crate::pci::mint_root_pci_dev_cap(cs, lbl, crate::pci::PCI_OBJ_VIRTIO_RNG)?
            } else {
                CapPtr::NULL
            };
            let pci_gpu_cap = if session_mode >= 20
                && crate::pci::is_pci_dev_present(crate::pci::PCI_OBJ_VIRTIO_GPU)
            {
                crate::pci::mint_root_pci_dev_cap(cs, lbl, crate::pci::PCI_OBJ_VIRTIO_GPU)?
            } else {
                CapPtr::NULL
            };
            let pci_disp_aux_cap = if session_mode >= 20
                && !safe_gop
                && crate::pci::is_pci_dev_present(crate::pci::PCI_OBJ_DISPLAY_AUX)
            {
                crate::pci::mint_root_pci_dev_cap(cs, lbl, crate::pci::PCI_OBJ_DISPLAY_AUX)?
            } else {
                CapPtr::NULL
            };
            let pci_xhci_cap = if session_mode >= 20
                && crate::pci::is_pci_dev_present(crate::pci::PCI_OBJ_XHCI)
            {
                crate::pci::mint_root_pci_dev_cap(cs, lbl, crate::pci::PCI_OBJ_XHCI)?
            } else {
                CapPtr::NULL
            };
            let pci_nvme_cap = if session_mode >= 20
                && !safe_gop
                && crate::pci::is_pci_dev_present(crate::pci::PCI_OBJ_NVME)
            {
                crate::pci::mint_root_pci_dev_cap(cs, lbl, crate::pci::PCI_OBJ_NVME)?
            } else {
                CapPtr::NULL
            };
            let pci_net_cap = if session_mode >= 20
                && !safe_gop
                && crate::pci::is_pci_dev_present(crate::pci::PCI_OBJ_NET)
            {
                crate::pci::mint_root_pci_dev_cap(cs, lbl, crate::pci::PCI_OBJ_NET)?
            } else {
                CapPtr::NULL
            };
            let pci_igpu_cap = if session_mode >= 20
                && crate::pci::is_pci_dev_present(crate::pci::PCI_OBJ_IGPU)
            {
                crate::pci::mint_root_pci_dev_cap(cs, lbl, crate::pci::PCI_OBJ_IGPU)?
            } else {
                CapPtr::NULL
            };
            let mut boot_flags = 0u64;
            if cmdline.contains("live_usb=1") {
                boot_flags |= BOOT_FLAG_LIVE_USB;
            }
            if cmdline.contains("storage=ro") {
                boot_flags |= BOOT_FLAG_STORAGE_RO;
            }
            if cmdline.contains("recovery=1") {
                boot_flags |= BOOT_FLAG_RECOVERY;
            }
            if crate::serial::is_uart_hw_present() {
                boot_flags |= BOOT_FLAG_UART_PRESENT;
            }
            if crate::iommu::status() == crate::iommu::VtdStatus::Enforcing {
                boot_flags |= BOOT_FLAG_VTD_ENFORCING;
            }
            if crate::iommu::is_gfx_scanout_deferred() {
                boot_flags |= BOOT_FLAG_VTD_GFX_DEFERRED;
            }
            if safe_gop {
                boot_flags |= BOOT_FLAG_SAFE_GOP;
            }
            if cmdline.contains("usb_persist_prefer=1") {
                boot_flags |= BOOT_FLAG_USB_PERSIST_PREFER;
            }
            if cmdline.contains("diag_selftest=1") {
                boot_flags |= BOOT_FLAG_DIAG_SELFTEST;
            }
            let (dgpu_packed, vmd_packed, lpss_i2c_count) = crate::pci::pci_topology_summary();
            let boot_ext = RootInitBootExtInfo {
                magic: ROOT_BOOT_EXT_MAGIC,
                usb_mod_cap: usb_mod_cap.0,
                pci_xhci_cap: pci_xhci_cap.0,
                nvme_mod_cap: nvme_mod_cap.0,
                pci_nvme_cap: pci_nvme_cap.0,
                lx_net_mod_cap: lx_net_mod_cap.0,
                pci_net_cap: pci_net_cap.0,
                pci_igpu_cap: pci_igpu_cap.0,
                boot_flags,
                dgpu_packed,
                vmd_packed,
                lpss_i2c_count: lpss_i2c_count as u64,
            };
            let vault_ctrl_cap = if session_mode >= 20 {
                crate::vault_ctrl::mint_root_vault_cap(cs, lbl)?
            } else {
                CapPtr::NULL
            };

            let (info_fr_cap, info_fh) =
                cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0)?;
            let mod_count = latched_boot_module_count().min(MAX_LATCHED_MODULES);
            let (pci_recs, pci_func_count) = crate::pci::latched_boot_pci_funcs();
            let boot_info = P1InitBootInfo {
                magic: ROOT_INIT_BOOT_MAGIC,
                echo_mod_cap: echo_cap.0,
                display_mod_cap: disp_cap.0,
                fb_cap: fb_cap.0,
                input_mod_cap: input_cap.0,
                shell_mod_cap: shell_cap.0,
                irq1_cap: irq1_cap.0,
                i8042_port_cap: i8042_cap.0,
                session_mode,
                irq12_cap: irq12_cap.0,
                sysmon_mod_cap: sysmon_cap.0,
                vfs_mod_cap: vfs_cap.0,
                files_mod_cap: files_cap.0,
                settings_mod_cap: settings_cap.0,
                power_mod_cap: power_cap.0,
                power_ctrl_cap: pwr_ctrl_cap.0,
                boot_manifest_count: mod_count as u64,
                blk_mod_cap: blk_cap.0,
                vault_mod_cap: vault_cap.0,
                pci_blk_cap: pci_blk_cap.0,
                vault_ctrl_cap: vault_ctrl_cap.0,
                broker_mod_cap: broker_cap.0,
                mock_drv_mod_cap: mock_drv_cap.0,
                lx_rng_mod_cap: lx_rng_cap.0,
                pci_rng_cap: pci_rng_cap.0,
                gpu_mod_cap: gpu_mod_cap.0,
                pci_gpu_cap: pci_gpu_cap.0,
                editor_mod_cap: editor_mod_cap.0,
                calc_mod_cap: calc_mod_cap.0,
                lx_drm_mod_cap: lx_drm_mod_cap.0,
                pci_disp_aux_cap: pci_disp_aux_cap.0,
                boot_pci_func_count: pci_func_count as u64,
            };
            // SAFETY: `info_fh` is a freshly allocated, zeroed 4 KiB frame (`0x000..0x100` header,
            // `0x100..0x700` manifest, `0x800..0x1000` PCI function enumeration records).
            unsafe {
                copy_nonoverlapping(
                    (&raw const boot_info).cast::<u8>(),
                    info_fh.hhdm_ptr(),
                    size_of::<P1InitBootInfo>(),
                );
                let mut m_idx = 0usize;
                for i in 0..mod_count {
                    if let Some((raw_path, sz)) = latched_boot_module_meta(i) {
                        let base = raw_path
                            .iter()
                            .rposition(|&b| b == b'/')
                            .map_or(raw_path, |pos| &raw_path[pos + 1..]);
                        let mut name = [0u8; 40];
                        let nlen = base.len().min(39);
                        name[..nlen].copy_from_slice(&base[..nlen]);
                        let entry = BootModManifestEntry {
                            name,
                            size_bytes: sz as u64,
                        };
                        let dst = info_fh
                            .hhdm_ptr()
                            .add(BOOT_MANIFEST_OFFSET + m_idx * size_of::<BootModManifestEntry>());
                        copy_nonoverlapping(
                            (&raw const entry).cast::<u8>(),
                            dst,
                            size_of::<BootModManifestEntry>(),
                        );
                        m_idx += 1;
                    }
                }
                copy_nonoverlapping(
                    (&raw const boot_ext).cast::<u8>(),
                    info_fh.hhdm_ptr().add(BOOT_EXT_INFO_OFFSET),
                    size_of::<RootInitBootExtInfo>(),
                );
                if pci_func_count > 0 {
                    copy_nonoverlapping(
                        pci_recs.as_ptr().cast::<u8>(),
                        info_fh.hhdm_ptr().add(crate::pci::BOOT_PCI_ENUM_OFFSET),
                        pci_func_count * size_of::<crate::pci::BootPciFuncRecord>(),
                    );
                }
            }
            map_4k(vs_h, BOOT_INFO_VADDR, info_fh, VmProt::USER_RO)
                .map_err(|_| CapError::PagingError)?;
            attach_cap_mapping_record(
                cspace_lookup_fast(cs, lbl, info_fr_cap, CapRights::READ, 0)?.global_slot,
                info_fr_cap.slot_gen(),
                vs_h.id,
                vs_h.generation,
                BOOT_INFO_VADDR,
                false,
            )?;
            let _ = vs_cap;
            Ok((BOOT_INFO_VADDR, session_mode))
        },
    )
    .expect("Failed to spawn P1 dry-init.elf");

    serial_println!(
        "[DRY-P1-READY] cpus={} mode={} fb={} ioapic={} pci_blk={}",
        online_cpus,
        session_mode,
        is_framebuffer_available(),
        ioapic_present(),
        crate::pci::is_pci_blk_present()
    );

    crate::sched::set_desktop_idle_hlt(session_mode >= 10);
    if online_cpus > 1 {
        set_ap_epoch_kind(ApEpochKind::PreemptiveSched);
        AP_PHASE_EPOCH.fetch_add(1, Ordering::AcqRel);
    }
    let bsp_pcpu = get_percpu_by_idx(0);
    start_cpu_timer_for_phase(bsp_pcpu);
    sti();
    while PHASE_PENDING_THREADS.load(Ordering::Acquire) > 0 {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        if !idle_dispatch_once(0) {
            crate::sched::idle_park_or_spin(0);
        }
    }
    stop_cpu_timer_for_phase(bsp_pcpu);
    cli();
    let (hlt_calls, hlt_tsc) = crate::sched::idle_hlt_telemetry(online_cpus);
    crate::sched::set_desktop_idle_hlt(false);
    set_preemptive_mode_active(false);
    set_lease_epoch(1);

    let ReclaimStatus::ReclaimedNow(outcome) =
        reclaim_process(init_proc.handle).expect("Failed to reclaim P1 dry-init")
    else {
        panic!("Expected ReclaimedNow for P1 dry-init");
    };
    assert_eq!(
        outcome,
        ProcessOutcome::Exited(0),
        "P1 dry-init exited with unexpected outcome {:?}",
        outcome
    );

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(
        leaked, 0,
        "Frame leak after P1 interactive session: {}",
        leaked
    );

    let (ext_irqs, irq_acks, _, _) = irq_telemetry();
    let (io_ops, io_denied) = crate::irq::ioport::ioport_telemetry();
    let (dma_denied, pci_quiesce) = crate::pci::pci_dma_telemetry();
    if session_mode >= 10 {
        serial_println!(
            "[DRY-IDLE-HLT] PASS: cpus={} hlt_calls={} hlt_tsc={} pwr_denied={} dma_denied={} pci_quiesce={} vault_denied={}",
            online_cpus,
            hlt_calls,
            hlt_tsc,
            crate::power::power_denied_count(),
            dma_denied,
            pci_quiesce,
            crate::vault_ctrl::vault_denied_count(),
        );
    }
    if session_mode >= 2 {
        serial_println!(
            "[DRY-P2-SESSION] PASS: cpus={} mode={} irq1={} irq12={} ext_irqs={} irq_acks={} ioport_ops={} ioport_denied={} leaked_frames={}",
            online_cpus,
            session_mode,
            irq_line_delivered(IRQ_KEYBOARD_PS2),
            irq_line_delivered(IRQ_MOUSE_PS2),
            ext_irqs,
            irq_acks,
            io_ops,
            io_denied,
            leaked
        );
    }
    serial_println!(
        "[DRY-P1-SESSION] PASS: cpus={} ext_irqs={} irq_acks={} ioport_ops={} ioport_denied={} leaked_frames={}",
        online_cpus,
        ext_irqs,
        irq_acks,
        io_ops,
        io_denied,
        leaked
    );
}
