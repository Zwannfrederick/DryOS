# DRY OS — Mimari Karar Kayıtları (Architecture Decision Records)

> **Tarih:** 10 Kasım 2026  
> **Durum:** Geliştirme Öncesi Temel Kararlar (ADR-001 .. ADR-006)

---

## ADR-001: x86-64 Önyükleme Protokolü Olarak Limine Seçimi

- **Durum:** Kabul Edildi (10 Kasım 2026)
- **Bağlam:** Faz 1'de x86-64 çekirdeğinin 64-bit Long Mode, yüksek yarı (*Higher-Half Direct Map - HHDM*), bellek haritası, ACPI (`MADT`, `DMAR`, `MCFG`, `SRAT`) tabloları, GOP çerçeve tamponu ve çok-çekirdekli AP (*Application Processor*) başlatmasıyla ayağa kalkması gerekiyor. Saf UEFI PE önyükleyici yazmak, çekirdek mantığına geçmeden önce yüzlerce satır önyükleme geçiş kodu ve AP gerçek-mod trampolini (`INIT-SIPI-SIPI` 16-bit -> 64-bit) gerektirir.
- **Karar:** Varsayılan önyükleme protokolü olarak **Limine Boot Protocol** (`limine-rs` crate / C başlık uyumlu istek blokları) kullanılacaktır.
- **Gerekçe ve Ödünleşimler:**
  - `[DOĞRULANDI]` Limine hem UEFI (`OVMF.4m.fd` bu makinede `/usr/share/edk2/x64/` altında mevcuttur) hem BIOS üzerinde çalışır, AP çekirdeklerini doğrudan 64-bit Long Mode'da uyandırabilir ve HHDM + RSDP adreslerini doğrudan teslim eder.
  - *Ödünleşim:* Harici önyükleyici ikilisine bağımlılık oluşur; ancak çekirdek giriş katmanı soyutlanarak ileride doğrudan UEFI Stub ekleme kapısı açık tutulacaktır.

---

## ADR-002: x86-64 Syscall ABI, Hata Modeli ve Esnek Çağrı Yüzeyi

- **Durum:** Kabul Edildi — Çağrı listesi Faz 1'de evrilebilir (10 Kasım 2026)
- **Bağlam:** x86-64 `SYSCALL` buyruğu donanımsal olarak `RCX` (kullanıcı `RIP`) ve `R11` (kullanıcı `RFLAGS`) yazmaçlarını ezer `[DOĞRULANDI]`. Ayrıca çekirdek sıcak yolunda kullanıcı belleğinden pointer okumak sayfa hatası ve TOCTOU yüzeyleri açar.
- **Karar:**
  1. **Yazmaç Kuralı:**
     - Giriş: `RAX` = `[63:32] flags/subop | [15:0] syscall_no`; Kontrol argümanları: `RDI` (`arg0`/hedef `CapPtr`), `RSI` (`arg1`), `RDX` (`arg2`), `R10` (`arg3` — `RCX` yerine), `R8` (`arg4`), `R9` (`arg5`).
     - Sıcak IPC Mesaj Yazmaçları: `R12`, `R13`, `R14`, `R15` (`mr0..mr3` = 32 bayt satır-içi yük; kopyasız yazmaç aktarımı).
     - Soğuk Yapılandırma Yolu: 6+ argümanlı soğuk çağrılar (`dry_thread_ctrl` vb.) göreve atanmış sabitlenmiş (*pinned*) 4 KiB **IPC Buffer** sayfasını kullanır.
  2. **Hata ve Dönüş Modeli:**
     - `RAX >= 0`: Başarılı dönüş (veya birincil değer/mesaj etiketi); `RDX`: İkincil dönüş (alınan `badge` veya yeni slot indeksi).
     - `RAX < 0`: Deterministik negatif hata kodu (`-1 = EPERM_CAP`, `-2 = ENOENT_CAP`, `-3 = EBUSY_PINNED`, `-4 = EWOULDBLOCK`, `-5 = ETIMEDOUT`, `-6 = EREVOKED`, `-7 = EINVAL`).
  3. **Yetki (Capability) Aktarımı:** Yalnızca `dry_ipc_call` içinde, göndericinin `MsgTag` bayrağı açıkça yetki devri istediğinde ve alıcının `Endpoint`/IPC Buffer kabul penceresi açık olduğunda tek bir capability atomik olarak türetilir/taşınır (*ambient authority* yoktur).
  4. **API Evrimi:** `ARCHITECTURE.md` içindeki 12 çağrı nihai/donmuş kabul edilmez; `FaultEndpoint`, `TCB`-bağlı `Notification` ve `iommu_pin_count` kuralları zorunludur, alt-operasyonlar Faz 1 prototipinde sadeleştirilebilir.

---

## ADR-003: Capability Tablosu (`CSpace`) — Önce Düz Tablo, Sonra İki Seviyeli Radiks

- **Durum:** Kabul Edildi (10 Kasım 2026)
- **Bağlam:** Tam 2 seviyeli dinamik radiks `CSpace` ve karmaşık türetme ağacı (*Capability Derivation Tree - CDT*), Faz 1'de önyükleme ve ilk IPC doğrulamasını geciktirir.
- **Karar:**
  - **Faz 1–2 (Başlangıç):** Süreç başına sabit boyutlu tek seviyeli korumalı dizi (*Flat Guarded CNode*, ör. 4 KiB veya 16 KiB sabit çerçeve içinde 128–512 adet 32-baytlık slot) + nesne başına atomik `generation: AtomicU32` ve `iommu_pin_count: AtomicU32`.
  - **Faz 3+:** `CapPtr` 64-bit formatı şimdiden radiks genişlemesine uyumlu tanımlanır (`[63:56] guard_bits | [31:0] slot_addr`), böylece kullanıcı ABI'sini kırmadan 2 seviyeli radiks ağacına ve alt-ağaç epoch `Revoke` yapısına geçilir.

---

## ADR-004: Zamanlayıcı Evrimi — Önce Doğrulanabilir Öncelikli RR + PI, Sonra Topolojik Work Stealing

- **Durum:** Kabul Edildi (10 Kasım 2026)
- **Bağlam:** Sanal çalışma süreli (`EEVDF`), hibrit P/E kapasite ağırlıklı, çok-NUMA iş çalan bir zamanlayıcıyı ilk günden yazmak hataları gizler ve resmi doğrulamayı zorlaştırır.
- **Karar:**
  1. **Faz 0–1 (Temel):**
     - Görev uyuma/uyanma ve stack sahipliği için yalnızca TLC + Loom ile doğrulanmış **H1 `v2` (`AtomicU32(exec_state, park_token)`)** protokolü kullanılır.
     - CPU başına sabit öncelik bit-maskeli (`u64` bitmap, 64 öncelik seviyesi) intrüzif FIFO runqueue (`TCB.rq_node`, sıfır heap tahsisi) + senkron IPC üzerinde doğrudan **Öncelik Bağışı (Priority Inheritance/Donation)** uygulanır.
     - Runqueue ekleme/çıkarma başlangıçta CPU-yerel çok kısa (kesme maskeli, ~15 döngü) `rq_lock` ile korunur; böylece kuyruk taşması (*overflow*) ve Chase-Lev ABA riski Faz 1'de sıfırlanır.
  2. **Faz 3–4 (Ölçekleme):** Temel zamanlayıcı doğrulandıktan sonra `EXEC_READY` görevler için LLC/SMT/NUMA farkındalıklı *work stealing*, `NO_HZ_FULL` ve açlık önleyici yaşlandırma (*aging* / sanal deadline) katmanı eklenir.

---

## ADR-005: Lisans Mimarisi ve Üçüncü Parti Bağımlılık Duvarı

- **Durum:** Kabul Edildi (10 Kasım 2026)
- **Bağlam:**
  - `[DOĞRULANDI]` Linux çekirdek sürücüleri (`igc.ko` dahil) ve çekirdek içi başlıklar **`GPL-2.0-only`** lisanslıdır.
  - `[DOĞRULANDI]` FSF ve ASF yorumuna göre saf **`Apache-2.0`** lisansı, patent fesih maddeleri sebebiyle **`GPL-2.0-only` ile hukuken uyumsuzdur**; `GPL-3.0` da `GPL-2.0-only` ile uyumsuzdur.
- **Karar:**
  1. **DRY Kernel ve Native Rust Servisleri / SDK:** **`MIT OR GPL-2.0-only`** ikili lisansıyla lisanslanır.
  2. **Linux Driver Islands (`libdry_lx_*` + çevrilen sürücü ikilileri):** Doğrudan **`GPL-2.0-only`** lisanslanır. Sürücü adası içine bağlanan (*statically/dynamically linked*) her yardımcı kütüphane zorunlu olarak `GPL-2.0-only`, `MIT` veya `BSD-2/3-Clause` olmalıdır.
  3. **Süreç Sınırı (Galvanic Separation):** Sürücü adaları native DRY servisleriyle Linux veri yapıları üzerinden değil, yalnızca işletim sisteminin genel amaçlı `IoRing` / `Notification` protokolü üzerinden ayrı süreçler olarak konuşur.
  4. **Otomatik CI Denetimi:** Depo kurulduğunda `cargo-deny` yapılandırması ile `Apache-2.0` (MIT alternatifi olmayan saf Apache-2.0) ve `GPL-3.0`/`LGPL-3.0` paketlerinin `GPL-2.0-only` bileşenlere bağlanması derleme hatası sayılır.

---

## ADR-006: Deney ve Geliştirme Sırası — Önce H1, Sonra H2; Çekirdeğin H2'den Bağımsızlaştırılması

- **Durum:** Kabul Edildi (10 Kasım 2026)
- **Bağlam:** Hem zamanlayıcı eşzamanlılık protokolü (H1) hem de Linux sürücü adası fizibilitesi (H2) kanıt beklemektedir; ancak tüm işletim sisteminin kaderini "her Linux sürücüsünün otomatik çevrilebilmesi" şartına bağlamak proje için varoluşsal tehlikedir.
- **Karar:**
  1. **Faz 0 Sırası:** Yarın (11 Kasım 2026) önce bağımsız, hızlı ve kesin yanlışlanabilir olan **H1 (TLC + Rust Loom + stres testi)** yürütülecek; hemen ardından **H2 (`igc` bağımlılık otomasyonu + `VFIO`/QEMU Driver Island PoC)** çalıştırılacaktır.
  2. **Mimari Bağımsızlık Kuralı:** `dry-kernel` (Faz 1) ve temel DRY OS (Faz 2), native Rust `VirtIO` ve `NVMe` sürücüleriyle **H2 tamamen başarısız olsa bile** kendi başına önyüklenen, çalışan ve test edilebilen eksiksiz bir mikroçekirdek işletim sistemi oluşturacaktır. H2 yalnızca Faz 3'te donanım kapsamını hızlandıran bir çarpan olarak konumlandırılır.

