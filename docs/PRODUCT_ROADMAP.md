# DRY OS — Ürün Vizyonu, Kurulum Profilleri ve Ürün Yol Haritası (`P0` – `P5`)

> **Belge Tarihi:** 17 Kasım 2026 (`Project Ascension 5.0 — v0.5.0`: `dry-usb` + `dry-nvme` RO + `libdry-lx-compat` v2/`8139cp.c` + `118`-SVG/`200%` DPI + `82K1` Salt-Okunur Denetim + `160 MiB` GPT Live USB)  
> **Başlangıç Referansları:** `Milestone 1C` (`1df3842e3ff1676c46349f072c623836a56f846a`) & `Low-Level Optimization Pass v1` (`69522a3482d3000dc796a6717446dbc8bb1cb25e`)  
> **Durum Sözleşmesi:** Bu belgedeki her hedef açıkça **`DONE (QEMU/OVMF)`**, **`READY FOR USER PHYSICAL EXECUTION`**, **`IN PROGRESS`**, **`PLANNED`** veya **`BLOCKED`** olarak işaretlenmiştir. `ROADMAP.md` içindeki mevcut teknik milestone numaraları (`Faz 0..5`, `Milestone 1A..1C`) değiştirilmeden korunmuş ve Bölüm 3'te ürün aşamalarıyla eşlenmiştir.

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
| **`P1` — Etkileşimli DRY OS** | **`DONE (QEMU/OVMF)`** / Fiziksel: **`READY FOR USER PHYSICAL EXECUTION`** | `Faz 2 — 2A-1..2B`, `2A-4a`, `2C-2`, `Ascension 5.0` M1 & M2 (`--p13-only`, `25/25`) | PS/2 + USB `xHCI` HID (`dry-usb`), BOT Part 2 `DRYFS_V1` RW/RO, salt-okunur `dry-nvme` (`INV-DISK-SAFE-01/02`), `12+` Linux CLI komutu, `32G` PMM, `DIAG_RING` + GOP panik, `VT-d`/RMRR/`<4K` BAR (`DONE (QEMU/OVMF)`); fiziksel `82K1` xHCI/VMD NVMe kullanıcı yürütmesine hazır |
| **`P2` — DRY Desktop Alpha/Beta** | **`DONE (QEMU/OVMF)`** *(`DRY Glass 2.1` + `118`-SVG + `200%` DPI: `--p11-only` `3/3`, `--p15-only` `4/4`, `15` PNG)* | `Milestone 2A-2..2B`, `Priority C`, `DRY Glass 2.0/2.1`, Team A Kimlik (`1e15333`) | `DRY Glass 2.1` AA compositor, `Iso-Kernel Prism` + `118` SVG (`6` marka, `21` app, `91` sistem), `Adaptive Window Strip`, `Alt+W`, `100%..200%` DPI, `Obsidian`/`Daylight`/`HC`, `6` uygulama, `VirtIO-GPU` + `GOP Fallback` |
| **`P3` — Linux & Windows Uyumluluğu** | **`IN PROGRESS`** *(`virtio-rng`, `bochs.c`, `8139cp+mii`: `DONE (QEMU/OVMF)`; `dry-linux-abi`: `PLANNED`)* | `2C-0..2C-2`, `libdry-lx-compat` v2 (6 dosya), `8139cp.c+mii.c` (`dry-lx-net.elf`), `82K1` GPU FW/WPR2/PRIME (`7d72cd3`) -> `Faz 3A` -> `Faz 5` | `dry-lx-rng`, `dry-lx-drm-bochs`, modüler `libdry-lx-compat` (`2,164` C LOC), 15-adımlı yaşam döngüsü + `linux-microvm` sözleşmesi, `56/56` SHA-256 + `8139cp.c+mii.c` (`--p14-only` `1/1 PASS`, `leaked_frames=0`); `dry-linux-abi` ELF64 (`PLANNED`) & fiziksel `Xe`/`gsp_ga10x` modeset (`BLOCKED`) |
| **`P4` — Paketleme ve Dağıtım** | **`PLANNED`** | `Faz 3` (`drypkg` çekirdeği) & `Faz 4` (`DRY Store` + Profiller) | Tasarım-öncelikli `drypkg` + `DRY Store`, isteğe bağlı runtime, imzalı paket, `Desktop`/`Server` kurulum profilleri, atomik *rollback* |
| **`P5` — Production Readiness** | **`DONE (QEMU xHCI Live USB)`** / Fiziksel Gates 3–7: **`READY FOR USER PHYSICAL EXECUTION`** | `Ascension 5.0` M5 (`160 MiB` GPT Live USB `--verify-boot`) -> `Faz 4` (Fiziksel `82K1`, Soak, Benchmark) | `96 MiB` `DRYOS_ESP` + `DRYOS_PERSIST` `DRYFS_V1` (`27/32` modül, `5` Limine profili, `5.491s` `PASS`; tam paket `135/135 PASS`); fiziksel Lenovo `82K1` USB önyükleme, Linux A/B benchmarkları ve 72s+ soak (`READY FOR USER PHYSICAL EXECUTION` / `PLANNED`) |

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

### `P1` — Etkileşimli DRY OS — Durum: `DONE (QEMU/OVMF)` *(M1 & M2 `--p13-only`: `25/25 PASS`; Fiziksel `82K1` xHCI/VMD NVMe: `READY FOR USER PHYSICAL EXECUTION`)*

- **Alt Hedefler (Dürüst Bileşen Durumu):**
  - Donanım IRQ yönlendirme (`SYS_IRQ_BIND`/`SYS_IRQ_ACK` + `IoApicHwCache`), PCI `MSI`/`MSI-X` (`0x40..=0x5F`), 64-bit `>4 GiB` & `< 4 KiB` alt-sayfa BAR (`page_suboff`), `preserve_active_gop`, çoklu-DRHD `VT-d` erteleme + `premap_unit_rmrrs` + 39-bit `SLPT`, `32 GiB` PMM, UART-sız `probe_com1_uart` + `16 KiB` `DIAG_RING` (`dmesg`) + `panic_fb.rs` GOP konsolu, `SYS_POWER_CTL` (ACPI S5, FADT Reset): **`DONE (QEMU/OVMF)`** / `ACPI S3` uyanma trampolini: **`PLANNED` (`-ENOSYS`)**
  - Gerçek klavye ve işaretçi girdisi (`dry-input` PS/2 `i8042` `UsQwerty`+`TurkishQ`+IntelliMouse + **`dry-usb` `xHCI` Boot HID klavye/fare**): **`DONE (QEMU/OVMF)`** / Fiziksel `82K1` `xHCI` & Synaptics `I2C-HID` (`06cb:ce78`): **`READY FOR USER PHYSICAL EXECUTION` / `PLANNED`**
  - İzole Ring 3 depolama ve CLI (`dry-vfs` `/boot` RO + `/ram` + `/persist` `DRYFS_V1` RW/RO, `dry-blk` VirtIO-Blk, **`dry-usb` BOT Bölüm 2 `DRYFS_V1` [`INV-DISK-SAFE-02`]**, **salt-okunur `dry-nvme` [`INV-DISK-SAFE-01`]**, `/persist/boot.log` ve `12+` Linux CLI komutu `dmesg`/`lspci`/`lsusb`/`lsblk`/`nvme`/`drvctl`/`uname`/`free`/`mounts`/`cat`/`write`/`sync`): **`DONE (QEMU/OVMF — --p13-only 25/25)`** / Fiziksel VMD sub-bus NVMe: **`PLANNED`**
  - Canlı tuş/fare girdisi -> çoklu çözünürlük `1024x768`/`1280x800`/`1920x1080` `VirtIO-GPU` + `GOP Fallback` ekran güncellemesi ve `1/2/4/8/20 vCPU` & `256M..2G` RAM donanım matrisi doğrulaması: **`DONE`**
  - Yüksek bant genişlikli blok/sürücü veri düzlemi (`IoRingSharedPage`, `Milestone 2B`/`2C-0`/`2C-1` & `2C-2` `TCB`-bağlı `Notification`, `AUD-K6/K7/K8` + `AUD-U3/U4/U8` kapatıldı): **`DONE`**
- **Bağımlılıklar:** `P0` (`DONE`); `Milestone 2A-1..2C-2` etkileşim, kalıcı `/persist`, FPU/SIMD `XSAVEOPT`, `FaultEndpoint`, `VT-d`, `MSI/MSI-X` ve `>4 GiB BAR` tamamlandı; yalnızca USB HID/NVMe sonraki adıma kaldı.
- **Azaltılan & Açık Riskler:** `IRQF_ONESHOT` fırtına kelepçesi (`R-14`), birleşik `i8042` (`R-15`), `248 B` sıcak syscall yığın bütçesi (`R-11`), `R-18` (`MAX_LATCHED_MODULES=32`), `VT-d`/`MSI`/`>4 GiB BAR` (`R-03` QEMU doğrulandı; fiziksel bare-metal `R-05` açık) ve `S3 = -ENOSYS` (`R-16`).

---

### `P2` — `DRY Desktop` Alpha/Beta — Durum: `DONE (QEMU/OVMF)` *(`DRY Glass 2.1` `--p11-only` `3/3` + `118`-SVG/`200%` DPI `--p15-only` `4/4` & `15` PNG)*

- **Alt Hedefler:**
  - Birleşik Ring 3 Display & Input servisi (`PS/2` + `dry-usb` HID), `#![no_std]` TrueType AA (`DrySans`/`DryMono`, SIL OFL 1.1), `118` özgün SVG (`Iso-Kernel Prism`, `6` marka + `21` uygulama + `91` sistem, `vector_assets_{sys,apps}.rs`), `100%/125%/150%/200%` DPI ölçekleme, `VirtIO-GPU` + canlı `UEFI GOP Fallback` (`gen=2`): **`DONE (QEMU/OVMF)`**
  - Tek Ring 3 `DRY Glass 2.1` Compositor üzerinde **varsayılan `Floating`** + tek komutla **`SideBySideTiled` / `Stacked` / `Tabbed`**, **`Adaptive Window Strip`** + `+N` taşma, `Alt+W` aranabilir önizleme ve arka düzeni bozmayan **`WindowRole::DialogTransient`** pencereler: **`DONE`**
  - `DRY Shell` / `DRY Glass`: buzlu arka plan bulanıklığı, üst sistem çubuğu, floating `Dock`, `Alt+Space` aranabilir `Launcher`, `Quick Settings`, `Notification Center`, canlı **`Obsidian Night` / `Frosted Daylight` / `High Contrast`** tema geçişi + `low_overhead_glass`, `/persist/desktop.cfg` tercihleri ve `DRY Vault` doğrulamalı `Session Lock`: **`DONE`**; tek olay akışlı `Clean/Diagnostic Boot`: **`PLANNED`**
  - Fare, klavye kısayolları ve üst çubuğu birleştiren ortak **`Action API` (`DesktopAction`)**: **`DONE`**
  - `DRY Vault` kriptografik kimlik doğrulamalı oturum kilidi + $O(1)$ `GLOBAL_LEASE_EPOCH`: **`DONE` (`Milestone 2B`)**
  - Kullanılabilir **6 yerel sistem uygulaması**: **`Terminal`** (`dry-shell`), **`Life Station`** (`dry-sysmon`), **`Files`** (`dry-files`), **`Settings`** (`dry-settings`), **`Editor`** (`dry-editor`, `/persist/notes.txt`) ve **`Calculator`** (`dry-calc`): **`DONE`**
- **Bağımlılıklar:** `P1` (`Input + IoRing + VFS`), çekirdek FPU/SIMD `XSAVEOPT`/`XRSTOR` ve `DRY Vault` oturum kiralaması (`DONE`).
- **Teknik Riskler:** Yazılımsal GOP kipinde bulanıklık maliyeti ve panel kalabalığı (`R-12` — `VirtIO-GPU`, `Adaptive Window Strip`, `low_overhead_glass` ve canlı GOP *fallback* ile azaltıldı).

---

### `P3` — Linux ve Windows Uyumluluğu — Durum: `IN PROGRESS` *(`virtio-rng`, `bochs.c`, `8139cp.c+mii.c`: `DONE (QEMU/OVMF --p14-only 1/1 PASS)`; `dry-linux-abi`: `PLANNED`)*

- **Kesin Uygulama Sırası ve Ring 3 Mimari Sınırı (`ADR-011`, `ADR-016..023`):**
  1. **Adım 1 — `Milestone 2A-4a`, `2C-1`, `2C-2`, `Ascension 4.0/5.0 M3/M4` (`DONE (QEMU/Host RO)`):** Modül tavanı (`27/32` aktif), `XSAVEOPT`/`XRSTOR`, `FaultEndpoint`, `virtio-rng.c` (`--lx-island-only`), `bochs.c` (`--p12-only`), 6 dosyalı modüler `libdry-lx-compat` (`2,164` C LOC), 15-adımlı `dry-broker` yaşam döngüsü + `linux-microvm` sözleşmesi, `56/56` SHA-256 doğrulamalı `8139cp.c+mii.c` (`dry-lx-net.elf`, `--p14-only` `1/1 PASS`: `3.95s`, `udp_echo=1`, `#UD gen=2`, `leaked_frames=0`), ve Lenovo `82K1` salt-okunur M4 envanter/FW/WPR2/PRIME (`22/162` `os-interface.h`) doğrulaması (`0` çekirdek Linux ABI/GPL satırı; özgün DRY kodu `Apache-2.0 OR MIT`).
  2. **Adım 2 — `Faz 3A–3B` (`PLANNED`):** `FaultEndpoint` + `ET_DYN` PIE üzerinde Ring 3 `dry-linux-abi` (`musl` ELF64) ve `H2` `igc`/`e1000e`/`r8168` (`10ec:8168`) NIC adaları.
  3. **Adım 3 — Fiziksel DMA/IOMMU (`P5`) ve Uzun Vadeli Seçilmiş Win32 (`Faz 5`, `PLANNED`):** Fiziksel `VT-d`/`AMD-Vi`, dar `Xe/GuC` KMS veya ayrık GPU MicroVM; Linux katmanı kararlılığa ulaştıktan sonra Ring 3 `dry-ntos` + Wine DLL + `DXVK`.
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

### `P5` — Production Readiness — Durum: `DONE (QEMU xHCI Live USB)` / Fiziksel Gates 3–7: `READY FOR USER PHYSICAL EXECUTION`

- **Alt Hedefler:**
  - UEFI `160 MiB` 2-bölümlü GPT (`96 MiB` FAT32 `DRYOS_ESP` + `DRYOS_PERSIST` `DRYFS_V1`, `27` modül, `5` Limine profili) `Live USB` üreticisi & QEMU `xHCI` önyükleme doğrulaması: **`DONE (QEMU xHCI)` (`tools/build_live_usb_image.py --verify-boot`, `results_live_usb_boot.json`, `5.491s`; birleşik paket `135/135 PASS`)** / Fiziksel Lenovo `82K1` Gates 3–7 (fiziksel USB önyükleme, xHCI HID/Bölüm 2 kalıcılık, Iris Xe GOP + çift-DRHD VT-d + VMD NVMe RO, RTX 3050 Ti): **`READY FOR USER PHYSICAL EXECUTION`** (bkz. `docs/ASCENSION_5_0_VALIDATION_MATRIX.md`)
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
