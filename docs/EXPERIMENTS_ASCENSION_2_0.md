# DRY OS — Project Ascension 2.0 Deney ve Doğrulama Raporu (`Milestone 2A-4a`, `2C-1` & `Priority C`)

> **Tarih:** 14 Kasım 2026 | **Taban Commit Zinciri:** `8572da3` -> `b24adc7` -> `ac340da` (`2A-4a`), `1345bcd` (`2C-1`), `1879128` (`Priority C`)  
> **Ham Telemetri Dosyaları:** [`tools/compat_bench/results_p6_lx_rng.json`](../tools/compat_bench/results_p6_lx_rng.json), [`tools/compat_bench/results_p7_gpu.json`](../tools/compat_bench/results_p7_gpu.json), [`experiments/h3-gpu-compat/results_h3_gpu.json`](../experiments/h3-gpu-compat/results_h3_gpu.json)

---

## 1. `Milestone 2A-4a` Çekirdek Önkoşul ve Sertleştirme Bulguları

| Alt Sistem / Kanıt | Doğrulanan Mekanizma | Ölçülen / Kanıtlanan Çıktı |
|---|---|---|
| **Boot Modül Kapasitesi (`R-18`)** | [`kernel/src/limine.rs`](../kernel/src/limine.rs), [`userspace/src/vfs_mem.rs`](../userspace/src/vfs_mem.rs) | `MAX_LATCHED_MODULES = 32`, `MAX_BOOT_ENTRIES = 36`, etkin `20` statik ELF64 modülü (`19` `userspace/` + `dry-lx-rng.elf`); taşma reddi etkin. |
| **FPU/SIMD İzolasyonu** | [`kernel/src/fpu.rs`](../kernel/src/fpu.rs), [`userspace/src/bin/dry_fpu_test.rs`](../userspace/src/bin/dry_fpu_test.rs) | İlk `#NM` tuzakta temiz `FCW = 0x037f`, `MXCSR = 0x1f80`, `0` yazmaçlar; `XSAVEOPT64`/`XRSTOR64` + `fpu_mode=fxsave` (`xcr0=0x3`, `area_bytes=512`) yedeği, `corrupt=0`, `leaked_frames=0`. |
| **`FaultEndpoint` İletimi** | [`kernel/src/proc/fault.rs`](../kernel/src/proc/fault.rs), [`userspace/src/bin/dry_fault_test.rs`](../userspace/src/bin/dry_fault_test.rs) | `SPAWN_FLAG_FAULT_EP` (`1 << 48`), çekirdek damgalı `FAULT_MSG_LABEL` (`0x7fa1`), `FAULT_REPLY_RESUME` (`0x7fa2`), `FAULT_BUDGET = 16`, `[DRY-FAULT-EP] PASS: delivered=20 resumed=17 storm_budget=16 leaked_frames=0`. |
| **Eşzamanlılık Denetimi (`AUD-K1..K15`)** | [`kernel/src/audit_tests.rs`](../kernel/src/audit_tests.rs), [`docs/AUDIT_2A4A.md`](./AUDIT_2A4A.md) | `AUD-K1..K5`, `AUD-K9`, `AUD-K13b`, `AUD-K15` + `AUD-U1/U2/U5/U6/U7/U9` kapatıldı; `[DRY-AUDIT-REG] PASS: cases=6/6 notif_rounds=200000 leaked_frames=0`. |
| **Syscall Yığın Bütçesi (`R-11`)** | `objdump -d -M intel .../release/dry-kernel` | `rust_syscall_dispatch = 248 B` (`0xf8` <= `256 B`), `sys_cold_dispatch = 312 B` (`0x138`), `sys_irq_ack = 40 B` (`0x28`), ABI `0..=31` (`+0` syscall). |

---

## 2. `Milestone 2C-1` Gerçek Linux `v6.18` `virtio-rng.c` Driver Island (`P6`)

Değiştirilmemiş upstream Linux `v6.18` (`7d0a66e4bb9081d75c82ec4957c50034cb0ea449`) [`drivers/char/hw_random/virtio-rng.c`](../vendor/linux-drivers/v6.18/virtio_rng/virtio-rng.c) (`sha256=720433a85ce0ce6f84e85842b54d23c7c8e9bad792c66b1066accbaa7fb95a75`) ayrı `GPL-2.0-only` Ring-3 [`dry-lx-rng`](../drivers/lx-virtio-rng-island/Cargo.toml) (`0x0008`) sürecinde QEMU `virtio-rng-pci-non-transitional` (`1AF4:1044`) üzerinde çalıştırıldı (`results_p6_lx_rng.json`, `3000 MHz` TSC):

| Metrik | Ölçülen Değer | Kanıtlanan İddia |
|---|---|---|
| **Gerçek Donanım & Entropi** | `1af4:1044`, `vq_irqs = 35`, `1,792 B`, `ones = 49%`, `32/32` benzersiz `u64` | Mock değil, gerçek split virtqueue DMA tamamlamaları (`random_recv_done`) |
| **Doğrudan 1-Sıçrama `Endpoint` (`p50 / p95 / p99`)** | `15,345` (`5.115 us`) / `145,755` / `279,669` cyc | Kiralanmış doğrudan istemci-ada veri yolu |
| **Broker 2-Sıçrama (`p50 / p95 / p99`)** | `19,906` (`6.635 us`) / `197,069` / `197,069` cyc | `~4,561` cyc ek kontrol-düzlemi sıçrama maliyeti |
| **`IoRing` Batch-16 Toplam / İşlem Başı** | `909,853` cyc toplam / `56,865.8` cyc/op | Brokersız SPSC `IoRing` toplu donanım kuyruğu |
| **`#UD` (`vec=6`) Çökme & `BME=0` Kurtarma** | `4,695,084` cyc (`1.565 ms`), `restarts = 1`, `quiesce = 3` | Çöküşte `BME=0` -> DMA iadesi -> `gen=2` yeniden başlatma |
| **Yetkisiz Erişim Reddi & Ayak İzi** | `dma_denied = 7`, `leaked_frames = 0`, `RSS = 26` sayfa (`104 KiB`) | `DEV_WORKSPACE` / sahte yetki reddi; sıfır çekirdek Linux kodu (`0` LOC) |

---

## 3. `Priority C` `VirtIO-GPU` 2D Scanout, GOP Fallback ve Konak GPU Denetimi (`P7` & `H3`)

- **Canlı QEMU `VirtIO-GPU` (`1AF4:1050`):** `1280x800` `B8G8R8X8_UNORM`, `gen=1` (`9` present, `22,080 KiB`, `22` `controlq` komutu) -> `#UD` (`vec=6`) enjeksiyonu -> canlı [`dry-display`](../userspace/src/bin/dry_display.rs) üzerinde otomatik `[DRY-WM] GPU_FALLBACK_TO_GOP reason=ep_revoked` -> `gen=2` (`3` present) -> QMP [`docs/p7_virtio_gpu_scanout.png`](./p7_virtio_gpu_scanout.png) (`998,700` arka-plan dışı piksel, `16` renk). Gecikme (`3115 MHz`): `present p50 = 3,416,216 cyc` (`1.097 ms`), `p99 = 4,184,298 cyc` (`1.343 ms`), kurtarma `27,184,357 cyc` (`8.727 ms`), `unauth_ipc_rejects = 7`, `kernel_pci_quiesces = 3`, `kernel_dma_denied = 3`, `leaked_frames = 0`, `RSS = 2,069` sayfa (`8.08 MiB`).
- **Salt-Okunur Konak GPU & PRIME Fence Denetimi ([`experiments/h3-gpu-compat/`](../experiments/h3-gpu-compat/audit_gpu_stacks.py)):**
  - **Intel Iris Xe (`0000:00:02.0`, `8086:9a49`, IRQ `153`):** BAR0 `16 MiB`, BAR2 `256 MiB`, `xe.ko.zst` = `1,213` UND (`592` `drm_kms_gem_ttm`), `i915.ko.zst` = `1,244` UND, `tgl_guc_70.1.1.bin.zst` (`102,651 B`, `15` dosya).
  - **NVIDIA RTX 3050 Ti Mobile (`0000:01:00.0`, `10de:25a0`, IRQ `130`):** BAR1 `4 GiB`, 4 modülde `1,189` UND (`470`+`105`+`273`+`341`) + `162` `os_*` (`os-interface.c` `3,147` LOC) + `81.31 MiB` (`27,043,992` + `58,215,936 B`) `ga10x` firmware.
  - **IOMMU (`VT-d`):** `DMAR` mevcut (`present_root_only`), ancak `/sys/class/iommu` ve `/sys/kernel/iommu_groups` `0` öğe (`vtd_active_in_host_linux = false`).
  - **Hybrid PRIME Probe ([`prime_fence_probe.c`](../experiments/h3-gpu-compat/prime_fence_probe.c)):** `4,096,000 B` tam kopya `p50 = 262,582 ns` (`p95 = 353,297 ns`) vs sıfır-kopya `USER_RO` + `IoRing` fence `p50 = 18 ns` (`p95 = 19 ns`) -> **`14,587.9x` hızlanma** (`stale_gen_rejected = true`, `checksum_ok = true`).

---

## 4. Sistemler Karşılaştırması: DRY OS Driver Islands vs Dış Ekosistem

| Boyut | **DRY OS Driver Islands (`2C-1` / `P7`)** | **Genode `dde_linux`** | **L4Re (`io` + `l4lx`)** | **Linux Kernel Library (`LKL`)** | **seL4 `sDDF`** |
|---|---|---|---|---|---|
| **Çekirdekte Linux Kodu** | **`0` LOC** (`Syscall ABI 0..=31` sabit) | `0` LOC (çekirdek dışı) | `0` LOC (`L4Linux` süreçte) | `0` LOC (konak çekirdeğe gömülmez) | `0` LOC |
| **Sürücü Uygunluk Alanı** | Ada-başı minimal C shim + `no_std` Rust (`GPL-2.0-only` izole crate) | C++ `DDE` kütüphanesi + ortak LX ortamı | Tam Linux VM veya `dde` adaptörü | Tek ikili içine tüm Linux çekirdeği | Sıfırdan C/Rust cihaz sürücüleri (`0` Linux kaynak) |
| **`virtio-rng` Island RSS** | **`26` sayfa (`104 KiB`)** (`1` C shim dosyası + `14` küçük başlık) | Süreç başına birkaç MiB `Lx::Env` | Onlarca MiB (`L4Linux` çekirdeği) | MB mertebesi (`liblkl.a` tam alt sistemler) | Çok küçük (`~64–256 KiB`), ancak upstream Linux C çalıştırmaz |
| **Kontrol / Veri Düzlemi** | `Device Broker` yalnızca kontrol (`0` sıcak hop); veri doğrudan `Endpoint` / `IoRing` | `Session` RPC / paylaşımlı `Packet_stream` | `Dataspace` + IPC | Uygulama-içi kütüphane çağrısı (ayrı koruma sınırı yok) | Statik paylaşımlı SPSC halkalar (`sDDF`) |
| **Arıza & DMA İzolasyonu** | `FaultEndpoint` (`#UD`) + senkron `BME=0` quiesce + CDT `revoke`: **`1.565 ms`** (`rng`), **`8.727 ms`** (`gpu` + canlı GOP fallback) | `Supervisor` yeniden başlatma (IOMMU gerektirir) | `Ned` yeniden başlatma + IOMMU | Tek adres alanında çöküş tüm süreci devirir | Yeniden başlatılabilir sürücü PD'leri + SMMU/VT-d |
| **Bare-Metal GPU Engeli** | `VT-d` (`DMAR`) + `MSI-X` + `>4 GiB BAR` yok (`BLOCKED`) | Mevcut DRM/GPU DDE portu büyük EMEKTİR | `vPCI` + `IOMMU` ile kısmi | Tam Linux GPU yığınına bağımlı | `sDDF` GPU/3D desteği yok |
