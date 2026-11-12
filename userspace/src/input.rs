// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Device-agnostic Ring 3 input event contract & bounded non-blocking `InputRingQueue`.
//!
//! Separates hardware backends (`ps2.rs`, future USB HID) from downstream consumers (`dry-shell`,
//! desktop compositor) while preserving strict event ordering and deterministic overflow accounting.

pub const DEV_ID_PS2_KBD: u16 = 0x0001;
pub const DEV_ID_SYNTH_TEST: u16 = 0x00ff;

pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CTRL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;

pub const EV_FLAG_REPEAT: u8 = 1 << 0;
pub const EV_FLAG_EXTENDED: u8 = 1 << 1;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEventKind {
    None = 0,
    KeyPress = 1,
    KeyRelease = 2,
    PointerMove = 3,
    PointerButton = 4,
}

impl InputEventKind {
    #[inline]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::KeyPress,
            2 => Self::KeyRelease,
            3 => Self::PointerMove,
            4 => Self::PointerButton,
            _ => Self::None,
        }
    }
}

/// Canonical device-agnostic input event (`KeyPress`/`KeyRelease` + forward-compatible pointer model).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEvent {
    pub seq: u32,
    pub device_id: u16,
    pub kind: InputEventKind,
    pub modifiers: u8,
    pub scancode: u16,
    pub keycode: u16,
    pub ascii: u8,
    pub flags: u8,
    pub dx: i16,
    pub dy: i16,
    pub buttons: u8,
}

impl InputEvent {
    pub const EMPTY: Self = Self {
        seq: 0,
        device_id: 0,
        kind: InputEventKind::None,
        modifiers: 0,
        scancode: 0,
        keycode: 0,
        ascii: 0,
        flags: 0,
        dx: 0,
        dy: 0,
        buttons: 0,
    };

    /// Pack event + cumulative `overflow_count` into the 4 inline IPC registers (`R12..R15`).
    #[inline]
    pub const fn to_ipc_mrs(self, overflow_count: u64) -> [u64; 4] {
        let mr0 = (self.seq as u64)
            | ((self.device_id as u64) << 32)
            | ((self.kind as u64) << 48)
            | ((self.modifiers as u64) << 56);
        let mr1 = (self.scancode as u64)
            | ((self.keycode as u64) << 16)
            | ((self.ascii as u64) << 32)
            | ((self.flags as u64) << 40);
        let mr2 = ((self.dx as u16) as u64)
            | (((self.dy as u16) as u64) << 16)
            | ((self.buttons as u64) << 32);
        [mr0, mr1, mr2, overflow_count]
    }

    /// Unpack `(InputEvent, overflow_count)` from the 4 inline IPC registers (`R12..R15`).
    #[inline]
    pub const fn from_ipc_mrs(mrs: [u64; 4]) -> (Self, u64) {
        let ev = Self {
            seq: (mrs[0] & 0xffff_ffff) as u32,
            device_id: ((mrs[0] >> 32) & 0xffff) as u16,
            kind: InputEventKind::from_u8(((mrs[0] >> 48) & 0xff) as u8),
            modifiers: ((mrs[0] >> 56) & 0xff) as u8,
            scancode: (mrs[1] & 0xffff) as u16,
            keycode: ((mrs[1] >> 16) & 0xffff) as u16,
            ascii: ((mrs[1] >> 32) & 0xff) as u8,
            flags: ((mrs[1] >> 40) & 0xff) as u8,
            dx: ((mrs[2] & 0xffff) as u16) as i16,
            dy: (((mrs[2] >> 16) & 0xffff) as u16) as i16,
            buttons: ((mrs[2] >> 32) & 0xff) as u8,
        };
        (ev, mrs[3])
    }
}

/// Fixed-capacity FIFO ring buffer (`N = 64` in `dry-input`) with explicit overflow accounting.
pub struct InputRingQueue<const N: usize> {
    buf: [InputEvent; N],
    head: usize,
    tail: usize,
    len: usize,
    overflow_count: u64,
}

impl<const N: usize> InputRingQueue<N> {
    pub const fn new() -> Self {
        Self {
            buf: [InputEvent::EMPTY; N],
            head: 0,
            tail: 0,
            len: 0,
            overflow_count: 0,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub const fn overflow_count(&self) -> u64 {
        self.overflow_count
    }

    /// Enqueue `ev` without blocking; drops incoming event and increments `overflow_count` if full.
    #[inline]
    pub fn push(&mut self, ev: InputEvent) -> bool {
        if self.len >= N {
            self.overflow_count = self.overflow_count.wrapping_add(1);
            return false;
        }
        self.buf[self.tail] = ev;
        self.tail = (self.tail + 1) % N;
        self.len += 1;
        true
    }

    #[inline]
    pub fn pop(&mut self) -> Option<InputEvent> {
        if self.len == 0 {
            return None;
        }
        let ev = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(ev)
    }
}

impl<const N: usize> Default for InputRingQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}
