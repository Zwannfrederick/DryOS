// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure `#![no_std]` IPv4, ICMP Echo, and UDP framing helpers.

use crate::net::proto::{
    checksum_16, r_u16, transport_checksum, w_u16, write_eth_hdr, ETH_HLEN, ETH_TYPE_IPV4,
    IP_PROTO_ICMP, IP_PROTO_UDP,
};

pub const IPV4_HLEN: usize = 20;
pub const UDP_HLEN: usize = 8;
pub const ICMP_HLEN: usize = 8;

pub struct Ipv4View<'a> {
    pub src_mac: [u8; 6],
    pub src_ip: [u8; 4],
    pub dst_ip: [u8; 4],
    pub proto: u8,
    pub payload: &'a [u8],
}

pub fn parse_ipv4_frame(frame: &[u8]) -> Option<Ipv4View<'_>> {
    if frame.len() < ETH_HLEN + IPV4_HLEN || r_u16(frame, 12) != ETH_TYPE_IPV4 {
        return None;
    }
    let ip = &frame[ETH_HLEN..];
    if (ip[0] >> 4) != 4 {
        return None;
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    if ihl < IPV4_HLEN || ip.len() < ihl {
        return None;
    }
    let tot = r_u16(ip, 2) as usize;
    if tot < ihl || tot > ip.len() {
        return None;
    }
    let frag = r_u16(ip, 6);
    if (frag & 0x3fff) != 0 {
        return None;
    }
    let mut src_mac = [0u8; 6];
    let mut src_ip = [0u8; 4];
    let mut dst_ip = [0u8; 4];
    src_mac.copy_from_slice(&frame[6..12]);
    src_ip.copy_from_slice(&ip[12..16]);
    dst_ip.copy_from_slice(&ip[16..20]);
    Some(Ipv4View {
        src_mac,
        src_ip,
        dst_ip,
        proto: ip[9],
        payload: &ip[ihl..tot],
    })
}

pub fn write_ipv4_header(
    pkt: &mut [u8],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    proto: u8,
    ip_id: u16,
    payload_len: usize,
) {
    pkt[0] = 0x45;
    pkt[1] = 0;
    w_u16(pkt, 2, (IPV4_HLEN + payload_len) as u16);
    w_u16(pkt, 4, ip_id);
    w_u16(pkt, 6, 0x4000); // DF
    pkt[8] = 64;
    pkt[9] = proto;
    w_u16(pkt, 10, 0);
    pkt[12..16].copy_from_slice(&src_ip);
    pkt[16..20].copy_from_slice(&dst_ip);
    let csum = checksum_16(&pkt[..IPV4_HLEN]);
    w_u16(pkt, 10, csum);
}

#[allow(clippy::too_many_arguments)]
pub fn build_udp_frame(
    out: &mut [u8],
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    ip_id: u16,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Option<usize> {
    let udp_len = UDP_HLEN + payload.len();
    let total = ETH_HLEN + IPV4_HLEN + udp_len;
    if total > out.len() {
        return None;
    }
    write_eth_hdr(out, dst_mac, src_mac, ETH_TYPE_IPV4);
    write_ipv4_header(&mut out[ETH_HLEN..], src_ip, dst_ip, IP_PROTO_UDP, ip_id, udp_len);
    let udp = &mut out[(ETH_HLEN + IPV4_HLEN)..total];
    w_u16(udp, 0, src_port);
    w_u16(udp, 2, dst_port);
    w_u16(udp, 4, udp_len as u16);
    w_u16(udp, 6, 0);
    udp[UDP_HLEN..].copy_from_slice(payload);
    let csum = transport_checksum(src_ip, dst_ip, IP_PROTO_UDP, udp);
    w_u16(udp, 6, csum);
    Some(total.max(60))
}

pub fn parse_udp_payload<'a>(v: &Ipv4View<'a>) -> Option<(u16, u16, &'a [u8])> {
    if v.proto != IP_PROTO_UDP || v.payload.len() < UDP_HLEN {
        return None;
    }
    let sport = r_u16(v.payload, 0);
    let dport = r_u16(v.payload, 2);
    let ulen = r_u16(v.payload, 4) as usize;
    if ulen < UDP_HLEN || ulen > v.payload.len() {
        return None;
    }
    Some((sport, dport, &v.payload[UDP_HLEN..ulen]))
}

#[allow(clippy::too_many_arguments)]
pub fn build_icmp_echo(
    out: &mut [u8],
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    ip_id: u16,
    icmp_type: u8,
    ident: u16,
    seq: u16,
    payload: &[u8],
) -> Option<usize> {
    let ilen = ICMP_HLEN + payload.len();
    let total = ETH_HLEN + IPV4_HLEN + ilen;
    if total > out.len() {
        return None;
    }
    write_eth_hdr(out, dst_mac, src_mac, ETH_TYPE_IPV4);
    write_ipv4_header(&mut out[ETH_HLEN..], src_ip, dst_ip, IP_PROTO_ICMP, ip_id, ilen);
    let icmp = &mut out[(ETH_HLEN + IPV4_HLEN)..total];
    icmp[0] = icmp_type;
    icmp[1] = 0;
    w_u16(icmp, 2, 0);
    w_u16(icmp, 4, ident);
    w_u16(icmp, 6, seq);
    icmp[ICMP_HLEN..].copy_from_slice(payload);
    let csum = checksum_16(icmp);
    w_u16(icmp, 2, csum);
    Some(total.max(60))
}

pub fn match_icmp_echo_reply(v: &Ipv4View<'_>, ident: u16, seq: u16) -> bool {
    v.proto == IP_PROTO_ICMP
        && v.payload.len() >= ICMP_HLEN
        && v.payload[0] == 0
        && v.payload[1] == 0
        && r_u16(v.payload, 4) == ident
        && r_u16(v.payload, 6) == seq
}
