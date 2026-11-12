// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Host-side Controlled Experiment (`Exp-B4`):
//! Compares synchronous per-operation RPC forwarding (1 syscall round-trip per op, modeling naive
//! API forwarding `E`) against DRY's shared-memory SPSC `IoRing` (`IoRingSharedPage`, `CAP = 32`)
//! with `eventfd` doorbells across batch sizes `1, 4, 8, 16, 32` across two OS threads/processes.

use std::arch::x86_64::_rdtsc;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

const RING_CAP: usize = 32;
const RING_MASK: u32 = (RING_CAP as u32) - 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IoSqe {
    opcode: u8,
    flags: u8,
    sector_count: u16,
    buf_offset: u32,
    lba: u64,
    user_tag: u64,
    _reserved: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IoCqe {
    user_tag: u64,
    status: i32,
    bytes_done: u32,
}

#[repr(C, align(4096))]
struct SharedRing {
    sq_head: AtomicU32,
    sq_tail: AtomicU32,
    cq_head: AtomicU32,
    cq_tail: AtomicU32,
    stop: AtomicBool,
    sqes: [IoSqe; RING_CAP],
    cqes: [IoCqe; RING_CAP],
}

impl SharedRing {
    fn new() -> Self {
        Self {
            sq_head: AtomicU32::new(0),
            sq_tail: AtomicU32::new(0),
            cq_head: AtomicU32::new(0),
            cq_tail: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            sqes: [IoSqe::default(); RING_CAP],
            cqes: [IoCqe::default(); RING_CAP],
        }
    }

    fn push_sqe(&self, sqe: IoSqe) -> bool {
        let tail = self.sq_tail.load(Ordering::Relaxed);
        let head = self.sq_head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= (RING_CAP as u32) {
            return false;
        }
        let idx = (tail & RING_MASK) as usize;
        unsafe {
            let slot = (&raw const self.sqes[idx]).cast_mut();
            std::ptr::write_volatile(slot, sqe);
        }
        self.sq_tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    fn pop_sqe(&self) -> Option<IoSqe> {
        let head = self.sq_head.load(Ordering::Relaxed);
        let tail = self.sq_tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let idx = (head & RING_MASK) as usize;
        let sqe = unsafe { std::ptr::read_volatile(&raw const self.sqes[idx]) };
        self.sq_head.store(head.wrapping_add(1), Ordering::Release);
        Some(sqe)
    }

    fn push_cqe(&self, cqe: IoCqe) -> bool {
        let tail = self.cq_tail.load(Ordering::Relaxed);
        let head = self.cq_head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= (RING_CAP as u32) {
            return false;
        }
        let idx = (tail & RING_MASK) as usize;
        unsafe {
            let slot = (&raw const self.cqes[idx]).cast_mut();
            std::ptr::write_volatile(slot, cqe);
        }
        self.cq_tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    fn pop_cqe(&self) -> Option<IoCqe> {
        let head = self.cq_head.load(Ordering::Relaxed);
        let tail = self.cq_tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let idx = (head & RING_MASK) as usize;
        let cqe = unsafe { std::ptr::read_volatile(&raw const self.cqes[idx]) };
        self.cq_head.store(head.wrapping_add(1), Ordering::Release);
        Some(cqe)
    }
}

unsafe impl Sync for SharedRing {}

fn percentile(sorted: &[u64], pct: f64) -> u64 {
    let idx = ((sorted.len() - 1) as f64 * pct).round() as usize;
    sorted[idx]
}

fn bench_rpc_socketpair(total_ops: usize) -> (f64, u64, u64, u64) {
    let (mut cli, mut srv) = UnixStream::pair().expect("socketpair");
    let worker = thread::spawn(move || {
        let mut req = [0u8; 32];
        let mut resp = [0u8; 16];
        for _ in 0..total_ops {
            srv.read_exact(&mut req).expect("srv read");
            resp[0..8].copy_from_slice(&req[16..24]);
            resp[12..16].copy_from_slice(&4096u32.to_le_bytes());
            srv.write_all(&resp).expect("srv write");
        }
    });

    let mut samples = Vec::with_capacity(total_ops);
    let mut req = [0u8; 32];
    let mut resp = [0u8; 16];
    let t0 = Instant::now();
    for i in 0..total_ops {
        req[0] = 1;
        req[16..24].copy_from_slice(&(i as u64).to_le_bytes());
        let c0 = unsafe { _rdtsc() };
        cli.write_all(&req).expect("cli write");
        cli.read_exact(&mut resp).expect("cli read");
        let c1 = unsafe { _rdtsc() };
        samples.push(c1.wrapping_sub(c0));
    }
    let elapsed_s = t0.elapsed().as_secs_f64();
    worker.join().expect("worker join");
    samples.sort_unstable();
    let mops = (total_ops as f64 / elapsed_s) / 1e6;
    (
        mops,
        percentile(&samples, 0.50),
        percentile(&samples, 0.95),
        percentile(&samples, 0.99),
    )
}

fn bench_ioring_doorbell(total_ops: usize, batch: usize) -> (f64, u64, u64, u64) {
    let ring = Arc::new(SharedRing::new());
    let (mut sq_bell_w, mut sq_bell_r) = UnixStream::pair().expect("sq bell");
    let (mut cq_bell_w, mut cq_bell_r) = UnixStream::pair().expect("cq bell");
    let ring_srv = Arc::clone(&ring);

    let worker = thread::spawn(move || {
        let mut tok = [0u8; 1];
        loop {
            if sq_bell_r.read_exact(&mut tok).is_err() || ring_srv.stop.load(Ordering::Acquire) {
                break;
            }
            let mut count = 0usize;
            while let Some(sqe) = ring_srv.pop_sqe() {
                let cqe = IoCqe {
                    user_tag: sqe.user_tag,
                    status: 0,
                    bytes_done: u32::from(sqe.sector_count) * 512,
                };
                while !ring_srv.push_cqe(cqe) {
                    std::hint::spin_loop();
                }
                count += 1;
            }
            if count > 0 {
                let _ = cq_bell_w.write_all(&[1u8]);
            }
        }
    });

    let rounds = total_ops / batch;
    let mut per_op_cycles = Vec::with_capacity(rounds);
    let mut tok = [0u8; 1];
    let t0 = Instant::now();
    let mut tag = 1u64;
    for _ in 0..rounds {
        let c0 = unsafe { _rdtsc() };
        for _ in 0..batch {
            let sqe = IoSqe {
                opcode: 1,
                flags: 0,
                sector_count: 8,
                buf_offset: 0,
                lba: tag,
                user_tag: tag,
                _reserved: 0,
            };
            assert!(ring.push_sqe(sqe));
            tag += 1;
        }
        sq_bell_w.write_all(&[1u8]).expect("ring sq bell");
        cq_bell_r.read_exact(&mut tok).expect("ring cq bell");
        let mut drained = 0usize;
        while drained < batch {
            if ring.pop_cqe().is_some() {
                drained += 1;
            } else {
                std::hint::spin_loop();
            }
        }
        let c1 = unsafe { _rdtsc() };
        per_op_cycles.push(c1.wrapping_sub(c0) / (batch as u64));
    }
    let elapsed_s = t0.elapsed().as_secs_f64();
    ring.stop.store(true, Ordering::Release);
    let _ = sq_bell_w.write_all(&[0u8]);
    worker.join().expect("worker join");

    per_op_cycles.sort_unstable();
    let mops = ((rounds * batch) as f64 / elapsed_s) / 1e6;
    (
        mops,
        percentile(&per_op_cycles, 0.50),
        percentile(&per_op_cycles, 0.95),
        percentile(&per_op_cycles, 0.99),
    )
}

fn main() {
    let total_ops = 32_768usize;
    let (rpc_mops, rpc_p50, rpc_p95, rpc_p99) = bench_rpc_socketpair(total_ops);
    println!("{{");
    println!("  \"experiment\": \"Exp-B4\",");
    println!("  \"total_ops_per_mode\": {total_ops},");
    println!(
        "  \"rpc_per_op_baseline\": {{\"mops_s\": {rpc_mops:.4}, \"cyc_per_op_p50\": {rpc_p50}, \"cyc_per_op_p95\": {rpc_p95}, \"cyc_per_op_p99\": {rpc_p99}}},"
    );
    println!("  \"ioring_doorbell_batches\": [");
    let batches = [1usize, 4, 8, 16, 32];
    for (i, &b) in batches.iter().enumerate() {
        let (mops, p50, p95, p99) = bench_ioring_doorbell(total_ops, b);
        let speedup = mops / rpc_mops;
        let comma = if i + 1 < batches.len() { "," } else { "" };
        println!(
            "    {{\"batch\": {b}, \"mops_s\": {mops:.4}, \"speedup_vs_rpc\": {speedup:.2}, \"cyc_per_op_p50\": {p50}, \"cyc_per_op_p95\": {p95}, \"cyc_per_op_p99\": {p99}}}{comma}"
        );
    }
    println!("  ]");
    println!("}}");
}
