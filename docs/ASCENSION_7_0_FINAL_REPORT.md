# DRY OS — Project Ascension 7.0 (`SILICON REALITY`) Final Report

**Repository:** `Zwannfrederick/DryOS`  
**Branch / Release Line:** `ascension-7.0` (`v0.6.0-dev` hardening line branched from `ascension-6.0` @ `f104e0e`, preserving `v0.5.0` @ `3c823f4`)  
**Scope Rule:** Minimum Overhead. Maximum Useful Work. Zero host interference (`0` physical USB writes, `0` NVMe/EFI edits, `0` host GPU unbind/reset/MMIO/VFIO/kernel-module actions).

---

## 1. Executive Summary (Milestones A–G)

| Milestone | Commit | Summary of Work Completed | Status Label |
| :--- | :---: | :--- | :---: |
| **A — Hardware Readiness Audit** | `5673c9c` | Deep audit of all 10 subsystems & 139 existing tests + 12 targeted hardening fixes across `kernel/src/iommu/{dmar,slpt,vtd,vtd_domain,mod,tests}.rs`, `acpi.rs`, `pci_cfg.rs`, `pci_window.rs`, `pci.rs`, `display.rs`, `irq/msi.rs`, `dma.rs`, and `userspace/src/{gpu_svc/*,wm_ipc.rs,fw_store.rs,ioring.rs,sup_hw_services.rs,virtio_gpu.rs}` + `lx-net-8139cp-island`. Report: [ASCENSION_7_0_HARDWARE_READINESS.md](ASCENSION_7_0_HARDWARE_READINESS.md). | `VERIFIED_QEMU` |
| **B — Safe Physical Boot Prep (`82K1`)** | `58c0975` | Added boot telemetry (`[DRY-IOMMU] UNIT`, `[DRY-PCI-TOPO]`), passive Lenovo `82K1` hardware profiler [hw_profile_lenovo_82k1.py](../tools/hw_profile_lenovo_82k1.py) -> `24` PCI funcs (`8086:9a49`, `10de:25a0`, eDP `1920x1080`, `DMAR` present), fail-closed DMA policy preserved, QEMU-verified Ascension 7.0 `160 MiB` Live USB, and operator playbook [PHYSICAL_USB_TEST_OPERATOR_GUIDE.md](PHYSICAL_USB_TEST_OPERATOR_GUIDE.md). | `VERIFIED_QEMU` *(Prep)* / `IMPLEMENTED_UNTESTED` *(Physical USB Gate)* |
| **C — Intel Iris Xe First Step (`8086:9a49`)** | `5bfabac` | Compared Options A (native DRY), B (Linux `i915`/`xe` compat island), and C (Linux GPU MicroVM) across 11 criteria; selected staged Option A inside Ring-3 without symbol bloat. Added `userspace/src/intel_tgl_prep.rs` (`342` lines) wired into `dry_display` (`[DRY-INTEL-PREP] PASS`). Report: [INTEL_IRIS_XE_COMPATIBILITY.md](INTEL_IRIS_XE_COMPATIBILITY.md). | `PARTIAL` *(HW validation pending)* |
| **D — NVIDIA RTX 3050 Ti Prep (`10de:25a0`)** | `74e1c71` | Audited `171` Linux OpenRM OS APIs via [nvidia_ga107_audit.py](../tools/nvidia_ga107_audit.py) (`4` IMPLEMENTED, `4` PARTIAL, `9` ENOSYS, `5` COMPILE_ONLY, `149` UNSUPPORTED) and passive GSP `ucodes_ga10x.bin` (`53.53 MiB`, SHA-256 `b8f3d94d...`). Established MicroVM vs huge-shim trade-off. Report: [NVIDIA_GA107_COMPATIBILITY.md](NVIDIA_GA107_COMPATIBILITY.md). | `PARTIAL` *(Audit)* / `UNSUPPORTED` *(Modeset & CUDA)* |
| **E — Verifiable 3D / Virgl Path** | `3a06ed8` | Created isolated non-root Virgl test env builder [bootstrap_qemu_virgl_env.py](../tools/bootstrap_qemu_virgl_env.py) (`.toolchains/virgl-env/`, Mesa `50_mesa.json` `llvmpipe`), implemented [virtio_gpu_3d.rs](../userspace/src/virtio_gpu_3d.rs) (`391` lines) with `SUBMIT_3D` clear + readback + `-0x1205` negative malformed-command test. Report: [GPU_ACCELERATION_RESULTS.md](GPU_ACCELERATION_RESULTS.md). | `VERIFIED_QEMU` *(Mesa `llvmpipe`)* |
| **F — GPU IPC & Display Optimization** | `d747f01` | Added `transfer_and_flush_rect(x, y, w, h)` and horizontal scanline/cursor `[x_start..x_end)` clipping (`-54.4%` dirty pixels, `-25.0%` `pres2w p50`), plus 9-tier `1920x1080x4` IPC harness [ipc_bench_a7.c](../tools/microvm_gpu_proto/ipc_bench_a7.c) (`T2` async ring `p50=435.20 us` vs `T0` `8,250.75 us`). Report: [PERFORMANCE_COMPARISON.md](PERFORMANCE_COMPARISON.md). | `VERIFIED_QEMU` *(Display)* / `PROTOTYPE` *(Host IPC)* |
| **G — Reliability & Daily-Use Hardening** | `6aa1f56` | Added `--p17-only` (`4/4` negative/recovery suite: multi-crash `dry-gpu` respawn `gen=1->2->3`, storage-less volatile boot `persist=0`, SMP-4 VT-d DMA fault isolation `vtd_faults=2`, and VirtIO-GPU 3D gate `neg_err=0x1205`), plus `143/143` full QEMU + `5/5` SMP/RAM matrix. | `VERIFIED_QEMU` |

---

## 2. Strict Feature Classification Ledger

Allowed labels exclusively: `VERIFIED_QEMU`, `VERIFIED_HARDWARE`, `IMPLEMENTED_UNTESTED`, `PROTOTYPE`, `PARTIAL`, `BLOCKED`, `UNSUPPORTED`.

| Subsystem / Feature | Classification | Evidence Summary |
| :--- | :---: | :--- |
| Fail-closed `VT-d` IOMMU (`DMAR` / `SLPT` / `vtd_domain`) | `VERIFIED_QEMU` | `qemu_boot_test.py` `--p16-only` + `--p17-only` `p17c` (`smp=4`, `vtd_enabled=1`, `vtd_faults=2`, `quiesces=5`, `leaked_frames=0`) & Live USB `VTD_ACTIVE`. |
| ACPI `MCFG`/`ECAM` `64`-slot PCI, MSI, SG `IOVA != HPA` DMA | `VERIFIED_QEMU` | Unified `143/143` QEMU suite (`[DRY-PCI-TOPO]`, `--p13-only` `25/25`, `--gpu-only`). |
| Ascension 7.0 `160 MiB` GPT Live USB (`dryos-ascension7-live-usb.img`) | `VERIFIED_QEMU` | Rebuilt & QEMU/OVMF `intel-iommu` boot-verified (`6.506s`, `27` modules, `leaked_frames=0`, SHA-256 `6165c54f...`). Bare-metal USB execution remains operator-gated. |
| Ring-3 `dry-gpu-svc`, `DisplayHandoverMachine` (`0x7f`), `DRY Glass 2.1` | `VERIFIED_QEMU` | `--gpu-only`, `--p9-only` `3/3`, `--p15-only`, `--p17-only` `p17a` (`gen=1->2->3`, `quiesce=4`, `corrupt-sb0-test ok=true`, `sec_audit=1`, `unauth_perm=false`). |
| VirtIO-GPU 2D with horizontal dirty-rect clipping | `VERIFIED_QEMU` | `--gpu-only` (`p7a` `491,520 -> 224,256` px [`-54.4%`], `pres2w p50` `-25.0%`) & `--p9-only` `3/3` (`1920x1080` `dirty_px=442255`). |
| VirtIO-GPU 3D / Virgl (`SUBMIT_3D` clear + readback + `-0x1205`) | `VERIFIED_QEMU` | `probe_virtio_gpu_3d.py --live-qemu` (`16.295s`, `px=0xffff0000`, `neg_err=0x1205`) & `--p17-only` `p17d` (`15.74s`) on Mesa software `llvmpipe`. |
| Storage-less volatile desktop degradation (`p17b`) | `VERIFIED_QEMU` | Zero disk/NVMe/USB boot completes to desktop (`persist=0`, `leaked_frames=0`, `3.99s`). |
| Lenovo `82K1` physical bare-metal boot (`DMAR`/eDP/xHCI on silicon) | `IMPLEMENTED_UNTESTED` | Telemetry & Live USB image ready and QEMU-verified; zero physical hardware runs executed (`VERIFIED_HARDWARE` count = `0`). |
| Multi-tier C `eventfd` / async ring / host POSIX SHM slot zero-copy (`T1..T5`) | `PROTOTYPE` | Verified between two isolated host Linux OS processes (`run_a7_perf_suite.py`, `all_integrity_ok=true`); not running inside cross-VM guest kernels. |
| Intel Iris Xe (`8086:9a49`) `intel_tgl_prep.rs` & native eDP modesetting | `PARTIAL` | Preparation state machine `[DRY-INTEL-PREP] PASS` (`VERIFIED_QEMU`); physical GTT/ring execution and eDP `165 Hz` link training are not executed on silicon. |
| AMD `IVRS` detection (`FailClosed`) vs AMD-Vi SLPT DMA translation | `PARTIAL` / `UNSUPPORTED` | ACPI `IVRS` parsed and reported fail-closed (`PARTIAL`); AMD-Vi page-table DMA translation is `UNSUPPORTED`. |
| NVIDIA RTX 3050 Ti (`10de:25a0`, GA107M) OpenRM shim & GSP audit | `PARTIAL` | `4` IMPLEMENTED / `4` PARTIAL / `9` ENOSYS / `5` COMPILE_ONLY / `149` UNSUPPORTED out of `171` OpenRM OS APIs + `ucodes_ga10x.bin` SHA-256 verified (`b8f3d94d...`). |
| Native Intel/NVIDIA physical 3D rendering, Vulkan/Venus, and CUDA compute | `UNSUPPORTED` | No silicon command submission, no cross-VM `DMA-BUF` / VFIO passthrough, and no Vulkan/CUDA userspace runtime on DRY OS. |
| Physical bare-metal verification (`hardware_verified`) | `UNSUPPORTED` *(Not Executed)* | Explicitly prohibited without user approval and operator test execution (**0 features** claimed as `VERIFIED_HARDWARE`). |

---

## 3. Direct Technical Answers to Section 13 Questions

### Q1: Lenovo `82K1` Donanımı Üzerinde Güvenli İlk Boot Hazırlığı Ne Durumda?
`READY_FOR_SAFE_OPERATOR_BOOT_TEST` (`IMPLEMENTED_UNTESTED` on silicon; `VERIFIED_QEMU` under `intel-iommu` `OVMF`).  
- [hw_profile_lenovo_82k1.py](../tools/hw_profile_lenovo_82k1.py) passively inventoried `24` PCI functions, confirming Intel Iris Xe (`8086:9a49`), RTX 3050 Ti (`10de:25a0`), internal eDP `1920x1080`, and ACPI `DMAR`.
- Milestone A/B added strict fail-closed VT-d (`SLPT`/`DMAR`), `[DRY-IOMMU] UNIT` and `[DRY-PCI-TOPO]` serial telemetry, storage-less volatile boot resilience (`p17b` `persist=0`), and rebuilt `kernel/build/dryos-ascension7-live-usb.img` (`160 MiB` GPT, `27` modules, `6.506s` QEMU boot `VTD_ACTIVE`, SHA-256 `6165c54f1617094a0ef3a38b2df5326721063fbf30d4fac79417ff1359b2d84b`).
- Remaining physical unknowns (multi-DRHD DMAR quirks, real xHCI controller timeouts, firmware GOP stride/handover timing) are documented with rollback instructions in [PHYSICAL_USB_TEST_OPERATOR_GUIDE.md](PHYSICAL_USB_TEST_OPERATOR_GUIDE.md).

### Q2: Intel Iris Xe İçin Kalan En Büyük Engeller Neler?
Iris Xe (`8086:9a49`) currently stops at **`PARTIAL`** (`[DRY-INTEL-PREP] PASS` in `userspace/src/intel_tgl_prep.rs`, `342` lines). The concrete remaining blockers to real pixel output and hardware acceleration on silicon are:
1. **GuC / HuC / DMC Firmware Loading & Auth:** Host blobs exist (`dmc=2.12`, `guc=70.1.1`), but physical BAR0 MMIO upload and GuC DMA authentication on silicon remain unexecuted.
2. **GGTT & PPGTT Page-Table Management:** DRY OS has scatter-gather `IOVA != HPA` DMA allocation (`SYS_DMA_CTL`), not a full GPU-side GTT/PPGTT binder.
3. **Engine Command Submission:** Physical GuC/Execlists RCS/BCS/VCS ringbuffer doorbell programming and IRQ completion handling.
4. **DDI / eDP `1920x1080 @ 165 Hz` Modesetting:** Keeping BIOS GOP active works (`gop_preserve=True`), whereas native DPCD AUX link training and transcoder/PLL reconfiguration without blanking the laptop panel remain unimplemented.

### Q3: NVIDIA RTX 3050 Ti İçin Gerçekçi Yol Hangisidir ve Neden?
A **dedicated Linux GPU MicroVM** (paired with future cross-VM `DMA-BUF`/VFIO memory sharing) — *not* re-implementing Linux kernel internals in `libdry-lx-compat`.  
- [nvidia_ga107_audit.py](../tools/nvidia_ga107_audit.py) and [nv_os_shim_check.c](../drivers/lx-nv-openrm-probe/nv_os_shim_check.c) establish that NVIDIA Open GPU Kernel Modules require **`171`** Linux kernel OS interfaces; DRY currently covers **`4` `IMPLEMENTED` + `4` `PARTIAL`**, leaving **`9` `ENOSYS` + `5` `COMPILE_ONLY` + `149` `UNSUPPORTED`** (workqueues, kthreads, PCIe AER/PM, `remap_pfn_range`, `dma_buf`, fence chains, ACPI `_DSM` Optimus power control).
- Furthermore, GA107 relies on a **`53.53 MiB`** signed GSP firmware (`ucodes_ga10x.bin`, SHA-256 `b8f3d94d...`) loaded into WPR2 memory. Maintaining 150+ high-churn Linux VM/synchronization shims natively would destabilize DRY OS's minimal TCB without delivering real OpenRM execution.

### Q4: Grafik Hızlandırmada Nerede Duruyoruz? (Virgl vs. Fiziksel GPU)
- **VirtIO-GPU 2D:** `VERIFIED_QEMU` (`143/143` suite, `--gpu-only`, `--p9-only` `1920x1080`/`1280x800`/`1024x768`, `leaked_frames=0`).
- **VirtIO-GPU 3D / Virgl:** `VERIFIED_QEMU` via isolated `.toolchains/virgl-env/` (`virglrenderer 1.3.0-2` + QEMU `11.1.3-1` GL/EGL + Mesa `50_mesa.json` software `llvmpipe`). Both `probe_virtio_gpu_3d.py --live-qemu` (`16.295s`) and `--p17-only` `p17d` (`15.74s`) prove real `VIRGL` context creation, `SUBMIT_3D` `CLEAR` (`0xffff0000`), pixel readback verification, and malformed-stream negative rejection (`neg_err=0x1205`) with zero leaked frames.
- **Boundary:** This is **software `llvmpipe`-backed Virgl inside QEMU**, *not* physical Iris Xe 3D, *not* Vulkan/Venus (`HOST_VIRGL_QEMU_GL_AVAILABLE` only), and *not* NVIDIA CUDA.

### Q5: Çoklu GPU Mimarisinin Üreticiden Bağımsızlığı Kanıtlandı mı?
**Yes inside OS boundaries (`VERIFIED_QEMU`) — hardware multi-GPU hot-switch remains `PARTIAL`/`IMPLEMENTED_UNTESTED`.**  
`kernel/src/` contains zero vendor GPU drivers (`0` GPL/Linux macro matches via `rg`). All display policy lives behind [userspace/src/gpu_svc/](../userspace/src/gpu_svc/mod.rs) (`GpuBackend` trait supporting `UefiGop`, `VirtioGpuNative`, `LxCompatDrm`, and `LinuxMicroVm`, `DisplayHandoverMachine` `0x7f`, sequential fence ring, and `broker_data_path=0`). Switching `VirtioGpu2d` to `VirtioGpu3d` (`3a06ed8`) or wiring `intel_tgl_prep.rs` (`5bfabac`) required zero kernel changes and zero compositor redesign.

### Q6: Hangi Performans Darboğazları Ölçülerek Giderildi?
1. **In-OS Full-Width Scanline Blit & DMA Transfer (`d747f01`):** Implementing `transfer_and_flush_rect(x, y, w, h)` and horizontal `[x_start..x_end)` cursor/blit clipping cut `p7a` (`1280x800`) dirty pixels from **`491,520` to `224,256` px (`-54.4%`)**, `pres2w p50` from **`1,598,765` to `1,198,974` cyc (`-25.0%`)**, and `pres2w p95` from **`2,105,356` to `1,511,537` cyc (`-28.2%`)**; at `1920x1080` (`--p9-only`), dirty pixels dropped to **`442,255` px (`~21.3%`)**.
2. **MicroVM GPU IPC Double-Copy + Pipe + CRC Overhead (`run_a7_perf_suite.py`):** At `1920x1080x4` (`8.29 MB`, `165 Hz` budget = `6,060.61 us`):
   - `T0` Python copied SHM baseline missed `165 Hz`: `p50=8,250.75 us`, `p95=8,691.71 us` (`951.22 MiB/s`).
   - `T2` Native C triple-buffer async ring + `eventfd`: **`p50=435.20 us` (`19.0x` faster)**, **`p95=576.84 us`**, `17,165.43 MiB/s` (`165Hz_p95=True`).
   - `T3` Dirty-rect batching (`5%`–`25%`): **`p50=19.20–109.88 us`**, **`p95=31.14–141.49 us`** (`0.5%–2.3%` of `165 Hz` budget).
   - `T5` Host POSIX SHM slot-index handoff: **`p50=0.24 us`**, `0 B` pixel copy *(explicitly host process SHM only, NOT cross-VM guest DMA-BUF)*.
3. **Preserved Baseline Hot Path:** `rust_syscall_dispatch` remains **`248 B`** (`sub $0xf8,%rsp`), `--p3-matrix` scales `1..20 vCPU` (`6.98s` at `20cpu/2G`), `--p13-only` passes `25/25`.

### Q7: Güvenilirlik ve Güvenlik Seviyesi Ascension 6.0’a Göre Ne Kadar Arttı?
- **Milestone A (`5673c9c`):** Fixed 12 subtle edge cases audited across VT-d SLPT/DMAR bounds, ECAM PCI windows, MSI masks, scatter-gather DMA IOVA cleanup, `gpu_svc` fence ordering, `fw_store` verification, `ioring` completion accounting, and broker restart cleanup.
- **Milestone G (`6aa1f56` `--p17-only` `4/4 PASS`):** Verified automated multi-crash `dry-gpu` respawn (`gen=1->2->3`, `quiesce=4`, `corrupt-sb0-test ok=true`), enforcement of capability-mode isolation (`sec_audit=1`, `unauth_perm=false`), zero-storage volatile desktop survival (`p17b` `persist=0`), and SMP-4 concurrent DMA fault containment (`p17c` `vtd_faults=2`, `quiesces=5`, `leaked_frames=0`).

### Q8: Gerçek Günlük Kullanım İçin Hangi Adımlar Hâlâ Zorunlu?
1. **Operator Bare-Metal Boot Execution:** Physically test `dryos-ascension7-live-usb.img` (`6165c54f...`) on Lenovo `82K1` following [PHYSICAL_USB_TEST_OPERATOR_GUIDE.md](PHYSICAL_USB_TEST_OPERATOR_GUIDE.md) and capture serial/photo telemetry.
2. **Real Silicon Intel Iris Xe Driver Completion:** Implement GGTT/PPGTT mapping, GuC/Execlists submission, and native DPCD/eDP `165 Hz` modesetting in Ring-3 `dry-gpu-intel`.
3. **Real Cross-VM `DMA-BUF` Zero-Copy / VFIO + OpenRM MicroVM:** Required before RTX 3050 Ti PRIME offload or real CUDA compute can exist safely.
4. **AMD-Vi SLPT Translation + Writable `DRYFS_V1` Persistence + Mesa/Vulkan Userspace:** Needed to graduate beyond QEMU development and read-only storage safety (`INV-DISK-SAFE-01/02`) to general-purpose workstation use.
