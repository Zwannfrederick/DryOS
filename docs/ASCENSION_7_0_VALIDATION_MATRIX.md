# DRY OS — Project Ascension 7.0 Validation Matrix (`v0.6.0-dev` on `ascension-7.0`)

**Date:** 19 Kasım 2026  
**Baseline Preserved:** `v0.5.0` @ `3c823f4` (`kernel/build/dryos-ascension5-live-usb.img`) and `ascension-6.0` @ `f104e0e` (`kernel/build/dryos-ascension6-live-usb.img`)  
**Scope Boundary:** All automated runtime results below are **`VERIFIED_QEMU`** (QEMU/KVM + OVMF + Mesa software `llvmpipe`) or **`PROTOTYPE`** (isolated host IPC processes). Physical bare-metal checks remain **`IMPLEMENTED_UNTESTED`** (**0 `VERIFIED_HARDWARE` claims**).

---

## 1. Automated QEMU/OVMF & Live USB Verification Matrix

| Suite / Command | Result | Key Observed Telemetry & Guarantees | Label |
| :--- | :---: | :--- | :---: |
| `python3 tools/qemu_boot_test.py` *(Full Unified Regression)* | **143 / 143 PASS** | 58 legacy + 9 P1 + 14 P2 + 6 P3 + 3 P4 + 2 2C-0 + 1 2C-1 + 1 GPU + 1 2C-2 + 3 Glass + 3 Glass 2.0 + 3 Glass 2.1 + 1 LxDRM + 25 M2 USB/NVMe + 1 M3 LxNet + 4 Brand/Icons + 4 Ascension 6.0 + 4 Ascension 7.0 (`--p17-only`); `leaked_frames=0`. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p3-matrix` *(SMP/RAM Matrix)* | **5 / 5 PASS** | `1cpu/256M` (`5.12s`), `2cpu/256M` (`5.24s`), `4cpu/512M` (`5.22s`), `8cpu/1G` (`5.32s`), `20cpu/2G` (`6.98s`), all `leaked_frames=0`. | `VERIFIED_QEMU` |
| `python3 tools/build_live_usb_image.py --output kernel/build/dryos-ascension7-live-usb.img --build --verify-boot` | **PASS** | `160 MiB` GPT (`DRYOS_ESP` + `DRYOS_PERSIST` `DRYFS_V1`), `27` Ring-3 ELFs, `esp_untouched=True`, QEMU `intel-iommu` `VTD_ACTIVE` (`6.506s`), `leaked_frames=0`, `exit=33`. SHA-256: `6165c54f1617094a0ef3a38b2df5326721063fbf30d4fac79417ff1359b2d84b`. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p17-only` *(Milestone G Suite)* | **4 / 4 PASS** | `p17a` (`gen=1->2->3`, `quiesce=4`, `corrupt-sb0-test ok=true`, `sec_audit=1`, `unauth_perm=false`, `5.16s`); `p17b` (no disk/NVMe/USB volatile desktop `persist=0`, `3.99s`); `p17c` (`smp=4`, `vtd_enabled=1`, `vtd_faults=2`, `quiesces=5`, `5.29s`); `p17d` (VirtIO-GPU 3D `VERIFIED_QEMU`, `capsets=2`, `px=0xffff0000`, `neg_err=0x1205`, `15.74s`). Output: `tools/compat_bench/results_p17_ascension7.json`. | `VERIFIED_QEMU` |
| `python3 tools/probe_virtio_gpu_3d.py --live-qemu` *(Virgl 3D Live Test)* | **PASS** | `[VIRTIO-GPU-3D-LIVE] status=VERIFIED_QEMU capsets=2 id=1 ctx=True res=True submit=True xfer=0x1100 px=0xffff0000 match=True neg_err=0x1205 clean=True leaked_frames=0 (16.295s)` using `.toolchains/virgl-env/` (`virglrenderer 1.3.0-2` + Mesa `50_mesa.json` `llvmpipe`). Output: `tools/compat_bench/results_virtio_gpu_3d_live.json`. | `VERIFIED_QEMU` *(Mesa `llvmpipe`)* |
| `python3 tools/qemu_boot_test.py --p16-only` | **4 / 4 PASS** | Fail-closed IOMMU / `IVRS`, `gpu_svc` handover/fences/broker, `HOST_VIRGL_QEMU_GL_AVAILABLE`, `INTEL-PREP` telemetry. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --gpu-only` & `--p9-only` | **1/1 & 3/3 PASS** | `p7a` (`1280x800`) dirty px `491,520 -> 224,256` (`-54.4%`), `pres2w p50` `1,598,765 -> 1,198,974` cyc (`-25.0%`), `p95` `2,105,356 -> 1,511,537` cyc (`-28.2%`), `colors=32`; `p9` (`1280x800`, `1920x1080` `dirty_px=442255`, `1024x768` `dirty_px=224256`); all `leaked_frames=0`. | `VERIFIED_QEMU` |

---

## 2. Host Read-Only Auditors & Performance Benchmark Results

| Validator / Benchmark | Result | Key Observed Measurements | Label |
| :--- | :---: | :--- | :---: |
| `python3 tools/microvm_gpu_proto/run_a7_perf_suite.py` | **PASS** (`all_integrity_ok=true`) | `1920x1080x4` (`165 Hz` = `6,060.61 us`): `T0` Python copied SHM `p50=8250.75 us` / `p95=8691.71 us` (`165Hz_p95=False`); `T1` double `eventfd` `p50=508.05 us` / `p95=645.69 us`; `T2` triple async ring `p50=435.20 us` / `p95=576.84 us` (`17,165.4 MiB/s`); `T3` dirty-rect `5%/15%/25%` `p50=19.20 / 67.53 / 109.88 us`; `T4` spin-futex `p50=568.01 us` (`T4b` `62.59 us`); `T5` host POSIX SHM zero-copy index `p50=0.24 us` / `p95=0.31 us` (`0 B` copy). Output: `tools/compat_bench/results_gpu_ipc_a7.json`. | `PROTOTYPE` |
| `python3 tools/gpu_fw_validators.py --self-test` | **PASS** | `dmc=2.12 guc=70.1.1 vbt_ok=True edid=1920x1080@165.01Hz gop_preserve=True gsp_wpr2=True os_iface=22/162 prime_ro_fence=True amd_ucode=True fw_store_sha256=True host_real_blobs=3`. | `VERIFIED_QEMU` *(Read-Only)* |
| `python3 tools/hw_profile_lenovo_82k1.py` | **PASS** | `[HW-PROFILE-82K1] product=82K1 pci_funcs=24 iris_xe=True rtx3050ti=True edp_fhd=True dmar_present=True hardware_verified=False`. | `PARTIAL` *(Passive Only)* |
| `python3 tools/nvidia_ga107_audit.py` | **PASS** | `[NV-GA107-AUDIT] total_apis=171 impl=4 partial=4 enosys=9 compile_only=5 unsupported=149 fw_blobs=1` (`ucodes_ga10x.bin` `53.53 MiB`, SHA-256 `b8f3d94d...`). | `PARTIAL` *(Passive Audit)* |

---

## 3. Binary Hygiene & Architectural Constraints

| Check | Measured Value | Constraint | Status |
| :--- | :--- | :--- | :---: |
| Kernel syscall dispatch stack frame (`objdump -d kernel/target/x86_64-unknown-none/release/dry-kernel` `<_RNvNtCsbqxGAvQp7FS_10dry_kernel7syscall21rust_syscall_dispatch>`) | `sub $0xf8,%rsp` (`248 B`) | `<= 256 B` | **PASS** |
| Largest `.rs` source file in `kernel/src` & `userspace/src` | `963` lines (`kernel/src/ipc/tests.rs`); UI/WM `962` (`userspace/src/wm.rs`), `944` (`dry_shell.rs`), `935` (`sched/mod.rs`), `919` (`lib.rs`), `915` (`endpoint.rs`), `913` (`dry_display.rs`) | `< 1,000` lines hard ceiling | **PASS** |
| GPL / Linux headers in `kernel/src/` (`rg -n '#include <linux/\|#include <drm/\|MODULE_LICENSE\|EXPORT_SYMBOL' kernel/src/`) | `0` matches (`KERNEL_GPL_ZERO_OK`) | Strictly `0` | **PASS** |
| Preserved Baseline Artifacts | `dryos-ascension5-live-usb.img` (`46c96b3c...`) & `dryos-ascension6-live-usb.img` (`2c85fee6...`) untouched | Never overwrite | **PASS** |

---

## 4. Honest Unverified / Out-of-Scope Boundaries

| Boundary | Honest State |
| :--- | :--- |
| Physical Lenovo `82K1` bare-metal USB boot | **`IMPLEMENTED_UNTESTED`** — waiting on user physical execution per [PHYSICAL_USB_TEST_OPERATOR_GUIDE.md](PHYSICAL_USB_TEST_OPERATOR_GUIDE.md). |
| VirtIO-GPU 3D Virgl rendering | **`VERIFIED_QEMU` (Software Mesa `llvmpipe`)** — NOT physical Intel/NVIDIA GL/Vulkan hardware acceleration. |
| `T5` zero-copy frame handoff | **`PROTOTYPE` (Host POSIX SHM only)** — NOT cross-VM guest `DMA-BUF` / `vhost-user-gpu` zero-copy. |
| Intel `8086:9a49` / NVIDIA `10de:25a0` modeset & CUDA | **`PARTIAL` / `UNSUPPORTED`** — requires silicon GGTT/ring/DPCD (`i915`/`xe`) or Linux OpenRM MicroVM + cross-VM DMA-BUF/VFIO. |
| AMD `IVRS` DMA remapping | **`PARTIAL` (FailClosed detection only)** — AMD-Vi SLPT page-table translation is `UNSUPPORTED`. |
