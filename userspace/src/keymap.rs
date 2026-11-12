// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 keycode & keymap translation (`US QWERTY` implemented; extensible `TurkishQ` interface).

use crate::input::MOD_SHIFT;

pub const KEY_NONE: u16 = 0;
pub const KEY_ESC: u16 = 1;
pub const KEY_ENTER: u16 = 2;
pub const KEY_BACKSPACE: u16 = 3;
pub const KEY_TAB: u16 = 4;
pub const KEY_SPACE: u16 = 5;

pub const KEY_LSHIFT: u16 = 10;
pub const KEY_RSHIFT: u16 = 11;
pub const KEY_LCTRL: u16 = 12;
pub const KEY_RCTRL: u16 = 13;
pub const KEY_LALT: u16 = 14;
pub const KEY_RALT: u16 = 15;

pub const KEY_UP: u16 = 20;
pub const KEY_DOWN: u16 = 21;
pub const KEY_LEFT: u16 = 22;
pub const KEY_RIGHT: u16 = 23;
pub const KEY_HOME: u16 = 24;
pub const KEY_END: u16 = 25;
pub const KEY_DELETE: u16 = 26;
pub const KEY_PAGEUP: u16 = 27;
pub const KEY_PAGEDOWN: u16 = 28;

pub const KEY_CHAR_BASE: u16 = 0x0100;

/// Extensible keyboard layout selector.
///
/// NOTE: Only `UsQwerty` is implemented in P1. `TurkishQ` is intentionally declared as an
/// unimplemented layout variant (`Err(KeymapError::LayoutNotImplemented)`) so UTF-8 / multi-byte
/// glyph composition can be plugged in during P2 without breaking the `dry-input` ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapLayout {
    UsQwerty,
    TurkishQ,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapError {
    LayoutNotImplemented,
}

/// Translate a normalized Set-1 key index (`base_code` in `0x01..=0x7F`, `extended = 0xE0` prefix)
/// under `modifiers` into `(keycode, ascii)`.
pub fn translate_set1(
    layout: KeymapLayout,
    base_code: u8,
    extended: bool,
    modifiers: u8,
) -> Result<(u16, u8), KeymapError> {
    match layout {
        KeymapLayout::UsQwerty => Ok(translate_us_qwerty(base_code, extended, modifiers)),
        KeymapLayout::TurkishQ => Err(KeymapError::LayoutNotImplemented),
    }
}

const fn translate_us_qwerty(base_code: u8, extended: bool, modifiers: u8) -> (u16, u8) {
    if extended {
        return match base_code {
            0x1C => (KEY_ENTER, b'\n'),
            0x1D => (KEY_RCTRL, 0),
            0x35 => (KEY_CHAR_BASE | (b'/' as u16), b'/'),
            0x38 => (KEY_RALT, 0),
            0x47 => (KEY_HOME, 0),
            0x48 => (KEY_UP, 0),
            0x49 => (KEY_PAGEUP, 0),
            0x4B => (KEY_LEFT, 0),
            0x4D => (KEY_RIGHT, 0),
            0x4F => (KEY_END, 0),
            0x50 => (KEY_DOWN, 0),
            0x51 => (KEY_PAGEDOWN, 0),
            0x53 => (KEY_DELETE, 0),
            _ => (KEY_NONE, 0),
        };
    }

    let shift = (modifiers & MOD_SHIFT) != 0;
    let (kc, normal, shifted): (u16, u8, u8) = match base_code {
        0x01 => return (KEY_ESC, 0x1b),
        0x02 => (b'1' as u16, b'1', b'!'),
        0x03 => (b'2' as u16, b'2', b'@'),
        0x04 => (b'3' as u16, b'3', b'#'),
        0x05 => (b'4' as u16, b'4', b'$'),
        0x06 => (b'5' as u16, b'5', b'%'),
        0x07 => (b'6' as u16, b'6', b'^'),
        0x08 => (b'7' as u16, b'7', b'&'),
        0x09 => (b'8' as u16, b'8', b'*'),
        0x0A => (b'9' as u16, b'9', b'('),
        0x0B => (b'0' as u16, b'0', b')'),
        0x0C => (b'-' as u16, b'-', b'_'),
        0x0D => (b'=' as u16, b'=', b'+'),
        0x0E => return (KEY_BACKSPACE, 0x08),
        0x0F => return (KEY_TAB, b'\t'),
        0x10 => (b'q' as u16, b'q', b'Q'),
        0x11 => (b'w' as u16, b'w', b'W'),
        0x12 => (b'e' as u16, b'e', b'E'),
        0x13 => (b'r' as u16, b'r', b'R'),
        0x14 => (b't' as u16, b't', b'T'),
        0x15 => (b'y' as u16, b'y', b'Y'),
        0x16 => (b'u' as u16, b'u', b'U'),
        0x17 => (b'i' as u16, b'i', b'I'),
        0x18 => (b'o' as u16, b'o', b'O'),
        0x19 => (b'p' as u16, b'p', b'P'),
        0x1A => (b'[' as u16, b'[', b'{'),
        0x1B => (b']' as u16, b']', b'}'),
        0x1C => return (KEY_ENTER, b'\n'),
        0x1D => return (KEY_LCTRL, 0),
        0x1E => (b'a' as u16, b'a', b'A'),
        0x1F => (b's' as u16, b's', b'S'),
        0x20 => (b'd' as u16, b'd', b'D'),
        0x21 => (b'f' as u16, b'f', b'F'),
        0x22 => (b'g' as u16, b'g', b'G'),
        0x23 => (b'h' as u16, b'h', b'H'),
        0x24 => (b'j' as u16, b'j', b'J'),
        0x25 => (b'k' as u16, b'k', b'K'),
        0x26 => (b'l' as u16, b'l', b'L'),
        0x27 => (b';' as u16, b';', b':'),
        0x28 => (b'\'' as u16, b'\'', b'"'),
        0x29 => (b'`' as u16, b'`', b'~'),
        0x2A => return (KEY_LSHIFT, 0),
        0x2B => (b'\\' as u16, b'\\', b'|'),
        0x2C => (b'z' as u16, b'z', b'Z'),
        0x2D => (b'x' as u16, b'x', b'X'),
        0x2E => (b'c' as u16, b'c', b'C'),
        0x2F => (b'v' as u16, b'v', b'V'),
        0x30 => (b'b' as u16, b'b', b'B'),
        0x31 => (b'n' as u16, b'n', b'N'),
        0x32 => (b'm' as u16, b'm', b'M'),
        0x33 => (b',' as u16, b',', b'<'),
        0x34 => (b'.' as u16, b'.', b'>'),
        0x35 => (b'/' as u16, b'/', b'?'),
        0x36 => return (KEY_RSHIFT, 0),
        0x38 => return (KEY_LALT, 0),
        0x39 => return (KEY_SPACE, b' '),
        _ => return (KEY_NONE, 0),
    };
    let ascii = if shift { shifted } else { normal };
    (KEY_CHAR_BASE | kc, ascii)
}
