// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone 2C-0 Adaptive Driver Islands PoC E2E Lifecycle Validator & Benchmark Suite
//! (`ADR-016` — `session_mode == 25` & `session_mode == 26`).

use crate::init_legacy::{domain_spec, log_line};
use crate::ioring::IoRingSharedPage;
use crate::island_proto::{
    mock_island_transform, pack_mock_sqe, LatencySamples64, BADGE_BROKER_ADMIN,
    BADGE_BROKER_CLIENT, BADGE_BROKER_ROGUE, BROKER_OP_ACQUIRE_EP, BROKER_OP_ACQUIRE_RING,
    BROKER_OP_CRASH_AND_RECOVER, BROKER_OP_FORWARD_TRANSFORM, BROKER_OP_QUERY_STATE,
    BROKER_OP_SET_PARK_MODE, BROKER_OP_SHUTDOWN, COMP_ID_DEV_BROKER, COMP_ID_MOCK_ISLAND,
    IPC_OP_ERR_STALE_GEN, ISLAND_DOORBELL_RING, MOCK_OP_TRANSFORM, MOCK_OP_WAKE_PROBE,
    POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1,
};
use crate::shell::LineBuilder;
use crate::vfs_proto::{vfs_create_file, vfs_read_file, vfs_sync, vfs_write_small};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_derive, sys_cap_query, sys_cap_revoke,
    sys_ep_create, sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create,
    sys_notify_signal, sys_notify_wait, sys_power_query, sys_proc_list, sys_proc_spawn,
    sys_proc_wait, sys_sysinfo, sys_vspace_map, RootInitBootInfo, UserCapInfo, UserPowerInfo,
    UserProcInfo, UserSysInfo, EBADCAP, EEXIST, EREVOKED, IPC_OP_ERR_PERM, IPC_OP_PONG, RIGHT_CALL,
    RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_SPAWN, VAULT_OP_STATUS,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;
const CLIENT_ISLAND_RING_VADDR: u64 = 0x2500_0000;
const GUARD_FILE_PATH: &[u8] = b"/persist/island-guard.txt";
const GUARD_FILE_TEXT: &[u8] = b"island-guard-intact";

struct BrokerHarness {
    proc_handle: u64,
    broker_ep: u64,
    admin_cap: u64,
    client_cap: u64,
    rogue_cap: u64,
    unbadged_cap: u64,
}

impl BrokerHarness {
    fn spawn(boot: &RootInitBootInfo) -> Result<Self, i32> {
        if boot.broker_mod_cap == 0 || boot.mock_drv_mod_cap == 0 {
            return Err(220);
        }
        let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
        let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

        let broker_ep = sys_ep_create(ipc_spec).map_err(|_| 221)?;
        let admin_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_ADMIN).map_err(|_| 222)?;
        let client_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_CLIENT).map_err(|_| 222)?;
        let rogue_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_ROGUE).map_err(|_| 222)?;
        let unbadged_cap = sys_cap_derive(broker_ep, cli_rights, 0, 0).map_err(|_| 222)?;

        let (proc_handle, _) = sys_proc_spawn(
            boot.broker_mod_cap,
            1,
            COMP_ID_DEV_BROKER,
            (broker_ep, srv_rights, 0x0808),
            (boot.mock_drv_mod_cap, RIGHT_READ | RIGHT_SPAWN),
        )
        .map_err(|_| 223)?;

        Ok(Self {
            proc_handle,
            broker_ep,
            admin_cap,
            client_cap,
            rogue_cap,
            unbadged_cap,
        })
    }

    fn shutdown_and_reclaim(self) -> Result<(), i32> {
        let (stag, _, _) = sys_ipc_call_inline(
            self.admin_cap,
            pack_msg_tag(BROKER_OP_SHUTDOWN, 0, 1, 0),
            [0; 4],
            0,
        )
        .map_err(|_| 238)?;
        if msg_tag_label(stag) != IPC_OP_PONG {
            return Err(239);
        }
        let _ = sys_proc_wait(self.proc_handle, 0, 0);
        for c in [
            self.admin_cap,
            self.client_cap,
            self.rogue_cap,
            self.unbadged_cap,
        ] {
            let _ = sys_cap_revoke(c, 1);
        }
        let _ = sys_cap_revoke(self.broker_ep, 2);
        Ok(())
    }
}

fn acquire_client_ring(
    client_cap: u64,
    ring_vaddr: u64,
) -> Result<(u64, u64, u64, &'static IoRingSharedPage), i32> {
    let (t0, ring_cap, _) =
        sys_ipc_call_with_grant(client_cap, BROKER_OP_ACQUIRE_RING, [0, 0, 0, 0], 0, 0, 0, 0)
            .map_err(|_| 226)?;
    let (t1, sq_notif, _) =
        sys_ipc_call_with_grant(client_cap, BROKER_OP_ACQUIRE_RING, [1, 0, 0, 0], 0, 0, 0, 0)
            .map_err(|_| 226)?;
    let (t2, cq_notif, _) =
        sys_ipc_call_with_grant(client_cap, BROKER_OP_ACQUIRE_RING, [2, 0, 0, 0], 0, 0, 0, 0)
            .map_err(|_| 226)?;
    if msg_tag_label(t0) != IPC_OP_PONG
        || msg_tag_label(t1) != IPC_OP_PONG
        || msg_tag_label(t2) != IPC_OP_PONG
        || ring_cap == 0
        || sq_notif == 0
        || cq_notif == 0
    {
        return Err(227);
    }
    sys_vspace_map(ring_cap, ring_vaddr, VSPACE_MAP_OP_4K_RW).map_err(|_| 228)?;
    // SAFETY: `ring_vaddr` maps the 4 KiB `IoRingSharedPage` initialized by `dry-mock-drv`.
    let ring = unsafe { &*(ring_vaddr as *const IoRingSharedPage) };
    Ok((ring_cap, sq_notif, cq_notif, ring))
}

/// `session_mode == 25` (`desktop4=1 island_poc=1`): End-to-end verification of `Device Broker` +
/// `Mock Driver Island` lifecycle, capability isolation, `#UD` crash recovery, stale capability
/// rejection, and zero interference with `dry-blk` / `dry-vfs` / `dry-vault`.
pub fn run_island_poc_lifecycle_audit(
    boot: &RootInitBootInfo,
    vfs_sup_cap: u64,
    vault_sup_cap: u64,
) -> Result<(), i32> {
    if vfs_sup_cap != 0 {
        match vfs_create_file(vfs_sup_cap, GUARD_FILE_PATH, 0) {
            Ok(_) | Err(EEXIST) => {}
            Err(_) => return Err(224),
        }
        vfs_write_small(vfs_sup_cap, GUARD_FILE_PATH, GUARD_FILE_TEXT, false).map_err(|_| 224)?;
        let _ = vfs_sync(vfs_sup_cap).map_err(|_| 224)?;
    }

    let bh = BrokerHarness::spawn(boot)?;

    // 1. Unauthorized badge, privilege escalation, & control-plane-only checks (`-EPERM`).
    let mut unauth_denied = 0u64;
    if let Ok((rt, _, _)) = sys_ipc_call_inline(
        bh.rogue_cap,
        pack_msg_tag(BROKER_OP_QUERY_STATE, 0, 1, 0),
        [0; 4],
        0,
    ) {
        if msg_tag_label(rt) == IPC_OP_ERR_PERM {
            unauth_denied += 1;
        }
    }
    if let Ok((rt, _, _)) =
        sys_ipc_call_with_grant(bh.unbadged_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(rt) == IPC_OP_ERR_PERM {
            unauth_denied += 1;
        }
    }
    if let Ok((rt, _, _)) = sys_ipc_call_inline(
        bh.client_cap,
        pack_msg_tag(BROKER_OP_CRASH_AND_RECOVER, 0, 1, 0),
        [0; 4],
        0,
    ) {
        if msg_tag_label(rt) == IPC_OP_ERR_PERM {
            unauth_denied += 1;
        }
    }
    // Normal client MUST NOT route data traffic through `Device Broker` (`ADR-016` `-EPERM`).
    if let Ok((rt, _, _)) = sys_ipc_call_inline(
        bh.client_cap,
        pack_msg_tag(BROKER_OP_FORWARD_TRANSFORM, 0, 4, 0),
        [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 1],
        0,
    ) {
        if msg_tag_label(rt) == IPC_OP_ERR_PERM {
            unauth_denied += 1;
        }
    }
    // `AUD-U9`: non-admin client MUST NOT change global park mode (`-EPERM`).
    if let Ok((rt, _, _)) = sys_ipc_call_inline(
        bh.client_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    ) {
        if msg_tag_label(rt) == IPC_OP_ERR_PERM {
            unauth_denied += 1;
        }
    }
    if unauth_denied < 5 {
        return Err(225);
    }

    // 2. `gen = 1` direct leased `Endpoint` + batched `IoRingSharedPage` data-plane verification.
    let exp_gen1 = mock_island_transform(POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 1);
    let (etag, gen1_drv_ep, emrs) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0)
            .map_err(|_| 226)?;
    let mac_isolated = emrs[1] != 0;
    if msg_tag_label(etag) != IPC_OP_PONG || gen1_drv_ep == 0 || emrs[0] != 1 || !mac_isolated {
        return Err(226);
    }
    let (dtag, _, dmrs) = sys_ipc_call_inline(
        gen1_drv_ep,
        pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
        [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 1],
        0,
    )
    .map_err(|_| 226)?;
    if msg_tag_label(dtag) != IPC_OP_PONG || dmrs[0] != exp_gen1 {
        return Err(226);
    }
    let gen1_digest = dmrs[0];

    let (gen1_ring_cap, gen1_sq_notif, gen1_cq_notif, ring) =
        acquire_client_ring(bh.client_cap, CLIENT_ISLAND_RING_VADDR)?;
    let (ptag, _, _) = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    )
    .map_err(|_| 229)?;
    if msg_tag_label(ptag) != IPC_OP_PONG {
        return Err(229);
    }

    let mut ring_batch_ok = true;
    for i in 0..8u64 {
        if !ring.push_sqe(pack_mock_sqe(POC_TEST_SEED, POC_TEST_WORD0 ^ i, POC_TEST_WORD1)) {
            ring_batch_ok = false;
        }
    }
    sys_notify_signal(gen1_sq_notif, ISLAND_DOORBELL_RING).map_err(|_| 229)?;
    let mut completed = 0u64;
    while completed < 8 {
        let _ = sys_notify_wait(gen1_cq_notif, 0, 0).map_err(|_| 229)?;
        while let Some(cqe) = ring.pop_cqe() {
            let exp_i =
                mock_island_transform(POC_TEST_SEED, POC_TEST_WORD0 ^ completed, POC_TEST_WORD1, 1);
            if cqe.status != 0 || cqe.user_tag != exp_i || cqe.bytes_done != 1 {
                ring_batch_ok = false;
            }
            completed += 1;
        }
    }
    if !ring_batch_ok {
        return Err(230);
    }

    // 3. Controlled `#UD` crash (`vec=6`) + supervisor/broker restart (`gen = 1 -> gen = 2`)!
    let (ctag, _, cmrs) = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_CRASH_AND_RECOVER, 0, 1, 0),
        [0; 4],
        0,
    )
    .map_err(|_| 231)?;
    let new_gen = cmrs[0];
    let fault_vec = cmrs[2];
    let restarts = cmrs[3];
    if msg_tag_label(ctag) != IPC_OP_PONG || new_gen != 2 || fault_vec != 6 || restarts < 1 {
        return Err(232);
    }

    // 4. Verify ALL prior-generation capabilities (`drv_ep`, `sq_notif`, `cq_notif`, `ring_cap`)
    //    and stale generation tokens (`expected_gen = 1`) are deterministically rejected!
    let stale_ep_badcap = matches!(
        sys_ipc_call_inline(
            gen1_drv_ep,
            pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
            [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 1],
            0,
        ),
        Err(EREVOKED | EBADCAP)
    );
    let stale_notif_badcap = matches!(
        sys_notify_signal(gen1_sq_notif, ISLAND_DOORBELL_RING),
        Err(EREVOKED | EBADCAP)
    );
    let mut cinfo = UserCapInfo::default();
    let stale_cq_badcap = matches!(sys_cap_query(gen1_cq_notif, &mut cinfo), EREVOKED | EBADCAP);
    let stale_ring_badcap = matches!(sys_cap_query(gen1_ring_cap, &mut cinfo), EREVOKED | EBADCAP);

    // 5. Lease fresh `gen = 2` direct `Endpoint`, verify stale generation token (`expected_gen = 1`)
    //    is rejected on both data plane & admin diagnostic probe, then verify `gen = 2` transform!
    let (e2tag, gen2_drv_ep, e2mrs) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0)
            .map_err(|_| 233)?;
    if msg_tag_label(e2tag) != IPC_OP_PONG || gen2_drv_ep == 0 || e2mrs[0] != 2 {
        return Err(233);
    }
    let (s_dtag, _, s_dmrs) = sys_ipc_call_inline(
        gen2_drv_ep,
        pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
        [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 1], // stale `expected_gen = 1`!
        0,
    )
    .map_err(|_| 233)?;
    let (s_btag, _, s_bmrs) = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_FORWARD_TRANSFORM, 0, 4, 0),
        [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 1], // admin diag stale `expected_gen = 1`!
        0,
    )
    .map_err(|_| 233)?;
    let stale_gen_revoked = msg_tag_label(s_dtag) == IPC_OP_ERR_STALE_GEN
        && s_dmrs[0] == (-EREVOKED) as u64
        && msg_tag_label(s_btag) == IPC_OP_ERR_STALE_GEN
        && s_bmrs[0] == (-EREVOKED) as u64;

    let (d2tag, _, d2mrs) = sys_ipc_call_inline(
        gen2_drv_ep,
        pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
        [POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 2],
        0,
    )
    .map_err(|_| 234)?;
    let exp_gen2 = mock_island_transform(POC_TEST_SEED, POC_TEST_WORD0, POC_TEST_WORD1, 2);
    if msg_tag_label(d2tag) != IPC_OP_PONG || d2mrs[0] != exp_gen2 {
        return Err(235);
    }
    let gen2_digest = d2mrs[0];
    let _ = sys_cap_revoke(gen2_drv_ep, 1);

    bh.shutdown_and_reclaim()?;

    let mut storage_vault_intact = true;
    if vfs_sup_cap != 0 {
        let mut rbuf = [0u8; 32];
        match vfs_read_file(vfs_sup_cap, GUARD_FILE_PATH, 0, &mut rbuf) {
            Ok((n, _)) if &rbuf[..n] == GUARD_FILE_TEXT => {}
            _ => storage_vault_intact = false,
        }
    }
    if vault_sup_cap != 0 {
        let v_ok = sys_ipc_call_inline(
            vault_sup_cap,
            pack_msg_tag(VAULT_OP_STATUS, 0, 1, 0),
            [0; 4],
            0,
        )
        .is_ok_and(|(vt, _, _)| msg_tag_label(vt) == IPC_OP_PONG);
        if !v_ok {
            storage_vault_intact = false;
        }
    }

    if !stale_ep_badcap
        || !stale_notif_badcap
        || !stale_cq_badcap
        || !stale_ring_badcap
        || !stale_gen_revoked
        || !storage_vault_intact
    {
        return Err(236);
    }

    let mut dlb = LineBuilder::new();
    dlb.push_bytes(b"[DRY-ISLAND-POC-DIGEST] gen1_digest=");
    dlb.push_u64(gen1_digest);
    dlb.push_bytes(b" gen2_digest=");
    dlb.push_u64(gen2_digest);
    log_line(&dlb);

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-ISLAND-POC] PASS: mac=");
    lb.push_u64(u64::from(mac_isolated));
    lb.push_bytes(b" unauth=");
    lb.push_u64(unauth_denied);
    lb.push_bytes(b" vec=");
    lb.push_u64(fault_vec);
    lb.push_bytes(b" restarts=");
    lb.push_u64(restarts);
    lb.push_bytes(b" ep_rev=");
    lb.push_u64(u64::from(stale_ep_badcap));
    lb.push_bytes(b" ntf_rev=");
    lb.push_u64(u64::from(stale_notif_badcap));
    lb.push_bytes(b" cq_rev=");
    lb.push_u64(u64::from(stale_cq_badcap));
    lb.push_bytes(b" rng_rev=");
    lb.push_u64(u64::from(stale_ring_badcap));
    lb.push_bytes(b" gen_rev=");
    lb.push_u64(u64::from(stale_gen_revoked));
    lb.push_bytes(b" batch=");
    lb.push_u64(u64::from(ring_batch_ok));
    lb.push_bytes(b" sv_ok=");
    lb.push_u64(u64::from(storage_vault_intact));
    log_line(&lb);
    Ok(())
}

fn find_comp_proc(procs: &[UserProcInfo], count: usize, comp_id: u16) -> UserProcInfo {
    let mut i = 0usize;
    while i < count {
        if procs[i].compartment_id == comp_id {
            return procs[i];
        }
        i += 1;
    }
    UserProcInfo::default()
}

/// `session_mode == 26` (`desktop4=1 island_bench=1`): Honest QEMU/KVM latency, wakeup, recovery,
/// idle CPU, PMM memory footprint, and `IoRing` batch amortization measurements (`AŞAMA 2.3`).
pub fn run_island_poc_benchmarks(boot: &RootInitBootInfo) -> Result<(), i32> {
    let mut sys0 = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys0);
    let tsc_mhz = sys0.tsc_mhz.max(1000);
    let free_before = sys0.free_frames_4k as u64;

    let sleep_notif = sys_notify_create(domain_spec(0, DESKTOP_IPC_COMP_ID)).map_err(|_| 240)?;
    let bh = BrokerHarness::spawn(boot)?;

    let (_, drv_ep, _) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0)
            .map_err(|_| 241)?;
    let (ring_cap, sq_notif, cq_notif, ring) =
        acquire_client_ring(bh.client_cap, CLIENT_ISLAND_RING_VADDR)?;

    // Snapshot active PMM & per-process `VSpace` page/PT footprint while Broker + Island + IoRing are live.
    let mut sys_act = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys_act);
    let free_active = sys_act.free_frames_4k as u64;
    let mut procs_act = [UserProcInfo::default(); 24];
    let (n_act, _) = sys_proc_list(&mut procs_act).unwrap_or((0, 0));
    let brk_info = find_comp_proc(&procs_act, n_act, COMP_ID_DEV_BROKER);
    let isl_info = find_comp_proc(&procs_act, n_act, COMP_ID_MOCK_ISLAND);

    // 1. Direct leased vs Brokered synchronous `Endpoint` request/reply (`8` warmup + `64` samples).
    let mut direct_lat = LatencySamples64::new();
    for i in 0..72u64 {
        let t0 = rdtsc_user();
        let _ = sys_ipc_call_inline(
            drv_ep,
            pack_msg_tag(MOCK_OP_TRANSFORM, 0, 4, 0),
            [POC_TEST_SEED, POC_TEST_WORD0 ^ i, POC_TEST_WORD1, 1],
            0,
        )
        .map_err(|_| 242)?;
        let dt = rdtsc_user().saturating_sub(t0);
        if i >= 8 {
            direct_lat.push(dt);
        }
    }

    // Admin-only A/B diagnostic reference loop (`BADGE_BROKER_ADMIN`) measuring why normal data
    // traffic is strictly banned from traversing `Device Broker` (`2x` 2-hop IPC penalty).
    let mut broker_lat = LatencySamples64::new();
    for i in 0..72u64 {
        let t0 = rdtsc_user();
        let _ = sys_ipc_call_inline(
            bh.admin_cap,
            pack_msg_tag(BROKER_OP_FORWARD_TRANSFORM, 0, 4, 0),
            [POC_TEST_SEED, POC_TEST_WORD0 ^ i, POC_TEST_WORD1, 1],
            0,
        )
        .map_err(|_| 243)?;
        let dt = rdtsc_user().saturating_sub(t0);
        if i >= 8 {
            broker_lat.push(dt);
        }
    }
    let (_, d_p50, d_p95, d_p99, _) = direct_lat.summary();
    let (_, b_p50, b_p95, b_p99, _) = broker_lat.summary();

    // 2. Idle-to-active wakeup latency (`32` parked `HLT` wakeups).
    let mut wake_1way = LatencySamples64::new();
    let mut wake_rtt = LatencySamples64::new();
    let mut hlt_verified = 0u64;
    for _ in 0..32u64 {
        let mut pw0 = UserPowerInfo::default();
        let _ = sys_power_query(&mut pw0);
        let _ = sys_notify_wait(sleep_notif, 0, 1_500u64.saturating_mul(tsc_mhz));
        let mut pw1 = UserPowerInfo::default();
        let _ = sys_power_query(&mut pw1);
        if pw1.idle_hlt_calls > pw0.idle_hlt_calls {
            hlt_verified += 1;
        }
        let t0 = rdtsc_user();
        let (_, _, wmrs) = sys_ipc_call_inline(
            drv_ep,
            pack_msg_tag(MOCK_OP_WAKE_PROBE, 0, 1, 0),
            [t0, 0, 0, 0],
            0,
        )
        .map_err(|_| 244)?;
        let t1 = rdtsc_user();
        wake_1way.push(wmrs[0].saturating_sub(t0).max(1));
        wake_rtt.push(t1.saturating_sub(t0).max(1));
    }
    let (_, w1_p50, w1_p95, w1_p99, _) = wake_1way.summary();
    let (_, wr_p50, wr_p95, wr_p99, _) = wake_rtt.summary();

    // 3. Quiescent idle CPU & per-compartment syscall delta over `100 ms`.
    let mut p0 = [UserProcInfo::default(); 24];
    let (np0, _) = sys_proc_list(&mut p0).unwrap_or((0, 0));
    let brk_s0 = find_comp_proc(&p0, np0, COMP_ID_DEV_BROKER).syscall_entries;
    let isl_s0 = find_comp_proc(&p0, np0, COMP_ID_MOCK_ISLAND).syscall_entries;
    let mut ipw0 = UserPowerInfo::default();
    let _ = sys_power_query(&mut ipw0);

    let _ = sys_notify_wait(sleep_notif, 0, 100_000u64.saturating_mul(tsc_mhz));

    let mut ipw1 = UserPowerInfo::default();
    let _ = sys_power_query(&mut ipw1);
    let mut p1 = [UserProcInfo::default(); 24];
    let (np1, _) = sys_proc_list(&mut p1).unwrap_or((0, 0));
    let brk_s1 = find_comp_proc(&p1, np1, COMP_ID_DEV_BROKER).syscall_entries;
    let isl_s1 = find_comp_proc(&p1, np1, COMP_ID_MOCK_ISLAND).syscall_entries;

    let broker_idle_syscalls = brk_s1.saturating_sub(brk_s0);
    let island_idle_syscalls = isl_s1.saturating_sub(isl_s0);
    let hlt_calls_delta = ipw1.idle_hlt_calls.saturating_sub(ipw0.idle_hlt_calls);
    let hlt_tsc_delta = ipw1.idle_hlt_tsc.saturating_sub(ipw0.idle_hlt_tsc);
    let elapsed_tsc_delta = ipw1.elapsed_tsc.saturating_sub(ipw0.elapsed_tsc).max(1);

    // 4. `IoRingSharedPage` unbatched (`batch=1`) vs batched (`batch=16`, `batch=32`) `cyc/op`.
    let (ptag, _, _) = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    )
    .map_err(|_| 245)?;
    if msg_tag_label(ptag) != IPC_OP_PONG {
        return Err(245);
    }

    let mut ring_res = [(0u64, 0u64, 0u64); 3];
    for (b_idx, batch) in [1usize, 16, 32].into_iter().enumerate() {
        let mut lat = LatencySamples64::new();
        for iter in 0..68u64 {
            let t0 = rdtsc_user();
            for j in 0..(batch as u64) {
                let _ = ring.push_sqe(pack_mock_sqe(POC_TEST_SEED, iter ^ j, POC_TEST_WORD1));
            }
            sys_notify_signal(sq_notif, ISLAND_DOORBELL_RING).map_err(|_| 245)?;
            let mut got = 0usize;
            while got < batch {
                let _ = sys_notify_wait(cq_notif, 0, 0).map_err(|_| 245)?;
                while ring.pop_cqe().is_some() {
                    got += 1;
                }
            }
            let dt_per_op = rdtsc_user().saturating_sub(t0) / (batch as u64);
            if iter >= 4 {
                lat.push(dt_per_op.max(1));
            }
        }
        let (_, p50, p95, p99, _) = lat.summary();
        ring_res[b_idx] = (p50, p95, p99);
    }

    let _ = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [0, 0, 0, 0],
        0,
    );
    let _ = sys_vspace_map(ring_cap, CLIENT_ISLAND_RING_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    for c in [drv_ep, ring_cap, sq_notif, cq_notif] {
        let _ = sys_cap_revoke(c, 1);
    }

    // 5. `#UD` crash-to-recovery latency (`4` consecutive crash-and-restart cycles).
    let mut rec_lat = LatencySamples64::new();
    for _ in 0..4u64 {
        let (ctag, _, cmrs) = sys_ipc_call_inline(
            bh.admin_cap,
            pack_msg_tag(BROKER_OP_CRASH_AND_RECOVER, 0, 1, 0),
            [0; 4],
            0,
        )
        .map_err(|_| 246)?;
        if msg_tag_label(ctag) != IPC_OP_PONG || cmrs[2] != 6 {
            return Err(246);
        }
        rec_lat.push(cmrs[1]);
    }
    let (rec_min, rec_p50, _, _, rec_max) = rec_lat.summary();

    // 6. Clean teardown & exact PMM 4 KiB frame conservation check.
    bh.shutdown_and_reclaim()?;
    let _ = sys_cap_revoke(sleep_notif, 2);

    let mut sys_end = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys_end);
    let free_after = sys_end.free_frames_4k as u64;
    // Note: `free_before` was recorded BEFORE `sleep_notif` (`0` frames) and BEFORE `BrokerHarness::spawn`.
    let pmm_active_delta_4k = free_before.saturating_sub(free_active);
    let pmm_leak_after_teardown_4k = free_before.saturating_sub(free_after);

    let mut l1 = LineBuilder::new();
    l1.push_bytes(b"[DRY-ISLAND-BENCH-IPC] mhz=");
    l1.push_u64(tsc_mhz);
    l1.push_bytes(b" d_p50=");
    l1.push_u64(d_p50);
    l1.push_bytes(b" d_p95=");
    l1.push_u64(d_p95);
    l1.push_bytes(b" d_p99=");
    l1.push_u64(d_p99);
    l1.push_bytes(b" b_p50=");
    l1.push_u64(b_p50);
    l1.push_bytes(b" b_p95=");
    l1.push_u64(b_p95);
    l1.push_bytes(b" b_p99=");
    l1.push_u64(b_p99);
    log_line(&l1);

    let mut l2 = LineBuilder::new();
    l2.push_bytes(b"[DRY-ISLAND-BENCH-WAKE] w1_p50=");
    l2.push_u64(w1_p50);
    l2.push_bytes(b" w1_p95=");
    l2.push_u64(w1_p95);
    l2.push_bytes(b" w1_p99=");
    l2.push_u64(w1_p99);
    l2.push_bytes(b" wr_p50=");
    l2.push_u64(wr_p50);
    l2.push_bytes(b" wr_p95=");
    l2.push_u64(wr_p95);
    l2.push_bytes(b" wr_p99=");
    l2.push_u64(wr_p99);
    l2.push_bytes(b" hlt_ok=");
    l2.push_u64(hlt_verified);
    log_line(&l2);

    let mut l3 = LineBuilder::new();
    l3.push_bytes(b"[DRY-ISLAND-BENCH-RECOVER] runs=4 min_cyc=");
    l3.push_u64(rec_min);
    l3.push_bytes(b" p50_cyc=");
    l3.push_u64(rec_p50);
    l3.push_bytes(b" max_cyc=");
    l3.push_u64(rec_max);
    l3.push_bytes(b" p50_us=");
    l3.push_u64(rec_p50 / tsc_mhz);
    log_line(&l3);

    let mut l4 = LineBuilder::new();
    l4.push_bytes(b"[DRY-ISLAND-BENCH-IDLE] brk_sys=");
    l4.push_u64(broker_idle_syscalls);
    l4.push_bytes(b" isl_sys=");
    l4.push_u64(island_idle_syscalls);
    l4.push_bytes(b" hlt_calls=");
    l4.push_u64(hlt_calls_delta);
    l4.push_bytes(b" hlt_tsc=");
    l4.push_u64(hlt_tsc_delta);
    l4.push_bytes(b" elap_tsc=");
    l4.push_u64(elapsed_tsc_delta);
    log_line(&l4);

    let mut l5 = LineBuilder::new();
    l5.push_bytes(b"[DRY-ISLAND-BENCH-MEM] brk_pg=");
    l5.push_u64(brk_info.mapped_pages_4k as u64);
    l5.push_bytes(b" brk_pt=");
    l5.push_u64(brk_info.pt_frames_4k as u64);
    l5.push_bytes(b" isl_pg=");
    l5.push_u64(isl_info.mapped_pages_4k as u64);
    l5.push_bytes(b" isl_pt=");
    l5.push_u64(isl_info.pt_frames_4k as u64);
    l5.push_bytes(b" pmm_act=");
    l5.push_u64(pmm_active_delta_4k);
    l5.push_bytes(b" pmm_leak=");
    l5.push_u64(pmm_leak_after_teardown_4k);
    log_line(&l5);

    let mut l6 = LineBuilder::new();
    l6.push_bytes(b"[DRY-ISLAND-BENCH-RING] b1_p50=");
    l6.push_u64(ring_res[0].0);
    l6.push_bytes(b" b1_p99=");
    l6.push_u64(ring_res[0].2);
    l6.push_bytes(b" b16_p50=");
    l6.push_u64(ring_res[1].0);
    l6.push_bytes(b" b16_p99=");
    l6.push_u64(ring_res[1].2);
    l6.push_bytes(b" b32_p50=");
    l6.push_u64(ring_res[2].0);
    l6.push_bytes(b" b32_p99=");
    l6.push_u64(ring_res[2].2);
    log_line(&l6);

    Ok(())
}
