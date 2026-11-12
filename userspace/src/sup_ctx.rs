// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Supervisor Application Lifecycle Model & Action Executor (`SupervisorCtx`, `Milestone 2A-3` & `2B`).
//!
//! Isolates application slot state, spawn/close/restart/reap transitions, `DRY Vault` unlock/lock
//! coordination, and runtime preference persistence away from the `run_p3_desktop_session` IPC loop.

use crate::action::{AppId, DesktopAction, LatencyRing, APP_COUNT};
use crate::init_legacy::log_line;
use crate::shell::LineBuilder;
use crate::sup_services::{
    apply_blank_timeout, apply_display_prefs, apply_input_prefs, apply_layout_action,
    apply_session_visual_state, request_suspend_s3, save_prefs_to_persist, FinalPowerMode,
    SupervisorPrefs,
};
use crate::vault_crypto::scrub_bytes;
use crate::vault_proto::{
    unpack_passphrase_mrs, unpack_vault_label, vault_lock_and_revoke, vault_unlock,
};
use crate::wm::SessionVisualState;
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_revoke, sys_ipc_call_inline, sys_proc_kill,
    sys_proc_spawn, sys_proc_wait, RootInitBootInfo, BADGE_SUP_FILES, BADGE_SUP_SETTINGS,
    BADGE_SUP_SHELL, BADGE_SUP_SYSMON, IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM,
    IPC_OP_PONG, IPC_OP_WIN_EMPTY, IPC_OP_WM_FOCUS_OWNER, IPC_OP_WM_OWNER_GONE,
    IPC_OP_WM_POLL_ACTION, IPC_OP_WM_SET_AUTH_LOCK, IPC_OP_WM_TAKE_LOCK_PIN,
    IPC_OP_WM_TOGGLE_TILING, RIGHT_CALL, RIGHT_READ, RIGHT_SEND,
};

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Stopped = 0,
    Running = 1,
    Crashed = 2,
    Unavailable = 3,
}

#[derive(Clone, Copy)]
pub struct AppSlot {
    pub app: AppId,
    pub mod_cap: u64,
    pub state: AppState,
    pub handle: u64,
    pub pid: u32,
    pub notif_cap: u64,
    pub launches: u16,
    pub last_fault_vec: u8,
    pub spawn_start_tsc: u64,
    pub ready_reported: bool,
}

impl AppSlot {
    pub const fn new(app: AppId, mod_cap: u64) -> Self {
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
    pub const fn pack_query_word(&self) -> u64 {
        (self.pid as u64)
            | ((self.state as u64) << 32)
            | ((self.last_fault_vec as u64) << 40)
            | ((self.launches as u64) << 48)
    }
}

pub struct SupervisorCtx {
    pub slots: [AppSlot; APP_COUNT],
    pub sup_ep: u64,
    pub disp_sup_cap: u64,
    pub input_ep: u64,
    pub input_sup_cap: u64,
    pub vfs_ep: u64,
    pub vfs_sup_cap: u64,
    pub vault_ep: u64,
    pub vault_sup_cap: u64,
    pub pwr_sup_cap: u64,
    pub tsc_mhz: u64,
    pub session_mode: u64,
    pub final_power_mode: FinalPowerMode,
    pub prefs: SupervisorPrefs,
    pub vault_auth_failures: u32,
    pub launch_lat: LatencyRing,
    pub unauth_rejects: u64,
    pub inval_rejects: u64,
    pub boot_interactive_logged: bool,
}

impl SupervisorCtx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        boot: &RootInitBootInfo,
        sup_ep: u64,
        disp_sup_cap: u64,
        input_ep: u64,
        input_sup_cap: u64,
        vfs_ep: u64,
        vfs_sup_cap: u64,
        vault_ep: u64,
        vault_sup_cap: u64,
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
            vfs_sup_cap,
            vault_ep,
            vault_sup_cap,
            pwr_sup_cap,
            tsc_mhz,
            session_mode: boot.session_mode,
            final_power_mode: FinalPowerMode::None,
            prefs: SupervisorPrefs::new(),
            vault_auth_failures: 0,
            launch_lat: LatencyRing::new(),
            unauth_rejects: 0,
            inval_rejects: 0,
            boot_interactive_logged: false,
        }
    }

    #[inline]
    pub fn running_mask(&self) -> u8 {
        let mut m = 0u8;
        for (i, s) in self.slots.iter().enumerate() {
            if s.state == AppState::Running {
                m |= 1u8 << i;
            }
        }
        m
    }

    pub fn sync_wm_running_mask(&self) {
        if self.disp_sup_cap != 0 {
            let _ = sys_ipc_call_inline(
                self.disp_sup_cap,
                pack_msg_tag(IPC_OP_WM_POLL_ACTION, 0, 2, 0),
                [self.running_mask() as u64, 1, 0, 0],
                0,
            );
        }
    }

    pub fn app_from_sup_badge(badge: u64) -> Option<AppId> {
        match badge {
            BADGE_SUP_SHELL => Some(AppId::Terminal),
            BADGE_SUP_SYSMON => Some(AppId::LifeStation),
            BADGE_SUP_FILES => Some(AppId::Files),
            BADGE_SUP_SETTINGS => Some(AppId::Settings),
            _ => None,
        }
    }

    pub fn launch_or_focus(&mut self, app: AppId) -> Result<(u32, u64), u32> {
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

    pub fn close_app(&mut self, app: AppId) -> Result<u32, u32> {
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

    pub fn restart_app(&mut self, app: AppId) -> Result<u32, u32> {
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

    pub fn reap_exited_apps(&mut self, blocking_app: Option<AppId>) {
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

    fn handle_vault_unlock_submit(&mut self) -> Result<[u64; 4], u32> {
        if self.disp_sup_cap == 0 || self.vault_sup_cap == 0 {
            return Err(IPC_OP_ERR_NOSYS);
        }
        let (rtag, _, mut pin_mrs) = sys_ipc_call_inline(
            self.disp_sup_cap,
            pack_msg_tag(IPC_OP_WM_TAKE_LOCK_PIN, 0, 0, 0),
            [0; 4],
            0,
        )
        .map_err(|_| IPC_OP_ERR_INVAL)?;
        let (rop, plen) = unpack_vault_label(msg_tag_label(rtag));
        if rop != IPC_OP_PONG {
            pin_mrs.fill(0);
            return Err(IPC_OP_ERR_INVAL);
        }
        let mut pin_buf = [0u8; 32];
        let unpack_res = unpack_passphrase_mrs(plen, pin_mrs, &mut pin_buf);
        pin_mrs.fill(0);
        let Ok(n) = unpack_res else {
            scrub_bytes(&mut pin_buf);
            return Err(IPC_OP_ERR_INVAL);
        };

        let auth_res = vault_unlock(self.vault_sup_cap, &pin_buf[..n]);
        scrub_bytes(&mut pin_buf);
        match auth_res {
            Ok(epoch) => {
                self.vault_auth_failures = 0;
                let _ = sys_ipc_call_inline(
                    self.disp_sup_cap,
                    pack_msg_tag(IPC_OP_WM_SET_AUTH_LOCK, 0, 3, 0),
                    [1, 0, 1, 0],
                    0,
                );
                let out =
                    apply_session_visual_state(self.disp_sup_cap, SessionVisualState::Active)?;
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-SUP] VAULT_UNLOCK auth=ok epoch=");
                lb.push_u64(epoch as u64);
                log_line(&lb);
                Ok(out)
            }
            Err(_) => {
                self.vault_auth_failures = self.vault_auth_failures.saturating_add(1);
                let _ = sys_ipc_call_inline(
                    self.disp_sup_cap,
                    pack_msg_tag(IPC_OP_WM_SET_AUTH_LOCK, 0, 3, 0),
                    [1, self.vault_auth_failures as u64, 1, 0],
                    0,
                );
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-SUP] VAULT_UNLOCK auth=denied fails=");
                lb.push_u64(self.vault_auth_failures as u64);
                log_line(&lb);
                Err(IPC_OP_ERR_PERM)
            }
        }
    }

    pub fn execute_action(&mut self, act: DesktopAction) -> Result<[u64; 4], u32> {
        match act {
            DesktopAction::None => {
                self.inval_rejects = self.inval_rejects.wrapping_add(1);
                Err(IPC_OP_ERR_INVAL)
            }
            DesktopAction::LaunchOrFocusApp(app) => {
                let (pid, spawned) = self.launch_or_focus(app)?;
                Ok([
                    pid as u64,
                    spawned,
                    self.slots[app as usize].launches as u64,
                    0,
                ])
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
                        save_prefs_to_persist(self.vfs_sup_cap, &self.prefs);
                        return Ok([rmrs[0], rmrs[1], 0, 0]);
                    }
                }
                Ok([0; 4])
            }
            DesktopAction::SetLayoutMode(m) => {
                let out = apply_layout_action(self.disp_sup_cap, &mut self.prefs, m)?;
                save_prefs_to_persist(self.vfs_sup_cap, &self.prefs);
                Ok(out)
            }
            DesktopAction::SetKeymap(k) => {
                let out = apply_input_prefs(self.input_sup_cap, &mut self.prefs, Some(k), None)?;
                save_prefs_to_persist(self.vfs_sup_cap, &self.prefs);
                Ok(out)
            }
            DesktopAction::SetPointerSpeed(s) => {
                let out = apply_input_prefs(self.input_sup_cap, &mut self.prefs, None, Some(s))?;
                save_prefs_to_persist(self.vfs_sup_cap, &self.prefs);
                Ok(out)
            }
            DesktopAction::SetThemeContrast(flags) => {
                let out = apply_display_prefs(self.disp_sup_cap, &mut self.prefs, flags)?;
                save_prefs_to_persist(self.vfs_sup_cap, &self.prefs);
                Ok(out)
            }
            DesktopAction::SetBlankTimeoutSec(sec) => {
                let out = apply_blank_timeout(self.disp_sup_cap, self.tsc_mhz, &mut self.prefs, sec)?;
                save_prefs_to_persist(self.vfs_sup_cap, &self.prefs);
                Ok(out)
            }
            DesktopAction::BlankDisplayNow => {
                apply_session_visual_state(self.disp_sup_cap, SessionVisualState::DisplayBlanked)
            }
            DesktopAction::LockSession => {
                if self.vault_sup_cap != 0 {
                    if let Ok(new_epoch) = vault_lock_and_revoke(self.vault_sup_cap) {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-SUP] VAULT_LOCK revoked_to_epoch=");
                        lb.push_u64(new_epoch as u64);
                        log_line(&lb);
                    }
                    if self.disp_sup_cap != 0 {
                        let _ = sys_ipc_call_inline(
                            self.disp_sup_cap,
                            pack_msg_tag(IPC_OP_WM_SET_AUTH_LOCK, 0, 3, 0),
                            [1, self.vault_auth_failures as u64, 1, 0],
                            0,
                        );
                    }
                }
                apply_session_visual_state(self.disp_sup_cap, SessionVisualState::SessionLocked)
            }
            DesktopAction::UnlockSession => {
                if self.vault_sup_cap != 0 {
                    return Err(IPC_OP_ERR_PERM);
                }
                apply_session_visual_state(self.disp_sup_cap, SessionVisualState::Active)
            }
            DesktopAction::SubmitVaultUnlock(_) => self.handle_vault_unlock_submit(),
            DesktopAction::RequestShutdown => {
                self.final_power_mode = if self.session_mode == 13 || self.session_mode == 23 {
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

    pub fn drain_wm_actions(&mut self) {
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
