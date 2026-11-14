// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-files` (`Files`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0104`)
//! Capability-Badged File Explorer (`SharedPixelSurface` + `ui/` toolkit).

#![no_std]
#![no_main]

use core::cell::UnsafeCell;

use dry_userspace::apps::files_ui::{
    files_vis_rows, render_files, FilesAction, FilesPromptMode, FilesView, FILES_PIXEL_VADDR,
    FILES_WIN_H, FILES_WIN_W,
};
use dry_userspace::apps::{app_log_line, app_poll_tsc, PixelAppWindow, PointerStep};
use dry_userspace::input::{InputEvent, InputEventKind};
use dry_userspace::keymap::{
    KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_PAGEDOWN, KEY_PAGEUP, KEY_TAB, KEY_UP,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_proto::{
    vfs_create_file, vfs_list_dir, vfs_read_file, vfs_rename_file, vfs_unlink_file,
    vfs_write_small, VfsDirEntry, VFS_FLAG_READONLY, VFS_FLAG_VOLATILE, VFS_MAX_NAME_LEN,
};
use dry_userspace::wm_types::{WindowRole, TITLE_KIND_FILES};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait, sys_write_debug,
    IPC_OP_INPUT_EVENT, IPC_OP_PONG, IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_GET_VFS_CAP,
    IPC_OP_SUP_HELLO, IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT, MSG_FLAG_NON_BLOCKING, RIGHT_READ,
    RIGHT_SIGNAL,
};

const INIT_WIN_X: u64 = 112;
const INIT_WIN_Y: u64 = 68;
const MAX_LIST_ENTRIES: usize = 28;
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

    #[inline]
    const fn idx(self) -> usize {
        match self {
            Self::Boot => 0,
            Self::Ram => 1,
            Self::Persist => 2,
        }
    }
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
    mode: FilesPromptMode,
    input_buf: [u8; 48],
    input_len: usize,
    status_msg: [u8; 72],
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
    mode: FilesPromptMode::Browse,
    input_buf: [0; 48],
    input_len: 0,
    status_msg: [0; 72],
    status_len: 0,
    status_warn: false,
}));

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
        let n = msg.len().min(self.status_msg.len());
        self.status_msg[..n].copy_from_slice(&msg[..n]);
        self.status_len = n;
        self.status_warn = warn;
    }

    fn sync_scroll(&mut self, vis_rows: usize) {
        let vr = vis_rows.max(1);
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
        } else if self.sel_idx >= self.list_scroll + vr {
            self.list_scroll = self.sel_idx + 1 - vr;
        }
    }

    fn refresh_dir(&mut self, auto_preview: bool, vis_rows: usize) {
        if self.vfs_ep == 0 {
            self.set_status(b"VFS service unavailable (-ENOSYS)", true);
            return;
        }
        match vfs_list_dir(self.vfs_ep, self.mount.path(), &mut self.entries) {
            Ok((n, flags)) => {
                self.entry_count = n;
                self.mount_flags = flags;
                self.sync_scroll(vis_rows);
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
        let nlen = name_copy.iter().position(|&b| b == 0).unwrap_or(name_copy.len());
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
                    app_log_line(&lb);

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
                    app_log_line(&lb);
                }
                self.set_status(b"Access denied: owned by different compartment (-EPERM)", true);
            }
        }
    }

    fn switch_mount(&mut self, target: MountTab, vis_rows: usize) {
        self.mount = target;
        self.sel_idx = 0;
        self.list_scroll = 0;
        self.mode = FilesPromptMode::Browse;
        self.refresh_dir(true, vis_rows);

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-FILES] MOUNT path=");
        lb.push_bytes(target.path());
        lb.push_bytes(b" ro=");
        lb.push_u64(u64::from(self.is_readonly()));
        lb.push_bytes(b" entries=");
        lb.push_u64(self.entry_count as u64);
        app_log_line(&lb);

        if self.is_readonly() {
            self.set_status(b"Mounted /boot [READ-ONLY initrd]", false);
        } else if target == MountTab::Persist {
            self.set_status(b"Mounted /persist [PERSISTENT VirtIO-Blk DRYFS_v1]", false);
        } else {
            self.set_status(b"Mounted /ram [VOLATILE: resets on reboot]", false);
        }
    }

    fn select_by_name(&mut self, target_name: &[u8], vis_rows: usize) {
        for i in 0..self.entry_count {
            if self.entries[i].name_bytes() == target_name {
                self.sel_idx = i;
                self.sync_scroll(vis_rows);
                break;
            }
        }
    }

    fn commit_prompt(&mut self, vis_rows: usize) {
        let mode = self.mode;
        self.mode = FilesPromptMode::Browse;
        let raw_in = &self.input_buf[..self.input_len];
        let mut local_in = [0u8; 48];
        local_in[..raw_in.len()].copy_from_slice(raw_in);
        let in_slice = &local_in[..raw_in.len()];

        match mode {
            FilesPromptMode::Browse => {}
            FilesPromptMode::Create => {
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
                        app_log_line(&lb);
                        self.refresh_dir(false, vis_rows);
                        self.select_by_name(fname, vis_rows);
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
            FilesPromptMode::Write => {
                if self.sel_idx >= self.entry_count {
                    return;
                }
                let name_copy = self.entries[self.sel_idx].name;
                let nlen = name_copy.iter().position(|&b| b == 0).unwrap_or(name_copy.len());
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
                        app_log_line(&lb);
                        self.refresh_dir(false, vis_rows);
                        self.load_selected_preview(false);
                        self.set_status(b"Saved text to file", false);
                    }
                    Err(_) => self.set_status(b"Write denied (-EPERM / -EROFS)", true),
                }
            }
            FilesPromptMode::Rename => {
                if self.sel_idx >= self.entry_count || in_slice.is_empty() {
                    return;
                }
                let old_copy = self.entries[self.sel_idx].name;
                let olen = old_copy.iter().position(|&b| b == 0).unwrap_or(old_copy.len());
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
                        app_log_line(&lb);
                        self.refresh_dir(false, vis_rows);
                        self.select_by_name(new_fname, vis_rows);
                        self.load_selected_preview(false);
                        self.set_status(b"Renamed file", false);
                    }
                    Err(_) => self.set_status(b"Rename denied or target exists", true),
                }
            }
        }
    }

    fn delete_selected(&mut self, vis_rows: usize) {
        if self.sel_idx >= self.entry_count {
            return;
        }
        let old_copy = self.entries[self.sel_idx].name;
        let olen = old_copy.iter().position(|&b| b == 0).unwrap_or(old_copy.len());
        let mut pbuf = [0u8; 96];
        let full = build_full_path(self.mount, &old_copy[..olen], &mut pbuf);
        match vfs_unlink_file(self.vfs_ep, full) {
            Ok(()) => {
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-FILES] UNLINK path=");
                lb.push_bytes(full);
                app_log_line(&lb);
                self.refresh_dir(true, vis_rows);
                self.set_status(b"Deleted file", false);
            }
            Err(_) => self.set_status(b"Delete denied (-EPERM / -EROFS)", true),
        }
    }

    fn apply_action(&mut self, act: FilesAction, vis_rows: usize) -> bool {
        match act {
            FilesAction::None => false,
            FilesAction::SelectMount(idx) => {
                match idx {
                    0 => self.switch_mount(MountTab::Boot, vis_rows),
                    1 => self.switch_mount(MountTab::Ram, vis_rows),
                    2 if self.persist_available => self.switch_mount(MountTab::Persist, vis_rows),
                    _ => self.set_status(b"Disabled: no VirtIO-Blk disk mounted", true),
                }
                true
            }
            FilesAction::SelectEntry(idx) => {
                if idx < self.entry_count {
                    self.sel_idx = idx;
                    self.sync_scroll(vis_rows);
                    self.load_selected_preview(false);
                }
                true
            }
            FilesAction::OpenEntry(idx) => {
                if idx < self.entry_count {
                    self.sel_idx = idx;
                    self.sync_scroll(vis_rows);
                    self.load_selected_preview(true);
                }
                true
            }
            FilesAction::StartCreate => {
                if self.is_readonly() {
                    let _ = sys_write_debug(b"[DRY-FILES] ROFS_DISABLED mount=/boot\n");
                    self.set_status(b"Disabled: /boot is read-only initrd (-EROFS)", true);
                } else {
                    self.mode = FilesPromptMode::Create;
                    self.input_len = 0;
                }
                true
            }
            FilesAction::StartWrite => {
                if self.is_readonly() {
                    let _ = sys_write_debug(b"[DRY-FILES] ROFS_DISABLED mount=/boot\n");
                    self.set_status(b"Disabled: /boot is read-only initrd (-EROFS)", true);
                } else if self.entry_count > 0 {
                    self.mode = FilesPromptMode::Write;
                    self.input_len = 0;
                }
                true
            }
            FilesAction::StartRename => {
                if self.is_readonly() {
                    let _ = sys_write_debug(b"[DRY-FILES] ROFS_DISABLED mount=/boot\n");
                    self.set_status(b"Disabled: /boot is read-only initrd (-EROFS)", true);
                } else if self.entry_count > 0 {
                    self.mode = FilesPromptMode::Rename;
                    self.input_len = 0;
                }
                true
            }
            FilesAction::DeleteSelected => {
                if self.is_readonly() {
                    let _ = sys_write_debug(b"[DRY-FILES] ROFS_DISABLED mount=/boot\n");
                    self.set_status(b"Disabled: /boot is read-only initrd (-EROFS)", true);
                } else if self.entry_count > 0 {
                    self.delete_selected(vis_rows);
                }
                true
            }
        }
    }

    fn handle_non_pointer_event(&mut self, ev: InputEvent, vis_rows: usize) -> (bool, bool) {
        match ev.kind {
            InputEventKind::MouseScroll => {
                if self.entry_count > 0 && self.mode == FilesPromptMode::Browse {
                    if ev.wheel > 0 {
                        self.sel_idx = self.sel_idx.saturating_sub(1);
                    } else if ev.wheel < 0 && self.sel_idx + 1 < self.entry_count {
                        self.sel_idx += 1;
                    }
                    self.sync_scroll(vis_rows);
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
                    app_log_line(&lb);
                    return (true, false);
                }
                (false, false)
            }
            InputEventKind::KeyPress => {
                if self.mode != FilesPromptMode::Browse {
                    if ev.keycode == KEY_ESC {
                        self.mode = FilesPromptMode::Browse;
                        self.set_status(b"Cancelled", false);
                    } else if ev.keycode == KEY_ENTER || ev.ascii == b'\n' {
                        self.commit_prompt(vis_rows);
                    } else if ev.keycode == KEY_BACKSPACE || ev.ascii == 0x08 {
                        self.input_len = self.input_len.saturating_sub(1);
                    } else if (0x20..=0x7E).contains(&ev.ascii)
                        && self.input_len < self.input_buf.len()
                    {
                        self.input_buf[self.input_len] = ev.ascii;
                        self.input_len += 1;
                    }
                    return (true, false);
                }

                if ev.ascii == b'q' || ev.ascii == b'Q' || ev.keycode == KEY_ESC {
                    return (false, true);
                }
                if ev.ascii == b'1' {
                    self.switch_mount(MountTab::Boot, vis_rows);
                    return (true, false);
                }
                if ev.ascii == b'2' {
                    self.switch_mount(MountTab::Ram, vis_rows);
                    return (true, false);
                }
                if ev.ascii == b'3' {
                    if self.persist_available {
                        self.switch_mount(MountTab::Persist, vis_rows);
                    } else {
                        self.set_status(b"Disabled: no VirtIO-Blk disk mounted", true);
                    }
                    return (true, false);
                }
                if ev.keycode == KEY_TAB {
                    let next = match (self.mount, self.persist_available) {
                        (MountTab::Boot, _) => MountTab::Ram,
                        (MountTab::Ram, true) => MountTab::Persist,
                        _ => MountTab::Boot,
                    };
                    self.switch_mount(next, vis_rows);
                    return (true, false);
                }
                if ev.keycode == KEY_UP && self.entry_count > 0 {
                    self.sel_idx = self.sel_idx.saturating_sub(1);
                    self.sync_scroll(vis_rows);
                    self.load_selected_preview(false);
                    return (true, false);
                }
                if ev.keycode == KEY_DOWN && self.sel_idx + 1 < self.entry_count {
                    self.sel_idx += 1;
                    self.sync_scroll(vis_rows);
                    self.load_selected_preview(false);
                    return (true, false);
                }
                if ev.keycode == KEY_PAGEUP && self.entry_count > 0 {
                    self.sel_idx = self.sel_idx.saturating_sub(5);
                    self.sync_scroll(vis_rows);
                    self.load_selected_preview(false);
                    return (true, false);
                }
                if ev.keycode == KEY_PAGEDOWN && self.entry_count > 0 {
                    self.sel_idx = (self.sel_idx + 5).min(self.entry_count - 1);
                    self.sync_scroll(vis_rows);
                    self.load_selected_preview(false);
                    return (true, false);
                }
                if ev.keycode == KEY_ENTER || ev.ascii == b'v' || ev.ascii == b'V' {
                    self.load_selected_preview(true);
                    return (true, false);
                }

                let act = match ev.ascii {
                    b'n' | b'N' => FilesAction::StartCreate,
                    b'e' | b'E' => FilesAction::StartWrite,
                    b'r' | b'R' => FilesAction::StartRename,
                    b'd' | b'D' => FilesAction::DeleteSelected,
                    _ => return (false, false),
                };
                (self.apply_action(act, vis_rows), false)
            }
            _ => (false, false),
        }
    }

    fn render_frame(&self, win: &mut PixelAppWindow, step: PointerStep) -> FilesAction {
        let view = FilesView {
            mount_idx: self.mount.idx(),
            persist_available: self.persist_available,
            readonly: self.is_readonly(),
            volatile: (self.mount_flags & VFS_FLAG_VOLATILE) != 0,
            entries: &self.entries[..self.entry_count],
            sel_idx: self.sel_idx,
            list_scroll: self.list_scroll,
            preview_bytes: &self.preview_buf[..self.preview_len],
            preview_total: self.preview_total,
            prompt_mode: self.mode,
            prompt_input: &self.input_buf[..self.input_len],
            status_msg: &self.status_msg[..self.status_len],
            status_warn: self.status_warn,
        };
        render_files(win, &view, step)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(sup_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, _) = read_cs_ss();
    if (cs & 3) != 3 || sup_ep_cap == 0 {
        sys_exit(211);
    }
    let notif_cap = sys_notify_create(0).unwrap_or_else(|_| sys_exit(212));

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

    let init_flags = PixelAppWindow::query_theme_flags(sup_ep_cap);
    let mut win = PixelAppWindow::alloc_and_map(FILES_PIXEL_VADDR, FILES_WIN_W, FILES_WIN_H, init_flags)
        .unwrap_or_else(|_| sys_exit(213));

    // SAFETY: Single-threaded Ring 3 application owning `EXPLORER`.
    let st = unsafe { &mut *EXPLORER.0.get() };
    st.vfs_ep = vfs_ep_cap;
    if vfs_ep_cap != 0 {
        let mut probe = [VfsDirEntry::EMPTY; 1];
        st.persist_available = vfs_list_dir(vfs_ep_cap, b"/persist", &mut probe).is_ok();
    }
    let vis_rows = files_vis_rows(win.theme(), win.w as usize, win.h as usize);
    st.switch_mount(
        if st.persist_available { MountTab::Persist } else { MountTab::Boot },
        vis_rows,
    );
    let _ = st.render_frame(&mut win, PointerStep::default());
    win.publish_initial();
    let win_id = win.attach(
        disp_ep_cap,
        notif_cap,
        INIT_WIN_X,
        INIT_WIN_Y,
        WindowRole::Normal,
        TITLE_KIND_FILES,
        0,
    );

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-FILES] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    rlb.push_bytes(b" boot_entries=");
    rlb.push_u64(st.entry_count as u64);
    app_log_line(&rlb);

    let poll_tsc = app_poll_tsc(200_000);
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
                    let vr = files_vis_rows(win.theme(), win.w as usize, win.h as usize);
                    if win.handle_window_config(ev) {
                        st.sync_scroll(files_vis_rows(win.theme(), win.w as usize, win.h as usize));
                        dirty = true;
                    } else if matches!(ev.kind, InputEventKind::KeyPress | InputEventKind::MouseScroll)
                    {
                        let (d, q) = st.handle_non_pointer_event(ev, vr);
                        dirty |= d;
                        if q {
                            should_exit = true;
                            break;
                        }
                    } else {
                        let step = win.ingest_pointer(ev);
                        if step.clicked {
                            let act = st.render_frame(&mut win, step);
                            dirty |= st.apply_action(act, vr);
                            dirty |= step.redraw;
                        } else if step.redraw {
                            dirty = true;
                        }
                    }
                } else {
                    break;
                }
            }
            if win.sync_prefs(sup_ep_cap) {
                st.sync_scroll(files_vis_rows(win.theme(), win.w as usize, win.h as usize));
                dirty = true;
            }
            if dirty && !should_exit {
                let _ = st.render_frame(&mut win, PointerStep::default());
                let _ = win.present_full(disp_ep_cap);
            }
        }

        if should_exit {
            break;
        }
        if let Ok(bits) = sys_notify_wait(notif_cap, 0, poll_tsc) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                break;
            }
        }
    }

    win.teardown(disp_ep_cap);
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
