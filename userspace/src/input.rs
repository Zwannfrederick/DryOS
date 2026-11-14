// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Device-agnostic Ring 3 input event contract & bounded non-blocking `InputRingQueue`.
//!
//! Separates hardware backends (`ps2.rs`, future USB HID) from downstream consumers (`dry-display`
//! window manager, `dry-shell`, `dry-sysmon`) while preserving strict `seq` ordering, 48-bit TSC
//! timestamps, motion coalescing under burst load, and deterministic overflow accounting.

pub const DEV_ID_PS2_KBD: u16 = 0x0001;
pub const DEV_ID_PS2_MOUSE: u16 = 0x0002;
pub const DEV_ID_SYNTH_TEST: u16 = 0x00ff;

pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CTRL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;

pub const EV_FLAG_REPEAT: u8 = 1 << 0;
pub const EV_FLAG_EXTENDED: u8 = 1 << 1;

pub const MOUSE_BTN_LEFT: u8 = 1 << 0;
pub const MOUSE_BTN_RIGHT: u8 = 1 << 1;
pub const MOUSE_BTN_MIDDLE: u8 = 1 << 2;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEventKind {
    None = 0,
    KeyPress = 1,
    KeyRelease = 2,
    MouseMove = 3,
    MouseButtonDown = 4,
    MouseButtonUp = 5,
    MouseScroll = 6,
    WindowConfig = 7,
}

impl InputEventKind {
    pub const POINTER_MOVE: Self = Self::MouseMove;

    #[inline]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::KeyPress,
            2 => Self::KeyRelease,
            3 => Self::MouseMove,
            4 => Self::MouseButtonDown,
            5 => Self::MouseButtonUp,
            6 => Self::MouseScroll,
            7 => Self::WindowConfig,
            _ => Self::None,
        }
    }
}

/// Canonical device-agnostic input event (`KeyPress`/`KeyRelease` + `MouseMove`/`MouseButton*`/`MouseScroll`).
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
    pub wheel: i8,
    /// Low 48 bits of `rdtsc_user()` captured when the event was decoded in `dry-input`.
    pub timestamp_tsc48: u64,
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
        wheel: 0,
        timestamp_tsc48: 0,
    };

    /// Construct a `WindowConfig` notification delivering new client dimensions and theme/DPI flags.
    #[inline]
    pub const fn window_config(client_w: u16, client_h: u16, theme_flags: u8) -> Self {
        Self {
            seq: 0,
            device_id: 0,
            kind: InputEventKind::WindowConfig,
            modifiers: theme_flags,
            scancode: 0,
            keycode: 0,
            ascii: 0,
            flags: 0,
            dx: client_w as i16,
            dy: client_h as i16,
            buttons: 0,
            wheel: 0,
            timestamp_tsc48: 0,
        }
    }

    /// Pack event + low-16-bit `overflow_count` + 48-bit TSC timestamp into inline IPC `R12..R15`.
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
            | ((self.buttons as u64) << 32)
            | (((self.wheel as u8) as u64) << 40);
        let ovf16 = if overflow_count > 0xffff {
            0xffff
        } else {
            overflow_count & 0xffff
        };
        let mr3 = ovf16 | ((self.timestamp_tsc48 & 0xffff_ffff_ffff) << 16);
        [mr0, mr1, mr2, mr3]
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
            wheel: (((mrs[2] >> 40) & 0xff) as u8) as i8,
            timestamp_tsc48: mrs[3] >> 16,
        };
        (ev, mrs[3] & 0xffff)
    }

    /// Compute elapsed TSC cycles modulo `2^48` between `timestamp_tsc48` and `now_tsc`.
    #[inline]
    pub const fn latency_cycles(self, now_tsc: u64) -> u64 {
        if self.timestamp_tsc48 == 0 {
            0
        } else {
            (now_tsc.wrapping_sub(self.timestamp_tsc48)) & 0xffff_ffff_ffff
        }
    }
}

/// Fixed-capacity FIFO ring buffer (`N = 64` in `dry-input`) with explicit overflow accounting.
pub struct InputRingQueue<const N: usize> {
    buf: [InputEvent; N],
    head: usize,
    tail: usize,
    len: usize,
    overflow_count: u64,
    coalesced_moves: u64,
}

impl<const N: usize> InputRingQueue<N> {
    pub const fn new() -> Self {
        Self {
            buf: [InputEvent::EMPTY; N],
            head: 0,
            tail: 0,
            len: 0,
            overflow_count: 0,
            coalesced_moves: 0,
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

    #[inline]
    pub const fn coalesced_moves(&self) -> u64 {
        self.coalesced_moves
    }

    pub fn reset(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.len = 0;
        self.overflow_count = 0;
        self.coalesced_moves = 0;
    }

    /// Enqueue `ev` strictly in FIFO order; drops incoming event and increments `overflow_count` if full.
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

    /// Enqueue `ev`, coalescing consecutive `MouseMove` deltas into the tail entry when queued so
    /// high-rate pointer motion bursts never starve discrete keyboard or mouse-button transitions.
    #[inline]
    pub fn push_coalescing(&mut self, ev: InputEvent) -> bool {
        if ev.kind == InputEventKind::MouseMove && self.len > 0 {
            let last_idx = (self.tail + N - 1) % N;
            let last = &mut self.buf[last_idx];
            if last.kind == InputEventKind::MouseMove
                && last.buttons == ev.buttons
                && last.modifiers == ev.modifiers
            {
                if let (Some(ndx), Some(ndy)) = (last.dx.checked_add(ev.dx), last.dy.checked_add(ev.dy))
                {
                    last.dx = ndx;
                    last.dy = ndy;
                    last.seq = ev.seq;
                    last.timestamp_tsc48 = ev.timestamp_tsc48;
                    self.coalesced_moves = self.coalesced_moves.wrapping_add(1);
                    return true;
                }
            }
        }
        self.push(ev)
    }

    /// Enqueue `ev`, replacing a trailing entry of the same kind (`MouseMove` with client-relative
    /// coordinates or `WindowConfig`) so window queues retain only the latest state snapshot.
    #[inline]
    pub fn push_replacing_abs(&mut self, ev: InputEvent) -> bool {
        if self.len > 0
            && matches!(
                ev.kind,
                InputEventKind::MouseMove | InputEventKind::WindowConfig
            )
        {
            let last_idx = (self.tail + N - 1) % N;
            let last = &mut self.buf[last_idx];
            if last.kind == ev.kind
                && (ev.kind == InputEventKind::WindowConfig
                    || (last.buttons == ev.buttons && last.modifiers == ev.modifiers))
            {
                *last = ev;
                self.coalesced_moves = self.coalesced_moves.wrapping_add(1);
                return true;
            }
        }
        self.push(ev)
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
