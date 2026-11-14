# DRY OS — Project Ascension 4.0 Experiment & Verification Ledger

> **Date:** 2026-11-14 | **Baseline:** `20f0561` (`DRY Glass 2.0`, `101/101`) | **Branch:** `main`  
> **Scope:** Tracks A & B (`DRY Glass 2.1`, Commit `2ed966b`), Track C (`libdry-lx-compat` + `v6.18` `bochs.c`), Track D (`8086:9a49` & `10de:25a0`), Track E (Live USB xHCI Builder), and Full Regression Gate (`105/105` + `5/5`).

---

## 1. Reproducible Verification Commands

```bash
python3 tools/lx_compat/lx_driver_manager.py verify --all      # 54 upstream SHA-256 checks
python3 -m unittest tools/lx_compat/test_lx_driver_manager.py   # 4/4 manager unit tests (.ko reject, cache, pin)
python3 tools/lx_compat/validate_target_gpus.py                 # Read-only Iris Xe & RTX 3050 Ti + OpenRM header compile
python3 tools/qemu_boot_test.py --p11-only                      # DRY Glass 2.1 multi-res/DPI suite (3/3 + 9 PNGs)
python3 tools/qemu_boot_test.py --p12-only                      # Linux v6.18 bochs.c DRM island E2E + 2 PNGs
python3 tools/build_live_usb_image.py --verify-boot             # 96 MiB FAT32 USB build + QEMU xHCI boot test
python3 tools/qemu_boot_test.py --p3-matrix                     # 5/5 SMP (1..20 vCPU) & RAM (256M..2G) matrix
python3 tools/qemu_boot_test.py                                 # Full 105/105 deterministic QEMU/KVM regression suite
```

---

## 2. Quantitative Results by Track

### 2.1. Tracks A & B — DRY Glass 2.1 (`--p11-only`, [`results_dry_glass_2_1.json`](../tools/compat_bench/results_dry_glass_2_1.json))

| Suite | Resolution / Configuration | Verified Behaviors | `leaked_frames` | Wall Time |
|---|---|---|:---:|:---:|
| `p11a_glass21_1280x800_grouped_overview_dpi` | `1280x800`, `100/125/150%` DPI, Daylight + HC | `windows_opened=7`, `Notes 2` grouping + `+2` strip overflow (`<= 35%`), `Alt+W` overview + `note` filter, `irq12=5` | `0` | `8.164s` |
| `p11b_glass21_1024x768_narrow_150dpi` | `1024x768` Narrow Tiled, `150%` DPI | Compact `IconRail` sidebars, clipped KPI `metric_card`s & file table | `0` | `3.620s` |
| `p11c_glass21_1920x1080_overview_tiled` | `1920x1080` Tiled + Overview | 5-app workspace + centered blurred overview | `0` | `5.128s` |

### 2.2. Track C — `libdry-lx-compat` & Linux `v6.18` `bochs.c` Island (`--p12-only`, [`results_p12_lx_compat.json`](../tools/compat_bench/results_p12_lx_compat.json))

- **Upstream Driver:** Unmodified `v6.18` `drivers/gpu/drm/tiny/bochs.c` (`829` lines, `sha256=93f5c4bc...`) in `dry-lx-drm-bochs.elf` (`60K`, `0x0212`) beside primary GOP desktop (`display_pci_funcs_visible = 2`, `managed_slot = 6`).
- **Security & Capability Boundaries:** `ro_dmabuf_enforced = true`, `kernel_dma_denied = 5`, `ioport_denied = 2`, `unauth = 5`, `leaked_frames = 0`.
- **Latencies (`3113 MHz` TSC):** Tier-1 `COMMIT_DAMAGE` `p50/p95/p99 = 57,016 / 68,050 / 73,963 cyc` (`18.315 us` median); Tier-2 `IoRing` 32 flushes = `1,612,765 cyc` (`50,398.9 cyc/flush`); `#UD` (`vec=6`) recovery (`BME=0` `kernel_pci_quiesces = 3`, `gen=2`) = `19,000,880 cyc` (`6.104 ms`).
- **Artifacts:** [`p12_bochs_drm_scanout.png`](screenshots/glass21/p12_bochs_drm_scanout.png) (`800x600`, `14` colors), [`p12_gop_desktop_coexist.png`](screenshots/glass21/p12_gop_desktop_coexist.png) (`1280x800`, `32` colors).

### 2.3. Track D — Target Hardware GPU Validation ([`results_target_gpus_a4.json`](../tools/compat_bench/results_target_gpus_a4.json))

- **Intel Iris Xe (`0000:00:02.0`, `8086:9a49`, `i915`):** `16 MiB` BAR0 (`0x6104000000`, `>4G`), `256 MiB` BAR2 (`0x4000000000`, prefetchable `>4G`), `64 B` I/O BAR4, `15` `tgl_*` blobs, catalog match `intel_i915_tgl`.
- **NVIDIA RTX 3050 Ti Mobile (`0000:01:00.0`, `10de:25a0`, `nvidia`, class `0x30200`):** `16 MiB` BAR0, `4 GiB` BAR1 (`0x6000000000`, `>4G`), `32 MiB` BAR3 (`0x6100000000`, `>4G`), `128 B` I/O BAR5, `gsp_ga10x.bin` (`27.04 MB`) + `ucodes_ga10x.bin` (`58.22 MB`), `-Werror` shim compile `fallback_build_ok = true` & `nvidia_header_build_ok = true` (`/usr/src/nvidia-615.78.08/kernel-open/common/inc`).

### 2.4. Track E & Kernel Invariants ([`results_live_usb_boot.json`](../tools/compat_bench/results_live_usb_boot.json))

- **Live USB xHCI UEFI Boot:** `kernel/build/dryos-ascension4-live-usb.img` (`96 MiB` MBR (`0xEF`) FAT32, `sha256=478f3901826b246eabc2fcce136e5a7d42e57fc0e5fb9b09d507a69e12c4dc34`, `24` modules + `dry-kernel` + multi-profile `limine.conf`) booted strictly via QEMU `qemu-xhci + usb-storage` -> `exit_code = 33`, `leaked_frames = 0` (`4.54s`).
- **Kernel Invariants:** `rust_syscall_dispatch = 248 B` (`0xf8` <= `256 B`), `Syscall ABI v0.1` (`0..=31`) unchanged, `0` Linux/GPL code in `dry-kernel`.
