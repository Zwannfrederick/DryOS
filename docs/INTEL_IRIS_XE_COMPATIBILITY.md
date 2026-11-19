# DRY OS Ascension 7.0 — Intel Iris Xe (`8086:9a49`) Compatibility & Strategy (`INTEL_IRIS_XE_COMPATIBILITY.md`)

**Target GPU:** Intel Iris Xe Graphics (Tiger Lake-H GT2, PCI ID `8086:9a49`, `0000:00:02.0`)  
**Display Panel:** Internal eDP `1920x1080 @ 165 Hz` (`card2-eDP-1`, EDID `128 B`, `6.06 ms` frame budget)  
**BAR Layout Observed (Read-Only Host Profile):** BAR0 `GTTMMADR` `16 MiB` (`0x6104000000`, 64-bit non-prefetchable); BAR2 `GMADR` `256 MiB` (`0x4000000000`, 64-bit prefetchable); BAR4 I/O `0x5000` (`64 B`)  
**Milestone C Status:** **`PARTIAL` / `READY FOR HARDWARE VALIDATION`** (Non-destructive initialization/firmware/GGTT/ring/IRQ contracts implemented & unit-verified in QEMU; zero bare-metal Tiger Lake MMIO writes or eDP modesetting executed yet)

---

## 1. Architectural Comparison (11 Criteria): Native DRY vs `i915`/`xe` Driver Island vs Linux GPU MicroVM

| Criterion | Approach A: Native DRY Gen12 Driver (`dry-intel-xe`) | Approach B: Linux `i915` / `xe` Driver Island (`libdry-lx-compat`) | Approach C: Linux GPU MicroVM (`qemu`/`crosvm` + VirtIO/SHM) |
| :--- | :--- | :--- | :--- |
| **1. Linux Kernel ABI Scope Needed** | **Zero** (`no_std` Rust using `SYS_DEV_MAP`, `SYS_DMA_CTL`, `SYS_IRQ_BIND`) | **Extreme (400–600+ symbols):** `drm_atomic`, `drm_dp_aux`, `i2c_algo_bit`, `workqueue`, `dma_fence`, `ttm`/`gem`, `shrinker`, `acpi_evaluate_object`, `iommu_group`, `pm_runtime` | **Zero** in DRY OS (`libvirt`/`Linux` host guest runs upstream `i915`/`xe` natively) |
| **2. Firmware Dependencies** | Low-medium: `tgl_dmc_ver2_12.bin` (DC-state), `tgl_guc_70.1.1.bin` (`~100 KiB`), optional `tgl_huc_7.9.3.bin` verified via `FwStreamVerifier` | Handled internally by `i915` via `request_firmware()` shim (needs synchronous blob staging) | Standard upstream `/lib/firmware/i915` inside disposable guest initramfs |
| **3. MMIO & Interrupt Access** | Direct Ring-3 `2M`/`4K` `UC`+`WC` `SYS_DEV_MAP` windows on `GTTMMADR`/`GMADR` + `SYS_IRQ_BIND(IRQF_ONESHOT)` | Requires C IRQ tasklet/workqueue + `pci_enable_msi` + `writel`/`readl` wrappers over `SYS_DEV_MAP` | Hardware owns MSI & MMIO only if VFIO-passed through (incompatible with primary GOP laptop eDP!) |
| **4. GEM / TTM Memory Management** | Native 64-bit `GGTT` (`0x800000` inside `GTTMMADR`) + `PPGTT` backed directly by `SYS_DMA_CTL` (`IOVA != HPA`) | Must emulate `struct page`, `sg_table`, `drm_gem_object`, `ttm_tt`, `vmap`, shrinker callbacks | Native upstream `drm_gem` / `xe_bo` inside Linux VM; guest->compositor copy/flip over IVSHMEM/SHM |
| **5. GPU Command Submission** | Clean GuC CTB (`H2G`/`G2H`) or Gen12 RCS0/BCS0 ring descriptor (`gen12_rcs0_ring_*`), ~400–800 LOC | Full `i915` execlist + GuC submission state machine + `i915_request` dependency graph (`~40k+` LOC) | Native upstream Intel Media/Vulkan/OpenGL driver in VM (`mesa-iris` / `anv`) |
| **6. Display Engine & eDP Modesetting** | **Practical:** Preserve BIOS/GOP transcoder (`eDP-1`) & flip `PLANE_SURF_1_A` (`0x7019c`) / `PIPE_CONF_A` (`0x70008`), ~600 LOC for safe KMS flip | Full `intel_dp` + `VBT` + `CDCLK` + `DPLL` + `PSR` + `DRRS` atomic pipeline (`~80k` LOC of fragile kernel glue) | **Severely limited on laptop eDP:** MUX-less laptops wire internal `eDP-1` directly to Iris Xe; dedicating iGPU to MicroVM blacks out DRY host GOP display unless using Type-1/GVT-d mediated SR-IOV |
| **7. DMA Isolation** | Per-device VT-d `SLPT` via `SYS_DMA_CTL` (`IOVA = 0xc000_0000`), `clflush` sync (`!is_device_coherent`) | Same via `libdry-lx-compat` DMA hooks (but risk of unshimmed `dma_alloc_coherent` leaks) | Requires IOMMU isolation for VFIO (blocked while host owns active GOP) |
| **8. DRY GPU Service Integration** | Native `GpuBackend` implementation (`probe`, `modeset`, `alloc_surface`, `present` with 12x16 cursor, `wait_fence`) | Wrapped via `GPU_OP_*` IPC bridge (`lx-drm-bochs` pattern) | Requires `dry-gpu-svc` SHM/triple-buffer compositor bridge (`T1..T5` IPC) |
| **9. Maintenance Cost** | Low for Display/2D/GGTT (`< 1,500 LOC`), high only if writing native 3D compiler/Mesa backend | **Prohibitive:** tracking upstream `v6.18` `i915`/`xe` internal refactors breaks ratusan shim stubs per release | Lowest for 3D (`Mesa` + upstream Linux kernel updates free), medium VM boot/IPC glue |
| **10. Feasibility on Lenovo 82K1** | **High** for native scanout/flip/GuC-init (`Step 1` feasible right now) | **Poor:** violates DRY OS principle against fabricating hundreds of fake Linux kernel symbols | **High for compute/3D offscreen rendering**; weak as primary boot display owner on MUX-less eDP |
| **11. Security & Performance** | Ring-3 compartment + `IRQF_ONESHOT` + VT-d `SLPT`; zero VM-exit overhead on eDP page flip (`< 1 us`) | Large Ring-3 C TCB (`~150k` LOC kernel C in one island) | Strong VM isolation for untrusted shader JITs, `~0.1–2.0 ms` IPC overhead with dirty-tile batching |

---

## 2. Architecture Selection & Rationale

- **Rejected:** **Approach B (Full `i915`/`xe` Source-Compat Driver Island)** — Passing `i915` or `xe` from Linux `v6.18` requires fabricating hundreds of non-trivial Linux subsystems (`drm_atomic`, `ttm`, `i2c`, `workqueue_struct`, `dma_resv`, `acpi_evaluate`, `rcu`, `tracepoints`), directly violating Project Ascension 7.0's core rule against fake Linux symbol bloat. `libdry-lx-compat` remains scoped strictly to simple self-contained drivers (`virtio-rng`, `bochs-drm`, `8139cp`).
- **Selected Hybrid Architecture:**
  1. **Primary Display & Scanout (Approach A — Native Minimal DRY Gen12 Driver `userspace/src/intel_tgl_prep.rs` -> future `dry-intel-xe`):** Minimal native Ring-3 Rust driver owns `8086:9a49` `GTTMMADR` (`16 MiB`), `GMADR` (`256 MiB`), preserves UEFI GOP active `eDP-1` (`1920x1080`) timings without risky blind PLL/VBT resets on initial bring-up, maps `SurfaceBufferSet` surfaces via VT-d-isolated `GGTT` PTEs, flips `PLANE_SURF_1_A`, and handles `DE_MASTER_IRQ` via `IRQF_ONESHOT`.
  2. **3D / Vulkan / OpenGL Acceleration (Approach C — MicroVM / Paravirtualized Offload):** Whenever full `Mesa Iris/ANV` 3D shaders are needed without writing a custom compiler, run Linux userspace/DRM in an isolated MicroVM or paravirtualized backend (`VirtIO-GPU 3D` / shared SHM ring, Milestone F) composited onto the native Iris Xe `eDP-1` scanout.

---

## 3. Non-Destructive Preparation Implemented (`userspace/src/intel_tgl_prep.rs`)

Implemented in `userspace/src/intel_tgl_prep.rs` (zero MMIO writes, zero fake Linux symbols):

1. **PCI & BAR Contract (`validate_tgl_pci_bars`):** Verifies `8086:9a49`, `PCI_DEV_FLAG_IOMMU_ACTIVE`, BAR0 `GTTMMADR` (`64-bit MMIO`, non-prefetchable, `>= 16 MiB`, covering `GGTT` at `+8 MiB..+16 MiB`), and BAR2 `GMADR` (`64-bit prefetchable >= 256 MiB`).
2. **Gen12 Typed Register Map:** Declares verified Tiger Lake offsets (`FORCEWAKE_RENDER` `0xA278`, `FORCEWAKE_MT` `0xA188`, `DE_MASTER_IRQ` `0x44400`, `PIPE_CONF_A` `0x70008`, `PLANE_SURF_1_A` `0x7019c`, `TRANS_CONF_EDP` `0x7f008`, `RCS0_RING_*` `0x2030..0x203c`).
3. **Intel CSS Firmware Header Parser & Streaming Verification (`parse_intel_css_header` + `FwStreamVerifier`):** Parses `module_type == 6`, `header_len >= 33` dwords (`132 B`), `module_vendor == 0x8086`, bounds-checks `total_bytes`, and integrates chunked SHA-256 tamper rejection.
4. **64-bit GGTT PTE Encoder & Ring Contract (`encode_ggtt_pte`, `validate_ring_submit`):** Enforces 4 KiB alignment and `<= 48-bit` IOVA bounds matching VT-d domain width, plus power-of-two ring sizing for `RCS0_RING_CTL`.
5. **IRQ & `dry-gpu-svc` Handover Contract (`tgl_default_irq_contract`, `tgl_gpu_svc_contract`):** Enforces `IRQF_ONESHOT` MSI binding, `1920x1080 @ 165 Hz` mode descriptor, and `DisplayHandoverState::FallbackGop` preservation on driver fault.

---

## 4. Verification Status

| Check | Result | Scope & Honesty Tag |
| :--- | :---: | :--- |
| `userspace/src/intel_tgl_prep.rs` unit self-test inside live QEMU `dry-display` | **PASS** (`[DRY-INTEL-PREP] PASS: dev=8086:9a49 bars_ok=1 css_ok=1 fw_tamper_rej=1 ggtt_ok=1 irq_ok=1 status=READY_FOR_HW_VAL`) | `VERIFIED_QEMU` (contract logic in Ring-3 ELF; no physical `8086:9a49` present in QEMU) |
| Physical Tiger Lake `GTTMMADR` MMIO read/forcewake, GuC DMA upload, `eDP-1` plane flip, render ring execution | **Not executed** (requires bare-metal Lenovo `82K1` operator boot) | **`PARTIAL` / `READY FOR HARDWARE VALIDATION`** |
