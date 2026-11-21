// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Badged Ring 3 VFS IPC Protocol & Client SDK (`Milestone 2A-3`, `ADR-014`).
//!
//! Designed for clean forward-compatibility with future multi-backend VFS & Linux POSIX shims:
//! - Canonical slash-delimited paths (`/boot/...`, `/ram/...`) validated without `..` traversal.
//! - Explicit backend capability flags (`VFS_FLAG_READONLY`, `VFS_FLAG_VOLATILE`, `VFS_FLAG_PRIVATE`)
//!   so UI clients (`Files`) only enable `create`/`write`/`rename`/`unlink` when the mount supports it.
//! - Uses DRY IPC inline message registers (`R12..R15`) + bounded `256 B` extra buffer (`MSG_FLAG_EXTRA_BUF`).

use core::mem::size_of;

use crate::{
    msg_tag_extra_len, msg_tag_label, pack_msg_tag, raw_ipc_syscall, sys_cap_revoke,
    sys_frame_alloc, sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_reply_inline,
    sys_vspace_map, EEXIST, UserIpcRecvExt, UserIpcSendExt, BADGE_BLK_VFS, EINVAL, IPC_OP_PONG,
    MAX_IPC_EXTRA_BYTES, MSG_FLAG_EXTRA_BUF, MSG_FLAG_GRANT_CAP, RIGHT_CALL, RIGHT_GRANT,
    RIGHT_MAP, RIGHT_READ, RIGHT_SEND, SYS_IPC_CALL, SYS_IPC_RECV, SYS_IPC_REPLY,
    VFS_OP_CORRUPT_SB0_TEST, VFS_OP_LINK, VFS_OP_MOUNT_BLK, VFS_OP_SYNC, VSPACE_MAP_OP_2M_RW,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_2M, VSPACE_MAP_OP_UNMAP_4K,
};

pub const VFS_OP_SEED_BOOT: u32 = 60;
pub const VFS_OP_LIST: u32 = 61;
pub const VFS_OP_STAT: u32 = 62;
pub const VFS_OP_READ: u32 = 63;
pub const VFS_OP_CREATE: u32 = 64;
pub const VFS_OP_WRITE: u32 = 65;
pub const VFS_OP_RENAME: u32 = 66;
pub const VFS_OP_UNLINK: u32 = 67;
pub const VFS_OP_QUERY_STATS: u32 = 68;
pub const VFS_OP_ERR: u32 = 69;
pub const VFS_OP_MKDIR: u32 = 70;
pub const VFS_OP_RMDIR: u32 = 71;
pub const VFS_OP_MOUNT_BIN_TAR: u32 = 72;
pub const VFS_OP_WRITE_CAP: u32 = 73;

pub const VFS_FLAG_DIR: u16 = 1 << 0;
pub const VFS_FLAG_READONLY: u16 = 1 << 1;
pub const VFS_FLAG_VOLATILE: u16 = 1 << 2;
pub const VFS_FLAG_PRIVATE: u16 = 1 << 3;
pub const VFS_FLAG_EXECUTABLE: u16 = 1 << 4;
pub const VFS_FLAG_PERSISTENT: u16 = 1 << 5;
pub const VFS_FLAG_EXTENT: u16 = 1 << 6;

pub const VFS_MAX_NAME_LEN: usize = 35;
pub const VFS_MAX_PATH_LEN: usize = 96;
pub const VFS_DIRENTS_PER_CALL: usize = 5; // 5 * 48 = 240 <= 256 B

/// Fixed 48-byte POD directory entry returned by `VFS_OP_LIST` / `VFS_OP_STAT`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VfsDirEntry {
    pub inode: u32,
    pub size_bytes: u32,
    pub flags: u16,
    pub owner_comp: u16,
    pub name: [u8; 36],
}

const _: () = assert!(size_of::<VfsDirEntry>() == 48);

impl VfsDirEntry {
    pub const EMPTY: Self = Self {
        inode: 0,
        size_bytes: 0,
        flags: 0,
        owner_comp: 0,
        name: [0; 36],
    };

    #[inline]
    pub fn name_bytes(&self) -> &[u8] {
        let len = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len());
        &self.name[..len]
    }
}

impl Default for VfsDirEntry {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[inline]
pub fn sys_ipc_recv_extra(
    ep_cap: u64,
    flags: u64,
    timeout: u64,
    extra_out: &mut [u8; MAX_IPC_EXTRA_BYTES],
    ext_out: &mut UserIpcRecvExt,
) -> Result<(u64, usize, [u64; 4]), isize> {
    ext_out.extra_buf_ptr = extra_out.as_mut_ptr() as u64;
    ext_out.extra_buf_cap = MAX_IPC_EXTRA_BYTES as u64;
    let (r0, _, mrs) = raw_ipc_syscall(
        SYS_IPC_RECV,
        ep_cap,
        flags,
        timeout,
        (ext_out as *mut UserIpcRecvExt) as u64,
        0,
        0,
        [0; 4],
    );
    if r0 < 0 {
        Err(r0)
    } else {
        let tag = r0 as u64;
        Ok((tag, msg_tag_extra_len(tag).min(MAX_IPC_EXTRA_BYTES), mrs))
    }
}

#[inline]
pub fn sys_ipc_call_extra(
    ep_cap: u64,
    label: u32,
    mrs: [u64; 4],
    send_extra: &[u8],
    recv_extra: &mut [u8; MAX_IPC_EXTRA_BYTES],
    timeout: u64,
) -> Result<(u64, usize, [u64; 4]), isize> {
    let slen = send_extra.len().min(MAX_IPC_EXTRA_BYTES);
    let flags = if slen > 0 { MSG_FLAG_EXTRA_BUF } else { 0 };
    let tag = pack_msg_tag(label, flags, 4, slen as u16);
    let send_ext = UserIpcSendExt {
        extra_buf_ptr: if slen > 0 { send_extra.as_ptr() as u64 } else { 0 },
        grant_cap: 0,
        grant_rights: 0,
        _pad: 0,
        grant_lease: 0,
        grant_badge: 0,
        shared_region_cap: 0,
        shared_offset_and_len: 0,
    };
    let mut recv_ext = UserIpcRecvExt {
        extra_buf_ptr: recv_extra.as_mut_ptr() as u64,
        extra_buf_cap: MAX_IPC_EXTRA_BYTES as u64,
        sender_badge: 0,
        reply_token: 0,
        transferred_cap: 0,
    };
    let (r0, _, out_mrs) = raw_ipc_syscall(
        SYS_IPC_CALL,
        ep_cap,
        tag,
        timeout,
        if slen > 0 {
            (&raw const send_ext) as u64
        } else {
            0
        },
        (&raw mut recv_ext) as u64,
        0,
        mrs,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        let rtag = r0 as u64;
        Ok((rtag, msg_tag_extra_len(rtag).min(MAX_IPC_EXTRA_BYTES), out_mrs))
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
pub fn sys_ipc_call_extra_grant(
    ep_cap: u64,
    label: u32,
    mrs: [u64; 4],
    send_extra: &[u8],
    grant_cap: u64,
    grant_rights: u16,
    grant_badge: u64,
    recv_extra: &mut [u8; MAX_IPC_EXTRA_BYTES],
    timeout: u64,
) -> Result<(u64, usize, [u64; 4]), isize> {
    let slen = send_extra.len().min(MAX_IPC_EXTRA_BYTES);
    let flags = (if slen > 0 { MSG_FLAG_EXTRA_BUF } else { 0 })
        | (if grant_cap != 0 { MSG_FLAG_GRANT_CAP } else { 0 });
    let tag = pack_msg_tag(label, flags, 4, slen as u16);
    let send_ext = UserIpcSendExt {
        extra_buf_ptr: if slen > 0 { send_extra.as_ptr() as u64 } else { 0 },
        grant_cap,
        grant_rights,
        _pad: 0,
        grant_lease: 0,
        grant_badge,
        shared_region_cap: 0,
        shared_offset_and_len: 0,
    };
    let mut recv_ext = UserIpcRecvExt {
        extra_buf_ptr: recv_extra.as_mut_ptr() as u64,
        extra_buf_cap: MAX_IPC_EXTRA_BYTES as u64,
        sender_badge: 0,
        reply_token: 0,
        transferred_cap: 0,
    };
    let (r0, _, out_mrs) = raw_ipc_syscall(
        SYS_IPC_CALL,
        ep_cap,
        tag,
        timeout,
        if flags != 0 { (&raw const send_ext) as u64 } else { 0 },
        (&raw mut recv_ext) as u64,
        0,
        mrs,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        let rtag = r0 as u64;
        Ok((rtag, msg_tag_extra_len(rtag).min(MAX_IPC_EXTRA_BYTES), out_mrs))
    }
}

#[inline]
pub fn sys_ipc_reply_extra(
    ep_cap: u64,
    reply_token: u64,
    label: u32,
    mrs: [u64; 4],
    extra: &[u8],
) -> Result<(), isize> {
    let elen = extra.len().min(MAX_IPC_EXTRA_BYTES);
    if elen == 0 {
        return sys_ipc_reply_inline(ep_cap, reply_token, pack_msg_tag(label, 0, 4, 0), mrs);
    }
    let tag = pack_msg_tag(label, MSG_FLAG_EXTRA_BUF, 4, elen as u16);
    let send_ext = UserIpcSendExt {
        extra_buf_ptr: extra.as_ptr() as u64,
        grant_cap: 0,
        grant_rights: 0,
        _pad: 0,
        grant_lease: 0,
        grant_badge: 0,
        shared_region_cap: 0,
        shared_offset_and_len: 0,
    };
    let (r0, _, _) = raw_ipc_syscall(
        SYS_IPC_REPLY,
        ep_cap,
        reply_token,
        tag,
        (&raw const send_ext) as u64,
        0,
        0,
        mrs,
    );
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

/// Seed a boot-manifest module entry into `/boot` (`BADGE_SUPERVISOR` only).
pub fn vfs_seed_boot_entry(vfs_ep: u64, name: &[u8], size_bytes: u64) -> Result<(), isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(
        vfs_ep,
        VFS_OP_SEED_BOOT,
        [size_bytes, 0, 0, 0],
        name,
        &mut rx,
        0,
    )?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(())
    } else {
        Err(mrs[0] as isize)
    }
}

/// List directory entries under `mount_path` (`"/boot"` or `"/ram"`) into `out`, returning `(written, mount_flags)`.
pub fn vfs_list_dir(
    vfs_ep: u64,
    mount_path: &[u8],
    out: &mut [VfsDirEntry],
) -> Result<(usize, u16), isize> {
    let mut cursor = 0usize;
    let mut written = 0usize;
    let mut mount_flags: u16;
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];

    loop {
        let (tag, rx_len, mrs) = sys_ipc_call_extra(
            vfs_ep,
            VFS_OP_LIST,
            [cursor as u64, 0, 0, 0],
            mount_path,
            &mut rx,
            0,
        )?;
        if msg_tag_label(tag) != IPC_OP_PONG {
            return Err(mrs[0] as isize);
        }
        let batch_n = (mrs[0] as usize).min(rx_len / size_of::<VfsDirEntry>());
        let total_n = mrs[1] as usize;
        mount_flags = (mrs[2] & 0xffff) as u16;
        if batch_n == 0 {
            break;
        }
        for i in 0..batch_n {
            if written >= out.len() {
                return Ok((written, mount_flags));
            }
            let base = i * size_of::<VfsDirEntry>();
            // SAFETY: `VfsDirEntry` is `#[repr(C)]` POD (`48` bytes) and `base + 48 <= rx_len`.
            let entry: VfsDirEntry =
                unsafe { core::ptr::read_unaligned(rx.as_ptr().add(base).cast()) };
            out[written] = entry;
            written += 1;
        }
        cursor += batch_n;
        if cursor >= total_n || written >= out.len() {
            break;
        }
    }
    Ok((written, mount_flags))
}

/// Read up to `dst.len()` bytes from `full_path` starting at `offset`, returning `(bytes_read, total_file_size)`.
pub fn vfs_read_file(
    vfs_ep: u64,
    full_path: &[u8],
    offset: usize,
    dst: &mut [u8],
) -> Result<(usize, usize), isize> {
    if full_path.is_empty() || full_path.len() > VFS_MAX_PATH_LEN {
        return Err(EINVAL);
    }
    let mut pos = 0usize;
    let mut total_size = 0usize;
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];

    while pos < dst.len() {
        let want = (dst.len() - pos).min(MAX_IPC_EXTRA_BYTES);
        let (tag, rx_len, mrs) = sys_ipc_call_extra(
            vfs_ep,
            VFS_OP_READ,
            [(offset + pos) as u64, want as u64, 0, 0],
            full_path,
            &mut rx,
            0,
        )?;
        if msg_tag_label(tag) != IPC_OP_PONG {
            return Err(mrs[0] as isize);
        }
        let got = (mrs[0] as usize).min(rx_len).min(want);
        total_size = mrs[1] as usize;
        if got == 0 {
            break;
        }
        dst[pos..(pos + got)].copy_from_slice(&rx[..got]);
        pos += got;
        if offset + pos >= total_size {
            break;
        }
    }
    Ok((pos, total_size))
}

/// Create a new empty file at `full_path` (`flags` may set `VFS_FLAG_PRIVATE`).
pub fn vfs_create_file(vfs_ep: u64, full_path: &[u8], flags: u16) -> Result<u32, isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(
        vfs_ep,
        VFS_OP_CREATE,
        [flags as u64, 0, 0, 0],
        full_path,
        &mut rx,
        0,
    )?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(mrs[0] as u32)
    } else {
        Err(mrs[0] as isize)
    }
}

/// Write/replace or append (`append == true`) up to `140` bytes into `full_path`.
/// Packs `path_len (u8) | path | payload` inside the 256-byte IPC extra buffer.
pub fn vfs_write_small(
    vfs_ep: u64,
    full_path: &[u8],
    payload: &[u8],
    append: bool,
) -> Result<usize, isize> {
    if full_path.is_empty()
        || full_path.len() > VFS_MAX_PATH_LEN
        || 1 + full_path.len() + payload.len() > MAX_IPC_EXTRA_BYTES
    {
        return Err(EINVAL);
    }
    let mut tx = [0u8; MAX_IPC_EXTRA_BYTES];
    tx[0] = full_path.len() as u8;
    tx[1..(1 + full_path.len())].copy_from_slice(full_path);
    let end = 1 + full_path.len() + payload.len();
    tx[(1 + full_path.len())..end].copy_from_slice(payload);

    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(
        vfs_ep,
        VFS_OP_WRITE,
        [u64::from(append), payload.len() as u64, 0, 0],
        &tx[..end],
        &mut rx,
        0,
    )?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(mrs[0] as usize)
    } else {
        Err(mrs[0] as isize)
    }
}

/// Rename `old_path` -> `new_path` on the same writable mount (`old_len (u8) | old_path | new_path`).
pub fn vfs_rename_file(vfs_ep: u64, old_path: &[u8], new_path: &[u8]) -> Result<(), isize> {
    if old_path.is_empty()
        || new_path.is_empty()
        || old_path.len() > VFS_MAX_PATH_LEN
        || new_path.len() > VFS_MAX_PATH_LEN
        || 1 + old_path.len() + new_path.len() > MAX_IPC_EXTRA_BYTES
    {
        return Err(EINVAL);
    }
    let mut tx = [0u8; MAX_IPC_EXTRA_BYTES];
    tx[0] = old_path.len() as u8;
    tx[1..(1 + old_path.len())].copy_from_slice(old_path);
    let end = 1 + old_path.len() + new_path.len();
    tx[(1 + old_path.len())..end].copy_from_slice(new_path);

    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) =
        sys_ipc_call_extra(vfs_ep, VFS_OP_RENAME, [0; 4], &tx[..end], &mut rx, 0)?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(())
    } else {
        Err(mrs[0] as isize)
    }
}

/// Unlink/delete `full_path` on a writable mount.
pub fn vfs_unlink_file(vfs_ep: u64, full_path: &[u8]) -> Result<(), isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) =
        sys_ipc_call_extra(vfs_ep, VFS_OP_UNLINK, [0; 4], full_path, &mut rx, 0)?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(())
    } else {
        Err(mrs[0] as isize)
    }
}

/// Mount `/persist` (`DRYFS_v1`) on `dry-vfs` by granting `blk_ep_cap` (`BADGE_SUPERVISOR` only).
/// Returns `(fresh_formatted, tx_seq, recovered_fallback)`.
pub fn vfs_mount_blk(vfs_ep: u64, blk_ep_cap: u64) -> Result<(bool, u64, bool), isize> {
    let rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL | RIGHT_GRANT;
    let (rtag, _, rmrs) = sys_ipc_call_with_grant(
        vfs_ep,
        VFS_OP_MOUNT_BLK,
        [0; 4],
        blk_ep_cap,
        rights,
        BADGE_BLK_VFS,
        0,
    )?;
    if msg_tag_label(rtag) == IPC_OP_PONG {
        Ok((rmrs[0] != 0, rmrs[1], rmrs[2] != 0))
    } else {
        Err(rmrs[0] as isize)
    }
}

/// Explicitly flush `/persist` via `VIRTIO_BLK_T_FLUSH` write barrier.
pub fn vfs_sync(vfs_ep: u64) -> Result<u64, isize> {
    let (rtag, _, rmrs) =
        sys_ipc_call_inline(vfs_ep, pack_msg_tag(VFS_OP_SYNC, 0, 0, 0), [0; 4], 0)?;
    if msg_tag_label(rtag) == IPC_OP_PONG {
        Ok(rmrs[0])
    } else {
        Err(rmrs[0] as isize)
    }
}

/// Create a hardlink `src_path -> dst_path` (`INV-SEC-05` rejects cross-mount with `-EXDEV` and
/// cross-compartment/cross-domain with `-EPERM`).
pub fn vfs_link_file(vfs_ep: u64, src_path: &[u8], dst_path: &[u8]) -> Result<u32, isize> {
    if src_path.is_empty()
        || dst_path.is_empty()
        || src_path.len() > VFS_MAX_PATH_LEN
        || dst_path.len() > VFS_MAX_PATH_LEN
        || 1 + src_path.len() + dst_path.len() > MAX_IPC_EXTRA_BYTES
    {
        return Err(EINVAL);
    }
    let mut tx = [0u8; MAX_IPC_EXTRA_BYTES];
    tx[0] = src_path.len() as u8;
    tx[1..(1 + src_path.len())].copy_from_slice(src_path);
    let end = 1 + src_path.len() + dst_path.len();
    tx[(1 + src_path.len())..end].copy_from_slice(dst_path);

    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(vfs_ep, VFS_OP_LINK, [0; 4], &tx[..end], &mut rx, 0)?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(mrs[0] as u32)
    } else {
        Err(mrs[0] as isize)
    }
}

/// Deliberate fault-injection test hook (`BADGE_SUPERVISOR` only): corrupt Superblock Slot 0 on disk.
pub fn vfs_corrupt_sb0_test(vfs_ep: u64) -> Result<(), isize> {
    let (rtag, _, rmrs) = sys_ipc_call_inline(
        vfs_ep,
        pack_msg_tag(VFS_OP_CORRUPT_SB0_TEST, 0, 0, 0),
        [0; 4],
        0,
    )?;
    if msg_tag_label(rtag) == IPC_OP_PONG {
        Ok(())
    } else {
        Err(rmrs[0] as isize)
    }
}

pub fn vfs_stat(vfs_ep: u64, full_path: &[u8]) -> Result<(u32, usize, u16), isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(vfs_ep, VFS_OP_STAT, [0; 4], full_path, &mut rx, 0)?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok((mrs[0] as u32, mrs[1] as usize, (mrs[2] & 0xffff) as u16))
    } else {
        Err(mrs[0] as isize)
    }
}

pub fn vfs_mkdir(vfs_ep: u64, full_path: &[u8]) -> Result<u32, isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(vfs_ep, VFS_OP_MKDIR, [0; 4], full_path, &mut rx, 0)?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(mrs[0] as u32)
    } else {
        Err(mrs[0] as isize)
    }
}

pub fn vfs_rmdir(vfs_ep: u64, full_path: &[u8]) -> Result<(), isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(vfs_ep, VFS_OP_RMDIR, [0; 4], full_path, &mut rx, 0)?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok(())
    } else {
        Err(mrs[0] as isize)
    }
}

pub fn vfs_mount_bin_tar(vfs_ep: u64, tar_cap: u64, tar_bytes: u64) -> Result<(), isize> {
    let (rtag, _, rmrs) = sys_ipc_call_with_grant(
        vfs_ep,
        VFS_OP_MOUNT_BIN_TAR,
        [tar_bytes, 0, 0, 0],
        tar_cap,
        RIGHT_READ | RIGHT_MAP,
        0,
        0,
    )?;
    if msg_tag_label(rtag) == IPC_OP_PONG {
        Ok(())
    } else {
        Err(rmrs[0] as isize)
    }
}

pub fn vfs_write_cap(
    vfs_ep: u64,
    full_path: &[u8],
    frame_cap: u64,
    byte_len: usize,
    append: bool,
) -> Result<(usize, usize), isize> {
    if full_path.is_empty() || full_path.len() > VFS_MAX_PATH_LEN {
        return Err(EINVAL);
    }
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra_grant(
        vfs_ep,
        VFS_OP_WRITE_CAP,
        [byte_len as u64, u64::from(append), 0, 0],
        full_path,
        frame_cap,
        RIGHT_READ | RIGHT_MAP,
        0,
        &mut rx,
        0,
    )?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok((mrs[0] as usize, mrs[1] as usize))
    } else {
        Err(mrs[0] as isize)
    }
}

/// Write arbitrary `data` (up to `512 KiB`) cleanly: small payloads stay inline; larger payloads
/// use an ephemeral zero-leak mapped `Frame4K`/`Frame2M` granted read/map over `VFS_OP_WRITE_CAP`.
pub fn vfs_write_all_at(
    vfs_ep: u64,
    full_path: &[u8],
    data: &[u8],
    append: bool,
    scratch_vaddr: u64,
) -> Result<usize, isize> {
    if 1 + full_path.len() + data.len() <= MAX_IPC_EXTRA_BYTES {
        match vfs_create_file(vfs_ep, full_path, 0) {
            Ok(_) | Err(EEXIST) => {}
            Err(e) => return Err(e),
        }
        return vfs_write_small(vfs_ep, full_path, data, append);
    }
    if data.len() > 2 * 1024 * 1024 {
        return Err(crate::ENOSPC);
    }
    match vfs_create_file(vfs_ep, full_path, 0) {
        Ok(_) | Err(EEXIST) => {}
        Err(e) => return Err(e),
    }
    let use_2m = data.len() > 4096;
    let frame_cap = sys_frame_alloc(u64::from(use_2m))?;
    let (map_op, unmap_op) = if use_2m {
        (VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_UNMAP_2M)
    } else {
        (VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K)
    };
    if let Err(e) = sys_vspace_map(frame_cap, scratch_vaddr, map_op) {
        let _ = sys_cap_revoke(frame_cap, 2);
        return Err(e);
    }
    // SAFETY: `scratch_vaddr` is freshly mapped `USER_RW` backed by `frame_cap` (`>= data.len()`).
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), scratch_vaddr as *mut u8, data.len());
    }
    let res = vfs_write_cap(vfs_ep, full_path, frame_cap, data.len(), append);
    let _ = sys_vspace_map(frame_cap, scratch_vaddr, unmap_op);
    let _ = sys_cap_revoke(frame_cap, 2);
    res.map(|(w, _)| w)
}
