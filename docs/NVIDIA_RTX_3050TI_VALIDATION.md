# DRY OS — NVIDIA RTX 3050 Ti Mobile (`10de:25a0`) Compatibility & OpenRM Validation Report

> **Milestones:** Project Ascension 4.0 Track D (`A4-4`, `2026-11-14`) -> Project Ascension 5.0 Track C / M4 (`2026-11-16`)  
> **Scope:** Strictly Read-Only Host Audit + Real `os-interface.h` Compile & Coverage Matrix + `gsp_ga10x.bin` ELF64/WPR2 & PRIME Validators  
> **Raw Evidence:** [`tools/compat_bench/results_target_gpus_a4.json`](../tools/compat_bench/results_target_gpus_a4.json), [`tools/compat_bench/lenovo_15ihu6_inventory.json`](../tools/compat_bench/lenovo_15ihu6_inventory.json), [`drivers/lx-nv-open-shim/nv_os_shim_check.c`](../drivers/lx-nv-open-shim/nv_os_shim_check.c), [`tools/gpu_fw_validators.py`](../tools/gpu_fw_validators.py)

---

## 1. Epistemic Honesty Statement

Host device inspection, OpenRM header compilation, and GSP firmware structural checks were performed **strictly read-only** (`0` sysfs writes, `0` `/dev/*` opens, `0` `nvidia` driver unbinds, `0` bare-metal MMIO accesses, `0` `efivars`/`efibootmgr` touches). Live hardware modesetting or GSP boot on physical `10de:25a0` is **not implemented**; this document establishes the exact PCIe/WPR2/PRIME constraints, audits `os-interface.h` coverage, and cleanly separates verified QEMU/host validators from bare-metal **Gates 5-7 (`READY FOR USER PHYSICAL EXECUTION`)**.

---

## 2. Observed Host Hardware, Power, BAR Layout & GSP Firmware (`0000:01:00.0`)

Captured via [`tools/lx_compat/validate_target_gpus.py`](../tools/lx_compat/validate_target_gpus.py) and [`tools/hw_inventory_lenovo_15ihu6.py`](../tools/hw_inventory_lenovo_15ihu6.py):

| Attribute | Observed Host Value |
| :--- | :--- |
| PCI BDF / IDs | `0000:01:00.0`, `vendor = 0x10de`, `device = 0x25a0` (GA107M [GeForce RTX 3050 Ti Mobile]) |
| Class / Revision / Subsystem | `class = 0x30200` (**3D controller / muxless Optimus dGPU**), `revision = 0xa1`, `subvendor = 0x17aa`, `subdevice = 0x3a5b` |
| Active Host Driver & Link | `nvidia` (`irq = 130`), current PCIe `2.5 GT/s x4` (max `16.0 GT/s x16`), `power_state = D0`, `d3cold_allowed = 1` |
| Catalog Match (`COMPAT_CATALOG.json`) | `nvidia_open_ga107` (`status = shim_compile_only`) |
| **BAR0** (MMIO Register Aperture) | `0x54000000 .. 0x54ffffff` (`16.0 MiB`, `flags = 0x40200`, `above_4g = false`) |
| **BAR1** (VRAM Aperture) | `0x6000000000 .. 0x60ffffffff` (**`4,096.0 MiB` [`4 GiB`]**, `flags = 0x14220c`, `prefetchable = true`, **`above_4g = true`**) |
| **BAR3** (Indirect/Auxiliary Memory) | `0x6100000000 .. 0x6101ffffff` (`32.0 MiB`, `flags = 0x14220c`, `prefetchable = true`, **`above_4g = true`**) |
| **BAR5** (I/O Window) | `0x4000 .. 0x407f` (`128 B`, `flags = 0x40101`, `is_io = true`) |
| Host GSP Firmware (`615.78.08`) | `gsp_ga10x.bin` (**`27,043,992 B`**), `ucodes_ga10x.bin` (**`58,215,936 B`**), plus `8` `/lib/firmware/nvidia/ga107/gsp/*.bin.zst` images (`535.113.01` / `570.144`) |

### Local Read-Only Firmware SHA-256 Hashes (`tools/provision_local_firmware.py`)
Redistributable-restricted NVIDIA firmware blobs are **never** committed to git; [`tools/provision_local_firmware.py`](../tools/provision_local_firmware.py) only stages into an explicit user `--staging-dir` outside git-tracked directories:
- `gsp_ga10x.bin` (`27,043,992 B`): `sha256=a20100cafd5a85e22b3e13188339730c6ced94bd5f654984dd2ed274ed58d584`
- `ucodes_ga10x.bin` (`58,215,936 B`): `sha256=eed53d76248582d71c629e8b6230aee48ce673c91d782301c94e98ac6a6db7aa`

---

## 3. Verified Freestanding `os-interface.h` Shim & Coverage Audit

[`drivers/lx-nv-open-shim/nv_os_shim_check.c`](../drivers/lx-nv-open-shim/nv_os_shim_check.c) (`173` lines, MIT) was compiled cleanly in Ascension 4.0 with `clang -std=gnu11 -target x86_64-unknown-none-elf -ffreestanding -fno-builtin -mno-red-zone -mno-sse -mno-sse2 -mno-avx -Wall -Wextra -Werror`:
1. **Standalone Freestanding Build (`fallback_build_ok = true`):** Validates baseline `NV_STATUS`/`NvU*` shim signatures with `0` host dependencies.
2. **Live NVIDIA OpenRM Header Build (`nvidia_header_build_ok = true`):** Compiled directly with `-DHAVE_NVIDIA_OS_INTERFACE_H=1` against `/usr/src/nvidia-615.78.08/kernel-open/common/inc/os-interface.h`, catching `os_release_semaphore` returning `NV_STATUS`.

### Ascension 5.0 `os-interface.h` Coverage Matrix (`22 / 162` = `13.58%`)
Audited programmatically by `audit_nvidia_os_interface_coverage()` in [`tools/gpu_fw_validators.py`](../tools/gpu_fw_validators.py):

| Functional Bucket | Total in `os-interface.h` | Implemented in Shim | Remaining |
| :--- | :---: | :---: | :---: |
| `memory_alloc_map` (`os_alloc_mem`, `os_mem_*`, `os_map_kernel_space`, ...) | `35` | `7` | `28` |
| `locks_sync_wq` (spinlocks, semaphores, mutexes, rwlocks, waitqueues) | `30` | `8` | `22` |
| `pci_io_dma` (`os_pci_read_{byte,word,dword}`, PCI write/FLR, IOMMU) | `22` | `3` | `19` |
| `time_delay` (`os_get_monotonic_time_ns`, `os_delay`, `os_delay_us`) | `5` | `3` | `2` |
| `string_dbg` (`os_snprintf`, `os_vsnprintf`, `os_log_error`, `os_dbg_*`) | `11` | `0` | `11` |
| `vfs_file` (`os_open_readonly_file`, `os_read_file`, ...) | `7` | `0` | `7` |
| `platform_cgroup_other` (`os_get_is_openrm` + cgroup/vGPU/Tegra/ACPI) | `52` | `1` | `51` |
| **Total (`615.78.08`)** | **`162`** | **`22`** | **`140`** |

---

## 4. Ascension 5.0 GSP ELF64/WPR2 & Muxless PRIME Validator Results

Executed `python3 tools/gpu_fw_validators.py --self-test` (`PASS`):
- **GSP ELF64 & WPR2 Layout Validator (`validate_gsp_elf64_and_wpr2`):**
  - Host `/lib/firmware/nvidia/615.78.08/gsp_ga10x.bin`: verified `ELF64 LE` relocatable RISC-V (`e_machine = 0xf3`), `19` sections, `.fwversion = "615.78.08"`, `.fwimage = 26,992,640 B`, `.note.gnu.build-id = 36 B`, and `.fwsignature_ga10x = 4,096 B`.
  - Host `/lib/firmware/nvidia/615.78.08/ucodes_ga10x.bin`: verified `NVUCODES` header (`version = 1`, `flags = 0x50d`, `58,215,936 B`).
  - `GspFwWprMeta` (`256 B`, `magic = 0xdc3aae21371a60b3`, `revision = 1`, `verified = 0xa0a0a0a0a0a0a0a0`) layout verified against `4 GiB` BAR1 VRAM bounds with `128 KiB`-aligned `wprStart`/`wprEnd`/`frtsOffset` (`1 MiB` FRTS), plus negative rejection of unaligned `wprStart`.
- **Muxless PRIME `READ_ONLY_EXPORT` + `dma_fence` Handoff (`validate_muxless_prime_handoff`):**
  - Models `01:00.0` (`class = 0x030200`) -> `2 MiB` (`2,097,152 B`) `FrameCap` sealed via `SYS_VAULT_CTL(VAULT_CTL_SEAL_RO_EXPORT)` (`READ | MAP`, zero `WRITE`/`EXEC` bits) + `dma_fence` (`seq`, `generation`, `last_crc`) consumed by `dry-display` GOP compositor without mutating `00:02.0` BAR2.
  - Verifies `ro_handoff_rc = 0`, `WRITE` escalation rejected (`-1` `-EPERM`), stale `generation` rejected (`-110` `-EREVOKED`), and stale `GLOBAL_LEASE_EPOCH` rejected (`-110` `-EREVOKED`), matching `prime_fence_probe.c` (`18 ns` vs `262,582 ns`).

---

## 5. Honest OpenRM Gaps & QEMU vs. Bare-Metal Gates 5-7

- **64-Bit BAR1 Scale & `RTD3 D3cold`:** BAR1 spans `4 GiB` (`0x6000000000 .. 0x60ffffffff`). Windowed `SYS_DEV_MAP` (`dev_map_window`) avoids eager full-aperture preallocation, while leaving `10de:25a0` in `D0` on battery drains ~8-15 W (`RISK_REGISTER.md` `R-17`).
- **Honest Gaps Blocking Live `10de:25a0` Bring-up:**
  1. Implementing the remaining `140` `os-interface.h` APIs (notably `os_alloc_pages_node`, `os_lock_user_pages`, timers, workqueues, `os_log_error`) backed by DRY `Notification`/`IoRing` primitives.
  2. Loading and authenticating `27.04 MB` `gsp_ga10x.bin` + `58.22 MB` `ucodes_ga10x.bin` into coherent DMA/WPR2 allocations to boot the GA107 RISC-V GSP processor.
  3. Integrating NVIDIA's `libspdm` / GSP-RM RPC command queue (`nvidia-modeset`, `nvidia-drm`) or `P5` `dry-vmm` VFIO passthrough with `RTD3 D3cold` power-down.
- **Status Separation:**
  - **QEMU & Host Read-Only Validators:** **PASS** (`nv_os_shim_check.c` `-Werror` build, `gpu_fw_validators.py --self-test`, `provision_local_firmware.py --self-test`, QEMU `--p8-only` `>4 GiB BAR` + `VT-d SLPT` + `MSI`).
  - **Bare-Metal Gates 5-7 (`82K1` Physical Live USB Boot, Physical `VT-d` + GOP, Physical dGPU `RTD3`/PRIME):** **`READY FOR USER PHYSICAL EXECUTION`** (never run by agent; live OpenRM GPU modeset remains `BLOCKED` on items 1-3).
