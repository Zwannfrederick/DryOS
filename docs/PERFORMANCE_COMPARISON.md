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
| **`T0` Python Copied SHM + Pipe + `zlib.crc32`** (`proto_bench.py`) | `16.59 MB` (2x) | `8,101.00` | `12,600.22` | `13,861.99` | `9,162.22` | `863.34` | `98.4%` | `57,312` | **NO** (`1.34x–2.08x` over) |
| **`T1` Native C Double-Buf Single-Copy SHM + `eventfd`** | `8.29 MB` (1x) | `456.33` | `562.75` | `618.94` | `469.31` | `16,776.39` | `99.8%` | `42,368` | **YES** (`17.8x` faster `p50`) |
| **`T2` Native C Triple-Buf Async Ring + `eventfd`** | `8.29 MB` (1x) | `406.41` | `478.89` | `528.68` | `417.03` | `18,877.44` | `106.5%` | `42,444` | **YES** (`19.9x` faster `p50`) |
| **`T3` Dirty-Rect Batch `5%` Damage + `eventfd`** | `417,792 B` | `19.30` | `23.04` | `37.09` | `20.16` | `388,832.26` | `107.6%` | `42,104` | **YES** (`0.4%` of budget) |
| **`T3` Dirty-Rect Batch `15%` Damage + `eventfd`** | `1,245,184 B` | `66.75` | `73.31` | `87.56` | `67.83` | `116,030.15` | `102.4%` | `42,376` | **YES** (`1.2%` of budget) |
| **`T3` Dirty-Rect Batch `25%` Damage + `eventfd`** | `2,076,672 B` | `108.09` | `119.45` | `153.77` | `110.21` | `71,408.34` | `101.4%` | `42,352` | **YES** (`2.0%` of budget) |
| **`T4` Recycle Pool + Spin-Then-Futex (Full 1080p)** | `8.29 MB` (1x) | `406.08` | `1,148.41` | `1,740.97` | `536.06` | `14,695.51` | `104.0%` | `42,224` | **YES** (`p50` `19.9x` faster) |
| **`T4b` Recycle Pool + Spin-Then-Futex (`15%` Dirty)** | `1,245,184 B` | `92.14` | `183.75` | `244.62` | `105.19` | `74,514.20` | `108.7%` | `42,224` | **YES** (`3.0%` of budget) |
| **`T5` Shared-Pool Slot-Index Handoff** *(host SHM only)* | **`0 B` pixel copy** | `0.06` | `0.27` | `0.53` | `0.15` | `40,999,273.28`* | `163.0%` | `34,244` | **YES** (`0 B` copy) |

*\*Note on `T5`: Effective throughput divides `8.29 MB` logical display frame size by slot-descriptor handoff latency (`0.06 us`); actual pixel `memcpy` bandwidth consumed is `0 B/s`.*

---

## 3. Bottlenecks Eliminated & Honest Zero-Copy Boundaries

1. **Where the Ascension 6.0 `8.2 ms` Bottleneck Actually Came From:**
   The Ascension 6.0 `proto_bench.py` baseline (`T0`) missed the 165 Hz `6.06 ms` deadline **not** because memory bandwidth on this host cannot transfer 1080p frames at 165 Hz, but because Python copied the `8.29 MB` frame **twice** (`producer->shm`, `shm->consumer`) and ran synchronous `zlib.crc32` twice per frame over blocking pipes (`863 MiB/s`).
2. **Real Native Single-Copy & Async Ring Performance (`T1`, `T2`, `T3`):**
   Moving to native compiled single-copy shared memory (`T1` `p50=456 us`) and triple-buffered pipelining (`T2` `p50=406 us`, `p95=479 us`) brings full-screen `1080p` transfer down to **<8% of the `6,060.6 us` 165 Hz budget**, while dirty-rectangle batching (`T3`) drops typical UI updates (`5%–15%`) to **`19–67 us`**.
3. **Important Architectural Honesty on `T5` vs. Cross-VM DMA-BUF:**
   - **Native DRY Ring-3 IPC (`dry-display` <-> `dry-gpu`):** Uses capability-mapped shared 2 MiB frames (`back_caps` `USER_RO`), eliminating duplicate userland staging inside native DRY services.
   - **Host-Process POSIX SHM Zero-Copy (`T5`):** Demonstrates `64 B` lock-free slot descriptor handoff (`0.06 us` p50, `34.2 MiB` RSS) between two host processes mapping the same `MAP_SHARED` pool (`VERIFIED` in host prototype).
   - **Cross-VM Guest `DMA-BUF` / Physical GPU Page-Flip:** **Remains `PROTOTYPE` / `UNSUPPORTED`** until a guest MicroVM shares KVM `ivshmem`/`vhost-user-gpu` `memfd` backing directly into DRY OS's display compositor without an intermediate host `memcpy`.
