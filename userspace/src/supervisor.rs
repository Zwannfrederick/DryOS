// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Unified Ring 3 Desktop Supervisor IPC Event Loop (`Milestone 2A-3` & `Milestone 2B`).
//!
//! Orchestrates service bring-up (`dry-vfs`, `dry-blk`, `dry-power`, `dry-display`, `dry-input`,
//! `dry-vault`), mounts `/persist` (`DRYFS_v1`), runs security/fault-recovery audits (`p3_sec` &
//! `p4_sec`), and dispatches supervisor IPC requests over `SupervisorCtx`.

use crate::abi_ext::{
    HWCHECK_MODE_DESKTOP_SAFE, HWCHECK_MODE_RECOVERY, HWCHECK_MODE_SAFE,
};
use crate::action::{AppId, DesktopAction};
use crate::hwdiag::mgr::{DiagManager, DIAG_SCRATCH_CAP};
use crate::init_legacy::{domain_spec, log_line, spawn_and_init_desktop_input};
use crate::island_audit::{run_island_poc_benchmarks, run_island_poc_lifecycle_audit};
use crate::gpu_audit::{
    run_gpu_scanout_audit, spawn_live_gpu, teardown_live_gpu, trigger_live_gpu_fault_and_recover,
    LiveGpuSession,
};
use crate::lx_drm_audit::{run_lx_drm_bochs_audit, teardown_live_lx_drm, LxDrmLiveSession};
use crate::lx_island_audit::{run_lx_island_audit, run_lx_net_audit};
use crate::shell::LineBuilder;
use crate::sup_audit::{run_p3_sup_security_checks, run_p4_security_and_fault_audit};
pub use crate::sup_ctx::AppState;
use crate::sup_ctx::SupervisorCtx;
use crate::sup_hw_services::{handle_hw_query_ipc, init_hw_services_and_persist, HwInitCaps};
use crate::sup_services::{
    apply_input_prefs, spawn_and_bind_p3_display, spawn_and_bind_power_service,
    spawn_and_seed_vfs, spawn_blk_service, spawn_vault_service, sync_live_layout_pref,
    teardown_p3_core_services_with_hw, FinalPowerMode, SupervisorPrefs,
};
use crate::vfs_proto::vfs_corrupt_sb0_test;
use crate::wm::SUP_BIT_WM_ACTION;
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_exit, sys_ipc_call_inline, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_ipc_reply_with_grant, sys_notify_create, sys_notify_wait, sys_proc_wait, sys_sysinfo,
    sys_write_debug, RootInitBootInfo, UserIpcRecvExt, UserSysInfo, BADGE_SUPERVISOR,
    BADGE_SUP_DISPLAY, BADGE_SUP_ROGUE, BADGE_SUP_SETTINGS, BADGE_SUP_SHELL, BADGE_SUP_SYSMON,
    BADGE_VAULT_SHELL, BADGE_VAULT_SYSMON, EAGAIN, ETIMEDOUT, IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS,
    IPC_OP_ERR_PERM, IPC_OP_PONG, IPC_OP_SUP_ACTION, IPC_OP_SUP_APP_EXITED,
    IPC_OP_SUP_CORRUPT_SB0_TEST, IPC_OP_SUP_GET_VAULT_CAP, IPC_OP_SUP_GET_VFS_CAP,
    IPC_OP_SUP_HELLO, IPC_OP_SUP_MINT_ROGUE_TEST, IPC_OP_SUP_QUERY_APPS, IPC_OP_SUP_QUERY_PREFS,
    IPC_OP_SUP_RECONNECT_INPUT, IPC_OP_SUP_SHUTDOWN, IPC_OP_WM_POLL_ACTION, IPC_OP_WM_QUERY_STATE,
    MSG_FLAG_NON_BLOCKING, RIGHT_CALL, RIGHT_READ, RIGHT_SEND,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;

static mut DIAG_SCRATCH: [u8; DIAG_SCRATCH_CAP] = [0u8; DIAG_SCRATCH_CAP];
static mut DIAG_MGR: DiagManager = DiagManager::empty();

/// Upper bound (`~2 s` at `3 GHz`) for reaping a crashed `dry-input` before refusing a restart.
const INPUT_REAP_TIMEOUT_TSC: u64 = 6_000_000_000;

#[inline]
pub const fn is_action_badge_authorized(badge: u64) -> bool {
    matches!(
        badge,
        BADGE_SUP_SHELL | BADGE_SUP_SYSMON | BADGE_SUP_SETTINGS | BADGE_SUP_DISPLAY
    )
}

pub fn run_p3_desktop_session(boot_info_addr: u64, boot: &RootInitBootInfo) -> ! {
    if boot.input_mod_cap == 0
        || boot.shell_mod_cap == 0
        || boot.sysmon_mod_cap == 0
        || boot.irq1_cap == 0
        || boot.i8042_port_cap == 0
    {
        sys_exit(139);
    }

    let _ = sys_write_debug(
        b"[DRY-INIT] Starting Milestone 2A-3/2B native desktop session in CPL3\n",
    );
    let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let poll_timeout_tsc = 40_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    let sup_ep = match sys_ep_create(ipc_spec) {
        Ok(c) => c,
        Err(_) => sys_exit(147),
    };
    let sup_notif = match sys_notify_create(ipc_spec) {
        Ok(c) => c,
        Err(_) => sys_exit(147),
    };

    let (vfs_handle, vfs_ep, vfs_sup_cap) = match spawn_and_seed_vfs(boot_info_addr, boot) {
        Ok(v) => v,
        Err(code) => sys_exit(code),
    };
    let (mut blk_handle, mut blk_ep, mut blk_sup_cap, blk_sectors) = match spawn_blk_service(boot) {
        Ok(v) => v,
        Err(code) => sys_exit(code),
    };
    let (pwr_handle, pwr_ep, pwr_sup_cap) = match spawn_and_bind_power_service(boot) {
        Ok(v) => v,
        Err(code) => sys_exit(code),
    };
    let (disp_handle, disp_ep, disp_sup_cap) =
        match spawn_and_bind_p3_display(boot, sup_ep, sup_notif) {
            Ok(v) => v,
            Err(code) => sys_exit(code),
        };
    let (mut input_handle, input_ep) = match spawn_and_init_desktop_input(boot, disp_ep) {
        Ok(v) => v,
        Err(code) => sys_exit(code),
    };
    let input_sup_cap = sys_cap_derive(input_ep, cli_rights, 0, BADGE_SUPERVISOR).unwrap_or(0);

    let mut init_prefs = SupervisorPrefs::new();
    let mut hw_state = init_hw_services_and_persist(
        boot_info_addr,
        boot,
        HwInitCaps {
            disp_ep,
            disp_sup_cap,
            input_sup_cap,
            vfs_ep,
            vfs_sup_cap,
            blk_ep,
            blk_sup_cap,
            blk_sectors,
            tsc_mhz: sys.tsc_mhz,
        },
        &mut init_prefs,
    );
    // SAFETY: Single-threaded supervisor loop in `dry-init` exclusively owns `DIAG_SCRATCH` and `DIAG_MGR`.
    let diag_scratch = unsafe { &mut *core::ptr::addr_of_mut!(DIAG_SCRATCH) };
    let diag_mgr = unsafe { &mut *core::ptr::addr_of_mut!(DIAG_MGR) };
    diag_mgr.init_at_boot(
        boot_info_addr,
        boot.boot_pci_func_count,
        &hw_state,
        sys.tsc_mhz,
        sys.online_cpus,
        diag_scratch,
    );
    if (diag_mgr.ctx.ext.hwval_flags & crate::abi_ext::HWVAL_FLAG_TEST_SEC_PROBE) != 0 {
        crate::sup_audit::run_asc8_m2_security_probe(&hw_state);
    }
    let (vault_handle, vault_ep, vault_sup_cap) =
        match spawn_vault_service(boot, vfs_ep, disp_sup_cap) {
            Ok(v) => v,
            Err(code) => sys_exit(code),
        };

    let mut ctx = SupervisorCtx::new(
        boot,
        sup_ep,
        disp_sup_cap,
        input_ep,
        input_sup_cap,
        vfs_ep,
        vfs_sup_cap,
        vault_ep,
        vault_sup_cap,
        pwr_sup_cap,
        sys.tsc_mhz,
    );
    ctx.prefs = init_prefs;

    if ctx.launch_or_focus(AppId::Terminal).is_err() {
        sys_exit(148);
    }
    if ctx.launch_or_focus(AppId::LifeStation).is_err() {
        sys_exit(165);
    }

    let mut sec_audit_done =
        !matches!(boot.session_mode, 11 | 21 | 25 | 26 | 27 | 28 | 29 | 31);
    let mut gpu_live: Option<LiveGpuSession> =
        if boot.pci_gpu_cap != 0
            && boot.gpu_mod_cap != 0
            && !matches!(boot.session_mode, 25..=29 | 31)
            && !matches!(
                diag_mgr.boot_hwcheck_mode,
                HWCHECK_MODE_SAFE | HWCHECK_MODE_DESKTOP_SAFE | HWCHECK_MODE_RECOVERY
            )
        {
            spawn_live_gpu(boot, disp_sup_cap).ok()
        } else {
            None
        };
    let mut lx_drm_live: Option<LxDrmLiveSession> = None;
    let mut restarts = 0u64;

    loop {
        ctx.reap_exited_apps(None);

        if let Ok(bits) = sys_notify_wait(sup_notif, MSG_FLAG_NON_BLOCKING as u64, 0) {
            if (bits & SUP_BIT_WM_ACTION) != 0 {
                ctx.drain_wm_actions();
            }
        }
        if ctx.gpu_fault_pending {
            ctx.gpu_fault_pending = false;
            let _ = trigger_live_gpu_fault_and_recover(boot, &mut gpu_live, disp_sup_cap);
        }
        if ctx.final_power_mode != FinalPowerMode::None {
            break;
        }

        if !sec_audit_done
            && ctx.slots[AppId::Terminal as usize].ready_reported
            && ctx.slots[AppId::LifeStation as usize].ready_reported
        {
            let windows_ready = if disp_sup_cap != 0 {
                sys_ipc_call_inline(
                    disp_sup_cap,
                    pack_msg_tag(IPC_OP_WM_QUERY_STATE, 0, 1, 0),
                    [0; 4],
                    0,
                )
                .is_ok_and(|(_, _, qmrs)| qmrs[3] >= 2)
            } else {
                true
            };
            if windows_ready {
                if boot.session_mode == 11 || boot.session_mode == 21 {
                    let bad_ok =
                        ctx.execute_action(DesktopAction::from_words(0xdead_beef, 999))
                            == Err(IPC_OP_ERR_INVAL)
                            && ctx.execute_action(DesktopAction::from_words(1, 99))
                                == Err(IPC_OP_ERR_INVAL);
                    if let Err(code) = run_p3_sup_security_checks(sup_ep, disp_ep, pwr_ep, bad_ok) {
                        sys_exit(code);
                    }
                }
                if boot.session_mode == 21 {
                    if let Err(code) = run_p4_security_and_fault_audit(
                        boot,
                        vfs_ep,
                        vfs_sup_cap,
                        vault_sup_cap,
                        &mut blk_handle,
                        &mut blk_ep,
                        &mut blk_sup_cap,
                    ) {
                        let mut elb = LineBuilder::new();
                        elb.push_bytes(b"[DRY-P4-SEC-FAIL] code=");
                        elb.push_u64(code as u64);
                        log_line(&elb);
                        sys_exit(code);
                    }
                    hw_state.blk_sup_cap = blk_sup_cap;
                } else if boot.session_mode == 25 {
                    if let Err(code) =
                        run_island_poc_lifecycle_audit(boot, vfs_sup_cap, vault_sup_cap)
                    {
                        let mut elb = LineBuilder::new();
                        elb.push_bytes(b"[DRY-ISLAND-POC-FAIL] code=");
                        elb.push_u64(code as u64);
                        log_line(&elb);
                        sys_exit(code);
                    }
                } else if boot.session_mode == 26 {
                    if let Err(code) = run_island_poc_benchmarks(boot) {
                        let mut elb = LineBuilder::new();
                        elb.push_bytes(b"[DRY-ISLAND-BENCH-FAIL] code=");
                        elb.push_u64(code as u64);
                        log_line(&elb);
                        sys_exit(code);
                    }
                } else if boot.session_mode == 27 {
                    if let Err(code) = run_lx_island_audit(boot, vfs_sup_cap, vault_sup_cap) {
                        let mut elb = LineBuilder::new();
                        elb.push_bytes(b"[DRY-LX-RNG-FAIL] code=");
                        elb.push_u64(code as u64);
                        log_line(&elb);
                        sys_exit(code);
                    }
                } else if boot.session_mode == 28 {
                    match run_gpu_scanout_audit(boot, disp_sup_cap, vfs_sup_cap, vault_sup_cap) {
                        Ok(live) => gpu_live = Some(live),
                        Err(code) => {
                            let mut elb = LineBuilder::new();
                            elb.push_bytes(b"[DRY-GPU-FAIL] code=");
                            elb.push_u64(code as u64);
                            log_line(&elb);
                            sys_exit(code);
                        }
                    }
                } else if boot.session_mode == 29 {
                    match run_lx_drm_bochs_audit(
                        boot_info_addr,
                        boot,
                        vfs_sup_cap,
                        vault_sup_cap,
                    ) {
                        Ok(live) => lx_drm_live = Some(live),
                        Err(code) => {
                            let mut elb = LineBuilder::new();
                            elb.push_bytes(b"[DRY-LX-DRM-FAIL] code=");
                            elb.push_u64(code as u64);
                            log_line(&elb);
                            sys_exit(code);
                        }
                    }
                } else if boot.session_mode == 31 {
                    if let Err(code) =
                        run_lx_net_audit(boot_info_addr, boot, vfs_sup_cap, vault_sup_cap)
                    {
                        sys_exit(code);
                    }
                }
                sec_audit_done = true;
            }
        }

        let mut ext = UserIpcRecvExt::default();
        let (tag, reply_tok, mrs) = match sys_ipc_recv_ext(sup_ep, 0, poll_timeout_tsc, &mut ext) {
            Ok(v) => v,
            Err(ETIMEDOUT | EAGAIN) => continue,
            Err(_) => break,
        };

        let op = msg_tag_label(tag);
        match op {
            IPC_OP_WM_POLL_ACTION => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [0; 4],
                    );
                }
                ctx.drain_wm_actions();
            }
            IPC_OP_SUP_HELLO => {
                let Some(app) = SupervisorCtx::app_from_sup_badge(ext.sender_badge) else {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    ctx.unauth_rejects = ctx.unauth_rejects.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                            [0; 4],
                        );
                    }
                    continue;
                };
                let idx = app as usize;
                if ext.transferred_cap != 0 {
                    if ctx.slots[idx].notif_cap != 0 {
                        let _ = sys_cap_revoke(ctx.slots[idx].notif_cap, 1);
                    }
                    ctx.slots[idx].notif_cap = ext.transferred_cap;
                }
                if !ctx.slots[idx].ready_reported {
                    ctx.slots[idx].ready_reported = true;
                    let dt = rdtsc_user().saturating_sub(ctx.slots[idx].spawn_start_tsc);
                    ctx.launch_lat.record(dt);
                    if !ctx.boot_interactive_logged && app == AppId::Terminal {
                        ctx.boot_interactive_logged = true;
                        let _ = sys_sysinfo(&mut sys);
                        let mut blb = LineBuilder::new();
                        blb.push_bytes(b"[DRY-P3-BENCH] boot_to_interactive_ms=");
                        blb.push_u64(sys.uptime_ms);
                        blb.push_bytes(b" elapsed_tsc=");
                        blb.push_u64(sys.elapsed_tsc);
                        log_line(&blb);
                    }
                }
                let launches = ctx.slots[idx].launches as u64;
                if disp_ep != 0 {
                    if sys_ipc_reply_with_grant(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [boot.session_mode, launches, 0, 0],
                        disp_ep,
                        cli_rights,
                        app.win_badge(),
                    )
                    .is_err()
                    {
                        sys_exit(149);
                    }
                } else if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [boot.session_mode, launches, 0, 0],
                    );
                }
            }
            IPC_OP_SUP_MINT_ROGUE_TEST => {
                if (boot.session_mode != 11 && boot.session_mode != 21)
                    || ext.sender_badge != BADGE_SUP_SHELL
                {
                    ctx.unauth_rejects = ctx.unauth_rejects.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                            [0; 4],
                        );
                    }
                    continue;
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_with_grant(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [1, 0, 0, 0],
                        sup_ep,
                        cli_rights,
                        BADGE_SUP_ROGUE,
                    );
                }
            }
            IPC_OP_SUP_CORRUPT_SB0_TEST => {
                if boot.session_mode < 20
                    || ext.sender_badge != BADGE_SUP_SHELL
                    || ctx.vfs_sup_cap == 0
                {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                            [0; 4],
                        );
                    }
                    continue;
                }
                let ok = vfs_corrupt_sb0_test(ctx.vfs_sup_cap).is_ok();
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(if ok { IPC_OP_PONG } else { IPC_OP_ERR_INVAL }, 0, 1, 0),
                        [u64::from(ok), 0, 0, 0],
                    );
                }
            }
            IPC_OP_SUP_QUERY_PREFS => {
                sync_live_layout_pref(ctx.disp_sup_cap, &mut ctx.prefs);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [ctx.prefs.pack_mr0(), 0, 0, 0],
                    );
                }
            }
            IPC_OP_SUP_GET_VFS_CAP => {
                let Some(app) = SupervisorCtx::app_from_sup_badge(ext.sender_badge) else {
                    ctx.unauth_rejects = ctx.unauth_rejects.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                            [0; 4],
                        );
                    }
                    continue;
                };
                if ctx.vfs_ep == 0 {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_NOSYS, 0, 1, 0),
                            [0; 4],
                        );
                    }
                } else if reply_tok != 0 {
                    let _ = sys_ipc_reply_with_grant(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [app.compartment_id() as u64, 0, 0, 0],
                        ctx.vfs_ep,
                        cli_rights,
                        app.compartment_id() as u64,
                    );
                }
            }
            IPC_OP_SUP_GET_VAULT_CAP => {
                let vbadge = match ext.sender_badge {
                    BADGE_SUP_SHELL => BADGE_VAULT_SHELL,
                    BADGE_SUP_SYSMON => BADGE_VAULT_SYSMON,
                    _ => {
                        ctx.unauth_rejects = ctx.unauth_rejects.wrapping_add(1);
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                sup_ep,
                                reply_tok,
                                pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                                [0; 4],
                            );
                        }
                        continue;
                    }
                };
                if ctx.vault_ep == 0 {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_NOSYS, 0, 1, 0),
                            [0; 4],
                        );
                    }
                } else if reply_tok != 0 {
                    let _ = sys_ipc_reply_with_grant(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [1, 0, 0, 0],
                        ctx.vault_ep,
                        cli_rights,
                        vbadge,
                    );
                }
            }
            IPC_OP_SUP_QUERY_APPS => {
                let words = if mrs[0] == 1 {
                    [
                        ctx.slots[4].pack_query_word(),
                        ctx.slots[5].pack_query_word(),
                        0,
                        0,
                    ]
                } else {
                    [
                        ctx.slots[0].pack_query_word(),
                        ctx.slots[1].pack_query_word(),
                        ctx.slots[2].pack_query_word(),
                        ctx.slots[3].pack_query_word(),
                    ]
                };
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        words,
                    );
                }
            }
            IPC_OP_SUP_ACTION => {
                if !is_action_badge_authorized(ext.sender_badge) {
                    ctx.unauth_rejects = ctx.unauth_rejects.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                            [ctx.unauth_rejects, 0, 0, 0],
                        );
                    }
                    continue;
                }
                let act = DesktopAction::from_words(mrs[0], mrs[1]);
                let caller_app = SupervisorCtx::app_from_sup_badge(ext.sender_badge);
                let self_terminating = matches!(
                    (act, caller_app),
                    (DesktopAction::CloseApp(a), Some(c)) | (DesktopAction::RestartApp(a), Some(c)) if a == c
                );
                if self_terminating && reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                    let _ = ctx.execute_action(act);
                } else {
                    let res = if matches!(act, DesktopAction::TriggerGpuFaultTest) {
                        trigger_live_gpu_fault_and_recover(boot, &mut gpu_live, disp_sup_cap)
                    } else {
                        ctx.execute_action(act)
                    };
                    match res {
                        Ok(out_mrs) => {
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    sup_ep,
                                    reply_tok,
                                    pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                                    out_mrs,
                                );
                            }
                        }
                        Err(err_op) => {
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    sup_ep,
                                    reply_tok,
                                    pack_msg_tag(err_op, 0, 1, 0),
                                    [0; 4],
                                );
                            }
                        }
                    }
                }
            }
            IPC_OP_SUP_APP_EXITED => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                if let Some(app) = SupervisorCtx::app_from_sup_badge(ext.sender_badge) {
                    ctx.reap_exited_apps(Some(app));
                }
            }
            IPC_OP_SUP_RECONNECT_INPUT => {
                // `AUD-U1`: only trusted desktop badges may request an input restart, and a live
                // `dry-input` must never block or terminate the root supervisor (bounded reap).
                let reaped = is_action_badge_authorized(ext.sender_badge)
                    && sys_proc_wait(input_handle, 0, INPUT_REAP_TIMEOUT_TSC).is_ok();
                if !reaped {
                    ctx.unauth_rejects = ctx.unauth_rejects.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                            [0; 4],
                        );
                    }
                    continue;
                }
                if ctx.input_sup_cap != 0 {
                    let _ = sys_cap_revoke(ctx.input_sup_cap, 1);
                }
                let _ = sys_cap_revoke(ctx.input_ep, 2);
                let (new_handle, new_ep) = match spawn_and_init_desktop_input(boot, disp_ep) {
                    Ok(v) => v,
                    Err(code) => sys_exit(code),
                };
                input_handle = new_handle;
                ctx.input_ep = new_ep;
                ctx.input_sup_cap =
                    sys_cap_derive(new_ep, cli_rights, 0, BADGE_SUPERVISOR).unwrap_or(0);
                let _ = apply_input_prefs(ctx.input_sup_cap, &mut ctx.prefs, None, None);
                restarts += 1;
                let _ = sys_write_debug(
                    b"[DRY-INIT] Reaped crashed dry-input, revoked stale cap & restarted\n",
                );
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_with_grant(
                        sup_ep,
                        reply_tok,
                        IPC_OP_PONG,
                        [restarts, 0, 0, 0],
                        ctx.input_ep,
                        cli_rights,
                        0x54e1,
                    );
                }
            }
            IPC_OP_SUP_SHUTDOWN => {
                if !is_action_badge_authorized(ext.sender_badge) {
                    ctx.unauth_rejects = ctx.unauth_rejects.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            sup_ep,
                            reply_tok,
                            pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                            [ctx.unauth_rejects, 0, 0, 0],
                        );
                    }
                    continue;
                }
                if !sec_audit_done {
                    let bad_ok =
                        ctx.execute_action(DesktopAction::from_words(0xdead_beef, 999))
                            == Err(IPC_OP_ERR_INVAL);
                    if let Err(code) = run_p3_sup_security_checks(sup_ep, disp_ep, pwr_ep, bad_ok) {
                        sys_exit(code);
                    }
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                ctx.final_power_mode = if boot.session_mode == 13 || boot.session_mode == 23 {
                    FinalPowerMode::HardwareShutdownS5
                } else {
                    FinalPowerMode::ExitHarness
                };
            }
            _ => {
                let auth = is_action_badge_authorized(ext.sender_badge)
                    || ext.sender_badge == BADGE_SUPERVISOR;
                if !diag_mgr.handle_ipc(sup_ep, reply_tok, op, mrs, auth, diag_scratch)
                    && !handle_hw_query_ipc(sup_ep, reply_tok, op, mrs, auth, &hw_state)
                    && reply_tok != 0
                {
                    let _ = sys_ipc_reply_inline(
                        sup_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [0; 4],
                    );
                }
            }
        }

        if ctx.final_power_mode != FinalPowerMode::None {
            break;
        }
    }

    for app in AppId::ALL {
        if ctx.slots[app as usize].state == AppState::Running {
            let _ = ctx.close_app(app);
        }
    }

    let (l50, l95, l99) = ctx.launch_lat.percentiles();
    let mut blb = LineBuilder::new();
    blb.push_bytes(b"[DRY-SUP-STATS] launch_cyc=");
    blb.push_u64(l50);
    blb.push_byte(b'/');
    blb.push_u64(l95);
    blb.push_byte(b'/');
    blb.push_u64(l99);
    blb.push_bytes(b" unauth_rej=");
    blb.push_u64(ctx.unauth_rejects);
    blb.push_bytes(b" inval_rej=");
    blb.push_u64(ctx.inval_rejects);
    log_line(&blb);

    if let Some(live_drm) = lx_drm_live.take() {
        teardown_live_lx_drm(live_drm);
    }

    if let Some(live) = gpu_live.take() {
        teardown_live_gpu(live, disp_sup_cap);
    }

    if let Err(code) = teardown_p3_core_services_with_hw(
        ctx.input_sup_cap,
        ctx.input_ep,
        input_handle,
        vault_handle,
        vault_ep,
        vault_sup_cap,
        vfs_handle,
        vfs_ep,
        vfs_sup_cap,
        blk_handle,
        blk_ep,
        blk_sup_cap,
        Some(&mut hw_state),
        disp_sup_cap,
        disp_ep,
        disp_handle,
        pwr_handle,
        pwr_ep,
        pwr_sup_cap,
        sup_notif,
        sup_ep,
        ctx.final_power_mode,
    ) {
        sys_exit(code);
    }
    let _ = sys_write_debug(b"[DRY-INIT] P3 desktop session verified & reaped cleanly\n");
    sys_exit(0);
}
