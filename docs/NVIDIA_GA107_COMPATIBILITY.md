# DRY OS Ascension 7.0 — NVIDIA GeForce RTX 3050 Ti (`10de:25a0`, GA107M) Compatibility Study (`NVIDIA_GA107_COMPATIBILITY.md`)

**Target GPU:** NVIDIA GeForce RTX 3050 Ti Laptop GPU (Ampere GA107M, PCI ID `10de:25a0`, `0000:01:00.0`, class `0x030200` Muxless 3D Controller)  
**Host Reference Sources Audited (Read-Only):** `/usr/src/nvidia-620.70.06/kernel-open/common/inc/os-interface.h`, `/lib/firmware/nvidia/620.70.06/ucodes_ga10x.bin` (`53.53 MiB`), `drivers/lx-nv-open-shim/nv_os_shim_check.c`  
**Milestone D Status:** **`PARTIAL` (Audit & Firmware/SHIM Classification) / `UNSUPPORTED` (OpenRM Execution & CUDA)**  
**Strict Honesty Claim:** `nv_os_shim_check.c` is compile-verified only (`-Werror`); **NO NVIDIA GPU initialization, GSP boot, VRAM allocation, or CUDA compute execution exists or is claimed.**

---

## 1. GA107 Hardware Topology & Resource Requirements (`0000:01:00.0`)

Verified read-only on Lenovo `82K1` (`tools/compat_bench/results_nvidia_ga107_audit.json`):

| Resource | Address / Size | Flags / Characteristics | DRY OS Readiness |
| :--- | :--- | :--- | :--- |
| **BAR0 (MMIO Registers)** | `0x5400_0000` (`16 MiB`) | 32-bit non-prefetchable (`0x40200`) | `SYS_DEV_MAP_WINDOW` (`4K` UC) can map windows up to PCI slot limits |
| **BAR1 (VRAM Aperture)** | `0x60_0000_0000` (`4 GiB`) | 64-bit prefetchable above-4G (`0x14220c`) | Exceeds `4 GiB`; requires chunked `SYS_DEV_MAP_WINDOW_2M` (`WC`) sliding mappings + VRAM buddy allocator |
| **BAR3 (User / Doorbell)** | `0x61_0000_0000` (`32 MiB`) | 64-bit prefetchable (`0x14220c`) | Chunked `2M` window mapping capable |
| **BAR5 (Legacy I/O)** | `0x4000` (`128 B`) | I/O port range (`0x40101`) | `SYS_IOPORT_RW` capable if explicitly needed (modern OpenRM uses BAR0 MMIO) |
| **MUXless Topology** | Class `0x030200` (3D Controller) | Zero physical display connectors wired on `82K1` (eDP-1 wired solely to Intel `00:02.0`) | Kept strictly passive by kernel (`dgpu_3d_passive=1`, zero GOP clobber) |

---

## 2. Full `os-interface.h` Classification (`nvidia-620.70.06`)

`tools/nvidia_ga107_audit.py` parsed all **`171`** `NV_API_CALL` prototypes in `/usr/src/nvidia-620.70.06/kernel-open/common/inc/os-interface.h` (which encompasses the `162` non-conditional base `os_*` prototypes plus `NV_LINUX` / `NV_VMWARE` conditional & debug helpers) and cross-referenced every entry in `drivers/lx-nv-open-shim/nv_os_shim_check.c`:

| Audit Classification | Count | Examples / Evidence | Meaning on DRY OS |
| :--- | :---: | :--- | :--- |
| **`IMPLEMENTED`** | **4** | `os_mem_copy`, `os_mem_set`, `os_mem_cmp`, `os_get_is_openrm` | Real non-stub freestanding implementations tested in `nv_os_shim_check.c` |
| **`PARTIAL`** | **4** | `os_alloc_mem` (16 KiB static bump slab, no free), `os_pci_read_{byte,word,dword}` (`dry_nv_rust_pci_cfg_rw` weak hook) | Bounded/prototype helpers; unsuitable for real `nvidia.ko` GSP-RM heap |
| **`ENOSYS`** | **9** | `os_free_mem`, `os_delay`, `os_delay_us`, `os_unmap_kernel_space`, `os_alloc/free_spinlock`, `os_free/acquire/release_semaphore` | Compile-time prototypes returning explicit `-ENOSYS` / `NV_ERR_NOT_SUPPORTED` |
| **`COMPILE_ONLY`** | **5** | `os_get_monotonic_time_ns`, `os_map_kernel_space`, `os_acquire/release_spinlock`, `os_alloc_semaphore` | No-op stubs compiling against headers ONLY; non-functional at runtime |
| **`UNSUPPORTED`** | **149** | `os_lock_user_pages`, `os_alloc_pages_node`, `os_map_device_paddr_range_protected`, `os_queue_work_item`, `os_iommu_sva_bind`, `os_read_device_certificate`, `os_nv_cap_*`, `os_dmem_cgroup_*`, etc. | Absent from DRY OS shim; would require major kernel/runtime facilities |
| **Total APIs Audited** | **171** | Full JSON table: `tools/compat_bench/results_nvidia_ga107_audit.json` | **`22` shimmed (`12.9%` compile coverage, only `4` truly implemented); `149` (`87.1%`) `UNSUPPORTED`** |

### Subsystem Feasibility Breakdown

| Subsystem | Total APIs | Implemented / Partial | Realistic DRY OS Ring-3 Path? |
| :--- | :---: | :---: | :--- |
| **`memory_vm`** (`os_alloc_pages_node`, `os_lock_user_pages`, `os_map_kernel_space`, CPU cache flush) | 37 | 3 / 1 | **Partly feasible** (`SYS_DMA_CTL`, `SYS_VSPACE_MAP`, `clflush`), **high complexity** for pinned user-page lists & IOMMU scatterlists |
| **`pci_mmio_io`** (`os_pci_*`, `os_pci_trigger_flr`, `os_iommu_sva_bind`, `os_io_*`) | 20 | 0 / 3 | **Feasible** (`SYS_DEV_MAP(CFG_RW)` + FLR), except `SVA/PASID` (`UNSUPPORTED`) |
| **`sync_locking`** (`spinlock`, `mutex`, `semaphore`, `rwlock`, `os_wait_queue`, `os_schedule`) | 26 | 0 / 0 | **Feasible** natively in Ring-3 via `Notification` + futex/spinlock, not yet wired to C shim |
| **`time_clocks`** (`os_get_monotonic_time_*`, `os_delay`, `os_get_cpu_frequency`) | 6 | 0 / 0 | **Feasible** via `SYS_SYSINFO` (`tsc_mhz`) + `rdtsc` + timer `Notification` |
| **`threads_strings_irq`** (`os_snprintf`, `os_queue_work_item`, `os_is_isr`, kernel worker threads) | 15 | 0 / 0 | Strings trivial; threaded bottom-half workqueues need Ring-3 worker pool + mutexes |
| **`linux_vfs_cgroup_sys`** (`os_open_and_read_file`, `os_cgroup_*`, `os_dmem_cgroup_*`, `os_nv_cap_*`, `os_imex_*`, `os_get_acpi_rsdp_from_uefi`, `os_read_device_certificate`) | 36 | 0 / 0 | **Not realistic natively** on microkernel (Linux VFS, `cgroup2`, `libspdm` TPM/SPDM certs, sysfs `nv-caps`) |
| **`misc_runtime`** | 31 | 1 / 0 | Mixed (`os_get_is_openrm` easy; hypervisor/grid/crashlog unnecessary) |

---

## 3. GA107 GSP Firmware, VRAM, Optimus/PRIME, DMA & CUDA Gaps

1. **GSP-RM & WPR2 Architecture (`ucodes_ga10x.bin`):**
   - Unlike legacy pre-Turing GPUs, OpenRM `nvidia-open` on GA107 offloads GPU initialization, power management, clock control, and context scheduling to an internal RISC-V **GSP (GPU System Processor)** booted into hardware-protected **WPR2 (Write-Protected Region 2)** via `SEC2` / `Booter` signed microcodes and authenticated over **SPDM (`libspdm` / `os_read_device_certificate`)**.
   - Host blob `/lib/firmware/nvidia/620.70.06/ucodes_ga10x.bin` measures **`56,128,512 B` (`53.53 MiB`)** (`magic="NVUCODES"`, SHA-256 `b8f3d94d...df5d3cbf`). DRY OS Milestone A replaced the legacy `16 MiB` single-buffer limit with **`FwStreamVerifier`** (`userspace/src/fw_store.rs`), making SHA-256 verification of >50 MiB GSP blobs memory-bounded, **but WPR2 carveout coordination and GSP RPC serialization remain unimplemented.**
2. **Optimus & PRIME Offload + Synchronization:**
   - Because `10de:25a0` has zero display connectors on `82K1`, any GA107 output must target offscreen surfaces synchronized via explicit **GPU fences** (`Timeline`/semaphore sync) and imported **read-only** into the compositor / Iris Xe scanout pipeline (matching DRY OS's cross-compartment `READ_ONLY_EXPORT` / `VSPACE_MAP_OP_2M_RO` invariant).
3. **VRAM & DMA Isolation:**
   - Requires BAR1 heap allocator (4 GiB aperture mapped on demand via `WC` windows) plus VT-d `SLPT` mappings for all host sysmem command buffers (`SYS_DMA_CTL`). Physical VT-d Interrupt Remapping (`VT-d IR`) is also needed for safe MSI pass-through if delegated to a MicroVM.
4. **CUDA & Graphics Userland (`UNSUPPORTED`):**
   - Running CUDA or Vulkan workloads requires not just `nvidia.ko` + GSP-RM, but also `nvidia-uvm.ko` (GPU page fault replay, HMM/migrated virtual memory), `/dev/nvidia*` character ioctl ABI emulation, closed-source user-space `libcuda.so` / `libnvidia-glcore.so` / `nvidia_icd.json` (`glibc`/`pthread`/`dlopen`), and POSIX `mmap`/`ioctl` runtime support. **None of this exists in DRY OS; CUDA is strictly `UNSUPPORTED`.**

---

## 4. Driver Island vs Linux GPU MicroVM Verdict

| Dimension | Option A: `libdry-lx-compat` OpenRM Driver Island | Option B: Linux GPU MicroVM (`VFIO` `01:00.0` + SHM IPC) |
| :--- | :--- | :--- |
| **Effort to Working GA107 Init** | Very high (`149` unsupported `os-interface.h` APIs + `nv-linux.h` + GSP RPC + `nvidia-uvm` + custom `ioctl` translator) | Moderate: unmodified upstream Linux `nvidia.ko` + `nvidia-uvm.ko` + `libcuda.so` run inside an isolated Linux guest with zero symbol emulation |
| **Muxless Laptop Advantage** | Competes for `root` kernel/OS compat work | **Fits `82K1` topology naturally:** internal `eDP-1` stays safely owned by Intel Iris Xe / DRY compositor while `01:00.0` can be isolated inside a dedicated IOMMU group |
| **Security & Fault Containment** | Large C blob + GSP state in `RootSystem` Ring-3 process | Hardware VM + VT-d containment; guest panic cannot corrupt DRY kernel or primary display |
| **CUDA / Vulkan Userland** | Blocked on full Linux POSIX/glibc/ioctl/UVM ABI | Available *as-is* inside the Linux MicroVM once `VFIO` + VT-d Interrupt Remapping + MicroVM DMA-BUF/SHM bridge are implemented |

**Decision:** Do **not** build a 150-symbol fake OpenRM Driver Island (`nv_os_shim_check.c` stays frozen as a small honest audit harness). Focus future GA107 enablement on **VT-d Interrupt Remapping + VFIO-style isolated MicroVM ownership** of `01:00.0` with a shared-memory PRIME/fence transport (optimized in Milestone F).
