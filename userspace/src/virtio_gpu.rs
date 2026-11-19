// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 VirtIO 1.0 Modern PCI (`1AF4:1050`) 2D Scanout Driver over `SYS_DEV_MAP` &
//! 2 MiB Pinned `DEVICE_DMA` (`DMA_ALLOC_FLAG_2M`) Frames.

use core::ptr::{copy_nonoverlapping, read_volatile, write_bytes, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::abi_ext::{sys_dev_map, sys_dma_alloc, sys_dma_alloc_2m, UserVirtioPciInfo};
use crate::{
    sys_cap_revoke, sys_irq_ack, sys_irq_bind, sys_notify_create, sys_notify_wait, sys_vspace_map,
    IRQ_ACK_UNBIND, IRQ_ACK_UNMASK, IRQF_ONESHOT, MSG_FLAG_NON_BLOCKING, VSPACE_MAP_OP_UNMAP_2M,
    VSPACE_MAP_OP_UNMAP_4K,
};

const GPU_BAR_VADDR: u64 = 0x2480_0000;
const GPU_VQ_VADDR: u64 = 0x2481_0000;
const GPU_CMD_VADDR: u64 = 0x2481_1000;
pub const GPU_SCANOUT_VA0: u64 = 0x2500_0000;
pub const MAX_GPU_SCANOUT_2M: usize = 5;
const TWO_MIB: usize = 2 * 1024 * 1024;

const VQ_MAX_SIZE: u16 = 16;
const VQ_DESC_OFF: u64 = 0x000;
const VQ_AVAIL_OFF: u64 = 0x400;
const VQ_USED_OFF: u64 = 0x800;
const CMD_RESP_OFF: u64 = 0x400;

const VIRTIO_STATUS_ACK: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
const VIRTIO_STATUS_FAILED: u8 = 128;
const VIRTIO_F_VERSION_1_HI: u32 = 1 << 0;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const VIRTIO_GPU_CMD_GET_CAPSET_INFO: u32 = 0x0108;
const VIRTIO_GPU_CMD_GET_CAPSET: u32 = 0x0109;

const VIRTIO_GPU_F_VIRGL: u32 = 1 << 0;

pub(crate) const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const VIRTIO_GPU_RESP_OK_CAPSET_INFO: u32 = 0x1102;
const VIRTIO_GPU_RESP_OK_CAPSET: u32 = 0x1103;
// Little-endian [B, G, R, X] matches UEFI GOP red_shift=16, green_shift=8, blue_shift=0.
const VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: u32 = 2;
const GPU_RES_ID_PRIMARY: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuCtrlHdr {
    kind: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    padding: u32,
}

impl VirtioGpuCtrlHdr {
    pub(crate) const fn new(kind: u32) -> Self {
        Self {
            kind,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            padding: 0,
        }
    }

    pub(crate) const fn with_ctx_fence(kind: u32, ctx_id: u32, fence_id: u64) -> Self {
        Self {
            kind,
            flags: 0,
            fence_id,
            ctx_id,
            padding: 0,
        }
    }

    pub(crate) const fn with_ctx_fence_flag(
        kind: u32,
        ctx_id: u32,
        fence_id: u64,
        flags: u32,
    ) -> Self {
        Self {
            kind,
            flags,
            fence_id,
            ctx_id,
            padding: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuDisplayOne {
    r: VirtioGpuRect,
    enabled: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuRespDisplayInfo {
    hdr: VirtioGpuCtrlHdr,
    pmodes: [VirtioGpuDisplayOne; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuResourceCreate2d {
    hdr: VirtioGpuCtrlHdr,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuResourceUnref {
    hdr: VirtioGpuCtrlHdr,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuResourceAttachBacking {
    hdr: VirtioGpuCtrlHdr,
    resource_id: u32,
    nr_entries: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuMemEntry {
    addr: u64,
    length: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuSetScanout {
    hdr: VirtioGpuCtrlHdr,
    r: VirtioGpuRect,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuTransferToHost2d {
    hdr: VirtioGpuCtrlHdr,
    r: VirtioGpuRect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuResourceFlush {
    hdr: VirtioGpuCtrlHdr,
    r: VirtioGpuRect,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuGetCapsetInfo {
    hdr: VirtioGpuCtrlHdr,
    capset_index: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuRespCapsetInfo {
    hdr: VirtioGpuCtrlHdr,
    capset_id: u32,
    capset_max_version: u32,
    capset_max_size: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioGpuGetCapset {
    hdr: VirtioGpuCtrlHdr,
    capset_id: u32,
    capset_version: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Virtio3dProbeInfo {
    pub virgl_supported: bool,
    pub num_capsets: u32,
    pub first_capset_id: u32,
    pub max_version: u32,
    pub max_size: u32,
    pub capset_resp_bytes: u32,
}

pub struct VirtioGpu2d {
    common_base: u64,
    notify_q0_addr: u64,
    isr_addr: u64,
    device_base: u64,
    pci_cap: u64,
    irq_notif: u64,
    cmd_pa: u64,
    vq_cap: u64,
    cmd_cap: u64,
    scanout_caps: [u64; MAX_GPU_SCANOUT_2M],
    scanout_pas: [u64; MAX_GPU_SCANOUT_2M],
    pub num_scanout_2m: usize,
    queue_size: u16,
    avail_idx: u16,
    last_used_idx: u16,
    pub msix_vec: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub width: u32,
    pub height: u32,
    pub virtq_cmds: u64,
    pub msi_irqs: u64,
    pub transfer_bytes: u64,
    pub probe_3d: Virtio3dProbeInfo,
    pub smoke_3d: crate::virtio_gpu_3d::Virtio3dSmokeResult,
}

#[inline]
unsafe fn mmio_r8(addr: u64) -> u8 {
    unsafe { read_volatile(addr as *const u8) }
}
#[inline]
unsafe fn mmio_w8(addr: u64, v: u8) {
    unsafe { write_volatile(addr as *mut u8, v) }
}
#[inline]
unsafe fn mmio_r16(addr: u64) -> u16 {
    unsafe { read_volatile(addr as *const u16) }
}
#[inline]
unsafe fn mmio_w16(addr: u64, v: u16) {
    unsafe { write_volatile(addr as *mut u16, v) }
}
#[inline]
unsafe fn mmio_r32(addr: u64) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}
#[inline]
unsafe fn mmio_w32(addr: u64, v: u32) {
    unsafe { write_volatile(addr as *mut u32, v) }
}
#[inline]
unsafe fn mmio_w64_split(addr: u64, v: u64) {
    unsafe {
        mmio_w32(addr, v as u32);
        mmio_w32(addr + 4, (v >> 32) as u32);
    }
}

impl VirtioGpu2d {
    pub fn init(pci_cap: u64) -> Result<Self, i32> {
        let mut info = UserVirtioPciInfo::default();
        sys_dev_map(pci_cap, GPU_BAR_VADDR, &mut info).map_err(|_| 191)?;

        let (vq_cap, vq_pa) = sys_dma_alloc(pci_cap, GPU_VQ_VADDR).map_err(|_| 192)?;
        let (cmd_cap, cmd_pa) = match sys_dma_alloc(pci_cap, GPU_CMD_VADDR) {
            Ok(pair) => pair,
            Err(_) => {
                let _ = sys_vspace_map(vq_cap, GPU_VQ_VADDR, VSPACE_MAP_OP_UNMAP_4K);
                let _ = sys_cap_revoke(vq_cap, 2);
                return Err(192);
            }
        };
        let cleanup_init_dma = || {
            let _ = sys_vspace_map(cmd_cap, GPU_CMD_VADDR, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(cmd_cap, 2);
            let _ = sys_vspace_map(vq_cap, GPU_VQ_VADDR, VSPACE_MAP_OP_UNMAP_4K);
            let _ = sys_cap_revoke(vq_cap, 2);
        };

        // SAFETY: DMA pages are freshly mapped `USER_RW` exclusively inside `dry-gpu`.
        unsafe {
            write_bytes(GPU_VQ_VADDR as *mut u8, 0, 4096);
            write_bytes(GPU_CMD_VADDR as *mut u8, 0, 4096);

            let common = GPU_BAR_VADDR + info.common_off as u64;
            mmio_w8(common + 0x14, 0);
            for _ in 0..10_000 {
                if mmio_r8(common + 0x14) == 0 {
                    break;
                }
                core::hint::spin_loop();
            }

            let mut status = VIRTIO_STATUS_ACK;
            mmio_w8(common + 0x14, status);
            status |= VIRTIO_STATUS_DRIVER;
            mmio_w8(common + 0x14, status);

            mmio_w32(common, 0);
            let dev_feat_lo = mmio_r32(common + 0x04);
            let virgl_supported = (dev_feat_lo & VIRTIO_GPU_F_VIRGL) != 0;

            mmio_w32(common, 1);
            let dev_feat_hi = mmio_r32(common + 0x04);
            if (dev_feat_hi & VIRTIO_F_VERSION_1_HI) == 0 {
                mmio_w8(common + 0x14, VIRTIO_STATUS_FAILED);
                cleanup_init_dma();
                return Err(194);
            }
            mmio_w32(common, 0);
            mmio_w32(common + 0x08, 0);
            mmio_w32(common + 0x0c, if virgl_supported { VIRTIO_GPU_F_VIRGL } else { 0 });
            mmio_w32(common + 0x08, 1);
            mmio_w32(common + 0x0c, dev_feat_hi & 0x3);

            status |= VIRTIO_STATUS_FEATURES_OK;
            mmio_w8(common + 0x14, status);
            if (mmio_r8(common + 0x14) & VIRTIO_STATUS_FEATURES_OK) == 0 {
                mmio_w8(common + 0x14, VIRTIO_STATUS_FAILED);
                cleanup_init_dma();
                return Err(194);
            }

            mmio_w16(common + 0x16, 0); // controlq
            let max_qsz = mmio_r16(common + 0x18);
            if max_qsz < 2 {
                cleanup_init_dma();
                return Err(195);
            }
            let mut qsz = max_qsz.min(VQ_MAX_SIZE);
            if !qsz.is_power_of_two() {
                qsz = 8;
            }
            mmio_w16(common + 0x18, qsz);
            qsz = mmio_r16(common + 0x18);
            if !(2..=VQ_MAX_SIZE).contains(&qsz) {
                cleanup_init_dma();
                return Err(195);
            }

            mmio_w64_split(common + 0x20, vq_pa + VQ_DESC_OFF);
            mmio_w64_split(common + 0x28, vq_pa + VQ_AVAIL_OFF);
            mmio_w64_split(common + 0x30, vq_pa + VQ_USED_OFF);
            let q_notify_off = mmio_r16(common + 0x1e) as u64;

            let mut irq_notif = 0u64;
            let mut msix_vec = 0u8;
            if let Ok(ncap) = sys_notify_create(0) {
                match sys_irq_bind(pci_cap, ncap, IRQF_ONESHOT) {
                    Ok(vec) => {
                        irq_notif = ncap;
                        msix_vec = vec;
                    }
                    Err(_) => {
                        let _ = sys_cap_revoke(ncap, 2);
                    }
                }
            }
            mmio_w16(common + 0x10, 0xffff);
            if irq_notif != 0 {
                mmio_w16(common + 0x1a, 0);
                if mmio_r16(common + 0x1a) == 0xffff {
                    let _ = sys_irq_ack(pci_cap, IRQ_ACK_UNBIND);
                    let _ = sys_cap_revoke(irq_notif, 2);
                    irq_notif = 0;
                    msix_vec = 0;
                }
            }

            mmio_w16(common + 0x1c, 1);

            status |= VIRTIO_STATUS_DRIVER_OK;
            mmio_w8(common + 0x14, status);

            let mut dev = Self {
                common_base: common,
                notify_q0_addr: GPU_BAR_VADDR
                    + info.notify_off as u64
                    + q_notify_off * info.notify_mult as u64,
                isr_addr: GPU_BAR_VADDR + info.isr_off as u64,
                device_base: if info.device_off != 0 {
                    GPU_BAR_VADDR + info.device_off as u64
                } else {
                    0
                },
                pci_cap,
                irq_notif,
                cmd_pa,
                vq_cap,
                cmd_cap,
                scanout_caps: [0; MAX_GPU_SCANOUT_2M],
                scanout_pas: [0; MAX_GPU_SCANOUT_2M],
                num_scanout_2m: 0,
                queue_size: qsz,
                avail_idx: 0,
                last_used_idx: 0,
                msix_vec,
                vendor_id: info.vendor_id,
                device_id: info.device_id,
                width: 1280,
                height: 800,
                virtq_cmds: 0,
                msi_irqs: 0,
                transfer_bytes: 0,
                probe_3d: Virtio3dProbeInfo {
                    virgl_supported,
                    ..Virtio3dProbeInfo::default()
                },
                smoke_3d: crate::virtio_gpu_3d::Virtio3dSmokeResult::default(),
            };

            if let Err(err) = dev.setup_scanout_2d() {
                dev.shutdown_clean();
                return Err(err);
            }
            dev.probe_3d_capsets();
            dev.smoke_3d = crate::virtio_gpu_3d::run_virgl_3d_smoke_test(&mut dev);
            Ok(dev)
        }
    }

    unsafe fn submit_sync(&mut self, req_len: u32, resp_len: u32) -> Result<u32, i32> {
        let qmask = (self.queue_size - 1) as usize;
        let d0 = (self.avail_idx as usize * 2) & qmask;
        let d1 = (d0 + 1) & qmask;
        let descs = (GPU_VQ_VADDR + VQ_DESC_OFF) as *mut VirtqDesc;
        // SAFETY: `d0` and `d1` are valid descriptors within `controlq`; `cmd_pa` points to 4 KiB DMA frame.
        unsafe {
            write_volatile(
                descs.add(d0),
                VirtqDesc {
                    addr: self.cmd_pa,
                    len: req_len,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: d1 as u16,
                },
            );
            write_volatile(
                descs.add(d1),
                VirtqDesc {
                    addr: self.cmd_pa + CMD_RESP_OFF,
                    len: resp_len,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );

            let avail_base = GPU_VQ_VADDR + VQ_AVAIL_OFF;
            let ring_slot = (self.avail_idx as usize) & qmask;
            write_volatile((avail_base + 4 + (ring_slot as u64) * 2) as *mut u16, d0 as u16);
            fence(Ordering::Release);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            write_volatile((avail_base + 2) as *mut u16, self.avail_idx);
            fence(Ordering::SeqCst);

            mmio_w16(self.notify_q0_addr, 0);

            let used_idx_ptr = (GPU_VQ_VADDR + VQ_USED_OFF + 2) as *const u16;
            if self.irq_notif != 0 {
                for _attempt in 0..4u32 {
                    match sys_notify_wait(self.irq_notif, 0, 20_000_000) {
                        Ok(bits) if bits != 0 => {
                            let _ = sys_irq_ack(self.pci_cap, IRQ_ACK_UNMASK);
                            self.msi_irqs = self.msi_irqs.wrapping_add(1);
                            fence(Ordering::Acquire);
                            if read_volatile(used_idx_ptr) != self.last_used_idx {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            }

            for _ in 0..4_000_000u32 {
                fence(Ordering::Acquire);
                let hw_used = read_volatile(used_idx_ptr);
                if hw_used != self.last_used_idx {
                    if self.irq_notif != 0 {
                        if let Ok(bits) =
                            sys_notify_wait(self.irq_notif, MSG_FLAG_NON_BLOCKING as u64, 0)
                        {
                            if bits != 0 {
                                let _ = sys_irq_ack(self.pci_cap, IRQ_ACK_UNMASK);
                                self.msi_irqs = self.msi_irqs.wrapping_add(1);
                            }
                        }
                    }
                    let _ = mmio_r8(self.isr_addr);
                    self.last_used_idx = self.last_used_idx.wrapping_add(1);
                    self.virtq_cmds = self.virtq_cmds.wrapping_add(1);
                    let resp_hdr = read_volatile((GPU_CMD_VADDR + CMD_RESP_OFF) as *const VirtioGpuCtrlHdr);
                    return Ok(resp_hdr.kind);
                }
                core::hint::spin_loop();
            }
        }
        Err(196)
    }

    unsafe fn cmd_simple<T: Copy>(&mut self, req: &T, expected_resp: u32) -> Result<(), i32> {
        let req_len = size_of::<T>() as u32;
        // SAFETY: `GPU_CMD_VADDR` owns 4 KiB DMA memory; `+0x400` holds the response.
        unsafe {
            copy_nonoverlapping((req as *const T).cast::<u8>(), GPU_CMD_VADDR as *mut u8, req_len as usize);
            let kind = self.submit_sync(req_len, size_of::<VirtioGpuCtrlHdr>() as u32)?;
            if kind != expected_resp {
                return Err(197);
            }
        }
        Ok(())
    }

    pub(crate) fn pci_cap(&self) -> u64 {
        self.pci_cap
    }

    pub(crate) unsafe fn submit_raw_cmd(&mut self, req_bytes: &[u8], resp_len: u32) -> Result<u32, i32> {
        if req_bytes.len() > CMD_RESP_OFF as usize {
            return Err(197);
        }
        // SAFETY: Caller ensures valid command bytes fitting in `[+0x000..+0x400)` of `GPU_CMD_VADDR`.
        unsafe {
            copy_nonoverlapping(req_bytes.as_ptr(), GPU_CMD_VADDR as *mut u8, req_bytes.len());
            self.submit_sync(req_bytes.len() as u32, resp_len)
        }
    }

    pub(crate) unsafe fn attach_backing_single_page(
        &mut self,
        res_id: u32,
        pa: u64,
        len: u32,
    ) -> Result<u32, i32> {
        #[repr(C)]
        struct SingleAttach {
            hdr: VirtioGpuResourceAttachBacking,
            ent: VirtioGpuMemEntry,
        }
        let pkt = SingleAttach {
            hdr: VirtioGpuResourceAttachBacking {
                hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
                resource_id: res_id,
                nr_entries: 1,
            },
            ent: VirtioGpuMemEntry {
                addr: pa,
                length: len,
                padding: 0,
            },
        };
        // SAFETY: POD packet fits cleanly inside `[+0x000..+0x400)` of `GPU_CMD_VADDR`.
        unsafe {
            copy_nonoverlapping((&raw const pkt).cast::<u8>(), GPU_CMD_VADDR as *mut u8, size_of::<SingleAttach>());
            self.submit_sync(size_of::<SingleAttach>() as u32, size_of::<VirtioGpuCtrlHdr>() as u32)
        }
    }

    pub(crate) unsafe fn unref_resource(&mut self, res_id: u32) -> Result<(), i32> {
        unsafe {
            self.cmd_simple(
                &VirtioGpuResourceUnref {
                    hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_UNREF),
                    resource_id: res_id,
                    padding: 0,
                },
                VIRTIO_GPU_RESP_OK_NODATA,
            )
        }
    }

    unsafe fn setup_scanout_2d(&mut self) -> Result<(), i32> {
        // 1. Query display pmode[0] (defaulting safely to 1280x800 if disabled on cold scanout).
        let hdr = VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_GET_DISPLAY_INFO);
        // SAFETY: `CMD` page holds header at `+0x000` and `VirtioGpuRespDisplayInfo` at `+0x400`.
        unsafe {
            write_volatile(GPU_CMD_VADDR as *mut VirtioGpuCtrlHdr, hdr);
            let kind = self.submit_sync(
                size_of::<VirtioGpuCtrlHdr>() as u32,
                size_of::<VirtioGpuRespDisplayInfo>() as u32,
            )?;
            if kind == VIRTIO_GPU_RESP_OK_DISPLAY_INFO {
                let disp = read_volatile((GPU_CMD_VADDR + CMD_RESP_OFF) as *const VirtioGpuRespDisplayInfo);
                if disp.pmodes[0].enabled != 0
                    && disp.pmodes[0].r.width >= 320
                    && disp.pmodes[0].r.height >= 240
                    && (disp.pmodes[0].r.width as usize) * (disp.pmodes[0].r.height as usize) * 4
                        <= TWO_MIB * MAX_GPU_SCANOUT_2M
                {
                    self.width = disp.pmodes[0].r.width;
                    self.height = disp.pmodes[0].r.height;
                }
            }

            // 2. Allocate `1..=MAX_GPU_SCANOUT_2M` pinned 2 MiB `DEVICE_DMA` scanout pages for live resolution.
            let total_bytes = (self.width as usize) * (self.height as usize) * 4;
            let need_2m = total_bytes.div_ceil(TWO_MIB).clamp(1, MAX_GPU_SCANOUT_2M);
            for i in 0..need_2m {
                let va = GPU_SCANOUT_VA0 + (i as u64) * (TWO_MIB as u64);
                let (s_cap, s_pa) = sys_dma_alloc_2m(self.pci_cap, va).map_err(|_| 193)?;
                self.scanout_caps[i] = s_cap;
                self.scanout_pas[i] = s_pa;
                self.num_scanout_2m = i + 1;
                write_bytes(va as *mut u8, 0, TWO_MIB);
            }

            // 3. Create 2D resource in `B8G8R8X8_UNORM` (`id = 1`).
            self.cmd_simple(
                &VirtioGpuResourceCreate2d {
                    hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_CREATE_2D),
                    resource_id: GPU_RES_ID_PRIMARY,
                    format: VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
                    width: self.width,
                    height: self.height,
                },
                VIRTIO_GPU_RESP_OK_NODATA,
            )?;

            // 4. Attach pinned 2 MiB `DEVICE_DMA` backing scatterlist entries (`pa0..paN`).
            let nr_entries = need_2m as u32;
            let attach = VirtioGpuResourceAttachBacking {
                hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
                resource_id: GPU_RES_ID_PRIMARY,
                nr_entries,
            };
            let hdr_sz = size_of::<VirtioGpuResourceAttachBacking>();
            let ent_sz = size_of::<VirtioGpuMemEntry>();
            write_volatile(GPU_CMD_VADDR as *mut VirtioGpuResourceAttachBacking, attach);
            let mut rem = total_bytes;
            for (i, &addr) in self.scanout_pas.iter().enumerate().take(need_2m) {
                let seg_len = rem.min(TWO_MIB) as u32;
                rem = rem.saturating_sub(TWO_MIB);
                write_volatile(
                    (GPU_CMD_VADDR + (hdr_sz + i * ent_sz) as u64) as *mut VirtioGpuMemEntry,
                    VirtioGpuMemEntry {
                        addr,
                        length: seg_len,
                        padding: 0,
                    },
                );
            }
            let resp_kind = self.submit_sync(
                (hdr_sz + (nr_entries as usize) * ent_sz) as u32,
                size_of::<VirtioGpuCtrlHdr>() as u32,
            )?;
            if resp_kind != VIRTIO_GPU_RESP_OK_NODATA {
                return Err(198);
            }

            // 4. Bind primary scanout 0 to resource 1.
            self.cmd_simple(
                &VirtioGpuSetScanout {
                    hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_SET_SCANOUT),
                    r: VirtioGpuRect {
                        x: 0,
                        y: 0,
                        width: self.width,
                        height: self.height,
                    },
                    scanout_id: 0,
                    resource_id: GPU_RES_ID_PRIMARY,
                },
                VIRTIO_GPU_RESP_OK_NODATA,
            )?;
        }
        Ok(())
    }

    pub fn transfer_and_flush_rows(&mut self, y: u32, h: u32) -> Result<(), i32> {
        if h == 0 || y >= self.height {
            return Ok(());
        }
        let clamped_h = h.min(self.height - y);
        let offset = (y as u64) * (self.width as u64) * 4;
        // SAFETY: Both `TRANSFER_TO_HOST_2D` and `RESOURCE_FLUSH` target clamped scanlines `[y..y+h)`.
        unsafe {
            self.cmd_simple(
                &VirtioGpuTransferToHost2d {
                    hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D),
                    r: VirtioGpuRect {
                        x: 0,
                        y,
                        width: self.width,
                        height: clamped_h,
                    },
                    offset,
                    resource_id: GPU_RES_ID_PRIMARY,
                    padding: 0,
                },
                VIRTIO_GPU_RESP_OK_NODATA,
            )?;
            self.cmd_simple(
                &VirtioGpuResourceFlush {
                    hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_FLUSH),
                    r: VirtioGpuRect {
                        x: 0,
                        y,
                        width: self.width,
                        height: clamped_h,
                    },
                    resource_id: GPU_RES_ID_PRIMARY,
                    padding: 0,
                },
                VIRTIO_GPU_RESP_OK_NODATA,
            )?;
        }
        self.transfer_bytes = self
            .transfer_bytes
            .saturating_add((clamped_h as u64) * (self.width as u64) * 4);
        Ok(())
    }

    /// Probe VirtIO-GPU 3D capset metadata read-only; never fails 2D operation when absent.
    pub fn probe_3d_capsets(&mut self) {
        if !self.probe_3d.virgl_supported || self.device_base == 0 {
            return;
        }
        // SAFETY: `device_base` points into BAR4 VirtIO-GPU device config (`num_capsets` at `+0x0c`).
        unsafe {
            let num_capsets = mmio_r32(self.device_base + 0x0c);
            self.probe_3d.num_capsets = num_capsets;
            if num_capsets == 0 {
                return;
            }
            let req = VirtioGpuGetCapsetInfo {
                hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_GET_CAPSET_INFO),
                capset_index: 0,
                padding: 0,
            };
            copy_nonoverlapping(
                (&raw const req).cast::<u8>(),
                GPU_CMD_VADDR as *mut u8,
                size_of::<VirtioGpuGetCapsetInfo>(),
            );
            if let Ok(VIRTIO_GPU_RESP_OK_CAPSET_INFO) = self.submit_sync(
                size_of::<VirtioGpuGetCapsetInfo>() as u32,
                size_of::<VirtioGpuRespCapsetInfo>() as u32,
            ) {
                let info =
                    read_volatile((GPU_CMD_VADDR + CMD_RESP_OFF) as *const VirtioGpuRespCapsetInfo);
                self.probe_3d.first_capset_id = info.capset_id;
                self.probe_3d.max_version = info.capset_max_version;
                self.probe_3d.max_size = info.capset_max_size;
                let cap_bytes = info.capset_max_size.min(1024);
                let creq = VirtioGpuGetCapset {
                    hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_GET_CAPSET),
                    capset_id: info.capset_id,
                    capset_version: info.capset_max_version,
                };
                copy_nonoverlapping(
                    (&raw const creq).cast::<u8>(),
                    GPU_CMD_VADDR as *mut u8,
                    size_of::<VirtioGpuGetCapset>(),
                );
                if let Ok(VIRTIO_GPU_RESP_OK_CAPSET) = self.submit_sync(
                    size_of::<VirtioGpuGetCapset>() as u32,
                    (size_of::<VirtioGpuCtrlHdr>() as u32).saturating_add(cap_bytes),
                ) {
                    self.probe_3d.capset_resp_bytes = cap_bytes;
                }
            }
        }
    }

    pub fn shutdown_clean(&mut self) {
        // SAFETY: Tears down scanout resource and resets VirtIO-GPU hardware cleanly before cap revocation.
        unsafe {
            let _ = self.cmd_simple(
                &VirtioGpuResourceUnref {
                    hdr: VirtioGpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_UNREF),
                    resource_id: GPU_RES_ID_PRIMARY,
                    padding: 0,
                },
                VIRTIO_GPU_RESP_OK_NODATA,
            );
            mmio_w8(self.common_base + 0x14, 0);
        }
        if self.irq_notif != 0 {
            let _ = sys_irq_ack(self.pci_cap, IRQ_ACK_UNBIND);
            let _ = sys_cap_revoke(self.irq_notif, 2);
            self.irq_notif = 0;
            self.msix_vec = 0;
        }
        let _ = sys_vspace_map(self.vq_cap, GPU_VQ_VADDR, VSPACE_MAP_OP_UNMAP_4K);
        let _ = sys_vspace_map(self.cmd_cap, GPU_CMD_VADDR, VSPACE_MAP_OP_UNMAP_4K);
        let _ = sys_cap_revoke(self.vq_cap, 1);
        let _ = sys_cap_revoke(self.cmd_cap, 1);
        for i in 0..self.num_scanout_2m {
            let cap = self.scanout_caps[i];
            if cap != 0 {
                let va = GPU_SCANOUT_VA0 + (i as u64) * (TWO_MIB as u64);
                let _ = sys_vspace_map(cap, va, VSPACE_MAP_OP_UNMAP_2M);
                let _ = sys_cap_revoke(cap, 1);
                self.scanout_caps[i] = 0;
            }
        }
        self.num_scanout_2m = 0;
    }
}
