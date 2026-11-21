// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-vfs` — Isolated Ring 3 (`CPL=3`) Capability-Badged Virtual Filesystem Service (`0x0004`).
//!
//! Dispatches IPC requests across `/boot` + `/ram` (`vfs_mem::MemMounts`), `/bin` (USTAR `vfs_tar`),
//! and `/persist` (`persist_fs::PersistMount` over lock-free `IoRingClient` to `dry-blk`).

#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use core::mem::size_of;

use dry_userspace::ioring::IoRingClient;
use dry_userspace::persist_fs::{DryFsSuperblock, PersistMount, VfsCallerCtx, DRYFS_MAGIC};
use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_mem::{
    is_valid_filename, name_eq, parse_file_path, parse_list_target, set_fixed_name, MemMounts,
    MountKind, BOOT_README_TEXT, OS_RELEASE_TEXT,
};
use dry_userspace::vfs_ops::{reply_vfs_err, try_exec_mut_op, VfsStats};
use dry_userspace::vfs_proto::{
    sys_ipc_recv_extra, sys_ipc_reply_extra, VfsDirEntry, VFS_DIRENTS_PER_CALL, VFS_FLAG_DIR,
    VFS_FLAG_EXECUTABLE, VFS_FLAG_PERSISTENT, VFS_FLAG_READONLY, VFS_FLAG_VOLATILE, VFS_OP_LIST,
    VFS_OP_MOUNT_BIN_TAR, VFS_OP_QUERY_STATS, VFS_OP_READ, VFS_OP_SEED_BOOT, VFS_OP_STAT,
    VFS_OP_WRITE_CAP,
};
use dry_userspace::vfs_tar::{list_entries as list_bin_tar, reply_bin_stat_or_read};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_reply_inline,
    sys_vspace_map, sys_write_debug, UserIpcRecvExt, BADGE_SUPERVISOR, EINVAL, ENOENT, EPERM,
    EROFS, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES, VFS_OP_CORRUPT_SB0_TEST,
    VFS_OP_MOUNT_BLK, VFS_OP_SYNC, VSPACE_MAP_OP_2M_RO, VSPACE_MAP_OP_UNMAP_2M,
};

const VFS_RING_VADDR: u64 = 0x2100_0000;
const VFS_DATA_VADDR: u64 = 0x2100_1000;
const VFS_BIN_TAR_VADDR: u64 = 0x2200_0000;

struct VfsState {
    mem: MemMounts,
    ring: IoRingClient,
    persist: PersistMount,
    persist_ro: bool,
    bin_tar_cap: u64,
    bin_tar_len: usize,
    stats: VfsStats,
}

impl VfsState {
    fn bin_tar_slice(&self) -> &[u8] {
        if self.bin_tar_cap == 0 || self.bin_tar_len == 0 {
            return &[];
        }
        // SAFETY: `bin_tar_cap` is mapped RO at `VFS_BIN_TAR_VADDR` (`<= 2 MiB`).
        unsafe { core::slice::from_raw_parts(VFS_BIN_TAR_VADDR as *const u8, self.bin_tar_len) }
    }
}

struct SingleThreadVfs(UnsafeCell<VfsState>);
// SAFETY: `dry-vfs` is strictly single-threaded in CPL=3.
unsafe impl Sync for SingleThreadVfs {}

static VFS_STATE: SingleThreadVfs = SingleThreadVfs(UnsafeCell::new(VfsState {
    mem: MemMounts::new(),
    ring: IoRingClient::empty(),
    persist: PersistMount::new(),
    persist_ro: false,
    bin_tar_cap: 0,
    bin_tar_len: 0,
    stats: VfsStats {
        ops_ok: 0,
        rofs_rejects: 0,
        unauth_rejects: 0,
        inval_rejects: 0,
    },
}));

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 196];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
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

        if ext.transferred_cap != 0
            && op != VFS_OP_MOUNT_BLK
            && op != VFS_OP_MOUNT_BIN_TAR
            && op != VFS_OP_WRITE_CAP
        {
            let _ = sys_cap_revoke(ext.transferred_cap, 1);
        }

        if try_exec_mut_op(
            op,
            srv_ep_cap,
            reply_tok,
            &rx[..rx_len],
            &mrs,
            caller,
            ext.sender_badge,
            ext.transferred_cap,
            &mut st.mem,
            &mut st.persist,
            &mut st.ring,
            st.persist_ro,
            &mut st.stats,
        ) {
            continue;
        }

        match op {
            VFS_OP_SEED_BOOT => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    st.stats.unauth_rejects = st.stats.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                let name = &rx[..rx_len];
                if !is_valid_filename(name) {
                    st.stats.inval_rejects = st.stats.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                }
                st.mem.add_static_boot_node(
                    name,
                    mrs[0].min(u32::MAX as u64) as u32,
                    VFS_FLAG_READONLY | VFS_FLAG_EXECUTABLE,
                    0,
                );
                st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
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
                    st.stats.unauth_rejects = st.stats.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                st.ring.teardown();
                st.persist.mounted = false;
                st.persist_ro = false;
                let req_ro = (mrs[0] & 1) != 0;
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
                if req_ro {
                    let max_b = ((st.ring.total_sectors / 8) as u32).clamp(16, 4096);
                    let mut ok_ro = false;
                    if st.ring.read_block_4k(0, &mut st.persist.io_buf_4k).is_ok() {
                        DryFsSuperblock::decode_into(&st.persist.io_buf_4k, &mut st.persist.sb);
                        ok_ro = st.persist.sb.magic == DRYFS_MAGIC && st.persist.sb.is_valid(max_b);
                    }
                    if !ok_ro && st.ring.read_block_4k(1, &mut st.persist.io_buf_4k).is_ok() {
                        DryFsSuperblock::decode_into(&st.persist.io_buf_4k, &mut st.persist.staging_sb);
                        ok_ro = st.persist.staging_sb.magic == DRYFS_MAGIC
                            && st.persist.staging_sb.is_valid(max_b);
                    }
                    if !ok_ro {
                        st.ring.teardown();
                        st.stats.rofs_rejects = st.stats.rofs_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
                        continue;
                    }
                }
                match st.persist.mount_or_format(&mut st.ring) {
                    Ok((fresh, tx_seq)) => {
                        st.persist_ro = req_ro;
                        st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-VFS] PERSIST_MOUNT fresh=");
                        lb.push_bytes(if fresh { b"true" } else { b"false" });
                        lb.push_bytes(b" ro=");
                        lb.push_bytes(if req_ro { b"true" } else { b"false" });
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
            VFS_OP_MOUNT_BIN_TAR => {
                if ext.sender_badge != BADGE_SUPERVISOR || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    st.stats.unauth_rejects = st.stats.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                let tlen = mrs[0] as usize;
                if tlen == 0 || tlen > 2 * 1024 * 1024 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                }
                if st.bin_tar_cap != 0 {
                    let _ = sys_vspace_map(st.bin_tar_cap, VFS_BIN_TAR_VADDR, VSPACE_MAP_OP_UNMAP_2M);
                    let _ = sys_cap_revoke(st.bin_tar_cap, 1);
                    st.bin_tar_cap = 0;
                    st.bin_tar_len = 0;
                }
                if let Err(e) = sys_vspace_map(ext.transferred_cap, VFS_BIN_TAR_VADDR, VSPACE_MAP_OP_2M_RO) {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                    continue;
                }
                st.bin_tar_cap = ext.transferred_cap;
                st.bin_tar_len = tlen;
                st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [tlen as u64, 0, 0, 0],
                    );
                }
            }
            VFS_OP_SYNC => {
                if !st.persist.mounted || st.persist_ro {
                    if reply_tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            srv_ep_cap,
                            reply_tok,
                            pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                            [if st.persist.mounted { st.persist.sb.tx_seq } else { 0 }, 0, 0, 0],
                        );
                    }
                    continue;
                }
                match st.ring.flush() {
                    Ok(()) => {
                        st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
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
                    st.stats.unauth_rejects = st.stats.unauth_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
                    continue;
                }
                if !st.persist.mounted {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                }
                if st.persist_ro {
                    st.stats.rofs_rejects = st.stats.rofs_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EROFS);
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
                let start_idx = mrs[0] as usize;
                let mut entries = [VfsDirEntry::EMPTY; VFS_DIRENTS_PER_CALL];
                let list_res = if &rx[..rx_len] == b"/" {
                    let mut root_defs: [(&[u8], u16); 4] = [
                        (b"boot", VFS_FLAG_DIR | VFS_FLAG_READONLY),
                        (b"bin", VFS_FLAG_DIR | VFS_FLAG_READONLY | VFS_FLAG_EXECUTABLE),
                        (b"ram", VFS_FLAG_DIR | VFS_FLAG_VOLATILE),
                        (b"persist", VFS_FLAG_DIR | VFS_FLAG_PERSISTENT),
                    ];
                    let total_n = if st.persist.mounted { 4 } else { 3 };
                    let mut batch_n = 0usize;
                    for (i, &(rname, rflags)) in root_defs.iter().take(total_n).enumerate() {
                        if i >= start_idx && batch_n < VFS_DIRENTS_PER_CALL {
                            let mut de = VfsDirEntry::EMPTY;
                            de.inode = (i + 1) as u32;
                            de.flags = rflags;
                            set_fixed_name(&mut de.name, rname);
                            entries[batch_n] = de;
                            batch_n += 1;
                        }
                    }
                    let _ = &mut root_defs;
                    Ok((batch_n, total_n, VFS_FLAG_READONLY))
                } else {
                    let Some((mk, sub)) = parse_list_target(&rx[..rx_len]) else {
                        st.stats.inval_rejects = st.stats.inval_rejects.wrapping_add(1);
                        reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                        continue;
                    };
                    if mk == MountKind::Persist && !st.persist.mounted {
                        reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                        continue;
                    }
                    match mk {
                        MountKind::Boot => {
                            let (b, t) = st.mem.list_boot(start_idx, &mut entries);
                            Ok((b, t, VFS_FLAG_READONLY))
                        }
                        MountKind::Bin => {
                            let (b, t) = list_bin_tar(st.bin_tar_slice(), start_idx, 5000, &mut entries);
                            Ok((b, t, VFS_FLAG_READONLY | VFS_FLAG_EXECUTABLE))
                        }
                        MountKind::Ram => st
                            .mem
                            .list_ram_sub(sub, start_idx, &mut entries)
                            .map(|(b, t)| (b, t, VFS_FLAG_VOLATILE)),
                        MountKind::Persist => st
                            .persist
                            .list_sub(sub, start_idx, &mut entries)
                            .map(|(b, t)| {
                                (
                                    b,
                                    t,
                                    VFS_FLAG_PERSISTENT | if st.persist_ro { VFS_FLAG_READONLY } else { 0 },
                                )
                            }),
                    }
                };
                let (batch_n, total_n, mount_flags) = match list_res {
                    Ok(v) => v,
                    Err(e) => {
                        st.stats.tally_err(e);
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                        continue;
                    }
                };
                st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
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
                        st.stats.tally_err(e);
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
                        st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
                        if op == VFS_OP_STAT {
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    srv_ep_cap,
                                    reply_tok,
                                    pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                    [node.inode as u64, node.size_bytes as u64, node.flags as u64, 0],
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
                    MountKind::Bin => {
                        match reply_bin_stat_or_read(
                            st.bin_tar_slice(),
                            fname,
                            offset,
                            want,
                            op == VFS_OP_STAT,
                            srv_ep_cap,
                            reply_tok,
                        ) {
                            Ok(()) => st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1),
                            Err(e) => {
                                st.stats.tally_err(e);
                                reply_vfs_err(srv_ep_cap, reply_tok, e);
                            }
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
                        if let Err(e) = MemMounts::check_ram_read_perm(&node_copy, ext.sender_badge) {
                            st.stats.tally_err(e);
                            reply_vfs_err(srv_ep_cap, reply_tok, e);
                            continue;
                        }
                        if op == VFS_OP_READ && (node_copy.flags & VFS_FLAG_DIR) != 0 {
                            st.stats.inval_rejects = st.stats.inval_rejects.wrapping_add(1);
                            reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                            continue;
                        }
                        st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
                        if op == VFS_OP_STAT {
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    srv_ep_cap,
                                    reply_tok,
                                    pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                    [node_copy.inode as u64, node_copy.len as u64, node_copy.flags as u64, 0],
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
                                [slice.len() as u64, node_copy.len as u64, node_copy.flags as u64, 0],
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
                        let dst = if op == VFS_OP_STAT { &mut out_buf[..0] } else { &mut out_buf[..want] };
                        match st.persist.read_slice(&mut st.ring, fname, caller, offset, dst) {
                            Ok((got, total, flags, ino)) => {
                                st.stats.ops_ok = st.stats.ops_ok.wrapping_add(1);
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
                                st.stats.tally_err(e);
                                reply_vfs_err(srv_ep_cap, reply_tok, e);
                            }
                        }
                    }
                }
            }
            VFS_OP_QUERY_STATS => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [
                            st.stats.ops_ok,
                            st.stats.rofs_rejects,
                            st.stats.unauth_rejects,
                            st.stats.inval_rejects,
                        ],
                    );
                }
            }
            IPC_OP_EXIT_CLEAN if ext.sender_badge != BADGE_SUPERVISOR => {
                // `AUD-U6`: only the root supervisor may shut the VFS down.
                st.stats.unauth_rejects = st.stats.unauth_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EPERM);
            }
            IPC_OP_EXIT_CLEAN => {
                if st.bin_tar_cap != 0 {
                    let _ = sys_vspace_map(st.bin_tar_cap, VFS_BIN_TAR_VADDR, VSPACE_MAP_OP_UNMAP_2M);
                    let _ = sys_cap_revoke(st.bin_tar_cap, 1);
                    st.bin_tar_cap = 0;
                    st.bin_tar_len = 0;
                }
                if st.persist.mounted {
                    let _ = st.ring.flush();
                    st.persist.mounted = false;
                }
                st.ring.teardown();
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VFS-STATS] ops_ok=");
                lb.push_u64(st.stats.ops_ok);
                lb.push_bytes(b" rofs_rej=");
                lb.push_u64(st.stats.rofs_rejects);
                lb.push_bytes(b" unauth_rej=");
                lb.push_u64(st.stats.unauth_rejects);
                lb.push_bytes(b" inval_rej=");
                lb.push_u64(st.stats.inval_rejects);
                lb.push_bytes(b" persist_commits=");
                lb.push_u64(st.persist.commits);
                log_line(&lb);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [st.stats.ops_ok, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => {
                st.stats.inval_rejects = st.stats.inval_rejects.wrapping_add(1);
                reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
            }
        }
    }
    sys_exit(0);
}
