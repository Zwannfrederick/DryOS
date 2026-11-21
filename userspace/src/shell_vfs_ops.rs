// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Hierarchical VFS tree, copy/move, and dynamic `exec` CLI commands (`pwd`, `cd`, `mkdir`,
//! `rmdir`, `cp`, `mv`, `exec`) for `dry-shell`.

use crate::abi_ext::IPC_OP_SUP_EXEC_VFS;
use crate::persist_fs::PERSIST_MAX_EXTENT_BYTES;
use crate::shell::{LineBuilder, ShellEngine};
use crate::shell_ext::ensure_vfs_cap;
use crate::surface::{STYLE_DEFAULT, STYLE_OK, STYLE_WARN};
use crate::vfs_mem::normalize_path;
use crate::vfs_proto::{
    sys_ipc_call_extra, vfs_create_file, vfs_list_dir, vfs_mkdir, vfs_read_file, vfs_rename_file,
    vfs_rmdir, vfs_stat, vfs_unlink_file, vfs_write_all_at, vfs_write_cap, VfsDirEntry,
    VFS_FLAG_DIR,
};
use crate::{
    msg_tag_label, sys_cap_revoke, sys_frame_alloc, sys_vspace_map, sys_write_debug, EEXIST,
    EINVAL, EXDEV, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES, VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_UNMAP_2M,
};

const SHELL_XFER_VADDR: u64 = 0x2400_0000;

fn emit_dbg(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
pub fn resolve_path(cwd: &[u8], raw: &[u8], out: &mut [u8; 96]) -> Option<usize> {
    normalize_path(cwd, raw, out)
}

fn split_two_args(raw: &[u8]) -> (&[u8], &[u8]) {
    let mut s = 0usize;
    while s < raw.len() && raw[s] == b' ' {
        s += 1;
    }
    let mut e = s;
    while e < raw.len() && raw[e] != b' ' {
        e += 1;
    }
    let first = &raw[s..e];
    let mut rs = e;
    while rs < raw.len() && raw[rs] == b' ' {
        rs += 1;
    }
    let mut re = raw.len();
    while re > rs && raw[re - 1] == b' ' {
        re -= 1;
    }
    (first, &raw[rs..re])
}

fn copy_vfs_file(vcap: u64, src: &[u8], dst: &[u8]) -> Result<usize, isize> {
    let (_, size, flags) = vfs_stat(vcap, src)?;
    if (flags & VFS_FLAG_DIR) != 0 {
        return Err(EINVAL);
    }
    if size > PERSIST_MAX_EXTENT_BYTES {
        return Err(crate::ENOSPC);
    }
    if size == 0 {
        return vfs_write_all_at(vcap, dst, &[], false, SHELL_XFER_VADDR);
    }
    if size <= 2048 {
        let mut buf = [0u8; 2048];
        let (got, _) = vfs_read_file(vcap, src, 0, &mut buf[..size])?;
        return vfs_write_all_at(vcap, dst, &buf[..got], false, SHELL_XFER_VADDR);
    }
    let frame_cap = sys_frame_alloc(1)?;
    if let Err(e) = sys_vspace_map(frame_cap, SHELL_XFER_VADDR, VSPACE_MAP_OP_2M_RW) {
        let _ = sys_cap_revoke(frame_cap, 2);
        return Err(e);
    }
    // SAFETY: `frame_cap` (`2 MiB`) is mapped RW exclusively in `dry-shell` at `SHELL_XFER_VADDR`
    // (`0x2400_0000`) and unmapped + revoked before returning below.
    let buf = unsafe { core::slice::from_raw_parts_mut(SHELL_XFER_VADDR as *mut u8, size) };
    let res = match vfs_read_file(vcap, src, 0, buf) {
        Ok((got, _)) => match vfs_create_file(vcap, dst, 0) {
            Ok(_) | Err(EEXIST) => vfs_write_cap(vcap, dst, frame_cap, got, false).map(|(w, _)| w),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    let _ = sys_vspace_map(frame_cap, SHELL_XFER_VADDR, VSPACE_MAP_OP_UNMAP_2M);
    let _ = sys_cap_revoke(frame_cap, 2);
    res
}

pub fn try_exec_vfs_tree_cmd(
    verb: &[u8],
    rest: &[u8],
    sup_ep_cap: u64,
    vfs_cap_slot: &mut u64,
    engine: &mut ShellEngine,
) -> bool {
    match verb {
        b"pwd" => {
            let mut lb = LineBuilder::new();
            lb.push_bytes(engine.cwd_bytes());
            engine.push_line(lb.as_bytes(), STYLE_OK);

            let mut slb = LineBuilder::new();
            slb.push_bytes(b"[DRY-SHELL] CMD=pwd cwd=");
            slb.push_bytes(engine.cwd_bytes());
            emit_dbg(&slb);
            true
        }
        b"cd" => {
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"cd: VFS unavailable", STYLE_WARN);
                return true;
            };
            let target_raw = if rest.is_empty() { b"/" } else { rest };
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), target_raw, &mut norm) else {
                engine.push_line(b"cd: invalid path", STYLE_WARN);
                return true;
            };
            let target = &norm[..nlen];
            let mut one = [VfsDirEntry::EMPTY; 1];
            match vfs_list_dir(vcap, target, &mut one) {
                Ok(_) => {
                    engine.set_cwd(target);
                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=cd cwd=");
                    slb.push_bytes(target);
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"cd error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"mkdir" | b"rmdir" => {
            if rest.is_empty() {
                engine.push_line(
                    if verb == b"mkdir" { b"Usage: mkdir <dir>" } else { b"Usage: rmdir <dir>" },
                    STYLE_WARN,
                );
                return true;
            }
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"VFS unavailable", STYLE_WARN);
                return true;
            };
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), rest, &mut norm) else {
                engine.push_line(b"Invalid path", STYLE_WARN);
                return true;
            };
            let target = &norm[..nlen];
            let res = if verb == b"mkdir" {
                vfs_mkdir(vcap, target).map(|_| ())
            } else {
                vfs_rmdir(vcap, target)
            };
            match res {
                Ok(()) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(if verb == b"mkdir" { b"Created dir " } else { b"Removed dir " });
                    lb.push_bytes(target);
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=");
                    slb.push_bytes(verb);
                    slb.push_bytes(b" path=");
                    slb.push_bytes(target);
                    slb.push_bytes(b" ok=true");
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(verb);
                    lb.push_bytes(b" error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"cp" => {
            let (s_raw, d_raw) = split_two_args(rest);
            if s_raw.is_empty() || d_raw.is_empty() {
                engine.push_line(b"Usage: cp <src> <dst>", STYLE_WARN);
                return true;
            }
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"cp: VFS unavailable", STYLE_WARN);
                return true;
            };
            let (mut sbuf, mut dbuf) = ([0u8; 96], [0u8; 96]);
            let (Some(slen), Some(dlen)) = (
                resolve_path(engine.cwd_bytes(), s_raw, &mut sbuf),
                resolve_path(engine.cwd_bytes(), d_raw, &mut dbuf),
            ) else {
                engine.push_line(b"cp: invalid path", STYLE_WARN);
                return true;
            };
            let (src, dst) = (&sbuf[..slen], &dbuf[..dlen]);
            match copy_vfs_file(vcap, src, dst) {
                Ok(bytes) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Copied ");
                    lb.push_u64(bytes as u64);
                    lb.push_bytes(b" B -> ");
                    lb.push_bytes(dst);
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=cp src=");
                    slb.push_bytes(src);
                    slb.push_bytes(b" dst=");
                    slb.push_bytes(dst);
                    slb.push_bytes(b" bytes=");
                    slb.push_u64(bytes as u64);
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"cp error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"mv" => {
            let (s_raw, d_raw) = split_two_args(rest);
            if s_raw.is_empty() || d_raw.is_empty() {
                engine.push_line(b"Usage: mv <src> <dst>", STYLE_WARN);
                return true;
            }
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, vfs_cap_slot) else {
                engine.push_line(b"mv: VFS unavailable", STYLE_WARN);
                return true;
            };
            let (mut sbuf, mut dbuf) = ([0u8; 96], [0u8; 96]);
            let (Some(slen), Some(dlen)) = (
                resolve_path(engine.cwd_bytes(), s_raw, &mut sbuf),
                resolve_path(engine.cwd_bytes(), d_raw, &mut dbuf),
            ) else {
                engine.push_line(b"mv: invalid path", STYLE_WARN);
                return true;
            };
            let (src, dst) = (&sbuf[..slen], &dbuf[..dlen]);
            let res = match vfs_rename_file(vcap, src, dst) {
                Ok(()) => Ok(()),
                Err(EXDEV) => copy_vfs_file(vcap, src, dst).and_then(|_| vfs_unlink_file(vcap, src)),
                Err(e) => Err(e),
            };
            match res {
                Ok(()) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Moved -> ");
                    lb.push_bytes(dst);
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=mv src=");
                    slb.push_bytes(src);
                    slb.push_bytes(b" dst=");
                    slb.push_bytes(dst);
                    slb.push_bytes(b" ok=true");
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"mv error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"exec" => {
            if rest.is_empty() {
                engine.push_line(b"Usage: exec </bin/prog.elf|/persist/prog.elf>", STYLE_WARN);
                return true;
            }
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), rest, &mut norm) else {
                engine.push_line(b"exec: invalid path", STYLE_WARN);
                return true;
            };
            let target = &norm[..nlen];
            let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
            match sys_ipc_call_extra(sup_ep_cap, IPC_OP_SUP_EXEC_VFS, [0; 4], target, &mut rx, 0) {
                Ok((tag, _, rmrs)) if msg_tag_label(tag) == IPC_OP_PONG => {
                    let rc = (rmrs[0] as i64) as i32;
                    let pid = rmrs[1];
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Executed ");
                    lb.push_bytes(target);
                    lb.push_bytes(b" (pid=");
                    lb.push_u64(pid);
                    lb.push_bytes(b", rc=");
                    lb.push_i64(rc as i64);
                    lb.push_byte(b')');
                    engine.push_line(lb.as_bytes(), if rc == 0 { STYLE_OK } else { STYLE_WARN });

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=exec path=");
                    slb.push_bytes(target);
                    slb.push_bytes(b" rc=");
                    slb.push_i64(rc as i64);
                    slb.push_bytes(b" pid=");
                    slb.push_u64(pid);
                    emit_dbg(&slb);
                }
                Ok((_, _, rmrs)) => {
                    let err = (rmrs[0] as i64) as isize;
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"exec error: ");
                    lb.push_i64(err as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=exec path=");
                    slb.push_bytes(target);
                    slb.push_bytes(b" err=");
                    slb.push_i64(err as i64);
                    emit_dbg(&slb);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"exec IPC error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"linux-run" => {
            if rest.is_empty() {
                engine.push_line(b"Usage: linux-run </bin/prog.elf|/ram/prog.elf>", STYLE_WARN);
                return true;
            }
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), rest, &mut norm) else {
                engine.push_line(b"linux-run: invalid path", STYLE_WARN);
                return true;
            };
            let target = &norm[..nlen];
            let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
            match sys_ipc_call_extra(
                sup_ep_cap,
                crate::abi_ext::IPC_OP_SUP_LINUX_RUN,
                [0; 4],
                target,
                &mut rx,
                0,
            ) {
                Ok((tag, rx_len, rmrs)) if msg_tag_label(tag) == IPC_OP_PONG => {
                    let rc = (rmrs[0] as i64) as i32;
                    let pid = rmrs[1];
                    let sc = rmrs[2];
                    if rx_len > 0 {
                        engine.push_line(&rx[..rx_len.min(128)], STYLE_DEFAULT);
                    }
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"Linux ELF ");
                    lb.push_bytes(target);
                    lb.push_bytes(b" (pid=");
                    lb.push_u64(pid);
                    lb.push_bytes(b", rc=");
                    lb.push_i64(rc as i64);
                    lb.push_bytes(b", syscalls=");
                    lb.push_u64(sc);
                    lb.push_byte(b')');
                    engine.push_line(lb.as_bytes(), if rc == 42 || rc == 0 { STYLE_OK } else { STYLE_WARN });

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=linux-run path=");
                    slb.push_bytes(target);
                    slb.push_bytes(b" rc=");
                    slb.push_i64(rc as i64);
                    slb.push_bytes(b" pid=");
                    slb.push_u64(pid);
                    slb.push_bytes(b" syscalls=");
                    slb.push_u64(sc);
                    slb.push_bytes(b" out=\"");
                    for &b in &rx[..rx_len.min(80)] {
                        slb.push_byte(if (0x20..=0x7e).contains(&b) { b } else { b'.' });
                    }
                    slb.push_byte(b'"');
                    emit_dbg(&slb);
                }
                Ok((_, _, rmrs)) => {
                    let err = (rmrs[0] as i64) as isize;
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"linux-run error: ");
                    lb.push_i64(err as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
                Err(code) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"linux-run IPC error: ");
                    lb.push_i64(code as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        _ => false,
    }
}
