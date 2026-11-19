// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared `#![no_std]` Hardware Validation & Diagnostic Framework (`hwdiag`).
//! Consumed uniformly by boot-time auto-diagnostics (`dry-init`), CLI (`dry-shell`),
//! and graphical status (`dry-sysmon`). Zero redundant engines.

pub mod checks_cpu_mem;
pub mod checks_gpu_storage;
pub mod checks_hw;
pub mod mgr;
pub mod report_json;

use core::mem::size_of;

use crate::abi_ext::{
    RootInitBootExtInfo, HWCHECK_MODE_DESKTOP_SAFE, HWCHECK_MODE_EXTENDED,
    HWCHECK_MODE_NVIDIA_EXP, HWCHECK_MODE_RECOVERY, HWCHECK_MODE_SAFE, HWVAL_FLAG_AUTO,
    HWVAL_FLAG_STORAGE_RO,
};
use crate::fat32_log::UsbLogStatus;
use crate::sup_hw_services::SupHwSummary;
use crate::{BootPciFuncRecord, MAX_BOOT_PCI_FUNCS};

pub const IPC_OP_SUP_DIAG_QUERY: u32 = 144;
pub const IPC_OP_SUP_DIAG_RUN: u32 = 145;

pub const CAT_CPU: u8 = 1;
pub const CAT_MEM: u8 = 2;
pub const CAT_ACPI: u8 = 3;
pub const CAT_PCI: u8 = 4;
pub const CAT_IOMMU: u8 = 5;
pub const CAT_GPU: u8 = 6;
pub const CAT_USB: u8 = 7;
pub const CAT_STORAGE: u8 = 8;
pub const CAT_DRIVERS: u8 = 9;
pub const CAT_COMPAT: u8 = 10;

pub const STATUS_PASS: u8 = 0;
pub const STATUS_FAIL: u8 = 1;
pub const STATUS_SKIP: u8 = 2;
pub const STATUS_BLOCKED: u8 = 3;

pub const SCOPE_CPU: u32 = 1 << 0;
pub const SCOPE_MEM: u32 = 1 << 1;
pub const SCOPE_ACPI: u32 = 1 << 2;
pub const SCOPE_PCI: u32 = 1 << 3;
pub const SCOPE_IOMMU: u32 = 1 << 4;
pub const SCOPE_GPU: u32 = 1 << 5;
pub const SCOPE_USB: u32 = 1 << 6;
pub const SCOPE_STORAGE: u32 = 1 << 7;
pub const SCOPE_DRIVERS: u32 = 1 << 8;
pub const SCOPE_COMPAT: u32 = 1 << 9;
pub const SCOPE_ALL: u32 = 0x3FF;

pub const PERSIST_NONE: u8 = 0;
pub const PERSIST_VERIFIED: u8 = 1;
pub const PERSIST_BLOCKED: u8 = 2;
pub const PERSIST_FAILED: u8 = 3;
pub const PERSIST_RECOVERY: u8 = 4;
pub const PERSIST_COMMITTING: u8 = 5;

pub const MAX_DIAG_RECORDS: usize = 32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DiagTestRecord {
    pub id: [u8; 28],
    pub observed: [u8; 32],
    pub expected: [u8; 24],
    pub recovery: [u8; 32],
    pub duration_us: u32,
    pub error_code: i32,
    pub category: u8,
    pub status: u8,
    pub hardware_verified: u8,
    pub _pad: u8,
}

const _: () = assert!(size_of::<DiagTestRecord>() == 128);

impl DiagTestRecord {
    pub const EMPTY: Self = Self {
        id: [0; 28],
        observed: [0; 32],
        expected: [0; 24],
        recovery: [0; 32],
        duration_us: 0,
        error_code: 0,
        category: 0,
        status: STATUS_SKIP,
        hardware_verified: 0,
        _pad: 0,
    };

    #[allow(clippy::too_many_arguments)]
    pub fn make(
        category: u8,
        status: u8,
        id: &[u8],
        observed: &[u8],
        expected: &[u8],
        recovery: &[u8],
        duration_us: u32,
        error_code: i32,
    ) -> Self {
        let mut r = Self::EMPTY;
        r.category = category;
        r.status = status;
        r.duration_us = duration_us;
        r.error_code = error_code;
        copy_cstr(&mut r.id, id);
        copy_cstr(&mut r.observed, observed);
        copy_cstr(&mut r.expected, expected);
        copy_cstr(&mut r.recovery, recovery);
        r
    }

    pub fn id_bytes(&self) -> &[u8] {
        cstr_slice(&self.id)
    }
    pub fn observed_bytes(&self) -> &[u8] {
        cstr_slice(&self.observed)
    }
    pub fn expected_bytes(&self) -> &[u8] {
        cstr_slice(&self.expected)
    }
    pub fn recovery_bytes(&self) -> &[u8] {
        cstr_slice(&self.recovery)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DiagSummaryWire {
    pub mode: u32,
    pub gate_mask: u32,
    pub block_reason: u32,
    pub free_kib: u32,
    pub writes_verified: u32,
    pub total_duration_us: u32,
    pub pass_count: u16,
    pub fail_count: u16,
    pub skip_count: u16,
    pub blocked_count: u16,
    pub total_tests: u16,
    pub pci_funcs: u16,
    pub auto_ran: u8,
    pub persist_status: u8,
    pub sha256_verified: u8,
    pub redact_default: u8,
    pub recovery_sessions: u8,
    pub files_exported: u8,
    pub hardware_verified: u8,
    pub storage_ro: u8,
    pub session_id: [u8; 20],
}

const _: () = assert!(size_of::<DiagSummaryWire>() == 64);

impl DiagSummaryWire {
    pub const fn empty() -> Self {
        Self {
            mode: HWCHECK_MODE_OFF_CONST,
            gate_mask: 0,
            block_reason: 0,
            free_kib: 0,
            writes_verified: 0,
            total_duration_us: 0,
            pass_count: 0,
            fail_count: 0,
            skip_count: 0,
            blocked_count: 0,
            total_tests: 0,
            pci_funcs: 0,
            auto_ran: 0,
            persist_status: PERSIST_NONE,
            sha256_verified: 0,
            redact_default: 1,
            recovery_sessions: 0,
            files_exported: 0,
            hardware_verified: 0,
            storage_ro: 1,
            session_id: [0; 20],
        }
    }
}

const HWCHECK_MODE_OFF_CONST: u32 = 0;

pub struct DiagContext {
    pub ext: RootInitBootExtInfo,
    pub pci_recs: [BootPciFuncRecord; MAX_BOOT_PCI_FUNCS],
    pub pci_count: usize,
    pub hw_sum: SupHwSummary,
    pub usb_logs: UsbLogStatus,
    pub persist_ro: bool,
    pub tsc_mhz: u64,
    pub online_cpus: u32,
    pub session_id: [u8; 20],
    pub session_len: usize,
}

impl DiagContext {
    pub const fn empty() -> Self {
        Self {
            ext: RootInitBootExtInfo::empty(),
            pci_recs: [BootPciFuncRecord::EMPTY; MAX_BOOT_PCI_FUNCS],
            pci_count: 0,
            hw_sum: SupHwSummary::empty(),
            usb_logs: UsbLogStatus::empty(),
            persist_ro: false,
            tsc_mhz: 0,
            online_cpus: 0,
            session_id: [0; 20],
            session_len: 0,
        }
    }

    pub fn mode_name(&self) -> &'static [u8] {
        mode_slug(self.ext.hwcheck_mode)
    }

    pub fn is_auto(&self) -> bool {
        (self.ext.hwval_flags & HWVAL_FLAG_AUTO) != 0
    }

    pub fn is_passive_safe(&self) -> bool {
        matches!(
            self.ext.hwcheck_mode,
            HWCHECK_MODE_SAFE | HWCHECK_MODE_DESKTOP_SAFE | HWCHECK_MODE_RECOVERY
        ) || self.ext.pci_passive_mode != 0
    }

    pub fn storage_ro(&self) -> bool {
        self.persist_ro || (self.ext.hwval_flags & HWVAL_FLAG_STORAGE_RO) != 0
    }
}

pub struct DiagSessionReport {
    pub records: [DiagTestRecord; MAX_DIAG_RECORDS],
    pub count: usize,
    pub summary: DiagSummaryWire,
}

impl DiagSessionReport {
    pub const fn empty() -> Self {
        Self {
            records: [DiagTestRecord::EMPTY; MAX_DIAG_RECORDS],
            count: 0,
            summary: DiagSummaryWire::empty(),
        }
    }

    pub fn push(&mut self, rec: DiagTestRecord) {
        if self.count < MAX_DIAG_RECORDS {
            self.records[self.count] = rec;
            self.count += 1;
        }
    }

    pub fn recompute_totals(&mut self, ctx: &DiagContext) {
        let (mut p, mut f, mut s, mut b, mut dur) = (0u16, 0u16, 0u16, 0u16, 0u32);
        for rec in &self.records[..self.count] {
            match rec.status {
                STATUS_PASS => p += 1,
                STATUS_FAIL => f += 1,
                STATUS_SKIP => s += 1,
                _ => b += 1,
            }
            dur = dur.saturating_add(rec.duration_us);
        }
        self.summary.mode = ctx.ext.hwcheck_mode;
        self.summary.pass_count = p;
        self.summary.fail_count = f;
        self.summary.skip_count = s;
        self.summary.blocked_count = b;
        self.summary.total_tests = self.count as u16;
        self.summary.total_duration_us = dur;
        self.summary.pci_funcs = ctx.pci_count as u16;
        self.summary.gate_mask = ctx.usb_logs.gate_mask;
        self.summary.block_reason = ctx.usb_logs.block_reason;
        self.summary.free_kib = ctx.usb_logs.free_kib;
        self.summary.writes_verified = ctx.usb_logs.writes_verified;
        self.summary.storage_ro = u8::from(ctx.storage_ro());
        self.summary.session_id[..ctx.session_len.min(20)]
            .copy_from_slice(&ctx.session_id[..ctx.session_len.min(20)]);
    }
}

#[inline(never)]
pub fn run_suite(ctx: &DiagContext, scope: u32, force_safe: bool, out: &mut DiagSessionReport) {
    out.count = 0;
    let safe = force_safe || ctx.is_passive_safe() || ctx.ext.hwcheck_mode == 0;
    if (scope & (SCOPE_CPU | SCOPE_MEM)) != 0 {
        checks_cpu_mem::run(ctx, scope, safe, out);
    }
    if (scope & (SCOPE_ACPI | SCOPE_PCI | SCOPE_IOMMU)) != 0 {
        checks_hw::run(ctx, scope, safe, out);
    }
    if (scope & (SCOPE_GPU | SCOPE_USB | SCOPE_STORAGE | SCOPE_DRIVERS | SCOPE_COMPAT)) != 0 {
        checks_gpu_storage::run(ctx, scope, safe, out);
    }
    out.recompute_totals(ctx);
}

pub fn mode_slug(mode: u32) -> &'static [u8] {
    match mode {
        HWCHECK_MODE_SAFE => b"safe",
        HWCHECK_MODE_DESKTOP_SAFE => b"desktop_safe",
        HWCHECK_MODE_EXTENDED => b"extended",
        HWCHECK_MODE_RECOVERY => b"recovery",
        HWCHECK_MODE_NVIDIA_EXP => b"nvidia_exp",
        _ => b"standard",
    }
}

pub fn status_slug(status: u8) -> &'static [u8] {
    match status {
        STATUS_PASS => b"PASS",
        STATUS_FAIL => b"FAIL",
        STATUS_SKIP => b"SKIP",
        _ => b"BLOCKED",
    }
}

pub fn persist_slug(st: u8) -> &'static [u8] {
    match st {
        PERSIST_VERIFIED => b"VERIFIED",
        PERSIST_BLOCKED => b"BLOCKED",
        PERSIST_FAILED => b"FAILED",
        PERSIST_RECOVERY => b"RECOVERY",
        PERSIST_COMMITTING => b"committing",
        _ => b"RAM_ONLY",
    }
}

pub fn category_slug(cat: u8) -> &'static [u8] {
    match cat {
        CAT_CPU => b"cpu",
        CAT_MEM => b"memory",
        CAT_ACPI => b"acpi",
        CAT_PCI => b"pci",
        CAT_IOMMU => b"iommu",
        CAT_GPU => b"gpu",
        CAT_USB => b"usb",
        CAT_STORAGE => b"storage",
        CAT_DRIVERS => b"drivers",
        _ => b"compat",
    }
}

pub fn copy_cstr(dst: &mut [u8], src: &[u8]) {
    dst.fill(0);
    let n = src.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&src[..n]);
}

pub fn cstr_slice(buf: &[u8]) -> &[u8] {
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    &buf[..n]
}

pub fn redact_guid_hex(guid: &[u8; 16], out: &mut [u8; 20]) -> usize {
    if *guid == [0u8; 16] {
        let s = b"none(unconfigured)";
        out[..s.len()].copy_from_slice(s);
        return s.len();
    }
    // Redacted format: "redacted-XX..YY" (`2` prefix bytes + `1` tail byte visible only).
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let prefix = b"redacted-";
    out[..9].copy_from_slice(prefix);
    out[9] = HEX[(guid[0] >> 4) as usize];
    out[10] = HEX[(guid[0] & 0xf) as usize];
    out[11] = HEX[(guid[1] >> 4) as usize];
    out[12] = HEX[(guid[1] & 0xf) as usize];
    out[13] = b'.';
    out[14] = b'.';
    out[15] = HEX[(guid[15] >> 4) as usize];
    out[16] = HEX[(guid[15] & 0xf) as usize];
    17
}
