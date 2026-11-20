// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! NVIDIA Driver Island (`0x0218`) IPC opcodes, badges, and compact `<= 256 B` wire structs.

use core::mem::size_of;

pub const COMP_ID_NV_ISLAND: u16 = 0x0218;

// Supervisor bridge IPC opcodes on `sup_ep` (right after `IPC_OP_SUP_DIAG_RUN = 145`).
pub const IPC_OP_SUP_NV_QUERY: u32 = 146;
pub const IPC_OP_SUP_NV_EXPORT: u32 = 147;

// Direct `dry-nv-island` endpoint opcodes.
pub const NV_OP_QUERY_STATUS: u32 = 190;
pub const NV_OP_PING_HEALTH: u32 = 191;
pub const NV_OP_AUDIT_CORRUPT: u32 = 192;

pub const BADGE_NV_CLIENT: u64 = 0x0218_8001;
pub const BADGE_NV_ROGUE: u64 = 0x0218_dead;

pub const NV_PREREQ_COUNT: usize = 14;
pub const NV_LEVEL_1_READY: u8 = 1;
pub const NV_STATUS_BLOCKED: u8 = 0;
pub const NV_STATUS_FAILED: u8 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvPrereqWire {
    pub idx: u8,
    pub met: u8,
    pub injected_fail: u8,
    pub _pad: u8,
    pub err_code: i32,
    pub name: [u8; 24],
    pub detail: [u8; 32],
}

const _: () = assert!(size_of::<NvPrereqWire>() == 64);

impl NvPrereqWire {
    pub const EMPTY: Self = Self {
        idx: 0,
        met: 0,
        injected_fail: 0,
        _pad: 0,
        err_code: 0,
        name: [0; 24],
        detail: [0; 32],
    };

    pub fn make(idx: u8, met: bool, injected_fail: bool, err_code: i32, name: &[u8], detail: &[u8]) -> Self {
        let mut out = Self::EMPTY;
        out.idx = idx;
        out.met = u8::from(met);
        out.injected_fail = u8::from(injected_fail);
        out.err_code = err_code;
        let nn = name.len().min(24);
        out.name[..nn].copy_from_slice(&name[..nn]);
        let nd = detail.len().min(32);
        out.detail[..nd].copy_from_slice(&detail[..nd]);
        out
    }

    pub fn name_bytes(&self) -> &[u8] {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        &self.name[..n]
    }

    pub fn detail_bytes(&self) -> &[u8] {
        let n = self.detail.iter().position(|&b| b == 0).unwrap_or(self.detail.len());
        &self.detail[..n]
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `NvPrereqWire` is `#[repr(C)]` POD (`64` bytes).
        unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>()) }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvIslandStatusWire {
    pub level: u8,
    pub overall_status: u8,
    pub spawned: u8,
    pub mac_isolated: u8,
    pub island_alive: u8,
    pub pci_present: u8,
    pub bar_layout_ok: u8,
    pub met_count: u8,
    pub blocked_count: u8,
    pub vm_restarts: u8,
    pub corrupt_ipc_rejected: u8,
    pub unauth_cap_rejected: u8,
    pub timeout_handled: u8,
    pub export_ok: u8,
    pub export_files: u8,
    pub nvidia_exp_optin: u8,
    pub pci_vid: u16,
    pub pci_did: u16,
    pub pci_bdf: u16,
    pub igpu_vid: u16,
    pub igpu_did: u16,
    pub vgpu_present: u8,
    pub gop_ok: u8,
    pub pid: u32,
    pub generation: u32,
    pub hwval_flags: u32,
    pub pci_bar0_addr: u64,
    pub pci_bar0_size: u64,
    pub last_ipc_err: i64,
    pub session_id: [u8; 20],
    pub reason: [u8; 44],
}

const _: () = assert!(size_of::<NvIslandStatusWire>() <= 256);

impl NvIslandStatusWire {
    pub const fn empty() -> Self {
        Self {
            level: NV_LEVEL_1_READY,
            overall_status: NV_STATUS_BLOCKED,
            spawned: 0,
            mac_isolated: 0,
            island_alive: 0,
            pci_present: 0,
            bar_layout_ok: 0,
            met_count: 0,
            blocked_count: NV_PREREQ_COUNT as u8,
            vm_restarts: 0,
            corrupt_ipc_rejected: 0,
            unauth_cap_rejected: 0,
            timeout_handled: 0,
            export_ok: 0,
            export_files: 0,
            nvidia_exp_optin: 0,
            pci_vid: 0,
            pci_did: 0,
            pci_bdf: 0,
            igpu_vid: 0,
            igpu_did: 0,
            vgpu_present: 0,
            gop_ok: 0,
            pid: 0,
            generation: 0,
            hwval_flags: 0,
            pci_bar0_addr: 0,
            pci_bar0_size: 0,
            last_ipc_err: 0,
            session_id: [0; 20],
            reason: [0; 44],
        }
    }

    pub fn set_reason(&mut self, msg: &[u8]) {
        self.reason = [0; 44];
        let n = msg.len().min(44);
        self.reason[..n].copy_from_slice(&msg[..n]);
    }

    pub fn reason_bytes(&self) -> &[u8] {
        let n = self.reason.iter().position(|&b| b == 0).unwrap_or(self.reason.len());
        &self.reason[..n]
    }

    pub fn session_bytes(&self) -> &[u8] {
        let n = self.session_id.iter().position(|&b| b == 0).unwrap_or(self.session_id.len());
        &self.session_id[..n]
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `NvIslandStatusWire` is `#[repr(C)]` POD (`<= 256` bytes).
        unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>()) }
    }
}
