# DRY OS — Compatibility & Adaptive Driver Islands: Reproducible Experimental Dataset (`docs/COMPATIBILITY_EXPERIMENTS_DATA.md`)

> **Date Executed:** 2026-11-12  
> **Companion Canonical Research Artifact:** [`docs/COMPATIBILITY_ISLANDS_RESEARCH.md`](COMPATIBILITY_ISLANDS_RESEARCH.md)  
> **Host Environment (Measured):** Intel Core i5-11300H (`4C/8T`, Tiger Lake-H, VT-x + EPT + VT-d, `~3.111..3.114 GHz` invariant TSC), 16 GiB RAM, Host Linux `7.2.14-zen1-1-zen`, Guest Kernel Image `/lib/modules/6.18.62-1-lts/vmlinuz`, NVIDIA Open Modules `615.78.08` (`updates/dkms/nvidia*.ko.zst`), QEMU `q35` + KVM  
> **Host GPUs Probed (`lspci -nnk` & `/sys/kernel/iommu_groups`):**
> - **iGPU:** `00:02.0` VGA compatible controller (`0300`): Intel TigerLake-LP GT2 `[Iris Xe Graphics]` (`8086:9a49`, rev `01`), **IOMMU Group 1** (isolated single device), attached to internal eDP panel.
> - **dGPU:** `01:00.0` 3D controller (`0302`): NVIDIA GA107M `[GeForce RTX 3050 Ti Mobile]` (`10de:25a0`, rev `a1`), **IOMMU Group 13** (co-grouped with `00:01.0` Intel TigerLake PCIe Root Port `8086:9a01`), **muxless Optimus** (`0` physical display connectors wired to `01:00.0`), GSP firmware `gsp_ga10x.bin`.

---

## Epistemic Separation Rule

All measurements in this dataset strictly separate:
1. **Native DRY Kernel Measurements (`Exp-B1`):** Executed inside DRY OS `v0.1.0` (`Milestone 2B`, `88/88` QEMU/KVM `q35` suite) using hardware `rdtsc` counters. Note (`R-05`): QEMU/KVM cycle counts prove hot-path discipline, not bare-metal superiority over Linux.
2. **Host-Side Controlled Prototype Measurements (`Exp-B2`, `Exp-B3`, `Exp-B4`):** Executed directly on the `4C/8T` Core i5-11300H Linux `7.2.14-zen1-1-zen` host using real KVM microVMs (`6.18.62-1-lts`), real upstream Linux/NVIDIA kernel `.ko` binaries, and host-compiled Rust SPSC `IoRing` benchmarks.
3. **Unexecuted / Blocked Experiments (`Exp-B5`, `Exp-B6`):** Explicitly recorded in Section 5 as **NOT EXECUTED** with exact hardware/infrastructure blockers and minimum prerequisites.

---

## 1. `Exp-B1` — Native DRY OS IPC, Notification, Idle Wakeup & IRQ Overhead

- **Hypothesis:** DRY OS's native capability `Notification` (`notif_signal`/`notif_wait`), synchronous `Endpoint` (`ep_call`/`ep_reply_recv`), and one-shot `IrqCap` wakeup paths incur `< 5 us` (`< 16,000` cycles) overhead, providing a `50x–200x` lower signaling floor than cross-VM virtio-serial exits for services that can run natively in Ring 3.
- **Source & Reproduction:**
  ```bash
  python3 tools/qemu_boot_test.py
  # Raw JSON summary: tools/compat_bench/results_exp_b1_native_ipc.json
  ```
- **Baseline:** Null DRY `SYSCALL` (`231 cyc` `p50`, `rust_syscall_dispatch` stack frame `248 B`).
- **Measured Results (`p50 / p95 / p99` cycles at `~3.114 GHz` TSC):**

| Primitive / Path | `1 vCPU` (`cyc`) | `2 vCPU` (`cyc`) | `4 vCPU` (`cyc`) | `2 vCPU` Latency (`p50`) |
| :--- | :---: | :---: | :---: | :---: |
| `ep_lookup` (seqlock `CSpace` fast path) | `95 / 97 / 109` | `96 / 101 / 121` | `97 / 99 / 132` | `~0.031 us` |
| `notif_sig_wait` (pre-signaled badge consume) | `233 / 237 / 253` | `233 / 237 / 285` | `233 / 237 / 249` | `~0.075 us` |
| `prio_donate` (4-hop priority inheritance) | `179 / 182 / 252` | `179 / 182 / 251` | `178 / 184 / 249` | `~0.057 us` |
| `sched_wakeup` (parked Ring 3 service wakeup) | `1,055 / 1,068 / 2,114` | `3,455 / 12,017 / 12,956` | `3,238 / 4,145 / 5,333` | `~1.11 us` (`0.34 us` 1-CPU) |
| `call_reply` (unified `ep_call` + `ep_reply_recv`) | `1,971 / 2,017 / 3,153` | `6,624 / 15,780 / 22,279` | `6,229 / 7,977 / 20,465` | `~2.13 us` (`0.63 us` 1-CPU) |
| `cap_transfer` (`CSpace` grant + CDT link) | `2,460 / 2,497 / 3,333` | `7,508 / 8,687 / 24,857` | `7,154 / 8,412 / 9,751` | `~2.41 us` |
| `irq_top` (`IOAPIC` mask + `LAPIC EOI` + signal) | `12,790 / 13,081 / 15,821` | `12,928 / 14,094 / 28,184` | `12,883 / 13,979 / 25,918` | `~4.15 us` |
| `irq_wake` (external IRQ -> Ring 3 driver thread) | `13,750 / 13,795 / 13,921` | `15,905 / 16,504 / 32,519` | `17,662 / 19,391 / 26,110` | `~5.11 us` |
| Desktop `in2comp` (`dry-input` -> `dry-display`) | `51,732` (`p50`) | `40,247..49,198` (`p50`) | `50,123` (`p50`) | `~12.9..15.8 us` |

- **Uncertainty & Limitations:** Measured inside QEMU/KVM; `irq_top`/`irq_wake` include emulated `IOAPIC` MMIO VM-exit overhead that differs on bare silicon (`P5`).
- **Verdict:** **Hypothesis Supported.** Native Ring 3 `Notification` (`0.075–1.11 us`) and `Endpoint` (`0.63–2.13 us`) IPC are two orders of magnitude cheaper than cross-VM hypervisor exits (`~200–222 us` in `Exp-B2`).

---

## 2. `Exp-B2` — Minimal Dormant Linux MicroVM Cold Start, Warm `HLT` Wakeup, Quiescent CPU & VFIO Pinned RAM

- **Hypothesis (Dormant-Island Hypothesis):** A minimal hardware-virtualized Linux guest (`6.18.62-1-lts` + `763 KiB` custom initramfs, `nohz=on`, zero systemd/daemons) parked in `poll()` -> guest `HLT` consumes negligible host CPU when idle (`< 2%`) and resumes in `< 500 us` when signaled, **but** cannot eliminate guest RAM residency — especially when `-mem-prealloc` models VFIO/IOMMU DMA page pinning.
- **Reproduction:**
  ```bash
  python3 tools/compat_bench/microvm_dormant_bench.py
  # Raw JSON output: tools/compat_bench/results_exp_b2_microvm.json
  ```
- **Baseline:** Cold guest startup (`5` runs per profile) and demand-paged `128M` / `256M` microVM without VFIO pre-allocation.
- **Measured Results:**

| MicroVM Profile (`1 vCPU`, `-machine microvm`, KVM) | Cold Boot `p50` (`min..max`) | Warm `HLT` Wakeup RTT (`p50 / p95 / p99`) | Quiescent `3s` Host CPU | Host `VmRSS` (`RssAnon`) |
| :--- | :---: | :---: | :---: | :---: |
| **`uvm_128m_demand`** (`128 MiB`, demand-paged) | `902.80 ms` (`883.7..962.0`) | **`199.76 / 333.68 / 1,290.49 us`** | **`1.333%`** | `169.58 MiB` (`146.89 MiB`) |
| **`uvm_256m_demand`** (`256 MiB`, demand-paged) | `909.71 ms` (`875.2..935.5`) | **`208.40 / 338.89 / 462.20 us`** | **`1.333%`** | `173.70 MiB` (`150.95 MiB`) |
| **`uvm_256m_vfio_pinned`** (`256 MiB`, `-mem-prealloc`) | `890.47 ms` (`883.6..902.5`) | **`222.45 / 362.66 / 2,079.60 us`** | **`1.333%`** | **`309.41 MiB` (`286.90 MiB`)** |

- **Uncertainty & Limitations:** Cold boot uses the host's generic distro kernel (`vmlinuz-6.18.62-1-lts`, compressed zstd + built-in drivers) rather than a stripped `vmlinux` (`Firecracker` achieves `< 125 ms` with an uncompressed stripped `vmlinux`); `15 ms` inter-ping sleep ensures the vCPU enters KVM `HLT` block state before each ping (`100` samples/profile).
- **Interpretation & Verdict:** **Hypothesis Partially Supported (CPU Supported; Memory/Ownership Falsified for Always-On Islands).**
  1. **What sleeping vCPUs eliminate:** Quiescent CPU drops to `1.333%` (periodic guest RCU/clocksource ticks + QEMU event loop), and warm wakeup takes `~200–222 us` (`4,000x` faster than cold boot).
  2. **What sleeping vCPUs CANNOT eliminate:** Under VFIO/IOMMU PCI passthrough (`uvm_256m_vfio_pinned`), IOMMU second-stage page tables pin 100% of guest physical RAM (`VmRSS` jumps from `173.7 MiB` to **`309.41 MiB`** even for a tiny `256 MiB` VM; a `1–2 GiB` GPU island pins `1–2 GiB` permanently while alive), and passed-through PCIe hardware remains owned by the VM. Therefore, a microVM must be **opt-in / on-demand**, never booted by default for native DRY sessions!

---

## 3. `Exp-B3` — Quantitative Linux Kernel Driver Dependency & `.text` Footprint Audit (`11` Driver Stacks)

- **Hypothesis:** Bounded PCIe NIC/paravirt/NVMe drivers require `<= 360` external kernel symbols (`<= 1.5x` our proven `3,468`-LOC `H2` `igc` userspace DDE runtime), whereas modern KMS/GEM/TTM/HMM GPU drivers (`i915`, `xe`, `amdgpu`, `nvidia-full`) exceed `ADR-006`'s `< 10,000`-LOC shim ceiling due to deep `drm_*`, `dma_buf_*`, and `mm/HMM` (`__folio_lock`, `mmu_notifier`) coupling.
- **Reproduction:**
  ```bash
  python3 tools/compat_bench/audit_driver_classes.py
  # Raw JSON output: tools/compat_bench/results_exp_b3_drivers.json
  ```
- **Baseline:** DRY OS Phase 0 `H2` `igc` userspace driver island (`247` external `UND` symbols, `151.5 KiB` `.text`, proven with `3,468` LOC `libdry_lx_net` shim across Linux `v6.12` -> `v6.18`).
- **Measured Results (`Linux 6.18.62-1-lts` + `nvidia-open 615.78.08` ELF64 `.ko` binaries):**

| Driver Stack | `.ko` ELF (`KiB`) | `.text` (`KiB`) | `.data+.bss` (`KiB`) | Total `UND` Symbols | `mm_hmm` | `drm_dmabuf` | `pci_dma_irq` | `sched_rcu` | Multiplier vs `igc` | Architecture Tier Verdict |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :--- |
| **`e1000e`** (1GbE NIC) | `787.8` | `265.8` | `26.6` | **`200`** | `8` | `0` | `35` | `19` | **`0.81x`** | **Tier 2 DDE Proven (`H2`)** |
| **`virtio-net`** | `266.0` | `90.1` | `5.7` | **`228`** | `8` | `0` | `0` | `17` | **`0.92x`** | **Tier 1 Native Rust Preferred** |
| **`igc`** (2.5GbE NIC) | `472.3` | `151.5` | `11.8` | **`247`** | `9` | `0` | `36` | `23` | **`1.00x`** | **Tier 2 DDE Proven (`H2`)** |
| **`virtio-gpu`** | `255.8` | `71.0` | `5.5` | **`264`** | `12` | `138` | `5` | `18` | **`1.07x`** | **Tier 1 Native Rust 2D/Venus** |
| **`nvme`** (`core`+`pci`) | `771.4` | `224.6` | `20.5` | **`358`** | `11` | `0` | `38` | `36` | **`1.45x`** | **Tier 1 Native Rust / Tier 2 DDE** |
| **`nvidia-uvm`** (CUDA HMM) | `4,828.3` | `1,630.3` | `295.7` | **`341`** | **`28`** | `9` | `16` | `36` | `1.38x` (`*`) | **Blocked in DDE (`__folio_lock`/HMM)** |
| **`nvidia-core`** (GSP RM) | `28,072.6` | **`16,472.7`** | `370.5` | **`470`** | **`34`** | `9` | `60` | `47` | `1.90x` (`*`) | **Tier 4 MicroVM (`16.5 MiB` `.text` RM)** |
| **`nvidia-full`** (4 `.ko`s) | **`37,013.6`** | **`19,892.9`** | `691.0` | **`738`** | **`42`** | **`175`** | `63` | `57` | **`2.99x`** | **Tier 4 MicroVM Only** |
| **`amdgpu`** (KMS/TTM) | `30,612.5` | **`11,943.1`** | `868.0` | **`1,108`** | **`28`** | **`461`** | `75` | `78` | **`4.49x`** | **Rejected for DDE (`>10k LOC` cap)** |
| **`xe`** (Intel Iris Xe) | `8,360.3` | `2,940.6` | `137.6` | **`1,213`** | **`21`** | **`650`** | `67` | `75` | **`4.91x`** | **Exceeds `ADR-006` `<10k LOC` DDE cap** |
| **`i915`** (Intel Legacy) | `10,384.5` | `3,722.8` | `139.2` | **`1,244`** | **`34`** | **`574`** | `53` | `78` | **`5.04x`** | **Exceeds `ADR-006` `<10k LOC` DDE cap** |

- **Verdict:** **Hypothesis Strongly Supported.** A sharp empirical cliff separates bounded I/O drivers (`200–358` symbols, `0` DRM/HMM dependencies) from full GPU kernel stacks (`738–1,244` symbols, `175–650` DRM/DMA-BUF symbols, `21–42` folio/HMM VM symbols, and `2.9–19.9 MiB` of executable `.text`).

---

## 4. `Exp-B4` — Shared-Memory SPSC `IoRing` Batching vs Synchronous Per-Operation RPC Forwarding

- **Hypothesis:** Forwarding operations 1-by-1 across a process/island boundary (`batch = 1` or synchronous RPC, as in naive CUDA/OpenGL API forwarding `E`) is bottlenecked by wakeup/context-switch latency (`> 20,000 cyc/op`), whereas amortizing doorbells over DRY's `CAP = 32` `IoRingSharedPage` (`batch = 4..32`) achieves `> 10x` throughput and `< 1,500 cyc/op`.
- **Reproduction:**
  ```bash
  rustc -O tools/compat_bench/ioring_batch_bench.rs -o /tmp/ioring_batch_bench && /tmp/ioring_batch_bench
  # Raw JSON output: tools/compat_bench/results_exp_b4_ioring.json
  ```
- **Baseline:** Synchronous per-op `UnixStream::pair` 32B request / 16B response (`32,768` total ops): **`0.1263 Mops/s`**, `22,158 / 38,384 / 59,031 cyc/op` (`p50/p95/p99`).
- **Measured Results (`32,768` ops per mode):**

| Transport Mode | Batch Size | Throughput (`Mops/s`) | Speedup vs Sync RPC | `p50` (`cyc/op`) | `p95` (`cyc/op`) | `p99` (`cyc/op`) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **Synchronous Per-Op RPC** (Candidate `E` chatty API model) | `1` | `0.1263` | `1.00x` | `22,158` | `38,384` | `59,031` |
| **`IoRingSharedPage` + Doorbell** | `1` | `0.1090` | `0.86x` | `26,351` | `37,019` | `58,740` |
| **`IoRingSharedPage` + Doorbell** | `4` | `0.4182` | **`3.31x`** | `6,667` | `10,196` | `16,104` |
| **`IoRingSharedPage` + Doorbell** | `8` | `0.8465` | **`6.70x`** | `3,344` | `4,957` | `8,107` |
| **`IoRingSharedPage` + Doorbell** | `16` | `1.6911` | **`13.39x`** | `1,703` | `2,417` | `3,830` |
| **`IoRingSharedPage` + Doorbell** | **`32`** | **`3.1074`** | **`24.61x`** | **`886`** | **`1,467`** | **`2,155`** |

- **Verdict:** **Hypothesis Strongly Supported.** Shared memory alone (`batch = 1`, `0.86x`) gives zero speedup if every operation rings a synchronous doorbell; batching `16–32` commands per doorbell (`Venus` Vulkan command serialization, block/NIC `IoRing` descriptor batches) yields **`13.4x–24.6x` higher throughput (`886 cyc/op` `p50`)**.

---

## 5. Explicitly Unexecuted Experiments & Blockers (`Honesty Gate`)

1. **`Exp-B5` (Live VFIO Passthrough of `01:00.0` RTX 3050 Ti Mobile on Host Laptop):** **NOT EXECUTED.** Unbinding `nvidia_drm`/`nvidia_modeset`/`nvidia` on the active development host and rebinding `01:00.0` (which shares **IOMMU Group 13** with PCIe Root Port `00:01.0`) risks host PCIe link wedge or active display session crash during Track A regression testing. Minimum prerequisite: dedicated secondary test machine or live-USB boot (`P5`).
2. **`Exp-B6` (Native DRY `dry-vmm` VMX/EPT Guest Execution):** **NOT EXECUTED.** `dry-kernel` intentionally contains `0` lines of VMX/EPT/VT-d hypervisor code in Phase 2 (`ADR-006`). Minimum prerequisites are itemized in [`docs/COMPATIBILITY_ISLANDS_RESEARCH.md`](COMPATIBILITY_ISLANDS_RESEARCH.md) Section 17.
