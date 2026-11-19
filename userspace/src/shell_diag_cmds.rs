// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Real `dry ...` terminal diagnostic commands invoking shared `hwdiag` over supervisor IPC.

use core::mem::size_of;

use crate::hwdiag::{
    mode_slug, persist_slug, status_slug, DiagSummaryWire, DiagTestRecord, IPC_OP_SUP_DIAG_QUERY,
    IPC_OP_SUP_DIAG_RUN, SCOPE_ACPI, SCOPE_ALL, SCOPE_COMPAT, SCOPE_CPU, SCOPE_DRIVERS, SCOPE_GPU,
    SCOPE_IOMMU, SCOPE_MEM, SCOPE_PCI, SCOPE_STORAGE, SCOPE_USB, STATUS_BLOCKED, STATUS_FAIL,
    STATUS_PASS, STATUS_SKIP,
};
use crate::shell::{LineBuilder, ShellEngine};
use crate::surface::{STYLE_ACCENT, STYLE_DEFAULT, STYLE_MUTED, STYLE_OK, STYLE_WARN};
use crate::vfs_proto::sys_ipc_call_extra;
use crate::{msg_tag_label, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES};

fn trim(s: &[u8]) -> &[u8] {
    let mut a = 0usize;
    while a < s.len() && s[a] == b' ' { a += 1; }
    let mut b = s.len();
    while b > a && s[b - 1] == b' ' { b -= 1; }
    &s[a..b]
}

fn render_records(sup_ep: u64, engine: &mut ShellEngine, filter_cat: Option<u8>, sum: &DiagSummaryWire) {
    let mut hdr = LineBuilder::new();
    hdr.push_bytes(b"DRY HWDIAG mode=");
    hdr.push_bytes(mode_slug(sum.mode));
    hdr.push_bytes(b" pass=");
    hdr.push_u64(sum.pass_count as u64);
    hdr.push_bytes(b" fail=");
    hdr.push_u64(sum.fail_count as u64);
    hdr.push_bytes(b" skip=");
    hdr.push_u64(sum.skip_count as u64);
    hdr.push_bytes(b" blocked=");
    hdr.push_u64(sum.blocked_count as u64);
    engine.push_line(hdr.as_bytes(), STYLE_ACCENT);

    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let mut idx = 0usize;
    while idx < sum.total_tests as usize {
        let Ok((tag, elen, mrs)) = sys_ipc_call_extra(sup_ep, IPC_OP_SUP_DIAG_QUERY, [1, idx as u64, 0, 0], &[], &mut rx, 0) else {
            break;
        };
        if msg_tag_label(tag) != IPC_OP_PONG {
            break;
        }
        let cnt = (mrs[1] as usize).min(elen / size_of::<DiagTestRecord>());
        if cnt == 0 {
            break;
        }
        for i in 0..cnt {
            let mut rec = DiagTestRecord::EMPTY;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    rx[i * size_of::<DiagTestRecord>()..].as_ptr(),
                    (&raw mut rec).cast::<u8>(),
                    size_of::<DiagTestRecord>(),
                );
            }
            if filter_cat.is_none_or(|c| c == rec.category) {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"  [");
                lb.push_bytes(status_slug(rec.status));
                lb.push_bytes(b"] ");
                lb.push_padded(rec.id_bytes(), 22);
                lb.push_bytes(rec.observed_bytes());
                let st = match rec.status {
                    STATUS_PASS => STYLE_OK,
                    STATUS_SKIP => STYLE_MUTED,
                    STATUS_FAIL | STATUS_BLOCKED => STYLE_WARN,
                    _ => STYLE_DEFAULT,
                };
                engine.push_line(lb.as_bytes(), st);
            }
        }
        idx = mrs[2] as usize;
    }
}

fn call_diag_run(sup_ep: u64, scope: u32, force_safe: bool, export_usb: bool) -> Option<DiagSummaryWire> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let flags = u64::from(force_safe) | (u64::from(export_usb) << 1);
    let (tag, elen, _) = sys_ipc_call_extra(sup_ep, IPC_OP_SUP_DIAG_RUN, [scope as u64, flags, 0, 0], &[], &mut rx, 0).ok()?;
    if msg_tag_label(tag) != IPC_OP_PONG || elen < size_of::<DiagSummaryWire>() {
        return None;
    }
    let mut s = DiagSummaryWire::empty();
    unsafe {
        core::ptr::copy_nonoverlapping(rx.as_ptr(), (&raw mut s).cast::<u8>(), size_of::<DiagSummaryWire>());
    }
    Some(s)
}

fn call_diag_query(sup_ep: u64) -> Option<DiagSummaryWire> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, elen, _) = sys_ipc_call_extra(sup_ep, IPC_OP_SUP_DIAG_QUERY, [0; 4], &[], &mut rx, 0).ok()?;
    if msg_tag_label(tag) != IPC_OP_PONG || elen < size_of::<DiagSummaryWire>() {
        return None;
    }
    let mut s = DiagSummaryWire::empty();
    unsafe {
        core::ptr::copy_nonoverlapping(rx.as_ptr(), (&raw mut s).cast::<u8>(), size_of::<DiagSummaryWire>());
    }
    Some(s)
}

pub fn try_exec_dry_cmd(
    raw_line: &[u8],
    sup_ep: u64,
    engine: &mut ShellEngine,
    debug_line: fn(&LineBuilder),
) -> bool {
    let cmd = trim(raw_line);
    if cmd != b"dry" && !cmd.starts_with(b"dry ") {
        return false;
    }
    let arg = if cmd.len() > 4 { trim(&cmd[4..]) } else { b"help" };
    if arg == b"help" {
        engine.push_line(b"Usage: dry <doctor|check --safe|--all|--cpu|--memory|--acpi|--pci|--iommu|--gpu|--usb|--storage|drivers|compat|report [--export-usb]>", STYLE_ACCENT);
        let _ = crate::sys_write_debug(b"[DRY-SHELL] CMD=dry help ok=true\n");
        return true;
    }

    let (slug, scope, force_safe, export_usb): (&[u8], u32, bool, bool) = match arg {
        b"doctor" => (b"doctor", SCOPE_ALL, true, false),
        b"check --safe" => (b"check_safe", SCOPE_ALL, true, false),
        b"check --all" => (b"check_all", SCOPE_ALL, false, false),
        b"check --cpu" => (b"check_cpu", SCOPE_CPU, true, false),
        b"check --memory" => (b"check_memory", SCOPE_MEM, true, false),
        b"check --acpi" => (b"check_acpi", SCOPE_ACPI, true, false),
        b"check --pci" => (b"check_pci", SCOPE_PCI, true, false),
        b"check --iommu" => (b"check_iommu", SCOPE_IOMMU, true, false),
        b"check --gpu" => (b"check_gpu", SCOPE_GPU, true, false),
        b"check --usb" => (b"check_usb", SCOPE_USB, true, false),
        b"check --storage" => (b"check_storage", SCOPE_STORAGE, true, false),
        b"drivers" => (b"drivers", SCOPE_DRIVERS, true, false),
        b"compat" => (b"compat", SCOPE_COMPAT, true, false),
        b"report" => (b"report", 0, false, false),
        b"report --export-usb" => (b"report_export", 0, false, true),
        _ => return false,
    };

    let sum_opt = if scope == 0 && !export_usb {
        call_diag_query(sup_ep)
    } else {
        call_diag_run(sup_ep, scope, force_safe, export_usb)
    };
    let Some(sum) = sum_opt else {
        engine.push_line(b"dry diag supervisor IPC unavailable", STYLE_WARN);
        return true;
    };

    if scope != 0 || slug == b"report" || slug == b"report_export" {
        render_records(sup_ep, engine, None, &sum);
        let mut pl = LineBuilder::new();
        pl.push_bytes(b"USB Log Persist: ");
        pl.push_bytes(persist_slug(sum.persist_status));
        pl.push_bytes(b" | gates=0x");
        pl.push_hex_width(sum.gate_mask as u64, 2);
        pl.push_bytes(b" | free_kib=");
        pl.push_u64(sum.free_kib as u64);
        pl.push_bytes(b" | files=");
        pl.push_u64(sum.files_exported as u64);
        engine.push_line(pl.as_bytes(), if sum.persist_status == 1 { STYLE_OK } else { STYLE_WARN });
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-SHELL] CMD=dry ");
    lb.push_bytes(slug);
    lb.push_bytes(b" pass=");
    lb.push_u64(sum.pass_count as u64);
    lb.push_bytes(b" fail=");
    lb.push_u64(sum.fail_count as u64);
    lb.push_bytes(b" skip=");
    lb.push_u64(sum.skip_count as u64);
    lb.push_bytes(b" blocked=");
    lb.push_u64(sum.blocked_count as u64);
    lb.push_bytes(b" persist=");
    lb.push_bytes(persist_slug(sum.persist_status));
    lb.push_bytes(b" gates=0x");
    lb.push_hex_width(sum.gate_mask as u64, 2);
    debug_line(&lb);
    true
}
