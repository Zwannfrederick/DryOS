// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-editor` (`DRY Notes`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0106`)
//! Native DRY Glass 2.0 Text Editor (`SharedPixelSurface` + `ui/` toolkit, `/persist/notes.txt`).

#![no_std]
#![no_main]

use dry_userspace::apps::editor_ui::{render_editor, EditorAction, EditorView};
use dry_userspace::apps::{app_log_line, app_poll_tsc, PixelAppWindow, PointerStep};
use dry_userspace::gfx::Rect;
use dry_userspace::input::{InputEvent, InputEventKind, MOD_CTRL};
use dry_userspace::keymap::{
    KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_LEFT, KEY_RIGHT, KEY_UP,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::ttf::{font_metrics, FontId};
use dry_userspace::vfs_proto::{
    vfs_create_file, vfs_list_dir, vfs_read_file, vfs_sync, vfs_write_small, VfsDirEntry,
};
use dry_userspace::wm_types::{WindowRole, TITLE_KIND_EDITOR};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait, EEXIST, IPC_OP_INPUT_EVENT,
    IPC_OP_PONG, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_GET_VFS_CAP, IPC_OP_SUP_HELLO,
    IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT, MSG_FLAG_NON_BLOCKING, RIGHT_READ, RIGHT_SIGNAL,
};

const EDITOR_SURF_VADDR: u64 = 0x1c00_0000;
const EDITOR_SCRATCH_SURF_VADDR: u64 = 0x1e00_0000;
const ALLOW_MULTI_FLAG: u64 = 1 << 17;
const WIN_W: u16 = 640;
const WIN_H: u16 = 400;
const INIT_X: u64 = 240;
const INIT_Y: u64 = 96;

const MAX_LINES: usize = 32;
const MAX_LINE_COLS: usize = 80;
const MAX_DOC_BYTES: usize = MAX_LINES * (MAX_LINE_COLS + 1);
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

const PERSIST_PATH: &[u8] = b"/persist/notes.txt";
const RAM_PATH: &[u8] = b"/ram/notes.txt";

struct EditorState {
    vfs_ep: u64,
    persist_ok: bool,
    lines: [[u8; MAX_LINE_COLS]; MAX_LINES],
    lens: [u8; MAX_LINES],
    line_count: usize,
    cur_row: usize,
    cur_col: usize,
    scroll_line: usize,
    dirty_doc: bool,
    last_bytes: usize,
    status: [u8; 48],
    status_len: usize,
}

impl EditorState {
    const fn new(vfs_ep: u64, persist_ok: bool) -> Self {
        Self {
            vfs_ep,
            persist_ok,
            lines: [[0; MAX_LINE_COLS]; MAX_LINES],
            lens: [0; MAX_LINES],
            line_count: 1,
            cur_row: 0,
            cur_col: 0,
            scroll_line: 0,
            dirty_doc: false,
            last_bytes: 0,
            status: [0; 48],
            status_len: 0,
        }
    }

    fn set_status(&mut self, msg: &[u8]) {
        let n = msg.len().min(self.status.len());
        self.status[..n].copy_from_slice(&msg[..n]);
        self.status_len = n;
    }

    fn active_path(&self) -> &'static [u8] {
        if self.persist_ok { PERSIST_PATH } else { RAM_PATH }
    }

    fn load_bytes(&mut self, raw: &[u8]) {
        self.lines = [[0; MAX_LINE_COLS]; MAX_LINES];
        self.lens = [0; MAX_LINES];
        self.line_count = 1;
        let mut r = 0usize;
        for &b in raw {
            if b == b'\n' {
                if r + 1 < MAX_LINES {
                    r += 1;
                    self.line_count = r + 1;
                } else {
                    break;
                }
            } else if (0x20..=0x7e).contains(&b) {
                let c = self.lens[r] as usize;
                if c < MAX_LINE_COLS {
                    self.lines[r][c] = b;
                    self.lens[r] = (c + 1) as u8;
                }
            }
        }
        self.cur_row = r;
        self.cur_col = self.lens[r] as usize;
        self.scroll_line = 0;
        self.last_bytes = raw.len();
        self.dirty_doc = false;
    }

    fn load_initial(&mut self) {
        if self.vfs_ep != 0 {
            let mut raw = [0u8; MAX_DOC_BYTES];
            if let Ok((n, _)) = vfs_read_file(self.vfs_ep, self.active_path(), 0, &mut raw) {
                self.load_bytes(&raw[..n]);
                self.set_status(b"Loaded existing notes");
                return;
            }
        }
        self.load_bytes(
            b"DRY Glass Notes\nPersistent scratchpad (/persist/notes.txt)\nPress Ctrl+S or click [Save] to flush.\n",
        );
        self.set_status(b"Ready (new notes buffer)");
    }

    fn total_bytes(&self) -> usize {
        let mut total = 0usize;
        for r in 0..self.line_count {
            total += self.lens[r] as usize;
            if r + 1 < self.line_count {
                total += 1;
            }
        }
        total
    }

    fn save_notes(&mut self) {
        if self.vfs_ep == 0 {
            self.set_status(b"VFS unavailable (-ENOSYS)");
            return;
        }
        let mut out = [0u8; MAX_DOC_BYTES];
        let mut n = 0usize;
        for r in 0..self.line_count {
            let len = self.lens[r] as usize;
            out[n..(n + len)].copy_from_slice(&self.lines[r][..len]);
            n += len;
            if r + 1 < self.line_count && n < out.len() {
                out[n] = b'\n';
                n += 1;
            }
        }
        let path = self.active_path();
        match vfs_create_file(self.vfs_ep, path, 0) {
            Ok(_) | Err(EEXIST) => {}
            Err(_) => {
                self.set_status(b"Save failed on create");
                return;
            }
        }
        if n == 0 {
            if vfs_write_small(self.vfs_ep, path, &[], false).is_err() {
                self.set_status(b"Save failed on write");
                return;
            }
        } else {
            let mut off = 0usize;
            let mut idx = 0usize;
            while off < n {
                let take = (n - off).min(180);
                if vfs_write_small(self.vfs_ep, path, &out[off..(off + take)], idx > 0).is_err() {
                    self.set_status(b"Save failed on chunk");
                    return;
                }
                off += take;
                idx += 1;
            }
        }
        if self.persist_ok {
            let _ = vfs_sync(self.vfs_ep);
        }
        self.dirty_doc = false;
        self.last_bytes = n;
        self.set_status(if self.persist_ok {
            b"Saved & synced to /persist/notes.txt"
        } else {
            b"Saved to volatile /ram/notes.txt"
        });

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-EDITOR] SAVED path=");
        lb.push_bytes(path);
        lb.push_bytes(b" bytes=");
        lb.push_u64(n as u64);
        app_log_line(&lb);
    }

    fn clear_doc(&mut self) {
        self.lines = [[0; MAX_LINE_COLS]; MAX_LINES];
        self.lens = [0; MAX_LINES];
        self.line_count = 1;
        self.cur_row = 0;
        self.cur_col = 0;
        self.scroll_line = 0;
        self.dirty_doc = true;
        self.set_status(b"Cleared notes buffer (unsaved)");
    }

    fn insert_char(&mut self, ch: u8) {
        let len = self.lens[self.cur_row] as usize;
        if len >= MAX_LINE_COLS {
            return;
        }
        let col = self.cur_col.min(len);
        let mut i = len;
        while i > col {
            self.lines[self.cur_row][i] = self.lines[self.cur_row][i - 1];
            i -= 1;
        }
        self.lines[self.cur_row][col] = ch;
        self.lens[self.cur_row] = (len + 1) as u8;
        self.cur_col = col + 1;
        self.dirty_doc = true;
    }

    fn insert_newline(&mut self) {
        if self.line_count >= MAX_LINES {
            return;
        }
        let r = self.cur_row;
        let len = self.lens[r] as usize;
        let col = self.cur_col.min(len);
        let mut i = self.line_count;
        while i > r + 1 {
            self.lines[i] = self.lines[i - 1];
            self.lens[i] = self.lens[i - 1];
            i -= 1;
        }
        let tail = len - col;
        let (head, tail_rows) = self.lines.split_at_mut(r + 1);
        tail_rows[0] = [0; MAX_LINE_COLS];
        tail_rows[0][..tail].copy_from_slice(&head[r][col..len]);
        self.lens[r + 1] = tail as u8;
        self.lens[r] = col as u8;
        self.line_count += 1;
        self.cur_row += 1;
        self.cur_col = 0;
        self.dirty_doc = true;
    }

    fn backspace(&mut self) {
        if self.cur_col > 0 {
            let r = self.cur_row;
            let len = self.lens[r] as usize;
            let del = self.cur_col - 1;
            for i in del..(len - 1) {
                self.lines[r][i] = self.lines[r][i + 1];
            }
            self.lens[r] = (len - 1) as u8;
            self.cur_col = del;
            self.dirty_doc = true;
        } else if self.cur_row > 0 {
            let prev = self.cur_row - 1;
            let prev_len = self.lens[prev] as usize;
            let cur_len = self.lens[self.cur_row] as usize;
            let move_n = cur_len.min(MAX_LINE_COLS - prev_len);
            let (head, tail_rows) = self.lines.split_at_mut(self.cur_row);
            head[prev][prev_len..(prev_len + move_n)]
                .copy_from_slice(&tail_rows[0][..move_n]);
            self.lens[prev] = (prev_len + move_n) as u8;
            for i in self.cur_row..(self.line_count - 1) {
                self.lines[i] = self.lines[i + 1];
                self.lens[i] = self.lens[i + 1];
            }
            self.line_count -= 1;
            self.cur_row = prev;
            self.cur_col = prev_len;
            self.dirty_doc = true;
        }
    }

    fn ensure_cursor_visible(&mut self, win: &PixelAppWindow) {
        let t = win.theme();
        let top_h = (t.ctrl_h + 14).clamp(42, 54);
        let body_h = (win.h as usize).saturating_sub(top_h + 30 + t.sp_sm * 2);
        let line_h = (font_metrics(FontId::MonoRegular, t.mono_px).line_height + 2).max(16) as usize;
        let vis = (body_h.saturating_sub(8) / line_h).max(1);
        if self.cur_row < self.scroll_line {
            self.scroll_line = self.cur_row;
        } else if self.cur_row >= self.scroll_line + vis {
            self.scroll_line = self.cur_row + 1 - vis;
        }
    }

    fn handle_key(&mut self, ev: InputEvent, win: &PixelAppWindow) -> (bool, bool) {
        if ev.kind != InputEventKind::KeyPress {
            return (false, false);
        }
        let scan7 = (ev.scancode & 0x7f) as u8;
        if (ev.modifiers & MOD_CTRL) != 0 {
            if ev.ascii == 0x13 || ev.ascii == b's' || ev.ascii == b'S' || scan7 == 0x1f {
                self.save_notes();
                return (true, false);
            }
            if ev.ascii == 0x11 || ev.ascii == b'q' || ev.ascii == b'Q' || scan7 == 0x10 {
                return (false, true);
            }
        }
        if ev.keycode == KEY_ESC {
            return (false, true);
        }
        match ev.keycode {
            KEY_ENTER => self.insert_newline(),
            KEY_BACKSPACE => self.backspace(),
            KEY_UP => {
                self.cur_row = self.cur_row.saturating_sub(1);
                self.cur_col = self.cur_col.min(self.lens[self.cur_row] as usize);
            }
            KEY_DOWN => {
                if self.cur_row + 1 < self.line_count {
                    self.cur_row += 1;
                    self.cur_col = self.cur_col.min(self.lens[self.cur_row] as usize);
                }
            }
            KEY_LEFT => {
                if self.cur_col > 0 {
                    self.cur_col -= 1;
                } else if self.cur_row > 0 {
                    self.cur_row -= 1;
                    self.cur_col = self.lens[self.cur_row] as usize;
                }
            }
            KEY_RIGHT => {
                if self.cur_col < self.lens[self.cur_row] as usize {
                    self.cur_col += 1;
                } else if self.cur_row + 1 < self.line_count {
                    self.cur_row += 1;
                    self.cur_col = 0;
                }
            }
            _ => {
                if (0x20..=0x7e).contains(&ev.ascii) {
                    self.insert_char(ev.ascii);
                } else {
                    return (false, false);
                }
            }
        }
        self.ensure_cursor_visible(win);
        (true, false)
    }

    fn render_frame(&mut self, win: &mut PixelAppWindow, scratch_open: bool, step: PointerStep) -> EditorAction {
        let mut line_slices: [&[u8]; MAX_LINES] = [&[]; MAX_LINES];
        for (r, slot) in line_slices.iter_mut().enumerate().take(self.line_count) {
            let len = (self.lens[r] as usize).min(MAX_LINE_COLS);
            *slot = &self.lines[r][..len];
        }
        let path = self.active_path();
        let total = self.total_bytes();
        let mut view = EditorView {
            path,
            persist_ok: self.persist_ok,
            scratch_open,
            line_slices: &line_slices[..self.line_count],
            cur_row: self.cur_row,
            cur_col: self.cur_col,
            scroll_line: &mut self.scroll_line,
            dirty_doc: self.dirty_doc,
            total_bytes: total,
            status: &self.status[..self.status_len],
        };
        render_editor(win, &mut view, step)
    }

    fn apply_action(&mut self, act: EditorAction) -> (bool, bool) {
        match act {
            EditorAction::None => (false, false),
            EditorAction::Save => {
                self.save_notes();
                (true, false)
            }
            EditorAction::Clear => {
                self.clear_doc();
                (true, false)
            }
            EditorAction::ToggleScratch => (true, true),
            EditorAction::MoveCursor(r, col) => {
                self.cur_row = r.min(self.line_count.saturating_sub(1));
                self.cur_col = col.min(self.lens[self.cur_row] as usize);
                (true, false)
            }
        }
    }
}

fn render_scratch_window(swin: &mut PixelAppWindow, buf: &[u8]) {
    let t = swin.theme();
    let mut c = swin.canvas();
    c.clear(t.bg_app);
    c.fill_rect(Rect::new(0, 0, swin.w as usize, 34), t.bg_sidebar);
    c.draw_text(14, 9, FontId::SansSemiBold, t.body_px, b"Quick Scratchpad (#2)", t.fg_primary);
    c.fill_bordered_rrect_aa(
        Rect::new(12, 44, (swin.w as usize).saturating_sub(24), (swin.h as usize).saturating_sub(58)),
        t.r_md,
        t.bg_card,
        t.border_subtle,
    );
    let line = if buf.is_empty() { b"Type quick notes here (separate window)." } else { buf };
    c.draw_text(22, 58, FontId::MonoRegular, t.mono_px, line, t.fg_primary);
    swin.publish_initial();
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(sup_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, _) = read_cs_ss();
    if (cs & 3) != 3 || sup_ep_cap == 0 {
        sys_exit(240);
    }
    let notif_cap = sys_notify_create(0).unwrap_or_else(|_| sys_exit(241));
    let disp_ep_cap = match sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0x0106, 0, 0, 0],
        notif_cap,
        RIGHT_READ | RIGHT_SIGNAL,
        0,
        0,
    ) {
        Ok((t, cap, _)) if msg_tag_label(t) == IPC_OP_PONG && cap != 0 => cap,
        _ => sys_exit(242),
    };

    let mut vfs_ep = 0u64;
    if let Ok((vtag, vcap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_GET_VFS_CAP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(vtag) == IPC_OP_PONG {
            vfs_ep = vcap;
        }
    }
    let mut tmp = [VfsDirEntry::EMPTY; 4];
    let persist_ok = vfs_ep != 0 && vfs_list_dir(vfs_ep, b"/persist", &mut tmp).is_ok();

    let init_flags = PixelAppWindow::query_theme_flags(sup_ep_cap);
    let mut win = PixelAppWindow::alloc_and_map(EDITOR_SURF_VADDR, WIN_W, WIN_H, init_flags)
        .unwrap_or_else(|_| sys_exit(243));
    let mut ed = EditorState::new(vfs_ep, persist_ok);
    ed.load_initial();
    let _ = ed.render_frame(&mut win, false, PointerStep::default());
    win.publish_initial();
    let win_id = win.attach(
        disp_ep_cap,
        notif_cap,
        INIT_X,
        INIT_Y,
        WindowRole::Normal,
        TITLE_KIND_EDITOR,
        0,
    );
    let mut scratch_win: Option<PixelAppWindow> = None;
    let mut scratch_buf = [0u8; 48];
    let mut scratch_len = 0usize;

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-EDITOR] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    rlb.push_bytes(b" vfs=");
    rlb.push_u64(u64::from(vfs_ep != 0));
    app_log_line(&rlb);

    let poll_tsc = app_poll_tsc(200_000);

    loop {
        let mut dirty = false;
        let mut scratch_dirty = false;
        let mut toggle_scratch = false;
        let mut exit_now = false;
        if win_id != 0 {
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    exit_now = true;
                    break;
                }
                if plbl == IPC_OP_INPUT_EVENT {
                    let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                    if win.handle_window_config(ev) {
                        ed.ensure_cursor_visible(&win);
                        dirty = true;
                    } else if ev.kind == InputEventKind::KeyPress {
                        if (ev.modifiers & MOD_CTRL) != 0
                            && (ev.ascii == 0x0e
                                || ev.ascii == b'n'
                                || ev.ascii == b'N'
                                || (ev.scancode & 0x7f) == 0x31)
                        {
                            toggle_scratch = true;
                        } else {
                            let (d, q) = ed.handle_key(ev, &win);
                            dirty |= d;
                            if q {
                                exit_now = true;
                                break;
                            }
                        }
                    } else {
                        let step = win.ingest_pointer(ev);
                        if step.clicked || step.scroll_dy != 0 {
                            let act = ed.render_frame(&mut win, scratch_win.is_some(), step);
                            let (d, tog) = ed.apply_action(act);
                            dirty |= d;
                            toggle_scratch |= tog;
                        } else if step.redraw {
                            dirty = true;
                        }
                    }
                } else {
                    break;
                }
            }
        }
        if let Some(ref mut swin) = scratch_win {
            let mut close_scratch = false;
            while swin.win_id != 0 {
                let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                    disp_ep_cap,
                    pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                    [swin.win_id as u64, 0, 0, 0],
                    0,
                ) else {
                    break;
                };
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    close_scratch = true;
                    break;
                }
                if plbl == IPC_OP_INPUT_EVENT {
                    let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                    if swin.handle_window_config(ev) {
                        scratch_dirty = true;
                    } else if ev.kind == InputEventKind::KeyPress {
                        if ev.keycode == KEY_ESC {
                            close_scratch = true;
                            break;
                        } else if ev.keycode == KEY_BACKSPACE && scratch_len > 0 {
                            scratch_len -= 1;
                            scratch_dirty = true;
                        } else if (0x20..=0x7e).contains(&ev.ascii) && scratch_len < scratch_buf.len() {
                            scratch_buf[scratch_len] = ev.ascii;
                            scratch_len += 1;
                            scratch_dirty = true;
                        }
                    } else {
                        let _ = swin.ingest_pointer(ev);
                    }
                } else {
                    break;
                }
            }
            if close_scratch {
                toggle_scratch = true;
            }
        }
        if toggle_scratch {
            if let Some(swin) = scratch_win.take() {
                swin.teardown(disp_ep_cap);
            } else if let Ok(mut swin) = PixelAppWindow::alloc_and_map(
                EDITOR_SCRATCH_SURF_VADDR,
                400,
                220,
                win.theme_flags,
            ) {
                render_scratch_window(&mut swin, &scratch_buf[..scratch_len]);
                let _ = swin.attach(
                    disp_ep_cap,
                    notif_cap,
                    INIT_X + 70,
                    INIT_Y + 48,
                    WindowRole::Normal,
                    TITLE_KIND_EDITOR,
                    ALLOW_MULTI_FLAG,
                );
                scratch_win = Some(swin);
            }
            dirty = true;
        }
        if win.sync_prefs(sup_ep_cap) {
            dirty = true;
        }
        if exit_now {
            break;
        }
        if let Some(ref mut swin) = scratch_win {
            if swin.sync_prefs(sup_ep_cap) || scratch_dirty {
                render_scratch_window(swin, &scratch_buf[..scratch_len]);
                let _ = swin.present_full(disp_ep_cap);
            }
        }
        if dirty && win_id != 0 {
            let _ = ed.render_frame(&mut win, scratch_win.is_some(), PointerStep::default());
            let _ = win.present_full(disp_ep_cap);
        }
        if let Ok(bits) = sys_notify_wait(notif_cap, 0, poll_tsc) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                break;
            }
        }
    }

    if let Some(swin) = scratch_win.take() {
        swin.teardown(disp_ep_cap);
    }
    win.teardown(disp_ep_cap);
    if vfs_ep != 0 {
        let _ = sys_cap_revoke(vfs_ep, 1);
    }
    let _ = sys_cap_revoke(disp_ep_cap, 1);
    let _ = sys_cap_revoke(notif_cap, 2);
    let _ = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_APP_EXITED, MSG_FLAG_NON_BLOCKING, 1, 0),
        [0x0106, 0, 0, 0],
        0,
    );
    sys_exit(0);
}
