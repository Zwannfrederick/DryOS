# DRY OS — Geliştirme Yol Haritası ve Faz Geçiş Kapıları

> **Başlangıç:** 11 Kasım 2026  
> **Temel İlke (`ADR-006`):** Her faz ölçülebilir bir mühendislik çıkış kapısına (*Exit Gate*) sahiptir. **DRY Kernel gelişimi (Faz 1–2), Linux sürücü çevirisinin (H2) tüm sürücü sınıflarında başarılı olmasına BAĞIMLI DEĞİLDİR.**

---

## Faz 0: H1 ve H2 Doğrulama & Fizibilite Kapısı
- **Hedef:** Çekirdek kodlamasına tam ölçekli girmeden önce en yüksek riskli iki mimari iddiayı yürütülebilir testlerle sınamak.
- **Kapsam:**
  1. `H1`: `AtomicU32(exec_state, park_token)` zamanlayıcı protokolünün TLC model kontrolü (Safety + Liveness ayrı), Rust `loom` zayıf bellek doğrulaması ve çok-çekirdekli stres testi.
  2. `H2`: Upstream Linux 6.12 & 6.18 `igc` sembol/başlık analizi, `libdry_lx_net` dar çalışma zamanı ve QEMU `intel-iommu` (`caching-mode=on`) + `vfio-pci` üzerinde kalıcı IOVA Arena PoC'si.
- **Çıkış Kapısı (Gate 0):**
  - `H1`: Pozitif TLC ve Loom testlerinde **0 ihlal**, negatif testlerde bilinen karşı-örneklerin yakalanması.
  - `H2`: Karar kapısı — `libdry_lx_net <= 5.000 LOC` ve `<= %1` sürücü değişikliğiyle paket geçerse H2 Faz 3 için onaylanır; aşarsa Faz 3 kapsamı daraltılır ancak **Faz 1 derhal başlar**.

---

## Faz 1: DRY Kernel Çekirdek Omurgası (`x86-64`, `no_std` Rust)
- **Hedef:** Bağımsız, BKL içermeyen, donanım izolasyonlu minimal mikroçekirdeğin QEMU ve gerçek x86-64 donanımda ayağa kalkması.
- **Kapsam:**
  - Limine UEFI önyükleme (`ADR-001`), HHDM, fiziksel çerçeve envanteri ve `Untyped` bellek retyping (çekirdek içi `kmalloc` yok).
  - 4-seviye MMU (`VSpace`), hedefli TLB shootdown (`active_cpus` maskesi) ve Intel VT-d / AMD-Vi IOMMU temel domain bağlayıcısı (`iommu_pin_count` korumalı).
  - APIC/IOAPIC/MSI-X kesme yönlendirme, `TSC-Deadline` zamanlayıcı, `INIT-SIPI` / Limine MP ile tüm mantıksal CPU'ların simetrik başlatılması.
  - Sabit boyutlu korumalı `CSpace` (`ADR-003`), `H1 v2` uyuma/uyanma + CPU başına öncelikli FIFO runqueue + IPC Öncelik Bağışı (`ADR-004`).
  - Yazmaç-tabanlı `Endpoint` IPC, `TCB`-bağlı `Notification` ve `FaultEndpoint` (`ADR-002`).
- **Çıkış Kapısı (Gate 1):**
  - QEMU `-smp 20` ve yerel 8-thread bare-metal üzerinde tüm çekirdeklerin uyanması; iki izole Ring 3 sürecinin `Endpoint` IPC + paylaşımlı `IoRing` ile 10 milyon mesajı 0 çökme ve 0 bellek sızıntısıyla aktarması; yetkisiz bellek ve sınır-dışı IOMMU DMA erişiminin donanım tuzağıyla engellenmesi.

---

## Faz 2: DRY OS Temel Native Ekosistem
- **Hedef:** Hiçbir yabancı sürücü çevirisine ihtiyaç duymadan kendi kendine yeten mikroçekirdek işletim sistemi katmanı.
- **Kapsam:**
  - Userspace Root/Capability Manager, VSpace Pager, PCI/IOMMU Bus Manager (PCIe FLR destekli).
  - Native Rust sürücüleri (`IoRing` + `DmaArena` üzerinde): `VirtIO-Blk`, `VirtIO-Net`, `NVMe` temel kuyruk sürücüsü, UEFI `GOP` / `VirtIO-GPU` 2D çerçeve tamponu, seri/PS2/USB-HID temel girdi.
  - İzole VFS sunucusu (salt-okunur initrd + temel disk dosya sistemi), sıfır-kopya ağ çoklayıcısı ve native `dry-sdk` süreç kabuğu.
- **Çıkış Kapısı (Gate 2):**
  - DRY OS'un QEMU ve bare-metal NVMe diskten önyüklenip ağ üzerinden TCP bağlantısı kurması; bir native sürücü süreci zorla öldürüldüğünde (`kill`) çekirdeğin ayakta kalıp sürücüyü yeniden başlatması.

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

