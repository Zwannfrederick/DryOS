// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded MMIO register window and DMA buffer slice validators.

use crate::abi::IslandError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmioWindow {
    base: usize,
    len: usize,
}

impl MmioWindow {
    pub const fn new(base: usize, len: usize) -> Result<Self, IslandError> {
        if len == 0 {
            return Err(IslandError::ZeroLength);
        }
        if base.checked_add(len).is_none() {
            return Err(IslandError::OutOfBounds);
        }
        Ok(Self { base, len })
    }

    pub const fn check_access(&self, offset: usize, width: usize) -> Result<usize, IslandError> {
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err(IslandError::BadWidth);
        }
        if offset % width != 0 {
            return Err(IslandError::BadAlignment);
        }
        let Some(end) = offset.checked_add(width) else {
            return Err(IslandError::OutOfBounds);
        };
        if end > self.len {
            return Err(IslandError::OutOfBounds);
        }
        match self.base.checked_add(offset) {
            Some(addr) => Ok(addr),
            None => Err(IslandError::OutOfBounds),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaRegion {
    virt_base: usize,
    phys_base: u64,
    len: usize,
}

impl DmaRegion {
    pub const fn new(virt_base: usize, phys_base: u64, len: usize) -> Result<Self, IslandError> {
        if len == 0 {
            return Err(IslandError::ZeroLength);
        }
        if virt_base.checked_add(len).is_none() || phys_base.checked_add(len as u64).is_none() {
            return Err(IslandError::OutOfBounds);
        }
        Ok(Self {
            virt_base,
            phys_base,
            len,
        })
    }

    pub const fn check_slice(
        &self,
        offset: usize,
        slice_len: usize,
        align: usize,
    ) -> Result<(usize, u64), IslandError> {
        if slice_len == 0 {
            return Err(IslandError::ZeroLength);
        }
        if !align.is_power_of_two() {
            return Err(IslandError::BadAlignment);
        }
        if offset % align != 0 {
            return Err(IslandError::BadAlignment);
        }
        let Some(end) = offset.checked_add(slice_len) else {
            return Err(IslandError::OutOfBounds);
        };
        if end > self.len {
            return Err(IslandError::OutOfBounds);
        }
        let (Some(va), Some(pa)) = (
            self.virt_base.checked_add(offset),
            self.phys_base.checked_add(offset as u64),
        ) else {
            return Err(IslandError::OutOfBounds);
        };
        Ok((va, pa))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmio_window_bounds_and_alignment() {
        assert_eq!(MmioWindow::new(0x1000, 0), Err(IslandError::ZeroLength));
        let w = MmioWindow::new(0x2000_0000, 4096).unwrap();
        assert_eq!(w.check_access(0, 4), Ok(0x2000_0000));
        assert_eq!(w.check_access(4092, 4), Ok(0x2000_0ffc));
        assert_eq!(w.check_access(2, 4), Err(IslandError::BadAlignment));
        assert_eq!(w.check_access(0, 3), Err(IslandError::BadWidth));
        assert_eq!(w.check_access(4096, 4), Err(IslandError::OutOfBounds));
        assert_eq!(w.check_access(usize::MAX - 3, 4), Err(IslandError::OutOfBounds));
    }

    #[test]
    fn dma_region_bounds_and_overflow() {
        assert_eq!(DmaRegion::new(0x1000, 0x2000, 0), Err(IslandError::ZeroLength));
        let r = DmaRegion::new(0x2400_0000, 0x8000_0000, 4096).unwrap();
        assert_eq!(r.check_slice(64, 512, 64), Ok((0x2400_0040, 0x8000_0040)));
        assert_eq!(r.check_slice(64, 0, 64), Err(IslandError::ZeroLength));
        assert_eq!(r.check_slice(32, 64, 64), Err(IslandError::BadAlignment));
        assert_eq!(r.check_slice(64, 64, 3), Err(IslandError::BadAlignment));
        assert_eq!(r.check_slice(4096, 4, 4), Err(IslandError::OutOfBounds));
        assert_eq!(
            r.check_slice(usize::MAX - 19, 32, 4),
            Err(IslandError::OutOfBounds)
        );
    }
}
