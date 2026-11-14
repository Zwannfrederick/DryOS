# DRY OS Priority C — GPU Compatibility, VirtIO-GPU 2D Scanout & Iris Xe / NVIDIA / PRIME Audit

## 1. Executive Summary

Priority C advances DRY OS from a single UEFI GOP compositor (`dry-display`) to a capability-isolated Ring-3 **VirtIO-GPU 1.0 (`1AF4:1050`) 2D Scanout Driver (`dry-gpu.elf`, Compartment `0x0009`)** with automatic crash-resilient **UEFI GOP fallback**, paired with a read-only empirical audit of the host's hybrid **Intel Iris Xe (`8086:9a49`)** + **NVIDIA RTX 3050 Ti Mobile (`10de:25a0`)** GPU stack and a zero-copy **PRIME Frame Capability + `IoRing` Fence** prototype (`experiments/h3-gpu-compat/`).

- **Verified hardware emulation (`--gpu-only`):** [`p7a_virtio_gpu_2d_scanout_and_gop_fallback`](../tools/qemu_p7_gpu_validators.py) boots QEMU/KVM (`2 vCPUs`, `-device virtio-gpu-pci,id=gpu0,disable-legacy=on,xres=1280,yres=800`), renders the full `1280x800` desktop via `dry-gpu.elf` (`B8G8R8X8_UNORM`), injects `#UD` (`vec=6`) via `FaultEndpoint`, verifies `[DRY-WM] GPU_FALLBACK_TO_GOP` on the live `dry-display` compositor, respawns `gen=2`, captures a verified QMP screendump of `gpu0` ([`docs/p7_virtio_gpu_scanout.png`](./p7_virtio_gpu_scanout.png)), and tears down cleanly (`leaked_frames=0`).
- **Bare-metal iGPU/dGPU reality check (`H3` audit):** Running unmodified Linux `xe.ko` (`1,213` UND symbols), `i915.ko` (`1,244` UND symbols), or NVIDIA's `open` stack (`1,189` UND symbols + `162` `os_*` OS-interface APIs + `81.3 MiB` `gsp_ga10x.bin`/`ucodes_ga10x.bin`) directly inside native Ring-3 shims remains **blocked on bare-metal** by missing **DRM GEM/TTM/dma-buf** infrastructure, binary firmware loaders (`tgl_guc`, `gsp_ga10x`), and unverified physical silicon testing — while the three kernel hardware prerequisites identified below (**hardware `VT-d` DMAR IOMMU enforcement**, **APIC `MSI`/`MSI-X` routing**, and **64-bit `>4 GiB` `WC` BAR mapping**) are now **implemented & QEMU-verified in `Milestone 2C-2` (`p8a_hw_foundations_vtd_msi_bar64_e2e`)**.

---

## 2. VirtIO-GPU 2D Scanout Architecture & Measured Telemetry (`P7`)

```
dry-display (CPL=3, 0x0101)
  ├── scene back_caps [2 x 2 MiB WB RAM] ──grant(RIGHT_READ|RIGHT_MAP)──▶ dry-gpu (CPL=3, 0x0009)
  ├── GPU_OP_PRESENT_RECT (bbox + cursor + flags) ────────────────────────▶   ├── VSPACE_MAP_OP_2M_RO (0x2600_0000)
  └── on EREVOKED / EBADCAP ──▶ [DRY-WM] GPU_FALLBACK_TO_GOP ──▶ UEFI GOP    ├── 2x2 MiB DEVICE_DMA scanout + 12x16 cursor
                                                                              ├── TRANSFER_TO_HOST_2D + RESOURCE_FLUSH
dry-init Supervisor ◀── FaultEndpoint (vec=6 #UD) ── kernel BME=0 quiesce     └── controlq via SYS_DEV_MAP (1AF4:1050)
```

| Metric ([`tools/compat_bench/results_p7_gpu.json`](../tools/compat_bench/results_p7_gpu.json)) | Verified Value |
|---|---|
| PCI Device / Display Mode | `1af4:1050` / `1280x800` `B8G8R8X8_UNORM` |
| Driver Generations Verified | `2` (`gen=1` -> `#UD` `vec=6` -> `gen=2`) |
| Scanout Presents (`gen=1` / `gen=2`) | `9` / `3` (`22,080 KiB` transferred, `22` `controlq` cmds) |
| Live UEFI GOP Fallback | `true` (`[DRY-WM] GPU_FALLBACK_TO_GOP reason=ep_revoked`) |
| Unauthorized Rejection (`kernel dma_denied` / `IPC unauth`) | `3` / `7` |
| Kernel `PCI Command.BME=0` Quiesces | `3` |
| Frame Leaks (`leaked_frames`) | `0` |
| End-to-End `dry-gpu` Present Latency (`p50` / `p95` / `p99` @ `3115 MHz`) | `3,416,216` (`1096.70 us`) / `4,184,298` / `4,184,298` cyc (`1343.27 us`) |
| Crash-to-Live-Scanout Recovery Time | `27,184,357` cyc (`8.727 ms`) |
| `dry-gpu` Resident Memory (`mapped_pages_4k + pt_frames_4k`) | `2,069` pages (`8.08 MiB`, includes `2x2M` DMA + `2x2M` RO backbuf) |
| QMP `gpu0` Screendump (`docs/p7_virtio_gpu_scanout.png`) | `1280x800`, `998,700` non-bg pixels, `16` distinct colors |

---

## 3. Host Hardware & Kernel Driver Dependency Audit (`H3`)

All measurements below were captured read-only from the host via [`experiments/h3-gpu-compat/audit_gpu_stacks.py`](../experiments/h3-gpu-compat/audit_gpu_stacks.py) into [`experiments/h3-gpu-compat/results_h3_gpu.json`](../experiments/h3-gpu-compat/results_h3_gpu.json).

### 3.1 Detected PCI GPUs & IOMMU State
- **Intel Iris Xe (`0000:00:02.0`, `0x8086:0x9a49`, class `0x030000`, host `i915`, IRQ `153`):** BAR0 `16 MiB` MMIO, BAR2 `256 MiB` prefetchable MMIO, BAR4 `64 B` I/O.
- **NVIDIA RTX 3050 Ti Mobile (`0000:01:00.0`, `0x10de:0x25a0`, class `0x030200`, host `nvidia`, IRQ `130`):** BAR0 `16 MiB` MMIO, BAR1 `4 GiB` (`4,294,967,296 B`) prefetchable MMIO, BAR3 `32 MiB` prefetchable MMIO, BAR5 `128 B` I/O.
- **VT-d / DMAR:** `/sys/firmware/acpi/tables/DMAR` exists (`present_root_only`), while `/sys/class/iommu` and `/sys/kernel/iommu_groups` are currently empty (`vtd_active_in_host_linux = false`; `iommu` boot parameter not enabled in host Linux cmdline).

### 3.2 Linux `6.18.62-1-lts` Undefined Symbol (`UND`) & Firmware Audit

| Kernel Module / Stack | Compressed / ELF Size | Undefined Symbols (`nm -u`) | Largest Dependency Buckets | Required Binary Firmware |
|---|---|---|---|---|
| `xe.ko.zst` | `1.51 MB` / `8.56 MB` | `1,213` | `drm_kms_gem_ttm` (`592`), `other` (`451`), `pci_dma_iommu_acpi` (`77`), `sync_wq_rcu_irq` (`50`), `dma_buf_resv_fence` (`33`) | `tgl_guc_70.1.1.bin.zst` (`102,651 B`, `15` `tgl_*` blobs) |
| `i915.ko.zst` | `1.84 MB` / `10.63 MB` | `1,244` | `other` (`550`), `drm_kms_gem_ttm` (`509`), `pci_dma_iommu_acpi` (`65`), `sync_wq_rcu_irq` (`61`), `dma_buf_resv_fence` (`40`) | Same `/lib/firmware/i915/tgl_*` |
| `nvidia*.ko.zst` (`4` modules) | `14.31 MB` / `37.90 MB` | `1,189` (`470`+`105`+`273`+`341`) | `os_*` C shim (`162` prototypes in `os-interface.h`, `3,147` LOC in `os-interface.c`) + `drm_kms_gem_ttm` (`151` in `nvidia-drm`) | `gsp_ga10x.bin` (`27,043,992 B`) + `ucodes_ga10x.bin` (`58,215,936 B`) |

### 3.3 Hybrid PRIME Zero-Copy Fence Benchmark (`prime_fence_probe.c`)
- Full `1280x800x4` (`4,096,000 B`) CPU copy (`48` iterations): **`262,582 ns`** median (`353,297 ns` p95).
- DRY OS `2 x 2 MiB` `USER_RO` Frame Capability + lockless `IoRing` atomic `dma_fence`: **`18 ns`** median (`19 ns` p95) — **`14,587.9x` faster** (`stale_gen_rejected = true`, `checksum_ok = true`).

---

## 4. Capability Classification Matrix (9 Subsystems x 4 GPU Paths)

| # | Capability / Subsystem | VirtIO-GPU (`1AF4:1050`) | Intel Iris Xe (`8086:9a49`) | NVIDIA Open (`10de:25a0`) | Hybrid PRIME (`dGPU -> iGPU`) |
|---|---|---|---|---|---|
| 1 | PCI / BAR Discovery | **PASS** (`p7a` `SYS_DEV_MAP`, `p8a` `BAR64 > 4 GiB` `WC`/`UC`) | **BLOCKED** (`256 MiB` aperture; only read-only sysfs audited) | **PASS (BAR64 mechanism)** (`p8a` `ivshmem` `>4 GiB` BAR `WC`/`UC` verified); **BLOCKED (driver)** | **NOT RUN** (requires both physical GPUs mapped) |
| 2 | Safe MMIO Mapping (`USER_MMIO_UC`) | **PASS** (`p7a`, `dma_denied=3`) | **NOT RUN** (no Ring-3 `i915`/`xe` island yet) | **NOT RUN** (`162` `os_*` functions unimplemented) | **PASS** (userspace `USER_RO` frame cap model verified in `p7a`/`h3`) |
| 3 | IRQ / MSI / MSI-X Delivery | **PASS** (`p8a` `MSI` `vec=0x40`, `32` IRQs via bound notif; `MSI-X` table `0x11` in `msi.rs`) | **BLOCKED** (requires `i915`/`xe` GTT/PPGTT driver stack) | **BLOCKED** (requires physical NVIDIA `GSP`/KMS stack) | **PASS** (atomic `IoRing` notify/fence validated in `p6a`/`h3`) |
| 4 | Firmware Loading (`GuC` / `GSP`) | **PASS** (none required for `2D` scanout) | **BLOCKED** (`tgl_guc_70.1.1` loader & HuC auth absent) | **BLOCKED** (`81.3 MiB` `gsp_ga10x` + `ucodes` RPC absent) | **NOT RUN** |
| 5 | Memory Mgmt (`GEM` / `TTM` / `DEVICE_DMA`) | **PASS** (`DMA_ALLOC_FLAG_2M` `2x2 MiB`) | **BLOCKED** (`drm_kms_gem_ttm`: `592`/`509` UND symbols) | **BLOCKED** (`TTM` + `12 MiB..4 GiB` VRAM allocator absent) | **PASS** (`2x2 MiB` RO `Frame` cap + `18 ns` fence vs `262.6 us` copy) |
| 6 | Modesetting / 2D Scanout | **PASS** (`1280x800` `p7_virtio_gpu_scanout.png`) | **BLOCKED** (`DRM` KMS CRTC/connector pipeline absent) | **BLOCKED** (`nvidia-modeset.ko` `105` UND symbols absent) | **PASS** (`dry-display` RO `back_caps` + GOP fallback live) |
| 7 | Command Submission | **PASS** (`22` `controlq` DMA cmds verified) | **BLOCKED** (`GuC` CTB submission / `xe_exec_queue` absent) | **BLOCKED** (`GSP` channel pushbuffer + host1x/FIFO absent) | **NOT RUN** |
| 8 | 3D / Compute Acceleration | **NOT RUN** (`Virgl` / `Venus` protocol out of 2D scope) | **NOT RUN** | **NOT RUN** (`nvidia-uvm.ko` `341` UND symbols) | **NOT RUN** |
| 9 | Security Boundary (`VT-d` IOMMU DMA) | **PASS** (`p8a` `VT-d` SLPT + `FRCD` fault + `BME=0`/reset/scrub quiesce) | **BLOCKED (Physical Silicon)** (kernel `VT-d` SLPT ready; bare-metal `8086:9a49` untested) | **BLOCKED (Physical Silicon)** (kernel `VT-d` SLPT ready; bare-metal `4 GiB` dGPU untested) | **PASS** (`USER_RO` + generation-bound lease model; `14,587.9x` speedup in `h3`) |

---

## 5. Security Boundary (`VT-d` / `DMAR`) & Engineering Roadmap

Without hardware **`VT-d` (`DMAR`) DMA remapping**, granting physical MMIO BARs of bare-metal `8086:9a49` or `10de:25a0` to any driver domain — even Ring-3 — would allow a compromised or buggy GPU command streamer / GSP firmware to DMA-write arbitrary physical RAM (`dry-kernel` or `DRY Vault`), because `PCI Command.BME=0` quiesce only acts synchronously on crash *after* DMA already ran. Therefore:
1. **Completed Hardware Prerequisites (`Milestone 2C-2` — `DONE`):** Kernel ACPI `DMAR` parser + `VT-d` per-device PCI `BDF` `SLPT` page tables (`FRCD` rogue DMA containment `vtd_faults=1`), APIC **MSI / MSI-X** interrupt delivery (`0x40..=0x5F`, `IRQF_ONESHOT`), `64`-bit `>4 GiB` `WC`/`UC` PCI BAR window mapping (`p8a_hw_foundations_vtd_msi_bar64_e2e`, `95/95` QEMU + `5/5` matrix, `leaked_frames=0`).
2. **Next Desktop & Graphics Track (`Project Ascension 3.0 Track B` + `2C-3`):** Multi-resolution `dry-display` + `dry-gpu` `DRY Glass` desktop compositor (`1024x768`, `1280x800`, `1920x1080`), followed by `VIRTIO_GPU_F_VIRGL` (`3D` command submission) and `PRIME` `Frame` capability swapchains.
3. **Physical Hardware Path (`Faz 4 / P5`):** Bare-metal validation of the `VT-d`/`MSI-X` stack and minimal `Xe` KMS/scanout shim targeting `GuC` submission first (`1,213` symbols vs `nvidia`'s proprietary binary + `81.3 MiB` GSP blob).
