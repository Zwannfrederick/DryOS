// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-gpu` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0009`) VirtIO-GPU 2D
//! Scanout Driver Island (`Priority C`).
//!
//! Architectural Isolation Properties:
//! - Maps ONLY the capability-gated VirtIO-GPU (`1AF4:1050`) MMIO BAR via `SYS_DEV_MAP`.
//! - Owns `2 x 2 MiB` pinned `DEVICE_DMA | NO_DELEGATE` scanout frames (`DMA_ALLOC_FLAG_2M`)
//!   + `2 x 4 KiB` `controlq` command/ring DMA pages.
//! - Maps `dry-display`'s `back_caps` strictly `USER_RO` (`VSPACE_MAP_OP_2M_RO`), composites the
//!   12x16 software cursor into its private DMA scanout buffer, and executes VirtIO 1.0 `2D`
//!   `TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH` commands.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ptr::{copy_nonoverlapping, write_bytes};

use dry_userspace::gfx::{CURSOR_H, CURSOR_SPRITE_12X16, CURSOR_W};
use dry_userspace::gpu_proto::{
    BADGE_GPU_DISPLAY, BADGE_GPU_SUP, GPU_OP_ATTACH_BACKBUF, GPU_OP_BIND_PCI,
    GPU_OP_PRESENT_RECT, GPU_OP_QUERY_INFO,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::virtio_gpu::{VirtioGpu2d, GPU_SCANOUT_VA0};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_revoke, sys_exit,
    sys_ipc_recv_ext, sys_ipc_reply_inline, sys_vspace_map, sys_write_debug, UserIpcRecvExt,
    EINVAL, EPERM, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN,
    IPC_OP_PONG, VSPACE_MAP_OP_2M_RO, VSPACE_MAP_OP_UNMAP_2M,
};

const BACKBUF_RO_VA0: u64 = 0x2600_0000;
const TWO_MIB: u64 = 2 * 1024 * 1024;

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
fn reply_err(ep: u64, tok: u64, label: u32, code: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(label, 0, 1, 0),
            [(-code) as u64, 0, 0, 0],
        );
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn blit_ro_backbuf_to_scanout(
    width: usize,
    height: usize,
    y0: usize,
    h0: usize,
    cur_x: usize,
    cur_y: usize,
    cur_vis: bool,
    cur_scale: usize,
    blanked: bool,
    high_contrast: bool,
) {
    let (cur_outline, cur_fill, cur_accent) = if high_contrast {
        (0x0000_0000u32, 0x00FF_EA00u32, 0x0000_FFEEu32)
    } else {
        (0x0006_090Fu32, 0x00F8_FAFCu32, 0x0022_D3EEu32)
    };
    let cur_w_px = CURSOR_W * cur_scale;
    let cur_h_px = CURSOR_H * cur_scale;

    for y in y0..(y0 + h0).min(height) {
        let row_bytes = width * 4;
        let byte_off = (y * row_bytes) as u64;
        // SAFETY: Both `[0..4 MiB)` RO backbuf and private `DEVICE_DMA` scanout frames are mapped at `0x2600_0000` and `0x2500_0000`.
        let src_row = (BACKBUF_RO_VA0 + byte_off) as *const u32;
        let dst_row = (GPU_SCANOUT_VA0 + byte_off) as *mut u32;
        if blanked {
            unsafe {
                write_bytes(dst_row.cast::<u8>(), 0, row_bytes);
            }
            continue;
        }
        unsafe {
            copy_nonoverlapping(src_row.cast::<u8>(), dst_row.cast::<u8>(), row_bytes);
        }
        if cur_vis && y >= cur_y && y < cur_y + cur_h_px {
            let sy = ((y - cur_y) / cur_scale).min(CURSOR_H - 1);
            let sprite_row = &CURSOR_SPRITE_12X16[sy];
            let x_end = (cur_x + cur_w_px).min(width);
            for x in cur_x..x_end {
                let sx = ((x - cur_x) / cur_scale).min(CURSOR_W - 1);
                let px = match sprite_row[sx] {
                    1 => cur_outline,
                    2 => cur_fill,
                    3 => cur_accent,
                    _ => continue,
                };
                // SAFETY: `x < width` and `y < height` inside mapped scanout buffer.
                unsafe {
                    dst_row.add(x).write(px);
                }
            }
        }
    }
}

fn emit_gpu_init_log(gpu: &VirtioGpu2d) {
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-GPU] INIT dev=0x");
    lb.push_hex_u64(gpu.vendor_id as u64);
    lb.push_bytes(b":0x");
    lb.push_hex_u64(gpu.device_id as u64);
    lb.push_bytes(b" scanout=");
    lb.push_u64(gpu.width as u64);
    lb.push_byte(b'x');
    lb.push_u64(gpu.height as u64);
    lb.push_bytes(b" fmt=B8G8R8X8 dma_2m=2 msix_vec=");
    lb.push_u64(gpu.msix_vec as u64);
    lb.push_bytes(b" msi_irqs=");
    lb.push_u64(gpu.msi_irqs);
    log_line(&lb);
}

#[no_mangle]
pub extern "C" fn _start(gpu_ep_cap: u64, pci_gpu_arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || gpu_ep_cap == 0 {
        sys_exit(190);
    }

    let mut pci_gpu_cap = pci_gpu_arg1;
    let mut gpu: Option<VirtioGpu2d> = if pci_gpu_arg1 != 0 {
        let Ok(g) = VirtioGpu2d::init(pci_gpu_arg1) else {
            sys_exit(191);
        };
        emit_gpu_init_log(&g);
        Some(g)
    } else {
        None
    };

    let mut attached_caps = [0u64; 2];
    let mut presents = 0u64;

    loop {
        let mut ext = UserIpcRecvExt::default();
        let Ok((tag, tok, mrs)) = sys_ipc_recv_ext(gpu_ep_cap, 0, 0, &mut ext) else {
            break;
        };
        let from_sup = ext.sender_badge == BADGE_GPU_SUP;
        let from_disp = ext.sender_badge == BADGE_GPU_DISPLAY;

        match msg_tag_label(tag) {
            GPU_OP_BIND_PCI => {
                if !from_sup {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if ext.transferred_cap == 0 || gpu.is_some() {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                let Ok(g) = VirtioGpu2d::init(ext.transferred_cap) else {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                };
                emit_gpu_init_log(&g);
                let dims = (g.width as u64) | ((g.height as u64) << 32);
                let vid = g.vendor_id as u64;
                let did = g.device_id as u64;
                pci_gpu_cap = ext.transferred_cap;
                gpu = Some(g);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        gpu_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [1, dims, vid, did],
                    );
                }
            }
            GPU_OP_QUERY_INFO => {
                if !(from_sup || from_disp) {
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let Some(ref g) = gpu else {
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                };
                if tok != 0 {
                    let dims = (g.width as u64) | ((g.height as u64) << 32);
                    let meta = 1u64 | (g.virtq_cmds << 32);
                    let _ = sys_ipc_reply_inline(
                        gpu_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [dims, presents, g.transfer_bytes, meta],
                    );
                }
            }
            GPU_OP_ATTACH_BACKBUF => {
                if !from_disp {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if gpu.is_none() {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                let idx = mrs[0] as usize;
                if idx >= 2 || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                let va = BACKBUF_RO_VA0 + (idx as u64) * TWO_MIB;
                if attached_caps[idx] != 0 {
                    let _ = sys_vspace_map(attached_caps[idx], va, VSPACE_MAP_OP_UNMAP_2M);
                    let _ = sys_cap_revoke(attached_caps[idx], 1);
                    attached_caps[idx] = 0;
                }
                if sys_vspace_map(ext.transferred_cap, va, VSPACE_MAP_OP_2M_RO).is_err() {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                attached_caps[idx] = ext.transferred_cap;
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        gpu_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [idx as u64, va, 0, 0],
                    );
                }
            }
            GPU_OP_PRESENT_RECT => {
                if !from_disp {
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let Some(ref mut g) = gpu else {
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                };
                let w = g.width as usize;
                let h = g.height as usize;
                let need_pages = (w * h * 4).div_ceil(TWO_MIB as usize).min(2);
                if attached_caps.iter().take(need_pages).any(|&c| c == 0) {
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }

                let t0 = rdtsc_user();
                let y0 = ((mrs[0] >> 32) as usize).min(h);
                let req_h = (mrs[1] >> 32) as usize;
                let h0 = req_h.min(h.saturating_sub(y0));
                let cur_x = (mrs[2] & 0xffff) as usize;
                let cur_y = ((mrs[2] >> 16) & 0xffff) as usize;
                let cur_vis = ((mrs[2] >> 32) & 1) != 0;
                let cur_scale = if ((mrs[2] >> 33) & 1) != 0 { 2 } else { 1 };
                let blanked = (mrs[3] & 1) != 0;
                let high_contrast = (mrs[3] & 2) != 0;

                if h0 > 0 {
                    // SAFETY: Both RO backbuf pages (`0x2600_0000..`) are verified mapped above.
                    unsafe {
                        blit_ro_backbuf_to_scanout(
                            w,
                            h,
                            y0,
                            h0,
                            cur_x,
                            cur_y,
                            cur_vis,
                            cur_scale,
                            blanked,
                            high_contrast,
                        );
                    }
                    if g.transfer_and_flush_rows(y0 as u32, h0 as u32).is_err() {
                        reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                        continue;
                    }
                }
                presents = presents.wrapping_add(1);
                let dt = rdtsc_user().saturating_sub(t0);
                let px = (w as u64) * (h0 as u64);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        gpu_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [px, dt, presents, g.virtq_cmds],
                    );
                }
            }
            IPC_OP_CRASH_UD2 => {
                if !from_sup {
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                // SAFETY: Controlled fault-injection test triggering `#UD` (`vector = 6`) into `FaultEndpoint`.
                unsafe {
                    asm!("ud2", options(nostack, nomem));
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if !from_sup {
                    reply_err(gpu_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                for (idx, cap) in attached_caps.iter_mut().enumerate() {
                    if *cap != 0 {
                        let va = BACKBUF_RO_VA0 + (idx as u64) * TWO_MIB;
                        let _ = sys_vspace_map(*cap, va, VSPACE_MAP_OP_UNMAP_2M);
                        let _ = sys_cap_revoke(*cap, 1);
                        *cap = 0;
                    }
                }
                let mut vq_cmds = 0u64;
                if let Some(mut g) = gpu.take() {
                    vq_cmds = g.virtq_cmds;
                    g.shutdown_clean();
                }
                if pci_gpu_cap != 0 {
                    let _ = sys_cap_revoke(pci_gpu_cap, 1);
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        gpu_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [1, presents, vq_cmds, 0],
                    );
                }
                sys_exit(0);
            }
            _ => reply_err(gpu_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL),
        }
    }
    sys_exit(0);
}
