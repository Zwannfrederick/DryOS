// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Native Terminal Networking CLI Commands (`ifconfig`, `ping`, `nslookup`, `httpget`)
//! connected over capability IPC to `dry-net`.

use crate::abi_ext::IPC_OP_SUP_GET_NET_CAP;
use crate::net::proto::{parse_ipv4, parse_u16_ascii, u32_to_ip};
use crate::net::{net_httpget, net_ifconfig, net_nslookup, net_ping};
use crate::shell::{LineBuilder, ShellEngine};
use crate::surface::{STYLE_DEFAULT, STYLE_OK, STYLE_WARN};
use crate::{
    msg_tag_label, sys_ipc_call_with_grant, sys_write_debug, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES,
};

fn emit_dbg(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn ensure_net_cap(sup_ep_cap: u64, cached_net_cap: &mut u64) -> Option<u64> {
    if *cached_net_cap != 0 {
        return Some(*cached_net_cap);
    }
    if let Ok((tag, cap, _)) =
        sys_ipc_call_with_grant(sup_ep_cap, IPC_OP_SUP_GET_NET_CAP, [0; 4], 0, 0, 0, 0)
    {
        if msg_tag_label(tag) == IPC_OP_PONG && cap != 0 {
            *cached_net_cap = cap;
            return Some(cap);
        }
    }
    None
}

fn push_ip(lb: &mut LineBuilder, ip: [u8; 4]) {
    for (i, &b) in ip.iter().enumerate() {
        if i > 0 {
            lb.push_byte(b'.');
        }
        lb.push_u64(u64::from(b));
    }
}

fn split_first_space(raw: &[u8]) -> (&[u8], &[u8]) {
    let mut s = 0usize;
    while s < raw.len() && raw[s] == b' ' {
        s += 1;
    }
    let mut e = s;
    while e < raw.len() && raw[e] != b' ' {
        e += 1;
    }
    let mut rs = e;
    while rs < raw.len() && raw[rs] == b' ' {
        rs += 1;
    }
    (&raw[s..e], &raw[rs..])
}

pub fn try_exec_net_cmd(
    verb: &[u8],
    rest: &[u8],
    sup_ep_cap: u64,
    cached_net_cap: &mut u64,
    engine: &mut ShellEngine,
) -> bool {
    match verb {
        b"ifconfig" => {
            let Some(nep) = ensure_net_cap(sup_ep_cap, cached_net_cap) else {
                engine.push_line(b"ifconfig: network service unavailable (-ENOSYS)", STYLE_WARN);
                let _ = sys_write_debug(b"[DRY-SHELL] CMD=ifconfig err=-38\n");
                return true;
            };
            match net_ifconfig(nep) {
                Ok(mrs) => {
                    let ip = u32_to_ip(mrs[1] as u32);
                    let gw = u32_to_ip((mrs[1] >> 32) as u32);
                    let dns = u32_to_ip(mrs[2] as u32);
                    let dhcp = ((mrs[2] >> 32) & 1) != 0;
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"eth0 ip=");
                    push_ip(&mut lb, ip);
                    lb.push_bytes(b" gw=");
                    push_ip(&mut lb, gw);
                    lb.push_bytes(b" dns=");
                    push_ip(&mut lb, dns);
                    lb.push_bytes(b" mac=");
                    for i in 0..6 {
                        if i > 0 {
                            lb.push_byte(b':');
                        }
                        lb.push_hex_width((mrs[0] >> (i * 8)) & 0xff, 2);
                    }
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=ifconfig ip=");
                    push_ip(&mut slb, ip);
                    slb.push_bytes(b" gw=");
                    push_ip(&mut slb, gw);
                    slb.push_bytes(b" dhcp=");
                    slb.push_u64(u64::from(dhcp));
                    emit_dbg(&slb);
                }
                Err(e) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"ifconfig error: ");
                    lb.push_i64(e as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"ping" => {
            let Some(ip) = parse_ipv4(rest) else {
                engine.push_line(b"Usage: ping <ipv4>", STYLE_WARN);
                return true;
            };
            let Some(nep) = ensure_net_cap(sup_ep_cap, cached_net_cap) else {
                engine.push_line(b"ping: network unavailable (-ENOSYS)", STYLE_WARN);
                return true;
            };
            match net_ping(nep, ip, 2) {
                Ok((ok_cnt, rtt_us)) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"PING ");
                    push_ip(&mut lb, ip);
                    lb.push_bytes(b": ");
                    lb.push_u64(u64::from(ok_cnt));
                    lb.push_bytes(b"/2 ok avg_us=");
                    lb.push_u64(rtt_us);
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=ping ip=");
                    push_ip(&mut slb, ip);
                    slb.push_bytes(b" ok=");
                    slb.push_u64(u64::from(ok_cnt));
                    slb.push_bytes(b" rtt_us=");
                    slb.push_u64(rtt_us);
                    emit_dbg(&slb);
                }
                Err(e) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"ping failed: ");
                    lb.push_i64(e as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"nslookup" => {
            let (host, srv_arg) = split_first_space(rest);
            if host.is_empty() {
                engine.push_line(b"Usage: nslookup <host> [dns_ip[:port]]", STYLE_WARN);
                return true;
            }
            let (dns_ip, dns_port) = if srv_arg.is_empty() {
                ([0u8; 4], 53u16)
            } else if let Some(cpos) = srv_arg.iter().position(|&b| b == b':') {
                let (Some(dip), Some(dp)) = (
                    parse_ipv4(&srv_arg[..cpos]),
                    parse_u16_ascii(&srv_arg[(cpos + 1)..]),
                ) else {
                    engine.push_line(b"nslookup: invalid server", STYLE_WARN);
                    return true;
                };
                (dip, dp)
            } else if let Some(dip) = parse_ipv4(srv_arg) {
                (dip, 53)
            } else {
                engine.push_line(b"nslookup: invalid server IP", STYLE_WARN);
                return true;
            };
            let Some(nep) = ensure_net_cap(sup_ep_cap, cached_net_cap) else {
                engine.push_line(b"nslookup: network unavailable (-ENOSYS)", STYLE_WARN);
                return true;
            };
            match net_nslookup(nep, host, dns_ip, dns_port) {
                Ok((ip, ttl, us)) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(host);
                    lb.push_bytes(b" -> ");
                    push_ip(&mut lb, ip);
                    lb.push_bytes(b" (ttl=");
                    lb.push_u64(u64::from(ttl));
                    lb.push_bytes(b", ");
                    lb.push_u64(us);
                    lb.push_bytes(b" us)");
                    engine.push_line(lb.as_bytes(), STYLE_OK);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=nslookup host=");
                    slb.push_bytes(host);
                    slb.push_bytes(b" ip=");
                    push_ip(&mut slb, ip);
                    emit_dbg(&slb);
                }
                Err(e) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"nslookup error: ");
                    lb.push_i64(e as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        b"httpget" => {
            let trimmed = rest.strip_prefix(b"http://").unwrap_or(rest);
            if trimmed.is_empty() {
                engine.push_line(b"Usage: httpget <ip|host>[:port][/path]", STYLE_WARN);
                return true;
            }
            let (hostport, path) = if let Some(spos) = trimmed.iter().position(|&b| b == b'/') {
                (&trimmed[..spos], &trimmed[spos..])
            } else {
                (trimmed, &b"/"[..])
            };
            let (host, port) = if let Some(cpos) = hostport.iter().position(|&b| b == b':') {
                let Some(p) = parse_u16_ascii(&hostport[(cpos + 1)..]) else {
                    engine.push_line(b"httpget: invalid port", STYLE_WARN);
                    return true;
                };
                (&hostport[..cpos], p)
            } else {
                (hostport, 80)
            };
            let srv_ip = parse_ipv4(host).unwrap_or([0; 4]);
            let Some(nep) = ensure_net_cap(sup_ep_cap, cached_net_cap) else {
                engine.push_line(b"httpget: network unavailable (-ENOSYS)", STYLE_WARN);
                return true;
            };
            let mut body = [0u8; MAX_IPC_EXTRA_BYTES];
            match net_httpget(nep, srv_ip, port, 53, host, path, &mut body) {
                Ok((status, blen, resolved_ip, us)) => {
                    let preview_n = blen.min(MAX_IPC_EXTRA_BYTES);
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"HTTP ");
                    lb.push_u64(u64::from(status));
                    lb.push_bytes(b" from ");
                    push_ip(&mut lb, resolved_ip);
                    lb.push_bytes(b" (");
                    lb.push_u64(blen as u64);
                    lb.push_bytes(b" B, ");
                    lb.push_u64(us);
                    lb.push_bytes(b" us)");
                    engine.push_line(lb.as_bytes(), if status == 200 { STYLE_OK } else { STYLE_WARN });

                    let text = &body[..preview_n.min(128)];
                    let mut plb = LineBuilder::new();
                    for &ch in text {
                        plb.push_byte(if (0x20..=0x7e).contains(&ch) { ch } else { b' ' });
                    }
                    engine.push_line(plb.as_bytes(), STYLE_DEFAULT);

                    let mut slb = LineBuilder::new();
                    slb.push_bytes(b"[DRY-SHELL] CMD=httpget status=");
                    slb.push_u64(u64::from(status));
                    slb.push_bytes(b" bytes=");
                    slb.push_u64(blen as u64);
                    slb.push_bytes(b" body=\"");
                    for &ch in &body[..preview_n.min(48)] {
                        slb.push_byte(if (0x20..=0x7e).contains(&ch) { ch } else { b'.' });
                    }
                    slb.push_byte(b'"');
                    emit_dbg(&slb);
                }
                Err(e) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"httpget error: ");
                    lb.push_i64(e as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                }
            }
            true
        }
        _ => false,
    }
}
