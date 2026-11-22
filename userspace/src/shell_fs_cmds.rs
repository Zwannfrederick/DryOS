// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Extracted `dry-shell` app lifecycle, WM/power, fault-injection, and storage/vault dispatchers
//! so `dry_shell.rs` stays focused on loop & terminal presentation.

use crate::action::{AppId, DesktopAction};
use crate::shell::{LineBuilder, ShellCommand, ShellEngine};
use crate::shell_ext::{exec_storage_vault_command, run_live_p3_sec_probes};
use crate::surface::{STYLE_OK, STYLE_WARN};
use crate::{
    msg_tag_label, pack_msg_tag, sys_ipc_call_inline, sys_write_debug, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_NOSYS, IPC_OP_INPUT_OVERFLOW_TEST, IPC_OP_PONG, IPC_OP_SUP_ACTION,
    IPC_OP_WM_TOGGLE_TILING,
};

#[derive(Default)]
pub struct ShellExtOutcome {
    pub post_present_action: Option<DesktopAction>,
    pub wants_self_exit: bool,
    pub trigger_crash_after_present: bool,
    pub wants_break: bool,
}

pub fn debug_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

pub fn call_sup_action(sup_ep_cap: u64, act: DesktopAction) -> Result<[u64; 4], u32> {
    let words = act.to_words();
    let (rtag, _, rmrs) = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
        [words[0], words[1], 0, 0],
        0,
    )
    .map_err(|_| IPC_OP_ERR_INVAL)?;
    let lbl = msg_tag_label(rtag);
    if lbl == IPC_OP_PONG {
        Ok(rmrs)
    } else {
        Err(lbl)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn try_exec_ext_cmd(
    cmd: &ShellCommand<'_>,
    sup_ep_cap: u64,
    disp_ep_cap: u64,
    input_ep_cap: u64,
    session_mode: u64,
    cached_vfs_cap: &mut u64,
    cached_vault_cap: &mut u64,
    engine: &mut ShellEngine,
) -> Option<ShellExtOutcome> {
    let mut out = ShellExtOutcome::default();
    match cmd {
        ShellCommand::OpenApp(arg) => {
            let mut app_name: &[u8] = arg;
            if let Some(raw_target) = arg.strip_prefix(b"editor ") {
                let mut tstart = 0usize;
                while tstart < raw_target.len() && raw_target[tstart] == b' ' {
                    tstart += 1;
                }
                let mut tend = raw_target.len();
                while tend > tstart && raw_target[tend - 1] == b' ' {
                    tend -= 1;
                }
                let raw_path = &raw_target[tstart..tend];
                if !raw_path.is_empty() {
                    if let Some(vcap) = crate::shell_ext::ensure_vfs_cap(sup_ep_cap, cached_vfs_cap) {
                        let mut norm = [0u8; 96];
                        if let Some(nlen) =
                            crate::shell_vfs_ops::resolve_path(engine.cwd_bytes(), raw_path, &mut norm)
                        {
                            match crate::vfs_proto::vfs_create_file(
                                vcap,
                                b"/ram/.editor_target",
                                0,
                            ) {
                                Ok(_) | Err(crate::EEXIST) => {
                                    let _ = crate::vfs_proto::vfs_write_small(
                                        vcap,
                                        b"/ram/.editor_target",
                                        &norm[..nlen],
                                        false,
                                    );
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    app_name = b"editor";
                }
            } else if arg == b"editor" {
                if let Some(vcap) = crate::shell_ext::ensure_vfs_cap(sup_ep_cap, cached_vfs_cap) {
                    let _ = crate::vfs_proto::vfs_unlink_file(vcap, b"/ram/.editor_target");
                }
            }
            match AppId::from_name(app_name) {
            Some(app) => match call_sup_action(sup_ep_cap, DesktopAction::LaunchOrFocusApp(app)) {
                Ok(rmrs) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(if rmrs[1] != 0 {
                        b"Launched app '"
                    } else {
                        b"Focused running app '"
                    });
                    lb.push_bytes(app.slug());
                    lb.push_bytes(b"' (pid=");
                    lb.push_u64(rmrs[0]);
                    lb.push_byte(b')');
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=open app=");
                    slb.push_bytes(app.slug());
                    slb.push_bytes(b" pid=");
                    slb.push_u64(rmrs[0]);
                    slb.push_bytes(b" spawned=");
                    slb.push_u64(rmrs[1]);
                    debug_line(&slb);
                }
                Err(IPC_OP_ERR_NOSYS) => {
                    engine.push_line(
                        b"App module not loaded in current boot image (-ENOSYS).",
                        STYLE_WARN,
                    );
                }
                Err(_) => {
                    engine.push_line(b"Failed to launch/focus app.", STYLE_WARN);
                }
            },
            None => engine.push_line(
                b"Usage: open <terminal|lifestation|files|settings|editor|calc>",
                STYLE_WARN,
            ),
            }
        }
        ShellCommand::CloseApp(arg) => match AppId::from_name(arg) {
            Some(app) => {
                if app == AppId::Terminal {
                    out.wants_self_exit = true;
                } else {
                    match call_sup_action(sup_ep_cap, DesktopAction::CloseApp(app)) {
                        Ok(rmrs) => {
                            let mut lb = LineBuilder::new();
                            lb.push_bytes(b"Closed app '");
                            lb.push_bytes(app.slug());
                            lb.push_bytes(b"' (reaped_pid=");
                            lb.push_u64(rmrs[0]);
                            lb.push_byte(b')');
                            engine.push_line(lb.as_bytes(), STYLE_OK);

                            let mut slb = LineBuilder::new();
                            slb.push_bytes(b"[DRY-SHELL] CMD=close app=");
                            slb.push_bytes(app.slug());
                            slb.push_bytes(b" pid=");
                            slb.push_u64(rmrs[0]);
                            debug_line(&slb);
                        }
                        Err(_) => {
                            engine.push_line(b"Failed to close app.", STYLE_WARN);
                        }
                    }
                }
            }
            None => engine.push_line(
                b"Usage: close <terminal|lifestation|files|settings|editor|calc>",
                STYLE_WARN,
            ),
        },
        ShellCommand::RestartApp(arg) => match AppId::from_name(arg) {
            Some(app) => match call_sup_action(sup_ep_cap, DesktopAction::RestartApp(app)) {
                Ok(rmrs) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Restarted app '");
                    lb.push_bytes(app.slug());
                    lb.push_bytes(b"' -> pid=");
                    lb.push_u64(rmrs[0]);
                    lb.push_bytes(b" launches=");
                    lb.push_u64(rmrs[1]);
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=restart app=");
                    slb.push_bytes(app.slug());
                    slb.push_bytes(b" pid=");
                    slb.push_u64(rmrs[0]);
                    debug_line(&slb);
                }
                Err(_) => engine.push_line(b"Failed to restart app.", STYLE_WARN),
            },
            None => engine.push_line(
                b"Usage: restart <terminal|lifestation|files|settings|editor|calc>",
                STYLE_WARN,
            ),
        },
        ShellCommand::Tile => {
            if disp_ep_cap != 0 {
                if let Ok((_, _, rmrs)) = sys_ipc_call_inline(
                    disp_ep_cap,
                    pack_msg_tag(IPC_OP_WM_TOGGLE_TILING, 0, 1, 0),
                    [0; 4],
                    0,
                ) {
                    engine.push_line(
                        if rmrs[0] != 0 {
                            b"Window layout -> TILED"
                        } else {
                            b"Window layout -> FLOATING"
                        },
                        STYLE_OK,
                    );
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=tile mode=");
                    slb.push_u64(rmrs[0]);
                    debug_line(&slb);
                }
            }
        }
        ShellCommand::Blank => {
            engine.push_line(b"Display blank requested (any input wakes).", STYLE_OK);
            out.post_present_action = Some(DesktopAction::BlankDisplayNow);
            let _ = sys_write_debug(b"[DRY-SHELL] CMD=blank ok=true\n");
        }
        ShellCommand::Lock => {
            engine.push_line(
                if session_mode >= 20 {
                    b"DRY Vault authenticated session lock engaged (leases revoked)."
                } else {
                    b"Session lock engaged (unauthenticated until DRY Vault auth)."
                },
                STYLE_OK,
            );
            out.post_present_action = Some(DesktopAction::LockSession);
            let _ = sys_write_debug(b"[DRY-SHELL] CMD=lock ok=true\n");
        }
        ShellCommand::Reboot => {
            engine.push_line(b"Requesting controlled system reboot...", STYLE_WARN);
            let _ = sys_write_debug(b"[DRY-SHELL] CMD=reboot ok=true\n");
            let _ = call_sup_action(sup_ep_cap, DesktopAction::RequestReboot);
            out.wants_break = true;
        }
        ShellCommand::Poweroff => {
            engine.push_line(
                b"Requesting controlled ACPI S5 hardware poweroff...",
                STYLE_WARN,
            );
            let _ = sys_write_debug(b"[DRY-SHELL] CMD=poweroff ok=true\n");
            let _ = call_sup_action(sup_ep_cap, DesktopAction::RequestPoweroffHw);
            out.wants_break = true;
        }
        ShellCommand::Suspend => match call_sup_action(sup_ep_cap, DesktopAction::RequestSuspendS3) {
            Err(IPC_OP_ERR_NOSYS) => {
                engine.push_line(
                    b"Suspend-to-RAM (S3) unsupported: -ENOSYS (UEFI wake vector planned).",
                    STYLE_WARN,
                );
                let _ = sys_write_debug(b"[DRY-SHELL] CMD=suspend res=ENOSYS\n");
            }
            _ => engine.push_line(b"Suspend requested.", STYLE_OK),
        },
        ShellCommand::OverflowTest => {
            if let Ok((_, _, omrs)) = sys_ipc_call_inline(
                input_ep_cap,
                pack_msg_tag(IPC_OP_INPUT_OVERFLOW_TEST, 0, 1, 0),
                [0; 4],
                0,
            ) {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"Input queue guard: retained=");
                lb.push_u64(omrs[0]);
                lb.push_bytes(b" dropped=");
                lb.push_u64(omrs[1]);
                lb.push_bytes(b" total_ovf=");
                lb.push_u64(omrs[2]);
                engine.push_line(lb.as_bytes(), STYLE_OK);

                let mut slb = LineBuilder::new();
                slb.push_bytes(b"[DRY-SHELL] CMD=overflow-test retained=");
                slb.push_u64(omrs[0]);
                slb.push_bytes(b" dropped=");
                slb.push_u64(omrs[1]);
                debug_line(&slb);
            }
        }
        ShellCommand::CrashInput => {
            engine.push_line(
                b"Injecting #UD fault into dry-input service...",
                STYLE_WARN,
            );
            out.trigger_crash_after_present = true;
        }
        ShellCommand::SecAudit => {
            run_live_p3_sec_probes(sup_ep_cap, engine);
        }
        ShellCommand::GpuFault => match call_sup_action(sup_ep_cap, DesktopAction::TriggerGpuFaultTest) {
            Ok(rmrs) => {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"GPU fault & recovery OK: gen=");
                lb.push_u64(rmrs[0]);
                lb.push_bytes(b" vec=");
                lb.push_u64(rmrs[1]);
                lb.push_byte(b' ');
                lb.push_u64(rmrs[2]);
                lb.push_byte(b'x');
                lb.push_u64(rmrs[3]);
                engine.push_line(lb.as_bytes(), STYLE_OK);

                let mut slb = LineBuilder::new();
                slb.push_bytes(b"[DRY-SHELL] CMD=gpufault gen=");
                slb.push_u64(rmrs[0]);
                slb.push_bytes(b" vec=");
                slb.push_u64(rmrs[1]);
                debug_line(&slb);
            }
            Err(_) => {
                engine.push_line(
                    b"GPU driver island unavailable or recovery failed.",
                    STYLE_WARN,
                );
            }
        },
        ShellCommand::StorageVault(verb, rest) => {
            if let Some(act) = exec_storage_vault_command(
                verb,
                rest,
                sup_ep_cap,
                cached_vfs_cap,
                cached_vault_cap,
                engine,
            ) {
                out.post_present_action = Some(act);
            }
        }
        _ => return None,
    }
    Some(out)
}
