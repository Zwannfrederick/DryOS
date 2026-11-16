//! Linux-Grade Read-Only System, Hardware, Storage & Driver Shell Commands (`shell_sys_cmds`).
//!
//! Invoked by `dry-shell` (`SecDomain::Userspace`) via `shell_ext::exec_storage_vault_command`,
//! using only `SYS_SYSINFO` (`dmesg`/`uname`/`free`), `SYS_PROC_LIST` (`drvctl`), and sanitized
//! supervisor telemetry IPCs (`IPC_OP_SUP_QUERY_*`) with zero ambient hardware capabilities.

use core::mem::size_of;

use crate::abi_ext::sys_dmesg_read;
use crate::nvme_ro::{NvmeInfo, NVME_FLAG_GPT_OK, NVME_FLAG_READY, NVME_FLAG_VMD_GATED};
use crate::shell::{LineBuilder, ShellEngine};
use crate::sup_hw_services::{
    SupHwSummary, PERSIST_BACKEND_NONE, PERSIST_BACKEND_USB_MSC, PERSIST_BACKEND_VIRTIO_BLK,
};
use crate::surface::{STYLE_ACCENT, STYLE_DEFAULT, STYLE_MUTED, STYLE_OK, STYLE_WARN};
use crate::usb::{
    UsbSummaryInfo, USB_CLASS_HID, USB_CLASS_MASS_STORAGE, USB_FLAG_DRYFS_VERIFIED,
    USB_FLAG_KBD_READY, USB_FLAG_MOUSE_READY, USB_FLAG_MSC_READY, USB_FLAG_READY, USB_PROTO_BOT,
    USB_PROTO_KBD, USB_PROTO_MOUSE, USB_SUBCLASS_BOOT,
};
use crate::vfs_proto::sys_ipc_call_extra;
use crate::{
    msg_tag_label, sys_proc_list, sys_sysinfo, sys_write_debug, BootPciFuncRecord, UserProcInfo,
    UserSysInfo, IPC_OP_PONG, IPC_OP_SUP_QUERY_HW, IPC_OP_SUP_QUERY_NVME, IPC_OP_SUP_QUERY_USB,
    MAX_IPC_EXTRA_BYTES,
};

fn emit_dbg(lb: &LineBuilder) {
    let mut out = [0u8; 196];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn query_hw_summary(sup_ep_cap: u64) -> Option<SupHwSummary> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, rlen, _) =
        sys_ipc_call_extra(sup_ep_cap, IPC_OP_SUP_QUERY_HW, [0, 0, 0, 0], &[], &mut rx, 0).ok()?;
    if msg_tag_label(tag) != IPC_OP_PONG {
        return None;
    }
    SupHwSummary::from_bytes(&rx[..rlen])
}

fn query_usb_summary(sup_ep_cap: u64) -> Option<UsbSummaryInfo> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, rlen, _) =
        sys_ipc_call_extra(sup_ep_cap, IPC_OP_SUP_QUERY_USB, [0; 4], &[], &mut rx, 0).ok()?;
    if msg_tag_label(tag) != IPC_OP_PONG {
        return None;
    }
    UsbSummaryInfo::from_bytes(&rx[..rlen])
}

fn query_nvme_summary(sup_ep_cap: u64) -> Option<NvmeInfo> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, rlen, _) =
        sys_ipc_call_extra(sup_ep_cap, IPC_OP_SUP_QUERY_NVME, [0; 4], &[], &mut rx, 0).ok()?;
    if msg_tag_label(tag) != IPC_OP_PONG {
        return None;
    }
    NvmeInfo::from_bytes(&rx[..rlen])
}

pub fn exec_sys_diag_command(
    sup_ep_cap: u64,
    verb: &[u8],
    rest: &[u8],
    engine: &mut ShellEngine,
) -> bool {
    match verb {
        b"dmesg" => exec_dmesg(engine),
        b"lspci" => exec_lspci(sup_ep_cap, engine),
        b"lsusb" => exec_lsusb(sup_ep_cap, engine),
        b"lsblk" => exec_lsblk(sup_ep_cap, engine),
        b"nvme" => exec_nvme(sup_ep_cap, engine),
        b"drvctl" => exec_drvctl(sup_ep_cap, engine),
        b"uname" => exec_uname(rest, engine),
        b"free" => exec_free(engine),
        b"mounts" => exec_mounts(sup_ep_cap, engine),
        _ => return false,
    }
    true
}

fn exec_dmesg(engine: &mut ShellEngine) {
    let mut sys = UserSysInfo::default();
    let mut buf = [0u8; 4096];
    let Ok(n) = sys_dmesg_read(&mut sys, &mut buf) else {
        engine.push_line(b"dmesg: read failed", STYLE_WARN);
        return;
    };
    let slice = &buf[..n.min(buf.len())];
    let mut lines = 0u64;
    let mut start = 0usize;
    // Count total lines first so only the newest 40 lines scroll into the terminal viewport.
    let mut total_lines = 0usize;
    for &b in slice {
        if b == b'\n' {
            total_lines += 1;
        }
    }
    let skip = total_lines.saturating_sub(40);
    let mut idx = 0usize;
    for i in 0..=slice.len() {
        if i == slice.len() || slice[i] == b'\n' {
            if i > start {
                if idx >= skip {
                    let mut seg = &slice[start..i];
                    if seg.last() == Some(&b'\r') {
                        seg = &seg[..seg.len() - 1];
                    }
                    engine.push_line(seg, STYLE_DEFAULT);
                }
                idx += 1;
                lines += 1;
            }
            start = i + 1;
        }
    }
    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=dmesg bytes=");
    slb.push_u64(n as u64);
    slb.push_bytes(b" lines=");
    slb.push_u64(lines);
    emit_dbg(&slb);
}

fn exec_lspci(sup_ep_cap: u64, engine: &mut ShellEngine) {
    engine.push_line(b"BDF       CLASS    VENDOR:DEV  BAR0_SZ   SLOT", STYLE_ACCENT);
    let mut printed = 0u64;
    let mut start = 0u64;
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];

    loop {
        let Ok((tag, rlen, mrs)) =
            sys_ipc_call_extra(sup_ep_cap, IPC_OP_SUP_QUERY_HW, [1, start, 0, 0], &[], &mut rx, 0)
        else {
            engine.push_line(b"lspci: supervisor query failed", STYLE_WARN);
            return;
        };
        if msg_tag_label(tag) != IPC_OP_PONG {
            break;
        }
        let total = mrs[0];
        let batch = (mrs[1] as usize).min(rlen / size_of::<BootPciFuncRecord>());
        if batch == 0 {
            break;
        }
        for i in 0..batch {
            let mut rec = BootPciFuncRecord::default();
            // SAFETY: `rx` holds `batch * 32` bytes (`BootPciFuncRecord` POD).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    rx[i * size_of::<BootPciFuncRecord>()..].as_ptr(),
                    (&raw mut rec).cast::<u8>(),
                    size_of::<BootPciFuncRecord>(),
                );
            }
            let mut lb = LineBuilder::new();
            lb.push_hex_width((rec.bus_dev_fn >> 8) as u64, 2);
            lb.push_byte(b':');
            lb.push_hex_width(((rec.bus_dev_fn >> 3) & 0x1f) as u64, 2);
            lb.push_byte(b'.');
            lb.push_u64((rec.bus_dev_fn & 7) as u64);
            lb.push_bytes(b"  [");
            lb.push_hex_width(rec.class_code as u64, 2);
            lb.push_byte(b':');
            lb.push_hex_width(rec.subclass as u64, 2);
            lb.push_byte(b':');
            lb.push_hex_width(rec.prog_if as u64, 2);
            lb.push_bytes(b"] ");
            lb.push_hex_width(rec.vendor_id as u64, 4);
            lb.push_byte(b':');
            lb.push_hex_width(rec.device_id as u64, 4);
            lb.push_bytes(b"  ");
            lb.push_u64(rec.bar0_size_bytes);
            lb.push_bytes(b"B slot=");
            lb.push_u64(rec.managed_slot as u64);
            engine.push_line(lb.as_bytes(), STYLE_DEFAULT);
            printed += 1;
        }
        start = mrs[2];
        if start >= total {
            break;
        }
    }

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=lspci count=");
    slb.push_u64(printed);
    emit_dbg(&slb);
}

fn exec_lsusb(sup_ep_cap: u64, engine: &mut ShellEngine) {
    let Some(s) = query_usb_summary(sup_ep_cap) else {
        engine.push_line(b"lsusb: USB telemetry unavailable", STYLE_WARN);
        return;
    };
    if (s.state_flags & USB_FLAG_READY) == 0 {
        engine.push_line(b"No xHCI USB controller active", STYLE_MUTED);
        let _ = sys_write_debug(b"[DRY-SHELL] CMD=lsusb devs=0 ready=0\n");
        return;
    }

    let mut hdr = LineBuilder::new();
    hdr.push_bytes(b"xHCI ");
    hdr.push_hex_width(s.controller_vid as u64, 4);
    hdr.push_byte(b':');
    hdr.push_hex_width(s.controller_did as u64, 4);
    hdr.push_bytes(b" ports=");
    hdr.push_u64(s.connected_ports as u64);
    hdr.push_byte(b'/');
    hdr.push_u64(s.max_ports as u64);
    hdr.push_bytes(b" irqs=");
    hdr.push_u64(s.irq_count);
    hdr.push_bytes(b" kbd_ev=");
    hdr.push_u64(s.kbd_events);
    hdr.push_bytes(b" mouse_ev=");
    hdr.push_u64(s.mouse_events);
    engine.push_line(hdr.as_bytes(), STYLE_ACCENT);

    let count = (s.device_count as usize).min(s.devices.len());
    for dev in &s.devices[..count] {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"Bus 001 Port ");
        lb.push_u64(dev.port as u64);
        lb.push_bytes(b" Slot ");
        lb.push_u64(dev.slot_id as u64);
        lb.push_bytes(b" Spd ");
        lb.push_u64(dev.speed as u64);
        lb.push_bytes(b" ID ");
        lb.push_hex_width(dev.vendor_id as u64, 4);
        lb.push_byte(b':');
        lb.push_hex_width(dev.product_id as u64, 4);
        lb.push_bytes(b" ");
        if dev.class_code == USB_CLASS_HID && dev.subclass == USB_SUBCLASS_BOOT {
            if dev.proto == USB_PROTO_KBD {
                lb.push_bytes(b"HID Boot Keyboard");
            } else if dev.proto == USB_PROTO_MOUSE {
                lb.push_bytes(b"HID Boot Mouse");
            } else {
                lb.push_bytes(b"HID Boot Device");
            }
        } else if dev.class_code == USB_CLASS_MASS_STORAGE && dev.proto == USB_PROTO_BOT {
            lb.push_bytes(b"USB Mass Storage BOT");
        } else {
            lb.push_bytes(b"USB Class ");
            lb.push_hex_width(dev.class_code as u64, 2);
        }
        engine.push_line(lb.as_bytes(), STYLE_DEFAULT);
    }

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=lsusb devs=");
    slb.push_u64(count as u64);
    slb.push_bytes(b" kbd=");
    slb.push_u64(u64::from((s.state_flags & USB_FLAG_KBD_READY) != 0));
    slb.push_bytes(b" mouse=");
    slb.push_u64(u64::from((s.state_flags & USB_FLAG_MOUSE_READY) != 0));
    slb.push_bytes(b" msc=");
    slb.push_u64(u64::from((s.state_flags & USB_FLAG_MSC_READY) != 0));
    slb.push_bytes(b" dryfs=");
    slb.push_u64(u64::from((s.state_flags & USB_FLAG_DRYFS_VERIFIED) != 0));
    slb.push_bytes(b" part2=");
    slb.push_u64(s.msc_part2_sectors);
    slb.push_bytes(b" kbd_ev=");
    slb.push_u64(s.kbd_events);
    slb.push_bytes(b" mouse_ev=");
    slb.push_u64(s.mouse_events);
    emit_dbg(&slb);
}

fn exec_lsblk(sup_ep_cap: u64, engine: &mut ShellEngine) {
    let hw = query_hw_summary(sup_ep_cap).unwrap_or_else(SupHwSummary::empty);
    let usb = query_usb_summary(sup_ep_cap).unwrap_or_else(UsbSummaryInfo::empty);
    let nvme = query_nvme_summary(sup_ep_cap).unwrap_or_else(NvmeInfo::empty);

    engine.push_line(b"NAME     TYPE   SECTORS    RO  MOUNT/STATE", STYLE_ACCENT);
    if hw.blk_sectors > 0 {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"vda      disk   ");
        lb.push_u64(hw.blk_sectors);
        lb.push_bytes(if hw.persist_backend == PERSIST_BACKEND_VIRTIO_BLK && hw.persist_ro != 0 {
            b"   1   /persist (DRYFS_V1)"
        } else if hw.persist_backend == PERSIST_BACKEND_VIRTIO_BLK {
            b"   0   /persist (DRYFS_V1)"
        } else {
            b"   0   unmounted"
        });
        engine.push_line(lb.as_bytes(), STYLE_DEFAULT);
    }
    if usb.msc_total_sectors > 0 {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"sda      disk   ");
        lb.push_u64(usb.msc_total_sectors);
        lb.push_bytes(b"   ");
        lb.push_u64(u64::from(usb.msc_ro != 0));
        lb.push_bytes(b"   USB-BOT");
        engine.push_line(lb.as_bytes(), STYLE_DEFAULT);
        if usb.msc_part2_sectors > 0 {
            let mut plb = LineBuilder::new();
            plb.push_bytes(b"  sda2   part   ");
            plb.push_u64(usb.msc_part2_sectors);
            plb.push_bytes(b"   ");
            plb.push_u64(u64::from(hw.persist_ro != 0));
            if hw.persist_backend == PERSIST_BACKEND_USB_MSC {
                plb.push_bytes(b"   /persist (DRYFS_V1)");
            } else {
                plb.push_bytes(b"   DRYFS_V1");
            }
            engine.push_line(plb.as_bytes(), STYLE_DEFAULT);
        }
    }
    if (nvme.state_flags & NVME_FLAG_READY) != 0 {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"nvme0n1  disk   ");
        lb.push_u64(nvme.nsze_sectors);
        lb.push_bytes(b"   1   READ-ONLY (unmounted)");
        engine.push_line(lb.as_bytes(), STYLE_DEFAULT);
    }

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=lsblk backend=");
    slb.push_u64(hw.persist_backend as u64);
    slb.push_bytes(b" ro=");
    slb.push_u64(hw.persist_ro as u64);
    slb.push_bytes(b" vda=");
    slb.push_u64(hw.blk_sectors);
    slb.push_bytes(b" sda2=");
    slb.push_u64(usb.msc_part2_sectors);
    slb.push_bytes(b" nvme=");
    slb.push_u64(nvme.nsze_sectors);
    emit_dbg(&slb);
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == b' ' || bytes[end - 1] == 0) {
        end -= 1;
    }
    &bytes[..end]
}

fn exec_nvme(sup_ep_cap: u64, engine: &mut ShellEngine) {
    let Some(n) = query_nvme_summary(sup_ep_cap) else {
        engine.push_line(b"nvme: telemetry unavailable", STYLE_WARN);
        return;
    };
    if (n.state_flags & NVME_FLAG_VMD_GATED) != 0 {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"VMD_DOMAIN_GATED (");
        lb.push_hex_width(n.vendor_id as u64, 4);
        lb.push_byte(b':');
        lb.push_hex_width(n.device_id as u64, 4);
        lb.push_bytes(b") -- Host NVMe Untouched (Read-Only)");
        engine.push_line(lb.as_bytes(), STYLE_WARN);
        let _ = sys_write_debug(b"[DRY-SHELL] CMD=nvme status=VMD_DOMAIN_GATED\n");
        return;
    }
    if (n.state_flags & NVME_FLAG_READY) == 0 {
        engine.push_line(b"No NVMe controller present", STYLE_MUTED);
        let _ = sys_write_debug(b"[DRY-SHELL] CMD=nvme status=NONE\n");
        return;
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"nvme0n1: model=\"");
    lb.push_bytes(trim_ascii(&n.model));
    lb.push_bytes(b"\" serial=\"");
    lb.push_bytes(trim_ascii(&n.serial));
    lb.push_bytes(b"\" sectors=");
    lb.push_u64(n.nsze_sectors);
    lb.push_bytes(b" lba=");
    lb.push_u64(n.lba_bytes as u64);
    lb.push_bytes(b"B gpt=");
    lb.push_bytes(if (n.state_flags & NVME_FLAG_GPT_OK) != 0 { b"OK" } else { b"NO" });
    lb.push_bytes(b" [READ-ONLY]");
    engine.push_line(lb.as_bytes(), STYLE_OK);

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=nvme status=READY nsze=");
    slb.push_u64(n.nsze_sectors);
    slb.push_bytes(b" gpt=");
    slb.push_u64(u64::from((n.state_flags & NVME_FLAG_GPT_OK) != 0));
    slb.push_bytes(b" ro=1");
    emit_dbg(&slb);
}

fn exec_drvctl(sup_ep_cap: u64, engine: &mut ShellEngine) {
    let hw = query_hw_summary(sup_ep_cap).unwrap_or_else(SupHwSummary::empty);
    let mut procs = [UserProcInfo::default(); 32];
    let (written, total) = sys_proc_list(&mut procs).unwrap_or((0, 0));

    let mut hdr = LineBuilder::new();
    hdr.push_bytes(b"Ring-3 Drivers: usb_pid=");
    hdr.push_u64(hw.usb_pid as u64);
    hdr.push_bytes(b" nvme_pid=");
    hdr.push_u64(hw.nvme_pid as u64);
    hdr.push_bytes(b" storage_ro=");
    hdr.push_u64(hw.persist_ro as u64);
    hdr.push_bytes(b" vmd_gated=");
    hdr.push_u64(hw.vmd_gated as u64);
    engine.push_line(hdr.as_bytes(), STYLE_ACCENT);

    for p in &procs[..written.min(12)] {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"  pid=");
        lb.push_u64(p.pid as u64);
        lb.push_bytes(b" dom=");
        lb.push_u64(p.domain as u64);
        lb.push_bytes(b" comp=0x");
        lb.push_hex_width(p.compartment_id as u64, 4);
        lb.push_bytes(b" pages=");
        lb.push_u64(p.mapped_pages_4k as u64);
        lb.push_bytes(b" syscalls=");
        lb.push_u64(p.syscall_entries);
        engine.push_line(lb.as_bytes(), STYLE_DEFAULT);
    }

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=drvctl procs=");
    slb.push_u64(total as u64);
    slb.push_bytes(b" usb=");
    slb.push_u64(hw.usb_pid as u64);
    slb.push_bytes(b" nvme=");
    slb.push_u64(hw.nvme_pid as u64);
    slb.push_bytes(b" ro=");
    slb.push_u64(hw.persist_ro as u64);
    emit_dbg(&slb);
}

fn exec_uname(rest: &[u8], engine: &mut ShellEngine) {
    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let mut lb = LineBuilder::new();
    if rest == b"-r" {
        lb.push_bytes(b"0.2.0-dryos-m2");
    } else if rest == b"-m" {
        lb.push_bytes(b"x86_64");
    } else {
        lb.push_bytes(b"DryOS 0.2.0-dryos-m2 x86_64 Capability-Microkernel (smp=");
        lb.push_u64(sys.online_cpus as u64);
        lb.push_bytes(b", Ring-3 Drivers)");
    }
    engine.push_line(lb.as_bytes(), STYLE_DEFAULT);

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=uname sys=DryOS arch=x86_64 smp=");
    slb.push_u64(sys.online_cpus as u64);
    emit_dbg(&slb);
}

fn exec_free(engine: &mut ShellEngine) {
    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let total_kib = (sys.usable_frames_4k as u64) * 4;
    let free_kib = (sys.free_frames_4k as u64) * 4;
    let used_kib = total_kib.saturating_sub(free_kib);

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"Mem: total=");
    lb.push_u64(total_kib);
    lb.push_bytes(b" KiB  used=");
    lb.push_u64(used_kib);
    lb.push_bytes(b" KiB  free=");
    lb.push_u64(free_kib);
    lb.push_bytes(b" KiB (4K: ");
    lb.push_u64(sys.free_frames_4k as u64);
    lb.push_byte(b'/');
    lb.push_u64(sys.usable_frames_4k as u64);
    lb.push_byte(b')');
    engine.push_line(lb.as_bytes(), STYLE_DEFAULT);

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=free total_kib=");
    slb.push_u64(total_kib);
    slb.push_bytes(b" used_kib=");
    slb.push_u64(used_kib);
    slb.push_bytes(b" free_kib=");
    slb.push_u64(free_kib);
    emit_dbg(&slb);
}

fn exec_mounts(sup_ep_cap: u64, engine: &mut ShellEngine) {
    let hw = query_hw_summary(sup_ep_cap).unwrap_or_else(SupHwSummary::empty);
    engine.push_line(b"/boot     tmpfs     ro   boot-modules", STYLE_DEFAULT);
    engine.push_line(b"/ram      tmpfs     rw   volatile", STYLE_DEFAULT);
    let mut lb = LineBuilder::new();
    let bname: &[u8] = match hw.persist_backend {
        PERSIST_BACKEND_VIRTIO_BLK => b"virtio-blk",
        PERSIST_BACKEND_USB_MSC => b"usb-msc-p2",
        _ => b"none",
    };
    if hw.persist_backend != PERSIST_BACKEND_NONE {
        lb.push_bytes(b"/persist  DRYFS_V1  ");
        lb.push_bytes(if hw.persist_ro != 0 { b"ro   " } else { b"rw   " });
        lb.push_bytes(bname);
        lb.push_bytes(b" secs=");
        lb.push_u64(hw.persist_sectors);
        lb.push_bytes(b" boot_log=");
        lb.push_u64(hw.boot_log_bytes as u64);
        lb.push_byte(b'B');
        engine.push_line(lb.as_bytes(), STYLE_OK);
    } else {
        engine.push_line(b"/persist  none      -    not mounted", STYLE_MUTED);
    }

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=mounts persist=");
    slb.push_u64(u64::from(hw.persist_backend != PERSIST_BACKEND_NONE));
    slb.push_bytes(b" backend=");
    slb.push_bytes(bname);
    slb.push_bytes(b" ro=");
    slb.push_u64(hw.persist_ro as u64);
    emit_dbg(&slb);
}
