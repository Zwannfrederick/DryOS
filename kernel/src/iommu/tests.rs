// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Self-contained unit tests for ACPI `DMAR` parsing (`Milestone 2C-2`).

use crate::iommu::dmar::{parse_dmar_bytes, DmarError};
use crate::serial_println;

fn finalize_checksum(buf: &mut [u8]) {
    buf[9] = 0;
    let sum = buf.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    buf[9] = 0u8.wrapping_sub(sum);
}

fn build_synthetic_dmar() -> [u8; 104] {
    // 48B DMAR header + 24B DRHD (16B + 8B scope) + 32B RMRR (24B + 8B scope) = 104B
    let mut b = [0u8; 104];
    b[0..4].copy_from_slice(b"DMAR");
    b[4..8].copy_from_slice(&104u32.to_le_bytes());
    b[8] = 1; // revision
    b[36] = 47; // HAW = 48-bit
    b[37] = 1; // INTR_REMAP flag

    // Offset 48: DRHD (type=0, len=24, flags=1 INCLUDE_PCI_ALL, seg=0, reg_base=0xfed9_0000)
    b[48..50].copy_from_slice(&0u16.to_le_bytes());
    b[50..52].copy_from_slice(&24u16.to_le_bytes());
    b[52] = 1;
    b[54..56].copy_from_slice(&0u16.to_le_bytes());
    b[56..64].copy_from_slice(&0x0000_0000_fed9_0000u64.to_le_bytes());
    // DRHD DevScope (len=8, bus=0, dev=2, func=0)
    b[64] = 1;
    b[65] = 8;
    b[69] = 0;
    b[70] = 2;
    b[71] = 0;

    // Offset 72: RMRR (type=1, len=32, seg=0, base=0x7fff_0000, limit=0x7fff_ffff)
    b[72..74].copy_from_slice(&1u16.to_le_bytes());
    b[74..76].copy_from_slice(&32u16.to_le_bytes());
    b[80..88].copy_from_slice(&0x0000_0000_7fff_0000u64.to_le_bytes());
    b[88..96].copy_from_slice(&0x0000_0000_7fff_ffffu64.to_le_bytes());
    // RMRR DevScope (len=8, bus=0, dev=0x1f, func=2)
    b[96] = 1;
    b[97] = 8;
    b[101] = 0;
    b[102] = 0x1f;
    b[103] = 2;

    finalize_checksum(&mut b);
    b
}

#[inline(never)]
pub fn run_dmar_unit_tests() {
    // 1. Valid synthetic DMAR with DRHD + RMRR + Device Scopes
    let valid = build_synthetic_dmar();
    let parsed = parse_dmar_bytes(&valid, false).expect("valid DMAR must parse");
    assert_eq!(parsed.haw, 48);
    assert_eq!(parsed.drhd_count, 1);
    assert!(parsed.drhds[0].include_all);
    assert_eq!(parsed.drhds[0].register_base_phys, 0xfed9_0000);
    assert_eq!(parsed.drhds[0].scope_count, 1);
    assert_eq!(parsed.drhds[0].scopes[0].dev, 2);
    assert_eq!(parsed.rmrr_count, 1);
    assert_eq!(parsed.rmrrs[0].base_addr, 0x7fff_0000);
    assert_eq!(parsed.rmrrs[0].limit_addr, 0x7fff_ffff);
    assert_eq!(parsed.rmrrs[0].scopes[0].dev, 0x1f);
    assert_eq!(parsed.rmrrs[0].scopes[0].func, 2);

    // 2. Bad checksum rejected
    let mut bad_csum = valid;
    bad_csum[9] ^= 0xff;
    assert_eq!(parse_dmar_bytes(&bad_csum, false).unwrap_err(), DmarError::BadChecksum);

    // 3. Zero structure length (infinite-loop DOS guard) rejected
    let mut zero_len = valid;
    zero_len[50..52].copy_from_slice(&0u16.to_le_bytes());
    finalize_checksum(&mut zero_len);
    assert_eq!(
        parse_dmar_bytes(&zero_len, false).unwrap_err(),
        DmarError::MalformedStructure
    );

    // 4. Truncated / OOB structure length rejected
    let mut oob_len = valid;
    oob_len[74..76].copy_from_slice(&512u16.to_le_bytes());
    finalize_checksum(&mut oob_len);
    assert_eq!(
        parse_dmar_bytes(&oob_len, false).unwrap_err(),
        DmarError::MalformedStructure
    );

    // 5. Inverted RMRR range (limit < base) rejected
    let mut bad_rmrr = valid;
    bad_rmrr[88..96].copy_from_slice(&0x0000_0000_7ffe_0000u64.to_le_bytes());
    finalize_checksum(&mut bad_rmrr);
    assert_eq!(
        parse_dmar_bytes(&bad_rmrr, false).unwrap_err(),
        DmarError::InvalidRange
    );

    serial_println!("[DRY-IOMMU] PASS: dmar_unit=5");
}
