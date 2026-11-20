// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Supervisor-side Mandatory NVIDIA Driver Island Lifecycle, Fault Testing, and IPC Bridge.
//!
//! Spawns `dry-nv-island.elf` (`SecDomain::Userspace`, `0x0218`) ONLY under explicit
//! `hwcheck_mode=nvidia_exp` / `nvidia_exp=1` boot profiles; keeps normal `safe` boots passive.

use core::mem::size_of;

use crate::abi_ext::{
    HWCHECK_MODE_NVIDIA_EXP, HWVAL_FLAG_NVIDIA_EXP, HWVAL_FLAG_TEST_NV_CORRUPT_IPC,
    HWVAL_FLAG_TEST_NV_TIMEOUT, HWVAL_FLAG_TEST_NV_UNAUTH_CAP, HWVAL_FLAG_TEST_NV_VM_CRASH,
};
use crate::hwdiag::mgr::DiagManager;
use crate::init_legacy::{domain_spec, log_line};
use crate::nvidia_island::prereq::{evaluate_nvidia_island, NvRuntimeProbe};
use crate::nvidia_island::protocol::{
    NvIslandStatusWire, NvPrereqWire, BADGE_NV_CLIENT, BADGE_NV_ROGUE, COMP_ID_NV_ISLAND,
    IPC_OP_SUP_NV_EXPORT, IPC_OP_SUP_NV_QUERY, NV_OP_AUDIT_CORRUPT, NV_OP_PING_HEALTH,
    NV_PREREQ_COUNT,
};
use crate::nvidia_island::session_logs::export_nvidia_logs;
use crate::shell::LineBuilder;
use crate::vfs_proto::{sys_ipc_call_extra, sys_ipc_reply_extra};
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_ipc_call_inline, sys_ipc_reply_inline, sys_proc_kill, sys_proc_spawn, sys_proc_wait,
    BADGE_SUPERVISOR, EPERM, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM,
    IPC_OP_EXIT_CLEAN, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES, RIGHT_CALL, RIGHT_READ, RIGHT_RECV,
    RIGHT_REPLY, RIGHT_SEND,
};

const NV_SCRATCH_CAP: usize = 16384;

pub struct SupNvManager {
    pub mod_cap: u64,
    pub nv_ep: u64,
    pub sup_cap: u64,
    pub cli_cap: u64,
    pub proc_handle: u64,
    pub rt: NvRuntimeProbe,
    pub status: NvIslandStatusWire,
    pub prereqs: [NvPrereqWire; NV_PREREQ_COUNT],
}

impl SupNvManager {
    pub const fn empty() -> Self {
        Self {
            mod_cap: 0,
            nv_ep: 0,
            sup_cap: 0,
            cli_cap: 0,
            proc_handle: 0,
            rt: NvRuntimeProbe {
                spawned: false,
                pid: 0,
                generation: 0,
                mac_isolated: false,
                island_alive: false,
                vm_restarts: 0,
                corrupt_ipc_rejected: false,
                unauth_cap_rejected: false,
                timeout_handled: false,
                export_ok: false,
                export_files: 0,
                last_ipc_err: 0,
            },
            status: NvIslandStatusWire::empty(),
            prereqs: [NvPrereqWire::EMPTY; NV_PREREQ_COUNT],
        }
    }

    fn spawn_island(&mut self) -> bool {
        if self.mod_cap == 0 {
            return false;
        }
        let spec = domain_spec(0, COMP_ID_NV_ISLAND);
        let Ok(ep) = sys_ep_create(spec) else { return false };
        let cli_r = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let srv_r = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;
        let Ok(sup_c) = sys_cap_derive(ep, cli_r, 0, BADGE_SUPERVISOR) else {
            let _ = sys_cap_revoke(ep, 2);
            return false;
        };
        let Ok(cli_c) = sys_cap_derive(ep, cli_r, 0, BADGE_NV_CLIENT) else {
            let _ = sys_cap_revoke(sup_c, 1);
            let _ = sys_cap_revoke(ep, 2);
            return false;
        };
        let Ok((ph, pid)) = sys_proc_spawn(
            self.mod_cap,
            0,
            COMP_ID_NV_ISLAND,
            (ep, srv_r, 0x0218),
            (0, 0),
        ) else {
            let _ = sys_cap_revoke(cli_c, 1);
            let _ = sys_cap_revoke(sup_c, 1);
            let _ = sys_cap_revoke(ep, 2);
            return false;
        };

        self.nv_ep = ep;
        self.sup_cap = sup_c;
        self.cli_cap = cli_c;
        self.proc_handle = ph;
        self.rt.spawned = true;
        self.rt.pid = pid as u32;
        self.rt.generation = self.rt.generation.wrapping_add(1);
        self.ping_health()
    }

    fn ping_health(&mut self) -> bool {
        if self.cli_cap == 0 {
            self.rt.island_alive = false;
            return false;
        }
        if let Ok((tag, _, mrs)) = sys_ipc_call_inline(
            self.cli_cap,
            pack_msg_tag(NV_OP_PING_HEALTH, 0, 1, 0),
            [0; 4],
            0,
        ) {
            if msg_tag_label(tag) == IPC_OP_PONG && mrs[0] == 1 {
                self.rt.mac_isolated = mrs[1] != 0;
                self.rt.island_alive = true;
                return true;
            }
        }
        self.rt.island_alive = false;
        false
    }

    fn stop_island(&mut self) {
        if self.proc_handle != 0 {
            if self.sup_cap != 0 {
                let _ = sys_ipc_call_inline(
                    self.sup_cap,
                    pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
                    [0; 4],
                    0,
                );
            }
            if sys_proc_wait(self.proc_handle, 0, 2_000_000_000).is_err() {
                let _ = sys_proc_kill(self.proc_handle);
                let _ = sys_proc_wait(self.proc_handle, 0, 2_000_000_000);
            }
            self.proc_handle = 0;
        }
        if self.cli_cap != 0 {
            let _ = sys_cap_revoke(self.cli_cap, 1);
            self.cli_cap = 0;
        }
        if self.sup_cap != 0 {
            let _ = sys_cap_revoke(self.sup_cap, 1);
            self.sup_cap = 0;
        }
        if self.nv_ep != 0 {
            let _ = sys_cap_revoke(self.nv_ep, 2);
            self.nv_ep = 0;
        }
        self.rt.island_alive = false;
    }

    #[inline(never)]
    pub fn init_at_boot(
        &mut self,
        diag: &DiagManager,
        tsc_mhz: u64,
        scratch: &mut [u8; NV_SCRATCH_CAP],
    ) {
        self.mod_cap = diag.ctx.ext.nv_island_mod_cap;
        let flags = diag.ctx.ext.hwval_flags;
        let exp_active =
            diag.ctx.ext.hwcheck_mode == HWCHECK_MODE_NVIDIA_EXP || (flags & HWVAL_FLAG_NVIDIA_EXP) != 0;
        let pci_recs = &diag.ctx.pci_recs[..diag.ctx.pci_count];
        let sess = &diag.ctx.session_id[..diag.ctx.session_len];

        if !exp_active {
            self.status = evaluate_nvidia_island(&diag.ctx.ext, pci_recs, sess, &self.rt, &mut self.prereqs);
            return;
        }

        if self.spawn_island() {
            // 1. Rogue badge / unauthorized capability check
            let cli_r = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
            if let Ok(rogue) = sys_cap_derive(self.nv_ep, cli_r, 0, BADGE_NV_ROGUE) {
                let r = sys_ipc_call_inline(rogue, pack_msg_tag(NV_OP_PING_HEALTH, 0, 1, 0), [0; 4], 0);
                let _ = sys_cap_revoke(rogue, 1);
                if matches!(r, Ok((t, _, _)) if msg_tag_label(t) == IPC_OP_ERR_PERM) {
                    self.rt.unauth_cap_rejected = true;
                }
            }
            if (flags & HWVAL_FLAG_TEST_NV_UNAUTH_CAP) != 0 && self.rt.unauth_cap_rejected {
                self.rt.last_ipc_err = -1;
            }

            // 2. Corrupt IPC frame rejection check
            if (flags & HWVAL_FLAG_TEST_NV_CORRUPT_IPC) != 0 {
                let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
                if let Ok((t, _, _)) = sys_ipc_call_extra(
                    self.cli_cap,
                    NV_OP_AUDIT_CORRUPT,
                    [0; 4],
                    b"BAD_CORRUPT_PKT",
                    &mut rx,
                    0,
                ) {
                    if msg_tag_label(t) == IPC_OP_ERR_INVAL {
                        self.rt.corrupt_ipc_rejected = true;
                        self.rt.last_ipc_err = -22;
                    }
                }
            }

            // 3. Real bounded timeout check on unserved temp endpoint
            if (flags & HWVAL_FLAG_TEST_NV_TIMEOUT) != 0 {
                if let Ok(tmp_ep) = sys_ep_create(domain_spec(0, COMP_ID_NV_ISLAND)) {
                    let to = 2_000u64.saturating_mul(tsc_mhz.max(1000));
                    if sys_ipc_call_inline(tmp_ep, pack_msg_tag(NV_OP_PING_HEALTH, 0, 1, 0), [0; 4], to).is_err() {
                        self.rt.timeout_handled = true;
                        self.rt.last_ipc_err = -110;
                    }
                    let _ = sys_cap_revoke(tmp_ep, 2);
                }
                let _ = self.ping_health();
            }

            // 4. Controlled VM / driver island `#UD` crash -> reap & respawn check
            if (flags & HWVAL_FLAG_TEST_NV_VM_CRASH) != 0 && self.sup_cap != 0 {
                let _ = sys_ipc_call_inline(self.sup_cap, pack_msg_tag(IPC_OP_CRASH_UD2, 0, 1, 0), [0; 4], 0);
                let w = sys_proc_wait(self.proc_handle, 0, 4_000_000_000);
                self.proc_handle = 0;
                self.stop_island();
                if matches!(w, Ok((1, d)) if (d & 0xff) == 6) {
                    self.rt.vm_restarts = self.rt.vm_restarts.saturating_add(1);
                    let _ = self.spawn_island();
                }
            }
        }

        self.status = evaluate_nvidia_island(&diag.ctx.ext, pci_recs, sess, &self.rt, &mut self.prereqs);
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-NV-ISLAND] AUDIT level=1 status=BLOCKED met=");
        lb.push_u64(self.status.met_count as u64);
        lb.push_bytes(b"/14 restarts=");
        lb.push_u64(self.rt.vm_restarts as u64);
        lb.push_bytes(b" corrupt_rej=");
        lb.push_u64(u64::from(self.rt.corrupt_ipc_rejected));
        lb.push_bytes(b" unauth_rej=");
        lb.push_u64(u64::from(self.rt.unauth_cap_rejected));
        lb.push_bytes(b" timeout_ok=");
        lb.push_u64(u64::from(self.rt.timeout_handled));
        log_line(&lb);

        self.do_export(diag, scratch);
    }

    #[inline(never)]
    fn do_export(&mut self, diag: &DiagManager, scratch: &mut [u8; NV_SCRATCH_CAP]) {
        let pci_recs = &diag.ctx.pci_recs[..diag.ctx.pci_count];
        match export_nvidia_logs(diag.usb_sup_cap, &mut self.status, &self.prereqs, pci_recs, scratch) {
            Ok(n) => {
                self.rt.export_ok = true;
                self.rt.export_files = n;
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-NV-EXP] EXPORT status=VERIFIED files=");
                lb.push_u64(n as u64);
                log_line(&lb);
            }
            Err(e) => {
                self.rt.export_ok = false;
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-NV-EXP] EXPORT status=BLOCKED err=");
                lb.push_i64(e as i64);
                log_line(&lb);
            }
        }
    }

    #[inline(never)]
    pub fn handle_ipc(
        &mut self,
        sup_ep: u64,
        reply_tok: u64,
        label: u32,
        mrs: [u64; 4],
        authorized: bool,
        diag: &DiagManager,
    ) -> bool {
        if label != IPC_OP_SUP_NV_QUERY && label != IPC_OP_SUP_NV_EXPORT {
            return false;
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
            IPC_OP_SUP_NV_QUERY => {
                if reply_tok == 0 {
                    return true;
                }
                if mrs[0] == 1 {
                    let idx = (mrs[1] as usize).min(NV_PREREQ_COUNT - 1);
                    let _ = sys_ipc_reply_extra(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [NV_PREREQ_COUNT as u64, idx as u64, 0, 0],
                        self.prereqs[idx].as_bytes(),
                    );
                } else {
                    let _ = sys_ipc_reply_extra(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [
                            self.status.level as u64,
                            self.status.met_count as u64,
                            self.status.export_files as u64,
                            0,
                        ],
                        self.status.as_bytes(),
                    );
                }
            }
            IPC_OP_SUP_NV_EXPORT => {
                // SAFETY: Single-threaded supervisor loop in `dry-init` owns `NV_SCRATCH`.
                let scratch = unsafe { &mut *core::ptr::addr_of_mut!(NV_SCRATCH) };
                self.do_export(diag, scratch);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_extra(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [
                            self.status.export_ok as u64,
                            self.status.export_files as u64,
                            0,
                            0,
                        ],
                        self.status.as_bytes(),
                    );
                }
            }
            _ => {}
        }
        true
    }
}

static mut NV_MGR: SupNvManager = SupNvManager::empty();
static mut NV_SCRATCH: [u8; NV_SCRATCH_CAP] = [0u8; NV_SCRATCH_CAP];

#[inline(never)]
pub fn init_nvidia_island_service(diag: &DiagManager, tsc_mhz: u64) {
    // SAFETY: Single-threaded supervisor loop in `dry-init` owns `NV_MGR` and `NV_SCRATCH`.
    let mgr = unsafe { &mut *core::ptr::addr_of_mut!(NV_MGR) };
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(NV_SCRATCH) };
    mgr.init_at_boot(diag, tsc_mhz, scratch);
}

#[inline(never)]
pub fn handle_nv_supervisor_ipc(
    sup_ep: u64,
    reply_tok: u64,
    label: u32,
    mrs: [u64; 4],
    authorized: bool,
    diag: &DiagManager,
) -> bool {
    let _ = size_of::<NvIslandStatusWire>();
    // SAFETY: Single-threaded supervisor loop in `dry-init` owns `NV_MGR`.
    let mgr = unsafe { &mut *core::ptr::addr_of_mut!(NV_MGR) };
    mgr.handle_ipc(sup_ep, reply_tok, label, mrs, authorized, diag)
}

#[inline(never)]
pub fn teardown_nvidia_island_service() {
    // SAFETY: Single-threaded supervisor teardown in `dry-init`.
    let mgr = unsafe { &mut *core::ptr::addr_of_mut!(NV_MGR) };
    mgr.stop_island();
}
