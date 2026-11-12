# DRY OS — Mimari Karar Kayıtları (Architecture Decision Records)

> **Tarih:** 10–12 Kasım 2026  
> **Durum:** Aktif Mimari ve Ürün Kararları (`ADR-001` .. `ADR-012`)

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
- **`Milestone 1B-D` Uygulama Kaydı (`kernel/src/{syscall/,uaccess.rs,proc/}` & `userspace/`):**
  - **MSR & Giriş/Çıkış Kapısı:** Her CPU üzerinde `IA32_EFER.SCE = 1`, `IA32_STAR = (0x0010 << 48) | (0x0008 << 32)`, `IA32_LSTAR = dry_syscall_entry` ve `IA32_FMASK` (`TF | IF | DF | IOPL | NT | AC` temizleme) programlandı. `dry_syscall_entry`, `swapgs` + `gs:[200]`/`gs:[192]` ile yığın değiştirir, 144 baytlık `SyscallFrame` kurar, dönüşte `validate_user_context` ile kanonik alt-yarı `RCX`/`RSP` ve `RFLAGS` doğrulaması yaparak `SYSRETQ` (`#GP` açığı kapalı) veya `IRETQ` yedek yolunu seçer ve `RDI, RSI, R8, R9, R10` yazmaçlarını sıfırlar.
  - **Sürümleme ve İlk 6 Çağrı (`ABI v0.1 = 0x0001_0000`):** `SYS_ABI_PING (0)`, `SYS_WRITE_DEBUG (1)`, `SYS_YIELD (2)`, `SYS_EXIT (3)`, `SYS_GET_TID (4)`, `SYS_CAP_QUERY (5)`; geçersiz çağrılar deterministik `-38` (`ENOSYS`), hatalı pointer'lar `-14` (`EFAULT`), geçersiz/yetkisiz capability `-9` (`EBADCAP`) döner.
  - **`Milestone 1C` IPC, Süpervizör ve Çerçeve Tamponu Uzantısı (`6..19`):** `SYS_EP_CREATE (6)`, `SYS_IPC_SEND (7)`, `SYS_IPC_RECV (8)`, `SYS_IPC_CALL (9)`, `SYS_IPC_REPLY (10)`, `SYS_IPC_REPLY_RECV (11)`, `SYS_NOTIF_CREATE (12)`, `SYS_NOTIF_SIGNAL (13)`, `SYS_NOTIF_WAIT (14)`, `SYS_CAP_DERIVE (15)`, `SYS_CAP_REVOKE_OR_DEL (16)`, `SYS_PROC_SPAWN (17)`, `SYS_PROC_WAIT (18)` ve `SYS_FB_MAP (19)` eklendi. `R12..R15` kopyasız satır-içi 4 mesaj yazmacı, tek-atımlık nesil-korumalı `ReplyToken`, `CSpace::cspace_transfer` yetki devri ve `-95` (`EIRQCTX`) / `-35` (`EDEADLK`) / `-32` (`EPIPE`) deterministik hata kodları yürürlüğe alındı.
  - **`Milestone 2A-1` (`P1 First Interaction`) Donanım IRQ, Dar Port G/Ç ve Yüzey Uzantısı (`20..25`):** Sıcak `rust_syscall_dispatch` çerçevesini (`248 B`) hiç büyütmeden soğuk `sys_cold_dispatch` (`216 B`) ve hızlı `irq` yolu üzerinden `SYS_IRQ_BIND (20)`, `SYS_IRQ_ACK (21)` (çift-ack `-114` `EALREADY` reddi), `SYS_IOPORT_RW (22)` (`IOPL=0` korunarak dar `IoPortCap` kontrolü), `SYS_SYSINFO (23)` (`UserSysInfo` 80 B), `SYS_FRAME_ALLOC (24)` ve `SYS_VSPACE_MAP (25)` eklendi (`ADR-012`).
  - **Donanım `SMAP` + İstisna Tablosu (`extable`):** `CPUID` ile `CR4.SMAP = 1` etkinleştirildi; `copy_from_user`/`copy_to_user` alt-yarı `U/S = 0` sayfaları reddeder ve daraltılmış `STAC..CLAC` bölgesi içindeki `#PF` tuzaklarını `-14` (`EFAULT`)'a çevirirken bölge dışındaki her Ring 0 `#PF` çekirdeği deterministik panikletir.

---

## ADR-003: Capability Tablosu (`CSpace`) — Önce Düz Tablo, Sonra İki Seviyeli Radiks

- **Durum:** Kabul Edildi (10 Kasım 2026) — `Milestone 1B-C` ile Faz 1 Düz Korumalı `CNode` + CDT Uygulandı (11 Kasım 2026)
- **Bağlam:** Tam 2 seviyeli dinamik radiks `CSpace` ve karmaşık türetme ağacı (*Capability Derivation Tree - CDT*), Faz 1'de önyükleme ve ilk IPC doğrulamasını geciktirir.
- **Karar:**
  - **Faz 1–2 (Başlangıç):** Süreç başına sabit boyutlu tek seviyeli korumalı dizi (*Flat Guarded CNode*, ör. 4 KiB veya 16 KiB sabit çerçeve içinde 128–512 adet slot) + nesne başına atomik `generation: AtomicU32` ve `iommu_pin_count: AtomicU32`.
  - **Faz 3+:** `CapPtr` 64-bit formatı şimdiden radiks genişlemesine uyumlu tanımlanır (`[63:56] guard_bits | [55:48] radix_bits | [47:16] slot_gen | [15:0] slot_idx`), böylece kullanıcı ABI'sini kırmadan 2 seviyeli radiks ağacına ve alt-ağaç epoch `Revoke` yapısına geçilir.
- **`Milestone 1B-C` & `1B-D` Uygulama Kaydı (`kernel/src/cspace/`):**
  - `MAX_CSPACES = 32` (`CSPACE_TABLE[0]` çekirdek rezervi, `1..31` kullanıcı `CSpace` yuvası), `CNODE_SLOTS = 256` (`CNODE_RADIX_BITS = 8`), önbellek satırı hizalı `64 B` `CapSlot` (`CNode` başına `16 KiB`, toplam `514 KiB` `CapSlot + 512 CapMapRecord` havuzu; slot `0 = CAP_NULL`).
  - Kilitsiz (`lock-free`) seqlock `cspace_lookup_fast` (`SeqCst` `slot_gen` + canlı nesne `generation` çift ABA kalkanı), `slot.in_flight_ops` aktif işlem pini, `CSpace`-sahibi `TCB` yuvalarının `cspace_destroy_object` (`release_dead_tcb_slot`) tamamlanana dek yeniden tahsis edilmemesi ve `CSpace`'ler arası intrüzif $O(k)$ ebeveyn-çocuk CDT bağlantısı.
  - Deterministik `revoke` sıralaması: **`Live -> Revoking` (`slot_gen + 1`) -> `in_flight_ops == 0` süzme -> `CapMapRecord` otomatik `VSpace` `unmap_4k` + senkron SMP TLB shootdown -> `EmptyRevoked` / geri kazanım**. `map_ref_count > 0`, `iommu_pin_count > 0`, `VSpace.active_cpus != 0` veya `TCB` `ExecState != Dead || stack_owner != NO_CPU` iken erken nesne yıkımı `Err(BusyActiveObject)` ile reddedilir.

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

---

## ADR-007: Güvenlik Etiketleri (`SecLabel`), Alan İzolasyonu ve `DRY Vault` Mimarisi

- **Durum:** Kabul Edildi (11 Kasım 2026)
- **Bağlam:** Geleneksel UNIX/Linux sistemlerinde `uid == 0` (`root`) veya geniş kapsamlı `CAP_SYS_ADMIN`, tek bir ele geçirilmiş servisin tüm sistem dosyalarını değiştirmesine, `/dev/mem` veya yüklenebilir çekirdek modülleri üzerinden Ring 0 çekirdek belleğine sızmasına ve path/symlink yarışlarıyla (*confused deputy*) yetki yükseltmesine yol açar. Öte yandan her yönetimsel işlemde parola/onay soran sistemler de onay yorgunluğu (*prompt fatigue*) yaratarak kullanıcıyı kalıcı admin kabuğu açmaya iter. DRY OS mikroçekirdek saflığını bozmadan zorunlu alan izolasyonu, nesne etiketleme ve ergonomik süreli yetkilendirme sağlamalıdır.
- **Karar (12 Temel Güvenlik Kuralı):**
  1. **Üç Güvenlik Alanı (`SecDomain`):** Sistem kesin sınırlarla ayrılmış üç alandan oluşur: `USERSPACE` (`2`), `ROOT_SYSTEM` (`1`) ve `KERNEL` (`0`).
  2. **Varsayılan Red ve Kontrollü Salt-Okunur Sistem Paylaşımı:** Normal `USERSPACE` süreçleri yalnızca açıkça yetkili oldukları `USERSPACE` nesnelerine yazabilir; `ROOT_SYSTEM` ve `KERNEL` nesnelerine erişim varsayılan olarak reddedilir (`EPERM_CAP`). Gerekli sistem kütüphaneleri ve salt-okunur paylaşımlar (`libc`, `dry-sdk`, yazıtipleri) yalnızca `SecFlags::READ_ONLY_EXPORT` bayrağıyla ve kesin `W^X` (`R` veya `R|X`, **asla `W` değil**) garantisiyle sunulur.
  3. **Zorunlu Güvenlik Etiketleri (`SecLabel`):** Dosyalar/dizinler, süreçler (`TCB`/`CSpace`), IPC uç noktaları (`Endpoint`/`Notification`), bellek çerçeveleri (`Frame`/`DmaArena`) ve cihaz nesneleri (`PciCap`/`IrqCap`) 64-bit sabit boyutlu bir `SecLabel { domain, flags, compartment_id, policy_tag }` taşır.
  4. **Ebeveyn Mirası ve Yetki Yükseltmeyen Ad Alanı İşlemleri:** Yeni oluşturulan dosya ve dizinler varsayılan olarak ebeveyn dizinin `SecLabel` etiketini miras alır (`dry_obj_retype` da `Untyped` ebeveyn etiketini miras alır). Taşıma (`rename`), `symlink`, `hardlink` veya `mount` işlemleri **asla yetki yükseltmez**: `hardlink` hedef inode üzerinde yazma/bağlama yetkisi gerektirir, `symlink` ortam yetkisi (*ambient authority*) taşımaz ve `USERSPACE` görünürlüğündeki `mount` işlemleri alan tavanını aşamaz.
  5. **Path/`cwd` Bağımsız Çift-Faktörlü Denetim:** Erişim denetimi hiçbir zaman yalnızca dosya yoluna (*pathname*) veya çalışma dizinine (`cwd`) dayanmaz; her işlem **Nesne `SecLabel` + Süreç Capability Seti** kesişimini (`Authorize(SubjLabel, SubjCap, ObjLabel)`) doğrular.
  6. **Kapsamlı `ROOT_SYSTEM` Developer Workspace:** `ROOT_SYSTEM` alanında ayrı bir `DEV_WORKSPACE` (`compartment_id` ile izole edilmiş geliştirici çalışma alanı) oluşturulabilir. Kullanıcı bu alanda yalnızca yetkilendirilmiş kapsam dahilinde derleme, paket yönetimi ve servis işlemleri yürütebilir; kapsam dışı sistem sırlarına (`DRY Vault` anahtarları vb.) erişemez.
  7. **`DRY Vault` ve Süreli/Kapsamlı Yetki Kiralama (*Lease*):** `DRY Vault`, kimlik doğrulamayı yapar ve belirli süre (`lease_epoch` / `expires_tsc`) ile belirli kapsam (`compartment_id` + daraltılmış hak maskesi) için geçerli yetkiler üretir. Aktif kiralama süresi boyunca tekrar eden geliştirme/paket/servis işlemleri için kullanıcıdan tekrar tekrar izin istenmez.
  8. **Mutlak Çekirdek Belleği Duvarı:** `ROOT_SYSTEM` yetkisi bile doğrudan `KERNEL` belleğine okuma veya yazma erişimi **vermez** (`/dev/mem`, Ring 3 HHDM eşlemesi, keyfi `MSR`/`CR` yazımı ve yüklenebilir Ring 0 modülü yoktur; IOMMU ikinci seviye tabloları da `KERNEL` çerçevelerini reddeder).
  9. **Canlı Etkinleşme vs. Sonraki Önyüklemede Çekirdek Güncellemesi:** Userspace servis, sürücü, paket ve politika değişiklikleri mümkün olan her durumda Linux benzeri biçimde (atomik nesil değişimi / servis devriyle) **anında canlı etkinleşir**. `dry-kernel` çekirdek güncellemeleri ise canlı Ring 0 yaması yapmaz; imzalı A/B önyükleme yuvasına yazılıp **sonraki boot'ta** etkinleşir ve kullanıcıya reboot önerilir.
  10. **Uçtan Uca Denetim Noktaları, Devir ve İptal:** Yetki ve etiket denetimi **VFS, IPC, bellek eşleme (`VSpace`), cihaz erişimi (`PciCap`/`IOMMU`) ve ayrıcalıklı servis çağrılarında** zorunlu uygulanır. Capability devri (`Mint`/`Grant`) yalnızca monotonik hak/süre daraltmayla (`NO_DELEGATE` kapalıysa) yapılabilir; iptal (`Revoke` veya lease bitimi) nesil sayacı + TLB/IOMMU temizliğiyle deterministik yürütülür.
  11. **Mikroçekirdek Sorumluluk Ayrımı:** `DRY Vault`, `VFS` ve `Policy Broker` **izole userspace (`ROOT_SYSTEM`) servisleridir**. `dry-kernel` çekirdek içinde dosya yolu, parola veya karmaşık politika dili barındırmaz; yalnızca `SecLabel` saklama, $O(1)$ alan/hak denetimi, sahtelenemez gönderici kimliği (`badge` + `sender_label`) ve `Revoke`/lease ilkellerini uygular.
  12. **Deny-by-Default ve Kapatılamaz Güvenlik:** Varsayılan politika **deny-by-default**'tur. Admin veya Developer Workspace hiçbir zaman güvenlik kontrollerinin kapatılması (`setenforce 0` / sınırsız `CAP_SYS_ADMIN`) anlamına gelmez.

### ADR-007 Tehdit Modeli (`T-01` .. `T-06`)

| Kod | Tehdit Senaryosu | Mimari Savunma Mekanizması |
| :--- | :--- | :--- |
| **`T-01`** | **Ele Geçirilmiş `USERSPACE` Süreci & Path/Link Yarışı:** `symlink`, `hardlink`, `rename` veya `..` TOCTOU yarışıyla `/system` kütüphanelerini ya da başka kullanıcı verisini ezme girişimi. | Path/`cwd` yerine hedef nesne `SecLabel` + açık capability kontrolü; ebeveyn etiket mirası; alanlar arası `hardlink` yasağı. |
| **`T-02`** | **Ele Geçirilmiş `ROOT_SYSTEM` Servisi / Admin Aracının Ring 0'a Sızması:** Root yetkili bir servisin çekirdek veri yapılarını (`TCB`, `CSpace`) bellekte yamama veya sahte DMA yönlendirme girişimi. | `SecDomain::Kernel` nesnelerinin hiçbir Ring 3 `VSpace` veya `IommuDomain` içine eşlenememesi (`SMEP`/`SMAP` + `dry_vspace_map` / `dry_iommu_bind` sert reddi). |
| **`T-03`** | **Yetkisiz Capability Sızıntısı ve Sahte IPC Kimliği:** İstemcinin kendisine ait olmayan yetkiyi devretmesi veya `Policy Broker` / `VFS`'e başka bir alan adına mesaj göndermesi. | `NO_DELEGATE` bayrağı, monotonik daraltma (`Mint`) ve çekirdeğin `dry_ipc_reply_recv` sırasında sahtelenemez `(badge, sender_label)` teslim etmesi. |
| **`T-04`** | **Süresi Dolmuş veya İptal Edilmiş Yetkinin Kullanımı (*Stale Lease*):** Developer Workspace oturumu kapandıktan sonra kalan eski capability ile paket/servis değiştirme girişimi. | Capability başlığındaki `lease_epoch` / `generation` alanlarının çekirdek ve `Policy Broker` sınırında atomik doğrulanması (`EREVOKED`). |
| **`T-05`** | **Paylaşılan Sistem Kütüphanesini Yazılabilir Eşleme:** `USERSPACE` sürecinin salt-okunur sistem kütüphanesi çerçevesini `PROT_WRITE` ile eşleyip diğer süreçleri zehirlemesi. | `SecFlags::READ_ONLY_EXPORT` + `W^X` kuralı: `dry_vspace_map` üzerinde `WRITE` talebinin çekirdek tarafından reddedilmesi. |
| **`T-06`** | **Onay Yorgunluğu (*Prompt Fatigue*) ve Sınırsız Admin Kabuğu:** Sürekli parola sorulması yüzünden kullanıcının tüm korumaları kapatan kalıcı root kabuğu açması. | `DRY Vault` süreli/kapsamlı `DEV_WORKSPACE` kiralaması + çekirdekte global güvenlik kapatma bayrağının bulunmaması. |

### ADR-007 Test Edilebilir Güvenlik Invariant'ları (`INV-SEC-01` .. `INV-SEC-08`)

1. **`INV-SEC-01 (NoRing3KernelMap)`:** `PTE_PRESENT | PTE_USER` bitleri açık hiçbir Ring 3 sayfa tablosu girişi veya `IommuDomain` eşlemesi `SecDomain::Kernel` etiketli fiziksel çerçeveyi gösteremez.
2. **`INV-SEC-02 (UserspaceWriteIsolation)`:** `subject.label.domain == Userspace` olan hiçbir özne, `object.label.domain in {RootSystem, Kernel}` olan bir nesne üzerinde `WRITE` / `MUTATE` hakkı elde edemez.
3. **`INV-SEC-03 (ImmutableSharedExport)`:** `SecFlags::READ_ONLY_EXPORT` taşıyan hiçbir nesne `WRITE` hakkıyla eşlenemez ve aktif eşlemesi varken yeniden tiplenemez (`dry_obj_retype` reddedilir).
4. **`INV-SEC-04 (MonotonicAttenuation)`:** Her capability türetme (`Mint` / `Grant`) adımında `child.rights ⊆ parent.rights` ve `child.lease_epoch <= parent.lease_epoch` sağlanır; `NO_DELEGATE` bayrağı varsa aktarım reddedilir.
5. **`INV-SEC-05 (NamespaceNonEscalation)`:** `rename`, `symlink`, `hardlink` veya `mount` üzerinden ulaşılan nesne üzerindeki etkin hak kümesi, nesneye doğrudan erişimdeki hak kümesini **asla aşamaz** ve nesnenin `SecLabel` değerini yükseltemez.
6. **`INV-SEC-06 (UnforgeableCallerLabel)`:** `dry_ipc_reply_recv` alıcısına iletilen `(badge, sender_label)` çifti doğrudan çekirdek `TCB`/`Endpoint` durumundan okunur; kullanıcı alanı bu alanları sahteleyemez.
7. **`INV-SEC-07 (DeterministicRevokeAndExpiry)`:** `Revoke` tamamlandığı veya `lease_epoch` süresi dolduğu andan itibaren türetilmiş tüm yetkiler `EREVOKED` (`-6`) döner; aktif eşlemeler TLB shootdown ile sökülür.
8. **`INV-SEC-08 (NonBypassablePolicy)`:** Çekirdek ve `Policy Broker` içinde `SecLabel` veya capability doğrulamasını devre dışı bırakan hiçbir çalışma zamanı bayrağı (`permissive mode` vb.) bulunmaz.

### `Milestone 1B-C` & `1C` Çekirdek Uygulama Kaydı ve Açık Kalan Faz Sınırları (11 Kasım 2026)

- **Doğrulanan Çekirdek Zorlamaları (`kernel/src/{security.rs,cspace/,vspace/,pmm/,ipc/,display.rs,proc/}`):**
  - Her `CSpace`, `Endpoint`, `Notification`, `BootModule` ve `Framebuffer` işleminde ortak `CapRights + SecDomain/SecLabel + lease_epoch` değerlendirmesi (`authorize_cspace_fast`, `p50 = 53..59 cyc`); `encode_compartment(user_ns, app_id)` ile kullanıcı/uygulama kompartman kodlaması; `Untyped::retype` ebeveyn etiket mirası.
  - `USERSPACE` / `ROOT_SYSTEM` / `KERNEL` duvarları (`ROOT_SYSTEM` özneleri dahi `KERNEL` nesnelerine erişemez ve eşleyemez); `NO_DELEGATE` ve `CapRights::GRANT` çapraz-kompartman aktarım kapıları (`cspace_transfer`); IPC üzerinden devredilen yetkilerin ebeveyn `revoke` edildiğinde geçişli CDT ile anında iptali (`-EREVOKED`).
  - `CapObjectKind::Framebuffer` yalnızca `RootSystem` kök süpervizörüne üretilir (`Userspace` talebi `Err(DomainViolation)` döner), `PTE_SW_MMIO` (`UC + NX`) ile kullanıcı adres alanına eşlenir ve `revoke`/çıkışta SMP TLB shootdown ile tamamen sökülür.
  - `DEV_WORKSPACE` süreli/kapsamlı `DRY Vault` kiralaması (`lease_epoch > 0 && !NO_DELEGATE` zorunlu; `now_epoch > lease_epoch` olduğunda anında `Err(LeaseExpired)`).
  - `SecFlags::READ_ONLY_EXPORT` için 3-katmanlı savunma (`CSpace` türetme/aktarım + `VSpace::protect_4k` yazma yükseltme engeli + `PMM` `map_ref_inc_data_frame` donanım sınırı: yalnızca `!writable && expected_owner.domain != Kernel` kabul edilir).
- **Henüz Uygulanmayan Gelecek Faz Sınırları (Dürüst Kapsam Beyanı):**
  - VFS dosya/dizin ad alanı kuralları (`INV-SEC-05`: `rename`, `symlink`, `hardlink`, `mount`, dosya tabanlı `mmap`), FPU/SIMD `XSAVE`/`XRSTOR` bağlam izolasyonu ve izole Ring 3 userspace `DRY Vault` / `Policy Broker` / `VFS` servis süreçleri Faz 2 kapsamındadır.

---

## ADR-008: Ürün Felsefesi ve Çift Kurulum Profili — Tek Çekirdek Üzerinde `DRY Desktop` ve `DRY Server`

- **Durum:** Kabul Edildi — Ürün ve Mimari Çerçeve (`PLANNED` Kurulum Profilleri, 11 Kasım 2026)
- **Bağlam:** Masaüstü ve sunucu için ayrı çekirdek çatalları (*forks*) veya ayrı paket/güvenlik modelleri geliştirmek hem doğrulama maliyetini ikiye katlar hem de mimari tutarlılığı bozar. Tersine, sunucu sürümünü yalnızca grafik arayüzü gizlenmiş bir masaüstü imajı olarak dağıtmak gereksiz arka plan servisleri, bellek tüketimi ve saldırı yüzeyi üretir.
- **Karar:**
  1. **Temel Mühendislik ve Ürün İlkeleri:**
     - *One kernel, multiple experiences:* Tek repository ve mümkün olan her hedef donanımda **tek `dry-kernel` ikilisi** kullanılır.
     - *Minimum overhead, maximum useful work* & *Zero unnecessary work:* Aynı işlevsellik, güvenlik ve doğruluk korunurken gereksiz CPU buyruğu, bellek kopyası, bağlam değişimi ve kilit çekişmesi ortadan kaldırılır.
     - *Rust by default, assembly by evidence:* Varsayılan dil güvenli/yalın `no_std` Rust'tır; satır-içi veya harici assembly yalnızca donanım sınırı (`SYSCALL`, `IDT`, `switch_to`, `SMAP`) veya `objdump`/döngü ölçümüyle kanıtlanmış sıcak yollarda kullanılır (`69522a3`).
     - *Security and isolation by architecture:* İzolasyon sonradan eklenen bir yama değil, `CSpace` + `ADR-007` `SecLabel` + Ring 3 servis sınırlarının doğal sonucudur.
     - *Functional minimalism, not feature deprivation:* Minimalizm, kullanıcının ihtiyaç duyduğu özellikleri silmek değil; aynı işlevi daha az kaynak, daha küçük güvenilir bilgi işlem tabanı (*TCB*) ve daha az karmaşıklıkla sunmaktır.
     - *Ölçeklenebilirlik ve Epistemik Dürüstlük:* Düşük sınıf dizüstü/istemci donanımından yüksek çekirdekli NUMA sunuculara kadar ölçeklenebilirlik hedeflenir. **QEMU/KVM mikrobenchmark sonuçları (`null_sys = 231 cyc`, `call_reply = 1.952 cyc`) mikroçekirdeğin Linux'tan hızlı olduğunu kanıtlamaz;** sentetik KVM ölçümleri gerçek fiziksel donanım ve bütün-sistem (*end-to-end*) iş yükü performansından her zaman ayrı raporlanır.
  2. **İki Birinci Sınıf Kurulum Profili (`PLANNED — P4`):**
     - **`DRY Desktop`:** `DRY Shell` ve tekil Ring 3 compositor, ekran/girdi/ses servisleri, `DRY Store`, grafik sistem uygulamaları; etkileşim gecikmesi (*frame pacing*, girdi tepkiselliği) ve enerji verimliliği öncelikli kaynak/zamanlayıcı politikaları.
     - **`DRY Server`:** Varsayılan olarak tamamen başsız (*headless*), sıfır masaüstü/compositor paket bağımlılığıyla temiz kurulum; CLI ve uzaktan yönetim araçları; yüksek verimli ağ/depolama/servis yığını; CPU yakınlığı (*affinity*), NUMA yerelliği, kaynak izolasyonu, yüksek eşzamanlı sunucu iş yükleri ve AI inference servislerine uygun bellek/G/Ç politikaları. **Server profili yalnızca arayüzü kapatılmış bir Desktop kurulumu değildir.**
  3. **Ortak Altyapı ve Çift Yönlü Geçiş:** Her iki profil aynı `drypkg` paket yöneticisini, aynı `Endpoint`/`Notification`/`IoRing` IPC mimarisini ve aynı `ADR-007` güvenlik modelini kullanır. Kurulum sonrasında sistemi yeniden biçimlendirmeden profil değiştirmek veya diğer profilin bileşenlerini eklemek/kaldırmak atomik paket nesilleriyle desteklenecektir.

---

## ADR-009: `DRY Desktop` / `DRY Shell` Pencere Yönetimi, Görsel Mimari ve Ortak `Action API`

- **Durum:** Kabul Edildi — İlk Dilim (`Floating` + 2-Sütun `Tiling` + Yazılımsal İmleç + 2 İzole Uygulama) `Milestone 2A-2` ile Uygulandı (`DONE — 79/79` QEMU/KVM); Tam `Stacking/Tabbed`, `Global Menu` ve `Action API` (`PLANNED — P2`; detaylar: `docs/DRY_DESKTOP_SPEC.md`)
- **Bağlam:** Mevcut masaüstü ortamları genellikle klavye-odaklı hız (tiling WM'ler) ile keşfedilebilirlik/rahatlık (geleneksel masaüstleri) arasında kullanıcıyı ikili bir seçime zorlar; görsel efektler ise ya düşük donanımı ezer ya da çekirdek/sürücü katmanına sızar.
- **Karar:**
  1. **Özgün Birleşik Tasarım Dili:** `DRY Shell`; Hyprland'in dinamik döşeme (*tiling*), çalışma alanı ve klavye hakimiyetini; KDE Plasma'nın zengin özelleştirme ve pencere kurallarını; macOS/GNOME çizgisinin tutarlı menü, global arama ve sadeliğini doğrudan kopyalamadan DRY OS'ye özgü tek bir tasarım dilinde birleştirir.
  2. **Tek Compositor Üzerinde Hibrit Pencere Yöneticisi:**
     - Varsayılan kip **Floating**'dir; ayarlardan veya kısayolla tek tıkla dinamik **Tiling** kipine geçilir; **Stacking** ve **Tabbed** pencere grupları yerleşik desteklenir.
     - Diyalog, açılır menü (*popup*), geçici (*transient*) ve küçük yardımcı pencereler varsayılan olarak `Floating` açılır ve arkadaki `Tiling` ağacını yeniden boyutlandırıp bozmaz.
     - `Floating <-> Tiling` geçişlerinde pencere geometrisi ve çalışma alanı yerleşimi korunur; çoklu çalışma alanı (*workspace*), kenar tutturma (*snap*) ve bildirimsel pencere kuralları tek Ring 3 compositor içinde çalışır.
  3. **Donanım-Uyarlamalı Görsel Tasarım ve Ring 3 Sınırı:**
     - Üstte `liquid-glass` esinli erişilebilirlik ve sistem çubuğu (global arama, aktif uygulama menüleri, sistem kontrolleri), koyu/açık tema, **Normal** ve **Compact UI** yoğunluk kipleri sunulur.
     - Tüm görsel efektler (ölçülü bulanıklık, gölge, akıcı animasyonlar) **kesin olarak Ring 3 compositor/userspace sorumluluğudur; `dry-kernel` içinde UI/efekt mantığı yer almaz.** Düşük donanımda veya yazılımsal GOP kipinde pahalı efektleri otomatik kapatan *fallback* profili ve kullanıcı erişilebilirliği için hareket azaltma / animasyon kapatma seçenekleri zorunludur.
  4. **Fare + Klavye Eşliği ve Ortak `Action API`:** Fare etkileşimleri, klavye kısayolları, bağlam menüleri (*context menus*), üst menü çubuğu ve komut paleti (*command palette*) aynı tipli **`Action API`** üzerinden uygulama ve kabuk komutlarını çağırır. Yeni kullanıcı her işlemi menülerden keşfedebilirken ileri seviye kullanıcı aynı eylemi klavye kısayolu veya komut paletiyle anında tetikler.

---

## ADR-010: Çift Açılış Deneyimi (`Clean Boot` / `Diagnostic Boot`) ve Kimlik Doğrulama Sınır Ayrımı

- **Durum:** Kabul Edildi — Tasarım Spesifikasyonu (`PLANNED — P2`, 11 Kasım 2026)
- **Bağlam:** Son kullanıcılar sessiz, temiz bir açılış beklerken sistem mühendisleri ve arıza giderme senaryoları gerçek zamanlı çekirdek/servis/güvenlik tanılama kayıtlarına ihtiyaç duyar. Ayrıca oturum açma bileşeni ile kullanıcının uygulama parolalarını tutan kasayı aynı süreçte birleştirmek güvenilir bilgi işlem tabanını (*TCB*) gereksiz büyütür.
- **Karar:**
  1. **Tek Olay Akışı, İki Userspace Görünümü:**
     - **`Clean Boot` (Varsayılan):** Koyu ve sade arka plan, merkezde DRY logosu ve ince gerçek ilerleme/etkinlik göstergesi; teknik metin kalabalığı olmadan hazır olunduğunda doğrudan `DRY Login` ekranına geçiş.
     - **`Diagnostic Boot`:** Arch Linux benzeri okunabilir canlı açılış günlüğü paneli; çekirdek, `ROOT_SYSTEM` servisleri, sürücüler ve `ADR-007` güvenlik doğrulama durumları; kritik arızalarda yapılandırılmış hata ve kurtarma görünümü.
     - Bu iki deneyim ayrı önyükleme mekanizmaları **değildir**; aynı yapılandırılmış önyükleme olay akışını tüketen iki görünüm kipidir ve önyükleme sırasında tek tuşla (ör. `Esc`) canlı olarak değiştirilebilir.
     - **Dürüst İlerleme Kuralı:** Sahte veya zamanlayıcıyla uydurulmuş yükleme yüzdeleri kesinlikle gösterilmez; yalnızca gerçek servis/birim hazırlık geçişleri yansıtılır.
  2. **Kimlik Doğrulama ile Parola Kasası Ayrımı:**
     - **Login / Authentication Manager (`SecDomain::RootSystem`):** Kullanıcı oturumunu doğrular ve `DRY Vault` üzerinden oturum/çalışma alanı `lease_epoch` yetkilerini başlatır.
     - **User Password Manager / Secret Store (`SecDomain::Userspace` izole kompartman):** Kullanıcının web/uygulama parolalarını ve kişisel sırlarını tutan ayrı şifreli kasadır; önyükleme veya oturum açma yöneticisinin TCB'si içine dahil edilmez.

---

## ADR-011: Paket Yöneticisi (`drypkg`), `DRY Store` ve Kademeli Uyumluluk Mimarisi

- **Durum:** Kabul Edildi — Mimari Çerçeve (`PLANNED — P3/P4`, 11 Kasım 2026)
- **Bağlam:** Mikroçekirdek tabanlı bir işletim sisteminin günlük kullanımda benimsenebilmesi için paket kurulumu, çalışma zamanı (*runtime*) yönetimi ve yabancı ikili uyumluluğu kullanıcıya karmaşık manuel adımlar yüklememeli; ancak bunu yaparken kullanılmayan dev uyumluluk katmanlarını sisteme zorla kurmamalı ve gerçekçi olmayan uyumluluk vaatlerinde bulunmamalıdır.
- **Karar:**
  1. **Ortak Motor (`drypkg` + `DRY Store`):** Hem CLI (`drypkg`) hem grafik mağaza (`DRY Store`) aynı arka uç motorunu kullanır. Temel akış **"Uygulamayı bul -> Kur -> Çalıştır"** sadeliğindedir; gereken çalışma zamanı ve uyumluluk katmanı otomatik çözümlenir.
  2. **Üç Uygulama Sınıfı ve Kesin Öncelik Sırası:**
     - **1. Native DRY Uygulamaları:** Doğrudan DRY SDK, `Syscall ABI` ve `ADR-007` capability sözleşmesiyle çalışan birincil hedef.
     - **2. Linux ELF / Linux ABI Uygulamaları (`P3` Birincil Uyumluluk Önceliği):** İzole userspace Linux personality sunucusu, `FaultEndpoint`/`SYSCALL` yönlendirmesi ve VFS/POSIX köprüsü üzerinde çalışır.
     - **3. Seçilmiş Windows PE / Win32 Uygulamaları (`P3+` İkincil / Uzun Vadeli Hedef):** Linux uyumluluğu olgunlaştıktan sonra seçilmiş kullanıcı alanı Win32/PE iş yükleri için devreye alınır.
  3. **Güvenlik, İsteğe Bağlı Runtime ve Şeffaflık Kuralları:**
     - İmzalı paket doğrulaması, `ADR-007` capability/sandbox izin bildirimi, atomik nesil güncellemesi ve tek-adım *rollback* zorunludur.
     - İhtiyaç duyulmayan hiçbir yabancı runtime (Linux ABI veya Win32 katmanı) önceden yüklenmez; yalnızca ilgili türde ilk paket kurulduğunda isteğe bağlı (*on-demand*) indirilir ve salt-okunur (`READ_ONLY_EXPORT`) paylaşılır.
     - **Dürüst Uyumluluk Sınırı:** Linux veya Windows uygulamalarının tamamının çalışacağı **asla vaat edilmez** (ör. çekirdek modu anti-cheat veya belgesiz çekirdek sürücüsü isteyen yazılımlar kapsam dışıdır). Her paketin uyumluluk sınıfı, beklenen başarım düzeyi ve bilinen kısıtlamaları `drypkg` ve `DRY Store` arayüzünde açıkça görünür tutulur.
  4. **Uygulama Öncesi Tasarım Kapısı:** Paket arşiv formatı, imza güven zinciri, servis bağımlılık şeması ve uyumluluk ABI sözleşmeleri kodlamaya geçilmeden önce ayrı bir teknik spesifikasyon olarak yazılıp denetlenecektir.

---

## ADR-012: Yetenek-Kapılı Harici Kesmeler (`IrqCap` One-Shot + `IoApicHwCache`), Dar `IoPortCap` (`IOPL=0`) ve Ring 3 Etkileşim Mimarisi

- **Durum:** Kabul Edildi ve `Milestone 2A-1` (`P1 First Interaction`) ile Uygulandı (`65/65` QEMU/KVM, 12 Kasım 2026)
- **Bağlam:** Kullanıcıdan gerçek klavye/cihaz girdisi alıp grafik ekranı dinamik güncellemek için harici donanım kesmelerine (`IOAPIC`/`ISA IRQ1`), cihaz yazmaç erişimine (`i8042` `0x60`/`0x64`), etkileşimli bir komut kabuğuna ve dinamik çerçeve tamponu çizimine ihtiyaç vardır. Ancak `RFLAGS.IOPL = 3` açmak, geniş I/O izin bit-haritası vermek, çekirdek kesme işleyicisinde protokol çözümlemek veya her kullanıcı sürecine doğrudan GOP `Framebuffer` eşlemek mikroçekirdek izolasyonunu bozar.
- **Karar:**
  1. **Gerçek ACPI MADT Keşfi ve Tek-Atımlık (`IRQF_ONESHOT`) `IrqCap` (`CapObjectKind::IrqLine = 11`):**
     - `IOAPIC` tabanı ve ISA `Interrupt Source Override` (`IRQ -> GSI`, polarite, tetikleme) kayıtları uydurulmaz; Limine RSDP -> XSDT/RSDT -> `MADT` üzerinden doğrulanarak keşfedilir (`acpi.rs`).
     - Harici vektörler (`0x30..=0x3F`) tetiklendiğinde çekirdek üst-yarısı (`handle_external_irq`) hiçbir protokol baytı okumaz ve bloklanan IPC yapmaz: hattı `IOAPIC` üzerinde anında maskeler (`masked_pending_ack = true`), `LAPIC EOI` yazar, kesme fırtınası eşiğini (`>= 256 IRQ / ~20 ms`) kontrol eder ve bağlı `Notification` nesnesine `notif_signal` gönderir.
     - Sürücü `SYS_IRQ_ACK (21)` çağırana dek hat maskeli kalır; ikinci ardışık `SYS_IRQ_ACK` `-114` (`EALREADY`) ile reddedilir (`0` çift-EOI, `0` kayıp uyandırma). `CSpace::revoke` veya sürücü çöküşünde hat atomik olarak maskelenip bağ çözülür.
  2. **Sıcak Kesme Yolunda `IoApicHwCache` (Sayfa Tablosu Yürüyüşü Elemesi):**
     - `IOAPIC` MMIO sayfası bir kez `KERNEL_VSPACE` içine `UC + NX` eşlendikten sonra `IOREGSEL`/`IOWIN` sanal adresleri ve her `GSI` girişinin yüksek/maskeli/açık 32-bit yönlendirme kelimeleri `IoApicHwCache` içinde önbelleklenir (`irq_top p50: 30.292 -> 13.014 cyc` [`-%57.0`], `irq_ack p50: 24.867 -> 7.788 cyc` [`-%68.7`]).
  3. **`RFLAGS.IOPL == 0` ve Ayrık Beyaz Listeli `IoPortCap` (`CapObjectKind::IoPort = 12`):**
     - Ring 3 süreçlerine hiçbir koşulda `IOPL > 0` veya genel port erişimi verilmez. `SYS_IOPORT_RW (22)` yalnızca `SecDomain::RootSystem` sürücüsünün elindeki `(base_port, port_count)` capability aralığını doğrular; PIC, PIT, PCI Config (`0xCF8..0xCFF`), CMOS ve QEMU çıkış portları çekirdekte sert kara listededir.
  4. **Üç Katmanlı Ring 3 Etkileşim Ayrımı (`dry-input` -> `dry-shell` -> `dry-display`):**
     - **`dry-input` (`RootSystem`):** Donanım arka ucu (`ps2.rs`) ile cihazdan bağımsız `InputEvent` kuyruğunu (`input.rs`, 64-olay taşma sayacı) ve tuş haritasını (`keymap.rs`: `UsQwerty` aktif, `TurkishQ` açık genişletme arayüzü) ayırır.
     - **`dry-shell` (`Userspace`, kompartman `0x0102`):** Sıfır donanım ve sıfır `Framebuffer` yetkisine sahiptir. `ADR-007` uyarınca ortak IPC uç noktaları `Userspace` kompartman etiketiyle oluşturulur (`RootSystem` sunucuları `RECV | REPLY` yapabilirken `Userspace` istemcisi `RootSystem` nesnelerine yazamaz).
     - **`dry-display` (`RootSystem`):** `dry-shell`'in `SharedTerminalSurface` (`Frame4K`) sayfasını salt-okunur (`USER_RO`) eşler, `Order2MiB` (`WB`) RAM arka tamponunda birleştirir ve yalnızca `DirtyRegion` dikdörtgenlerini GOP MMIO'ya 64-bit yazımlarla aktarır.
- **Test Edilebilir Güvenlik Invariant'ları (`INV-IRQ-01` .. `INV-IRQ-05`):**
  1. **`INV-IRQ-01 (AlwaysIoplZero)`:** Tüm Ring 3 `TrapFrame` ve `SyscallFrame` dönüşlerinde `RFLAGS.IOPL == 0` zorunludur.
  2. **`INV-IRQ-02 (DiscretePortAllowlist)`:** Beyaz liste dışı veya kara listedeki hiçbir x86 I/O portuna capability üretilemez ve erişilemez.
  3. **`INV-IRQ-03 (SingleOwnerOneShotIrq)`:** Her `GSI` aynı anda en fazla bir canlı `IrqCap` sahibine bağlanabilir; kesme anında maskelenir ve çift `SYS_IRQ_ACK` reddedilir.
  4. **`INV-IRQ-04 (RevokeMasksHardwareLine)`:** `IrqCap` iptal edildiğinde veya sahip süreç sonlandığında donanım hattı maskelenir ve eşzamanlı kesmeler panik/UAF üretmeden düşürülür.
  5. **`INV-IRQ-05 (NoFramebufferInShell)`:** `SecDomain::Userspace` içindeki kabuk/uygulama süreçleri doğrudan `CapObjectKind::Framebuffer` yetkisi alamaz (`Err(TransferEscalation)`).

---

## ADR-013: Birleşik `i8042` Denetleyici Sahipliği (`IRQ1` + `IRQ12`), Sıfır-UB Atomik Yüzey Anlık Görüntüsü ve Ring 3 Çok-Pencereli Compositor Mimarisi

- **Durum:** Kabul Edildi ve `Milestone 2A-2` (`DRY Desktop Preview`) ile Uygulandı (`79/79` QEMU/KVM, 12 Kasım 2026)
- **Bağlam:** PS/2 klavye (`IRQ1`) ve PS/2 fare (`IRQ12`) aynı `i8042` denetleyici portlarını (`0x60` veri, `0x64` durum/komut) paylaşır. İki ayrı Ring 3 sürücüsünün bu portlara eşzamanlı erişmesi `CCB` yazmalarını, `0xD4` yardımcı cihaz komut dizilerini ve 3-baytlık fare paket senkronizasyonunu bozar. Ayrıca birden fazla `SecDomain::Userspace` penceresinin (`dry-shell`, `dry-sysmon`) paylaşımlı bellek yüzeylerini `dry-display` ile paylaşırken ham `&mut SharedTerminalSurface` referansı oluşturması Rust'ta eşzamanlı veri yarışı / Tanımsız Davranış (*Undefined Behavior*) doğurur; `1280x800x32` (`4.096.000 B`) GOP çerçeve tamponunu tek `2 MiB` arka tamponla sürmek ise sınır dışı (*OOB*) risk yaratır.
- **Karar:**
  1. **Tek `i8042` Denetleyici Sahibi (`dry-input` İçinde Birleşik `IRQ1` + `IRQ12`):**
     - `0x60`/`0x64` `IoPortCap` yetkileri ve hem `IRQ1` (`vec = 0x31`) hem `IRQ12` (`vec = 0x3c`) `IrqCap` yetkileri yalnızca tek `dry-input` (`RootSystem` `0x0003`) sürecine verilir; çekirdek ikinci bir `IRQ12` bağlamasını `CapError::BusyActiveObject` ile reddeder.
     - `dry-input` tek bir `Notification` üzerinde `BADGE_IRQ1_KBD (1 << 0)` ve `BADGE_IRQ12_MOUSE (1 << 1)` bitlerini bekler; her uyanışta `status & 0x01 (OBF)` döngüsünde **`status & 0x20 (AUX_OBF)`** bitine göre baytı klavye (`Ps2Decoder`) veya fare (`Ps2MouseDecoder`, `bit 3 == 1` hizalama + `X/Y overflow` elemesi) çözücüsüne yönlendirir ve yalnızca tetiklenen hatlar için `SYS_IRQ_ACK(IRQ_ACK_UNMASK)` çağırır.
     - Cihazdan bağımsız `InputEvent` şeması sürüm uyumlu genişletilmiştir (`MouseMove = 3`, `MouseButtonDown = 4`, `MouseButtonUp = 5`, `MouseScroll = 6`, `DEV_ID_PS2_MOUSE = 2`, 48-bit `timestamp_tsc` ve kuyrukta ardışık göreli hareket birleştirme `push_coalescing`).
  2. **Açık `4 MiB` (`2 x Order2MiB`) Arka Tampon Denetimi ve Kirli-Dikdörtgen Yazılımsal İmleç:**
     - `1280 x 800 x 4 B = 4.096.000 B` (`1.000 x Frame4K`) doğrusal GOP alanı için `dry-display` önyüklemede `required_2m = ceil(mapped_bytes / 2 MiB) = 2` adet bitişik `Frame2M` (`4.194.304 B >= 4.096.000 B`) ayırır, sınırları doğrular ve çıkışta `VSPACE_MAP_OP_UNMAP_2M (5)` ile sıfır sızıntıyla (`leaked_frames = 0`) çözer.
     - `WB` RAM arka tamponu **imleçsiz temiz sahneyi** tutar; `12x16` yazılımsal imleç yalnızca GOP MMIO tarama satırı kopyası sırasında anlık bindirilir. Böylece fare hareketi yalnızca eski ve yeni `12x16` dikdörtgenleri (`DirtyRegion` içinde ~`92` değişen piksel, `p50 = ~8.8k..10.8k cyc` [`~3 µs`]) günceller; iz bırakmaz ve tam ekran yeniden çizimi yapmaz.
  3. **Sıfır-UB Atomik `SharedWindowSurface` ve Özel `SurfaceSnapshot`:**
     - Paylaşımlı `Frame4K` (`3.904 B`) üzerinde hiçbir süreç `&SharedWindowSurface` veya `&mut SharedWindowSurface` oluşturmaz; üretici (`USER_RW`) `AtomicU64`/`AtomicU32`/`AtomicU16` (`Release` tek/çift `seq` çiti) ile yazar, `dry-display` (`USER_RO`) ise `Acquire` yüklemelerle yerel `SurfaceSnapshot` kopyası çıkarır (en fazla 2 iyimser deneme, kilitlenme yok).
  4. **Tamamen Ring 3 Çok-Pencereli Yöneticisi (`wm.rs`, `0` Çekirdek UI Satırı):**
     - Pencere kimliği, sahip rozeti (`owner_badge`), Z-sırası, odak, başlık çubuğu sürükleme, `[x]` kapatma, `Alt+Tab` ve `Floating <-> SideBySideTiled` (`Alt+T` / üst çubuk düğmesi) tamamen `dry-display` içinde çalışır.
     - `dry-shell` (`0x0102`) ve `dry-sysmon` (`0x0103`) ayrı `Userspace` `VSpace` + `CSpace` süreçleridir; `Endpoint` rozetleri çekirdek tarafından damgalandığından çapraz pencere `SURFACE_PRESENT`, girdi okuma, kapatma ve sahte olay enjeksiyonu `SysError::Perm` ile reddedilir.
- **Test Edilebilir Güvenlik Invariant'ları (`INV-WM-01` .. `INV-WM-06`):**
  1. **`INV-WM-01 (SingleI8042ControllerOwner)`:** `0x60`/`0x64` portları ve `IRQ1`/`IRQ12` hatları aynı anda yalnızca tek `RootSystem` girdi servisine bağlanabilir.
  2. **`INV-WM-02 (ZeroKernelWindowPolicy)`:** `dry-kernel` içinde pencere, imleç, Z-sırası, başlık çubuğu veya yazıtipi kodu bulunmaz (`0` satır).
  3. **`INV-WM-03 (BadgeGatedSurfaceAndFocusInput)`:** Pencere sahibi yalnızca kendi rozetine bağlı yüzeyi güncelleyebilir ve yalnızca odaktaki pencere klavye olayı alabilir.
  4. **`INV-WM-04 (AtomicSharedSurfaceNoAliasing)`:** Paylaşımlı yüzey belleğine yalnızca atomik ilkellerle erişilir; çapraz süreç `USER_RO -> USER_RW` yükseltmesi `CapError::InsufficientRights` ile reddedilir.
  5. **`INV-WM-05 (AuditedBackingBufferBounds)`:** Arka tampon kapasitesi her zaman doğrulanmış `mapped_bytes` değerini tam kapsar; kanonik taşma ve hizasız eşlemeler çekirdekte reddedilir.
  6. **`INV-WM-06 (AppCrashContainment)`:** Bir uygulama penceresi (`dry-sysmon`) `#UD`/`#PF` ile çöktüğünde süpervizör ve WM kaynağı temizler; diğer pencere (`dry-shell`) ve masaüstü kesintisiz çalışmaya devam eder.



