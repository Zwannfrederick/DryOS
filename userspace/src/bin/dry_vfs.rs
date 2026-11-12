// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-vfs` — Isolated Ring 3 (`CPL=3`) Capability-Badged Virtual Filesystem Service (`0x0004`).
//!
//! Backends (`Milestone 2A-3`):
//! - `/boot` (`VFS_FLAG_READONLY`): Real Limine boot-module manifest + `/boot/os-release` + `/boot/README.txt`.
//!   Strictly rejects `CREATE`/`WRITE`/`RENAME`/`UNLINK` with `EROFS`.
//! - `/ram` (`VFS_FLAG_VOLATILE`): In-RAM writable filesystem (`32` static inodes, `64 KiB` `.bss`).
//!   Enforces `ADR-007` compartment badge ownership (`sender_comp == inode.owner_comp`) and explicitly
//!   advertises `VFS_FLAG_VOLATILE` (no fake disk persistence).

#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use core::mem::size_of;

use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_proto::{
    sys_ipc_recv_extra, sys_ipc_reply_extra, VfsDirEntry, VFS_DIRENTS_PER_CALL,
    VFS_FLAG_EXECUTABLE, VFS_FLAG_PRIVATE, VFS_FLAG_READONLY, VFS_FLAG_VOLATILE, VFS_MAX_NAME_LEN,
    VFS_OP_CREATE, VFS_OP_ERR, VFS_OP_LIST, VFS_OP_QUERY_STATS, VFS_OP_READ, VFS_OP_RENAME,
    VFS_OP_SEED_BOOT, VFS_OP_STAT, VFS_OP_UNLINK, VFS_OP_WRITE,
};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_exit, sys_ipc_reply_inline, sys_write_debug,
    UserIpcRecvExt, BADGE_SUPERVISOR, EEXIST, EINVAL, ENOENT, ENOSPC, EPERM, EROFS,
    IPC_OP_EXIT_CLEAN, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES,
};

const MAX_BOOT_ENTRIES: usize = 20;
const MAX_RAM_INODES: usize = 32;
const MAX_RAM_FILE_BYTES: usize = 2048;

const OS_RELEASE_TEXT: &[u8] = b"NAME=\"DRY OS\"\nVERSION=\"0.3.0-2A3\"\nID=dryos\nARCH=\"x86_64\"\nKERNEL=\"dry-kernel (Rust capability microkernel)\"\nSTORAGE=\"/boot (ro initrd) + /ram (volatile ramfs)\"\n";

const BOOT_README_TEXT: &[u8] = b"DRY OS /boot Read-Only Initrd Mount\n-----------------------------------\nContains real Limine UEFI boot modules loaded into physical memory.\nWrite, create, rename, and delete are disabled (-EROFS).\nSwitch to /ram for volatile read-write scratch files.\n";

const RAM_WELCOME_TEXT: &[u8] = b"DRY OS /ram Volatile Memory Filesystem\n--------------------------------------\nSTATUS: Read-Write (RAM-backed)\nNOTICE: Persistent NVMe/VirtIO block storage is NOT mounted yet.\nAll files in /ram will be erased on reboot or shutdown.\n";

const RAM_VAULT_SECRET_TEXT: &[u8] = b"DRY_VAULT_SEAL_SEED_COMP_0001_ONLY\n";

#[derive(Clone, Copy)]
struct BootNode {
    used: bool,
    inode: u32,
    flags: u16,
    name: [u8; 36],
    size_bytes: u32,
    static_kind: u8, // 0 = ELF module summary, 1 = os-release, 2 = README.txt
}

impl BootNode {
    const EMPTY: Self = Self {
        used: false,
        inode: 0,
        flags: 0,
        name: [0; 36],
        size_bytes: 0,
        static_kind: 0,
    };
}

#[derive(Clone, Copy)]
struct RamInode {
    used: bool,
    inode: u32,
    flags: u16,
    owner_comp: u16,
    len: usize,
    name: [u8; 36],
}

impl RamInode {
    const EMPTY: Self = Self {
        used: false,
        inode: 0,
        flags: 0,
        owner_comp: 0,
        len: 0,
        name: [0; 36],
    };
}

struct VfsState {
    next_inode: u32,
    boot_nodes: [BootNode; MAX_BOOT_ENTRIES],
    ram_inodes: [RamInode; MAX_RAM_INODES],
    ram_data: [[u8; MAX_RAM_FILE_BYTES]; MAX_RAM_INODES],
    ops_ok: u64,
    rofs_rejects: u64,
    unauth_rejects: u64,
    inval_rejects: u64,
}

struct SingleThreadVfs(UnsafeCell<VfsState>);
// SAFETY: `dry-vfs` is strictly single-threaded in CPL=3.
unsafe impl Sync for SingleThreadVfs {}

static VFS_STATE: SingleThreadVfs = SingleThreadVfs(UnsafeCell::new(VfsState {
    next_inode: 100,
    boot_nodes: [BootNode::EMPTY; MAX_BOOT_ENTRIES],
    ram_inodes: [RamInode::EMPTY; MAX_RAM_INODES],
    ram_data: [[0u8; MAX_RAM_FILE_BYTES]; MAX_RAM_INODES],
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MountKind {
    Boot,
    Ram,
}

fn is_valid_filename(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > VFS_MAX_NAME_LEN || name == b"." || name == b".." {
        return false;
    }
    name.iter().all(|&b| {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')
    })
}

fn parse_mount_dir(path: &[u8]) -> Option<MountKind> {
    match path {
        b"/boot" | b"/boot/" => Some(MountKind::Boot),
        b"/ram" | b"/ram/" => Some(MountKind::Ram),
        _ => None,
    }
}

fn parse_file_path(path: &[u8]) -> Result<(MountKind, &[u8]), isize> {
    if let Some(rest) = path.strip_prefix(b"/boot/") {
        if is_valid_filename(rest) {
            return Ok((MountKind::Boot, rest));
        }
        return Err(EINVAL);
    }
    if let Some(rest) = path.strip_prefix(b"/ram/") {
        if is_valid_filename(rest) {
            return Ok((MountKind::Ram, rest));
        }
        return Err(EINVAL);
    }
    Err(ENOENT)
}

fn set_fixed_name(dst: &mut [u8; 36], src: &[u8]) {
    *dst = [0u8; 36];
    let n = src.len().min(VFS_MAX_NAME_LEN);
    dst[..n].copy_from_slice(&src[..n]);
}

fn name_eq(fixed: &[u8; 36], target: &[u8]) -> bool {
    let len = fixed.iter().position(|&b| b == 0).unwrap_or(fixed.len());
    &fixed[..len] == target
}

impl VfsState {
    fn init_defaults(&mut self) {
        self.add_static_boot_node(b"os-release", OS_RELEASE_TEXT.len() as u32, VFS_FLAG_READONLY, 1);
        self.add_static_boot_node(b"README.txt", BOOT_README_TEXT.len() as u32, VFS_FLAG_READONLY, 2);

        // Public volatile welcome banner (`owner_comp = 0`, readable by all compartments).
        let _ = self.seed_ram_file(b"welcome.txt", 0, VFS_FLAG_VOLATILE, RAM_WELCOME_TEXT);
        // Private compartment file (`owner_comp = 0x0001`) proving cross-compartment `-EPERM` isolation!
        let _ = self.seed_ram_file(
            b"vault_key.priv",
            0x0001,
            VFS_FLAG_VOLATILE | VFS_FLAG_PRIVATE,
            RAM_VAULT_SECRET_TEXT,
        );
    }

    fn alloc_inode_num(&mut self) -> u32 {
        let id = self.next_inode;
        self.next_inode = self.next_inode.wrapping_add(1).max(100);
        id
    }

    fn add_static_boot_node(&mut self, name: &[u8], size_bytes: u32, flags: u16, static_kind: u8) {
        if !is_valid_filename(name) {
            return;
        }
        if self.boot_nodes.iter().any(|n| n.used && name_eq(&n.name, name)) {
            return;
        }
        let ino = self.alloc_inode_num();
        if let Some(slot) = self.boot_nodes.iter_mut().find(|n| !n.used) {
            slot.used = true;
            slot.inode = ino;
            slot.flags = flags;
            slot.size_bytes = size_bytes;
            slot.static_kind = static_kind;
            set_fixed_name(&mut slot.name, name);
        }
    }

    fn seed_ram_file(&mut self, name: &[u8], owner_comp: u16, flags: u16, content: &[u8]) -> Result<u32, isize> {
        let idx = self
            .ram_inodes
            .iter()
            .position(|n| !n.used)
            .ok_or(ENOSPC)?;
        let ino = self.alloc_inode_num();
        let take = content.len().min(MAX_RAM_FILE_BYTES);
        self.ram_data[idx][..take].copy_from_slice(&content[..take]);
        let node = &mut self.ram_inodes[idx];
        node.used = true;
        node.inode = ino;
        node.flags = flags | VFS_FLAG_VOLATILE;
        node.owner_comp = owner_comp;
        node.len = take;
        set_fixed_name(&mut node.name, name);
        Ok(ino)
    }

    fn render_boot_elf_summary(node: &BootNode, out: &mut [u8; 256]) -> usize {
        let mut lb = LineBuilder::new();
        let nlen = node.name.iter().position(|&b| b == 0).unwrap_or(node.name.len());
        lb.push_bytes(b"MODULE: /boot/");
        lb.push_bytes(&node.name[..nlen]);
        lb.push_bytes(b"\nFORMAT: ELF64 x86_64 Ring-3 Boot Image\nSIZE_BYTES: ");
        lb.push_u64(node.size_bytes as u64);
        lb.push_bytes(b"\nMOUNT: /boot (Limine UEFI Initrd, READ-ONLY)\nMUTATION: Denied (-EROFS)\n");
        let s = lb.as_bytes();
        let take = s.len().min(out.len());
        out[..take].copy_from_slice(&s[..take]);
        take
    }

    #[inline]
    fn check_ram_read_perm(&mut self, node: &RamInode, sender_badge: u64) -> Result<(), isize> {
        if sender_badge == BADGE_SUPERVISOR {
            return Ok(());
        }
        let sender_comp = (sender_badge & 0xffff) as u16;
        if (node.flags & VFS_FLAG_PRIVATE) != 0 && node.owner_comp != 0 && sender_comp != node.owner_comp {
            self.unauth_rejects = self.unauth_rejects.wrapping_add(1);
            return Err(EPERM);
        }
        Ok(())
    }

    #[inline]
    fn check_ram_write_perm(&mut self, node: &RamInode, sender_badge: u64) -> Result<(), isize> {
        if sender_badge == BADGE_SUPERVISOR {
            return Ok(());
        }
        let sender_comp = (sender_badge & 0xffff) as u16;
        if sender_comp == 0 || (node.owner_comp != 0 && sender_comp != node.owner_comp) {
            self.unauth_rejects = self.unauth_rejects.wrapping_add(1);
            return Err(EPERM);
        }
        Ok(())
    }
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

#[no_mangle]
pub extern "C" fn _start(srv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || srv_ep_cap == 0 {
        sys_exit(120);
    }

    // SAFETY: Single-threaded `dry-vfs` server owns `VFS_STATE` exclusively.
    let st = unsafe { &mut *VFS_STATE.0.get() };
    st.init_defaults();
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
                st.add_static_boot_node(
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
            VFS_OP_LIST => {
                let Some(mk) = parse_mount_dir(&rx[..rx_len]) else {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                };
                let start_idx = mrs[0] as usize;
                let mut entries = [VfsDirEntry::EMPTY; VFS_DIRENTS_PER_CALL];
                let mut batch_n = 0usize;
                let mut total_n = 0usize;
                let mount_flags = match mk {
                    MountKind::Boot => {
                        for n in &st.boot_nodes {
                            if !n.used {
                                continue;
                            }
                            if total_n >= start_idx && batch_n < VFS_DIRENTS_PER_CALL {
                                entries[batch_n] = VfsDirEntry {
                                    inode: n.inode,
                                    size_bytes: n.size_bytes,
                                    flags: n.flags,
                                    owner_comp: 0,
                                    name: n.name,
                                };
                                batch_n += 1;
                            }
                            total_n += 1;
                        }
                        VFS_FLAG_READONLY
                    }
                    MountKind::Ram => {
                        for n in &st.ram_inodes {
                            if !n.used {
                                continue;
                            }
                            if total_n >= start_idx && batch_n < VFS_DIRENTS_PER_CALL {
                                entries[batch_n] = VfsDirEntry {
                                    inode: n.inode,
                                    size_bytes: n.len as u32,
                                    flags: n.flags,
                                    owner_comp: n.owner_comp,
                                    name: n.name,
                                };
                                batch_n += 1;
                            }
                            total_n += 1;
                        }
                        VFS_FLAG_VOLATILE
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
                        if e == EINVAL {
                            st.inval_rejects = st.inval_rejects.wrapping_add(1);
                        }
                        reply_vfs_err(srv_ep_cap, reply_tok, e);
                        continue;
                    }
                };
                let offset = mrs[0] as usize;
                let want = (mrs[1] as usize).clamp(1, MAX_IPC_EXTRA_BYTES);
                match mk {
                    MountKind::Boot => {
                        let Some(node) = st.boot_nodes.iter().find(|n| n.used && name_eq(&n.name, fname)) else {
                            reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                            continue;
                        };
                        let mut dyn_buf = [0u8; 256];
                        let content: &[u8] = match node.static_kind {
                            1 => OS_RELEASE_TEXT,
                            2 => BOOT_README_TEXT,
                            _ => {
                                let len = VfsState::render_boot_elf_summary(node, &mut dyn_buf);
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
                                    [node.inode as u64, node.size_bytes as u64, node.flags as u64, 0],
                                );
                            }
                        } else if reply_tok != 0 {
                            let slice = if offset < content.len() {
                                let end = (offset + want).min(content.len());
                                &content[offset..end]
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
                        let Some(idx) = st.ram_inodes.iter().position(|n| n.used && name_eq(&n.name, fname)) else {
                            reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                            continue;
                        };
                        let node_copy = st.ram_inodes[idx];
                        if let Err(e) = st.check_ram_read_perm(&node_copy, ext.sender_badge) {
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
                                    [node_copy.inode as u64, node_copy.len as u64, node_copy.flags as u64, 0],
                                );
                            }
                        } else if reply_tok != 0 {
                            let slice = if offset < node_copy.len {
                                let end = (offset + want).min(node_copy.len);
                                &st.ram_data[idx][offset..end]
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
                if st.ram_inodes.iter().any(|n| n.used && name_eq(&n.name, fname)) {
                    reply_vfs_err(srv_ep_cap, reply_tok, EEXIST);
                    continue;
                }
                let req_priv = (mrs[0] as u16) & VFS_FLAG_PRIVATE;
                match st.seed_ram_file(fname, sender_comp, VFS_FLAG_VOLATILE | req_priv, b"") {
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
                let Some(idx) = st.ram_inodes.iter().position(|n| n.used && name_eq(&n.name, fname)) else {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                };
                let node_copy = st.ram_inodes[idx];
                if let Err(e) = st.check_ram_write_perm(&node_copy, ext.sender_badge) {
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                    continue;
                }
                let append = mrs[0] != 0;
                let base_pos = if append { node_copy.len } else { 0 };
                if base_pos + payload.len() > MAX_RAM_FILE_BYTES {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOSPC);
                    continue;
                }
                st.ram_data[idx][base_pos..(base_pos + payload.len())].copy_from_slice(payload);
                st.ram_inodes[idx].len = base_pos + payload.len();
                st.ops_ok = st.ops_ok.wrapping_add(1);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [payload.len() as u64, st.ram_inodes[idx].len as u64, 0, 0],
                    );
                }
            }
            VFS_OP_RENAME => {
                if rx_len < 3 {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                }
                let old_len = rx[0] as usize;
                if old_len == 0 || 1 + old_len >= rx_len {
                    st.inval_rejects = st.inval_rejects.wrapping_add(1);
                    reply_vfs_err(srv_ep_cap, reply_tok, EINVAL);
                    continue;
                }
                let old_p = &rx[1..(1 + old_len)];
                let new_p = &rx[(1 + old_len)..rx_len];
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
                let Some(idx) = st.ram_inodes.iter().position(|n| n.used && name_eq(&n.name, old_name)) else {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                };
                if st.ram_inodes.iter().any(|n| n.used && name_eq(&n.name, new_name)) {
                    reply_vfs_err(srv_ep_cap, reply_tok, EEXIST);
                    continue;
                }
                let node_copy = st.ram_inodes[idx];
                if let Err(e) = st.check_ram_write_perm(&node_copy, ext.sender_badge) {
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                    continue;
                }
                set_fixed_name(&mut st.ram_inodes[idx].name, new_name);
                st.ops_ok = st.ops_ok.wrapping_add(1);
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VFS] RENAME old=");
                lb.push_bytes(old_name);
                lb.push_bytes(b" new=");
                lb.push_bytes(new_name);
                log_line(&lb);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
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
                let Some(idx) = st.ram_inodes.iter().position(|n| n.used && name_eq(&n.name, fname)) else {
                    reply_vfs_err(srv_ep_cap, reply_tok, ENOENT);
                    continue;
                };
                let node_copy = st.ram_inodes[idx];
                if let Err(e) = st.check_ram_write_perm(&node_copy, ext.sender_badge) {
                    reply_vfs_err(srv_ep_cap, reply_tok, e);
                    continue;
                }
                st.ram_inodes[idx] = RamInode::EMPTY;
                st.ram_data[idx].fill(0);
                st.ops_ok = st.ops_ok.wrapping_add(1);
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VFS] UNLINK path=/ram/");
                lb.push_bytes(fname);
                log_line(&lb);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        srv_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
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
            IPC_OP_EXIT_CLEAN => {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-VFS-STATS] ops_ok=");
                lb.push_u64(st.ops_ok);
                lb.push_bytes(b" rofs_rej=");
                lb.push_u64(st.rofs_rejects);
                lb.push_bytes(b" unauth_rej=");
                lb.push_u64(st.unauth_rejects);
                lb.push_bytes(b" inval_rej=");
                lb.push_u64(st.inval_rejects);
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
