// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! USB Hub Class Support (`0x09`) for 1-level xHCI downstream enumeration.

use crate::usb::xhci::XhciController;

pub const USB_DT_HUB: u8 = 0x29;
pub const USB_DT_SS_HUB: u8 = 0x2a;

pub const HUB_FEAT_PORT_RESET: u16 = 4;
pub const HUB_FEAT_PORT_POWER: u16 = 8;
pub const HUB_FEAT_C_PORT_CONNECTION: u16 = 16;
pub const HUB_FEAT_C_PORT_RESET: u16 = 20;

const PORT_STAT_CONNECTION: u16 = 1 << 0;
const PORT_STAT_ENABLE: u16 = 1 << 1;
const PORT_STAT_RESET: u16 = 1 << 4;
const PORT_STAT_LOW_SPEED: u16 = 1 << 9;
const PORT_STAT_HIGH_SPEED: u16 = 1 << 10;

#[derive(Clone, Copy)]
pub struct HubInfo {
    pub nbr_ports: u8,
    pub pwr_on_ms: u64,
}

#[inline]
pub fn hub_set_port_feature(
    hc: &mut XhciController,
    slot_id: u8,
    port_1based: u8,
    feat: u16,
) -> Result<(), isize> {
    hc.control_xfer(slot_id, 0x23, 0x03, feat, u16::from(port_1based), 0)
        .map(|_| ())
}

#[inline]
pub fn hub_clear_port_feature(
    hc: &mut XhciController,
    slot_id: u8,
    port_1based: u8,
    feat: u16,
) -> Result<(), isize> {
    hc.control_xfer(slot_id, 0x23, 0x01, feat, u16::from(port_1based), 0)
        .map(|_| ())
}

#[inline]
pub fn hub_get_port_status(
    hc: &mut XhciController,
    slot_id: u8,
    port_1based: u8,
) -> Result<(u16, u16), isize> {
    let n = hc.control_xfer(slot_id, 0xa3, 0x00, 0, u16::from(port_1based), 4)?;
    if n < 4 {
        return Err(-5);
    }
    let b = hc.ctrl_bounce_slice();
    let status = u16::from_le_bytes([b[0], b[1]]);
    let change = u16::from_le_bytes([b[2], b[3]]);
    Ok((status, change))
}

pub fn hub_read_descriptor(
    hc: &mut XhciController,
    slot_id: u8,
    hub_speed: u8,
) -> Result<HubInfo, isize> {
    let wval = if hub_speed >= 4 {
        u16::from(USB_DT_SS_HUB) << 8
    } else {
        u16::from(USB_DT_HUB) << 8
    };
    let res = hc.control_xfer(slot_id, 0xa0, 0x06, wval, 0, 9);
    let n = match res {
        Ok(n) if n >= 7 => n,
        _ if hub_speed >= 4 => hc.control_xfer(slot_id, 0xa0, 0x06, u16::from(USB_DT_HUB) << 8, 0, 9)?,
        Ok(_) => return Err(-5),
        Err(e) => return Err(e),
    };
    if n < 7 {
        return Err(-5);
    }
    let b = hc.ctrl_bounce_slice();
    let nbr_ports = b[2].clamp(1, 8);
    let pwr_on_ms = (u64::from(b[5]) * 2).clamp(20, 100);
    Ok(HubInfo {
        nbr_ports,
        pwr_on_ms,
    })
}

/// Power and reset a downstream hub port, returning xHCI speed code (`1`=FS, `2`=LS, `3`=HS, `4`=SS)
/// if an active, enabled downstream device is attached.
pub fn hub_power_and_reset_port(
    hc: &mut XhciController,
    slot_id: u8,
    hub_speed: u8,
    port_1based: u8,
    pwr_on_ms: u64,
) -> Option<u8> {
    let _ = hub_set_port_feature(hc, slot_id, port_1based, HUB_FEAT_PORT_POWER);
    hc.sleep_ms(pwr_on_ms.min(30));

    let Ok((st, _chg)) = hub_get_port_status(hc, slot_id, port_1based) else {
        return None;
    };
    if (st & PORT_STAT_CONNECTION) == 0 {
        return None;
    }

    let _ = hub_clear_port_feature(hc, slot_id, port_1based, HUB_FEAT_C_PORT_CONNECTION);
    if hub_set_port_feature(hc, slot_id, port_1based, HUB_FEAT_PORT_RESET).is_err() {
        return None;
    }

    for _ in 0..25 {
        hc.sleep_ms(5);
        let Ok((cur, _)) = hub_get_port_status(hc, slot_id, port_1based) else {
            return None;
        };
        if (cur & PORT_STAT_RESET) == 0 && (cur & PORT_STAT_ENABLE) != 0 {
            let _ = hub_clear_port_feature(hc, slot_id, port_1based, HUB_FEAT_C_PORT_RESET);
            if (cur & PORT_STAT_CONNECTION) == 0 {
                return None;
            }
            let spd = if hub_speed >= 4 {
                4
            } else if (cur & PORT_STAT_LOW_SPEED) != 0 {
                2
            } else if (cur & PORT_STAT_HIGH_SPEED) != 0 {
                3
            } else {
                1
            };
            return Some(spd);
        }
    }
    None
}
