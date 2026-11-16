// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-broker` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0008`) Adaptive Driver
//! Island `Device Broker` (`Milestone 2C-0` — `ADR-016`).
//!
//! Responsibilities (`ADR-016` Control-Plane-Only Contract):
//! - Strictly control-plane: normal client driver data traffic NEVER flows through `dry-broker`
//!   (`BADGE_BROKER_CLIENT` data-forwarding attempts are rejected with `-EPERM`; clients use
//!   leased `Endpoint`s and batched `IoRingSharedPage` directly).
//! - Enforces capability badge authorization (`BADGE_BROKER_CLIENT` / `BADGE_BROKER_ADMIN`) and
//!   rejects unauthorized/rogue requests (`-EPERM`, `denied_ops` counter).
//! - Spawns, supervises, and tracks the lifecycle (`generation`, `restarts`, `state`) of the
//!   isolated `SecDomain::Userspace` `Mock Driver Island` (`dry-mock-drv.elf`, `0x0209`).
//! - Delegates revocable per-generation `Endpoint`, `Notification`, and `IoRingSharedPage`
//!   capabilities; upon island `#UD` crash or restart, reaps the dead process, revokes the entire
//!   prior generation capability subtree (`SYS_CAP_REVOKE` `op = 0` / `op = 2`), and forbids stale
//!   capability or stale generation reuse (`-EBADCAP` / `IPC_OP_ERR_STALE_GEN`).

#![no_std]
#![no_main]

use dry_userspace::broker_lifecycle::{
    validate_backend_request, BrokerLifecycle, DriverBackendKind, LifecycleStep,
};
use dry_userspace::init_legacy::{domain_spec, log_line};
use dry_userspace::island_proto::{
    island_session_badge, BADGE_BROKER_ADMIN, BADGE_BROKER_CLIENT, BADGE_ISLAND_BROKER,
    BROKER_OP_ACQUIRE_EP, BROKER_OP_ACQUIRE_RING, BROKER_OP_CRASH_AND_RECOVER,
    BROKER_OP_FORWARD_TRANSFORM, BROKER_OP_QUERY_STATE, BROKER_OP_SET_PARK_MODE,
    BROKER_OP_SHUTDOWN, COMP_ID_DEV_BROKER, COMP_ID_MOCK_ISLAND, IPC_OP_ERR_STALE_GEN,
    ISLAND_DOORBELL_CTL, ISLAND_STATE_FAULTED, ISLAND_STATE_RUNNING, ISLAND_STATE_STOPPED,
    MOCK_OP_BIND_NOTIF, MOCK_OP_INIT_ISLAND, MOCK_OP_SET_PARK_MODE, MOCK_OP_TRANSFORM,
    POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1,
};
use dry_userspace::lx_drv_proto::{
    lx_fault_badge, BADGE_BROKER_LX_MODE, BROKER_OP_BIND_PCI_DEV, BROKER_OP_DRVCTL_QUERY,
    COMP_ID_LX_DRM_BOCHS_ISLAND, COMP_ID_LX_NET_8139CP_ISLAND, COMP_ID_LX_RNG_ISLAND,
    LX_DRM_OP_QUERY_MODE, LX_NET_OP_QUERY_INFO, LX_OP_BIND_PCI, LX_OP_READ_ENTROPY,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_derive, sys_cap_query,
    sys_cap_revoke, sys_ep_create, sys_exit, sys_ipc_call_inline, sys_ipc_call_with_grant,
    sys_ipc_recv_ext, sys_ipc_reply_inline, sys_ipc_reply_with_grant, sys_ipc_send_inline,
    sys_notify_create,
    sys_notify_signal, sys_proc_kill, sys_proc_spawn, sys_proc_spawn_with_fault_ep,
    sys_proc_wait, sys_sysinfo, UserCapInfo, UserIpcRecvExt, UserSysInfo, BADGE_SUPERVISOR,
    EBADCAP, EINVAL, EPERM, EPIPE, EREVOKED, FAULT_MSG_LABEL, FAULT_REPLY_KILL, IPC_OP_CRASH_UD2,
    IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, RIGHT_CALL, RIGHT_MAP,
    RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_SIGNAL, RIGHT_WAIT, RIGHT_WRITE,
};

#[derive(Copy, Clone)]
struct PciAttachCfg {
    fault_ep: u64,
    pci_dev_cap: u64,
    comp_id: u16,
    rev_id: u8,
}

#[inline]
fn lx_probe_opcode(comp_id: u16) -> u32 {
    match comp_id {
        COMP_ID_LX_DRM_BOCHS_ISLAND => LX_DRM_OP_QUERY_MODE,
        COMP_ID_LX_NET_8139CP_ISLAND => LX_NET_OP_QUERY_INFO,
        _ => LX_OP_READ_ENTROPY,
    }
}

#[inline]
fn probe_island_ready(drv_broker_cap: u64, comp_id: u16, gen: u64, lx_mode: bool, has_pci: bool, ipc_timeout_tsc: u64) -> bool {
    if lx_mode && has_pci {
        sys_ipc_call_inline(
            drv_broker_cap,
            pack_msg_tag(lx_probe_opcode(comp_id), 0, 4, 0),
            [24, 0, 0, gen],
            ipc_timeout_tsc,
        )
        .is_ok_and(|(rt, _, _)| msg_tag_label(rt) == IPC_OP_PONG)
    } else {
        sys_ipc_call_inline(
            drv_broker_cap,
            pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
            [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, gen],
            ipc_timeout_tsc,
        )
        .is_ok_and(|(rt, _, _)| msg_tag_label(rt) == IPC_OP_PONG)
    }
}

#[inline]
fn reply_err(ep: u64, tok: u64, op: u32, err_code: isize, aux: u64) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(op, 0, 2, 0),
            [(-err_code) as u64, aux, 0, 0],
        );
    }
}

#[inline]
fn reply_ok(ep: u64, tok: u64, count: u8, args: [u64; 4]) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(ep, tok, pack_msg_tag(IPC_OP_PONG, 0, count, 0), args);
    }
}

#[inline]
fn revoke_slot(slot: &mut u64, mode: u64) {
    if *slot != 0 {
        let _ = sys_cap_revoke(*slot, mode);
        *slot = 0;
    }
}

struct IslandInstance {
    state: u64,
    proc_handle: u64,
    island_pid: u64,
    generation: u64,
    mac_isolated: bool,
    pci_dev_id: u64,
    drv_ep: u64,
    drv_broker_cap: u64,
    drv_sup_cap: u64,
    sq_notif: u64,
    cq_notif: u64,
    ring_cap: u64,
    fault_send_cap: u64,
    park_mode: u64,
}

impl IslandInstance {
    const EMPTY: Self = Self {
        state: ISLAND_STATE_STOPPED,
        proc_handle: 0,
        island_pid: 0,
        generation: 0,
        mac_isolated: false,
        pci_dev_id: 0,
        drv_ep: 0,
        drv_broker_cap: 0,
        drv_sup_cap: 0,
        sq_notif: 0,
        cq_notif: 0,
        ring_cap: 0,
        fault_send_cap: 0,
        park_mode: 0,
    };

    fn spawn(
        island_mod_cap: u64,
        generation: u64,
        pci_cfg: Option<PciAttachCfg>,
        lc: &mut BrokerLifecycle,
        ipc_timeout_tsc: u64,
    ) -> Result<Self, isize> {
        let comp_id = pci_cfg.map_or(COMP_ID_MOCK_ISLAND, |c| c.comp_id);
        let island_spec = domain_spec(0, comp_id);
        let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

        let mut inst = Self {
            state: ISLAND_STATE_FAULTED,
            proc_handle: 0,
            island_pid: 0,
            generation,
            mac_isolated: false,
            pci_dev_id: 0,
            drv_ep: 0,
            drv_broker_cap: 0,
            drv_sup_cap: 0,
            sq_notif: 0,
            cq_notif: 0,
            ring_cap: 0,
            fault_send_cap: 0,
            park_mode: 0,
        };

        inst.drv_ep = sys_ep_create(island_spec)?;
        let cap_init = (|| -> Result<(), isize> {
            inst.sq_notif = sys_notify_create(island_spec)?;
            inst.cq_notif = sys_notify_create(island_spec)?;
            inst.drv_broker_cap = sys_cap_derive(inst.drv_ep, cli_rights, 0, BADGE_ISLAND_BROKER)?;
            inst.drv_sup_cap = sys_cap_derive(inst.drv_ep, cli_rights, 0, BADGE_SUPERVISOR)?;
            if let Some(cfg) = pci_cfg {
                inst.fault_send_cap =
                    sys_cap_derive(cfg.fault_ep, cli_rights, 0, lx_fault_badge(generation))?;
                let (ph, pid) = sys_proc_spawn_with_fault_ep(
                    island_mod_cap,
                    1,
                    comp_id,
                    (inst.drv_ep, srv_rights, 0x151d),
                    inst.fault_send_cap,
                )?;
                inst.proc_handle = ph;
                inst.island_pid = pid;
            } else {
                let (ph, pid) = sys_proc_spawn(
                    island_mod_cap,
                    0,
                    COMP_ID_MOCK_ISLAND,
                    (inst.drv_ep, srv_rights, 0x151d),
                    (0, 0),
                )?;
                inst.proc_handle = ph;
                inst.island_pid = pid;
            }
            Ok(())
        })();
        if let Err(e) = cap_init {
            inst.revoke_generation_caps();
            lc.mark_spawn_failed();
            return Err(e);
        }
        inst.state = ISLAND_STATE_RUNNING;
        let _ = lc.advance(LifecycleStep::IslandCreated);

        let post_init = (|| -> Result<(), isize> {
            for (slot_idx, ncap, rights) in [
                (0u64, inst.sq_notif, RIGHT_READ | RIGHT_WAIT),
                (1u64, inst.cq_notif, RIGHT_READ | RIGHT_SIGNAL),
            ] {
                let (tag, _, _) = sys_ipc_call_with_grant(
                    inst.drv_broker_cap, MOCK_OP_BIND_NOTIF, [slot_idx, 0, 0, 0],
                    ncap, rights, 0, ipc_timeout_tsc,
                )?;
                if msg_tag_label(tag) != IPC_OP_PONG {
                    return Err(EBADCAP);
                }
            }
            let _ = lc.advance(LifecycleStep::CapsAssigned);
            if let Some(cfg) = pci_cfg {
                let (ptag, _, pmrs) = sys_ipc_call_with_grant(
                    inst.drv_broker_cap, LX_OP_BIND_PCI, [u64::from(cfg.rev_id), 0, 0, 0],
                    cfg.pci_dev_cap, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP, 0, ipc_timeout_tsc,
                )?;
                if msg_tag_label(ptag) != IPC_OP_PONG || pmrs[0] == 0 {
                    return Err(EBADCAP);
                }
                inst.pci_dev_id = pmrs[0];
                lc.pci_vendor_device = pmrs[0] as u32;
            }
            let _ = lc.advance(LifecycleStep::ResourcesMapped);
            let (itag, granted_ring, imrs) = sys_ipc_call_with_grant(
                inst.drv_broker_cap, MOCK_OP_INIT_ISLAND, [generation, 0, 0, 0],
                0, 0, 0, ipc_timeout_tsc,
            )?;
            if msg_tag_label(itag) != IPC_OP_PONG || granted_ring == 0 || imrs[0] != generation {
                return Err(EBADCAP);
            }
            inst.ring_cap = granted_ring;
            inst.mac_isolated = imrs[1] != 0;
            Ok(())
        })();
        if let Err(e) = post_init {
            inst.force_reap_and_revoke(ipc_timeout_tsc);
            lc.mark_spawn_failed();
            return Err(e);
        }
        let _ = lc.advance(LifecycleStep::Initialized);
        let _ = lc.advance(LifecycleStep::Published);
        let _ = lc.advance(LifecycleStep::Monitoring);
        Ok(inst)
    }

    #[inline]
    fn wake_if_ring_parked(&self) {
        if self.park_mode == 1 && self.sq_notif != 0 {
            let _ = sys_notify_signal(self.sq_notif, ISLAND_DOORBELL_CTL);
        }
    }

    fn revoke_generation_caps(&mut self) {
        if self.ring_cap != 0 {
            let _ = sys_cap_revoke(self.ring_cap, 0);
            revoke_slot(&mut self.ring_cap, 1);
        }
        revoke_slot(&mut self.fault_send_cap, 1);
        revoke_slot(&mut self.drv_broker_cap, 1);
        revoke_slot(&mut self.drv_sup_cap, 1);
        revoke_slot(&mut self.drv_ep, 2);
        revoke_slot(&mut self.sq_notif, 2);
        revoke_slot(&mut self.cq_notif, 2);
        self.park_mode = 0;
    }

    fn force_reap_and_revoke(&mut self, ipc_timeout_tsc: u64) {
        if self.proc_handle != 0 {
            if sys_proc_wait(self.proc_handle, 1, 0).is_err() {
                let _ = sys_proc_kill(self.proc_handle);
                let _ = sys_proc_wait(self.proc_handle, 0, ipc_timeout_tsc);
            }
            self.proc_handle = 0;
        }
        self.revoke_generation_caps();
        self.state = ISLAND_STATE_STOPPED;
    }

    fn check_async_crash_and_recover(
        &mut self,
        island_mod_cap: u64,
        pci_cfg: Option<PciAttachCfg>,
        restarts: &mut u64,
        lc: &mut BrokerLifecycle,
        ipc_timeout_tsc: u64,
    ) -> Result<(), isize> {
        if self.state == ISLAND_STATE_FAULTED {
            self.revoke_generation_caps();
            let next_gen = self.generation.wrapping_add(1);
            lc.begin_fault_recovery(next_gen);
            *self = Self::spawn(island_mod_cap, next_gen, pci_cfg, lc, ipc_timeout_tsc)?;
            *restarts = restarts.wrapping_add(1);
            return Ok(());
        }
        if self.state != ISLAND_STATE_RUNNING || self.proc_handle == 0 {
            return Ok(());
        }
        if let Ok((_w_kind, _w_detail)) = sys_proc_wait(self.proc_handle, 1, 0) {
            self.proc_handle = 0;
            self.state = ISLAND_STATE_FAULTED;
            self.revoke_generation_caps();
            let next_gen = self.generation.wrapping_add(1);
            lc.begin_fault_recovery(next_gen);
            *self = Self::spawn(island_mod_cap, next_gen, pci_cfg, lc, ipc_timeout_tsc)?;
            *restarts = restarts.wrapping_add(1);
        }
        Ok(())
    }

    fn shutdown_clean(&mut self, ipc_timeout_tsc: u64) {
        if self.state == ISLAND_STATE_RUNNING && self.drv_sup_cap != 0 && self.proc_handle != 0 {
            self.wake_if_ring_parked();
            let _ = sys_ipc_call_inline(
                self.drv_sup_cap,
                pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
                [0; 4],
                ipc_timeout_tsc,
            );
            if sys_proc_wait(self.proc_handle, 0, ipc_timeout_tsc).is_err() {
                let _ = sys_proc_kill(self.proc_handle);
                let _ = sys_proc_wait(self.proc_handle, 0, ipc_timeout_tsc);
            }
            self.proc_handle = 0;
        }
        self.revoke_generation_caps();
        self.state = ISLAND_STATE_STOPPED;
    }
}

#[no_mangle]
pub extern "C" fn _start(broker_ep: u64, mock_drv_mod_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || broker_ep == 0 || mock_drv_mod_cap == 0 {
        sys_exit(185);
    }
    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let ipc_timeout_tsc = 2_000_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    let mut cinfo = UserCapInfo::default();
    let lx_mode = sys_cap_query(broker_ep, &mut cinfo) == 0 && cinfo.badge == BADGE_BROKER_LX_MODE;
    let fault_ep = if lx_mode {
        match sys_ep_create(domain_spec(0, COMP_ID_DEV_BROKER)) {
            Ok(c) => c,
            Err(_) => sys_exit(185),
        }
    } else {
        0
    };
    let mut saved_pci_cap = 0u64;
    let mut saved_comp_id = COMP_ID_LX_RNG_ISLAND;
    let mut saved_rev_id = 0u8;
    let mut lifecycle = BrokerLifecycle::new(
        if lx_mode { DriverBackendKind::LxCompatIsland } else { DriverBackendKind::NativeDriverIsland },
        if lx_mode { COMP_ID_LX_RNG_ISLAND } else { COMP_ID_MOCK_ISLAND },
    );

    let mut island = if lx_mode {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-BROKER] Ready in CPL3 RootSystem [0x0008] mode=pci-deferred");
        log_line(&lb);
        IslandInstance::EMPTY
    } else {
        lifecycle.prepare_for_spawn(DriverBackendKind::NativeDriverIsland, COMP_ID_MOCK_ISLAND, 0);
        let inst = match IslandInstance::spawn(
            mock_drv_mod_cap,
            1,
            None,
            &mut lifecycle,
            ipc_timeout_tsc,
        ) {
            Ok(i) => i,
            Err(_) => sys_exit(186),
        };
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-BROKER] Ready in CPL3 RootSystem [0x0008] island_pid=");
        lb.push_u64(inst.island_pid);
        lb.push_bytes(b" gen=1");
        log_line(&lb);
        inst
    };

    let mut restarts = 0u64;
    let mut forwarded_ops = 0u64;
    let mut denied_ops = 0u64;
    let mut stale_rejects = 0u64;

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, tok, mrs) = match sys_ipc_recv_ext(broker_ep, 0, 0, &mut ext) {
            Ok(v) => v,
            Err(_) => break,
        };

        let op = msg_tag_label(tag);
        let is_admin = ext.sender_badge == BADGE_BROKER_ADMIN;
        let is_client = is_admin || ext.sender_badge == BADGE_BROKER_CLIENT;

        if !is_client {
            if ext.transferred_cap != 0 {
                let _ = sys_cap_revoke(ext.transferred_cap, 1);
            }
            denied_ops = denied_ops.wrapping_add(1);
            reply_err(broker_ep, tok, IPC_OP_ERR_PERM, EPERM, denied_ops);
            continue;
        }

        let active_pci = if lx_mode && saved_pci_cap != 0 {
            Some(PciAttachCfg {
                fault_ep,
                pci_dev_cap: saved_pci_cap,
                comp_id: saved_comp_id,
                rev_id: saved_rev_id,
            })
        } else {
            None
        };
        let _ = island.check_async_crash_and_recover(
            mock_drv_mod_cap,
            active_pci,
            &mut restarts,
            &mut lifecycle,
            ipc_timeout_tsc,
        );

        match op {
            BROKER_OP_BIND_PCI_DEV => {
                if !is_admin || !lx_mode || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    denied_ops = denied_ops.wrapping_add(1);
                    reply_err(
                        broker_ep,
                        tok,
                        if !is_admin { IPC_OP_ERR_PERM } else { IPC_OP_ERR_INVAL },
                        if !is_admin { EPERM } else { EINVAL },
                        denied_ops,
                    );
                    continue;
                }
                let req_backend = if mrs[2] != 0 { mrs[2] } else { 1 };
                let validated_backend = match validate_backend_request(req_backend, mrs[3]) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                        denied_ops = denied_ops.wrapping_add(1);
                        reply_err(
                            broker_ep,
                            tok,
                            if e == EPERM { IPC_OP_ERR_PERM } else { IPC_OP_ERR_INVAL },
                            e,
                            denied_ops,
                        );
                        continue;
                    }
                };
                if saved_pci_cap != 0 {
                    let _ = sys_cap_revoke(saved_pci_cap, 1);
                }
                saved_pci_cap = ext.transferred_cap;
                saved_comp_id = if mrs[0] != 0 { mrs[0] as u16 } else { COMP_ID_LX_RNG_ISLAND };
                saved_rev_id = (mrs[1] & 0xff) as u8;
                lifecycle.prepare_for_spawn(validated_backend, saved_comp_id, saved_rev_id);
                island.force_reap_and_revoke(ipc_timeout_tsc);
                island = match IslandInstance::spawn(
                    mock_drv_mod_cap,
                    1,
                    Some(PciAttachCfg {
                        fault_ep,
                        pci_dev_cap: saved_pci_cap,
                        comp_id: saved_comp_id,
                        rev_id: saved_rev_id,
                    }),
                    &mut lifecycle,
                    ipc_timeout_tsc,
                ) {
                    Ok(i) => i,
                    Err(e) => {
                        reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, e, 0);
                        continue;
                    }
                };
                reply_ok(
                    broker_ep,
                    tok,
                    4,
                    [
                        island.generation,
                        island.island_pid,
                        island.pci_dev_id,
                        u64::from(island.mac_isolated),
                    ],
                );
            }
            BROKER_OP_DRVCTL_QUERY => {
                reply_ok(broker_ep, tok, 4, lifecycle.encode_drvctl(island.island_pid));
            }
            BROKER_OP_QUERY_STATE => {
                let packed_rej =
                    (denied_ops & 0xffff_ffff) | ((stale_rejects & 0xffff_ffff) << 32);
                reply_ok(broker_ep, tok, 4, [island.state, island.generation, restarts, packed_rej]);
            }
            BROKER_OP_FORWARD_TRANSFORM => {
                // Control-plane-only invariant (`ADR-016`): normal client data traffic MUST NOT
                // traverse `Device Broker`. Only `BADGE_BROKER_ADMIN` diagnostic benchmark probes
                // may invoke the 2-hop reference path.
                if !is_admin {
                    denied_ops = denied_ops.wrapping_add(1);
                    reply_err(broker_ep, tok, IPC_OP_ERR_PERM, EPERM, denied_ops);
                    continue;
                }
                let expected_gen = mrs[3];
                if expected_gen != 0 && expected_gen != island.generation {
                    stale_rejects = stale_rejects.wrapping_add(1);
                    if tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            broker_ep,
                            tok,
                            pack_msg_tag(IPC_OP_ERR_STALE_GEN, 0, 3, 0),
                            [(-EREVOKED) as u64, island.generation, stale_rejects, 0],
                        );
                    }
                    continue;
                }
                island.wake_if_ring_parked();
                let fwd_res = if lx_mode && saved_pci_cap != 0 {
                    sys_ipc_call_inline(
                        island.drv_broker_cap,
                        pack_msg_tag(lx_probe_opcode(saved_comp_id), 0, 4, 0),
                        [24, 0, 0, island.generation],
                        ipc_timeout_tsc,
                    )
                } else {
                    sys_ipc_call_inline(
                        island.drv_broker_cap,
                        pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
                        [mrs[0], mrs[1], mrs[2], island.generation],
                        ipc_timeout_tsc,
                    )
                };
                match fwd_res {
                    Ok((rtag, _, rmrs)) if msg_tag_label(rtag) == IPC_OP_PONG => {
                        forwarded_ops = forwarded_ops.wrapping_add(1);
                        reply_ok(
                            broker_ep,
                            tok,
                            4,
                            [rmrs[0], rmrs[1], island.generation, forwarded_ops],
                        );
                    }
                    _ => {
                        let _ = island.check_async_crash_and_recover(
                            mock_drv_mod_cap,
                            active_pci,
                            &mut restarts,
                            &mut lifecycle,
                            ipc_timeout_tsc,
                        );
                        reply_err(broker_ep, tok, IPC_OP_ERR_STALE_GEN, EPIPE, island.generation);
                    }
                }
            }
            BROKER_OP_ACQUIRE_EP => {
                if tok != 0 {
                    let sess_badge = island_session_badge(island.generation);
                    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
                    let _ = sys_ipc_reply_with_grant(
                        broker_ep,
                        tok,
                        IPC_OP_PONG,
                        [
                            island.generation,
                            u64::from(island.mac_isolated),
                            island.island_pid,
                            restarts,
                        ],
                        island.drv_ep,
                        cli_rights,
                        sess_badge,
                    );
                }
            }
            BROKER_OP_ACQUIRE_RING => {
                if tok == 0 {
                    continue;
                }
                let (src_cap, rights) = match mrs[0] {
                    0 => (island.ring_cap, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP),
                    1 => (island.sq_notif, RIGHT_READ | RIGHT_SIGNAL),
                    2 => (island.cq_notif, RIGHT_READ | RIGHT_WAIT),
                    _ => {
                        reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, EINVAL, 0);
                        continue;
                    }
                };
                if src_cap == 0 {
                    reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, EBADCAP, 0);
                    continue;
                }
                let _ = sys_ipc_reply_with_grant(
                    broker_ep,
                    tok,
                    IPC_OP_PONG,
                    [island.generation, mrs[0], 0, 0],
                    src_cap,
                    rights,
                    0,
                );
            }
            BROKER_OP_SET_PARK_MODE => {
                if !is_admin {
                    denied_ops = denied_ops.wrapping_add(1);
                    reply_err(broker_ep, tok, IPC_OP_ERR_PERM, EPERM, denied_ops);
                    continue;
                }
                let target_mode = if mrs[0] != 0 { 1 } else { 0 };
                island.wake_if_ring_parked();
                match sys_ipc_call_inline(
                    island.drv_broker_cap,
                    pack_msg_tag(MOCK_OP_SET_PARK_MODE, 0, 1, 0),
                    [target_mode, 0, 0, 0],
                    ipc_timeout_tsc,
                ) {
                    Ok((rtag, _, rmrs)) if msg_tag_label(rtag) == IPC_OP_PONG => {
                        island.park_mode = rmrs[0];
                        reply_ok(broker_ep, tok, 2, [island.park_mode, island.generation, 0, 0]);
                    }
                    _ => reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, EPIPE, island.generation),
                }
            }
            BROKER_OP_CRASH_AND_RECOVER => {
                if !is_admin {
                    denied_ops = denied_ops.wrapping_add(1);
                    reply_err(broker_ep, tok, IPC_OP_ERR_PERM, EPERM, denied_ops);
                    continue;
                }
                let t0 = rdtsc_user();
                island.wake_if_ring_parked();
                let _ = sys_ipc_send_inline(
                    island.drv_sup_cap,
                    pack_msg_tag(IPC_OP_CRASH_UD2, 0, 0, 0),
                    [0; 4],
                    ipc_timeout_tsc,
                );
                let fault_vec = (|| -> Result<u64, isize> {
                    let fault_vec_from_ep = if fault_ep != 0 && island.fault_send_cap != 0 {
                        let mut fext = UserIpcRecvExt::default();
                        let (ftag, ftok, fmrs) =
                            sys_ipc_recv_ext(fault_ep, 0, ipc_timeout_tsc, &mut fext)?;
                        let fvec = if msg_tag_label(ftag) == FAULT_MSG_LABEL
                            && fext.sender_badge == lx_fault_badge(island.generation)
                        {
                            fmrs[0] & 0xff
                        } else {
                            0
                        };
                        if ftok != 0 {
                            let _ = sys_ipc_reply_inline(
                                fault_ep, ftok, pack_msg_tag(FAULT_REPLY_KILL, 0, 1, 0), [0; 4],
                            );
                        }
                        Some(fvec)
                    } else {
                        None
                    };
                    let (w_kind, w_detail) = sys_proc_wait(island.proc_handle, 0, ipc_timeout_tsc)?;
                    Ok(fault_vec_from_ep.unwrap_or(if w_kind == 1 { w_detail & 0xff } else { 0 }))
                })();
                let fault_vec = match fault_vec {
                    Ok(v) => v,
                    Err(e) => {
                        island.force_reap_and_revoke(ipc_timeout_tsc);
                        island.state = ISLAND_STATE_FAULTED;
                        reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, e, 0);
                        continue;
                    }
                };
                island.proc_handle = 0;
                island.state = ISLAND_STATE_FAULTED;

                island.revoke_generation_caps();
                let next_gen = island.generation.wrapping_add(1);
                lifecycle.begin_fault_recovery(next_gen);
                island = match IslandInstance::spawn(
                    mock_drv_mod_cap,
                    next_gen,
                    active_pci,
                    &mut lifecycle,
                    ipc_timeout_tsc,
                ) {
                    Ok(i) => i,
                    Err(e) => {
                        island.generation = next_gen;
                        island.state = ISLAND_STATE_FAULTED;
                        reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, e, next_gen);
                        continue;
                    }
                };
                // Readiness probe on freshly restarted generation before declaring recovery complete.
                let probe_ok = probe_island_ready(
                    island.drv_broker_cap,
                    saved_comp_id,
                    island.generation,
                    lx_mode,
                    saved_pci_cap != 0,
                    ipc_timeout_tsc,
                );
                if !probe_ok {
                    island.force_reap_and_revoke(ipc_timeout_tsc);
                    island.state = ISLAND_STATE_FAULTED;
                    lifecycle.mark_spawn_failed();
                    reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, EPIPE, island.generation);
                    continue;
                }
                let dt_cyc = rdtsc_user().saturating_sub(t0);
                restarts = restarts.wrapping_add(1);
                reply_ok(broker_ep, tok, 4, [island.generation, dt_cyc, fault_vec, restarts]);
            }
            BROKER_OP_SHUTDOWN => {
                if !is_admin {
                    denied_ops = denied_ops.wrapping_add(1);
                    reply_err(broker_ep, tok, IPC_OP_ERR_PERM, EPERM, denied_ops);
                    continue;
                }
                island.shutdown_clean(ipc_timeout_tsc);
                lifecycle.mark_unloaded();
                if saved_pci_cap != 0 {
                    let _ = sys_cap_revoke(saved_pci_cap, 1);
                }
                if fault_ep != 0 {
                    let _ = sys_cap_revoke(fault_ep, 2);
                }
                let mut slb = LineBuilder::new();
                slb.push_bytes(b"[DRY-BROKER-STATS] forwarded=");
                slb.push_u64(forwarded_ops);
                slb.push_bytes(b" restarts=");
                slb.push_u64(restarts);
                slb.push_bytes(b" denied=");
                slb.push_u64(denied_ops);
                slb.push_bytes(b" stale_rej=");
                slb.push_u64(stale_rejects);
                log_line(&slb);

                let mut lcb = LineBuilder::new();
                lcb.push_bytes(b"[DRY-BROKER-LIFECYCLE] step=");
                lcb.push_bytes(lifecycle.step.name());
                lcb.push_bytes(b" backend=");
                lcb.push_bytes(lifecycle.backend.name());
                lcb.push_bytes(b" comp=0x");
                lcb.push_hex_u64(u64::from(lifecycle.comp_id));
                lcb.push_bytes(b" gen=");
                lcb.push_u64(lifecycle.generation);
                lcb.push_bytes(b" visited=0x");
                lcb.push_hex_u64(u64::from(lifecycle.visited_mask));
                log_line(&lcb);

                reply_ok(broker_ep, tok, 4, [forwarded_ops, restarts, denied_ops, stale_rejects]);
                sys_exit(0);
            }
            _ => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, EINVAL, 0);
            }
        }
    }
    island.shutdown_clean(ipc_timeout_tsc);
    sys_exit(0);
}
