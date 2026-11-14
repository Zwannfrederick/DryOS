// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-settings` (`Settings`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0105`)
//! Live Runtime System Preferences & `DialogTransient` Helper (`Milestone 2A-3`).
//!
//! Categories (100% live runtime-applied via `IPC_OP_SUP_ACTION`):
//! 1. Window Mode (`Floating`, `Side-by-Side Tiling`, `Stacked`, `Tabbed`)
//! 2. Input Preferences (`US QWERTY` / `Turkish Q`, Pointer Speed `1x` / `2x`)
//! 3. Display Preferences (`Standard Dark` / `High Contrast`, Cursor Size `1x` / `2x`)
//! 4. Power & Idle Policy (`Off` / `5s` / `30s` / `60s` DPMS blank timeout)
//! 5. Session Preferences (`Lock Session` + honest notice: volatile RAM only, no disk persistence)
//!
//! Plus `[H]` toggles an auto-floating `WindowRole::DialogTransient` Quick-Help dialog window.

#![no_std]
#![no_main]

use dry_userspace::action::DesktopAction;
use dry_userspace::gfx::BOX_HORIZ;
use dry_userspace::input::{InputEvent, InputEventKind};
use dry_userspace::keymap::{KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_LEFT, KEY_RIGHT, KEY_UP};
use dry_userspace::shell::LineBuilder;
use dry_userspace::sup_services::SupervisorPrefs;
use dry_userspace::surface::{
    SharedWindowSurface, SurfaceCell, STYLE_ACCENT, STYLE_DEFAULT, STYLE_HEADER, STYLE_MUTED,
    STYLE_OK, STYLE_WARN,
};
use dry_userspace::wm::{WindowRole, TITLE_KIND_DIALOG, TITLE_KIND_SETTINGS};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait,
    sys_vspace_map, sys_write_debug, IPC_OP_INPUT_EVENT, IPC_OP_PONG, IPC_OP_SUP_ACTION,
    IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO, IPC_OP_SUP_QUERY_PREFS, IPC_OP_SURFACE_ATTACH,
    IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_BIND_NOTIF, IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED,
    IPC_OP_WIN_POLL_EVENT, MSG_FLAG_NON_BLOCKING, RIGHT_MAP, RIGHT_READ, RIGHT_SIGNAL,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const SET_SURF_VADDR: u64 = 0x1a00_0000;
const DLG_SURF_VADDR: u64 = 0x1a00_1000;
const SET_COLS: usize = 62;
const SET_ROWS: usize = 22;
const DLG_COLS: usize = 42;
const DLG_ROWS: usize = 9;

const INIT_WIN_X: u64 = 590;
const INIT_WIN_Y: u64 = 84;
const INIT_DLG_X: u64 = 430;
const INIT_DLG_Y: u64 = 240;

const ALLOW_MULTI_FLAG: u64 = 1 << 17;
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;
const ROW_COUNT: usize = 9;

const BLANK_PRESETS: [u16; 4] = [0, 5, 30, 60];

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn write_row_cols(surf: &SharedWindowSurface, cols: usize, row: usize, text: &[u8], style: u8) {
    let mut cells = [SurfaceCell::BLANK; SET_COLS];
    for (i, &ch) in text.iter().take(cols).enumerate() {
        cells[i] = SurfaceCell { ch, style };
    }
    surf.store_row_slice(cols, row, &cells[..cols]);
}

fn render_dialog_surface(surf: &SharedWindowSurface) {
    let odd = surf.begin_write();
    let div = [BOX_HORIZ; DLG_COLS];
    write_row_cols(
        surf,
        DLG_COLS,
        0,
        b"QUICK HELP // TRANSIENT DIALOG ROLE",
        STYLE_HEADER,
    );
    write_row_cols(surf, DLG_COLS, 1, &div, STYLE_MUTED);
    write_row_cols(
        surf,
        DLG_COLS,
        2,
        b" WindowRole::DialogTransient stays",
        STYLE_OK,
    );
    write_row_cols(
        surf,
        DLG_COLS,
        3,
        b" auto-floating above Tiling, Stacked,",
        STYLE_OK,
    );
    write_row_cols(
        surf,
        DLG_COLS,
        4,
        b" and Tabbed workspace layouts.",
        STYLE_OK,
    );
    write_row_cols(surf, DLG_COLS, 5, b"", STYLE_DEFAULT);
    write_row_cols(
        surf,
        DLG_COLS,
        6,
        b" Shortcuts: Alt+1..6 Apps  Alt+Space Search",
        STYLE_ACCENT,
    );
    write_row_cols(surf, DLG_COLS, 7, &div, STYLE_MUTED);
    write_row_cols(
        surf,
        DLG_COLS,
        8,
        b" Press [H] or [Esc] to close dialog",
        STYLE_WARN,
    );
    surf.end_write(odd);
}

struct SettingsUi {
    sup_ep: u64,
    disp_ep: u64,
    notif_cap: u64,
    dlg_surf_cap: u64,
    dlg_win_id: u32,
    prefs: SupervisorPrefs,
    sel_row: usize,
    status: [u8; SET_COLS],
    status_len: usize,
}

impl SettingsUi {
    fn set_status(&mut self, msg: &[u8]) {
        let n = msg.len().min(SET_COLS);
        self.status[..n].copy_from_slice(&msg[..n]);
        self.status_len = n;
    }

    fn query_live_prefs(&mut self) {
        if let Ok((rtag, _, rmrs)) = sys_ipc_call_inline(
            self.sup_ep,
            pack_msg_tag(IPC_OP_SUP_QUERY_PREFS, 0, 1, 0),
            [0; 4],
            0,
        ) {
            if msg_tag_label(rtag) == IPC_OP_PONG {
                self.prefs = SupervisorPrefs::unpack_mr0(rmrs[0]);
            }
        }
    }

    fn send_action(&mut self, act: DesktopAction) -> bool {
        let [w0, w1] = act.to_words();
        if let Ok((rtag, _, _)) = sys_ipc_call_inline(
            self.sup_ep,
            pack_msg_tag(IPC_OP_SUP_ACTION, 0, 2, 0),
            [w0, w1, 0, 0],
            0,
        ) {
            if msg_tag_label(rtag) == IPC_OP_PONG {
                self.query_live_prefs();
                return true;
            }
        }
        false
    }

    fn toggle_dialog(&mut self, dlg_surf: &SharedWindowSurface) {
        if self.disp_ep == 0 {
            return;
        }
        if self.dlg_win_id != 0 {
            let old = self.dlg_win_id;
            let _ = sys_ipc_call_inline(
                self.disp_ep,
                pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
                [old as u64, 0, 0, 0],
                0,
            );
            self.dlg_win_id = 0;
            self.set_status(b"Closed DialogTransient helper window");
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-SETTINGS] DIALOG open=0 win_id=");
            lb.push_u64(old as u64);
            log_line(&lb);
            return;
        }

        render_dialog_surface(dlg_surf);
        let attach_mrs = [
            (DLG_COLS as u64) | ((DLG_ROWS as u64) << 16),
            INIT_DLG_X | (INIT_DLG_Y << 32),
            (WindowRole::DialogTransient as u64)
                | ((TITLE_KIND_DIALOG as u64) << 8)
                | ALLOW_MULTI_FLAG,
            0,
        ];
        if let Ok((rtag, _, rmrs)) = sys_ipc_call_with_grant(
            self.disp_ep,
            IPC_OP_SURFACE_ATTACH,
            attach_mrs,
            self.dlg_surf_cap,
            RIGHT_READ | RIGHT_MAP,
            0,
            0,
        ) {
            if msg_tag_label(rtag) == IPC_OP_PONG {
                self.dlg_win_id = rmrs[0] as u32;
                let _ = sys_ipc_call_with_grant(
                    self.disp_ep,
                    IPC_OP_WIN_BIND_NOTIF,
                    [self.dlg_win_id as u64, 0, 0, 0],
                    self.notif_cap,
                    RIGHT_READ | RIGHT_SIGNAL,
                    0,
                    0,
                );
                self.set_status(b"Opened auto-floating DialogTransient helper window");
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-SETTINGS] DIALOG open=1 win_id=");
                lb.push_u64(self.dlg_win_id as u64);
                log_line(&lb);
            }
        }
    }

    fn adjust_row(&mut self, row: usize, step: isize) {
        match row {
            0 => {
                let cur = self.prefs.layout_mode as isize;
                let next = (cur + step).rem_euclid(4) as u8;
                if self.send_action(DesktopAction::SetLayoutMode(next)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY layout=");
                    lb.push_u64(self.prefs.layout_mode as u64);
                    log_line(&lb);
                    self.set_status(b"Applied Window Manager layout mode");
                }
            }
            1 => {
                let next = if self.prefs.keymap == 0 { 1 } else { 0 };
                if self.send_action(DesktopAction::SetKeymap(next)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY keymap=");
                    lb.push_u64(self.prefs.keymap as u64);
                    log_line(&lb);
                    self.set_status(if next == 1 {
                        b"Applied Turkish Q keyboard layout live"
                    } else {
                        b"Applied US QWERTY keyboard layout live"
                    });
                }
            }
            2 => {
                let next = if self.prefs.pointer_speed <= 1 { 2 } else { 1 };
                if self.send_action(DesktopAction::SetPointerSpeed(next)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY ptr_speed=");
                    lb.push_u64(self.prefs.pointer_speed as u64);
                    log_line(&lb);
                    self.set_status(b"Applied PS/2 pointer speed multiplier");
                }
            }
            3 => {
                let next = self.prefs.theme_flags ^ 1;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY contrast=");
                    lb.push_u64((self.prefs.theme_flags & 1) as u64);
                    log_line(&lb);
                    self.set_status(b"Applied compositor display contrast theme");
                }
            }
            4 => {
                let next = self.prefs.theme_flags ^ 2;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY cursor_large=");
                    lb.push_u64(((self.prefs.theme_flags >> 1) & 1) as u64);
                    log_line(&lb);
                    self.set_status(b"Applied software cursor size preference");
                }
            }
            5 => {
                let cur_i = BLANK_PRESETS
                    .iter()
                    .position(|&s| s == self.prefs.blank_timeout_sec)
                    .unwrap_or(0) as isize;
                let next_s = BLANK_PRESETS[(cur_i + step).rem_euclid(4) as usize];
                if self.send_action(DesktopAction::SetBlankTimeoutSec(next_s)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY blank_s=");
                    lb.push_u64(self.prefs.blank_timeout_sec as u64);
                    log_line(&lb);
                    self.set_status(b"Updated Display Idle DPMS blank policy");
                }
            }
            6 => {
                let _ = self.send_action(DesktopAction::LockSession);
                let _ = sys_write_debug(b"[DRY-SETTINGS] APPLY lock_session=1\n");
                self.set_status(b"Requested Session Lock shield");
            }
            7 => {
                let next = self.prefs.theme_flags ^ 4;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY light_mode=");
                    lb.push_u64(((self.prefs.theme_flags >> 2) & 1) as u64);
                    log_line(&lb);
                    self.set_status(b"Applied DRY Glass Obsidian Dark / Frosted Daylight theme");
                }
            }
            8 => {
                let next = self.prefs.theme_flags ^ 8;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-SETTINGS] APPLY low_overhead_glass=");
                    lb.push_u64(((self.prefs.theme_flags >> 3) & 1) as u64);
                    log_line(&lb);
                    self.set_status(b"Applied DRY Glass frosted blur / low-overhead material");
                }
            }
            _ => {}
        }
    }

    fn handle_event(&mut self, ev: InputEvent, dlg_surf: &SharedWindowSurface) -> bool {
        if ev.kind == InputEventKind::MouseScroll {
            if ev.wheel > 0 {
                self.sel_row = self.sel_row.saturating_sub(1);
            } else if ev.wheel < 0 && self.sel_row + 1 < ROW_COUNT {
                self.sel_row += 1;
            }
            return false;
        }
        if ev.kind != InputEventKind::KeyPress {
            return false;
        }
        if ev.keycode == KEY_ESC && self.dlg_win_id != 0 {
            self.toggle_dialog(dlg_surf);
            return false;
        }
        if ev.ascii == b'q' || ev.ascii == b'Q' {
            return true;
        }
        if ev.ascii == b'h' || ev.ascii == b'H' {
            self.toggle_dialog(dlg_surf);
            return false;
        }
        if ev.ascii == b'l' || ev.ascii == b'L' {
            self.adjust_row(6, 1);
            return false;
        }
        if ev.ascii == b'c' || ev.ascii == b'C' {
            self.adjust_row(4, 1);
            return false;
        }
        if ev.ascii == b't' || ev.ascii == b'T' {
            self.sel_row = 7;
            self.adjust_row(7, 1);
            return false;
        }
        if ev.ascii == b'b' || ev.ascii == b'B' {
            self.sel_row = 8;
            self.adjust_row(8, 1);
            return false;
        }
        if ev.ascii == b'g' || ev.ascii == b'G' {
            if self.send_action(DesktopAction::TriggerGpuFaultTest) {
                let _ = sys_write_debug(b"[DRY-SETTINGS] APPLY gpu_fault_test=1\n");
                self.set_status(b"Triggered live VirtIO-GPU fault & recovery test");
            }
            return false;
        }
        if (b'1'..=b'9').contains(&ev.ascii) {
            self.sel_row = (ev.ascii - b'1') as usize;
            self.adjust_row(self.sel_row, 1);
            return false;
        }
        match ev.keycode {
            KEY_UP => self.sel_row = self.sel_row.saturating_sub(1),
            KEY_DOWN => {
                if self.sel_row + 1 < ROW_COUNT {
                    self.sel_row += 1;
                }
            }
            KEY_LEFT => self.adjust_row(self.sel_row, -1),
            KEY_RIGHT | KEY_ENTER => self.adjust_row(self.sel_row, 1),
            _ => {}
        }
        false
    }

    fn render(&self, surf: &SharedWindowSurface) {
        let odd = surf.begin_write();
        let div = [BOX_HORIZ; SET_COLS];

        write_row_cols(
            surf,
            SET_COLS,
            0,
            b"DRY SETTINGS // LIVE SYSTEM PREFERENCES [CPL=3 0x0105]",
            STYLE_HEADER,
        );
        write_row_cols(surf, SET_COLS, 1, &div, STYLE_MUTED);
        if self.prefs.persist_backed {
            write_row_cols(
                surf,
                SET_COLS,
                2,
                b" STORAGE: /persist/desktop.cfg [PERSISTENT CoW + VirtIO FLUSH]",
                STYLE_OK,
            );
        } else {
            write_row_cols(
                surf,
                SET_COLS,
                2,
                b" NOTICE: Volatile RAM session -- resets on reboot (no disk)",
                STYLE_WARN,
            );
        }
        write_row_cols(surf, SET_COLS, 3, &div, STYLE_MUTED);

        let layout_str: &[u8] = match self.prefs.layout_mode {
            1 => b"< Side-by-Side Tiling >",
            2 => b"< Stacked Cascade >",
            3 => b"< Tabbed Workspace >",
            _ => b"< Floating (Default) >",
        };
        let km_str: &[u8] = if self.prefs.keymap == 1 {
            b"< Turkish Q >"
        } else {
            b"< US QWERTY >"
        };
        let spd_str: &[u8] = if self.prefs.pointer_speed >= 2 {
            b"< 2x Fast >"
        } else {
            b"< 1x Standard >"
        };
        let theme_str: &[u8] = if (self.prefs.theme_flags & 1) != 0 {
            b"< High Contrast >"
        } else {
            b"< Standard Dark >"
        };
        let cur_str: &[u8] = if (self.prefs.theme_flags & 2) != 0 {
            b"< 2x Large (24x36) >"
        } else {
            b"< 1x Normal (12x18) >"
        };
        let blank_str: &[u8] = match self.prefs.blank_timeout_sec {
            5 => b"< 5 Seconds >",
            30 => b"< 30 Seconds >",
            60 => b"< 60 Seconds >",
            _ => b"< Off (Never Blank) >",
        };

        let shield_str: &[u8] = if self.prefs.persist_backed {
            b"[Enter/L] Lock + DRY Vault Auth"
        } else {
            b"[Enter/L] Lock Session (No Auth)"
        };
        let glass_theme_str: &[u8] = if (self.prefs.theme_flags & 4) != 0 {
            b"< Frosted Daylight >"
        } else {
            b"< Obsidian Dark >"
        };
        let glass_blur_str: &[u8] = if (self.prefs.theme_flags & 8) != 0 {
            b"< Low-Overhead >"
        } else {
            b"< Frosted Blur >"
        };

        let rows_spec: [(&[u8], &[u8]); ROW_COUNT] = [
            (b"[1] Window Layout   : ", layout_str),
            (b"[2] Keyboard Layout : ", km_str),
            (b"[3] Pointer Speed   : ", spd_str),
            (b"[4] Display Theme   : ", theme_str),
            (b"[5] Cursor Size [C] : ", cur_str),
            (b"[6] Idle DPMS Blank : ", blank_str),
            (b"[7] Session Shield  : ", shield_str),
            (b"[8] Glass Theme [T] : ", glass_theme_str),
            (b"[9] Glass Blur  [B] : ", glass_blur_str),
        ];

        for (i, (label, val)) in rows_spec.iter().enumerate() {
            let r = 5 + i;
            let selected = i == self.sel_row;
            let mut lb = LineBuilder::new();
            lb.push_bytes(if selected { b" > " } else { b"   " });
            lb.push_bytes(label);
            lb.push_bytes(val);
            write_row_cols(
                surf,
                SET_COLS,
                r,
                lb.as_bytes(),
                if selected { STYLE_OK } else { STYLE_DEFAULT },
            );
        }
        for r in (5 + ROW_COUNT)..18 {
            write_row_cols(surf, SET_COLS, r, b"", STYLE_DEFAULT);
        }

        write_row_cols(surf, SET_COLS, 18, &div, STYLE_MUTED);
        let dlg_hint: &[u8] = if self.dlg_win_id != 0 {
            b" [Up/Dn] Row  [L/R/Enter] Change  [H] Close Help Dialog  [Q]"
        } else {
            b" [Up/Dn] Row  [L/R/Enter] Change  [H] Open Help Dialog   [Q]"
        };
        write_row_cols(surf, SET_COLS, 19, dlg_hint, STYLE_ACCENT);
        write_row_cols(
            surf,
            SET_COLS,
            20,
            &self.status[..self.status_len],
            STYLE_OK,
        );
        write_row_cols(
            surf,
            SET_COLS,
            21,
            b" DRY Glass: [T] Theme  [B] Blur  [G] GPU Recovery  [H] Help",
            STYLE_MUTED,
        );
        surf.end_write(odd);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(sup_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, _) = read_cs_ss();
    if (cs & 3) != 3 || sup_ep_cap == 0 {
        sys_exit(221);
    }

    let notif_cap = match sys_notify_create(0) {
        Ok(c) => c,
        Err(_) => sys_exit(222),
    };

    let mut disp_ep_cap: u64 = 0;
    if let Ok((tag, cap, _)) = sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0; 4],
        notif_cap,
        RIGHT_READ | RIGHT_SIGNAL,
        0,
        0,
    ) {
        if msg_tag_label(tag) == IPC_OP_PONG && cap != 0 {
            disp_ep_cap = cap;
        }
    }

    let surf_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(223),
    };
    let dlg_surf_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(223),
    };
    if sys_vspace_map(surf_cap, SET_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err()
        || sys_vspace_map(dlg_surf_cap, DLG_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err()
    {
        sys_exit(224);
    }

    // SAFETY: Both `SET_SURF_VADDR` and `DLG_SURF_VADDR` are exclusively mapped `USER_RW`.
    let surf = unsafe {
        SharedWindowSurface::init_at(SET_SURF_VADDR, SET_COLS as u16, SET_ROWS as u16, false)
    };
    let dlg_surf = unsafe {
        SharedWindowSurface::init_at(DLG_SURF_VADDR, DLG_COLS as u16, DLG_ROWS as u16, false)
    };

    let mut ui = SettingsUi {
        sup_ep: sup_ep_cap,
        disp_ep: disp_ep_cap,
        notif_cap,
        dlg_surf_cap,
        dlg_win_id: 0,
        prefs: SupervisorPrefs::new(),
        sel_row: 0,
        status: [0; SET_COLS],
        status_len: 0,
    };
    ui.query_live_prefs();
    ui.set_status(b"Ready: changes take effect immediately across Ring 3 services");
    ui.render(surf);

    let mut main_win_id: u32 = 0;
    if disp_ep_cap != 0 {
        let attach_mrs = [
            (SET_COLS as u64) | ((SET_ROWS as u64) << 16),
            INIT_WIN_X | (INIT_WIN_Y << 32),
            (WindowRole::Normal as u64) | ((TITLE_KIND_SETTINGS as u64) << 8) | ALLOW_MULTI_FLAG,
            0,
        ];
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
                main_win_id = rmrs[0] as u32;
            }
        }
        if main_win_id != 0 {
            let _ = sys_ipc_call_with_grant(
                disp_ep_cap,
                IPC_OP_WIN_BIND_NOTIF,
                [main_win_id as u64, 0, 0, 0],
                notif_cap,
                RIGHT_READ | RIGHT_SIGNAL,
                0,
                0,
            );
        }
    }

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-SETTINGS] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(main_win_id as u64);
    log_line(&rlb);

    let mut should_exit = false;
    loop {
        let mut dirty = false;
        if disp_ep_cap != 0 && main_win_id != 0 {
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [main_win_id as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    should_exit = true;
                    break;
                }
                if plbl == IPC_OP_INPUT_EVENT {
                    let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                    if ui.handle_event(ev, dlg_surf) {
                        should_exit = true;
                        break;
                    }
                    dirty = true;
                } else {
                    break;
                }
            }
        }

        if disp_ep_cap != 0 && ui.dlg_win_id != 0 && !should_exit {
            let dwid = ui.dlg_win_id;
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [dwid as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    ui.toggle_dialog(dlg_surf);
                    dirty = true;
                    break;
                }
                if plbl == IPC_OP_INPUT_EVENT {
                    let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                    if ui.handle_event(ev, dlg_surf) {
                        should_exit = true;
                        break;
                    }
                    dirty = true;
                    if ui.dlg_win_id == 0 {
                        break;
                    }
                } else {
                    break;
                }
            }
        }

        if dirty && !should_exit {
            ui.query_live_prefs();
            ui.render(surf);
            let _ = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 2, 0),
                [main_win_id as u64, ((SET_ROWS as u64) << 16), 0, 0],
                0,
            );
        }

        if should_exit {
            break;
        }
        if let Ok(bits) = sys_notify_wait(notif_cap, 0, 0) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                break;
            }
        }
    }

    if disp_ep_cap != 0 {
        if ui.dlg_win_id != 0 {
            let _ = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
                [ui.dlg_win_id as u64, 0, 0, 0],
                0,
            );
        }
        if main_win_id != 0 {
            let _ = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
                [main_win_id as u64, 0, 0, 0],
                0,
            );
        }
    }
    let _ = sys_vspace_map(dlg_surf_cap, DLG_SURF_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let _ = sys_cap_revoke(dlg_surf_cap, 2);
    let _ = sys_vspace_map(surf_cap, SET_SURF_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let _ = sys_cap_revoke(surf_cap, 2);
    if disp_ep_cap != 0 {
        let _ = sys_cap_revoke(disp_ep_cap, 1);
    }
    let _ = sys_cap_revoke(notif_cap, 2);
    let _ = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_APP_EXITED, MSG_FLAG_NON_BLOCKING, 1, 0),
        [0; 4],
        0,
    );
    sys_exit(0);
}
