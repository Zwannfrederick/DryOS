// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! VirtIO-GPU 3D (`VIRTIO_GPU_F_VIRGL`) Context, 3D Resource, Fenced `SUBMIT_3D`,
//! and `TRANSFER_FROM_HOST_3D` Readback Protocol + Ring-3 Self-Tests (`Milestone E`).

use core::mem::size_of;
use core::ptr::{read_volatile, write_bytes};

use crate::abi_ext::sys_dma_alloc;
use crate::virtio_gpu::{
    VirtioGpu2d, VirtioGpuCtrlHdr, VIRTIO_GPU_RESP_OK_NODATA,
};
use crate::{sys_cap_revoke, sys_vspace_map, VSPACE_MAP_OP_UNMAP_4K};

const GPU_3D_BUF_VADDR: u64 = 0x2481_2000;

pub const VIRTIO_GPU_FLAG_FENCE: u32 = 1 << 0;

pub const VIRTIO_GPU_CMD_CTX_CREATE: u32 = 0x0200;
pub const VIRTIO_GPU_CMD_CTX_DESTROY: u32 = 0x0201;
pub const VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_3D: u32 = 0x0204;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D: u32 = 0x0205;
pub const VIRTIO_GPU_CMD_TRANSFER_FROM_HOST_3D: u32 = 0x0206;
pub const VIRTIO_GPU_CMD_SUBMIT_3D: u32 = 0x0207;

pub const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

const PIPE_TEXTURE_2D: u32 = 2;
const VIRGL_FORMAT_B8G8R8A8_UNORM: u32 = 1;
const VIRGL_BIND_RENDER_TARGET: u32 = 1 << 1;
const VIRGL_BIND_SAMPLER_VIEW: u32 = 1 << 3;

const VIRGL_CCMD_CREATE_OBJECT: u32 = 1;
const VIRGL_CCMD_DESTROY_OBJECT: u32 = 3;
const VIRGL_CCMD_SET_FRAMEBUFFER_STATE: u32 = 5;
const VIRGL_CCMD_CLEAR: u32 = 7;
const VIRGL_OBJECT_SURFACE: u32 = 8;
const PIPE_CLEAR_COLOR0: u32 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuCtxCreate {
    pub hdr: VirtioGpuCtrlHdr,
    pub nlen: u32,
    pub context_init: u32,
    pub debug_name: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuCtxResource {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuResourceCreate3d {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuBox {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuTransferHost3d {
    pub hdr: VirtioGpuCtrlHdr,
    pub box_3d: VirtioGpuBox,
    pub offset: u64,
    pub resource_id: u32,
    pub level: u32,
    pub stride: u32,
    pub layer_stride: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuCmdSubmit {
    pub hdr: VirtioGpuCtrlHdr,
    pub size: u32,
    pub padding: u32,
}

const _: () = assert!(size_of::<VirtioGpuCtrlHdr>() == 24);
const _: () = assert!(size_of::<VirtioGpuCtxCreate>() == 96);
const _: () = assert!(size_of::<VirtioGpuCtxResource>() == 32);
const _: () = assert!(size_of::<VirtioGpuResourceCreate3d>() == 72);
const _: () = assert!(size_of::<VirtioGpuBox>() == 24);
const _: () = assert!(size_of::<VirtioGpuTransferHost3d>() == 72);
const _: () = assert!(size_of::<VirtioGpuCmdSubmit>() == 32);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Virtio3dSmokeResult {
    pub unit_ok: bool,
    pub executed: bool,
    pub ctx_ok: bool,
    pub res_ok: bool,
    pub submit_fenced_ok: bool,
    pub xfer_resp_kind: u32,
    pub readback_pixel: u32,
    pub pixel_match: bool,
    pub neg_err_kind: u32,
    pub cleaned_up: bool,
}

#[inline(always)]
pub const fn virgl_cmd0(cmd: u32, obj: u32, len_words: u32) -> u32 {
    (cmd & 0xff) | ((obj & 0xff) << 8) | ((len_words & 0xffff) << 16)
}

/// Encode a deterministic Virgl Gallium clear sequence (`CREATE_OBJECT(SURFACE)` ->
/// `SET_FRAMEBUFFER_STATE` -> `CLEAR(COLOR0)` -> `DESTROY_OBJECT(SURFACE)`).
/// Returns `([u32; 21], payload_bytes)`.
pub fn encode_virgl_clear_cmdbuf(
    surface_handle: u32,
    res_id: u32,
    rgba_f32: [f32; 4],
) -> ([u32; 21], usize) {
    let words = [
        // 1. VIRGL_CCMD_CREATE_OBJECT(SURFACE, len=5) -> 6 words
        virgl_cmd0(VIRGL_CCMD_CREATE_OBJECT, VIRGL_OBJECT_SURFACE, 5),
        surface_handle,
        res_id,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
        0,
        0,
        // 2. VIRGL_CCMD_SET_FRAMEBUFFER_STATE(len=3) -> 4 words
        virgl_cmd0(VIRGL_CCMD_SET_FRAMEBUFFER_STATE, 0, 3),
        1, // nr_cbufs
        0, // zsurf_handle
        surface_handle,
        // 3. VIRGL_CCMD_CLEAR(len=8) -> 9 words
        virgl_cmd0(VIRGL_CCMD_CLEAR, 0, 8),
        PIPE_CLEAR_COLOR0,
        rgba_f32[0].to_bits(),
        rgba_f32[1].to_bits(),
        rgba_f32[2].to_bits(),
        rgba_f32[3].to_bits(),
        0, // depth low32
        0, // depth high32
        0, // stencil
        // 4. VIRGL_CCMD_DESTROY_OBJECT(SURFACE, len=1) -> 2 words
        virgl_cmd0(VIRGL_CCMD_DESTROY_OBJECT, VIRGL_OBJECT_SURFACE, 1),
        surface_handle,
    ];
    (words, words.len() * size_of::<u32>())
}

/// Verify VirtIO-GPU 3D wire struct sizes and Virgl clear stream encoding in Ring 3.
pub fn verify_virtio_3d_protocol_invariants() -> bool {
    let (words, bytes) = encode_virgl_clear_cmdbuf(1, 2, [1.0, 0.0, 0.0, 1.0]);
    bytes == 84
        && words[0] == virgl_cmd0(VIRGL_CCMD_CREATE_OBJECT, VIRGL_OBJECT_SURFACE, 5)
        && words[6] == virgl_cmd0(VIRGL_CCMD_SET_FRAMEBUFFER_STATE, 0, 3)
        && words[10] == virgl_cmd0(VIRGL_CCMD_CLEAR, 0, 8)
        && words[12] == 1.0f32.to_bits()
        && words[15] == 1.0f32.to_bits()
        && words[19] == virgl_cmd0(VIRGL_CCMD_DESTROY_OBJECT, VIRGL_OBJECT_SURFACE, 1)
}

#[inline]
unsafe fn as_bytes<T>(val: &T) -> &[u8] {
    // SAFETY: Caller supplies POD `#[repr(C)]` request structs.
    unsafe { core::slice::from_raw_parts((val as *const T).cast::<u8>(), size_of::<T>()) }
}

/// Execute a live VirtIO-GPU 3D Virgl smoke test when `VIRTIO_GPU_F_VIRGL` is negotiated.
///
/// Never fails 2D initialization on error; cleans up any temporary DMA page and 3D context/resource
/// before returning so frame leak accounting stays at `0`.
///
/// # Safety
/// Must be called from `dry-gpu` with a valid initialized `VirtioGpu2d` and `GPU_CMD_VADDR` mapping.
pub unsafe fn run_virgl_3d_smoke_test(dev: &mut VirtioGpu2d) -> Virtio3dSmokeResult {
    let mut res = Virtio3dSmokeResult {
        unit_ok: verify_virtio_3d_protocol_invariants(),
        ..Virtio3dSmokeResult::default()
    };
    if !res.unit_ok || !dev.probe_3d.virgl_supported || dev.probe_3d.num_capsets == 0 {
        return res;
    }

    const CTX_ID: u32 = 1;
    const RES_ID_3D: u32 = 2;
    const SURF_HANDLE: u32 = 1;
    const WIDTH: u32 = 16;
    const HEIGHT: u32 = 16;
    const STRIDE: u32 = WIDTH * 4;
    const BUF_LEN: u32 = STRIDE * HEIGHT;

    let Ok((buf_cap, buf_pa)) = sys_dma_alloc(dev.pci_cap(), GPU_3D_BUF_VADDR) else {
        return res;
    };
    // SAFETY: `GPU_3D_BUF_VADDR` is a freshly allocated 4 KiB `USER_RW` DMA frame.
    unsafe {
        write_bytes(GPU_3D_BUF_VADDR as *mut u8, 0, 4096);
    }
    res.executed = true;

    let mut debug_name = [0u8; 64];
    debug_name[..10].copy_from_slice(b"dryos-3d\0\0");
    let ctx_req = VirtioGpuCtxCreate {
        hdr: VirtioGpuCtrlHdr::with_ctx_fence(VIRTIO_GPU_CMD_CTX_CREATE, CTX_ID, 0),
        nlen: 8,
        context_init: 0,
        debug_name,
    };
    // SAFETY: Pod request passed to `VirtioGpu2d::submit_raw_cmd`.
    res.ctx_ok = unsafe {
        dev.submit_raw_cmd(as_bytes(&ctx_req), size_of::<VirtioGpuCtrlHdr>() as u32)
            == Ok(VIRTIO_GPU_RESP_OK_NODATA)
    };

    if res.ctx_ok {
        let create_req = VirtioGpuResourceCreate3d {
            hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_CREATE_3D),
            resource_id: RES_ID_3D,
            target: PIPE_TEXTURE_2D,
            format: VIRGL_FORMAT_B8G8R8A8_UNORM,
            bind: VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW,
            width: WIDTH,
            height: HEIGHT,
            depth: 1,
            array_size: 1,
            last_level: 0,
            nr_samples: 0,
            flags: 0,
            padding: 0,
        };
        let r_create = unsafe {
            dev.submit_raw_cmd(as_bytes(&create_req), size_of::<VirtioGpuCtrlHdr>() as u32)
                == Ok(VIRTIO_GPU_RESP_OK_NODATA)
        };
        let r_attach_backing = r_create
            && unsafe { dev.attach_backing_single_page(RES_ID_3D, buf_pa, BUF_LEN) }
                == Ok(VIRTIO_GPU_RESP_OK_NODATA);
        let attach_ctx = VirtioGpuCtxResource {
            hdr: VirtioGpuCtrlHdr::with_ctx_fence(VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE, CTX_ID, 0),
            resource_id: RES_ID_3D,
            padding: 0,
        };
        let r_attach_ctx = r_attach_backing
            && unsafe {
                dev.submit_raw_cmd(as_bytes(&attach_ctx), size_of::<VirtioGpuCtrlHdr>() as u32)
                    == Ok(VIRTIO_GPU_RESP_OK_NODATA)
            };
        res.res_ok = r_attach_ctx;

        if res.res_ok {
            let (cmd_words, cmd_bytes) = encode_virgl_clear_cmdbuf(SURF_HANDLE, RES_ID_3D, [1.0, 0.0, 0.0, 1.0]);
            let submit_hdr = VirtioGpuCmdSubmit {
                hdr: VirtioGpuCtrlHdr::with_ctx_fence_flag(
                    VIRTIO_GPU_CMD_SUBMIT_3D,
                    CTX_ID,
                    1,
                    VIRTIO_GPU_FLAG_FENCE,
                ),
                size: cmd_bytes as u32,
                padding: 0,
            };
            let mut pkt = [0u8; size_of::<VirtioGpuCmdSubmit>() + 84];
            let hdr_slice = unsafe { as_bytes(&submit_hdr) };
            pkt[..size_of::<VirtioGpuCmdSubmit>()].copy_from_slice(hdr_slice);
            let stream_bytes = unsafe {
                core::slice::from_raw_parts(cmd_words.as_ptr().cast::<u8>(), cmd_bytes)
            };
            pkt[size_of::<VirtioGpuCmdSubmit>()..].copy_from_slice(stream_bytes);

            res.submit_fenced_ok = unsafe {
                dev.submit_raw_cmd(&pkt, size_of::<VirtioGpuCtrlHdr>() as u32)
                    == Ok(VIRTIO_GPU_RESP_OK_NODATA)
            };

            if res.submit_fenced_ok {
                let xfer = VirtioGpuTransferHost3d {
                    hdr: VirtioGpuCtrlHdr::with_ctx_fence_flag(
                        VIRTIO_GPU_CMD_TRANSFER_FROM_HOST_3D,
                        CTX_ID,
                        2,
                        VIRTIO_GPU_FLAG_FENCE,
                    ),
                    box_3d: VirtioGpuBox {
                        x: 0,
                        y: 0,
                        z: 0,
                        w: WIDTH,
                        h: HEIGHT,
                        d: 1,
                    },
                    offset: 0,
                    resource_id: RES_ID_3D,
                    level: 0,
                    stride: STRIDE,
                    layer_stride: 0,
                };
                match unsafe {
                    dev.submit_raw_cmd(as_bytes(&xfer), size_of::<VirtioGpuCtrlHdr>() as u32)
                } {
                    Ok(rk) => {
                        res.xfer_resp_kind = rk;
                        if rk == VIRTIO_GPU_RESP_OK_NODATA {
                            let px = unsafe { read_volatile(GPU_3D_BUF_VADDR as *const u32) };
                            res.readback_pixel = px;
                            // B8G8R8A8_UNORM (R=1.0, G=0.0, B=0.0, A=1.0) => 0xFFFF_0000 little-endian.
                            res.pixel_match = px == 0xFFFF_0000 || (px & 0x00FF_FFFF) == 0x00FF_0000;
                        }
                    }
                    Err(e) => {
                        res.xfer_resp_kind = e as u32;
                    }
                }
            }

            // Negative test: truncated/oversized `SUBMIT_3D` (`size=1024` inside 32 B packet)
            // or unknown command opcode (`0x02FF`) must return a VirtIO-GPU error (>= 0x1200).
            let bad_submit = VirtioGpuCmdSubmit {
                hdr: VirtioGpuCtrlHdr::with_ctx_fence(VIRTIO_GPU_CMD_SUBMIT_3D, CTX_ID, 0),
                size: 1024,
                padding: 0,
            };
            if let Ok(err_kind) =
                unsafe { dev.submit_raw_cmd(as_bytes(&bad_submit), size_of::<VirtioGpuCtrlHdr>() as u32) }
            {
                if err_kind >= VIRTIO_GPU_RESP_ERR_UNSPEC {
                    res.neg_err_kind = err_kind;
                }
            }
            if res.neg_err_kind == 0 {
                let bad_op = VirtioGpuCtrlHdr::with_ctx_fence(0x02FF, CTX_ID, 0);
                if let Ok(err_kind) =
                    unsafe { dev.submit_raw_cmd(as_bytes(&bad_op), size_of::<VirtioGpuCtrlHdr>() as u32) }
                {
                    if err_kind >= VIRTIO_GPU_RESP_ERR_UNSPEC {
                        res.neg_err_kind = err_kind;
                    }
                }
            }

            let detach = VirtioGpuCtxResource {
                hdr: VirtioGpuCtrlHdr::with_ctx_fence(VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE, CTX_ID, 0),
                resource_id: RES_ID_3D,
                padding: 0,
            };
            let _ = unsafe {
                dev.submit_raw_cmd(as_bytes(&detach), size_of::<VirtioGpuCtrlHdr>() as u32)
            };
        }

        if r_create {
            let _ = unsafe { dev.unref_resource(RES_ID_3D) };
        }
        let destroy = VirtioGpuCtrlHdr::with_ctx_fence(VIRTIO_GPU_CMD_CTX_DESTROY, CTX_ID, 0);
        let d_ok = unsafe {
            dev.submit_raw_cmd(as_bytes(&destroy), size_of::<VirtioGpuCtrlHdr>() as u32)
                == Ok(VIRTIO_GPU_RESP_OK_NODATA)
        };
        res.cleaned_up = d_ok;
    }

    let _ = sys_vspace_map(buf_cap, GPU_3D_BUF_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let _ = sys_cap_revoke(buf_cap, 2);
    if !res.ctx_ok {
        res.cleaned_up = true;
    }
    res
}
