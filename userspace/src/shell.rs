// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 Interactive Shell Engine (`InputEditor`, `CommandParser`, `ScrollbackBuffer`,
//! `SurfacePresenter`, and live telemetry formatters).

use crate::surface::{
    SharedTerminalSurface, SurfaceCell, STYLE_ACCENT, STYLE_DEFAULT, STYLE_HEADER, STYLE_MUTED,
    STYLE_OK, STYLE_PROMPT, STYLE_WARN, TERM_COLS, TERM_ROWS,
};
use crate::UserSysInfo;

pub const SCROLLBACK_CAP: usize = 64;
pub const PROMPT_STR: &[u8] = b"dry@ring3:~$ ";
pub const MAX_INPUT_LEN: usize = TERM_COLS - PROMPT_STR.len() - 2; // 65 chars

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShellCommand<'a> {
    Empty,
    Help,
    Clear,
    System,
    Uptime,
    Echo(&'a [u8]),
    OverflowTest,
    CrashInput,
    Shutdown,
    Unknown(&'a [u8]),
}

/// Pure parser layer: splits trimmed input into command verb and argument slice.
pub fn parse_command(raw: &[u8]) -> ShellCommand<'_> {
    let mut start = 0usize;
    while start < raw.len() && raw[start] == b' ' {
        start += 1;
    }
    let mut end = raw.len();
    while end > start && raw[end - 1] == b' ' {
        end -= 1;
    }
    let trimmed = &raw[start..end];
    if trimmed.is_empty() {
        return ShellCommand::Empty;
    }

    let mut verb_end = 0usize;
    while verb_end < trimmed.len() && trimmed[verb_end] != b' ' {
        verb_end += 1;
    }
    let verb = &trimmed[..verb_end];
    let mut arg_start = verb_end;
    while arg_start < trimmed.len() && trimmed[arg_start] == b' ' {
        arg_start += 1;
    }
    let rest = &trimmed[arg_start..];

    match verb {
        b"help" => ShellCommand::Help,
        b"clear" => ShellCommand::Clear,
        b"system" => ShellCommand::System,
        b"uptime" => ShellCommand::Uptime,
        b"echo" => ShellCommand::Echo(rest),
        b"overflow-test" => ShellCommand::OverflowTest,
        b"crash-input" => ShellCommand::CrashInput,
        b"shutdown" | b"exit" => ShellCommand::Shutdown,
        _ => ShellCommand::Unknown(verb),
    }
}

/// Compact 80-column stack formatter (zero heap, zero `core::fmt` bloat).
pub struct LineBuilder {
    buf: [u8; TERM_COLS],
    len: usize,
}

impl LineBuilder {
    pub const fn new() -> Self {
        Self {
            buf: [b' '; TERM_COLS],
            len: 0,
        }
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn push_bytes(&mut self, s: &[u8]) {
        for &b in s {
            if self.len >= TERM_COLS {
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    pub fn push_u64(&mut self, mut val: u64) {
        if val == 0 {
            self.push_bytes(b"0");
            return;
        }
        let mut tmp = [0u8; 20];
        let mut n = 0usize;
        while val > 0 {
            tmp[n] = b'0' + (val % 10) as u8;
            val /= 10;
            n += 1;
        }
        while n > 0 {
            n -= 1;
            self.push_bytes(&tmp[n..=n]);
        }
    }

    pub fn push_u64_pad3(&mut self, val: u64) {
        let d2 = ((val / 100) % 10) as u8 + b'0';
        let d1 = ((val / 10) % 10) as u8 + b'0';
        let d0 = (val % 10) as u8 + b'0';
        self.push_bytes(&[d2, d1, d0]);
    }
}

impl Default for LineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Interactive line editor state (cursor column, insertion, backspace, delete).
pub struct InputEditor {
    buf: [u8; MAX_INPUT_LEN],
    len: usize,
    cursor: usize,
}

impl InputEditor {
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_INPUT_LEN],
            len: 0,
            cursor: 0,
        }
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    #[inline]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.cursor = 0;
    }

    pub fn insert_ascii(&mut self, ch: u8) -> bool {
        if !(0x20..=0x7E).contains(&ch) || self.len >= MAX_INPUT_LEN {
            return false;
        }
        let mut i = self.len;
        while i > self.cursor {
            self.buf[i] = self.buf[i - 1];
            i -= 1;
        }
        self.buf[self.cursor] = ch;
        self.len += 1;
        self.cursor += 1;
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let mut i = self.cursor - 1;
        while i + 1 < self.len {
            self.buf[i] = self.buf[i + 1];
            i += 1;
        }
        self.len -= 1;
        self.cursor -= 1;
        true
    }

    pub fn delete_at_cursor(&mut self) -> bool {
        if self.cursor >= self.len {
            return false;
        }
        let mut i = self.cursor;
        while i + 1 < self.len {
            self.buf[i] = self.buf[i + 1];
            i += 1;
        }
        self.len -= 1;
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor > 0 {
            self.cursor -= 1;
            true
        } else {
            false
        }
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor < self.len {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    pub fn move_home(&mut self) -> bool {
        let changed = self.cursor != 0;
        self.cursor = 0;
        changed
    }

    pub fn move_end(&mut self) -> bool {
        let changed = self.cursor != self.len;
        self.cursor = self.len;
        changed
    }
}

impl Default for InputEditor {
    fn default() -> Self {
        Self::new()
    }
}

/// Bounded 64-line scrollback buffer + shell session state.
pub struct ShellEngine {
    lines: [[SurfaceCell; TERM_COLS]; SCROLLBACK_CAP],
    head: usize,
    count: usize,
    scroll_up: usize,
    pub editor: InputEditor,
    pub cmd_count: u32,
    full_dirty: bool,
}

impl ShellEngine {
    pub const fn new() -> Self {
        Self {
            lines: [[SurfaceCell::BLANK; TERM_COLS]; SCROLLBACK_CAP],
            head: 0,
            count: 0,
            scroll_up: 0,
            editor: InputEditor::new(),
            cmd_count: 0,
            full_dirty: true,
        }
    }

    pub fn init_banner(&mut self) {
        self.clear_history();
        self.push_line(
            b"DRY OS Interactive Shell v0.1  [CPL=3 Userspace Compartment 0x0102]",
            STYLE_HEADER,
        );
        self.push_line(
            b"Type 'help' for commands. Use Alt+Arrow keys to move this window.",
            STYLE_MUTED,
        );
        self.push_line(b"", STYLE_DEFAULT);
    }

    pub fn clear_history(&mut self) {
        self.head = 0;
        self.count = 0;
        self.scroll_up = 0;
        self.full_dirty = true;
    }

    pub fn scroll_history(&mut self, delta_up: isize) -> bool {
        let max_hist_rows = TERM_ROWS - 1;
        let max_scroll = self.count.saturating_sub(max_hist_rows);
        let next = if delta_up >= 0 {
            (self.scroll_up + (delta_up as usize)).min(max_scroll)
        } else {
            self.scroll_up.saturating_sub((-delta_up) as usize)
        };
        if next != self.scroll_up {
            self.scroll_up = next;
            self.full_dirty = true;
            true
        } else {
            false
        }
    }

    pub fn push_line(&mut self, bytes: &[u8], style: u8) {
        self.scroll_up = 0;
        self.full_dirty = true;
        if bytes.is_empty() {
            self.push_raw_row(&[], style);
            return;
        }
        let mut offset = 0usize;
        while offset < bytes.len() {
            let end = (offset + TERM_COLS).min(bytes.len());
            self.push_raw_row(&bytes[offset..end], style);
            offset = end;
        }
    }

    pub fn push_echoed_prompt(&mut self, input: &[u8]) {
        self.scroll_up = 0;
        self.full_dirty = true;
        let slot = if self.count < SCROLLBACK_CAP {
            let idx = (self.head + self.count) % SCROLLBACK_CAP;
            self.count += 1;
            idx
        } else {
            let idx = self.head;
            self.head = (self.head + 1) % SCROLLBACK_CAP;
            idx
        };
        let row = &mut self.lines[slot];
        *row = [SurfaceCell::BLANK; TERM_COLS];
        let mut col = 0usize;
        for &b in PROMPT_STR {
            if col < TERM_COLS {
                row[col] = SurfaceCell {
                    ch: b,
                    style: STYLE_PROMPT,
                };
                col += 1;
            }
        }
        for &b in input {
            if col < TERM_COLS {
                row[col] = SurfaceCell {
                    ch: b,
                    style: STYLE_DEFAULT,
                };
                col += 1;
            }
        }
    }

    fn push_raw_row(&mut self, slice: &[u8], style: u8) {
        let slot = if self.count < SCROLLBACK_CAP {
            let idx = (self.head + self.count) % SCROLLBACK_CAP;
            self.count += 1;
            idx
        } else {
            let idx = self.head;
            self.head = (self.head + 1) % SCROLLBACK_CAP;
            idx
        };
        let row = &mut self.lines[slot];
        *row = [SurfaceCell::BLANK; TERM_COLS];
        for (i, &ch) in slice.iter().take(TERM_COLS).enumerate() {
            row[i] = SurfaceCell { ch, style };
        }
    }

    pub fn format_help(&mut self) {
        self.push_line(b"Available Ring 3 shell commands:", STYLE_ACCENT);
        self.push_line(b"  help           Show this command summary & shortcuts", STYLE_DEFAULT);
        self.push_line(b"  clear          Clear terminal scrollback & viewport", STYLE_DEFAULT);
        self.push_line(b"  system         Live kernel CPU, APIC, RAM, IRQ & IPC telemetry", STYLE_DEFAULT);
        self.push_line(b"  uptime         Live monotonic TSC uptime & calibrated clock", STYLE_DEFAULT);
        self.push_line(b"  echo <text>    Print text arguments to terminal", STYLE_DEFAULT);
        self.push_line(b"  overflow-test  Verify bounded dry-input ring overflow guard", STYLE_DEFAULT);
        self.push_line(b"  crash-input    Trigger dry-input #UD crash & supervisor restart", STYLE_DEFAULT);
        self.push_line(b"  shutdown       Cleanly stop Ring 3 services & exit QEMU", STYLE_DEFAULT);
        self.push_line(b"Shortcuts: Alt+Arrows move floating window | Shift+Up/Down scroll", STYLE_MUTED);
    }

    pub fn format_system(&mut self, info: &UserSysInfo) {
        let apic_str: &[u8] = match info.apic_mode {
            1 => b"x2APIC-MSR",
            2 => b"xAPIC-MMIO",
            _ => b"Disabled",
        };
        let usable_mib = (info.usable_frames_4k as u64 * 4) / 1024;
        let free_mib = (info.free_frames_4k as u64 * 4) / 1024;

        let mut l1 = LineBuilder::new();
        l1.push_bytes(b"Kernel: DRY Microkernel ABI=0x10000 | CPUs=");
        l1.push_u64(info.online_cpus as u64);
        l1.push_bytes(b" | APIC=");
        l1.push_bytes(apic_str);
        self.push_line(l1.as_bytes(), STYLE_OK);

        let mut l2 = LineBuilder::new();
        l2.push_bytes(b"HW: IOAPIC=");
        l2.push_bytes(if info.ioapic_present != 0 { b"ON" } else { b"OFF" });
        l2.push_bytes(b" GOP=");
        l2.push_bytes(if info.fb_present != 0 { b"ON" } else { b"HEADLESS" });
        l2.push_bytes(b" | RAM free=");
        l2.push_u64(free_mib);
        l2.push_bytes(b"/");
        l2.push_u64(usable_mib);
        l2.push_bytes(b" MiB (");
        l2.push_u64(info.free_frames_4k as u64);
        l2.push_bytes(b" 4K)");
        self.push_line(l2.as_bytes(), STYLE_DEFAULT);

        let mut l3 = LineBuilder::new();
        l3.push_bytes(b"Counts: syscalls=");
        l3.push_u64(info.total_syscalls);
        l3.push_bytes(b" ctx_sw=");
        l3.push_u64(info.total_ctx_switches);
        l3.push_bytes(b" timer_irqs=");
        l3.push_u64(info.timer_irqs);
        l3.push_bytes(b" ext_irqs=");
        l3.push_u64(info.total_ext_irqs);
        self.push_line(l3.as_bytes(), STYLE_ACCENT);
    }

    pub fn format_uptime(&mut self, info: &UserSysInfo) {
        let secs = info.uptime_ms / 1000;
        let ms = info.uptime_ms % 1000;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"Uptime: ");
        lb.push_u64(secs);
        lb.push_bytes(b".");
        lb.push_u64_pad3(ms);
        lb.push_bytes(b" s | TSC=");
        lb.push_u64(info.tsc_mhz);
        lb.push_bytes(b" MHz | elapsed_cyc=");
        lb.push_u64(info.elapsed_tsc);
        self.push_line(lb.as_bytes(), STYLE_OK);
    }

    pub fn format_unknown(&mut self, verb: &[u8]) {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"Unknown command: '");
        lb.push_bytes(verb);
        lb.push_bytes(b"' (type 'help')");
        self.push_line(lb.as_bytes(), STYLE_WARN);
    }

    /// Project visible scrollback + active prompt line into `surf`, setting minimal dirty row bounds.
    pub fn project_to_surface(&mut self, surf: &mut SharedTerminalSurface, sys: Option<&UserSysInfo>) {
        let hist_viewport = TERM_ROWS - 1;
        let end_idx = self.count.saturating_sub(self.scroll_up);
        let visible_hist = end_idx.min(hist_viewport);
        let start_idx = end_idx - visible_hist;

        let prompt_row = visible_hist; // Immediately below history while `< 23` lines, then pinned at row 23!
        if self.full_dirty {
            for r in 0..TERM_ROWS {
                let row_slice = &mut surf.cells[r * TERM_COLS..(r + 1) * TERM_COLS];
                if r < visible_hist {
                    let ring_idx = (self.head + start_idx + r) % SCROLLBACK_CAP;
                    row_slice.copy_from_slice(&self.lines[ring_idx]);
                } else {
                    row_slice.fill(SurfaceCell::BLANK);
                }
            }
            surf.dirty_row_min = 0;
            surf.dirty_row_max = (TERM_ROWS - 1) as u16;
            self.full_dirty = false;
        } else {
            surf.dirty_row_min = prompt_row as u16;
            surf.dirty_row_max = prompt_row as u16;
        }

        let prow = &mut surf.cells[prompt_row * TERM_COLS..(prompt_row + 1) * TERM_COLS];
        prow.fill(SurfaceCell::BLANK);
        let mut col = 0usize;
        for &b in PROMPT_STR {
            prow[col] = SurfaceCell {
                ch: b,
                style: STYLE_PROMPT,
            };
            col += 1;
        }
        for &b in self.editor.as_bytes() {
            if col < TERM_COLS {
                prow[col] = SurfaceCell {
                    ch: b,
                    style: STYLE_DEFAULT,
                };
                col += 1;
            }
        }

        surf.cursor_col = (PROMPT_STR.len() + self.editor.cursor()).min(TERM_COLS - 1) as u16;
        surf.cursor_row = prompt_row as u16;
        surf.cmd_count = self.cmd_count;
        if let Some(s) = sys {
            surf.ext_irqs_snap = s.total_ext_irqs as u32;
            surf.uptime_sec_snap = (s.uptime_ms / 1000) as u32;
        }
        surf.seq = surf.seq.wrapping_add(1);
    }
}

impl Default for ShellEngine {
    fn default() -> Self {
        Self::new()
    }
}
