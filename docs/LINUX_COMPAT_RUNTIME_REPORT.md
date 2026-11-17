# DRY OS — Universal Linux Driver Compatibility Runtime Report (`libdry-lx-compat`)

> **Milestone:** Project Ascension 4.0 Track C (`A4-3`) | **Date:** 2026-11-14 | **Status:** Verified on QEMU/KVM (`--p12-only`)  
> **Raw Evidence:** [`tools/compat_bench/results_p12_lx_compat.json`](../tools/compat_bench/results_p12_lx_compat.json), [`tools/lx_compat/COMPAT_CATALOG.json`](../tools/lx_compat/COMPAT_CATALOG.json)

---

## 1. Architectural Boundary & Rationale (Why `.ko` Binaries Are Rejected)

DRY OS enforces strict microkernel isolation (`ADR-005`, `ADR-006`, `ADR-016`, `INV-ISL-05`):

1. **Zero Linux / GPL Code in Ring 0:** [`dry-kernel`](../kernel/src/main.rs) remains `MIT OR GPL-2.0-only` with `0` lines of Linux ABI, POSIX, or DRM headers. `Syscall ABI v0.1` (`0..=31`) is unchanged and `rust_syscall_dispatch` remains **`248 B` (`0xf8`)** (`<= 256 B`).
2. **Source-Level Compatibility Over Binary `.ko` Loading:** [`tools/lx_compat/lx_driver_manager.py`](../tools/lx_compat/lx_driver_manager.py) explicitly rejects Linux kernel module binaries (`.ko`, `.ko.zst`, `.ko.xz`, `.ko.gz`, or ELF `ET_REL` objects) with `KO_BINARY_REJECTED`. Loading un-recompiled `.ko` binaries would tie DRY OS to volatile Linux internal struct layouts (`MODVERSIONS`), `EXPORT_SYMBOL_GPL` kernel assumptions, ambient Ring-0 pointer dereferences, and kernel-thread sleeps. Instead, DRY OS fetches exact SHA-256-locked upstream C driver sources (`v6.18`, commit `7d0a66e4bb9081d75c82ec4957c50034cb0ea449`) and compiles them unmodified at source level into **isolated Ring-3 driver island binaries**.

---

## 2. Reusable Source-Level Runtime & Toolchain (`libdry-lx-compat` + `lx_driver_manager.py`)

| Component | Path | Responsibility & Observed Guarantees |
| :--- | :--- | :--- |
| **Driver Manager CLI** | [`tools/lx_compat/lx_driver_manager.py`](../tools/lx_compat/lx_driver_manager.py) (`347` lines) | `fetch`, `verify` (`54` SHA-256-locked files: `virtio-rng.c`, `drm_bochs/bochs.c`, `52` `igc` `v6.12`/`v6.18` sources), `match-pci`, `load-module` (`.ko` rejection), content-addressed `build-cache`, and `pin`/`rollback`. Unit tests: `4/4 OK` ([`test_lx_driver_manager.py`](../tools/lx_compat/test_lx_driver_manager.py)). |
| **Freestanding C Runtime** | [`drivers/libdry-lx-compat/src/lx_drm_compat.c`](../drivers/libdry-lx-compat/src/lx_drm_compat.c) (`623` lines) | Implements `devm_zalloc`, `devm_ioremap[_wc]`, `drm_dev_alloc`, EDID parsing (`0x00 FF ... 0x00`), DRM atomic damage commit hooks, and `readb`/`writel` BAR accessors without heap/libc (`-Wall -Wextra -Werror`, `0` SSE/AVX). |
| **Compat Headers** | [`drivers/libdry-lx-compat/include/`](../drivers/libdry-lx-compat/include/drm/drm_compat_all.h) | Narrow `<linux/{compat_base,bug,aperture,module,pci}.h>`, `<video/vga.h>`, and `17` `<drm/*.h>` forwarders compiling unmodified upstream `bochs.c` (`829` lines). |
| **Kernel PCI Aux Slot** | [`kernel/src/pci_cfg.rs`](../kernel/src/pci_cfg.rs), [`kernel/src/pci.rs`](../kernel/src/pci.rs), [`kernel/src/iommu/vtd.rs`](../kernel/src/iommu/vtd.rs) | Expands `PCI_DEV_COUNT = 7` with `PCI_OBJ_DISPLAY_AUX = 6` (`"display-aux"`), primary GOP BAR0 collision guard (skipping primary `-vga std`), and 32-byte `BootPciFuncRecord` discovery table (`boot_info + 0x0800`). |
| **Userspace Meta & Proto** | [`userspace/src/lx_compat_meta.rs`](../userspace/src/lx_compat_meta.rs), [`userspace/src/lx_drv_proto.rs`](../userspace/src/lx_drv_proto.rs) | Safe `read_boot_pci_funcs`, `find_boot_pci_managed(..., 6)`, `COMP_ID_LX_DRM_BOCHS_ISLAND = 0x0212`, direct IPC `LX_DRM_OP_*` (`170..173`), and Tier-2 `IORING_OP_DRM_FLUSH = 18`. |

---

## 3. Second Genuine Linux `v6.18` Driver: `drivers/gpu/drm/tiny/bochs.c`

Unmodified upstream Linux `v6.18` [`vendor/linux-drivers/v6.18/drm_bochs/bochs.c`](../tools/lx_compat/COMPAT_CATALOG.json) (`22,785 B`, `829` lines, `sha256=93f5c4bc2811ad1e0961a4e3d5c0410ac4b2336b2bdc96cc2c40802cc439ffe3`) is linked exclusively inside [`drivers/lx-drm-bochs-island/`](../drivers/lx-drm-bochs-island/src/main.rs) (`dry-lx-drm-bochs.elf`, `60K`, `CPL=3`, compartment `0x0212`).

```text
Supervisor (dry-init) ──control──▶ Device Broker (0x0008, BADGE_BROKER_LX_MODE)
                                       │ binds PCI_OBJ_DISPLAY_AUX (slot 6) & FaultEndpoint
                                       ▼
Client (lx_drm_audit) ──2M RW Frame──▶ grant(RIGHT_READ | RIGHT_MAP) ──▶ dry-lx-drm-bochs (0x0212)
  ├── paints 800x600 test card         ├── VSPACE_MAP_OP_2M_RO (0x2800_0000; no write right)
  ├── Tier-1: LX_DRM_OP_COMMIT_DAMAGE  ├── unmodified v6.18 bochs.c + libdry-lx-compat
  └── Tier-2: IoRing SQE (OP_DRM_FLUSH)└── SYS_DEV_MAP BAR0 (WC/UC FB) + BAR2 (MMIO/EDID)
```

### Observed Verification Metrics (`--p12-only`)

From [`tools/compat_bench/results_p12_lx_compat.json`](../tools/compat_bench/results_p12_lx_compat.json) (QEMU Q35 + `-device intel-iommu` + `-vga std` + `-device bochs-display,id=bochs0,vgamem=4M` at `3113 MHz` TSC):

| Verification Metric | Measured Result |
| :--- | :--- |
| Secondary PCI GPU & Multi-GPU Enumeration | `1234:1111` (`bdf=32`, `managed_slot=6`), `display_pci_funcs_visible = 2` (primary GOP untouched) |
| Upstream Source Integrity | `54/54` files SHA-256 verified (`0` patches to `bochs.c` or `virtio-rng.c`) |
| Capability Isolation & RO PRIME DMA-BUF | `ro_dmabuf_enforced = true`, `kernel_dma_denied = 5`, `ioport_denied = 2` (gen1/gen2 self-test), `unauth = 5` |
| Hardware Probe inside Island | `bar0_mapped = true`, `bar2_mapped = true`, `edid_valid = true`, `res = 800x600` (`14` colors, `473,069` non-bg px) |
| Tier-1 Direct IPC Damage Flush (`64x64`, 32 ops) | `p50 = 57,016 cyc` (`18.315 us`), `p95 = 68,050 cyc`, `p99 = 73,963 cyc` |
| Tier-2 `IoRingSharedPage` Damage Flush (32 ops) | `1,612,765 cyc` total (**`50,398.9 cyc/flush`**) |
| Fault Injection & Recovery (`FaultEndpoint` + `BME=0`) | `#UD` (`vec=6`), `restarts = 1` (`gen=2` live), `kernel_pci_quiesces = 3`, `19,000,880 cyc` (`6,103.72 us`), `stale_gen` + `ep/ntf/cq/ring` revoked |
| Resident Memory & Clean Exit | `island_rss_frames_4k = 1,564` (~6.11 MiB incl. BAR0+DMA-BUF maps), `leaked_frames = 0`, `wall_time = 3.818s` |

### Visual Scanout Coexistence Proofs
1. Secondary Linux DRM `bochs.c` `gen=2` `800x600` scanout (`device="bochs0"`): [`p12_bochs_drm_scanout.png`](screenshots/glass21/p12_bochs_drm_scanout.png)
2. Concurrent Primary UEFI GOP DRY Glass 2.1 desktop: [`p12_gop_desktop_coexist.png`](screenshots/glass21/p12_gop_desktop_coexist.png)

---

## 4. Ascension 5.0 (`v0.5.0`) M3: Modular `libdry-lx-compat` v2, 15-Step Lifecycle & `8139cp.c` + `mii.c`

Project Ascension 5.0 promotes `libdry-lx-compat` (`MIT OR GPL-2.0-only`) from a single DRM shim into a modular multi-subsystem Ring-3 Linux driver compatibility runtime without adding a single GPL line to `kernel/`:

### 4.1. 6-File Modular `libdry-lx-compat` (`2,164` Reusable C LOC, All `< 600` Lines)

| Module (`drivers/libdry-lx-compat/src/`) | LOC | Capabilities Provided |
| :--- | :---: | :--- |
| `lx_core_sync_wq.c` | `419` | `spinlock`/`mutex`, `completion`, `jiffies`, bounded single-threaded `workqueue`/`delayed_work`/`napi_struct` polling |
| `lx_pci_dma_irq.c` | `453` | `pci_enable_device`/`pci_set_master`, `pci_iomap` with `< 4 KiB` sub-page `UserPciBarInfo.page_suboff`, coherent `sys_dma_alloc` + `2M` arena, `dma_map_single`, `request_irq` (MSI `sys_irq_bind` + deterministic INTx `IntrStatus`/NAPI poll fallback) |
| `lx_firmware_pm.c` | `162` | `request_firmware`/`release_firmware` registration + runtime PM transitions (`RPM_ACTIVE`/`RPM_SUSPENDED`) |
| `lx_dmabuf_fence.c` | `173` | `dma_fence` refcounting/signaling + `dma_buf` handle wrappers over DRY `FrameCap` (`READ_ONLY_EXPORT`/`DEVICE_DMA`) |
| `lx_drm_compat.c` | `464` | `drm_dev_alloc`, `devm_ioremap[_wc]`, EDID probe, atomic shadow-plane damage commit hooks (`bochs.c`) |
| `lx_net_compat.c` | `493` | `alloc_etherdev`, `sk_buff` ring pool (`128 x 2048 B`), `eth_type_trans`, NAPI RX/TX, `mii_if_info` bridge, and QEMU SLIRP (`10.0.2.15`) ARP + UDP echo (`port 7777`) responder |

### 4.2. 15-Step Broker Lifecycle & Multi-Backend Contracts

- **15-Step Lifecycle State Machine (`userspace/src/broker_lifecycle.rs`, `233` lines & `lx_driver_manager.py`, `517` lines):** Explicit verified transitions across `DETECTED -> MATCHED -> ASSESSED -> SOURCE_RESOLVED -> DEPS_FIRMWARE_OK -> BUILT_VERIFIED -> ISLAND_CREATED -> CAPS_ASSIGNED -> RESOURCES_MAPPED -> INITIALIZED -> PUBLISHED -> MONITORING -> FAULT_RECOVERY -> UNLOADED -> ROLLED_BACK`.
- **Multi-Backend Contracts (`COMPAT_CATALOG.json` & `dry_broker.rs`, `793` lines):** Supports `native-driver-island`, `lx-compat-island`, and `linux-microvm` backend contracts, uniformly enforcing `CONTRACT_FLAG_REQUIRE_RING0_KO -> -EPERM`, `CONTRACT_FLAG_ASSUME_IGPU_PASSTHROUGH -> -EINVAL`, and mandatory `FaultEndpoint` supervision, surfaced to `Terminal` `drvctl` via `BROKER_OP_DRVCTL_QUERY`.

### 4.3. Third Unmodified Linux `v6.18` Driver: Realtek `8139cp.c` + `mii.c` (`--p14-only`)

- **Upstream Sources (`56/56` SHA-256 Verified):** `tools/lx_compat/fetch_8139cp_sources.py` + `vendor/linux-drivers/MANIFEST.json` pin unmodified `v6.18` `drivers/net/ethernet/realtek/8139cp.c` (`2,121` LOC) and `drivers/net/mii.c` (`668` LOC).
- **Isolated `GPL-2.0-only` Island (`drivers/lx-net-8139cp-island/src/main.rs`, `613` lines):** Compiles `dry-lx-net.elf` (`comp_id = 0x0214`, `SecDomain::RootSystem` for non-MSI INTx/PCI access, `FaultEndpoint`-supervised). Uses Wave-1 `< 4 KiB` sub-page BAR1 (`page_suboff`) mapping (`eff_bar_limit = bar.size_bytes.max(4096)`), `2 MiB` pinned DMA arena, and `libdry-lx-compat` (`2,164` shared C LOC vs `613` LOC island glue).
- **Verification Status (`--p14-only` PASS):** `lx_driver_manager.py verify --all` (`56/56` files, `0` modified), `test_lx_driver_manager.py` (`5/5 OK`), `qemu_boot_test.py --build-only` (`clippy -D warnings` + `liblx_net_8139cp.a` + `dry-lx-net.elf`), and `python3 tools/qemu_boot_test.py --p14-only` (**`1/1 PASS`**, `3.95s` standalone / `3.85s` in combined `135/135` suite) all pass: QEMU Q35 `-device rtl8139` observed `8139cp+mii`, `mac=0x563412005452`, `bar1_suboff=0`, `tx=2`, `rx=2`, `arp=1`, `udp_echo=1`, `#UD vec=6`, `restarts=1` (`gen=2`), `drvctl=0x1fff`, and `leaked_frames=0`.
