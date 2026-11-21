// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring-3 Linux ABI Compatibility Layer (`dry-linux-abi`, Milestone 3 Part B).
//!
//! Mediates intercepted x86-64 `SYSCALL` requests from isolated Linux ELF64 guests
//! (`SecDomain::Userspace`, `0` capabilities) over `Endpoint` IPC using kernel-validated
//! `MEM_READ`, `MEM_WRITE`, `MAP_ANON`, `RET`, and `EXIT` replies.

pub mod fd_table;
pub mod syscalls;

use crate::abi_ext::{
    SYSCALL_REDIRECT_LABEL, SYSCALL_REPLY_EXIT, SYSCALL_REPLY_MAP_ANON, SYSCALL_REPLY_MEM_READ,
    SYSCALL_REPLY_MEM_WRITE, SYSCALL_REPLY_RET,
};
use crate::vfs_proto::{sys_ipc_recv_extra, sys_ipc_reply_extra};
use crate::{msg_tag_label, pack_msg_tag, sys_cap_revoke, sys_ipc_reply_inline, UserIpcRecvExt, EINVAL, EIO, MAX_IPC_EXTRA_BYTES};

pub const COMP_ID_LINUX_ABI: u16 = 0x0191;
pub const COMP_ID_LINUX_GUEST: u16 = 0x0192;

const LX_XFER_VADDR: u64 = 0x2600_0000;

fn subop_roundtrip(
    redirect_ep: u64,
    reply_tok: &mut u64,
    reply_op: u32,
    mrs: [u64; 4],
    extra: &[u8],
    rx_buf: &mut [u8; MAX_IPC_EXTRA_BYTES],
) -> Result<(usize, [u64; 4]), isize> {
    if extra.is_empty() {
        sys_ipc_reply_inline(
            redirect_ep,
            *reply_tok,
            pack_msg_tag(reply_op, 0, 4, 0),
            mrs,
        )?;
    } else {
        sys_ipc_reply_extra(redirect_ep, *reply_tok, reply_op, mrs, extra)?;
    }
    let mut ext = UserIpcRecvExt::default();
    let (tag, rx_len, rmrs) = sys_ipc_recv_extra(redirect_ep, 0, 0, rx_buf, &mut ext)?;
    if ext.transferred_cap != 0 {
        let _ = sys_cap_revoke(ext.transferred_cap, 1);
    }
    *reply_tok = ext.reply_token;
    if msg_tag_label(tag) != SYSCALL_REDIRECT_LABEL || rmrs[3] != 1 {
        return Err(EIO);
    }
    Ok((rx_len, rmrs))
}

pub fn guest_mem_read(
    redirect_ep: u64,
    reply_tok: &mut u64,
    mut guest_va: u64,
    dst: &mut [u8],
) -> Result<(), isize> {
    let mut off = 0usize;
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    while off < dst.len() {
        let chunk = (dst.len() - off).min(MAX_IPC_EXTRA_BYTES);
        let (got, rmrs) = subop_roundtrip(
            redirect_ep,
            reply_tok,
            SYSCALL_REPLY_MEM_READ,
            [guest_va, chunk as u64, 0, 0],
            &[],
            &mut rx,
        )?;
        let st = rmrs[0] as isize;
        if st < 0 {
            return Err(st);
        }
        if got < chunk {
            return Err(EIO);
        }
        dst[off..(off + chunk)].copy_from_slice(&rx[..chunk]);
        off += chunk;
        guest_va = guest_va.wrapping_add(chunk as u64);
    }
    Ok(())
}

pub fn guest_mem_read_cstr(
    redirect_ep: u64,
    reply_tok: &mut u64,
    guest_va: u64,
    out: &mut [u8; 96],
) -> Result<usize, isize> {
    let mut tmp = [0u8; 96];
    let mut off = 0usize;
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    while off < tmp.len() {
        let chunk = (tmp.len() - off).min(64);
        let (got, rmrs) = subop_roundtrip(
            redirect_ep,
            reply_tok,
            SYSCALL_REPLY_MEM_READ,
            [guest_va.wrapping_add(off as u64), chunk as u64, 0, 0],
            &[],
            &mut rx,
        )?;
        let st = rmrs[0] as isize;
        if st < 0 {
            if off == 0 {
                return Err(st);
            }
            break;
        }
        let n = got.min(chunk);
        for &b in &rx[..n] {
            if b == 0 {
                out[..off].copy_from_slice(&tmp[..off]);
                return Ok(off);
            }
            tmp[off] = b;
            off += 1;
        }
        if n < chunk {
            break;
        }
    }
    Err(EINVAL)
}

pub fn guest_mem_write(
    redirect_ep: u64,
    reply_tok: &mut u64,
    mut guest_va: u64,
    src: &[u8],
) -> Result<(), isize> {
    let mut off = 0usize;
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    while off < src.len() {
        let chunk = (src.len() - off).min(MAX_IPC_EXTRA_BYTES);
        let (_, rmrs) = subop_roundtrip(
            redirect_ep,
            reply_tok,
            SYSCALL_REPLY_MEM_WRITE,
            [guest_va, chunk as u64, 0, 0],
            &src[off..(off + chunk)],
            &mut rx,
        )?;
        let st = rmrs[0] as isize;
        if st < 0 {
            return Err(st);
        }
        off += chunk;
        guest_va = guest_va.wrapping_add(chunk as u64);
    }
    Ok(())
}

pub fn guest_map_anon(
    redirect_ep: u64,
    reply_tok: &mut u64,
    mut va: u64,
    mut pages: u64,
    prot_bits: u64,
) -> Result<(), isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    while pages > 0 {
        let batch = pages.min(32);
        let (_, rmrs) = subop_roundtrip(
            redirect_ep,
            reply_tok,
            SYSCALL_REPLY_MAP_ANON,
            [va, batch, prot_bits, 0],
            &[],
            &mut rx,
        )?;
        let st = rmrs[0] as isize;
        if st < 0 {
            return Err(st);
        }
        pages -= batch;
        va = va.wrapping_add(batch * 4096);
    }
    Ok(())
}

#[inline]
pub fn reply_syscall_ret(
    redirect_ep: u64,
    reply_tok: u64,
    ret: i64,
    fs_update: Option<u64>,
) -> Result<(), isize> {
    let (fs_val, flags) = match fs_update {
        Some(fs) => (fs, 1u64),
        None => (0u64, 0u64),
    };
    sys_ipc_reply_inline(
        redirect_ep,
        reply_tok,
        pack_msg_tag(SYSCALL_REPLY_RET, 0, 4, 0),
        [ret as u64, fs_val, 0, flags],
    )
}

#[inline]
pub fn reply_syscall_exit(redirect_ep: u64, reply_tok: u64, code: i32) -> Result<(), isize> {
    sys_ipc_reply_inline(
        redirect_ep,
        reply_tok,
        pack_msg_tag(SYSCALL_REPLY_EXIT, 0, 1, 0),
        [(code as i64) as u64, 0, 0, 0],
    )
}
