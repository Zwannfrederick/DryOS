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
