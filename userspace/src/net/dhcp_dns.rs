// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure `#![no_std]` DHCP client (with QEMU SLIRP defaults) and DNS A-record resolver.

use crate::net::ipv4::build_udp_frame;
use crate::net::proto::{r_u16, r_u32, w_u16, w_u32, BCAST_MAC};

pub const DHCP_CLIENT_PORT: u16 = 68;
pub const DHCP_SERVER_PORT: u16 = 67;
const DHCP_MAGIC_COOKIE: u32 = 0x6382_5363;

#[derive(Clone, Copy)]
pub struct DhcpOfferInfo {
    pub msg_type: u8,
    pub your_ip: [u8; 4],
    pub server_ip: [u8; 4],
    pub router_ip: [u8; 4],
    pub dns_ip: [u8; 4],
    pub netmask: [u8; 4],
}

pub fn build_dhcp_packet(
    out: &mut [u8; 512],
    src_mac: [u8; 6],
    xid: u32,
    is_request: bool,
    req_ip: [u8; 4],
    server_ip: [u8; 4],
) -> Option<usize> {
    let mut pkt = [0u8; 320];
    pkt[0] = 1; // BOOTREQUEST
    pkt[1] = 1; // Ethernet
    pkt[2] = 6; // HLEN
    w_u32(&mut pkt, 4, xid);
    w_u16(&mut pkt, 10, 0x8000); // Broadcast flag
    pkt[28..34].copy_from_slice(&src_mac);
    w_u32(&mut pkt, 236, DHCP_MAGIC_COOKIE);

    let mut pos = 240usize;
    pkt[pos] = 53;
    pkt[pos + 1] = 1;
    pkt[pos + 2] = if is_request { 3 } else { 1 };
    pos += 3;

    if is_request {
        pkt[pos] = 50;
        pkt[pos + 1] = 4;
        pkt[pos + 2..pos + 6].copy_from_slice(&req_ip);
        pos += 6;
        pkt[pos] = 54;
        pkt[pos + 1] = 4;
        pkt[pos + 2..pos + 6].copy_from_slice(&server_ip);
        pos += 6;
    }

    // Parameter Request List: Subnet(1), Router(3), DNS(6).
    pkt[pos] = 55;
    pkt[pos + 1] = 3;
    pkt[pos + 2] = 1;
    pkt[pos + 3] = 3;
    pkt[pos + 4] = 6;
    pos += 5;
    pkt[pos] = 255;
    pos += 1;

    build_udp_frame(
        out,
        BCAST_MAC,
        src_mac,
        [0; 4],
        [255; 4],
        1,
        DHCP_CLIENT_PORT,
        DHCP_SERVER_PORT,
        &pkt[..pos.max(300)],
    )
}

pub fn parse_dhcp_reply(udp_payload: &[u8], expect_xid: u32, our_mac: [u8; 6]) -> Option<DhcpOfferInfo> {
    if udp_payload.len() < 240
        || udp_payload[0] != 2
        || r_u32(udp_payload, 4) != expect_xid
        || udp_payload[28..34] != our_mac
        || r_u32(udp_payload, 236) != DHCP_MAGIC_COOKIE
    {
        return None;
    }
    let mut info = DhcpOfferInfo {
        msg_type: 0,
        your_ip: [udp_payload[16], udp_payload[17], udp_payload[18], udp_payload[19]],
        server_ip: [udp_payload[20], udp_payload[21], udp_payload[22], udp_payload[23]],
        router_ip: [10, 0, 2, 2],
        dns_ip: [10, 0, 2, 3],
        netmask: [255, 255, 255, 0],
    };

    let mut i = 240usize;
    while i < udp_payload.len() {
        let opt = udp_payload[i];
        i += 1;
        if opt == 0 {
            continue;
        }
        if opt == 255 || i >= udp_payload.len() {
            break;
        }
        let olen = udp_payload[i] as usize;
        i += 1;
        if i + olen > udp_payload.len() {
            break;
        }
        let data = &udp_payload[i..(i + olen)];
        match opt {
            53 if olen >= 1 => info.msg_type = data[0],
            1 if olen >= 4 => info.netmask.copy_from_slice(&data[..4]),
            3 if olen >= 4 => info.router_ip.copy_from_slice(&data[..4]),
            6 if olen >= 4 => info.dns_ip.copy_from_slice(&data[..4]),
            54 if olen >= 4 => info.server_ip.copy_from_slice(&data[..4]),
            _ => {}
        }
        i += olen;
    }
    (info.msg_type == 2 || info.msg_type == 5).then_some(info)
}

pub fn build_dns_a_query(host: &[u8], qid: u16, out: &mut [u8; 256]) -> Option<usize> {
    if host.is_empty() || host.len() > 190 {
        return None;
    }
    out.fill(0);
    w_u16(out, 0, qid);
    w_u16(out, 2, 0x0100); // RD=1
    w_u16(out, 4, 1); // QDCOUNT=1
    let mut pos = 12usize;
    for label in host.split(|&b| b == b'.') {
        if label.is_empty() || label.len() > 63 || pos + 1 + label.len() + 5 >= out.len() {
            return None;
        }
        out[pos] = label.len() as u8;
        pos += 1;
        out[pos..(pos + label.len())].copy_from_slice(label);
        pos += label.len();
    }
    out[pos] = 0;
    pos += 1;
    w_u16(out, pos, 1); // QTYPE=A
    w_u16(out, pos + 2, 1); // QCLASS=IN
    Some(pos + 4)
}

fn skip_dns_name(pkt: &[u8], mut off: usize) -> Option<usize> {
    let mut steps = 0usize;
    while off < pkt.len() && steps < 64 {
        let len = pkt[off];
        if len == 0 {
            return Some(off + 1);
        }
        if (len & 0xc0) == 0xc0 {
            return (off + 2 <= pkt.len()).then_some(off + 2);
        }
        off = off.checked_add(1 + (len as usize))?;
        steps += 1;
    }
    None
}

pub fn parse_dns_a_reply(pkt: &[u8], expect_id: u16) -> Option<([u8; 4], u32)> {
    if pkt.len() < 12 || r_u16(pkt, 0) != expect_id {
        return None;
    }
    let flags = r_u16(pkt, 2);
    if (flags & 0x8000) == 0 || (flags & 0x000f) != 0 {
        return None;
    }
    let qd = r_u16(pkt, 4) as usize;
    let an = r_u16(pkt, 6) as usize;
    let mut off = 12usize;
    for _ in 0..qd {
        off = skip_dns_name(pkt, off)?;
        off = off.checked_add(4)?;
        if off > pkt.len() {
            return None;
        }
    }
    for _ in 0..an {
        off = skip_dns_name(pkt, off)?;
        if off + 10 > pkt.len() {
            return None;
        }
        let rtype = r_u16(pkt, off);
        let rclass = r_u16(pkt, off + 2);
        let ttl = r_u32(pkt, off + 4);
        let rdlen = r_u16(pkt, off + 8) as usize;
        off += 10;
        if off + rdlen > pkt.len() {
            return None;
        }
        if rtype == 1 && rclass == 1 && rdlen == 4 {
            return Some(([pkt[off], pkt[off + 1], pkt[off + 2], pkt[off + 3]], ttl));
        }
        off += rdlen;
    }
    None
}
