# DRY OS — Driver Island ABI v1 & SDK Guide (`DRIVER_ISLAND_SDK.md`)

**Package:** [`drivers/dry-driver-sdk/`](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/drivers/dry-driver-sdk/Cargo.toml)  
**Reference Implementation:** [`drivers/sdk-example-pci/`](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/drivers/sdk-example-pci/src/main.rs) (`dry-sdk-demo.elf`)  
**Protocol Source of Truth:** [`userspace/src/island_proto.rs`](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/userspace/src/island_proto.rs)

---

## 1. Driver Island ABI v1 Contract

All Driver Island ABI v1 components run in Ring 3 (`CPL=3`), typically confined to `SecDomain::Userspace` (`sys_proc_kill(1) == Err(EPERM)`), and communicate via capability-badged IPC (`dry-broker` control plane) plus optional shared `IoRing` pages for lockless data paths.

### Version Constant & Feature Bits

| Constant | Value | Meaning |
| :--- | :--- | :--- |
| `ISLAND_ABI_VERSION_V1` | `0x0001_0000` | Major `1`, minor `0` Driver Island ABI version |
| `ISLAND_FEAT_IORING` | `1 << 0` | Island supports `IoRingSharedPage` doorbell (`IPC_OP_BOUND_NOTIF`) |
| `ISLAND_FEAT_QUIESCE` | `1 << 1` | Island implements clean `ISLAND_OP_QUIESCE_UNLOAD` teardown |
| `ISLAND_FEAT_MMIO_BOUNDS` | `1 << 2` | Island enforces SDK `MmioWindow` / `DmaRegion` bounds checks |

### Lifecycle & Control-Plane Opcodes

| Endpoint | Opcode | Value | Caller Badge | Behavior / Wire Reply |
| :--- | :--- | :---: | :--- | :--- |
| Direct Island (`drv_ep`) | `ISLAND_OP_HELLO` | `157` | `BADGE_ISLAND_BROKER` / `BADGE_SUPERVISOR` | Validates `mrs[0] == ISLAND_ABI_VERSION_V1`. On match: `IPC_OP_PONG` `[ISLAND_ABI_VERSION_V1, generation, feature_flags, 0]`. On mismatch: `IPC_OP_ERR_INVAL` (`EINVAL`). |
| Direct Island (`drv_ep`) | `ISLAND_OP_QUIESCE_UNLOAD` | `158` | `BADGE_ISLAND_BROKER` / `BADGE_SUPERVISOR` | Advances lifecycle (`Running -> Quiescing -> Stopped`), unbinds `sq_notif`, revokes `sq_notif`/`cq_notif` and `ring_cap`, replies `IPC_OP_PONG`, exits cleanly (`0`). |
| `Device Broker` (`broker_ep`) | `BROKER_OP_ISLAND_HELLO` | `169` | `BADGE_BROKER_ADMIN` / `BADGE_BROKER_CLIENT` | Forwards `mrs[0]` ABI version to active island via `ISLAND_OP_HELLO`; propagates `IPC_OP_PONG` or `IPC_OP_ERR_INVAL`. |
| `Device Broker` (`broker_ep`) | `BROKER_OP_QUIESCE_UNLOAD` | `180` | `BADGE_BROKER_ADMIN` only | Sends `ISLAND_OP_QUIESCE_UNLOAD`, reaps `proc_handle` via `sys_proc_wait`, revokes generation caps, transitions broker state to `ISLAND_STATE_STOPPED`, updates `BrokerLifecycle` to `Unloaded`, replies `IPC_OP_PONG` `[gen, restarts, acked, lifecycle_step]`. |

Unclaimed IPC capability transfers (`ext.transferred_cap != 0` on non-grant opcodes) MUST be revoked immediately (`revoke_unused_cap`) so callers cannot accumulate orphan slots inside the island or broker `CSpace`.

---

## 2. `dry-driver-sdk` API Overview (`#![cfg_attr(not(test), no_std)]`)

Zero external crate dependencies; safe on both `x86_64-unknown-none` and Linux host unit-test targets:

- **[`abi.rs`](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/drivers/dry-driver-sdk/src/abi.rs):**
  - `validate_abi_hello(requested_ver, active_gen, features) -> Result<[u64; 4], IslandError>`
  - `IslandLifecycle`: enforced transitions `Stopped -> Running -> Quiescing -> Stopped` and `* -> Faulted -> Running`.
- **[`mmio.rs`](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/drivers/dry-driver-sdk/src/mmio.rs):**
  - `MmioWindow::new(base, len)` & `check_access(offset, width)` (rejects zero windows, non-`1/2/4/8` widths, unaligned offsets, integer overflow, and OOB accesses).
  - `DmaRegion::new(virt_base, phys_base, len)` & `check_slice(offset, slice_len, align)` (overflow-safe virtual+physical offset calculation and power-of-two alignment enforcement).
- **[`ring.rs`](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/drivers/dry-driver-sdk/src/ring.rs):**
  - `SDK_IORING_MAGIC` (`0x4452_4952`), `SdkSqe` (`32 B`), `SdkCqe` (`16 B`), and `check_ring_header(magic, sq_head, sq_tail, capacity)`.

---

## 3. Building & Testing

### Host Unit Tests (`cargo test`)
```bash
cargo test --manifest-path drivers/dry-driver-sdk/Cargo.toml
```
Verified tests (`5/5 PASS`):
- `abi::tests::abi_hello_accepts_v1_and_rejects_mismatch`
- `abi::tests::lifecycle_transitions_enforce_valid_states`
- `mmio::tests::mmio_window_bounds_and_alignment`
- `mmio::tests::dma_region_bounds_and_overflow`
- `ring::tests::wire_sizes_and_ring_validation`

### Reference Island Bare-Metal Build & Live QEMU E2E (`p18e_driver_sdk_lifecycle`)
`tools/qemu_boot_test.py` compiles `drivers/sdk-example-pci` (`cargo clippy -D warnings` + `--release` targeting `x86_64-unknown-none`) and latches `dry-sdk-demo.elf` as Limine boot module `#28`.

Booting with `desktop4=1 sdk_demo=1` (`session_mode=32`) executes `run_sdk_demo_lifecycle_audit`:
- Negotiates `ISLAND_ABI_VERSION_V1` (`0x0001_0000`) -> `PASS`
- Rejects mismatched ABI (`0x0002_0000`) with `IPC_OP_ERR_INVAL` -> `PASS`
- Executes direct IPC transform + `MOCK_OP_TRANSFORM` -> `PASS`
- Injects `#UD` fault (`BROKER_OP_CRASH_AND_RECOVER`) -> clean reap & respawn to `gen=2` -> `PASS`
- Executes graceful `BROKER_OP_QUIESCE_UNLOAD` + broker teardown -> `leaked_frames=0` (`[DRY-SDK-DEMO] PASS: abi_v1=1 mismatch_rej=1 op_ok=1 crash_rec_gen=2 quiesce_unload=1`).
