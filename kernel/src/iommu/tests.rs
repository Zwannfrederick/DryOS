// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Self-contained unit tests for ACPI `DMAR` parsing (`Milestone 2C-2`).

use core::ptr::read_volatile;

use crate::iommu::dmar::{parse_dmar_bytes, DmarError};
use crate::iommu::ivrs::parse_ivrs_bytes;
use crate::iommu::ops::{
    cmdline_has_token, evaluate_iommu_policy_flags, is_dma_allowed_status,
    is_hw_enforcing_status, IommuBackendKind, IommuCaps, IommuOps, IommuPerms, IommuStatus,
};
use crate::iommu::{
    active_iommu_ops, backend_kind, caps, fail_closed_ops_for_test, status,
};
use crate::iommu::slpt::{
    clear_slpt_4k, flush_entire_page, free_all_domain_frames, is_dedicated_gop_drhd, map_slpt_4k,
    DomainState, VTD_KERNEL_LABEL,
};
use crate::pmm::{alloc_pages, phys_to_hhdm, AllocFlags, PageOrder};
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

    // 6. Dual-DRHD active-GOP deferral check (dedicated non-include_all 00:02.0 + catch-all + xHCI RMRR)
    // 48B DMAR + 24B DRHD0 (00:02.0) + 16B DRHD1 (INCLUDE_ALL) + 32B RMRR (00:14.0) = 120B
    let mut tgl = [0u8; 120];
    tgl[0..4].copy_from_slice(b"DMAR");
    tgl[4..8].copy_from_slice(&120u32.to_le_bytes());
    tgl[8] = 1;
    tgl[36] = 38; // HAW = 39-bit (Tiger Lake)
    // DRHD 0: type=0 len=24 flags=0 reg=0xfed9_0000 scope=00:02.0
    tgl[50..52].copy_from_slice(&24u16.to_le_bytes());
    tgl[56..64].copy_from_slice(&0x0000_0000_fed9_0000u64.to_le_bytes());
    tgl[64] = 1;
    tgl[65] = 8;
    tgl[70] = 2;
    // DRHD 1: type=0 len=16 flags=1 INCLUDE_PCI_ALL reg=0xfed9_1000
    tgl[74..76].copy_from_slice(&16u16.to_le_bytes());
    tgl[76] = 1;
    tgl[80..88].copy_from_slice(&0x0000_0000_fed9_1000u64.to_le_bytes());
    // RMRR: type=1 len=32 scope=00:14.0
    tgl[88..90].copy_from_slice(&1u16.to_le_bytes());
    tgl[90..92].copy_from_slice(&32u16.to_le_bytes());
    tgl[96..104].copy_from_slice(&0x0000_0000_7c00_0000u64.to_le_bytes());
    tgl[104..112].copy_from_slice(&0x0000_0000_7c00_3fffu64.to_le_bytes());
    tgl[112] = 1;
    tgl[113] = 8;
    tgl[118] = 0x14;
    finalize_checksum(&mut tgl);
    let tgl_dmar = parse_dmar_bytes(&tgl, false).expect("TGL dual-DRHD DMAR must parse");
    assert_eq!(tgl_dmar.haw, 39);
    assert_eq!(tgl_dmar.drhd_count, 2);
    assert!(is_dedicated_gop_drhd(&tgl_dmar.drhds[0], Some((0, 2, 0))));
    assert!(!is_dedicated_gop_drhd(&tgl_dmar.drhds[1], Some((0, 2, 0))));
    assert_eq!(tgl_dmar.rmrrs[0].scopes[0].dev, 0x14);

    // 7. 39-bit 3-level SLPT (`levels_4 = false`) boundary & full frame reclamation
    let mut dom39 = DomainState::empty(11);
    let root_h = alloc_pages(PageOrder::Order4KiB, VTD_KERNEL_LABEL, AllocFlags::ZERO)
        .expect("SLPT39 root alloc");
    flush_entire_page(true, root_h.hhdm_ptr());
    dom39.sl_root = Some(root_h);
    let gpa_ok = 0x40_0000_0000u64; // 256 GiB (< 512 GiB = 1<<39)
    let gpa_oob = 0x80_0000_0000u64; // 512 GiB (== 1<<39, invalid for 39-bit SLPT)
    assert!(map_slpt_4k(&mut dom39, true, gpa_ok, false).is_ok());
    assert_eq!(dom39.pt_count, 2); // PDPT -> PD + PT = 2 intermediate frames
    assert!(map_slpt_4k(&mut dom39, true, gpa_oob, false).is_err());
    let lvl2_idx = ((gpa_ok >> 30) & 0x1ff) as usize;
    // SAFETY: `root_h` is a mapped 4 KiB HHDM page populated by `map_slpt_4k`.
    let root_entry = unsafe { read_volatile((phys_to_hhdm(root_h.phys_addr()) as *const u64).add(lvl2_idx)) };
    assert_ne!(root_entry & 0x3, 0);
    clear_slpt_4k(true, root_h.phys_addr(), gpa_ok, false);
    free_all_domain_frames(&mut dom39);
    assert_eq!(dom39.pt_count, 0);

    serial_println!("[DRY-IOMMU] PASS: slpt39_rmrr_unit=2");

    // 8. ACPI IVRS parser unit tests (IVHD 0x10 + IVHD 0x11 + IVMD 0x21 + negative bounds)
    let mut ivrs = [0u8; 144];
    ivrs[0..4].copy_from_slice(b"IVRS");
    ivrs[4..8].copy_from_slice(&144u32.to_le_bytes());
    ivrs[8] = 2;
    let iv_info: u32 = 1 | (40u32 << 8) | (48u32 << 15);
    ivrs[36..40].copy_from_slice(&iv_info.to_le_bytes());
    // Entry 0 (offset 48, len 24): IVHD type 0x10
    ivrs[48] = 0x10;
    ivrs[50..52].copy_from_slice(&24u16.to_le_bytes());
    ivrs[52..54].copy_from_slice(&0x0002u16.to_le_bytes());
    ivrs[54..56].copy_from_slice(&0x0040u16.to_le_bytes());
    ivrs[56..64].copy_from_slice(&0x0000_0000_feb8_0000u64.to_le_bytes());
    // Entry 1 (offset 72, len 40): IVHD type 0x11
    ivrs[72] = 0x11;
    ivrs[74..76].copy_from_slice(&40u16.to_le_bytes());
    ivrs[76..78].copy_from_slice(&0x0002u16.to_le_bytes());
    ivrs[80..88].copy_from_slice(&0x0000_0000_feb9_0000u64.to_le_bytes());
    // Entry 2 (offset 112, len 32): IVMD type 0x21
    ivrs[112] = 0x21;
    ivrs[114..116].copy_from_slice(&32u16.to_le_bytes());
    ivrs[116..118].copy_from_slice(&0x0010u16.to_le_bytes());
    ivrs[128..136].copy_from_slice(&0x0000_0000_7c00_0000u64.to_le_bytes());
    ivrs[136..144].copy_from_slice(&0x0000_0000_0001_0000u64.to_le_bytes());
    finalize_checksum(&mut ivrs);
    let iv_ok = parse_ivrs_bytes(&ivrs).expect("valid IVRS must parse");
    assert_eq!(iv_ok.pa_bits, 40);
    assert_eq!(iv_ok.va_bits, 48);
    assert_eq!(iv_ok.ivhd_count, 2);
    assert_eq!(iv_ok.ivmd_count, 1);
    assert!(iv_ok.ivhds[0].valid && iv_ok.ivhds[0].entry_type == 0x10);
    assert_eq!(iv_ok.ivhds[0].mmio_base, 0x0000_0000_feb8_0000);
    assert_eq!(iv_ok.ivhds[0].iommu_bdf, 0x0002);
    assert_eq!(iv_ok.ivhds[0].cap_offset, 0x0040);
    assert!(!iv_ok.ivhds[0].efr_sup && iv_ok.ivhds[1].efr_sup);
    assert_eq!(
        (iv_ok.ivhds[1].flags as u16) | iv_ok.ivhds[1].segment | iv_ok.ivhds[1].iommu_info,
        0
    );
    assert!(iv_ok.ivmds[0].valid && iv_ok.ivmds[0].entry_type == 0x21);
    assert_eq!(iv_ok.ivmds[0].device_id, 0x0010);
    assert_eq!(iv_ok.ivmds[0].start_addr, 0x0000_0000_7c00_0000);
    assert_eq!(iv_ok.ivmds[0].mem_len, 0x0000_0000_0001_0000);
    assert_eq!(iv_ok.ivmds[0].flags, 0);

    let mut bad_ivrs_csum = ivrs;
    bad_ivrs_csum[9] ^= 0xff;
    assert!(parse_ivrs_bytes(&bad_ivrs_csum).is_err());

    let mut bad_ivrs_align = ivrs;
    bad_ivrs_align[56..64].copy_from_slice(&0x0000_0000_feb8_0100u64.to_le_bytes());
    finalize_checksum(&mut bad_ivrs_align);
    assert!(parse_ivrs_bytes(&bad_ivrs_align).is_err());

    let mut bad_ivrs_zero = ivrs;
    bad_ivrs_zero[50..52].copy_from_slice(&0u16.to_le_bytes());
    finalize_checksum(&mut bad_ivrs_zero);
    assert!(parse_ivrs_bytes(&bad_ivrs_zero).is_err());

    let mut bad_ivrs_short11 = ivrs;
    bad_ivrs_short11[74..76].copy_from_slice(&24u16.to_le_bytes());
    bad_ivrs_short11[96] = 0x21;
    bad_ivrs_short11[98..100].copy_from_slice(&32u16.to_le_bytes());
    bad_ivrs_short11[4..8].copy_from_slice(&128u32.to_le_bytes());
    finalize_checksum(&mut bad_ivrs_short11[..128]);
    assert!(parse_ivrs_bytes(&bad_ivrs_short11[..128]).is_err());

    let mut bad_ivmd_len = ivrs;
    bad_ivmd_len[136..144].copy_from_slice(&0u64.to_le_bytes());
    finalize_checksum(&mut bad_ivmd_len);
    assert!(parse_ivrs_bytes(&bad_ivmd_len).is_err());

    serial_println!("[DRY-IOMMU] PASS: ivrs_unit=6");

    // 9. Fail-closed IOMMU policy & state transition security matrix
    let matrix = [
        (false, false, false, false),
        (false, true, false, false),
        (false, true, true, false),
        (true, false, false, false),
        (true, false, true, false),
        (true, true, true, false),
        (false, false, true, false),
        (true, true, false, true),
    ];
    for (hv, opt, live, expect) in matrix {
        assert_eq!(evaluate_iommu_policy_flags(hv, opt, live), expect);
    }
    assert!(!cmdline_has_token(
        "console=com1 xdry_qemu_test_no_iommu=1",
        "dry_qemu_test_no_iommu=1"
    ));
    assert!(cmdline_has_token(
        "console=com1 dry_qemu_test_no_iommu=1 desktop4=1",
        "dry_qemu_test_no_iommu=1"
    ));
    assert!(!is_dma_allowed_status(IommuStatus::Uninitialized));
    assert!(!is_dma_allowed_status(IommuStatus::UnsupportedFailClosed));
    assert!(!is_dma_allowed_status(IommuStatus::FailedClosed));
    assert!(is_dma_allowed_status(IommuStatus::TestUnsafeNoIommu));
    assert!(is_dma_allowed_status(IommuStatus::Enforcing));
    assert!(!is_hw_enforcing_status(IommuStatus::TestUnsafeNoIommu));
    assert!(is_hw_enforcing_status(IommuStatus::Enforcing));
    assert!(IommuPerms::RW.contains(IommuPerms::READ));
    let none_caps = IommuCaps::none();
    assert!(
        none_caps.max_aw_bits == 0
            && none_caps.backend == IommuBackendKind::None
            && !none_caps.page_4k
            && !none_caps.page_2m
            && !none_caps.coherent
            && !none_caps.int_remap
            && !none_caps.fault_reporting
            && !none_caps.reserved_regions
    );
    let active_ops: &dyn IommuOps = active_iommu_ops();
    assert_eq!(active_ops.kind(), backend_kind());
    let cur_caps = caps();
    assert_eq!(active_ops.caps().backend, cur_caps.backend);
    assert!(active_ops.attach_device(usize::MAX, 0, 0, 0).is_err());
    active_ops.detach_device(usize::MAX);
    assert!(active_ops.map_iova(usize::MAX, 0, 0, 4096, IommuPerms::RW).is_err());
    active_ops.unmap_iova(usize::MAX, 0, 4096);
    let _ = active_ops.poll_faults();
    let _ = active_ops.apply_reserved_regions(0);
    let fc_ops = fail_closed_ops_for_test();
    assert!(fc_ops.attach_device(0, 0, 0, 0).is_err());
    assert!(fc_ops.map_iova(0, 0, 0, 4096, IommuPerms::READ).is_err());
    fc_ops.detach_device(0);
    fc_ops.unmap_iova(0, 0, 4096);
    let _ = fc_ops.poll_faults();
    assert!(!status().as_str().is_empty());
    assert!(!IommuBackendKind::None.as_str().is_empty());
    assert!(active_ops.max_dma_mask() == 0 || active_ops.max_dma_mask() >= 0xffff_ffff);

    serial_println!("[DRY-IOMMU] PASS: policy_unit=17");
}
