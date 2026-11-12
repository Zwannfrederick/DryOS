// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone 2C-0 Adaptive Driver Islands PoC Protocol (`ADR-016`):
//! Capability-Gated `Device Broker` (`0x0008`, `SecDomain::RootSystem`) + Isolated Ring 3
//! `Mock Driver Island` (`0x0209`, `SecDomain::Userspace`) Opcodes, Badges, Deterministic
//! Request Transform, and `no_std` Percentile Telemetry.

use crate::ioring::{IoCqe, IoSqe};

// Compartment IDs (`ADR-016` prototype compartments)
pub const COMP_ID_DEV_BROKER: u16 = 0x0008;
pub const COMP_ID_MOCK_ISLAND: u16 = 0x0209;

// Unforgeable capability badges on `broker_ep` (`dry-broker.elf`)
pub const BADGE_BROKER_ADMIN: u64 = 0x0008_ffff;
pub const BADGE_BROKER_CLIENT: u64 = 0x0008_8001;
pub const BADGE_BROKER_ROGUE: u64 = 0x0008_dead;

// Unforgeable capability badges on `drv_ep` (`dry-mock-drv.elf`)
pub const BADGE_ISLAND_BROKER: u64 = 0x0209_0001;
pub const BADGE_ISLAND_SESSION_BASE: u64 = 0x0209_1000;

#[inline]
pub const fn island_session_badge(generation: u64) -> u64 {
    BADGE_ISLAND_SESSION_BASE | (generation & 0xff)
}

// `Notification` doorbell bits on `sq_notif`
pub const ISLAND_DOORBELL_RING: u64 = 1 << 0;
pub const ISLAND_DOORBELL_CTL: u64 = 1 << 1;

// `IoRingSharedPage` opcode processed by `dry-mock-drv.elf`
pub const IORING_OP_MOCK_TRANSFORM: u8 = 16;

// `Mock Driver Island` (`dry-mock-drv.elf`) IPC opcodes
pub const MOCK_OP_BIND_NOTIF: u32 = 150;
pub const MOCK_OP_INIT_ISLAND: u32 = 151;
pub const MOCK_OP_TRANSFORM: u32 = 152;
pub const MOCK_OP_WAKE_PROBE: u32 = 153;
pub const MOCK_OP_SET_PARK_MODE: u32 = 154;

// Error reply opcode when client supplies a stale/revoked generation token (`-EREVOKED`)
pub const IPC_OP_ERR_STALE_GEN: u32 = 159;

// `Device Broker` (`dry-broker.elf`) IPC opcodes
pub const BROKER_OP_QUERY_STATE: u32 = 160;
pub const BROKER_OP_FORWARD_TRANSFORM: u32 = 161;
pub const BROKER_OP_ACQUIRE_EP: u32 = 162;
pub const BROKER_OP_ACQUIRE_RING: u32 = 163;
pub const BROKER_OP_SET_PARK_MODE: u32 = 164;
pub const BROKER_OP_CRASH_AND_RECOVER: u32 = 165;
pub const BROKER_OP_SHUTDOWN: u32 = 166;

// Broker-tracked driver island lifecycle states
pub const ISLAND_STATE_STOPPED: u64 = 0;
pub const ISLAND_STATE_RUNNING: u64 = 1;
pub const ISLAND_STATE_FAULTED: u64 = 2;

// Test seed & payload words shared between PoC harness and Python E2E verifier
pub const POC_TEST_SEED: u64 = 0x4452_5932_4330_3031;
pub const POC_TEST_WORD0: u64 = 0x1122_3344_5566_7788;
pub const POC_TEST_WORD1: u64 = 0x99aa_bbcc_ddee_ff00;

/// Deterministic 64-bit SplitMix64 + IEEE 802.3 CRC32 packet transform parameterized by the
/// active island `generation` so stale-generation replay is cryptographically/checksum-distinct.
#[inline]
pub fn mock_island_transform(seed: u64, word0: u64, word1: u64, generation: u64) -> u64 {
    let mut x = seed ^ generation.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for w in [word0, word1] {
        let mut z = x.wrapping_add(w).wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x = z ^ (z >> 31);
    }

    let mut crc: u32 = 0xffff_ffff;
    for chunk in [
        seed.to_le_bytes(),
        word0.to_le_bytes(),
        word1.to_le_bytes(),
        generation.to_le_bytes(),
    ] {
        for b in chunk {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }
    crc = !crc;
    (x & 0xffff_ffff_0000_0000) | (crc as u64)
}

#[inline]
pub const fn pack_mock_sqe(seed: u64, word0: u64, word1: u64) -> IoSqe {
    IoSqe {
        opcode: IORING_OP_MOCK_TRANSFORM,
        flags: 0,
        sector_count: 1,
        buf_offset: seed as u32,
        lba: word0,
        user_tag: word1,
        _reserved: seed,
    }
}

#[inline]
pub fn execute_mock_sqe(sqe: IoSqe, generation: u64) -> IoCqe {
    if sqe.opcode != IORING_OP_MOCK_TRANSFORM {
        return IoCqe {
            user_tag: 0,
            status: -22,
            bytes_done: 0,
        };
    }
    let digest = mock_island_transform(sqe._reserved, sqe.lba, sqe.user_tag, generation);
    IoCqe {
        user_tag: digest,
        status: 0,
        bytes_done: generation as u32,
    }
}

/// Fixed-capacity (`64` samples) zero-allocation latency distribution calculator (`p50/p95/p99`).
pub struct LatencySamples64 {
    buf: [u64; 64],
    len: usize,
}

impl LatencySamples64 {
    pub const fn new() -> Self {
        Self {
            buf: [0; 64],
            len: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, val: u64) {
        if self.len < self.buf.len() {
            self.buf[self.len] = val;
            self.len += 1;
        }
    }

    /// Sort in-place and return `(min, p50, p95, p99, max)`.
    pub fn summary(&mut self) -> (u64, u64, u64, u64, u64) {
        if self.len == 0 {
            return (0, 0, 0, 0, 0);
        }
        let mut i = 1usize;
        while i < self.len {
            let key = self.buf[i];
            let mut j = i;
            while j > 0 && self.buf[j - 1] > key {
                self.buf[j] = self.buf[j - 1];
                j -= 1;
            }
            self.buf[j] = key;
            i += 1;
        }
        let n = self.len;
        let idx50 = (n * 50) / 100;
        let idx95 = ((n * 95) / 100).min(n - 1);
        let idx99 = ((n * 99) / 100).min(n - 1);
        (
            self.buf[0],
            self.buf[idx50.min(n - 1)],
            self.buf[idx95],
            self.buf[idx99],
            self.buf[n - 1],
        )
    }
}

impl Default for LatencySamples64 {
    fn default() -> Self {
        Self::new()
    }
}
