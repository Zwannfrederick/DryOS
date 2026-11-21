// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal `#![no_std]` POSIX USTAR reader for `/bin` backed by a read-only `Frame2M`.

use crate::vfs_mem::set_fixed_name;
use crate::vfs_proto::{VfsDirEntry, VFS_FLAG_EXECUTABLE, VFS_FLAG_READONLY, VFS_MAX_NAME_LEN};

const TAR_BLK: usize = 512;

#[inline]
fn parse_octal(bytes: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == 0) {
        i += 1;
    }
    if i >= bytes.len() {
        return Some(0);
    }
    let mut val = 0usize;
    let mut saw_digit = false;
    while i < bytes.len() && bytes[i] != 0 && bytes[i] != b' ' {
        let b = bytes[i];
        if !(b'0'..=b'7').contains(&b) {
            return None;
        }
        val = val.checked_mul(8)?.checked_add((b - b'0') as usize)?;
        saw_digit = true;
        i += 1;
    }
    if saw_digit { Some(val) } else { Some(0) }
}

fn clean_tar_name(raw_name: &[u8]) -> &[u8] {
    let end = raw_name.iter().position(|&b| b == 0).unwrap_or(raw_name.len());
    let mut s = &raw_name[..end];
    if let Some(rest) = s.strip_prefix(b"./") {
        s = rest;
    }
    while let Some(rest) = s.strip_prefix(b"/") {
        s = rest;
    }
    while let Some(rest) = s.strip_suffix(b"/") {
        s = rest;
    }
    s
}

#[inline]
fn is_ustar_header(hdr: &[u8]) -> bool {
    hdr.len() >= TAR_BLK && (&hdr[257..262] == b"ustar" || hdr[257] == 0)
}

pub fn next_entry<'a>(tar: &'a [u8], off: &mut usize) -> Option<(&'a [u8], &'a [u8])> {
    while *off + TAR_BLK <= tar.len() {
        let hdr = &tar[*off..(*off + TAR_BLK)];
        if hdr.iter().all(|&b| b == 0) {
            *off = tar.len();
            return None;
        }
        if !is_ustar_header(hdr) {
            *off = tar.len();
            return None;
        }
        let size = match parse_octal(&hdr[124..136]) {
            Some(sz) => sz,
            None => {
                *off = tar.len();
                return None;
            }
        };
        let typeflag = hdr[156];
        let name = clean_tar_name(&hdr[0..100]);
        let data_off = *off + TAR_BLK;
        let aligned = (size + (TAR_BLK - 1)) & !(TAR_BLK - 1);
        let next_off = data_off.saturating_add(aligned);
        if data_off + size > tar.len() {
            *off = tar.len();
            return None;
        }
        *off = next_off;
        if (typeflag == 0 || typeflag == b'0') && !name.is_empty() && name.len() <= VFS_MAX_NAME_LEN
        {
            return Some((name, &tar[data_off..(data_off + size)]));
        }
    }
    None
}

pub fn find_file<'a>(tar: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut off = 0usize;
    while let Some((ename, data)) = next_entry(tar, &mut off) {
        if ename == name {
            return Some(data);
        }
    }
    None
}

pub fn entry_inode(tar: &[u8], name: &[u8], base_inode: u32) -> Option<(u32, usize)> {
    let mut off = 0usize;
    let mut idx = 0u32;
    while let Some((ename, data)) = next_entry(tar, &mut off) {
        if ename == name {
            return Some((base_inode + idx, data.len()));
        }
        idx += 1;
    }
    None
}

pub fn list_entries(
    tar: &[u8],
    start_idx: usize,
    base_inode: u32,
    out: &mut [VfsDirEntry],
) -> (usize, usize) {
    let mut off = 0usize;
    let mut batch_n = 0usize;
    let mut total_n = 0usize;
    while let Some((ename, data)) = next_entry(tar, &mut off) {
        if total_n >= start_idx && batch_n < out.len() {
            let mut de = VfsDirEntry::EMPTY;
            de.inode = base_inode + (total_n as u32);
            de.size_bytes = data.len() as u32;
            de.flags = VFS_FLAG_READONLY | VFS_FLAG_EXECUTABLE;
            set_fixed_name(&mut de.name, ename);
            out[batch_n] = de;
            batch_n += 1;
        }
        total_n += 1;
    }
    (batch_n, total_n)
}

pub fn reply_bin_stat_or_read(
    tar: &[u8],
    fname: &[u8],
    offset: usize,
    want: usize,
    is_stat: bool,
    srv_ep_cap: u64,
    reply_tok: u64,
) -> Result<(), isize> {
    let (ino, total) = entry_inode(tar, fname, 5000).ok_or(crate::ENOENT)?;
    let flags = VFS_FLAG_READONLY | VFS_FLAG_EXECUTABLE;
    if reply_tok == 0 {
        return Ok(());
    }
    if is_stat {
        let _ = crate::sys_ipc_reply_inline(
            srv_ep_cap,
            reply_tok,
            crate::pack_msg_tag(crate::IPC_OP_PONG, 0, 3, 0),
            [ino as u64, total as u64, flags as u64, 0],
        );
        return Ok(());
    }
    let data = find_file(tar, fname).ok_or(crate::ENOENT)?;
    let slice = if offset < data.len() {
        &data[offset..(offset + want).min(data.len())]
    } else {
        &[]
    };
    let _ = crate::vfs_proto::sys_ipc_reply_extra(
        srv_ep_cap,
        reply_tok,
        crate::IPC_OP_PONG,
        [slice.len() as u64, total as u64, flags as u64, 0],
        slice,
    );
    Ok(())
}
