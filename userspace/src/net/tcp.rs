// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal pure `#![no_std]` RFC 793 TCP client + HTTP/1.0 GET framing.

use crate::net::ipv4::{write_ipv4_header, Ipv4View, IPV4_HLEN};
use crate::net::proto::{
    r_u16, r_u32, transport_checksum, w_u16, w_u32, write_eth_hdr, ETH_HLEN, ETH_TYPE_IPV4,
    IP_PROTO_TCP,
};

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;
pub const TCP_HLEN: usize = 20;

#[allow(clippy::too_many_arguments)]
pub fn build_tcp_frame(
    out: &mut [u8],
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    ip_id: u16,
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
) -> Option<usize> {
    let tlen = TCP_HLEN + payload.len();
    let total = ETH_HLEN + IPV4_HLEN + tlen;
    if total > out.len() {
        return None;
    }
    write_eth_hdr(out, dst_mac, src_mac, ETH_TYPE_IPV4);
    write_ipv4_header(&mut out[ETH_HLEN..], src_ip, dst_ip, IP_PROTO_TCP, ip_id, tlen);
    let tcp = &mut out[(ETH_HLEN + IPV4_HLEN)..total];
    tcp[..TCP_HLEN].fill(0);
    w_u16(tcp, 0, sport);
    w_u16(tcp, 2, dport);
    w_u32(tcp, 4, seq);
    w_u32(tcp, 8, ack);
    tcp[12] = 0x50; // data offset = 5 (20 bytes)
    tcp[13] = flags;
    w_u16(tcp, 14, 16384); // RX window
    if !payload.is_empty() {
        tcp[TCP_HLEN..].copy_from_slice(payload);
    }
    let csum = transport_checksum(src_ip, dst_ip, IP_PROTO_TCP, tcp);
    w_u16(tcp, 16, csum);
    Some(total.max(60))
}

pub struct TcpSegView<'a> {
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub data: &'a [u8],
}

pub fn parse_tcp_segment<'a>(
    v: &Ipv4View<'a>,
    expected_sport: u16,
    expected_dport: u16,
) -> Option<TcpSegView<'a>> {
    if v.proto != IP_PROTO_TCP || v.payload.len() < TCP_HLEN {
        return None;
    }
    let sport = r_u16(v.payload, 0);
    let dport = r_u16(v.payload, 2);
    if sport != expected_dport || dport != expected_sport {
        return None;
    }
    let hdr_len = usize::from(v.payload[12] >> 4) * 4;
    if hdr_len < TCP_HLEN || hdr_len > v.payload.len() {
        return None;
    }
    Some(TcpSegView {
        seq: r_u32(v.payload, 4),
        ack: r_u32(v.payload, 8),
        flags: v.payload[13],
        data: &v.payload[hdr_len..],
    })
}

pub fn build_http_get_req(host: &[u8], path: &[u8], out: &mut [u8; 512]) -> usize {
    let mut pos = 0usize;
    let mut push = |s: &[u8]| {
        let take = s.len().min(out.len().saturating_sub(pos));
        out[pos..(pos + take)].copy_from_slice(&s[..take]);
        pos += take;
    };
    push(b"GET ");
    if path.is_empty() {
        push(b"/");
    } else {
        if !path.starts_with(b"/") {
            push(b"/");
        }
        push(path);
    }
    push(b" HTTP/1.0\r\nHost: ");
    push(if host.is_empty() { b"localhost" } else { host });
    push(b"\r\nConnection: close\r\n\r\n");
    pos
}

pub fn parse_http_status_and_body(raw: &[u8]) -> (u16, usize) {
    let mut status = 0u16;
    if let Some(sp) = raw.iter().position(|&b| b == b' ') {
        if raw.len() >= sp + 4 {
            let d = &raw[(sp + 1)..(sp + 4)];
            if d.iter().all(|c| c.is_ascii_digit()) {
                status = u16::from(d[0] - b'0') * 100
                    + u16::from(d[1] - b'0') * 10
                    + u16::from(d[2] - b'0');
            }
        }
    }
    for i in 0..raw.len().saturating_sub(3) {
        if &raw[i..(i + 4)] == b"\r\n\r\n" {
            return (status, i + 4);
        }
    }
    for i in 0..raw.len().saturating_sub(1) {
        if &raw[i..(i + 2)] == b"\n\n" {
            return (status, i + 2);
        }
    }
    (status, 0)
}
