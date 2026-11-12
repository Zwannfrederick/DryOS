# DRY OS — Teknik Çekirdek ve Altyapı Yol Haritası (`ROADMAP.md`)

> **Son Güncelleme:** 12 Kasım 2026 (`Milestone 2A-3` / `Functional Desktop Foundation, Native Applications & Power Lifecycle` Kapanışı)  
> **Durum Etiketleri:** `DONE` (tamamlandı & doğrulandı), `IN PROGRESS` (aktif geliştirme), `PLANNED` (tasarlandı/sırada), `BLOCKED` (dış/donanım engelli).  
> **Ürün Eşlemesi:** Kullanıcı deneyimi, `DRY Desktop` / `DRY Server` kurulum profilleri ve `P0..P5` ürün aşamaları için ayrı belge: `docs/PRODUCT_ROADMAP.md` (bu belgedeki teknik milestone numaraları değiştirilmeden korunur).  
> **Temel İlke (`ADR-006`):** **DRY Kernel gelişimi (Faz 1–2), Linux sürücü çevirisinin (`H2`) tüm sürücü sınıflarında başarılı olmasına BAĞIMLI DEĞİLDİR.**

---

## Faz 0: `H1` ve `H2` Doğrulama & Fizibilite Kapısı — `DONE` (`11 Kasım 2026`)
- **Kapsam & Kanıtlar:**
  1. **`H1` (`DONE`):** `AtomicU32(exec_state, park_token)` zamanlayıcı protokolü TLC (`84.351` durum Safety, `54.095` durum Liveness), Rust `loom` (`39.135` permütasyon) ve 20-CPU stres testiyle (`>24M` geçiş) `0` hatayla doğrulandı.
  2. **`H2` (`DONE` — QEMU/VFIO PoC):** Upstream Linux `v6.12` ve `v6.18` `igc` (`18.528` satır) `%0` değişiklik ve `3.468` satırlık `GPL-2.0-only` runtime (`v6.12 -> v6.18` farkı `38` satır) ile derlenip test edildi; `e1000e` QEMU `vfio-pci` + emüle `intel-iommu` DMA PoC doğrulandı (fiziksel silikon IOMMU güvenliği Faz 4/`P5` bare-metal kapısındadır: `PLANNED`).

---

## Faz 1: DRY Kernel Çekirdek Omurgası (`x86-64`, `no_std` Rust) — `DONE (QEMU/KVM 56/56)` / `PLANNED (Bare-Metal & Fiziksel IOMMU)`
- **Tamamlanan Alt Aşamalar (`DONE`):**
  - **`Milestone 1A` (`fad423b` — `DONE`):** Repo-yerel kilitli `rust-src` + Limine UEFI önyükleme (`bc46907`), per-CPU `GDT`/`TSS` (`IST1` `#DF` yığını) ve 256-girişli `IDT` (`7ecf5b2`), `1/2/4/20 vCPU` SMP (`0ae3a94`), işbirlikçi (`b6ad2e9`) ve `x2APIC`/`xAPIC` kalibrasyonlu preemptive `H1` zamanlayıcısı (`fad423b`, `21/21` QEMU testi).
  - **`Milestone 1B-A` (`78ac64b` — `DONE`):** `BootstrapAllocator` mühürleme, `16 B` `FrameMeta` (`%0.39` ek yük), per-CPU şarjörlü `PMM` (`Order4KiB`/`Order2MiB`), kompartmanlar arası sıfırlama ve `Untyped` `split/retype/revoke` (`26/26` QEMU testi).
  - **`Milestone 1B-B` (`6445641` — `DONE`):** 4-seviye `VSpace`, varsayılan `W^X`, `KERNEL_VSPACE` devri (`~53 MiB` önyükleyici çerçevesi geri kazanımı), `2 MiB` büyük sayfa ve `active_cpus` hedefli SMP TLB shootdown (`35/35` QEMU testi).
  - **`Milestone 1B-C` (`b4ca6f8` — `DONE`):** Korumalı `CSpace` (`MAX_CSPACES = 32`, `256` yuva/`CNode`, `64 B/slot`), kilitsiz seqlock `cspace_lookup_fast`, intrüzif CDT $O(k)$ `revoke` + otomatik `VSpace` `unmap` ve `ADR-007` 3-alan/kiralama zorlaması (`46/46` QEMU testi).
  - **`Milestone 1B-D` (`29002d3` — `DONE`):** Gerçek `CPL=3` statik ELF64 süreçleri (`user-hello.elf`, `user-peer.elf`), `Syscall ABI v0.1`, `CR4.SMAP` + `#PF` `extable` `uaccess`, `NMI/#DF` MSR-doğrulamalı `swapgs` ve sıralı/idempotent süreç geri kazanımı (`50/50` QEMU testi).
  - **`Milestone 1C` (`1df3842` — `DONE`):** Senkron `Endpoint` (`send/recv/call/reply/reply_recv`) + asenkron `Notification`, 4-sıçramalı Öncelik Bağışı, `CPL=3` kök süpervizör (`dry-init.elf`), çökme/yeniden başlatma doğrulamalı `dry-echo.elf` ve yetenek-kapılı ilk Ring 3 UEFI GOP grafik çıktısı (`dry-display.elf`, `1280x800x32`, `56/56` QEMU testi + canlı QMP `screendump`).
  - **`Low-Level Optimization Pass v1` (`69522a3` — `DONE`):** Koşullu APIC One-Shot yeniden kurma (`0` gönüllü VM-exit), `STR` VM-exit elemesi, `248 B` çerçeveli `rust_syscall_dispatch` (`null_sys = 231 cyc`), birleşik `ep_reply_recv` (`call_reply = 1.952 cyc`, `12.8x` kazanç), `tzcnt` `active_mask` runqueue ve `-16 KiB` yerleşik meta küçültme (`56/56` QEMU testi).
- **Faz 2 / `P1..P5` Aşamalarına Devredilen Açık Çekirdek Maddeleri:**
  - Donanım IRQ/IOAPIC -> `Notification` bağlaması (`SYS_IRQ_BIND`/`SYS_IRQ_ACK` + `IoPortCap`) ve yetenek-kapılı ACPI güç kontrolü (`CapObjectKind::PowerCtrl` + `SYS_POWER_CTL`) -> **`Milestone 2A-1..2A-3` ile TAMAMLANDI (`DONE — 85/85 QEMU`)**.
  - `CapObjectKind::IoRing` sıfır-kopyalı paylaşımlı bellek veri düzlemi, `TCB`-bağlı `Notification`, `FaultEndpoint` userspace pager yönlendirmesi ve FPU/SIMD `XSAVEOPT` -> **Faz 2 `Milestone 2A-4` (`PLANNED`)**.
  - Fiziksel bare-metal önyükleme ve gerçek Intel VT-d / AMD-Vi IOMMU silikon doğrulaması -> **Faz 4 / `P5` (`PLANNED`)**.

---

## Faz 2: DRY OS Temel Native Ekosistem, Etkileşimli Sistem, VFS Etiketleme ve `DRY Vault` — `IN PROGRESS` (`Milestone 2A-1`, `2A-2` & `2A-3`: `DONE`)
- **Hedef:** Yabancı sürücü çevirisine bağımlı olmadan etkileşimli (`P1`), grafik masaüstü temelli (`P2` işlevsel masaüstü temeli ve çekirdek servisleri) ve `ADR-007` alanlarıyla korunan kendi kendine yeten mikroçekirdek işletim sistemi katmanı.
- **Kapsam ve Alt Kilometre Taşları (Tarihsel Faz 0/1 Numaraları Korunmuştur):**
  - **`Milestone 2A-1` (`P1 First Interaction` — `DONE`, 12 Kasım 2026, `65/65` QEMU, `9dab7a6`):** ACPI MADT/IOAPIC keşfi (`acpi.rs`), `IrqCap` (`0x30..=0x3F`, `IRQF_ONESHOT` maske + fırtına koruması + `IoApicHwCache`) ve dar `IoPortCap` (`IOPL=0`), Ring 3 `PS/2 i8042` girdi sürücüsü (`dry-input.elf`), izole `SecDomain::Userspace` etkileşimli kabuk (`dry-shell.elf`), çift-tamponlu (`WB Frame2M` + `DirtyRegion`) dinamik `dry-display.elf` GOP compositor + taşınabilir floating terminal penceresi (`Alt+Ok`), canlı `#UD` sürücü çökme/yeniden başlatma kurtarması ve gerçek QMP `send-key` + `screendump` (`docs/p1_interactive_shell.png`) doğrulaması.
  - **`Milestone 2A-2` (`DRY Desktop Preview`: Fare, Yazılımsal İmleç & Çok-Pencereli Ring 3 WM — `DONE`, 12 Kasım 2026, `79/79` QEMU, `85ec902`):** Tek `i8042` sahibi `dry-input.elf` içinde birleşik `IRQ1` + `IRQ12` (`PS/2 AUX` fare, 3-bayt senkron kilidi, `push_coalescing`), `4 MiB` (`2 x Frame2M`) doğrulanmış arka tampon + `VSPACE_MAP_OP_UNMAP_2M`, iz bırakmayan `12x16` kirli-dikdörtgen yazılımsal imleç (`cur p50 = ~8.8k..10.8k cyc`), sıfır-UB atomik `SharedWindowSurface` + `SurfaceSnapshot`, tamamen Ring 3 `WindowManager` (`Floating` + 2-sütun `SideBySideTiled`, başlık çubuğu sürükleme, tıklamayla odak/Z-sırası, `[x]` kapatma, `Alt+Tab`, `Alt+T`), eşzamanlı iki izole `Userspace` penceresi (`dry-shell.elf` `0x0102` + `dry-sysmon.elf` `0x0103`), uygulama çökme izolasyonu ve QMP `screendump` kanıtı (`docs/p2_desktop_preview.png`).
  - **`Milestone 2A-3` (`Functional Desktop Foundation, Native Applications & Power Lifecycle` — `DONE`, 12 Kasım 2026, `85/85` QEMU + `5/5` SMP/RAM Matrisi, `0da9d9d`..`58ccda6`):** Birleşik uygulama yaşam döngüsü (`Launcher`, `DesktopAction`, `SYS_PROC_LIST`/`SYS_PROC_KILL`, çökme izolasyonu + yeniden başlatma), 4 yerel `Userspace` uygulaması (`Terminal`, `Life Station`, `Files`, `Settings` + `DialogTransient`), Ring 3 `dry-vfs.elf` (salt-okunur `/boot` + kompartman-yalıtımlı uçucu `/ram`), PS/2 IntelliMouse 4-bayt tekerlek (`MouseScroll`), canlı `UsQwerty`/`TurkishQ` geçişi, `High Contrast` + `2x` imleç, `Floating`/`SideBySideTiled`/`Stacked`/`Tabbed` düzenleri, `dry-power.elf` (`CapObjectKind::PowerCtrl`, yarışsız `sti; hlt` + `VEC_RESCHED_WAKE`, `DPMS` `0x000000` karartma + 5 sn boşta sayacı, `Session Lock` kalkanı, gerçek donanım `ACPI S5` kapanış & `FADT` yeniden başlatma, dürüst `S3 = -ENOSYS`), `-512 KiB` çekirdek `.bss` küçültme ve `1..20 vCPU` / `256M..2G` RAM matris doğrulaması (`docs/p3a_apps_lifecycle.png`, `docs/p3b_files_settings_wm.png`, `docs/p3c_lock_shield.png`).
  - **`Milestone 2A-4` (`DRY Shell` Görsel Katmanı, Piksel Yüzeyleri, `IoRing`, `FaultEndpoint` & `XSAVE` — `PLANNED`, Sıradaki):** `liquid-glass` üst çubuk ve tema/animasyon katmanı (*Low-End Fallback* ile), `CapObjectKind::IoRing` sıfır-kopyalı piksel/veri halkaları, `TCB`-bağlı `Notification`, `VirtIO-Input`/USB `xHCI` HID girdi arka uçları, `FaultEndpoint` userspace pager ve FPU/SIMD `XSAVEOPT`/`XRSTOR` bağlam izolasyonu.
  - **Milestone 2B (`DRY Vault`, `Policy Broker` & Kalıcı Etiketli `VFS`):** İzole `ROOT_SYSTEM` `dry-vault` (kimlik doğrulamalı oturum kilidi + `lease_epoch` kiralaması), `dry-policy`, kalıcı blok cihazı üzerinde `ADR-007` etiket miraslı `dry-vfs` (`rename`/`symlink`/`hardlink`/`mount` yetki yükseltmeme garantisi: `INV-SEC-05`), `READ_ONLY_EXPORT` kütüphane paylaşımı ve `DEV_WORKSPACE`.
  - **Milestone 2C (Native Sürücüler, Görsel Servis & `Low-Level Optimization Pass v2`):** `DmaArena` (`iommu_pin_count`) üzerinde native Rust `VirtIO-Blk`, `VirtIO-Net`, temel `NVMe`, `VirtIO-GPU` 2D/çift-tamponlu ekran sunucusu; ayrıca `Optimization Pass v2` (`CR4.PCIDE`/`INVPCID`, doğrudan IPC thread switch, `IA32_TSC_DEADLINE`).
- **Çıkış Kapısı (Gate 2):** Etkileşimli klavye -> Ring 3 kabuk -> canlı ekran döngüsü; NVMe/`VirtIO-Blk` diskten önyükleme ve ağ bağlantısı; `INV-SEC-01..08` VFS/kiralama negatif testlerinin `%100` geçmesi.

---

## Faz 3: Uyumluluk Katmanı (`Linux ELF` Öncelikli), Paket Altyapısı ve Sürücü Adaları — `PLANNED`
- **Kapsam (`PLANNED`):**
  - İki seviyeli radiks `CSpace` ve epoch-tabanlı `Revoke` (`ADR-003`); topoloji-farkındalıklı (`LLC/SMT/NUMA`) *work stealing* ve `NO_HZ_FULL` (`ADR-004`).
  - **Linux ELF Personality (`ADR-011` Öncelik #1):** `FaultEndpoint` / `SYSCALL` yönlendirme trampolini + userspace Linux ABI/POSIX sunucusu ile `musl`/temel `glibc` uygulamalarının çalıştırılması.
  - **Linux Driver Islands (`GPL-2.0-only`):** Faz 0 `H2`'de kanıtlanan `igc`, `e1000e`, `igb` ve USB `xHCI` sürücü adalarının DRY `PciCap` + `DmaArena` + `IoRing` üzerine taşınması.
  - **`drypkg` Çekirdek Spesifikasyonu ve İlk Paket Motoru (`ADR-011`):** İmzalı paket formatı, isteğe bağlı runtime çözümleme, capability sandbox profilleri ve atomik nesil/*rollback* altyapısı.
- **Çıkış Kapısı (Gate 3):** Değiştirilmemiş Linux ELF ikililerinin çalışması; izole `igc`/`e1000e` sürücü adasının 0 sıcak-yol IOMMU syscall ile ağ trafiği taşıması ve FLR ile çökmelerden kurtulması.

---

## Faz 4: Kararlılık, Bare-Metal Doğrulama, Kurulum Profilleri ve Performans Sertleştirme — `PLANNED`
- **Kapsam (`PLANNED`):**
  - `DRY Desktop` ve `DRY Server` kurulum profillerinin (`ADR-008`), `DRY Store` arayüzünün ve çift yönlü profil geçişlerinin tamamlanması (`P4`).
  - Sürekli *fuzzing*, *fault injection* ve `>= 72` saatlik *soak* testleri; gerçek fiziksel donanımda (USB live boot + fiziksel VT-d/AMD-Vi IOMMU + 20+ mantıksal CPU sunucu) ölçekleme ve Linux ile adil, tekrarlanabilir A/B benchmark raporları (`P5`).
  - **AArch64 Çekirdek Portu:** GICv3, SMMUv3 ve zayıf bellek modeli + DMA önbellek tutarlılığı doğrulaması.

---

## Faz 5: İleri Uyumluluk (`Win32`) ve Platform Genişletme (Uzun Vadeli) — `PLANNED`
- **Kapsam (`PLANNED`):**
  - Seçilmiş kullanıcı alanı **Windows PE / Win32** uygulamaları için `dry-ntos` + Wine DLL + `DXVK` uyumluluk katmanı (`ADR-011` Öncelik #2; çekirdek modu anti-cheat kapsam dışı).
  - `VirtIO-GPU` Venus/Rutabaga Vulkan hızlandırması, seçilmiş KMS/DMA-BUF ekran sürücüleri ve RISC-V (`RV64GC` + `RVWMO`) çekirdek portu.
