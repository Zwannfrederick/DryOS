// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! In-Memory VFS Mount Models (`/boot` Read-Only Initrd & `/ram` Volatile Scratch) + Path Parsers
//! (`Milestone 2A-3` & `2B`).
//!
//! Keeps `dry-vfs` IPC server dispatch cleanly separated from the `/boot` + `/ram` inode models.

use crate::abi_ext::ENOTEMPTY;
use crate::shell::LineBuilder;
use crate::vfs_proto::{
    VfsDirEntry, VFS_DIRENTS_PER_CALL, VFS_FLAG_DIR, VFS_FLAG_PRIVATE, VFS_FLAG_READONLY,
    VFS_FLAG_VOLATILE, VFS_MAX_NAME_LEN,
};
use crate::{BADGE_SUPERVISOR, EEXIST, EINVAL, ENOENT, ENOSPC, EPERM};

pub const MAX_BOOT_ENTRIES: usize = 36;
pub const MAX_RAM_INODES: usize = 40;
pub const MAX_RAM_FILE_BYTES: usize = 16384;

pub const OS_RELEASE_TEXT: &[u8] = b"NAME=\"DRY OS\"\nVERSION=\"0.4.0-2B\"\nID=dryos\nARCH=\"x86_64\"\nKERNEL=\"dry-kernel (Rust capability microkernel)\"\nSTORAGE=\"/boot (ro initrd) + /ram (volatile) + /persist (DRYFS_v1 CoW)\"\n";

pub const BOOT_README_TEXT: &[u8] = b"DRY OS /boot Read-Only Initrd Mount\n-----------------------------------\nContains real Limine UEFI boot modules loaded into physical memory.\nWrite, create, rename, and delete are disabled (-EROFS).\nSwitch to /ram (volatile) or /persist (VirtIO-Blk CoW) for writes.\n";

pub const RAM_WELCOME_TEXT: &[u8] = b"DRY OS /ram Volatile Memory Filesystem\n--------------------------------------\nSTATUS: Read-Write (RAM-backed)\nNOTICE: All files in /ram will be erased on reboot or shutdown.\nUse /persist when a VirtIO-Blk disk is attached for durable storage.\n";

pub const RAM_VAULT_SECRET_TEXT: &[u8] = b"DRY_VAULT_SEAL_SEED_COMP_0001_ONLY\n";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    Boot,
    Bin,
    Ram,
    Persist,
}

#[derive(Clone, Copy)]
pub struct BootNode {
    pub used: bool,
    pub inode: u32,
    pub flags: u16,
    pub name: [u8; 36],
    pub size_bytes: u32,
    pub static_kind: u8,
}

impl BootNode {
    pub const EMPTY: Self = Self {
        used: false,
        inode: 0,
        flags: 0,
        name: [0; 36],
        size_bytes: 0,
        static_kind: 0,
    };
}

#[derive(Clone, Copy)]
pub struct RamInode {
    pub used: bool,
    pub inode: u32,
    pub flags: u16,
    pub owner_comp: u16,
    pub len: usize,
    pub name: [u8; 36],
}

impl RamInode {
    pub const EMPTY: Self = Self {
        used: false,
        inode: 0,
        flags: 0,
        owner_comp: 0,
        len: 0,
        name: [0; 36],
    };
}

pub fn is_valid_filename(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > VFS_MAX_NAME_LEN || name == b"." || name == b".." {
        return false;
    }
    name.iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

pub fn is_valid_subpath(sub: &[u8]) -> bool {
    if sub.is_empty()
        || sub.len() > VFS_MAX_NAME_LEN
        || sub.starts_with(b"/")
        || sub.ends_with(b"/")
    {
        return false;
    }
    for seg in sub.split(|&b| b == b'/') {
        if !is_valid_filename(seg) {
            return false;
        }
    }
    true
}

#[inline]
pub fn parent_dir_of(subpath: &[u8]) -> Option<&[u8]> {
    subpath.iter().rposition(|&b| b == b'/').map(|pos| &subpath[..pos])
}

#[inline]
pub fn list_direct_child<'a>(dir_prefix: &[u8], entry_name: &'a [u8]) -> Option<&'a [u8]> {
    if dir_prefix.is_empty() {
        return if !entry_name.contains(&b'/') {
            Some(entry_name)
        } else {
            None
        };
    }
    if entry_name.len() > dir_prefix.len() + 1
        && &entry_name[..dir_prefix.len()] == dir_prefix
        && entry_name[dir_prefix.len()] == b'/'
    {
        let rest = &entry_name[(dir_prefix.len() + 1)..];
        if !rest.is_empty() && !rest.contains(&b'/') {
            return Some(rest);
        }
    }
    None
}

#[inline]
pub fn is_child_of(dir_prefix: &[u8], entry_name: &[u8]) -> bool {
    entry_name.len() > dir_prefix.len()
        && &entry_name[..dir_prefix.len()] == dir_prefix
        && entry_name[dir_prefix.len()] == b'/'
}

pub fn normalize_path(cwd: &[u8], raw: &[u8], out: &mut [u8; 96]) -> Option<usize> {
    if raw.is_empty() {
        return None;
    }
    let mut segs: [&[u8]; 12] = [b""; 12];
    let mut depth = 0usize;
    if !raw.starts_with(b"/") {
        for seg in cwd.split(|&b| b == b'/') {
            if seg.is_empty() || seg == b"." {
                continue;
            }
            if depth >= segs.len() {
                return None;
            }
            segs[depth] = seg;
            depth += 1;
        }
    }
    for seg in raw.split(|&b| b == b'/') {
        if seg.is_empty() || seg == b"." {
            continue;
        }
        if seg == b".." {
            depth = depth.saturating_sub(1);
            continue;
        }
        if !is_valid_filename(seg) || depth >= segs.len() {
            return None;
        }
        segs[depth] = seg;
        depth += 1;
    }
    if depth == 0 {
        out[0] = b'/';
        return Some(1);
    }
    let mut pos = 0usize;
    for seg in &segs[..depth] {
        if pos + 1 + seg.len() > out.len() {
            return None;
        }
        out[pos] = b'/';
        pos += 1;
        out[pos..(pos + seg.len())].copy_from_slice(seg);
        pos += seg.len();
    }
    Some(pos)
}

pub fn parse_mount_dir(path: &[u8]) -> Option<MountKind> {
    match path {
        b"/boot" | b"/boot/" => Some(MountKind::Boot),
        b"/bin" | b"/bin/" => Some(MountKind::Bin),
        b"/ram" | b"/ram/" => Some(MountKind::Ram),
        b"/persist" | b"/persist/" => Some(MountKind::Persist),
        _ => None,
    }
}

pub fn parse_list_target(path: &[u8]) -> Option<(MountKind, &[u8])> {
    if let Some(mk) = parse_mount_dir(path) {
        return Some((mk, b""));
    }
    let trimmed = path.strip_suffix(b"/").unwrap_or(path);
    if let Some(sub) = trimmed.strip_prefix(b"/ram/") {
        return is_valid_subpath(sub).then_some((MountKind::Ram, sub));
    }
    if let Some(sub) = trimmed.strip_prefix(b"/persist/") {
        return is_valid_subpath(sub).then_some((MountKind::Persist, sub));
    }
    None
}

pub fn parse_file_path(path: &[u8]) -> Result<(MountKind, &[u8]), isize> {
    if let Some(rest) = path.strip_prefix(b"/boot/") {
        return if is_valid_filename(rest) {
            Ok((MountKind::Boot, rest))
        } else {
            Err(EINVAL)
        };
    }
    if let Some(rest) = path.strip_prefix(b"/bin/") {
        return if is_valid_filename(rest) {
            Ok((MountKind::Bin, rest))
        } else {
            Err(EINVAL)
        };
    }
    if let Some(rest) = path.strip_prefix(b"/ram/") {
        return if is_valid_subpath(rest) {
            Ok((MountKind::Ram, rest))
        } else {
            Err(EINVAL)
        };
    }
    if let Some(rest) = path.strip_prefix(b"/persist/") {
        return if is_valid_subpath(rest) {
            Ok((MountKind::Persist, rest))
        } else {
            Err(EINVAL)
        };
    }
    Err(ENOENT)
}

pub fn parse_two_paths(rx: &[u8]) -> Result<(&[u8], &[u8]), isize> {
    if rx.len() < 3 {
        return Err(EINVAL);
    }
    let first_len = rx[0] as usize;
    if first_len == 0 || 1 + first_len >= rx.len() {
        return Err(EINVAL);
    }
    Ok((&rx[1..(1 + first_len)], &rx[(1 + first_len)..]))
}

pub fn set_fixed_name(dst: &mut [u8; 36], src: &[u8]) {
    *dst = [0u8; 36];
    let n = src.len().min(VFS_MAX_NAME_LEN);
    dst[..n].copy_from_slice(&src[..n]);
}

pub fn name_eq(fixed: &[u8; 36], target: &[u8]) -> bool {
    let len = fixed.iter().position(|&b| b == 0).unwrap_or(fixed.len());
    &fixed[..len] == target
}

pub struct MemMounts {
    pub next_inode: u32,
    pub boot_nodes: [BootNode; MAX_BOOT_ENTRIES],
    pub ram_inodes: [RamInode; MAX_RAM_INODES],
    pub ram_data: [[u8; MAX_RAM_FILE_BYTES]; MAX_RAM_INODES],
}

impl MemMounts {
    pub const fn new() -> Self {
        Self {
            next_inode: 100,
            boot_nodes: [BootNode::EMPTY; MAX_BOOT_ENTRIES],
            ram_inodes: [RamInode::EMPTY; MAX_RAM_INODES],
            ram_data: [[0u8; MAX_RAM_FILE_BYTES]; MAX_RAM_INODES],
        }
    }

    pub fn init_defaults(&mut self) {
        self.add_static_boot_node(
            b"os-release",
            OS_RELEASE_TEXT.len() as u32,
            VFS_FLAG_READONLY,
            1,
        );
        self.add_static_boot_node(
            b"README.txt",
            BOOT_README_TEXT.len() as u32,
            VFS_FLAG_READONLY,
            2,
        );
        let _ = self.seed_ram_file(b"welcome.txt", 0, VFS_FLAG_VOLATILE, RAM_WELCOME_TEXT);
        let _ = self.seed_ram_file(
            b"vault_key.priv",
            0x0001,
            VFS_FLAG_VOLATILE | VFS_FLAG_PRIVATE,
            RAM_VAULT_SECRET_TEXT,
        );
    }

    pub fn alloc_inode_num(&mut self) -> u32 {
        let id = self.next_inode;
        self.next_inode = self.next_inode.wrapping_add(1).max(100);
        id
    }

    pub fn add_static_boot_node(&mut self, name: &[u8], size_bytes: u32, flags: u16, static_kind: u8) {
        if !is_valid_filename(name)
            || self
                .boot_nodes
                .iter()
                .any(|n| n.used && name_eq(&n.name, name))
        {
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

    pub fn seed_ram_file(
        &mut self,
        name: &[u8],
        owner_comp: u16,
        flags: u16,
        content: &[u8],
    ) -> Result<u32, isize> {
        if let Some(parent) = parent_dir_of(name) {
            if !self.has_ram_dir(parent) {
                return Err(ENOENT);
            }
        }
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

    pub fn list_boot(&self, start_idx: usize, out: &mut [VfsDirEntry; VFS_DIRENTS_PER_CALL]) -> (usize, usize) {
        let mut batch_n = 0usize;
        let mut total_n = 0usize;
        for n in &self.boot_nodes {
            if !n.used {
                continue;
            }
            if total_n >= start_idx && batch_n < VFS_DIRENTS_PER_CALL {
                out[batch_n] = VfsDirEntry {
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
        (batch_n, total_n)
    }

    pub fn list_ram(&self, start_idx: usize, out: &mut [VfsDirEntry; VFS_DIRENTS_PER_CALL]) -> (usize, usize) {
        self.list_ram_sub(b"", start_idx, out).unwrap_or((0, 0))
    }

    pub fn list_ram_sub(
        &self,
        dir_prefix: &[u8],
        start_idx: usize,
        out: &mut [VfsDirEntry; VFS_DIRENTS_PER_CALL],
    ) -> Result<(usize, usize), isize> {
        if !dir_prefix.is_empty() {
            let dir_node = self
                .ram_inodes
                .iter()
                .find(|n| n.used && name_eq(&n.name, dir_prefix))
                .ok_or(ENOENT)?;
            if (dir_node.flags & VFS_FLAG_DIR) == 0 {
                return Err(EINVAL);
            }
        }
        let mut batch_n = 0usize;
        let mut total_n = 0usize;
        for n in &self.ram_inodes {
            if !n.used {
                continue;
            }
            let nlen = n.name.iter().position(|&b| b == 0).unwrap_or(n.name.len());
            let Some(child_name) = list_direct_child(dir_prefix, &n.name[..nlen]) else {
                continue;
            };
            if total_n >= start_idx && batch_n < VFS_DIRENTS_PER_CALL {
                let mut de = VfsDirEntry {
                    inode: n.inode,
                    size_bytes: n.len as u32,
                    flags: n.flags,
                    owner_comp: n.owner_comp,
                    name: [0; 36],
                };
                set_fixed_name(&mut de.name, child_name);
                out[batch_n] = de;
                batch_n += 1;
            }
            total_n += 1;
        }
        Ok((batch_n, total_n))
    }

    pub fn has_ram_dir(&self, dir_path: &[u8]) -> bool {
        self.ram_inodes
            .iter()
            .any(|n| n.used && name_eq(&n.name, dir_path) && (n.flags & VFS_FLAG_DIR) != 0)
    }

    pub fn mkdir_ram(&mut self, path: &[u8], owner_comp: u16) -> Result<u32, isize> {
        if self.ram_inodes.iter().any(|n| n.used && name_eq(&n.name, path)) {
            return Err(EEXIST);
        }
        if let Some(parent) = parent_dir_of(path) {
            if !self.has_ram_dir(parent) {
                return Err(ENOENT);
            }
        }
        self.seed_ram_file(path, owner_comp, VFS_FLAG_VOLATILE | VFS_FLAG_DIR, b"")
    }

    pub fn rmdir_ram(&mut self, path: &[u8], sender_badge: u64) -> Result<(), isize> {
        let idx = self
            .ram_inodes
            .iter()
            .position(|n| n.used && name_eq(&n.name, path))
            .ok_or(ENOENT)?;
        let node_copy = self.ram_inodes[idx];
        if (node_copy.flags & VFS_FLAG_DIR) == 0 {
            return Err(EINVAL);
        }
        Self::check_ram_write_perm(&node_copy, sender_badge)?;
        for other in &self.ram_inodes {
            if !other.used {
                continue;
            }
            let len = other.name.iter().position(|&b| b == 0).unwrap_or(other.name.len());
            if is_child_of(path, &other.name[..len]) {
                return Err(ENOTEMPTY);
            }
        }
        self.ram_inodes[idx] = RamInode::EMPTY;
        self.ram_data[idx].fill(0);
        Ok(())
    }

    pub fn render_boot_elf_summary(node: &BootNode, out: &mut [u8; 256]) -> usize {
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

    pub fn check_ram_read_perm(node: &RamInode, sender_badge: u64) -> Result<(), isize> {
        if sender_badge == BADGE_SUPERVISOR {
            return Ok(());
        }
        let sender_comp = (sender_badge & 0xffff) as u16;
        if (node.flags & VFS_FLAG_PRIVATE) != 0
            && node.owner_comp != 0
            && sender_comp != node.owner_comp
        {
            return Err(EPERM);
        }
        Ok(())
    }

    pub fn check_ram_write_perm(node: &RamInode, sender_badge: u64) -> Result<(), isize> {
        if sender_badge == BADGE_SUPERVISOR {
            return Ok(());
        }
        let sender_comp = (sender_badge & 0xffff) as u16;
        if sender_comp == 0 || (node.owner_comp != 0 && sender_comp != node.owner_comp) {
            return Err(EPERM);
        }
        Ok(())
    }

    pub fn rename_ram(&mut self, old_name: &[u8], new_name: &[u8], sender_badge: u64) -> Result<(), isize> {
        let idx = self
            .ram_inodes
            .iter()
            .position(|n| n.used && name_eq(&n.name, old_name))
            .ok_or(ENOENT)?;
        if (self.ram_inodes[idx].flags & VFS_FLAG_DIR) != 0 {
            return Err(EINVAL);
        }
        if self.ram_inodes.iter().any(|n| n.used && name_eq(&n.name, new_name)) {
            return Err(EEXIST);
        }
        if let Some(parent) = parent_dir_of(new_name) {
            if !self.has_ram_dir(parent) {
                return Err(ENOENT);
            }
        }
        let node_copy = self.ram_inodes[idx];
        Self::check_ram_write_perm(&node_copy, sender_badge)?;
        set_fixed_name(&mut self.ram_inodes[idx].name, new_name);
        Ok(())
    }

    pub fn unlink_ram(&mut self, fname: &[u8], sender_badge: u64) -> Result<(), isize> {
        let idx = self
            .ram_inodes
            .iter()
            .position(|n| n.used && name_eq(&n.name, fname))
            .ok_or(ENOENT)?;
        let node_copy = self.ram_inodes[idx];
        if (node_copy.flags & VFS_FLAG_DIR) != 0 {
            return Err(EINVAL);
        }
        Self::check_ram_write_perm(&node_copy, sender_badge)?;
        self.ram_inodes[idx] = RamInode::EMPTY;
        self.ram_data[idx].fill(0);
        Ok(())
    }
}

impl Default for MemMounts {
    fn default() -> Self {
        Self::new()
    }
}
