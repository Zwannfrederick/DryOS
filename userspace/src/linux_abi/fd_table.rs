// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal POSIX File Descriptor Table backed by `dry-vfs` IPC and debug stdout capture.

use crate::linux_abi::LX_XFER_VADDR;
use crate::vfs_mem::normalize_path;
use crate::vfs_proto::{vfs_create_file, vfs_read_file, vfs_stat, vfs_write_all_at, VFS_FLAG_DIR};
use crate::{sys_write_debug, EEXIST};

pub const MAX_FDS: usize = 16;
pub const MAX_STDOUT_CAPTURE: usize = 224;

const O_ACCMODE: u32 = 3;
const O_WRONLY: u32 = 1;
const O_RDWR: u32 = 2;
const O_CREAT: u32 = 64;
const O_TRUNC: u32 = 512;
const O_APPEND: u32 = 1024;

const LX_EBADF: i64 = -9;
const LX_EACCES: i64 = -13;
const LX_EEXIST: i64 = -17;
const LX_EISDIR: i64 = -21;
const LX_EINVAL: i64 = -22;
const LX_EMFILE: i64 = -24;
const LX_ENOENT: i64 = -2;
const LX_EROFS: i64 = -30;

#[derive(Clone, Copy)]
pub struct FdEntry {
    pub open: bool,
    pub is_stdio: bool,
    pub writable: bool,
    pub append: bool,
    pub wrote_once: bool,
    pub is_dir: bool,
    pub inode: u32,
    pub offset: usize,
    pub size: usize,
    pub path_len: usize,
    pub path: [u8; 96],
}

impl FdEntry {
    const EMPTY: Self = Self {
        open: false,
        is_stdio: false,
        writable: false,
        append: false,
        wrote_once: false,
        is_dir: false,
        inode: 0,
        offset: 0,
        size: 0,
        path_len: 0,
        path: [0; 96],
    };
}

pub struct FdTable {
    entries: [FdEntry; MAX_FDS],
    pub stdout_buf: [u8; MAX_STDOUT_CAPTURE],
    pub stdout_len: usize,
}

impl FdTable {
    pub const fn new() -> Self {
        let mut t = Self {
            entries: [FdEntry::EMPTY; MAX_FDS],
            stdout_buf: [0; MAX_STDOUT_CAPTURE],
            stdout_len: 0,
        };
        t.entries[0].open = true;
        t.entries[0].is_stdio = true;
        t.entries[1].open = true;
        t.entries[1].is_stdio = true;
        t.entries[1].writable = true;
        t.entries[2].open = true;
        t.entries[2].is_stdio = true;
        t.entries[2].writable = true;
        t
    }

    pub fn open_path(&mut self, vfs_cap: u64, raw_path: &[u8], flags: u32) -> i64 {
        let Some(slot_idx) = (3..MAX_FDS).find(|&i| !self.entries[i].open) else {
            return LX_EMFILE;
        };
        let mut norm = [0u8; 96];
        let Some(nlen) = normalize_path(b"/", raw_path, &mut norm) else {
            return LX_EINVAL;
        };
        let path = &norm[..nlen];
        let acc = flags & O_ACCMODE;
        let want_write = acc == O_WRONLY || acc == O_RDWR;
        let writable_mount = path.starts_with(b"/ram/") || path.starts_with(b"/persist/");
        if (want_write || (flags & (O_CREAT | O_TRUNC)) != 0) && !writable_mount {
            return LX_EROFS;
        }
        if vfs_cap == 0 {
            return LX_ENOENT;
        }

        let stat_res = vfs_stat(vfs_cap, path);
        let (inode, size, vflags) = match stat_res {
            Ok((ino, sz, fl)) => {
                if (flags & O_CREAT) != 0 && (flags & 128) != 0 {
                    return LX_EEXIST;
                }
                (ino, sz, fl)
            }
            Err(_) if (flags & O_CREAT) != 0 => {
                let Ok(ino) = vfs_create_file(vfs_cap, path, 0) else {
                    return LX_EACCES;
                };
                (ino, 0, 0)
            }
            Err(_) => return LX_ENOENT,
        };

        let is_dir = (vflags & VFS_FLAG_DIR) != 0;
        if want_write && is_dir {
            return LX_EISDIR;
        }
        if want_write && (flags & O_TRUNC) != 0 {
            let _ = vfs_write_all_at(vfs_cap, path, &[], false, LX_XFER_VADDR);
        }

        let e = &mut self.entries[slot_idx];
        *e = FdEntry::EMPTY;
        e.open = true;
        e.writable = want_write;
        e.append = (flags & O_APPEND) != 0;
        e.wrote_once = false;
        e.is_dir = is_dir;
        e.inode = inode;
        e.size = if (flags & O_TRUNC) != 0 { 0 } else { size };
        e.offset = if e.append { e.size } else { 0 };
        e.path_len = nlen;
        e.path[..nlen].copy_from_slice(path);
        slot_idx as i64
    }

    pub fn close_fd(&mut self, fd: usize) -> i64 {
        if fd >= MAX_FDS || !self.entries[fd].open {
            return LX_EBADF;
        }
        if fd >= 3 {
            self.entries[fd] = FdEntry::EMPTY;
        }
        0
    }

    pub fn read_fd(&mut self, vfs_cap: u64, fd: usize, out: &mut [u8]) -> i64 {
        if fd >= MAX_FDS || !self.entries[fd].open {
            return LX_EBADF;
        }
        let e = &mut self.entries[fd];
        if e.is_stdio || e.is_dir || vfs_cap == 0 {
            return if e.is_dir { LX_EISDIR } else { 0 };
        }
        let path = &e.path[..e.path_len];
        match vfs_read_file(vfs_cap, path, e.offset, out) {
            Ok((got, total)) => {
                e.size = total;
                e.offset += got;
                got as i64
            }
            Err(_) => LX_ENOENT,
        }
    }

    pub fn write_fd(&mut self, vfs_cap: u64, fd: usize, data: &[u8]) -> i64 {
        if fd >= MAX_FDS || !self.entries[fd].open {
            return LX_EBADF;
        }
        if !self.entries[fd].writable {
            return LX_EBADF;
        }
        if self.entries[fd].is_stdio {
            for &b in data {
                if self.stdout_len < MAX_STDOUT_CAPTURE {
                    self.stdout_buf[self.stdout_len] = if (0x20..=0x7e).contains(&b) { b } else { b' ' };
                    self.stdout_len += 1;
                }
            }
            let _ = sys_write_debug(data);
            return data.len() as i64;
        }
        if vfs_cap == 0 {
            return LX_EROFS;
        }
        let e = &mut self.entries[fd];
        let append = e.append || e.wrote_once;
        let path = &e.path[..e.path_len];
        match vfs_write_all_at(vfs_cap, path, data, append, LX_XFER_VADDR) {
            Ok(w) => {
                e.wrote_once = true;
                e.offset += w;
                if e.offset > e.size {
                    e.size = e.offset;
                }
                w as i64
            }
            Err(err) if err == EEXIST => LX_EEXIST,
            Err(_) => LX_EROFS,
        }
    }

    pub fn lseek_fd(&mut self, fd: usize, off: i64, whence: u64) -> i64 {
        if fd >= MAX_FDS || !self.entries[fd].open || self.entries[fd].is_stdio {
            return LX_EBADF;
        }
        let e = &mut self.entries[fd];
        let base = match whence {
            0 => 0i64,
            1 => e.offset as i64,
            2 => e.size as i64,
            _ => return LX_EINVAL,
        };
        let next = base.saturating_add(off);
        if next < 0 {
            return LX_EINVAL;
        }
        e.offset = next as usize;
        next
    }

    pub fn fill_stat(&self, fd: usize, st_buf: &mut [u8; 144]) -> i64 {
        if fd >= MAX_FDS || !self.entries[fd].open {
            return LX_EBADF;
        }
        st_buf.fill(0);
        let e = &self.entries[fd];
        let mode: u32 = if e.is_stdio {
            0o020666
        } else if e.is_dir {
            0o040755
        } else {
            0o100644
        };
        st_buf[8..16].copy_from_slice(&(u64::from(e.inode.max(1))).to_le_bytes());
        st_buf[16..24].copy_from_slice(&1u64.to_le_bytes());
        st_buf[24..28].copy_from_slice(&mode.to_le_bytes());
        st_buf[48..56].copy_from_slice(&(e.size as i64).to_le_bytes());
        st_buf[56..64].copy_from_slice(&4096i64.to_le_bytes());
        st_buf[64..72].copy_from_slice(&(((e.size as i64) + 511) / 512).to_le_bytes());
        0
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}
