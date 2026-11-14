// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-files` (`Files`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0104`)
//! Capability-Badged File Explorer (`Milestone 2A-3`).
//!
//! Architectural & Epistemic Honesty Properties:
//! - Holds zero hardware/kernel capabilities (`sup_ep`, `disp_ep`, and compartment-badged `vfs_ep` only).
//! - Enables `Create`/`Write`/`Rename`/`Delete` ONLY when the active mount reports writable support
//!   (`(mount_flags & VFS_FLAG_READONLY) == 0`); `/boot` mutations are visibly disabled.
//! - Explicitly warns that `/ram` is volatile RAM storage (`Resets on reboot — no disk mounted`).

#![no_std]
#![no_main]

use core::cell::UnsafeCell;

use dry_userspace::gfx::BOX_HORIZ;
use dry_userspace::input::{InputEvent, InputEventKind};
use dry_userspace::keymap::{
    KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_PAGEDOWN, KEY_PAGEUP, KEY_TAB, KEY_UP,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::surface::{
    SharedWindowSurface, SurfaceCell, STYLE_ACCENT, STYLE_DEFAULT, STYLE_HEADER, STYLE_MUTED,
    STYLE_OK, STYLE_PROMPT, STYLE_WARN,
};
use dry_userspace::vfs_proto::{
    vfs_create_file, vfs_list_dir, vfs_read_file, vfs_rename_file, vfs_unlink_file,
    vfs_write_small, VfsDirEntry, VFS_FLAG_PRIVATE, VFS_FLAG_READONLY, VFS_FLAG_VOLATILE,
    VFS_MAX_NAME_LEN,
};
use dry_userspace::wm::{WindowRole, TITLE_KIND_FILES};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait,
    sys_vspace_map, sys_write_debug, IPC_OP_INPUT_EVENT, IPC_OP_PONG, IPC_OP_SUP_APP_EXITED,
    IPC_OP_SUP_GET_VFS_CAP, IPC_OP_SUP_HELLO, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_PRESENT,
    IPC_OP_WIN_BIND_NOTIF, IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT,
    MSG_FLAG_NON_BLOCKING, RIGHT_MAP, RIGHT_READ, RIGHT_SIGNAL, VSPACE_MAP_OP_4K_RW,
    VSPACE_MAP_OP_UNMAP_4K,
};

const FILES_SURF_VADDR: u64 = 0x1900_0000;
const FILES_COLS: usize = 66;
const FILES_ROWS: usize = 22;
const INIT_WIN_X: u64 = 112;
const INIT_WIN_Y: u64 = 68;
const MAX_LIST_ENTRIES: usize = 28;
const LIST_VIEW_ROWS: usize = 12; // rows 6..18
const PREVIEW_BUF_CAP: usize = 512;
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MountTab {
    Boot,
    Ram,
    Persist,
}

impl MountTab {
    #[inline]
    const fn path(self) -> &'static [u8] {
        match self {
            Self::Boot => b"/boot",
            Self::Ram => b"/ram",
            Self::Persist => b"/persist",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UiMode {
    Browse,
    PromptCreate,
    PromptWrite,
    PromptRename,
}

struct ExplorerState {
    vfs_ep: u64,
    persist_available: bool,
    mount: MountTab,
    mount_flags: u16,
    entries: [VfsDirEntry; MAX_LIST_ENTRIES],
    entry_count: usize,
    sel_idx: usize,
    list_scroll: usize,
    preview_buf: [u8; PREVIEW_BUF_CAP],
    preview_len: usize,
    preview_total: usize,
    mode: UiMode,
    input_buf: [u8; 48],
    input_len: usize,
    status_msg: [u8; FILES_COLS],
    status_len: usize,
    status_warn: bool,
}

struct SingleThreadExplorer(UnsafeCell<ExplorerState>);
// SAFETY: `dry-files` is strictly single-threaded in CPL=3.
unsafe impl Sync for SingleThreadExplorer {}

static EXPLORER: SingleThreadExplorer = SingleThreadExplorer(UnsafeCell::new(ExplorerState {
    vfs_ep: 0,
    persist_available: false,
    mount: MountTab::Boot,
    mount_flags: VFS_FLAG_READONLY,
    entries: [VfsDirEntry::EMPTY; MAX_LIST_ENTRIES],
    entry_count: 0,
    sel_idx: 0,
    list_scroll: 0,
    preview_buf: [0; PREVIEW_BUF_CAP],
    preview_len: 0,
    preview_total: 0,
    mode: UiMode::Browse,
    input_buf: [0; 48],
    input_len: 0,
    status_msg: [0; FILES_COLS],
    status_len: 0,
    status_warn: false,
}));

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn write_row(surf: &SharedWindowSurface, row: usize, text: &[u8], style: u8) {
    let mut cells = [SurfaceCell::BLANK; FILES_COLS];
    for (i, &ch) in text.iter().take(FILES_COLS).enumerate() {
        cells[i] = SurfaceCell { ch, style };
    }
    surf.store_row_slice(FILES_COLS, row, &cells);
}

fn build_full_path<'a>(mount: MountTab, name: &[u8], out: &'a mut [u8; 96]) -> &'a [u8] {
    let mp = mount.path();
    let mut p = 0usize;
    out[..mp.len()].copy_from_slice(mp);
    p += mp.len();
    out[p] = b'/';
    p += 1;
    let take = name.len().min(out.len() - p);
    out[p..(p + take)].copy_from_slice(&name[..take]);
    &out[..(p + take)]
}

impl ExplorerState {
    #[inline]
    fn is_readonly(&self) -> bool {
        (self.mount_flags & VFS_FLAG_READONLY) != 0
    }

    fn set_status(&mut self, msg: &[u8], warn: bool) {
        let n = msg.len().min(FILES_COLS);
        self.status_msg[..n].copy_from_slice(&msg[..n]);
        self.status_len = n;
        self.status_warn = warn;
    }

    fn sync_scroll(&mut self) {
        if self.entry_count == 0 {
            self.sel_idx = 0;
            self.list_scroll = 0;
            return;
        }
        if self.sel_idx >= self.entry_count {
            self.sel_idx = self.entry_count - 1;
        }
        if self.sel_idx < self.list_scroll {
            self.list_scroll = self.sel_idx;
        } else if self.sel_idx >= self.list_scroll + LIST_VIEW_ROWS {
            self.list_scroll = self.sel_idx + 1 - LIST_VIEW_ROWS;
        }
    }

    fn refresh_dir(&mut self, auto_preview: bool) {
        if self.vfs_ep == 0 {
            self.set_status(b"VFS service unavailable (-ENOSYS)", true);
            return;
        }
        match vfs_list_dir(self.vfs_ep, self.mount.path(), &mut self.entries) {
            Ok((n, flags)) => {
                self.entry_count = n;
                self.mount_flags = flags;
                self.sync_scroll();
                if auto_preview {
                    self.load_selected_preview(false);
                }
            }
            Err(_) => {
                self.entry_count = 0;
                self.set_status(b"Failed to list directory", true);
            }
        }
    }

    fn load_selected_preview(&mut self, log_view: bool) {
        self.preview_len = 0;
        self.preview_total = 0;
        if self.sel_idx >= self.entry_count {
            return;
        }
        let name_copy = self.entries[self.sel_idx].name;
        let nlen = name_copy
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_copy.len());
        let mut pbuf = [0u8; 96];
        let full = build_full_path(self.mount, &name_copy[..nlen], &mut pbuf);
        match vfs_read_file(self.vfs_ep, full, 0, &mut self.preview_buf) {
            Ok((got, total)) => {
                self.preview_len = got;
                self.preview_total = total;
                if log_view {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-FILES] VIEW path=");
                    lb.push_bytes(full);
                    lb.push_bytes(b" bytes=");
                    lb.push_u64(total as u64);
                    log_line(&lb);
                    let mut st = LineBuilder::new();
                    st.push_bytes(b"Opened ");
                    st.push_bytes(full);
                    st.push_bytes(b" (");
                    st.push_u64(total as u64);
                    st.push_bytes(b" B)");
                    self.set_status(st.as_bytes(), false);
                }
            }
            Err(err) => {
                let msg = b"[ACCESS DENIED BY COMPARTMENT BADGE (-EPERM)]";
                self.preview_buf[..msg.len()].copy_from_slice(msg);
                self.preview_len = msg.len();
                self.preview_total = msg.len();
                if log_view {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-FILES] PERM_DENIED path=");
                    lb.push_bytes(full);
                    lb.push_bytes(b" err=");
                    lb.push_u64((-err) as u64);
                    log_line(&lb);
                }
                self.set_status(b"Access denied: owned by different compartment (-EPERM)", true);
            }
        }
    }

    fn switch_mount(&mut self, target: MountTab) {
        self.mount = target;
        self.sel_idx = 0;
        self.list_scroll = 0;
        self.mode = UiMode::Browse;
        self.refresh_dir(true);
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-FILES] MOUNT path=");
        lb.push_bytes(target.path());
        lb.push_bytes(b" ro=");
        lb.push_u64(u64::from(self.is_readonly()));
        lb.push_bytes(b" entries=");
        lb.push_u64(self.entry_count as u64);
        log_line(&lb);
        if self.is_readonly() {
            self.set_status(b"Mounted /boot [READ-ONLY initrd]", false);
        } else if target == MountTab::Persist {
            self.set_status(b"Mounted /persist [PERSISTENT VirtIO-Blk DRYFS_v1]", false);
        } else {
            self.set_status(b"Mounted /ram [VOLATILE: resets on reboot]", false);
        }
    }

    fn select_by_name(&mut self, target_name: &[u8]) {
        for i in 0..self.entry_count {
            if self.entries[i].name_bytes() == target_name {
                self.sel_idx = i;
                self.sync_scroll();
                break;
            }
        }
    }

    fn commit_prompt(&mut self) {
        let mode = self.mode;
        self.mode = UiMode::Browse;
        let raw_in = &self.input_buf[..self.input_len];
        let mut local_in = [0u8; 48];
        local_in[..raw_in.len()].copy_from_slice(raw_in);
        let in_slice = &local_in[..raw_in.len()];

        match mode {
            UiMode::Browse => {}
            UiMode::PromptCreate => {
                let fname = if in_slice.is_empty() {
                    b"note.txt".as_slice()
                } else {
                    &in_slice[..in_slice.len().min(VFS_MAX_NAME_LEN)]
                };
                let mut pbuf = [0u8; 96];
                let full = build_full_path(self.mount, fname, &mut pbuf);
                match vfs_create_file(self.vfs_ep, full, 0) {
                    Ok(ino) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-FILES] CREATE path=");
                        lb.push_bytes(full);
                        lb.push_bytes(b" ino=");
                        lb.push_u64(ino as u64);
                        log_line(&lb);
                        self.refresh_dir(false);
                        self.select_by_name(fname);
                        self.load_selected_preview(false);
                        self.set_status(
                            if self.mount == MountTab::Persist {
                                b"Created persistent file in /persist (CoW + FLUSH)"
                            } else {
                                b"Created volatile file in /ram"
                            },
                            false,
                        );
                    }
                    Err(_) => self.set_status(b"Create failed (invalid or duplicate name)", true),
                }
            }
            UiMode::PromptWrite => {
                if self.sel_idx >= self.entry_count {
                    return;
                }
                let name_copy = self.entries[self.sel_idx].name;
                let nlen = name_copy
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(name_copy.len());
                let mut pbuf = [0u8; 96];
                let full = build_full_path(self.mount, &name_copy[..nlen], &mut pbuf);
                let mut payload = [0u8; 50];
                let plen = in_slice.len().min(48);
                payload[..plen].copy_from_slice(&in_slice[..plen]);
                payload[plen] = b'\n';
                match vfs_write_small(self.vfs_ep, full, &payload[..=plen], false) {
                    Ok(written) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-FILES] WRITE path=");
                        lb.push_bytes(full);
                        lb.push_bytes(b" bytes=");
                        lb.push_u64(written as u64);
                        log_line(&lb);
                        self.refresh_dir(false);
                        self.load_selected_preview(false);
                        self.set_status(b"Saved text to volatile /ram file", false);
                    }
                    Err(_) => self.set_status(b"Write denied (-EPERM / -EROFS)", true),
                }
            }
            UiMode::PromptRename => {
                if self.sel_idx >= self.entry_count || in_slice.is_empty() {
                    return;
                }
                let old_copy = self.entries[self.sel_idx].name;
                let olen = old_copy
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(old_copy.len());
                let new_fname = &in_slice[..in_slice.len().min(VFS_MAX_NAME_LEN)];
                let mut old_buf = [0u8; 96];
                let mut new_buf = [0u8; 96];
                let old_full = build_full_path(self.mount, &old_copy[..olen], &mut old_buf);
                let new_full = build_full_path(self.mount, new_fname, &mut new_buf);
                match vfs_rename_file(self.vfs_ep, old_full, new_full) {
                    Ok(()) => {
                        let mut lb = LineBuilder::new();
                        lb.push_bytes(b"[DRY-FILES] RENAME old=");
                        lb.push_bytes(&old_copy[..olen]);
                        lb.push_bytes(b" new=");
                        lb.push_bytes(new_fname);
                        log_line(&lb);
                        self.refresh_dir(false);
                        self.select_by_name(new_fname);
                        self.load_selected_preview(false);
                        self.set_status(b"Renamed file in /ram", false);
                    }
                    Err(_) => self.set_status(b"Rename denied or target exists", true),
                }
            }
        }
    }

    fn delete_selected(&mut self) {
        if self.sel_idx >= self.entry_count {
            return;
        }
        let old_copy = self.entries[self.sel_idx].name;
        let olen = old_copy
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(old_copy.len());
        let mut pbuf = [0u8; 96];
        let full = build_full_path(self.mount, &old_copy[..olen], &mut pbuf);
        match vfs_unlink_file(self.vfs_ep, full) {
            Ok(()) => {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-FILES] UNLINK path=");
                lb.push_bytes(full);
                log_line(&lb);
                self.refresh_dir(true);
                self.set_status(b"Deleted file from /ram", false);
            }
            Err(_) => self.set_status(b"Delete denied (-EPERM / -EROFS)", true),
        }
    }

    fn handle_event(&mut self, ev: InputEvent) -> bool {
        match ev.kind {
            InputEventKind::MouseScroll => {
                if self.entry_count > 0 && self.mode == UiMode::Browse {
                    if ev.wheel > 0 {
                        self.sel_idx = self.sel_idx.saturating_sub(1);
                    } else if ev.wheel < 0 && self.sel_idx + 1 < self.entry_count {
                        self.sel_idx += 1;
                    }
                    self.sync_scroll();
                    self.load_selected_preview(false);
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-FILES] SCROLL wheel=");
                    if ev.wheel < 0 {
                        lb.push_byte(b'-');
                        lb.push_u64((-ev.wheel) as u64);
                    } else {
                        lb.push_u64(ev.wheel as u64);
                    }
                    lb.push_bytes(b" sel=");
                    lb.push_u64(self.sel_idx as u64);
                    log_line(&lb);
                }
                false
            }
            InputEventKind::KeyPress => {
                if self.mode != UiMode::Browse {
                    if ev.keycode == KEY_ESC {
                        self.mode = UiMode::Browse;
                        self.set_status(b"Cancelled", false);
                    } else if ev.keycode == KEY_ENTER || ev.ascii == b'\n' {
                        self.commit_prompt();
                    } else if ev.keycode == KEY_BACKSPACE || ev.ascii == 0x08 {
                        self.input_len = self.input_len.saturating_sub(1);
                    } else if (0x20..=0x7E).contains(&ev.ascii)
                        && self.input_len < self.input_buf.len()
                    {
                        self.input_buf[self.input_len] = ev.ascii;
                        self.input_len += 1;
                    }
                    return false;
                }

                if ev.ascii == b'q' || ev.ascii == b'Q' {
                    return true;
                }
                if ev.ascii == b'1' {
                    self.switch_mount(MountTab::Boot);
                    return false;
                }
                if ev.ascii == b'2' {
                    self.switch_mount(MountTab::Ram);
                    return false;
                }
                if ev.ascii == b'3' {
                    if self.persist_available {
                        self.switch_mount(MountTab::Persist);
                    } else {
                        self.set_status(b"Disabled: no VirtIO-Blk disk mounted", true);
                    }
                    return false;
                }
                if ev.keycode == KEY_TAB {
                    let next = match (self.mount, self.persist_available) {
                        (MountTab::Boot, _) => MountTab::Ram,
                        (MountTab::Ram, true) => MountTab::Persist,
                        _ => MountTab::Boot,
                    };
                    self.switch_mount(next);
                    return false;
                }
                if ev.keycode == KEY_UP && self.entry_count > 0 {
                    self.sel_idx = self.sel_idx.saturating_sub(1);
                    self.sync_scroll();
                    self.load_selected_preview(false);
                    return false;
                }
                if ev.keycode == KEY_DOWN && self.sel_idx + 1 < self.entry_count {
                    self.sel_idx += 1;
                    self.sync_scroll();
                    self.load_selected_preview(false);
                    return false;
                }
                if ev.keycode == KEY_PAGEUP && self.entry_count > 0 {
                    self.sel_idx = self.sel_idx.saturating_sub(5);
                    self.sync_scroll();
                    self.load_selected_preview(false);
                    return false;
                }
                if ev.keycode == KEY_PAGEDOWN && self.entry_count > 0 {
                    self.sel_idx = (self.sel_idx + 5).min(self.entry_count - 1);
                    self.sync_scroll();
                    self.load_selected_preview(false);
                    return false;
                }
                if ev.keycode == KEY_ENTER || ev.ascii == b'v' || ev.ascii == b'V' {
                    self.load_selected_preview(true);
                    return false;
                }

                let is_mut_key = matches!(
                    ev.ascii,
                    b'n' | b'N' | b'e' | b'E' | b'r' | b'R' | b'd' | b'D'
                );
                if is_mut_key && self.is_readonly() {
                    let _ = sys_write_debug(b"[DRY-FILES] ROFS_DISABLED mount=/boot\n");
                    self.set_status(b"Disabled: /boot is read-only initrd (-EROFS)", true);
                    return false;
                }

                match ev.ascii {
                    b'n' | b'N' => {
                        self.mode = UiMode::PromptCreate;
                        self.input_len = 0;
                    }
                    b'e' | b'E' if self.entry_count > 0 => {
                        self.mode = UiMode::PromptWrite;
                        self.input_len = 0;
                    }
                    b'r' | b'R' if self.entry_count > 0 => {
                        self.mode = UiMode::PromptRename;
                        self.input_len = 0;
                    }
                    b'd' | b'D' if self.entry_count > 0 => {
                        self.delete_selected();
                    }
                    _ => {}
                }
                false
            }
            _ => false,
        }
    }

    fn render(&self, surf: &SharedWindowSurface) {
        let odd = surf.begin_write();
        let div = [BOX_HORIZ; FILES_COLS];

        write_row(
            surf,
            0,
            b"DRY FILES // GLASS CAPABILITY VFS EXPLORER [0x0104]",
            STYLE_HEADER,
        );
        write_row(surf, 1, &div, STYLE_MUTED);

        let tabs: &[u8] = match (self.mount, self.persist_available) {
            (MountTab::Boot, false) => {
                b" >[1: /boot (RO Initrd)]<    [2: /ram (Volatile RW)]   [Tab] Swap"
            }
            (MountTab::Ram, false) => {
                b"  [1: /boot (RO Initrd)]    >[2: /ram (Volatile RW)]<  [Tab] Swap"
            }
            (MountTab::Boot, true) => {
                b" >[1:/boot RO]<  [2:/ram Vol]  [3:/persist Disk RW]   [Tab] Swap"
            }
            (MountTab::Ram, true) => {
                b"  [1:/boot RO]  >[2:/ram Vol]< [3:/persist Disk RW]   [Tab] Swap"
            }
            (MountTab::Persist, _) => {
                b"  [1:/boot RO]   [2:/ram Vol] >[3:/persist Disk RW]<  [Tab] Swap"
            }
        };
        write_row(surf, 2, tabs, STYLE_ACCENT);

        if self.is_readonly() {
            write_row(
                surf,
                3,
                b" BACKEND: /boot [READ-ONLY] -- Create/Write/Rename/Delete DISABLED",
                STYLE_MUTED,
            );
        } else if (self.mount_flags & VFS_FLAG_VOLATILE) != 0 {
            write_row(
                surf,
                3,
                b" NOTICE: /ram is VOLATILE RAM -- All files reset on reboot!",
                STYLE_WARN,
            );
        } else {
            write_row(
                surf,
                3,
                b" BACKEND: /persist [PERSISTENT VirtIO-Blk DRYFS_v1 -- CoW + FLUSH]",
                STYLE_OK,
            );
        }

        write_row(surf, 4, &div, STYLE_MUTED);
        write_row(
            surf,
            5,
            b" DIRECTORY ENTRIES            | FILE PREVIEW",
            STYLE_ACCENT,
        );

        // Split preview text into lines for right-hand pane (`cols 31..66`).
        let mut prev_lines: [&[u8]; LIST_VIEW_ROWS] = [&[]; LIST_VIEW_ROWS];
        let pslice = &self.preview_buf[..self.preview_len];
        let mut p_start = 0usize;
        for slot in &mut prev_lines {
            if p_start >= pslice.len() {
                break;
            }
            let rem = &pslice[p_start..];
            let nl = rem.iter().position(|&b| b == b'\n').unwrap_or(rem.len());
            let take = nl.min(33);
            *slot = &rem[..take];
            p_start += if take < nl { take } else { (nl + 1).min(rem.len()) };
        }

        for (r, &prev_ln) in prev_lines.iter().enumerate() {
            let row = 6 + r;
            let mut cells = [SurfaceCell::BLANK; FILES_COLS];
            let idx = self.list_scroll + r;
            if idx < self.entry_count {
                let e = &self.entries[idx];
                let selected = idx == self.sel_idx;
                let row_style = if selected {
                    STYLE_OK
                } else if (e.flags & VFS_FLAG_PRIVATE) != 0 {
                    STYLE_WARN
                } else {
                    STYLE_DEFAULT
                };
                let mut lb = LineBuilder::new();
                lb.push_bytes(if selected { b"> " } else { b"  " });
                let nm = e.name_bytes();
                let take = nm.len().min(18);
                lb.push_bytes(&nm[..take]);
                for _ in take..19 {
                    lb.push_byte(b' ');
                }
                if (e.flags & VFS_FLAG_PRIVATE) != 0 {
                    lb.push_bytes(b"PRIV ");
                } else {
                    lb.push_u64(e.size_bytes as u64);
                    lb.push_byte(b'B');
                }
                for (c, &ch) in lb.as_bytes().iter().take(29).enumerate() {
                    cells[c] = SurfaceCell { ch, style: row_style };
                }
            }
            cells[30] = SurfaceCell {
                ch: b'|',
                style: STYLE_MUTED,
            };
            for (c, &ch) in prev_ln.iter().take(34).enumerate() {
                let safe = if (0x20..=0x7E).contains(&ch) { ch } else { b'.' };
                cells[32 + c] = SurfaceCell {
                    ch: safe,
                    style: STYLE_DEFAULT,
                };
            }
            surf.store_row_slice(FILES_COLS, row, &cells);
        }

        write_row(surf, 18, &div, STYLE_MUTED);
        if self.is_readonly() {
            write_row(
                surf,
                19,
                b" [Enter/V] View  [Up/Dn/Wheel] Select  [N/E/R/D: Disabled RO] [Q]",
                STYLE_MUTED,
            );
        } else {
            write_row(
                surf,
                19,
                b" [Enter/V] View  [N] New  [E] Write  [R] Rename  [D] Del  [Q] Quit",
                STYLE_ACCENT,
            );
        }

        match self.mode {
            UiMode::Browse => {
                let st_style = if self.status_warn { STYLE_WARN } else { STYLE_OK };
                write_row(surf, 20, &self.status_msg[..self.status_len], st_style);
                write_row(
                    surf,
                    21,
                    b" Isolation: Compartment 0x0104 badge checked per VFS inode",
                    STYLE_MUTED,
                );
            }
            pmode => {
                let prefix: &[u8] = match (pmode, self.mount) {
                    (UiMode::PromptCreate, MountTab::Persist) => b" CREATE /persist/> ",
                    (UiMode::PromptCreate, _) => b" CREATE /ram/> ",
                    (UiMode::PromptWrite, _) => b" WRITE TEXT> ",
                    (UiMode::PromptRename, MountTab::Persist) => b" RENAME TO /persist/> ",
                    (UiMode::PromptRename, _) => b" RENAME TO /ram/> ",
                    (UiMode::Browse, _) => b"",
                };
                let mut plb = LineBuilder::new();
                plb.push_bytes(prefix);
                plb.push_bytes(&self.input_buf[..self.input_len]);
                plb.push_byte(b'_');
                write_row(surf, 20, plb.as_bytes(), STYLE_PROMPT);
                write_row(surf, 21, b" [Enter] Confirm   [Esc] Cancel", STYLE_ACCENT);
            }
        }

        surf.end_write(odd);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(sup_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, _) = read_cs_ss();
    if (cs & 3) != 3 || sup_ep_cap == 0 {
        sys_exit(211);
    }

    let notif_cap = match sys_notify_create(0) {
        Ok(c) => c,
        Err(_) => sys_exit(212),
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

    let mut vfs_ep_cap: u64 = 0;
    if let Ok((vtag, vcap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_GET_VFS_CAP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(vtag) == IPC_OP_PONG && vcap != 0 {
            vfs_ep_cap = vcap;
        }
    }

    let surf_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(213),
    };
    if sys_vspace_map(surf_cap, FILES_SURF_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(214);
    }
    // SAFETY: `FILES_SURF_VADDR` is mapped `USER_RW` exclusively in `dry-files`'s private `VSpace`.
    let surf = unsafe {
        SharedWindowSurface::init_at(FILES_SURF_VADDR, FILES_COLS as u16, FILES_ROWS as u16, false)
    };
    // SAFETY: Single-threaded Ring 3 application owning `EXPLORER`.
    let st = unsafe { &mut *EXPLORER.0.get() };
    st.vfs_ep = vfs_ep_cap;
    if vfs_ep_cap != 0 {
        let mut probe = [VfsDirEntry::EMPTY; 1];
        st.persist_available = vfs_list_dir(vfs_ep_cap, b"/persist", &mut probe).is_ok();
    }
    st.switch_mount(if st.persist_available {
        MountTab::Persist
    } else {
        MountTab::Boot
    });
    st.render(surf);

    let mut win_id: u32 = 0;
    if disp_ep_cap != 0 {
        let attach_mrs = [
            (FILES_COLS as u64) | ((FILES_ROWS as u64) << 16),
            INIT_WIN_X | (INIT_WIN_Y << 32),
            (WindowRole::Normal as u64) | ((TITLE_KIND_FILES as u64) << 8),
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
    }

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-FILES] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    rlb.push_bytes(b" boot_entries=");
    rlb.push_u64(st.entry_count as u64);
    log_line(&rlb);

    let mut should_exit = false;
    loop {
        if disp_ep_cap != 0 && win_id != 0 {
            let mut dirty = false;
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    should_exit = true;
                    break;
                }
                if plbl == IPC_OP_INPUT_EVENT {
                    let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                    if st.handle_event(ev) {
                        should_exit = true;
                        break;
                    }
                    dirty = true;
                } else {
                    break;
                }
            }
            if dirty && !should_exit {
                st.render(surf);
                let _ = sys_ipc_call_inline(
                    disp_ep_cap,
                    pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 2, 0),
                    [win_id as u64, ((FILES_ROWS as u64) << 16), 0, 0],
                    0,
                );
            }
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

    if disp_ep_cap != 0 && win_id != 0 {
        let _ = sys_ipc_call_inline(
            disp_ep_cap,
            pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
            [win_id as u64, 0, 0, 0],
            0,
        );
    }
    let _ = sys_vspace_map(surf_cap, FILES_SURF_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let _ = sys_cap_revoke(surf_cap, 2);
    if vfs_ep_cap != 0 {
        let _ = sys_cap_revoke(vfs_ep_cap, 1);
    }
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
