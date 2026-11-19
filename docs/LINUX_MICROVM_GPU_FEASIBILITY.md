# DRY OS Linux MicroVM GPU Runtime Feasibility & Transport Boundary

## Scope & Honest Architecture Boundary

This document evaluates running large multi-file Linux GPU driver stacks (`i915`/`xe`, `amdgpu`, NVIDIA `open-gpu-kernel-modules` + GSP-RM) inside a disposable KVM/Linux microVM compartment orchestrated by DRY OS `dry-gpu-svc` (`GpuBackendKind::LinuxMicroVm` / `GpuExecutionTier::MicroVmCopied`), rather than importing Linux `.ko` modules into Ring 0 or re-implementing tens of thousands of upstream Linux kernel APIs in `libdry-lx-compat`.

Critical honesty boundaries enforced across DRY OS Ascension 6.0:

1. **Transport mode is `SHARED_MEMORY_IPC_COPIED` (`zero_copy_dmabuf: false`):** Frames rendered or decoded inside a disposable Linux microVM are copied across a shared-memory ring buffer into the DRY Glass compositor's host buffer; there is NO cross-VM/host zero-copy PRIME DMA-BUF page flipping in the prototype.
2. **Physical iGPU passthrough is NOT assumed (`physical_igpu_passthrough_assumed: false`):** Single-iGPU laptops (such as Lenovo Tiger Lake `8086:9a49`) actively own UEFI GOP scanout on BAR2; passing through the sole boot iGPU via VFIO would either tear down early console/GOP scanout or require headless muxing.
3. **GOP-to-KMS handover fallback:** Any microVM crash triggers `DisplayHandoverMachine::enter_fault_and_fallback()` back to `UefiGop` without corrupting compositor state.

## Prototype Benchmark (`tools/microvm_gpu_proto/proto_bench.py`)

Artifact: [`tools/compat_bench/results_microvm_gpu_proto.json`](../tools/compat_bench/results_microvm_gpu_proto.json)

### Host Environment Probes (`x86_64`, read-only)

- `/dev/kvm` read/write access: `true`
- `/usr/bin/qemu-system-x86_64`: present
- Host RAM: `31868 MiB` total (`16017 MiB` available at sample time)
- Estimated minimal microVM RAM footprint: `~128 MiB`
- Estimated cold boot latency with a dedicated minimal kernel + initramfs: `250-900 ms` (general distro kernels require initramfs tuning and add module probe overhead)

### Measured Two-Process `mmap` Shared Ring + Doorbell (`SHARED_MEMORY_IPC_COPIED`)

120 timed iterations (+8 warmup), full per-frame IEEE CRC32 integrity verification (`0` CRC/ack failures):

| Workload | Payload Bytes | `p50` (us) | `p95` (us) | `p99` (us) | Throughput (`MiB/s`) | Zero-Copy |
|---|---:|---:|---:|---:|---:|---|
| Dirty Tile `256x256` XRGB | 262,144 | 216.03 | 268.94 | 281.69 | 1,102.41 | `false` (`SHARED_MEMORY_IPC_COPIED`) |
| Frame `1280x720` XRGB | 3,686,400 | 3,632.53 | 4,147.94 | 4,269.66 | 955.90 | `false` (`SHARED_MEMORY_IPC_COPIED`) |
| Frame `1920x1080` XRGB | 8,294,400 | 8,071.27 | 11,046.15 | 14,859.50 | 922.96 | `false` (`SHARED_MEMORY_IPC_COPIED`) |

Interpretation:
- For **damage-tracked UI rectangles** (`256x256` tiles at `~0.22 ms`), copied shared-memory IPC comfortably fits within a `16.6 ms` (`60 Hz`) frame budget (`~1.3%` of frame interval).
- For **full unclipped `1080p` frames** (`~8.07 ms` p50, `~14.86 ms` p99), memory copy overhead consumes roughly half to nearly all of a `60 Hz` budget, making dirty-rect damage tracking mandatory until real cross-domain grant/DMA-BUF page-flipping exists.

## Vendor Routing Matrix (Why Not Everything Goes in `libdry-lx-compat`)

| Driver Family | Recommended Execution Tier | Rationale | Current DRY OS 0.6 Status |
|---|---|---|---|
| Simple single-file Linux DRM/net/rng (`bochs.c`, `8139cp.c`, `virtio-rng.c`) | `LxCompatIsland` (Ring-3 C island + `libdry-lx-compat`) | Small upstream surface; capability-gated MMIO/DMA/IRQ + `#UD` respawn verified | **ACTIVE AND TESTED** in QEMU |
| `virtio-gpu` (`1af4:1050`) 2D/3D | `NativeDriverIsland` (`dry-gpu` Rust Ring-3 driver) | Minimal MMIO/virtq protocol; 3D capability discovery (`GET_CAPSET_INFO`/`GET_CAPSET`) wired read-only | **2D ACTIVE + 3D CAPSET PROBE WIRED** (`HOST_VIRGL_UNAVAILABLE` on current host QEMU) |
| Intel Iris Xe (`8086:9a49`, `i915`/`xe`) | `LinuxMicroVm` (`MicroVmCopied`) or dedicated native port | Large multi-file GEM/TTM/GuC/DMC stack; cannot blindly drop `.ko` in Ring-0; single-iGPU keeps GOP fallback | **CATALOG/FIRMWARE VALIDATED ONLY** |
| AMD Radeon (`amdgpu` e.g. `1002:73df`) | `LinuxMicroVm` (`MicroVmCopied`) | Independent portability target (`COMMON` header ucode check + IOMMU fail-closed `IVRS`); no AMD HW assumed | **PORTABILITY PROFILE ONLY** |
| NVIDIA RTX 3050 Ti (`10de:25a0`, OpenRM + GSP) | `LinuxMicroVm` (or dedicated GSP-RPC host VM with dGPU VFIO-like IOMMU domain) | Requires `libspdm` + `~27-50 MiB` GSP ELF64 + `162` `os_*` APIs (`4` implemented, `4` partial, `9` ENOSYS stub, `5` compile-only in shim) | **SHIM COMPILE + HEADER/WPR2 AUDIT ONLY** |
