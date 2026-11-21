// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Native Terminal Storage (`/persist`, `/ram`, `/boot`) & `DRY Vault` CLI Commands + Live
//! Security Probes (`Milestone 2A-3` & `Milestone 2B`).

use crate::action::{AppId, DesktopAction};
use crate::shell::{LineBuilder, ShellEngine};
use crate::shell_sys_cmds::exec_sys_diag_command;
use crate::shell_vfs_ops::{resolve_path, try_exec_vfs_tree_cmd};
use crate::surface::{STYLE_ACCENT, STYLE_DEFAULT, STYLE_MUTED, STYLE_OK, STYLE_WARN};
use crate::vault_proto::{
    vault_change_passphrase, vault_get_sealed_ro_status_cap, vault_lock_and_revoke,
    vault_mint_leased_cap, vault_query_status, vault_unlock,
};
use crate::vfs_proto::{
    vfs_create_file, vfs_link_file, vfs_list_dir, vfs_read_file, vfs_sync, vfs_unlink_file,
    vfs_write_small, VfsDirEntry, VFS_FLAG_DIR, VFS_FLAG_EXECUTABLE, VFS_FLAG_PERSISTENT,
    VFS_FLAG_PRIVATE, VFS_FLAG_READONLY,
};
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_query, sys_cap_revoke, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_power_ctl, sys_proc_kill, sys_vspace_map, sys_write_debug,
    UserCapInfo, EEXIST, EPERM, EREVOKED, EROFS, EXDEV, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM,
    IPC_OP_PONG, IPC_OP_SUP_ACTION, IPC_OP_SUP_CORRUPT_SB0_TEST, IPC_OP_SUP_GET_VAULT_CAP,
    IPC_OP_SUP_GET_VFS_CAP, IPC_OP_SUP_MINT_ROGUE_TEST, POWER_OP_REBOOT, POWER_OP_SHUTDOWN_S5,
    VSPACE_MAP_OP_4K_RO, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const SHELL_TEST_RO_VADDR: u64 = 0x1810_0000;

fn emit_dbg(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn split_first_token(raw: &[u8]) -> (&[u8], &[u8]) {
    let mut s = 0usize;
    while s < raw.len() && raw[s] == b' ' {
        s += 1;
    }
    let mut e = s;
    while e < raw.len() && raw[e] != b' ' {
        e += 1;
    }
    let tok = &raw[s..e];
    let mut rs = e;
    while rs < raw.len() && raw[rs] == b' ' {
        rs += 1;
    }
    (tok, &raw[rs..])
}

pub fn ensure_vfs_cap(sup_ep_cap: u64, cached_vfs_cap: &mut u64) -> Option<u64> {
    if *cached_vfs_cap != 0 {
        return Some(*cached_vfs_cap);
    }
    if let Ok((tag, cap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_GET_VFS_CAP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(tag) == IPC_OP_PONG && cap != 0 {
            *cached_vfs_cap = cap;
            return Some(cap);
        }
    }
    None
}

pub fn ensure_vault_cap(sup_ep_cap: u64, cached_vault_cap: &mut u64) -> Option<u64> {
    if *cached_vault_cap != 0 {
        return Some(*cached_vault_cap);
    }
    if let Ok((tag, cap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_GET_VAULT_CAP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(tag) == IPC_OP_PONG && cap != 0 {
            *cached_vault_cap = cap;
            return Some(cap);
        }
    }
    None
}

pub fn exec_storage_vault_command(
    verb: &[u8],
    rest: &[u8],
    sup_ep_cap: u64,
    vfs_cap_slot: &mut u64,
    vault_cap_slot: &mut u64,
    engine: &mut ShellEngine,
) -> Option<DesktopAction> {
    if try_exec_vfs_tree_cmd(verb, rest, sup_ep_cap, vfs_cap_slot, engine) {
        return None;
    }
    match verb {
        b"ls" => {
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"ls: VFS service unavailable", STYLE_WARN);
                return None;
            };
            let mut norm = [0u8; 96];
            let nlen = if rest.is_empty() {
                let src = if engine.cwd_bytes() == b"/" {
                    b"/persist"
                } else {
                    engine.cwd_bytes()
                };
                let n = src.len().min(norm.len());
                norm[..n].copy_from_slice(&src[..n]);
                n
            } else {
                let Some(n) = resolve_path(engine.cwd_bytes(), rest, &mut norm) else {
                    engine.push_line(b"ls: invalid path", STYLE_WARN);
                    return None;
                };
                n
            };
            let path = &norm[..nlen];
            let mut entries = [VfsDirEntry::EMPTY; 24];
            match vfs_list_dir(vcap, path, &mut entries) {
                Ok((count, mflags)) => {
                    let mut hdr = LineBuilder::new();
                    hdr.push_bytes(b"DIR ");
                    hdr.push_bytes(path);
                    hdr.push_bytes(if (mflags & VFS_FLAG_PERSISTENT) != 0 {
                        b" [DRYFS_v1 PERSISTENT CoW]"
                    } else if (mflags & VFS_FLAG_EXECUTABLE) != 0 {
                        b" [BIN RO]"
                    } else if (mflags & VFS_FLAG_READONLY) != 0 {
                        b" [BOOT RO]"
                    } else {
                        b" [RAM VOLATILE]"
                    });
                    hdr.push_bytes(b" entries=");
                    hdr.push_u64(count as u64);
                    engine.push_line(hdr.as_bytes(), STYLE_ACCENT);
                    for e in entries.iter().take(count) {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"  ");
                        lb.push_padded(e.name_bytes(), 24);
                        lb.push_u64(e.size_bytes as u64);
                        lb.push_bytes(b" B");
                        if (e.flags & VFS_FLAG_DIR) != 0 {
                            lb.push_bytes(b" [DIR]");
                        }
                        if (e.flags & VFS_FLAG_PRIVATE) != 0 {
                            lb.push_bytes(b" [PRIV]");
                        } else if (e.flags & VFS_FLAG_READONLY) != 0 {
                            lb.push_bytes(b" [RO]");
                        }
                        if (e.flags & VFS_FLAG_EXECUTABLE) != 0 {
                            lb.push_bytes(b" [EXEC]");
                        }
                        engine.push_line(lb.as_bytes(), STYLE_DEFAULT);
                    }
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=ls path=");
                    slb.push_bytes(path);
                    slb.push_bytes(b" count=");
                    slb.push_u64(count as u64);
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"ls error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            None
        }
        b"cat" => {
            if rest.is_empty() {
                engine.push_line(b"Usage: cat </persist/file|/ram/file|/boot/file>", STYLE_WARN);
                return None;
            }
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"cat: VFS unavailable", STYLE_WARN);
                return None;
            };
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), rest, &mut norm) else {
                engine.push_line(b"cat: invalid path", STYLE_WARN);
                return None;
            };
            let path = &norm[..nlen];
            let mut buf = [0u8; 2048];
            match vfs_read_file(vcap, path, 0, &mut buf) {
                Ok((got, total)) => {
                    let slice = &buf[..got];
                    let mut start = 0usize;
                    for i in 0..=slice.len() {
                        if i == slice.len() || slice[i] == b'\n' {
                            let mut seg = &slice[start..i];
                            if seg.last() == Some(&b'\r') {
                                seg = &seg[..seg.len() - 1];
                            }
                            if !(i == slice.len() && seg.is_empty() && start > 0) {
                                engine.push_line(seg, STYLE_DEFAULT);
                            }
                            start = i + 1;
                        }
                    }
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=cat path=");
                    slb.push_bytes(path);
                    slb.push_bytes(b" bytes=");
                    slb.push_u64(total as u64);
                    slb.push_bytes(b" text=\"");
                    let preview_n = got.min(64);
                    for &ch in &slice[..preview_n] {
                        slb.push_byte(if (0x20..=0x7e).contains(&ch) { ch } else { b'.' });
                    }
                    slb.push_byte(b'"');
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"cat error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=cat path=");
                    slb.push_bytes(path);
                    slb.push_bytes(b" err=");
                    slb.push_i64(code as i64);
                    emit_dbg(&slb);
                }
            }
            None
        }
        b"write" => {
            let (raw_path, payload) = split_first_token(rest);
            if raw_path.is_empty() || payload.is_empty() {
                engine.push_line(b"Usage: write </persist/name|/ram/name> <text>", STYLE_WARN);
                let mut slb = LineBuilder::new();
                slb.push_bytes(b"[DRY-SHELL] CMD=write usage_err rest=\"");
                slb.push_bytes(rest);
                slb.push_byte(b'"');
                emit_dbg(&slb);
                return None;
            }
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"write: VFS unavailable", STYLE_WARN);
                let _ = sys_write_debug(b"[DRY-SHELL] CMD=write err=no_vfs_cap\n");
                return None;
            };
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), raw_path, &mut norm) else {
                engine.push_line(b"write: invalid path", STYLE_WARN);
                return None;
            };
            let path = &norm[..nlen];
            match vfs_create_file(vcap, path, 0) {
                Ok(_) | Err(EEXIST) => {}
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"write create failed: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=write create_err=");
                    slb.push_i64(code as i64);
                    slb.push_bytes(b" path=\"");
                    slb.push_bytes(path);
                    slb.push_byte(b'"');
                    emit_dbg(&slb);
                    return None;
                }
            }
            match vfs_write_small(vcap, path, payload, false) {
                Ok(written) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Committed ");
                    lb.push_u64(written as u64);
                    lb.push_bytes(b" B -> ");
                    lb.push_bytes(path);
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=write path=");
                    slb.push_bytes(path);
                    slb.push_bytes(b" bytes=");
                    slb.push_u64(written as u64);
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"write failed: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=write write_err=");
                    slb.push_i64(code as i64);
                    slb.push_bytes(b" path=\"");
                    slb.push_bytes(path);
                    slb.push_byte(b'"');
                    emit_dbg(&slb);
                }
            }
            None
        }
        b"rm" => {
            if rest.is_empty() {
                engine.push_line(b"Usage: rm <path>", STYLE_WARN);
                return None;
            }
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"rm: VFS unavailable", STYLE_WARN);
                return None;
            };
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), rest, &mut norm) else {
                engine.push_line(b"rm: invalid path", STYLE_WARN);
                return None;
            };
            let path = &norm[..nlen];
            match vfs_unlink_file(vcap, path) {
                Ok(()) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Unlinked ");
                    lb.push_bytes(path);
                    engine.push_line(lb.as_bytes(), STYLE_OK);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=rm path=");
                    slb.push_bytes(path);
                    slb.push_bytes(b" ok=true");
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"rm error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            None
        }
        b"ln" => {
            let (raw_src, raw_dst) = split_first_token(rest);
            if raw_src.is_empty() || raw_dst.is_empty() {
                engine.push_line(b"Usage: ln <src_path> <dst_path>", STYLE_WARN);
                return None;
            }
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"ln: VFS unavailable", STYLE_WARN);
                return None;
            };
            let (mut sbuf, mut dbuf) = ([0u8; 96], [0u8; 96]);
            let (Some(slen), Some(dlen)) = (
                resolve_path(engine.cwd_bytes(), raw_src, &mut sbuf),
                resolve_path(engine.cwd_bytes(), raw_dst, &mut dbuf),
            ) else {
                engine.push_line(b"ln: invalid path", STYLE_WARN);
                return None;
            };
            let (src, dst) = (&sbuf[..slen], &dbuf[..dlen]);
            match vfs_link_file(vcap, src, dst) {
                Ok(ino) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Linked ino=");
                    lb.push_u64(ino as u64);
                    lb.push_bytes(b" -> ");
                    lb.push_bytes(dst);
                    engine.push_line(lb.as_bytes(), STYLE_OK);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=ln ok=true ino=");
                    slb.push_u64(ino as u64);
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"ln rejected (");
                    if code == EXDEV {
                        lb.push_bytes(b"-EXDEV cross-mount");
                    } else if code == EPERM {
                        lb.push_bytes(b"-EPERM INV-SEC-05");
                    } else {
                        lb.push_i64(code as i64);
                    }
                    lb.push_byte(b')');
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=ln err=");
                    slb.push_i64(code as i64);
                    emit_dbg(&slb);
                }
            }
            None
        }
        b"sync" => {
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"sync: VFS unavailable", STYLE_WARN);
                return None;
            };
            match vfs_sync(vcap) {
                Ok(tx_seq) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"VIRTIO_BLK_T_FLUSH barrier complete (tx_seq=");
                    lb.push_u64(tx_seq);
                    lb.push_byte(b')');
                    engine.push_line(lb.as_bytes(), STYLE_OK);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=sync tx_seq=");
                    slb.push_u64(tx_seq);
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"sync error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            None
        }
        b"vault-status" => {
            let Some(vlt) = ensure_vault_cap(sup_ep_cap, vault_cap_slot) else {
                engine.push_line(b"DRY Vault service not running in this profile.", STYLE_MUTED);
                return None;
            };
            match vault_query_status(vlt) {
                Ok(st) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"DRY Vault: state=");
                    lb.push_bytes(if st.unlocked { b"UNLOCKED" } else { b"SEALED" });
                    lb.push_bytes(b" epoch=");
                    lb.push_u64(st.lease_epoch as u64);
                    lb.push_bytes(b" persist=");
                    lb.push_bytes(if st.persistent_backed { b"ON" } else { b"RAM" });
                    lb.push_bytes(b" fails=");
                    lb.push_u64(st.auth_failures as u64);
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=vault-status unlocked=");
                    slb.push_bytes(if st.unlocked { b"true" } else { b"false" });
                    slb.push_bytes(b" epoch=");
                    slb.push_u64(st.lease_epoch as u64);
                    slb.push_bytes(b" fails=");
                    slb.push_u64(st.auth_failures as u64);
                    emit_dbg(&slb);
                }
                Err(_) => engine.push_line(b"vault-status failed", STYLE_WARN),
            }
            None
        }
        b"vault-unlock" => {
            let Some(vlt) = ensure_vault_cap(sup_ep_cap, vault_cap_slot) else {
                engine.push_line(b"DRY Vault unavailable.", STYLE_WARN);
                return None;
            };
            match vault_unlock(vlt, rest) {
                Ok(epoch) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"DRY Vault unlocked (epoch=");
                    lb.push_u64(epoch as u64);
                    lb.push_byte(b')');
                    engine.push_line(lb.as_bytes(), STYLE_OK);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=vault-unlock auth=ok epoch=");
                    slb.push_u64(epoch as u64);
                    emit_dbg(&slb);
                }
                Err(_) => {
                    engine.push_line(b"DRY Vault authentication denied.", STYLE_WARN);
                    let _ = sys_write_debug(b"[DRY-SHELL] CMD=vault-unlock auth=denied\n");
                }
            }
            None
        }
        b"vault-lock" => {
            engine.push_line(
                b"Engaging DRY Vault authenticated lock & revoking leased caps...",
                STYLE_OK,
            );
            let _ = sys_write_debug(b"[DRY-SHELL] CMD=vault-lock ok=true\n");
            Some(DesktopAction::LockSession)
        }
        b"vault-passwd" => {
            let (old_p, new_p) = split_first_token(rest);
            if old_p.is_empty() || new_p.is_empty() {
                engine.push_line(b"Usage: vault-passwd <old_pass> <new_pass>", STYLE_WARN);
                return None;
            }
            let Some(vlt) = ensure_vault_cap(sup_ep_cap, vault_cap_slot) else {
                engine.push_line(b"DRY Vault unavailable.", STYLE_WARN);
                return None;
            };
            match vault_change_passphrase(vlt, old_p, new_p) {
                Ok(gen) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Vault PBKDF2-SHA256 verifier rotated & flushed (gen=");
                    lb.push_u64(gen as u64);
                    lb.push_byte(b')');
                    engine.push_line(lb.as_bytes(), STYLE_OK);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=vault-passwd ok=true gen=");
                    slb.push_u64(gen as u64);
                    emit_dbg(&slb);
                }
                Err(_) => {
                    engine.push_line(b"vault-passwd denied (wrong old passphrase).", STYLE_WARN);
                    let _ = sys_write_debug(b"[DRY-SHELL] CMD=vault-passwd ok=false\n");
                }
            }
            None
        }
        b"vault-test" => {
            let Some(vlt) = ensure_vault_cap(sup_ep_cap, vault_cap_slot) else {
                engine.push_line(b"DRY Vault unavailable.", STYLE_WARN);
                return None;
            };
            let mut lease_ok = false;
            if let Ok((leased_cap, _)) = vault_mint_leased_cap(vlt) {
                let mut ci = UserCapInfo::default();
                let pre = sys_cap_query(leased_cap, &mut ci) == 0;
                let _ = vault_lock_and_revoke(vlt);
                let post = sys_cap_query(leased_cap, &mut ci) == EREVOKED;
                let _ = sys_cap_revoke(leased_cap, 1);
                lease_ok = pre && post;
            }
            let mut ro_ok = false;
            if let Ok(ro_cap) = vault_get_sealed_ro_status_cap(vlt) {
                let rw_denied =
                    sys_vspace_map(ro_cap, SHELL_TEST_RO_VADDR, VSPACE_MAP_OP_4K_RW) == Err(EPERM);
                let ro_mapped =
                    sys_vspace_map(ro_cap, SHELL_TEST_RO_VADDR, VSPACE_MAP_OP_4K_RO).is_ok();
                if ro_mapped {
                    let _ = sys_vspace_map(ro_cap, SHELL_TEST_RO_VADDR, VSPACE_MAP_OP_UNMAP_4K);
                }
                let _ = sys_cap_revoke(ro_cap, 1);
                ro_ok = rw_denied && ro_mapped;
            }
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"Vault proof: lease_revoked=");
            lb.push_bytes(if lease_ok { b"true" } else { b"false" });
            lb.push_bytes(b" ro_export_wx=");
            lb.push_bytes(if ro_ok { b"true" } else { b"false" });
            engine.push_line(
                lb.as_bytes(),
                if lease_ok && ro_ok { STYLE_OK } else { STYLE_WARN },
            );
            let mut slb = LineBuilder::new();
            slb.push_bytes(b"[DRY-SHELL] CMD=vault-test lease_revoked=");
            slb.push_bytes(if lease_ok { b"true" } else { b"false" });
            slb.push_bytes(b" ro_export_wx=");
            slb.push_bytes(if ro_ok { b"true" } else { b"false" });
            emit_dbg(&slb);
            None
        }
        b"corrupt-sb0-test" => {
            match sys_ipc_call_inline(
                sup_ep_cap,
                pack_msg_tag(IPC_OP_SUP_CORRUPT_SB0_TEST, 0, 0, 0),
                [0; 4],
                0,
            ) {
                Ok((rtag, _, _)) if msg_tag_label(rtag) == IPC_OP_PONG => {
                    engine.push_line(
                        b"Injected SB0 CRC corruption on /persist (Slot 1 intact).",
                        STYLE_WARN,
                    );
                    let _ = sys_write_debug(b"[DRY-SHELL] CMD=corrupt-sb0-test ok=true\n");
                }
                _ => engine.push_line(b"corrupt-sb0-test denied/unavailable.", STYLE_WARN),
            }
            None
        }
        _ => {
            static CACHED_NET_CAP: core::sync::atomic::AtomicU64 =
                core::sync::atomic::AtomicU64::new(0);
            let mut net_cap = CACHED_NET_CAP.load(core::sync::atomic::Ordering::Relaxed);
            if !crate::shell_net_cmds::try_exec_net_cmd(
                verb,
                rest,
                sup_ep_cap,
                &mut net_cap,
                engine,
            ) {
                let _ = exec_sys_diag_command(sup_ep_cap, verb, rest, engine);
            } else {
                CACHED_NET_CAP.store(net_cap, core::sync::atomic::Ordering::Relaxed);
            }
            None
        }
    }
}

pub fn run_live_p3_sec_probes(sup_ep_cap: u64, engine: &mut ShellEngine) {
    let kill_eperm = sys_proc_kill(1) == Err(EPERM);

    let mut unauth_perm = false;
    if let Ok((rtag, rogue_cap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_MINT_ROGUE_TEST, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(rtag) == IPC_OP_PONG && rogue_cap != 0 {
            let call_act = |act: DesktopAction| -> Result<[u64; 4], u32> {
                let w = act.to_words();
                let (t, _, m) = sys_ipc_call_inline(
                    rogue_cap,
                    pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
                    [w[0], w[1], 0, 0],
                    0,
                )
                .map_err(|_| IPC_OP_ERR_INVAL)?;
                let l = msg_tag_label(t);
                if l == IPC_OP_PONG { Ok(m) } else { Err(l) }
            };
            let r1 = call_act(DesktopAction::LaunchOrFocusApp(AppId::LifeStation));
            let r2 = call_act(DesktopAction::CloseApp(AppId::Terminal));
            let r3 = call_act(DesktopAction::RequestShutdown);
            let r4 = call_act(DesktopAction::RequestReboot);
            let _ = sys_cap_revoke(rogue_cap, 1);
            unauth_perm = r1 == Err(IPC_OP_ERR_PERM)
                && r2 == Err(IPC_OP_ERR_PERM)
                && r3 == Err(IPC_OP_ERR_PERM)
                && r4 == Err(IPC_OP_ERR_PERM);
        }
    }

    let bad1 = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
        [0xdead_beef, 999, 0, 0],
        0,
    )
    .map(|(t, _, _)| msg_tag_label(t));
    let bad2 = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
        [1, 99, 0, 0],
        0,
    )
    .map(|(t, _, _)| msg_tag_label(t));
    let bad_ipc_inval = bad1 == Ok(IPC_OP_ERR_INVAL) && bad2 == Ok(IPC_OP_ERR_INVAL);

    let mut vfs_rofs = false;
    let mut vfs_eperm = false;
    if let Ok((vtag, vcap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_GET_VFS_CAP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(vtag) == IPC_OP_PONG && vcap != 0 {
            let mut scratch = [0u8; 32];
            vfs_rofs = vfs_create_file(vcap, b"/boot/pwn.txt", 0) == Err(EROFS);
            vfs_eperm = vfs_read_file(vcap, b"/ram/vault_key.priv", 0, &mut scratch) == Err(EPERM)
                && vfs_unlink_file(vcap, b"/ram/vault_key.priv") == Err(EPERM);
            let _ = sys_cap_revoke(vcap, 1);
        }
    }

    let pwr_eperm = sys_power_ctl(POWER_OP_SHUTDOWN_S5, 0) == Err(EPERM)
        && sys_power_ctl(POWER_OP_REBOOT, 0xdead_beef) == Err(EPERM);

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"Security audit: kill_eperm=");
    lb.push_bytes(if kill_eperm { b"true" } else { b"false" });
    lb.push_bytes(b" unauth_perm=");
    lb.push_bytes(if unauth_perm { b"true" } else { b"false" });
    lb.push_bytes(b" pwr_eperm=");
    lb.push_bytes(if pwr_eperm { b"true" } else { b"false" });
    engine.push_line(
        lb.as_bytes(),
        if kill_eperm && unauth_perm && bad_ipc_inval && vfs_rofs && vfs_eperm && pwr_eperm {
            STYLE_OK
        } else {
            STYLE_WARN
        },
    );

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-SHELL] CMD=sec-audit kill_eperm=");
    slb.push_bytes(if kill_eperm { b"true" } else { b"false" });
    slb.push_bytes(b" unauth_perm=");
    slb.push_bytes(if unauth_perm { b"true" } else { b"false" });
    slb.push_bytes(b" bad_ipc_inval=");
    slb.push_bytes(if bad_ipc_inval { b"true" } else { b"false" });
    slb.push_bytes(b" vfs_rofs=");
    slb.push_bytes(if vfs_rofs { b"true" } else { b"false" });
    slb.push_bytes(b" vfs_eperm=");
    slb.push_bytes(if vfs_eperm { b"true" } else { b"false" });
    slb.push_bytes(b" pwr_eperm=");
    slb.push_bytes(if pwr_eperm { b"true" } else { b"false" });
    emit_dbg(&slb);
}
