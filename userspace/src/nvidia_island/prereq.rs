// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Fail-closed 14-prerequisite evaluator for Mandatory NVIDIA Driver Island (`Level 1`).

use crate::abi_ext::{
    RootInitBootExtInfo, HWCHECK_MODE_NVIDIA_EXP, HWVAL_FLAG_NVIDIA_EXP,
    HWVAL_FLAG_TEST_NV_BAD_BAR, HWVAL_FLAG_TEST_NV_CORRUPT_IPC, HWVAL_FLAG_TEST_NV_NO_GPU,
    HWVAL_FLAG_TEST_NV_TIMEOUT, HWVAL_FLAG_TEST_NV_UNAUTH_CAP, HWVAL_FLAG_TEST_NV_VM_CRASH,
    IOMMU_AUDIT_DMAR_PRESENT, IOMMU_AUDIT_HW_ENFORCING, IOMMU_AUDIT_INT_REMAP,
};
use crate::hwdiag::checks_gpu_storage::{find_intel_gpu, find_nvidia_gpu};
use crate::nvidia_island::protocol::{
    NvIslandStatusWire, NvPrereqWire, NV_LEVEL_1_READY, NV_PREREQ_COUNT, NV_STATUS_BLOCKED,
};
use crate::{BootPciFuncRecord, PCI_BOOT_EXTRA_BAD_BAR};

/// `read_cpuid_features_u64()` bit 6 (`CPUID.1:ECX[5]`, VMX) in `kernel/src/irq/session.rs`.
const CPUID_FEAT_VMX: u64 = 1 << 6;

#[derive(Clone, Copy, Default)]
pub struct NvRuntimeProbe {
    pub spawned: bool,
    pub pid: u32,
    pub generation: u32,
    pub mac_isolated: bool,
    pub island_alive: bool,
    pub vm_restarts: u8,
    pub corrupt_ipc_rejected: bool,
    pub unauth_cap_rejected: bool,
    pub timeout_handled: bool,
    pub export_ok: bool,
    pub export_files: u8,
    pub last_ipc_err: i64,
}

pub fn evaluate_nvidia_island(
    ext: &RootInitBootExtInfo,
    pci_recs: &[BootPciFuncRecord],
    session_id: &[u8],
    rt: &NvRuntimeProbe,
    prereqs_out: &mut [NvPrereqWire; NV_PREREQ_COUNT],
) -> NvIslandStatusWire {
    let flags = ext.hwval_flags;
    let nv_opt = find_nvidia_gpu(pci_recs, flags);
    let igpu_opt = find_intel_gpu(pci_recs);
    let has_vgpu = pci_recs.iter().any(|r| r.vendor_id == 0x1af4 && r.device_id == 0x1050);

    let pci_present = nv_opt.is_some();
    let bad_bar_inj = (flags & HWVAL_FLAG_TEST_NV_BAD_BAR) != 0;
    let bar_ok = nv_opt.is_some_and(|r| {
        !bad_bar_inj && (r.extra_flags & PCI_BOOT_EXTRA_BAD_BAR) == 0 && r.bar0_size_bytes > 0
    });
    let dmar_ok = (ext.iommu_audit_flags & (IOMMU_AUDIT_DMAR_PRESENT | IOMMU_AUDIT_HW_ENFORCING))
        == (IOMMU_AUDIT_DMAR_PRESENT | IOMMU_AUDIT_HW_ENFORCING);
    let ir_ok = (ext.iommu_audit_flags & IOMMU_AUDIT_INT_REMAP) != 0;
    let vmx_ok = (ext.cpuid_features & CPUID_FEAT_VMX) != 0;
    let exp_optin = ext.hwcheck_mode == HWCHECK_MODE_NVIDIA_EXP || (flags & HWVAL_FLAG_NVIDIA_EXP) != 0;
    let mac_ok = rt.spawned && rt.mac_isolated && rt.island_alive;

    prereqs_out[0] = NvPrereqWire::make(
        1,
        pci_present,
        false,
        if pci_present { 0 } else { -19 },
        b"pci_device_10de",
        if pci_present { b"10de_gpu_detected" } else { b"BLOCKED_no_10de_pci_device" },
    );
    prereqs_out[1] = NvPrereqWire::make(
        2,
        bar_ok,
        bad_bar_inj,
        if bar_ok { 0 } else { -22 },
        b"pci_bar_layout",
        if bad_bar_inj {
            b"FAIL_injected_bad_bar"
        } else if bar_ok {
            b"bar0_non_zero_passive"
        } else {
            b"BLOCKED_missing_or_bad_bar"
        },
    );
    prereqs_out[2] = NvPrereqWire::make(
        3,
        dmar_ok,
        false,
        if dmar_ok { 0 } else { -1 },
        b"iommu_dmar_enforcing",
        if dmar_ok { b"vtd_dmar_slpt_active" } else { b"BLOCKED_no_hw_iommu" },
    );
    prereqs_out[3] = NvPrereqWire::make(
        4,
        ir_ok,
        false,
        if ir_ok { 0 } else { -38 },
        b"iommu_int_remap",
        if ir_ok { b"vtd_ir_active" } else { b"BLOCKED_vtd_ir_unimplemented" },
    );
    prereqs_out[4] = NvPrereqWire::make(
        5, false, false, -38, b"vfio_pci_assign", b"BLOCKED_no_vfio_type1_group",
    );
    prereqs_out[5] = NvPrereqWire::make(
        6,
        vmx_ok,
        false,
        if vmx_ok { 0 } else { -38 },
        b"cpuid_vmx_cap",
        if vmx_ok { b"cpuid_leaf1_ecx5_present" } else { b"BLOCKED_no_cpuid_vmx" },
    );
    prereqs_out[6] = NvPrereqWire::make(
        7, false, false, -38, b"vtx_hypervisor", b"BLOCKED_no_ring0_vmcs_vmm",
    );
    prereqs_out[7] = NvPrereqWire::make(
        8, false, false, -2, b"microvm_guest_img", b"BLOCKED_no_guest_kernel_rootfs",
    );
    prereqs_out[8] = NvPrereqWire::make(
        9, false, false, -2, b"gsp_fw_ucodes", b"BLOCKED_no_ucodes_ga10x_wpr2",
    );
    prereqs_out[9] = NvPrereqWire::make(
        10, false, false, -38, b"spdm_cert_verify", b"BLOCKED_no_libspdm_attestation",
    );
    prereqs_out[10] = NvPrereqWire::make(
        11, false, false, -38, b"openrm_os_shim", b"BLOCKED_149_of_171_unsupported",
    );
    prereqs_out[11] = NvPrereqWire::make(
        12, false, false, -38, b"prime_shm_fence", b"BLOCKED_no_ro_export_bridge",
    );
    prereqs_out[12] = NvPrereqWire::make(
        13,
        mac_ok,
        (flags & HWVAL_FLAG_TEST_NV_VM_CRASH) != 0 && !rt.island_alive,
        if mac_ok { 0 } else { -1 },
        b"mac_island_confin",
        if mac_ok { b"userspace_domain_isolated" } else { b"BLOCKED_island_not_spawned" },
    );
    prereqs_out[13] = NvPrereqWire::make(
        14,
        exp_optin,
        false,
        if exp_optin { 0 } else { -1 },
        b"nvidia_exp_optin",
        if exp_optin { b"explicit_nvidia_exp_boot" } else { b"BLOCKED_safe_profile_gated" },
    );

    let mut met_count = 0u8;
    for p in prereqs_out.iter() {
        if p.met != 0 {
            met_count += 1;
        }
    }

    let mut st = NvIslandStatusWire::empty();
    st.level = NV_LEVEL_1_READY;
    st.overall_status = NV_STATUS_BLOCKED;
    st.spawned = u8::from(rt.spawned);
    st.mac_isolated = u8::from(rt.mac_isolated);
    st.island_alive = u8::from(rt.island_alive);
    st.pci_present = u8::from(pci_present);
    st.bar_layout_ok = u8::from(bar_ok);
    st.met_count = met_count;
    st.blocked_count = (NV_PREREQ_COUNT as u8).saturating_sub(met_count);
    st.vm_restarts = rt.vm_restarts;
    st.corrupt_ipc_rejected = u8::from(rt.corrupt_ipc_rejected);
    st.unauth_cap_rejected = u8::from(rt.unauth_cap_rejected);
    st.timeout_handled = u8::from(rt.timeout_handled);
    st.export_ok = u8::from(rt.export_ok);
    st.export_files = rt.export_files;
    st.nvidia_exp_optin = u8::from(exp_optin);
    if let Some(nv) = nv_opt {
        st.pci_vid = nv.vendor_id;
        st.pci_did = nv.device_id;
        st.pci_bdf = nv.bus_dev_fn;
        st.pci_bar0_size = nv.bar0_size_bytes;
    }
    if let Some(ig) = igpu_opt {
        st.igpu_vid = ig.vendor_id;
        st.igpu_did = ig.device_id;
    }
    st.vgpu_present = u8::from(has_vgpu);
    st.gop_ok = u8::from(ext.fb_width > 0 && ext.fb_phys_base != 0);
    st.pid = rt.pid;
    st.generation = rt.generation;
    st.hwval_flags = flags;
    st.last_ipc_err = rt.last_ipc_err;

    let sn = session_id.len().min(st.session_id.len());
    st.session_id[..sn].copy_from_slice(&session_id[..sn]);

    if (flags & HWVAL_FLAG_TEST_NV_NO_GPU) != 0 {
        st.set_reason(b"no_10de_gpu_level1_blocked");
    } else if bad_bar_inj {
        st.set_reason(b"bad_pci_bar_injected_blocked");
    } else if !pci_present {
        st.set_reason(b"no_10de_gpu_level1_blocked");
    } else if !exp_optin {
        st.set_reason(b"nvidia_exp_optin_required_safe_mode");
    } else if (flags & HWVAL_FLAG_TEST_NV_VM_CRASH) != 0 {
        st.set_reason(b"vm_crash_reaped_level1_blocked");
    } else if (flags & HWVAL_FLAG_TEST_NV_TIMEOUT) != 0 {
        st.set_reason(b"ipc_timeout_handled_level1_blocked");
    } else if (flags & HWVAL_FLAG_TEST_NV_CORRUPT_IPC) != 0 {
        st.set_reason(b"corrupt_ipc_rejected_level1_blocked");
    } else if (flags & HWVAL_FLAG_TEST_NV_UNAUTH_CAP) != 0 {
        st.set_reason(b"unauth_cap_rejected_level1_blocked");
    } else {
        st.set_reason(b"needs_vtx_ir_vfio_openrm_gsp");
    }
    st
}
