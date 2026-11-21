// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Lazy on-demand Supervisor Networking Bring-Up (`dry-broker` + `dry-lx-net` + `/bin/dry-net.elf`)
//! so historical tests without `-device rtl8139` or with `pci_passive` incur zero overhead.

use crate::abi_ext::{RootInitBootExtInfo, BOOT_EXT_INFO_OFFSET, ROOT_BOOT_EXT_MAGIC};
use crate::init_legacy::{domain_spec, log_line};
use crate::island_proto::{
    BADGE_BROKER_ADMIN, BADGE_BROKER_CLIENT, BROKER_OP_SHUTDOWN, COMP_ID_DEV_BROKER,
};
use crate::lx_compat_meta::find_boot_pci_managed;
use crate::lx_drv_proto::{
    BADGE_BROKER_LX_MODE, BROKER_OP_BIND_PCI_DEV, COMP_ID_LX_NET_8139CP_ISLAND,
};
use crate::net::COMP_ID_NET_SVC;
use crate::shell::LineBuilder;
use crate::sup_exec::spawn_vfs_elf_checked;
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_reply_inline, sys_ipc_reply_with_grant,
    sys_proc_spawn, sys_proc_wait, RootInitBootInfo, BADGE_SUPERVISOR, BADGE_SUP_SHELL,
    IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, RIGHT_CALL, RIGHT_GRANT,
    RIGHT_MAP, RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_SPAWN, RIGHT_WRITE,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;

pub struct SupNetState {
    pub active: bool,
    pub broker_handle: u64,
    pub broker_ep: u64,
    pub admin_cap: u64,
    pub client_cap: u64,
    pub drv_ep: u64,
    pub net_handle: u64,
    pub net_ep: u64,
    pub net_sup_cap: u64,
}

impl SupNetState {
    pub const fn new() -> Self {
        Self {
            active: false,
            broker_handle: 0,
            broker_ep: 0,
            admin_cap: 0,
            client_cap: 0,
            drv_ep: 0,
            net_handle: 0,
            net_ep: 0,
            net_sup_cap: 0,
        }
    }

    pub fn ensure_started(
        &mut self,
        boot_info_addr: u64,
        boot: &RootInitBootInfo,
        vfs_sup_cap: u64,
    ) -> Result<u64, u32> {
        if self.active && self.net_ep != 0 {
            return Ok(self.net_ep);
        }
        if boot_info_addr == 0 || vfs_sup_cap == 0 || boot.broker_mod_cap == 0 {
            return Err(IPC_OP_ERR_NOSYS);
        }
        // SAFETY: Kernel populates `RootInitBootExtInfo` at `boot_info_addr + BOOT_EXT_INFO_OFFSET`.
        let ext = unsafe {
            core::ptr::read_unaligned(
                (boot_info_addr + BOOT_EXT_INFO_OFFSET) as *const RootInitBootExtInfo,
            )
        };
        if ext.magic != ROOT_BOOT_EXT_MAGIC
            || ext.lx_net_mod_cap == 0
            || ext.pci_net_cap == 0
            || ext.pci_passive_mode != 0
        {
            return Err(IPC_OP_ERR_NOSYS);
        }

        let pci_rev = find_boot_pci_managed(boot_info_addr, boot.boot_pci_func_count, 0x10ec, 0x8139, 9)
            .or_else(|| find_boot_pci_managed(boot_info_addr, boot.boot_pci_func_count, 0x10ec, 0x8139, 0xff))
            .map_or(0, |r| r.revision_id);

        let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
        let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

        let Ok(broker_ep) = sys_ep_create(ipc_spec) else {
            return Err(IPC_OP_ERR_NOSYS);
        };
        let Ok(admin_cap) = sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_ADMIN) else {
            let _ = sys_cap_revoke(broker_ep, 2);
            return Err(IPC_OP_ERR_NOSYS);
        };
        let Ok(client_cap) = sys_cap_derive(broker_ep, cli_rights | RIGHT_GRANT, 0, BADGE_BROKER_CLIENT) else {
            let _ = sys_cap_revoke(admin_cap, 1);
            let _ = sys_cap_revoke(broker_ep, 2);
            return Err(IPC_OP_ERR_NOSYS);
        };
        let Ok((broker_handle, _)) = sys_proc_spawn(
            boot.broker_mod_cap,
            1,
            COMP_ID_DEV_BROKER,
            (broker_ep, srv_rights, BADGE_BROKER_LX_MODE),
            (ext.lx_net_mod_cap, RIGHT_READ | RIGHT_SPAWN),
        ) else {
            let _ = sys_cap_revoke(client_cap, 1);
            let _ = sys_cap_revoke(admin_cap, 1);
            let _ = sys_cap_revoke(broker_ep, 2);
            return Err(IPC_OP_ERR_NOSYS);
        };

        let bound_ok = sys_ipc_call_with_grant(
            admin_cap,
            BROKER_OP_BIND_PCI_DEV,
            [COMP_ID_LX_NET_8139CP_ISLAND as u64, u64::from(pci_rev), 1, 0],
            ext.pci_net_cap,
            RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT,
            0,
            0,
        )
        .is_ok_and(|(t, _, m)| msg_tag_label(t) == IPC_OP_PONG && m[0] == 1);

        if !bound_ok {
            let _ = sys_ipc_call_inline(
                admin_cap,
                pack_msg_tag(BROKER_OP_SHUTDOWN, 0, 1, 0),
                [0; 4],
                0,
            );
            let _ = sys_proc_wait(broker_handle, 0, 0);
            let _ = sys_cap_revoke(client_cap, 1);
            let _ = sys_cap_revoke(admin_cap, 1);
            let _ = sys_cap_revoke(broker_ep, 2);
            return Err(IPC_OP_ERR_NOSYS);
        }

        let Ok(net_ep) = sys_ep_create(ipc_spec) else {
            let _ = sys_ipc_call_inline(
                admin_cap,
                pack_msg_tag(BROKER_OP_SHUTDOWN, 0, 1, 0),
                [0; 4],
                0,
            );
            let _ = sys_proc_wait(broker_handle, 0, 0);
            let _ = sys_cap_revoke(client_cap, 1);
            let _ = sys_cap_revoke(admin_cap, 1);
            let _ = sys_cap_revoke(broker_ep, 2);
            return Err(IPC_OP_ERR_NOSYS);
        };
        let net_sup_cap = sys_cap_derive(net_ep, cli_rights, 0, BADGE_SUPERVISOR).unwrap_or(0);
        let Ok((net_handle, net_pid)) = spawn_vfs_elf_checked(
            vfs_sup_cap,
            b"/bin/dry-net.elf",
            0,
            COMP_ID_NET_SVC,
            (net_ep, srv_rights, 0),
            (client_cap, cli_rights),
        ) else {
            let _ = sys_cap_revoke(net_sup_cap, 1);
            let _ = sys_cap_revoke(net_ep, 2);
            let _ = sys_ipc_call_inline(
                admin_cap,
                pack_msg_tag(BROKER_OP_SHUTDOWN, 0, 1, 0),
                [0; 4],
                0,
            );
            let _ = sys_proc_wait(broker_handle, 0, 0);
            let _ = sys_cap_revoke(client_cap, 1);
            let _ = sys_cap_revoke(admin_cap, 1);
            let _ = sys_cap_revoke(broker_ep, 2);
            return Err(IPC_OP_ERR_NOSYS);
        };

        self.active = true;
        self.broker_handle = broker_handle;
        self.broker_ep = broker_ep;
        self.admin_cap = admin_cap;
        self.client_cap = client_cap;
        self.drv_ep = 0;
        self.net_handle = net_handle;
        self.net_ep = net_ep;
        self.net_sup_cap = net_sup_cap;

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-INIT] Network stack service ready pid=");
        lb.push_u64(net_pid);
        log_line(&lb);
        Ok(net_ep)
    }

    pub fn teardown(&mut self) {
        if !self.active {
            return;
        }
        if self.net_sup_cap != 0 {
            let _ = sys_ipc_call_inline(
                self.net_sup_cap,
                pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
                [0; 4],
                0,
            );
            let _ = sys_proc_wait(self.net_handle, 0, 0);
            let _ = sys_cap_revoke(self.net_sup_cap, 1);
        }
        if self.net_ep != 0 {
            let _ = sys_cap_revoke(self.net_ep, 2);
        }
        if self.drv_ep != 0 {
            let _ = sys_cap_revoke(self.drv_ep, 1);
        }
        if self.admin_cap != 0 {
            let _ = sys_ipc_call_inline(
                self.admin_cap,
                pack_msg_tag(BROKER_OP_SHUTDOWN, 0, 1, 0),
                [0; 4],
                0,
            );
            let _ = sys_proc_wait(self.broker_handle, 0, 0);
            let _ = sys_cap_revoke(self.admin_cap, 1);
        }
        if self.client_cap != 0 {
            let _ = sys_cap_revoke(self.client_cap, 1);
        }
        if self.broker_ep != 0 {
            let _ = sys_cap_revoke(self.broker_ep, 2);
        }
        *self = Self::new();
    }
}

impl Default for SupNetState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn handle_get_net_cap_ipc(
    sup_ep: u64,
    reply_tok: u64,
    sender_badge: u64,
    boot_info_addr: u64,
    boot: &RootInitBootInfo,
    vfs_sup_cap: u64,
    state: &mut SupNetState,
) {
    if sender_badge != BADGE_SUP_SHELL && sender_badge != BADGE_SUPERVISOR {
        if reply_tok != 0 {
            let _ = sys_ipc_reply_inline(
                sup_ep,
                reply_tok,
                pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                [0; 4],
            );
        }
        return;
    }
    match state.ensure_started(boot_info_addr, boot, vfs_sup_cap) {
        Ok(net_ep) => {
            if reply_tok != 0 {
                let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
                let _ = sys_ipc_reply_with_grant(
                    sup_ep,
                    reply_tok,
                    IPC_OP_PONG,
                    [1, 0, 0, 0],
                    net_ep,
                    cli_rights,
                    u64::from(COMP_ID_NET_SVC),
                );
            }
        }
        Err(err_op) => {
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    sup_ep,
                    reply_tok,
                    pack_msg_tag(err_op, 0, 1, 0),
                    [0; 4],
                );
            }
        }
    }
}
