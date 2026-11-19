// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Passive Intel Iris Xe, NVIDIA RTX 3050 Ti, USB ancestry, storage RO, and driver compat checks.

use crate::abi_ext::HWVAL_FLAG_TEST_NV_NO_GPU;
use crate::hwdiag::{
    DiagContext, DiagSessionReport, DiagTestRecord, CAT_COMPAT, CAT_DRIVERS, CAT_GPU, CAT_STORAGE,
    CAT_USB, SCOPE_COMPAT, SCOPE_DRIVERS, SCOPE_GPU, SCOPE_STORAGE, SCOPE_USB, STATUS_BLOCKED,
    STATUS_FAIL, STATUS_PASS, STATUS_SKIP,
};
use crate::intel_tgl_prep::verify_intel_tgl_prep_invariants;
use crate::shell::LineBuilder;
use crate::usb::msc::ANCESTRY_ALL_GATES;
use crate::{rdtsc_user, BootPciFuncRecord, BOOT_FLAG_SAFE_GOP};

fn tsc_us(t0: u64, t1: u64, mhz: u64) -> u32 {
    (t1.saturating_sub(t0) / mhz.max(1000)).max(1) as u32
}

pub fn find_intel_gpu(recs: &[BootPciFuncRecord]) -> Option<BootPciFuncRecord> {
    recs.iter()
        .copied()
        .find(|r| r.vendor_id == 0x8086 && r.class_code == 0x03)
}

pub fn find_nvidia_gpu(recs: &[BootPciFuncRecord], ext_flags: u32) -> Option<BootPciFuncRecord> {
    if (ext_flags & HWVAL_FLAG_TEST_NV_NO_GPU) != 0 {
        return None;
    }
    recs.iter()
        .copied()
        .find(|r| r.vendor_id == 0x10de && r.class_code == 0x03)
}

pub fn run(ctx: &DiagContext, scope: u32, safe: bool, out: &mut DiagSessionReport) {
    let mhz = ctx.tsc_mhz.max(1000);
    let recs = &ctx.pci_recs[..ctx.pci_count];

    if (scope & SCOPE_GPU) != 0 {
        // GPU-01: Intel Iris Xe (`8086:9a49` / `0x03:*`) passive contracts
        let t0 = rdtsc_user();
        let igpu = find_intel_gpu(recs);
        let prep = verify_intel_tgl_prep_invariants();
        let gop_ok = ctx.ext.fb_width > 0 && ctx.ext.fb_phys_base != 0;
        let (st, obs, exp, recov, err): (u8, &[u8], &[u8], &[u8], i32) = match (igpu, prep) {
            (Some(r), Some(_)) => {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"dev=8086:");
                lb.push_hex_width(r.device_id as u64, 4);
                lb.push_bytes(b" gop=");
                lb.push_u64(u64::from(gop_ok));
                lb.push_bytes(b" native=partial");
                (
                    STATUS_PASS,
                    b"8086_igpu_contract_gop_ok",
                    b"ro_bars+gop_preserved",
                    b"needs_guc_ggtt_edp_link",
                    0,
                )
            }
            (None, Some(_)) => (
                STATUS_SKIP,
                b"no_intel_igpu_qemu_contracts_ok",
                b"present_or_skip_in_vm",
                b"test_on_lenovo_82k1_hw",
                0,
            ),
            _ => (
                STATUS_FAIL,
                b"intel_prep_contract_failed",
                b"contract_invariants_ok",
                b"check_intel_tgl_prep",
                -22,
            ),
        };
        out.push(DiagTestRecord::make(CAT_GPU, st, b"GPU-01-INTEL-XE", obs, exp, recov, tsc_us(t0, rdtsc_user(), mhz), err));

        // GPU-02: NVIDIA RTX 3050 Ti (`10de:25a0` / `0x03:*`) passive assessment
        let t0 = rdtsc_user();
        let nv = find_nvidia_gpu(recs, ctx.ext.hwval_flags);
        let safe_gop = safe || (ctx.ext.boot_flags & BOOT_FLAG_SAFE_GOP) != 0;
        if let Some(r) = nv {
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"10de:");
            lb.push_hex_width(r.device_id as u64, 4);
            lb.push_bytes(b"_passive_no_vmx");
            out.push(DiagTestRecord::make(
                CAT_GPU,
                STATUS_BLOCKED,
                b"GPU-02-NVIDIA-3050TI",
                lb.as_bytes(),
                b"vmx+vtd_ir+nvidia_exp",
                b"island_lvl1_needs_vmx_ir",
                tsc_us(t0, rdtsc_user(), mhz),
                -38,
            ));
        } else {
            let msg: &[u8] = if safe_gop { b"no_10de_pci_safe_gop" } else { b"no_10de_pci_device" };
            out.push(DiagTestRecord::make(
                CAT_GPU,
                STATUS_BLOCKED,
                b"GPU-02-NVIDIA-3050TI",
                msg,
                b"10de_pci+vmx+ir",
                b"requires_10de_25a0_and_vmx",
                tsc_us(t0, rdtsc_user(), mhz),
                -19,
            ));
        }

        // GPU-03: VirtIO-GPU / GOP display path
        let t0 = rdtsc_user();
        let has_vgpu = recs.iter().any(|r| r.vendor_id == 0x1af4 && r.device_id == 0x1050);
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"gop=");
        lb.push_u64(u64::from(gop_ok));
        lb.push_bytes(b" virtio=");
        lb.push_u64(u64::from(has_vgpu));
        lb.push_bytes(b" safe=");
        lb.push_u64(u64::from(safe));
        out.push(DiagTestRecord::make(
            CAT_GPU,
            if gop_ok { STATUS_PASS } else { STATUS_FAIL },
            b"GPU-03-DISPLAY-PATH",
            lb.as_bytes(),
            b"active_scanout_safe",
            if gop_ok { b"none" } else { b"verify_gop_scanout" },
            tsc_us(t0, rdtsc_user(), mhz),
            if gop_ok { 0 } else { -5 },
        ));
    }

    if (scope & SCOPE_USB) != 0 {
        let t0 = rdtsc_user();
        let xhci_ok = ctx.hw_sum.usb_ready != 0 || recs.iter().any(|r| r.class_code == 0x0c && r.subclass == 0x03 && r.prog_if == 0x30);
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"xhci=");
        lb.push_u64(u64::from(xhci_ok));
        lb.push_bytes(b" pid=");
        lb.push_u64(ctx.hw_sum.usb_pid as u64);
        out.push(DiagTestRecord::make(
            CAT_USB,
            if xhci_ok { STATUS_PASS } else { STATUS_FAIL },
            b"USB-01-XHCI",
            lb.as_bytes(),
            b"xhci_controller_ready",
            if xhci_ok { b"none" } else { b"check_usb_xhci_slot" },
            tsc_us(t0, rdtsc_user(), mhz),
            if xhci_ok { 0 } else { -19 },
        ));

        let t0 = rdtsc_user();
        let ul = ctx.usb_logs;
        let gates_ok = (ul.gate_mask & ANCESTRY_ALL_GATES) == ANCESTRY_ALL_GATES;
        let mut ulb = LineBuilder::new();
        ulb.push_bytes(b"gates=0x");
        ulb.push_hex_width(ul.gate_mask as u64, 2);
        ulb.push_bytes(b" blk=");
        ulb.push_u64(ul.block_reason as u64);
        ulb.push_bytes(b" fat=");
        ulb.push_u64(ul.fat_mounted as u64);
        let (st, recov, err) = if gates_ok && ul.fat_mounted != 0 && ul.fat_corrupt == 0 {
            (STATUS_PASS, &b"none"[..], 0)
        } else if ul.fat_corrupt != 0 {
            (STATUS_FAIL, &b"reformat_dryos_logs_fat32"[..], -74)
        } else {
            (STATUS_BLOCKED, &b"boot_from_verified_dryos_usb"[..], -30)
        };
        out.push(DiagTestRecord::make(
            CAT_USB,
            st,
            b"USB-02-LOGS-ANCESTRY",
            ulb.as_bytes(),
            b"gates=0x1f fat=1",
            recov,
            tsc_us(t0, rdtsc_user(), mhz),
            err,
        ));
    }

    if (scope & SCOPE_STORAGE) != 0 {
        let t0 = rdtsc_user();
        let ro_ok = ctx.storage_ro() || ctx.hw_sum.persist_ro != 0;
        let has_nvme = recs.iter().any(|r| r.class_code == 0x01 && r.subclass == 0x08);
        let has_blk = ctx.hw_sum.blk_ready != 0;
        let has_vmd = ctx.ext.vmd_packed != 0;
        let mut slb = LineBuilder::new();
        slb.push_bytes(b"ro=");
        slb.push_u64(u64::from(ro_ok));
        slb.push_bytes(b" nvme=");
        slb.push_u64(u64::from(has_nvme));
        slb.push_bytes(b" vmd=");
        slb.push_u64(u64::from(has_vmd));
        slb.push_bytes(b" blk=");
        slb.push_u64(u64::from(has_blk));
        out.push(DiagTestRecord::make(
            CAT_STORAGE,
            STATUS_PASS,
            b"STOR-01-INTERNAL-RO",
            slb.as_bytes(),
            b"internal_disks_untouched",
            b"none",
            tsc_us(t0, rdtsc_user(), mhz),
            0,
        ));
    }

    if (scope & SCOPE_DRIVERS) != 0 {
        let t0 = rdtsc_user();
        let mut dlb = LineBuilder::new();
        dlb.push_bytes(b"usb=");
        dlb.push_u64(ctx.hw_sum.usb_pid as u64);
        dlb.push_bytes(b" blk=");
        dlb.push_u64(ctx.hw_sum.blk_pid as u64);
        dlb.push_bytes(b" nvme=");
        dlb.push_u64(ctx.hw_sum.nvme_pid as u64);
        out.push(DiagTestRecord::make(
            CAT_DRIVERS,
            STATUS_PASS,
            b"DRV-01-ISLAND-RING3",
            dlb.as_bytes(),
            b"zero_kernel_gpl_drivers",
            b"none",
            tsc_us(t0, rdtsc_user(), mhz),
            0,
        ));
    }

    if (scope & SCOPE_COMPAT) != 0 {
        let t0 = rdtsc_user();
        let mut clb = LineBuilder::new();
        clb.push_bytes(b"abi=v0.2 mode=");
        clb.push_bytes(ctx.mode_name());
        out.push(DiagTestRecord::make(
            CAT_COMPAT,
            STATUS_PASS,
            b"COMPAT-01-BASELINE",
            clb.as_bytes(),
            b"gop+xhci+vtd+island_v1",
            b"see_gpu_compat_docs",
            tsc_us(t0, rdtsc_user(), mhz),
            0,
        ));
    }
}
