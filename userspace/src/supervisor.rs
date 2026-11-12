// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Unified Ring 3 Application Lifecycle, Launcher Registry & Desktop Supervisor (`Milestone 2A-3`).
//!
//! Responsibilities:
//! - Spawns core Ring 3 system services (`dry-display`, `dry-input`, and optional `dry-vfs` / `dry-power`).
//! - Manages isolated `SecDomain::Userspace` native applications (`Terminal`, `Life Station`,
//!   `Files`, `Settings`) with single-instance focus-or-spawn, clean close, restart, `#UD`/`#PF`
//!   fault isolation, automatic `WM_OWNER_GONE` window reaping, and launch latency telemetry.
//! - Enforces capability-badge authorization on `IPC_OP_SUP_ACTION` (`BADGE_SUP_ROGUE` / `BADGE_SUP_FILES`
//!   rejected with `IPC_OP_ERR_PERM`; malformed action words rejected with `IPC_OP_ERR_INVAL`).

use crate::action::{AppId, DesktopAction, LatencyRing, APP_COUNT};
use crate::init_legacy::{domain_spec, log_line, run_wm_security_audit, spawn_and_init_desktop_input};
use crate::shell::LineBuilder;
use crate::sup_services::{
    apply_blank_timeout, apply_display_prefs, apply_input_prefs, apply_layout_action,
    apply_session_visual_state, probe_power_badge_gate, request_suspend_s3,
    spawn_and_bind_p3_display, spawn_and_bind_power_service, spawn_and_seed_vfs,
    sync_live_layout_pref, teardown_p3_core_services, FinalPowerMode, SupervisorPrefs,
};
use crate::wm::{SessionVisualState, SUP_BIT_WM_ACTION};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_exit, sys_ipc_call_inline, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_ipc_reply_with_grant, sys_notify_create, sys_notify_wait, sys_proc_kill, sys_proc_spawn,
    sys_proc_wait, sys_sysinfo, sys_write_debug, RootInitBootInfo, UserIpcRecvExt, UserSysInfo,
    BADGE_SUPERVISOR, BADGE_SUP_DISPLAY, BADGE_SUP_FILES, BADGE_SUP_ROGUE, BADGE_SUP_SETTINGS,
    BADGE_SUP_SHELL, BADGE_SUP_SYSMON, EAGAIN, ETIMEDOUT, IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS,
    IPC_OP_ERR_PERM, IPC_OP_PONG, IPC_OP_SUP_ACTION, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_GET_VFS_CAP,
    IPC_OP_SUP_HELLO, IPC_OP_SUP_MINT_ROGUE_TEST, IPC_OP_SUP_QUERY_APPS, IPC_OP_SUP_QUERY_PREFS,
    IPC_OP_SUP_RECONNECT_INPUT, IPC_OP_SUP_SHUTDOWN, IPC_OP_WIN_EMPTY,
    IPC_OP_WM_FOCUS_OWNER, IPC_OP_WM_OWNER_GONE, IPC_OP_WM_POLL_ACTION, IPC_OP_WM_QUERY_STATE,
    IPC_OP_WM_TOGGLE_TILING, MSG_FLAG_NON_BLOCKING, RIGHT_CALL, RIGHT_READ, RIGHT_SEND,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Stopped = 0,
    Running = 1,
    Crashed = 2,
    Unavailable = 3,
}

#[derive(Clone, Copy)]
struct AppSlot {
    app: AppId,
    mod_cap: u64,
    state: AppState,
    handle: u64,
    pid: u32,
    notif_cap: u64,
    launches: u16,
    last_fault_vec: u8,
    spawn_start_tsc: u64,
    ready_reported: bool,
}

impl AppSlot {
    const fn new(app: AppId, mod_cap: u64) -> Self {
        Self {
            app,
            mod_cap,
            state: if mod_cap != 0 {
                AppState::Stopped
            } else {
                AppState::Unavailable
            },
            handle: 0,
            pid: 0,
            notif_cap: 0,
            launches: 0,
            last_fault_vec: 0,
            spawn_start_tsc: 0,
            ready_reported: false,
        }
    }

    #[inline]
    const fn pack_query_word(&self) -> u64 {
        (self.pid as u64)
            | ((self.state as u64) << 32)
            | ((self.last_fault_vec as u64) << 40)
            | ((self.launches as u64) << 48)
    }
}

struct SupervisorCtx {
    slots: [AppSlot; APP_COUNT],
    sup_ep: u64,
    disp_sup_cap: u64,
    input_ep: u64,
    input_sup_cap: u64,
    vfs_ep: u64,
    pwr_sup_cap: u64,
    tsc_mhz: u64,
    session_mode: u64,
    final_power_mode: FinalPowerMode,
    prefs: SupervisorPrefs,
    launch_lat: LatencyRing,
    unauth_rejects: u64,
    inval_rejects: u64,
    boot_interactive_logged: bool,
}

impl SupervisorCtx {
    #[allow(clippy::too_many_arguments)]
    fn new(
        boot: &RootInitBootInfo,
        sup_ep: u64,
        disp_sup_cap: u64,
        input_ep: u64,
        input_sup_cap: u64,
        vfs_ep: u64,
        pwr_sup_cap: u64,
        tsc_mhz: u64,
    ) -> Self {
        Self {
            slots: [
                AppSlot::new(AppId::Terminal, boot.shell_mod_cap),
                AppSlot::new(AppId::LifeStation, boot.sysmon_mod_cap),
                AppSlot::new(AppId::Files, boot.files_mod_cap),
                AppSlot::new(AppId::Settings, boot.settings_mod_cap),
            ],
            sup_ep,
            disp_sup_cap,
            input_ep,
            input_sup_cap,
            vfs_ep,
            pwr_sup_cap,
            tsc_mhz,
            session_mode: boot.session_mode,
            final_power_mode: FinalPowerMode::None,
            prefs: SupervisorPrefs::new(),
            launch_lat: LatencyRing::new(),
            unauth_rejects: 0,
            inval_rejects: 0,
            boot_interactive_logged: false,
        }
    }

    #[inline]
    fn running_mask(&self) -> u8 {
        let mut m = 0u8;
        for (i, s) in self.slots.iter().enumerate() {
            if s.state == AppState::Running {
                m |= 1u8 << i;
            }
        }
        m
    }

    fn sync_wm_running_mask(&self) {
        if self.disp_sup_cap != 0 {
            let _ = sys_ipc_call_inline(
                self.disp_sup_cap,
                pack_msg_tag(IPC_OP_WM_POLL_ACTION, 0, 2, 0),
                [self.running_mask() as u64, 1, 0, 0],
                0,
            );
        }
    }

    fn app_from_sup_badge(badge: u64) -> Option<AppId> {
        match badge {
            BADGE_SUP_SHELL => Some(AppId::Terminal),
            BADGE_SUP_SYSMON => Some(AppId::LifeStation),
            BADGE_SUP_FILES => Some(AppId::Files),
            BADGE_SUP_SETTINGS => Some(AppId::Settings),
            _ => None,
        }
    }

    fn launch_or_focus(&mut self, app: AppId) -> Result<(u32, u64), u32> {
        let idx = app as usize;
        if self.slots[idx].mod_cap == 0 {
            return Err(IPC_OP_ERR_NOSYS);
        }
        if self.slots[idx].state == AppState::Running {
            let pid = self.slots[idx].pid;
            let mut wid = 0u64;
            if self.disp_sup_cap != 0 {
                if let Ok((_, _, rmrs)) = sys_ipc_call_inline(
                    self.disp_sup_cap,
                    pack_msg_tag(IPC_OP_WM_FOCUS_OWNER, 0, 3, 0),
                    [app.win_badge(), self.running_mask() as u64, 1, 0],
                    0,
                ) {
                    wid = rmrs[0];
                }
            }
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-SUP] APP_FOCUS app=");
            lb.push_bytes(app.slug());
            lb.push_bytes(b" pid=");
            lb.push_u64(pid as u64);
            lb.push_bytes(b" wid=");
            lb.push_u64(wid);
            log_line(&lb);
            return Ok((pid, 0));
        }

        let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let grant1 = if app == AppId::Terminal {
            (self.input_ep, cli_rights)
        } else {
            (0, 0)
        };

        let t0 = rdtsc_user();
        let (handle, pid) = sys_proc_spawn(
            self.slots[idx].mod_cap,
            0,
            app.compartment_id(),
            (self.sup_ep, cli_rights, app.sup_badge()),
            grant1,
        )
        .map_err(|_| IPC_OP_ERR_INVAL)?;

        let slot = &mut self.slots[idx];
        if slot.notif_cap != 0 {
            let _ = sys_cap_revoke(slot.notif_cap, 1);
            slot.notif_cap = 0;
        }
        slot.state = AppState::Running;
        slot.handle = handle;
        slot.pid = pid as u32;
        slot.launches = slot.launches.saturating_add(1);
        slot.last_fault_vec = 0;
        slot.spawn_start_tsc = t0;
        slot.ready_reported = false;

        let launches = slot.launches;
        self.sync_wm_running_mask();

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-SUP] APP_SPAWNED app=");
        lb.push_bytes(app.slug());
        lb.push_bytes(b" pid=");
        lb.push_u64(pid);
        lb.push_bytes(b" launches=");
        lb.push_u64(launches as u64);
        log_line(&lb);

        Ok((pid as u32, 1))
    }

    fn close_app(&mut self, app: AppId) -> Result<u32, u32> {
        let idx = app as usize;
        if self.slots[idx].mod_cap == 0 {
            return Err(IPC_OP_ERR_NOSYS);
        }
        if self.slots[idx].state != AppState::Running {
            return Ok(0);
        }
        let old_pid = self.slots[idx].pid;
        let handle = self.slots[idx].handle;
        if self.slots[idx].notif_cap != 0 {
            let _ = sys_cap_revoke(self.slots[idx].notif_cap, 1);
            self.slots[idx].notif_cap = 0;
        }
        if self.disp_sup_cap != 0 {
            let _ = sys_ipc_call_inline(
                self.disp_sup_cap,
                pack_msg_tag(IPC_OP_WM_OWNER_GONE, 0, 1, 0),
                [app.win_badge(), 0, 0, 0],
                0,
            );
        }
        let _ = sys_proc_kill(handle);
        let _ = sys_proc_wait(handle, 0, 0);

        self.slots[idx].state = AppState::Stopped;
        self.slots[idx].pid = 0;
        self.slots[idx].handle = 0;
        self.slots[idx].last_fault_vec = 0;
        self.sync_wm_running_mask();

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-SUP] APP_CLOSED app=");
        lb.push_bytes(app.slug());
        lb.push_bytes(b" pid=");
        lb.push_u64(old_pid as u64);
        log_line(&lb);
        Ok(old_pid)
    }

    fn restart_app(&mut self, app: AppId) -> Result<u32, u32> {
        if self.slots[app as usize].mod_cap == 0 {
            return Err(IPC_OP_ERR_NOSYS);
        }
        if self.slots[app as usize].state == AppState::Running {
            let _ = self.close_app(app);
        }
        let (new_pid, _) = self.launch_or_focus(app)?;
        let launches = self.slots[app as usize].launches;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-SUP] APP_RESTARTED app=");
        lb.push_bytes(app.slug());
        lb.push_bytes(b" pid=");
        lb.push_u64(new_pid as u64);
        lb.push_bytes(b" launches=");
        lb.push_u64(launches as u64);
        log_line(&lb);
        Ok(new_pid)
    }

    fn reap_exited_apps(&mut self, blocking_app: Option<AppId>) {
        let mut changed = false;
        for idx in 0..APP_COUNT {
            if self.slots[idx].state != AppState::Running {
                continue;
            }
            let app = self.slots[idx].app;
            let flags = if blocking_app == Some(app) { 0 } else { 1 };
            if let Ok((w_kind, w_detail)) = sys_proc_wait(self.slots[idx].handle, flags, 0) {
                let old_pid = self.slots[idx].pid;
                if self.slots[idx].notif_cap != 0 {
                    let _ = sys_cap_revoke(self.slots[idx].notif_cap, 1);
                    self.slots[idx].notif_cap = 0;
                }
                if self.disp_sup_cap != 0 {
                    let _ = sys_ipc_call_inline(
                        self.disp_sup_cap,
                        pack_msg_tag(IPC_OP_WM_OWNER_GONE, 0, 1, 0),
                        [app.win_badge(), 0, 0, 0],
                        0,
                    );
                }
                let vec = if w_kind == 1 {
                    (w_detail & 0xff) as u8
                } else {
                    0
                };
                self.slots[idx].state = if w_kind == 1 {
                    AppState::Crashed
                } else {
                    AppState::Stopped
                };
                self.slots[idx].last_fault_vec = vec;
                self.slots[idx].pid = 0;
                self.slots[idx].handle = 0;
                changed = true;

                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-SUP] APP_REAPED app=");
                lb.push_bytes(app.slug());
                lb.push_bytes(b" pid=");
                lb.push_u64(old_pid as u64);
                lb.push_bytes(b" kind=");
                lb.push_u64(w_kind as u64);
                lb.push_bytes(b" vec=");
                lb.push_u64(vec as u64);
                lb.push_bytes(b" window_cleaned=true");
                log_line(&lb);
            }
        }
        if changed {
            self.sync_wm_running_mask();
        }
    }

    fn execute_action(&mut self, act: DesktopAction) -> Result<[u64; 4], u32> {
        match act {
            DesktopAction::None => {
                self.inval_rejects = self.inval_rejects.wrapping_add(1);
                Err(IPC_OP_ERR_INVAL)
            }
            DesktopAction::LaunchOrFocusApp(app) => {
                let (pid, spawned) = self.launch_or_focus(app)?;
                Ok([pid as u64, spawned, self.slots[app as usize].launches as u64, 0])
            }
            DesktopAction::CloseApp(app) => {
                let pid = self.close_app(app)?;
                Ok([pid as u64, 0, 0, 0])
            }
            DesktopAction::RestartApp(app) => {
                let pid = self.restart_app(app)?;
                Ok([pid as u64, self.slots[app as usize].launches as u64, 0, 0])
            }
            DesktopAction::ToggleTiling => {
                if self.disp_sup_cap != 0 {
                    if let Ok((_, _, rmrs)) = sys_ipc_call_inline(
                        self.disp_sup_cap,
                        pack_msg_tag(IPC_OP_WM_TOGGLE_TILING, 0, 1, 0),
                        [0; 4],
                        0,
                    ) {
                        self.prefs.layout_mode = (rmrs[0] & 0xff) as u8;
                        return Ok([rmrs[0], rmrs[1], 0, 0]);
                    }
                }
                Ok([0; 4])
            }
            DesktopAction::SetLayoutMode(m) => {
                apply_layout_action(self.disp_sup_cap, &mut self.prefs, m)
            }
            DesktopAction::SetKeymap(k) => {
                apply_input_prefs(self.input_sup_cap, &mut self.prefs, Some(k), None)
            }
            DesktopAction::SetPointerSpeed(s) => {
                apply_input_prefs(self.input_sup_cap, &mut self.prefs, None, Some(s))
            }
            DesktopAction::SetThemeContrast(flags) => {
                apply_display_prefs(self.disp_sup_cap, &mut self.prefs, flags)
            }
            DesktopAction::SetBlankTimeoutSec(sec) => {
                apply_blank_timeout(self.disp_sup_cap, self.tsc_mhz, &mut self.prefs, sec)
            }
            DesktopAction::BlankDisplayNow => {
                apply_session_visual_state(self.disp_sup_cap, SessionVisualState::DisplayBlanked)
            }
            DesktopAction::LockSession => {
                apply_session_visual_state(self.disp_sup_cap, SessionVisualState::SessionLocked)
            }
            DesktopAction::UnlockSession => {
                apply_session_visual_state(self.disp_sup_cap, SessionVisualState::Active)
            }
            DesktopAction::RequestShutdown => {
                self.final_power_mode = if self.session_mode == 13 {
                    FinalPowerMode::HardwareShutdownS5
                } else {
                    FinalPowerMode::ExitHarness
                };
                Ok([1, 0, 0, 0])
            }
            DesktopAction::RequestPoweroffHw => {
                self.final_power_mode = FinalPowerMode::HardwareShutdownS5;
                Ok([3, 0, 0, 0])
            }
            DesktopAction::RequestReboot => {
                self.final_power_mode = FinalPowerMode::HardwareReboot;
                Ok([2, 0, 0, 0])
            }
            DesktopAction::RequestSuspendS3 => request_suspend_s3(self.pwr_sup_cap),
            DesktopAction::CloseWindow(_) | DesktopAction::CycleFocus => Ok([0; 4]),
        }
    }

    fn drain_wm_actions(&mut self) {
        if self.disp_sup_cap == 0 {
            return;
        }
        for _ in 0..16 {
            let Ok((rtag, _, rmrs)) = sys_ipc_call_inline(
                self.disp_sup_cap,
                pack_msg_tag(IPC_OP_WM_POLL_ACTION, 0, 2, 0),
                [self.running_mask() as u64, 0, 0, 0],
                0,
            ) else {
                break;
            };
            if msg_tag_label(rtag) == IPC_OP_WIN_EMPTY {
                break;
            }
            let act = DesktopAction::from_words(rmrs[0], rmrs[1]);
            let _ = self.execute_action(act);
            if rmrs[2] == 0 || self.final_power_mode != FinalPowerMode::None {
                break;
            }
        }
    }
}

fn run_p3_sup_security_audit(
    sup_ep: u64,
    disp_ep: u64,
    pwr_ep: u64,
    ctx: &mut SupervisorCtx,
) -> Result<(), i32> {
    if disp_ep != 0 {
        run_wm_security_audit(disp_ep)?;
    }
    probe_power_badge_gate(pwr_ep)?;

    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let rogue_sup = sys_cap_derive(sup_ep, cli_rights, 0, BADGE_SUP_ROGUE).map_err(|_| 184)?;
    let _ = sys_cap_revoke(rogue_sup, 1);

    if is_action_badge_authorized(BADGE_SUP_ROGUE)
        || is_action_badge_authorized(BADGE_SUP_FILES)
        || is_action_badge_authorized(0)
    {
        return Err(185);
    }
    if !is_action_badge_authorized(BADGE_SUP_SHELL)
        || !is_action_badge_authorized(BADGE_SUP_SETTINGS)
    {
        return Err(186);
    }
    if ctx.execute_action(DesktopAction::from_words(0xdead_beef, 999)) != Err(IPC_OP_ERR_INVAL) {
        return Err(187);
    }
    if ctx.execute_action(DesktopAction::from_words(1, 99)) != Err(IPC_OP_ERR_INVAL) {
        return Err(188);
    }

    let _ = sys_write_debug(
        b"[DRY-P3-SEC] PASS: badge_gate_verified=true bad_action_rejected=true\n",
    );
    Ok(())
}

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

    let _ = sys_write_debug(b"[DRY-INIT] Starting Milestone 2A-3 native desktop session in CPL3\n");
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

    let mut ctx = SupervisorCtx::new(
        boot,
        sup_ep,
        disp_sup_cap,
        input_ep,
        input_sup_cap,
        vfs_ep,
        pwr_sup_cap,
        sys.tsc_mhz,
    );
    if ctx.launch_or_focus(AppId::Terminal).is_err() {
        sys_exit(148);
    }
    if ctx.launch_or_focus(AppId::LifeStation).is_err() {
        sys_exit(165);
    }

    let mut sec_audit_done = boot.session_mode != 11;
    let mut restarts = 0u64;

    loop {
        ctx.reap_exited_apps(None);

        if let Ok(bits) = sys_notify_wait(sup_notif, MSG_FLAG_NON_BLOCKING as u64, 0) {
            if (bits & SUP_BIT_WM_ACTION) != 0 {
                ctx.drain_wm_actions();
            }
        }
        if ctx.final_power_mode != FinalPowerMode::None {
            break;
        }

        if !sec_audit_done
            && ctx.slots[AppId::Terminal as usize].ready_reported
            && ctx.slots[AppId::LifeStation as usize].ready_reported
        {
            if disp_sup_cap != 0 {
                if let Ok((_, _, qmrs)) = sys_ipc_call_inline(
                    disp_sup_cap,
                    pack_msg_tag(IPC_OP_WM_QUERY_STATE, 0, 1, 0),
                    [0; 4],
                    0,
                ) {
                    if qmrs[3] >= 2 {
                        if let Err(code) =
                            run_p3_sup_security_audit(sup_ep, disp_ep, pwr_ep, &mut ctx)
                        {
                            sys_exit(code);
                        }
                        sec_audit_done = true;
                    }
                }
            } else {
                if let Err(code) = run_p3_sup_security_audit(sup_ep, disp_ep, pwr_ep, &mut ctx) {
                    sys_exit(code);
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
                if boot.session_mode != 11 || ext.sender_badge != BADGE_SUP_SHELL {
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
            IPC_OP_SUP_QUERY_APPS => {
                let words = [
                    ctx.slots[0].pack_query_word(),
                    ctx.slots[1].pack_query_word(),
                    ctx.slots[2].pack_query_word(),
                    ctx.slots[3].pack_query_word(),
                ];
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
                // Reply BEFORE executing `CloseApp` / `RestartApp` if an app closes/restarts ITSELF
                // (e.g. `close terminal` or `restart terminal` typed inside `Terminal`) so `kill_process`
                // doesn't race with `reply_tok`!
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
                    match ctx.execute_action(act) {
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
                let (w_kind, w_detail) = match sys_proc_wait(input_handle, 0, 0) {
                    Ok(v) => v,
                    Err(_) => sys_exit(150),
                };
                if w_kind != 1 || (w_detail & 0xff) != 6 {
                    sys_exit(151);
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
                    if let Err(code) = run_p3_sup_security_audit(sup_ep, disp_ep, pwr_ep, &mut ctx)
                    {
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
                ctx.final_power_mode = if boot.session_mode == 13 {
                    FinalPowerMode::HardwareShutdownS5
                } else {
                    FinalPowerMode::ExitHarness
                };
            }
            _ => {
                if reply_tok != 0 {
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

    // Controlled teardown of all running user applications before stopping core services.
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

    if let Err(code) = teardown_p3_core_services(
        ctx.input_sup_cap,
        ctx.input_ep,
        input_handle,
        vfs_handle,
        vfs_ep,
        vfs_sup_cap,
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
