// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-vfs` — Isolated Ring 3 (`CPL=3`) Capability-Badged Virtual Filesystem Service (`0x0004`).
//!
//! Dispatches IPC requests across `/boot` + `/ram` (`vfs_mem::MemMounts`) and `/persist`
//! (`persist_fs::PersistMount` over lock-free `IoRingClient` to `dry-blk`).

#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use core::mem::size_of;

use dry_userspace::ioring::IoRingClient;
use dry_userspace::persist_fs::{PersistMount, VfsCallerCtx};
use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_mem::{
    is_valid_filename, name_eq, parse_file_path, parse_mount_dir, parse_two_paths, MemMounts,
    MountKind, BOOT_README_TEXT, MAX_RAM_FILE_BYTES, OS_RELEASE_TEXT,
};
use dry_userspace::vfs_proto::{
    sys_ipc_recv_extra, sys_ipc_reply_extra, VfsDirEntry, VFS_DIRENTS_PER_CALL,
    VFS_FLAG_EXECUTABLE, VFS_FLAG_PRIVATE, VFS_FLAG_READONLY, VFS_FLAG_VOLATILE, VFS_OP_CREATE,
    VFS_OP_ERR, VFS_OP_LIST, VFS_OP_QUERY_STATS, VFS_OP_READ, VFS_OP_RENAME, VFS_OP_SEED_BOOT,
    VFS_OP_STAT, VFS_OP_UNLINK, VFS_OP_WRITE,
};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_reply_inline,
    sys_write_debug, UserIpcRecvExt, BADGE_SUPERVISOR, EEXIST, EINVAL, ENOENT, ENOSPC, EPERM,
    EROFS, EXDEV, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES, VFS_OP_CORRUPT_SB0_TEST,
    VFS_OP_LINK, VFS_OP_MOUNT_BLK, VFS_OP_SYNC,
};

const VFS_RING_VADDR: u64 = 0x2100_0000;
const VFS_DATA_VADDR: u64 = 0x2100_1000;

struct VfsState {
    mem: MemMounts,
    ring: IoRingClient,
    persist: PersistMount,
    ops_ok: u64,
    rofs_rejects: u64,
    unauth_rejects: u64,
    inval_rejects: u64,
}

impl VfsState {
    #[inline]
    fn tally_err(&mut self, err: isize) {
        match err {
            EROFS => self.rofs_rejects = self.rofs_rejects.wrapping_add(1),
            EPERM => self.unauth_rejects = self.unauth_rejects.wrapping_add(1),
            EINVAL => self.inval_rejects = self.inval_rejects.wrapping_add(1),
            _ => {}
        }
    }
}

struct SingleThreadVfs(UnsafeCell<VfsState>);
// SAFETY: `dry-vfs` is strictly single-threaded in CPL=3.
unsafe impl Sync for SingleThreadVfs {}

static VFS_STATE: SingleThreadVfs = SingleThreadVfs(UnsafeCell::new(VfsState {
    mem: MemMounts::new(),
    ring: IoRingClient::empty(),
    persist: PersistMount::new(),
    ops_ok: 0,
    rofs_rejects: 0,
    unauth_rejects: 0,
    inval_rejects: 0,
}));

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 196];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn reply_vfs_err(ep: u64, tok: u64, err: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(VFS_OP_ERR, 0, 1, 0),
            [err as u64, 0, 0, 0],
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(srv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || srv_ep_cap == 0 {
        sys_exit(120);
    }

    // SAFETY: Single-threaded `dry-vfs` server owns `VFS_STATE` exclusively.
    let st = unsafe { &mut *VFS_STATE.0.get() };
    st.mem.init_defaults();
    let _ = sys_write_debug(b"[DRY-VFS] Ready in CPL3 RootSystem (/boot ro + /ram volatile)\n");

    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, rx_len, mrs) = match sys_ipc_recv_extra(srv_ep_cap, 0, 0, &mut rx, &mut ext) {
            Ok(v) => v,
            Err(_) => break,
        };
        let reply_tok = ext.reply_token;
        let op = msg_tag_label(tag);
        let caller = VfsCallerCtx::from_badge(ext.sender_badge);

        if ext.transferred_cap != 0 && op != VFS_OP_MOUNT_BLK {
            let _ = sys_cap_revoke(ext.transferred_cap, 1);
        }

        match op {
            VFS_OP_SEED_BOOT => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    st.unauth_rejects = st.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                let name = &rx[..rx_len];
                if !is_valid_filename(name) {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                }
                st.mem.add_static_boot_node(
                    name,
                    mrs[0].min(u32::MAX as u64) as u32,
                    VFS_FLAG_READONLY | VFS_FLAG_EXECUTABLE,
                    0,
                );
                st.ops_ok = st.ops_ok.wrapping_add(1);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
            }
            VFS_OP_MOUNT_BLK => {
                if ext.sender_badge != BADGE_SUPERVISOR || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    st.unauth_rejects = st.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                st.ring.teardown();
                st.persist.mounted = false;
                let client = match IoRingClient::connect(
                    ext.transferred_cap,
                    VFS_RING_VADDR,
                    VFS_DATA_VADDR,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                        continue;
                    }
                };
                st.ring = client;
                match st.persist.mount_or_format(&mut st.ring) {
                    Ok((fresh, tx_seq)) => {
                        st.ops_ok = st.ops_ok.wrapping_add(1);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-VFS] PERSIST_MOUNT fresh=");
                        lb.push_bytes(if fresh { b"true" } else { b"false" });
                        lb.push_bytes(b" tx_seq=");
                        lb.push_u64(tx_seq);
                        lb.push_bytes(b" recovered=");
                        lb.push_bytes(if st.persist.recovered_fallback {
                            b"true"
                        } else {
                            b"false"
                        });
                        log_line(&lb);
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                [
                                    u64::from(fresh),
                                    tx_seq,
                                    u64::from(st.persist.recovered_fallback),
                                    0,
                                ],
                            );
                        }
                    }
                    Err(e) => {
                        st.ring.teardown();
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                    }
                }
            }
            VFS_OP_SYNC => {
                if !st.persist.mounted {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                            [0, 0, 0, 0],
                        );
                    }
                    continue;
                }
                match st.ring.flush() {
                    Ok(()) => {
                        st.ops_ok = st.ops_ok.wrapping_add(1);
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                                [st.persist.sb.tx_seq, st.persist.commits, 0, 0],
                            );
                        }
                    }
                    Err(e) => reply_vfs_err(srv_ep_cap, reply_tok, e),
                }
            }
            VFS_OP_CORRUPT_SB0_TEST => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    st.unauth_rejects = st.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                if !st.persist.mounted {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                }
                match st.persist.corrupt_slot0_for_recovery_test(&mut st.ring) {
                    Ok(()) => {
                        let _ = sys_write_debug(b"[DRY-VFS] CORRUPT_SB0_TEST slot=0 active=1\n");
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                                [1, 0, 0, 0],
                            );
                        }
                    }
                    Err(e) => reply_vfs_err(srv_ep_cap, reply_tok, e),
                }
            }
            VFS_OP_LIST => {
                let Some(mk) = parse_mount_dir(&rx[..rx_len]) else {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                };
                if mk == MountKind::Persist && !st.persist.mounted {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                }
                let start_idx = mrs[0] as usize;
                let mut entries = [VfsDirEntry::EMPTY; VFS_DIRENTS_PER_CALL];
                let (batch_n, total_n, mount_flags) = match mk {
                    MountKind::Boot => {
                        let (b, t) = st.mem.list_boot(start_idx, &mut entries);
                        (b, t, VFS_FLAG_READONLY)
                    }
                    MountKind::Ram => {
                        let (b, t) = st.mem.list_ram(start_idx, &mut entries);
                        (b, t, VFS_FLAG_VOLATILE)
                    }
                    MountKind::Persist => {
                        let (b, t) = st.persist.list(start_idx, &mut entries);
                        (b, t, 0)
                    }
                };
                st.ops_ok = st.ops_ok.wrapping_add(1);
                if reply_tok != 0 {
                    let byte_len = batch_n * size_of::<VfsDirEntry>();
                    // SAFETY: `entries` is a contiguous `[VfsDirEntry; 5]` POD array (`<= 240` bytes).
                    let raw = unsafe {
                        core::slice::from_raw_parts(entries.as_ptr().cast::<u8>(), byte_len)
                    };
                    let _ = sys_ipc_reply_extra(
                        srv_ep_cap,
                        reply_tok,
                        IPC_OP_PONG,
                        [batch_n as u64, total_n as u64, mount_flags as u64, 0],
                        raw,
                    );
                }
            }
            VFS_OP_STAT | VFS_OP_READ => {
                let (mk, fname) = match parse_file_path(&rx[..rx_len]) {
                    Ok(v) => v,
                    Err(e) => {
                        st.tally_err(e);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                        continue;
                    }
                };
                let offset = mrs[0] as usize;
                let want = (mrs[1] as usize).clamp(1, MAX_IPC_EXTRA_BYTES);
                match mk {
                    MountKind::Boot => {
                        let Some(node) = st
                            .mem
                            .boot_nodes
                            .iter()
                            .find(|n| n.used && name_eq(&n.name, fname))
                        else {
                            reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                            continue;
                        };
                        let mut dyn_buf = [0u8; 256];
                        let content: &[u8] = match node.static_kind {
                            1 => OS_RELEASE_TEXT,
                            2 => BOOT_README_TEXT,
                            _ => {
                                let len = MemMounts::render_boot_elf_summary(node, &mut dyn_buf);
                                &dyn_buf[..len]
                            }
                        };
                        st.ops_ok = st.ops_ok.wrapping_add(1);
                        if op == VFS_OP_STAT {
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    srv_ep_cap,
                                    reply_tok,
                                    pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                    [
                                        node.inode as u64,
                                        node.size_bytes as u64,
                                        node.flags as u64,
                                        0,
                                    ],
                                );
                            }
                        } else if reply_tok != 0 {
                            let slice = if offset < content.len() {
                                &content[offset..(offset + want).min(content.len())]
                            } else {
                                &[]
                            };
                            let _ = sys_ipc_reply_extra(
                                srv_ep_cap,
                                reply_tok,
                                IPC_OP_PONG,
                                [slice.len() as u64, content.len() as u64, node.flags as u64, 0],
                                slice,
                            );
                        }
                    }
                    MountKind::Ram => {
                        let Some(idx) = st
                            .mem
                            .ram_inodes
                            .iter()
                            .position(|n| n.used && name_eq(&n.name, fname))
                        else {
                            reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                            continue;
                        };
                        let node_copy = st.mem.ram_inodes[idx];
                        if let Err(e) = MemMounts::check_ram_read_perm(&node_copy, ext.sender_badge)
                        {
                            st.tally_err(e);
                            reply_vfs_err(srv_ep_cap, reply_tok, e);
                            continue;
                        }
                        st.ops_ok = st.ops_ok.wrapping_add(1);
                        if op == VFS_OP_STAT {
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    srv_ep_cap,
                                    reply_tok,
                                    pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                    [
                                        node_copy.inode as u64,
                                        node_copy.len as u64,
                                        node_copy.flags as u64,
                                        0,
                                    ],
                                );
                            }
                        } else if reply_tok != 0 {
                            let slice = if offset < node_copy.len {
                                &st.mem.ram_data[idx][offset..(offset + want).min(node_copy.len)]
                            } else {
                                &[]
                            };
                            let _ = sys_ipc_reply_extra(
                                srv_ep_cap,
                                reply_tok,
                                IPC_OP_PONG,
                                [
                                    slice.len() as u64,
                                    node_copy.len as u64,
                                    node_copy.flags as u64,
                                    0,
                                ],
                                slice,
                            );
                        }
                    }
                    MountKind::Persist => {
                        if !st.persist.mounted {
                            reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                            continue;
                        }
                        let mut out_buf = [0u8; MAX_IPC_EXTRA_BYTES];
                        let dst = if op == VFS_OP_STAT {
                            &mut out_buf[..0]
                        } else {
                            &mut out_buf[..want]
                        };
                        match st.persist.read_slice(&mut st.ring, fname, caller, offset, dst) {
                            Ok((got, total, flags, ino)) => {
                                st.ops_ok = st.ops_ok.wrapping_add(1);
                                if op == VFS_OP_STAT {
                                    if reply_tok != 0 {
                                        let _ = sys_ipc_reply_inline(
                                            srv_ep_cap,
                                            reply_tok,
                                            pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                            [ino as u64, total as u64, flags as u64, 0],
                                        );
                                    }
                                } else if reply_tok != 0 {
                                    let _ = sys_ipc_reply_extra(
                                        srv_ep_cap,
                                        reply_tok,
                                        IPC_OP_PONG,
                                        [got as u64, total as u64, flags as u64, 0],
                                        &out_buf[..got],
                                    );
                                }
                            }
                            Err(e) => {
                                st.tally_err(e);
                                reply_vfs_err(srv_ep_cap, reply_tok, e);
                            }
                        }
                    }
                }
            }
            VFS_OP_CREATE => {
                let (mk, fname) = match parse_file_path(&rx[..rx_len]) {
                    Ok(v) => v,
                    Err(e) => {
                        st.inval_rejects = st.inval_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                        continue;
                    }
                };
                if mk == MountKind::Boot {
                    st.rofs_rejects = st.rofs_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                    continue;
                }
                let sender_comp = (ext.sender_badge & 0xffff) as u16;
                if sender_comp == 0 && ext.sender_badge != BADGE_SUPERVISOR {
                    st.unauth_rejects = st.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                let req_priv = (mrs[0] as u16) & VFS_FLAG_PRIVATE;
                if mk == MountKind::Persist {
                    if !st.persist.mounted {
                        reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                        continue;
                    }
                    match st.persist.create(&mut st.ring, fname, caller, req_priv) {
                        Ok(ino) => {
                            st.ops_ok = st.ops_ok.wrapping_add(1);
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
                            st.tally_err(e);
                            reply_vfs_err(srv_ep_cap, reply_tok, e);
                        }
                    }
                    continue;
                }
                if st
                    .mem
                    .ram_inodes
                    .iter()
                    .any(|n| n.used && name_eq(&n.name, fname))
                {
                    reply_vfs_err(srv_ep_cap, reply_tok, EEXIST);
                    continue;
                }
                match st
                    .mem
                    .seed_ram_file(fname, sender_comp, VFS_FLAG_VOLATILE | req_priv, b"")
                {
                    Ok(ino) => {
                        st.ops_ok = st.ops_ok.wrapping_add(1);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-VFS] CREATE path=/ram/");
                        lb.push_bytes(fname);
                        lb.push_bytes(b" ino=");
                        lb.push_u64(ino as u64);
                        lb.push_bytes(b" comp=0x");
                        lb.push_hex_u64(sender_comp as u64);
                        log_line(&lb);
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                                [ino as u64, 0, 0, 0],
                            );
                        }
                    }
                    Err(e) => reply_vfs_err(srv_ep_cap, reply_tok, e),
                }
            }
            VFS_OP_WRITE => {
                if rx_len < 2 {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                }
                let plen = rx[0] as usize;
                if plen == 0 || 1 + plen > rx_len {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                }
                let path = &rx[1..(1 + plen)];
                let payload = &rx[(1 + plen)..rx_len];
                let (mk, fname) = match parse_file_path(path) {
                    Ok(v) => v,
                    Err(e) => {
                        st.inval_rejects = st.inval_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                        continue;
                    }
                };
                if mk == MountKind::Boot {
                    st.rofs_rejects = st.rofs_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                    continue;
                }
                let append = mrs[0] != 0;
                if mk == MountKind::Persist {
                    if !st.persist.mounted {
                        reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                        continue;
                    }
                    match st.persist.write(&mut st.ring, fname, caller, payload, append) {
                        Ok((written, total)) => {
                            st.ops_ok = st.ops_ok.wrapping_add(1);
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
                            st.tally_err(e);
                            reply_vfs_err(srv_ep_cap, reply_tok, e);
                        }
                    }
                    continue;
                }
                let Some(idx) = st
                    .mem
                    .ram_inodes
                    .iter()
                    .position(|n| n.used && name_eq(&n.name, fname))
                else {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                };
                let node_copy = st.mem.ram_inodes[idx];
                if let Err(e) = MemMounts::check_ram_write_perm(&node_copy, ext.sender_badge) {
                    st.tally_err(e);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                    continue;
                }
                let base_pos = if append { node_copy.len } else { 0 };
                if base_pos + payload.len() > MAX_RAM_FILE_BYTES {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOSPC);
                    continue;
                }
                st.mem.ram_data[idx][base_pos..(base_pos + payload.len())].copy_from_slice(payload);
                st.mem.ram_inodes[idx].len = base_pos + payload.len();
                st.ops_ok = st.ops_ok.wrapping_add(1);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [payload.len() as u64, st.mem.ram_inodes[idx].len as u64, 0, 0],
                    );
                }
            }
            VFS_OP_RENAME => {
                let Ok((old_p, new_p)) = parse_two_paths(&rx[..rx_len]) else {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                };
                let ((mk_old, old_name), (mk_new, new_name)) =
                    match (parse_file_path(old_p), parse_file_path(new_p)) {
                        (Ok(a), Ok(b)) => (a, b),
                        _ => {
                            st.inval_rejects = st.inval_rejects.wrapping_add(1);
                            reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                            continue;
                        }
                    };
                if mk_old == MountKind::Boot || mk_new == MountKind::Boot {
                    st.rofs_rejects = st.rofs_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                    continue;
                }
                if mk_old != mk_new {
                    reply_vfs_err(srv_ep_cap, reply_tok, EXDEV);
                    continue;
                }
                let res = if mk_old == MountKind::Persist {
                    if !st.persist.mounted {
                        Err(ENOENT)
                    } else {
                        st.persist.rename(&mut st.ring, old_name, new_name, caller)
                    }
                } else {
                    let r = st.mem.rename_ram(old_name, new_name, ext.sender_badge);
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
                        st.ops_ok = st.ops_ok.wrapping_add(1);
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
                        st.tally_err(e);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                    }
                }
            }
            VFS_OP_LINK => {
                let Ok((src_p, dst_p)) = parse_two_paths(&rx[..rx_len]) else {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                };
                let ((mk_src, src_name), (mk_dst, dst_name)) =
                    match (parse_file_path(src_p), parse_file_path(dst_p)) {
                        (Ok(a), Ok(b)) => (a, b),
                        _ => {
                            st.inval_rejects = st.inval_rejects.wrapping_add(1);
                            reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                            continue;
                        }
                    };
                if mk_src != mk_dst {
                    reply_vfs_err(srv_ep_cap, reply_tok, EXDEV);
                    continue;
                }
                match mk_src {
                    MountKind::Boot => {
                        st.rofs_rejects = st.rofs_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                    }
                    MountKind::Persist => {
                        if !st.persist.mounted {
                            reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                            continue;
                        }
                        match st.persist.link(&mut st.ring, src_name, dst_name, caller) {
                            Ok(ino) => {
                                st.ops_ok = st.ops_ok.wrapping_add(1);
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
                                st.tally_err(e);
                                reply_vfs_err(srv_ep_cap, reply_tok, e);
                            }
                        }
                    }
                    MountKind::Ram => reply_vfs_err(srv_ep_cap, reply_tok, EXDEV),
                }
            }
            VFS_OP_UNLINK => {
                let (mk, fname) = match parse_file_path(&rx[..rx_len]) {
                    Ok(v) => v,
                    Err(e) => {
                        st.inval_rejects = st.inval_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                        continue;
                    }
                };
                if mk == MountKind::Boot {
                    st.rofs_rejects = st.rofs_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                    continue;
                }
                let res = if mk == MountKind::Persist {
                    if !st.persist.mounted {
                        Err(ENOENT)
                    } else {
                        st.persist.unlink(&mut st.ring, fname, caller)
                    }
                } else {
                    let r = st.mem.unlink_ram(fname, ext.sender_badge);
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
                        st.ops_ok = st.ops_ok.wrapping_add(1);
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
                        st.tally_err(e);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                    }
                }
            }
            VFS_OP_QUERY_STATS => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [st.ops_ok, st.rofs_rejects, st.unauth_rejects, st.inval_rejects],
                    );
                }
            }
            IPC_OP_EXIT_CLEAN if ext.sender_badge != BADGE_SUPERVISOR => {
                // `AUD-U6`: only the root supervisor may shut the VFS down.
                st.unauth_rejects = st.unauth_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
            }
            IPC_OP_EXIT_CLEAN => {
                if st.persist.mounted {
                    let _ = st.ring.flush();
                    st.persist.mounted = false;
                }
                st.ring.teardown();
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VFS-STATS] ops_ok=");
                lb.push_u64(st.ops_ok);
                lb.push_bytes(b" rofs_rej=");
                lb.push_u64(st.rofs_rejects);
                lb.push_bytes(b" unauth_rej=");
                lb.push_u64(st.unauth_rejects);
                lb.push_bytes(b" inval_rej=");
                lb.push_u64(st.inval_rejects);
                lb.push_bytes(b" persist_commits=");
                lb.push_u64(st.persist.commits);
                log_line(&lb);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [st.ops_ok, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => {
                st.inval_rejects = st.inval_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
            }
        }
    }
    sys_exit(0);
}
