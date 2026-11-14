// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-settings` — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0105`)
//! Native DRY Glass 2.0 Settings (`PixelAppWindow` + `ui/` toolkit + lazy `DialogTransient`).

#![no_std]
#![no_main]

use dry_userspace::action::DesktopAction;
use dry_userspace::apps::settings_ui::{
    first_row_for_category, render_help_dialog, render_settings, settings_vis_rows, SettingsAction,
    SettingsView, DLG_PIXEL_VADDR, DLG_WIN_H, DLG_WIN_W, ROW_COUNT, SET_PIXEL_VADDR, SET_WIN_H,
    SET_WIN_W,
};
use dry_userspace::apps::{app_log_line, app_poll_tsc, PixelAppWindow, PointerStep};
use dry_userspace::input::{InputEvent, InputEventKind};
use dry_userspace::keymap::{KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_LEFT, KEY_RIGHT, KEY_UP};
use dry_userspace::shell::LineBuilder;
use dry_userspace::sup_services::SupervisorPrefs;
use dry_userspace::wm_types::{WindowRole, TITLE_KIND_DIALOG, TITLE_KIND_SETTINGS};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait, IPC_OP_INPUT_EVENT, IPC_OP_PONG,
    IPC_OP_SUP_ACTION, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO, IPC_OP_SUP_QUERY_PREFS,
    IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT, MSG_FLAG_NON_BLOCKING, RIGHT_READ, RIGHT_SIGNAL,
};

const INIT_X: u64 = 590;
const INIT_Y: u64 = 84;
const DLG_X: u64 = 430;
const DLG_Y: u64 = 240;
const ALLOW_MULTI_FLAG: u64 = 1 << 17;
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;
const BLANK_PRESETS: [u16; 4] = [0, 5, 30, 60];

struct SettingsApp {
    sup_ep: u64,
    disp_ep: u64,
    notif_cap: u64,
    main_win: PixelAppWindow,
    dlg_win: Option<PixelAppWindow>,
    prefs: SupervisorPrefs,
    sel_row: usize,
    list_scroll: usize,
    status: [u8; 64],
    status_len: usize,
}

impl SettingsApp {
    fn set_status(&mut self, msg: &[u8]) {
        let n = msg.len().min(self.status.len());
        self.status[..n].copy_from_slice(&msg[..n]);
        self.status_len = n;
    }

    fn ensure_visible(&mut self) {
        let vis = settings_vis_rows(
            self.main_win.theme(),
            self.main_win.w as usize,
            self.main_win.h as usize,
        );
        if self.sel_row < self.list_scroll {
            self.list_scroll = self.sel_row;
        } else if self.sel_row >= self.list_scroll + vis {
            self.list_scroll = self.sel_row + 1 - vis;
        }
        let max_scroll = ROW_COUNT.saturating_sub(vis);
        self.list_scroll = self.list_scroll.min(max_scroll);
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

    fn sync_window_themes(&mut self) {
        let tf = self.prefs.theme_flags & 0x3f;
        self.main_win.theme_flags = tf;
        if let Some(ref mut dlg) = self.dlg_win {
            dlg.theme_flags = tf;
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
            msg_tag_label(rtag) == IPC_OP_PONG
        } else {
            false
        }
    }

    fn log_apply_u64(key: &[u8], val: u64) {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-SETTINGS] APPLY ");
        lb.push_bytes(key);
        lb.push_byte(b'=');
        lb.push_u64(val);
        app_log_line(&lb);
    }

    fn apply_scale_bits(&mut self, bits: u8) -> bool {
        let next_bits = bits.min(2);
        let next_flags = (self.prefs.theme_flags & 0x0f) | (next_bits << 4);
        if self.send_action(DesktopAction::SetThemeContrast(next_flags)) {
            self.prefs.theme_flags = next_flags;
            self.sync_window_themes();
            let pct: u64 = match next_bits {
                1 => 125,
                2 => 150,
                _ => 100,
            };
            Self::log_apply_u64(b"dpi_pct", pct);
            match next_bits {
                1 => self.set_status(b"Display Scale -> 125% HiDPI"),
                2 => self.set_status(b"Display Scale -> 150% HiDPI"),
                _ => self.set_status(b"Display Scale -> 100% Standard"),
            }
            true
        } else {
            self.set_status(b"Display Scale rejected by supervisor");
            false
        }
    }

    fn adjust_row(&mut self, row: usize, step: i32) -> bool {
        match row {
            0 => {
                let next = ((self.prefs.layout_mode as i32 + step).rem_euclid(4)) as u8;
                if self.send_action(DesktopAction::SetLayoutMode(next)) {
                    self.prefs.layout_mode = next;
                    Self::log_apply_u64(b"layout", next as u64);
                    self.set_status(b"Updated workspace window layout");
                    return true;
                }
            }
            1 => {
                let next = if self.prefs.keymap == 1 { 0 } else { 1 };
                if self.send_action(DesktopAction::SetKeymap(next)) {
                    self.prefs.keymap = next;
                    Self::log_apply_u64(b"keymap", next as u64);
                    self.set_status(if next == 1 { b"Keymap -> Turkish-Q" } else { b"Keymap -> US QWERTY" });
                    return true;
                }
            }
            2 => {
                let next = if self.prefs.pointer_speed >= 2 { 1 } else { 2 };
                if self.send_action(DesktopAction::SetPointerSpeed(next)) {
                    self.prefs.pointer_speed = next;
                    Self::log_apply_u64(b"ptr_speed", next as u64);
                    self.set_status(if next >= 2 { b"Pointer Speed -> Fast 2x" } else { b"Pointer Speed -> Normal 1x" });
                    return true;
                }
            }
            3 => {
                let next = self.prefs.theme_flags ^ 1;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    self.prefs.theme_flags = next;
                    self.sync_window_themes();
                    let v = (next & 1) as u64;
                    Self::log_apply_u64(b"contrast", v);
                    self.set_status(if v != 0 { b"High Contrast enabled" } else { b"Standard Contrast restored" });
                    return true;
                }
            }
            4 => {
                let next = self.prefs.theme_flags ^ 2;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    self.prefs.theme_flags = next;
                    self.sync_window_themes();
                    let v = ((next >> 1) & 1) as u64;
                    Self::log_apply_u64(b"cursor_large", v);
                    self.set_status(if v != 0 { b"Large Cursor 2x enabled" } else { b"Cursor 1x restored" });
                    return true;
                }
            }
            5 => {
                let cur_idx = BLANK_PRESETS
                    .iter()
                    .position(|&v| v == self.prefs.blank_timeout_sec)
                    .unwrap_or(0) as i32;
                let next_idx = (cur_idx + step).rem_euclid(BLANK_PRESETS.len() as i32) as usize;
                let sec = BLANK_PRESETS[next_idx];
                if self.send_action(DesktopAction::SetBlankTimeoutSec(sec)) {
                    self.prefs.blank_timeout_sec = sec;
                    Self::log_apply_u64(b"blank_s", sec as u64);
                    self.set_status(b"Updated DPMS blank timeout");
                    return true;
                }
            }
            6 => {
                if self.send_action(DesktopAction::LockSession) {
                    Self::log_apply_u64(b"lock_session", 1);
                    self.set_status(b"Session locked via DRY Vault");
                    return true;
                }
            }
            7 => {
                let next = self.prefs.theme_flags ^ 4;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    self.prefs.theme_flags = next;
                    self.sync_window_themes();
                    let v = ((next >> 2) & 1) as u64;
                    Self::log_apply_u64(b"light_mode", v);
                    self.set_status(if v != 0 { b"Theme -> Frosted Daylight" } else { b"Theme -> Obsidian Night" });
                    return true;
                }
            }
            8 => {
                let next = self.prefs.theme_flags ^ 8;
                if self.send_action(DesktopAction::SetThemeContrast(next)) {
                    self.prefs.theme_flags = next;
                    self.sync_window_themes();
                    let v = ((next >> 3) & 1) as u64;
                    Self::log_apply_u64(b"low_overhead_glass", v);
                    self.set_status(if v != 0 { b"Glass Blur -> Low-Overhead" } else { b"Glass Blur -> Full Frosted" });
                    return true;
                }
            }
            _ => {
                let cur = ((self.prefs.theme_flags >> 4) & 3).min(2) as i32;
                let next = (cur + step).rem_euclid(3) as u8;
                return self.apply_scale_bits(next);
            }
        }
        self.set_status(b"Supervisor rejected action (-EACCES)");
        false
    }

    fn trigger_gpu_test(&mut self) {
        if self.send_action(DesktopAction::TriggerGpuFaultTest) {
            Self::log_apply_u64(b"gpu_fault_test", 1);
            self.set_status(b"Triggered VirtIO-GPU live #UD recovery test");
        } else {
            self.set_status(b"GPU recovery test unavailable");
        }
    }

    fn open_help_dialog(&mut self) {
        if self.dlg_win.is_some() {
            return;
        }
        let tf = self.prefs.theme_flags & 0x3f;
        let Ok(mut dlg) = PixelAppWindow::alloc_and_map(DLG_PIXEL_VADDR, DLG_WIN_W, DLG_WIN_H, tf)
        else {
            self.set_status(b"Dialog surface alloc failed");
            return;
        };
        let _ = render_help_dialog(&mut dlg, PointerStep::default());
        dlg.publish_initial();
        let dlg_wid = dlg.attach(
            self.disp_ep,
            self.notif_cap,
            DLG_X,
            DLG_Y,
            WindowRole::DialogTransient,
            TITLE_KIND_DIALOG,
            ALLOW_MULTI_FLAG,
        );
        if dlg_wid == 0 {
            dlg.teardown(self.disp_ep);
            self.set_status(b"Dialog attach failed");
            return;
        }
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-SETTINGS] DIALOG open=1 win_id=");
        lb.push_u64(dlg_wid as u64);
        app_log_line(&lb);
        self.dlg_win = Some(dlg);
        self.set_status(b"Help dialog opened");
    }

    fn close_help_dialog(&mut self) {
        if let Some(dlg) = self.dlg_win.take() {
            let wid = dlg.win_id;
            dlg.teardown(self.disp_ep);
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-SETTINGS] DIALOG open=0 win_id=");
            lb.push_u64(wid as u64);
            app_log_line(&lb);
            self.set_status(b"Help dialog closed");
        }
    }

    fn toggle_help_dialog(&mut self) {
        if self.dlg_win.is_some() {
            self.close_help_dialog();
        } else {
            self.open_help_dialog();
        }
    }

    fn apply_ui_action(&mut self, act: SettingsAction) -> (bool, bool) {
        let mut dirty_main = false;
        let mut dirty_dlg = false;
        match act {
            SettingsAction::None => {}
            SettingsAction::SelectCategory(cat) => {
                self.sel_row = first_row_for_category(cat).min(ROW_COUNT - 1);
                self.ensure_visible();
                dirty_main = true;
            }
            SettingsAction::SelectRow(row) => {
                self.sel_row = row.min(ROW_COUNT - 1);
                self.ensure_visible();
                dirty_main = true;
            }
            SettingsAction::AdjustRow(row, step) => {
                self.sel_row = row.min(ROW_COUNT - 1);
                self.ensure_visible();
                let _ = self.adjust_row(self.sel_row, step);
                dirty_main = true;
                dirty_dlg = self.dlg_win.is_some();
            }
            SettingsAction::SetKeymap(km) => {
                self.sel_row = 1;
                self.ensure_visible();
                if self.send_action(DesktopAction::SetKeymap(km)) {
                    self.prefs.keymap = km;
                    Self::log_apply_u64(b"keymap", km as u64);
                    self.set_status(if km == 1 { b"Keymap -> Turkish-Q" } else { b"Keymap -> US QWERTY" });
                }
                dirty_main = true;
            }
            SettingsAction::SetPointerSpeed(spd) => {
                self.sel_row = 2;
                self.ensure_visible();
                if self.send_action(DesktopAction::SetPointerSpeed(spd)) {
                    self.prefs.pointer_speed = spd;
                    Self::log_apply_u64(b"ptr_speed", spd as u64);
                    self.set_status(if spd >= 2 { b"Pointer Speed -> Fast 2x" } else { b"Pointer Speed -> Normal 1x" });
                }
                dirty_main = true;
            }
            SettingsAction::SetScaleBits(bits) => {
                self.sel_row = 9;
                self.ensure_visible();
                let _ = self.apply_scale_bits(bits);
                dirty_main = true;
                dirty_dlg = self.dlg_win.is_some();
            }
            SettingsAction::ToggleHelp => {
                self.toggle_help_dialog();
                dirty_main = true;
            }
            SettingsAction::TriggerGpuTest => {
                self.trigger_gpu_test();
                dirty_main = true;
            }
        }
        (dirty_main, dirty_dlg)
    }

    fn handle_key(&mut self, ev: InputEvent) -> (bool, bool, bool) {
        if ev.kind != InputEventKind::KeyPress {
            return (false, false, false);
        }
        if ev.keycode == KEY_ESC && self.dlg_win.is_some() {
            self.close_help_dialog();
            return (true, false, false);
        }
        match ev.ascii {
            b'q' | b'Q' => return (false, false, true),
            b'h' | b'H' => {
                self.toggle_help_dialog();
                return (true, false, false);
            }
            b'l' | b'L' => {
                let _ = self.adjust_row(6, 1);
                return (true, false, false);
            }
            b'c' | b'C' => {
                let _ = self.adjust_row(4, 1);
                return (true, self.dlg_win.is_some(), false);
            }
            b't' | b'T' => {
                self.sel_row = 7;
                self.ensure_visible();
                let _ = self.adjust_row(7, 1);
                return (true, self.dlg_win.is_some(), false);
            }
            b'b' | b'B' => {
                self.sel_row = 8;
                self.ensure_visible();
                let _ = self.adjust_row(8, 1);
                return (true, self.dlg_win.is_some(), false);
            }
            b'g' | b'G' => {
                self.trigger_gpu_test();
                return (true, false, false);
            }
            b'1'..=b'9' => {
                self.sel_row = (ev.ascii - b'1') as usize;
                self.ensure_visible();
                let _ = self.adjust_row(self.sel_row, 1);
                return (true, self.dlg_win.is_some(), false);
            }
            b'0' | b's' | b'S' => {
                self.sel_row = 9;
                self.ensure_visible();
                let _ = self.adjust_row(9, 1);
                return (true, self.dlg_win.is_some(), false);
            }
            _ => {}
        }
        match ev.keycode {
            KEY_UP => {
                self.sel_row = if self.sel_row == 0 { ROW_COUNT - 1 } else { self.sel_row - 1 };
                self.ensure_visible();
                (true, false, false)
            }
            KEY_DOWN => {
                self.sel_row = (self.sel_row + 1) % ROW_COUNT;
                self.ensure_visible();
                (true, false, false)
            }
            KEY_LEFT => {
                let _ = self.adjust_row(self.sel_row, -1);
                (true, self.dlg_win.is_some(), false)
            }
            KEY_RIGHT | KEY_ENTER => {
                let _ = self.adjust_row(self.sel_row, 1);
                (true, self.dlg_win.is_some(), false)
            }
            _ => (false, false, false),
        }
    }

    fn redraw_main(&mut self) {
        if self.main_win.win_id != 0 {
            let v = SettingsView {
                prefs: self.prefs,
                sel_row: self.sel_row,
                list_scroll: self.list_scroll,
                dlg_open: self.dlg_win.is_some(),
                status_msg: &self.status[..self.status_len],
            };
            let _ = render_settings(&mut self.main_win, &v, PointerStep::default());
            let _ = self.main_win.present_full(self.disp_ep);
        }
    }

    fn redraw_dlg(&mut self) {
        if let Some(ref mut dlg) = self.dlg_win {
            let _ = render_help_dialog(dlg, PointerStep::default());
            let _ = dlg.present_full(self.disp_ep);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(sup_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, _) = read_cs_ss();
    if (cs & 3) != 3 || sup_ep_cap == 0 {
        sys_exit(191);
    }
    let notif_cap = sys_notify_create(0).unwrap_or_else(|_| sys_exit(192));
    let disp_ep = match sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0; 4],
        notif_cap,
        RIGHT_READ | RIGHT_SIGNAL,
        0,
        0,
    ) {
        Ok((tag, cap, _)) if msg_tag_label(tag) == IPC_OP_PONG && cap != 0 => cap,
        _ => sys_exit(193),
    };

    let init_flags = PixelAppWindow::query_theme_flags(sup_ep_cap);
    let main_win = PixelAppWindow::alloc_and_map(SET_PIXEL_VADDR, SET_WIN_W, SET_WIN_H, init_flags)
        .unwrap_or_else(|_| sys_exit(194));

    let mut app = SettingsApp {
        sup_ep: sup_ep_cap,
        disp_ep,
        notif_cap,
        main_win,
        dlg_win: None,
        prefs: SupervisorPrefs::new(),
        sel_row: 0,
        list_scroll: 0,
        status: [0; 64],
        status_len: 0,
    };
    app.query_live_prefs();
    app.sync_window_themes();
    app.set_status(b"Live DRY Glass 2.0 Settings");

    let init_v = SettingsView {
        prefs: app.prefs,
        sel_row: app.sel_row,
        list_scroll: app.list_scroll,
        dlg_open: false,
        status_msg: &app.status[..app.status_len],
    };
    let _ = render_settings(&mut app.main_win, &init_v, PointerStep::default());
    app.main_win.publish_initial();
    let main_wid = app.main_win.attach(
        disp_ep,
        notif_cap,
        INIT_X,
        INIT_Y,
        WindowRole::Normal,
        TITLE_KIND_SETTINGS,
        ALLOW_MULTI_FLAG,
    );

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-SETTINGS] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(main_wid as u64);
    app_log_line(&rlb);

    let poll_tsc = app_poll_tsc(200_000);

    loop {
        let mut dirty_main = false;
        let mut dirty_dlg = false;
        let mut exit_now = false;

        if app.main_win.win_id != 0 {
            let wid = app.main_win.win_id;
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                app.disp_ep,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [wid as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    exit_now = true;
                    break;
                }
                if plbl != IPC_OP_INPUT_EVENT {
                    break;
                }
                let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                if app.main_win.handle_window_config(ev) {
                    app.ensure_visible();
                    dirty_main = true;
                } else if ev.kind == InputEventKind::KeyPress {
                    let (dm, dd, q) = app.handle_key(ev);
                    dirty_main |= dm;
                    dirty_dlg |= dd;
                    if q {
                        exit_now = true;
                        break;
                    }
                } else if ev.kind == InputEventKind::MouseScroll {
                    let _ = app.main_win.ingest_pointer(ev);
                    if ev.wheel > 0 {
                        app.sel_row = app.sel_row.saturating_sub(1);
                    } else if ev.wheel < 0 {
                        app.sel_row = (app.sel_row + 1).min(ROW_COUNT - 1);
                    }
                    app.ensure_visible();
                    dirty_main = true;
                } else {
                    let step = app.main_win.ingest_pointer(ev);
                    if step.clicked {
                        let v = SettingsView {
                            prefs: app.prefs,
                            sel_row: app.sel_row,
                            list_scroll: app.list_scroll,
                            dlg_open: app.dlg_win.is_some(),
                            status_msg: &app.status[..app.status_len],
                        };
                        let act = render_settings(&mut app.main_win, &v, step);
                        let (dm, dd) = app.apply_ui_action(act);
                        dirty_main |= dm || step.redraw;
                        dirty_dlg |= dd;
                    } else if step.redraw {
                        dirty_main = true;
                    }
                }
            }
        }

        if let Some(dlg_wid) = app.dlg_win.as_ref().map(|d| d.win_id) {
            let mut close_dlg = false;
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                app.disp_ep,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [dlg_wid as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    close_dlg = true;
                    break;
                }
                if plbl != IPC_OP_INPUT_EVENT {
                    break;
                }
                let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                if let Some(ref mut dlg) = app.dlg_win {
                    if dlg.handle_window_config(ev) {
                        dirty_dlg = true;
                        continue;
                    }
                }
                if ev.kind == InputEventKind::KeyPress {
                    if ev.keycode == KEY_ESC || ev.ascii == b'h' || ev.ascii == b'H' {
                        close_dlg = true;
                        break;
                    }
                    let (dm, dd, q) = app.handle_key(ev);
                    dirty_main |= dm;
                    dirty_dlg |= dd;
                    if q {
                        exit_now = true;
                        break;
                    }
                } else {
                    let step = if let Some(ref mut dlg) = app.dlg_win {
                        dlg.ingest_pointer(ev)
                    } else {
                        PointerStep::default()
                    };
                    if step.clicked {
                        if let Some(ref mut dlg) = app.dlg_win {
                            if render_help_dialog(dlg, step) {
                                close_dlg = true;
                                break;
                            }
                        }
                        dirty_dlg = true;
                    } else if step.redraw {
                        dirty_dlg = true;
                    }
                }
            }
            if close_dlg {
                app.close_help_dialog();
                dirty_main = true;
                dirty_dlg = false;
            }
        }

        if app.main_win.sync_prefs(app.sup_ep) {
            app.query_live_prefs();
            app.sync_window_themes();
            app.ensure_visible();
            dirty_main = true;
            dirty_dlg = app.dlg_win.is_some();
        }

        if exit_now {
            break;
        }
        if dirty_main {
            app.redraw_main();
        }
        if dirty_dlg {
            app.redraw_dlg();
        }
        if let Ok(bits) = sys_notify_wait(app.notif_cap, 0, poll_tsc) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                break;
            }
        }
    }

    app.close_help_dialog();
    app.main_win.teardown(app.disp_ep);
    let _ = sys_cap_revoke(app.disp_ep, 1);
    let _ = sys_cap_revoke(app.notif_cap, 2);
    let _ = sys_ipc_call_inline(
        app.sup_ep,
        pack_msg_tag(IPC_OP_SUP_APP_EXITED, MSG_FLAG_NON_BLOCKING, 1, 0),
        [0; 4],
        0,
    );
    sys_exit(0);
}
