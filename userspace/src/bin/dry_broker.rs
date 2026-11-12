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

use dry_userspace::init_legacy::{domain_spec, log_line};
use dry_userspace::island_proto::{
    island_session_badge, BADGE_BROKER_ADMIN, BADGE_BROKER_CLIENT, BADGE_ISLAND_BROKER,
    BROKER_OP_ACQUIRE_EP, BROKER_OP_ACQUIRE_RING, BROKER_OP_CRASH_AND_RECOVER,
    BROKER_OP_FORWARD_TRANSFORM, BROKER_OP_QUERY_STATE, BROKER_OP_SET_PARK_MODE,
    BROKER_OP_SHUTDOWN, COMP_ID_MOCK_ISLAND, IPC_OP_ERR_STALE_GEN, ISLAND_DOORBELL_CTL,
    ISLAND_STATE_FAULTED, ISLAND_STATE_RUNNING, ISLAND_STATE_STOPPED, MOCK_OP_BIND_NOTIF,
    MOCK_OP_INIT_ISLAND, MOCK_OP_SET_PARK_MODE, MOCK_OP_TRANSFORM, POC_TEST_SEED, POC_TEST_WORD0,
    POC_TEST_WORD1,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_derive, sys_cap_revoke,
    sys_ep_create, sys_exit, sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_recv_ext,
    sys_ipc_reply_inline, sys_ipc_reply_with_grant, sys_ipc_send_inline, sys_notify_create,
    sys_notify_signal, sys_proc_spawn, sys_proc_wait, UserIpcRecvExt,
    BADGE_SUPERVISOR, EBADCAP, EINVAL, EPERM, EPIPE, EREVOKED, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, RIGHT_CALL, RIGHT_MAP, RIGHT_READ, RIGHT_RECV,
    RIGHT_REPLY, RIGHT_SEND, RIGHT_SIGNAL, RIGHT_WAIT, RIGHT_WRITE,
};

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

struct IslandInstance {
    state: u64,
    proc_handle: u64,
    island_pid: u64,
    generation: u64,
    mac_isolated: bool,
    drv_ep: u64,
    drv_broker_cap: u64,
    drv_sup_cap: u64,
    sq_notif: u64,
    cq_notif: u64,
    ring_cap: u64,
    park_mode: u64,
}

impl IslandInstance {
    fn spawn(mock_mod_cap: u64, generation: u64) -> Result<Self, isize> {
        let island_spec = domain_spec(0, COMP_ID_MOCK_ISLAND);
        let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

        let drv_ep = sys_ep_create(island_spec)?;
        let sq_notif = match sys_notify_create(island_spec) {
            Ok(c) => c,
            Err(e) => {
                let _ = sys_cap_revoke(drv_ep, 2);
                return Err(e);
            }
        };
        let cq_notif = match sys_notify_create(island_spec) {
            Ok(c) => c,
            Err(e) => {
                let _ = sys_cap_revoke(sq_notif, 2);
                let _ = sys_cap_revoke(drv_ep, 2);
                return Err(e);
            }
        };
        let drv_broker_cap = sys_cap_derive(drv_ep, cli_rights, 0, BADGE_ISLAND_BROKER)?;
        let drv_sup_cap = sys_cap_derive(drv_ep, cli_rights, 0, BADGE_SUPERVISOR)?;

        let (proc_handle, island_pid) = sys_proc_spawn(
            mock_mod_cap,
            0, // `SecDomain::Userspace` isolated compartment (`0x0209`)
            COMP_ID_MOCK_ISLAND,
            (drv_ep, srv_rights, 0x151d),
            (0, 0),
        )?;

        let mut inst = Self {
            state: ISLAND_STATE_RUNNING,
            proc_handle,
            island_pid,
            generation,
            mac_isolated: false,
            drv_ep,
            drv_broker_cap,
            drv_sup_cap,
            sq_notif,
            cq_notif,
            ring_cap: 0,
            park_mode: 0,
        };

        let (b0, _, _) = sys_ipc_call_with_grant(
            drv_broker_cap,
            MOCK_OP_BIND_NOTIF,
            [0, 0, 0, 0],
            sq_notif,
            RIGHT_READ | RIGHT_WAIT,
            0,
            0,
        )?;
        let (b1, _, _) = sys_ipc_call_with_grant(
            drv_broker_cap,
            MOCK_OP_BIND_NOTIF,
            [1, 0, 0, 0],
            cq_notif,
            RIGHT_READ | RIGHT_SIGNAL,
            0,
            0,
        )?;
        if msg_tag_label(b0) != IPC_OP_PONG || msg_tag_label(b1) != IPC_OP_PONG {
            inst.force_reap_and_revoke();
            return Err(EBADCAP);
        }

        let (itag, granted_ring, imrs) = sys_ipc_call_with_grant(
            drv_broker_cap,
            MOCK_OP_INIT_ISLAND,
            [generation, 0, 0, 0],
            0,
            0,
            0,
            0,
        )?;
        if msg_tag_label(itag) != IPC_OP_PONG || granted_ring == 0 || imrs[0] != generation {
            inst.force_reap_and_revoke();
            return Err(EBADCAP);
        }
        inst.ring_cap = granted_ring;
        inst.mac_isolated = imrs[1] != 0;
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
            let _ = sys_cap_revoke(self.ring_cap, 1);
            self.ring_cap = 0;
        }
        if self.drv_broker_cap != 0 {
            let _ = sys_cap_revoke(self.drv_broker_cap, 1);
            self.drv_broker_cap = 0;
        }
        if self.drv_sup_cap != 0 {
            let _ = sys_cap_revoke(self.drv_sup_cap, 1);
            self.drv_sup_cap = 0;
        }
        if self.drv_ep != 0 {
            let _ = sys_cap_revoke(self.drv_ep, 2);
            self.drv_ep = 0;
        }
        if self.sq_notif != 0 {
            let _ = sys_cap_revoke(self.sq_notif, 2);
            self.sq_notif = 0;
        }
        if self.cq_notif != 0 {
            let _ = sys_cap_revoke(self.cq_notif, 2);
            self.cq_notif = 0;
        }
        self.park_mode = 0;
    }

    fn force_reap_and_revoke(&mut self) {
        if self.proc_handle != 0 {
            let _ = sys_proc_wait(self.proc_handle, 1, 0);
            self.proc_handle = 0;
        }
        self.revoke_generation_caps();
        self.state = ISLAND_STATE_STOPPED;
    }

    fn check_async_crash_and_recover(
        &mut self,
        mock_mod_cap: u64,
        restarts: &mut u64,
    ) -> Result<(), isize> {
        if self.state != ISLAND_STATE_RUNNING || self.proc_handle == 0 {
            return Ok(());
        }
        if let Ok((_w_kind, _w_detail)) = sys_proc_wait(self.proc_handle, 1, 0) {
            self.proc_handle = 0;
            self.state = ISLAND_STATE_FAULTED;
            self.revoke_generation_caps();
            let next_gen = self.generation.wrapping_add(1);
            *self = Self::spawn(mock_mod_cap, next_gen)?;
            *restarts = restarts.wrapping_add(1);
        }
        Ok(())
    }

    fn shutdown_clean(&mut self) {
        if self.state == ISLAND_STATE_RUNNING && self.drv_sup_cap != 0 && self.proc_handle != 0 {
            self.wake_if_ring_parked();
            let _ = sys_ipc_call_inline(
                self.drv_sup_cap,
                pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
                [0; 4],
                0,
            );
            let _ = sys_proc_wait(self.proc_handle, 0, 0);
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

    let mut island = match IslandInstance::spawn(mock_drv_mod_cap, 1) {
        Ok(i) => i,
        Err(_) => sys_exit(186),
    };

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-BROKER] Ready in CPL3 RootSystem [0x0008] island_pid=");
    lb.push_u64(island.island_pid);
    lb.push_bytes(b" gen=1");
    log_line(&lb);

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

        let _ = island.check_async_crash_and_recover(mock_drv_mod_cap, &mut restarts);

        match op {
            BROKER_OP_QUERY_STATE => {
                let packed_rej =
                    (denied_ops & 0xffff_ffff) | ((stale_rejects & 0xffff_ffff) << 32);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        broker_ep,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [island.state, island.generation, restarts, packed_rej],
                    );
                }
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
                match sys_ipc_call_inline(
                    island.drv_broker_cap,
                    pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
                    [mrs[0], mrs[1], mrs[2], island.generation],
                    0,
                ) {
                    Ok((rtag, _, rmrs)) if msg_tag_label(rtag) == IPC_OP_PONG => {
                        forwarded_ops = forwarded_ops.wrapping_add(1);
                        if tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                broker_ep,
                                tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                                [rmrs[0], rmrs[1], island.generation, forwarded_ops],
                            );
                        }
                    }
                    _ => {
                        let _ = island.check_async_crash_and_recover(mock_drv_mod_cap, &mut restarts);
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
                let target_mode = if mrs[0] != 0 { 1 } else { 0 };
                island.wake_if_ring_parked();
                match sys_ipc_call_inline(
                    island.drv_broker_cap,
                    pack_msg_tag(MOCK_OP_SET_PARK_MODE, 0, 1, 0),
                    [target_mode, 0, 0, 0],
                    0,
                ) {
                    Ok((rtag, _, rmrs)) if msg_tag_label(rtag) == IPC_OP_PONG => {
                        island.park_mode = rmrs[0];
                        if tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                broker_ep,
                                tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                                [island.park_mode, island.generation, 0, 0],
                            );
                        }
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
                    0,
                );
                let (w_kind, w_detail) = match sys_proc_wait(island.proc_handle, 0, 0) {
                    Ok(v) => v,
                    Err(e) => {
                        reply_err(broker_ep, tok, IPC_OP_ERR_INVAL, e, 0);
                        continue;
                    }
                };
                island.proc_handle = 0;
                island.state = ISLAND_STATE_FAULTED;
                let fault_vec = if w_kind == 1 { w_detail & 0xff } else { 0 };

                island.revoke_generation_caps();
                let next_gen = island.generation.wrapping_add(1);
                island = match IslandInstance::spawn(mock_drv_mod_cap, next_gen) {
                    Ok(i) => i,
                    Err(_) => sys_exit(187),
                };
                // Readiness probe on freshly restarted generation before declaring recovery complete.
                let probe_ok = sys_ipc_call_inline(
                    island.drv_broker_cap,
                    pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
                    [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, island.generation],
                    0,
                )
                .is_ok_and(|(rt, _, _)| msg_tag_label(rt) == IPC_OP_PONG);
                if !probe_ok {
                    sys_exit(188);
                }
                let dt_cyc = rdtsc_user().saturating_sub(t0);
                restarts = restarts.wrapping_add(1);

                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        broker_ep,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [island.generation, dt_cyc, fault_vec, restarts],
                    );
                }
            }
            BROKER_OP_SHUTDOWN => {
                if !is_admin {
                    denied_ops = denied_ops.wrapping_add(1);
                    reply_err(broker_ep, tok, IPC_OP_ERR_PERM, EPERM, denied_ops);
                    continue;
                }
                island.shutdown_clean();
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

                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        broker_ep,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [forwarded_ops, restarts, denied_ops, stale_rejects],
                    );
                }
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
    island.shutdown_clean();
    sys_exit(0);
}
