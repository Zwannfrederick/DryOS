// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Extracted mutating VFS operations (`CREATE`, `WRITE`, `WRITE_CAP`, `MKDIR`, `RMDIR`, `RENAME`,
//! `LINK`, `UNLINK`) so `dry_vfs.rs` stays concise and all permission/RO checks share one path.

use crate::ioring::IoRingClient;
use crate::persist_fs::{PersistMount, VfsCallerCtx, PERSIST_MAX_EXTENT_BYTES};
use crate::shell::LineBuilder;
use crate::vfs_mem::{
    name_eq, parse_file_path, parse_two_paths, MemMounts, MountKind, MAX_RAM_FILE_BYTES,
};
use crate::vfs_proto::{
    VFS_FLAG_DIR, VFS_FLAG_PRIVATE, VFS_FLAG_VOLATILE, VFS_OP_CREATE, VFS_OP_ERR, VFS_OP_MKDIR,
    VFS_OP_RENAME, VFS_OP_RMDIR, VFS_OP_UNLINK, VFS_OP_WRITE, VFS_OP_WRITE_CAP,
};
use crate::{
    pack_msg_tag, sys_cap_query, sys_cap_revoke, sys_ipc_reply_inline, sys_vspace_map,
    sys_write_debug, UserCapInfo, BADGE_SUPERVISOR, EEXIST, EINVAL, ENOENT, ENOSPC, EPERM, EROFS,
    EXDEV, IPC_OP_PONG, VFS_OP_LINK, VSPACE_MAP_OP_2M_RO, VSPACE_MAP_OP_4K_RO,
    VSPACE_MAP_OP_UNMAP_2M, VSPACE_MAP_OP_UNMAP_4K,
};

const VFS_XFER_VADDR: u64 = 0x2220_0000;
const CAP_KIND_FRAME_4K: u8 = 2;
const CAP_KIND_FRAME_2M: u8 = 3;

#[derive(Clone, Copy, Default)]
pub struct VfsStats {
    pub ops_ok: u64,
    pub rofs_rejects: u64,
    pub unauth_rejects: u64,
    pub inval_rejects: u64,
}

impl VfsStats {
    #[inline]
    pub fn tally_err(&mut self, err: isize) {
        match err {
            EROFS => self.rofs_rejects = self.rofs_rejects.wrapping_add(1),
            EPERM => self.unauth_rejects = self.unauth_rejects.wrapping_add(1),
            EINVAL => self.inval_rejects = self.inval_rejects.wrapping_add(1),
            _ => {}
        }
    }
}

pub fn reply_vfs_err(ep: u64, tok: u64, err: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(VFS_OP_ERR, 0, 1, 0),
            [err as u64, 0, 0, 0],
        );
    }
}

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 196];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
fn is_ro_mount(mk: MountKind, persist_ro: bool) -> bool {
    matches!(mk, MountKind::Boot | MountKind::Bin) || (mk == MountKind::Persist && persist_ro)
}

#[inline]
fn check_writer_comp(sender_badge: u64) -> Result<u16, isize> {
    let sender_comp = (sender_badge & 0xffff) as u16;
    if sender_comp == 0 && sender_badge != BADGE_SUPERVISOR {
        Err(EPERM)
    } else {
        Ok(sender_comp)
    }
}

#[allow(clippy::too_many_arguments)]
fn write_payload_common(
    mem: &mut MemMounts,
    persist: &mut PersistMount,
    ring: &mut IoRingClient,
    persist_ro: bool,
    path: &[u8],
    payload: &[u8],
    append: bool,
    caller: VfsCallerCtx,
    sender_badge: u64,
) -> Result<(usize, usize), isize> {
    let (mk, fname) = parse_file_path(path)?;
    if is_ro_mount(mk, persist_ro) {
        return Err(EROFS);
    }
    if mk == MountKind::Persist {
        if !persist.mounted {
            return Err(ENOENT);
        }
        return persist.write_full(ring, fname, caller, payload, append);
    }
    let idx = mem
        .ram_inodes
        .iter()
        .position(|n| n.used && name_eq(&n.name, fname))
        .ok_or(ENOENT)?;
    let node_copy = mem.ram_inodes[idx];
    if (node_copy.flags & VFS_FLAG_DIR) != 0 {
        return Err(EINVAL);
    }
    MemMounts::check_ram_write_perm(&node_copy, sender_badge)?;
    let base_pos = if append { node_copy.len } else { 0 };
    if base_pos.saturating_add(payload.len()) > MAX_RAM_FILE_BYTES {
        return Err(ENOSPC);
    }
    mem.ram_data[idx][base_pos..(base_pos + payload.len())].copy_from_slice(payload);
    mem.ram_inodes[idx].len = base_pos + payload.len();
    Ok((payload.len(), mem.ram_inodes[idx].len))
}

#[allow(clippy::too_many_arguments)]
pub fn try_exec_mut_op(
    op: u32,
    srv_ep_cap: u64,
    reply_tok: u64,
    rx: &[u8],
    mrs: &[u64; 4],
    caller: VfsCallerCtx,
    sender_badge: u64,
    transferred_cap: u64,
    mem: &mut MemMounts,
    persist: &mut PersistMount,
    ring: &mut IoRingClient,
    persist_ro: bool,
    stats: &mut VfsStats,
) -> bool {
    match op {
        VFS_OP_CREATE | VFS_OP_MKDIR => {
            let (mk, fname) = match parse_file_path(rx) {
                Ok(v) => v,
                Err(e) => {
                    stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                    return true;
                }
            };
            if is_ro_mount(mk, persist_ro) {
                stats.rofs_rejects = stats.rofs_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                return true;
            }
            let Ok(sender_comp) = check_writer_comp(sender_badge) else {
                stats.unauth_rejects = stats.unauth_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                return true;
            };
            let res = if mk == MountKind::Persist {
                if !persist.mounted {
                    Err(ENOENT)
                } else if op == VFS_OP_MKDIR {
                    persist.mkdir(ring, fname, caller)
                } else {
                    persist.create(ring, fname, caller, (mrs[0] as u16) & VFS_FLAG_PRIVATE)
                }
            } else if op == VFS_OP_MKDIR {
                mem.mkdir_ram(fname, sender_comp)
            } else if mem.ram_inodes.iter().any(|n| n.used && name_eq(&n.name, fname)) {
                Err(EEXIST)
            } else {
                let req_priv = (mrs[0] as u16) & VFS_FLAG_PRIVATE;
                let r = mem.seed_ram_file(fname, sender_comp, VFS_FLAG_VOLATILE | req_priv, b"");
                if let Ok(ino) = r {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-VFS] CREATE path=/ram/");
                    lb.push_bytes(fname);
                    lb.push_bytes(b" ino=");
                    lb.push_u64(ino as u64);
                    lb.push_bytes(b" comp=0x");
                    lb.push_hex_u64(sender_comp as u64);
                    log_line(&lb);
                }
                r
            };
            match res {
                Ok(ino) => {
                    stats.ops_ok = stats.ops_ok.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                            [ino as u64, 0, 0, 0],
                        );
                    }
                }
                Err(e) => {
                    stats.tally_err(e);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                }
            }
            true
        }
        VFS_OP_WRITE => {
            if rx.len() < 2 {
                stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                return true;
            }
            let plen = rx[0] as usize;
            if plen == 0 || 1 + plen > rx.len() {
                stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                return true;
            }
            let res = write_payload_common(
                mem,
                persist,
                ring,
                persist_ro,
                &rx[1..(1 + plen)],
                &rx[(1 + plen)..],
                mrs[0] != 0,
                caller,
                sender_badge,
            );
            match res {
                Ok((written, total)) => {
                    stats.ops_ok = stats.ops_ok.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                            [written as u64, total as u64, 0, 0],
                        );
                    }
                }
                Err(e) => {
                    stats.tally_err(e);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                }
            }
            true
        }
        VFS_OP_WRITE_CAP => {
            let blen = mrs[0] as usize;
            let append = mrs[1] != 0;
            if transferred_cap == 0 || blen > PERSIST_MAX_EXTENT_BYTES {
                if transferred_cap != 0 {
                    let _ = sys_cap_revoke(transferred_cap, 1);
                }
                stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                return true;
            }
            let mut ci = UserCapInfo::default();
            if sys_cap_query(transferred_cap, &mut ci) != 0 {
                let _ = sys_cap_revoke(transferred_cap, 1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                return true;
            }
            let (cap_max, map_op, unmap_op) = match ci.obj_kind {
                CAP_KIND_FRAME_4K => (4096usize, VSPACE_MAP_OP_4K_RO, VSPACE_MAP_OP_UNMAP_4K),
                CAP_KIND_FRAME_2M => (2 * 1024 * 1024, VSPACE_MAP_OP_2M_RO, VSPACE_MAP_OP_UNMAP_2M),
                _ => {
                    let _ = sys_cap_revoke(transferred_cap, 1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    return true;
                }
            };
            if blen > cap_max || sys_vspace_map(transferred_cap, VFS_XFER_VADDR, map_op).is_err() {
                let _ = sys_cap_revoke(transferred_cap, 1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                return true;
            }
            // SAFETY: `transferred_cap` is mapped RO at `VFS_XFER_VADDR` covering `blen <= cap_max`.
            let data = unsafe { core::slice::from_raw_parts(VFS_XFER_VADDR as *const u8, blen) };
            let res = write_payload_common(
                mem,
                persist,
                ring,
                persist_ro,
                rx,
                data,
                append,
                caller,
                sender_badge,
            );
            let _ = sys_vspace_map(transferred_cap, VFS_XFER_VADDR, unmap_op);
            let _ = sys_cap_revoke(transferred_cap, 1);
            match res {
                Ok((written, total)) => {
                    stats.ops_ok = stats.ops_ok.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                            [written as u64, total as u64, 0, 0],
                        );
                    }
                }
                Err(e) => {
                    stats.tally_err(e);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                }
            }
            true
        }
        VFS_OP_RENAME => {
            let Ok((old_p, new_p)) = parse_two_paths(rx) else {
                stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                return true;
            };
            let ((mk_old, old_name), (mk_new, new_name)) =
                match (parse_file_path(old_p), parse_file_path(new_p)) {
                    (Ok(a), Ok(b)) => (a, b),
                    _ => {
                        stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                        return true;
                    }
                };
            if is_ro_mount(mk_old, persist_ro) || is_ro_mount(mk_new, persist_ro) {
                stats.rofs_rejects = stats.rofs_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                return true;
            }
            if mk_old != mk_new {
                reply_vfs_err(srv_ep_cap, reply_tok, EXDEV);
                return true;
            }
            let res = if mk_old == MountKind::Persist {
                if !persist.mounted {
                    Err(ENOENT)
                } else {
                    persist.rename(ring, old_name, new_name, caller)
                }
            } else {
                let r = mem.rename_ram(old_name, new_name, sender_badge);
                if r.is_ok() {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-VFS] RENAME old=");
                    lb.push_bytes(old_name);
                    lb.push_bytes(b" new=");
                    lb.push_bytes(new_name);
                    log_line(&lb);
                }
                r
            };
            match res {
                Ok(()) => {
                    stats.ops_ok = stats.ops_ok.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                            [1, 0, 0, 0],
                        );
                    }
                }
                Err(e) => {
                    stats.tally_err(e);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                }
            }
            true
        }
        VFS_OP_LINK => {
            let Ok((src_p, dst_p)) = parse_two_paths(rx) else {
                stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                return true;
            };
            let ((mk_src, src_name), (mk_dst, dst_name)) =
                match (parse_file_path(src_p), parse_file_path(dst_p)) {
                    (Ok(a), Ok(b)) => (a, b),
                    _ => {
                        stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                        return true;
                    }
                };
            if mk_src != mk_dst {
                reply_vfs_err(srv_ep_cap, reply_tok, EXDEV);
                return true;
            }
            match mk_src {
                MountKind::Boot | MountKind::Bin => {
                    stats.rofs_rejects = stats.rofs_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                }
                MountKind::Persist if persist_ro => {
                    stats.rofs_rejects = stats.rofs_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                }
                MountKind::Persist => {
                    if !persist.mounted {
                        reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    } else {
                        match persist.link(ring, src_name, dst_name, caller) {
                            Ok(ino) => {
                                stats.ops_ok = stats.ops_ok.wrapping_add(1);
                                if reply_tok != 0 {
                                    let _ = sys_ipc_reply_inline(
                                        srv_ep_cap,
                                        reply_tok,
                                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                                        [ino as u64, 0, 0, 0],
                                    );
                                }
                            }
                            Err(e) => {
                                stats.tally_err(e);
                                reply_vfs_err(srv_ep_cap, reply_tok, e);
                            }
                        }
                    }
                }
                MountKind::Ram => reply_vfs_err(srv_ep_cap, reply_tok, EXDEV),
            }
            true
        }
        VFS_OP_UNLINK | VFS_OP_RMDIR => {
            let (mk, fname) = match parse_file_path(rx) {
                Ok(v) => v,
                Err(e) => {
                    stats.inval_rejects = stats.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                    return true;
                }
            };
            if is_ro_mount(mk, persist_ro) {
                stats.rofs_rejects = stats.rofs_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                return true;
            }
            let res = if mk == MountKind::Persist {
                if !persist.mounted {
                    Err(ENOENT)
                } else if op == VFS_OP_RMDIR {
                    persist.rmdir(ring, fname, caller)
                } else {
                    persist.unlink(ring, fname, caller)
                }
            } else if op == VFS_OP_RMDIR {
                mem.rmdir_ram(fname, sender_badge)
            } else {
                let r = mem.unlink_ram(fname, sender_badge);
                if r.is_ok() {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-VFS] UNLINK path=/ram/");
                    lb.push_bytes(fname);
                    log_line(&lb);
                }
                r
            };
            match res {
                Ok(()) => {
                    stats.ops_ok = stats.ops_ok.wrapping_add(1);
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                            [1, 0, 0, 0],
                        );
                    }
                }
                Err(e) => {
                    stats.tally_err(e);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                }
            }
            true
        }
        _ => false,
    }
}
