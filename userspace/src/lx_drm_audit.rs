// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone A4-3 Genuine Linux `v6.18` `drivers/gpu/drm/tiny/bochs.c` Driver Island E2E Audit
//! (`session_mode == 29` via `desktop4=1 lx_drm=1`).

use crate::abi_ext::{sys_dev_query_bars, UserPciDeviceInfo};
use crate::init_legacy::{domain_spec, log_line};
use crate::ioring::IoRingSharedPage;
use crate::island_proto::{
    LatencySamples64, BADGE_BROKER_ADMIN, BADGE_BROKER_CLIENT, BADGE_BROKER_ROGUE,
    BROKER_OP_ACQUIRE_EP, BROKER_OP_ACQUIRE_RING, BROKER_OP_CRASH_AND_RECOVER,
    BROKER_OP_FORWARD_TRANSFORM, BROKER_OP_SET_PARK_MODE, BROKER_OP_SHUTDOWN, COMP_ID_DEV_BROKER,
    IPC_OP_ERR_STALE_GEN,
};
use crate::lx_compat_meta::{bar_is_mmio, count_boot_pci_class, find_boot_pci_managed};
use crate::lx_drv_proto::{
    pack_drm_flush_sqe, BADGE_BROKER_LX_MODE, BROKER_OP_BIND_PCI_DEV, COMP_ID_LX_DRM_BOCHS_ISLAND,
    LX_DRM_OP_COMMIT_DAMAGE, LX_DRM_OP_IMPORT_DMABUF, LX_DRM_OP_MODESET, LX_DRM_OP_QUERY_MODE,
};
use crate::shell::LineBuilder;
use crate::vfs_proto::{vfs_create_file, vfs_read_file, vfs_sync, vfs_write_small};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_derive, sys_cap_query, sys_cap_revoke,
    sys_ep_create, sys_frame_alloc, sys_ipc_call_inline, sys_ipc_call_with_grant,
    sys_notify_signal, sys_notify_wait, sys_proc_list, sys_proc_spawn, sys_proc_wait, sys_sysinfo,
    sys_vspace_map, RootInitBootInfo, UserCapInfo, UserProcInfo, UserSysInfo, EBADCAP, EEXIST,
    EREVOKED, IPC_OP_ERR_PERM, IPC_OP_PONG, RIGHT_CALL, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ,
    RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_SPAWN, RIGHT_WRITE, VAULT_OP_STATUS,
    VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_2M, VSPACE_MAP_OP_UNMAP_4K,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;
const CLIENT_DRM_RING_VADDR: u64 = 0x2620_0000;
const CLIENT_DMABUF_VADDR: u64 = 0x2800_0000;
const BOCHS_W: u32 = 800;
const BOCHS_H: u32 = 600;
const BOCHS_PITCH: u32 = BOCHS_W * 4;
const LX_DRM_GUARD_PATH: &[u8] = b"/persist/lx-drm-guard.txt";
const LX_DRM_GUARD_TEXT: &[u8] = b"lx-drm-guard-ok";

pub struct LxDrmBrokerHarness {
    proc_handle: u64,
    broker_ep: u64,
    admin_cap: u64,
    client_cap: u64,
    rogue_cap: u64,
    unbadged_cap: u64,
}

pub struct LxDrmLiveSession {
    broker: LxDrmBrokerHarness,
    dmabuf_cap: u64,
}

impl LxDrmBrokerHarness {
    fn spawn(boot: &RootInitBootInfo) -> Result<Self, i32> {
        if boot.broker_mod_cap == 0 || boot.lx_drm_mod_cap == 0 || boot.pci_disp_aux_cap == 0 {
            return Err(290);
        }
        let ipc_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
        let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
        let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

        let broker_ep = sys_ep_create(ipc_spec).map_err(|_| 291)?;
        let admin_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_ADMIN).map_err(|_| 292)?;
        let client_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_CLIENT).map_err(|_| 292)?;
        let rogue_cap =
            sys_cap_derive(broker_ep, cli_rights, 0, BADGE_BROKER_ROGUE).map_err(|_| 292)?;
        let unbadged_cap = sys_cap_derive(broker_ep, cli_rights, 0, 0).map_err(|_| 292)?;

        let (proc_handle, _) = sys_proc_spawn(
            boot.broker_mod_cap,
            1,
            COMP_ID_DEV_BROKER,
            (broker_ep, srv_rights, BADGE_BROKER_LX_MODE),
            (boot.lx_drm_mod_cap, RIGHT_READ | RIGHT_SPAWN),
        )
        .map_err(|_| 293)?;

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
        .map_err(|_| 299)?;
        if msg_tag_label(stag) != IPC_OP_PONG {
            return Err(299);
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
        .map_err(|_| 297)?;
        if msg_tag_label(tag) != IPC_OP_PONG || cap == 0 {
            return Err(297);
        }
        *dst = cap;
    }
    Ok((out[0], out[1], out[2]))
}

fn paint_bochs_test_card(gen: u64) {
    const BARS: [u32; 8] = [
        0x00e2e8f0, 0x00facc15, 0x0022d3ee, 0x0034d399, 0x00e879f9, 0x00fb7185, 0x0060a5fa,
        0x0038bdf8,
    ];
    let px_ptr = CLIENT_DMABUF_VADDR as *mut u32;
    let total_px = (BOCHS_W as usize) * (BOCHS_H as usize);
    // SAFETY: `CLIENT_DMABUF_VADDR` maps a writable 2 MiB frame (`>= 800*600*4` bytes).
    let fb = unsafe { core::slice::from_raw_parts_mut(px_ptr, total_px) };
    for y in 0..BOCHS_H as usize {
        let row = &mut fb[y * (BOCHS_W as usize)..(y + 1) * (BOCHS_W as usize)];
        for (x, pixel) in row.iter_mut().enumerate() {
            let mut color = if (x % 40 == 0) || (y % 40 == 0) {
                0x001e293b
            } else {
                0x000f172a
            };
            if (64..=736).contains(&x) && (40..=96).contains(&y) {
                color = if gen >= 2 { 0x000ea5e9 } else { 0x003b82f6 };
            } else if (64..=736).contains(&x) && (120..=360).contains(&y) {
                let idx = ((x - 64) * 8) / 673;
                color = BARS[idx.min(7)];
            } else if (64..=390).contains(&x) && (388..=500).contains(&y) {
                color = 0x00a78bfa;
            } else if (410..=736).contains(&x) && (388..=500).contains(&y) {
                color = if gen >= 2 { 0x0010b981 } else { 0x00f59e0b };
            } else if (64..=736).contains(&x) && (520..=564).contains(&y) {
                color = if gen >= 2 { 0x00059669 } else { 0x00475569 };
            }
            *pixel = color;
        }
    }
}

fn verify_negative_caps(bh: &LxDrmBrokerHarness) -> Result<u64, i32> {
    let mut dummy_pci = UserPciDeviceInfo::default();
    let cap_guard_ok = sys_dev_query_bars(bh.broker_ep, &mut dummy_pci).is_err();
    let mut denied = 0u64;
    for bad in [bh.rogue_cap, bh.unbadged_cap] {
        if sys_ipc_call_inline(
            bad,
            pack_msg_tag(BROKER_OP_ACQUIRE_EP, 0, 1, 0),
            [0; 4],
            0,
        )
        .is_ok_and(|(t, _, _)| msg_tag_label(t) == IPC_OP_ERR_PERM)
        {
            denied += 1;
        }
    }
    for bad_op in [
        BROKER_OP_FORWARD_TRANSFORM,
        BROKER_OP_CRASH_AND_RECOVER,
        BROKER_OP_BIND_PCI_DEV,
    ] {
        if sys_ipc_call_inline(bh.client_cap, pack_msg_tag(bad_op, 0, 1, 0), [0; 4], 0)
            .is_ok_and(|(t, _, _)| msg_tag_label(t) == IPC_OP_ERR_PERM)
        {
            denied += 1;
        }
    }
    if !cap_guard_ok || denied < 5 {
        return Err(294);
    }
    Ok(denied)
}

fn setup_gen_scanout(
    drv_ep: u64,
    dmabuf_cap: u64,
    gen: u64,
) -> Result<(u64, bool, bool, bool), i32> {
    let (qtag, _, qmrs) = sys_ipc_call_inline(
        drv_ep,
        pack_msg_tag(LX_DRM_OP_QUERY_MODE, 0, 4, 0),
        [0, 0, 0, gen],
        0,
    )
    .map_err(|_| 295)?;
    if msg_tag_label(qtag) != IPC_OP_PONG {
        return Err(295);
    }
    let edid_ok = (qmrs[3] & 1) != 0;
    let bar0_ok = (qmrs[3] & 2) != 0;
    let bar2_ok = (qmrs[3] & 4) != 0;

    let (mtag, _, mmrs) = sys_ipc_call_inline(
        drv_ep,
        pack_msg_tag(LX_DRM_OP_MODESET, 0, 4, 0),
        [BOCHS_W as u64, BOCHS_H as u64, 0, gen],
        0,
    )
    .map_err(|_| 296)?;
    if msg_tag_label(mtag) != IPC_OP_PONG || mmrs[0] != BOCHS_W as u64 || mmrs[1] != BOCHS_H as u64
    {
        return Err(296);
    }

    paint_bochs_test_card(gen);
    let (itag, _, imrs) = sys_ipc_call_with_grant(
        drv_ep,
        LX_DRM_OP_IMPORT_DMABUF,
        [BOCHS_W as u64, BOCHS_H as u64, BOCHS_PITCH as u64, gen],
        dmabuf_cap,
        RIGHT_READ | RIGHT_MAP,
        0,
        0,
    )
    .map_err(|_| 297)?;
    if msg_tag_label(itag) != IPC_OP_PONG {
        return Err(297);
    }
    let (ctag, _, _) = sys_ipc_call_inline(
        drv_ep,
        pack_msg_tag(LX_DRM_OP_COMMIT_DAMAGE, 0, 4, 0),
        [
            0,
            (BOCHS_W as u64) | ((BOCHS_H as u64) << 32),
            0,
            gen,
        ],
        0,
    )
    .map_err(|_| 298)?;
    if msg_tag_label(ctag) != IPC_OP_PONG {
        return Err(298);
    }
    Ok((imrs[0], edid_ok, bar0_ok, bar2_ok))
}

pub fn run_lx_drm_bochs_audit(
    boot_info_addr: u64,
    boot: &RootInitBootInfo,
    vfs_sup_cap: u64,
    vault_sup_cap: u64,
) -> Result<LxDrmLiveSession, i32> {
    let mut sys0 = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys0);
    let tsc_mhz = sys0.tsc_mhz.max(1000);

    let bochs_rec =
        find_boot_pci_managed(boot_info_addr, boot.boot_pci_func_count, 0x1234, 0x1111, 6)
            .ok_or(289)?;
    let disp_pci = count_boot_pci_class(boot_info_addr, boot.boot_pci_func_count, 0x03);
    if bochs_rec.managed_slot != 6 || !bar_is_mmio(bochs_rec.bar_flags[0]) || disp_pci < 2 {
        return Err(289);
    }

    if vfs_sup_cap != 0 {
        match vfs_create_file(vfs_sup_cap, LX_DRM_GUARD_PATH, 0) {
            Ok(_) | Err(EEXIST) => {}
            Err(_) => return Err(290),
        }
        vfs_write_small(vfs_sup_cap, LX_DRM_GUARD_PATH, LX_DRM_GUARD_TEXT, false)
            .map_err(|_| 290)?;
        vfs_sync(vfs_sup_cap).map_err(|_| 290)?;
    }

    let bh = LxDrmBrokerHarness::spawn(boot)?;
    let unauth_denied = verify_negative_caps(&bh)?;

    let (btag, _, bmrs) = sys_ipc_call_with_grant(
        bh.admin_cap,
        BROKER_OP_BIND_PCI_DEV,
        [COMP_ID_LX_DRM_BOCHS_ISLAND as u64, 0, 0, 0],
        boot.pci_disp_aux_cap,
        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT,
        0,
        0,
    )
    .map_err(|_| 295)?;
    let vdev_packed = bmrs[2];
    let vendor_id = (vdev_packed >> 16) & 0xffff;
    let device_id = vdev_packed & 0xffff;
    let mac_neg_ok = bmrs[3] != 0;
    if msg_tag_label(btag) != IPC_OP_PONG
        || bmrs[0] != 1
        || vendor_id != 0x1234
        || device_id != 0x1111
        || !mac_neg_ok
    {
        return Err(295);
    }

    let (ep1_tag, gen1_drv_ep, ep1_mrs) =
        sys_ipc_call_with_grant(bh.client_cap, BROKER_OP_ACQUIRE_EP, [0; 4], 0, 0, 0, 0)
            .map_err(|_| 296)?;
    if msg_tag_label(ep1_tag) != IPC_OP_PONG || gen1_drv_ep == 0 || ep1_mrs[0] != 1 {
        return Err(296);
    }
    let (gen1_ring_cap, gen1_sq_notif, gen1_cq_notif) = acquire_client_ring(bh.client_cap)?;
    if sys_vspace_map(gen1_ring_cap, CLIENT_DRM_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        return Err(296);
    }
    // SAFETY: `gen1_ring_cap` maps the live 4 KiB `IoRingSharedPage` from `dry-lx-drm-bochs`.
    let ring = unsafe { &*(CLIENT_DRM_RING_VADDR as *const IoRingSharedPage) };

    let dmabuf_cap = sys_frame_alloc(1).map_err(|_| 296)?;
    if sys_vspace_map(dmabuf_cap, CLIENT_DMABUF_VADDR, VSPACE_MAP_OP_2M_RW).is_err() {
        return Err(296);
    }

    let (ro_enforced, edid_ok, bar0_ok, bar2_ok) = setup_gen_scanout(gen1_drv_ep, dmabuf_cap, 1)?;
    if ro_enforced == 0 || !bar0_ok || !bar2_ok {
        return Err(297);
    }

    // Benchmark 32 Tier-1 direct IPC damage flushes (`64x64`).
    let mut dir_lat = LatencySamples64::new();
    for i in 0..32u64 {
        let dx = (i % 10) * 64;
        let dy = ((i / 10) % 6) * 64;
        let t0 = rdtsc_user();
        let (ctag, _, _) = sys_ipc_call_inline(
            gen1_drv_ep,
            pack_msg_tag(LX_DRM_OP_COMMIT_DAMAGE, 0, 4, 0),
            [dx | (dy << 32), 64 | (64 << 32), 0, 1],
            0,
        )
        .map_err(|_| 298)?;
        dir_lat.push(rdtsc_user().saturating_sub(t0));
        if msg_tag_label(ctag) != IPC_OP_PONG {
            return Err(298);
        }
    }

    // Benchmark 32 Tier-2 `IoRingSharedPage` damage flushes (2 x 16 SQE batches).
    let _ = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [1, 0, 0, 0],
        0,
    );
    let rt0 = rdtsc_user();
    for batch in 0..2u16 {
        for i in 0..16u16 {
            let tile = batch * 16 + i;
            let sqe = pack_drm_flush_sqe((tile % 10) * 64, ((tile / 10) % 6) * 64, 64, 64, 1);
            if !ring.push_sqe(sqe) {
                return Err(298);
            }
        }
        let _ = sys_notify_signal(gen1_sq_notif, 1);
        let mut got = 0u32;
        for _ in 0..32 {
            while let Some(cqe) = ring.pop_cqe() {
                if cqe.status != 0 {
                    return Err(298);
                }
                got += 1;
            }
            if got >= 16 {
                break;
            }
            let _ = sys_notify_wait(gen1_cq_notif, 0, 0);
        }
        if got != 16 {
            return Err(298);
        }
    }
    let ring32_cyc = rdtsc_user().saturating_sub(rt0);
    let _ = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_SET_PARK_MODE, 0, 1, 0),
        [0, 0, 0, 0],
        0,
    );

    let mut procs = [UserProcInfo::default(); 24];
    let mut lx_rss_4k = 0u64;
    if let Ok((pcnt, _)) = sys_proc_list(&mut procs) {
        for p in procs.iter().take(pcnt) {
            if p.compartment_id == COMP_ID_LX_DRM_BOCHS_ISLAND {
                lx_rss_4k = (p.mapped_pages_4k + p.pt_frames_4k) as u64;
                break;
            }
        }
    }

    let _ = sys_vspace_map(gen1_ring_cap, CLIENT_DRM_RING_VADDR, VSPACE_MAP_OP_UNMAP_4K);

    // Controlled `#UD` fault injection -> kernel `BME=0` quiesce -> restart `gen=2`.
    let (ctag, _, cmrs) = sys_ipc_call_inline(
        bh.admin_cap,
        pack_msg_tag(BROKER_OP_CRASH_AND_RECOVER, 0, 1, 0),
        [0; 4],
        0,
    )
    .map_err(|_| 298)?;
    let (new_gen, recov_cyc, fault_vec, restarts) = (cmrs[0], cmrs[1], cmrs[2], cmrs[3]);
    if msg_tag_label(ctag) != IPC_OP_PONG || new_gen != 2 || fault_vec != 6 || restarts < 1 {
        return Err(298);
    }

    let ep_rev = matches!(
        sys_ipc_call_inline(
            gen1_drv_ep,
            pack_msg_tag(LX_DRM_OP_QUERY_MODE, 0, 4, 0),
            [0, 0, 0, 1],
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
            .map_err(|_| 298)?;
    if msg_tag_label(e2tag) != IPC_OP_PONG || gen2_drv_ep == 0 || e2mrs[0] != 2 {
        return Err(298);
    }
    let (stag, _, smrs) = sys_ipc_call_inline(
        gen2_drv_ep,
        pack_msg_tag(LX_DRM_OP_QUERY_MODE, 0, 4, 0),
        [0, 0, 0, 1],
        0,
    )
    .map_err(|_| 298)?;
    let stale_gen = msg_tag_label(stag) == IPC_OP_ERR_STALE_GEN && smrs[0] == (-EREVOKED) as u64;

    let _ = setup_gen_scanout(gen2_drv_ep, dmabuf_cap, 2)?;
    let _ = sys_cap_revoke(gen2_drv_ep, 1);

    let mut persist_ok = true;
    if vfs_sup_cap != 0 {
        let mut rbuf = [0u8; 32];
        match vfs_read_file(vfs_sup_cap, LX_DRM_GUARD_PATH, 0, &mut rbuf) {
            Ok((n, _)) if &rbuf[..n] == LX_DRM_GUARD_TEXT => {}
            _ => persist_ok = false,
        }
    }
    let vault_ok = vault_sup_cap == 0
        || sys_ipc_call_inline(
            vault_sup_cap,
            pack_msg_tag(VAULT_OP_STATUS, 0, 1, 0),
            [0; 4],
            0,
        )
        .is_ok_and(|(vt, _, _)| msg_tag_label(vt) == IPC_OP_PONG);

    if !ep_rev || !ntf_rev || !cq_rev || !ring_rev || !stale_gen || !persist_ok || !vault_ok {
        return Err(299);
    }

    let (_, dp50, dp95, dp99, _) = dir_lat.summary();
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-LX-DRM] PASS: vendor_id=0x");
    lb.push_hex_u64(vendor_id);
    lb.push_bytes(b" device_id=0x");
    lb.push_hex_u64(device_id);
    lb.push_bytes(b" bdf=");
    lb.push_u64(bochs_rec.bus_dev_fn as u64);
    lb.push_bytes(b" slot=");
    lb.push_u64(bochs_rec.managed_slot as u64);
    lb.push_bytes(b" disp_pci=");
    lb.push_u64(disp_pci as u64);
    lb.push_bytes(b" mac_neg=");
    lb.push_u64(u64::from(mac_neg_ok));
    lb.push_bytes(b" ro_dma=");
    lb.push_u64(ro_enforced);
    lb.push_bytes(b" edid_ok=");
    lb.push_u64(u64::from(edid_ok));
    lb.push_bytes(b" bar0_ok=");
    lb.push_u64(u64::from(bar0_ok));
    lb.push_bytes(b" bar2_ok=");
    lb.push_u64(u64::from(bar2_ok));
    log_line(&lb);

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-LX-DRM-RECOV] unauth=");
    rlb.push_u64(unauth_denied);
    rlb.push_bytes(b" vec=");
    rlb.push_u64(fault_vec);
    rlb.push_bytes(b" restarts=");
    rlb.push_u64(restarts);
    rlb.push_bytes(b" ep_rev=1 ntf_rev=1 cq_rev=1 ring_rev=1 stale_gen=1 persist_ok=1 vault_ok=1");
    log_line(&rlb);

    let mut blb = LineBuilder::new();
    blb.push_bytes(b"[DRY-LX-DRM-BENCH] dir_p50=");
    blb.push_u64(dp50);
    blb.push_bytes(b" dir_p95=");
    blb.push_u64(dp95);
    blb.push_bytes(b" dir_p99=");
    blb.push_u64(dp99);
    blb.push_bytes(b" ring32_cyc=");
    blb.push_u64(ring32_cyc);
    blb.push_bytes(b" recov_cyc=");
    blb.push_u64(recov_cyc);
    blb.push_bytes(b" rss_4k=");
    blb.push_u64(lx_rss_4k);
    blb.push_bytes(b" tsc_mhz=");
    blb.push_u64(tsc_mhz);
    log_line(&blb);

    let mut slb = LineBuilder::new();
    slb.push_bytes(b"[DRY-LX-DRM] SCANOUT_READY w=800 h=600 gen=2");
    log_line(&slb);

    Ok(LxDrmLiveSession {
        broker: bh,
        dmabuf_cap,
    })
}

pub fn teardown_live_lx_drm(sess: LxDrmLiveSession) {
    let _ = sess.broker.shutdown_and_reclaim();
    let _ = sys_vspace_map(sess.dmabuf_cap, CLIENT_DMABUF_VADDR, VSPACE_MAP_OP_UNMAP_2M);
    let _ = sys_cap_revoke(sess.dmabuf_cap, 2);
}
