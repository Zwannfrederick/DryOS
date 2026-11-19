// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Supervisor Security & Fault-Recovery Verification Suites (`Milestone 2A-3` `p3_sec` &
//! `Milestone 2B` `p4_sec` — `ADR-007` Rules 1–12, `INV-SEC-01..08`, `INV-CAP-08`, `R-03`).

use crate::init_legacy::{log_line, run_wm_security_audit};
use crate::shell::LineBuilder;
use crate::sup_services::{probe_power_badge_gate, spawn_blk_service, DEV_WS_COMP_ID};
use crate::supervisor::is_action_badge_authorized;
use crate::vault_proto::{
    vault_get_sealed_ro_status_cap, vault_lock_and_revoke, vault_mint_leased_cap, vault_unlock,
    VAULT_DEFAULT_PASSPHRASE,
};
use crate::vfs_proto::{vfs_link_file, vfs_mount_blk, vfs_read_file};
use crate::{
    pack_msg_tag, sys_cap_derive, sys_cap_query, sys_cap_revoke, sys_ipc_send_inline,
    sys_proc_spawn_ex, sys_proc_wait, sys_vault_query_epoch, sys_vspace_map, sys_write_debug,
    RootInitBootInfo, UserCapInfo, BADGE_SUP_FILES, BADGE_SUP_ROGUE, BADGE_SUP_SETTINGS,
    BADGE_SUP_SHELL, BADGE_VFS_DEV_WS, EPERM, EREVOKED, EXDEV, IPC_OP_CRASH_UD2, RIGHT_CALL,
    RIGHT_GRANT, RIGHT_READ, RIGHT_SEND, SEC_FLAG_DEV_WORKSPACE, VSPACE_MAP_OP_4K_RO,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const AUDIT_RO_VADDR: u64 = 0x1e00_0000;

pub fn run_p3_sup_security_checks(
    sup_ep: u64,
    disp_ep: u64,
    pwr_ep: u64,
    bad_word_ok: bool,
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
        || !bad_word_ok
    {
        return Err(186);
    }

    let _ = sys_write_debug(
        b"[DRY-P3-SEC] PASS: badge_gate_verified=true bad_action_rejected=true\n",
    );
    Ok(())
}

/// Live QEMU/KVM Milestone 2B security & fault-isolation audit (`desktop4=1 p4_sec=1`):
/// 1. Spawns a real Ring 3 `DEV_WORKSPACE` (`ADR-007` Rule 6) compartment (`0x002a`) and proves
///    hardware rejection of `SYS_DMA_ALLOC`, `SYS_DEV_MAP`, `SYS_VAULT_CTL(ADVANCE_EPOCH)`, and
///    `NO_DELEGATE` `/persist/vault.sec` reads, while allowing `READ_ONLY_EXPORT` `/persist/README.txt`.
/// 2. Mints an epoch-leased capability (`VAULT_OP_MINT_LEASE`), advances `GLOBAL_LEASE_EPOCH` via
///    `VAULT_OP_LOCK_REVOKE`, and verifies instant `-EREVOKED` (`-110`) rejection (`INV-SEC-07`),
///    plus `READ_ONLY_EXPORT` `W^X` write-mapping rejection (`INV-SEC-03`).
/// 3. Verifies `INV-SEC-05` cross-mount (`-EXDEV`) and cross-domain/private (`-EPERM`) link rejection.
/// 4. Injects `#UD` (`IPC_OP_CRASH_UD2`) into `dry-blk`, reaps vector `6` (triggering Ring 0 PCI
///    `Command.BME=0` quiesce & `iommu_pin_count` release), restarts `dry-blk`, remounts `/persist`,
///    and verifies data integrity!
#[allow(clippy::too_many_arguments)]
pub fn run_p4_security_and_fault_audit(
    boot: &RootInitBootInfo,
    vfs_ep: u64,
    vfs_sup_cap: u64,
    vault_sup_cap: u64,
    blk_handle: &mut u64,
    blk_ep: &mut u64,
    blk_sup_cap: &mut u64,
) -> Result<(), i32> {
    if boot.vault_mod_cap == 0 || vfs_ep == 0 || vault_sup_cap == 0 || *blk_ep == 0 {
        return Err(200);
    }
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;

    // 1. Scoped `DEV_WORKSPACE` (`SecFlags::DEV_WORKSPACE`, `0x002a`) hardware confinement check:
    //    First verify unleased (`lease_epoch == 0`) delegation into `DEV_WORKSPACE` is denied (`-EPERM`,
    //    `INV-SEC-07`), then delegate an epoch-leased capability (`lease_epoch == cur_epoch`).
    let unleased_rejected = sys_proc_spawn_ex(
        boot.vault_mod_cap,
        1,
        SEC_FLAG_DEV_WORKSPACE,
        DEV_WS_COMP_ID,
        (vfs_ep, cli_rights, BADGE_VFS_DEV_WS),
        (0, 0),
    ) == Err(EPERM);
    if !unleased_rejected {
        return Err(201);
    }
    let cur_epoch = sys_vault_query_epoch().unwrap_or(1);
    let leased_vfs_devws = sys_cap_derive(
        vfs_ep,
        cli_rights | RIGHT_GRANT,
        cur_epoch,
        BADGE_VFS_DEV_WS,
    )
    .map_err(|_| 201)?;
    let (devws_handle, _) = match sys_proc_spawn_ex(
        boot.vault_mod_cap,
        1,
        SEC_FLAG_DEV_WORKSPACE,
        DEV_WS_COMP_ID,
        (leased_vfs_devws, cli_rights, 0),
        (0, 0),
    ) {
        Ok(v) => v,
        Err(_) => {
            let _ = sys_cap_revoke(leased_vfs_devws, 1);
            return Err(201);
        }
    };
    let wait_devws = sys_proc_wait(devws_handle, 0, 0);
    let _ = sys_cap_revoke(leased_vfs_devws, 1);
    if wait_devws != Ok((0, 0)) {
        return Err(202);
    }

    // 2. `DRY Vault` epoch lease revocation (`INV-SEC-07`) & `READ_ONLY_EXPORT` (`INV-SEC-03`).
    let (leased_cap, _) = vault_mint_leased_cap(vault_sup_cap).map_err(|_| 203)?;
    let mut ci = UserCapInfo::default();
    let pre_ok = sys_cap_query(leased_cap, &mut ci) == 0;
    let _ = vault_lock_and_revoke(vault_sup_cap).map_err(|_| 204)?;
    let post_revoked = sys_cap_query(leased_cap, &mut ci) == EREVOKED;
    let _ = sys_cap_revoke(leased_cap, 1);
    let _ = vault_unlock(vault_sup_cap, VAULT_DEFAULT_PASSPHRASE).map_err(|_| 205)?;
    if !pre_ok || !post_revoked {
        return Err(206);
    }

    let ro_cap = vault_get_sealed_ro_status_cap(vault_sup_cap).map_err(|_| 207)?;
    let rw_denied = sys_vspace_map(ro_cap, AUDIT_RO_VADDR, VSPACE_MAP_OP_4K_RW) == Err(EPERM);
    let ro_ok = sys_vspace_map(ro_cap, AUDIT_RO_VADDR, VSPACE_MAP_OP_4K_RO).is_ok();
    let mut magic_ok = false;
    if ro_ok {
        // SAFETY: `AUDIT_RO_VADDR` is mapped `USER_RO` (`4 KiB`).
        let hdr = unsafe { core::slice::from_raw_parts(AUDIT_RO_VADDR as *const u8, 8) };
        magic_ok = hdr == b"DRYVLTRO";
        let _ = sys_vspace_map(ro_cap, AUDIT_RO_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    }
    let _ = sys_cap_revoke(ro_cap, 1);
    if !rw_denied || !ro_ok || !magic_ok {
        return Err(208);
    }

    // 3. `INV-SEC-05` cross-mount (`-EXDEV`) & cross-domain (`-EPERM`) link isolation.
    let vfs_user = sys_cap_derive(vfs_ep, cli_rights, 0, 0x0102).map_err(|_| 209)?;
    let xdev_ok =
        vfs_link_file(vfs_user, b"/boot/dry-init.elf", b"/persist/pwn_init") == Err(EXDEV);
    let eperm_link_ok =
        vfs_link_file(vfs_user, b"/persist/vault.sec", b"/persist/pwn_vault") == Err(EPERM);
    let mut dummy = [0u8; 32];
    let vault_sec_hidden =
        vfs_read_file(vfs_user, b"/persist/vault.sec", 0, &mut dummy) == Err(EPERM);
    let _ = sys_cap_revoke(vfs_user, 1);
    if !xdev_ok || !eperm_link_ok || !vault_sec_hidden {
        return Err(210);
    }

    // 4. `R-03` Block driver `#UD` crash -> Ring 0 PCI `BME=0` quiesce & DMA unpin -> Supervisor restart!
    let _ = sys_ipc_send_inline(*blk_sup_cap, pack_msg_tag(IPC_OP_CRASH_UD2, 0, 0, 0), [0; 4], 0);
    let (w_kind, w_detail) = sys_proc_wait(*blk_handle, 0, 0).map_err(|_| 211)?;
    if w_kind != 1 || (w_detail & 0xff) != 6 {
        return Err(212);
    }
    let _ = sys_cap_revoke(*blk_sup_cap, 1);
    let _ = sys_cap_revoke(*blk_ep, 2);

    let (nh, nep, nsup, sectors) = spawn_blk_service(boot)?;
    if sectors == 0 {
        return Err(213);
    }
    *blk_handle = nh;
    *blk_ep = nep;
    *blk_sup_cap = nsup;

    let (_, tx_seq, _) = vfs_mount_blk(vfs_sup_cap, nep).map_err(|_| 214)?;
    let mut check_buf = [0u8; 32];
    let (got, _) =
        vfs_read_file(vfs_sup_cap, b"/persist/README.txt", 0, &mut check_buf).map_err(|_| 215)?;
    if got == 0 || tx_seq == 0 {
        return Err(216);
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(
        b"[DRY-P4-SEC] PASS: devws_confined=true lease_revoked=true ro_export_wx=true link_isolated=true blk_crash_recovered=true tx_seq=",
    );
    lb.push_u64(tx_seq);
    log_line(&lb);
    Ok(())
}

/// Ascension 8.0 M2 Security Hardening Probe (`dry_test_sec_probe=1`):
/// verifies non-supervisor `#UD` rejection and unclaimed IPC capability slot reclamation on `dry-usb`.
#[inline(never)]
pub fn run_asc8_m2_security_probe(hw: &crate::sup_hw_services::HwServicesState) {
    if hw.usb_ep == 0 || hw.usb_sup_cap == 0 {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-SEC-M2] FAIL: usb_ep_missing=1");
        log_line(&lb);
        return;
    }

    let rogue_rights = crate::RIGHT_READ | crate::RIGHT_SEND | crate::RIGHT_CALL;
    let usb_unauth_ud2_eperm = if let Ok(rogue_usb) =
        crate::sys_cap_derive(hw.usb_ep, rogue_rights, 0, crate::BADGE_SUP_ROGUE)
    {
        let res = crate::sys_ipc_call_inline(
            rogue_usb,
            crate::pack_msg_tag(crate::IPC_OP_CRASH_UD2, 0, 1, 0),
            [0; 4],
            0,
        );
        let _ = crate::sys_cap_revoke(rogue_usb, 1);
        matches!(
            res,
            Ok((tag, _, mrs))
                if crate::msg_tag_label(tag) == crate::IPC_OP_ERR_PERM
                    && (mrs[0] as isize) == crate::EPERM
        )
    } else {
        false
    };

    let mut flood_ok = 0u32;
    for _ in 0..16 {
        let Ok(fcap) = crate::sys_frame_alloc(0) else {
            flood_ok = 0;
            break;
        };
        let call_res = crate::sys_ipc_call_with_grant(
            hw.usb_sup_cap,
            crate::abi_ext::BLK_OP_QUERY_INFO,
            [0; 4],
            fcap,
            crate::RIGHT_READ,
            0,
            0,
        );
        let _ = crate::sys_cap_revoke(fcap, 1);
        if matches!(call_res, Ok((tag, _, _)) if crate::msg_tag_label(tag) == crate::IPC_OP_PONG) {
            flood_ok += 1;
        }
    }
    let cap_flood_reclaimed = flood_ok == 16;

    let mut rx = [0u8; crate::MAX_IPC_EXTRA_BYTES];
    let usb_alive = matches!(
        crate::vfs_proto::sys_ipc_call_extra(
            hw.usb_sup_cap,
            crate::USB_OP_QUERY_SUMMARY,
            [0; 4],
            &[],
            &mut rx,
            0,
        ),
        Ok((tag, _, _)) if crate::msg_tag_label(tag) == crate::IPC_OP_PONG
    );

    let mut lb = LineBuilder::new();
    if usb_unauth_ud2_eperm && usb_alive && cap_flood_reclaimed {
        lb.push_bytes(b"[DRY-SEC-M2] PASS: usb_unauth_ud2_eperm=1 usb_alive=1 cap_flood_reclaimed=1");
    } else {
        lb.push_bytes(b"[DRY-SEC-M2] FAIL: usb_unauth_ud2_eperm=");
        lb.push_u64(u64::from(usb_unauth_ud2_eperm));
        lb.push_bytes(b" usb_alive=");
        lb.push_u64(u64::from(usb_alive));
        lb.push_bytes(b" cap_flood_reclaimed=");
        lb.push_u64(u64::from(cap_flood_reclaimed));
    }
    log_line(&lb);
}
