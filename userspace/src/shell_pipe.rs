// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Bounded single-stage shell pipelines (`| grep`, `| head`, `| wc`) and VFS output
//! redirection (`>`, `>>`) for `dry-shell`.
//!
//! Safety / Compatibility contract:
//! - Strictly rejects multiple pipes/redirections or any unsupported POSIX metacharacters
//!   (`;`, `&`, `<`, `` ` ``, `$`, `\\`).
//! - Any pipe RHS verb other than `grep`, `head`, or `wc` returns `None`, allowing `parse_command`
//!   to fall through to `ShellCommand::UnsupportedPosix(b'|')` (preserving `--p3-only` `echo a | cat`).

use crate::shell::{LineBuilder, ShellEngine};
use crate::shell_ext::ensure_vfs_cap;
use crate::shell_vfs_ops::resolve_path;
use crate::surface::{STYLE_DEFAULT, STYLE_OK, STYLE_WARN, TERM_COLS};
use crate::vfs_proto::{vfs_create_file, vfs_read_file, vfs_stat, vfs_write_small, VFS_FLAG_DIR};
use crate::{sys_write_debug, EEXIST};

#[derive(Clone, Copy)]
pub enum PipeFilter<'a> {
    Grep(&'a [u8]),
    Head(usize),
    Wc,
}

#[derive(Clone, Copy)]
pub enum PipeOrRedirKind<'a> {
    Pipe(PipeFilter<'a>),
    Redir { path: &'a [u8], append: bool },
}

#[derive(Clone, Copy)]
pub struct PipeOrRedir<'a> {
    lhs: &'a [u8],
    kind: PipeOrRedirKind<'a>,
}

impl<'a> PipeOrRedir<'a> {
    #[inline]
    pub const fn lhs(&self) -> &'a [u8] {
        self.lhs
    }
}

#[inline]
fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let Some(&b) = s.first() {
        if b == b' ' {
            s = &s[1..];
        } else {
            break;
        }
    }
    while let Some(&b) = s.last() {
        if b == b' ' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn parse_usize(s: &[u8]) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let mut v = 0usize;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.saturating_mul(10).saturating_add((b - b'0') as usize);
    }
    Some(v)
}

pub fn parse_pipe_or_redir(raw: &[u8]) -> Option<PipeOrRedir<'_>> {
    let trimmed = trim_ascii(raw);
    if trimmed.is_empty() {
        return None;
    }
    for &b in trimmed {
        if matches!(b, b'<' | b';' | b'&' | b'$' | b'`' | b'\\') {
            return None;
        }
    }
    let has_pipe = trimmed.contains(&b'|');
    let has_gt = trimmed.contains(&b'>');
    if has_pipe && has_gt {
        return None;
    }
    if has_pipe {
        let ppos = trimmed.iter().position(|&b| b == b'|')?;
        if trimmed[ppos + 1..].contains(&b'|') {
            return None;
        }
        let lhs = trim_ascii(&trimmed[..ppos]);
        let rhs = trim_ascii(&trimmed[ppos + 1..]);
        if lhs.is_empty() || rhs.is_empty() {
            return None;
        }
        let mut vend = 0usize;
        while vend < rhs.len() && rhs[vend] != b' ' {
            vend += 1;
        }
        let verb = &rhs[..vend];
        let arg = trim_ascii(&rhs[vend..]);
        let filter = match verb {
            b"grep" => {
                if arg.is_empty() {
                    return None;
                }
                PipeFilter::Grep(arg)
            }
            b"head" => {
                let n = if arg.is_empty() {
                    10
                } else if let Some(rest) = arg.strip_prefix(b"-n") {
                    parse_usize(trim_ascii(rest))?.clamp(1, 32)
                } else {
                    parse_usize(arg)?.clamp(1, 32)
                };
                PipeFilter::Head(n)
            }
            b"wc" => {
                if !arg.is_empty() && arg != b"-l" {
                    return None;
                }
                PipeFilter::Wc
            }
            _ => return None,
        };
        return Some(PipeOrRedir {
            lhs,
            kind: PipeOrRedirKind::Pipe(filter),
        });
    }
    if has_gt {
        let gpos = trimmed.iter().position(|&b| b == b'>')?;
        let (append, rhs_start) = if gpos + 1 < trimmed.len() && trimmed[gpos + 1] == b'>' {
            (true, gpos + 2)
        } else {
            (false, gpos + 1)
        };
        if trimmed[rhs_start..].contains(&b'>') {
            return None;
        }
        let lhs = trim_ascii(&trimmed[..gpos]);
        let path = trim_ascii(&trimmed[rhs_start..]);
        if lhs.is_empty() || path.is_empty() {
            return None;
        }
        return Some(PipeOrRedir {
            lhs,
            kind: PipeOrRedirKind::Redir { path, append },
        });
    }
    None
}

pub fn finish_pipe_or_redir(
    parsed: &PipeOrRedir<'_>,
    sup_ep_cap: u64,
    cached_vfs_cap: &mut u64,
    lines: &[[u8; TERM_COLS]],
    lens: &[u8],
    engine: &mut ShellEngine,
) {
    match parsed.kind {
        PipeOrRedirKind::Pipe(filter) => {
            let mut out_lines = 0usize;
            let filter_name: &[u8] = match filter {
                PipeFilter::Grep(pat) => {
                    for (row, &len) in lines.iter().zip(lens.iter()) {
                        let s = &row[..(len as usize).min(TERM_COLS)];
                        if contains_bytes(s, pat) {
                            engine.push_line(s, STYLE_DEFAULT);
                            out_lines += 1;
                        }
                    }
                    b"grep"
                }
                PipeFilter::Head(max_n) => {
                    for (row, &len) in lines.iter().zip(lens.iter()).take(max_n) {
                        engine.push_line(&row[..(len as usize).min(TERM_COLS)], STYLE_DEFAULT);
                        out_lines += 1;
                    }
                    b"head"
                }
                PipeFilter::Wc => {
                    let mut bytes = 0usize;
                    for &len in lens {
                        bytes += (len as usize).min(TERM_COLS) + 1;
                    }
                    let mut lb = LineBuilder::new();
                    lb.push_u64(lines.len() as u64);
                    lb.push_bytes(b" lines ");
                    lb.push_u64(bytes as u64);
                    lb.push_bytes(b" bytes");
                    engine.push_line(lb.as_bytes(), STYLE_OK);
                    out_lines = 1;
                    b"wc"
                }
            };
            let mut slb = LineBuilder::new();
            slb.push_bytes(b"[DRY-SHELL] CMD=pipe filter=");
            slb.push_bytes(filter_name);
            slb.push_bytes(b" in_lines=");
            slb.push_u64(lines.len() as u64);
            slb.push_bytes(b" out_lines=");
            slb.push_u64(out_lines as u64);
            let s = slb.as_bytes();
            let mut out = [0u8; 192];
            let n = s.len().min(out.len() - 1);
            out[..n].copy_from_slice(&s[..n]);
            out[n] = b'\n';
            let _ = sys_write_debug(&out[..=n]);
        }
        PipeOrRedirKind::Redir { path, append } => {
            let Some(vcap) = ensure_vfs_cap(sup_ep_cap, cached_vfs_cap) else {
                engine.push_line(b"redir: VFS unavailable", STYLE_WARN);
                return;
            };
            let mut norm = [0u8; 96];
            let Some(nlen) = resolve_path(engine.cwd_bytes(), path, &mut norm) else {
                engine.push_line(b"redir: invalid path", STYLE_WARN);
                return;
            };
            let target = &norm[..nlen];
            let mut buf = [0u8; 2048];
            let mut n = 0usize;
            for (row, &len) in lines.iter().zip(lens.iter()) {
                let s = &row[..(len as usize).min(TERM_COLS)];
                let copy = s.len().min(buf.len().saturating_sub(n + 1));
                buf[n..n + copy].copy_from_slice(&s[..copy]);
                n += copy;
                if n < buf.len() {
                    buf[n] = b'\n';
                    n += 1;
                }
            }
            match vfs_create_file(vcap, target, 0) {
                Ok(_) | Err(EEXIST) => {}
                Err(e) => {
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"redir create failed: ");
                    lb.push_i64(e as i64);
                    engine.push_line(lb.as_bytes(), STYLE_WARN);
                    return;
                }
            }
            let existing_tail = if append {
                if let Ok((_, sz, flags)) = vfs_stat(vcap, target) {
                    if (flags & VFS_FLAG_DIR) == 0 && sz > 0 && sz <= buf.len() {
                        let mut cur = [0u8; 2048];
                        if let Ok((got, _)) = vfs_read_file(vcap, target, 0, &mut cur[..sz]) {
                            Some((cur, got))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let mut final_buf = [0u8; 2048];
            let mut final_len = 0usize;
            if let Some((cur, got)) = existing_tail {
                let take = got.min(final_buf.len());
                final_buf[..take].copy_from_slice(&cur[..take]);
                final_len = take;
            }
            let take_new = n.min(final_buf.len().saturating_sub(final_len));
            final_buf[final_len..final_len + take_new].copy_from_slice(&buf[..take_new]);
            final_len += take_new;

            let mut off = 0usize;
            let mut ok = true;
            if final_len == 0 {
                ok = vfs_write_small(vcap, target, &[], false).is_ok();
            } else {
                while off < final_len {
                    let chunk = (final_len - off).min(180);
                    if vfs_write_small(vcap, target, &final_buf[off..off + chunk], off > 0).is_err() {
                        ok = false;
                        break;
                    }
                    off += chunk;
                }
            }
            if !ok {
                engine.push_line(b"redir write failed", STYLE_WARN);
                return;
            }
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"Wrote ");
            lb.push_u64(final_len as u64);
            lb.push_bytes(b" bytes -> ");
            lb.push_bytes(target);
            engine.push_line(lb.as_bytes(), STYLE_OK);

            let mut slb = LineBuilder::new();
            slb.push_bytes(b"[DRY-SHELL] CMD=redir op=");
            slb.push_bytes(if append { b">>" } else { b">" });
            slb.push_bytes(b" path=");
            slb.push_bytes(target);
            slb.push_bytes(b" bytes=");
            slb.push_u64(n as u64);
            let s = slb.as_bytes();
            let mut out = [0u8; 192];
            let dbg_n = s.len().min(out.len() - 1);
            out[..dbg_n].copy_from_slice(&s[..dbg_n]);
            out[dbg_n] = b'\n';
            let _ = sys_write_debug(&out[..=dbg_n]);
        }
    }
}
