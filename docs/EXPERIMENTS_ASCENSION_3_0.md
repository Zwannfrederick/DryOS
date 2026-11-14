# DRY OS — Project Ascension 3.0 Deney ve Doğrulama Raporu (`Track A: 2C-2` & `Track B: DRY Glass`)

> **Tarih:** 14 Kasım 2026 | **Taban Commit (`Track A`):** `8d230df` (`Milestone 2C-2` + `2A-4a` P1 Kapanışı) | **Bu Aşama (`Track B`):** `DRY Glass` Desktop (`98/98` QEMU + `5/5` Matris DOĞRULANDI)  
> **Ham Telemetri:** [`tools/compat_bench/p8_hw_foundations_latest.json`](../tools/compat_bench/p8_hw_foundations_latest.json), [`tools/compat_bench/results_p9_glass.json`](../tools/compat_bench/results_p9_glass.json)  
> **Not:** Tüm ölçütler QEMU/KVM (`Intel Core i5-11300H`, `~3.114 GHz` TSC) üzerinde çalıştırılmıştır; `QEMU/KVM != bare-metal`.

---

## 1. Track A — Donanım Temelleri ve `2A-4a` P1 Kapanışı (`Milestone 2C-2` / `8d230df`)

[`tools/qemu_p8_hw_validators.py`](../tools/qemu_p8_hw_validators.py) (`p8a_hw_foundations_vtd_msi_bar64_e2e`) ve [`userspace/src/bin/dry_hw_test.rs`](../userspace/src/bin/dry_hw_test.rs) ile emüle Intel `VT-d` IOMMU (`-device intel-iommu`), QEMU `edu` (`MSI`), ve `ivshmem-plain` (`>4 GiB` `BAR2`) üzerinde doğrulanmıştır (`tsc_mhz = 3114`, `wall_time_sec = 3.812`):

| Alt Sistem | Çekirdek / Userspace Modülü | Ölçülen / Kanıtlanan Çıktı |
|---|---|---|
| **ACPI `DMAR` & `VT-d` `SLPT`** | [`kernel/src/iommu/{dmar,slpt,vtd,tests}.rs`](../kernel/src/iommu/vtd.rs), [`kernel/src/pci.rs`](../kernel/src/pci.rs) | `dmar_unit = 5`, BDF bazlı 4-seviye `SLPT`, yetkisiz DMA girişiminde `vtd_rogue_faults = 1`, `dma_rt_cyc = 621,818,287`, çıkış/çöküşte `kernel_pci_quiesces = 3` (`MSI` maske + `BME=0` + `SLPT/IOTLB` scrub). |
| **PCI `MSI`/`MSI-X` (`0x40..=0x5F`)** | [`kernel/src/irq/msi.rs`](../kernel/src/irq/msi.rs), [`kernel/src/idt.rs`](../kernel/src/idt.rs) | `msi_vec = 0x40`, `msi_irqs = 32`, `kernel_msi_irqs = 33`, `kernel_msi_acks = 33`. |
| **64-bit `>4 GiB` `BAR` & `PAT4` `WC`** | [`kernel/src/pci_bar.rs`](../kernel/src/pci_bar.rs), [`kernel/src/pci_window.rs`](../kernel/src/pci_window.rs), [`kernel/src/vspace/pte.rs`](../kernel/src/vspace/pte.rs) | `bar64_above4g = true` (`virtio-blk` `0x7000000000`, `virtio-gpu` `0x7000004000`, `[DRY-BAR64] unit_cases=8`), `wc_64k_cyc = 25,629` / `uc_64k_cyc = 11,297` (`64 KiB` tutarlılık). |
| **`TCB`-Bağlı `Notification`** | [`kernel/src/ipc/bound_notif.rs`](../kernel/src/ipc/bound_notif.rs) | Sıfır ek syscall ile birleşik `IPC+IRQ` beklemesi: **`msi_bound_p50_cyc = 76,037` (`24.42 µs`)**. |
| **`2A-4a` Kalan 6 `P1` Kapanışı** | [`kernel/src/gdt_tss.rs`](../kernel/src/gdt_tss.rs), [`sched/donate.rs`](../kernel/src/sched/donate.rs), `ioring.rs`, `dry_{blk,broker}.rs` | `AUD-K6/K7/K8` (`NMI` `IST2` & `#MC` `IST3` ayrık `8 KiB` yığın, `32 KiB` `boot_stack`), `AUD-U3/U4/U8`, `leaked_frames = 0`, `rust_syscall_dispatch = 248 B` (`0xf8`). |

---

## 2. Track B — `DRY Glass` Masaüstü, `SharedPixelSurface` & 6 Yerel Uygulama (`--p9-only`)

[`tools/qemu_p9_glass_validators.py`](../tools/qemu_p9_glass_validators.py) içinde canlı QMP `gpu0` `screendump`, PS/2 klavye (`send-key`) + fare (`input-send-event`) etkileşimi ve kalıcı `/persist` (`DRYFS_v1`) üzerinden üç çözünürlük doğrulandı:

| Test | Çözünürlük / Donanım | Doğrulanan Akışlar ve Telemetri | `leaked_frames` | Süre |
|---|---|---|:---:|:---:|
| **`p9a_glass_1280x800_apps_and_themes`** | `1280x800x32`, `VirtIO-GPU` (`back_2m=2`) + `VirtIO-Blk` | `Obsidian Dark` ana ekran -> `Alt+Space` aranabilir `Launcher` -> `calc` filtre & çalıştırma -> `dry-calc` (`6 * 7 = 42`, `HEX 0x2a`) -> `Alt+5` `dry-editor` yazım + `Ctrl+S` `/persist/notes.txt` (`118 B`) -> `Quick Settings` (`(1224,14)`) -> `Frosted Daylight` canlı tema geçişi -> `Notification Center` -> Terminal `apps` (`running=4`) + `cat /persist/notes.txt` (`118 B`) -> `irq12=30` | **`0`** | `8.33 s` |
| **`p9b_glass_1920x1080_gpu_fault_recovery`** | `1920x1080x32`, `VirtIO-GPU` (`back_2m=4`, `dma_2m=4`, `VGA.vgamem_mb=32`) + `VirtIO-Blk` | `4/6` uygulama (`Terminal`, `Life Station`, `Files`, `Settings`) + `Alt+T` `SideBySideTiled` -> `gpufault` (`#UD vec=6`) -> canlı `[DRY-WM] GPU_FALLBACK_TO_GOP reason=ep_revoked` -> `[DRY-GPU] LIVE_RECOVERY_OK gen=2 vec=6 w=1920 h=1080`, `pci_quiesce = 3` | **`0`** | `5.01 s` |
| **`p9c_glass_1024x768_compact`** | `1024x768x32`, `VirtIO-GPU` (`back_2m=2`) | Kompakt topbar etiketleri (`Glass`, `1:Trm..6:Clc`), taşmasız floating `Dock` + `Calculator` + `Terminal` odak/Z-sırası (`1024x768`) | **`0`** | `4.64 s` |

---

## 3. Görsel Kanıt Galerisi (`docs/screenshots/*.png`, QMP `gpu0` Canlı Çıktıları)

Tüm PNG çıktıları `screendump (device="gpu0", head=0)` ile üretilmiş ve görsel kalite/kontrast/hizalama yönünden doğrudan incelenmiştir:

| Aşama / Görünüm | Dosya | Boyut | Aktif Pikseller / Renkler |
|---|---|---:|---:|
| **Obsidian Dark Ana Ekran** | [`docs/screenshots/glass_1280x800_dark_home.png`](screenshots/glass_1280x800_dark_home.png) | `1280x800` | `1,022,448 px` / `32` |
| **Aranabilir Buzlu Cam Launcher** | [`docs/screenshots/glass_1280x800_launcher.png`](screenshots/glass_1280x800_launcher.png) | `1280x800` | `1,022,448 px` / `32` |
| **`dry-calc` (`6 * 7 = 42`, `0x2a`)** | [`docs/screenshots/glass_1280x800_calc.png`](screenshots/glass_1280x800_calc.png) | `1280x800` | `1,022,448 px` / `32` |
| **`dry-editor` (`/persist/notes.txt`, `118 B`)** | [`docs/screenshots/glass_1280x800_editor.png`](screenshots/glass_1280x800_editor.png) | `1280x800` | `1,022,448 px` / `32` |
| **Quick Settings (`Obsidian` / `Frosted Blur`)** | [`docs/screenshots/glass_1280x800_quick_settings.png`](screenshots/glass_1280x800_quick_settings.png) | `1280x800` | `1,022,142 px` / `32` |
| **Frosted Daylight Canlı Tema (6+6 Yüzey)** | [`docs/screenshots/glass_1280x800_frosted_daylight.png`](screenshots/glass_1280x800_frosted_daylight.png) | `1280x800` | `818,584 px` / `32` |
| **Notification Center Paneli** | [`docs/screenshots/glass_1280x800_notifications.png`](screenshots/glass_1280x800_notifications.png) | `1280x800` | `818,272 px` / `32` |
| **Full HD 4-Pencere `Tiled` (`Files`+`Settings`)** | [`docs/screenshots/glass_1920x1080_tiled_gpu.png`](screenshots/glass_1920x1080_tiled_gpu.png) | `1920x1080` | `2,071,408 px` / `32` |
| **Full HD `gpufault` Sonrası `gen=2` Kurtarma** | [`docs/screenshots/glass_1920x1080_recovered_gen2.png`](screenshots/glass_1920x1080_recovered_gen2.png) | `1920x1080` | `2,071,408 px` / `32` |
| **`1024x768` Kompakt Topbar & Floating Dock** | [`docs/screenshots/glass_1024x768_compact.png`](screenshots/glass_1024x768_compact.png) | `1024x768` | `785,136 px` / `32` |

---

## 4. Tekrarlanabilir Doğrulama Komutları

```bash
python3 tools/qemu_boot_test.py --build-only      # Clippy (-D warnings) + 23 ELF64 + dry-kernel
python3 tools/qemu_boot_test.py --p8-only         # Track A: Milestone 2C-2 VT-d, MSI/MSI-X, BAR64
python3 tools/qemu_boot_test.py --p9-only         # Track B: DRY Glass 1280x800 / 1920x1080 / 1024x768
python3 tools/qemu_boot_test.py --p3-matrix       # 5/5 SMP (1..20 vCPU) & RAM (256M..2G) matrisi
python3 tools/qemu_boot_test.py                   # Tam deterministik QEMU/KVM regresyon paketi
```
