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
