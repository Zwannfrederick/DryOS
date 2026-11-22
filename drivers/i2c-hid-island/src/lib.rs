// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only / safe-mode Intel LPSS `i2c-hid` Touchpad Protocol Parser & Guarded Island Core.

#![cfg_attr(not(test), no_std)]

use dry_driver_sdk::{IslandError, MmioWindow};

/// Safety fence for real Lenovo IdeaPad Gaming 3 15IHU6 / `82K1` validation:
/// never issue unverified physical LPSS I2C controller writes on hardware.
pub const ALLOW_PHYSICAL_I2C_WRITES: bool = false;

pub const I2C_HID_DESC_LEN: u16 = 30;
pub const I2C_HID_BCD_V1: u16 = 0x0100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I2cHidDescriptor {
    pub hid_desc_len: u16,
    pub bcd_version: u16,
    pub report_desc_len: u16,
    pub report_desc_reg: u16,
    pub input_reg: u16,
    pub max_input_len: u16,
    pub output_reg: u16,
    pub cmd_reg: u16,
    pub data_reg: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub version_id: u16,
}

#[inline]
fn le16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

impl I2cHidDescriptor {
    pub fn parse(raw: &[u8]) -> Result<Self, IslandError> {
        if raw.len() < 30 {
            return Err(IslandError::BadAlignment);
        }
        let desc = Self {
            hid_desc_len: le16(raw, 0),
            bcd_version: le16(raw, 2),
            report_desc_len: le16(raw, 4),
            report_desc_reg: le16(raw, 6),
            input_reg: le16(raw, 8),
            max_input_len: le16(raw, 10),
            output_reg: le16(raw, 12),
            cmd_reg: le16(raw, 16),
            data_reg: le16(raw, 18),
            vendor_id: le16(raw, 20),
            product_id: le16(raw, 22),
            version_id: le16(raw, 24),
        };
        if desc.hid_desc_len != I2C_HID_DESC_LEN
            || desc.bcd_version != I2C_HID_BCD_V1
            || desc.input_reg == 0
            || desc.max_input_len < 6
        {
            return Err(IslandError::AbiVersionMismatch);
        }
        Ok(desc)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchpadReport {
    pub report_id: u8,
    pub buttons: u8,
    pub fingers: u8,
    pub dx: i16,
    pub dy: i16,
    pub scroll_dy: i8,
}

impl TouchpadReport {
    /// Parse a compact HID PTP / touchpad packet (`len_lo, len_hi, report_id, buttons, dx_l, dx_h, dy_l, dy_h [, fingers, scroll]`).
    pub fn parse_packet(pkt: &[u8]) -> Result<Self, IslandError> {
        if pkt.len() < 8 {
            return Err(IslandError::ZeroLength);
        }
        let pkt_len = le16(pkt, 0) as usize;
        if pkt_len < 8 || pkt_len > pkt.len() {
            return Err(IslandError::OutOfBounds);
        }
        let report_id = pkt[2];
        let buttons = pkt[3] & 0x07;
        let dx = i16::from_le_bytes([pkt[4], pkt[5]]).clamp(-512, 512);
        let dy = i16::from_le_bytes([pkt[6], pkt[7]]).clamp(-512, 512);
        let fingers = if pkt_len >= 9 { pkt[8].min(5) } else { u8::from(dx != 0 || dy != 0 || buttons != 0) };
        let scroll_dy = if pkt_len >= 10 { (pkt[9] as i8).clamp(-16, 16) } else { 0 };
        Ok(Self {
            report_id,
            buttons,
            fingers,
            dx,
            dy,
            scroll_dy,
        })
    }

    pub fn from_packed_word(word: u64) -> Result<Self, IslandError> {
        let bytes = word.to_le_bytes();
        Self::parse_packet(&bytes)
    }

    #[inline]
    pub const fn pack_event_word(self) -> u64 {
        ((self.dx as u16) as u64)
            | (((self.dy as u16) as u64) << 16)
            | ((self.buttons as u64) << 32)
            | ((self.fingers as u64) << 40)
            | (((self.scroll_dy as u8) as u64) << 48)
    }
}

pub struct I2cHidSafetyGate {
    win: MmioWindow,
}

impl I2cHidSafetyGate {
    pub const fn new(base_va: usize, size_bytes: usize) -> Result<Self, IslandError> {
        match MmioWindow::new(base_va, size_bytes) {
            Ok(win) => Ok(Self { win }),
            Err(e) => Err(e),
        }
    }

    pub const fn read_allowed(&self, offset: usize) -> Result<usize, IslandError> {
        self.win.check_access(offset, 4)
    }

    pub const fn check_mmio_write(&self, offset: usize) -> Result<usize, IslandError> {
        if !ALLOW_PHYSICAL_I2C_WRITES {
            return Err(IslandError::BadStateTransition);
        }
        self.win.check_access(offset, 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_desc() -> [u8; 30] {
        let mut d = [0u8; 30];
        d[0..2].copy_from_slice(&30u16.to_le_bytes());
        d[2..4].copy_from_slice(&0x0100u16.to_le_bytes());
        d[4..6].copy_from_slice(&128u16.to_le_bytes());
        d[6..8].copy_from_slice(&0x0002u16.to_le_bytes());
        d[8..10].copy_from_slice(&0x0003u16.to_le_bytes());
        d[10..12].copy_from_slice(&64u16.to_le_bytes());
        d[12..14].copy_from_slice(&0x0004u16.to_le_bytes());
        d[16..18].copy_from_slice(&0x0005u16.to_le_bytes());
        d[18..20].copy_from_slice(&0x0006u16.to_le_bytes());
        d[20..22].copy_from_slice(&0x06cb_u16.to_le_bytes()); // Synaptics-class VID
        d[22..24].copy_from_slice(&0xce7e_u16.to_le_bytes());
        d[24..26].copy_from_slice(&0x0100u16.to_le_bytes());
        d
    }

    #[test]
    fn parses_valid_i2c_hid_descriptor_and_rejects_bad_version() {
        let raw = sample_desc();
        let parsed = I2cHidDescriptor::parse(&raw).expect("valid descriptor");
        assert_eq!(parsed.vendor_id, 0x06cb);
        assert_eq!(parsed.product_id, 0xce7e);
        assert_eq!(parsed.max_input_len, 64);

        let mut bad = raw;
        bad[2..4].copy_from_slice(&0x0200u16.to_le_bytes());
        assert_eq!(I2cHidDescriptor::parse(&bad), Err(IslandError::AbiVersionMismatch));
    }

    #[test]
    fn decodes_touchpad_report_and_blocks_physical_writes() {
        assert!(!ALLOW_PHYSICAL_I2C_WRITES);
        let gate = I2cHidSafetyGate::new(0x2000_0000, 4096).unwrap();
        assert!(gate.read_allowed(0x10).is_ok());
        assert_eq!(gate.check_mmio_write(0x10), Err(IslandError::BadStateTransition));

        let mut pkt = [0u8; 10];
        pkt[0..2].copy_from_slice(&10u16.to_le_bytes());
        pkt[2] = 0x04;
        pkt[3] = 0x01; // left button
        pkt[4..6].copy_from_slice(&(-12i16).to_le_bytes());
        pkt[6..8].copy_from_slice(&(25i16).to_le_bytes());
        pkt[8] = 2; // two-finger gesture
        pkt[9] = (-2i8) as u8;
        let rep = TouchpadReport::parse_packet(&pkt).expect("valid touchpad packet");
        assert_eq!(rep.dx, -12);
        assert_eq!(rep.dy, 25);
        assert_eq!(rep.buttons, 1);
        assert_eq!(rep.fingers, 2);
        assert_eq!(rep.scroll_dy, -2);
    }
}
