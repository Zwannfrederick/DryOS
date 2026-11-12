// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 PS/2 i8042 keyboard backend (`0x0060` data, `0x0064` status/command via `IoPortCap`).
//!
//! Zero protocol state lives in Ring 0: controller setup, FIFO draining, `0xE0`/`0xE1` prefix
//! framing, modifier tracking, typematic repeat tagging, and invalid scancode filtering execute
//! entirely inside isolated `CPL=3` `dry-input`.

use crate::input::{
    InputEvent, InputEventKind, InputRingQueue, DEV_ID_PS2_KBD, EV_FLAG_EXTENDED, EV_FLAG_REPEAT,
    MOD_ALT, MOD_CTRL, MOD_SHIFT,
};
use crate::keymap::{
    translate_set1, KeymapLayout, KEY_LALT, KEY_LCTRL, KEY_LSHIFT, KEY_NONE, KEY_RALT, KEY_RCTRL,
    KEY_RSHIFT,
};
use crate::{sys_ioport_inb, sys_ioport_outb};

pub const I8042_DATA_PORT: u16 = 0x0060;
pub const I8042_STATUS_PORT: u16 = 0x0064;
pub const I8042_CMD_PORT: u16 = 0x0064;

const STATUS_OBF: u8 = 1 << 0;
const STATUS_IBF: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

const CMD_READ_CCB: u8 = 0x20;
const CMD_WRITE_CCB: u8 = 0x60;
const KBD_CMD_ENABLE_SCAN: u8 = 0xF4;

const CCB_IRQ1_ENABLE: u8 = 1 << 0;
const CCB_KBD_CLOCK_DISABLE: u8 = 1 << 4;
const CCB_TRANSLATE_SET1: u8 = 1 << 6;

const MOD_BIT_LSHIFT: u8 = 1 << 0;
const MOD_BIT_RSHIFT: u8 = 1 << 1;
const MOD_BIT_LCTRL: u8 = 1 << 2;
const MOD_BIT_RCTRL: u8 = 1 << 3;
const MOD_BIT_LALT: u8 = 1 << 4;
const MOD_BIT_RALT: u8 = 1 << 5;

pub struct Ps2Decoder {
    layout: KeymapLayout,
    next_seq: u32,
    e0_pending: bool,
    e1_skip: u8,
    raw_mod_bits: u8,
    pressed_bitmap: [u64; 4],
    invalid_bytes: u64,
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
        }
    }

    #[inline]
    pub const fn invalid_bytes(&self) -> u64 {
        self.invalid_bytes
    }

    #[inline]
    fn active_modifiers(&self) -> u8 {
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

    /// Feed a single Set-1 scancode byte from port `0x60` and emit a normalized `InputEvent` if complete.
    pub fn feed_byte(&mut self, byte: u8) -> Option<InputEvent> {
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
            // Controller responses & error tokens (`ACK`, `RESEND`, `BAT_OK`, overrun `0x00`/`0xFF`).
            0x00 | 0xAA | 0xEE | 0xFA | 0xFC | 0xFD | 0xFE | 0xFF => {
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

        // Ignore fake shift make/break prefixes (`0xE0 0x2A` / `0xE0 0xAA`) emitted around PrintScreen.
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

        // First probe keycode so modifier transitions update BEFORE computing effective modifiers.
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
            buttons: 0,
        })
    }

    /// Drain up to 32 pending bytes from the i8042 output buffer into `queue`.
    pub fn drain_hw_fifo<const N: usize>(
        &mut self,
        ioport_cap: u64,
        queue: &mut InputRingQueue<N>,
    ) -> Result<usize, isize> {
        let mut pushed = 0usize;
        for _ in 0..32 {
            let status = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
            if (status & STATUS_OBF) == 0 {
                break;
            }
            let data = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
            if (status & STATUS_AUX_DATA) != 0 {
                // AUX PS/2 mouse channel byte drained so i8042 never stalls; mouse backend attaches in P2.
                continue;
            }
            if let Some(ev) = self.feed_byte(data) {
                let _ = queue.push(ev);
                pushed += 1;
            }
        }
        Ok(pushed)
    }
}

fn wait_ibf_clear(ioport_cap: u64) -> Result<(), isize> {
    for _ in 0..1024 {
        let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
        if (st & STATUS_IBF) == 0 {
            return Ok(());
        }
    }
    Ok(())
}

fn flush_obf(ioport_cap: u64) -> Result<(), isize> {
    for _ in 0..32 {
        let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
        if (st & STATUS_OBF) == 0 {
            break;
        }
        let _ = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
    }
    Ok(())
}

/// Initialize the QEMU/PC i8042 keyboard path prior to unmasking `IRQ1`:
/// flushes stale OBF bytes, enables Set-1 translation + keyboard scanning (`0xF4`), and enables `IRQ1`.
pub fn init_i8042_keyboard(ioport_cap: u64) -> Result<(), isize> {
    flush_obf(ioport_cap)?;

    wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_CMD_PORT, CMD_READ_CCB)?;
    let mut ccb = CCB_TRANSLATE_SET1;
    for _ in 0..1024 {
        let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
        if (st & STATUS_OBF) != 0 {
            ccb = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
            break;
        }
    }

    // Enable keyboard clock + Set-1 translation with `IRQ1` temporarily masked while sending `0xF4`.
    let setup_ccb = (ccb | CCB_TRANSLATE_SET1) & !(CCB_IRQ1_ENABLE | CCB_KBD_CLOCK_DISABLE);
    wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_CMD_PORT, CMD_WRITE_CCB)?;
    wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_DATA_PORT, setup_ccb)?;

    // Ensure keyboard scanning is enabled (`0xF4`) and consume `0xFA` ACK.
    wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_DATA_PORT, KBD_CMD_ENABLE_SCAN)?;
    for _ in 0..1024 {
        let st = sys_ioport_inb(ioport_cap, I8042_STATUS_PORT)?;
        if (st & STATUS_OBF) != 0 {
            let _ = sys_ioport_inb(ioport_cap, I8042_DATA_PORT)?;
            break;
        }
    }

    // Enable `IRQ1` generation (`bit 0 = 1`) and drain any residual byte before IOAPIC unmask.
    let active_ccb = setup_ccb | CCB_IRQ1_ENABLE;
    wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_CMD_PORT, CMD_WRITE_CCB)?;
    wait_ibf_clear(ioport_cap)?;
    sys_ioport_outb(ioport_cap, I8042_DATA_PORT, active_ccb)?;
    flush_obf(ioport_cap)
}
