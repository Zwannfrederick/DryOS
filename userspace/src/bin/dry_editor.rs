// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-editor` (`DRY Notes`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0106`)
//! Native Pixel-Surface Text Editor (`SharedPixelSurface` + `PixelCanvas`, `/persist/notes.txt`).

#![no_std]
#![no_main]

use core::slice::from_raw_parts_mut;

use dry_userspace::gfx::Rect;
use dry_userspace::glass::app_kit::{AppPalette, PixelCanvas};
use dry_userspace::input::{InputEvent, InputEventKind, MOD_CTRL, MOUSE_BTN_LEFT};
use dry_userspace::keymap::{
    KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_LEFT, KEY_RIGHT, KEY_UP,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::sup_services::SupervisorPrefs;
use dry_userspace::surface::{
    init_pixel_surface_rw, publish_pixel_rect_rw, PIXEL_SURF_HEADER_BYTES, SURF_ATTACH_FLAG_PIXEL,
};
use dry_userspace::vfs_proto::{
    vfs_create_file, vfs_list_dir, vfs_read_file, vfs_sync, vfs_write_small, VfsDirEntry,
};
use dry_userspace::wm::{WindowRole, TITLE_KIND_EDITOR};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait,
    sys_sysinfo, sys_vspace_map, sys_write_debug, UserSysInfo, EEXIST, IPC_OP_INPUT_EVENT,
    IPC_OP_PONG, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_GET_VFS_CAP, IPC_OP_SUP_HELLO,
    IPC_OP_SUP_QUERY_PREFS, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_BIND_NOTIF,
    IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT, MSG_FLAG_NON_BLOCKING, RIGHT_MAP,
    RIGHT_READ, RIGHT_SIGNAL, VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_UNMAP_2M,
};

const EDITOR_SURF_VADDR: u64 = 0x1c00_0000;
const WIN_W: u16 = 640;
const WIN_H: u16 = 400;
const INIT_X: u64 = 240;
const INIT_Y: u64 = 96;

const MAX_LINES: usize = 12;
const MAX_LINE_COLS: usize = 64;
const MAX_DOC_BYTES: usize = MAX_LINES * (MAX_LINE_COLS + 1);
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

const SAVE_BTN_RECT: Rect = Rect::new(400, 8, 134, 26);
const CLEAR_BTN_RECT: Rect = Rect::new(542, 8, 84, 26);
const CARD_RECT: Rect = Rect::new(12, 46, 616, 314);
const STATUS_RECT: Rect = Rect::new(12, 366, 616, 26);
const PERSIST_PATH: &[u8] = b"/persist/notes.txt";
const RAM_PATH: &[u8] = b"/ram/notes.txt";

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

struct EditorState {
    vfs_ep: u64,
    persist_ok: bool,
    light_mode: bool,
    lines: [[u8; MAX_LINE_COLS]; MAX_LINES],
    lens: [u8; MAX_LINES],
    line_count: usize,
    cur_row: usize,
    cur_col: usize,
    dirty_doc: bool,
    last_bytes: usize,
    status: [u8; 48],
    status_len: usize,
}

impl EditorState {
    const fn new(vfs_ep: u64, persist_ok: bool, light_mode: bool) -> Self {
        Self {
            vfs_ep,
            persist_ok,
            light_mode,
            lines: [[0; MAX_LINE_COLS]; MAX_LINES],
            lens: [0; MAX_LINES],
            line_count: 1,
            cur_row: 0,
            cur_col: 0,
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
        log_line(&lb);
    }

    fn clear_doc(&mut self) {
        self.lines = [[0; MAX_LINE_COLS]; MAX_LINES];
        self.lens = [0; MAX_LINES];
        self.line_count = 1;
        self.cur_row = 0;
        self.cur_col = 0;
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

    fn handle_event(&mut self, ev: InputEvent) -> (bool, bool) {
        if ev.kind == InputEventKind::MouseButtonDown && (ev.scancode as u8) == MOUSE_BTN_LEFT {
            let mx = ev.dx.max(0) as usize;
            let my = ev.dy.max(0) as usize;
            if SAVE_BTN_RECT.contains(mx, my) {
                self.save_notes();
                return (true, false);
            }
            if CLEAR_BTN_RECT.contains(mx, my) {
                self.clear_doc();
                return (true, false);
            }
            if CARD_RECT.contains(mx, my) {
                let row = my.saturating_sub(58) / 24;
                if row < self.line_count {
                    self.cur_row = row;
                    let col = mx.saturating_sub(60) / 8;
                    self.cur_col = col.min(self.lens[row] as usize);
                    return (true, false);
                }
            }
            return (false, false);
        }
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
        (true, false)
    }

    fn render(&self, pix: &mut [u32]) {
        let pal = AppPalette::for_mode(self.light_mode);
        let mut c = PixelCanvas::new(pix, WIN_W as usize, WIN_H as usize);
        c.clear(pal.bg);
        c.fill_rect(Rect::new(0, 0, WIN_W as usize, 40), pal.surface);
        c.fill_rect(Rect::new(0, 40, WIN_W as usize, 1), pal.border);

        c.fill_rounded_card(Rect::new(12, 8, 232, 26), 5, pal.surface_alt, pal.border);
        c.text(
            20,
            13,
            self.active_path(),
            if self.persist_ok { pal.ok } else { pal.warn },
            None,
        );
        c.button(
            SAVE_BTN_RECT,
            b"Save (Ctrl+S)",
            pal.btn_accent,
            pal.fg_on_accent,
            pal.accent,
        );
        c.button(CLEAR_BTN_RECT, b"Clear", pal.btn_bg, pal.fg, pal.border);

        c.fill_rounded_card(CARD_RECT, 6, pal.surface, pal.border);
        c.fill_rect(Rect::new(52, CARD_RECT.y + 6, 1, CARD_RECT.h - 12), pal.border);

        for r in 0..MAX_LINES {
            let ry = 56 + r * 24;
            if r == self.cur_row {
                c.fill_rect(Rect::new(56, ry.saturating_sub(2), 564, 22), pal.surface_alt);
            }
            let mut nb = LineBuilder::new();
            let num = (r + 1) as u64;
            if num < 10 {
                nb.push_byte(b' ');
            }
            nb.push_u64(num);
            c.text(
                22,
                ry,
                nb.as_bytes(),
                if r == self.cur_row { pal.accent } else { pal.fg_muted },
                None,
            );
            if r < self.line_count {
                let len = self.lens[r] as usize;
                c.text(62, ry, &self.lines[r][..len], pal.fg, None);
            }
            if r == self.cur_row {
                let cx = 62 + self.cur_col * 8;
                c.fill_rect(Rect::new(cx, ry, 2, 16), pal.accent);
            }
        }

        c.fill_rounded_card(STATUS_RECT, 5, pal.surface, pal.border);
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"Ln ");
        lb.push_u64((self.cur_row + 1) as u64);
        lb.push_bytes(b", Col ");
        lb.push_u64((self.cur_col + 1) as u64);
        lb.push_bytes(if self.dirty_doc { b"  [MODIFIED]" } else { b"  [SAVED]" });
        c.text(
            22,
            371,
            lb.as_bytes(),
            if self.dirty_doc { pal.warn } else { pal.ok },
            None,
        );
        c.text(218, 371, &self.status[..self.status_len], pal.fg_muted, None);

        let mut bb = LineBuilder::new();
        bb.push_u64(self.total_bytes() as u64);
        bb.push_bytes(b" B");
        let bbytes = bb.as_bytes();
        c.text(620usize.saturating_sub(bbytes.len() * 8), 371, bbytes, pal.fg_muted, None);
    }
}

fn query_light_mode(sup_ep: u64) -> bool {
    if let Ok((rtag, _, rmrs)) = sys_ipc_call_inline(
        sup_ep,
        pack_msg_tag(IPC_OP_SUP_QUERY_PREFS, 0, 1, 0),
        [0; 4],
        0,
    ) {
        if msg_tag_label(rtag) == IPC_OP_PONG {
            let p = SupervisorPrefs::unpack_mr0(rmrs[0]);
            return (p.theme_flags & 4) != 0 && (p.theme_flags & 1) == 0;
        }
    }
    false
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

    let surf_cap = sys_frame_alloc(1).unwrap_or_else(|_| sys_exit(243));
    if sys_vspace_map(surf_cap, EDITOR_SURF_VADDR, VSPACE_MAP_OP_2M_RW).is_err() {
        sys_exit(244);
    }
    // SAFETY: `EDITOR_SURF_VADDR` is a freshly allocated, mapped `2 MiB` `USER_RW` frame.
    unsafe { init_pixel_surface_rw(EDITOR_SURF_VADDR, WIN_W, WIN_H) };
    // SAFETY: Pixel plane begins immediately after the 64-byte header and covers `WIN_W * WIN_H` `u32`s.
    let pix = unsafe {
        from_raw_parts_mut(
            (EDITOR_SURF_VADDR + PIXEL_SURF_HEADER_BYTES as u64) as *mut u32,
            (WIN_W as usize) * (WIN_H as usize),
        )
    };

    let mut ed = EditorState::new(vfs_ep, persist_ok, query_light_mode(sup_ep_cap));
    ed.load_initial();
    ed.render(pix);
    // SAFETY: `EDITOR_SURF_VADDR` is initialized above.
    unsafe { publish_pixel_rect_rw(EDITOR_SURF_VADDR, 0, 0, WIN_W, WIN_H) };

    let attach_mrs = [
        (WIN_W as u64) | ((WIN_H as u64) << 16),
        INIT_X | (INIT_Y << 32),
        (WindowRole::Normal as u64) | ((TITLE_KIND_EDITOR as u64) << 8) | SURF_ATTACH_FLAG_PIXEL,
        0,
    ];
    let mut win_id = 0u32;
    if let Ok((rtag, _, rmrs)) = sys_ipc_call_with_grant(
        disp_ep_cap,
        IPC_OP_SURFACE_ATTACH,
        attach_mrs,
        surf_cap,
        RIGHT_READ | RIGHT_MAP,
        0,
        0,
    ) {
        if msg_tag_label(rtag) == IPC_OP_PONG {
            win_id = rmrs[0] as u32;
        }
    }
    if win_id != 0 {
        let _ = sys_ipc_call_with_grant(
            disp_ep_cap,
            IPC_OP_WIN_BIND_NOTIF,
            [win_id as u64, 0, 0, 0],
            notif_cap,
            RIGHT_READ | RIGHT_SIGNAL,
            0,
            0,
        );
    }

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-EDITOR] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    rlb.push_bytes(b" vfs=");
    rlb.push_u64(u64::from(vfs_ep != 0));
    log_line(&rlb);

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let poll_tsc = 200_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    loop {
        let mut dirty = false;
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
                    let (d, q) = ed.handle_event(ev);
                    dirty |= d;
                    if q {
                        exit_now = true;
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let next_light = query_light_mode(sup_ep_cap);
        if next_light != ed.light_mode {
            ed.light_mode = next_light;
            dirty = true;
        }
        if exit_now {
            break;
        }
        if dirty && win_id != 0 {
            ed.render(pix);
            // SAFETY: `EDITOR_SURF_VADDR` remains mapped RW until exit cleanup below.
            unsafe { publish_pixel_rect_rw(EDITOR_SURF_VADDR, 0, 0, WIN_W, WIN_H) };
            let _ = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            );
        }
        if let Ok(bits) = sys_notify_wait(notif_cap, 0, poll_tsc) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                break;
            }
        }
    }

    if win_id != 0 {
        let _ = sys_ipc_call_inline(
            disp_ep_cap,
            pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
            [win_id as u64, 0, 0, 0],
            0,
        );
    }
    let _ = sys_vspace_map(surf_cap, EDITOR_SURF_VADDR, VSPACE_MAP_OP_UNMAP_2M);
    let _ = sys_cap_revoke(surf_cap, 2);
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
