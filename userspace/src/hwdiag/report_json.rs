// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Generates all 13 structured hardware validation artifacts + `manifest.sha256`
//! cleanly in a reusable RAM scratch buffer with default serial/UUID redaction.

use crate::abi_ext::sys_dmesg_read;
use crate::hwdiag::checks_gpu_storage::{find_intel_gpu, find_nvidia_gpu};
use crate::hwdiag::{
    category_slug, persist_slug, redact_guid_hex, status_slug, DiagContext, DiagSessionReport,
};
use crate::vault_crypto::Sha256;
use crate::{UserSysInfo, PCI_BOOT_EXTRA_BUS_MASTER, PCI_BOOT_EXTRA_PASSIVE, PCI_DEV_FLAG_MSI, PCI_DEV_FLAG_MSIX};

pub struct JsonBuf<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> JsonBuf<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    pub fn push(&mut self, s: &[u8]) {
        let take = s.len().min(self.buf.len().saturating_sub(self.len));
        self.buf[self.len..self.len + take].copy_from_slice(&s[..take]);
        self.len += take;
    }
    pub fn push_str(&mut self, s: &[u8]) {
        self.push(b"\"");
        for &b in s {
            match b {
                b'"' => self.push(b"\\\""),
                b'\\' => self.push(b"\\\\"),
                b'\n' => self.push(b"\\n"),
                b'\r' => self.push(b"\\r"),
                0x20..=0x7E => self.push(&[b]),
                _ => self.push(b"?"),
            }
        }
        self.push(b"\"");
    }
    pub fn push_u64(&mut self, mut n: u64) {
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        if n == 0 {
            self.push(b"0");
            return;
        }
        while n > 0 {
            i -= 1;
            tmp[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        self.push(&tmp[i..]);
    }
    pub fn push_i64(&mut self, n: i64) {
        if n < 0 {
            self.push(b"-");
            self.push_u64(n.wrapping_neg() as u64);
        } else {
            self.push_u64(n as u64);
        }
    }
    pub fn push_hex(&mut self, val: u64, width: usize) {
        const H: &[u8; 16] = b"0123456789abcdef";
        self.push(b"\"0x");
        for i in (0..width.min(16)).rev() {
            self.push(&[H[((val >> (i * 4)) & 0xf) as usize]]);
        }
        self.push(b"\"");
    }
    pub fn push_bool(&mut self, v: bool) {
        self.push(if v { b"true" } else { b"false" });
    }
}

pub const REPORT_FILENAMES: [&[u8]; 12] = [
    b"summary.json",
    b"hardware_inventory.json",
    b"cpu_memory.json",
    b"acpi_uefi.json",
    b"pci_topology.json",
    b"iommu_dma.json",
    b"intel_gpu.json",
    b"nvidia_gpu.json",
    b"usb_storage.json",
    b"driver_compatibility.json",
    b"kernel.log",
    b"test_results.json",
];

pub fn render_file<'a>(
    idx: usize,
    ctx: &DiagContext,
    rep: &DiagSessionReport,
    scratch: &'a mut [u8],
) -> &'a [u8] {
    if idx == 10 {
        let mut info = UserSysInfo::default();
        let got = sys_dmesg_read(&mut info, scratch).unwrap_or(0).min(scratch.len());
        if got > 0 {
            return &scratch[..got];
        }
        let msg = b"[DRY-HWDIAG] dmesg_empty\n";
        let n = msg.len().min(scratch.len());
        scratch[..n].copy_from_slice(&msg[..n]);
        return &scratch[..n];
    }
    let mut j = JsonBuf::new(scratch);
    let mut guid_redacted = [0u8; 20];
    let guid_rlen = redact_guid_hex(&ctx.ext.boot_disk_guid, &mut guid_redacted);
    let git = option_env!("DRYOS_GIT_COMMIT").unwrap_or("ascension-8.0").as_bytes();
    let kver = option_env!("DRYOS_KERNEL_VERSION").unwrap_or("0.8.0-ascension8").as_bytes();

    match idx {
        0 => {
            // summary.json
            j.push(b"{\n  \"kernel_version\": ");
            j.push_str(kver);
            j.push(b",\n  \"git_commit\": ");
            j.push_str(git);
            j.push(b",\n  \"session_id\": ");
            j.push_str(&ctx.session_id[..ctx.session_len]);
            j.push(b",\n  \"boot_profile\": ");
            j.push_str(ctx.mode_name());
            j.push(b",\n  \"environment\": ");
            j.push_str(if (ctx.ext.cpuid_features & (1 << 8)) != 0 { b"qemu_or_hypervisor" } else { b"bare_metal" });
            j.push(b",\n  \"hardware_verified\": false,\n  \"redacted_ids_by_default\": true,\n  \"pass\": ");
            j.push_u64(rep.summary.pass_count as u64);
            j.push(b",\n  \"fail\": ");
            j.push_u64(rep.summary.fail_count as u64);
            j.push(b",\n  \"skip\": ");
            j.push_u64(rep.summary.skip_count as u64);
            j.push(b",\n  \"blocked\": ");
            j.push_u64(rep.summary.blocked_count as u64);
            j.push(b",\n  \"persistence_status\": ");
            j.push_str(persist_slug(rep.summary.persist_status));
            j.push(b",\n  \"usb_ancestry_gates\": ");
            j.push_hex(ctx.usb_logs.gate_mask as u64, 2);
            j.push(b",\n  \"storage_ro\": ");
            j.push_bool(ctx.storage_ro());
            j.push(b"\n}\n");
        }
        1 => {
            // hardware_inventory.json
            j.push(b"{\n  \"cpus_online\": ");
            j.push_u64(ctx.online_cpus as u64);
            j.push(b",\n  \"tsc_khz\": ");
            j.push_u64(ctx.ext.tsc_khz as u64);
            j.push(b",\n  \"usable_mib\": ");
            j.push_u64(ctx.ext.mem_usable_mib as u64);
            j.push(b",\n  \"pci_functions\": ");
            j.push_u64(ctx.pci_count as u64);
            j.push(b",\n  \"gop_width\": ");
            j.push_u64(ctx.ext.fb_width as u64);
            j.push(b",\n  \"gop_height\": ");
            j.push_u64(ctx.ext.fb_height as u64);
            j.push(b",\n  \"vmd_detected\": ");
            j.push_bool(ctx.ext.vmd_packed != 0);
            j.push(b",\n  \"dgpu_detected\": ");
            j.push_bool(ctx.ext.dgpu_packed != 0 || find_nvidia_gpu(&ctx.pci_recs[..ctx.pci_count], ctx.ext.hwval_flags).is_some());
            j.push(b"\n}\n");
        }
        2 => {
            // cpu_memory.json
            j.push(b"{\n  \"cpus_online\": ");
            j.push_u64(ctx.online_cpus as u64);
            j.push(b",\n  \"tsc_khz\": ");
            j.push_u64(ctx.ext.tsc_khz as u64);
            j.push(b",\n  \"cpuid_features_hex\": ");
            j.push_hex(ctx.ext.cpuid_features, 8);
            j.push(b",\n  \"mem_usable_mib\": ");
            j.push_u64(ctx.ext.mem_usable_mib as u64);
            j.push(b",\n  \"mem_regions\": ");
            j.push_u64(ctx.ext.mem_regions as u64);
            j.push(b"\n}\n");
        }
        3 => {
            // acpi_uefi.json
            j.push(b"{\n  \"acpi_audit_flags\": ");
            j.push_hex(ctx.ext.acpi_audit_flags as u64, 4);
            j.push(b",\n  \"acpi_oem_id\": ");
            j.push_str(&ctx.ext.acpi_oem_id);
            j.push(b",\n  \"gop_width\": ");
            j.push_u64(ctx.ext.fb_width as u64);
            j.push(b",\n  \"gop_height\": ");
            j.push_u64(ctx.ext.fb_height as u64);
            j.push(b",\n  \"gop_pitch\": ");
            j.push_u64(ctx.ext.fb_pitch as u64);
            j.push(b",\n  \"secure_boot_state\": \"post_ebs_ro_skip\"\n}\n");
        }
        4 => {
            // pci_topology.json
            j.push(b"{\n  \"pci_passive_mode\": ");
            j.push_bool(ctx.is_passive_safe());
            j.push(b",\n  \"devices\": [\n");
            for (i, r) in ctx.pci_recs[..ctx.pci_count].iter().enumerate() {
                if i > 0 { j.push(b",\n"); }
                j.push(b"    {\"bdf\": ");
                j.push_hex(r.bus_dev_fn as u64, 4);
                j.push(b", \"vid\": ");
                j.push_hex(r.vendor_id as u64, 4);
                j.push(b", \"did\": ");
                j.push_hex(r.device_id as u64, 4);
                j.push(b", \"class\": ");
                j.push_hex(r.class_code as u64, 2);
                j.push(b", \"subclass\": ");
                j.push_hex(r.subclass as u64, 2);
                j.push(b", \"passive\": ");
                j.push_bool((r.extra_flags & PCI_BOOT_EXTRA_PASSIVE) != 0);
                j.push(b", \"bme\": ");
                j.push_bool((r.extra_flags & PCI_BOOT_EXTRA_BUS_MASTER) != 0);
                j.push(b", \"msi\": ");
                j.push_bool((r.dev_flags & (PCI_DEV_FLAG_MSI as u8)) != 0);
                j.push(b", \"msix\": ");
                j.push_bool((r.dev_flags & (PCI_DEV_FLAG_MSIX as u8)) != 0);
                j.push(b"}");
            }
            j.push(b"\n  ]\n}\n");
        }
        5 => {
            // iommu_dma.json
            let im = ctx.ext.iommu_audit_flags;
            j.push(b"{\n  \"iommu_audit_flags\": ");
            j.push_hex(im as u64, 8);
            j.push(b",\n  \"dmar_present\": ");
            j.push_bool((im & 1) != 0);
            j.push(b",\n  \"vtd_hw_enforcing\": ");
            j.push_bool((im & 2) != 0);
            j.push(b",\n  \"int_remap\": ");
            j.push_bool((im & 4) != 0);
            j.push(b",\n  \"drhd_count\": ");
            j.push_u64(((im >> 8) & 0xFF) as u64);
            j.push(b",\n  \"rmrr_count\": ");
            j.push_u64(((im >> 16) & 0xFF) as u64);
            j.push(b",\n  \"max_aw_bits\": ");
            j.push_u64(((im >> 24) & 0xFF) as u64);
            j.push(b"\n}\n");
        }
        6 => {
            // intel_gpu.json
            let ig = find_intel_gpu(&ctx.pci_recs[..ctx.pci_count]);
            j.push(b"{\n  \"present\": ");
            j.push_bool(ig.is_some());
            j.push(b",\n  \"vendor\": \"0x8086\",\n  \"device\": ");
            j.push_hex(ig.map_or(0, |r| r.device_id as u64), 4);
            j.push(b",\n  \"allow_physical_gpu_writes\": false,\n  \"gop_handover\": ");
            j.push_bool(ctx.ext.fb_width > 0);
            j.push(b",\n  \"missing_native\": [\"guc_upload\", \"ggtt_ppgtt\", \"rcs_ring\", \"edp_165hz_aux\"],\n  \"status\": \"PARTIAL\"\n}\n");
        }
        7 => {
            // nvidia_gpu.json
            let nv = find_nvidia_gpu(&ctx.pci_recs[..ctx.pci_count], ctx.ext.hwval_flags);
            j.push(b"{\n  \"present\": ");
            j.push_bool(nv.is_some());
            j.push(b",\n  \"vendor\": \"0x10de\",\n  \"device\": ");
            j.push_hex(nv.map_or(0, |r| r.device_id as u64), 4);
            j.push(b",\n  \"driver_level\": \"Level 1 - DRIVER ISLAND READY\",\n  \"native_execution_supported\": false,\n  \"microvm_requirements\": [\"vtx_vmx\", \"vtd_int_remap\", \"vfio_dmar\", \"gsp_fw\"]\n}\n");
        }
        8 => {
            // usb_storage.json
            j.push(b"{\n  \"boot_disk_guid_redacted\": ");
            j.push_str(&guid_redacted[..guid_rlen]);
            j.push(b",\n  \"ancestry_gates\": ");
            j.push_hex(ctx.usb_logs.gate_mask as u64, 2);
            j.push(b",\n  \"block_reason\": ");
            j.push_u64(ctx.usb_logs.block_reason as u64);
            j.push(b",\n  \"dryos_logs_mounted\": ");
            j.push_bool(ctx.usb_logs.fat_mounted != 0);
            j.push(b",\n  \"free_kib\": ");
            j.push_u64(ctx.usb_logs.free_kib as u64);
            j.push(b",\n  \"internal_storage_ro\": ");
            j.push_bool(ctx.storage_ro());
            j.push(b",\n  \"vmd_gated\": ");
            j.push_bool(ctx.hw_sum.vmd_gated != 0);
            j.push(b"\n}\n");
        }
        9 => {
            // driver_compatibility.json
            j.push(b"{\n  \"architecture\": \"microkernel_ring3_driver_islands\",\n  \"kernel_gpl_drivers\": 0,\n  \"usb_pid\": ");
            j.push_u64(ctx.hw_sum.usb_pid as u64);
            j.push(b",\n  \"blk_pid\": ");
            j.push_u64(ctx.hw_sum.blk_pid as u64);
            j.push(b",\n  \"nvme_pid\": ");
            j.push_u64(ctx.hw_sum.nvme_pid as u64);
            j.push(b",\n  \"nvidia_island_level\": 1\n}\n");
        }
        _ => {
            // test_results.json
            j.push(b"{\n  \"session_id\": ");
            j.push_str(&ctx.session_id[..ctx.session_len]);
            j.push(b",\n  \"boot_profile\": ");
            j.push_str(ctx.mode_name());
            j.push(b",\n  \"hardware_verified\": false,\n  \"results\": [\n");
            for (i, r) in rep.records[..rep.count].iter().enumerate() {
                if i > 0 { j.push(b",\n"); }
                j.push(b"    {\"id\": ");
                j.push_str(r.id_bytes());
                j.push(b", \"category\": ");
                j.push_str(category_slug(r.category));
                j.push(b", \"status\": ");
                j.push_str(status_slug(r.status));
                j.push(b", \"observed\": ");
                j.push_str(r.observed_bytes());
                j.push(b", \"expected\": ");
                j.push_str(r.expected_bytes());
                j.push(b", \"duration_us\": ");
                j.push_u64(r.duration_us as u64);
                j.push(b", \"error_code\": ");
                j.push_i64(r.error_code as i64);
                j.push(b", \"recovery\": ");
                j.push_str(r.recovery_bytes());
                j.push(b", \"hardware_verified\": false}");
            }
            j.push(b"\n  ]\n}\n");
        }
    }
    let len = j.len;
    &scratch[..len]
}

pub fn render_manifest<'a>(digests: &[[u8; 32]; 12], scratch: &'a mut [u8]) -> &'a [u8] {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut j = JsonBuf::new(scratch);
    for (idx, name) in REPORT_FILENAMES.iter().enumerate() {
        for &byte in &digests[idx] {
            j.push(&[H[(byte >> 4) as usize], H[(byte & 0xf) as usize]]);
        }
        j.push(b"  ");
        j.push(name);
        j.push(b"\n");
    }
    let len = j.len;
    &scratch[..len]
}

pub fn compute_sha256(bytes: &[u8]) -> [u8; 32] {
    let mut s = Sha256::new();
    s.update(bytes);
    s.finalize()
}
