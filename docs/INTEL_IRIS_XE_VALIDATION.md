# DRY OS — Intel Iris Xe (`8086:9a49`) Hardware Compatibility & Validation Report

> **Milestone:** Project Ascension 4.0 Track D (`A4-4`) | **Date:** 2026-11-14 | **Scope:** Strictly Read-Only Host Inspection + Kernel Readiness Matrix  
> **Raw Evidence:** [`tools/compat_bench/results_target_gpus_a4.json`](../tools/compat_bench/results_target_gpus_a4.json), [`tools/lx_compat/validate_target_gpus.py`](../tools/lx_compat/validate_target_gpus.py)

---

## 1. Epistemic Honesty Statement

No bare-metal `i915` or `xe` Linux GPU driver execution or physical hardware modesetting was attempted or claimed in this milestone. Host Linux GPU state was inspected **strictly read-only** via `/sys/bus/pci/devices/*` and `/lib/firmware/i915/` (`0` sysfs writes, `0` module unbinds, `0` root commands). Real bare-metal desktop output on this laptop currently relies on the **UEFI GOP** fallback path packaged into [`dryos-ascension4-live-usb.img`](../tools/build_live_usb_image.py).

---

## 2. Observed Host Device & Firmware Inventory (`0000:00:02.0`)

Captured by [`tools/lx_compat/validate_target_gpus.py`](../tools/lx_compat/validate_target_gpus.py) into [`results_target_gpus_a4.json`](../tools/compat_bench/results_target_gpus_a4.json):

| Attribute | Observed Host Value |
| :--- | :--- |
| PCI BDF / IDs | `0000:00:02.0`, `vendor = 0x8086`, `device = 0x9a49` (TigerLake-LP GT2 [Iris Xe Graphics]) |
| Class / Revision / Subsystem | `class = 0x30000`, `revision = 0x1`, `subvendor = 0x17aa`, `subdevice = 0x3a5b` |
| Active Host Driver | `i915` |
| Catalog Match (`COMPAT_CATALOG.json`) | `intel_i915_tgl` (`status = profile_audit_only`, planned capabilities listed) |
| **BAR0** (MMIO Registers / GTTMMADR) | `0x6104000000 .. 0x6104ffffff` (`16.0 MiB`, `flags = 0x140204`, `is_mem = true`, **`above_4g = true`**) |
| **BAR2** (Aperture / GMADR) | `0x4000000000 .. 0x400fffffff` (`256.0 MiB`, `flags = 0x14220c`, `is_prefetchable = true`, **`above_4g = true`**) |
| **BAR4** (Legacy I/O Window) | `0x5000 .. 0x503f` (`64 B`, `flags = 0x40101`, `is_io = true`) |
| Firmware Inventory (`/lib/firmware/i915/tgl_*`) | **`15` firmware files** present (`tgl_dmc_ver2_04..2_12.bin.zst`, `tgl_guc_35.2.0..70.bin.zst`, `tgl_huc_*`) |

---

## 3. DRY OS Architectural Readiness vs. Verified Gaps

### Already Built & QEMU-Verified Prerequisites in DRY OS
- **64-Bit `>4 GiB` PCI BARs (`BAR0 = 0x6104000000`, `BAR2 = 0x4000000000`):** [`kernel/src/pci_bar.rs`](../kernel/src/pci_bar.rs) and [`kernel/src/pci_window.rs`](../kernel/src/pci_window.rs) support decoding and mapping 64-bit `>4 GiB` UC and `PAT4` Write-Combining (`WC`) prefetchable BAR windows via `SYS_DEV_MAP` (`29`), proven in `Milestone 2C-2` (`--p8-only`).
- **`VT-d` IOMMU Isolation & MSI Routing:** [`kernel/src/iommu/`](../kernel/src/iommu/vtd.rs) parses ACPI `DMAR` tables and isolates PCI DMA per-BDF via 4-level `SLPT` tables (`FRCD` containment + `quiesce_slot_for_cspace`), while [`kernel/src/irq/msi.rs`](../kernel/src/irq/msi.rs) delivers APIC `MSI`/`MSI-X` (`0x40..=0x5F`) to `TCB`-bound `Notification`s (`24.42 us` median in `2C-2`).
- **Multi-GPU Safe Enumeration & Live USB UEFI GOP Fallback:** [`kernel/src/pci_cfg.rs`](../kernel/src/pci_cfg.rs) exports sanitized `BootPciFuncRecord` descriptors without overwriting the primary UEFI GOP console, ensuring the DRY Glass 2.1 desktop renders natively via GOP on hardware lacking an active native iGPU driver.

### Honest Remaining Engineering Gaps for Native `8086:9a49` Modeset
As quantified in [`docs/GPU_COMPATIBILITY_REPORT.md`](GPU_COMPATIBILITY_REPORT.md), upstream `i915.ko` / `xe.ko` carry `1,244` / `1,213` undefined kernel symbols (`drm_kms_gem_ttm` = `509` / `592` symbols). Turning `profile_audit_only` into a live TigerLake modesetting Driver Island (`Ascension 5.0+`) requires:
1. **GEM / TTM / GGTT Memory Layer:** A Ring-3 `libdry-lx-compat` allocator backing GGTT/PPGTT page tables and shmem/vram buffer objects via pinned `DEVICE_DMA` frames and `VT-d` `SLPT`.
2. **Firmware Streaming (`request_firmware` + Zstd):** An offline or VFS `/boot/firmware` `.bin.zst` decompressor to load `tgl_dmc_ver2_12.bin` and `tgl_guc_70.1.1.bin` into DMA buffers and authenticate GuC/HuC.
3. **TigerLake Display Engine & Platform Hooks:** Display PHY/DPLL programming, DP AUX/GMBUS EDID transactions, ACPI OpRegion / VBT table parsing, and `P5` bare-metal silicon testing under physical `VT-d` DMAR.
