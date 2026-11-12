# DRY OS — Ürün Vizyonu, Kurulum Profilleri ve Ürün Yol Haritası (`P0` – `P5`)

> **Belge Tarihi:** 12 Kasım 2026 (`Milestone 2A-2` / `DRY Desktop Preview` Güncellemesi)  
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
| **`P1` — Etkileşimli DRY OS** | **`IN PROGRESS`** *(`Milestone 2A-1` & `2A-2`: `DONE`, `79/79` QEMU)* | `Faz 2 — Milestone 2A-1` & `2A-2` (`DONE`) + `2A-3` & `2B` (`PLANNED`) | PS/2 klavye (`IRQ1`) + PS/2 fare (`IRQ12`), `dry-input` -> IPC -> `dry-shell` & `dry-sysmon` -> dinamik GOP ekranı (`DONE`); USB HID, `IoRing`, `VFS` (`PLANNED`) |
| **`P2` — DRY Desktop Alpha/Beta** | **`IN PROGRESS`** *(Preview Dilimi `Milestone 2A-2`: `DONE`, `79/79` QEMU)* | `Faz 2 — Milestone 2A-2` (`DONE`) + `2B/2C` + Ring 3 UI servisleri (`PLANNED`) | Yazılımsal imleç, çok-pencereli Ring 3 `Floating`/`SideBySideTiled` WM + 2 izole pencere (`DONE`); `Stacking/Tabbed`, `Action API`, `Clean/Diagnostic Boot`, Login (`PLANNED`) |
| **`P3` — Linux & Windows Uyumluluğu** | **`PLANNED`** *(`H2` PoC: `DONE`)* | `Faz 3` (Linux ELF + Sürücü Adaları) & `Faz 5` (Seçilmiş Win32) | Önce Linux ELF/ABI/POSIX/VFS + `H2` sürücü adaları; ardından seçilmiş Windows PE/Win32 uygulamaları |
| **`P4` — Paketleme ve Dağıtım** | **`PLANNED`** | `Faz 3` (`drypkg` çekirdeği) & `Faz 4` (`DRY Store` + Profiller) | Tasarım-öncelikli `drypkg` + `DRY Store`, isteğe bağlı runtime, imzalı paket, `Desktop`/`Server` kurulum profilleri, atomik *rollback* |
| **`P5` — Production Readiness** | **`PLANNED`** | `Faz 4` (Bare-Metal, IOMMU, Soak, Benchmark) | Gerçek donanım + USB live boot, Desktop/Server iş yükü testleri, adil Linux A/B benchmarkları, 72s+ soak/fuzz ve açık raporlar |

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

### `P1` — Etkileşimli DRY OS — Durum: `IN PROGRESS` *(`Milestone 2A-1` & `2A-2`: `DONE` — `79/79` QEMU; Kalan Cihaz/Servis Dilimleri: `PLANNED`)*

- **Alt Hedefler (Dürüst Bileşen Durumu):**
  - Donanım IRQ yönlendirme ilkeli (`SYS_IRQ_BIND`/`SYS_IRQ_ACK` + `IoApicHwCache`: ACPI MADT `IOAPIC`/ISA `IRQ1`+`IRQ12` -> `Notification`, tek sahip + `IRQF_ONESHOT` mask/ack + fırtına kelepçesi) ve dar `IoPortCap` (`IOPL=0`): **`DONE` (`IOAPIC`/ISA)** / **`PLANNED` (`MSI-X`)**
  - Gerçek klavye girdisi (QEMU `PS/2 i8042` `IRQ1` -> Ring 3 `dry-input.elf`, US QWERTY + `0xE0` + 64-olay taşma koruması; `TurkishQ` genişletme arayüzü tanımlı): **`DONE` (`PS/2` US QWERTY)** / **`PLANNED` (`VirtIO-Input`, USB HID, `TurkishQ` tablosu)**
  - Fare / işaretçi girdisi (Tek `i8042` sahibi `dry-input.elf` içinde gerçek `PS/2 AUX` `IRQ12` sürücüsü, 3-bayt senkron kilidi, `MouseMove`/`MouseButtonDown`/`MouseButtonUp` ve `12x16` kirli-dikdörtgen yazılımsal imleç): **`DONE` (`PS/2 AUX`)** / **`PLANNED` (`VirtIO-Input`, USB `xHCI` HID)**
  - Capability IPC üzerinden çalışan izole Ring 3 interaktif terminal (`dry-shell.elf`, `0x0102`) ve canlı sistem izleyicisi (`dry-sysmon.elf`, `0x0103`, sıfır donanım/FB yetkisi): **`DONE`**
  - Canlı tuş/fare girdisi -> `4 MiB` (`2 x Frame2M` WB) + `DirtyRegion` GOP ekran güncellemesi, başlık çubuğu sürükleme, odak/Z-sırası, `SideBySideTiled` düzen ve QMP `screendump` piksel kanıtları (`docs/p1_interactive_shell.png`, `docs/p2_desktop_preview.png`): **`DONE`**
  - Temel donanım/sistem servisleri (`IoRing` paylaşımlı bellek veri düzlemi, `TCB`-bağlı `Notification`, `FaultEndpoint`, FPU `XSAVEOPT` ve başlangıç etiketli `VFS`): **`PLANNED` (`Milestone 2A-3` / `2B`)**
- **Bağımlılıklar:** `P0` (`DONE`); `Milestone 2A-1` & `2A-2` klavye/fare/WM/ekran zinciri tamamlandı; yüksek bant genişlikli veri düzlemi ve VFS `Milestone 2A-3`/`2B`'ye bağlıdır.
- **Azaltılan & Açık Riskler:** `IRQF_ONESHOT` fırtına kelepçesi (`R-14`), birleşik `i8042` + sıfır-UB atomik yüzey + `4 MiB` arka tampon denetimi (`R-15`), `DirtyRegion` imleç aktarımı (`92 px = ~10.9k cyc`, `R-12`) ve `248 B` sıcak syscall yığın bütçesi (`R-11`) `79/79` QEMU testiyle doğrulandı.

---

### `P2` — `DRY Desktop` Alpha/Beta — Durum: `IN PROGRESS` *(Preview Dilimi `Milestone 2A-2`: `DONE`; Tam Masaüstü: `PLANNED` — `docs/DRY_DESKTOP_SPEC.md`)*

- **Alt Hedefler:**
  - Birleşik Ring 3 Display & Input servisi ve sıfır-UB atomik `SharedWindowSurface` (`Frame4K`) önizlemesi **`DONE`**; piksel-tamponlu `IoRing` + `SharedPayloadDesc` yüzeyleri: **`PLANNED`**
  - Tek Ring 3 Compositor üzerinde **varsayılan Floating** + tek tık **2-Sütun `SideBySideTiled`** (`Alt+T` / üst bar) önizlemesi **`DONE`**; tam dinamik ağaç + **Stacking / Tabbed** + transient diyalog kuralları: **`PLANNED`**
  - `DRY Shell`: önizleme üst sistem çubuğu (`MODE`, `FOCUS`, `WINS`) **`DONE`**; tam `liquid-glass` bar, global arama, aktif uygulama menüleri, koyu/açık tema, **Normal**/**Compact UI** yoğunluğu: **`PLANNED`**
  - Fare, klavye kısayolları, bağlam menüleri ve komut paletini birleştiren ortak **`Action API`**: **`PLANNED`**
  - Tek olay akışlı **`Clean Boot`** (varsayılan) ve **`Diagnostic Boot`** (`Esc` ile canlı geçiş, sıfır sahte yüzde) + parola kasasından ayrılmış **`DRY Login` / Authentication Manager**: **`PLANNED`**
  - Kullanılabilir temel sistem uygulamaları: Terminal (`dry-shell`) ve Sistem İzleyici (`dry-sysmon`) önizlemeleri **`DONE`**; Dosya Gezgini ve Ayarlar: **`PLANNED`**
- **Bağımlılıklar:** `P1` (`Input + IoRing + VFS`), kullanıcı alanında kayan nokta/vektör çizimi için çekirdek FPU/SIMD (`XSAVEOPT`/`XRSTOR`) bağlam değişimi ve `DRY Vault` oturum kiralaması.
- **Teknik Riskler:** Yazılımsal GOP kipinde bulanıklık/alfa karışımı maliyeti (`R-12` — donanım *fallback* zorunlu); pencere kuralları ve odak yönetiminde durum karmaşıklığı.

---

### `P3` — Linux ve Windows Uyumluluğu — Durum: `PLANNED` *(`H2` Ağ Sürücü Adası PoC: `DONE`)*

- **Alt Hedefler (Kesin Öncelik Sırasıyla — `ADR-011`):**
  1. **Öncelik 1 — Linux ELF / Linux ABI / POSIX / VFS (`PLANNED`):** `FaultEndpoint` / `SYSCALL` yönlendirme trampolini, izole userspace Linux personality sunucusu, etiketli VFS/soket köprüsü ve değiştirilmemiş gerçek Linux CLI/geliştirici/sunucu uygulamalarının doğrulaması.
  2. **Sürücü ve Sistem Servisi Entegrasyonu (`PLANNED`, PoC `DONE`):** `H2` `igc`/`e1000e`/USB `xHCI` `GPL-2.0-only` sürücü adalarının DRY `PciCap` + `DmaArena` + `IoRing` yığınına bağlanması.
  3. **Öncelik 2 — Seçilmiş Windows PE / Win32 Uygulamaları (`PLANNED` — Uzun Vadeli):** Linux katmanı kararlılığa ulaştıktan sonra seçilmiş kullanıcı alanı Win32 iş yükleri için `dry-ntos` + Wine DLL + `DXVK` çalışma zamanı.
- **Bağımlılıklar:** `P1`/`P2` (`FaultEndpoint`, dinamik ELF/`mmap`, etiketli `VFS`, ağ/depolama servisleri, FPU `XSAVE`).
- **Teknik Riskler & Dürüst Sınır:** `fork`/`clone`/`futex`/`epoll` semantik uç durumları; **Linux ve Windows uygulamalarının `%100`'ünün çalışacağı vaat edilmez** (çekirdek modu anti-cheat ve belgesiz çekirdek sürücüleri kapsam dışıdır — `R-13`).

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

### `P5` — Production Readiness — Durum: `PLANNED`

- **Alt Hedefler:**
  - Gerçek fiziksel x86-64 donanım (`bare-metal`) kurulumu, fiziksel Intel VT-d / AMD-Vi IOMMU doğrulaması ve USB Live Boot imajı: **`PLANNED`**
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
