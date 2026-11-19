// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Passive ACPI, UEFI GOP, PCIe read-only topology, and VT-d/IOMMU diagnostics.

use crate::abi_ext::{
    ACPI_AUDIT_CHECKSUM_FAIL, ACPI_AUDIT_DMAR_OK, ACPI_AUDIT_FACP_OK, ACPI_AUDIT_IVRS_FOUND,
    ACPI_AUDIT_MADT_OK, ACPI_AUDIT_MALFORMED_TABLE, ACPI_AUDIT_MCFG_OK, ACPI_AUDIT_RSDP_OK,
    HWVAL_FLAG_TEST_NO_IOMMU_MODE, IOMMU_AUDIT_DMAR_PRESENT, IOMMU_AUDIT_FAIL_CLOSED,
    IOMMU_AUDIT_HW_ENFORCING, IOMMU_AUDIT_INT_REMAP, IOMMU_AUDIT_IVRS_PRESENT,
};
use crate::hwdiag::{
    DiagContext, DiagSessionReport, DiagTestRecord, CAT_ACPI, CAT_IOMMU, CAT_PCI, SCOPE_ACPI,
    SCOPE_IOMMU, SCOPE_PCI, STATUS_BLOCKED, STATUS_FAIL, STATUS_PASS, STATUS_SKIP,
};
use crate::shell::LineBuilder;
use crate::{
    rdtsc_user, PCI_BOOT_EXTRA_BAD_BAR, PCI_BOOT_EXTRA_PASSIVE, PCI_DEV_FLAG_MSI,
    PCI_DEV_FLAG_MSIX, PCI_DEV_FLAG_PCIE,
};

fn tsc_us(t0: u64, t1: u64, mhz: u64) -> u32 {
    (t1.saturating_sub(t0) / mhz.max(1000)).max(1) as u32
}

pub fn run(ctx: &DiagContext, scope: u32, _safe: bool, out: &mut DiagSessionReport) {
    let mhz = ctx.tsc_mhz.max(1000);
    let af = ctx.ext.acpi_audit_flags;

    if (scope & SCOPE_ACPI) != 0 {
        let t0 = rdtsc_user();
        let bad_acpi = (af & (ACPI_AUDIT_CHECKSUM_FAIL | ACPI_AUDIT_MALFORMED_TABLE)) != 0;
        let rsdp_ok = (af & ACPI_AUDIT_RSDP_OK) != 0;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"flags=0x");
        lb.push_hex_width(af as u64, 3);
        lb.push_bytes(b" madt=");
        lb.push_u64(u64::from((af & ACPI_AUDIT_MADT_OK) != 0));
        lb.push_bytes(b" mcfg=");
        lb.push_u64(u64::from((af & ACPI_AUDIT_MCFG_OK) != 0));
        let ok = rsdp_ok && !bad_acpi && (af & ACPI_AUDIT_MADT_OK) != 0 && (af & ACPI_AUDIT_FACP_OK) != 0;
        out.push(DiagTestRecord::make(
            CAT_ACPI,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"ACPI-01-TABLES-CRC",
            lb.as_bytes(),
            b"rsdp+madt+facp+crc_ok",
            if ok { b"none" } else { b"inspect_firmware_acpi" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -74 },
        ));

        // UEFI-01: GOP Display handover verification
        let t0 = rdtsc_user();
        let gop_ok = ctx.ext.fb_width > 0
            && ctx.ext.fb_height > 0
            && ctx.ext.fb_pitch >= (ctx.ext.fb_width as u32) * 4
            && ctx.ext.fb_phys_base != 0;
        let mut lb = LineBuilder::new();
        lb.push_u64(ctx.ext.fb_width as u64);
        lb.push_byte(b'x');
        lb.push_u64(ctx.ext.fb_height as u64);
        lb.push_bytes(b" pitch=");
        lb.push_u64(ctx.ext.fb_pitch as u64);
        out.push(DiagTestRecord::make(
            CAT_ACPI,
            if gop_ok { STATUS_PASS } else { STATUS_FAIL },
            b"UEFI-01-GOP-FB",
            lb.as_bytes(),
            b"gop_linear_fb_valid",
            if gop_ok { b"none" } else { b"check_uefi_gop_mode" },
            tsc_us(t0, rdtsc_user(), mhz),
            if gop_ok { 0 } else { -19 },
        ));

        // UEFI-02: Secure Boot read-only safe check (post-ExitBootServices NVRAM untouched)
        let t0 = rdtsc_user();
        let (sb_status, sb_obs) = match ctx.ext.secure_boot_state {
            2 => (STATUS_PASS, &b"enabled"[..]),
            1 => (STATUS_PASS, &b"disabled"[..]),
            _ => (STATUS_SKIP, &b"post_ebs_nvram_untouched"[..]),
        };
        out.push(DiagTestRecord::make(
            CAT_ACPI,
            sb_status,
            b"UEFI-02-SECUREBOOT",
            sb_obs,
            b"ro_or_skip_no_nvram_wr",
            b"check_bios_setup_if_req",
            tsc_us(t0, rdtsc_user(), mhz),
            0,
        ));
    }

    if (scope & SCOPE_PCI) != 0 {
        let t0 = rdtsc_user();
        let recs = &ctx.pci_recs[..ctx.pci_count];
        let bad_bar = recs.iter().any(|r| (r.extra_flags & PCI_BOOT_EXTRA_BAD_BAR) != 0);
        let passive_cnt = recs.iter().filter(|r| (r.extra_flags & PCI_BOOT_EXTRA_PASSIVE) != 0).count();
        let ok = ctx.pci_count > 0 && !bad_bar;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"funcs=");
        lb.push_u64(ctx.pci_count as u64);
        lb.push_bytes(b" passive=");
        lb.push_u64(passive_cnt as u64);
        lb.push_bytes(b" bad_bar=");
        lb.push_u64(u64::from(bad_bar));
        out.push(DiagTestRecord::make(
            CAT_PCI,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"PCI-01-ENUM-BARS",
            lb.as_bytes(),
            b"funcs>0 bad_bar=0",
            if ok { b"none" } else { b"bad_pci_bar_detected" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -22 },
        ));

        // PCI-02: Read-only MSI/MSI-X/PCIe capability discovery + Intel VMD detection
        let t0 = rdtsc_user();
        let msi_cnt = recs
            .iter()
            .filter(|r| (r.dev_flags & (PCI_DEV_FLAG_MSI as u8 | PCI_DEV_FLAG_MSIX as u8)) != 0)
            .count();
        let pcie_cnt = recs.iter().filter(|r| (r.dev_flags & (PCI_DEV_FLAG_PCIE as u8)) != 0).count();
        let vmd_present = ctx.ext.vmd_packed != 0;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"msi_devs=");
        lb.push_u64(msi_cnt as u64);
        lb.push_bytes(b" pcie=");
        lb.push_u64(pcie_cnt as u64);
        lb.push_bytes(b" vmd=");
        lb.push_u64(u64::from(vmd_present));
        out.push(DiagTestRecord::make(
            CAT_PCI,
            STATUS_PASS,
            b"PCI-02-CAPS-VMD",
            lb.as_bytes(),
            b"ro_caps_enumerated",
            b"none",
            tsc_us(t0, rdtsc_user(), mhz),
            0,
        ));
    }

    if (scope & SCOPE_IOMMU) != 0 {
        let t0 = rdtsc_user();
        let im = ctx.ext.iommu_audit_flags;
        let dmar = (im & IOMMU_AUDIT_DMAR_PRESENT) != 0 || (af & ACPI_AUDIT_DMAR_OK) != 0;
        let ivrs = (im & IOMMU_AUDIT_IVRS_PRESENT) != 0 || (af & ACPI_AUDIT_IVRS_FOUND) != 0;
        let hw_enf = (im & IOMMU_AUDIT_HW_ENFORCING) != 0;
        let fail_cl = (im & IOMMU_AUDIT_FAIL_CLOSED) != 0;
        let ir = (im & IOMMU_AUDIT_INT_REMAP) != 0;
        let drhds = (im >> 8) & 0xFF;
        let rmrrs = (im >> 16) & 0xFF;
        let test_no_iommu = (ctx.ext.hwval_flags & HWVAL_FLAG_TEST_NO_IOMMU_MODE) != 0;

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"vtd=");
        lb.push_u64(u64::from(hw_enf));
        lb.push_bytes(b" dmar=");
        lb.push_u64(u64::from(dmar));
        lb.push_bytes(b" drhd=");
        lb.push_u64(drhds as u64);
        lb.push_bytes(b" rmrr=");
        lb.push_u64(rmrrs as u64);

        let (st, err, recov) = if hw_enf && dmar && !test_no_iommu {
            (STATUS_PASS, 0, &b"none"[..])
        } else if test_no_iommu || fail_cl || !dmar {
            (STATUS_BLOCKED, -1, &b"enable_vtd_in_bios_uefi"[..])
        } else {
            (STATUS_FAIL, -5, &b"check_dmar_slpt"[..])
        };
        out.push(DiagTestRecord::make(
            CAT_IOMMU,
            st,
            b"IOMMU-01-VTD-DMA",
            lb.as_bytes(),
            b"vtd_enforcing=1 dmar=1",
            recov,
            tsc_us(t0, rdtsc_user(), mhz),
            err,
        ));

        let t0 = rdtsc_user();
        let mut ilb = LineBuilder::new();
        ilb.push_bytes(b"int_remap=");
        ilb.push_u64(u64::from(ir));
        ilb.push_bytes(b" ivrs=");
        ilb.push_u64(u64::from(ivrs));
        ilb.push_bytes(b" aw=");
        ilb.push_u64(((im >> 24) & 0xFF) as u64);
        out.push(DiagTestRecord::make(
            CAT_IOMMU,
            if ir { STATUS_PASS } else { STATUS_BLOCKED },
            b"IOMMU-02-INTREMAP",
            ilb.as_bytes(),
            b"vtd_ir=1",
            if ir { b"none" } else { b"ir_req_for_microvm_vfio" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ir { 0 } else { -38 },
        ));
    }
}
