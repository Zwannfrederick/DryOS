// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure `#![no_std]` Ethernet, ARP cache, IPv4 address parsing, and RFC 1071 checksums.

pub const ETH_HLEN: usize = 14;
pub const ETH_TYPE_IPV4: u16 = 0x0800;
pub const ETH_TYPE_ARP: u16 = 0x0806;

pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP: u8 = 6;
pub const IP_PROTO_UDP: u8 = 17;

pub const BCAST_MAC: [u8; 6] = [0xff; 6];
pub const ZERO_MAC: [u8; 6] = [0; 6];

#[inline]
pub fn r_u16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}

#[inline]
pub fn w_u16(b: &mut [u8], off: usize, v: u16) {
    let bytes = v.to_be_bytes();
    b[off] = bytes[0];
    b[off + 1] = bytes[1];
}

#[inline]
pub fn r_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline]
pub fn w_u32(b: &mut [u8], off: usize, v: u32) {
    let bytes = v.to_be_bytes();
    b[off..off + 4].copy_from_slice(&bytes);
}

#[inline]
pub const fn ip_to_u32(ip: [u8; 4]) -> u32 {
    u32::from_be_bytes(ip)
}

#[inline]
pub const fn u32_to_ip(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

pub fn parse_u16_ascii(s: &[u8]) -> Option<u16> {
    if s.is_empty() || s.len() > 5 {
        return None;
    }
    let mut val = 0u32;
    for &c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        val = val * 10 + u32::from(c - b'0');
        if val > u16::MAX as u32 {
            return None;
        }
    }
    Some(val as u16)
}

pub fn parse_ipv4(s: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut idx = 0usize;
    for part in s.split(|&b| b == b'.') {
        if idx >= 4 || part.is_empty() || part.len() > 3 {
            return None;
        }
        let mut val = 0u16;
        for &c in part {
            if !c.is_ascii_digit() {
                return None;
            }
            val = val * 10 + u16::from(c - b'0');
            if val > 255 {
                return None;
            }
        }
        out[idx] = val as u8;
        idx += 1;
    }
    if idx == 4 { Some(out) } else { None }
}

pub fn checksum_sum(mut acc: u32, data: &[u8]) -> u32 {
    let mut i = 0usize;
    while i + 1 < data.len() {
        acc = acc.wrapping_add(u32::from(u16::from_be_bytes([data[i], data[i + 1]])));
        i += 2;
    }
    if i < data.len() {
        acc = acc.wrapping_add(u32::from(data[i]) << 8);
    }
    acc
}

pub fn checksum_fold(mut acc: u32) -> u16 {
    while (acc >> 16) != 0 {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    !(acc as u16)
}

pub fn checksum_16(data: &[u8]) -> u16 {
    checksum_fold(checksum_sum(0, data))
}

pub fn transport_checksum(src: [u8; 4], dst: [u8; 4], proto: u8, payload: &[u8]) -> u16 {
    let mut acc = checksum_sum(0, &src);
    acc = checksum_sum(acc, &dst);
    acc = acc.wrapping_add(u32::from(proto));
    acc = acc.wrapping_add(payload.len() as u32);
    acc = checksum_sum(acc, payload);
    let c = checksum_fold(acc);
    if proto == IP_PROTO_UDP && c == 0 { 0xffff } else { c }
}

pub fn write_eth_hdr(dst: &mut [u8], dst_mac: [u8; 6], src_mac: [u8; 6], eth_type: u16) {
    dst[0..6].copy_from_slice(&dst_mac);
    dst[6..12].copy_from_slice(&src_mac);
    w_u16(dst, 12, eth_type);
}

#[derive(Clone, Copy)]
pub struct ArpEntry {
    pub ip: [u8; 4],
    pub mac: [u8; 6],
    pub valid: bool,
}

pub struct ArpCache {
    entries: [ArpEntry; 4],
    next_evict: usize,
}

impl ArpCache {
    pub const fn new() -> Self {
        Self {
            entries: [ArpEntry {
                ip: [0; 4],
                mac: ZERO_MAC,
                valid: false,
            }; 4],
            next_evict: 0,
        }
    }

    pub fn lookup(&self, ip: [u8; 4]) -> Option<[u8; 6]> {
        self.entries.iter().find(|e| e.valid && e.ip == ip).map(|e| e.mac)
    }

    pub fn insert(&mut self, ip: [u8; 4], mac: [u8; 6]) {
        if mac == ZERO_MAC || mac == BCAST_MAC || ip == [0; 4] {
            return;
        }
        if let Some(e) = self.entries.iter_mut().find(|e| e.valid && e.ip == ip) {
            e.mac = mac;
            return;
        }
        let slot = self
            .entries
            .iter()
            .position(|e| !e.valid)
            .unwrap_or_else(|| {
                let s = self.next_evict;
                self.next_evict = (self.next_evict + 1) & 3;
                s
            });
        self.entries[slot] = ArpEntry { ip, mac, valid: true };
    }
}

impl Default for ArpCache {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build_arp_packet(
    out: &mut [u8; 60],
    oper: u16,
    sha: [u8; 6],
    spa: [u8; 4],
    tha: [u8; 6],
    tpa: [u8; 4],
) -> usize {
    out.fill(0);
    let dst_eth = if oper == 1 { BCAST_MAC } else { tha };
    write_eth_hdr(out, dst_eth, sha, ETH_TYPE_ARP);
    w_u16(out, 14, 1); // Ethernet
    w_u16(out, 16, ETH_TYPE_IPV4);
    out[18] = 6;
    out[19] = 4;
    w_u16(out, 20, oper);
    out[22..28].copy_from_slice(&sha);
    out[28..32].copy_from_slice(&spa);
    out[32..38].copy_from_slice(if oper == 1 { &ZERO_MAC } else { &tha });
    out[38..42].copy_from_slice(&tpa);
    60
}

/// Process an incoming ARP frame; returns optional 60-byte ARP reply if it asked for `our_ip`.
pub fn process_arp(
    frame: &[u8],
    our_mac: [u8; 6],
    our_ip: [u8; 4],
    cache: &mut ArpCache,
    reply_out: &mut [u8; 60],
) -> Option<usize> {
    if frame.len() < ETH_HLEN + 28 || r_u16(frame, 12) != ETH_TYPE_ARP {
        return None;
    }
    let a = &frame[ETH_HLEN..];
    if r_u16(a, 0) != 1 || r_u16(a, 2) != ETH_TYPE_IPV4 || a[4] != 6 || a[5] != 4 {
        return None;
    }
    let oper = r_u16(a, 6);
    let mut sha = [0u8; 6];
    let mut spa = [0u8; 4];
    let mut tpa = [0u8; 4];
    sha.copy_from_slice(&a[8..14]);
    spa.copy_from_slice(&a[14..18]);
    tpa.copy_from_slice(&a[24..28]);
    cache.insert(spa, sha);
    if oper == 1 && our_ip != [0; 4] && tpa == our_ip {
        return Some(build_arp_packet(reply_out, 2, our_mac, our_ip, sha, spa));
    }
    None
}
