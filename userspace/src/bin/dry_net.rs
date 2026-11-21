// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-net` — Ring-3 TCP/IP Network Stack Service (`0x000c`).

#![no_std]
#![no_main]

use core::cell::UnsafeCell;

use dry_userspace::island_proto::BROKER_OP_ACQUIRE_EP;
use dry_userspace::net::proto::{ip_to_u32, u32_to_ip};
use dry_userspace::net::{NetEngine, NET_OP_HTTPGET, NET_OP_IFCONFIG, NET_OP_NSLOOKUP, NET_OP_PING};
use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_proto::{sys_ipc_recv_extra, sys_ipc_reply_extra};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_with_grant, sys_ipc_reply_inline, sys_sysinfo, sys_vspace_map, sys_write_debug,
    UserIpcRecvExt, UserSysInfo, BADGE_SUPERVISOR, EINVAL, EPERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG,
    MAX_IPC_EXTRA_BYTES, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const NET_RAW_BOUNCE_VADDR: u64 = 0x2420_0000;
const NET_OP_ERR: u32 = 0x69;

struct NetCell(UnsafeCell<NetEngine>);
// SAFETY: `dry-net` executes strictly single-threaded in CPL=3.
unsafe impl Sync for NetCell {}

static NET_ENGINE: NetCell = NetCell(UnsafeCell::new(NetEngine::new()));

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
fn reply_err(ep: u64, tok: u64, err: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(NET_OP_ERR, 0, 1, 0),
            [err as u64, 0, 0, 0],
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(net_ep: u64, broker_cli_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || net_ep == 0 || broker_cli_cap == 0 {
        sys_exit(190);
    }

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);

    let (drv_ep, drv_gen) = match sys_ipc_call_with_grant(
        broker_cli_cap,
        BROKER_OP_ACQUIRE_EP,
        [0; 4],
        0,
        0,
        0,
        0,
    ) {
        Ok((t, ep, mrs)) if msg_tag_label(t) == IPC_OP_PONG && ep != 0 => (ep, mrs[0].max(1)),
        _ => {
            let _ = sys_cap_revoke(broker_cli_cap, 1);
            sys_exit(190);
        }
    };
    let _ = sys_cap_revoke(broker_cli_cap, 1);

    let raw_cap = sys_frame_alloc(0).unwrap_or_else(|_| sys_exit(191));
    if sys_vspace_map(raw_cap, NET_RAW_BOUNCE_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        let _ = sys_cap_revoke(drv_ep, 1);
        sys_exit(192);
    }

    // SAFETY: Single-threaded service exclusively owns `NET_ENGINE`.
    let eng = unsafe { &mut *NET_ENGINE.0.get() };
    if eng.init(drv_ep, raw_cap, NET_RAW_BOUNCE_VADDR, drv_gen, sys.tsc_mhz).is_err() {
        let _ = sys_vspace_map(raw_cap, NET_RAW_BOUNCE_VADDR, VSPACE_MAP_OP_UNMAP_4K);
        let _ = sys_cap_revoke(raw_cap, 2);
        let _ = sys_cap_revoke(drv_ep, 1);
        sys_exit(193);
    }

    let mut blb = LineBuilder::new();
    blb.push_bytes(b"[DRY-NET] Ready in CPL3 Userspace dhcp=");
    blb.push_u64(u64::from(eng.dhcp_ok));
    blb.push_bytes(b" ip=");
    for (i, &b) in eng.ip.iter().enumerate() {
        if i > 0 {
            blb.push_byte(b'.');
        }
        blb.push_u64(u64::from(b));
    }
    log_line(&blb);

    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, rx_len, mrs) = match sys_ipc_recv_extra(net_ep, 0, 0, &mut rx, &mut ext) {
            Ok(v) => v,
            Err(_) => break,
        };
        let reply_tok = ext.reply_token;
        if ext.transferred_cap != 0 {
            let _ = sys_cap_revoke(ext.transferred_cap, 1);
        }
        let op = msg_tag_label(tag);

        match op {
            NET_OP_IFCONFIG => {
                let mac_packed = (0..6).fold(0u64, |acc, i| acc | (u64::from(eng.mac[i]) << (i * 8)));
                let ip_gw = (ip_to_u32(eng.ip) as u64) | ((ip_to_u32(eng.gw) as u64) << 32);
                let dns_flags = (ip_to_u32(eng.dns) as u64)
                    | (u64::from(eng.dhcp_ok) << 32)
                    | (u64::from(eng.link_up) << 33);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        net_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [mac_packed, ip_gw, dns_flags, ip_to_u32(eng.mask) as u64],
                    );
                }
            }
            NET_OP_PING => {
                let dst = u32_to_ip(mrs[0] as u32);
                match eng.ping(dst, mrs[1].max(1) as u32) {
                    Ok((ok_cnt, rtt_us)) => {
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                net_ep,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                                [u64::from(ok_cnt), rtt_us, 0, 0],
                            );
                        }
                    }
                    Err(e) => reply_err(net_ep, reply_tok, e),
                }
            }
            NET_OP_NSLOOKUP => {
                let host = &rx[..rx_len];
                let dns_ip = u32_to_ip(mrs[0] as u32);
                let dns_port = mrs[1] as u16;
                match eng.nslookup(host, dns_ip, dns_port) {
                    Ok((ip, ttl, us)) => {
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                net_ep,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                [ip_to_u32(ip) as u64, u64::from(ttl), us, 0],
                            );
                        }
                    }
                    Err(e) => reply_err(net_ep, reply_tok, e),
                }
            }
            NET_OP_HTTPGET => {
                if rx_len < 2 {
                    reply_err(net_ep, reply_tok, EINVAL);
                    continue;
                }
                let hlen = rx[0] as usize;
                let plen = rx[1] as usize;
                if 2 + hlen + plen > rx_len {
                    reply_err(net_ep, reply_tok, EINVAL);
                    continue;
                }
                let host = &rx[2..(2 + hlen)];
                let path = &rx[(2 + hlen)..(2 + hlen + plen)];
                let mut srv_ip = u32_to_ip(mrs[0] as u32);
                let port = mrs[1] as u16;
                let dns_port = mrs[2] as u16;
                if srv_ip == [0; 4] {
                    match eng.nslookup(host, [0; 4], dns_port) {
                        Ok((resolved, _, _)) => srv_ip = resolved,
                        Err(e) => {
                            reply_err(net_ep, reply_tok, e);
                            continue;
                        }
                    }
                }
                match eng.http_get(srv_ip, port, host, path) {
                    Ok((status, body_off, rx_total, us)) => {
                        let body = &eng.http_buf[body_off..rx_total];
                        let preview_n = body.len().min(MAX_IPC_EXTRA_BYTES);
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_extra(
                                net_ep,
                                reply_tok,
                                IPC_OP_PONG,
                                [
                                    u64::from(status),
                                    body.len() as u64,
                                    ip_to_u32(srv_ip) as u64,
                                    us,
                                ],
                                &body[..preview_n],
                            );
                        }
                    }
                    Err(e) => reply_err(net_ep, reply_tok, e),
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    reply_err(net_ep, reply_tok, EPERM);
                    continue;
                }
                let _ = sys_vspace_map(raw_cap, NET_RAW_BOUNCE_VADDR, VSPACE_MAP_OP_UNMAP_4K);
                let _ = sys_cap_revoke(raw_cap, 2);
                let _ = sys_cap_revoke(drv_ep, 1);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        net_ep,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => reply_err(net_ep, reply_tok, EINVAL),
        }
    }
    sys_exit(0);
}
