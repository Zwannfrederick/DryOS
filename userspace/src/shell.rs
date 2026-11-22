// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 Native Terminal Engine (`InputEditor`, `CommandHistory`, `CommandParser`,
//! `ScrollbackBuffer`, `SurfacePresenter`, and real kernel/supervisor telemetry formatters).

use crate::action::{compartment_short_name, AppId, APP_COUNT};
use crate::surface::{
    SharedTerminalSurface, SurfaceCell, STYLE_ACCENT, STYLE_DEFAULT, STYLE_HEADER, STYLE_MUTED,
    STYLE_OK, STYLE_PROMPT, STYLE_WARN, TERM_COLS, TERM_ROWS,
};
use crate::{UserPowerInfo, UserProcInfo, UserSysInfo};

pub const SCROLLBACK_CAP: usize = 128;
pub const HIST_CAP: usize = 16;
pub const PROMPT_STR: &[u8] = b"dry@ring3:~$ ";
pub const MAX_INPUT_LEN: usize = TERM_COLS - PROMPT_STR.len() - 2; // 65 chars

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShellCommand<'a> {
    Empty,
    Help,
    Clear,
    System,
    SysInfo,
    Uptime,
    Ps,
    Apps,
    History,
    OpenApp(&'a [u8]),
    CloseApp(&'a [u8]),
    RestartApp(&'a [u8]),
    Tile,
    Blank,
    Lock,
    Reboot,
    Poweroff,
    Suspend,
    Echo(&'a [u8]),
    OverflowTest,
    CrashInput,
    SecAudit,
    GpuFault,
    Shutdown,
    Clip(&'a [u8]),
    StorageVault(&'a [u8], &'a [u8]),
    UnsupportedPosix(u8),
    Unknown(&'a [u8]),
}

/// Pure parser layer: rejects unsupported POSIX/bash metacharacters explicitly (never faking
/// pipelines, redirections, subshells, or env expansion) and splits verb + argument slice.
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

    for &b in trimmed {
        if matches!(b, b'|' | b'>' | b'<' | b';' | b'&' | b'$' | b'`' | b'\\') {
            return ShellCommand::UnsupportedPosix(b);
        }
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
        b"sysinfo" => ShellCommand::SysInfo,
        b"uptime" => ShellCommand::Uptime,
        b"ps" => ShellCommand::Ps,
        b"apps" => ShellCommand::Apps,
        b"history" => ShellCommand::History,
        b"open" => ShellCommand::OpenApp(rest),
        b"close" => ShellCommand::CloseApp(rest),
        b"restart" => ShellCommand::RestartApp(rest),
        b"tile" => ShellCommand::Tile,
        b"blank" => ShellCommand::Blank,
        b"lock" => ShellCommand::Lock,
        b"reboot" => ShellCommand::Reboot,
        b"poweroff" => ShellCommand::Poweroff,
        b"suspend" => ShellCommand::Suspend,
        b"echo" => ShellCommand::Echo(rest),
        b"overflow-test" => ShellCommand::OverflowTest,
        b"crash-input" => ShellCommand::CrashInput,
        b"sec-audit" => ShellCommand::SecAudit,
        b"gpufault" => ShellCommand::GpuFault,
        b"shutdown" | b"exit" => ShellCommand::Shutdown,
        b"clip" => ShellCommand::Clip(rest),
        b"ls" | b"cat" | b"write" | b"rm" | b"ln" | b"sync" | b"pwd" | b"cd" | b"mkdir"
        | b"rmdir" | b"cp" | b"mv" | b"exec" | b"linux-run" | b"i2c-hid-test"
        | b"ifconfig" | b"ping" | b"nslookup" | b"httpget"
        | b"dmesg" | b"lspci" | b"lsusb" | b"lsblk"
        | b"nvme" | b"drvctl" | b"uname" | b"free" | b"mounts" | b"vault-status"
        | b"vault-unlock" | b"vault-lock" | b"vault-passwd" | b"vault-test"
        | b"corrupt-sb0-test" => {
            ShellCommand::StorageVault(verb, rest)
        }
        _ => ShellCommand::Unknown(verb),
    }
}

/// Compact 192-column stack formatter (zero heap, zero `core::fmt` bloat).
pub struct LineBuilder {
    buf: [u8; 192],
    len: usize,
}

impl LineBuilder {
    pub const fn new() -> Self {
        Self {
            buf: [b' '; 192],
            len: 0,
        }
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    #[inline]
    pub fn push_byte(&mut self, b: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    pub fn push_bytes(&mut self, s: &[u8]) {
        for &b in s {
            if self.len >= self.buf.len() {
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    pub fn push_padded(&mut self, s: &[u8], width: usize) {
        let take = s.len().min(width);
        self.push_bytes(&s[..take]);
        for _ in take..width {
            self.push_byte(b' ');
        }
    }

    pub fn push_i64(&mut self, val: i64) {
        if val < 0 {
            self.push_byte(b'-');
            self.push_u64(val.unsigned_abs());
        } else {
            self.push_u64(val as u64);
        }
    }

    pub fn push_hex_u64(&mut self, mut val: u64) {
        if val == 0 {
            self.push_byte(b'0');
            return;
        }
        let mut tmp = [0u8; 16];
        let mut n = 0usize;
        while val > 0 {
            let nib = (val & 0x0f) as u8;
            tmp[n] = if nib < 10 { b'0' + nib } else { b'a' + (nib - 10) };
            val >>= 4;
            n += 1;
        }
        while n > 0 {
            n -= 1;
            self.push_byte(tmp[n]);
        }
    }

    pub fn push_hex_width(&mut self, val: u64, digits: usize) {
        let w = digits.clamp(1, 16);
        let mut i = w;
        while i > 0 {
            i -= 1;
            let nib = ((val >> (i * 4)) & 0x0f) as u8;
            self.push_byte(if nib < 10 { b'0' + nib } else { b'a' + (nib - 10) });
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

/// Interactive line editor state (cursor column, insertion, backspace, delete, history replacement).
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

    pub fn set_bytes(&mut self, src: &[u8]) {
        let take = src.len().min(MAX_INPUT_LEN);
        self.buf[..take].copy_from_slice(&src[..take]);
        self.len = take;
        self.cursor = take;
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

const fn default_cwd() -> [u8; 96] {
    let mut b = [0u8; 96];
    b[0] = b'/';
    b
}

/// Bounded 128-line scrollback buffer + 16-entry command history + shell session state.
pub struct ShellEngine {
    lines: [[SurfaceCell; TERM_COLS]; SCROLLBACK_CAP],
    head: usize,
    count: usize,
    total_pushed: usize,
    scroll_up: usize,
    hist_buf: [[u8; MAX_INPUT_LEN]; HIST_CAP],
    hist_lens: [u8; HIST_CAP],
    hist_head: usize,
    hist_count: usize,
    hist_nav: usize,
    draft_buf: [u8; MAX_INPUT_LEN],
    draft_len: usize,
    cwd: [u8; 96],
    cwd_len: usize,
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
            total_pushed: 0,
            scroll_up: 0,
            hist_buf: [[0; MAX_INPUT_LEN]; HIST_CAP],
            hist_lens: [0; HIST_CAP],
            hist_head: 0,
            hist_count: 0,
            hist_nav: 0,
            draft_buf: [0; MAX_INPUT_LEN],
            draft_len: 0,
            cwd: default_cwd(),
            cwd_len: 1,
            editor: InputEditor::new(),
            cmd_count: 0,
            full_dirty: true,
        }
    }

    #[inline]
    pub fn cwd_bytes(&self) -> &[u8] {
        &self.cwd[..self.cwd_len.clamp(1, self.cwd.len())]
    }

    pub fn set_cwd(&mut self, path: &[u8]) {
        self.cwd.fill(0);
        if path.is_empty() {
            self.cwd[0] = b'/';
            self.cwd_len = 1;
        } else {
            let n = path.len().min(self.cwd.len());
            self.cwd[..n].copy_from_slice(&path[..n]);
            self.cwd_len = n;
        }
    }

    pub fn init_banner(&mut self) {
        self.clear_history();
        self.push_line(
            b"DRY OS Native Terminal v0.3  [CPL=3 Userspace Compartment 0x0102]",
            STYLE_HEADER,
        );
        self.push_line(
            b"Type 'help' for commands ('sysinfo', 'ps', 'apps', 'open <app>').",
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

    pub fn record_command_history(&mut self, cmd: &[u8]) {
        self.hist_nav = 0;
        self.draft_len = 0;
        if cmd.is_empty() {
            return;
        }
        if self.hist_count > 0 {
            let last_idx = (self.hist_head + self.hist_count - 1) % HIST_CAP;
            let last_len = self.hist_lens[last_idx] as usize;
            if &self.hist_buf[last_idx][..last_len] == cmd {
                return;
            }
        }
        let slot = if self.hist_count < HIST_CAP {
            let idx = (self.hist_head + self.hist_count) % HIST_CAP;
            self.hist_count += 1;
            idx
        } else {
            let idx = self.hist_head;
            self.hist_head = (self.hist_head + 1) % HIST_CAP;
            idx
        };
        let take = cmd.len().min(MAX_INPUT_LEN);
        self.hist_buf[slot][..take].copy_from_slice(&cmd[..take]);
        self.hist_lens[slot] = take as u8;
    }

    pub fn history_prev(&mut self) -> bool {
        if self.hist_count == 0 || self.hist_nav >= self.hist_count {
            return false;
        }
        if self.hist_nav == 0 {
            let cur = self.editor.as_bytes();
            self.draft_len = cur.len();
            self.draft_buf[..self.draft_len].copy_from_slice(cur);
        }
        self.hist_nav += 1;
        let idx = (self.hist_head + self.hist_count - self.hist_nav) % HIST_CAP;
        let len = self.hist_lens[idx] as usize;
        let mut tmp = [0u8; MAX_INPUT_LEN];
        tmp[..len].copy_from_slice(&self.hist_buf[idx][..len]);
        self.editor.set_bytes(&tmp[..len]);
        true
    }

    pub fn history_next(&mut self) -> bool {
        if self.hist_nav == 0 {
            return false;
        }
        self.hist_nav -= 1;
        if self.hist_nav == 0 {
            let len = self.draft_len;
            let mut tmp = [0u8; MAX_INPUT_LEN];
            tmp[..len].copy_from_slice(&self.draft_buf[..len]);
            self.editor.set_bytes(&tmp[..len]);
        } else {
            let idx = (self.hist_head + self.hist_count - self.hist_nav) % HIST_CAP;
            let len = self.hist_lens[idx] as usize;
            let mut tmp = [0u8; MAX_INPUT_LEN];
            tmp[..len].copy_from_slice(&self.hist_buf[idx][..len]);
            self.editor.set_bytes(&tmp[..len]);
        }
        true
    }

    pub fn scroll_history(&mut self, delta_up: isize) -> bool {
        return_scroll(self, delta_up, TERM_ROWS - 1)
    }

    pub fn scroll_history_rows(&mut self, delta_up: isize, win_rows: usize) -> bool {
        return_scroll(self, delta_up, win_rows.saturating_sub(1).max(1))
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
        let slot = self.alloc_scroll_slot();
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

    fn alloc_scroll_slot(&mut self) -> usize {
        self.total_pushed = self.total_pushed.wrapping_add(1);
        if self.count < SCROLLBACK_CAP {
            let idx = (self.head + self.count) % SCROLLBACK_CAP;
            self.count += 1;
            idx
        } else {
            let idx = self.head;
            self.head = (self.head + 1) % SCROLLBACK_CAP;
            idx
        }
    }

    #[inline]
    pub const fn mark_output(&self) -> (usize, usize) {
        (self.total_pushed, self.count)
    }

    pub fn take_lines_since(
        &mut self,
        mark_pushed: usize,
        out: &mut [[u8; TERM_COLS]],
        lens: &mut [u8],
    ) -> usize {
        let pushed = self.total_pushed.wrapping_sub(mark_pushed).min(self.count);
        if pushed == 0 {
            return 0;
        }
        let start_off = self.count - pushed;
        let copy_n = pushed.min(out.len()).min(lens.len());
        for j in 0..copy_n {
            let slot = (self.head + start_off + j) % SCROLLBACK_CAP;
            let row = &self.lines[slot];
            let mut end = TERM_COLS;
            while end > 0 && (row[end - 1].ch == b' ' || row[end - 1].ch == 0) {
                end -= 1;
            }
            for c in 0..end {
                out[j][c] = row[c].ch;
            }
            lens[j] = end as u8;
        }
        self.count -= pushed;
        self.full_dirty = true;
        copy_n
    }

    fn push_raw_row(&mut self, slice: &[u8], style: u8) {
        let slot = self.alloc_scroll_slot();
        let row = &mut self.lines[slot];
        *row = [SurfaceCell::BLANK; TERM_COLS];
        for (i, &ch) in slice.iter().take(TERM_COLS).enumerate() {
            row[i] = SurfaceCell { ch, style };
        }
    }

    pub fn format_help(&mut self) {
        self.push_line(b"DRY Native Terminal Commands (real kernel/service APIs):", STYLE_ACCENT);
        self.push_line(b"  help              Command summary & desktop shortcuts", STYLE_DEFAULT);
        self.push_line(b"  clear | history   Clear viewport | show 16-entry command history", STYLE_DEFAULT);
        self.push_line(b"  echo <text>       Print literal text (| grep/head/wc, >, >> supported)", STYLE_DEFAULT);
        self.push_line(b"  sysinfo | uptime  Live CPU, APIC, RAM, IPC, IRQ & TSC telemetry", STYLE_DEFAULT);
        self.push_line(b"  ps                Live Ring 3 process table (SYS_PROC_LIST)", STYLE_DEFAULT);
        self.push_line(b"  apps              Registered native apps & supervisor state", STYLE_DEFAULT);
        self.push_line(b"  open|close|restart <terminal|lifestation|files|settings|editor|calc>", STYLE_DEFAULT);
        self.push_line(b"  clip set|get      Shared desktop clipboard IPC | Ctrl+C/V/O in Editor", STYLE_DEFAULT);
        self.push_line(b"  tile              Toggle Floating / Tiling window layout", STYLE_DEFAULT);
        self.push_line(
            b"  ls|cd|pwd|mkdir|rmdir|cat|write|cp|mv|rm|ln|sync|exec|linux-run  VFS",
            STYLE_DEFAULT,
        );
        self.push_line(
            b"  ifconfig | ping <ip> | nslookup <host> | httpget <url>  Net",
            STYLE_DEFAULT,
        );
        self.push_line(b"  dmesg|lspci|lsusb|lsblk|nvme|drvctl|i2c-hid-test|uname|free|mounts", STYLE_DEFAULT);
        self.push_line(b"  vault-status|vault-unlock|vault-lock|vault-passwd  DRY Vault", STYLE_DEFAULT);
        self.push_line(b"  gpufault|blank|lock|reboot|poweroff|shutdown|suspend  System", STYLE_DEFAULT);
        self.push_line(b"Keys: Up/Down hist | PgUp/PgDn scroll | Alt+1..6 apps | Alt+Space", STYLE_MUTED);
    }

    pub fn format_history(&mut self) {
        if self.hist_count == 0 {
            self.push_line(b"(command history empty)", STYLE_MUTED);
            return;
        }
        for i in 0..self.hist_count {
            let idx = (self.hist_head + i) % HIST_CAP;
            let len = self.hist_lens[idx] as usize;
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"  ");
            lb.push_u64((i + 1) as u64);
            lb.push_bytes(b"  ");
            let mut tmp = [0u8; MAX_INPUT_LEN];
            tmp[..len].copy_from_slice(&self.hist_buf[idx][..len]);
            lb.push_bytes(&tmp[..len]);
            self.push_line(lb.as_bytes(), STYLE_DEFAULT);
        }
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
        l3.push_bytes(b" ipc=");
        l3.push_u64(info.total_ipc_msgs);
        l3.push_bytes(b" ctx_sw=");
        l3.push_u64(info.total_ctx_switches);
        l3.push_bytes(b" irq1/12=");
        l3.push_u64(info.irq1_delivered);
        l3.push_byte(b'/');
        l3.push_u64(info.irq12_delivered);
        self.push_line(l3.as_bytes(), STYLE_ACCENT);
    }

    pub fn format_power_telemetry(&mut self, pwr: &UserPowerInfo) {
        let mut lp = LineBuilder::new();
        lp.push_bytes(b"Power : HLT=");
        lp.push_u64(pwr.idle_hlt_calls);
        lp.push_bytes(b" calls | ACPI S5=");
        lp.push_bytes(if pwr.s5_supported != 0 { b"ON" } else { b"OFF" });
        lp.push_bytes(b" Reset=");
        lp.push_bytes(if pwr.reset_supported != 0 { b"ON" } else { b"OFF" });
        lp.push_bytes(b" S3=-ENOSYS");
        self.push_line(lp.as_bytes(), STYLE_OK);
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

    pub fn format_ps(&mut self, procs: &[UserProcInfo], total: usize) {
        let mut hdr = LineBuilder::new();
        hdr.push_bytes(b"PID  DOM   COMP    NAME         STATE    MEM(KiB)  SYSCALLS (total=");
        hdr.push_u64(total as u64);
        hdr.push_byte(b')');
        self.push_line(hdr.as_bytes(), STYLE_ACCENT);

        for p in procs {
            let dom_str: &[u8] = if p.domain == 1 { b"Root " } else { b"User " };
            let st_str: &[u8] = match p.state {
                2 => b"Running ",
                3 => b"Exiting ",
                4 => b"Faulting",
                5 => b"Killing ",
                _ => b"Init    ",
            };
            let name = compartment_short_name(p.compartment_id, p.domain);
            let mem_kib = ((p.mapped_pages_4k as u64) + (p.pt_frames_4k as u64)) * 4;

            let mut lb = LineBuilder::new();
            let mut pid_buf = LineBuilder::new();
            pid_buf.push_u64(p.pid as u64);
            lb.push_padded(pid_buf.as_bytes(), 5);
            lb.push_bytes(dom_str);
            lb.push_byte(b' ');
            lb.push_bytes(b"0x");
            let mut hex = LineBuilder::new();
            hex.push_hex_u64(p.compartment_id as u64);
            lb.push_padded(hex.as_bytes(), 6);
            lb.push_padded(name, 13);
            lb.push_bytes(st_str);
            lb.push_byte(b' ');
            let mut mb = LineBuilder::new();
            mb.push_u64(mem_kib);
            lb.push_padded(mb.as_bytes(), 10);
            lb.push_u64(p.syscall_entries);
            self.push_line(lb.as_bytes(), STYLE_DEFAULT);
        }
    }

    pub fn format_apps(&mut self, words: [u64; APP_COUNT]) {
        self.push_line(
            b"APP          COMP    STATE            PID   LAUNCHES",
            STYLE_ACCENT,
        );
        for (app, &w) in AppId::ALL.iter().copied().zip(words.iter()) {
            let pid = (w & 0xffff_ffff) as u32;
            let st = ((w >> 32) & 0xff) as u8;
            let vec = ((w >> 40) & 0xff) as u8;
            let launches = ((w >> 48) & 0xffff) as u16;

            let mut lb = LineBuilder::new();
            lb.push_padded(app.slug(), 13);
            lb.push_bytes(b"0x0");
            lb.push_hex_u64(app.compartment_id() as u64);
            lb.push_bytes(b"  ");

            let mut st_lb = LineBuilder::new();
            let row_style = match st {
                1 => {
                    st_lb.push_bytes(b"RUNNING");
                    STYLE_OK
                }
                2 => {
                    st_lb.push_bytes(b"CRASHED(vec=");
                    st_lb.push_u64(vec as u64);
                    st_lb.push_byte(b')');
                    STYLE_WARN
                }
                3 => {
                    st_lb.push_bytes(b"UNAVAILABLE");
                    STYLE_MUTED
                }
                _ => {
                    st_lb.push_bytes(b"STOPPED");
                    STYLE_DEFAULT
                }
            };
            lb.push_padded(st_lb.as_bytes(), 17);
            let mut pb = LineBuilder::new();
            pb.push_u64(pid as u64);
            lb.push_padded(pb.as_bytes(), 6);
            lb.push_u64(launches as u64);
            self.push_line(lb.as_bytes(), row_style);
        }
    }

    pub fn format_unsupported_posix(&mut self, op_ch: u8) {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"Unsupported POSIX/bash syntax '");
        lb.push_byte(op_ch);
        lb.push_bytes(b"': DRY Terminal executes native commands only.");
        self.push_line(lb.as_bytes(), STYLE_WARN);
    }

    pub fn format_unknown(&mut self, verb: &[u8]) {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"Unknown command: '");
        lb.push_bytes(verb);
        lb.push_bytes(b"' (type 'help')");
        self.push_line(lb.as_bytes(), STYLE_WARN);
    }

    #[inline]
    pub const fn count(&self) -> usize {
        self.count
    }

    #[inline]
    pub const fn scroll_up(&self) -> usize {
        self.scroll_up
    }

    #[inline]
    pub fn history_row(&self, logical_idx: usize) -> &[SurfaceCell; TERM_COLS] {
        &self.lines[(self.head + logical_idx) % SCROLLBACK_CAP]
    }

    /// Project visible scrollback + active prompt line into `surf` (`cols x rows`) using zero-UB
    /// atomic seqlock publication.
    pub fn project_to_surface(
        &mut self,
        surf: &SharedTerminalSurface,
        win_cols: usize,
        win_rows: usize,
        sys: Option<&UserSysInfo>,
    ) {
        use core::sync::atomic::Ordering;
        let cols = win_cols.clamp(1, TERM_COLS);
        let rows = win_rows.clamp(2, TERM_ROWS);
        let hist_viewport = rows - 1;
        let end_idx = self.count.saturating_sub(self.scroll_up);
        let visible_hist = end_idx.min(hist_viewport);
        let start_idx = end_idx - visible_hist;
        let prompt_row = visible_hist;

        let odd = surf.begin_write();
        let blank_row = [SurfaceCell::BLANK; TERM_COLS];

        if self.full_dirty {
            for r in 0..rows {
                if r < visible_hist {
                    let ring_idx = (self.head + start_idx + r) % SCROLLBACK_CAP;
                    surf.store_row_slice(cols, r, &self.lines[ring_idx][..cols]);
                } else {
                    surf.store_row_slice(cols, r, &blank_row[..cols]);
                }
            }
            surf.dirty_row_min.store(0, Ordering::Relaxed);
            surf.dirty_row_max.store((rows - 1) as u16, Ordering::Relaxed);
            self.full_dirty = false;
        } else {
            surf.dirty_row_min.store(prompt_row as u16, Ordering::Relaxed);
            surf.dirty_row_max.store(prompt_row as u16, Ordering::Relaxed);
        }

        let mut prow = [SurfaceCell::BLANK; TERM_COLS];
        let mut col = 0usize;
        for &b in PROMPT_STR {
            if col < cols {
                prow[col] = SurfaceCell {
                    ch: b,
                    style: STYLE_PROMPT,
                };
                col += 1;
            }
        }
        for &b in self.editor.as_bytes() {
            if col < cols {
                prow[col] = SurfaceCell {
                    ch: b,
                    style: STYLE_DEFAULT,
                };
                col += 1;
            }
        }
        if self.scroll_up > 0 && cols >= 20 {
            let mut sb = LineBuilder::new();
            sb.push_bytes(b"[SCROLL +");
            sb.push_u64(self.scroll_up as u64);
            sb.push_byte(b']');
            let sbytes = sb.as_bytes();
            let start_c = cols.saturating_sub(sbytes.len());
            for (k, &ch) in sbytes.iter().enumerate() {
                if start_c + k < cols {
                    prow[start_c + k] = SurfaceCell {
                        ch,
                        style: STYLE_WARN,
                    };
                }
            }
        }
        surf.store_row_slice(cols, prompt_row, &prow[..cols]);

        surf.cursor_col.store(
            (PROMPT_STR.len() + self.editor.cursor()).min(cols - 1) as u16,
            Ordering::Relaxed,
        );
        surf.cursor_row.store(prompt_row as u16, Ordering::Relaxed);
        surf.cmd_count.store(self.cmd_count, Ordering::Relaxed);
        if let Some(s) = sys {
            surf.ext_irqs_snap
                .store(s.total_ext_irqs as u32, Ordering::Relaxed);
            surf.uptime_sec_snap
                .store((s.uptime_ms / 1000) as u32, Ordering::Relaxed);
        }
        surf.end_write(odd);
    }
}

fn return_scroll(eng: &mut ShellEngine, delta_up: isize, max_hist_rows: usize) -> bool {
    let max_scroll = eng.count.saturating_sub(max_hist_rows);
    let next = if delta_up >= 0 {
        (eng.scroll_up + (delta_up as usize)).min(max_scroll)
    } else {
        eng.scroll_up.saturating_sub((-delta_up) as usize)
    };
    if next != eng.scroll_up {
        eng.scroll_up = next;
        eng.full_dirty = true;
        true
    } else {
        false
    }
}

impl Default for ShellEngine {
    fn default() -> Self {
        Self::new()
    }
}
