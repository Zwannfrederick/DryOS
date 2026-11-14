# Milestone 2C-1: Genuine Linux Driver Island (`v6.18` `virtio-rng.c`)

**Status:** VERIFIED (`--lx-island-only`, `--island-only`, `--smp-only 2`)  
**Architectural Decision:** `ADR-005`, `ADR-016`  
**Syscall ABI:** Frozen at `0..=31` (`0` Linux ABI, Linux headers, or GPL-only code in `kernel/` or `userspace/`)

---

## 1. Upstream Driver Provenance

| Attribute | Value |
|---|---|
| Repository | Torvalds `linux.git` (`https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git`) |
| Tag / Commit | `v6.18` (`7d0a66e4bb9081d75c82ec4957c50034cb0ea449`) |
| Upstream Source | `drivers/char/hw_random/virtio-rng.c` (`259` lines) |
| Local Unmodified Copy | `vendor/linux-drivers/v6.18/virtio_rng/virtio-rng.c` |
| SHA-256 Digest | `720433a85ce0ce6f84e85842b54d23c7c8e9bad792c66b1066accbaa7fb95a75` (`0` patches, `0` edits) |
| Integrity Enforcer | `tools/h2-audit/fetch_virtio_rng_sources.py --verify` (`build_lx_rng_island()` gate) |
| PCI Device Target | QEMU `virtio-rng-pci-non-transitional` (`1AF4:1044`) |

---

## 2. License Boundary & Compartment Isolation

- **Crate Isolation:** `drivers/lx-virtio-rng-island/` is a standalone Cargo workspace (`license = "GPL-2.0-only"`, `#![no_std]`, `#![no_main]`, `lto = false`).
- **Two-Stage Native Build (`dry-lx-rng.elf`):**
  1. `clang -std=gnu11 -target x86_64-unknown-none-elf -ffreestanding -fno-builtin -fno-stack-protector -mcmodel=small -mno-red-zone -mno-sse -mno-sse2 -mno-avx -O2 -Wall -Wextra -Wno-unused-parameter -Werror -DKBUILD_MODNAME='"virtio_rng"'` compiles the unmodified upstream `virtio-rng.c` alongside `cshim/lx_virtio_shim.c` into `kernel/build/lx_rng/liblx_virtio_rng.a`.
  2. `dry-lx-rng` links `liblx_virtio_rng.a` with `userspace/src/` dual-licensed Rust IPC/capability wrappers under `SecDomain::RootSystem` (`compartment_id = 0x0008`), running strictly in Ring 3 (`CPL=3`) with no `kernel/` or `userspace/` GPL taint.
- **3-Slot Generalized PCI & DMA Kernel Substrate:**
  - `kernel/src/pci.rs` + `kernel/src/pci_cfg.rs` manage `PCI_OBJ_VIRTIO_BLK (0)`, `PCI_OBJ_VIRTIO_RNG (1)`, and `PCI_OBJ_VIRTIO_GPU (2)` with per-slot `owner_cspace` locking.
  - `VIRTIO_PCI_CAP_DEVICE_CFG` is optional (`1AF4:1044` omits device-specific PCI config).
  - Unprivileged callers (`DEV_WORKSPACE` or wrong capability rights/generation) are rejected with `-EPERM`/`-EBADCAP` on `SYS_DEV_MAP` (`29`) and `SYS_DMA_ALLOC` (`30`).

---

## 3. Minimal Linux Kernel API Compatibility Layer

All Linux symbols live in `drivers/lx-virtio-rng-island/cshim/include/` and `cshim/lx_virtio_shim.c` (`0` lines in Ring 0):

| Linux Header / Group | Implemented Symbols | DRY OS Userspace Mapping | Semantic Notes & Limits |
|---|---|---|---|
| `<linux/slab.h>` | `kzalloc`, `kfree`, `DEFINE_IDA`, `ida_alloc`, `ida_free` | Fixed `8 x 512 B` slot allocator backed by a single `SYS_DMA_ALLOC` pinned 4 KiB `DEVICE_DMA \| NO_DELEGATE` page (`g_slab_va`/`g_slab_pa`). | Zero-bounce DMA: `vi->data` allocated via `kzalloc` resides directly in `DEVICE_DMA` memory. Bounded to 4 KiB total / 8 allocations. |
| `<linux/scatterlist.h>` | `struct scatterlist`, `sg_init_one` | Translates `slab_va` pointer `[g_slab_va, g_slab_va + 4096)` to physical `g_slab_pa + offset` in `sg->dma_addr`. | Single-element `sg` (`num == 1`) only; chained SG tables are unsupported and rejected with `-EINVAL`. |
| `<linux/virtio.h>` | `virtio_find_single_vq`, `virtio_device_ready`, `virtio_reset_device`, `virtqueue_add_inbuf`, `virtqueue_kick`, `virtqueue_get_buf`, `module_virtio_driver` | Programs VirtIO 1.0 CommonCfg (`+0x14` status, `+0x16` qsel, `+0x20/+0x28/+0x30` split ring DMA PAs) over capability-mapped `USER_MMIO_UC` BAR (`SYS_DEV_MAP`). `lx_shim_init_device` invokes both `drv->probe` (`virtrng_probe`) and `drv->scan` (`virtrng_scan`). | Bounded split virtqueue (`VIRTQ_MAX_SIZE = 16`); 16-entry token table (`vq->tokens`); no indirect descriptors or packed rings. |
| `<linux/completion.h>` | `struct completion`, `init_completion`, `reinit_completion`, `complete`, `wait_for_completion_killable` | Atomic `done` word (`__atomic_store_n`/`__atomic_load_n` acquire-release); `wait_for_completion_killable` polls `used->idx` and invokes `vq->callback` (`random_recv_done`) -> `complete(&vi->have_data)` + `PAUSE`. | Cooperative bounded poll (`2,000,000` iterations max, returns `-EINTR` on stall) rather than sleeping kernel task waitqueue + MSI-X top-half IRQ. |
| `<linux/spinlock.h>`, `<asm/barrier.h>` | `spinlock_t`, `spin_lock_init`, `spin_lock`, `spin_unlock`, `smp_mb`, `smp_rmb`, `smp_wmb`, `smp_store_release`, `smp_load_acquire` | GCC/Clang `__atomic_*` intrinsics (`__ATOMIC_ACQUIRE` / `__ATOMIC_RELEASE` / `__ATOMIC_SEQ_CST`) + `__builtin_ia32_pause()`. | Single-threaded Ring 3 island (`IRQ`-disabled sleep semantics omitted). |
| `<linux/hw_random.h>` | `struct hwrng`, `hwrng_register`, `hwrng_unregister` | Registers `vi->hwrng` pointer in `g_registered_rng`; `lx_shim_read_entropy` dispatches `hwrng->read` (`virtio_read`). | Single active `hwrng` instance per island compartment. |
| `<linux/err.h>`, `<linux/kernel.h>`, `<linux/pm.h>` | `ERR_PTR`, `PTR_ERR`, `IS_ERR`, `min_t`, `sprintf`, `memcpy`, `memset`, `pm_sleep_ptr` | Freestanding C11 helpers (`-fno-builtin`). `pm_sleep_ptr(_ptr)` keeps suspend/restore symbols referenced for `-Wall -Wextra -Werror`. | Subset of `sprintf` (`%d/%i/%u/%x/%X/%s`); no dynamic `vsnprintf` float/wide formats. |

---

## 4. Control Plane vs Data Plane & Fault Recovery (`DRY Broker`)

```
[dry-broker] ──(Control Plane: spawn + FaultEndpoint + PCI grant + generation=1..2)──> [dry-lx-rng (CPL3)]
     ^                                                                                        |
     | (BROKER_OP_ACQUIRE_EP / ACQUIRE_RING: 2-hop setup)                                    | MMIO BAR + pinned DEVICE_DMA
     v                                                                                        v
[Supervisor Client] ══════(Direct Endpoint + IoRing Batch16 Data Plane)═══════════> [QEMU virtio-rng-pci 1af4:1044]
```

1. **Control-Plane Isolation:** `dry-broker` spawns `dry-lx-rng.elf` via `sys_proc_spawn_with_fault_ep` (`BADGE_BROKER_LX_MODE`), binds `pci_rng_cap` via `BROKER_OP_BIND_PCI_DEV`, mints badged client `Endpoint`s (`RIGHT_READ | RIGHT_SEND | RIGHT_CALL`) and `IoRing` shared pages with explicit `generation`, and never enters the steady-state data loop unless explicitly invoked via `BROKER_OP_FORWARD_TRANSFORM`.
2. **Data-Plane Execution:** Client requests (`LX_OP_READ_ENTROPY` or batched 16x `IoRingSharedPage` SQEs) invoke `lx_shim_read_entropy` -> `virtio_read` (`virtio-rng.c`) -> `virtqueue_add_inbuf` -> `virtqueue_kick` -> `random_recv_done` -> real host/QEMU entropy bytes.
3. **Fault & DMA Quiesce Containment (`R-03`):** On `#UD` fault injection (`vector = 6`), Ring 0 delivers `FAULT_MSG_LABEL` over `FaultEndpoint`; `dry-broker` replies `FAULT_REPLY_KILL`, reaps `dry-lx-rng.elf`, revokes `gen=1` caps (`Endpoint`, `Notification`, `IoRing` CQs), and Ring 0 `quiesce_slot_for_cspace` clears `PCI Command.BME = 0` + masks legacy INTx *before* releasing `DEVICE_DMA` `iommu_pin_count`. `dry-broker` then spawns `gen=2` `dry-lx-rng.elf` (`restarts=1`), probes live entropy read, and rejects all `gen=1` stale caps (`-EBADCAP`/`-EREVOKED`/`IPC_OP_ERR_STALE_GEN`).

---

## 5. Verified Empirical Results (`tools/compat_bench/results_p6_lx_rng.json`)

Executed on QEMU/KVM (`smp=2`, `-device virtio-rng-pci-non-transitional`, `--lx-island-only`):

| Metric | Measured Value | Verification Target |
|---|---|---|
| Bound PCI Device | `1af4:1044` (`virtio-rng`) | Genuine VirtIO 1.0 device probe + `.scan` |
| Hardware Virtqueue Completions (`vq_irqs`) | `35` (`>= 28` required) | Genuine device completions (`random_recv_done`) |
| Entropy Delivered & Distribution | `1792` bytes, `ones=49%`, `32/32` unique `u64` | Non-mock, high-entropy stream |
| Direct 1-Hop Latency (`p50 / p95 / p99`) | `15,019 / 76,414 / 213,439` cyc (`15.02 us` p50) | Fast client-to-island direct path |
| Broker 2-Hop Latency (`p50 / p95 / p99`) | `20,106 / 72,183 / 72,183` cyc (`20.11 us` p50) | `~5,087` cyc control-plane hop overhead |
| `IoRing` Batch-16 Throughput | `935,344` cyc total (`58,459.0` cyc/op) | Zero-broker-hop batched SQE/CQE |
| `#UD` (`vec=6`) Recovery & DMA Quiesce | `8,831,637` cyc (`8.83 ms`), `pci_quiesce=3`, `restarts=1` | Hardware `BME=0` before DMA unpin |
| Unauthorized Access & Leaks | `dma_denied=7`, `leaked_frames=0` | `DEV_WORKSPACE` / rogue / stale cap rejection |
| Island Working Set (`RSS`) | `26` x 4 KiB frames (`104 KiB`) | Minimal `GPL-2.0-only` Ring 3 footprint |
