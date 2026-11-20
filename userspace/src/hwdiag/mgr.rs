// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Supervisor Hardware Diagnostics Manager (`dry-init` ownership, capability-gated IPC).

use core::mem::size_of;
use core::ptr::read_volatile;

use crate::abi_ext::{
    RootInitBootExtInfo, BOOT_EXT_INFO_OFFSET, HWCHECK_MODE_RECOVERY, ROOT_BOOT_EXT_MAGIC,
};
use crate::fat32_log::{
    UsbLogStatus, USB_OP_LOG_COMMIT_FILE, USB_OP_LOG_LIST_SESSIONS, USB_OP_LOG_READ_FILE,
    USB_OP_LOG_STAGE_APPEND, USB_OP_LOG_STAGE_RESET, USB_OP_LOG_STATUS,
};
use crate::hwdiag::report_json::{compute_sha256, render_file, render_manifest, REPORT_FILENAMES};
use crate::hwdiag::{
    mode_slug, persist_slug, run_suite, DiagContext, DiagSessionReport, DiagSummaryWire,
    DiagTestRecord, IPC_OP_SUP_DIAG_QUERY, IPC_OP_SUP_DIAG_RUN, PERSIST_BLOCKED,
    PERSIST_COMMITTING, PERSIST_FAILED, PERSIST_RECOVERY, PERSIST_VERIFIED, SCOPE_ALL,
};
use crate::init_legacy::log_line;
use crate::lx_compat_meta::read_boot_pci_funcs;
use crate::shell::LineBuilder;
use crate::sup_hw_services::{HwServicesState, SupHwSummary};
use crate::usb::msc::ANCESTRY_ALL_GATES;
use crate::vault_crypto::ct_eq_32;
use crate::vfs_proto::{sys_ipc_call_extra, sys_ipc_reply_extra};
use crate::{
    msg_tag_label, pack_msg_tag, sys_ipc_call_inline, sys_ipc_reply_inline, EPERM,
    IPC_OP_ERR_PERM, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES,
};

pub const DIAG_SCRATCH_CAP: usize = 20 * 1024;

pub struct DiagManager {
    pub ctx: DiagContext,
    pub report: DiagSessionReport,
    pub usb_sup_cap: u64,
    pub boot_hwcheck_mode: u32,
    pub boot_export_seq: u32,
}

impl DiagManager {
    pub const fn empty() -> Self {
        Self {
            ctx: DiagContext::empty(),
            report: DiagSessionReport::empty(),
            usb_sup_cap: 0,
            boot_hwcheck_mode: 0,
            boot_export_seq: 0,
        }
    }

    #[inline(never)]
    pub fn init_at_boot(
        &mut self,
        boot_info_addr: u64,
        pci_func_count: u64,
        hw: &HwServicesState,
        tsc_mhz: u64,
        online_cpus: u32,
        scratch: &mut [u8; DIAG_SCRATCH_CAP],
    ) {
        if boot_info_addr != 0 {
            let raw = unsafe { read_volatile((boot_info_addr + BOOT_EXT_INFO_OFFSET) as *const RootInitBootExtInfo) };
            if raw.magic == ROOT_BOOT_EXT_MAGIC {
                self.ctx.ext = raw;
            }
        }
        self.ctx.pci_count = read_boot_pci_funcs(boot_info_addr, pci_func_count, &mut self.ctx.pci_recs);
        self.ctx.hw_sum = build_hw_sum(hw);
        self.ctx.usb_logs = query_usb_log_status(hw.usb_sup_cap);
        self.ctx.persist_ro = hw.persist_ro;
        self.ctx.tsc_mhz = tsc_mhz;
        self.ctx.online_cpus = online_cpus;
        self.ctx.session_id = [0; 20];
        self.ctx.session_len = 0;

        self.report.count = 0;
        self.report.summary = DiagSummaryWire::empty();
        self.usb_sup_cap = hw.usb_sup_cap;
        self.boot_hwcheck_mode = self.ctx.ext.hwcheck_mode;
        self.boot_export_seq = self.ctx.usb_logs.session_count as u32 + 1;
        self.assign_session_id();

        if self.ctx.ext.hwcheck_mode == HWCHECK_MODE_RECOVERY {
            self.run_recovery_mode(scratch);
            return;
        }

        if self.ctx.is_auto() {
            run_suite(&self.ctx, SCOPE_ALL, self.ctx.is_passive_safe(), &mut self.report);
            self.report.summary.auto_ran = 1;
            {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-HWDIAG] AUTO_COMPLETE mode=");
                lb.push_bytes(mode_slug(self.ctx.ext.hwcheck_mode));
                lb.push_bytes(b" pass=");
                lb.push_u64(self.report.summary.pass_count as u64);
                lb.push_bytes(b" fail=");
                lb.push_u64(self.report.summary.fail_count as u64);
                lb.push_bytes(b" skip=");
                lb.push_u64(self.report.summary.skip_count as u64);
                lb.push_bytes(b" blocked=");
                lb.push_u64(self.report.summary.blocked_count as u64);
                lb.push_bytes(b" session=");
                lb.push_bytes(&self.ctx.session_id[..self.ctx.session_len]);
                log_line(&lb);
            }
            self.export_to_usb(scratch);
        } else {
            self.report.recompute_totals(&self.ctx);
        }
    }

    fn assign_session_id(&mut self) {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"sess-");
        let seq = self.boot_export_seq.max(1);
        if seq < 1000 { lb.push_byte(b'0'); }
        if seq < 100 { lb.push_byte(b'0'); }
        if seq < 10 { lb.push_byte(b'0'); }
        lb.push_u64(seq as u64);
        let b = lb.as_bytes();
        let n = b.len().min(20);
        self.ctx.session_id.fill(0);
        self.ctx.session_id[..n].copy_from_slice(&b[..n]);
        self.ctx.session_len = n;
        self.boot_export_seq = seq + 1;
    }

    #[inline(never)]
    fn run_recovery_mode(&mut self, scratch: &mut [u8; DIAG_SCRATCH_CAP]) {
        self.ctx.usb_logs = query_usb_log_status(self.usb_sup_cap);
        let mut sess_rx = [0u8; MAX_IPC_EXTRA_BYTES];
        let mut cnt = 0u64;
        let mut latest: &[u8] = b"none";
        let mut latest_buf = [0u8; 40];
        if self.usb_sup_cap != 0 {
            if let Ok((tag, elen, rmrs)) = sys_ipc_call_extra(
                self.usb_sup_cap,
                USB_OP_LOG_LIST_SESSIONS,
                [0; 4],
                &[],
                &mut sess_rx,
                0,
            ) {
                if msg_tag_label(tag) == IPC_OP_PONG {
                    cnt = rmrs[0];
                    let list = &sess_rx[..elen];
                    let mut chosen: &[u8] = b"";
                    for line in list.split(|&b| b == b'\n') {
                        if !line.is_empty() {
                            chosen = line;
                        }
                    }
                    if !chosen.is_empty() {
                        let n = chosen.len().min(latest_buf.len());
                        latest_buf[..n].copy_from_slice(&chosen[..n]);
                        latest = &latest_buf[..n];
                        // Verify persisted summary.json read-back cleanly in recovery viewer!
                        let mut p = LineBuilder::new();
                        p.push_bytes(latest);
                        p.push_bytes(b"/summary.json");
                        let _ = read_usb_file(self.usb_sup_cap, p.as_bytes(), scratch);
                    }
                }
            }
        }
        self.report.summary.mode = HWCHECK_MODE_RECOVERY;
        self.report.summary.persist_status = PERSIST_RECOVERY;
        self.report.summary.recovery_sessions = cnt.min(255) as u8;
        self.report.recompute_totals(&self.ctx);
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-HWDIAG] RECOVERY_SESSIONS count=");
        lb.push_u64(cnt);
        lb.push_bytes(b" latest=");
        lb.push_bytes(latest);
        log_line(&lb);
    }

    #[inline(never)]
    pub fn export_to_usb(&mut self, scratch: &mut [u8; DIAG_SCRATCH_CAP]) {
        self.ctx.usb_logs = query_usb_log_status(self.usb_sup_cap);
        let ul = self.ctx.usb_logs;
        let ready = self.usb_sup_cap != 0
            && (ul.gate_mask & ANCESTRY_ALL_GATES) == ANCESTRY_ALL_GATES
            && ul.fat_mounted != 0
            && ul.fat_corrupt == 0
            && ul.read_only == 0;

        if !ready {
            self.report.summary.persist_status = if ul.fat_corrupt != 0 { PERSIST_FAILED } else { PERSIST_BLOCKED };
            self.report.summary.sha256_verified = 0;
            self.report.summary.files_exported = 0;
            self.report.recompute_totals(&self.ctx);
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-HWDIAG] PERSIST status=");
            lb.push_bytes(persist_slug(self.report.summary.persist_status));
            lb.push_bytes(b" files=0 sha256_verified=0 reason=");
            lb.push_u64(ul.block_reason as u64);
            log_line(&lb);
            return;
        }

        if self.report.summary.files_exported > 0 {
            self.assign_session_id();
            self.report.recompute_totals(&self.ctx);
        }

        self.report.summary.persist_status = PERSIST_COMMITTING;
        let mut digests = [[0u8; 32]; 12];
        let mut ok_count = 0u8;
        let mut fail_err = 0i64;
        const COMMIT_ORDER: [usize; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0];

        for &idx in COMMIT_ORDER.iter() {
            let fname = REPORT_FILENAMES[idx];
            let payload = render_file(idx, &self.ctx, &self.report, scratch);
            let expect_sha = compute_sha256(payload);
            digests[idx] = expect_sha;
            let mut rpath = LineBuilder::new();
            rpath.push_bytes(&self.ctx.session_id[..self.ctx.session_len]);
            rpath.push_byte(b'/');
            rpath.push_bytes(fname);
            match stage_and_commit_file(self.usb_sup_cap, payload, rpath.as_bytes(), &expect_sha) {
                Ok(()) => ok_count += 1,
                Err(e) => {
                    fail_err = e as i64;
                    break;
                }
            }
        }

        let mut all_ok = ok_count == 12;
        if all_ok {
            let man = render_manifest(&digests, scratch);
            let man_sha = compute_sha256(man);
            let mut rpath = LineBuilder::new();
            rpath.push_bytes(&self.ctx.session_id[..self.ctx.session_len]);
            rpath.push_bytes(b"/manifest.sha256");
            if let Err(e) = stage_and_commit_file(self.usb_sup_cap, man, rpath.as_bytes(), &man_sha) {
                all_ok = false;
                fail_err = e as i64;
            } else {
                ok_count = 13;
            }
        }

        self.ctx.usb_logs = query_usb_log_status(self.usb_sup_cap);
        self.report.summary.persist_status = if all_ok { PERSIST_VERIFIED } else { PERSIST_FAILED };
        self.report.summary.sha256_verified = u8::from(all_ok);
        self.report.summary.files_exported = if all_ok { 13 } else { ok_count };
        self.report.recompute_totals(&self.ctx);

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-HWDIAG] PERSIST status=");
        lb.push_bytes(persist_slug(self.report.summary.persist_status));
        lb.push_bytes(b" files=");
        lb.push_u64(self.report.summary.files_exported as u64);
        lb.push_bytes(b" sha256_verified=");
        lb.push_u64(self.report.summary.sha256_verified as u64);
        lb.push_bytes(b" reason=");
        if all_ok { lb.push_bytes(b"ok"); } else { lb.push_i64(fail_err); }
        log_line(&lb);
    }

    #[inline(never)]
    pub fn handle_ipc(
        &mut self,
        sup_ep: u64,
        reply_tok: u64,
        label: u32,
        mrs: [u64; 4],
        authorized: bool,
        scratch: &mut [u8; DIAG_SCRATCH_CAP],
    ) -> bool {
        if label != IPC_OP_SUP_DIAG_QUERY && label != IPC_OP_SUP_DIAG_RUN {
            return false;
        }
        if !authorized {
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(sup_ep, reply_tok, pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0), [EPERM as u64, 0, 0, 0]);
            }
            return true;
        }
        match label {
            IPC_OP_SUP_DIAG_QUERY => {
                self.ctx.usb_logs = query_usb_log_status(self.usb_sup_cap);
                self.report.recompute_totals(&self.ctx);
                if mrs[0] == 1 {
                    let total = self.report.count;
                    let start = (mrs[1] as usize).min(total);
                    let batch = (total - start).min(2); // 2 * 128 B = 256 B
                    let byte_len = batch * size_of::<DiagTestRecord>();
                    let bytes = unsafe {
                        core::slice::from_raw_parts(self.report.records[start..].as_ptr().cast::<u8>(), byte_len)
                    };
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_extra(
                            sup_ep,
                            reply_tok,
                            IPC_OP_PONG,
                            [total as u64, batch as u64, (start + batch) as u64, self.report.summary.persist_status as u64],
                            bytes,
                        );
                    }
                } else if reply_tok != 0 {
                    let s = self.report.summary;
                    let bytes = unsafe {
                        core::slice::from_raw_parts((&raw const s).cast::<u8>(), size_of::<DiagSummaryWire>())
                    };
                    let _ = sys_ipc_reply_extra(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [s.pass_count as u64, s.fail_count as u64, s.skip_count as u64, s.blocked_count as u64],
                        bytes,
                    );
                }
            }
            IPC_OP_SUP_DIAG_RUN => {
                let scope = mrs[0] as u32;
                let force_safe = (mrs[1] & 1) != 0;
                let do_export = (mrs[1] & 2) != 0;
                if scope != 0 {
                    run_suite(&self.ctx, scope, force_safe || self.ctx.is_passive_safe(), &mut self.report);
                }
                if do_export {
                    self.export_to_usb(scratch);
                }
                if reply_tok != 0 {
                    let s = self.report.summary;
                    let bytes = unsafe {
                        core::slice::from_raw_parts((&raw const s).cast::<u8>(), size_of::<DiagSummaryWire>())
                    };
                    let _ = sys_ipc_reply_extra(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [s.pass_count as u64, s.fail_count as u64, s.skip_count as u64, s.blocked_count as u64],
                        bytes,
                    );
                }
            }
            _ => {}
        }
        true
    }
}

#[inline(never)]
pub fn query_usb_log_status(usb_sup_cap: u64) -> UsbLogStatus {
    if usb_sup_cap == 0 {
        return UsbLogStatus::default();
    }
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    if let Ok((tag, elen, _)) = sys_ipc_call_extra(usb_sup_cap, USB_OP_LOG_STATUS, [0; 4], &[], &mut rx, 0) {
        if msg_tag_label(tag) == IPC_OP_PONG && elen >= size_of::<UsbLogStatus>() {
            let mut out = UsbLogStatus::default();
            unsafe {
                core::ptr::copy_nonoverlapping(rx.as_ptr(), (&raw mut out).cast::<u8>(), size_of::<UsbLogStatus>());
            }
            return out;
        }
    }
    UsbLogStatus::default()
}

#[inline(never)]
pub fn stage_and_commit_file(usb_sup_cap: u64, data: &[u8], rel_path: &[u8], expect_sha: &[u8; 32]) -> Result<(), isize> {
    if usb_sup_cap == 0 {
        return Err(-19);
    }
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (rt, _, _) = sys_ipc_call_inline(usb_sup_cap, pack_msg_tag(USB_OP_LOG_STAGE_RESET, 0, 1, 0), [0; 4], 0)?;
    if msg_tag_label(rt) != IPC_OP_PONG {
        return Err(-5);
    }
    let mut off = 0usize;
    while off < data.len() {
        let take = (data.len() - off).min(MAX_IPC_EXTRA_BYTES);
        let (at, _, _) = sys_ipc_call_extra(usb_sup_cap, USB_OP_LOG_STAGE_APPEND, [0; 4], &data[off..off + take], &mut rx, 0)?;
        if msg_tag_label(at) != IPC_OP_PONG {
            return Err(-5);
        }
        off += take;
    }
    let (ct, clen, cmrs) = sys_ipc_call_extra(usb_sup_cap, USB_OP_LOG_COMMIT_FILE, [0; 4], rel_path, &mut rx, 0)?;
    if msg_tag_label(ct) != IPC_OP_PONG || clen < 32 {
        let code = cmrs[0] as isize;
        return Err(if code < 0 { code } else { -5 });
    }
    let mut got = [0u8; 32];
    got.copy_from_slice(&rx[..32]);
    if !ct_eq_32(&got, expect_sha) {
        return Err(-5);
    }
    Ok(())
}

pub fn read_usb_file(usb_sup_cap: u64, rel_path: &[u8], out: &mut [u8]) -> usize {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let mut off = 0usize;
    while let Ok((tag, clen, mrs)) =
        sys_ipc_call_extra(usb_sup_cap, USB_OP_LOG_READ_FILE, [off as u64, 0, 0, 0], rel_path, &mut rx, 0)
    {
        if msg_tag_label(tag) != IPC_OP_PONG || clen == 0 {
            break;
        }
        let take = clen.min(out.len().saturating_sub(off));
        out[off..off + take].copy_from_slice(&rx[..take]);
        off += take;
        if off >= mrs[0] as usize || off >= out.len() {
            break;
        }
    }
    off
}

fn build_hw_sum(hw: &HwServicesState) -> SupHwSummary {
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
    sum
}
