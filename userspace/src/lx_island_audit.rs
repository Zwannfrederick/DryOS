// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone 2C-1 Genuine Linux `v6.18` `virtio-rng.c` Driver Island E2E Audit & Benchmark
//! (`ADR-005`, `ADR-016` — `session_mode == 27` via `desktop4=1 lx_island=1`).

use crate::abi_ext::{
    sys_dev_map, RootInitBootExtInfo, UserVirtioPciInfo, BOOT_EXT_INFO_OFFSET, ROOT_BOOT_EXT_MAGIC,
};
use crate::broker_lifecycle::{
    LifecycleStep, CONTRACT_FLAG_ASSUME_IGPU_PASSTHROUGH, CONTRACT_FLAG_REQUIRE_RING0_KO,
    DRVCTL_CONTRACT_FAULT_EP_REQUIRED, DRVCTL_CONTRACT_IOMMU_ISOLATED,
    DRVCTL_CONTRACT_NO_IGPU_PASSTHROUGH, DRVCTL_CONTRACT_RING0_KO_FORBIDDEN, DRVCTL_CONTRACT_VALID,
};
use crate::init_legacy::{domain_spec, log_line};
use crate::ioring::IoRingSharedPage;
use crate::island_proto::{
    LatencySamples64, BADGE_BROKER_ADMIN, BADGE_BROKER_CLIENT, BADGE_BROKER_ROGUE,
    BROKER_OP_ACQUIRE_EP, BROKER_OP_ACQUIRE_RING, BROKER_OP_CRASH_AND_RECOVER,
    BROKER_OP_FORWARD_TRANSFORM, BROKER_OP_SET_PARK_MODE, BROKER_OP_SHUTDOWN,
    COMP_ID_DEV_BROKER, IPC_OP_ERR_STALE_GEN, ISLAND_DOORBELL_CTL,
};
use crate::lx_compat_meta::find_boot_pci_managed;
use crate::lx_drv_proto::{
    pack_net_poll_sqe, pack_rng_sqe, BADGE_BROKER_LX_MODE, BROKER_OP_BIND_PCI_DEV,
    BROKER_OP_DRVCTL_QUERY, COMP_ID_LX_NET_8139CP_ISLAND, COMP_ID_LX_RNG_ISLAND,
    IORING_RNG_DATA_OFFSET, LX_NET_OP_POLL_RX_TX, LX_NET_OP_QUERY_INFO, LX_NET_OP_SEND_ARP,
    LX_OP_READ_ENTROPY,
};
use crate::shell::LineBuilder;
use crate::vfs_proto::{vfs_create_file, vfs_read_file, vfs_sync, vfs_write_small};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_derive, sys_cap_query, sys_cap_revoke,
    sys_ep_create, sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_signal,
    sys_notify_wait, sys_proc_list, sys_proc_spawn, sys_proc_wait, sys_sysinfo, sys_vspace_map,
    RootInitBootInfo, UserCapInfo, UserProcInfo, UserSysInfo, EBADCAP, EEXIST, EREVOKED,
    IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_PONG, RIGHT_CALL, RIGHT_GRANT, RIGHT_MAP,
    RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_SPAWN, RIGHT_WRITE, VAULT_OP_STATUS,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;
const CLIENT_LX_RING_VADDR: u64 = 0x2600_0000;
const LX_GUARD_PATH: &[u8] = b"/persist/lx-guard.txt";
const LX_GUARD_TEXT: &[u8] = b"lx-island-guard-ok";
const LX_NET_GUARD_PATH: &[u8] = b"/persist/lx-net-guard.txt";
const LX_NET_GUARD_TEXT: &[u8] = b"lx-net-guard-ok";

struct LxBrokerHarness {
    proc_handle: u64,
    broker_ep: u64,
    admin_cap: u64,
    client_cap: u64,
    rogue_cap: u64,
    unbadged_cap: u64,
}

impl LxBrokerHarness {
    fn spawn(boot: &RootInitBootInfo) -> Result<Self, i32> {
        if boot.pci_rng_cap == 0 {
            return Err(250);
        }
        Self::spawn_with(boot.broker_mod_cap, boot.lx_rng_mod_cap)
    }

    fn spawn_with(broker_mod_cap: u64, island_mod_cap: u64) -> Result<Self, i32> {
        if broker_mod_cap == 0 || island_mod_cap == 0 {
            return Err(250);
        }
        let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
        let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

        let broker_ep = sys_ep_create(ipc_spec).map_err(|_| 251)?;
        let admin_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_ADMIN).map_err(|_| 252)?;
        let client_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_CLIENT).map_err(|_| 252)?;
        let rogue_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_ROGUE).map_err(|_| 252)?;
        let unbadged_cap = sys_cap_derive(broker_ep, cli_rights, 0, 0).map_err(|_| 252)?;

        let (proc_handle, _) = sys_proc_spawn(
            broker_mod_cap,
            1,
            COMP_ID_DEV_BROKER,
            (broker_ep, srv_rights, BADGE_BROKER_LX_MODE),
            (island_mod_cap, RIGHT_READ | RIGHT_SPAWN),
        )
        .map_err(|_| 253)?;

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
        .map_err(|_| 268)?;
        if msg_tag_label(stag) != IPC_OP_PONG {
            return Err(269);
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

fn acquire_client_ring(client_cap: u64) -> Result<(u64, u64, u64), i32> {
    let mut out = [0u64; 3];
    for (idx, dst) in out.iter_mut().enumerate() {
        let (tag, cap, _) = sys_ipc_call_with_grant(
            client_cap,
            BROKER_OP_ACQUIRE_RING,
            [idx as u64, 0, 0, 0],
            0,
            0,
            0,
            0,
        )
        .map_err(|_| 257)?;
        if msg_tag_label(tag) != IPC_OP_PONG || cap == 0 {
            return Err(257);
        }
        *dst = cap;
    }
    Ok((out[0], out[1], out[2]))
}

fn write_persist_guard(vfs_sup_cap: u64, path: &[u8], text: &[u8], err: i32) -> Result<(), i32> {
    if vfs_sup_cap != 0 {
        match vfs_create_file(vfs_sup_cap, path, 0) {
            Ok(_) | Err(EEXIST) => {}
            Err(_) => return Err(err),
        }
        vfs_write_small(vfs_sup_cap, path, text, false).map_err(|_| err)?;
        vfs_sync(vfs_sup_cap).map_err(|_| err)?;
    }
    Ok(())
}

fn verify_persist_and_vault(vfs_sup_cap: u64, vault_sup_cap: u64, path: &[u8], expected: &[u8]) -> (bool, bool) {
    let mut persist_ok = true;
    if vfs_sup_cap != 0 {
        let mut rbuf = [0u8; 32];
        match vfs_read_file(vfs_sup_cap, path, 0, &mut rbuf) {
            Ok((n, _)) if &rbuf[..n] == expected => {}
            _ => persist_ok = false,
        }
    }
    let mut vault_ok = true;
    if vault_sup_cap != 0 {
        vault_ok = sys_ipc_call_inline(
            vault_sup_cap, pack_msg_tag(VAULT_OP_STATUS, 0, 1, 0), [0; 4], 0,
        )
        .is_ok_and(|(vt, _, _)| msg_tag_label(vt) == IPC_OP_PONG);
    }
    (persist_ok, vault_ok)
}

fn check_unauth_denials(bh: &LxBrokerHarness) -> u64 {
    let mut denied = 0u64;
    for bad_cap in [bh.rogue_cap, bh.unbadged_cap] {
        if sys_ipc_call_inline(bad_cap, pack_msg_tag(BROKER_OP_ACQUIRE_EP, 0, 1, 0), [0; 4], 0)
            .is_ok_and(|(t, _, _)| msg_tag_label(t) == IPC_OP_ERR_PERM)
        {
            denied += 1;
        }
    }
    for cli_forbidden in [BROKER_OP_FORWARD_TRANSFORM, BROKER_OP_CRASH_AND_RECOVER, BROKER_OP_BIND_PCI_DEV] {
        if sys_ipc_call_inline(bh.client_cap, pack_msg_tag(cli_forbidden, 0, 1, 0), [0; 4], 0)
            .is_ok_and(|(t, _, _)| msg_tag_label(t) == IPC_OP_ERR_PERM)
        {
            denied += 1;
        }
    }
    denied
}

pub fn run_lx_island_audit(
    boot: &RootInitBootInfo,
    vfs_sup_cap: u64,
    vault_sup_cap: u64,
) -> Result<(), i32> {
    let mut sys0 = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys0);
    let tsc_mhz = sys0.tsc_mhz.max(1000);

    write_persist_guard(vfs_sup_cap, LX_GUARD_PATH, LX_GUARD_TEXT, 249)?;

    let bh = LxBrokerHarness::spawn(boot)?;

    // Wrong-object-kind `SYS_DEV_MAP` call from client domain must be rejected by kernel PCI guard.
    let mut dummy_info = UserVirtioPciInfo::default();
    let cap_kind_guard_ok =
        sys_dev_map(bh.broker_ep, CLIENT_LX_RING_VADDR + 0x10000, &mut dummy_info).is_err();

    let unauth_denied = check_unauth_denials(&bh);
    if !cap_kind_guard_ok || unauth_denied < 5 {
        return Err(254);
    }

    // Bind VirtIO-RNG PCI cap into isolated `dry-lx-rng.elf` (`FaultEndpoint` + `SYS_DEV_MAP` + `SYS_DMA_ALLOC`).
    let (btag, _, bmrs) = sys_ipc_call_with_grant(
        bh.admin_cap,
        BROKER_OP_BIND_PCI_DEV,
        [COMP_ID_LX_RNG_ISLAND as u64, 0, 0, 0],
        boot.pci_rng_cap,
        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT,
        0,
        0,
    )
    .map_err(|_| 255)?;
    if msg_tag_label(btag) != IPC_OP_PONG || bmrs[0] != 1 {
        return Err(255);
    }
    let vdev_packed = bmrs[2];
    let vendor_id = (vdev_packed >> 16) & 0xffff;
    let device_id = vdev_packed & 0xffff;
    let mac_neg_ok = bmrs[3] != 0;
    if vendor_id != 0x1af4 || !mac_neg_ok {
        return Err(256);
    }

    let (ep1_tag, gen1_drv_ep, ep1_mrs) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0)
            .map_err(|_| 256)?;
    if msg_tag_label(ep1_tag) != IPC_OP_PONG || gen1_drv_ep == 0 || ep1_mrs[0] != 1 {
        return Err(256);
    }
    let (gen1_ring_cap, gen1_sq_notif, gen1_cq_notif) = acquire_client_ring(bh.client_cap)?;
    if sys_vspace_map(gen1_ring_cap, CLIENT_LX_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        return Err(258);
    }
    // SAFETY: `gen1_ring_cap` maps the live 4 KiB `IoRingSharedPage` from `dry-lx-rng`.
    let ring = unsafe { &*(CLIENT_LX_RING_VADDR as *const IoRingSharedPage) };

    // 1. Direct 1-hop `Endpoint` entropy sampling (`32 x 24 = 768` bytes) via Linux `virtrng_probe`/`scan`/`read`.
    let mut dir_lat = LatencySamples64::new();
    let mut words = [0u64; 32];
    let mut total_bytes = 0u64;
    let mut total_ones = 0u64;
    for slot in &mut words {
        let t0 = rdtsc_user();
        let (tag, _, mrs) = sys_ipc_call_inline(
            gen1_drv_ep,
            pack_msg_tag(LX_OP_READ_ENTROPY, 0, 4, 0),
            [24, 0, 0, 1],
            0,
        )
        .map_err(|_| 259)?;
        let dt = rdtsc_user().saturating_sub(t0);
        if msg_tag_label(tag) != IPC_OP_PONG || (mrs[3] & 0xffff_ffff) != 1 {
            return Err(259);
        }
        dir_lat.push(dt);
        *slot = mrs[0];
        total_bytes += 24;
        total_ones += (mrs[0].count_ones() + mrs[1].count_ones() + mrs[2].count_ones()) as u64;
    }
    let gen1_sample = words[0];

    let mut uniq_words = 0u64;
    for i in 0..32 {
        let mut dup = false;
        for j in 0..i {
            if words[i] == words[j] {
                dup = true;
                break;
            }
        }
        if !dup {
            uniq_words += 1;
        }
    }

    // 2. Batched `IoRingSharedPage` (`16 x 64 = 1,024` bytes) in event-driven `park_mode = 1`.
    let (ptag, _, _) = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    )
    .map_err(|_| 260)?;
    if msg_tag_label(ptag) != IPC_OP_PONG {
        return Err(260);
    }
    let rt0 = rdtsc_user();
    for i in 0..16u32 {
        let sqe = pack_rng_sqe(IORING_RNG_DATA_OFFSET + i * 64, 64, 1);
        if !ring.push_sqe(sqe) {
            return Err(260);
        }
    }
    let _ = sys_notify_signal(gen1_sq_notif, 1);
    let mut ring_done = 0u32;
    for _ in 0..32 {
        while let Some(cqe) = ring.pop_cqe() {
            if cqe.status != 0 || cqe.bytes_done != 64 {
                return Err(260);
            }
            ring_done += 1;
        }
        if ring_done >= 16 {
            break;
        }
        let _ = sys_notify_wait(gen1_cq_notif, 0, 0);
    }
    let ring16_cyc = rdtsc_user().saturating_sub(rt0);
    if ring_done != 16 {
        return Err(260);
    }
    // SAFETY: `2048..3072` inside `IoRingSharedPage` holds `16 x 64` hardware entropy bytes.
    let ring_words = unsafe {
        core::slice::from_raw_parts(
            (CLIENT_LX_RING_VADDR + (IORING_RNG_DATA_OFFSET as u64)) as *const u64,
            128,
        )
    };
    for &rw in ring_words {
        total_ones += rw.count_ones() as u64;
    }
    total_bytes += 1024;

    let _ = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [0, 0, 0, 0],
        0,
    );

    // 3. Broker 2-hop admin diagnostic probe (`16 x 24` bytes) for A/B latency comparison.
    let mut brk_lat = LatencySamples64::new();
    for _ in 0..16 {
        let bt0 = rdtsc_user();
        let (ftag, _, _) = sys_ipc_call_inline(
            bh.admin_cap,
            pack_msg_tag(BROKER_OP_FORWARD_TRANSFORM, 0, 4, 0),
            [0; 4],
            0,
        )
        .map_err(|_| 261)?;
        let bdt = rdtsc_user().saturating_sub(bt0);
        if msg_tag_label(ftag) != IPC_OP_PONG {
            return Err(261);
        }
        brk_lat.push(bdt);
    }
    let (ltag, _, lmrs) = sys_ipc_call_inline(
        gen1_drv_ep,
        pack_msg_tag(LX_OP_READ_ENTROPY, 0, 4, 0),
        [24, 0, 0, 1],
        0,
    )
    .map_err(|_| 261)?;
    if msg_tag_label(ltag) != IPC_OP_PONG {
        return Err(261);
    }
    let vq_irqs = lmrs[3] >> 32;

    let mut procs = [UserProcInfo::default(); 24];
    let mut lx_rss_4k = 0u64;
    if let Ok((pcnt, _)) = sys_proc_list(&mut procs) {
        for p in procs.iter().take(pcnt) {
            if p.compartment_id == COMP_ID_LX_RNG_ISLAND {
                lx_rss_4k = (p.mapped_pages_4k + p.pt_frames_4k) as u64;
                break;
            }
        }
    }

    let _ = sys_vspace_map(gen1_ring_cap, CLIENT_LX_RING_VADDR, VSPACE_MAP_OP_UNMAP_4K);

    // 4. Controlled `#UD` (`vec=6`) via `FaultEndpoint` + kernel `BME=0` quiesce + restart (`gen=2`).
    let (ctag, _, cmrs) = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_CRASH_AND_RECOVER, 0, 1, 0),
        [0; 4],
        0,
    )
    .map_err(|_| 262)?;
    let new_gen = cmrs[0];
    let recov_cyc = cmrs[1];
    let fault_vec = cmrs[2];
    let restarts = cmrs[3];
    if msg_tag_label(ctag) != IPC_OP_PONG || new_gen != 2 || fault_vec != 6 || restarts < 1 {
        return Err(262);
    }

    let ep_rev = matches!(
        sys_ipc_call_inline(
            gen1_drv_ep,
            pack_msg_tag(LX_OP_READ_ENTROPY, 0, 4, 0),
            [16, 0, 0, 1],
            0,
        ),
        Err(EREVOKED | EBADCAP)
    );
    let ntf_rev = matches!(sys_notify_signal(gen1_sq_notif, 1), Err(EREVOKED | EBADCAP));
    let mut cinfo = UserCapInfo::default();
    let cq_rev = matches!(sys_cap_query(gen1_cq_notif, &mut cinfo), EREVOKED | EBADCAP);
    let ring_rev = matches!(sys_cap_query(gen1_ring_cap, &mut cinfo), EREVOKED | EBADCAP);

    let (e2tag, gen2_drv_ep, e2mrs) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0)
            .map_err(|_| 263)?;
    if msg_tag_label(e2tag) != IPC_OP_PONG || gen2_drv_ep == 0 || e2mrs[0] != 2 {
        return Err(263);
    }
    let (stag, _, smrs) = sys_ipc_call_inline(
        gen2_drv_ep,
        pack_msg_tag(LX_OP_READ_ENTROPY, 0, 4, 0),
        [16, 0, 0, 1],
        0,
    )
    .map_err(|_| 264)?;
    let stale_gen = msg_tag_label(stag) == IPC_OP_ERR_STALE_GEN && smrs[0] == (-EREVOKED) as u64;

    let (g2tag, _, g2mrs) = sys_ipc_call_inline(
        gen2_drv_ep,
        pack_msg_tag(LX_OP_READ_ENTROPY, 0, 4, 0),
        [24, 0, 0, 2],
        0,
    )
    .map_err(|_| 264)?;
    if msg_tag_label(g2tag) != IPC_OP_PONG || g2mrs[0] == 0 || g2mrs[0] == gen1_sample {
        return Err(265);
    }
    let _ = sys_cap_revoke(gen2_drv_ep, 1);

    bh.shutdown_and_reclaim()?;

    let (persist_ok, vault_ok) =
        verify_persist_and_vault(vfs_sup_cap, vault_sup_cap, LX_GUARD_PATH, LX_GUARD_TEXT);

    let ones_pct = (total_ones * 100) / (total_bytes * 8);
    if !(40..=60).contains(&ones_pct)
        || uniq_words < 30
        || vq_irqs < 28
        || !ep_rev
        || !ntf_rev
        || !cq_rev
        || !ring_rev
        || !stale_gen
        || !persist_ok
        || !vault_ok
    {
        return Err(266);
    }

    let (_, dp50, dp95, dp99, _) = dir_lat.summary();
    let (_, bp50, bp95, bp99, _) = brk_lat.summary();

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-LX-RNG-PASS] vendor_id=0x");
    lb.push_hex_u64(vendor_id);
    lb.push_bytes(b" device_id=0x");
    lb.push_hex_u64(device_id);
    lb.push_bytes(b" mac_neg=");
    lb.push_u64(u64::from(mac_neg_ok));
    lb.push_bytes(b" unauth=");
    lb.push_u64(unauth_denied);
    lb.push_bytes(b" vq_irqs=");
    lb.push_u64(vq_irqs);
    lb.push_bytes(b" bytes=");
    lb.push_u64(total_bytes);
    lb.push_bytes(b" ones_pct=");
    lb.push_u64(ones_pct);
    lb.push_bytes(b" uniq=");
    lb.push_u64(uniq_words);
    lb.push_bytes(b" vec=");
    lb.push_u64(fault_vec);
    lb.push_bytes(b" restarts=");
    lb.push_u64(restarts);
    lb.push_bytes(b" ep_rev=");
    lb.push_u64(u64::from(ep_rev));
    lb.push_bytes(b" ntf_rev=");
    lb.push_u64(u64::from(ntf_rev));
    log_line(&lb);

    let mut lb2 = LineBuilder::new();
    lb2.push_bytes(b"[DRY-LX-RNG-PASS2] cq_rev=");
    lb2.push_u64(u64::from(cq_rev));
    lb2.push_bytes(b" ring_rev=");
    lb2.push_u64(u64::from(ring_rev));
    lb2.push_bytes(b" stale_gen=");
    lb2.push_u64(u64::from(stale_gen));
    lb2.push_bytes(b" persist_ok=");
    lb2.push_u64(u64::from(persist_ok));
    lb2.push_bytes(b" vault_ok=");
    lb2.push_u64(u64::from(vault_ok));
    lb2.push_bytes(b" ones=");
    lb2.push_u64(total_ones);
    log_line(&lb2);

    let mut blb = LineBuilder::new();
    blb.push_bytes(b"[DRY-LX-RNG-BENCH] dir_p50=");
    blb.push_u64(dp50);
    blb.push_bytes(b" dir_p95=");
    blb.push_u64(dp95);
    blb.push_bytes(b" dir_p99=");
    blb.push_u64(dp99);
    blb.push_bytes(b" brk_p50=");
    blb.push_u64(bp50);
    blb.push_bytes(b" brk_p95=");
    blb.push_u64(bp95);
    blb.push_bytes(b" brk_p99=");
    blb.push_u64(bp99);
    blb.push_bytes(b" ring16_cyc=");
    blb.push_u64(ring16_cyc);
    blb.push_bytes(b" recov_cyc=");
    blb.push_u64(recov_cyc);
    blb.push_bytes(b" rss_4k=");
    blb.push_u64(lx_rss_4k);
    blb.push_bytes(b" tsc_mhz=");
    blb.push_u64(tsc_mhz);
    log_line(&blb);
    Ok(())
}

pub fn run_lx_net_audit(
    boot_info_addr: u64,
    boot: &RootInitBootInfo,
    vfs_sup_cap: u64,
    vault_sup_cap: u64,
) -> Result<(), i32> {
    let res = run_lx_net_audit_inner(boot_info_addr, boot, vfs_sup_cap, vault_sup_cap);
    if let Err(code) = res {
        let mut elb = LineBuilder::new();
        elb.push_bytes(b"[DRY-LX-NET-FAIL] code=");
        elb.push_u64(code as u64);
        log_line(&elb);
    }
    res
}

fn assert_net_drvctl(admin_cap: u64, exp_gen: u64, exp_restarts: u64, req_mask: u64, err: i32) -> Result<u64, i32> {
    let (dtag, _, dmrs) = sys_ipc_call_inline(
        admin_cap, pack_msg_tag(BROKER_OP_DRVCTL_QUERY, 0, 1, 0), [0; 4], 0,
    )
    .map_err(|_| err)?;
    let req_contract = DRVCTL_CONTRACT_RING0_KO_FORBIDDEN
        | DRVCTL_CONTRACT_NO_IGPU_PASSTHROUGH
        | DRVCTL_CONTRACT_FAULT_EP_REQUIRED
        | DRVCTL_CONTRACT_IOMMU_ISOLATED
        | DRVCTL_CONTRACT_VALID;
    let visited = dmrs[1] & 0xffff;
    if msg_tag_label(dtag) != IPC_OP_PONG
        || (dmrs[0] & 0xff) != u64::from(LifecycleStep::Monitoring.as_u8())
        || ((dmrs[0] >> 8) & 0xff) != 1
        || ((dmrs[0] >> 16) & 0xffff) != u64::from(COMP_ID_LX_NET_8139CP_ISLAND)
        || (dmrs[0] >> 40) != exp_gen
        || (dmrs[3] >> 32) != exp_restarts
        || (visited & req_mask) != req_mask
        || (dmrs[3] & req_contract) != req_contract
    {
        return Err(err);
    }
    Ok(visited)
}

fn run_lx_net_audit_inner(
    boot_info_addr: u64,
    boot: &RootInitBootInfo,
    vfs_sup_cap: u64,
    vault_sup_cap: u64,
) -> Result<(), i32> {
    if boot_info_addr == 0 {
        return Err(270);
    }
    // SAFETY: Kernel populates `RootInitBootExtInfo` read-only at `boot_info_addr + BOOT_EXT_INFO_OFFSET`.
    let ext = unsafe { &*((boot_info_addr + BOOT_EXT_INFO_OFFSET) as *const RootInitBootExtInfo) };
    if ext.magic != ROOT_BOOT_EXT_MAGIC || ext.lx_net_mod_cap == 0 || ext.pci_net_cap == 0 {
        return Err(270);
    }
    let pci_rev = find_boot_pci_managed(boot_info_addr, boot.boot_pci_func_count, 0x10ec, 0x8139, 9)
        .or_else(|| find_boot_pci_managed(boot_info_addr, boot.boot_pci_func_count, 0x10ec, 0x8139, 0xff))
        .map_or(0, |r| r.revision_id);

    let mut sys0 = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys0);
    let tsc_mhz = sys0.tsc_mhz.max(1000);

    write_persist_guard(vfs_sup_cap, LX_NET_GUARD_PATH, LX_NET_GUARD_TEXT, 271)?;
    let bh = LxBrokerHarness::spawn_with(boot.broker_mod_cap, ext.lx_net_mod_cap)?;

    let mut dummy_info = UserVirtioPciInfo::default();
    if sys_dev_map(bh.broker_ep, CLIENT_LX_RING_VADDR + 0x10000, &mut dummy_info).is_ok()
        || check_unauth_denials(&bh) < 5
    {
        return Err(272);
    }
    for (bad_flag, exp_label) in [
        (CONTRACT_FLAG_REQUIRE_RING0_KO, IPC_OP_ERR_PERM),
        (CONTRACT_FLAG_ASSUME_IGPU_PASSTHROUGH, IPC_OP_ERR_INVAL),
    ] {
        let probe_cap = sys_cap_derive(bh.broker_ep, RIGHT_READ | RIGHT_GRANT, 0, 0).map_err(|_| 273)?;
        let (ctag, _, _) = sys_ipc_call_with_grant(
            bh.admin_cap, BROKER_OP_BIND_PCI_DEV,
            [COMP_ID_LX_NET_8139CP_ISLAND as u64, u64::from(pci_rev), 2, bad_flag],
            probe_cap, RIGHT_READ, 0, 0,
        )
        .map_err(|_| 273)?;
        let _ = sys_cap_revoke(probe_cap, 1);
        if msg_tag_label(ctag) != exp_label {
            return Err(273);
        }
    }

    let (btag, _, bmrs) = sys_ipc_call_with_grant(
        bh.admin_cap, BROKER_OP_BIND_PCI_DEV,
        [COMP_ID_LX_NET_8139CP_ISLAND as u64, u64::from(pci_rev), 1, 0],
        ext.pci_net_cap, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT, 0, 0,
    )
    .map_err(|_| 274)?;
    if msg_tag_label(btag) != IPC_OP_PONG || bmrs[0] != 1 {
        return Err(274);
    }
    let (vendor_id, device_id) = ((bmrs[2] >> 16) & 0xffff, bmrs[2] & 0xffff);
    if vendor_id != 0x10ec || device_id != 0x8139 || bmrs[3] == 0 {
        return Err(275);
    }
    let _ = assert_net_drvctl(bh.admin_cap, 1, 0, 0x0fff, 276)?;

    let (ep1_tag, gen1_drv_ep, ep1_mrs) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0).map_err(|_| 277)?;
    if msg_tag_label(ep1_tag) != IPC_OP_PONG || gen1_drv_ep == 0 || ep1_mrs[0] != 1 {
        return Err(277);
    }
    let (gen1_ring_cap, gen1_sq_notif, gen1_cq_notif) = acquire_client_ring(bh.client_cap)?;
    if sys_vspace_map(gen1_ring_cap, CLIENT_LX_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        return Err(278);
    }
    // SAFETY: `gen1_ring_cap` maps the live 4 KiB `IoRingSharedPage` from `dry-lx-net`.
    let ring = unsafe { &*(CLIENT_LX_RING_VADDR as *const IoRingSharedPage) };

    let (qtag, _, qmrs) = sys_ipc_call_inline(
        gen1_drv_ep, pack_msg_tag(LX_NET_OP_QUERY_INFO, 0, 4, 0), [0, 0, 0, 1], 0,
    )
    .map_err(|_| 279)?;
    let mac_u64 = qmrs[0];
    let bar1_mapped = ((qmrs[1] >> 8) & 0xff) != 0;
    let bar1_suboff = (qmrs[1] >> 16) & 0xffff;
    let mtu = (qmrs[1] >> 40) & 0xffff_ffff;
    if msg_tag_label(qtag) != IPC_OP_PONG || mac_u64 == 0 || !bar1_mapped || mtu != 1500 {
        return Err(279);
    }

    // Exercise live `8139cp.c` hardware TX + `IoRingSharedPage` ARP poll to QEMU SLIRP `10.0.2.2`.
    let (atag, _, _) = sys_ipc_call_inline(
        gen1_drv_ep, pack_msg_tag(LX_NET_OP_SEND_ARP, 0, 4, 0), [0, 0, 0, 1], 0,
    )
    .map_err(|_| 280)?;
    if msg_tag_label(atag) != IPC_OP_PONG
        || !ring.push_sqe(pack_net_poll_sqe(64, 0, 1))
        || sys_notify_signal(gen1_sq_notif, ISLAND_DOORBELL_CTL).is_err()
        || sys_notify_wait(gen1_cq_notif, 0, tsc_mhz.saturating_mul(2_000_000)).is_err()
        || ring.pop_cqe().is_none_or(|c| c.status != 0)
    {
        return Err(280);
    }

    let _ = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    );

    let mut rdy = LineBuilder::new();
    rdy.push_bytes(b"[DRY-LX-NET] UDP_LISTEN_READY port=7777 gen=1");
    log_line(&rdy);

    let mut tx_pkts = 0u64;
    let mut rx_pkts = 0u64;
    let mut arp_replies = 0u64;
    let mut udp_echo = 0u64;
    let poll_t0 = rdtsc_user();
    let poll_budget = tsc_mhz.saturating_mul(8_000_000);
    while rdtsc_user().saturating_sub(poll_t0) < poll_budget {
        let (ptag, _, pmrs) = sys_ipc_call_inline(
            gen1_drv_ep, pack_msg_tag(LX_NET_OP_POLL_RX_TX, 0, 4, 0), [0, 0, 0, 1], 0,
        )
        .map_err(|_| 281)?;
        if msg_tag_label(ptag) == IPC_OP_PONG {
            tx_pkts = pmrs[0];
            rx_pkts = pmrs[1];
            arp_replies = pmrs[2];
            udp_echo = pmrs[3] & 0xffff_ffff;
            if udp_echo >= 1 && tx_pkts >= 2 && rx_pkts >= 1 {
                break;
            }
        }
        let _ = sys_notify_wait(gen1_cq_notif, 0, tsc_mhz.saturating_mul(5_000));
    }
    if udp_echo < 1 || tx_pkts == 0 || rx_pkts == 0 {
        return Err(281);
    }

    let (ctag, _, cmrs) = sys_ipc_call_inline(
        bh.admin_cap, pack_msg_tag(BROKER_OP_CRASH_AND_RECOVER, 0, 1, 0), [0; 4], 0,
    )
    .map_err(|_| 282)?;
    if msg_tag_label(ctag) != IPC_OP_PONG || cmrs[0] != 2 || cmrs[3] != 1 {
        return Err(282);
    }
    let (fault_vec, restarts) = (cmrs[2], cmrs[3]);
    let drvctl_visited = assert_net_drvctl(bh.admin_cap, 2, 1, 0x1fff, 283)?;

    let mut cinfo = UserCapInfo::default();
    let ep_rev = sys_cap_query(gen1_drv_ep, &mut cinfo) != 0;
    let ring_rev = sys_cap_query(gen1_ring_cap, &mut cinfo) != 0
        && sys_cap_query(gen1_sq_notif, &mut cinfo) != 0
        && sys_cap_query(gen1_cq_notif, &mut cinfo) != 0;
    let _ = sys_vspace_map(0, CLIENT_LX_RING_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let stale_gen = sys_ipc_call_inline(
        bh.admin_cap, pack_msg_tag(BROKER_OP_FORWARD_TRANSFORM, 0, 4, 0), [0, 0, 0, 1], 0,
    )
    .is_ok_and(|(st, _, _)| msg_tag_label(st) == IPC_OP_ERR_STALE_GEN);

    let (ep2_tag, gen2_drv_ep, ep2_mrs) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0).map_err(|_| 284)?;
    if msg_tag_label(ep2_tag) != IPC_OP_PONG || gen2_drv_ep == 0 || ep2_mrs[0] != 2 {
        return Err(284);
    }
    let (g2q, _, g2qmrs) = sys_ipc_call_inline(
        gen2_drv_ep, pack_msg_tag(LX_NET_OP_QUERY_INFO, 0, 4, 0), [0, 0, 0, 2], 0,
    )
    .map_err(|_| 285)?;
    let (g2a, _, g2amrs) = sys_ipc_call_inline(
        gen2_drv_ep, pack_msg_tag(LX_NET_OP_SEND_ARP, 0, 4, 0), [0, 0, 0, 2], 0,
    )
    .map_err(|_| 285)?;
    if msg_tag_label(g2q) != IPC_OP_PONG
        || g2qmrs[0] != mac_u64
        || msg_tag_label(g2a) != IPC_OP_PONG
        || g2amrs[0] == 0
    {
        return Err(285);
    }
    let _ = sys_cap_revoke(gen2_drv_ep, 1);

    bh.shutdown_and_reclaim()?;

    let (persist_ok, vault_ok) =
        verify_persist_and_vault(vfs_sup_cap, vault_sup_cap, LX_NET_GUARD_PATH, LX_NET_GUARD_TEXT);
    if !ep_rev || !ring_rev || !stale_gen || !persist_ok || !vault_ok {
        return Err(286);
    }

    let mut lb = LineBuilder::new();
    for (k, v, hex) in [
        (&b"[DRY-LX-NET-PASS] vendor_id=0x"[..], vendor_id, true),
        (b" device_id=0x", device_id, true),
        (b" rev=0x", u64::from(pci_rev), true),
        (b" mac=0x", mac_u64, true),
        (b" bar1_suboff=", bar1_suboff, false),
        (b" tx_pkts=", tx_pkts, false),
        (b" rx_pkts=", rx_pkts, false),
        (b" udp_echo=", udp_echo, false),
        (b" arp=", arp_replies, false),
        (b" vec=", fault_vec, false),
        (b" restarts=", restarts, false),
    ] {
        lb.push_bytes(k);
        if hex { lb.push_hex_u64(v); } else { lb.push_u64(v); }
    }
    log_line(&lb);

    let mut lb2 = LineBuilder::new();
    for (k, v, hex) in [
        (&b"[DRY-LX-NET-PASS2] drvctl_visited=0x"[..], drvctl_visited, true),
        (b" ep_rev=", u64::from(ep_rev), false),
        (b" ring_rev=", u64::from(ring_rev), false),
        (b" stale_gen=", u64::from(stale_gen), false),
        (b" persist_ok=", u64::from(persist_ok), false),
        (b" vault_ok=", u64::from(vault_ok), false),
    ] {
        lb2.push_bytes(k);
        if hex { lb2.push_hex_u64(v); } else { lb2.push_u64(v); }
    }
    log_line(&lb2);
    Ok(())
}
