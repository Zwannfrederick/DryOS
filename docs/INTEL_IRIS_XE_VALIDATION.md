# DRY OS — Intel Iris Xe (`8086:9a49`) Hardware Compatibility & Validation Report

> **Milestones:** Project Ascension 4.0 Track D (`A4-4`, `2026-11-14`) -> Project Ascension 5.0 Track C / M4 (`2026-11-16`)  
> **Scope:** Strictly Read-Only Host Inspection + CSS/OpRegion/EDID/GOP Preservation Validators + Readiness Matrix  
> **Raw Evidence:** [`tools/compat_bench/results_target_gpus_a4.json`](../tools/compat_bench/results_target_gpus_a4.json), [`tools/compat_bench/lenovo_15ihu6_inventory.json`](../tools/compat_bench/lenovo_15ihu6_inventory.json), [`tools/lx_compat/validate_target_gpus.py`](../tools/lx_compat/validate_target_gpus.py), [`tools/gpu_fw_validators.py`](../tools/gpu_fw_validators.py)

---

## 1. Epistemic Honesty Statement

No bare-metal `i915` or `xe` Linux GPU driver execution or physical hardware modesetting was attempted or claimed in either Ascension 4.0 or Ascension 5.0. Host Linux GPU state on the Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`, Core i5-11300H, `39-bit` phys / `48-bit` virt, `32 GiB` RAM, `8250.nr_uarts=0`, Secure Boot `disabled`) was inspected **strictly read-only** via `/sys/bus/pci/devices/*`, `/sys/class/drm/card2-eDP-1/edid`, and `/lib/firmware/i915/` (`0` sysfs writes, `0` `/dev/*` opens, `0` module unbinds, `0` `efivars`/`efibootmgr` touches). Real bare-metal desktop output on this laptop currently relies on the **UEFI GOP** path packaged into [`build_live_usb_image.py`](../tools/build_live_usb_image.py) (`dryos-ascension4-live-usb.img`), with physical execution tracked under **Gates 5-7 (`READY FOR USER PHYSICAL EXECUTION`)**.

---

## 2. Observed Host Device & Firmware Inventory (`0000:00:02.0`)

Captured by [`tools/lx_compat/validate_target_gpus.py`](../tools/lx_compat/validate_target_gpus.py) (`results_target_gpus_a4.json`) and [`tools/hw_inventory_lenovo_15ihu6.py`](../tools/hw_inventory_lenovo_15ihu6.py) (`lenovo_15ihu6_inventory.json`):

| Attribute | Observed Host Value |
| :--- | :--- |
| PCI BDF / IDs | `0000:00:02.0`, `vendor = 0x8086`, `device = 0x9a49` (TigerLake-LP GT2 [Iris Xe Graphics]) |
| Class / Revision / Subsystem | `class = 0x30000`, `revision = 0x1`, `subvendor = 0x17aa`, `subdevice = 0x3a5b` |
| Active Host Driver | `i915` (`irq = 153`) |
| Catalog Match (`COMPAT_CATALOG.json`) | `intel_i915_tgl` (`status = profile_audit_only`, planned capabilities listed) |
| **BAR0** (MMIO Registers / GTTMMADR) | `0x6104000000 .. 0x6104ffffff` (`16.0 MiB`, `flags = 0x140204`, `is_mem = true`, **`above_4g = true`**) |
| **BAR2** (Aperture / GMADR) | `0x4000000000 .. 0x400fffffff` (`256.0 MiB`, `flags = 0x14220c`, `is_prefetchable = true`, **`above_4g = true`**) |
| **BAR4** (Legacy I/O Window) | `0x5000 .. 0x503f` (`64 B`, `flags = 0x40101`, `is_io = true`) |
| Host eDP-1 Panel & EDID | `AUO 0xb69b` (`B156HAN12.H`, EDID `1.4`, `128 B`, `1920x1080@165.01Hz`, `405,000 kHz` pixel clock, `checksum_mod256 = 0`) |
| Firmware Inventory (`/lib/firmware/i915/tgl_*`) | **`15` firmware files** present (`tgl_dmc_ver2_04..2_12.bin.zst`, `tgl_guc_35.2.0..70.bin.zst`, `tgl_huc_*`) |

### Local Read-Only Firmware SHA-256 Hashes (`tools/provision_local_firmware.py`)
Vendor firmware is **never** committed to git; [`tools/provision_local_firmware.py`](../tools/provision_local_firmware.py) audits local hashes read-only and stages only into an explicit user-chosen `--staging-dir` outside git-tracked directories:
- `tgl_dmc_ver2_12.bin.zst` (`5,141 B` compressed / `19,760 B` raw): `sha256=cf8d44b8b879ffd62bf618074fb861c2306699827f4a7f25816f9b8cb7848c4c`
- `tgl_guc_70.1.1.bin.zst` (`102,651 B` compressed / `277,440 B` raw): `sha256=283a9eada519b87090ee8eefcb7b39e779b3844a57ee3c03e093ead7b86656ef`
- `tgl_huc.bin.zst` (`154,252 B` compressed): `sha256=0450a7390c8d6c2d0034aa1a74edde979257009f847b63c9e7b5206ab1fe11b9`

---

## 3. Ascension 5.0 Track C / M4 Validator Results (`gpu_fw_validators.py --self-test`)

Executed `python3 tools/gpu_fw_validators.py --self-test` (`PASS`, `628` lines) against both deterministic synthetic test vectors (valid + corrupted) and live read-only host blobs:

| Check | Target / Vector | Verified Result |
| :--- | :--- | :--- |
| **CSS DMC Header Validator** | Host `tgl_dmc_ver2_12.bin.zst` (`19,760 B` raw) + synthetic valid/corrupt | **PASS** (`module_type=9`, `header=128 B`, `size_dw=4940`, `version=2.12`, `pkg_num_entries=2`, `0x40403e3e` magic at `528`; corrupt `size_dw` rejected) |
| **CSS GuC Header Validator** | Host `tgl_guc_70.1.1.bin.zst` (`277,440 B` raw) + synthetic valid/corrupt | **PASS** (`module_type=6`, `vendor=0x8086`, `header=644 B`, `2048-bit` RSA key, `size_dw=69425` matching `raw + 260 B` RSA tail, `version=70.1.1`; bad vendor rejected) |
| **ACPI IGD OpRegion & `$VBT`** | Synthetic `8 KiB` OpRegion (`IntelGraphicsMem`, `$VBT`, `BIOS_DATA_BLOCK `) + corrupt | **PASS** (`opregion_version=2.0`, `mboxes=0xf`, `vbt_offset=1024`, `bdb_ok=True`; bad magic rejected) |
| **EDID 1.4 Checksum & DTD1** | Host `/sys/class/drm/card2-eDP-1/edid` (`128 B`) + synthetic valid/bad checksum | **PASS** (`AUO 0xb69b`, `1920x1080@165.01Hz`, `checksum_mod256=0`; corrupted checksum rejected) |
| **Gen12 `preserve_active_gop` Contract** | Contract model + source check on `kernel/src/{pci_cfg,pci_window,pci}.rs` | **PASS** (`bar_probe_writes=0`, `SYS_DEV_MAP=-EPERM`, `bme_preserved=True`, BAR2 scanout CRC untouched while active) |

---

## 4. DRY OS Architectural Readiness vs. Verified Gaps & Physical Gates

### Already Built & QEMU/Validator-Verified Prerequisites in DRY OS
- **64-Bit `>4 GiB` PCI BARs (`BAR0 = 0x6104000000`, `BAR2 = 0x4000000000`):** [`kernel/src/pci_bar.rs`](../kernel/src/pci_bar.rs) and [`kernel/src/pci_window.rs`](../kernel/src/pci_window.rs) support decoding and mapping 64-bit `>4 GiB` UC and `PAT4` Write-Combining (`WC`) prefetchable BAR windows via `SYS_DEV_MAP` (`29`), proven in `Milestone 2C-2` (`--p8-only`).
- **`VT-d` IOMMU Isolation & MSI Routing:** [`kernel/src/iommu/`](../kernel/src/iommu/vtd.rs) parses ACPI `DMAR` tables and isolates PCI DMA per-BDF via 4-level `SLPT` tables (`FRCD` containment + `quiesce_slot_for_cspace`), while [`kernel/src/irq/msi.rs`](../kernel/src/irq/msi.rs) delivers APIC `MSI`/`MSI-X` (`0x40..=0x5F`) to `TCB`-bound `Notification`s (`24.42 us` median in `2C-2`).
- **Gen12 Active GOP Preservation & UART-less Boot Safety:** [`kernel/src/pci_cfg.rs`](../kernel/src/pci_cfg.rs) marks primary `00:02.0` (`PCI_OBJ_IGPU`) with `preserve_active_gop = true` (`read_bars_readonly`, skipping BAR sizing writes and `SYS_DEV_MAP` mutations), and [`kernel/src/serial.rs`](../kernel/src/serial.rs) probes COM1 scratch/LSR (`!= 0xff`) so `8250.nr_uarts=0` laptops never stall.

### Honest Remaining Engineering Gaps for Native `8086:9a49` Modeset
As quantified in [`docs/GPU_COMPATIBILITY_REPORT.md`](GPU_COMPATIBILITY_REPORT.md), upstream `i915.ko` / `xe.ko` carry `1,244` / `1,213` undefined kernel symbols (`drm_kms_gem_ttm` = `509` / `592` symbols). Turning `profile_audit_only` into a live TigerLake modesetting Driver Island requires:
1. **GEM / TTM / GGTT Memory Layer:** A Ring-3 `libdry-lx-compat` allocator backing GGTT/PPGTT page tables and shmem/vram buffer objects via pinned `DEVICE_DMA` frames and `VT-d` `SLPT`.
2. **Firmware Streaming (`request_firmware` + Zstd):** An offline or VFS `/boot/firmware` `.bin.zst` decompressor to load `tgl_dmc_ver2_12.bin` and `tgl_guc_70.1.1.bin` into DMA buffers and authenticate GuC/HuC.
3. **TigerLake Display Engine & Platform Hooks:** Display PHY/DPLL programming, DP AUX/GMBUS EDID transactions, live ACPI OpRegion / VBT table retrieval from UEFI/ACPI memory, and bare-metal silicon execution.

### QEMU/Validator PASS vs. Bare-Metal Gates 5-7
- **QEMU & Host Read-Only Validators:** **PASS** (`results_target_gpus_a4.json`, `lenovo_15ihu6_inventory.json`, `gpu_fw_validators.py --self-test`, `provision_local_firmware.py --self-test`, QEMU `--p8-only` & `--p12-only`).
- **Gate 5 (Physical Live USB Boot on `82K1` via xHCI `00:14.0` + GOP Desktop):** **`READY FOR USER PHYSICAL EXECUTION`** (never executed by agent).
- **Gate 6 (Physical `VT-d` `DMAR` + `00:02.0` `8086:9a49` `preserve_active_gop` Verification):** **`READY FOR USER PHYSICAL EXECUTION`** (never executed by agent).
- **Gate 7 (Physical Hybrid Optimus / PRIME / `RTD3` Verification):** **`READY FOR USER PHYSICAL EXECUTION`** (requires user physical USB run; native `i915`/`xe` modeset remains `BLOCKED` until items 1-3 above land).
