// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-hw-test` — Milestone 2C-2 Ring-3 Hardware Foundations Verification Suite (`hw_test=1`).
//!
//! One ELF, two roles via `_start(arg0, arg1)`:
//! - **Child fault worker** (`arg1 != HW_TEST_BOOT_MAGIC`): claims `QEMU edu` BAR0, allocates pinned
//!   `VT-d` DMA memory, handles one `PCI MSI` interrupt under `IRQF_ONESHOT`, executes a DMA copy,
//!   and hits `#UD` (`ud2`) while armed to verify supervisor `FaultEndpoint` hardware quiesce.
//! - **Supervisor** (`arg0 == 0x0070_0000 && arg1 == HW_TEST_BOOT_MAGIC`):
//!   1. `ivshmem-plain` (`1AF4:1110`): `BAR64 > 4 GiB`, `WC` vs `UC` aliasing + bounds rejection.
//!   2. Supervised `#UD` child quiesce (`edu` `1234:11e8`) + `FaultEndpoint` recovery.
//!   3. Post-recovery `edu` `PCI MSI` (`0x40..=0x5F`) via `TCB`-bound `Notification` (`sys_ipc_recv_ext`)
//!      with direct `sys_notify_wait` `EBUSY` enforcement and `IRQF_ONESHOT` unmasking.
//!   4. Intel `VT-d` SLPT allowed DMA round-trip (`RAM <-> edu 0x40000`) + rogue unmapped DMA
//!      containment (`FRCD` hardware fault > 0, zero memory corruption).

#![no_std]
#![no_main]

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

use dry_userspace::init_legacy::log_line;
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, sys_cap_derive, sys_cap_revoke, sys_dev_map_window,
    sys_dev_query_bars, sys_dma_alloc, sys_ep_create, sys_exit, sys_ipc_recv_ext,
    sys_ipc_reply_inline, sys_irq_ack, sys_irq_bind, sys_notify_bind_tcb, sys_notify_create,
    sys_notify_signal, sys_notify_unbind_tcb, sys_notify_wait, sys_proc_spawn_with_fault_ep,
    sys_proc_wait, sys_sysinfo, UserIpcRecvExt, UserPciDeviceInfo, UserSysInfo, EBUSY, EINVAL,
    FAULT_MSG_LABEL, FAULT_REPLY_KILL, IPC_OP_BOUND_NOTIF, IRQF_ONESHOT, IRQ_ACK_UNBIND,
    IRQ_ACK_UNMASK, MSG_FLAG_NON_BLOCKING, PCI_BAR_FLAG_64BIT, PCI_BAR_FLAG_ABOVE_4G,
    PCI_BAR_FLAG_MMIO, PCI_BAR_FLAG_PREFETCHABLE, PCI_DEV_FLAG_IOMMU_ACTIVE, PCI_DEV_FLAG_MSI,
    RIGHT_CALL, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_SEND, RIGHT_WRITE,
};

const HW_TEST_BOOT_MAGIC: u64 = 0x4452_595f_4857_5431;
const CHILD_COMP_ID: u16 = 0x0238;
const BADGE_HW_FAULT: u64 = 0x00fa_8801;

// Virtual windows for supervisor (`0x2000_0000..0x21ff_ffff`) and child (`0x2200_0000..`).
const IV_WC_VA: u64 = 0x2000_0000;
const IV_UC_OFF_VA: u64 = 0x2010_0000;
const IV_UC_ALIAS_VA: u64 = 0x2020_0000;
const OOB_PROBE_VA: u64 = 0x2030_0000;

const EDU_BAR_VA: u64 = 0x2100_0000;
const EDU_DMA_VA: u64 = 0x2110_0000;
const CHILD_EDU_BAR_VA: u64 = 0x2200_0000;
const CHILD_EDU_DMA_VA: u64 = 0x2210_0000;

// QEMU `edu` MMIO register offsets (`hw/misc/edu.c`).
const EDU_REG_ID: u64 = 0x00;
const EDU_REG_LIVENESS: u64 = 0x04;
const EDU_REG_IRQ_STATUS: u64 = 0x24;
const EDU_REG_IRQ_RAISE: u64 = 0x60;
const EDU_REG_IRQ_ACK: u64 = 0x64;
const EDU_REG_DMA_SRC: u64 = 0x80;
const EDU_REG_DMA_DST: u64 = 0x88;
const EDU_REG_DMA_CNT: u64 = 0x90;
const EDU_REG_DMA_CMD: u64 = 0x98;

const EDU_INTERNAL_BUF: u64 = 0x4_0000;
const EDU_DMA_START: u64 = 0x01;
const EDU_DMA_TO_RAM: u64 = 0x02;

#[repr(C)]
#[derive(Clone, Copy)]
struct HwTestBootInfo {
    magic: u64,
    hw_mod_cap: u64,
    edu_pci_cap: u64,
    ivshmem_pci_cap: u64,
}

#[inline(always)]
fn read_cs_ss() -> (u16, u16) {
    let cs: u16;
    let ss: u16;
    // SAFETY: Reading CS and SS segment selectors is unprivileged.
    unsafe {
        asm!("mov {0:x}, cs", "mov {1:x}, ss", out(reg) cs, out(reg) ss, options(nomem, nostack));
    }
    (cs, ss)
}

#[inline(always)]
unsafe fn mmio_read32(addr: u64) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

#[inline(always)]
unsafe fn mmio_write32(addr: u64, val: u32) {
    unsafe { write_volatile(addr as *mut u32, val) }
}

#[inline(always)]
unsafe fn mmio_read64(addr: u64) -> u64 {
    unsafe { read_volatile(addr as *const u64) }
}

#[inline(always)]
unsafe fn mmio_write64(addr: u64, val: u64) {
    unsafe { write_volatile(addr as *mut u64, val) }
}

#[inline(always)]
fn sfence() {
    // SAFETY: `sfence` orders WC writes cleanly before subsequent UC verification reads.
    unsafe {
        asm!("sfence", options(nostack, preserves_flags));
    }
}

fn fail(code: i32) -> ! {
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-HW-FAIL] code=");
    lb.push_u64(code as u64);
    log_line(&lb);
    sys_exit(code)
}

fn edu_wait_dma_idle(bar_va: u64, timeout_tsc: u64) -> bool {
    let t0 = rdtsc_user();
    while rdtsc_user().saturating_sub(t0) < timeout_tsc {
        // SAFETY: `bar_va + EDU_REG_DMA_CMD` is mapped UC `edu` BAR0 MMIO.
        if (unsafe { mmio_read64(bar_va + EDU_REG_DMA_CMD) } & EDU_DMA_START) == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn child_fault_worker(edu_cap: u64) -> ! {
    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let timeout_tsc = 2_000_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    let mut info = UserPciDeviceInfo::default();
    if sys_dev_query_bars(edu_cap, &mut info).is_err() {
        sys_exit(11);
    }
    if sys_dev_map_window(edu_cap, CHILD_EDU_BAR_VA, 0, false, 4, 0).is_err() {
        sys_exit(12);
    }
    let (_dma_cap, dma_pa) = sys_dma_alloc(edu_cap, CHILD_EDU_DMA_VA).unwrap_or_else(|_| sys_exit(13));

    let ncap = sys_notify_create(0).unwrap_or_else(|_| sys_exit(14));
    let vec = sys_irq_bind(edu_cap, ncap, IRQF_ONESHOT).unwrap_or_else(|_| sys_exit(15));
    if !(0x40..=0x5f).contains(&vec) {
        sys_exit(16);
    }

    // Exercise one live MSI under IRQF_ONESHOT and one DMA transaction before crashing via `#UD`.
    // SAFETY: `CHILD_EDU_BAR_VA` & `CHILD_EDU_DMA_VA` are validly mapped for the child process.
    unsafe {
        mmio_write32(CHILD_EDU_BAR_VA + EDU_REG_IRQ_RAISE, 0x11);
    }
    if sys_notify_wait(ncap, 0, timeout_tsc).is_err() {
        sys_exit(17);
    }
    // SAFETY: Acknowledge child `edu` interrupt register before unmasking MSI.
    unsafe {
        let isr = mmio_read32(CHILD_EDU_BAR_VA + EDU_REG_IRQ_STATUS);
        mmio_write32(CHILD_EDU_BAR_VA + EDU_REG_IRQ_ACK, isr);
        write_volatile(CHILD_EDU_DMA_VA as *mut u64, 0xfeed_beef_2c20_c11d);
        mmio_write64(CHILD_EDU_BAR_VA + EDU_REG_DMA_SRC, dma_pa);
        mmio_write64(CHILD_EDU_BAR_VA + EDU_REG_DMA_DST, EDU_INTERNAL_BUF);
        mmio_write64(CHILD_EDU_BAR_VA + EDU_REG_DMA_CNT, 64);
        mmio_write64(CHILD_EDU_BAR_VA + EDU_REG_DMA_CMD, EDU_DMA_START);
    }
    if sys_irq_ack(edu_cap, IRQ_ACK_UNMASK).is_err() {
        sys_exit(18);
    }
    if !edu_wait_dma_idle(CHILD_EDU_BAR_VA, timeout_tsc) {
        sys_exit(19);
    }

    let mut clb = LineBuilder::new();
    clb.push_bytes(b"[DRY-HW-CHILD] Armed edu msi_vec=0x");
    clb.push_hex_u64(vec as u64);
    clb.push_bytes(b" hitting #UD for FaultEndpoint quiesce test");
    log_line(&clb);

    // SAFETY: Deliberate `#UD` fault while holding active PCI BME, VT-d SLPT DMA, and MSI binding.
    unsafe {
        asm!("ud2", options(nomem, nostack));
    }
    sys_exit(20)
}

fn test_ivshmem_bar64_wc_and_bounds(iv_cap: u64, edu_cap: u64) -> (u64, u64, u64) {
    let mut iv_info = UserPciDeviceInfo::default();
    if sys_dev_query_bars(iv_cap, &mut iv_info).is_err() {
        fail(101);
    }
    let bar2 = iv_info.bars[2];
    let req_flags =
        PCI_BAR_FLAG_MMIO | PCI_BAR_FLAG_64BIT | PCI_BAR_FLAG_PREFETCHABLE | PCI_BAR_FLAG_ABOVE_4G;
    if (bar2.flags & req_flags) != req_flags || bar2.size_bytes < 256 * 1024 * 1024 {
        fail(102);
    }

    let mut bounds_rej = 0u64;
    let oob_page_off = (bar2.size_bytes / 4096) as u32;
    if sys_dev_map_window(iv_cap, OOB_PROBE_VA, 2, true, 4, oob_page_off) == Err(EINVAL) {
        bounds_rej += 1;
    } else {
        fail(103);
    }
    if sys_dev_map_window(iv_cap, OOB_PROBE_VA, 2, true, 0, 0) == Err(EINVAL) {
        bounds_rej += 1;
    } else {
        fail(104);
    }
    if sys_dev_map_window(edu_cap, OOB_PROBE_VA, 0, true, 1, 0) == Err(EINVAL) {
        bounds_rej += 1;
    } else {
        fail(105);
    }

    if sys_dev_map_window(iv_cap, IV_WC_VA, 2, true, 16, 0).is_err() {
        fail(106);
    }
    if sys_dev_map_window(iv_cap, IV_UC_OFF_VA, 2, false, 4, 16).is_err() {
        fail(107);
    }
    if sys_dev_map_window(iv_cap, IV_UC_ALIAS_VA, 2, false, 4, 0).is_err() {
        fail(108);
    }

    // Write via `BAR2` WC mapping, fence, read back via `UC` alias of offset 0.
    for i in 0..16u64 {
        let pat = 0x2c20_ba64_0000_0000 ^ (i.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // SAFETY: `IV_WC_VA + i*8` is mapped `USER_MMIO_WC` to `ivshmem` BAR2 page 0.
        unsafe {
            write_volatile((IV_WC_VA + i * 8) as *mut u64, pat);
        }
    }
    sfence();
    for i in 0..16u64 {
        let pat = 0x2c20_ba64_0000_0000 ^ (i.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // SAFETY: `IV_UC_ALIAS_VA` aliases `ivshmem` BAR2 page 0 via `USER_MMIO_UC`.
        let got = unsafe { read_volatile((IV_UC_ALIAS_VA + i * 8) as *const u64) };
        if got != pat {
            fail(109);
        }
    }
    // Verify offset window (`page_offset = 16`) doesn't clobber offset 0.
    // SAFETY: `IV_UC_OFF_VA` and `IV_UC_ALIAS_VA` map distinct BAR2 regions.
    unsafe {
        write_volatile(IV_UC_OFF_VA as *mut u64, 0xfeed_face_cafe_0010);
        if read_volatile(IV_UC_OFF_VA as *const u64) != 0xfeed_face_cafe_0010 {
            fail(110);
        }
        if read_volatile(IV_UC_ALIAS_VA as *const u64) != 0x2c20_ba64_0000_0000 {
            fail(111);
        }
    }

    // Measure 64 KiB WC vs UC write cycles.
    let tw0 = rdtsc_user();
    for idx in 0..8192u64 {
        // SAFETY: `IV_WC_VA` covers 16 4 KiB pages = 64 KiB.
        unsafe {
            write_volatile((IV_WC_VA + idx * 8) as *mut u64, idx ^ 0x55aa_55aa_55aa_55aa);
        }
    }
    sfence();
    let wc_64k_cyc = rdtsc_user().saturating_sub(tw0);

    let tu0 = rdtsc_user();
    for pass in 0..4u64 {
        for idx in 0..2048u64 {
            // SAFETY: `IV_UC_OFF_VA` covers 4 4 KiB pages = 16 KiB (`4 * 16 KiB = 64 KiB`).
            unsafe {
                write_volatile((IV_UC_OFF_VA + idx * 8) as *mut u64, pass.wrapping_add(idx));
            }
        }
    }
    sfence();
    let uc_64k_cyc = rdtsc_user().saturating_sub(tu0);

    (bounds_rej, wc_64k_cyc, uc_64k_cyc)
}

fn test_supervised_child_crash_and_quiesce(hw_mod_cap: u64, edu_cap: u64, timeout_tsc: u64) -> u64 {
    let fault_ep = sys_ep_create(0).unwrap_or_else(|_| fail(201));
    let fault_send =
        sys_cap_derive(fault_ep, RIGHT_SEND | RIGHT_CALL, 0, BADGE_HW_FAULT).unwrap_or_else(|_| fail(202));
    let edu_child = sys_cap_derive(
        edu_cap,
        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT,
        0,
        0,
    )
    .unwrap_or_else(|_| fail(203));

    let (child_h, _pid) = sys_proc_spawn_with_fault_ep(
        hw_mod_cap,
        1, // `SecDomain::RootSystem` isolated compartment (`CHILD_COMP_ID`) for PCI child
        CHILD_COMP_ID,
        (edu_child, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP, 0),
        fault_send,
    )
    .unwrap_or_else(|e| fail(204 - (e as i32) * 1000));

    let mut fext = UserIpcRecvExt::default();
    let (ftag, ftok, fmrs) =
        sys_ipc_recv_ext(fault_ep, 0, timeout_tsc, &mut fext).unwrap_or_else(|_| fail(205));
    if msg_tag_label(ftag) != FAULT_MSG_LABEL || fext.sender_badge != BADGE_HW_FAULT {
        fail(206);
    }
    let fault_vec = fmrs[0] & 0xff;
    if fault_vec != 6 {
        fail(207);
    }
    if ftok != 0 {
        let _ = sys_ipc_reply_inline(
            fault_ep,
            ftok,
            pack_msg_tag(FAULT_REPLY_KILL, 0, 1, 0),
            [0; 4],
        );
    }
    if sys_proc_wait(child_h, 0, timeout_tsc).is_err() {
        fail(208);
    }
    // Revoke any child-derived caps cleanly while retaining `edu_cap`.
    let _ = sys_cap_revoke(edu_cap, 0);
    let _ = sys_cap_revoke(fault_ep, 1);
    fault_vec
}

fn median_32(vals: &mut [u64; 32]) -> u64 {
    for i in 1..32 {
        let key = vals[i];
        let mut j = i;
        while j > 0 && vals[j - 1] > key {
            vals[j] = vals[j - 1];
            j -= 1;
        }
        vals[j] = key;
    }
    vals[16]
}

fn test_edu_msi_bound_notif_and_vtd_dma(edu_cap: u64, timeout_tsc: u64) -> (u8, u64, u64, u64, u64) {
    let mut edu_info = UserPciDeviceInfo::default();
    if sys_dev_query_bars(edu_cap, &mut edu_info).is_err() {
        fail(301);
    }
    let req_edu_flags = PCI_DEV_FLAG_MSI | PCI_DEV_FLAG_IOMMU_ACTIVE;
    if (edu_info.dev_flags & req_edu_flags) != req_edu_flags {
        fail(302);
    }
    if sys_dev_map_window(edu_cap, EDU_BAR_VA, 0, false, 4, 0).is_err() {
        fail(303);
    }
    // Verify `edu` ID & MMIO liveness register (`0x00` and `~` inversion on `0x04`).
    // SAFETY: `EDU_BAR_VA` maps 4 pages of `edu` BAR0 MMIO as `USER_MMIO_UC`.
    unsafe {
        let id = mmio_read32(EDU_BAR_VA + EDU_REG_ID);
        if (id >> 24) != 0x01 || (id & 0xff) != 0xed {
            fail(304);
        }
        mmio_write32(EDU_BAR_VA + EDU_REG_LIVENESS, 0xdead_beef);
        if mmio_read32(EDU_BAR_VA + EDU_REG_LIVENESS) != !0xdead_beef {
            fail(305);
        }
    }

    let ep = sys_ep_create(0).unwrap_or_else(|_| fail(306));
    let msi_notif = sys_notify_create(0).unwrap_or_else(|_| fail(307));
    let vec = sys_irq_bind(edu_cap, msi_notif, IRQF_ONESHOT).unwrap_or_else(|_| fail(308));
    if !(0x40..=0x5f).contains(&vec) {
        fail(309);
    }
    if sys_notify_bind_tcb(msi_notif).is_err() {
        fail(310);
    }
    // Direct wait on a `TCB`-bound notification must return `EBUSY`.
    if sys_notify_wait(msi_notif, MSG_FLAG_NON_BLOCKING as u64, 0) != Err(EBUSY) {
        fail(311);
    }

    // Verify zero-poll software signal delivery via `sys_ipc_recv_ext` first.
    if sys_notify_signal(msi_notif, 0x80).is_err() {
        fail(312);
    }
    let mut ext = UserIpcRecvExt::default();
    let (stag, _, _) = sys_ipc_recv_ext(ep, 0, timeout_tsc, &mut ext).unwrap_or_else(|_| fail(313));
    if msg_tag_label(stag) != IPC_OP_BOUND_NOTIF || (ext.sender_badge & 0x80) == 0 {
        fail(314);
    }

    // Run 32 live `QEMU edu` PCI MSI round-trips through unified `sys_ipc_recv_ext` + `IRQF_ONESHOT`.
    let mut lat_cycs = [0u64; 32];
    for (i, lat_slot) in lat_cycs.iter_mut().enumerate() {
        let t0 = rdtsc_user();
        // SAFETY: Raising an MSI interrupt via `edu` `0x60` triggers IDT `vec` (`0x40..=0x5f`).
        unsafe {
            mmio_write32(EDU_BAR_VA + EDU_REG_IRQ_RAISE, 0x100 + (i as u32));
        }
        ext = UserIpcRecvExt::default();
        let (tag, _, _) =
            sys_ipc_recv_ext(ep, 0, timeout_tsc, &mut ext).unwrap_or_else(|_| fail(315));
        if msg_tag_label(tag) != IPC_OP_BOUND_NOTIF || (ext.sender_badge & 1) == 0 {
            fail(316);
        }
        *lat_slot = rdtsc_user().saturating_sub(t0);
        // SAFETY: Clear `edu` ISR (`0x24 -> 0x64`) before unmasking MSI under `IRQF_ONESHOT`.
        unsafe {
            let isr = mmio_read32(EDU_BAR_VA + EDU_REG_IRQ_STATUS);
            if isr == 0 {
                fail(317);
            }
            mmio_write32(EDU_BAR_VA + EDU_REG_IRQ_ACK, isr);
        }
        if sys_irq_ack(edu_cap, IRQ_ACK_UNMASK).is_err() {
            fail(318);
        }
    }
    let msi_p50_cyc = median_32(&mut lat_cycs);

    if sys_notify_unbind_tcb(msi_notif).is_err() {
        fail(319);
    }
    if sys_irq_ack(edu_cap, IRQ_ACK_UNBIND).is_err() {
        fail(320);
    }

    // VT-d SLPT DMA test: allowed round-trip RAM <-> `edu` SRAM (`0x40000`).
    let (_dma_cap, dma_pa) = sys_dma_alloc(edu_cap, EDU_DMA_VA).unwrap_or_else(|_| fail(321));
    if sys_dev_query_bars(edu_cap, &mut edu_info).is_err() {
        fail(322);
    }
    let base_faults = edu_info.iommu_faults as u64;

    for i in 0..32u64 {
        let pat = 0x4452_595f_5654_4400 ^ (i.wrapping_mul(0x1000_1000_0001_0003));
        // SAFETY: `EDU_DMA_VA` is a pinned `DEVICE_DMA` 4 KiB page mapped into `edu`'s VT-d SLPT.
        unsafe {
            write_volatile((EDU_DMA_VA + i * 8) as *mut u64, pat);
        }
    }
    let td0 = rdtsc_user();
    // SAFETY: Program valid DMA RAM -> EDU internal 4 KiB buffer (`0x40000`), then clear & copy back.
    unsafe {
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_SRC, dma_pa);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_DST, EDU_INTERNAL_BUF);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_CNT, 256);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_CMD, EDU_DMA_START);
    }
    if !edu_wait_dma_idle(EDU_BAR_VA, timeout_tsc) {
        fail(323);
    }
    for i in 0..32u64 {
        // SAFETY: Zero DMA bounce page to verify genuine DMA writeback.
        unsafe {
            write_volatile((EDU_DMA_VA + i * 8) as *mut u64, 0);
        }
    }
    // SAFETY: DMA EDU internal buffer -> host DMA physical address.
    unsafe {
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_SRC, EDU_INTERNAL_BUF);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_DST, dma_pa);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_CNT, 256);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_CMD, EDU_DMA_START | EDU_DMA_TO_RAM);
    }
    if !edu_wait_dma_idle(EDU_BAR_VA, timeout_tsc) {
        fail(324);
    }
    let dma_rt_cyc = rdtsc_user().saturating_sub(td0);
    for i in 0..32u64 {
        let pat = 0x4452_595f_5654_4400 ^ (i.wrapping_mul(0x1000_1000_0001_0003));
        // SAFETY: Read back DMA'd pattern.
        let got = unsafe { read_volatile((EDU_DMA_VA + i * 8) as *const u64) };
        if got != pat {
            fail(325);
        }
    }

    // Rogue DMA fault test: target unmapped RAM `rogue_pa` outside `edu`'s VT-d SLPT domain.
    let rogue_pa = if dma_pa >= 0x0600_0000 {
        dma_pa - 0x0020_0000
    } else {
        dma_pa + 0x0020_0000
    };
    // SAFETY: Deliberate unmapped DMA write attempt to confirm Intel `VT-d` blocks rogue DMA & logs `FRCD`.
    unsafe {
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_SRC, EDU_INTERNAL_BUF);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_DST, rogue_pa);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_CNT, 64);
        mmio_write64(EDU_BAR_VA + EDU_REG_DMA_CMD, EDU_DMA_START | EDU_DMA_TO_RAM);
    }
    let _ = edu_wait_dma_idle(EDU_BAR_VA, timeout_tsc);

    let pause_notif = sys_notify_create(0).unwrap_or_else(|_| fail(326));
    let mut observed_faults = 0u64;
    for _ in 0..20 {
        if sys_dev_query_bars(edu_cap, &mut edu_info).is_err() {
            fail(327);
        }
        let cur = edu_info.iommu_faults as u64;
        if cur > base_faults {
            observed_faults = cur.saturating_sub(base_faults);
            break;
        }
        let _ = sys_notify_wait(pause_notif, 0, timeout_tsc / 200);
    }
    if observed_faults == 0 {
        fail(328);
    }
    // Verify allowed DMA page remained intact after rogue DMA rejection.
    // SAFETY: Reading first word of valid DMA buffer.
    if unsafe { read_volatile(EDU_DMA_VA as *const u64) } != 0x4452_595f_5654_4400 {
        fail(329);
    }

    let _ = sys_cap_revoke(ep, 1);
    let _ = sys_cap_revoke(msi_notif, 1);
    let _ = sys_cap_revoke(pause_notif, 1);
    (vec, 32, msi_p50_cyc, dma_rt_cyc, observed_faults)
}

#[no_mangle]
pub extern "C" fn _start(arg0: u64, arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || arg0 == 0 {
        sys_exit(100);
    }
    if arg1 != HW_TEST_BOOT_MAGIC {
        child_fault_worker(arg0);
    }

    // SAFETY: `arg0` (`0x0070_0000`) is mapped `USER_RO` with `HwTestBootInfo` by `pci_hw_test.rs`.
    let boot = unsafe { read_volatile(arg0 as *const HwTestBootInfo) };
    if boot.magic != HW_TEST_BOOT_MAGIC {
        fail(100);
    }

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let tsc_mhz = sys.tsc_mhz.max(1000);
    let timeout_tsc = 2_000_000u64.saturating_mul(tsc_mhz);

    let (bounds_rej, wc_64k_cyc, uc_64k_cyc) =
        test_ivshmem_bar64_wc_and_bounds(boot.ivshmem_pci_cap, boot.edu_pci_cap);
    let crash_vec = test_supervised_child_crash_and_quiesce(
        boot.hw_mod_cap,
        boot.edu_pci_cap,
        timeout_tsc,
    );
    let (msi_vec, msi_irqs, msi_p50_cyc, dma_rt_cyc, vtd_rogue_faults) =
        test_edu_msi_bound_notif_and_vtd_dma(boot.edu_pci_cap, timeout_tsc);

    let mut plb = LineBuilder::new();
    plb.push_bytes(b"[DRY-HW-PASS] bar64_above4g=1 wc_ok=1 bounds_rej=");
    plb.push_u64(bounds_rej);
    plb.push_bytes(b" msi_vec=0x");
    plb.push_hex_u64(msi_vec as u64);
    plb.push_bytes(b" msi_irqs=");
    plb.push_u64(msi_irqs);
    plb.push_bytes(b" bound_notif_ok=1 vtd_dma_ok=1 vtd_rogue_faults=");
    plb.push_u64(vtd_rogue_faults);
    plb.push_bytes(b" crash_vec=");
    plb.push_u64(crash_vec);
    plb.push_bytes(b" recovery_ok=1");
    log_line(&plb);

    let mut blb = LineBuilder::new();
    blb.push_bytes(b"[DRY-HW-BENCH] tsc_mhz=");
    blb.push_u64(tsc_mhz);
    blb.push_bytes(b" wc_64k_cyc=");
    blb.push_u64(wc_64k_cyc);
    blb.push_bytes(b" uc_64k_cyc=");
    blb.push_u64(uc_64k_cyc);
    blb.push_bytes(b" msi_bound_p50_cyc=");
    blb.push_u64(msi_p50_cyc);
    blb.push_bytes(b" dma_rt_cyc=");
    blb.push_u64(dma_rt_cyc);
    log_line(&blb);

    sys_exit(0)
}
