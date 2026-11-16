//! Ring-3 USB HID Boot Keyboard (`03:01:01`) and Boot Mouse (`03:01:02`) Driver.

use core::ptr::read_volatile;

use crate::input::{
    InputEvent, InputEventKind, InputRingQueue, EV_FLAG_EXTENDED, MOD_ALT, MOD_CTRL, MOD_SHIFT,
    MOUSE_BTN_LEFT, MOUSE_BTN_MIDDLE, MOUSE_BTN_RIGHT,
};
use crate::keymap::{
    translate_set1, KeymapLayout, KEY_LALT, KEY_LCTRL, KEY_LSHIFT, KEY_NONE, KEY_RALT, KEY_RCTRL,
    KEY_RSHIFT,
};
use crate::rdtsc_user;
use crate::usb::xhci::XhciController;

pub const DEV_ID_USB_KBD: u16 = 0x0003;
pub const DEV_ID_USB_MOUSE: u16 = 0x0004;

pub struct UsbHidKbd {
    pub active: bool,
    pub slot_id: u8,
    pub dci: u8,
    pub ring_idx: usize,
    pub ep_addr: u8,
    pub mps: u16,
    pub buf_va: u64,
    pub buf_pa: u64,
    pub prev_mods: u8,
    pub prev_keys: [u8; 6],
    pub caps_lock: bool,
    pub reports: u64,
    pub events: u64,
}

impl UsbHidKbd {
    pub const fn empty() -> Self {
        Self {
            active: false,
            slot_id: 0,
            dci: 0,
            ring_idx: 0,
            ep_addr: 0,
            mps: 8,
            buf_va: 0,
            buf_pa: 0,
            prev_mods: 0,
            prev_keys: [0; 6],
            caps_lock: false,
            reports: 0,
            events: 0,
        }
    }

    pub fn arm(&self, hc: &mut XhciController) {
        if self.active {
            hc.arm_normal_trb(self.slot_id, self.dci, self.ring_idx, self.buf_pa, 8);
        }
    }

    pub fn on_report(
        &mut self,
        layout: KeymapLayout,
        next_seq: &mut u32,
        queue: &mut InputRingQueue<64>,
    ) {
        if !self.active {
            return;
        }
        let mut rep = [0u8; 8];
        for (i, dst) in rep.iter_mut().enumerate() {
            // SAFETY: `self.buf_va` is a 64-byte slice inside pinned `DMA_CTRL_BOUNCE_VA`.
            *dst = unsafe { read_volatile((self.buf_va + i as u64) as *const u8) };
        }
        self.reports = self.reports.wrapping_add(1);

        // Phantom RollOver (`0x01`) ignores the key array update cleanly.
        if rep[2..8].contains(&0x01) {
            return;
        }

        let new_mods = rep[0];
        let dry_mods = hid_mods_to_dry(new_mods);
        let tsc48 = rdtsc_user() & 0x0000_ffff_ffff_ffff;

        for bit in 0..8u8 {
            let was = (self.prev_mods >> bit) & 1 != 0;
            let now = (new_mods >> bit) & 1 != 0;
            if was != now {
                if let Some((kc, sc)) = mod_bit_to_key(bit) {
                    let kind = if now {
                        InputEventKind::KeyPress
                    } else {
                        InputEventKind::KeyRelease
                    };
                    Self::push_key(queue, next_seq, (kind, dry_mods, sc, kc, 0, 0), tsc48);
                    self.events = self.events.wrapping_add(1);
                }
            }
        }
        self.prev_mods = new_mods;

        let old_keys = self.prev_keys;
        let new_keys = [rep[2], rep[3], rep[4], rep[5], rep[6], rep[7]];
        for old_u in old_keys {
            if old_u >= 4 && !new_keys.contains(&old_u) {
                self.emit_usage(old_u, false, dry_mods, layout, (next_seq, queue, tsc48));
            }
        }
        for new_u in new_keys {
            if new_u >= 4 && !old_keys.contains(&new_u) {
                if new_u == 0x39 {
                    self.caps_lock = !self.caps_lock;
                }
                self.emit_usage(new_u, true, dry_mods, layout, (next_seq, queue, tsc48));
            }
        }
        self.prev_keys = new_keys;
    }

    fn emit_usage(
        &mut self,
        usage: u8,
        pressed: bool,
        dry_mods: u8,
        layout: KeymapLayout,
        sink: (&mut u32, &mut InputRingQueue<64>, u64),
    ) {
        let Some((sc1, ext)) = hid_usage_to_set1(usage) else {
            return;
        };
        let is_alpha = (0x04..=0x1d).contains(&usage);
        let eff_mods = if is_alpha && self.caps_lock {
            dry_mods ^ MOD_SHIFT
        } else {
            dry_mods
        };
        let Ok((keycode, ascii)) = translate_set1(layout, sc1, ext, eff_mods) else {
            return;
        };
        if keycode == KEY_NONE {
            return;
        }
        let eff_ascii = if pressed && (dry_mods & MOD_CTRL) != 0 && ascii.is_ascii_alphabetic() {
            ascii.to_ascii_lowercase() - b'a' + 1
        } else if pressed {
            ascii
        } else {
            0
        };
        let kind = if pressed {
            InputEventKind::KeyPress
        } else {
            InputEventKind::KeyRelease
        };
        let full_sc = if ext { 0xe000 | u16::from(sc1) } else { u16::from(sc1) };
        let flags = if ext { EV_FLAG_EXTENDED } else { 0 };
        let (next_seq, queue, tsc48) = sink;
        Self::push_key(queue, next_seq, (kind, dry_mods, full_sc, keycode, eff_ascii, flags), tsc48);
        self.events = self.events.wrapping_add(1);
    }

    fn push_key(
        queue: &mut InputRingQueue<64>,
        next_seq: &mut u32,
        fields: (InputEventKind, u8, u16, u16, u8, u8),
        tsc48: u64,
    ) {
        let (kind, modifiers, scancode, keycode, ascii, flags) = fields;
        let seq = *next_seq;
        *next_seq = next_seq.wrapping_add(1);
        queue.push(InputEvent {
            seq,
            device_id: DEV_ID_USB_KBD,
            kind,
            modifiers,
            scancode,
            keycode,
            ascii,
            flags,
            dx: 0,
            dy: 0,
            buttons: 0,
            wheel: 0,
            timestamp_tsc48: tsc48,
        });
    }
}

pub struct UsbHidMouse {
    pub active: bool,
    pub slot_id: u8,
    pub dci: u8,
    pub ring_idx: usize,
    pub ep_addr: u8,
    pub mps: u16,
    pub buf_va: u64,
    pub buf_pa: u64,
    pub prev_buttons: u8,
    pub reports: u64,
    pub events: u64,
}

impl UsbHidMouse {
    pub const fn empty() -> Self {
        Self {
            active: false,
            slot_id: 0,
            dci: 0,
            ring_idx: 0,
            ep_addr: 0,
            mps: 4,
            buf_va: 0,
            buf_pa: 0,
            prev_buttons: 0,
            reports: 0,
            events: 0,
        }
    }

    pub fn arm(&self, hc: &mut XhciController) {
        if self.active {
            let len = (self.mps as u32).clamp(3, 8);
            hc.arm_normal_trb(self.slot_id, self.dci, self.ring_idx, self.buf_pa, len);
        }
    }

    pub fn on_report(
        &mut self,
        residual: u32,
        next_seq: &mut u32,
        queue: &mut InputRingQueue<64>,
    ) {
        if !self.active {
            return;
        }
        let req_len = (self.mps as usize).clamp(3, 8);
        let got = req_len.saturating_sub(residual as usize);
        if got < 3 {
            return;
        }
        let mut rep = [0u8; 8];
        for (i, dst) in rep[..got].iter_mut().enumerate() {
            // SAFETY: `self.buf_va` is a 64-byte slice inside pinned `DMA_CTRL_BOUNCE_VA`.
            *dst = unsafe { read_volatile((self.buf_va + i as u64) as *const u8) };
        }
        self.reports = self.reports.wrapping_add(1);

        let buttons = rep[0] & 0x07;
        let dx = i16::from(rep[1] as i8);
        let dy = i16::from(rep[2] as i8);
        let wheel = if got >= 4 { rep[3] as i8 } else { 0 };
        let tsc48 = rdtsc_user() & 0x0000_ffff_ffff_ffff;

        for bit in [MOUSE_BTN_LEFT, MOUSE_BTN_RIGHT, MOUSE_BTN_MIDDLE] {
            let was = (self.prev_buttons & bit) != 0;
            let now = (buttons & bit) != 0;
            if was != now {
                let seq = *next_seq;
                *next_seq = next_seq.wrapping_add(1);
                queue.push(InputEvent {
                    seq,
                    device_id: DEV_ID_USB_MOUSE,
                    kind: if now {
                        InputEventKind::MouseButtonDown
                    } else {
                        InputEventKind::MouseButtonUp
                    },
                    modifiers: 0,
                    scancode: u16::from(bit),
                    keycode: u16::from(bit),
                    ascii: 0,
                    flags: 0,
                    dx: 0,
                    dy: 0,
                    buttons,
                    wheel: 0,
                    timestamp_tsc48: tsc48,
                });
                self.events = self.events.wrapping_add(1);
            }
        }
        self.prev_buttons = buttons;

        if dx != 0 || dy != 0 {
            let seq = *next_seq;
            *next_seq = next_seq.wrapping_add(1);
            queue.push(InputEvent {
                seq,
                device_id: DEV_ID_USB_MOUSE,
                kind: InputEventKind::MouseMove,
                modifiers: 0,
                scancode: 0,
                keycode: 0,
                ascii: 0,
                flags: 0,
                dx,
                dy,
                buttons,
                wheel: 0,
                timestamp_tsc48: tsc48,
            });
            self.events = self.events.wrapping_add(1);
        }

        if wheel != 0 {
            let seq = *next_seq;
            *next_seq = next_seq.wrapping_add(1);
            queue.push(InputEvent {
                seq,
                device_id: DEV_ID_USB_MOUSE,
                kind: InputEventKind::MouseScroll,
                modifiers: 0,
                scancode: 0,
                keycode: 0,
                ascii: 0,
                flags: 0,
                dx: 0,
                dy: 0,
                buttons,
                wheel,
                timestamp_tsc48: tsc48,
            });
            self.events = self.events.wrapping_add(1);
        }
    }
}

pub fn init_boot_hid_protocol(hc: &mut XhciController, slot_id: u8, iface_num: u8) {
    // SET_PROTOCOL(0 = Boot Protocol) + SET_IDLE(0 = only report on change/poll).
    let _ = hc.control_xfer(slot_id, 0x21, 0x0b, 0, u16::from(iface_num), 0);
    let _ = hc.control_xfer(slot_id, 0x21, 0x0a, 0, u16::from(iface_num), 0);
}

#[inline]
const fn hid_mods_to_dry(m: u8) -> u8 {
    let mut out = 0u8;
    if (m & ((1 << 1) | (1 << 5))) != 0 {
        out |= MOD_SHIFT;
    }
    if (m & ((1 << 0) | (1 << 4))) != 0 {
        out |= MOD_CTRL;
    }
    if (m & ((1 << 2) | (1 << 6))) != 0 {
        out |= MOD_ALT;
    }
    out
}

#[inline]
const fn mod_bit_to_key(bit: u8) -> Option<(u16, u16)> {
    match bit {
        0 => Some((KEY_LCTRL, 0x1d)),
        1 => Some((KEY_LSHIFT, 0x2a)),
        2 => Some((KEY_LALT, 0x38)),
        4 => Some((KEY_RCTRL, 0xe01d)),
        5 => Some((KEY_RSHIFT, 0x36)),
        6 => Some((KEY_RALT, 0xe038)),
        _ => None,
    }
}

const fn hid_usage_to_set1(usage: u8) -> Option<(u8, bool)> {
    let (sc, ext) = match usage {
        0x04 => (0x1e, false), // a
        0x05 => (0x30, false), // b
        0x06 => (0x2e, false), // c
        0x07 => (0x20, false), // d
        0x08 => (0x12, false), // e
        0x09 => (0x21, false), // f
        0x0a => (0x22, false), // g
        0x0b => (0x23, false), // h
        0x0c => (0x17, false), // i
        0x0d => (0x24, false), // j
        0x0e => (0x25, false), // k
        0x0f => (0x26, false), // l
        0x10 => (0x32, false), // m
        0x11 => (0x31, false), // n
        0x12 => (0x18, false), // o
        0x13 => (0x19, false), // p
        0x14 => (0x10, false), // q
        0x15 => (0x13, false), // r
        0x16 => (0x1f, false), // s
        0x17 => (0x14, false), // t
        0x18 => (0x16, false), // u
        0x19 => (0x2f, false), // v
        0x1a => (0x11, false), // w
        0x1b => (0x2d, false), // x
        0x1c => (0x15, false), // y
        0x1d => (0x2c, false), // z
        0x1e => (0x02, false), // 1
        0x1f => (0x03, false), // 2
        0x20 => (0x04, false), // 3
        0x21 => (0x05, false), // 4
        0x22 => (0x06, false), // 5
        0x23 => (0x07, false), // 6
        0x24 => (0x08, false), // 7
        0x25 => (0x09, false), // 8
        0x26 => (0x0a, false), // 9
        0x27 => (0x0b, false), // 0
        0x28 => (0x1c, false), // Enter
        0x29 => (0x01, false), // Esc
        0x2a => (0x0e, false), // Backspace
        0x2b => (0x0f, false), // Tab
        0x2c => (0x39, false), // Space
        0x2d => (0x0c, false), // -
        0x2e => (0x0d, false), // =
        0x2f => (0x1a, false), // [
        0x30 => (0x1b, false), // ]
        0x31 | 0x32 => (0x2b, false), // \
        0x33 => (0x27, false), // ;
        0x34 => (0x28, false), // '
        0x35 => (0x29, false), // `
        0x36 => (0x33, false), // ,
        0x37 => (0x34, false), // .
        0x38 => (0x35, false), // /
        0x4a => (0x47, true),  // Home
        0x4b => (0x49, true),  // PageUp
        0x4c => (0x53, true),  // Delete
        0x4d => (0x4f, true),  // End
        0x4e => (0x51, true),  // PageDown
        0x4f => (0x4d, true),  // Right
        0x50 => (0x4b, true),  // Left
        0x51 => (0x50, true),  // Down
        0x52 => (0x48, true),  // Up
        _ => return None,
    };
    Some((sc, ext))
}
