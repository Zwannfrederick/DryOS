# DRY OS Project Ascension 6.0 -- Validation Matrix (`v0.6.0-dev` / `ascension-6.0`)

**Baseline Reference:** `v0.5.0` (`3c823f4`)  
**Scope:** Ring-0 fail-closed IOMMU & vendor-neutral `IommuOps` (`DMAR` + `IVRS`), ACPI `MCFG`/`ECAM`, 64-slot PCI registry, scatter-gather DMA (`IOVA != HPA`) & 2 MiB `WC`/`UC` MMIO, vendor-neutral Ring-3 `dry-gpu-svc` & GOP-to-KMS handover FSM, decoupled `libdry-lx-compat`, SHA-256 firmware store, 15-stage broker lifecycle, MicroVM GPU IPC prototype, VirtIO-GPU 3D capset discovery, Vyukov `StaticRunQueue` preemption fix, and xHCI/IoRing timeout alignment.  
**Physical Hardware Gate:** Physical USB flashing and bare-metal Lenovo `82K1` GPU modesetting remain **explicitly gated for manual operator execution** (zero `/dev/*` mutations, PCI resets, or host GPU unbinds performed).

---

## 1. Automated QEMU/OVMF & Host Suite Results (Post Final Edit)

| Suite / Gate | Command | Result | Key Observed Evidence |
| :--- | :--- | :---: | :--- |
| Full QEMU/OVMF Unified Suite | `python3 tools/qemu_boot_test.py` | **PASS (139/139)** | `58` Legacy + `9` P1 + `14` P2 + `6` P3 + `3` P4 + `2` 2C-0 + `1` 2C-1 + `1` GPU + `1` 2C-2 + `9` Glass/2.0/2.1 + `1` LxDRM + `25` USB/NVMe + `1` LxNet + `4` Brand + `4` Ascension 6.0 |
| SMP/RAM Desktop Matrix | `python3 tools/qemu_boot_test.py --p3-matrix` | **PASS (5/5)** | `1cpu/256M`, `2cpu/256M`, `4cpu/512M`, `8cpu/1G`, `20cpu/2G` (`hlt_calls=13646`, `leaked_frames=0`, `8.02s`) |
| `4 GiB` High-Memory Bring-up | `python3 tools/qemu_boot_test.py --smp-only 2 --mem 4G` | **PASS (1/1)** | `boot_2cpu_ok` (`8.048s`, `leaked_frames=0`, `PMM` + `SLPT39` + `VSpace` + `CSpace` + `IPC`) |
| USB xHCI + BOT + NVMe RO (3-boot) | `python3 tools/qemu_boot_test.py --p13-only` | **PASS (25/25)** | `21.62s`, `syscalls=94889` (`p13_boot2_rw`), `nvme_sha=596a7c63e906`, ESP & NVMe untouched |
| Ascension 6.0 Dedicated Gate (`p16`) | Included in `qemu_boot_test.py` (`--p16-only`) | **PASS (4/4)** | `p16a` (`8.815s`), `p16b` (`4.224s`), `p16c` (`1.754s`), `p16d` (`1.672s`) (`tools/compat_bench/results_p16_ascension6.json`) |
| Live USB GPT Build & `VT-d` Boot | `python3 tools/build_live_usb_image.py --output kernel/build/dryos-ascension6-live-usb.img --build --verify-boot` | **PASS (1/1)** | `160 MiB` GPT, `27` modules, `[DRY-IOMMU] VTD_ACTIVE`, `esp_untouched=true`, `leaked_frames=0`, `exit=33`, `6.603s` |

---

## 2. Ascension 6.0 Sub-Test Evidence (`p16a` .. `p16d`)

- **`p16a_iommu_fail_closed_and_ivrs`:**
  - Without hardware IOMMU and without `dry_qemu_test_no_iommu=1`: `dry-kernel` logged `[DRY-IOMMU] FAIL_CLOSED` and refused untrusted DMA (`exit=35`, `[DRY-HW-FAIL] code=106`, `[DRY-FB]` panic banner).
  - With QEMU `amd-iommu` (`IVRS`): `dry-kernel` detected `IVRS` (`[DRY-IOMMU] IVRS detected`), classified `IommuBackendKind::AmdVi` as `FailClosed` (no fabricated SLPT/IOTLB claims), and cleanly booted non-DMA paths (`exit=33`, `[DRY-BOOT-OK]`).
- **`p16b_gpu_svc_4backends_handover_fences`:**
  - Verified `dry-gpu-svc` (`[DRY-GPU-SVC] PASS: backends=4 handover=0x7f synth=2 rollbacks=3 broker_data_path=0`), WM integration (`[DRY-WM-GPU-SVC] state=KMS_ACTIVE ... broker_data_path=0`), `dry-fw-store` SHA-256 verification & tamper/`ENOENT` rejection (`[DRY-FW-STORE] PASS: entries=3 verified=2 tamper_rej=1 enoent_rej=1`), 15-stage broker lifecycle & illegal-transition/respawn checks (`[DRY-LIFECYCLE] PASS: steps=15 mask=0x7fff illegal_rej=1 respawns=1`), and VirtIO-GPU 3D probe (`[DRY-GPU] 3D_PROBE virgl=...`) with `leaked_frames=0`.
- **`p16c_lx_neutrality_and_onboarding`:**
  - `drivers/libdry-lx-compat/` contains `0` `bochs` references (isolated to `drivers/lx-drm-bochs-island/src/bochs_island_glue.c`).
  - `python3 -m unittest tools/lx_compat/test_lx_driver_manager.py`: **6/6 tests OK**.
  - Onboarding truth table (`11` stages): `drm_bochs` `11/11`, `virtio_gpu_native` `11/11`, `synthetic_gpu` `11/11`; hardware/portability targets honestly gated at `4/11`: `intel_i915_tgl` (`profile_audit_only`), `amdgpu_generic` (`portability_profile_only`), `nvidia_open_ga107` (`shim_compile_only`).
  - `tools/gpu_fw_validators.py --self-test`: PASS (`dmc=2.12`, `guc=70.1.1`, `gsp_wpr2=True`, `os_iface=22/162`, `amd_ucode=True`, `fw_store_sha256=True`, `host_real_blobs=3`).
- **`p16d_microvm_proto_and_virtio3d_probe`:**
  - `tools/microvm_gpu_proto/proto_bench.py`: PASS (`dev_kvm_rw=True`, `transport_mode=SHARED_MEMORY_IPC_COPIED`, `zero_copy_dmabuf=False`, `physical_igpu_passthrough_assumed=false`).
    * `256x256` dirty tile (`262,144 B`, 60 iters): `p50=218.08 us`, `p95=297.49 us`, `p99=334.66 us`, `1,045.51 MiB/s`, `ack_failures=0`.
    * `1280x720` frame (`3,686,400 B`, 60 iters): `p50=3,664.06 us`, `933.80 MiB/s`.
    * `1920x1080` frame (`8,294,400 B`, 60 iters): `p50=8,199.37 us`, `941.05 MiB/s`.
  - `tools/probe_virtio_gpu_3d.py`: PASS (`status=HOST_VIRGL_UNAVAILABLE`, host QEMU lacks `virtio-gpu-gl-pci` / `virglrenderer`).

---

## 3. Live USB Artifacts & Binary Hygiene

- **Preserved `v0.5.0` Live USB Image (`kernel/build/dryos-ascension5-live-usb.img`):**
  - SHA-256: `46c96b3c46e1af5f9ede408260be24bf86b805d92d11b6462a5dee8eed93fbfc` (untouched).
- **Verified `Ascension 6.0` Live USB Image (`kernel/build/dryos-ascension6-live-usb.img`):**
  - SHA-256: `2c85fee6f4d79bee58cfe1d5cfdfbd32644268f4ee148d5e54214a67d1c1d1e0`
  - ESP Prefix (`LBA < 198656`) Pre/Post Boot SHA-256: `1199259f68217bff6fa7a2ffed57e78870a17550c2e53f81ca166b959b342cb8` (`esp_untouched=true`).
- **Ring-0 Kernel Static Invariants:**
  - `dry_kernel::syscall::rust_syscall_dispatch` stack frame: `248 B` (`sub $0xf8,%rsp <= 256 B`), zero new syscalls (`ABI v0.1 0..=31`).
  - Zero `#include <linux/`, `#include <drm/`, `MODULE_LICENSE`, `EXPORT_SYMBOL`, or `GPL-2.0-only`-only files in `kernel/src/`.
  - All files added or modified on `ascension-6.0` remain strictly `< 1,000` lines (largest touched source file: `userspace/src/wm.rs` at `962` lines).

---

## 4. Honest Hardware & Architecture Limitations

| Subsystem / Target | Ascension 6.0 Status | What Is NOT Claimed Done |
| :--- | :--- | :--- |
| Intel `VT-d` (`DMAR`/`SLPT`) | Implemented & QEMU Tested (`VTD_ACTIVE` + fail-closed) | Real bare-metal dual-DRHD Tiger Lake boot test awaits operator execution |
| AMD-Vi (`IVRS`) | ACPI `IVRS` detection + `FailClosed` backend only | Full AMD-Vi command buffer, IOMMU page tables, and IOTLB invalidation |
| Vendor-Neutral `dry-gpu-svc` | Implemented & QEMU Tested (`4` backends, handover FSM, fences) | Physical i915/xe or NVIDIA KMS scanout |
| `libdry-lx-compat` (`2,207` C LOC) | Vendor-neutral, real `CFG_RW` + `SYS_DMA_CTL` SG hooks | Full multi-file `drm/i915` / `amdgpu` TTM/GEM execution |
| NVIDIA OpenRM Shim (`nv_os_shim_check.c`) | `-Werror` compile against `/usr/src/nvidia-620.70.06` (`22` classified syms) | `4` IMPLEMENTED, `4` PARTIAL, `9` STUB_ENOSYS, `5` COMPILE_ONLY out of `162` APIs; no GSP-RM / `libspdm` bring-up |
| Linux MicroVM GPU Path | Evaluated + IPC prototype benchmark (`SHARED_MEMORY_IPC_COPIED`) | Zero-copy PRIME DMA-BUF cross-VM page flip (`zero_copy_dmabuf=False`) |
| VirtIO-GPU 3D (`VIRTIO_GPU_F_VIRGL`) | Guest capset discovery wired (`[DRY-GPU] 3D_PROBE`) | 3D command submission (host QEMU reports `HOST_VIRGL_UNAVAILABLE`) |
