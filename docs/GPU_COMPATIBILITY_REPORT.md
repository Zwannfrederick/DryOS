# DRY OS Priority C & M4 — GPU Compatibility, VirtIO-GPU / Bochs DRM & Iris Xe / NVIDIA / PRIME Audit

## 1. Executive Summary

Priority C and Ascension 4.0/5.0 Track C advance DRY OS from a single UEFI GOP compositor (`dry-display`) to capability-isolated Ring-3 display drivers (**VirtIO-GPU 1.0 `1AF4:1050`** `dry-gpu.elf` and unmodified **Linux `v6.18` `bochs.c` `1234:1111`** `dry-lx-drm-bochs.elf` on `libdry-lx-compat`) with live **UEFI GOP fallback**, paired with a strictly read-only hardware inventory of the **Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`)** hybrid **Intel Iris Xe (`8086:9a49`)** + **NVIDIA RTX 3050 Ti Mobile (`10de:25a0`)** GPU stack and offline firmware/OpRegion/EDID/WPR2/PRIME validators (`tools/gpu_fw_validators.py`).

- **Verified QEMU GPU Emulation (`--gpu-only` / `--p12-only`):**
  - [`p7a_virtio_gpu_2d_scanout_and_gop_fallback`](../tools/qemu_p7_gpu_validators.py) boots QEMU/KVM (`2 vCPUs`, `1AF4:1050`), renders `1280x800` via `dry-gpu.elf`, injects `#UD` (`vec=6`), verifies `[DRY-WM] GPU_FALLBACK_TO_GOP`, respawns `gen=2`, captures [`docs/p7_virtio_gpu_scanout.png`](./p7_virtio_gpu_scanout.png), and exits cleanly (`leaked_frames=0`).
  - [`p12a_linux_v618_bochs_drm_island`](../tools/qemu_p12_lx_compat_validators.py) runs unmodified `v6.18` `bochs.c` (`sha256=93f5c4bc...`) in `dry-lx-drm-bochs.elf` (`0x0212`, `PCI_OBJ_DISPLAY_AUX = 6`) alongside the primary GOP desktop (`display_pci_funcs_visible = 2`, `18.315 us` median damage commit, `800x600` scanout + `1280x800` GOP coexist, `#UD` `vec=6` -> `gen=2` recovery in `6.104 ms`, `leaked_frames=0`).
- **Bare-metal iGPU/dGPU reality check (`H3` + Ascension 5.0 Track C / M4):** Running unmodified Linux `xe.ko` (`1,213` UND symbols), `i915.ko` (`1,244` UND symbols), or NVIDIA's `open` stack (`1,189` UND symbols + `162` `os_*` APIs of which `22` are prototyped in `nv_os_shim_check.c` + `81.3 MiB` `gsp_ga10x.bin`/`ucodes_ga10x.bin`) on physical hardware remains **blocked for live GPU modesetting** by missing DRM GEM/TTM/dma-buf, GuC/GSP firmware loaders, and physical silicon verification — while kernel prerequisites (**`VT-d` DMAR `SLPT`**, **APIC `MSI`/`MSI-X`**, **64-bit `>4 GiB` `WC` BAR**, **`preserve_active_gop`**, and **UART-less `8250.nr_uarts=0` `serial.rs` probing**) are **implemented & QEMU/validator-verified**, leaving physical **Gates 5-7** as **`READY FOR USER PHYSICAL EXECUTION`**.

---

## 2. VirtIO-GPU & Linux `bochs.c` 2D Scanout Telemetry (`P7` / `P12`)

```
dry-display (CPL=3, 0x0101)
  ├── scene back_caps [2 x 2 MiB WB RAM] ──grant(RIGHT_READ|RIGHT_MAP)──▶ dry-gpu (CPL=3, 0x0009)
  ├── GPU_OP_PRESENT_RECT (bbox + cursor + flags) ────────────────────────▶   ├── VSPACE_MAP_OP_2M_RO (0x2600_0000)
  └── on EREVOKED / EBADCAP ──▶ [DRY-WM] GPU_FALLBACK_TO_GOP ──▶ UEFI GOP    ├── 2x2 MiB DEVICE_DMA scanout + 12x16 cursor
                                                                              ├── TRANSFER_TO_HOST_2D + RESOURCE_FLUSH
dry-init Supervisor ◀── FaultEndpoint (vec=6 #UD) ── kernel BME=0 quiesce     └── controlq via SYS_DEV_MAP (1AF4:1050)
```

| Metric ([`results_p7_gpu.json`](../tools/compat_bench/results_p7_gpu.json) & [`results_p12_lx_compat.json`](../tools/compat_bench/results_p12_lx_compat.json)) | Verified Value |
|---|---|
| `P7` VirtIO-GPU (`1af4:1050`, `1280x800`) | `gen=2` (`#UD` `vec=6`), `9`+`3` presents (`22,080 KiB`), `p50=1096.70 us`, recovery `8.727 ms`, `leaked_frames=0` |
| `P12` Linux `v6.18` `bochs.c` (`1234:1111`, `800x600`) | `ro_dmabuf_enforced=true`, `disp_pci=2`, `p50=18.315 us`, `ring32=50,398.9 cyc/flush`, `gen=2` (`6.104 ms`), `leaked_frames=0` |
| `P7` QMP `gpu0` Screendump (`docs/p7_virtio_gpu_scanout.png`) | `1280x800`, `998,700` non-bg pixels, `16` distinct colors |

---

## 3. Host Hardware, Kernel Driver UND & Firmware/PRIME Audit (`H3` + M4)

Measurements captured read-only from the Lenovo `82K1` host via [`audit_gpu_stacks.py`](../experiments/h3-gpu-compat/audit_gpu_stacks.py), [`hw_inventory_lenovo_15ihu6.py`](../tools/hw_inventory_lenovo_15ihu6.py), [`gpu_fw_validators.py`](../tools/gpu_fw_validators.py), and [`provision_local_firmware.py`](../tools/provision_local_firmware.py):

### 3.1 Detected Host Platform, PCI GPUs & IOMMU State
- **Host Platform (`lenovo_15ihu6_inventory.json`):** Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`, BIOS `H4CN22WW(V1.07)`), Core i5-11300H (`4C/8T`, `39b` phys / `48b` virt), `32 GiB` RAM, `8250.nr_uarts=0`, Secure Boot `disabled`, `24` PCI functions (`0000:*` + VMD `10000:*`).
- **Intel Iris Xe (`0000:00:02.0`, `8086:9a49`, `0x030000`, `i915`, IRQ `153`):** BAR0 `16 MiB` `>4G`, BAR2 `256 MiB` prefetchable `>4G`, BAR4 `64 B` I/O; drives `eDP-1` `AUO 0xb69b` (`1920x1080@165.01Hz`, EDID `1.4` `128 B` verified).
- **NVIDIA RTX 3050 Ti Mobile (`0000:01:00.0`, `10de:25a0`, `0x030200` muxless, `nvidia`, IRQ `130`):** BAR0 `16 MiB`, BAR1 `4 GiB` `>4G` prefetchable, BAR3 `32 MiB` `>4G` prefetchable, BAR5 `128 B` I/O, `d3cold_allowed=1` (`D0` active on host).
- **VT-d / DMAR:** `/sys/firmware/acpi/tables/DMAR` exists (`present_root_only`); host Linux `iommu_groups` is empty because host cmdline omits `intel_iommu=on`.

### 3.2 Linux `6.18.62-1-lts` Undefined Symbol (`UND`) & Firmware Audit

| Kernel Module / Stack | Compressed / ELF Size | Undefined Symbols (`nm -u`) | Largest Dependency Buckets | Required Binary Firmware |
|---|---|---|---|---|
| `xe.ko.zst` | `1.51 MB` / `8.56 MB` | `1,213` | `drm_kms_gem_ttm` (`592`), `other` (`451`), `pci_dma_iommu_acpi` (`77`), `sync_wq_rcu_irq` (`50`), `dma_buf_resv_fence` (`33`) | `tgl_guc_70.1.1.bin.zst` (`102,651 B`, `15` `tgl_*` blobs) |
| `i915.ko.zst` | `1.84 MB` / `10.63 MB` | `1,244` | `other` (`550`), `drm_kms_gem_ttm` (`509`), `pci_dma_iommu_acpi` (`65`), `sync_wq_rcu_irq` (`61`), `dma_buf_resv_fence` (`40`) | Same `/lib/firmware/i915/tgl_*` |
| `nvidia*.ko.zst` (`4` modules) | `14.31 MB` / `37.90 MB` | `1,189` (`470`+`105`+`273`+`341`) | `os_*` C shim (`162` prototypes in `os-interface.h`, `22/162` in `nv_os_shim_check.c`) + `drm_kms_gem_ttm` (`151` in `nvidia-drm`) | `gsp_ga10x.bin` (`27,043,992 B`) + `ucodes_ga10x.bin` (`58,215,936 B`) |

### 3.3 M4 Validators & Hybrid PRIME Zero-Copy Fence Benchmark
- **`prime_fence_probe.c`:** Full `1280x800x4` (`4,096,000 B`) CPU copy (`48` iters): `262,582 ns` p50 vs `2 x 2 MiB` `USER_RO` `FrameCap` + `IoRing` atomic `dma_fence`: `18 ns` p50 (**`14,587.9x` faster**).
- **`gpu_fw_validators.py --self-test` (`PASS`):** Validated host `tgl_dmc_ver2_12.bin.zst` (`2.12`, `19,760 B`), `tgl_guc_70.1.1.bin.zst` (`70.1.1`, `277,440 B`, `2048-bit` RSA), synthetic OpRegion `$VBT`, host `eDP-1` EDID (`AUO 0xb69b`, `1920x1080@165.01Hz`), Gen12 `preserve_active_gop` BAR2 preservation, host `gsp_ga10x.bin` (`ELF64 RISC-V 0xf3`, `.fwimage=26,992,640 B`, `615.78.08`) + `ucodes_ga10x.bin` (`NVUCODES`) + `128 KiB`-aligned WPR2 metadata, `22/162` `os-interface.h` coverage, and muxless PRIME `READ_ONLY_EXPORT` (`-EPERM` on write, `-EREVOKED` on stale gen/epoch).
- **`provision_local_firmware.py --self-test` (`PASS`):** Confirmed zero default writes, `5` forbidden repo/system paths rejected, and SHA-256 manifest staging in temp dir.

---

## 4. Capability Classification Matrix (9 Subsystems x 4 GPU Paths)

| # | Capability / Subsystem | VirtIO-GPU (`1AF4:1050`) / Bochs (`1234:1111`) | Intel Iris Xe (`8086:9a49`) | NVIDIA Open (`10de:25a0`) | Hybrid PRIME (`dGPU -> iGPU`) |
|---|---|---|---|---|---|
| 1 | PCI / BAR Discovery | **PASS** (`p7a`/`p12a` `SYS_DEV_MAP`, `p8a` `BAR64 >4G`) | **VALIDATED (Read-Only + GOP Guard)** (`preserve_active_gop` active) | **PASS (BAR64 mech)** (`p8a`) / **READY (Host Inv)** | **PASS (Model)** (`03:02` muxless contract) |
| 2 | Safe MMIO Mapping (`USER_MMIO_UC`) | **PASS** (`p7a`/`p12a`, `dma_denied=3..5`) | **BLOCKED (Physical Modeset)** (no Ring-3 `xe` island) | **BLOCKED (Driver)** (`140` `os_*` funcs remaining) | **PASS** (`USER_RO` `READ_ONLY_EXPORT` model verified) |
| 3 | IRQ / MSI / MSI-X Delivery | **PASS** (`p8a` `MSI` `vec=0x40`, `32` IRQs) | **BLOCKED** (requires `i915`/`xe` GTT/PPGTT stack) | **BLOCKED** (requires physical NVIDIA GSP/KMS) | **PASS** (`IoRing` atomic `dma_fence` `18 ns` p50) |
| 4 | Firmware Validation / Loading | **PASS** (none required for 2D scanout) | **VALIDATOR PASS** (`dmc 2.12`, `guc 70.1.1`) / **LOAD BLOCKED** | **VALIDATOR PASS** (`gsp_ga10x` ELF64/WPR2) / **LOAD BLOCKED** | **N/A** |
| 5 | Memory Mgmt (`GEM`/`TTM`/`DMA`) | **PASS** (`DMA_ALLOC_FLAG_2M`) | **BLOCKED** (`drm_kms_gem_ttm`: `592`/`509` UND) | **BLOCKED** (`TTM` + `4 GiB` VRAM allocator absent) | **PASS** (`2 MiB` RO `FrameCap` + `W^X` enforcement) |
| 6 | Modesetting / 2D Scanout | **PASS** (`p7`/`p12` PNGs verified) | **GOP FALLBACK READY** / **NATIVE KMS BLOCKED** | **BLOCKED** (`nvidia-modeset.ko` `105` UND absent) | **PASS** (`dry-display` RO `back_caps` + GOP coexist) |
| 7 | Command Submission | **PASS** (`controlq` / `bochs` dirty flush) | **BLOCKED** (`GuC` CTB / `xe_exec_queue` absent) | **BLOCKED** (`GSP` RPC pushbuffer absent) | **NOT RUN (Physical)** |
| 8 | 3D / Compute Acceleration | **NOT RUN** (`Virgl`/`Venus` out of 2D scope) | **NOT RUN** | **NOT RUN** (`nvidia-uvm.ko` `341` UND) | **NOT RUN** |
| 9 | Security Boundary (`VT-d` IOMMU) | **PASS** (`p8a` `VT-d` SLPT + `FRCD` + `BME=0` scrub) | **QEMU PASS / GATE 6 READY FOR USER PHYSICAL EXECUTION** | **QEMU PASS / GATE 7 READY FOR USER PHYSICAL EXECUTION** | **PASS (Contract)** (`-EPERM` RW, `-EREVOKED` stale) |

---

## 5. QEMU/Validator PASS vs. Bare-Metal Physical Gates (Gates 5-7)

- **QEMU & Offline Validators (`PASS`):** `Milestone 2C-2` (`--p8-only`: `VT-d` DMAR `SLPT`, `MSI/MSI-X`, `>4 GiB` `WC`/`UC` BAR, `leaked_frames=0`), `--p7-only` (`VirtIO-GPU` + GOP fallback), `--p12-only` (`v6.18` `bochs.c`), `build_live_usb_image.py --verify-boot`, `hw_inventory_lenovo_15ihu6.py`, `gpu_fw_validators.py --self-test`, and `provision_local_firmware.py --self-test`.
- **Gate 5 (Physical Live USB Boot on Lenovo `82K1` via xHCI `00:14.0`, `8250.nr_uarts=0`, GOP Desktop):** **`READY FOR USER PHYSICAL EXECUTION`** (not executed by agent).
- **Gate 6 (Physical `VT-d` `DMAR` + `00:02.0` `8086:9a49` `preserve_active_gop` Verification):** **`READY FOR USER PHYSICAL EXECUTION`** (not executed by agent).
- **Gate 7 (Physical Hybrid Optimus `01:00.0` `10de:25a0` `RTD3 D3cold` / GSP WPR2 / PRIME Check):** **`READY FOR USER PHYSICAL EXECUTION`** (native `i915`/`xe`/OpenRM modeset remains `BLOCKED` pending GEM/TTM + firmware loader + GSP RPC).
