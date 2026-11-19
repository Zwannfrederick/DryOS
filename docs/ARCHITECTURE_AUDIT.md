# DRY OS — Ascension 8.0 Architecture & Code Quality Audit

**Branch:** `ascension-8.0`  
**Scope:** Kernel core (`kernel/src/`), Ring-3 userspace services & shared libraries (`userspace/src/`), isolated driver islands (`drivers/`), and Python build/QEMU validation tooling (`tools/`).

---

## 1. Structural & Invariant Summary

DRY OS follows a capability-based microkernel architecture (`dry-kernel` in Ring 0) paired with capability-isolated Ring-3 services (`SecDomain::RootSystem` for hardware/infrastructure servers and `SecDomain::Userspace` for driver islands, UI compositor, shell, and apps).

### Key Verified Resource & ABI Ceilings

| Invariant / Budget | Limit | Observed Status | Source / Evidence |
| :--- | :--- | :--- | :--- |
| Kernel syscall dispatch stack frame (`rust_syscall_dispatch`) | `<= 256 B` | `248 B` (`sub $0xf8, %rsp`) | `objdump -d kernel/target/x86_64-unknown-none/release/dry-kernel` |
| Ring-3 process stack | `16 KiB` + lower guard page | Preserved (`4 * 4 KiB`) | `kernel/src/proc/elf.rs` |
| Boot module latch slots (`MAX_LATCHED_MODULES`) | `32` | `27` occupied (5 free before M3/M4) | `tools/qemu_boot_test.py` (`USERSPACE_MODS`) |
| `RootInitBootExtInfo` ABI struct | `<= 256 B` | `176 B` (compile-time asserted) | `kernel/src/irq/session.rs#L235-L236`, `userspace/src/abi_ext.rs#L788-L789` |
| Per-file source size ceiling | `< 1,000` LOC | Max `963` LOC (`kernel/src/ipc/tests.rs`) | Recursive LOC scan across `kernel/src`, `userspace/src`, `tools` |
| Kernel GPL / proprietary driver isolation | Zero GPL/vendor driver code in kernel | Clean (`dry-lx-*` isolated in `drivers/`) | `drivers/`, `kernel/src/` |

---

## 2. Largest Source Files Audit (Top 10 Measured)

All source and tooling files remain under the 1,000-line hard ceiling. Several files approaching the 900+ LOC band are tracked for modular extraction whenever new features are added:

1. `kernel/src/ipc/tests.rs` — `963` LOC (in-kernel IPC regression self-tests)
2. `userspace/src/wm.rs` — `962` LOC (compositor surface/window management)
3. `userspace/src/bin/dry_shell.rs` — `953` LOC (interactive terminal; hardware diagnostic commands extracted to `userspace/src/shell_diag_cmds.rs`)
4. `userspace/src/lib.rs` — `935` LOC (core syscall wrappers & IPC constants; extended ABI helpers extracted to `userspace/src/abi_ext.rs`)
5. `kernel/src/sched/mod.rs` — `935` LOC (SMP scheduler & PI/donation core)
6. `kernel/src/ipc/endpoint.rs` — `919` LOC (synchronous capability-grant IPC & scheduling donation)
7. `userspace/src/bin/dry_display.rs` — `913` LOC (display compositor loop)
8. `tools/qemu_boot_test.py` — `912` LOC (test runner; validators extracted into `tools/qemu_p*_validators.py`)
9. `userspace/src/init_legacy.rs` — `909` LOC (Ring-3 init bootstrap & legacy helper paths)
10. `kernel/src/dma.rs` — `909` LOC (pinned DMA allocation, scatter-gather IOVA mapping, cache maintenance)

**Architectural rule applied going forward:** No new command handlers, GPU subsystems, or validator matrices are added inline to `dry_shell.rs`, `lib.rs`, `supervisor.rs`, or `qemu_boot_test.py`; new functionality is placed in dedicated focused submodules (`shell_gpu_cmds.rs`, `nvidia_island/*`, `sup_nv_island.rs`, `qemu_p18_*_validators.py`).

---

## 3. Architectural Findings Identified & Remediated (M2)

### ARCH-01: IPC Scheduling Donation Order vs. Capability Transfer Failure (`kernel/src/ipc/endpoint.rs`)
- **Finding:** In `ep_send_or_call`, `donate_on_call(cur_slot, rcv_slot)` previously executed *after* `cspace_transfer`, preventing cycle detection before capability slot mutation on faulted transfers and missing explicit donation rollback on transfer failure.
- **Fix (`kernel/src/ipc/endpoint.rs#L163-L185`):** Moved `donate_on_call(cur_slot, rcv_slot)` before `cspace_transfer`. When `cspace_transfer` fails on a `Call`, `revert_donation_for_caller(cur_slot)` restores donation state cleanly alongside waiter list restoration.

### ARCH-02: Redundant PCI `sys_dev_map` Error Quiesce on Already-Owned Device (`kernel/src/pci.rs`)
- **Finding:** In `sys_dev_map`, a user-pointer fault (`copy_to_user` failure) unconditionally invoked `quiesce_slot_for_cspace(dev_idx, cspace_id)`, even when `claimed_new == false` (the calling CSpace already owned the active slot from an earlier valid mapping).
- **Fix (`kernel/src/pci.rs#L641-L735`):** Recorded `claimed_new` in `mapped_info` and gated `quiesce_slot_for_cspace(dev_idx, cspace_id)` on `if claimed_new` so failed duplicate mappings unmap their temporary BAR pages without resetting an already-running device slot.

### ARCH-03: Unlocked SG Frame Handle Cache Maintenance (`kernel/src/dma.rs`)
- **Finding:** `dma_op_sync` previously copied `sg.frames` out of `with_dma_slot_lock` and ran `clflush` + `mfence` outside the slot lock, creating a theoretical race window if another thread freed the scatter-gather buffer concurrently.
- **Fix (`kernel/src/dma.rs#L801-L835`):** Sampled `is_device_coherent(dev_idx)` before acquiring the slot lock (preserving lock ordering), then performed the `clflush` loop, `fence(Ordering::SeqCst)`, and `mfence` strictly under `with_dma_slot_lock`.

### ARCH-04: Non-Blocking Fault Endpoint Drainage in `Device Broker` (`userspace/src/bin/dry_broker.rs`)
- **Finding:** `IslandInstance::check_async_crash_and_recover` polled `sys_proc_wait(..., 1, 0)` without first draining `cfg.fault_ep` non-blocking, missing explicit `FAULT_REPLY_KILL` handling and leaving any fault-attached capability unrevoked when an asynchronous crash occurred outside `BROKER_OP_CRASH_AND_RECOVER`.
- **Fix (`userspace/src/bin/dry_broker.rs#L321-L337`):** Added non-blocking `sys_ipc_recv_ext(cfg.fault_ep, MSG_FLAG_NON_BLOCKING as u64, 0, &mut fext)` check, calling `revoke_unused_cap(fext.transferred_cap)`, replying `FAULT_REPLY_KILL`, and transitioning `self.state = ISLAND_STATE_FAULTED`.

### ARCH-05: Ring-3 16 KiB Stack Discipline in Supervisor Services (`userspace/src/supervisor.rs`, `hwdiag/mgr.rs`)
- **Observation:** Because Ring-3 ELF processes map 4 stack pages (`16 KiB` + guard page), M1/M2 keep large staging/diagnostic buffers (`DIAG_MGR`, `DIAG_SCRATCH`, `LOG_STAGE_BUF`) statically allocated in `.bss` and annotate multi-step orchestration routines (`init_at_boot`, `export_to_usb`, `run_asc8_m2_security_probe`) with `#[inline(never)]`.

---

## 4. Build & Regression Verification Evidence (Post-M2 Fixes)

All checks executed on `ascension-8.0` after applying the M2 fixes:

- `cargo clippy (-D warnings)`: **0 errors, 0 warnings** across `dry-userspace`, `dry-lx-rng`, `dry-lx-drm-bochs`, `dry-lx-net`, and `dry-kernel`.
- `python3 tools/qemu_boot_test.py --p18-only`: **12/12 PASS** (`p18a` `11.86s`, `p18b` `12.97s`, `9/9 p18c` `58.35s`, `p18d` `6.41s`).
- `python3 tools/qemu_boot_test.py --p17-only`: **4/4 PASS** (`p17a` `5.27s`, `p17b` `3.99s`, `p17c` `5.22s`, `p17d` `27.95s`).
- `python3 tools/qemu_boot_test.py --p13-only`: **25/25 PASS** (`19.68s`, `nvme_sha=596a7c63e906`).
- `python3 tools/qemu_boot_test.py --p8-only`: **1/1 PASS** (`4.28s`, `vtd_faults=1`, `quiesce=5`, `leaked_frames=0`).
