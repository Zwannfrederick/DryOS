// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring-3 TCP/IP Network Stack (`dry-net`) IPC Protocol, Engine & Client SDK.

pub mod dhcp_dns;
pub mod ipv4;
pub mod proto;
pub mod tcp;

use crate::lx_drv_proto::{
    LX_NET_OP_ATTACH_RAW_PAGE, LX_NET_OP_QUERY_INFO, LX_NET_OP_RAW_RX, LX_NET_OP_RAW_TX,
};
use crate::net::dhcp_dns::{
    build_dhcp_packet, build_dns_a_query, parse_dhcp_reply, parse_dns_a_reply, DHCP_CLIENT_PORT,
    DHCP_SERVER_PORT,
};
use crate::net::ipv4::{build_icmp_echo, build_udp_frame, match_icmp_echo_reply, parse_ipv4_frame, parse_udp_payload};
use crate::net::proto::{
    build_arp_packet, ip_to_u32, process_arp, u32_to_ip, ArpCache, ZERO_MAC,
};
use crate::net::tcp::{
    build_http_get_req, build_tcp_frame, parse_http_status_and_body, parse_tcp_segment, TCP_ACK,
    TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN,
};
use crate::vfs_proto::{sys_ipc_call_extra, sys_ipc_call_extra_grant};
use crate::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_ipc_call_inline, sys_ipc_call_with_grant,
    sys_yield, EINVAL, EIO, ETIMEDOUT, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES, RIGHT_MAP, RIGHT_READ,
    RIGHT_WRITE,
};

pub const COMP_ID_NET_SVC: u16 = 0x000c;

pub const NET_OP_IFCONFIG: u32 = 211;
pub const NET_OP_PING: u32 = 212;
pub const NET_OP_NSLOOKUP: u32 = 213;
pub const NET_OP_HTTPGET: u32 = 214;

pub const HTTP_RX_CAP: usize = 4096;

pub struct NetEngine {
    pub drv_ep: u64,
    pub raw_cap: u64,
    pub raw_va: u64,
    pub drv_gen: u64,
    pub tsc_mhz: u64,
    pub mac: [u8; 6],
    pub ip: [u8; 4],
    pub mask: [u8; 4],
    pub gw: [u8; 4],
    pub dns: [u8; 4],
    pub dhcp_ok: bool,
    pub link_up: bool,
    pub arp: ArpCache,
    pub ip_id: u16,
    pub next_port: u16,
    pub tx_buf: [u8; 1600],
    pub rx_buf: [u8; 1600],
    pub http_buf: [u8; HTTP_RX_CAP],
}

impl NetEngine {
    pub const fn new() -> Self {
        Self {
            drv_ep: 0,
            raw_cap: 0,
            raw_va: 0,
            drv_gen: 1,
            tsc_mhz: 2000,
            mac: ZERO_MAC,
            ip: [10, 0, 2, 15],
            mask: [255, 255, 255, 0],
            gw: [10, 0, 2, 2],
            dns: [10, 0, 2, 3],
            dhcp_ok: false,
            link_up: false,
            arp: ArpCache::new(),
            ip_id: 1,
            next_port: 41000,
            tx_buf: [0; 1600],
            rx_buf: [0; 1600],
            http_buf: [0; HTTP_RX_CAP],
        }
    }

    pub fn init(&mut self, drv_ep: u64, raw_cap: u64, raw_va: u64, drv_gen: u64, tsc_mhz: u64) -> Result<(), isize> {
        self.drv_ep = drv_ep;
        self.raw_cap = raw_cap;
        self.raw_va = raw_va;
        self.drv_gen = drv_gen;
        self.tsc_mhz = tsc_mhz.max(1000);

        let (qtag, _, qmrs) = sys_ipc_call_inline(
            drv_ep,
            pack_msg_tag(LX_NET_OP_QUERY_INFO, 0, 4, 0),
            [0, 0, 0, drv_gen],
            0,
        )?;
        if msg_tag_label(qtag) != IPC_OP_PONG {
            return Err(EIO);
        }
        for (i, b) in self.mac.iter_mut().enumerate() {
            *b = ((qmrs[0] >> (i * 8)) & 0xff) as u8;
        }
        self.link_up = (qmrs[1] & 0xff) != 0;

        let (atag, _, _) = sys_ipc_call_with_grant(
            drv_ep,
            LX_NET_OP_ATTACH_RAW_PAGE,
            [1, 0, 0, drv_gen],
            raw_cap,
            RIGHT_READ | RIGHT_WRITE | RIGHT_MAP,
            0,
            0,
        )?;
        if msg_tag_label(atag) != IPC_OP_PONG {
            return Err(EINVAL);
        }

        let _ = self.try_dhcp();
        Ok(())
    }

    fn alloc_ip_id(&mut self) -> u16 {
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1).max(1);
        id
    }

    fn alloc_ephemeral_port(&mut self) -> u16 {
        let p = self.next_port;
        self.next_port = if p >= 60000 { 41000 } else { p + 1 };
        p
    }

    pub fn tx_raw(&self, frame: &[u8]) -> Result<(), isize> {
        let len = frame.len();
        if self.raw_va == 0 || !(14..=1514).contains(&len) {
            return Err(EINVAL);
        }
        // SAFETY: `raw_va` is an owned 4 KiB RW bounce page (`0..2048` for TX) shared with `dry-lx-net`.
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), self.raw_va as *mut u8, len);
        }
        let (tag, _, _) = sys_ipc_call_inline(
            self.drv_ep,
            pack_msg_tag(LX_NET_OP_RAW_TX, 0, 4, 0),
            [len as u64, 0, 0, self.drv_gen],
            0,
        )?;
        if msg_tag_label(tag) == IPC_OP_PONG { Ok(()) } else { Err(EINVAL) }
    }

    pub fn rx_raw(&mut self) -> usize {
        if self.raw_va == 0 {
            return 0;
        }
        let Ok((tag, _, mrs)) = sys_ipc_call_inline(
            self.drv_ep,
            pack_msg_tag(LX_NET_OP_RAW_RX, 0, 4, 0),
            [0, 0, 0, self.drv_gen],
            0,
        ) else {
            return 0;
        };
        if msg_tag_label(tag) != IPC_OP_PONG {
            return 0;
        }
        let n = (mrs[0] as usize).min(self.rx_buf.len());
        if n > 0 {
            // SAFETY: `raw_va + 2048` holds valid received frame bytes written by `dry-lx-net`.
            unsafe {
                core::ptr::copy_nonoverlapping((self.raw_va + 2048) as *const u8, self.rx_buf.as_mut_ptr(), n);
            }
            let mut arp_rep = [0u8; 60];
            if let Some(rlen) = process_arp(&self.rx_buf[..n], self.mac, self.ip, &mut self.arp, &mut arp_rep) {
                let _ = self.tx_raw(&arp_rep[..rlen]);
            }
        }
        n
    }

    fn try_dhcp(&mut self) -> bool {
        let xid = 0x4452_5931u32;
        let mut tx = [0u8; 512];
        let Some(dlen) = build_dhcp_packet(&mut tx, self.mac, xid, false, [0; 4], [0; 4]) else {
            return false;
        };
        if self.tx_raw(&tx[..dlen]).is_err() {
            return false;
        }
        let deadline = rdtsc_user().saturating_add(self.tsc_mhz.saturating_mul(600_000));
        let mut offered = None;
        while rdtsc_user() < deadline {
            let n = self.rx_raw();
            if n == 0 {
                let _ = sys_yield();
                continue;
            }
            let frame = &self.rx_buf[..n];
            if let Some(v) = parse_ipv4_frame(frame) {
                if let Some((sp, dp, upay)) = parse_udp_payload(&v) {
                    if sp == DHCP_SERVER_PORT && dp == DHCP_CLIENT_PORT {
                        if let Some(info) = parse_dhcp_reply(upay, xid, self.mac) {
                            if info.msg_type == 2 {
                                self.arp.insert(info.server_ip, v.src_mac);
                                offered = Some(info);
                                break;
                            }
                        }
                    }
                }
            }
        }
        let Some(offer) = offered else {
            return false;
        };
        let Some(rlen) = build_dhcp_packet(&mut tx, self.mac, xid, true, offer.your_ip, offer.server_ip) else {
            return false;
        };
        if self.tx_raw(&tx[..rlen]).is_err() {
            return false;
        }
        let ack_dl = rdtsc_user().saturating_add(self.tsc_mhz.saturating_mul(600_000));
        while rdtsc_user() < ack_dl {
            let n = self.rx_raw();
            if n == 0 {
                let _ = sys_yield();
                continue;
            }
            if let Some(v) = parse_ipv4_frame(&self.rx_buf[..n]) {
                if let Some((sp, dp, upay)) = parse_udp_payload(&v) {
                    if sp == DHCP_SERVER_PORT && dp == DHCP_CLIENT_PORT {
                        if let Some(ack) = parse_dhcp_reply(upay, xid, self.mac) {
                            if ack.msg_type == 5 && ack.your_ip != [0; 4] {
                                self.ip = ack.your_ip;
                                self.mask = ack.netmask;
                                self.gw = ack.router_ip;
                                self.dns = ack.dns_ip;
                                self.dhcp_ok = true;
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    fn next_hop_for(&self, dst_ip: [u8; 4]) -> [u8; 4] {
        let same_net = (0..4).all(|i| (dst_ip[i] & self.mask[i]) == (self.ip[i] & self.mask[i]));
        if same_net { dst_ip } else { self.gw }
    }

    pub fn resolve_next_hop_mac(&mut self, dst_ip: [u8; 4]) -> Option<[u8; 6]> {
        let hop = self.next_hop_for(dst_ip);
        if let Some(mac) = self.arp.lookup(hop) {
            return Some(mac);
        }
        for _ in 0..3 {
            let mut pkt = [0u8; 60];
            let len = build_arp_packet(&mut pkt, 1, self.mac, self.ip, [0; 6], hop);
            let _ = self.tx_raw(&pkt[..len]);
            let dl = rdtsc_user().saturating_add(self.tsc_mhz.saturating_mul(250_000));
            while rdtsc_user() < dl {
                let _ = self.rx_raw();
                if let Some(mac) = self.arp.lookup(hop) {
                    return Some(mac);
                }
                let _ = sys_yield();
            }
        }
        if hop != self.gw {
            return self.arp.lookup(self.gw);
        }
        None
    }

    pub fn ping(&mut self, dst_ip: [u8; 4], count: u32) -> Result<(u32, u64), isize> {
        let dst_mac = self.resolve_next_hop_mac(dst_ip).ok_or(ETIMEDOUT)?;
        let count = count.clamp(1, 4);
        let mut ok_cnt = 0u32;
        let mut total_us = 0u64;
        for seq in 1..=(count as u16) {
            let t0 = rdtsc_user();
            let ip_id = self.alloc_ip_id();
            let Some(flen) = build_icmp_echo(
                &mut self.tx_buf,
                dst_mac,
                self.mac,
                self.ip,
                dst_ip,
                ip_id,
                8,
                0x4452,
                seq,
                b"DRYOS-PING",
            ) else {
                return Err(EINVAL);
            };
            let _ = self.tx_raw(&self.tx_buf[..flen]);
            let dl = t0.saturating_add(self.tsc_mhz.saturating_mul(600_000));
            while rdtsc_user() < dl {
                let n = self.rx_raw();
                if n == 0 {
                    let _ = sys_yield();
                    continue;
                }
                if let Some(v) = parse_ipv4_frame(&self.rx_buf[..n]) {
                    if v.src_ip == dst_ip && match_icmp_echo_reply(&v, 0x4452, seq) {
                        ok_cnt += 1;
                        total_us += (rdtsc_user().saturating_sub(t0)) / self.tsc_mhz.max(1);
                        break;
                    }
                }
            }
        }
        if ok_cnt == 0 {
            Err(ETIMEDOUT)
        } else {
            Ok((ok_cnt, total_us / u64::from(ok_cnt)))
        }
    }

    pub fn nslookup(&mut self, host: &[u8], dns_ip: [u8; 4], dns_port: u16) -> Result<([u8; 4], u32, u64), isize> {
        let srv_ip = if dns_ip == [0; 4] { self.dns } else { dns_ip };
        let port = if dns_port == 0 { 53 } else { dns_port };
        let dst_mac = self.resolve_next_hop_mac(srv_ip).ok_or(ETIMEDOUT)?;
        let mut qbuf = [0u8; 256];
        let qid = self.alloc_ip_id();
        let qlen = build_dns_a_query(host, qid, &mut qbuf).ok_or(EINVAL)?;
        let sport = self.alloc_ephemeral_port();
        let ip_id = self.alloc_ip_id();
        let flen = build_udp_frame(
            &mut self.tx_buf,
            dst_mac,
            self.mac,
            self.ip,
            srv_ip,
            ip_id,
            sport,
            port,
            &qbuf[..qlen],
        )
        .ok_or(EINVAL)?;
        let t0 = rdtsc_user();
        self.tx_raw(&self.tx_buf[..flen])?;
        let dl = t0.saturating_add(self.tsc_mhz.saturating_mul(1_200_000));
        while rdtsc_user() < dl {
            let n = self.rx_raw();
            if n == 0 {
                let _ = sys_yield();
                continue;
            }
            if let Some(v) = parse_ipv4_frame(&self.rx_buf[..n]) {
                if let Some((sp, dp, upay)) = parse_udp_payload(&v) {
                    if sp == port && dp == sport {
                        if let Some((ip, ttl)) = parse_dns_a_reply(upay, qid) {
                            let us = rdtsc_user().saturating_sub(t0) / self.tsc_mhz.max(1);
                            return Ok((ip, ttl, us));
                        }
                    }
                }
            }
        }
        Err(ETIMEDOUT)
    }

    pub fn http_get(
        &mut self,
        srv_ip: [u8; 4],
        port: u16,
        host: &[u8],
        path: &[u8],
    ) -> Result<(u16, usize, usize, u64), isize> {
        let dst_mac = self.resolve_next_hop_mac(srv_ip).ok_or(ETIMEDOUT)?;
        let port = if port == 0 { 80 } else { port };
        let sport = self.alloc_ephemeral_port();
        let mut seq = 0x1020_3040u32.wrapping_add(u32::from(sport));
        let mut ack = 0u32;

        let t0 = rdtsc_user();
        let ip_id = self.alloc_ip_id();
        let syn_len = build_tcp_frame(
            &mut self.tx_buf, dst_mac, self.mac, self.ip, srv_ip, ip_id, sport, port, seq, 0, TCP_SYN, &[],
        )
        .ok_or(EINVAL)?;
        self.tx_raw(&self.tx_buf[..syn_len])?;
        seq = seq.wrapping_add(1);

        let mut connected = false;
        let syn_dl = t0.saturating_add(self.tsc_mhz.saturating_mul(1_500_000));
        while rdtsc_user() < syn_dl {
            let n = self.rx_raw();
            if n == 0 {
                let _ = sys_yield();
                continue;
            }
            if let Some(v) = parse_ipv4_frame(&self.rx_buf[..n]) {
                if let Some(seg) = parse_tcp_segment(&v, sport, port) {
                    if v.src_ip == srv_ip
                        && (seg.flags & (TCP_SYN | TCP_ACK)) == (TCP_SYN | TCP_ACK)
                        && seg.ack == seq
                    {
                        ack = seg.seq.wrapping_add(1);
                        connected = true;
                        break;
                    }
                    if v.src_ip == srv_ip && (seg.flags & TCP_RST) != 0 {
                        return Err(crate::EPIPE);
                    }
                }
            }
        }
        if !connected {
            return Err(ETIMEDOUT);
        }

        let id_ack = self.alloc_ip_id();
        if let Some(alen) = build_tcp_frame(
            &mut self.tx_buf, dst_mac, self.mac, self.ip, srv_ip, id_ack, sport, port, seq, ack, TCP_ACK, &[],
        ) {
            let _ = self.tx_raw(&self.tx_buf[..alen]);
        }

        let mut req_buf = [0u8; 512];
        let rlen = build_http_get_req(host, path, &mut req_buf);
        let id_req = self.alloc_ip_id();
        let req_flen = build_tcp_frame(
            &mut self.tx_buf,
            dst_mac,
            self.mac,
            self.ip,
            srv_ip,
            id_req,
            sport,
            port,
            seq,
            ack,
            TCP_ACK | TCP_PSH,
            &req_buf[..rlen],
        )
        .ok_or(EINVAL)?;
        self.tx_raw(&self.tx_buf[..req_flen])?;
        seq = seq.wrapping_add(rlen as u32);

        let mut rx_len = 0usize;
        let rx_dl = rdtsc_user().saturating_add(self.tsc_mhz.saturating_mul(2_000_000));
        let mut saw_fin = false;
        while rdtsc_user() < rx_dl {
            let n = self.rx_raw();
            if n == 0 {
                let _ = sys_yield();
                continue;
            }
            if let Some(v) = parse_ipv4_frame(&self.rx_buf[..n]) {
                if let Some(seg) = parse_tcp_segment(&v, sport, port) {
                    if v.src_ip != srv_ip {
                        continue;
                    }
                    let seg_flags = seg.flags;
                    let seg_seq = seg.seq;
                    let seg_dlen = seg.data.len();
                    let mut send_ack = false;
                    if seg_seq == ack && seg_dlen > 0 {
                        let take = seg_dlen.min(HTTP_RX_CAP - rx_len);
                        self.http_buf[rx_len..(rx_len + take)].copy_from_slice(&seg.data[..take]);
                        rx_len += take;
                        ack = ack.wrapping_add(seg_dlen as u32);
                        send_ack = true;
                    }
                    if send_ack {
                        let id_a = self.alloc_ip_id();
                        if let Some(fl) = build_tcp_frame(
                            &mut self.tx_buf, dst_mac, self.mac, self.ip, srv_ip, id_a, sport, port, seq, ack, TCP_ACK, &[],
                        ) {
                            let _ = self.tx_raw(&self.tx_buf[..fl]);
                        }
                    }
                    if (seg_flags & TCP_FIN) != 0 {
                        if seg_seq == ack || seg_seq.wrapping_add(seg_dlen as u32) == ack {
                            ack = ack.wrapping_add(1);
                        }
                        saw_fin = true;
                        break;
                    }
                    if (seg_flags & TCP_RST) != 0 {
                        break;
                    }
                }
            }
        }
        let id_fin = self.alloc_ip_id();
        if let Some(fl) = build_tcp_frame(
            &mut self.tx_buf,
            dst_mac,
            self.mac,
            self.ip,
            srv_ip,
            id_fin,
            sport,
            port,
            seq,
            ack,
            TCP_ACK | if saw_fin { 0 } else { TCP_FIN },
            &[],
        ) {
            let _ = self.tx_raw(&self.tx_buf[..fl]);
        }
        if rx_len == 0 {
            return Err(ETIMEDOUT);
        }
        let (status, body_off) = parse_http_status_and_body(&self.http_buf[..rx_len]);
        let us = rdtsc_user().saturating_sub(t0) / self.tsc_mhz.max(1);
        Ok((status, body_off.min(rx_len), rx_len, us))
    }
}

impl Default for NetEngine {
    fn default() -> Self {
        Self::new()
    }
}

pub fn net_ifconfig(net_ep: u64) -> Result<[u64; 4], isize> {
    let (tag, _, mrs) = sys_ipc_call_inline(net_ep, pack_msg_tag(NET_OP_IFCONFIG, 0, 0, 0), [0; 4], 0)?;
    if msg_tag_label(tag) == IPC_OP_PONG { Ok(mrs) } else { Err(mrs[0] as isize) }
}

pub fn net_ping(net_ep: u64, dst_ip: [u8; 4], count: u32) -> Result<(u32, u64), isize> {
    let (tag, _, mrs) = sys_ipc_call_inline(
        net_ep,
        pack_msg_tag(NET_OP_PING, 0, 2, 0),
        [ip_to_u32(dst_ip) as u64, u64::from(count), 0, 0],
        0,
    )?;
    if msg_tag_label(tag) == IPC_OP_PONG { Ok((mrs[0] as u32, mrs[1])) } else { Err(mrs[0] as isize) }
}

pub fn net_nslookup(net_ep: u64, host: &[u8], dns_ip: [u8; 4], dns_port: u16) -> Result<([u8; 4], u32, u64), isize> {
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, _, mrs) = sys_ipc_call_extra(
        net_ep,
        NET_OP_NSLOOKUP,
        [ip_to_u32(dns_ip) as u64, u64::from(dns_port), 0, 0],
        host,
        &mut rx,
        0,
    )?;
    if msg_tag_label(tag) == IPC_OP_PONG {
        Ok((u32_to_ip(mrs[0] as u32), mrs[1] as u32, mrs[2]))
    } else {
        Err(mrs[0] as isize)
    }
}

pub fn net_httpget(
    net_ep: u64,
    srv_ip: [u8; 4],
    port: u16,
    dns_port: u16,
    host: &[u8],
    path: &[u8],
    body_out: &mut [u8],
) -> Result<(u16, usize, [u8; 4], u64), isize> {
    if host.len() > 96 || path.len() > 140 || 2 + host.len() + path.len() > MAX_IPC_EXTRA_BYTES {
        return Err(EINVAL);
    }
    let mut tx = [0u8; MAX_IPC_EXTRA_BYTES];
    tx[0] = host.len() as u8;
    tx[1] = path.len() as u8;
    tx[2..(2 + host.len())].copy_from_slice(host);
    let end = 2 + host.len() + path.len();
    tx[(2 + host.len())..end].copy_from_slice(path);

    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let (tag, rx_len, mrs) = sys_ipc_call_extra_grant(
        net_ep,
        NET_OP_HTTPGET,
        [ip_to_u32(srv_ip) as u64, u64::from(port), u64::from(dns_port), 0],
        &tx[..end],
        0,
        0,
        0,
        &mut rx,
        0,
    )?;
    if msg_tag_label(tag) != IPC_OP_PONG {
        return Err(mrs[0] as isize);
    }
    let copy_n = rx_len.min(body_out.len());
    body_out[..copy_n].copy_from_slice(&rx[..copy_n]);
    Ok((mrs[0] as u16, mrs[1] as usize, u32_to_ip(mrs[2] as u32), mrs[3]))
}
