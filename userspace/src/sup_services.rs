// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Supervisor Core Service Spawners (`dry-vfs`, `dry-power`) & Live Runtime Preference Sync (`Milestone 2A-3`).

use crate::init_legacy::{domain_spec, log_line};
use crate::shell::LineBuilder;
use crate::vfs_proto::vfs_seed_boot_entry;
use crate::wm::SessionVisualState;
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_derive, sys_cap_revoke, sys_ep_create, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_proc_spawn, sys_proc_wait, sys_write_debug, BootModManifestEntry,
    RootInitBootInfo, BADGE_SUPERVISOR, BADGE_SUP_DISPLAY, BADGE_SUP_ROGUE, BOOT_MANIFEST_OFFSET,
    IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_INPUT_SET_PREFS,
    IPC_OP_PONG, IPC_OP_PWR_REBOOT, IPC_OP_PWR_SHUTDOWN_S5, IPC_OP_PWR_SUSPEND_S3,
    IPC_OP_WM_BIND_SUP_NOTIF, IPC_OP_WM_QUERY_STATE, IPC_OP_WM_SET_LAYOUT, IPC_OP_WM_SET_PREFS,
    IPC_OP_WM_SET_SESSION_STATE, RIGHT_CALL, RIGHT_GRANT, RIGHT_MANAGE, RIGHT_MAP, RIGHT_READ,
    RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_SIGNAL, RIGHT_WRITE,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;
const DISPLAY_COMP_ID: u16 = 0x0002;
pub const VFS_COMP_ID: u16 = 0x0004;
pub const POWER_COMP_ID: u16 = 0x0005;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalPowerMode {
    None,
    ExitHarness,
    HardwareShutdownS5,
    HardwareReboot,
}

/// Live runtime session preferences maintained by `dry-init` (volatile RAM; no fake disk persistence).
#[derive(Clone, Copy)]
pub struct SupervisorPrefs {
    pub layout_mode: u8,
    pub keymap: u8,
    pub pointer_speed: u8,
    /// Bit 0 = `high_contrast`, Bit 1 = `cursor_large`.
    pub theme_flags: u8,
    pub blank_timeout_sec: u16,
}

impl SupervisorPrefs {
    pub const fn new() -> Self {
        Self {
            layout_mode: 0,
            keymap: 0,
            pointer_speed: 1,
            theme_flags: 0,
            blank_timeout_sec: 0,
        }
    }

    #[inline]
    pub const fn pack_mr0(&self) -> u64 {
        (self.layout_mode as u64)
            | ((self.keymap as u64) << 8)
            | ((self.pointer_speed as u64) << 16)
            | ((self.theme_flags as u64) << 24)
            | ((self.blank_timeout_sec as u64) << 32)
    }

    #[inline]
    pub const fn unpack_mr0(mr0: u64) -> Self {
        Self {
            layout_mode: (mr0 & 0xff) as u8,
            keymap: ((mr0 >> 8) & 0xff) as u8,
            pointer_speed: ((mr0 >> 16) & 0xff) as u8,
            theme_flags: ((mr0 >> 24) & 0xff) as u8,
            blank_timeout_sec: ((mr0 >> 32) & 0xffff) as u16,
        }
    }
}

impl Default for SupervisorPrefs {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn `dry-display` (`0x0002`) in `p3_mode` and bind supervisor notification/wake channels.
/// Returns `(disp_handle, disp_ep, disp_sup_cap)`.
pub fn spawn_and_bind_p3_display(
    boot: &RootInitBootInfo,
    sup_ep: u64,
    sup_notif: u64,
) -> Result<(u64, u64, u64), i32> {
    if boot.fb_cap == 0 || boot.display_mod_cap == 0 {
        return Ok((0, 0, 0));
    }
    let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let disp_ep = sys_ep_create(ipc_spec).map_err(|_| 144)?;
    let disp_sup_cap = sys_cap_derive(disp_ep, cli_rights, 0, BADGE_SUPERVISOR).map_err(|_| 144)?;
    let fb_delegatable = sys_cap_derive(
        boot.fb_cap,
        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT,
        0,
        0,
    )
    .map_err(|_| 145)?;
    let (disp_handle, _) = sys_proc_spawn(
        boot.display_mod_cap,
        1,
        DISPLAY_COMP_ID,
        (disp_ep, RIGHT_READ | RIGHT_RECV | RIGHT_REPLY, 0xd152),
        (fb_delegatable, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP),
    )
    .map_err(|_| 146)?;

    let _ = sys_ipc_call_with_grant(
        disp_sup_cap,
        IPC_OP_WM_BIND_SUP_NOTIF,
        [1, 0x03, 0, 0],
        sup_notif,
        RIGHT_READ | RIGHT_SIGNAL,
        0,
        0,
    );
    let _ = sys_ipc_call_with_grant(
        disp_sup_cap,
        IPC_OP_WM_BIND_SUP_NOTIF,
        [1, 0x03, 1, 0],
        sup_ep,
        RIGHT_READ | RIGHT_SEND,
        BADGE_SUP_DISPLAY,
        0,
    );
    Ok((disp_handle, disp_ep, disp_sup_cap))
}

/// Spawn `dry-vfs` (`0x0004`) if `boot.vfs_mod_cap != 0` and seed `/boot` from Limine's boot manifest.
/// Returns `(vfs_handle, vfs_ep, vfs_sup_cap)`.
pub fn spawn_and_seed_vfs(
    boot_info_addr: u64,
    boot: &RootInitBootInfo,
) -> Result<(u64, u64, u64), i32> {
    if boot.vfs_mod_cap == 0 {
        return Ok((0, 0, 0));
    }
    let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

    let vfs_ep = sys_ep_create(ipc_spec).map_err(|_| 160)?;
    let vfs_sup_cap = sys_cap_derive(vfs_ep, cli_rights, 0, BADGE_SUPERVISOR).map_err(|_| 160)?;
    let (vfs_handle, vfs_pid) = sys_proc_spawn(
        boot.vfs_mod_cap,
        1,
        VFS_COMP_ID,
        (vfs_ep, srv_rights, 0x7f50),
        (0, 0),
    )
    .map_err(|_| 161)?;

    let mut seeded = 0u64;
    if boot_info_addr != 0 && boot.boot_manifest_count > 0 {
        let count = (boot.boot_manifest_count as usize).min(20);
        let base_ptr = (boot_info_addr + BOOT_MANIFEST_OFFSET) as *const BootModManifestEntry;
        for i in 0..count {
            // SAFETY: Kernel mapped `0x0070_0000..0x0070_1000` (`4 KiB`) `USER_RO` and wrote
            // `boot_manifest_count` (`<= 16`) `48 B` `BootModManifestEntry` structs at `+0x100`.
            let entry = unsafe { core::ptr::read_unaligned(base_ptr.add(i)) };
            let nlen = entry
                .name
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(entry.name.len());
            if nlen > 0 && vfs_seed_boot_entry(vfs_sup_cap, &entry.name[..nlen], entry.size_bytes).is_ok() {
                seeded += 1;
            }
        }
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-INIT] VFS service ready pid=");
    lb.push_u64(vfs_pid);
    lb.push_bytes(b" boot_mods=");
    lb.push_u64(seeded);
    log_line(&lb);

    Ok((vfs_handle, vfs_ep, vfs_sup_cap))
}

/// Spawn `dry-power` (`0x0005`) if `boot.power_mod_cap != 0` and delegate `CapObjectKind::PowerCtrl`.
/// Returns `(pwr_handle, pwr_ep, pwr_sup_cap)`.
pub fn spawn_and_bind_power_service(boot: &RootInitBootInfo) -> Result<(u64, u64, u64), i32> {
    if boot.power_mod_cap == 0 || boot.power_ctrl_cap == 0 {
        return Ok((0, 0, 0));
    }
    let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

    let pwr_ep = sys_ep_create(ipc_spec).map_err(|_| 191)?;
    let pwr_sup_cap = sys_cap_derive(pwr_ep, cli_rights, 0, BADGE_SUPERVISOR).map_err(|_| 191)?;
    let pwr_ctrl_deleg = sys_cap_derive(
        boot.power_ctrl_cap,
        RIGHT_READ | RIGHT_MANAGE | RIGHT_GRANT,
        0,
        0,
    )
    .map_err(|_| 192)?;
    let (pwr_handle, pwr_pid) = sys_proc_spawn(
        boot.power_mod_cap,
        1,
        POWER_COMP_ID,
        (pwr_ep, srv_rights, 0x5057),
        (pwr_ctrl_deleg, RIGHT_READ | RIGHT_MANAGE),
    )
    .map_err(|_| 193)?;

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-INIT] Power service ready pid=");
    lb.push_u64(pwr_pid);
    log_line(&lb);

    Ok((pwr_handle, pwr_ep, pwr_sup_cap))
}

pub fn probe_power_badge_gate(pwr_ep: u64) -> Result<(), i32> {
    if pwr_ep == 0 {
        return Ok(());
    }
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let rogue_pwr = sys_cap_derive(pwr_ep, cli_rights, 0, BADGE_SUP_ROGUE).map_err(|_| 194)?;
    let res = sys_ipc_call_inline(
        rogue_pwr,
        pack_msg_tag(IPC_OP_PWR_SHUTDOWN_S5, 0, 1, 0),
        [0; 4],
        0,
    );
    let _ = sys_cap_revoke(rogue_pwr, 1);
    match res {
        Ok((rtag, _, _)) if msg_tag_label(rtag) == IPC_OP_ERR_PERM => Ok(()),
        _ => Err(195),
    }
}

pub fn shutdown_vfs_service(vfs_handle: u64, vfs_ep: u64, vfs_sup_cap: u64) -> Result<(), i32> {
    if vfs_ep == 0 {
        return Ok(());
    }
    let _ = sys_ipc_call_inline(
        vfs_sup_cap,
        pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
        [0; 4],
        0,
    );
    if sys_proc_wait(vfs_handle, 0, 0) != Ok((0, 0)) {
        return Err(162);
    }
    let _ = sys_cap_revoke(vfs_sup_cap, 1);
    let _ = sys_cap_revoke(vfs_ep, 2);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn teardown_p3_core_services(
    input_sup_cap: u64,
    input_ep: u64,
    input_handle: u64,
    vfs_handle: u64,
    vfs_ep: u64,
    vfs_sup_cap: u64,
    disp_sup_cap: u64,
    disp_ep: u64,
    disp_handle: u64,
    pwr_handle: u64,
    pwr_ep: u64,
    pwr_sup_cap: u64,
    sup_notif: u64,
    sup_ep: u64,
    final_mode: FinalPowerMode,
) -> Result<(), i32> {
    let _ = sys_ipc_call_inline(
        input_ep,
        pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
        [0; 4],
        0,
    );
    if sys_proc_wait(input_handle, 0, 0) != Ok((0, 0)) {
        return Err(158);
    }
    if input_sup_cap != 0 {
        let _ = sys_cap_revoke(input_sup_cap, 1);
    }
    let _ = sys_cap_revoke(input_ep, 2);

    shutdown_vfs_service(vfs_handle, vfs_ep, vfs_sup_cap)?;

    if disp_ep != 0 {
        let _ = sys_ipc_call_inline(
            disp_sup_cap,
            pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
            [0; 4],
            0,
        );
        if sys_proc_wait(disp_handle, 0, 0) != Ok((0, 0)) {
            return Err(159);
        }
        let _ = sys_cap_revoke(disp_sup_cap, 1);
        let _ = sys_cap_revoke(disp_ep, 2);
    }

    if final_mode == FinalPowerMode::HardwareShutdownS5 && pwr_sup_cap != 0 {
        let _ = sys_write_debug(b"[DRY-INIT] Controlled teardown complete -> invoking dry-power S5\n");
        let _ = sys_ipc_call_inline(
            pwr_sup_cap,
            pack_msg_tag(IPC_OP_PWR_SHUTDOWN_S5, 0, 1, 0),
            [0; 4],
            0,
        );
    } else if final_mode == FinalPowerMode::HardwareReboot && pwr_sup_cap != 0 {
        let _ =
            sys_write_debug(b"[DRY-INIT] Controlled teardown complete -> invoking dry-power reset\n");
        let _ = sys_ipc_call_inline(
            pwr_sup_cap,
            pack_msg_tag(IPC_OP_PWR_REBOOT, 0, 1, 0),
            [0; 4],
            0,
        );
    }

    if pwr_ep != 0 {
        let _ = sys_ipc_call_inline(
            pwr_sup_cap,
            pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
            [0; 4],
            0,
        );
        if sys_proc_wait(pwr_handle, 0, 0) != Ok((0, 0)) {
            return Err(196);
        }
        let _ = sys_cap_revoke(pwr_sup_cap, 1);
        let _ = sys_cap_revoke(pwr_ep, 2);
    }

    let _ = sys_cap_revoke(sup_notif, 2);
    let _ = sys_cap_revoke(sup_ep, 2);
    Ok(())
}

pub fn sync_live_layout_pref(disp_sup_cap: u64, prefs: &mut SupervisorPrefs) {
    if disp_sup_cap == 0 {
        return;
    }
    if let Ok((rtag, _, qmrs)) = sys_ipc_call_inline(
        disp_sup_cap,
        pack_msg_tag(IPC_OP_WM_QUERY_STATE, 0, 1, 0),
        [0; 4],
        0,
    ) {
        if msg_tag_label(rtag) == IPC_OP_PONG {
            prefs.layout_mode = ((qmrs[0] >> 48) & 0xff) as u8;
        }
    }
}

pub fn apply_layout_action(
    disp_sup_cap: u64,
    prefs: &mut SupervisorPrefs,
    target_mode: u8,
) -> Result<[u64; 4], u32> {
    if target_mode > 3 {
        return Err(IPC_OP_ERR_INVAL);
    }
    if disp_sup_cap == 0 {
        prefs.layout_mode = target_mode;
        return Ok([target_mode as u64, 0, 0, 0]);
    }
    let (rtag, _, rmrs) = sys_ipc_call_inline(
        disp_sup_cap,
        pack_msg_tag(IPC_OP_WM_SET_LAYOUT, 0, 1, 0),
        [target_mode as u64, 0, 0, 0],
        0,
    )
    .map_err(|_| IPC_OP_ERR_INVAL)?;
    if msg_tag_label(rtag) != IPC_OP_PONG {
        return Err(msg_tag_label(rtag));
    }
    prefs.layout_mode = (rmrs[0] & 0xff) as u8;
    Ok([rmrs[0], rmrs[1], 0, 0])
}

pub fn apply_input_prefs(
    input_sup_cap: u64,
    prefs: &mut SupervisorPrefs,
    new_keymap: Option<u8>,
    new_speed: Option<u8>,
) -> Result<[u64; 4], u32> {
    let km = new_keymap.unwrap_or(prefs.keymap);
    let spd = new_speed.unwrap_or(prefs.pointer_speed);
    if km > 1 || !(1..=2).contains(&spd) {
        return Err(IPC_OP_ERR_INVAL);
    }
    if input_sup_cap != 0 {
        let (rtag, _, _) = sys_ipc_call_inline(
            input_sup_cap,
            pack_msg_tag(IPC_OP_INPUT_SET_PREFS, 0, 2, 0),
            [km as u64, spd as u64, 0, 0],
            0,
        )
        .map_err(|_| IPC_OP_ERR_INVAL)?;
        if msg_tag_label(rtag) != IPC_OP_PONG {
            return Err(msg_tag_label(rtag));
        }
    }
    prefs.keymap = km;
    prefs.pointer_speed = spd;
    Ok([km as u64, spd as u64, 0, 0])
}

pub fn apply_display_prefs(
    disp_sup_cap: u64,
    prefs: &mut SupervisorPrefs,
    flags: u8,
) -> Result<[u64; 4], u32> {
    if (flags & !0x3) != 0 {
        return Err(IPC_OP_ERR_INVAL);
    }
    let mut px = 0u64;
    let mut cyc = 0u64;
    if disp_sup_cap != 0 {
        let (rtag, _, rmrs) = sys_ipc_call_inline(
            disp_sup_cap,
            pack_msg_tag(IPC_OP_WM_SET_PREFS, 0, 1, 0),
            [flags as u64, 0, 0, 0],
            0,
        )
        .map_err(|_| IPC_OP_ERR_INVAL)?;
        if msg_tag_label(rtag) != IPC_OP_PONG {
            return Err(msg_tag_label(rtag));
        }
        px = rmrs[1];
        cyc = rmrs[2];
    }
    prefs.theme_flags = flags;
    Ok([flags as u64, px, cyc, 0])
}

pub fn apply_blank_timeout(
    disp_sup_cap: u64,
    tsc_mhz: u64,
    prefs: &mut SupervisorPrefs,
    sec: u16,
) -> Result<[u64; 4], u32> {
    if sec > 3600 {
        return Err(IPC_OP_ERR_INVAL);
    }
    let blank_tsc = (sec as u64)
        .saturating_mul(1_000_000)
        .saturating_mul(tsc_mhz.max(1000));
    if disp_sup_cap != 0 {
        let (rtag, _, _) = sys_ipc_call_inline(
            disp_sup_cap,
            pack_msg_tag(IPC_OP_WM_SET_SESSION_STATE, 0, 3, 0),
            [u64::MAX, blank_tsc, 1, 0],
            0,
        )
        .map_err(|_| IPC_OP_ERR_INVAL)?;
        if msg_tag_label(rtag) != IPC_OP_PONG {
            return Err(msg_tag_label(rtag));
        }
    }
    prefs.blank_timeout_sec = sec;
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-SUP] PREFS blank_timeout_s=");
    lb.push_u64(sec as u64);
    log_line(&lb);
    Ok([sec as u64, blank_tsc, 0, 0])
}

pub fn apply_session_visual_state(
    disp_sup_cap: u64,
    state: SessionVisualState,
) -> Result<[u64; 4], u32> {
    if disp_sup_cap == 0 {
        return Ok([state as u64, 0, 0, 0]);
    }
    let (rtag, _, rmrs) = sys_ipc_call_inline(
        disp_sup_cap,
        pack_msg_tag(IPC_OP_WM_SET_SESSION_STATE, 0, 3, 0),
        [state as u64, 0, 0, 0],
        0,
    )
    .map_err(|_| IPC_OP_ERR_INVAL)?;
    if msg_tag_label(rtag) != IPC_OP_PONG {
        return Err(msg_tag_label(rtag));
    }
    Ok(rmrs)
}

pub fn request_suspend_s3(pwr_sup_cap: u64) -> Result<[u64; 4], u32> {
    if pwr_sup_cap != 0 {
        if let Ok((rtag, _, _)) = sys_ipc_call_inline(
            pwr_sup_cap,
            pack_msg_tag(IPC_OP_PWR_SUSPEND_S3, 0, 1, 0),
            [0; 4],
            0,
        ) {
            return Err(msg_tag_label(rtag));
        }
    } else {
        let _ = sys_write_debug(
            b"[DRY-POWER] S3 suspend rejected: -ENOSYS (UEFI real-mode wake vector unimplemented)\n",
        );
    }
    Err(IPC_OP_ERR_NOSYS)
}
