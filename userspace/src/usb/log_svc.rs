// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-gated (`BADGE_SUPERVISOR`) USB Log Service bridging `dry-usb` xHCI MSC
//! and `fat32_log::Fat32LogFs` while keeping `dry_usb.rs` concise.

use core::mem::size_of;

use crate::abi_ext::{HWVAL_FLAG_TEST_USB_DISCONNECT, HWVAL_FLAG_TEST_USB_WRITE_FAIL};
use crate::fat32_log::{
    Fat32LogFs, LogBlockDevice, UsbLogStatus, FAT_ERR_UNVERIFIED_ANCESTRY,
    USB_OP_LOG_COMMIT_FILE, USB_OP_LOG_CONFIG, USB_OP_LOG_LIST_SESSIONS, USB_OP_LOG_READ_FILE,
    USB_OP_LOG_STAGE_APPEND, USB_OP_LOG_STAGE_RESET, USB_OP_LOG_STATUS,
};
use crate::shell::LineBuilder;
use crate::usb::msc::{UsbMscBot, ANCESTRY_ALL_GATES};
use crate::usb::xhci::XhciController;
use crate::vfs_proto::sys_ipc_reply_extra;
use crate::{
    pack_msg_tag, sys_ipc_reply_inline, EINVAL, ENOMEM, EPERM, EPIPE, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_PERM, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES,
};

pub const LOG_STAGE_CAP: usize = 32 * 1024;

pub struct MscLogDev<'a> {
    pub hc: &'a mut XhciController,
    pub msc: &'a mut UsbMscBot,
}

impl LogBlockDevice for MscLogDev<'_> {
    fn part_sectors(&self) -> u64 {
        self.msc.logs_sectors
    }
    fn read_sector(&mut self, rel_lba: u64, out: &mut [u8; 512]) -> Result<(), isize> {
        self.msc.read_logs_sector(self.hc, rel_lba, out)
    }
    fn write_sector(&mut self, rel_lba: u64, data: &[u8; 512]) -> Result<(), isize> {
        self.msc.write_logs_sector(self.hc, rel_lba, data)
    }
    fn flush(&mut self) -> Result<(), isize> {
        self.msc.sync_logs_cache(self.hc)
    }
}

pub struct UsbLogServerState {
    pub fat_logs: Fat32LogFs,
    pub logs_read_only: bool,
    pub stage_len: usize,
    pub writes_verified: u32,
    pub write_errors: u32,
}

impl UsbLogServerState {
    pub const fn empty() -> Self {
        Self {
            fat_logs: Fat32LogFs::empty(),
            logs_read_only: false,
            stage_len: 0,
            writes_verified: 0,
            write_errors: 0,
        }
    }

    pub fn build_status(&mut self, hc: &mut XhciController, msc: &mut UsbMscBot) -> UsbLogStatus {
        let (gate_mask, block_reason) = msc.verify_logs_ancestry();
        if (gate_mask & ANCESTRY_ALL_GATES) == ANCESTRY_ALL_GATES
            && !self.fat_logs.mounted
            && !self.fat_logs.corrupt
        {
            let mut dev = MscLogDev { hc, msc };
            let _ = self.fat_logs.mount(&mut dev);
        }
        let mut sess_cnt = 0u8;
        if self.fat_logs.mounted {
            let mut dev = MscLogDev { hc, msc };
            if let Ok(Some((hw_clus, _, _))) =
                self.fat_logs.find_entry(&mut dev, self.fat_logs.root_clus, b"hardware-tests")
            {
                let mut tmp = [0u8; 64];
                if let Ok((_, cnt)) = self.fat_logs.list_subdirs(&mut dev, hw_clus, &mut tmp) {
                    sess_cnt = cnt;
                }
            }
        }
        UsbLogStatus {
            gate_mask,
            block_reason,
            fat_mounted: u8::from(self.fat_logs.mounted),
            fat_corrupt: u8::from(self.fat_logs.corrupt),
            read_only: u8::from(self.logs_read_only),
            session_count: sess_cnt,
            free_kib: self.fat_logs.free_kib(),
            total_kib: self.fat_logs.total_kib(),
            logs_sectors: msc.logs_sectors,
            writes_verified: self.writes_verified,
            write_errors: self.write_errors,
        }
    }

    fn commit_staged(
        &mut self,
        hc: &mut XhciController,
        msc: &mut UsbMscBot,
        stage_buf: &[u8; LOG_STAGE_CAP],
        rel_path: &[u8],
    ) -> Result<[u8; 32], isize> {
        let (gates, _) = msc.verify_logs_ancestry();
        if gates != ANCESTRY_ALL_GATES || self.logs_read_only {
            self.write_errors = self.write_errors.saturating_add(1);
            return Err(FAT_ERR_UNVERIFIED_ANCESTRY);
        }
        if rel_path.is_empty() || rel_path.windows(2).any(|w| w == b"..") {
            return Err(EINVAL);
        }
        let mut parts = rel_path.split(|&b| b == b'/');
        let Some(sess) = parts.next() else { return Err(EINVAL) };
        let Some(second) = parts.next() else { return Err(EINVAL) };
        let third = parts.next();
        if parts.next().is_some() || sess.is_empty() || second.is_empty() {
            return Err(EINVAL);
        }
        let mut dev = MscLogDev { hc, msc };
        if !self.fat_logs.mounted && !self.fat_logs.corrupt {
            self.fat_logs.mount(&mut dev)?;
        }
        if !self.fat_logs.mounted {
            self.write_errors = self.write_errors.saturating_add(1);
            return Err(EPIPE);
        }
        let hw_clus = self.fat_logs.ensure_dir(&mut dev, self.fat_logs.root_clus, b"hardware-tests")?;
        let sess_clus = self.fat_logs.ensure_dir(&mut dev, hw_clus, sess)?;
        let (target_dir, fname) = if let Some(f3) = third {
            if f3.is_empty() {
                return Err(EINVAL);
            }
            let sub_clus = self.fat_logs.ensure_dir(&mut dev, sess_clus, second)?;
            (sub_clus, f3)
        } else {
            (sess_clus, second)
        };
        let slen = self.stage_len.min(LOG_STAGE_CAP);
        match self.fat_logs.write_file_verified(&mut dev, target_dir, fname, &stage_buf[..slen]) {
            Ok(sha) => {
                self.writes_verified = self.writes_verified.saturating_add(1);
                Ok(sha)
            }
            Err(e) => {
                self.write_errors = self.write_errors.saturating_add(1);
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_ipc(
        &mut self,
        srv_ep_cap: u64,
        tok: u64,
        label: u32,
        from_sup: bool,
        mrs: [u64; 4],
        extra_rx: &[u8],
        hc: &mut XhciController,
        msc: &mut UsbMscBot,
        stage_buf: &mut [u8; LOG_STAGE_CAP],
        log_fn: fn(&LineBuilder),
    ) -> bool {
        let reply_err = |lbl: u32, err: isize| {
            if tok != 0 {
                let _ = sys_ipc_reply_inline(srv_ep_cap, tok, pack_msg_tag(lbl, 0, 1, 0), [err as u64, 0, 0, 0]);
            }
        };
        if !from_sup {
            reply_err(IPC_OP_ERR_PERM, EPERM);
            return true;
        }
        match label {
            USB_OP_LOG_CONFIG => {
                let mut guid = [0u8; 16];
                guid[0..8].copy_from_slice(&mrs[0].to_le_bytes());
                guid[8..16].copy_from_slice(&mrs[1].to_le_bytes());
                msc.expected_disk_guid = guid;
                let hflags = mrs[2] as u32;
                msc.test_write_fail = (hflags & HWVAL_FLAG_TEST_USB_WRITE_FAIL) != 0;
                msc.test_disconnect = (hflags & HWVAL_FLAG_TEST_USB_DISCONNECT) != 0;
                self.logs_read_only = (mrs[3] & 1) != 0;
                let ls = self.build_status(hc, msc);
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-USB] LOGS_ANCESTRY gates=0x");
                lb.push_hex_width(ls.gate_mask as u64, 2);
                lb.push_bytes(b" block=");
                lb.push_u64(ls.block_reason as u64);
                lb.push_bytes(b" mounted=");
                lb.push_u64(ls.fat_mounted as u64);
                lb.push_bytes(b" free_kib=");
                lb.push_u64(ls.free_kib as u64);
                log_fn(&lb);
                if tok != 0 {
                    let b = unsafe { core::slice::from_raw_parts((&raw const ls).cast::<u8>(), size_of::<UsbLogStatus>()) };
                    let _ = sys_ipc_reply_extra(
                        srv_ep_cap, tok, IPC_OP_PONG,
                        [ls.gate_mask as u64, ls.block_reason as u64, ls.fat_mounted as u64, ls.free_kib as u64], b,
                    );
                }
            }
            USB_OP_LOG_STATUS => {
                let ls = self.build_status(hc, msc);
                if tok != 0 {
                    let b = unsafe { core::slice::from_raw_parts((&raw const ls).cast::<u8>(), size_of::<UsbLogStatus>()) };
                    let _ = sys_ipc_reply_extra(
                        srv_ep_cap, tok, IPC_OP_PONG,
                        [ls.gate_mask as u64, ls.block_reason as u64, ls.fat_mounted as u64, ls.free_kib as u64], b,
                    );
                }
            }
            USB_OP_LOG_STAGE_RESET => {
                self.stage_len = 0;
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(srv_ep_cap, tok, pack_msg_tag(IPC_OP_PONG, 0, 1, 0), [1, 0, 0, 0]);
                }
            }
            USB_OP_LOG_STAGE_APPEND => {
                if self.stage_len.saturating_add(extra_rx.len()) > LOG_STAGE_CAP {
                    reply_err(IPC_OP_ERR_INVAL, ENOMEM);
                    return true;
                }
                stage_buf[self.stage_len..self.stage_len + extra_rx.len()].copy_from_slice(extra_rx);
                self.stage_len += extra_rx.len();
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(srv_ep_cap, tok, pack_msg_tag(IPC_OP_PONG, 0, 1, 0), [self.stage_len as u64, 0, 0, 0]);
                }
            }
            USB_OP_LOG_COMMIT_FILE => match self.commit_staged(hc, msc, stage_buf, extra_rx) {
                Ok(sha) => if tok != 0 {
                    let _ = sys_ipc_reply_extra(srv_ep_cap, tok, IPC_OP_PONG, [1, self.stage_len as u64, self.writes_verified as u64, 0], &sha);
                },
                Err(e) => reply_err(IPC_OP_ERR_INVAL, e),
            },
            USB_OP_LOG_LIST_SESSIONS => {
                let ls = self.build_status(hc, msc);
                let mut list_buf = [0u8; MAX_IPC_EXTRA_BYTES];
                let mut list_len = 0usize;
                let mut cnt = 0u8;
                if ls.fat_mounted != 0 {
                    let mut dev = MscLogDev { hc, msc };
                    if let Ok(Some((hw_c, _, _))) = self.fat_logs.find_entry(&mut dev, self.fat_logs.root_clus, b"hardware-tests") {
                        if let Ok((n, c)) = self.fat_logs.list_subdirs(&mut dev, hw_c, &mut list_buf) {
                            list_len = n;
                            cnt = c;
                        }
                    }
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_extra(srv_ep_cap, tok, IPC_OP_PONG, [cnt as u64, list_len as u64, ls.gate_mask as u64, 0], &list_buf[..list_len]);
                }
            }
            USB_OP_LOG_READ_FILE => {
                let offset = mrs[0] as usize;
                let ls = self.build_status(hc, msc);
                if ls.fat_mounted == 0 {
                    reply_err(IPC_OP_ERR_INVAL, FAT_ERR_UNVERIFIED_ANCESTRY);
                    return true;
                }
                let mut total_sz = 0usize;
                let mut chunk_len = 0usize;
                let mut out_chunk = [0u8; MAX_IPC_EXTRA_BYTES];
                let mut parts = extra_rx.split(|&b| b == b'/');
                if let (Some(sess), Some(second), third, None) = (parts.next(), parts.next(), parts.next(), parts.next()) {
                    let mut dev = MscLogDev { hc, msc };
                    if let Ok(Some((hw_c, _, _))) = self.fat_logs.find_entry(&mut dev, self.fat_logs.root_clus, b"hardware-tests") {
                        if let Ok(Some((sc, _, _))) = self.fat_logs.find_entry(&mut dev, hw_c, sess) {
                            let target = match third {
                                Some(f3) if !f3.is_empty() => {
                                    if let Ok(Some((sub_c, _, _))) = self.fat_logs.find_entry(&mut dev, sc, second) {
                                        self.fat_logs.find_entry(&mut dev, sub_c, f3)
                                    } else {
                                        Ok(None)
                                    }
                                }
                                None if !second.is_empty() => self.fat_logs.find_entry(&mut dev, sc, second),
                                _ => Ok(None),
                            };
                            if let Ok(Some((fc, fsz, _))) = target {
                                if let Ok(n) = self.fat_logs.read_file_bytes(&mut dev, fc, fsz, stage_buf) {
                                    total_sz = n;
                                    if offset < n {
                                        chunk_len = (n - offset).min(MAX_IPC_EXTRA_BYTES);
                                        out_chunk[..chunk_len].copy_from_slice(&stage_buf[offset..offset + chunk_len]);
                                    }
                                }
                            }
                        }
                    }
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_extra(srv_ep_cap, tok, IPC_OP_PONG, [total_sz as u64, chunk_len as u64, 0, 0], &out_chunk[..chunk_len]);
                }
            }
            _ => return false,
        }
        true
    }
}
