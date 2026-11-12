// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure `no_std` Cryptographic & Integrity Primitives (`Milestone 2B` — `ADR-006`, `ADR-007`).
//!
//! Provides standard, zero-dependency implementations of:
//! - FIPS 180-4 `SHA-256`
//! - RFC 2104 `HMAC-SHA256`
//! - RFC 8018 `PBKDF2-HMAC-SHA256` (`32`-byte verifier derivation)
//! - Constant-time `32`-byte verifier comparison (`ct_eq_32`) + volatile zeroize (`scrub_bytes`)
//! - IEEE 802.3 `CRC32` (`0xEDB88320`) for `DRYFS_v1` superblocks, data blocks, and `vault.sec`

use core::hint::black_box;

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const SHA256_IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    pub const fn new() -> Self {
        Self {
            state: SHA256_IV,
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    fn compress_block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, slot) in w.iter_mut().enumerate().take(16) {
            let b = i * 4;
            *slot = u32::from_be_bytes([block[b], block[b + 1], block[b + 2], block[b + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            if data.len() < need {
                self.buf[self.buf_len..self.buf_len + data.len()].copy_from_slice(data);
                self.buf_len += data.len();
                return;
            }
            self.buf[self.buf_len..64].copy_from_slice(&data[..need]);
            let blk = self.buf;
            self.compress_block(&blk);
            self.buf_len = 0;
            data = &data[need..];
        }

        while data.len() >= 64 {
            let mut blk = [0u8; 64];
            blk.copy_from_slice(&data[..64]);
            self.compress_block(&blk);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        if self.buf_len > 56 {
            self.buf[self.buf_len..64].fill(0);
            let blk = self.buf;
            self.compress_block(&blk);
            self.buf_len = 0;
        }

        self.buf[self.buf_len..56].fill(0);
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let blk = self.buf;
        self.compress_block(&blk);

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn sha256_digest(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// RFC 2104 `HMAC-SHA256(key, msg_a || msg_b)`.
pub fn hmac_sha256_two(key: &[u8], msg_a: &[u8], msg_b: &[u8]) -> [u8; 32] {
    let mut k_pad = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256_digest(key);
        k_pad[..32].copy_from_slice(&hashed);
    } else {
        k_pad[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k_pad[i];
        opad[i] ^= k_pad[i];
    }

    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(msg_a);
    inner.update(msg_b);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_digest);
    let out = outer.finalize();
    scrub_bytes(&mut k_pad);
    scrub_bytes(&mut ipad);
    scrub_bytes(&mut opad);
    out
}

/// RFC 8018 `PBKDF2-HMAC-SHA256` deriving a `32`-byte (`1` SHA-256 block) verifier.
pub fn pbkdf2_hmac_sha256_32(password: &[u8], salt: &[u8; 16], iterations: u32) -> [u8; 32] {
    let iters = iterations.max(1);
    let block_idx = 1u32.to_be_bytes();
    let mut u = hmac_sha256_two(password, salt, &block_idx);
    let mut t = u;
    for _ in 1..iters {
        u = hmac_sha256_two(password, &u, &[]);
        for j in 0..32 {
            t[j] ^= u[j];
        }
    }
    scrub_bytes(&mut u);
    t
}

/// Constant-time `32`-byte equality check (no data-dependent branches).
#[inline(never)]
pub fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    black_box(diff) == 0
}

/// Volatile memory scrub so compiler dead-store elimination never elides passphrase/key zeroing.
#[inline(never)]
pub fn scrub_bytes(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid mutable byte reference.
        unsafe {
            core::ptr::write_volatile(b, 0);
        }
    }
    let _ = black_box(buf.as_ptr());
}

/// Update a running inverted IEEE 802.3 `CRC-32` state (`init = 0xffff_ffff`, finalize with `!state`).
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    crc
}

/// IEEE 802.3 `CRC-32` (`0xEDB88320`) over arbitrary byte slice.
pub fn crc32_ieee(data: &[u8]) -> u32 {
    !crc32_update(0xffff_ffff, data)
}
