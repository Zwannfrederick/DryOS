# DRY OS — Geliştirme Yol Haritası ve Faz Geçiş Kapıları

> **Başlangıç:** 11 Kasım 2026  
> **Temel İlke (`ADR-006`):** Her faz ölçülebilir bir mühendislik çıkış kapısına (*Exit Gate*) sahiptir. **DRY Kernel gelişimi (Faz 1–2), Linux sürücü çevirisinin (H2) tüm sürücü sınıflarında başarılı olmasına BAĞIMLI DEĞİLDİR.**

---

## Faz 0: H1 ve H2 Doğrulama & Fizibilite Kapısı `[TAMAMLANDI — 11 Kasım 2026]`
- **Hedef:** Çekirdek kodlamasına tam ölçekli girmeden önce en yüksek riskli iki mimari iddiayı yürütülebilir testlerle sınamak.
- **Kapsam & Gerçekleşen Durum:**
  1. `H1` `[GEÇTİ]`: `AtomicU32(exec_state, park_token)` zamanlayıcı protokolü TLC (`84.351` durum Safety, `54.095` durum Liveness), Rust `loom` (`39.135` permütasyon) ve 20-CPU stres testiyle (`>24M` geçiş) `0` hatayla doğrulandı.
  2. `H2` `[GEÇTİ]`: Upstream Linux `v6.12` ve `v6.18` `igc` (`18.528` satır) `%0` değişiklik ve `3.468` satırlık `GPL-2.0-only` runtime (`v6.12->v6.18` farkı `38` satır) ile derlenip test edildi; ayrı `e1000e` QEMU `vfio-pci` + emüle `intel-iommu` DMA PoC doğrulandı (fiziksel silikon IOMMU güvenliği Faz 3/4 bare-metal kapısına bırakıldı).

---

## Faz 1: DRY Kernel Çekirdek Omurgası (`x86-64`, `no_std` Rust) `[AKTİF]`
- **Hedef:** Bağımsız, BKL içermeyen, donanım ve alan izolasyonlu (`ADR-007`) minimal mikroçekirdeğin QEMU ve gerçek x86-64 donanımda ayağa kalkması.
- **Kapsam:**
  - **Milestone 1A (Temel Önyükleme, CPU Durumu, SMP ve `H1` Entegrasyonu):**
    - Repo-yerel tekrarlanabilir `no_std` Rust + Limine UEFI önyükleme araç zinciri (`ADR-001`), bellek haritası ve HHDM doğrulaması, `COM1` seri konsol ve deterministik panik işleyicisi (`1 CPU` otomatik QEMU testi).
    - `GDT`, `TSS` (`IST` yığınlarıyla), `IDT` ve temel donanım istisna işleme (`#BP`, `#DF`, `#GP`, `#PF`).
    - Kademeli SMP başlatma (`2 -> 4 -> 20 vCPU`) ve TLC/Loom'da doğrulanan `H1` zamanlayıcısının assembly `switch_to` + `post_switch_*` kurallarıyla çekirdeğe taşınması.
  - **Milestone 1B (Bellek, Capability, Güvenlik İlkelleri ve IPC):**
    - Fiziksel çerçeve envanteri ve `Untyped` bellek retyping (çekirdek içi `kmalloc` yok).
    - 4-seviye MMU (`VSpace`), hedefli TLB shootdown (`active_cpus` maskesi) ve Intel VT-d / AMD-Vi IOMMU temel domain bağlayıcısı (`iommu_pin_count` korumalı).
    - Sabit boyutlu korumalı `CSpace` (`ADR-003`) + **`ADR-007` sıfır-borç güvenlik ilkelleri**: çekirdek nesnelerinde `SecLabel` (`KERNEL` / `ROOT_SYSTEM` / `USERSPACE`, `READ_ONLY_EXPORT`, `compartment_id`), capability `lease_epoch` alanı, sahtelenemez gönderici etiketi (`INV-SEC-01..04, 06, 08`).
    - Yazmaç-tabanlı `Endpoint` IPC, `TCB`-bağlı `Notification` ve `FaultEndpoint` (`ADR-002`).
- **Çıkış Kapısı (Gate 1):**
  - QEMU `-smp 20` ve yerel 8-thread bare-metal üzerinde tüm çekirdeklerin uyanması; iki izole Ring 3 sürecinin `Endpoint` IPC + paylaşımlı `IoRing` ile 10 milyon mesajı 0 çökme ve 0 bellek sızıntısıyla aktarması; `INV-SEC-01..04, 06, 08` güvenlik invariant'larının ve sınır-dışı IOMMU DMA engelinin donanım tuzağıyla doğrulanması.

---

## Faz 2: DRY OS Temel Native Ekosistem, VFS Etiketleme ve `DRY Vault`
- **Hedef:** Hiçbir yabancı sürücü çevirisine ihtiyaç duymadan kendi kendine yeten, `ADR-007` güvenlik alanlarıyla korunan mikroçekirdek işletim sistemi katmanı.
- **Kapsam:**
  - `ROOT_SYSTEM` servisleri: Root/Capability Manager, VSpace Pager, PCI/IOMMU Bus Manager (PCIe FLR destekli), **`DRY Vault`** (kimlik doğrulama + süreli/kapsamlı `lease_epoch` yetki üretimi) ve **`Policy Broker`** (deny-by-default karar motoru).
  - Native Rust sürücüleri (`IoRing` + `DmaArena` üzerinde): `VirtIO-Blk`, `VirtIO-Net`, `NVMe` temel kuyruk sürücüsü, UEFI `GOP` / `VirtIO-GPU` 2D çerçeve tamponu, seri/PS2/USB-HID temel girdi.
  - **`ADR-007` Etiketli VFS ve Developer Workspace:** Ebeveynden `SecLabel` mirası alan, `rename`/`symlink`/`hardlink`/`mount` ile yetki yükseltmeyi reddeden (`INV-SEC-05`) izole VFS sunucusu; kontrollü salt-okunur sistem kütüphaneleri (`READ_ONLY_EXPORT`); kapsamlı `DEV_WORKSPACE` (`compartment_id`); canlı servis/paket etkinleştirme ve A/B sonraki-boot çekirdek güncelleme yuvası.
- **Çıkış Kapısı (Gate 2):**
  - DRY OS'un QEMU ve bare-metal NVMe diskten önyüklenip TCP bağlantısı kurması; bir `USERSPACE` sürecinin `ROOT_SYSTEM` veya `KERNEL` nesnelerine yazma ve `symlink`/`hardlink` ile yetki yükseltme girişimlerinin `%100` reddedilmesi (`INV-SEC-01..08`); `DRY Vault` kiralama süresi dolduğunda (`INV-SEC-07`) yetkilerin anında düşmesi.

---

## Faz 3: Uyumluluk Katmanı — Linux ELF Personality & Seçilmiş Sürücü Adaları
- **Hedef:** Çekirdek saflığını bozmadan uygulama ve çevre birimi ekosistemini genişletmek.
- **Kapsam:**
  - İki seviyeli radiks `CSpace` ve epoch-tabanlı BKL'siz `Revoke` (`ADR-003`).
  -Topoloji-farkındalıklı (`LLC -> Fiziksel Çekirdek -> SMT`) `EXEC_READY` *work stealing* ve tek görevli çekirdeklerde `NO_HZ_FULL` (`ADR-004`).
  - **Linux ELF Personality:** `FaultEndpoint` / `SYSCALL` trampolini + `libdry_linux_abi` ile statik/musl (ve temel glibc) CLI/sunucu ikililerinin çalıştırılması.
  - **Linux Driver Islands (`GPL-2.0-only`):** Gate 0'ı geçen sınıflar için (`igc`, `e1000e`, `igb`, USB `xHCI`) `dryc-drv` destekli izole sürücü adalarının DRY `PciCap` + `DmaArena` üzerine taşınması.
- **Çıkış Kapısı (Gate 3):**
  - Değiştirilmemiş Linux ELF test araçlarının çalışması; izole `igc`/`e1000e` sürücü adasının Strict IOMMU altında 0 sıcak-yol IOMMU syscall ile kararlı ağ trafiği taşıması ve FLR ile çökmelerden kurtulması.

---

## Faz 4: Kararlılık, Güvenlik Sertleştirme ve Çok-Çekirdek Performans Doğrulaması
- **Hedef:** Üretim kalitesinde hata toleransı, güvenlik denetimi ve 20+ mantıksal CPU bare-metal doğrulaması.
- **Kapsam:**
  - Syscall ABI, IPC, `CSpace` `Revoke` ve `IoRing` sınırları için sürekli *fuzzing* ve hata enjeksiyonu (*fault injection*).
  - Gerçek 20+ mantıksal CPU'lu (SMT ve P/E hibrit) fiziksel test donanımında 1, 2, 4, 8, 16, 20 CPU ölçekleme, gecikme (`p99.9`) ve IOMMU throughput ölçümleri.
  - **AArch64 Çekirdek Portu:** GICv3, SMMUv3 ve zayıf bellek modeli + DMA önbellek tutarlılığı doğrulaması.
- **Çıkış Kapısı (Gate 4 — Developer Preview Adayı):**
  - 72 saatlik kesintisiz bare-metal stres testinde 0 çekirdek paniği; 20 CPU bağımsız hesaplama yükünde `>= %98.5` faydalı kullanıcı zamanı.

---

## Faz 5: İleri Uyumluluk ve Platform Genişletme (Uzun Vadeli)
- **Kapsam:**
  - RISC-V (`RV64GC` + `RVWMO` + `AIA`/`IOPMP`) çekirdek portu.
  - `VirtIO-GPU` Venus/Rutabaga Vulkan hızlandırması ve seçilmiş donanımlarda dar kapsamlı KMS/DMA-BUF ekran sunucusu.
  - Araştırma/deneysel userspace Windows NT/Win32 alt-sistemi (`dry-ntos` + Wine DLL + `DXVK`).

