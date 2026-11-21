// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Linux x86-64 Syscall Dispatch Table for `dry-linux-abi`.

use crate::linux_abi::fd_table::FdTable;
use crate::linux_abi::{
    guest_map_anon, guest_mem_read, guest_mem_read_cstr, guest_mem_write, reply_syscall_exit,
    reply_syscall_ret,
};
use crate::rdtsc_user;

const SYS_LX_READ: u64 = 0;
const SYS_LX_WRITE: u64 = 1;
const SYS_LX_OPEN: u64 = 2;
const SYS_LX_CLOSE: u64 = 3;
const SYS_LX_FSTAT: u64 = 5;
const SYS_LX_LSEEK: u64 = 8;
const SYS_LX_MMAP: u64 = 9;
const SYS_LX_MUNMAP: u64 = 11;
const SYS_LX_BRK: u64 = 12;
const SYS_LX_WRITEV: u64 = 20;
const SYS_LX_GETPID: u64 = 39;
const SYS_LX_EXIT: u64 = 60;
const SYS_LX_UNAME: u64 = 63;
const SYS_LX_ARCH_PRCTL: u64 = 158;
const SYS_LX_SET_TID_ADDRESS: u64 = 218;
const SYS_LX_CLOCK_GETTIME: u64 = 228;
const SYS_LX_EXIT_GROUP: u64 = 231;
const SYS_LX_OPENAT: u64 = 257;
const SYS_LX_GETRANDOM: u64 = 318;

const ARCH_SET_FS: u64 = 0x1002;
const MAP_ANONYMOUS: u64 = 0x20;
const AT_FDCWD: i32 = -100;

const LX_EINVAL: i64 = -22;
const LX_ENOMEM: i64 = -12;
const LX_EACCES: i64 = -13;
const LX_ENOSYS: i64 = -38;

const BRK_BASE: u64 = 0x0000_0020_0000_0000;
const BRK_LIMIT: u64 = 0x0000_0020_0100_0000;
const MMAP_BASE: u64 = 0x0000_0030_0000_0000;
const MMAP_LIMIT: u64 = 0x0000_0040_0000_0000;
const MAX_RW_CHUNK: usize = 4096;

pub struct GuestCtx {
    pub redirect_ep: u64,
    pub guest_pid: u64,
    pub vfs_cap: u64,
    pub tsc_mhz: u64,
    pub brk_cur: u64,
    pub brk_mapped: u64,
    pub mmap_next: u64,
    pub syscall_count: u64,
    pub exited: Option<i32>,
    pub fds: FdTable,
}

impl GuestCtx {
    pub const fn new(redirect_ep: u64, guest_pid: u64, vfs_cap: u64, tsc_mhz: u64) -> Self {
        Self {
            redirect_ep,
            guest_pid,
            vfs_cap,
            tsc_mhz,
            brk_cur: BRK_BASE,
            brk_mapped: BRK_BASE,
            mmap_next: MMAP_BASE,
            syscall_count: 0,
            exited: None,
            fds: FdTable::new(),
        }
    }

    pub fn handle_syscall(
        &mut self,
        nr: u64,
        args: [u64; 6],
        mut reply_tok: u64,
    ) -> Result<(), isize> {
        self.syscall_count += 1;
        let [a0, a1, a2, a3, _a4, _a5] = args;

        if nr == SYS_LX_EXIT || nr == SYS_LX_EXIT_GROUP {
            let code = a0 as i32;
            self.exited = Some(code);
            return reply_syscall_exit(self.redirect_ep, reply_tok, code);
        }

        let mut fs_update: Option<u64> = None;
        let ret: i64 = match nr {
            SYS_LX_READ => {
                let want = (a2 as usize).min(MAX_RW_CHUNK);
                let mut buf = [0u8; MAX_RW_CHUNK];
                let n = self.fds.read_fd(self.vfs_cap, a0 as usize, &mut buf[..want]);
                if n > 0 && guest_mem_write(self.redirect_ep, &mut reply_tok, a1, &buf[..(n as usize)]).is_err() {
                    -14
                } else {
                    n
                }
            }
            SYS_LX_WRITE => {
                let want = (a2 as usize).min(MAX_RW_CHUNK);
                let mut buf = [0u8; MAX_RW_CHUNK];
                if want > 0 && guest_mem_read(self.redirect_ep, &mut reply_tok, a1, &mut buf[..want]).is_err() {
                    -14
                } else {
                    self.fds.write_fd(self.vfs_cap, a0 as usize, &buf[..want])
                }
            }
            SYS_LX_WRITEV => {
                let cnt = (a2 as usize).min(8);
                let mut iov_raw = [0u8; 8 * 16];
                let ilen = cnt * 16;
                if guest_mem_read(self.redirect_ep, &mut reply_tok, a1, &mut iov_raw[..ilen]).is_err() {
                    -14
                } else {
                    let mut total = 0i64;
                    let mut chunk = [0u8; 512];
                    for i in 0..cnt {
                        let base = u64::from_le_bytes(iov_raw[(i * 16)..(i * 16 + 8)].try_into().unwrap());
                        let len = u64::from_le_bytes(iov_raw[(i * 16 + 8)..(i * 16 + 16)].try_into().unwrap()) as usize;
                        let take = len.min(chunk.len());
                        if take == 0 {
                            continue;
                        }
                        if guest_mem_read(self.redirect_ep, &mut reply_tok, base, &mut chunk[..take]).is_err() {
                            total = -14;
                            break;
                        }
                        let w = self.fds.write_fd(self.vfs_cap, a0 as usize, &chunk[..take]);
                        if w < 0 {
                            total = w;
                            break;
                        }
                        total += w;
                    }
                    total
                }
            }
            SYS_LX_OPEN => {
                let mut pbuf = [0u8; 96];
                match guest_mem_read_cstr(self.redirect_ep, &mut reply_tok, a0, &mut pbuf) {
                    Ok(n) => self.fds.open_path(self.vfs_cap, &pbuf[..n], a1 as u32),
                    Err(_) => -14,
                }
            }
            SYS_LX_OPENAT => {
                if (a0 as i32) != AT_FDCWD {
                    LX_EINVAL
                } else {
                    let mut pbuf = [0u8; 96];
                    match guest_mem_read_cstr(self.redirect_ep, &mut reply_tok, a1, &mut pbuf) {
                        Ok(n) => self.fds.open_path(self.vfs_cap, &pbuf[..n], a2 as u32),
                        Err(_) => -14,
                    }
                }
            }
            SYS_LX_CLOSE => self.fds.close_fd(a0 as usize),
            SYS_LX_FSTAT => {
                let mut st = [0u8; 144];
                let r = self.fds.fill_stat(a0 as usize, &mut st);
                if r == 0 && guest_mem_write(self.redirect_ep, &mut reply_tok, a1, &st).is_err() {
                    -14
                } else {
                    r
                }
            }
            SYS_LX_LSEEK => self.fds.lseek_fd(a0 as usize, a1 as i64, a2),
            SYS_LX_BRK => {
                if !(BRK_BASE..=BRK_LIMIT).contains(&a0) {
                    self.brk_cur as i64
                } else {
                    let target_map = (a0 + 4095) & !4095;
                    if target_map > self.brk_mapped {
                        let pages = (target_map - self.brk_mapped) / 4096;
                        if guest_map_anon(self.redirect_ep, &mut reply_tok, self.brk_mapped, pages, 3).is_ok() {
                            self.brk_mapped = target_map;
                            self.brk_cur = a0;
                        }
                    } else {
                        self.brk_cur = a0;
                    }
                    self.brk_cur as i64
                }
            }
            SYS_LX_MMAP => {
                let len = a1;
                let prot = a2;
                let flags = a3;
                let wx = (prot & 2) != 0 && (prot & 4) != 0;
                if len == 0 || len > (256 * 1024) || (flags & MAP_ANONYMOUS) == 0 || wx {
                    if wx { LX_EACCES } else { LX_EINVAL }
                } else {
                    let pages = len.div_ceil(4096);
                    let va = self.mmap_next;
                    let next = va + pages * 4096;
                    if next > MMAP_LIMIT {
                        LX_ENOMEM
                    } else if guest_map_anon(self.redirect_ep, &mut reply_tok, va, pages, prot & 7).is_ok() {
                        self.mmap_next = next;
                        va as i64
                    } else {
                        LX_ENOMEM
                    }
                }
            }
            SYS_LX_MUNMAP => 0,
            SYS_LX_GETPID => self.guest_pid as i64,
            SYS_LX_UNAME => {
                let mut uts = [0u8; 390];
                uts[0..6].copy_from_slice(b"DRY-OS");
                uts[65..70].copy_from_slice(b"dryos");
                uts[130..135].copy_from_slice(b"0.9.0");
                uts[195..208].copy_from_slice(b"Ascension-9.0");
                uts[260..266].copy_from_slice(b"x86_64");
                uts[325..331].copy_from_slice(b"(none)");
                if guest_mem_write(self.redirect_ep, &mut reply_tok, a0, &uts).is_err() {
                    -14
                } else {
                    0
                }
            }
            SYS_LX_ARCH_PRCTL => {
                if a0 == ARCH_SET_FS {
                    fs_update = Some(a1);
                    0
                } else {
                    LX_EINVAL
                }
            }
            SYS_LX_SET_TID_ADDRESS => self.guest_pid as i64,
            SYS_LX_CLOCK_GETTIME => {
                let ticks = rdtsc_user();
                let mhz = self.tsc_mhz.max(1000);
                let us = ticks / mhz;
                let mut ts = [0u8; 16];
                ts[0..8].copy_from_slice(&((us / 1_000_000) as i64).to_le_bytes());
                ts[8..16].copy_from_slice(&(((us % 1_000_000) * 1000) as i64).to_le_bytes());
                if guest_mem_write(self.redirect_ep, &mut reply_tok, a1, &ts).is_err() {
                    -14
                } else {
                    0
                }
            }
            SYS_LX_GETRANDOM => {
                let n = (a1 as usize).min(256);
                let mut rnd = [0u8; 256];
                let mut s = rdtsc_user() ^ 0x9e37_79b9_7f4a_7c15;
                for (i, b) in rnd[..n].iter_mut().enumerate() {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    *b = ((s >> ((i & 7) * 8)) & 0xff) as u8;
                }
                if guest_mem_write(self.redirect_ep, &mut reply_tok, a0, &rnd[..n]).is_err() {
                    -14
                } else {
                    n as i64
                }
            }
            _ => LX_ENOSYS,
        };

        reply_syscall_ret(self.redirect_ep, reply_tok, ret, fs_update)
    }
}
