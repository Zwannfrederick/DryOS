// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! GPU & Mandatory NVIDIA Driver Island terminal commands (`nvidia-smi`, `nvidia-smi -L`,
//! `nvidia-smi -q`, `dry gpu status`, `dry gpu nvidia ...`) backed by live supervisor IPC.

use core::mem::size_of;

use crate::nvidia_island::protocol::{
    NvIslandStatusWire, NvPrereqWire, IPC_OP_SUP_NV_EXPORT, IPC_OP_SUP_NV_QUERY, NV_PREREQ_COUNT,
};
use crate::nvidia_island::smi_format::{
    emit_dry_gpu_status, emit_nvidia_prereqs, emit_nvidia_smi, emit_nvidia_smi_list,
    emit_nvidia_smi_query, LineKind,
};
use crate::shell::{LineBuilder, ShellEngine};
use crate::surface::{STYLE_ACCENT, STYLE_DEFAULT, STYLE_WARN};
use crate::vfs_proto::sys_ipc_call_extra;
use crate::{msg_tag_label, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES};

fn trim(s: &[u8]) -> &[u8] {
    let mut a = 0usize;
    while a < s.len() && s[a] == b' ' { a += 1; }
    let mut b = s.len();
    while b > a && s[b - 1] == b' ' { b -= 1; }
    &s[a..b]
}

fn push_term(engine: &mut ShellEngine, line: &[u8], kind: LineKind) {
    let style = match kind {
        LineKind::Accent => STYLE_ACCENT,
        LineKind::Normal => STYLE_DEFAULT,
        LineKind::Warn => STYLE_WARN,
    };
    engine.push_line(line, style);
}

fn query_nv_status(sup_ep: u64, do_export: bool) -> Option<NvIslandStatusWire> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let op = if do_export { IPC_OP_SUP_NV_EXPORT } else { IPC_OP_SUP_NV_QUERY };
    let (tag, elen, _) = sys_ipc_call_extra(sup_ep, op, [0; 4], &[], &mut rx, 0).ok()?;
    if msg_tag_label(tag) != IPC_OP_PONG || elen < size_of::<NvIslandStatusWire>() {
        return None;
    }
    let mut st = NvIslandStatusWire::empty();
    // SAFETY: `NvIslandStatusWire` is `#[repr(C)]` POD validated to `elen >= size_of`.
    unsafe {
        core::ptr::copy_nonoverlapping(rx.as_ptr(), (&raw mut st).cast::<u8>(), size_of::<NvIslandStatusWire>());
    }
    Some(st)
}

fn query_nv_prereqs(sup_ep: u64) -> [NvPrereqWire; NV_PREREQ_COUNT] {
    let mut out = [NvPrereqWire::EMPTY; NV_PREREQ_COUNT];
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    for (idx, dst) in out.iter_mut().enumerate() {
        if let Ok((tag, elen, _)) = sys_ipc_call_extra(sup_ep, IPC_OP_SUP_NV_QUERY, [1, idx as u64, 0, 0], &[], &mut rx, 0) {
            if msg_tag_label(tag) == IPC_OP_PONG && elen >= size_of::<NvPrereqWire>() {
                // SAFETY: `NvPrereqWire` is `#[repr(C)]` POD (`64` bytes).
                unsafe {
                    core::ptr::copy_nonoverlapping(rx.as_ptr(), (dst as *mut NvPrereqWire).cast::<u8>(), size_of::<NvPrereqWire>());
                }
            }
        }
    }
    out
}

pub fn try_exec_gpu_cmd(
    raw_line: &[u8],
    sup_ep: u64,
    engine: &mut ShellEngine,
    debug_line: fn(&LineBuilder),
) -> bool {
    let cmd = trim(raw_line);
    let slug: &[u8] = match cmd {
        b"nvidia-smi" => b"nvidia-smi",
        b"nvidia-smi -L" => b"nvidia-smi -L",
        b"nvidia-smi -q" => b"nvidia-smi -q",
        b"dry gpu status" => b"dry gpu status",
        b"dry gpu nvidia status" => b"dry gpu nvidia status",
        b"dry gpu nvidia prereqs" => b"dry gpu nvidia prereqs",
        b"dry gpu nvidia export" => b"dry gpu nvidia export",
        _ => return false,
    };

    let do_export = slug == b"dry gpu nvidia export";
    let Some(st) = query_nv_status(sup_ep, do_export) else {
        engine.push_line(b"GPU/NVIDIA supervisor IPC unavailable", STYLE_WARN);
        return true;
    };

    match slug {
        b"nvidia-smi" | b"dry gpu nvidia status" => {
            emit_nvidia_smi(&st, |l, k| push_term(engine, l, k));
        }
        b"nvidia-smi -L" => {
            emit_nvidia_smi_list(&st, |l, k| push_term(engine, l, k));
        }
        b"nvidia-smi -q" => {
            let prereqs = query_nv_prereqs(sup_ep);
            emit_nvidia_smi_query(&st, &prereqs, |l, k| push_term(engine, l, k));
        }
        b"dry gpu status" => {
            emit_dry_gpu_status(&st, |l, k| push_term(engine, l, k));
        }
        b"dry gpu nvidia prereqs" => {
            let prereqs = query_nv_prereqs(sup_ep);
            emit_nvidia_prereqs(&st, &prereqs, |l, k| push_term(engine, l, k));
        }
        b"dry gpu nvidia export" => {
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"NVIDIA nvidia_exp/ USB export: ");
            if st.export_ok != 0 {
                lb.push_bytes(b"VERIFIED files=");
                lb.push_u64(st.export_files as u64);
                lb.push_bytes(b" (7 data + manifest.sha256)");
                engine.push_line(lb.as_bytes(), STYLE_ACCENT);
            } else {
                lb.push_bytes(b"BLOCKED (DRYOS_LOGS unavailable or RO)");
                engine.push_line(lb.as_bytes(), STYLE_WARN);
            }
        }
        _ => {}
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-SHELL] CMD=");
    lb.push_bytes(slug);
    lb.push_bytes(b" level=");
    lb.push_u64(st.level as u64);
    lb.push_bytes(b" status=BLOCKED spawned=");
    lb.push_u64(st.spawned as u64);
    lb.push_bytes(b" met=");
    lb.push_u64(st.met_count as u64);
    lb.push_bytes(b"/14 pci=");
    lb.push_u64(st.pci_present as u64);
    lb.push_bytes(b" export_ok=");
    lb.push_u64(st.export_ok as u64);
    lb.push_bytes(b" files=");
    lb.push_u64(st.export_files as u64);
    debug_line(&lb);
    true
}
