# DRY OS — Ürün Vizyonu, Kurulum Profilleri ve Ürün Yol Haritası (`P0` – `P5`)

> **Belge Tarihi:** 14 Kasım 2026 (`Project Ascension 4.0`: `DRY Glass 2.1` + `libdry-lx-compat` Linux DRM Runtime + `bochs.c` Adası + Hedef GPU Doğrulaması + `Live USB`)  
> **Başlangıç Referansları:** `Milestone 1C` (`1df3842e3ff1676c46349f072c623836a56f846a`) & `Low-Level Optimization Pass v1` (`69522a3482d3000dc796a6717446dbc8bb1cb25e`)  
> **Durum Sözleşmesi:** Bu belgedeki her hedef açıkça **`DONE`**, **`IN PROGRESS`**, **`PLANNED`** veya **`BLOCKED`** olarak işaretlenmiştir. `ROADMAP.md` içindeki mevcut teknik milestone numaraları (`Faz 0..5`, `Milestone 1A..1C`) değiştirilmeden korunmuş ve Bölüm 3'te ürün aşamalarıyla eşlenmiştir.

---

## 1. DRY OS Ürün ve Mühendislik Felsefesi (`ADR-008`)

DRY OS; bağımsız, Rust tabanlı, `no_std`, yetenek yönelimli (*capability-oriented*) bir mikroçekirdek işletim sistemidir.

- **One kernel, multiple experiences:** Tek repository ve ortak `dry-kernel` ikilisi üzerinde hem etkileşimli masaüstü (`DRY Desktop`) hem yüksek verimli başsız sunucu (`DRY Server`) deneyimi sunulur.
- **Minimum overhead, maximum useful work & Zero unnecessary work:** Aynı işlevsellik, güvenlik ve doğruluk korunurken gereksiz CPU buyruğu, bellek kopyası, bağlam değişimi ve kilit çekişmesi elenir.
- **Rust by default, assembly by evidence:** Varsayılan dil güvenli/yalın Rust'tır; assembly yalnızca donanım geçiş kapılarında ve ölçümle (`objdump` + döngü sayacı) kanıtlanmış sıcak yollarda kullanılır.
- **Security and isolation by architecture:** Güvenlik; `CSpace`, `ADR-007` 3-alan duvarları (`KERNEL` / `ROOT_SYSTEM` / `USERSPACE`), `SecLabel` ve Ring 3 servis izolasyonunun doğal sonucudur.
- **Functional minimalism, not feature deprivation:** Minimalizm kullanıcıdan özellik esirgemek değil, aynı yeteneği daha az kaynak, daha küçük TCB ve daha yalın mimariyle sunmaktır.
- **Ölçeklenebilirlik ve Epistemik Dürüstlük:** Düşük sınıf istemci donanımından çok çekirdekli NUMA sunuculara kadar ölçeklenebilirlik hedeflenir. **DRY mikroçekirdeğinin Linux'tan hızlı olduğu henüz kanıtlanmamıştır.** QEMU/KVM içindeki mikrobenchmark sonuçları (`null_sys = 231 cyc`, `call_reply = 1.952 cyc`), gerçek fiziksel donanım (`bare-metal`) ve bütün-sistem (*end-to-end*) iş yükü performansından kesin olarak ayrı tutulur.

---

## 2. Kurulum Profilleri: `DRY Desktop` ve `DRY Server` (`PLANNED — P4`)

Her iki kurulum profili **aynı `dry-kernel` ikilisini**, **aynı `Endpoint`/`Notification`/`IoRing` IPC mimarisini**, **aynı `ADR-007` güvenlik/etiket modelini** ve **aynı `drypkg` paket yöneticisini** paylaşır.

| Boyut | `DRY Desktop` Profili (`PLANNED`) | `DRY Server` Profili (`PLANNED`) |
| :--- | :--- | :--- |
| **Varsayılan Ortam** | `DRY Shell`, tekil Ring 3 compositor, `Clean Boot` + `DRY Login` | Varsayılan **headless** (`Diagnostic`/seri/uzak konsol), **sıfır** masaüstü/GUI paket bağımlılığı |
| **Temel Servisler** | Display, girdi (klavye/fare), ses, `DRY Store`, grafik sistem uygulamaları | CLI yönetimi, güvenli uzaktan yönetim, yüksek verimli ağ/depolama/servis orkestrasyonu |
| **Kaynak & Zamanlayıcı Politikası** | Etkileşim gecikmesi (*input-to-photon*), kare hızı kararlılığı ve pil/enerji verimliliği öncelikli | CPU yakınlığı (*affinity*), NUMA yerelliği, katı kompartman kaynak izolasyonu, yüksek *throughput* |
| **Hedef İş Yükleri** | Günlük masaüstü kullanımı, yazılım geliştirme (`DEV_WORKSPACE`), üretkenlik ve medya | Yüksek eşzamanlı ağ/depolama servisleri, konteyner/kompartman barındırma ve **AI inference** düğümleri |
| **Kurulum Sonrası Esneklik** | `drypkg` ile sunucu servisleri eklenebilir veya başsız profile dönüştürülebilir | Yeniden kurulum yapmadan `drypkg` ile masaüstü bileşenleri eklenebilir |

> **Mimari Kural:** `DRY Server`, yalnızca grafik arayüzü kapatılmış bir `DRY Desktop` imajı **değildir**; paket ağacı, başlangıç servis grafı ve zamanlayıcı/bellek politikaları sunucu iş yükleri için sıfırdan yalın kurulur.

---

## 3. Teknik Milestone <-> Ürün Yol Haritası Eşleme Tablosu

| Ürün Aşaması | Durum | Eşleşen Teknik Milestone (`ROADMAP.md`) | Temel Çıktı ve Doğrulama Kapısı |
| :--- | :---: | :--- | :--- |
| **`P0` — Mikroçekirdek Temeli** | **`DONE` (QEMU `56/56`)** | `Faz 0` (`H1`, `H2`), `Milestone 1A`, `1B-A..1B-D`, `1C` (`1df3842`), `Opt Pass v1` (`69522a3`) | SMP (`1..20 vCPU`), `H1`, `PMM`, `VSpace`, `CSpace`/`ADR-007`, Ring 3 ELF64, IPC, kök servisler ve ilk Ring 3 GOP ekranı |
| **`P1` — Etkileşimli DRY OS** | **`IN PROGRESS`** *(`Milestone 2A-1..2C-2` + `PCI 0x0212` + `Live USB`: `DONE`, `105/105` QEMU + `5/5` Matris)* | `Faz 2 — 2A-1..2B`, `2A-4a`, `2C-2`, `Ascension 4.0 PCI/USB` (`DONE`) + `NVMe` (`PLANNED`) | PS/2 klavye/fare, Ring 3 `dry-vfs` (`/boot`, `/ram`, `/persist` `DRYFS_v1`), `dry-blk` (`VirtIO-Blk` + `IoRing`), `dry-power`, `VT-d` (`DMAR`), `MSI/MSI-X`, `>4 GiB BAR`, `0x0212` `PCI_OBJ_DISPLAY_AUX` + `96 MiB` UEFI `Live USB` (`DONE`); `NVMe` & fiziksel `xHCI` HID (`PLANNED`) |
| **`P2` — DRY Desktop Alpha/Beta** | **`DONE` (QEMU/KVM)** *(`DRY Glass 2.0` + `2.1`: `--p11-only` `3/3 PASS`, `9` PNG)* | `Milestone 2A-2..2B`, `Priority C`, `DRY Glass v1 / 2.0 / 2.1` (`DONE`); `Clean/Diagnostic Boot` (`PLANNED`) | `DRY Glass 2.1` AA compositor, `Adaptive Window Strip` + `Alt+W` önizleme, `IconRail`, `100%/125%/150%` DPI, `Obsidian`/`Daylight`/`HC` temalar, `6` yerel uygulama (`dry-editor`, `dry-calc`), `1024x768..1920x1080` `VirtIO-GPU` + canlı `GOP Fallback` |
| **`P3` — Linux & Windows Uyumluluğu** | **`IN PROGRESS`** *(`H2` + `2C-0..2C-2` + `libdry-lx-compat` + `v6.18 bochs.c`: `DONE` [`--p12-only`])* | `2C-0..2C-2` + `libdry-lx-compat` (`DONE`) -> `2C-3` (`igc`/`e1000e` NIC & `dry-linux-abi`) -> `Faz 5` (Win32) | `dry-lx-rng.elf` (`1af4:1044`) + `libdry-lx-compat` üzerinde `v6.18` `bochs.c` (`dry-lx-drm-bochs.elf`) + `nv_os_shim_check.c` + `DRY_HOST_ONLY` GPU envanteri (`DONE`); `dry-linux-abi` ELF64 & fiziksel `Xe`/`gsp_ga10x` (`PLANNED`, `0` çekirdek Linux ABI kodu) |
| **`P4` — Paketleme ve Dağıtım** | **`PLANNED`** | `Faz 3` (`drypkg` çekirdeği) & `Faz 4` (`DRY Store` + Profiller) | Tasarım-öncelikli `drypkg` + `DRY Store`, isteğe bağlı runtime, imzalı paket, `Desktop`/`Server` kurulum profilleri, atomik *rollback* |
| **`P5` — Production Readiness** | **`IN PROGRESS`** *(`96 MiB` MBR (`0xEF`) + FAT32 UEFI `Live USB` QEMU `xHCI` boot `PASS`; Bare-Metal: `PLANNED`)* | `Ascension 4.0 Track E` (`Live USB` builder `DONE`) -> `Faz 4` (Bare-Metal, IOMMU, Soak, Benchmark) | `build_live_usb_image.py` ile doğrulanmış `96 MiB` UEFI `Live USB`; gerçek bare-metal donanım, Desktop/Server iş yükleri, adil Linux A/B benchmarkları ve 72s+ soak testleri (`PLANNED`) |

---

## 4. Ayrıntılı Ürün Aşamaları, Bağımlılıklar ve Riskler

### `P0` — Mikroçekirdek Temeli — Durum: `DONE` *(QEMU/KVM Doğrulaması; Bare-Metal: `PLANNED`)*

| Alt Hedef | Durum | Kanıt / Commit |
| :--- | :---: | :--- |
| TLA+/`loom` `H1` zamanlayıcı kanıtı ve `H2` `igc` (`v6.12`/`v6.18`) userspace sürücü adası PoC | **`DONE`** | `formal/h1/DrySched.tla`, `experiments/h1-sched`, `experiments/h2-island` (`EXPERIMENTS.md` Bölüm A–B) |
| Limine UEFI boot, `1/2/4/8/20 vCPU` SMP, per-CPU `GDT/TSS/IST1/GS_BASE` ve preemptive `H1` | **`DONE`** | `bc46907`, `7ecf5b2`, `0ae3a94`, `b6ad2e9`, `fad423b` (`21/21` QEMU testi) |
| `PMM` (`16 B` `FrameMeta`, per-CPU şarjör), `Untyped`, 4-seviye `VSpace` (`W^X`) ve SMP TLB shootdown | **`DONE`** | `78ac64b`, `6445641` (`35/35` QEMU testi, `leaked_frames = 0`, `stale_tlb_reads = 0`) |
| Korumalı `CSpace`, çift-nesil ABA kalkanı, geçişli CDT `revoke` ve `ADR-007` 3-alan/kiralama zorlaması | **`DONE`** | `a443e6c`, `b4ca6f8` (`46/46` QEMU testi, `uaf_errors = 0`) |
| Gerçek `CPL=3` ELF64 süreçleri, `Syscall ABI v0.1`, `CR4.SMAP` + `#PF` `extable` `uaccess` | **`DONE`** | `29002d3` (`50/50` QEMU testi, `Stage F 16/16`) |
| Capability `Endpoint`/`Notification` IPC, 4-sıçrama Öncelik Bağışı, `dry-init`/`dry-echo`/`dry-display` GOP | **`DONE`** | `1df3842` (`56/56` QEMU testi + canlı QMP `screendump`: `1280x800`, `non_bg_px = 281.600`) |
| `Low-Level Optimization Pass v1` (`null_sys 231 cyc`, birleşik `ep_reply_recv` `1.952 cyc` [`12.8x`]) | **`DONE`** | `69522a3` (`56/56` QEMU testi, `-16 KiB` yerleşik meta, `0` Clippy uyarısı) |

---

### `P1` — Etkileşimli DRY OS — Durum: `IN PROGRESS` *(`Milestone 2A-1..2C-2`: `DONE` — `95/95` QEMU [`8d230df`] + `5/5` SMP/RAM Matrisi; Native NVMe/USB HID: `PLANNED`)*

- **Alt Hedefler (Dürüst Bileşen Durumu):**
  - Donanım IRQ yönlendirme ilkeli (`SYS_IRQ_BIND`/`SYS_IRQ_ACK` + `IoApicHwCache`), PCI `MSI`/`MSI-X` (`0x40..=0x5F`), 64-bit `>4 GiB` `BAR` + `PAT4` `WC` eşlemesi, ACPI `DMAR` + `VT-d` per-BDF `SLPT` DMA izolasyonu, `TCB`-bağlı birleşik `Notification`, dar `IoPortCap` (`IOPL=0`), `PciDev`/`SYS_DMA_ALLOC` (`BME=0` susturma) ve yetenek-kapılı güç kontrolü (`CapObjectKind::PowerCtrl` + `SYS_POWER_CTL`: ACPI S5, FADT Reset, `sti; hlt`): **`DONE` (`IOAPIC`/`MSI`/`MSI-X`/`VT-d`/`>4 GiB BAR`/ACPI S5/Reset)** / **`PLANNED` (Gerçek-mod `ACPI S3` uyanma trampolini)**
  - Gerçek klavye girdisi (QEMU `PS/2 i8042` `IRQ1` -> Ring 3 `dry-input.elf`, canlı `UsQwerty` + `TurkishQ` haritaları, `0xE0` navigasyon ve 64-olay taşma koruması): **`DONE` (`PS/2` `UsQwerty` + `TurkishQ`)** / **`PLANNED` (`VirtIO-Input`, USB HID)**
  - Fare / işaretçi girdisi (Tek `i8042` sahibi `dry-input.elf` içinde PS/2 IntelliMouse 4-bayt tekerlek protokolü, `MouseMove`/`MouseButtonDown`/`MouseButtonUp`/`MouseScroll`, imleç hız çarpanı ve `1x`/`2x` yazılımsal imleç): **`DONE` (`PS/2 AUX + IntelliMouse`)** / **`PLANNED` (`VirtIO-Input`, USB `xHCI` HID)**
  - İzole Ring 3 dosya sistemi ve kalıcı depolama (`dry-vfs.elf` + `dry-blk.elf`: salt-okunur `/boot` + uçucu `/ram` + çift A/B `CRC32` CoW kalıcı `/persist` `DRYFS_v1` `VirtIO-Blk` `IoRing`): **`DONE` (`/boot` + `/ram` + kalıcı `VirtIO-Blk` `/persist`)** / **`PLANNED` (Native `NVMe` arka ucu — `Milestone 2C-3`)**
  - Canlı tuş/fare girdisi -> çoklu çözünürlük `1024x768`/`1280x800`/`1920x1080` `VirtIO-GPU` + `GOP Fallback` ekran güncellemesi ve `1/2/4/8/20 vCPU` & `256M..2G` RAM donanım matrisi doğrulaması: **`DONE`**
  - Yüksek bant genişlikli blok/sürücü veri düzlemi (`IoRingSharedPage`, `Milestone 2B`/`2C-0`/`2C-1` & `2C-2` `TCB`-bağlı `Notification`, `AUD-K6/K7/K8` + `AUD-U3/U4/U8` kapatıldı): **`DONE`**
- **Bağımlılıklar:** `P0` (`DONE`); `Milestone 2A-1..2C-2` etkileşim, kalıcı `/persist`, FPU/SIMD `XSAVEOPT`, `FaultEndpoint`, `VT-d`, `MSI/MSI-X` ve `>4 GiB BAR` tamamlandı; yalnızca USB HID/NVMe sonraki adıma kaldı.
- **Azaltılan & Açık Riskler:** `IRQF_ONESHOT` fırtına kelepçesi (`R-14`), birleşik `i8042` (`R-15`), `248 B` sıcak syscall yığın bütçesi (`R-11`), `R-18` (`MAX_LATCHED_MODULES=32`), `VT-d`/`MSI`/`>4 GiB BAR` (`R-03` QEMU doğrulandı; fiziksel bare-metal `R-05` açık) ve `S3 = -ENOSYS` (`R-16`).

---

### `P2` — `DRY Desktop` Alpha/Beta — Durum: `DONE` *(QEMU/KVM `1024x768..1920x1080` — `DRY Glass 2.1`: `--p11-only` `3/3 PASS` + `9` PNG; `Clean/Diagnostic Boot`: `PLANNED`)*

- **Alt Hedefler:**
  - Birleşik Ring 3 Display & Input servisi, saf `#![no_std]` TrueType AA (`DrySans`/`DryMono`, SIL OFL 1.1), SDF vektör `IconRail`, ortak `ui/` kiti, `100%/125%/150%` DPI ölçekleme + `VirtIO-GPU` (`MAX_BACKBUF_2M = 8`, `MAX_GPU_SCANOUT_2M = 5`) ve canlı `UEFI GOP Fallback` (`gen=2` kurtarma): **`DONE`**
  - Tek Ring 3 `DRY Glass 2.1` Compositor üzerinde **varsayılan `Floating`** + tek komutla **`SideBySideTiled` / `Stacked` / `Tabbed`**, **`Adaptive Window Strip`** + `+N` taşma, `Alt+W` aranabilir önizleme ve arka düzeni bozmayan **`WindowRole::DialogTransient`** pencereler: **`DONE`**
  - `DRY Shell` / `DRY Glass`: buzlu arka plan bulanıklığı, üst sistem çubuğu, floating `Dock`, `Alt+Space` aranabilir `Launcher`, `Quick Settings`, `Notification Center`, canlı **`Obsidian Night` / `Frosted Daylight` / `High Contrast`** tema geçişi + `low_overhead_glass`, `/persist/desktop.cfg` tercihleri ve `DRY Vault` doğrulamalı `Session Lock`: **`DONE`**; tek olay akışlı `Clean/Diagnostic Boot`: **`PLANNED`**
  - Fare, klavye kısayolları ve üst çubuğu birleştiren ortak **`Action API` (`DesktopAction`)**: **`DONE`**
  - `DRY Vault` kriptografik kimlik doğrulamalı oturum kilidi + $O(1)$ `GLOBAL_LEASE_EPOCH`: **`DONE` (`Milestone 2B`)**
  - Kullanılabilir **6 yerel sistem uygulaması**: **`Terminal`** (`dry-shell`), **`Life Station`** (`dry-sysmon`), **`Files`** (`dry-files`), **`Settings`** (`dry-settings`), **`Editor`** (`dry-editor`, `/persist/notes.txt`) ve **`Calculator`** (`dry-calc`): **`DONE`**
- **Bağımlılıklar:** `P1` (`Input + IoRing + VFS`), çekirdek FPU/SIMD `XSAVEOPT`/`XRSTOR` ve `DRY Vault` oturum kiralaması (`DONE`).
- **Teknik Riskler:** Yazılımsal GOP kipinde bulanıklık maliyeti ve panel kalabalığı (`R-12` — `VirtIO-GPU`, `Adaptive Window Strip`, `low_overhead_glass` ve canlı GOP *fallback* ile azaltıldı).

---

### `P3` — Linux ve Windows Uyumluluğu — Durum: `IN PROGRESS` *(`H2` + `2C-0..2C-2` + `libdry-lx-compat` + `v6.18 bochs.c` [`--p12-only`]: `DONE`)*

- **Kesin Uygulama Sırası ve Ring 3 Mimari Sınırı (`ADR-011`, `ADR-016..022`):**
  1. **Adım 1 — `Milestone 2A-4a`, `2C-1`, `2C-2` & `Ascension 4.0 Tracks C/D` (`DONE`):** Modül tavanı (`16 -> 32`, `24/32` aktif), `XSAVEOPT`/`XRSTOR`, `FaultEndpoint`, Linux `v6.18` `virtio-rng.c` adası (`dry-lx-rng.elf`), ACPI `DMAR` + `VT-d`, `MSI/MSI-X`, `>4 GiB BAR`, `TCB`-bağlı `Notification`, ortak statik C `libdry-lx-compat` runtime (`56` sembol, `.ko` reddi) + `v6.18` `bochs.c` DRM adası (`dry-lx-drm-bochs.elf`, `0x0212` + GOP BAR0 koruması) ve `DRY_HOST_ONLY` `8086:9a49` / `10de:25a0` + `nv_os_shim_check.c` doğrulaması tamamlandı (`dry-kernel` içinde `0` satır Linux ABI/GPL kodu).
  2. **Adım 2 — `Ascension 5.0 / Milestone 2C-3` (`PLANNED`):** `H2` `igc`/`e1000e` NIC adası ve `FaultEndpoint` + `TCB`-bağlı `Notification` üzerinde ilk Ring 3 `dry-linux-abi` (`musl` ELF64).
  3. **Adım 3 — Genişletilmiş Linux ELF ABI / Sürücü Uyumluluğu (`PLANNED`):** `< 400` sembollü sürücü adaları (`xHCI`) ve `Faz 5` (`Win32`).
  4. **Adım 4 — Fiziksel DMA/IOMMU (`P5`) ve Uzun Vadeli Seçilmiş Win32 (`Faz 5`, `PLANNED`):** Fiziksel `VT-d`/`AMD-Vi` IOMMU, dar `Xe/GuC` KMS veya ayrık GPU MicroVM (`P5`) doğrulaması; Linux katmanı kararlılığa ulaştıktan sonra seçilmiş kullanıcı alanı Win32 iş yükleri için Ring 3 `dry-ntos` + Wine DLL + `DXVK`.
- **Bağımlılıklar:** `Milestone 2A-4a` (`MAX_LATCHED_MODULES = 32`, `XSAVEOPT`, `FaultEndpoint`), dinamik ELF/`mmap`, etiketli `VFS`.
- **Teknik Riskler & Dürüst Sınır:** `libdry-lx-compat` kaynak-bazlı statik derleme (`GPL-2.0-only` Ring 3) kullanır, ham `.ko` dosyaları reddedilir (`R-19`); konak `8086:9a49` (`Iris Xe`) ve `10de:25a0` (`RTX 3050 Ti`) henüz bare-metal modeset için `BLOCKED` durumundadır; **Linux/Windows uygulamalarının `%100` çalışmama sınırı** (`0` çekirdek modu `.sys` / anti-cheat — `R-13`) şeffaf tutulur.

---

### `P4` — Paketleme, Kurulum Profilleri ve Dağıtım — Durum: `PLANNED`

- **Alt Hedefler:**
  - Uygulama öncesi paket formatı, kriptografik imza zinciri, servis bağımlılık grafı ve uyumluluk ABI spesifikasyonu: **`PLANNED`** *(Zorunlu ön koşul)*
  - `drypkg` ortak paket motoru (CLI) ve `DRY Store` grafik arayüzü (*Bul -> Kur -> Çalıştır*): **`PLANNED`**
  - Otomatik bağımlılık ve **isteğe bağlı (*on-demand*) runtime/uyumluluk katmanı kurulumu** (istenmeyen yabancı runtime'ların asla önceden yüklenmemesi; görünür uyumluluk seviyesi ve kısıtlama etiketleri): **`PLANNED`**
  - Capability tabanlı uygulama izin/sandbox profilleri (`ADR-007`), atomik nesil güncellemesi ve tek-adım *rollback* (canlı userspace etkinleşmesi vs sonraki-boot A/B çekirdek yuvası): **`PLANNED`**
  - `DRY Desktop` ve `DRY Server` kurulum profilleri + kurulum sonrası çift yönlü profil dönüşümü: **`PLANNED`**
- **Bağımlılıklar:** Kalıcı depolama/`VFS` (`P1/P2`), `DRY Vault` & `Policy Broker` (`Milestone 2B`), `P3` runtime sözleşmeleri.

---

### `P5` — Production Readiness — Durum: `IN PROGRESS` *(`Live USB` xHCI QEMU `PASS`; Fiziksel Donanım & Soak: `PLANNED`)*

- **Alt Hedefler:**
  - UEFI MBR (`0xEF`) + FAT32 `Live USB` imaj üreticisi & QEMU `xHCI` önyükleme doğrulaması: **`DONE` (`tools/build_live_usb_image.py`, `--verify-boot`)** / Gerçek fiziksel x86-64 donanım (`bare-metal`) kurulumu ve fiziksel Intel VT-d / AMD-Vi IOMMU doğrulaması: **`PLANNED`**
  - `DRY Desktop` günlük kullanım kararlılık, uyku/uyanma, ses/ekran/girdi gecikme testleri: **`PLANNED`**
  - `DRY Server` çok-çekirdekli ağ, NVMe depolama, kompartman izolasyonu ve AI inference iş yükü testleri: **`PLANNED`**
  - Aynı fiziksel makine ve özdeş iş yükleri üzerinde **Linux ile adil, şeffaf ve yeniden üretilebilir A/B performans karşılaştırması**: **`PLANNED`**
  - Sürekli güvenlik denetimi, syscall/IPC/VFS/paket *fuzzing*, hata enjeksiyonu ve `>= 72` saatlik kesintisiz *stress/soak* testleri: **`PLANNED`**
  - Kamuya açık metodoloji ve ham veri içeren yeniden üretilebilir benchmark raporları: **`PLANNED`**

---

## 5. Tüm Aşamalar Boyunca Sürekli Doğrulama Disiplini (`P0 -> P5` Zorunlu Matris)

Performans, doğruluk, güvenlik ve entegrasyon testleri **yalnızca `P5`'e bırakılmaz**; her aşamanın kabul kapısında zorunlu olarak koşar:

| Doğrulama Ekseni | `P0` (`DONE` — `56/56`) | `P1` (Etkileşimli OS) | `P2` (Desktop Alpha/Beta) | `P3` (Uyumluluk) | `P4` (`drypkg` & Profiller) | `P5` (Production) |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Doğruluk & Entegrasyon** | `56/56` QEMU, SMP `1..20`, QMP piksel kanıtı | Canlı IRQ -> Input -> Shell -> Ekran otomasyonu | Compositor sahne, WM geçiş ve `BootEvent` testleri | Gerçek Linux ELF / seçilmiş Win32 regresyon paketi | Atomik kurulum, *rollback* ve Desktop<->Server geçiş testi | USB live boot, çoklu donanım matrisi, `>=72s` soak |
| **Güvenlik & İzolasyon** | `ADR-007`, `SMAP`, `W^X`, 18 negatif arıza testi | Yetkisiz IRQ/port/MMIO erişim reddi testleri | Compositor yüzey izolasyonu, Login vs PassVault ayrımı | Yabancı ABI sandbox kaçış ve `symlink`/`mount` testleri | Paket imza sahteciliği ve yetki aşımı reddi testleri | Tam sistem *fuzzing*, kırmızı-takım ve fiziksel IOMMU DMA |
| **Performans & Bütçe** | `p50/p95/p99` döngü, `objdump`, `leaked_frames=0` | Tuş-ekran gecikmesi, `IoRing` kopyasız verim | Kare süresi (`p99 ms`), düşük donanım *fallback* FPS/RAM | Native vs Linux personality ek yük (`overhead`) ölçümü | Soğuk paket sorgu/kurulum süresi, başsız Server ayak izi | Özdeş bare-metal üzerinde adil DRY OS vs Linux A/B raporu |
