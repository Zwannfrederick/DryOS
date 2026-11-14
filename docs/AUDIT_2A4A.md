# DRY OS — Independent Baseline Audit (Milestone 2A-4a -> 2C-2 Closure, 14 Nov 2026)

Scope: `9f9c575` -> `Milestone 2C-2` (`main`). Method: (1) the full QEMU/KVM suite was re-run before any change
(**90/90 PASS**, 6 min 01 s, host i5-11300H, QEMU 11.1.3, KVM) and re-verified at **95/95 PASS** + **5/5 SMP/RAM matrix** after `Milestone 2C-2`; (2) three independent read-only
code audits (CSpace/IPC/IRQ, VSpace/PMM/process lifecycle, Ring 3 services); (3) every finding
selected for fixing was first reproduced by a failing test, then fixed, then re-run.

Status legend: **FIXED+TEST** (reproducer failed before, passes after) · **FIXED** (fixed by
argument, no deterministic reproducer) · **OPEN** (confirmed by code reading, not fixed here) ·
**SUSPECTED** (plausible, not confirmed). Nothing below marked OPEN/SUSPECTED was executed.

No finding gives a Ring 3 process read/write access to memory it does not own. The real defects
were denial of service, revocation bypass, and lost wakeups.

## Kernel findings

| ID | Sev | Finding | Status |
| :- | :-: | :--- | :--- |
| AUD-K1 | P1 | `cspace_delete_cap` (`SYS_CAP_REVOKE` op 1) accepted *root* capabilities. A process deleting its own root `TCB`/`VSpace` cap made `reclaim_process` fail after the `Reaping` CAS: process slot, TCB, VSpace, CSpace leaked forever and the parent's `SYS_PROC_WAIT` spun in the kernel. Any `Userspace` child could hang `dry-init`. Deleting root `Frame` caps leaked RAM past process death. | **FIXED+TEST** — root caps of object-owning kinds return `-EBUSY`; `reclaim_process` no longer bails out after `Reaping` (`cspace/revoke.rs`, `proc/mod.rs`). |
| AUD-K2 | P1 | `SYS_VSPACE_MAP` op `UNMAP_4K` had no `Frame4K` kind check and detached the mapping record before unmapping. With a `Frame2M`, `Framebuffer` or `PciDev` cap the record was removed while the mapping (all but page 0 for MMIO ranges) stayed: the mapping then survived revocation. | **FIXED+TEST** (`cspace/objects.rs`). |
| AUD-K3 | P1 | `cspace_destroy_object` through a *derived* capability destroyed the object while the root (and sibling) caps stayed `Live`. Consequences: (a) a thread already past lookup on the root cap could enqueue on a *recycled* Endpoint/Notification index (cross-object message/cap theft, badge injection); (b) concurrent frame map vs destroy, or sibling double destroy, reached `pmm_invariant_panic` (userspace-triggerable kernel panic). | **FIXED+TEST** — only the root capability reclaims an object; a non-root holder's destroy revokes its own subtree and drops its reference. Revocation already waits for in-flight users of all descendants, so the root-only rule closes the whole class. |
| AUD-K4 | P1 | `Notification` lost wakeup: the waiter swapped `pending_bits` and only then published `waiter_slot`; the signaller did `fetch_or` then read `waiter_slot` (Dekker order inverted). A driver waiting on a one-shot IRQ without timeout could hang forever. Reproduced 3/3 runs at 400 000 ping-pong rounds on 2 vCPUs. | **FIXED+TEST** — publish-then-recheck under `n.lock` (`ipc/notif.rs`). 400 000 rounds, 0 lost, 2 and 4 vCPUs. |
| AUD-K4b | P1 | A second waiter could register while the first was `WAIT_DELIVERED` but not yet resumed, zeroing its delivered bits. | **FIXED** — registration requires `WAIT_IDLE`. |
| AUD-K5 | P1 | `recv_locked` recorded deferred sender wakeups in a 4-entry array and silently dropped the rest: 5 senders failing at delivery (bogus grant) plus one delivered sender left two threads blocked forever. | **FIXED+TEST** — overflow wakes immediately (`ipc/endpoint.rs`). |
| AUD-K6 | P1 | Priority-donation leak: detaching a stale outgoing edge does not recompute the previous server's `effective_prio` (`sched/donate.rs:186-190`). | **FIXED+TEST** — stale outgoing edge teardown calls `recompute_chain_from` (`sched/donate.rs`); regression `case_donate_retarget_recomputes_prev_server` passes in `audit_tests.rs`. |
| AUD-K7 | P1 | NMI/#MC use IST 0; an NMI inside the `SYSCALL` entry/exit window runs on the user-chosen `RSP` (`idt.rs:474`). No NMI source exists in QEMU today. | **FIXED+TEST** — `NMI` (`IST2`) and `#MC` (`IST3`) bound to disjoint `8 KiB` per-CPU guard stacks (`gdt_tss.rs`, `idt.rs`); `case_nmi_mc_ist_stacks_disjoint` verified (`audit_tests.rs`) + `32 KiB` `boot_stack`. |
| AUD-K8 | P1 | `quiesce_pci_for_cspace` unpins DMA frames without a device reset; `SYS_DEV_MAP` can then re-enable Bus Master with an empty pin table (`pci.rs`). Requires a `RootSystem` `PciDev` holder, which without an IOMMU is kernel-equivalent anyway (`R-03`). | **FIXED+TEST** — `quiesce_slot_for_cspace` disables MSI/MSI-X, writes VirtIO `device_status=0` (with poll wait), clears `PCI Command.BME=0` + masks INTx, and calls `detach_and_scrub_domain` (`VT-d` SLPT/IOTLB scrub) before unpinning DMA frames (`pci.rs`); verified in `p8a_hw_foundations_vtd_msi_bar64_e2e` (`quiesce=3`, `vtd_faults=1`, `leaked_frames=0`). |
| AUD-K9 | P1 | `cspace_spawn_tcb` publishes a runnable thread before `insert_root_cap`; `OutOfSlots` (ELF of ~248 pages) leaves an orphan thread (`cspace/objects.rs:710-731`). | **FIXED** — `reserve_root_slot` / `abort_root_slot` / `commit_root_slot` reserve the `CNode` slot before spawning the `TCB` (`cspace/mod.rs`, `cspace/objects.rs`). |
| AUD-K10 | P2 | Slot stuck `Revoking` when object teardown fails after the state change (`cspace/revoke.rs:535-566`); child derived during destroy survives for kinds without teardown. | OPEN |
| AUD-K11 | P2 | IRQ storm clamp leaves a throttled line unusable until rebind, and rebind resets the storm window (`irq/mod.rs`). | OPEN |
| AUD-K12 | P2 | Global mapping-record pool is 512 entries, no per-CSpace quota (system-wide map/spawn DoS, `cspace/cap.rs:23`). | OPEN |
| AUD-K13 | P2 | Stale range record after `copy_to_user` failure in `sys_fb_map`/`sys_dev_map`; `owner_cspace` check-then-store race in `pci.rs`; ELF `seg_end + 0xfff` overflow (`proc/elf.rs:173`). | **PARTIAL** — `pci.rs` (`AUD-K13b`) `owner_cspace` claim converted to atomic `compare_exchange` with rollback (`pci.rs`); remaining items OPEN. |
| AUD-K14 | P2 | Lock-free page-table walk in `uaccess.rs` with `IF=1` may read a freed page-table frame; PMM global lock spins without polling shootdowns. | SUSPECTED |
| AUD-K15 | P1 | A process killed while blocked in an untimed IPC wait is woken but re-parks; it never terminates and its parent's `SYS_PROC_WAIT` blocks. | **FIXED+TEST** — pre/post-park `current_thread_killed` and first-iteration `try_resolve_ipc_wait` checks in `ipc/notif.rs` and `ipc/endpoint_state.rs` (`audit_tests.rs`). |

## Ring 3 findings

| ID | Sev | Finding | Status |
| :- | :-: | :--- | :--- |
| AUD-U1 | P1 | `IPC_OP_SUP_RECONNECT_INPUT` had no badge check and did an untimed `SYS_PROC_WAIT` on a live child: any app could hang or terminate (`sys_exit(150/151)`) the root supervisor (`supervisor.rs`). | **FIXED** — trusted desktop badges only, bounded reap, error reply instead of exit. Legacy P1/P2 sessions (`init_legacy.rs`) keep the old path. |
| AUD-U5 | P1 | `IoRingSharedPage::pop_sqe` accepted `tail - head > CAP` from the peer (up to 2^32 bogus entries). | **FIXED** (bounded; a hostile peer can still rewrite `head`, see below). |
| AUD-U6 | P1 | `dry-vfs` `IPC_OP_EXIT_CLEAN` was unauthenticated. | **FIXED** — `BADGE_SUPERVISOR` only. |
| AUD-U2 | P1 | DRYFS reformats the volume on *any* verification failure, including a transient read error or one bad data block (`persist_fs.rs:259-317`). | **FIXED** — refuses wipe/reformat (`EIO_RING`) when `DRYFS_MAGIC` is present on either superblock (`persist_fs.rs`). |
| AUD-U3 | P1 | `dry-vfs` waits forever on `cq_notif` if `dry-blk` dies with a request pending; CQE `user_tag` is not checked (`ioring.rs:276-284`). | **FIXED+TEST** — `IoRingClient::exec_sqe` enforces `cqe.user_tag == tag`, bounded `20 x 250 ms` `cq_notif` waits, and timeout health check (`BLK_OP_QUERY_INFO` returning `EIO_RING` on `EPIPE/EREVOKED/EBADCAP`, `ioring.rs`). |
| AUD-U4 | P1 | `dry-blk` reuses in-flight descriptors after a timeout without a device reset (`dry_blk.rs:351-376`). | **FIXED** — `dry_blk.rs` sets `self.poisoned = true` and runs `self.reset_device()` before returning `Err(EIO_RING)` on completion timeout. |
| AUD-U7 | P1 | Vault fails open: reseeds the default passphrase on any read failure, `UNLOCK` unthrottled, starts unlocked, `PASSWD` swaps before persisting, unbounded PBKDF2 iterations in the record (`dry_vault.rs`, `vault_proto.rs`). | **PARTIAL** — corrupt `/persist/vault.sec` fails closed (`unlocked=false`, no overwrite) and `pbkdf2_iters` bounded to `256..=65536` (`dry_vault.rs`, `vault_proto.rs`). |
| AUD-U8 | P1 | Broker crash recovery can stick in `FAULTED`; untimed calls to the island; leaks on error paths (`dry_broker.rs`). | **FIXED+TEST** — `dry_broker.rs` passes `ipc_timeout_tsc = 2_000_000 * tsc_mhz.max(1000)` to all island IPC/wait paths and reports `ISLAND_STATE_FAULTED` + error reply instead of exiting (`--island-only` + `--lx-island-only` PASS, `leaked_frames=0`). |
| AUD-U9 | P2 | `BROKER_OP_SET_PARK_MODE` is not admin-gated; every client gets the same SPSC ring. Consumers keep their index in the shared page. On-disk `size_bytes` unchecked. Notify offset unbounded in `dry-blk`. | **PARTIAL** — `BROKER_OP_SET_PARK_MODE` now enforces `is_admin` (`dry_broker.rs`). |

## Verified as sound (by reading)

Pin/revoke Dekker (`SeqCst` both sides), slot-generation ABA, CDT insert vs `revoke_children`,
capability narrowing rules, reply-token binding, endpoint park/wake arbitration, lock order
(endpoint -> {CDT, donate}; notif/IRQ/IOAPIC leaves), TLB shootdown ordering and frame-free after
ack, `W^X`, `map_mmio_4k` callers, user-copy bounds + SMAP fixup, `SYSRET` canonical/RFLAGS checks,
kill vs in-flight syscall barrier, PMM magazines, cross-compartment frame zeroing, IoRing memory
orderings, broker admin gates and revoke-before-respawn ordering, DRYFS single-crash ordering.

## Reproduction

`kernel/src/audit_tests.rs` runs in every positive SMP boot:
`[DRY-AUDIT-REG] PASS: cpus=N cases=9/9 notif_rounds=200000 leaked_frames=0`.

`p8a_hw_foundations_vtd_msi_bar64_e2e` (`tools/qemu_p8_hw_validators.py` / `dry-hw-test.elf`) verifies `BAR64 > 4 GiB` `WC`/`UC`, `VT-d` `SLPT` + `FRCD` fault containment, `PCI MSI` `vec=0x40` via `TCB`-bound `Notification`, and `#UD` quiesce/recovery (`95/95` QEMU + `5/5` matrix, `leaked_frames=0`).
