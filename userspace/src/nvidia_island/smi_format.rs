// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared honest formatting for `nvidia-smi`, `nvidia-smi -L`, `nvidia-smi -q`,
//! `dry gpu status`, `dry gpu nvidia status`, and `dry gpu nvidia prereqs`.
//!
//! Consumed identically by `shell_gpu_cmds.rs` and `nvidia_island/session_logs.rs`
//! so terminal UI and USB log bundles share one source of truth with zero fabricated NVML data.

use crate::nvidia_island::protocol::{NvIslandStatusWire, NvPrereqWire, NV_PREREQ_COUNT};
use crate::shell::LineBuilder;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Accent,
    Normal,
    Warn,
}

fn push_bdf(lb: &mut LineBuilder, packed_bdf: u16) {
    let bus = (packed_bdf >> 8) as u64;
    let dev = ((packed_bdf >> 3) & 0x1f) as u64;
    let func = (packed_bdf & 0x7) as u64;
    lb.push_bytes(b"0000:");
    lb.push_hex_width(bus, 2);
    lb.push_byte(b':');
    lb.push_hex_width(dev, 2);
    lb.push_byte(b'.');
    lb.push_u64(func);
}

pub fn emit_nvidia_smi<F: FnMut(&[u8], LineKind)>(st: &NvIslandStatusWire, mut emit: F) {
    emit(b"+--------------------------------------------------------------------------+", LineKind::Accent);
    emit(b"| DRY OS NVIDIA-SMI BRIDGE    Driver Ver: N/A (BLOCKED)   CUDA: N/A        |", LineKind::Accent);
    emit(b"+--------------------------------------------------------------------------+", LineKind::Accent);
    let mut l1 = LineBuilder::new();
    l1.push_bytes(b"| Island: Level ");
    l1.push_u64(st.level as u64);
    l1.push_bytes(b" (DRIVER ISLAND READY)   State: BLOCKED (No OpenRM/GSP)");
    emit(l1.as_bytes(), LineKind::Warn);

    let mut l2 = LineBuilder::new();
    if st.pci_present != 0 {
        l2.push_bytes(b"| GPU 0: ");
        l2.push_hex_width(st.pci_vid as u64, 4);
        l2.push_byte(b':');
        l2.push_hex_width(st.pci_did as u64, 4);
        l2.push_bytes(b" @ ");
        push_bdf(&mut l2, st.pci_bdf);
        l2.push_bytes(b"  BAR0=");
        l2.push_u64(st.pci_bar0_size / (1024 * 1024));
        l2.push_bytes(b" MiB (PASSIVE)");
    } else {
        l2.push_bytes(b"| GPU 0: None detected (BLOCKED - no 10de:* PCI display controller)");
    }
    emit(l2.as_bytes(), LineKind::Normal);
    emit(b"| Temp: N/A   Perf: N/A   Pwr: N/A / N/A   Memory-Usage: N/A   Util: N/A   |", LineKind::Normal);

    let mut l4 = LineBuilder::new();
    l4.push_bytes(b"| Prereqs met=");
    l4.push_u64(st.met_count as u64);
    l4.push_byte(b'/');
    l4.push_u64((st.met_count + st.blocked_count) as u64);
    l4.push_bytes(b" island_spawned=");
    l4.push_u64(st.spawned as u64);
    l4.push_bytes(b" reason=");
    l4.push_bytes(st.reason_bytes());
    emit(l4.as_bytes(), LineKind::Warn);
    emit(b"+--------------------------------------------------------------------------+", LineKind::Accent);
}

pub fn emit_nvidia_smi_list<F: FnMut(&[u8], LineKind)>(st: &NvIslandStatusWire, mut emit: F) {
    if st.pci_present != 0 {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"GPU 0: PCI ");
        lb.push_hex_width(st.pci_vid as u64, 4);
        lb.push_byte(b':');
        lb.push_hex_width(st.pci_did as u64, 4);
        lb.push_bytes(b" BDF=");
        push_bdf(&mut lb, st.pci_bdf);
        lb.push_bytes(b" (Status: BLOCKED Level ");
        lb.push_u64(st.level as u64);
        lb.push_bytes(b", UUID: REDACTED)");
        emit(lb.as_bytes(), LineKind::Warn);
    } else {
        emit(
            b"No active NVIDIA GPU bound (BLOCKED - no 10de:* PCI device / Level 1 Control Island only)",
            LineKind::Warn,
        );
    }
}

pub fn emit_nvidia_smi_query<F: FnMut(&[u8], LineKind)>(
    st: &NvIslandStatusWire,
    prereqs: &[NvPrereqWire; NV_PREREQ_COUNT],
    mut emit: F,
) {
    emit(b"============== NVSMI LOG (DRY OS HONEST BRIDGE) ==============", LineKind::Accent);
    emit(b"Driver Version                  : N/A (BLOCKED - OpenRM unsupported)", LineKind::Warn);
    emit(b"CUDA Version                    : N/A (UNSUPPORTED)", LineKind::Warn);
    let mut l1 = LineBuilder::new();
    l1.push_bytes(b"Island Level                    : Level ");
    l1.push_u64(st.level as u64);
    l1.push_bytes(b" - DRIVER ISLAND READY (status=BLOCKED)");
    emit(l1.as_bytes(), LineKind::Normal);

    let mut l2 = LineBuilder::new();
    l2.push_bytes(b"PCI Device / BAR0               : ");
    if st.pci_present != 0 {
        l2.push_hex_width(st.pci_vid as u64, 4);
        l2.push_byte(b':');
        l2.push_hex_width(st.pci_did as u64, 4);
        l2.push_bytes(b" bar0_bytes=");
        l2.push_u64(st.pci_bar0_size);
        l2.push_bytes(b" bar_ok=");
        l2.push_u64(st.bar_layout_ok as u64);
    } else {
        l2.push_bytes(b"None detected (N/A)");
    }
    emit(l2.as_bytes(), LineKind::Normal);
    emit(b"FB Memory / Temp / Power / Util : N/A (BLOCKED - zero MMIO/NVML)", LineKind::Normal);
    emit(b"Prerequisites Summary           :", LineKind::Accent);
    for p in prereqs.iter() {
        let mut pl = LineBuilder::new();
        pl.push_bytes(if p.met != 0 { b"  [MET]     " } else { b"  [BLOCKED] " });
        pl.push_bytes(p.name_bytes());
        pl.push_bytes(b" -> ");
        pl.push_bytes(p.detail_bytes());
        emit(pl.as_bytes(), if p.met != 0 { LineKind::Normal } else { LineKind::Warn });
    }
}

pub fn emit_dry_gpu_status<F: FnMut(&[u8], LineKind)>(st: &NvIslandStatusWire, mut emit: F) {
    emit(b"DRY OS GPU Subsystem Readiness (Passive Non-Destructive Status)", LineKind::Accent);
    let mut gop = LineBuilder::new();
    gop.push_bytes(b"  UEFI GOP Scanout : ");
    gop.push_bytes(if st.gop_ok != 0 { b"ACTIVE (preserved)" } else { b"UNAVAILABLE" });
    emit(gop.as_bytes(), if st.gop_ok != 0 { LineKind::Normal } else { LineKind::Warn });

    let mut ig = LineBuilder::new();
    ig.push_bytes(b"  Intel Iris Xe    : ");
    if st.igpu_vid != 0 {
        ig.push_hex_width(st.igpu_vid as u64, 4);
        ig.push_byte(b':');
        ig.push_hex_width(st.igpu_did as u64, 4);
        ig.push_bytes(b" (PASSIVE contracts ok, ALLOW_PHYSICAL_GPU_WRITES=false)");
    } else {
        ig.push_bytes(b"Absent in VM; Gen12 contracts verified (ALLOW_PHYSICAL_GPU_WRITES=false)");
    }
    emit(ig.as_bytes(), LineKind::Normal);

    let mut vg = LineBuilder::new();
    vg.push_bytes(b"  VirtIO-GPU 2D/3D : ");
    vg.push_bytes(if st.vgpu_present != 0 { b"Present (1af4:1050)" } else { b"Not attached" });
    emit(vg.as_bytes(), LineKind::Normal);

    let mut nv = LineBuilder::new();
    nv.push_bytes(b"  NVIDIA GA107     : Level ");
    nv.push_u64(st.level as u64);
    nv.push_bytes(b" BLOCKED spawned=");
    nv.push_u64(st.spawned as u64);
    nv.push_bytes(b" met=");
    nv.push_u64(st.met_count as u64);
    nv.push_bytes(b"/14 reason=");
    nv.push_bytes(st.reason_bytes());
    emit(nv.as_bytes(), LineKind::Warn);
}

pub fn emit_nvidia_prereqs<F: FnMut(&[u8], LineKind)>(
    st: &NvIslandStatusWire,
    prereqs: &[NvPrereqWire; NV_PREREQ_COUNT],
    mut emit: F,
) {
    let mut hdr = LineBuilder::new();
    hdr.push_bytes(b"NVIDIA Driver Island Prerequisites: ");
    hdr.push_u64(st.met_count as u64);
    hdr.push_bytes(b"/14 MET, ");
    hdr.push_u64(st.blocked_count as u64);
    hdr.push_bytes(b" BLOCKED (Level ");
    hdr.push_u64(st.level as u64);
    hdr.push_byte(b')');
    emit(hdr.as_bytes(), LineKind::Accent);
    for p in prereqs.iter() {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"  #");
        if p.idx < 10 { lb.push_byte(b'0'); }
        lb.push_u64(p.idx as u64);
        lb.push_bytes(if p.met != 0 { b" [MET]     " } else { b" [BLOCKED] " });
        lb.push_bytes(p.name_bytes());
        lb.push_bytes(b" : ");
        lb.push_bytes(p.detail_bytes());
        emit(lb.as_bytes(), if p.met != 0 { LineKind::Normal } else { LineKind::Warn });
    }
}
