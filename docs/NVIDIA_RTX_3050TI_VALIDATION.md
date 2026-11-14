# DRY OS — NVIDIA RTX 3050 Ti Mobile (`10de:25a0`) Compatibility & OpenRM Shim Validation Report

> **Milestone:** Project Ascension 4.0 Track D (`A4-4`) | **Date:** 2026-11-14 | **Scope:** Strictly Read-Only Host Audit + Real `os-interface.h` Freestanding Compile  
> **Raw Evidence:** [`tools/compat_bench/results_target_gpus_a4.json`](../tools/compat_bench/results_target_gpus_a4.json), [`drivers/lx-nv-open-shim/nv_os_shim_check.c`](../drivers/lx-nv-open-shim/nv_os_shim_check.c)

---

## 1. Epistemic Honesty Statement

Host device inspection and OpenRM header compilation were performed **strictly read-only** (`0` sysfs writes, `0` `nvidia` driver unbinds, `0` bare-metal MMIO accesses). Live hardware modesetting or GSP boot on physical `10de:25a0` is **not implemented**; this document establishes the exact PCIe/firmware constraints and compiles DRY OS's first prototype `os-interface.h` shim cleanly against live host NVIDIA OpenRM headers.

---

## 2. Observed Host Hardware, BAR Layout & GSP Firmware (`0000:01:00.0`)

Captured via [`tools/lx_compat/validate_target_gpus.py`](../tools/lx_compat/validate_target_gpus.py):

| Attribute | Observed Host Value |
| :--- | :--- |
| PCI BDF / IDs | `0000:01:00.0`, `vendor = 0x10de`, `device = 0x25a0` (GA107M [GeForce RTX 3050 Ti Mobile]) |
| Class / Revision / Subsystem | `class = 0x30200` (**3D controller / muxless Optimus dGPU**), `revision = 0xa1`, `subvendor = 0x17aa`, `subdevice = 0x3a5b` |
| Active Host Driver | `nvidia` |
| Catalog Match (`COMPAT_CATALOG.json`) | `nvidia_open_ga107` (`status = shim_compile_only`) |
| **BAR0** (MMIO Register Aperture) | `0x54000000 .. 0x54ffffff` (`16.0 MiB`, `flags = 0x40200`, `above_4g = false`) |
| **BAR1** (VRAM Aperture) | `0x6000000000 .. 0x60ffffffff` (**`4,096.0 MiB` [`4 GiB`]**, `flags = 0x14220c`, `prefetchable = true`, **`above_4g = true`**) |
| **BAR3** (Indirect/Auxiliary Memory) | `0x6100000000 .. 0x6101ffffff` (`32.0 MiB`, `flags = 0x14220c`, `prefetchable = true`, **`above_4g = true`**) |
| **BAR5** (I/O Window) | `0x4000 .. 0x407f` (`128 B`, `flags = 0x40101`, `is_io = true`) |
| Host GSP Firmware (`615.78.08`) | `gsp_ga10x.bin` (**`27,043,992 B`**), `ucodes_ga10x.bin` (**`58,215,936 B`**), plus `8` `/lib/firmware/nvidia/ga107/gsp/*.bin.zst` images (`535.113.01` / `570.144`) |

---

## 3. Verified Freestanding `os-interface.h` Compatibility Shim

[`drivers/lx-nv-open-shim/nv_os_shim_check.c`](../drivers/lx-nv-open-shim/nv_os_shim_check.c) (`173` lines, MIT) was compiled cleanly with `clang -std=gnu11 -target x86_64-unknown-none-elf -ffreestanding -fno-builtin -mno-red-zone -mno-sse -mno-sse2 -mno-avx -Wall -Wextra -Werror`:

1. **Standalone Freestanding Build (`fallback_build_ok = true`):** Validates baseline `NV_STATUS`/`NvU*` shim signatures with `0` host dependencies.
2. **Live NVIDIA OpenRM Header Build (`nvidia_header_build_ok = true`):** Compiled directly with `-DHAVE_NVIDIA_OS_INTERFACE_H=1` against **`/usr/src/nvidia-615.78.08/kernel-open/common/inc/os-interface.h`**, catching and resolving a real signature constraint (`os_release_semaphore` returns `NV_STATUS`, not `void`). Verified stubs cover `os_alloc_mem`, `os_free_mem`, `os_get_monotonic_time_ns`, `os_delay`, `os_delay_us`, `os_mem_copy/set/cmp`, `os_pci_read_{byte,word,dword}`, `os_map/unmap_kernel_space`, spinlock/mutex/semaphore primitives, and `os_get_num_phys_pages`.

---

## 4. Hybrid Optimus Display Topology & Remaining OpenRM Gaps

- **Muxless 3D Controller (`0x030200`) & PRIME Integration:** Physical display panels on this laptop are driven via the Intel Iris Xe iGPU (`0000:00:02.0`). Rendered buffers from `10de:25a0` must cross to the compositor via **PRIME DMA-BUF sharing**, matching the read-only `Order2MiB` `FrameCap` (`RIGHT_READ | RIGHT_MAP`, no write right) architecture proven in `bochs.c` (`A4-3`) and `prime_fence_probe.c` (`18 ns` vs `262,582 ns`).
- **64-Bit BAR1 Scale:** BAR1 alone spans `4 GiB` (`0x6000000000 .. 0x60ffffffff`). Windowed `SYS_DEV_MAP` (`dev_map_window`) prevents needing eager full-aperture `4 GiB` page-table preallocation in Ring-3 islands, though `SLPT` scaling for large VRAM apertures remains a `P5` focus.
- **Honest Gaps Blocking Full OpenRM Bring-up:**
  1. Implementing the remaining ~145 functions in `os-interface.h` (`os_alloc_pages_node`, `os_lock_user_pages`, timers, workqueues) backed by DRY `Notification`/`IoRing` primitives.
  2. Loading and authenticating **`27.04 MiB` `gsp_ga10x.bin`** + **`58.22 MiB` `ucodes_ga10x.bin`** into coherent DMA/WPR2 allocations to boot the GA107 RISC-V GSP processor.
  3. Integrating NVIDIA's `libspdm` / GSP-RM RPC command queue (`nvidia-modeset`, `nvidia-drm`) or `P5` `dry-vmm` VFIO passthrough with `RTD3 D3cold` power-down (`RISK_REGISTER.md` `R-17`).
