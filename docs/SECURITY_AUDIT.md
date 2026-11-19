# DRY OS — Ascension 8.0 Security Audit

**Branch:** `ascension-8.0`  
**Audit Method:** Code-level inspection of kernel syscall/IOMMU/DMA/IPC paths and Ring-3 service dispatchers, followed by live QEMU exploit/fault probing (`p18d_security_hardening`, `p18c`, `p17`, `p13`, `p8`).

---

## 1. Threat Model & Trust Boundaries

1. **Ring 0 Kernel (`dry-kernel`):** Enforces capability rights (`READ`, `WRITE`, `SEND`, `CALL`, `RECV`, `REPLY`, `GRANT`, `MAP`, `SPAWN`, `REVOKE`), lease epochs, IOMMU DMAR page tables, and `SecDomain` mandatory access control (`RootSystem` vs. `Userspace`).
2. **Root Hardware Servers (`dry-usb`, `dry-blk`, `dry-nvme`, `dry-gpu`, `dry-broker`):** Hold delegated PCI/DMA/MMIO capabilities inside `SecDomain::RootSystem` (except unprivileged driver islands in `SecDomain::Userspace`) and expose IPC endpoints to authorized badges (`BADGE_SUPERVISOR`, `BADGE_BLK_VFS`, `BADGE_GPU_DISPLAY`, `BADGE_BROKER_CLIENT`, etc.).
3. **Unprivileged Endpoints & Drivers (`dry-shell`, `dry-mock-drv`, `dry-sdk-demo`, future `dry-nv-island`):** Confined to `SecDomain::Userspace`; cannot invoke `SYS_PROC_KILL`, mint arbitrary PCI/IOPORT caps, or issue destructive hardware writes.

---

## 2. Security Vulnerabilities Found & Remediated (M2)

| ID | Severity | Location | Vulnerability / Risk | Remediation | Verification |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **SEC-01** | **High** | `userspace/src/bin/dry_usb.rs#L661-L670` | `IPC_OP_CRASH_UD2` was unauthenticated — any client holding `usb_ep` could intentionally trigger `#UD` DoS against the USB xHCI/HID/MSC stack. | Restricted `IPC_OP_CRASH_UD2` strictly to `from_sup` (`ext.sender_badge == BADGE_SUPERVISOR`); non-supervisor callers receive `IPC_OP_ERR_PERM` (`EPERM`). | `p18d_security_hardening` (`usb_unauth_ud2_eperm=1 usb_alive=1`) |
| **SEC-02** | **High** | `kernel/src/dma.rs#L371-L376`, `L622-L638` | `sys_dma_alloc` and `dma_op_alloc_sg` mapped allocated physical frames into device IOMMU domains without rejecting frames overlapping ACPI DMAR RMRR reserved ranges. | Added `crate::iommu::overlaps_rmrr(...)` check prior to `map_dma_range` / `map_dma_range_iova`; rejects overlapping allocations with `SysError::Perm`, unpins frames, rolls back prior SG mappings, and releases newly claimed PCI slots. | `cargo clippy` + `--p8-only` + `--p18-only` (`leaked_frames=0`) |
| **SEC-03** | **Medium** | `userspace/src/bin/{dry_usb,dry_blk,dry_nvme,dry_gpu,dry_broker}.rs`, `userspace/src/abi_ext.rs#L823-L829` | Capabilities granted alongside unexpected or rejected IPC opcodes remained in the server's `CSpace`, allowing an unprivileged caller to bloat/exhaust server capability slots (`cap_flood`) or pin frames longer than intended. | Added shared helper `revoke_unused_cap(ext.transferred_cap)` across all 5 hardware/broker services for any opcode not expecting a cap transfer (and inside `dry_broker` fault polling). | `p18d_security_hardening`: 16 granted `sys_frame_alloc(0)` caps to `dry-usb` reclaimed cleanly (`cap_flood_reclaimed=1`, `leaked_frames=0`) |
| **SEC-04** | **Medium** | `userspace/src/bin/dry_gpu.rs#L336-L346` | `GPU_OP_PRESENT_RECT` computed `w * h * 4` with unchecked `usize` multiplication and accepted zero-byte regions before computing scanout page counts. | Replaced with `w.checked_mul(h).and_then(|v| v.checked_mul(4))` plus `byte_len == 0` guard, returning `IPC_OP_ERR_INVAL` (`EINVAL`). | `--p17-only` (`p17a`, `p17d` Virgl 3D) |
| **SEC-05** | **Medium** | `kernel/src/dma.rs#L801-L835` | `clflush` + `mfence` executed after dropping the DMA slot lock in `dma_op_sync`, risking use-after-free/unpin if the SG buffer were freed concurrently. | Moved cacheline flush loop and serialization barriers inside `with_dma_slot_lock` while preserving lock ordering by sampling `is_device_coherent(dev_idx)` beforehand. | `--p8-only` (`dma_rt`, `vtd_faults=1`, `leaked_frames=0`) |
| **SEC-06** | **Low/Medium** | `kernel/src/pci.rs#L730-L735` | `sys_dev_map` called `quiesce_slot_for_cspace` on `copy_to_user` fault even when the CSpace already held ownership prior to the failing call. | Tracked `claimed_new: bool` and only quiesced on rollback when `claimed_new` is true. | `--p8-only`, `--p18-only`, `--p13-only` |
| **SEC-07** | **Medium** | `kernel/src/ipc/endpoint.rs#L163-L185` | `donate_on_call` ran after `cspace_transfer`, leaving donation rollback missing if capability transfer failed and allowing cap slot mutation before cycle detection. | Executed `donate_on_call` before `cspace_transfer` and added `revert_donation_for_caller` on transfer failure. | `--p17-only`, `--p18-only` |

---

## 3. USB Persistence & Physical Media Safety Verification

- **5-Gate USB MSC Ancestry (`userspace/src/usb/`):** Persistence writes to `DRYOS_LOGS` (Partition 3) require xHCI host ancestry, GPT header/entry CRC32 validation, exact boot-disk GUID match (`usb_disk_guid=...`), same-disk ESP verification, and `DRYOS_LOGS` partition attributes.
- **No-Fallback Policy:** Missing Partition 3 (`p18c/neg1_missing_p3`), wrong disk GUID (`p18c/neg3_wrong_guid`), corrupt FAT32 BPB (`p18c/neg4_corrupt_bpb`), full disk `-ENOSPC` (`p18c/neg5_full_fat32`, `reason=-28`), injected USB write failure (`p18c/neg2_write_fail`), disconnect (`p18c/neg6_usb_disconn`), and no-IOMMU xHCI fail-closed mode (`p18c/neg9_no_iommu`) were all verified with **SHA-256 unchanged canary NVMe/VirtIO-Blk disks**.
- **Commit Ordering:** Diagnostic persistence writes `PERSIST_COMMITTING` first, commits all 12 session artifacts (`[1..11, 0]`), commits `manifest.sha256`, and only after `SYNCHRONIZE CACHE(10)` + FAT32 read-back verification marks `PERSIST_VERIFIED` (`p18a`, `p18b`).

---

## 4. Residual Risks & Deferred Items

1. **Bare-Metal Execution:** Real Lenovo IdeaPad Gaming 3 15IHU6 physical USB/ACPI/DMAR behavior has not yet been executed on bare metal (`hardware_verified=false`); physical operator validation is deferred to host-side USB boot after image completion.
2. **IOMMU Absent Mode:** Without Intel VT-d / AMD-Vi DMAR enforcement (`p18c/neg9_no_iommu`), xHCI/NVMe/DMA drivers fail closed (`blocked >= 1`) and legacy PS/2 `i8042` remains available for recovery shell inspection.
3. **NVIDIA GA107 Native Execution:** Remains fail-closed `BLOCKED` pending IOMMU interrupt remapping / VFIO / MicroVM / OpenRM / signed GSP prerequisites (tracked in Milestone 4).
