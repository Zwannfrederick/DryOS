// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Passive CPU, scheduler, TSC, memory map, and safe scratch-frame diagnostics.

use core::ptr::{read_volatile, write_volatile};

use crate::hwdiag::{
    DiagContext, DiagSessionReport, DiagTestRecord, CAT_CPU, CAT_MEM, SCOPE_CPU, SCOPE_MEM,
    STATUS_FAIL, STATUS_PASS,
};
use crate::shell::LineBuilder;
use crate::vault_crypto::crc32_ieee;
use crate::{
    rdtsc_user, sys_cap_revoke, sys_frame_alloc, sys_sysinfo, sys_vspace_map, sys_yield,
    UserSysInfo, VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const DIAG_SCRATCH_VADDR: u64 = 0x1F00_0000;

fn tsc_us(t0: u64, t1: u64, mhz: u64) -> u32 {
    (t1.saturating_sub(t0) / mhz.max(1000)).max(1) as u32
}

pub fn run(ctx: &DiagContext, scope: u32, _safe: bool, out: &mut DiagSessionReport) {
    let mhz = ctx.tsc_mhz.max(1000);

    if (scope & SCOPE_CPU) != 0 {
        // CPU-01: CPUID features
        let t0 = rdtsc_user();
        let f = ctx.ext.cpuid_features;
        let sse2 = (f & (1 << 0)) != 0;
        let nx = (f & (1 << 14)) != 0;
        let avx = (f & (1 << 1)) != 0;
        let avx2 = (f & (1 << 9)) != 0;
        let smep = (f & (1 << 11)) != 0;
        let smap = (f & (1 << 12)) != 0;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"avx=");
        lb.push_u64(u64::from(avx));
        lb.push_bytes(b" avx2=");
        lb.push_u64(u64::from(avx2));
        lb.push_bytes(b" smep=");
        lb.push_u64(u64::from(smep));
        lb.push_bytes(b" smap=");
        lb.push_u64(u64::from(smap));
        let st = if sse2 && nx { STATUS_PASS } else { STATUS_FAIL };
        out.push(DiagTestRecord::make(
            CAT_CPU,
            st,
            b"CPU-01-CPUID",
            lb.as_bytes(),
            b"sse2=1 nx=1",
            if st == STATUS_PASS { b"none" } else { b"check_x86_features" },
            tsc_us(t0, rdtsc_user(), mhz),
            if st == STATUS_PASS { 0 } else { -22 },
        ));

        // CPU-02: APIC topology & SMP online count
        let t0 = rdtsc_user();
        let mut sys = UserSysInfo::default();
        let rc = sys_sysinfo(&mut sys);
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"cpus=");
        lb.push_u64(sys.online_cpus as u64);
        lb.push_bytes(b" khz=");
        lb.push_u64(ctx.ext.tsc_khz as u64);
        let ok = rc.is_ok() && sys.online_cpus >= 1 && ctx.ext.tsc_khz > 0;
        out.push(DiagTestRecord::make(
            CAT_CPU,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"CPU-02-APIC-SMP",
            lb.as_bytes(),
            b"cpus>=1 tsc_khz>0",
            if ok { b"none" } else { b"verify_madt_apic" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -5 },
        ));

        // CPU-03: TSC monotonicity
        let t0 = rdtsc_user();
        let mut prev = t0;
        let mut mono = true;
        for _ in 0..64 {
            let cur = rdtsc_user();
            if cur < prev {
                mono = false;
                break;
            }
            prev = cur;
        }
        let delta = prev.saturating_sub(t0);
        let inv = (f & (1 << 13)) != 0;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"delta=");
        lb.push_u64(delta);
        lb.push_bytes(b" inv_tsc=");
        lb.push_u64(u64::from(inv));
        let ok = mono && delta > 0;
        out.push(DiagTestRecord::make(
            CAT_CPU,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"CPU-03-TSC-MONO",
            lb.as_bytes(),
            b"monotonic delta>0",
            if ok { b"none" } else { b"check_tsc_sync" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -5 },
        ));

        // CPU-04: Bounded computational integrity test
        let t0 = rdtsc_user();
        let mut buf = [0u8; 256];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((i as u32 * 37 + 13) & 0xff) as u8;
        }
        let c1 = crc32_ieee(&buf);
        let c2 = crc32_ieee(&buf);
        let ok = c1 == c2 && c1 != 0;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"crc32=0x");
        lb.push_hex_width(c1 as u64, 8);
        out.push(DiagTestRecord::make(
            CAT_CPU,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"CPU-04-COMPUTE",
            lb.as_bytes(),
            b"deterministic_crc32",
            if ok { b"none" } else { b"cpu_math_mismatch" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -5 },
        ));

        // CPU-05: Scheduler yield & syscall switch integrity
        let t0 = rdtsc_user();
        let mut s0 = UserSysInfo::default();
        let mut s1 = UserSysInfo::default();
        let _ = sys_sysinfo(&mut s0);
        for _ in 0..8 {
            let _ = sys_yield();
        }
        let _ = sys_sysinfo(&mut s1);
        let d_sys = s1.total_syscalls.saturating_sub(s0.total_syscalls);
        let ok = d_sys >= 8 && s1.uptime_ms >= s0.uptime_ms;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"yield_sys=");
        lb.push_u64(d_sys);
        lb.push_bytes(b" up_ms=");
        lb.push_u64(s1.uptime_ms);
        out.push(DiagTestRecord::make(
            CAT_CPU,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"CPU-05-SCHED-CTX",
            lb.as_bytes(),
            b"yield_syscalls>=8",
            if ok { b"none" } else { b"check_sched_timer" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -5 },
        ));
    }

    if (scope & SCOPE_MEM) != 0 {
        // MEM-01: UEFI memory map summary validation
        let t0 = rdtsc_user();
        let ok = ctx.ext.mem_usable_mib > 0 && ctx.ext.mem_regions > 0;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"usable_mib=");
        lb.push_u64(ctx.ext.mem_usable_mib as u64);
        lb.push_bytes(b" reg=");
        lb.push_u64(ctx.ext.mem_regions as u64);
        out.push(DiagTestRecord::make(
            CAT_MEM,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"MEM-01-MEMMAP",
            lb.as_bytes(),
            b"usable_mib>0 reg>0",
            if ok { b"none" } else { b"check_uefi_memmap" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -12 },
        ));

        // MEM-02: PMM / VMM frame accounting invariant
        let t0 = rdtsc_user();
        let mut sys = UserSysInfo::default();
        let rc = sys_sysinfo(&mut sys);
        let ok = rc.is_ok() && sys.usable_frames_4k > 0 && sys.free_frames_4k <= sys.usable_frames_4k;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"free4k=");
        lb.push_u64(sys.free_frames_4k as u64);
        lb.push_bytes(b"/");
        lb.push_u64(sys.usable_frames_4k as u64);
        out.push(DiagTestRecord::make(
            CAT_MEM,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"MEM-02-PMM-INV",
            lb.as_bytes(),
            b"free<=total && >0",
            if ok { b"none" } else { b"pmm_accounting_err" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -12 },
        ));

        // MEM-03: Allocated-only frame alloc/map/pattern/unmap/revoke check (zero unknown PA writes)
        let t0 = rdtsc_user();
        let mut alloc_ok = false;
        let mut err_c = 0i32;
        if let Ok(fr_cap) = sys_frame_alloc(0) {
            if sys_vspace_map(fr_cap, DIAG_SCRATCH_VADDR, VSPACE_MAP_OP_4K_RW).is_ok() {
                let mut match_ok = true;
                for q in 0..512u64 {
                    let ptr = (DIAG_SCRATCH_VADDR + q * 8) as *mut u64;
                    let pat = 0xA55A_5AA5_0000_0000u64 ^ q.wrapping_mul(0x9E37_79B9);
                    // SAFETY: Newly allocated & mapped 4 KiB private scratch page at `DIAG_SCRATCH_VADDR`.
                    unsafe {
                        write_volatile(ptr, pat);
                        if read_volatile(ptr) != pat {
                            match_ok = false;
                            break;
                        }
                    }
                }
                let _ = sys_vspace_map(fr_cap, DIAG_SCRATCH_VADDR, VSPACE_MAP_OP_UNMAP_4K);
                alloc_ok = match_ok;
                if !match_ok {
                    err_c = -5;
                }
            } else {
                err_c = -14;
            }
            let _ = sys_cap_revoke(fr_cap, 1);
        } else {
            err_c = -12;
        }
        out.push(DiagTestRecord::make(
            CAT_MEM,
            if alloc_ok { STATUS_PASS } else { STATUS_FAIL },
            b"MEM-03-ALLOC-RW",
            if alloc_ok { b"4K_rw_pattern_verified" } else { b"4K_scratch_failed" },
            b"alloc_map_rw_free_ok",
            if alloc_ok { b"none" } else { b"check_vmm_pt_alloc" },
            tsc_us(t0, rdtsc_user(), mhz),
            err_c,
        ));

        // MEM-04: Capability boundary & guard isolation check
        let t0 = rdtsc_user();
        let null_rev = sys_cap_revoke(0, 1).err().unwrap_or(0);
        let bad_map = sys_vspace_map(0, 0xFFFF_FFFF_8000_0000, VSPACE_MAP_OP_4K_RW).err().unwrap_or(0);
        let ok = null_rev < 0 && bad_map < 0;
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"null_cap=");
        lb.push_i64(null_rev as i64);
        lb.push_bytes(b" kva=");
        lb.push_i64(bad_map as i64);
        out.push(DiagTestRecord::make(
            CAT_MEM,
            if ok { STATUS_PASS } else { STATUS_FAIL },
            b"MEM-04-CAP-GUARD",
            lb.as_bytes(),
            b"reject_null_and_kva",
            if ok { b"none" } else { b"cap_boundary_breach" },
            tsc_us(t0, rdtsc_user(), mhz),
            if ok { 0 } else { -1 },
        ));
    }
}
