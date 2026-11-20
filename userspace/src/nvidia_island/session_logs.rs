// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Structured `nvidia_exp/` 7-file + `manifest.sha256` USB log exporter.
//!
//! Uses caller-supplied `.bss` scratch (`16 KiB`) and reuses `hwdiag::mgr::stage_and_commit_file`
//! with 3-segment `<sess>/nvidia_exp/<fname>` paths. Zero fabricated NVML values.

use crate::hwdiag::mgr::{query_usb_log_status, read_usb_file, stage_and_commit_file};
use crate::hwdiag::report_json::{compute_sha256, JsonBuf};
use crate::nvidia_island::protocol::{NvIslandStatusWire, NvPrereqWire, NV_PREREQ_COUNT};
use crate::nvidia_island::smi_format::{
    emit_nvidia_smi, emit_nvidia_smi_list, emit_nvidia_smi_query, LineKind,
};
use crate::shell::LineBuilder;
use crate::usb::msc::ANCESTRY_ALL_GATES;
use crate::BootPciFuncRecord;

pub const NV_EXP_FILENAMES: [&[u8]; 7] = [
    b"nvidia_island_status.json",
    b"nvidia_prerequisites.json",
    b"nvidia_pci_inventory.json",
    b"nvidia_smi_output.txt",
    b"nvidia_smi_list.txt",
    b"nvidia_smi_query.txt",
    b"nvidia_ipc_audit.json",
];

fn render_text_lines(
    scratch: &mut [u8],
    f: impl FnOnce(&mut dyn FnMut(&[u8], LineKind)),
) -> &[u8] {
    let mut len = 0usize;
    let mut push_fn = |line: &[u8], _kind: LineKind| {
        let take = line.len().min(scratch.len().saturating_sub(len + 1));
        scratch[len..len + take].copy_from_slice(&line[..take]);
        len += take;
        if len < scratch.len() {
            scratch[len] = b'\n';
            len += 1;
        }
    };
    f(&mut push_fn);
    &scratch[..len]
}

fn render_nv_file<'a>(
    idx: usize,
    st: &NvIslandStatusWire,
    prereqs: &[NvPrereqWire; NV_PREREQ_COUNT],
    pci_recs: &[BootPciFuncRecord],
    scratch: &'a mut [u8],
) -> &'a [u8] {
    match idx {
        3 => render_text_lines(scratch, |cb| emit_nvidia_smi(st, |l, k| cb(l, k))),
        4 => render_text_lines(scratch, |cb| emit_nvidia_smi_list(st, |l, k| cb(l, k))),
        5 => render_text_lines(scratch, |cb| emit_nvidia_smi_query(st, prereqs, |l, k| cb(l, k))),
        _ => {
            let mut j = JsonBuf::new(scratch);
            match idx {
                0 => {
                    j.push(b"{\n  \"session_id\": ");
                    j.push_str(st.session_bytes());
                    j.push(b",\n  \"level\": ");
                    j.push_u64(st.level as u64);
                    j.push(b",\n  \"level_name\": \"DRIVER_ISLAND_READY\",\n  \"status\": \"BLOCKED\",\n  \"hardware_verified\": false,\n  \"spawned\": ");
                    j.push_bool(st.spawned != 0);
                    j.push(b",\n  \"mac_isolated\": ");
                    j.push_bool(st.mac_isolated != 0);
                    j.push(b",\n  \"island_alive\": ");
                    j.push_bool(st.island_alive != 0);
                    j.push(b",\n  \"pid\": ");
                    j.push_u64(st.pid as u64);
                    j.push(b",\n  \"generation\": ");
                    j.push_u64(st.generation as u64);
                    j.push(b",\n  \"met_count\": ");
                    j.push_u64(st.met_count as u64);
                    j.push(b",\n  \"blocked_count\": ");
                    j.push_u64(st.blocked_count as u64);
                    j.push(b",\n  \"reason\": ");
                    j.push_str(st.reason_bytes());
                    j.push(b",\n  \"nvml_telemetry\": \"N/A (BLOCKED)\"\n}\n");
                }
                1 => {
                    j.push(b"{\n  \"met\": ");
                    j.push_u64(st.met_count as u64);
                    j.push(b",\n  \"total\": 14,\n  \"prerequisites\": [\n");
                    for (i, p) in prereqs.iter().enumerate() {
                        if i > 0 { j.push(b",\n"); }
                        j.push(b"    {\"id\": ");
                        j.push_u64(p.idx as u64);
                        j.push(b", \"name\": ");
                        j.push_str(p.name_bytes());
                        j.push(b", \"met\": ");
                        j.push_bool(p.met != 0);
                        j.push(b", \"injected_fail\": ");
                        j.push_bool(p.injected_fail != 0);
                        j.push(b", \"err_code\": ");
                        j.push_i64(p.err_code as i64);
                        j.push(b", \"detail\": ");
                        j.push_str(p.detail_bytes());
                        j.push(b"}");
                    }
                    j.push(b"\n  ]\n}\n");
                }
                2 => {
                    j.push(b"{\n  \"nvidia_present\": ");
                    j.push_bool(st.pci_present != 0);
                    j.push(b",\n  \"bar_layout_ok\": ");
                    j.push_bool(st.bar_layout_ok != 0);
                    j.push(b",\n  \"gpu_devices\": [\n");
                    let mut first = true;
                    for r in pci_recs.iter().filter(|r| r.class_code == 0x03) {
                        if !first { j.push(b",\n"); }
                        first = false;
                        j.push(b"    {\"bdf\": ");
                        j.push_hex(r.bus_dev_fn as u64, 4);
                        j.push(b", \"vendor\": ");
                        j.push_hex(r.vendor_id as u64, 4);
                        j.push(b", \"device\": ");
                        j.push_hex(r.device_id as u64, 4);
                        j.push(b", \"bar0_bytes\": ");
                        j.push_u64(r.bar0_size_bytes);
                        j.push(b", \"extra_flags\": ");
                        j.push_u64(r.extra_flags as u64);
                        j.push(b"}");
                    }
                    j.push(b"\n  ]\n}\n");
                }
                _ => {
                    j.push(b"{\n  \"hwval_flags\": ");
                    j.push_hex(st.hwval_flags as u64, 8);
                    j.push(b",\n  \"vm_restarts\": ");
                    j.push_u64(st.vm_restarts as u64);
                    j.push(b",\n  \"corrupt_ipc_rejected\": ");
                    j.push_bool(st.corrupt_ipc_rejected != 0);
                    j.push(b",\n  \"unauth_cap_rejected\": ");
                    j.push_bool(st.unauth_cap_rejected != 0);
                    j.push(b",\n  \"timeout_handled\": ");
                    j.push_bool(st.timeout_handled != 0);
                    j.push(b",\n  \"last_ipc_err\": ");
                    j.push_i64(st.last_ipc_err);
                    j.push(b"\n}\n");
                }
            }
            let n = j.as_bytes().len();
            &scratch[..n]
        }
    }
}

fn render_nv_manifest<'a>(digests: &[[u8; 32]; 7], scratch: &'a mut [u8]) -> &'a [u8] {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut j = JsonBuf::new(scratch);
    for (idx, name) in NV_EXP_FILENAMES.iter().enumerate() {
        for &byte in &digests[idx] {
            j.push(&[H[(byte >> 4) as usize], H[(byte & 0xf) as usize]]);
        }
        j.push(b"  ");
        j.push(name);
        j.push(b"\n");
    }
    let n = j.as_bytes().len();
    &scratch[..n]
}

#[inline(never)]
pub fn export_nvidia_logs(
    usb_sup_cap: u64,
    st: &mut NvIslandStatusWire,
    prereqs: &[NvPrereqWire; NV_PREREQ_COUNT],
    pci_recs: &[BootPciFuncRecord],
    scratch: &mut [u8; 16384],
) -> Result<u8, isize> {
    let ul = query_usb_log_status(usb_sup_cap);
    if usb_sup_cap == 0
        || (ul.gate_mask & ANCESTRY_ALL_GATES) != ANCESTRY_ALL_GATES
        || ul.fat_mounted == 0
        || ul.fat_corrupt != 0
        || ul.read_only != 0
    {
        st.export_ok = 0;
        st.export_files = 0;
        return Err(if ul.fat_corrupt != 0 { -5 } else { -19 });
    }

    let sess = st.session_bytes();
    let sess_name: &[u8] = if sess.is_empty() { b"sess-nv01" } else { sess };
    let mut sess_copy = [0u8; 20];
    let sn = sess_name.len().min(20);
    sess_copy[..sn].copy_from_slice(&sess_name[..sn]);

    let mut digests = [[0u8; 32]; 7];
    for (idx, _) in NV_EXP_FILENAMES.iter().enumerate() {
        let payload = render_nv_file(idx, st, prereqs, pci_recs, scratch);
        digests[idx] = compute_sha256(payload);
    }
    let expected_man_sha = {
        let man = render_nv_manifest(&digests, scratch);
        compute_sha256(man)
    };
    let mut man_path = LineBuilder::new();
    man_path.push_bytes(&sess_copy[..sn]);
    man_path.push_bytes(b"/nvidia_exp/manifest.sha256");

    if st.export_ok != 0 {
        let got_n = read_usb_file(usb_sup_cap, man_path.as_bytes(), scratch);
        if got_n > 0 && compute_sha256(&scratch[..got_n]) == expected_man_sha {
            st.export_files = 8;
            return Ok(8);
        }
    }

    let mut committed = 0u8;
    for (idx, &fname) in NV_EXP_FILENAMES.iter().enumerate() {
        let payload = render_nv_file(idx, st, prereqs, pci_recs, scratch);
        let mut rpath = LineBuilder::new();
        rpath.push_bytes(&sess_copy[..sn]);
        rpath.push_bytes(b"/nvidia_exp/");
        rpath.push_bytes(fname);
        if let Err(e) = stage_and_commit_file(usb_sup_cap, payload, rpath.as_bytes(), &digests[idx]) {
            st.export_ok = 0;
            st.export_files = committed;
            return Err(e);
        }
        committed += 1;
    }

    let man = render_nv_manifest(&digests, scratch);
    if let Err(e) = stage_and_commit_file(usb_sup_cap, man, man_path.as_bytes(), &expected_man_sha) {
        st.export_ok = 0;
        st.export_files = committed;
        return Err(e);
    }

    st.export_ok = 1;
    st.export_files = 8;
    Ok(8)
}
