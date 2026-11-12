// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared Ring 3 Desktop Action API & Native Application Registry Types (`Milestone 2A-3`).
//!
//! Ensures mouse clicks (Topbar Launcher pills, window controls), keyboard shortcuts (`Alt+1..4`,
//! `Alt+Tab`, `Alt+T`, `Alt+G`, `Alt+L`), `Settings` controls, and `Terminal` commands all emit
//! the exact same strongly-typed `DesktopAction` payloads.

use crate::wm::{
    TITLE_KIND_FILES, TITLE_KIND_SETTINGS, TITLE_KIND_SYSMON, TITLE_KIND_TERMINAL,
};
use crate::{
    BADGE_SUP_FILES, BADGE_SUP_SETTINGS, BADGE_SUP_SHELL, BADGE_SUP_SYSMON, BADGE_WIN_FILES,
    BADGE_WIN_SETTINGS, BADGE_WIN_SHELL, BADGE_WIN_SYSMON,
};

pub const APP_COUNT: usize = 4;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppId {
    Terminal = 0,
    LifeStation = 1,
    Files = 2,
    Settings = 3,
}

impl AppId {
    pub const ALL: [Self; APP_COUNT] = [
        Self::Terminal,
        Self::LifeStation,
        Self::Files,
        Self::Settings,
    ];

    #[inline]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Terminal),
            1 => Some(Self::LifeStation),
            2 => Some(Self::Files),
            3 => Some(Self::Settings),
            _ => None,
        }
    }

    pub fn from_name(raw: &[u8]) -> Option<Self> {
        match raw {
            b"terminal" | b"term" | b"shell" | b"1" => Some(Self::Terminal),
            b"life" | b"lifestation" | b"sysmon" | b"2" => Some(Self::LifeStation),
            b"files" | b"fs" | b"3" => Some(Self::Files),
            b"settings" | b"set" | b"prefs" | b"4" => Some(Self::Settings),
            _ => None,
        }
    }

    #[inline]
    pub const fn slug(self) -> &'static [u8] {
        match self {
            Self::Terminal => b"terminal",
            Self::LifeStation => b"lifestation",
            Self::Files => b"files",
            Self::Settings => b"settings",
        }
    }

    #[inline]
    pub const fn pill_label(self) -> &'static [u8] {
        match self {
            Self::Terminal => b"1:Term",
            Self::LifeStation => b"2:Life",
            Self::Files => b"3:Files",
            Self::Settings => b"4:Set",
        }
    }

    #[inline]
    pub const fn compartment_id(self) -> u16 {
        match self {
            Self::Terminal => 0x0102,
            Self::LifeStation => 0x0103,
            Self::Files => 0x0104,
            Self::Settings => 0x0105,
        }
    }

    #[inline]
    pub const fn win_badge(self) -> u64 {
        match self {
            Self::Terminal => BADGE_WIN_SHELL,
            Self::LifeStation => BADGE_WIN_SYSMON,
            Self::Files => BADGE_WIN_FILES,
            Self::Settings => BADGE_WIN_SETTINGS,
        }
    }

    #[inline]
    pub const fn sup_badge(self) -> u64 {
        match self {
            Self::Terminal => BADGE_SUP_SHELL,
            Self::LifeStation => BADGE_SUP_SYSMON,
            Self::Files => BADGE_SUP_FILES,
            Self::Settings => BADGE_SUP_SETTINGS,
        }
    }

    #[inline]
    pub const fn title_kind(self) -> u8 {
        match self {
            Self::Terminal => TITLE_KIND_TERMINAL,
            Self::LifeStation => TITLE_KIND_SYSMON,
            Self::Files => TITLE_KIND_FILES,
            Self::Settings => TITLE_KIND_SETTINGS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopAction {
    None,
    LaunchOrFocusApp(AppId),
    CloseApp(AppId),
    RestartApp(AppId),
    CloseWindow(u32),
    ToggleTiling,
    SetLayoutMode(u8),
    CycleFocus,
    SetKeymap(u8),
    SetPointerSpeed(u8),
    SetThemeContrast(u8),
    SetBlankTimeoutSec(u16),
    BlankDisplayNow,
    LockSession,
    UnlockSession,
    RequestShutdown,
    RequestReboot,
    RequestSuspendS3,
    RequestPoweroffHw,
}

impl DesktopAction {
    #[inline]
    pub const fn to_words(self) -> [u64; 2] {
        match self {
            Self::None => [0, 0],
            Self::LaunchOrFocusApp(app) => [1, app as u64],
            Self::CloseApp(app) => [2, app as u64],
            Self::RestartApp(app) => [3, app as u64],
            Self::CloseWindow(wid) => [4, wid as u64],
            Self::ToggleTiling => [5, 0],
            Self::SetLayoutMode(m) => [6, m as u64],
            Self::CycleFocus => [7, 0],
            Self::SetKeymap(k) => [8, k as u64],
            Self::SetPointerSpeed(s) => [9, s as u64],
            Self::SetThemeContrast(c) => [10, c as u64],
            Self::SetBlankTimeoutSec(sec) => [11, sec as u64],
            Self::BlankDisplayNow => [12, 0],
            Self::LockSession => [13, 0],
            Self::UnlockSession => [14, 0],
            Self::RequestShutdown => [15, 0],
            Self::RequestReboot => [16, 0],
            Self::RequestSuspendS3 => [17, 0],
            Self::RequestPoweroffHw => [18, 0],
        }
    }

    #[inline]
    pub const fn from_words(w0: u64, w1: u64) -> Self {
        match w0 {
            1 => match AppId::from_u8(w1 as u8) {
                Some(a) => Self::LaunchOrFocusApp(a),
                None => Self::None,
            },
            2 => match AppId::from_u8(w1 as u8) {
                Some(a) => Self::CloseApp(a),
                None => Self::None,
            },
            3 => match AppId::from_u8(w1 as u8) {
                Some(a) => Self::RestartApp(a),
                None => Self::None,
            },
            4 => Self::CloseWindow(w1 as u32),
            5 => Self::ToggleTiling,
            6 => Self::SetLayoutMode((w1 & 0xff) as u8),
            7 => Self::CycleFocus,
            8 => Self::SetKeymap((w1 & 0xff) as u8),
            9 => Self::SetPointerSpeed((w1 & 0xff) as u8),
            10 => Self::SetThemeContrast((w1 & 0xff) as u8),
            11 => Self::SetBlankTimeoutSec((w1 & 0xffff) as u16),
            12 => Self::BlankDisplayNow,
            13 => Self::LockSession,
            14 => Self::UnlockSession,
            15 => Self::RequestShutdown,
            16 => Self::RequestReboot,
            17 => Self::RequestSuspendS3,
            18 => Self::RequestPoweroffHw,
            _ => Self::None,
        }
    }
}

/// Fixed-capacity lock-free ring buffer for queued `DesktopAction` items in `WindowManager`.
pub struct ActionQueue<const N: usize> {
    items: [DesktopAction; N],
    head: usize,
    len: usize,
}

impl<const N: usize> ActionQueue<N> {
    pub const fn new() -> Self {
        Self {
            items: [DesktopAction::None; N],
            head: 0,
            len: 0,
        }
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, act: DesktopAction) -> bool {
        if matches!(act, DesktopAction::None) || self.len >= N {
            return false;
        }
        let idx = (self.head + self.len) % N;
        self.items[idx] = act;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<DesktopAction> {
        if self.len == 0 {
            return None;
        }
        let act = self.items[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(act)
    }
}

impl<const N: usize> Default for ActionQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
pub const fn compartment_short_name(comp_id: u16, domain: u8) -> &'static [u8] {
    match (domain, comp_id) {
        (1, 0x0000) => b"dry-init",
        (1, 0x0002) => b"dry-display",
        (1, 0x0003) => b"dry-input",
        (1, 0x0004) => b"dry-vfs",
        (1, 0x0005) => b"dry-power",
        (_, 0x0101) => b"dry-echo",
        (_, 0x0102) => b"Terminal",
        (_, 0x0103) => b"LifeStation",
        (_, 0x0104) => b"Files",
        (_, 0x0105) => b"Settings",
        (1, _) => b"root-svc",
        _ => b"user-app",
    }
}

/// Fixed 64-sample latency ring computing exact `p50/p95/p99` TSC cycle percentiles.
pub struct LatencyRing {
    samples: [u32; 64],
    count: usize,
}

impl LatencyRing {
    pub const fn new() -> Self {
        Self {
            samples: [0; 64],
            count: 0,
        }
    }

    #[inline]
    pub fn record(&mut self, cyc: u64) {
        let idx = self.count & 63;
        self.samples[idx] = cyc.min(u32::MAX as u64) as u32;
        self.count = self.count.wrapping_add(1);
    }

    pub fn percentiles(&self) -> (u64, u64, u64) {
        let n = self.count.min(64);
        if n == 0 {
            return (0, 0, 0);
        }
        let mut tmp = [0u32; 64];
        tmp[..n].copy_from_slice(&self.samples[..n]);
        for i in 1..n {
            let key = tmp[i];
            let mut j = i;
            while j > 0 && tmp[j - 1] > key {
                tmp[j] = tmp[j - 1];
                j -= 1;
            }
            tmp[j] = key;
        }
        (
            tmp[(n - 1) * 50 / 100] as u64,
            tmp[(n - 1) * 95 / 100] as u64,
            tmp[(n - 1) * 99 / 100] as u64,
        )
    }
}

impl Default for LatencyRing {
    fn default() -> Self {
        Self::new()
    }
}
