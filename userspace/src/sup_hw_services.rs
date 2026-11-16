//! Supervisor Hardware Orchestration (`dry-usb`, `dry-nvme`, VMD, `/persist` backend selection,
//! `/persist/boot.log` persistence, and read-only telemetry IPCs for `dry-shell`).

use core::mem::size_of;
use core::ptr::read_volatile;

use crate::abi_ext::{
    sys_dmesg_read, sys_ipc_reply_with_grant_lease, RootInitBootExtInfo, UserPciBarInfo,
    BOOT_EXT_INFO_OFFSET, BOOT_FLAG_STORAGE_RO, BOOT_FLAG_USB_PERSIST_PREFER,
    ROOT_BOOT_EXT_MAGIC, BLK_OP_QUERY_INFO, VFS_OP_MOUNT_BLK,
};
use crate::init_legacy::{domain_spec, log_line};
use crate::lx_compat_meta::read_boot_pci_funcs;
use crate::nvme_ro::{NvmeInfo, NVME_FLAG_READY, NVME_FLAG_VMD_GATED};
use crate::shell::LineBuilder;
use crate::sup_services::{restore_prefs_from_persist, set_persist_read_only, SupervisorPrefs};
use crate::usb::{UsbSummaryInfo, USB_FLAG_DRYFS_VERIFIED, USB_FLAG_MSC_READY, USB_FLAG_READY};
use crate::vfs_proto::{
    sys_ipc_call_extra, sys_ipc_reply_extra, vfs_create_file, vfs_sync, vfs_write_small,
};
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_reply_inline, sys_proc_spawn,
    sys_proc_wait, BootPciFuncRecord, RootInitBootInfo, UserIpcSendExt, UserSysInfo,
    BADGE_BLK_VFS, BADGE_INPUT_SVC, BADGE_SUPERVISOR, COMP_ID_NVME_DRV, COMP_ID_USB_DRV, EEXIST,
    EINVAL, EPERM, IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN,
    IPC_OP_PONG, IPC_OP_SUP_QUERY_BLK, IPC_OP_SUP_QUERY_HW, IPC_OP_SUP_QUERY_NVME,
    IPC_OP_SUP_QUERY_USB, MAX_BOOT_PCI_FUNCS, MAX_IPC_EXTRA_BYTES, NVME_OP_QUERY_INFO, RIGHT_CALL,
    RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_WRITE,
    SYS_IPC_CALL, USB_OP_QUERY_SUMMARY, USB_OP_SET_WM_SINK, raw_ipc_syscall, msg_tag_extra_len,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;
const BOOT_LOG_PATH: &[u8] = b"/persist/boot.log";

pub const PERSIST_BACKEND_NONE: u8 = 0;
pub const PERSIST_BACKEND_VIRTIO_BLK: u8 = 1;
pub const PERSIST_BACKEND_USB_MSC: u8 = 2;

/// Compact supervisor HW/mount telemetry returned by `IPC_OP_SUP_QUERY_HW (sub_op=0)` (`<= 256` B).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SupHwSummary {
    pub boot_flags: u64,
    pub vmd_packed: u64,
    pub dgpu_packed: u64,
    pub pci_count: u32,
    pub usb_pid: u32,
    pub nvme_pid: u32,
    pub blk_pid: u32,
    pub persist_sectors: u64,
    pub blk_sectors: u64,
    pub usb_part2_sectors: u64,
    pub boot_log_bytes: u32,
    pub persist_backend: u8,
    pub persist_ro: u8,
    pub usb_ready: u8,
    pub nvme_ready: u8,
    pub vmd_gated: u8,
    pub blk_ready: u8,
    pub _pad: [u8; 2],
    pub blk_io_ops: u64,
    pub blk_flush_ops: u64,
}

const _: () = assert!(size_of::<SupHwSummary>() <= 256);

impl SupHwSummary {
    pub const fn empty() -> Self {
        Self {
            boot_flags: 0,
            vmd_packed: 0,
            dgpu_packed: 0,
            pci_count: 0,
            usb_pid: 0,
            nvme_pid: 0,
            blk_pid: 0,
            persist_sectors: 0,
            blk_sectors: 0,
            usb_part2_sectors: 0,
            boot_log_bytes: 0,
            persist_backend: PERSIST_BACKEND_NONE,
            persist_ro: 0,
            usb_ready: 0,
            nvme_ready: 0,
            vmd_gated: 0,
            blk_ready: 0,
            _pad: [0; 2],
            blk_io_ops: 0,
            blk_flush_ops: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `SupHwSummary` is `#[repr(C)]` POD (`<= 256` B).
        unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>()) }
    }

    pub fn from_bytes(raw: &[u8]) -> Option<Self> {
        if raw.len() < size_of::<Self>() {
            return None;
        }
        let mut out = Self::empty();
        // SAFETY: `SupHwSummary` is `#[repr(C)]` POD (`<= 256` B).
        unsafe {
            core::ptr::copy_nonoverlapping(
                raw.as_ptr(),
                (&raw mut out).cast::<u8>(),
                size_of::<Self>(),
            );
        }
        Some(out)
    }
}

pub struct HwServicesState {
    pub boot_info_addr: u64,
    pub pci_func_count: u64,
    pub boot_flags: u64,
    pub vmd_packed: u64,
    pub dgpu_packed: u64,
    pub usb_handle: u64,
    pub usb_ep: u64,
    pub usb_sup_cap: u64,
    pub usb_pid: u32,
    pub usb_part2_sectors: u64,
    pub nvme_handle: u64,
    pub nvme_ep: u64,
    pub nvme_sup_cap: u64,
    pub nvme_pid: u32,
    pub blk_sup_cap: u64,
    pub blk_pid: u32,
    pub blk_sectors: u64,
    pub persist_backend: u8,
    pub persist_ro: bool,
    pub persist_sectors: u64,
    pub boot_log_bytes: u32,
}

impl HwServicesState {
    pub const fn empty() -> Self {
        Self {
            boot_info_addr: 0,
            pci_func_count: 0,
            boot_flags: 0,
            vmd_packed: 0,
            dgpu_packed: 0,
            usb_handle: 0,
            usb_ep: 0,
            usb_sup_cap: 0,
            usb_pid: 0,
            usb_part2_sectors: 0,
            nvme_handle: 0,
            nvme_ep: 0,
            nvme_sup_cap: 0,
            nvme_pid: 0,
            blk_sup_cap: 0,
            blk_pid: 0,
            blk_sectors: 0,
            persist_backend: PERSIST_BACKEND_NONE,
            persist_ro: false,
            persist_sectors: 0,
            boot_log_bytes: 0,
        }
    }

    pub fn teardown(&mut self) {
        if self.usb_handle != 0 {
            if self.usb_sup_cap != 0 {
                let _ = sys_ipc_call_inline(
                    self.usb_sup_cap,
                    pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
                    [0; 4],
                    0,
                );
            }
            let _ = sys_proc_wait(self.usb_handle, 0, 0);
            if self.usb_sup_cap != 0 {
                let _ = sys_cap_revoke(self.usb_sup_cap, 1);
            }
            if self.usb_ep != 0 {
                let _ = sys_cap_revoke(self.usb_ep, 2);
            }
            self.usb_handle = 0;
            self.usb_sup_cap = 0;
            self.usb_ep = 0;
        }
        if self.nvme_handle != 0 {
            if self.nvme_sup_cap != 0 {
                let _ = sys_ipc_call_inline(
                    self.nvme_sup_cap,
                    pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
                    [0; 4],
                    0,
                );
            }
            let _ = sys_proc_wait(self.nvme_handle, 0, 0);
            if self.nvme_sup_cap != 0 {
                let _ = sys_cap_revoke(self.nvme_sup_cap, 1);
            }
            if self.nvme_ep != 0 {
                let _ = sys_cap_revoke(self.nvme_ep, 2);
            }
            self.nvme_handle = 0;
            self.nvme_sup_cap = 0;
            self.nvme_ep = 0;
        }
    }
}

pub fn read_boot_ext_info(boot_info_addr: u64) -> Option<RootInitBootExtInfo> {
    if boot_info_addr == 0 {
        return None;
    }
    // SAFETY: Kernel maps `boot_info_addr` `USER_RO` and populates `RootInitBootExtInfo` at `+0x0700`.
    let ext = unsafe {
        read_volatile((boot_info_addr + BOOT_EXT_INFO_OFFSET) as *const RootInitBootExtInfo)
    };
    if ext.magic == ROOT_BOOT_EXT_MAGIC {
        Some(ext)
    } else {
        None
    }
}

pub struct HwInitCaps {
    pub disp_ep: u64,
    pub disp_sup_cap: u64,
    pub input_sup_cap: u64,
    pub vfs_ep: u64,
    pub vfs_sup_cap: u64,
    pub blk_ep: u64,
    pub blk_sup_cap: u64,
    pub blk_sectors: u64,
    pub tsc_mhz: u64,
}

pub fn init_hw_services_and_persist(
    boot_info_addr: u64,
    boot: &RootInitBootInfo,
    caps: HwInitCaps,
    prefs: &mut SupervisorPrefs,
) -> HwServicesState {
    let _ = size_of::<UserPciBarInfo>();
    let _ = size_of::<UserIpcSendExt>();
    let _ = sys_ipc_reply_with_grant_lease;

    let HwInitCaps {
        disp_ep,
        disp_sup_cap,
        input_sup_cap,
        vfs_ep,
        vfs_sup_cap,
        blk_ep,
        blk_sup_cap,
        blk_sectors,
        tsc_mhz,
    } = caps;

    let mut st = HwServicesState::empty();
    st.boot_info_addr = boot_info_addr;
    st.pci_func_count = boot.boot_pci_func_count;
    st.blk_sup_cap = blk_sup_cap;
    st.blk_sectors = blk_sectors;

    let Some(ext) = read_boot_ext_info(boot_info_addr) else {
        // Fallback: preserve exact legacy mount if boot ext block is absent.
        if blk_sectors > 0
            && mount_persist_with_mode(
                vfs_sup_cap,
                blk_ep,
                false,
                disp_sup_cap,
                input_sup_cap,
                tsc_mhz,
                prefs,
            )
        {
            st.persist_backend = PERSIST_BACKEND_VIRTIO_BLK;
            st.persist_sectors = blk_sectors;
        }
        return st;
    };

    st.boot_flags = ext.boot_flags;
    st.vmd_packed = ext.vmd_packed;
    st.dgpu_packed = ext.dgpu_packed;
    st.persist_ro = (ext.boot_flags & BOOT_FLAG_STORAGE_RO) != 0;
    set_persist_read_only(st.persist_ro);

    // 1. Spawn `dry-usb` (xHCI + Boot HID + BOT MSC) if xHCI PCI cap & ELF module are present.
    if ext.usb_mod_cap != 0 && ext.pci_xhci_cap != 0 && boot.session_mode >= 20 {
        if let Ok((h, pid, ep, sup_cap)) =
            spawn_pci_driver(ext.usb_mod_cap, ext.pci_xhci_cap, COMP_ID_USB_DRV, 0x05b0)
        {
            st.usb_handle = h;
            st.usb_pid = pid as u32;
            st.usb_ep = ep;
            st.usb_sup_cap = sup_cap;

            let sink_res = sys_ipc_call_with_grant(
                sup_cap,
                USB_OP_SET_WM_SINK,
                [u64::from(st.persist_ro), prefs.keymap as u64, 0, 0],
                disp_ep,
                RIGHT_READ | RIGHT_SEND | RIGHT_CALL,
                BADGE_INPUT_SVC,
                0,
            );
            if let Ok((_, _, mrs)) = sink_res {
                st.usb_part2_sectors = mrs[0];
            }
        }
    }

    // 2. Spawn strictly read-only `dry-nvme` if NVMe PCI cap is present, else report Intel VMD gating.
    if ext.nvme_mod_cap != 0 && ext.pci_nvme_cap != 0 && boot.session_mode >= 20 {
        if let Ok((h, pid, ep, sup_cap)) =
            spawn_pci_driver(ext.nvme_mod_cap, ext.pci_nvme_cap, COMP_ID_NVME_DRV, 0x4e76)
        {
            st.nvme_handle = h;
            st.nvme_pid = pid as u32;
            st.nvme_ep = ep;
            st.nvme_sup_cap = sup_cap;
        }
    } else if ext.vmd_packed != 0 {
        let did = (ext.vmd_packed & 0xffff) as u16;
        let vid = ((ext.vmd_packed >> 16) & 0xffff) as u16;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-INIT] VMD_DOMAIN_GATED (");
        lb.push_hex_width(vid as u64, 4);
        lb.push_byte(b':');
        lb.push_hex_width(did as u64, 4);
        lb.push_bytes(b") -- Host NVMe Untouched (Read-Only)");
        log_line(&lb);
    }

    // 3. Select `/persist` backend without disturbing existing VirtIO-Blk test sessions.
    let prefer_usb = (ext.boot_flags & BOOT_FLAG_USB_PERSIST_PREFER) != 0 || blk_sectors == 0;
    let (chosen_ep, chosen_kind, chosen_secs) = if !prefer_usb && blk_sectors > 0 {
        (blk_ep, PERSIST_BACKEND_VIRTIO_BLK, blk_sectors)
    } else if st.usb_ep != 0 && st.usb_part2_sectors > 0 {
        (st.usb_ep, PERSIST_BACKEND_USB_MSC, st.usb_part2_sectors)
    } else if blk_sectors > 0 {
        (blk_ep, PERSIST_BACKEND_VIRTIO_BLK, blk_sectors)
    } else {
        (0, PERSIST_BACKEND_NONE, 0)
    };

    if chosen_ep != 0
        && mount_persist_with_mode(
            vfs_sup_cap,
            chosen_ep,
            st.persist_ro,
            disp_sup_cap,
            input_sup_cap,
            tsc_mhz,
            prefs,
        )
    {
        st.persist_backend = chosen_kind;
        st.persist_sectors = chosen_secs;
        // Sync updated keyboard layout to `dry-usb` if `/persist/desktop.cfg` restored layout.
        if st.usb_sup_cap != 0 {
            let _ = sys_ipc_call_inline(
                st.usb_sup_cap,
                pack_msg_tag(USB_OP_SET_WM_SINK, 0, 2, 0),
                [u64::from(st.persist_ro), prefs.keymap as u64, 0, 0],
                0,
            );
        }
        if !st.persist_ro {
            st.boot_log_bytes = persist_boot_log(vfs_ep, vfs_sup_cap) as u32;
        }
    }

    st
}

fn spawn_pci_driver(
    mod_cap: u64,
    pci_cap: u64,
    comp_id: u16,
    badge: u64,
) -> Result<(u64, u64, u64, u64), i32> {
    let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL | RIGHT_GRANT;
    let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

    let ep = sys_ep_create(ipc_spec).map_err(|_| 181)?;
    let sup_cap = sys_cap_derive(ep, cli_rights, 0, BADGE_SUPERVISOR).map_err(|_| 182)?;
    let pci_deleg = sys_cap_derive(pci_cap, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT, 0, 0)
        .map_err(|_| 183)?;
    let (handle, pid) = sys_proc_spawn(
        mod_cap,
        1,
        comp_id,
        (ep, srv_rights, badge),
        (pci_deleg, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP),
    )
    .map_err(|_| 184)?;
    Ok((handle, pid, ep, sup_cap))
}

fn mount_persist_with_mode(
    vfs_sup_cap: u64,
    blk_ep: u64,
    read_only: bool,
    disp_sup_cap: u64,
    input_sup_cap: u64,
    tsc_mhz: u64,
    prefs: &mut SupervisorPrefs,
) -> bool {
    if vfs_sup_cap == 0 || blk_ep == 0 {
        return false;
    }
    let Ok((tag, _, mrs)) = sys_ipc_call_with_grant(
        vfs_sup_cap,
        VFS_OP_MOUNT_BLK,
        [u64::from(read_only), 0, 0, 0],
        blk_ep,
        RIGHT_READ | RIGHT_SEND | RIGHT_CALL | RIGHT_GRANT,
        BADGE_BLK_VFS,
        0,
    ) else {
        return false;
    };
    if msg_tag_label(tag) != IPC_OP_PONG {
        return false;
    }
    let fresh = mrs[0] != 0;
    let tx_seq = mrs[1];
    let recovered = mrs[2] != 0;
    prefs.persist_backed = true;

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=");
    lb.push_bytes(if fresh { b"true" } else { b"false" });
    lb.push_bytes(b" ro=");
    lb.push_bytes(if read_only { b"true" } else { b"false" });
    lb.push_bytes(b" tx_seq=");
    lb.push_u64(tx_seq);
    lb.push_bytes(b" recovered=");
    lb.push_bytes(if recovered { b"true" } else { b"false" });
    log_line(&lb);

    restore_prefs_from_persist(vfs_sup_cap, disp_sup_cap, input_sup_cap, tsc_mhz, prefs);
    true
}

fn persist_boot_log(vfs_ep: u64, vfs_sup_cap: u64) -> usize {
    let mut sys = UserSysInfo::default();
    let mut log_buf = [0u8; 4096];
    let Ok(n) = sys_dmesg_read(&mut sys, &mut log_buf) else {
        return 0;
    };
    if n == 0 {
        return 0;
    }
    // Mint a Userspace-domain cap (`comp=0x000f`) so `dry-shell` (`SecDomain::Userspace`) can read
    // `/persist/boot.log` while remaining unable to tamper with supervisor files or overwrite it.
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let Ok(log_vfs_cap) = sys_cap_derive(vfs_ep, cli_rights, 0, 0x0200_000f) else {
        return 0;
    };
    match vfs_create_file(log_vfs_cap, BOOT_LOG_PATH, 0) {
        Ok(_) | Err(EEXIST) => {}
        Err(_) => {
            let _ = sys_cap_revoke(log_vfs_cap, 1);
            return 0;
        }
    }
    let mut off = 0usize;
    let total = n.min(4096);
    while off < total {
        let chunk = (total - off).min(224);
        let append = off > 0;
        if vfs_write_small(log_vfs_cap, BOOT_LOG_PATH, &log_buf[off..off + chunk], append).is_err()
        {
            break;
        }
        off += chunk;
    }
    let _ = sys_cap_revoke(log_vfs_cap, 1);
    let _ = vfs_sync(vfs_sup_cap);
    off
}

/// Dispatch M2 hardware/PCI/USB/NVMe/mount query IPCs from authorized Ring-3 shell/system apps.
pub fn handle_hw_query_ipc(
    sup_ep: u64,
    reply_tok: u64,
    label: u32,
    mrs: [u64; 4],
    authorized: bool,
    hw: &HwServicesState,
) -> bool {
    match label {
        IPC_OP_SUP_QUERY_HW
        | IPC_OP_SUP_QUERY_USB
        | IPC_OP_SUP_QUERY_NVME
        | IPC_OP_SUP_QUERY_BLK => {}
        _ => return false,
    }

    if !authorized {
        if reply_tok != 0 {
            let _ = sys_ipc_reply_inline(
                sup_ep,
                reply_tok,
                pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                [EPERM as u64, 0, 0, 0],
            );
        }
        return true;
    }

    match label {
        IPC_OP_SUP_QUERY_HW => {
            if mrs[0] == 1 {
                let mut all_recs = [BootPciFuncRecord::default(); MAX_BOOT_PCI_FUNCS];
                let total = read_boot_pci_funcs(hw.boot_info_addr, hw.pci_func_count, &mut all_recs);
                let start = (mrs[1] as usize).min(total);
                let batch = (total - start).min(MAX_IPC_EXTRA_BYTES / size_of::<BootPciFuncRecord>());
                let byte_len = batch * size_of::<BootPciFuncRecord>();
                // SAFETY: `[BootPciFuncRecord]` is `#[repr(C)]` POD (`32` bytes each).
                let bytes = unsafe {
                    core::slice::from_raw_parts(all_recs[start..].as_ptr().cast::<u8>(), byte_len)
                };
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_extra(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [total as u64, batch as u64, (start + batch) as u64, 0],
                        bytes,
                    );
                }
            } else {
                let mut sum = SupHwSummary::empty();
                sum.boot_flags = hw.boot_flags;
                sum.vmd_packed = hw.vmd_packed;
                sum.dgpu_packed = hw.dgpu_packed;
                sum.pci_count = hw.pci_func_count as u32;
                sum.usb_pid = hw.usb_pid;
                sum.nvme_pid = hw.nvme_pid;
                sum.blk_pid = hw.blk_pid;
                sum.persist_sectors = hw.persist_sectors;
                sum.blk_sectors = hw.blk_sectors;
                sum.usb_part2_sectors = hw.usb_part2_sectors;
                sum.boot_log_bytes = hw.boot_log_bytes;
                sum.persist_backend = hw.persist_backend;
                sum.persist_ro = u8::from(hw.persist_ro);
                sum.usb_ready = u8::from(hw.usb_sup_cap != 0);
                sum.nvme_ready = u8::from(hw.nvme_sup_cap != 0);
                sum.vmd_gated = u8::from(hw.nvme_sup_cap == 0 && hw.vmd_packed != 0);
                sum.blk_ready = u8::from(hw.blk_sectors > 0);
                if hw.blk_sup_cap != 0 {
                    if let Ok((tag, _, bmrs)) =
                        sys_ipc_call_inline(hw.blk_sup_cap, pack_msg_tag(BLK_OP_QUERY_INFO, 0, 1, 0), [0; 4], 0)
                    {
                        if msg_tag_label(tag) == IPC_OP_PONG {
                            sum.blk_io_ops = bmrs[2];
                            sum.blk_flush_ops = bmrs[3];
                        }
                    }
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_extra(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [
                            hw.boot_flags,
                            hw.persist_sectors,
                            u64::from(hw.persist_backend),
                            u64::from(hw.persist_ro),
                        ],
                        sum.as_bytes(),
                    );
                }
            }
        }
        IPC_OP_SUP_QUERY_USB => {
            let mut usb_sum = UsbSummaryInfo::empty();
            if hw.usb_sup_cap != 0 {
                let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
                if let Ok((rtag, rlen, _)) =
                    sys_ipc_call_extra(hw.usb_sup_cap, USB_OP_QUERY_SUMMARY, [0; 4], &[], &mut rx, 0)
                {
                    if msg_tag_label(rtag) == IPC_OP_PONG {
                        if let Some(parsed) = UsbSummaryInfo::from_bytes(&rx[..rlen]) {
                            usb_sum = parsed;
                        }
                    }
                }
            }
            if reply_tok != 0 {
                let _ = sys_ipc_reply_extra(
                    sup_ep,
                    reply_tok,
                    IPC_OP_PONG,
                    [
                        u64::from(usb_sum.state_flags),
                        usb_sum.msc_part2_sectors,
                        usb_sum.kbd_events,
                        usb_sum.mouse_events,
                    ],
                    usb_sum.as_bytes(),
                );
            }
        }
        IPC_OP_SUP_QUERY_NVME => {
            let mut ninfo = NvmeInfo::empty();
            if hw.nvme_sup_cap != 0 {
                let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
                if let Ok((rtag, rlen, _)) =
                    sys_ipc_call_extra(hw.nvme_sup_cap, NVME_OP_QUERY_INFO, [0; 4], &[], &mut rx, 0)
                {
                    if msg_tag_label(rtag) == IPC_OP_PONG {
                        if let Some(parsed) = NvmeInfo::from_bytes(&rx[..rlen]) {
                            ninfo = parsed;
                        }
                    }
                }
            } else if hw.vmd_packed != 0 {
                ninfo.device_id = (hw.vmd_packed & 0xffff) as u16;
                ninfo.vendor_id = ((hw.vmd_packed >> 16) & 0xffff) as u16;
                ninfo.bus_dev_fn = ((hw.vmd_packed >> 32) & 0xffff) as u16;
                ninfo.state_flags = NVME_FLAG_VMD_GATED;
            }
            if reply_tok != 0 {
                let _ = sys_ipc_reply_extra(
                    sup_ep,
                    reply_tok,
                    IPC_OP_PONG,
                    [
                        u64::from(ninfo.state_flags),
                        ninfo.nsze_sectors,
                        u64::from(ninfo.lba_bytes),
                        0,
                    ],
                    ninfo.as_bytes(),
                );
            }
        }
        IPC_OP_SUP_QUERY_BLK => {
            let _ = (EINVAL, IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS, raw_ipc_syscall, SYS_IPC_CALL, msg_tag_extra_len);
            let mut blk_info = [0u64; 4];
            if hw.blk_sup_cap != 0 {
                if let Ok((rtag, _, rmrs)) = sys_ipc_call_inline(
                    hw.blk_sup_cap,
                    pack_msg_tag(BLK_OP_QUERY_INFO, 0, 1, 0),
                    [0; 4],
                    0,
                ) {
                    if msg_tag_label(rtag) == IPC_OP_PONG {
                        blk_info = rmrs;
                    }
                }
            }
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    sup_ep,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                    blk_info,
                );
            }
        }
        _ => unreachable!(),
    }

    let _ = (USB_FLAG_READY, USB_FLAG_MSC_READY, USB_FLAG_DRYFS_VERIFIED, NVME_FLAG_READY);
    true
}
