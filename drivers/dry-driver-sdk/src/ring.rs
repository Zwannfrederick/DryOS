// SPDX-License-Identifier: MIT OR Apache-2.0
//! Driver Island `IoRing` descriptor framing and header sanity helpers.

use crate::abi::IslandError;

pub const SDK_IORING_MAGIC: u32 = 0x4452_4952;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SdkSqe {
    pub opcode: u8,
    pub flags: u8,
    pub sector_count: u16,
    pub buf_offset: u32,
    pub lba: u64,
    pub user_tag: u64,
    pub reserved: u64,
}

impl SdkSqe {
    #[inline]
    pub const fn new(opcode: u8, lba: u64, user_tag: u64, buf_offset: u32, sector_count: u16) -> Self {
        Self {
            opcode,
            flags: 0,
            sector_count,
            buf_offset,
            lba,
            user_tag,
            reserved: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SdkCqe {
    pub user_tag: u64,
    pub status: i32,
    pub bytes_done: u32,
}

impl SdkCqe {
    #[inline]
    pub const fn ok(user_tag: u64, bytes_done: u32) -> Self {
        Self {
            user_tag,
            status: 0,
            bytes_done,
        }
    }

    #[inline]
    pub const fn err(user_tag: u64, errno: i32) -> Self {
        Self {
            user_tag,
            status: errno,
            bytes_done: 0,
        }
    }
}

/// Verify `IoRing` magic, power-of-two capacity, and non-overflowing pending delta.
pub const fn check_ring_header(
    magic: u32,
    sq_head: u32,
    sq_tail: u32,
    capacity: u32,
) -> Result<u32, IslandError> {
    if magic != SDK_IORING_MAGIC {
        return Err(IslandError::BadRingMagic);
    }
    if capacity == 0 || !capacity.is_power_of_two() {
        return Err(IslandError::RingCorrupt);
    }
    let pending = sq_tail.wrapping_sub(sq_head);
    if pending > capacity {
        return Err(IslandError::RingCorrupt);
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn wire_sizes_and_ring_validation() {
        assert_eq!(size_of::<SdkSqe>(), 32);
        assert_eq!(size_of::<SdkCqe>(), 16);
        assert_eq!(check_ring_header(SDK_IORING_MAGIC, 2, 6, 32), Ok(4));
        assert_eq!(check_ring_header(SDK_IORING_MAGIC, u32::MAX - 1, 2, 32), Ok(4));
        assert_eq!(check_ring_header(0xdead_beef, 0, 1, 32), Err(IslandError::BadRingMagic));
        assert_eq!(check_ring_header(SDK_IORING_MAGIC, 0, 1, 24), Err(IslandError::RingCorrupt));
        assert_eq!(check_ring_header(SDK_IORING_MAGIC, 0, 40, 32), Err(IslandError::RingCorrupt));
    }
}
