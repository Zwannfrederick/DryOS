// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 Unified PS/2 i8042 Controller (`0x0060` data, `0x0064` status/command via `IoPortCap`).
//!
//! Enforces the **Single Controller Ownership** invariant (`Milestone 2A-2`): keyboard (`IRQ1`) and
//! auxiliary PS/2 mouse (`IRQ12`) share ports `0x60`/`0x64` inside one isolated `CPL=3` `dry-input`
//! process. Zero protocol state, scancode tables, or mouse packet sync lives in `dry-kernel`.

use crate::input::{
    InputEvent, InputEventKind, InputRingQueue, DEV_ID_PS2_KBD, DEV_ID_PS2_MOUSE, EV_FLAG_EXTENDED,
    EV_FLAG_REPEAT, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOUSE_BTN_LEFT, MOUSE_BTN_MIDDLE,
    MOUSE_BTN_RIGHT,
};
use crate::keymap::{
    translate_set1, KeymapLayout, KEY_LALT, KEY_LCTRL, KEY_LSHIFT, KEY_NONE, KEY_RALT, KEY_RCTRL,
    KEY_RSHIFT,
};
use crate::{rdtsc_user, sys_ioport_inb, sys_ioport_outb};

pub const I8042_DATA_PORT: u16 = 0x0060;
pub const I8042_STATUS_PORT: u16 = 0x0064;
pub const I8042_CMD_PORT: u16 = 0x0064;

const STATUS_OBF: u8 = 1 << 0;
const STATUS_IBF: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

const CMD_READ_CCB: u8 = 0x20;
const CMD_WRITE_CCB: u8 = 0x60;
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_WRITE_AUX: u8 = 0xD4;

const DEV_CMD_ENABLE_SCAN: u8 = 0xF4;
const MOUSE_CMD_SET_DEFAULTS: u8 = 0xF6;
const PS2_RESP_ACK: u8 = 0xFA;
const PS2_RESP_RESEND: u8 = 0xFE;

const CCB_IRQ1_ENABLE: u8 = 1 << 0;
const CCB_IRQ12_ENABLE: u8 = 1 << 1;
const CCB_KBD_CLOCK_DISABLE: u8 = 1 << 4;
const CCB_AUX_CLOCK_DISABLE: u8 = 1 << 5;
const CCB_TRANSLATE_SET1: u8 = 1 << 6;

const MOD_BIT_LSHIFT: u8 = 1 << 0;
const MOD_BIT_RSHIFT: u8 = 1 << 1;
const MOD_BIT_LCTRL: u8 = 1 << 2;
const MOD_BIT_RCTRL: u8 = 1 << 3;
const MOD_BIT_LALT: u8 = 1 << 4;
const MOD_BIT_RALT: u8 = 1 << 5;

const MOUSE_PKT_SYNC_BIT: u8 = 1 << 3;
const MOUSE_PKT_X_SIGN: u8 = 1 << 4;
const MOUSE_PKT_Y_SIGN: u8 = 1 << 5;
const MOUSE_PKT_X_OVF: u8 = 1 << 6;
const MOUSE_PKT_Y_OVF: u8 = 1 << 7;

/// 3-byte standard PS/2 AUX mouse packet decoder with sync-bit recovery and overflow protection.
pub struct Ps2MouseDecoder {
    pkt: [u8; 3],
    idx: u8,
    buttons: u8,
    pub packets_ok: u64,
    pub desync_drops: u64,
    pub overflow_drops: u64,
    pub move_events: u64,
    pub button_events: u64,
}

impl Ps2MouseDecoder {
    pub const fn new() -> Self {
        Self {
            pkt: [0; 3],
            idx: 0,
            buttons: 0,
            packets_ok: 0,
            desync_drops: 0,
            overflow_drops: 0,
            move_events: 0,
            button_events: 0,
        }
    }

    /// Feed one AUX (`STATUS_AUX_DATA == 1`) byte from `0x60` and emit up to 4 normalized events
    /// (up to 3 button transitions + 1 relative motion event) into `out`.
    pub fn feed_byte(
        &mut self,
        byte: u8,
        modifiers: u8,
        next_seq: &mut u32,
        tsc48: u64,
        out: &mut [InputEvent; 4],
    ) -> usize {
        if self.idx == 0 {
            if (byte & MOUSE_PKT_SYNC_BIT) == 0 {
                self.desync_drops = self.desync_drops.wrapping_add(1);
                return 0;
            }
            self.pkt[0] = byte;
            self.idx = 1;
            return 0;
        }

        self.pkt[self.idx as usize] = byte;
        self.idx += 1;
        if self.idx < 3 {
            return 0;
        }
        self.idx = 0;

        let b0 = self.pkt[0];
        let b1 = self.pkt[1];
        let b2 = self.pkt[2];
        self.packets_ok = self.packets_ok.wrapping_add(1);

        let mut emitted = 0usize;
        let new_buttons = b0 & 0x07;
        let changed = new_buttons ^ self.buttons;
        self.buttons = new_buttons;

        for btn_bit in [MOUSE_BTN_LEFT, MOUSE_BTN_RIGHT, MOUSE_BTN_MIDDLE] {
            if (changed & btn_bit) != 0 && emitted < out.len() {
                let is_down = (new_buttons & btn_bit) != 0;
                let seq = *next_seq;
                *next_seq = next_seq.wrapping_add(1);
                out[emitted] = InputEvent {
                    seq,
                    device_id: DEV_ID_PS2_MOUSE,
                    kind: if is_down {
                        InputEventKind::MouseButtonDown
                    } else {
                        InputEventKind::MouseButtonUp
                    },
                    modifiers,
                    scancode: btn_bit as u16,
                    keycode: btn_bit as u16,
                    ascii: 0,
                    flags: 0,
                    dx: 0,
                    dy: 0,
                    buttons: new_buttons,
                    wheel: 0,
                    timestamp_tsc48: tsc48,
                };
                emitted += 1;
                self.button_events = self.button_events.wrapping_add(1);
            }
        }

        if (b0 & (MOUSE_PKT_X_OVF | MOUSE_PKT_Y_OVF)) != 0 {
            self.overflow_drops = self.overflow_drops.wrapping_add(1);
            return emitted;
        }

        let dx: i16 = (b1 as i16) - if (b0 & MOUSE_PKT_X_SIGN) != 0 { 256 } else { 0 };
        let dy_ps2_up: i16 = (b2 as i16) - if (b0 & MOUSE_PKT_Y_SIGN) != 0 { 256 } else { 0 };
        // PS/2 packet Y+ points upward; screen Y+ points downward.
        let dy_screen: i16 = -dy_ps2_up;

        if (dx != 0 || dy_screen != 0) && emitted < out.len() {
            let seq = *next_seq;
            *next_seq = next_seq.wrapping_add(1);
            out[emitted] = InputEvent {
                seq,
                device_id: DEV_ID_PS2_MOUSE,
                kind: InputEventKind::MouseMove,
                modifiers,
                scancode: 0,
                keycode: 0,
                ascii: 0,
                flags: 0,
                dx,
                dy: dy_screen,
                buttons: new_buttons,
                wheel: 0,
                timestamp_tsc48: tsc48,
            };
            emitted += 1;
            self.move_events = self.move_events.wrapping_add(1);
        }

        emitted
    }
}

impl Default for Ps2MouseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Unified PS/2 Set-1 keyboard + AUX mouse state machine sharing a single monotonic `seq` stream.
pub struct Ps2Decoder {
    layout: KeymapLayout,
    next_seq: u32,
    e0_pending: bool,
    e1_skip: u8,
    raw_mod_bits: u8,
    pressed_bitmap: [u64; 4],
    invalid_bytes: u64,
    pub kbd_events: u64,
    pub mouse_enabled: bool,
    pub mouse: Ps2MouseDecoder,
}

impl Ps2Decoder {
    pub const fn new(layout: KeymapLayout) -> Self {
        Self {
            layout,
            next_seq: 1,
            e0_pending: false,
            e1_skip: 0,
            raw_mod_bits: 0,
            pressed_bitmap: [0; 4],
            invalid_bytes: 0,
            kbd_events: 0,
            mouse_enabled: false,
            mouse: Ps2MouseDecoder::new(),
        }
    }

    #[inline]
    pub const fn invalid_bytes(&self) -> u64 {
        self.invalid_bytes
    }

    #[inline]
    pub fn active_modifiers(&self) -> u8 {
        let mut m = 0u8;
        if (self.raw_mod_bits & (MOD_BIT_LSHIFT | MOD_BIT_RSHIFT)) != 0 {
            m |= MOD_SHIFT;
        }
        if (self.raw_mod_bits & (MOD_BIT_LCTRL | MOD_BIT_RCTRL)) != 0 {
            m |= MOD_CTRL;
        }
        if (self.raw_mod_bits & (MOD_BIT_LALT | MOD_BIT_RALT)) != 0 {
            m |= MOD_ALT;
        }
        m
    }

    #[inline]
    fn update_modifier_key(&mut self, keycode: u16, pressed: bool) {
        let bit = match keycode {
            KEY_LSHIFT => MOD_BIT_LSHIFT,
            KEY_RSHIFT => MOD_BIT_RSHIFT,
            KEY_LCTRL => MOD_BIT_LCTRL,
            KEY_RCTRL => MOD_BIT_RCTRL,
            KEY_LALT => MOD_BIT_LALT,
            KEY_RALT => MOD_BIT_RALT,
            _ => 0,
        };
        if bit != 0 {
            if pressed {
                self.raw_mod_bits |= bit;
            } else {
                self.raw_mod_bits &= !bit;
            }
        }
    }

    /// Feed a single Set-1 keyboard scancode byte (`STATUS_AUX_DATA == 0`) from port `0x60`.
    pub fn feed_byte(&mut self, byte: u8) -> Option<InputEvent> {
        self.feed_kbd_byte_at(byte, rdtsc_user() & 0xffff_ffff_ffff)
    }

    pub fn feed_kbd_byte_at(&mut self, byte: u8, tsc48: u64) -> Option<InputEvent> {
        if self.e1_skip > 0 {
            self.e1_skip -= 1;
            return None;
        }
        match byte {
            0xE0 => {
                self.e0_pending = true;
                return None;
            }
            0xE1 => {
                self.e0_pending = false;
                self.e1_skip = 2;
                return None;
            }
            // Note: in i8042 Set-1 translation, `0xAA` is `0x2A | 0x80` (Left Shift break) whenever
            // Left Shift is currently held; only treat `0xAA` as BAT self-test if Left Shift is up!
            0xAA if (self.raw_mod_bits & MOD_BIT_LSHIFT) == 0 => {
                self.e0_pending = false;
                return None;
            }
            0x00 | 0xEE | 0xFA | 0xFC | 0xFD | 0xFE | 0xFF => {
                self.e0_pending = false;
                if byte == 0x00 || byte == 0xFF {
                    self.invalid_bytes = self.invalid_bytes.wrapping_add(1);
                }
                return None;
            }
            _ => {}
        }

        let extended = self.e0_pending;
        self.e0_pending = false;

        let is_release = (byte & 0x80) != 0;
        let base_code = byte & 0x7F;
        if base_code == 0 {
            self.invalid_bytes = self.invalid_bytes.wrapping_add(1);
            return None;
        }

        if extended && (base_code == 0x2A || base_code == 0x36) {
            return None;
        }

        let slot_idx = (base_code as usize) + if extended { 128 } else { 0 };
        let word_i = slot_idx / 64;
        let bit_m = 1u64 << (slot_idx % 64);
        let was_pressed = (self.pressed_bitmap[word_i] & bit_m) != 0;
        if is_release {
            self.pressed_bitmap[word_i] &= !bit_m;
        } else {
            self.pressed_bitmap[word_i] |= bit_m;
        }

        let pre_mods = self.active_modifiers();
        let Ok((kc_probe, _)) = translate_set1(self.layout, base_code, extended, pre_mods) else {
            return None;
        };
        if kc_probe == KEY_NONE {
            self.invalid_bytes = self.invalid_bytes.wrapping_add(1);
            return None;
        }
        self.update_modifier_key(kc_probe, !is_release);

        let eff_mods = self.active_modifiers();
        let (keycode, ascii) =
            translate_set1(self.layout, base_code, extended, eff_mods).unwrap_or((KEY_NONE, 0));

        let mut flags = 0u8;
        if extended {
            flags |= EV_FLAG_EXTENDED;
        }
        if !is_release && was_pressed {
            flags |= EV_FLAG_REPEAT;
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.kbd_events = self.kbd_events.wrapping_add(1);
        let full_scancode = if extended {
            0xE000u16 | (byte as u16)
        } else {
            byte as u16
        };

        Some(InputEvent {
            seq,
            device_id: DEV_ID_PS2_KBD,
            kind: if is_release {
                InputEventKind::KeyRelease
            } else {
                InputEventKind::KeyPress
            },
            modifiers: eff_mods,
            scancode: full_scancode,
            keycode,
            ascii: if is_release { 0 } else { ascii },
            flags,
            dx: 0,
            dy: 0,
            buttons: self.mouse.buttons,
            wheel: 0,
            timestamp_tsc48: tsc48,
        })
    }

    /// Drain up to 48 pending keyboard (`STATUS_AUX_DATA == 0`) and AUX mouse (`STATUS_AUX_DATA != 0`)
    /// bytes from the shared i8042 output buffer into `queue`.
    pub fn drain_hw_fifo<const N: usize>(
        &mut self,
        ioport_cap: u64,
        queue: &mut InputRingQueue<N>,
    ) -> Result<usize, isize> {
        let mut pushed = 0usize;
        let mut mouse_out = [InputEvent::EMPTY; 4];
        for _ in 0..48 {
            let status = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
            if (status & STATUS_OBF) == 0 {
                break;
            }
            let data = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
            let tsc48 = rdtsc_user() & 0xffff_ffff_ffff;
            if (status & STATUS_AUX_DATA) != 0 {
                if self.mouse_enabled {
                    let mods = self.active_modifiers();
                    let n = self.mouse.feed_byte(data, mods, &mut self.next_seq, tsc48, &mut mouse_out);
                    for item in mouse_out.iter().take(n) {
                        if queue.push_coalescing(*item) {
                            pushed += 1;
                        }
                    }
                }
            } else if let Some(ev) = self.feed_kbd_byte_at(data, tsc48) {
                if queue.push(ev) {
                    pushed += 1;
                }
            }
        }
        Ok(pushed)
    }
}

fn wait_ibf_clear(ioport_cap: u64) -> Result<bool, isize> {
    for _ in 0..2048 {
        let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
        if (st & STATUS_IBF) == 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn flush_obf(ioport_cap: u64) -> Result<(), isize> {
    for _ in 0..48 {
        let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
        if (st & STATUS_OBF) == 0 {
            break;
        }
        let _ = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
    }
    Ok(())
}

fn read_ccb(ioport_cap: u64) -> Result<u8, isize> {
    let _ = wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_CMD_PORT, CMD_READ_CCB)?;
    for _ in 0..2048 {
        let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
        if (st & STATUS_OBF) != 0 {
            let b = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
            if (st & STATUS_AUX_DATA) == 0 {
                return Ok(b);
            }
        }
    }
    Ok(CCB_TRANSLATE_SET1)
}

fn write_ccb(ioport_cap: u64, ccb: u8) -> Result<(), isize> {
    let _ = wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_CMD_PORT, CMD_WRITE_CCB)?;
    let _ = wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_DATA_PORT, ccb)
}

fn send_kbd_cmd_expect_ack(ioport_cap: u64, cmd: u8) -> Result<bool, isize> {
    for _attempt in 0..3 {
        if !wait_ibf_clear(ioport_cap)? {
            return Ok(false);
        }
        sys_ioport_outb(ioport_cap, I8042_DATA_PORT, cmd)?;
        let mut resend = false;
        for _ in 0..2048 {
            let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
            if (st & STATUS_OBF) != 0 {
                let b = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
                if (st & STATUS_AUX_DATA) != 0 {
                    continue;
                }
                if b == PS2_RESP_ACK {
                    return Ok(true);
                }
                if b == PS2_RESP_RESEND {
                    resend = true;
                    break;
                }
            }
        }
        if !resend {
            return Ok(false);
        }
    }
    Ok(false)
}

fn send_aux_cmd_expect_ack(ioport_cap: u64, cmd: u8) -> Result<bool, isize> {
    for _attempt in 0..3 {
        if !wait_ibf_clear(ioport_cap)? {
            return Ok(false);
        }
        sys_ioport_outb(ioport_cap, I8042_CMD_PORT, CMD_WRITE_AUX)?;
        if !wait_ibf_clear(ioport_cap)? {
            return Ok(false);
        }
        sys_ioport_outb(ioport_cap, I8042_DATA_PORT, cmd)?;

        let mut resend = false;
        for _ in 0..4096 {
            let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
            if (st & STATUS_OBF) != 0 {
                let b = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
                if (st & STATUS_AUX_DATA) == 0 {
                    continue;
                }
                if b == PS2_RESP_ACK {
                    return Ok(true);
                }
                if b == PS2_RESP_RESEND {
                    resend = true;
                    break;
                }
            }
        }
        if !resend {
            return Ok(false);
        }
    }
    Ok(false)
}

/// Initialize the QEMU/PC i8042 keyboard path prior to unmasking `IRQ1`:
/// flushes stale OBF bytes, enables Set-1 translation + keyboard scanning (`0xF4`), and enables `IRQ1`.
pub fn init_i8042_keyboard(ioport_cap: u64) -> Result<(), isize> {
    flush_obf(ioport_cap)?;
    let ccb = read_ccb(ioport_cap)?;
    let setup_ccb = (ccb | CCB_TRANSLATE_SET1 | CCB_AUX_CLOCK_DISABLE)
        & !(CCB_IRQ1_ENABLE | CCB_IRQ12_ENABLE | CCB_KBD_CLOCK_DISABLE);
    write_ccb(ioport_cap, setup_ccb)?;
    let _ = send_kbd_cmd_expect_ack(ioport_cap, DEV_CMD_ENABLE_SCAN)?;
    let active_ccb = setup_ccb | CCB_IRQ1_ENABLE;
    write_ccb(ioport_cap, active_ccb)?;
    flush_obf(ioport_cap)
}

/// Initialize the PS/2 auxiliary mouse device (`0xA8`, `0xD4 -> 0xF6/0xF4`, CCB `IRQ1 | IRQ12`)
/// under single-controller serialization while IOAPIC pins are masked/quiesced.
///
/// Returns `Ok(true)` if the PS/2 mouse acknowledged setup, or `Ok(false)` if absent/disconnected
/// (preserving keyboard `IRQ1` operation cleanly).
pub fn init_i8042_mouse(ioport_cap: u64) -> Result<bool, isize> {
    flush_obf(ioport_cap)?;

    // Mask both IRQ1 and IRQ12 inside CCB during AUX command sequencing so keyboard/mouse replies
    // cannot interleave with asynchronous interrupts.
    let base_ccb = read_ccb(ioport_cap)?;
    let quiet_ccb = (base_ccb | CCB_TRANSLATE_SET1)
        & !(CCB_IRQ1_ENABLE | CCB_IRQ12_ENABLE | CCB_KBD_CLOCK_DISABLE | CCB_AUX_CLOCK_DISABLE);
    write_ccb(ioport_cap, quiet_ccb)?;

    let _ = wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_CMD_PORT, CMD_ENABLE_AUX)?;
    flush_obf(ioport_cap)?;

    let defaults_ok = send_aux_cmd_expect_ack(ioport_cap, MOUSE_CMD_SET_DEFAULTS)?;
    let enable_ok = if defaults_ok {
        send_aux_cmd_expect_ack(ioport_cap, DEV_CMD_ENABLE_SCAN)?
    } else {
        false
    };

    let final_ccb = if enable_ok {
        quiet_ccb | CCB_IRQ1_ENABLE | CCB_IRQ12_ENABLE
    } else {
        (quiet_ccb | CCB_IRQ1_ENABLE | CCB_AUX_CLOCK_DISABLE) & !CCB_IRQ12_ENABLE
    };
    write_ccb(ioport_cap, final_ccb)?;
    flush_obf(ioport_cap)?;
    Ok(enable_ok)
}
