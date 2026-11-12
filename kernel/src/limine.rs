// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Limine Boot Protocol (`limine.h` @ `6e85ddd6f928`, Base Revision 3) request & response types.

use core::cell::UnsafeCell;
use core::ffi::c_char;
use core::mem::{offset_of, size_of};
use core::ptr::read_volatile;
use core::sync::atomic::AtomicU64;

pub const LIMINE_COMMON_MAGIC_0: u64 = 0xc7b1_dd30_df4c_8b88;
pub const LIMINE_COMMON_MAGIC_1: u64 = 0x0a82_e883_a194_f07b;

pub const LIMINE_FIRMWARE_EFI64: u64 = 2;

pub const LIMINE_MEMMAP_USABLE: u64 = 0;
pub const LIMINE_MEMMAP_RESERVED: u64 = 1;
pub const LIMINE_MEMMAP_ACPI_RECLAIMABLE: u64 = 2;
pub const LIMINE_MEMMAP_ACPI_NVS: u64 = 3;
pub const LIMINE_MEMMAP_BAD_MEMORY: u64 = 4;
pub const LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE: u64 = 5;
pub const LIMINE_MEMMAP_EXECUTABLE_AND_MODULES: u64 = 6;
pub const LIMINE_MEMMAP_FRAMEBUFFER: u64 = 7;

#[repr(transparent)]
pub struct BootSlot<T>(pub UnsafeCell<T>);

// SAFETY: Limine populates `.limine_requests` strictly before `_start` runs on the BSP,
// after which the kernel only performs volatile reads on response fields.
unsafe impl<T> Sync for BootSlot<T> {}

impl<T: Copy> BootSlot<T> {
    #[inline]
    pub fn read(&self) -> T {
        // SAFETY: Pointer is valid static storage populated by bootloader before `_start`.
        unsafe { read_volatile(self.0.get()) }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BootloaderInfoResponse {
    pub revision: u64,
    pub name: *const c_char,
    pub version: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BootloaderInfoRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const BootloaderInfoResponse,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FirmwareTypeResponse {
    pub revision: u64,
    pub firmware_type: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FirmwareTypeRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const FirmwareTypeResponse,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExecutableCmdlineResponse {
    pub revision: u64,
    pub cmdline: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExecutableCmdlineRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const ExecutableCmdlineResponse,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HhdmResponse {
    pub revision: u64,
    pub offset: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HhdmRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const HhdmResponse,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub kind: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemmapResponse {
    pub revision: u64,
    pub entry_count: u64,
    pub entries: *const *const MemmapEntry,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemmapRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const MemmapResponse,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExecutableAddressResponse {
    pub revision: u64,
    pub physical_base: u64,
    pub virtual_base: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExecutableAddressRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const ExecutableAddressResponse,
}

#[repr(C)]
pub struct LimineMpInfo {
    pub processor_id: u32,
    pub lapic_id: u32,
    pub reserved: u64,
    pub goto_address: AtomicU64,
    pub extra_argument: u64,
}

const _: () = assert!(size_of::<LimineMpInfo>() == 32);
const _: () = assert!(offset_of!(LimineMpInfo, goto_address) == 16);
const _: () = assert!(offset_of!(LimineMpInfo, extra_argument) == 24);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineMpResponse {
    pub revision: u64,
    pub flags: u32,
    pub bsp_lapic_id: u32,
    pub cpu_count: u64,
    pub cpus: *const *mut LimineMpInfo,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineMpRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const LimineMpResponse,
    pub flags: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineFile {
    pub revision: u64,
    pub address: *const u8,
    pub size: u64,
    pub path: *const c_char,
    pub string: *const c_char,
    pub media_type: u32,
    pub unused: u32,
    pub tftp_ipv4: [u8; 4],
    pub tftp_port: u32,
    pub partition_index: u32,
    pub mbr_disk_id: u32,
    pub gpt_disk_uuid: [u8; 16],
    pub gpt_part_uuid: [u8; 16],
    pub part_uuid: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineModuleResponse {
    pub revision: u64,
    pub module_count: u64,
    pub modules: *const *const LimineFile,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineModuleRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const LimineModuleResponse,
    pub internal_module_count: u64,
    pub internal_modules: *const *const u8,
}

pub const LIMINE_FRAMEBUFFER_RGB: u8 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineFramebuffer {
    pub address: *mut u8,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
    pub unused: [u8; 7],
    pub edid_size: u64,
    pub edid: *const u8,
    pub mode_count: u64,
    pub modes: *const *const u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineFramebufferResponse {
    pub revision: u64,
    pub framebuffer_count: u64,
    pub framebuffers: *const *const LimineFramebuffer,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineFramebufferRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const LimineFramebufferResponse,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineRsdpResponse {
    pub revision: u64,
    pub address: *const u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LimineRsdpRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *const LimineRsdpResponse,
}

#[used]
#[link_section = ".limine_requests_start"]
pub static REQUESTS_START_MARKER: [u64; 4] = [
    0xf6b8_f4b3_9de7_d1ae,
    0xfab9_1a69_40fc_b9cf,
    0x785c_6ed0_15d3_e316,
    0x181e_920a_7852_b9d9,
];

#[used]
#[link_section = ".limine_requests"]
pub static BASE_REVISION: BootSlot<[u64; 3]> =
    BootSlot(UnsafeCell::new([0xf956_2b2d_5c95_a6c8, 0x6a7b_3849_4453_6bdc, 3]));

#[used]
#[link_section = ".limine_requests"]
pub static BOOTLOADER_INFO_REQ: BootSlot<BootloaderInfoRequest> = BootSlot(UnsafeCell::new(
    BootloaderInfoRequest {
        id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0xf550_38d8_e2a1_202f, 0x2794_26fc_f5f5_9740],
        revision: 0,
        response: core::ptr::null(),
    },
));

#[used]
#[link_section = ".limine_requests"]
pub static FIRMWARE_TYPE_REQ: BootSlot<FirmwareTypeRequest> = BootSlot(UnsafeCell::new(
    FirmwareTypeRequest {
        id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0x8c2f_75d9_0bef_28a8, 0x7045_a468_8eac_00c3],
        revision: 0,
        response: core::ptr::null(),
    },
));

#[used]
#[link_section = ".limine_requests"]
pub static CMDLINE_REQ: BootSlot<ExecutableCmdlineRequest> = BootSlot(UnsafeCell::new(
    ExecutableCmdlineRequest {
        id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0x4b16_1536_e598_651e, 0xb390_ad4a_2f1f_303a],
        revision: 0,
        response: core::ptr::null(),
    },
));

#[used]
#[link_section = ".limine_requests"]
pub static HHDM_REQ: BootSlot<HhdmRequest> = BootSlot(UnsafeCell::new(HhdmRequest {
    id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0x48dc_f1cb_8ad2_b852, 0x6398_4e95_9a98_244b],
    revision: 0,
    response: core::ptr::null(),
}));

#[used]
#[link_section = ".limine_requests"]
pub static MEMMAP_REQ: BootSlot<MemmapRequest> = BootSlot(UnsafeCell::new(MemmapRequest {
    id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0x67cf_3d9d_378a_806f, 0xe304_acdf_c50c_3c62],
    revision: 0,
    response: core::ptr::null(),
}));

#[used]
#[link_section = ".limine_requests"]
pub static EXEC_ADDR_REQ: BootSlot<ExecutableAddressRequest> = BootSlot(UnsafeCell::new(
    ExecutableAddressRequest {
        id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0x71ba_7686_3cc5_5f63, 0xb264_4a48_c516_a487],
        revision: 0,
        response: core::ptr::null(),
    },
));

#[used]
#[link_section = ".limine_requests"]
pub static MP_REQ: BootSlot<LimineMpRequest> = BootSlot(UnsafeCell::new(LimineMpRequest {
    id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0x95a6_7b81_9a1b_857e, 0xa0b6_1b72_3b6a_73e0],
    revision: 0,
    response: core::ptr::null(),
    flags: 0,
}));

#[used]
#[link_section = ".limine_requests"]
pub static MODULE_REQ: BootSlot<LimineModuleRequest> = BootSlot(UnsafeCell::new(LimineModuleRequest {
    id: [LIMINE_COMMON_MAGIC_0, LIMINE_COMMON_MAGIC_1, 0x3e7e_2797_02be_32af, 0xca1c_4f3b_d128_0cee],
    revision: 0,
    response: core::ptr::null(),
    internal_module_count: 0,
    internal_modules: core::ptr::null(),
}));

#[used]
#[link_section = ".limine_requests"]
pub static FRAMEBUFFER_REQ: BootSlot<LimineFramebufferRequest> = BootSlot(UnsafeCell::new(
    LimineFramebufferRequest {
        id: [
            LIMINE_COMMON_MAGIC_0,
            LIMINE_COMMON_MAGIC_1,
            0x9d58_27dc_d881_dd75,
            0xa314_8604_f6fa_b11b,
        ],
        revision: 0,
        response: core::ptr::null(),
    },
));

#[used]
#[link_section = ".limine_requests"]
pub static RSDP_REQ: BootSlot<LimineRsdpRequest> = BootSlot(UnsafeCell::new(LimineRsdpRequest {
    id: [
        LIMINE_COMMON_MAGIC_0,
        LIMINE_COMMON_MAGIC_1,
        0xc5e7_7b6b_397e_7b43,
        0x2763_7845_accd_cf3c,
    ],
    revision: 0,
    response: core::ptr::null(),
}));

#[used]
#[link_section = ".limine_requests_end"]
pub static REQUESTS_END_MARKER: [u64; 2] = [0xadc0_e053_1bb1_0d03, 0x9572_709f_3176_4c62];

pub fn base_revision_supported() -> bool {
    BASE_REVISION.read()[2] == 0
}

pub unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        return "<null>";
    }
    let mut len = 0usize;
    // SAFETY: Caller guarantees `ptr` points to a NUL-terminated bootloader ASCII/UTF-8 string.
    unsafe {
        while len < 512 && *ptr.add(len) != 0 {
            len += 1;
        }
        let bytes = core::slice::from_raw_parts(ptr.cast::<u8>(), len);
        core::str::from_utf8(bytes).unwrap_or("<non-utf8>")
    }
}

struct CmdlineBuf(UnsafeCell<[u8; 256]>, UnsafeCell<usize>);
// SAFETY: Populated once on BSP with `CLI` during early boot before APs start or `BootloaderReclaimable`
// frames are reclaimed; thereafter strictly immutable `'static` `.bss` storage.
unsafe impl Sync for CmdlineBuf {}

static STATIC_CMDLINE: CmdlineBuf = CmdlineBuf(UnsafeCell::new([0u8; 256]), UnsafeCell::new(0));

/// Copy Limine's `BOOTLOADER_RECLAIMABLE` command-line string into permanent kernel `.bss` so
/// reclaiming bootloader memory in `Milestone 1B-B` never leaves a dangling `&'static str`.
pub fn latch_cmdline_to_static_bss() -> &'static str {
    let cmd_resp = CMDLINE_REQ.read().response;
    if cmd_resp.is_null() {
        return "";
    }
    // SAFETY: Executed once on BSP (`CLI`) while Limine `BOOTLOADER_RECLAIMABLE` mappings are intact.
    unsafe {
        let src = cstr_to_str((*cmd_resp).cmdline).as_bytes();
        let copy_len = src.len().min(255);
        let dst = &mut *STATIC_CMDLINE.0.get();
        dst[..copy_len].copy_from_slice(&src[..copy_len]);
        *STATIC_CMDLINE.1.get() = copy_len;
        core::str::from_utf8(&dst[..copy_len]).unwrap_or("")
    }
}

pub const MAX_LATCHED_MODULES: usize = 12;

#[derive(Clone, Copy)]
struct LatchedModule {
    addr: *const u8,
    size: usize,
    path_buf: [u8; 64],
    path_len: usize,
}

impl LatchedModule {
    const fn empty() -> Self {
        Self {
            addr: core::ptr::null(),
            size: 0,
            path_buf: [0; 64],
            path_len: 0,
        }
    }
}

struct LatchedModules(UnsafeCell<[LatchedModule; MAX_LATCHED_MODULES]>, UnsafeCell<usize>);
// SAFETY: Populated once on BSP (`CLI`) prior to SMP release and `BootloaderReclaimable` reclaim;
// module payload frames themselves reside in `LIMINE_MEMMAP_EXECUTABLE_AND_MODULES` (never reclaimed).
unsafe impl Sync for LatchedModules {}

static STATIC_MODULES: LatchedModules =
    LatchedModules(UnsafeCell::new([LatchedModule::empty(); MAX_LATCHED_MODULES]), UnsafeCell::new(0));

/// Latch Limine `MODULE_REQ` descriptors into permanent `.bss` before `BootloaderReclaimable` reclaim.
pub fn latch_boot_modules_to_static_bss() -> usize {
    let resp = MODULE_REQ.read().response;
    if resp.is_null() {
        return 0;
    }
    // SAFETY: Executed once on BSP (`CLI`) while `LimineModuleResponse` is mapped.
    unsafe {
        let count = ((*resp).module_count as usize).min(MAX_LATCHED_MODULES);
        let mods_ptr = (*resp).modules;
        let dst = &mut *STATIC_MODULES.0.get();
        let mut stored = 0usize;
        for i in 0..count {
            let f_ptr = *mods_ptr.add(i);
            if f_ptr.is_null() || (*f_ptr).address.is_null() || (*f_ptr).size == 0 {
                continue;
            }
            let p_str = cstr_to_str((*f_ptr).path).as_bytes();
            let plen = p_str.len().min(64);
            let tail = &p_str[p_str.len() - plen..];
            dst[stored].addr = (*f_ptr).address;
            dst[stored].size = (*f_ptr).size as usize;
            dst[stored].path_buf[..plen].copy_from_slice(tail);
            dst[stored].path_len = plen;
            stored += 1;
        }
        *STATIC_MODULES.1.get() = stored;
        stored
    }
}

/// Look up a Limine boot module payload by trailing path suffix (e.g., `"user-hello.elf"`).
pub fn get_boot_module(path_suffix: &str) -> Option<&'static [u8]> {
    let idx = find_boot_module_index(path_suffix)?;
    get_boot_module_by_index(idx)
}

/// Find a latched Limine boot module index (`0..MAX_LATCHED_MODULES`) by trailing path suffix.
pub fn find_boot_module_index(path_suffix: &str) -> Option<usize> {
    let needle = path_suffix.as_bytes();
    // SAFETY: `STATIC_MODULES` is immutable after `latch_boot_modules_to_static_bss`.
    unsafe {
        let count = *STATIC_MODULES.1.get();
        let arr = &*STATIC_MODULES.0.get();
        for (i, m) in arr.iter().take(count).enumerate() {
            let p = &m.path_buf[..m.path_len];
            if p.ends_with(needle) {
                return Some(i);
            }
        }
    }
    None
}

/// Retrieve a latched boot module byte slice by validated index (`CapObjectKind::BootModule`).
pub fn get_boot_module_by_index(idx: usize) -> Option<&'static [u8]> {
    // SAFETY: `STATIC_MODULES` is immutable after `latch_boot_modules_to_static_bss`.
    unsafe {
        let count = *STATIC_MODULES.1.get();
        if idx >= count {
            return None;
        }
        let m = &(*STATIC_MODULES.0.get())[idx];
        if m.addr.is_null() || m.size == 0 {
            None
        } else {
            Some(core::slice::from_raw_parts(m.addr, m.size))
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LatchedFramebuffer {
    pub hhdm_virt: u64,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

struct StaticFramebuffer(UnsafeCell<Option<LatchedFramebuffer>>);
// SAFETY: Latched once on BSP (`CLI`) before SMP bring-up and `BootloaderReclaimable` reclaim.
unsafe impl Sync for StaticFramebuffer {}

static STATIC_FB: StaticFramebuffer = StaticFramebuffer(UnsafeCell::new(None));

/// Latch primary Limine GOP framebuffer metadata into permanent `.bss` prior to bootloader reclaim.
pub fn latch_framebuffer_to_static_bss() -> bool {
    let resp = FRAMEBUFFER_REQ.read().response;
    if resp.is_null() {
        return false;
    }
    // SAFETY: Executed once on BSP (`CLI`) while `LimineFramebufferResponse` is mapped.
    unsafe {
        if (*resp).framebuffer_count == 0 || (*resp).framebuffers.is_null() {
            return false;
        }
        let fb = *(*resp).framebuffers;
        if fb.is_null() || (*fb).address.is_null() {
            return false;
        }
        *STATIC_FB.0.get() = Some(LatchedFramebuffer {
            hhdm_virt: (*fb).address as u64,
            width: (*fb).width,
            height: (*fb).height,
            pitch: (*fb).pitch,
            bpp: (*fb).bpp,
            memory_model: (*fb).memory_model,
            red_mask_size: (*fb).red_mask_size,
            red_mask_shift: (*fb).red_mask_shift,
            green_mask_size: (*fb).green_mask_size,
            green_mask_shift: (*fb).green_mask_shift,
            blue_mask_size: (*fb).blue_mask_size,
            blue_mask_shift: (*fb).blue_mask_shift,
        });
        true
    }
}

#[inline]
pub fn get_latched_framebuffer() -> Option<LatchedFramebuffer> {
    // SAFETY: Read-only after `latch_framebuffer_to_static_bss` on BSP.
    unsafe { *STATIC_FB.0.get() }
}

/// Read the ACPI `RSDP` pointer supplied by Limine (`Base Revision 3` physical address or HHDM).
pub fn get_rsdp_raw_addr() -> Option<u64> {
    let resp = RSDP_REQ.read().response;
    if resp.is_null() {
        return None;
    }
    // SAFETY: Validated non-null response populated by Limine before `_start`.
    let addr = unsafe { (*resp).address as u64 };
    (addr != 0).then_some(addr)
}
