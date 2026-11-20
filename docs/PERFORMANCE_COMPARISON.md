# DRY OS — Project Ascension 7.0: GPU IPC & Display Performance Comparison

**Milestone:** F (`GPU IPC ve Performans Optimizasyonu`)  
**Target Budget:** `1920x1080x4` (`8,294,400 B`) at **`165 Hz` = `6.06 ms` (`6,060.61 us`)** per frame  
**Benchmark Harness:** [tools/microvm_gpu_proto/ipc_bench_a7.c](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/tools/microvm_gpu_proto/ipc_bench_a7.c) + [tools/microvm_gpu_proto/run_a7_perf_suite.py](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/tools/microvm_gpu_proto/run_a7_perf_suite.py)

---

## 1. Real In-OS Ring-3 Optimization: Horizontal Dirty-Rect Clipping

Previously (`Ascension 6.0`), [userspace/src/bin/dry_gpu.rs](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/userspace/src/bin/dry_gpu.rs) extracted only `(y0, h0)` from `GPU_OP_PRESENT_RECT`, ignoring `(x0, w0)` and always blitting + DMA-flushing full scanline width (`0..width`).

In Ascension 7.0 Milestone F:
- Added `VirtioGpu2d::transfer_and_flush_rect(x, y, w, h)` in [userspace/src/virtio_gpu.rs](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/userspace/src/virtio_gpu.rs) with spec-compliant byte offset `((y * width) + x) * 4`.
- Updated `blit_ro_backbuf_to_scanout` and `GPU_OP_PRESENT_RECT` in [userspace/src/bin/dry_gpu.rs](file:///home/zwannfrederick/Masa%C3%BCst%C3%BC/Sektor/Coding/dryos/userspace/src/bin/dry_gpu.rs) to copy only `[x_start..x_end)` per row and intersect the 12x16 software cursor horizontally inside `[x_start..x_end)` (falling back to full width only when `blanked || req_w == 0`).

### Measured QEMU/KVM Before-vs-After (`--gpu-only` @ `1280x800` + `--p9-only` @ `1920x1080` / `1024x768`)

| Metric (QEMU Serial Telemetry) | Before (`transfer_and_flush_rows`, full width) | After (`transfer_and_flush_rect`, `[x0..x0+w0)`) | Improvement |
| :--- | :---: | :---: | :---: |
| `p7a` (`1280x800`) Dirty Pixels / Present (`[DRY-SHELL] PRESENT`) | `491,520 px` (`1280x384`) | `224,256 px` (`584x384`) | **-54.4% pixels** |
| `p7a` Compositor Present `p50` (`[DRY-WM-BENCH] pres2w`) | `1,598,765 cyc` (`~513.6 us`) | `1,198,974 cyc` (`~385.2 us`) | **-25.0% latency** |
| `p7a` Compositor Present `p95` (`[DRY-WM-BENCH] pres2w`) | `2,105,356 cyc` (`~676.3 us`) | `1,511,537 cyc` (`~485.6 us`) | **-28.2% p95 latency** |
| `p9b` (`1920x1080`) Dirty Pixels / Present | `Full-row clamp` (`>1.36M px`) | `442,255 px` (`~21.3%` of screen) | Clipped scanout + pixel-exact PNG |
| Visual Integrity (`_ppm_to_png_and_verify` + `leaked_frames`) | `colors=32`, `leaked_frames=0` | `colors=32`, `leaked_frames=0` | Preserved (`--gpu-only` & `--p9-only` PASS) |

---

## 2. MicroVM / Service GPU IPC Multi-Tier Benchmark (`1920x1080x4` vs `165 Hz`)

All tiers were measured back-to-back on the **same Linux host**, **same `1920x1080x4` (`8,294,400 B`) logical frame resolution**, across two isolated OS processes communicating via shared memory + synchronization primitives (`tools/compat_bench/results_gpu_ipc_a7.json`). Every tier enforces per-frame pixel/tag verification (`all_integrity_ok = true`).

| Tier & Mechanism | Copied / Frame | `p50` (us) | `p95` (us) | `p99` (us) | `mean` (us) | Effective 1080p Throughput (MiB/s) | CPU Util (2-proc) | Max RSS (KiB) | Within 165 Hz (`6,060.61 us` `p95`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| **`T0` Python Copied SHM + Pipe + `zlib.crc32`** (`proto_bench.py`) | `16.59 MB` (2x) | `8,250.75` | `8,691.71` | `8,788.09` | `8,315.79` | `951.22` | `99.5%` | `57,240` | **NO** (`1.36x–1.43x` over) |
| **`T1` Native C Double-Buf Single-Copy SHM + `eventfd`** | `8.29 MB` (1x) | `508.05` | `645.69` | `697.82` | `515.47` | `15,292.35` | `100.3%` | `42,096` | **YES** (`16.2x` faster `p50`) |
| **`T2` Native C Triple-Buf Async Ring + `eventfd`** | `8.29 MB` (1x) | `435.20` | `576.84` | `695.30` | `458.40` | `17,165.43` | `103.6%` | `42,444` | **YES** (`19.0x` faster `p50`) |
| **`T3` Dirty-Rect Batch `5%` Damage + `eventfd`** | `417,792 B` | `19.20` | `31.14` | `43.35` | `20.93` | `374,566.34` | `106.4%` | `42,444` | **YES** (`0.5%` of budget) |
| **`T3` Dirty-Rect Batch `15%` Damage + `eventfd`** | `1,245,184 B` | `67.53` | `90.50` | `108.60` | `71.65` | `109,833.91` | `102.1%` | `42,444` | **YES** (`1.5%` of budget) |
| **`T3` Dirty-Rect Batch `25%` Damage + `eventfd`** | `2,076,672 B` | `109.88` | `141.49` | `151.72` | `115.32` | `68,281.23` | `101.4%` | `42,224` | **YES** (`2.3%` of budget) |
| **`T4` Recycle Pool + Spin-Then-Futex (Full 1080p)** | `8.29 MB` (1x) | `568.01` | `689.79` | `756.09` | `564.54` | `13,949.32` | `102.9%` | `42,160` | **YES** (`p50` `14.5x` faster) |
| **`T4b` Recycle Pool + Spin-Then-Futex (`15%` Dirty)** | `1,245,184 B` | `62.59` | `75.23` | `89.28` | `64.75` | `121,208.09` | `110.0%` | `42,224` | **YES** (`1.2%` of budget) |
| **`T5` Shared-Pool Slot-Index Handoff** *(host SHM only)* | **`0 B` pixel copy** | `0.24` | `0.31` | `0.34` | `0.24` | `28,687,172.71`* | `196.3%` | `34,560` | **YES** (`0 B` copy) |

*\*Note on `T5`: Effective throughput divides `8.29 MB` logical display frame size by slot-descriptor handoff latency (`0.24 us`); actual pixel `memcpy` bandwidth consumed is `0 B/s`.*

---

## 3. Bottlenecks Eliminated & Honest Zero-Copy Boundaries

1. **Where the Ascension 6.0 `8.2 ms` Bottleneck Actually Came From:**
   The Ascension 6.0 `proto_bench.py` baseline (`T0`) missed the 165 Hz `6.06 ms` deadline **not** because memory bandwidth on this host cannot transfer 1080p frames at 165 Hz, but because Python copied the `8.29 MB` frame **twice** (`producer->shm`, `shm->consumer`) and ran synchronous `zlib.crc32` twice per frame over blocking pipes (`951.2 MiB/s`).
2. **Real Native Single-Copy & Async Ring Performance (`T1`, `T2`, `T3`):**
   Moving to native compiled single-copy shared memory (`T1` `p50=508.05 us`) and triple-buffered pipelining (`T2` `p50=435.20 us`, `p95=576.84 us`) brings full-screen `1080p` transfer down to **<10% of the `6,060.61 us` 165 Hz budget**, while dirty-rectangle batching (`T3`) drops typical UI updates (`5%–15%`) to **`19.20–67.53 us`** (p95 `31.14–90.50 us`).
3. **Important Architectural Honesty on `T5` vs. Cross-VM DMA-BUF:**
   - **Native DRY Ring-3 IPC (`dry-display` <-> `dry-gpu`):** Uses capability-mapped shared 2 MiB frames (`back_caps` `USER_RO`), eliminating duplicate userland staging inside native DRY services.
   - **Host-Process POSIX SHM Zero-Copy (`T5`):** Demonstrates `64 B` lock-free slot descriptor handoff (`0.24 us` p50, `34.56 MiB` RSS) between two host processes mapping the same `MAP_SHARED` pool (`VERIFIED` in host prototype).
   - **Cross-VM Guest `DMA-BUF` / Physical GPU Page-Flip:** **Remains `PROTOTYPE` / `UNSUPPORTED`** until a guest MicroVM shares KVM `ivshmem`/`vhost-user-gpu` `memfd` backing directly into DRY OS's display compositor without an intermediate host `memcpy`.

---

## 4. Project Ascension 8.0 System & Hardware-Validation Benchmarks (`VERIFIED_QEMU`)

> [!IMPORTANT]
> **Measurement Scope Boundary (`hardware_verified = false`):** All timings and counts in this section were measured on **QEMU/KVM + OVMF** (and Mesa software `llvmpipe` for `p17d` Virgl). Physical bare-metal benchmarks on the target **Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`)** remain **`IMPLEMENTED_UNTESTED`** until executed manually by the operator.

### 4.1 HWVAL Live USB Persistence, Driver Island SDK & NVIDIA Level-1 Gate Timings

| Workload / Suite (`ascension-8.0`) | Observed Duration | Key Measured Throughput / Invariants | Source |
| :--- | :---: | :--- | :--- |
| **HWVAL 256 MiB Live USB `--verify-boot`** | **`7.316 s`** | xHCI + VT-d boot, passive check (`pass=20 fail=0 skip=2 blocked=2`), 13-file FAT32+LFN write + read-back SHA-256 + `fsck.fat -n`, `esp_and_persist_untouched=true`, `leaked_frames=0` | [results_hwval_usb_boot.json](../tools/compat_bench/results_hwval_usb_boot.json) |
| **`p18a` Safe Auto Diagnostics & Canaries** | **`12.84 s`** | `15` `dry` CLI commands, `2` persisted sessions (`sess-0001..0002`, `13` files each), `2` canary disks (`NVMe` & `VirtIO-Blk`) byte-identical | [results_p18_hwval.json](../tools/compat_bench/results_p18_hwval.json) |
| **`p18b` Recovery Mode & Multi-Session** | **`12.79 s`** | `RECOVERY` RO inspection (`DRYOS_LOGS` SHA-256 unchanged) + `sess-0003` clean append | [results_p18_hwval.json](../tools/compat_bench/results_p18_hwval.json) |
| **`p18c` Negative Persistence/HW Fault Matrix (`9/9`)** | **`61.33 s`** | Missing P3, write-fail, wrong GPT GUID, corrupt BPB, full FAT32, USB disconnect, bad ACPI, bad PCI BAR, no-IOMMU fail-closed (`9/9 PASS`) | [results_p18_hwval.json](../tools/compat_bench/results_p18_hwval.json) |
| **`p18d` M2 IPC/Capability Security Probe** | **`10.02 s`** | Post-hardening IPC donation/quiesce/DMA checks, `leaked_frames=0`, NVMe untouched | [results_p18_hwval.json](../tools/compat_bench/results_p18_hwval.json) |
| **`p18e` Driver Island ABI v1 SDK Lifecycle** | **`7.41 s`** | `dry-sdk-demo.elf` (`#28/32`): `ISLAND_ABI_VERSION_V1 = 0x0001_0000` handshake, mismatch reject, `#UD` `crash_rec_gen=2`, `QUIESCE_UNLOAD` | [results_p18_hwval.json](../tools/compat_bench/results_p18_hwval.json) |
| **`p18f` NVIDIA Level-1 (`nvidia_exp`) E2E** | **`10.18 s`** | `7` GPU/NVIDIA CLI commands, `14` prereqs (`met=4`, honest `BLOCKED` / `N/A`), `7+1` `nvidia_exp/` FAT32 files + `fsck.fat -n` clean | [results_p18_hwval.json](../tools/compat_bench/results_p18_hwval.json) |
| **`p18nv` Safe Gate + `p18g` (`6/6`) NVIDIA Faults** | **`60.87 s`** | `dry-nv-island.elf` unspawned in safe mode + `6/6` fault tests (`nv1_no_gpu .. nv6_unauth_cap`) | [results_p18_hwval.json](../tools/compat_bench/results_p18_hwval.json) |
| **`dry-driver-sdk` Host Unit Suite (`5/5`)** | **`< 0.01 s`** | `abi_hello`, `lifecycle_transitions`, `mmio_window_bounds`, `dma_region_bounds`, `ring_validation` (`5/5 PASS`) | `cargo test` |

### 4.2 Core Microkernel, SMP Scalability & Island Regressions (Ascension 8.0 Final Validation)

| Gate / Benchmark | Result | Measured Numbers | Target / Limit |
| :--- | :---: | :--- | :---: |
| **Syscall Hot-Path Stack Frame (`dry-kernel`)** | **PASS** | `rust_syscall_dispatch`: `sub $0xf8,%rsp` = **`248 B`** | `<= 256 B` |
| **Driver Island `2C-0` IPC & Recovery (`results_poc_2c0_island.json`)** | **PASS** | Direct leased EP `d_p50 = 5,769 cyc` vs Broker `b_p50 = 11,354 cyc` (`1.97x`), `b32_p50 = 843 cyc/op`, `#UD` recovery `p50 = 1,416 us`, `pmm_leak = 0` | `leaked_frames = 0` |
| **HW Foundations `2C-2` (`p8_hw_foundations_latest.json`)** | **PASS** | `6.324 s`, `msi_bound_p50 = 70,857 cyc` (`22.75 us`), `wc_64k_cyc = 48,167`, `uc_64k_cyc = 16,664`, `vtd_rogue_faults = 1`, `kernel_pci_quiesces = 5`, `leaked_frames = 0` | `leaked_frames = 0` |
| **SMP / RAM Scaling Matrix (`--p3-matrix`)** | **`5 / 5 PASS`** | `1cpu/256M` **`5.09 s`** (`hlt=567`) · `2cpu/256M` **`4.17 s`** (`hlt=1,249`) · `4cpu/512M` **`5.17 s`** (`hlt=2,578`) · `8cpu/1G` **`5.26 s`** (`hlt=5,260`) · `20cpu/2G` **`6.63 s`** (`hlt=13,447`), all `leaked_frames = 0` | Linear scaling, `0` leaks |
| **Reliability & Virgl 3D Suite (`--p17-only`)** | **`4 / 4 PASS`** | `p17a` **`5.212 s`** (`gen=1->2->3`, `quiesce=4`) · `p17b` **`5.043 s`** (`persist=0`) · `p17c` **`4.358 s`** (`smp=4`, `vtd_faults=2`, `quiesces=5`) · `p17d` **`15.521 s`** (`Virgl` `llvmpipe`, `px=0xffff0000`, `neg_err=0x1205`) | `leaked_frames = 0` |
| **Full Unified QEMU Regression** | **`164 / 164 PASS`** | `58` Legacy + `9` P1 + `14` P2 + `6` P3 + `3` P4 + `2` 2C-0 + `1` 2C-1 + `1` GPU + `1` 2C-2 + `9` Glass/2.0/2.1 + `1` LxDRM + `25` USB/NVMe + `1` LxNet + `4` Brand + `4` Ascension 6.0 + `4` Ascension 7.0 + `21` Ascension 8.0 | Zero failures, `29/32` slots |
