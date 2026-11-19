# DRY OS — Cross-Vendor Hardware & GPU Readiness Scorecard (`Project Ascension 7.0`)

**Architectural Principle:** `kernel/src/` stays **`100%` vendor-neutral and GPL-free (`0` Linux/DRM headers or symbols)**. All device discovery, IOMMU isolation policy, scatter-gather DMA, WC/UC MMIO, firmware integrity verification, and GPU backend selection run via generic capability abstractions and modular Ring-3 compartments under [userspace/src/gpu_svc/](../userspace/src/gpu_svc/mod.rs).

---

## 1. Cross-Vendor GPU & Display Readiness Matrix

| Target GPU / Display Path | Backend Architecture | Firmware & Resource Audit | Current DRY OS Readiness | Honest Technical State |
| :--- | :--- | :--- | :---: | :--- |
| **QEMU `virtio-gpu-pci` (2D Scanout)** | Native Ring-3 Rust (`VirtioGpu2d` in `userspace/src/virtio_gpu.rs`) | `SYS_DMA_CTL` SG DMA (`IOVA != HPA`), `transfer_and_flush_rect`, `[x_start..x_end)` cursor/blit clip | **`VERIFIED_QEMU`** | Production-grade QEMU 2D scanout (`143/143`, `-54.4%` dirty px, `1920x1080` validated). |
| **QEMU `virtio-gpu-gl-pci` (3D Virgl)** | Native Ring-3 Rust (`VirtioGpu3d` in `userspace/src/virtio_gpu_3d.rs`) | `VIRTIO_GPU_F_VIRGL`, `GET_CAPSET_INFO` (`capsets=2`), `SUBMIT_3D` clear + readback + `-0x1205` negative fault test | **`VERIFIED_QEMU`** *(Mesa `llvmpipe`)* | Real QEMU 3D Virgl protocol verified in `.toolchains/virgl-env/`; host GPU GL/Vulkan passthrough not claimed. |
| **Bochs / QEMU StdVGA (`drm` island)** | Decoupled C island (`drivers/lx-drm-bochs-island/bochs_island_glue.c`) | `libdry-lx-compat` (`0` `bochs` refs in core compat), `DEV_MAP_OP_CFG_RW` + `SYS_DMA_CTL` | **`VERIFIED_QEMU`** | End-to-end modeset & scanout test passes inside isolated Ring-3 island (`--p12` / unified suite). |
| **Intel Iris Xe (`8086:9a49`, Tiger Lake)** | Staged Option A (`userspace/src/intel_tgl_prep.rs` -> native Ring-3 `dry-gpu-intel`) | Host blobs verified (`dmc=2.12`, `guc=70.1.1`, eDP `1920x1080@165.01Hz`, `gop_preserve=True`); `[DRY-INTEL-PREP] PASS` (`342` lines) | **`PARTIAL`** *(Ready for HW Prep)* | Prep/BAR/MMIO/firmware state machine verified in QEMU; GGTT/PPGTT, GuC submission & physical DPCD modeset pending silicon. |
| **NVIDIA RTX 3050 Ti (`10de:25a0`, GA107M)** | Audited Linux MicroVM path (`drivers/lx-nv-openrm-probe/` + `nvidia_ga107_audit.py`) | `ucodes_ga10x.bin` (`53.53 MiB`, SHA-256 `b8f3d94d...`); `171` OpenRM OS APIs audited (`4` impl, `4` partial, `9` enosys, `5` compile_only, `149` unsupported) | **`PARTIAL`** *(Audit)* / **`UNSUPPORTED`** *(Exec/CUDA)* | Realistic path requires dedicated Linux OpenRM MicroVM + cross-VM DMA-BUF/VFIO; native modeset & CUDA unimplemented. |
| **AMD iGPU / dGPU (`ACPI IVRS`)** | Fail-closed IOMMU detection (`kernel/src/iommu/ivrs.rs`) | ACPI `IVRS` parsed (`IommuBackendKind::AmdVi`), `amd_ucode=True` in self-test, enforces `FailClosed` on unimplemented SLPT | **`PARTIAL`** *(Detect)* / **`UNSUPPORTED`** *(SLPT)* | Safe by construction: refuses untrusted DMA until native AMD-Vi SLPT/IOTLB translation is built. |

---

## 2. Proof of Vendor-Independent `dry-gpu-svc` Isolation

Any GPU backend swaps cleanly beneath DRY OS without altering the microkernel or window manager (`dry-display` / `wm`):

1. **Kernel Privilege Wall (`kernel/src/iommu/` & `pci/`):**  
   The kernel exposes only vendor-agnostic `IommuOps` (`DMAR`/`IVRS` fail-closed), `64`-slot `MCFG`/`ECAM` enumeration, MSI vectors, `SYS_DMA_CTL` scatter-gather `IOVA` translation, and `WC`/`UC` MMIO caps (`PAT4`). `rust_syscall_dispatch` (`248 B`) never branches on Intel, NVIDIA, AMD, or VirtIO device IDs.
2. **Ring-3 Broker & Backend Contract (`userspace/src/gpu_svc/`):**  
   `DryGpuService` implements a unified `GpuBackend` surface across **4 orthogonal backends** (`UefiGop`, `VirtioGpuNative`, `LxCompatDrm`, `LinuxMicroVm`), governed by:
   - `DisplayHandoverMachine` (`0x7f`) cleanly separating firmware `GOP_ACTIVE` from runtime `KMS_ACTIVE`
   - Sequential fence ring & `SurfaceBufferSet` capability sharing
   - **`broker_data_path=0`:** Compositor (`dry-display`) and `wm` never hold PCI config or MMIO caps; `dry-gpu` crash/respawn (`p17a` `gen=1->2->3`, `quiesce=4`) preserves system stability.
3. **Empirical Confirmation Across Ascension 6.0 & 7.0:**  
   Integrating `VirtioGpu3d` (`3a06ed8`), `transfer_and_flush_rect` clipping (`d747f01`), and `intel_tgl_prep.rs` (`5bfabac`) left `kernel/src/` unmodified for GPU logic and required **zero** changes to `dry_shell` or compositor clients.
