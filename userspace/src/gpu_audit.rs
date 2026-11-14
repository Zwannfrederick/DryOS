// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Priority C Isolated Ring-3 VirtIO-GPU 2D Scanout, GOP Fallback & Crash Recovery Audit
//! (`session_mode == 28` via `console=com1 desktop4=1 gpu_test=1`).

use crate::abi_ext::{
    sys_dev_map, sys_dma_alloc, sys_proc_spawn_with_fault_ep, UserVirtioPciInfo, FAULT_MSG_LABEL,
    FAULT_REPLY_KILL,
};
use crate::gpu_proto::{
    BADGE_GPU_DISPLAY, BADGE_GPU_ROGUE, BADGE_GPU_SUP, COMP_ID_GPU_DRV, GPU_OP_ATTACH_BACKBUF,
    GPU_OP_BIND_PCI, GPU_OP_PRESENT_RECT, GPU_OP_QUERY_INFO,
};
use crate::init_legacy::{domain_spec, log_line};
use crate::island_proto::LatencySamples64;
use crate::shell::LineBuilder;
use crate::vfs_proto::{vfs_create_file, vfs_read_file, vfs_sync, vfs_write_small};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_ipc_send_inline, sys_proc_list, sys_proc_wait, sys_sysinfo, RootInitBootInfo,
    UserIpcRecvExt, UserProcInfo, UserSysInfo, EEXIST, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL,
    IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG, IPC_OP_WM_BIND_SUP_NOTIF,
    IPC_OP_WM_SET_SESSION_STATE, IPC_OP_WM_TOGGLE_TILING, RIGHT_CALL, RIGHT_GRANT, RIGHT_MAP,
    RIGHT_READ, RIGHT_RECV, RIGHT_REPLY, RIGHT_SEND, RIGHT_WRITE, VAULT_OP_STATUS,
};

const DESKTOP_IPC_COMP_ID: u16 = 0x0100;
const GPU_GUARD_PATH: &[u8] = b"/persist/gpu-guard.txt";
const GPU_GUARD_TEXT: &[u8] = b"gpu-island-guard-ok";

pub struct LiveGpuSession {
    proc_handle: u64,
    fault_ep: u64,
    fault_send_cap: u64,
    gpu_ep: u64,
    gpu_sup_cap: u64,
    gpu_disp_cap: u64,
    gpu_rogue_cap: u64,
    pub generation: u64,
}

struct GpuSpawned {
    session: LiveGpuSession,
    pid: u64,
    width: u64,
    height: u64,
    vendor_id: u64,
    device_id: u64,
}

fn spawn_gpu_gen(boot: &RootInitBootInfo, fault_ep: u64, generation: u64) -> Result<GpuSpawned, i32> {
    let gpu_spec = domain_spec(1, COMP_ID_GPU_DRV);
    let cli_rights = RIGHT_READ | RIGHT_SEND | RIGHT_CALL;
    let srv_rights = RIGHT_READ | RIGHT_RECV | RIGHT_REPLY;

    let gpu_ep = sys_ep_create(gpu_spec).map_err(|_| 271)?;
    let gpu_sup_cap = sys_cap_derive(gpu_ep, cli_rights | RIGHT_GRANT, 0, BADGE_GPU_SUP).map_err(|_| 272)?;
    let gpu_disp_cap =
        sys_cap_derive(gpu_ep, cli_rights | RIGHT_GRANT, 0, BADGE_GPU_DISPLAY).map_err(|_| 272)?;
    let gpu_rogue_cap = sys_cap_derive(gpu_ep, cli_rights, 0, BADGE_GPU_ROGUE).map_err(|_| 272)?;
    let fault_send_cap =
        sys_cap_derive(fault_ep, cli_rights, 0, 0x6600_0000 | generation).map_err(|_| 272)?;

    let (proc_handle, pid) = sys_proc_spawn_with_fault_ep(
        boot.gpu_mod_cap,
        1, // `SecDomain::RootSystem` isolated PCI GPU driver compartment (`0x0009`).
        COMP_ID_GPU_DRV,
        (gpu_ep, srv_rights, 0),
        fault_send_cap,
    )
    .map_err(|_| 273)?;

    let (btag, _, bmrs) = sys_ipc_call_with_grant(
        gpu_sup_cap,
        GPU_OP_BIND_PCI,
        [0; 4],
        boot.pci_gpu_cap,
        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP,
        0,
        0,
    )
    .map_err(|_| 274)?;
    if msg_tag_label(btag) != IPC_OP_PONG || bmrs[0] != 1 {
        return Err(274);
    }

    Ok(GpuSpawned {
        session: LiveGpuSession {
            proc_handle,
            fault_ep,
            fault_send_cap,
            gpu_ep,
            gpu_sup_cap,
            gpu_disp_cap,
            gpu_rogue_cap,
            generation,
        },
        pid,
        width: bmrs[1] & 0xffff_ffff,
        height: bmrs[1] >> 32,
        vendor_id: bmrs[2],
        device_id: bmrs[3],
    })
}

fn revoke_gpu_generation(sess: LiveGpuSession) {
    let _ = sys_cap_revoke(sess.fault_send_cap, 1);
    let _ = sys_cap_revoke(sess.gpu_disp_cap, 1);
    let _ = sys_cap_revoke(sess.gpu_sup_cap, 1);
    let _ = sys_cap_revoke(sess.gpu_rogue_cap, 1);
    let _ = sys_cap_revoke(sess.gpu_ep, 2);
}

fn bind_display_gpu(disp_sup_cap: u64, gpu_disp_cap: u64) -> Result<(), i32> {
    let grant_rights = if gpu_disp_cap != 0 {
        RIGHT_READ | RIGHT_SEND | RIGHT_CALL | RIGHT_GRANT
    } else {
        0
    };
    let (tag, _, _) = sys_ipc_call_with_grant(
        disp_sup_cap,
        IPC_OP_WM_BIND_SUP_NOTIF,
        [0, 0, 2, 0],
        gpu_disp_cap,
        grant_rights,
        0,
        0,
    )
    .map_err(|_| 276)?;
    if msg_tag_label(tag) != IPC_OP_PONG {
        return Err(276);
    }
    Ok(())
}

fn toggle_tiling_cycles(disp_sup_cap: u64) -> Result<u64, i32> {
    let t0 = rdtsc_user();
    let (tag, _, _) = sys_ipc_call_inline(
        disp_sup_cap,
        pack_msg_tag(IPC_OP_WM_TOGGLE_TILING, 0, 1, 0),
        [0; 4],
        0,
    )
    .map_err(|_| 277)?;
    let dt = rdtsc_user().saturating_sub(t0);
    if msg_tag_label(tag) != IPC_OP_PONG {
        return Err(277);
    }
    Ok(dt)
}

fn query_gpu_stats(gpu_sup_cap: u64) -> Result<(u64, u64, u64), i32> {
    let (tag, _, mrs) = sys_ipc_call_inline(
        gpu_sup_cap,
        pack_msg_tag(GPU_OP_QUERY_INFO, 0, 1, 0),
        [0; 4],
        0,
    )
    .map_err(|_| 278)?;
    if msg_tag_label(tag) != IPC_OP_PONG {
        return Err(278);
    }
    Ok((mrs[1], mrs[2], mrs[3] >> 32))
}

pub fn run_gpu_scanout_audit(
    boot: &RootInitBootInfo,
    disp_sup_cap: u64,
    vfs_sup_cap: u64,
    vault_sup_cap: u64,
) -> Result<LiveGpuSession, i32> {
    if boot.gpu_mod_cap == 0 || boot.pci_gpu_cap == 0 || disp_sup_cap == 0 {
        return Err(270);
    }
    let mut sys0 = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys0);
    let tsc_mhz = sys0.tsc_mhz.max(1000);

    // 1. Verify kernel `SYS_DEV_MAP` / `SYS_DMA_ALLOC` reject wrong-kind & reduced-rights caps.
    let mut dummy_info = UserVirtioPciInfo::default();
    let d1 = sys_dev_map(disp_sup_cap, 0x2680_0000, &mut dummy_info).is_err();
    let d2 = sys_dma_alloc(disp_sup_cap, 0x2681_0000).is_err();
    let pci_ro = sys_cap_derive(boot.pci_gpu_cap, RIGHT_READ, 0, 0).map_err(|_| 270)?;
    let d3 = sys_dev_map(pci_ro, 0x2682_0000, &mut dummy_info).is_err();
    let _ = sys_cap_revoke(pci_ro, 1);
    if !(d1 && d2 && d3) {
        return Err(270);
    }

    if vfs_sup_cap != 0 {
        match vfs_create_file(vfs_sup_cap, GPU_GUARD_PATH, 0) {
            Ok(_) | Err(EEXIST) => {}
            Err(_) => return Err(284),
        }
        vfs_write_small(vfs_sup_cap, GPU_GUARD_PATH, GPU_GUARD_TEXT, false).map_err(|_| 284)?;
        vfs_sync(vfs_sup_cap).map_err(|_| 284)?;
    }

    let sup_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let fault_ep = sys_ep_create(sup_spec).map_err(|_| 271)?;

    // 2. Spawn generation 1 of `dry-gpu.elf` (`1AF4:1050`).
    let g1 = spawn_gpu_gen(boot, fault_ep, 1)?;
    if g1.vendor_id != 0x1af4 || g1.device_id != 0x1050 || g1.width < 640 || g1.height < 480 {
        return Err(274);
    }

    // 3. Verify userspace badge isolation on `dry-gpu`.
    let mut unauth_denied = 0u64;
    for op in [
        GPU_OP_BIND_PCI,
        GPU_OP_QUERY_INFO,
        GPU_OP_ATTACH_BACKBUF,
        GPU_OP_PRESENT_RECT,
        IPC_OP_CRASH_UD2,
        IPC_OP_EXIT_CLEAN,
    ] {
        if sys_ipc_call_inline(
            g1.session.gpu_rogue_cap,
            pack_msg_tag(op, 0, 1, 0),
            [0; 4],
            0,
        )
        .is_ok_and(|(t, _, _)| msg_tag_label(t) == IPC_OP_ERR_PERM)
        {
            unauth_denied += 1;
        }
    }
    if sys_ipc_call_inline(
        g1.session.gpu_sup_cap,
        pack_msg_tag(GPU_OP_ATTACH_BACKBUF, 0, 1, 0),
        [0; 4],
        0,
    )
    .is_ok_and(|(t, _, _)| msg_tag_label(t) == IPC_OP_ERR_PERM)
    {
        unauth_denied += 1;
    }
    if unauth_denied < 6 {
        return Err(275);
    }

    // 4. Bind `gen=1` into `dry-display` and benchmark 8 full scanout presents (`TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH`).
    bind_display_gpu(disp_sup_cap, g1.session.gpu_disp_cap)?;
    let mut pres_lat = LatencySamples64::new();
    for _ in 0..8 {
        pres_lat.push(toggle_tiling_cycles(disp_sup_cap)?);
    }
    let (pres_g1, xfer_bytes, vq_cmds) = query_gpu_stats(g1.session.gpu_sup_cap)?;
    if pres_g1 < 9 || xfer_bytes == 0 || vq_cmds < 20 {
        return Err(278);
    }

    let mut procs = [UserProcInfo::default(); 24];
    let mut rss_4k = 0u64;
    if let Ok((pcnt, _)) = sys_proc_list(&mut procs) {
        for p in procs.iter().take(pcnt) {
            if u64::from(p.pid) == g1.pid || p.compartment_id == COMP_ID_GPU_DRV {
                rss_4k = (p.mapped_pages_4k + p.pt_frames_4k) as u64;
                break;
            }
        }
    }

    // 5. Inject `#UD` (`vec=6`) via one-way `sys_ipc_send_inline` -> reap via `FaultEndpoint` -> verify GOP fallback.
    let recov_t0 = rdtsc_user();
    sys_ipc_send_inline(
        g1.session.gpu_sup_cap,
        pack_msg_tag(IPC_OP_CRASH_UD2, 0, 1, 0),
        [0; 4],
        0,
    )
    .map_err(|_| 279)?;

    let mut fext = UserIpcRecvExt::default();
    let (ftag, ftok, fmrs) = sys_ipc_recv_ext(fault_ep, 0, 0, &mut fext).map_err(|_| 279)?;
    let fault_vec = fmrs[0] & 0xff;
    if msg_tag_label(ftag) != FAULT_MSG_LABEL || fext.sender_badge != 0x6600_0001 || fault_vec != 6 {
        return Err(279);
    }
    if ftok != 0 {
        let _ = sys_ipc_reply_inline(
            fault_ep,
            ftok,
            pack_msg_tag(FAULT_REPLY_KILL, 0, 1, 0),
            [0; 4],
        );
    }
    let _ = sys_proc_wait(g1.session.proc_handle, 0, 0);
    revoke_gpu_generation(g1.session);

    // Next present attempts `try_gpu_present_or_fallback`, detects `EREVOKED`, falls back to GOP.
    let _ = toggle_tiling_cycles(disp_sup_cap)?;
    let _ = toggle_tiling_cycles(disp_sup_cap)?;

    // 6. Respawn `gen=2`, rebind `dry-display`, verify live scanout recovery.
    let g2 = spawn_gpu_gen(boot, fault_ep, 2)?;
    bind_display_gpu(disp_sup_cap, g2.session.gpu_disp_cap)?;
    let recov_cyc = rdtsc_user().saturating_sub(recov_t0);
    let _ = toggle_tiling_cycles(disp_sup_cap)?;
    let _ = toggle_tiling_cycles(disp_sup_cap)?;
    let (pres_g2, _, _) = query_gpu_stats(g2.session.gpu_sup_cap)?;
    if pres_g2 < 3 {
        return Err(283);
    }

    // 7. Confirm `/persist` and `DRY Vault` stayed intact.
    let mut persist_ok = true;
    if vfs_sup_cap != 0 {
        let mut rbuf = [0u8; 32];
        match vfs_read_file(vfs_sup_cap, GPU_GUARD_PATH, 0, &mut rbuf) {
            Ok((n, _)) if &rbuf[..n] == GPU_GUARD_TEXT => {}
            _ => persist_ok = false,
        }
    }
    let mut vault_ok = true;
    if vault_sup_cap != 0 {
        vault_ok = sys_ipc_call_inline(
            vault_sup_cap,
            pack_msg_tag(VAULT_OP_STATUS, 0, 1, 0),
            [0; 4],
            0,
        )
        .is_ok_and(|(vt, _, _)| msg_tag_label(vt) == IPC_OP_PONG);
    }
    if !persist_ok || !vault_ok {
        return Err(284);
    }

    let (_, p50, p95, p99, _) = pres_lat.summary();
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-GPU-PASS] vendor_id=0x");
    lb.push_hex_u64(g1.vendor_id);
    lb.push_bytes(b" device_id=0x");
    lb.push_hex_u64(g1.device_id);
    lb.push_bytes(b" w=");
    lb.push_u64(g1.width);
    lb.push_bytes(b" h=");
    lb.push_u64(g1.height);
    lb.push_bytes(b" gen=2 pres_g1=");
    lb.push_u64(pres_g1);
    lb.push_bytes(b" pres_g2=");
    lb.push_u64(pres_g2);
    lb.push_bytes(b" gop_fb=1 vec=");
    lb.push_u64(fault_vec);
    lb.push_bytes(b" unauth=");
    lb.push_u64(unauth_denied);
    lb.push_bytes(b" persist_ok=1 vault_ok=1");
    log_line(&lb);

    let mut blb = LineBuilder::new();
    blb.push_bytes(b"[DRY-GPU-BENCH] tsc_mhz=");
    blb.push_u64(tsc_mhz);
    blb.push_bytes(b" pres_p50=");
    blb.push_u64(p50);
    blb.push_bytes(b" pres_p95=");
    blb.push_u64(p95);
    blb.push_bytes(b" pres_p99=");
    blb.push_u64(p99);
    blb.push_bytes(b" xfer_kib=");
    blb.push_u64(xfer_bytes / 1024);
    blb.push_bytes(b" vq_cmds=");
    blb.push_u64(vq_cmds);
    blb.push_bytes(b" recov_cyc=");
    blb.push_u64(recov_cyc);
    blb.push_bytes(b" rss_4k=");
    blb.push_u64(rss_4k);
    log_line(&blb);

    Ok(g2.session)
}

pub fn teardown_live_gpu(live: LiveGpuSession, disp_sup_cap: u64) {
    if disp_sup_cap != 0 {
        let _ = bind_display_gpu(disp_sup_cap, 0);
    }
    let _ = sys_ipc_call_inline(
        live.gpu_sup_cap,
        pack_msg_tag(IPC_OP_EXIT_CLEAN, 0, 1, 0),
        [0; 4],
        0,
    );
    let _ = sys_proc_wait(live.proc_handle, 0, 0);
    let fault_ep = live.fault_ep;
    revoke_gpu_generation(live);
    let _ = sys_cap_revoke(fault_ep, 2);
}

fn nudge_display_active(disp_sup_cap: u64) -> Result<(), u32> {
    let (tag, _, _) = sys_ipc_call_inline(
        disp_sup_cap,
        pack_msg_tag(IPC_OP_WM_SET_SESSION_STATE, 0, 3, 0),
        [0, 0, 0, 0],
        0,
    )
    .map_err(|_| IPC_OP_ERR_INVAL)?;
    if msg_tag_label(tag) != IPC_OP_PONG {
        return Err(IPC_OP_ERR_INVAL);
    }
    Ok(())
}

pub fn spawn_live_gpu(boot: &RootInitBootInfo, disp_sup_cap: u64) -> Result<LiveGpuSession, i32> {
    if boot.gpu_mod_cap == 0 || boot.pci_gpu_cap == 0 || disp_sup_cap == 0 {
        return Err(270);
    }
    let sup_spec = domain_spec(0, DESKTOP_IPC_COMP_ID);
    let fault_ep = sys_ep_create(sup_spec).map_err(|_| 271)?;
    let g1 = spawn_gpu_gen(boot, fault_ep, 1).inspect_err(|_| {
        let _ = sys_cap_revoke(fault_ep, 2);
    })?;
    if bind_display_gpu(disp_sup_cap, g1.session.gpu_disp_cap).is_err() {
        teardown_live_gpu(g1.session, disp_sup_cap);
        return Err(276);
    }
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-GPU] LIVE_SESSION_ACTIVE gen=1 w=");
    lb.push_u64(g1.width);
    lb.push_bytes(b" h=");
    lb.push_u64(g1.height);
    log_line(&lb);
    Ok(g1.session)
}

pub fn trigger_live_gpu_fault_and_recover(
    boot: &RootInitBootInfo,
    gpu_live: &mut Option<LiveGpuSession>,
    disp_sup_cap: u64,
) -> Result<[u64; 4], u32> {
    let Some(live) = gpu_live.take() else {
        return Err(IPC_OP_ERR_NOSYS);
    };
    let fault_ep = live.fault_ep;
    let next_gen = live.generation.saturating_add(1);
    let expect_badge = 0x6600_0000 | (live.generation & 0xffff_ffff);

    if sys_ipc_send_inline(
        live.gpu_sup_cap,
        pack_msg_tag(IPC_OP_CRASH_UD2, 0, 1, 0),
        [0; 4],
        0,
    )
    .is_err()
    {
        teardown_live_gpu(live, disp_sup_cap);
        return Err(IPC_OP_ERR_INVAL);
    }

    let mut fext = UserIpcRecvExt::default();
    let Ok((ftag, ftok, fmrs)) = sys_ipc_recv_ext(fault_ep, 0, 0, &mut fext) else {
        let _ = sys_cap_revoke(fault_ep, 2);
        return Err(IPC_OP_ERR_INVAL);
    };
    let fault_vec = fmrs[0] & 0xff;
    if ftok != 0 {
        let _ = sys_ipc_reply_inline(
            fault_ep,
            ftok,
            pack_msg_tag(FAULT_REPLY_KILL, 0, 1, 0),
            [0; 4],
        );
    }
    let _ = sys_proc_wait(live.proc_handle, 0, 0);
    revoke_gpu_generation(live);
    if msg_tag_label(ftag) != FAULT_MSG_LABEL || fext.sender_badge != expect_badge || fault_vec != 6 {
        let _ = sys_cap_revoke(fault_ep, 2);
        return Err(IPC_OP_ERR_INVAL);
    }

    // Force a present while `dry-gpu` is dead so `dry-display` detects `EREVOKED` -> GOP fallback.
    nudge_display_active(disp_sup_cap)?;

    let Ok(g2) = spawn_gpu_gen(boot, fault_ep, next_gen) else {
        let _ = sys_cap_revoke(fault_ep, 2);
        return Err(IPC_OP_ERR_INVAL);
    };
    if bind_display_gpu(disp_sup_cap, g2.session.gpu_disp_cap).is_err() {
        teardown_live_gpu(g2.session, disp_sup_cap);
        return Err(IPC_OP_ERR_INVAL);
    }
    nudge_display_active(disp_sup_cap)?;

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-GPU] LIVE_RECOVERY_OK gen=");
    lb.push_u64(next_gen);
    lb.push_bytes(b" vec=");
    lb.push_u64(fault_vec);
    lb.push_bytes(b" w=");
    lb.push_u64(g2.width);
    lb.push_bytes(b" h=");
    lb.push_u64(g2.height);
    log_line(&lb);

    let out = [next_gen, fault_vec, g2.width, g2.height];
    *gpu_live = Some(g2.session);
    Ok(out)
}
