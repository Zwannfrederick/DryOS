# DRY OS — Teknik ve Ürün Risk Kaydı (`RISK_REGISTER.md`)

> **Son Güncelleme:** 14 Kasım 2026 (`Project Ascension 3.0`: `Milestone 2C-2` Donanım Temelleri + `Track B DRY Glass` Desktop Kapanışı)  
> **Sıralama:** Teknik şiddet ve mimari yıkıcılık derecesine göre (`P0` Kritik/Varoluşsal -> `P2` Orta).  
> **Durum Etiketleri:** `MITIGATED (QEMU/Formal)` (kodda ve testlerde koruma altında; sürekli regresyon zorunlu), `OPEN / PLANNED` (gelecek fazlarda aktif risk), `BOUNDED BY DESIGN` (mimari sınırla kapsam dışı tutuldu).

---

## P0 — Varoluşsal / Mimariyi Kırabilecek Riskler

### R-01: Zamanlayıcıda `switch_to`, Zayıf Bellek Sıralaması veya `active_mask` Kaynaklı Stack Ezilmesi / Kayıp Uyandırma
- **Durum:** `MITIGATED (QEMU/Formal)` — Sürekli regresyon koruması altında (`formal/h1/DrySched.tla`, `loom_sched`, `65/65` QEMU).
- **Teknik Mekanizma:** (a) `RSP` geçişinden önce `prev` yığınının serbest bırakılması; (b) `prepare_park` ile `Notification::signal` arasında açık `fence(Ordering::SeqCst)` unutulması; (c) `PerCpuRunQueue::pop_masked` içinde `active_mask` biti temizlenirken eşzamanlı `push` yarışında kuyruğun yetim kalması.
- **Uygulanan Savunma:** Merkezi assembly `dry_switch_to -> finish_post_switch` sınırı (`stack_owner` yalnızca yeni yığında bırakılır), zorunlu `SeqCst` çitleri ve `pop_masked` içinde Dekker yeniden kontrolü (`fetch_and` sonrası `!rings[p].is_empty()` ise `fetch_or`). **Bu bölgelere dokunan her değişiklik negatif yarış testi gerektirir.**

### R-02: Capability `Revoke` veya Süreç Çıkışı Sırasında Eşzamanlı `Lookup` / `Wakeup` (*Use-After-Free* & ABA)
- **Durum:** `MITIGATED (QEMU/Formal)` — `Milestone 1B-C`, `1B-D` ve `1C` ile doğrulandı (`uaf_errors = 0`).
- **Teknik Mekanizma:** Bir çekirdek nesnesi (`TCB`, `Endpoint`, `Frame4K`, `VSpace`) kullanılırken başka bir CPU'da `revoke` edilmesi veya `TCB` yuvasının erken geri dönüştürülüp ABA çarpışması yaratması.
- **Uygulanan Savunma:** Çift-nesil ABA kalkanı (`slot_gen` + canlı nesne `generation`), `with_pinned_cap` içindeki `slot.in_flight_ops` (`SeqCst`) Dekker pini, `CSpace`-sahibi `TCB` yuvalarının yalnızca `cspace_destroy_object -> release_dead_tcb_slot` sonrasında havuza dönmesi ve çerçeve geri kazanımı öncesi senkron SMP TLB shootdown (`INV-CAP-05`, `INV-USR-08`).

### R-03: Kalıcı IOVA `DmaArena` İçinde DMA Sahiplik İhlali ve Çökme Sonrası Kaçak DMA
- **Durum:** `MITIGATED (QEMU VT-d SLPT + Reset + BME=0 + Pin)` / `OPEN (Bare-Metal Fiziksel VT-d/AMD-Vi Silikon — P5)` (`Milestone 2B` `ADR-015` + `Milestone 2C-2` `ADR-019` ile çekirdek mekanizması doğrulandı).
- **Azaltma:** `SYS_DMA_ALLOC` çerçeveleri `DEVICE_DMA | NO_DELEGATE` ve `iommu_pin_count == 1` ile sabitlenir (`INV-CAP-08`, erken `destroy` `-EBUSY`); yalnızca yetkili DMA sayfaları `VT-d` `SLPT` bağlamına eşlenir ve yetkisiz cihaz DMA'sı `FRCD` fault kaydında yakalanır (`p8a`: `vtd_faults=1`); sürücü çöküşünde veya çıkışında `quiesce_slot_for_cspace` sırasıyla `MSI` susturma, VirtIO `device_status=0` sıfırlama, `PCI Command.BME = 0` temizleme ve `detach_and_scrub_domain` (`VT-d` SLPT/IOTLB temizliği) uyguladıktan sonra pinleri bırakır (`quiesce=3`, `leaked_frames=0`). Fiziksel silikon bare-metal VT-d/AMD-Vi doğrulaması `P5` kapısındadır.

### R-04: Alt-Sistem Geçişli Bağımlılık Patlaması (*Monolithic Shim Creep*)
- **Durum:** `BOUNDED BY DESIGN` (`ADR-006`).
- **Kanıt & Azaltma:** `igc` (`247` sembol) ve `e1000e` (`200` sembol) dar bir ağ adasına (`3.028 SLOC` runtime) sığarken `amdgpu` (`1.107` sembol, `30.5 MiB` ELF) ve `nvidia-open` (`HMM`/`mmu_notifier`) sığmamaktadır. Ortak `LinuxKPI` yasaktır; `< 10.000 LOC` sınırını aşan sınıflar çevrilmez.

### R-11: Sabit Çekirdek Yığını Bütçesi, Derleyici Satır-İçi (*Inlining*) ve `NMI/#MC` `IST` Güvenliği (Güncelleme — 14 Kasım 2026)
- **Durum:** `MITIGATED (QEMU)` — Yeni kod eklenirken aktif dikkat gerektirir!
- **Kanıt:** `Optimization Pass v1` sırasında `verify_cpu_stack_canaries_fast`, `bsp_kmain` (`5.448 B`) -> `verify_userspace_and_syscalls` içine gömülen `run_stage_g_benchmarks` (`8.192 B` tampon) + `on_process_terminating` (`1.816 B` `[WakeBatchItem; 64]`) zincirinin `16.384 B` yığın tavanını aştığını (`16.680 B`) yakaladı; ayrıca `NMI`/`#MC` kesmeleri `IST0` üzerindeydi (`AUD-K7`).
- **Azaltma:** Soğuk/test/benchmark fonksiyonları `#[inline(never)]` ile izole edilerek `boot_stack` `32 KiB`'a sertleştirildi (`Milestone 2C-2`); `#DF` (`IST1`), `NMI` (`IST2`) ve `#MC` (`IST3`) kesmeleri her CPU için kanaryalı ayrık `8 KiB` yığınlara bağlandı (`gdt_tss.rs`, `audit_tests.rs` `cases=9/9`); `rust_syscall_dispatch` (`248 B`) `<= 256 B` tavanında tutuldu. **Kural:** Çekirdekte `>= 1 KiB` yığın dizisi ayırmak yasaktır veya mutlaka `#[inline(never)]` yaprak fonksiyon / per-CPU statik kazıma alanı kullanılmalıdır.

### R-14: Harici Donanım Kesme Fırtınası (`IRQ Storm`), `Revoke` Yarışı ve Port I/O Ayrıcalık Sızıntısı (Yeni — 12 Kasım 2026)
- **Durum:** `MITIGATED (QEMU)` — `Milestone 2A-1` & `2A-2` (`ADR-012`, `ADR-013`, `23` donanım/E2E/güvenlik testiyle doğrulandı).
- **Teknik Mekanizma:** (a) Geveze veya arızalı bir cihazın kesme fırtınasıyla CPU'ları kilitlemesi; (b) `CSpace::revoke` veya sürücü çöküşü sırasında gelen kesmenin bayat `Notification` nesnesine erişmesi; (c) Ring 3 sürücüsüne `IOPL=3` veya geniş port erişimi verilmesiyle PIC/PIT/PCI Config yazmaçlarının bozulması.
- **Uygulanan Savunma:** Üst-yarı `handle_external_irq` içinde anında `IOAPIC` maskeleme (`IRQF_ONESHOT`) + `>= 256 IRQ / ~20 ms` fırtına kelepçesi (`storm_clamped=1`), `IrqSlotState` spinlock altında atomik `revoke` maskelemesi (`revoke_race_ok=true`, `lost_wakeups=0`), çift `SYS_IRQ_ACK` reddi (`-EALREADY`) ve `RFLAGS.IOPL == 0` korunarak yalnızca ayrık beyaz listedeki portlara (`0x60`/`0x64`, `0x2F8`) izin veren `IoPortCap`.

### R-15: Paylaşımlı `i8042` Klavye/Fare Yarışı, Çapraz-Süreç Paylaşımlı Bellek Rust UB ve GOP Arka Tampon Sınır Aşımı (Yeni — 12 Kasım 2026)
- **Durum:** `MITIGATED (QEMU)` — `Milestone 2A-2` (`ADR-013`, `79/79` QEMU testiyle doğrulandı).
- **Teknik Mekanizma:** (a) İki ayrı sürücünün `0x60`/`0x64` portlarını bağımsız okumasıyla klavye tarama kodları ile 3-baytlık PS/2 fare paketlerinin birbirine karışması; (b) İki farklı sürecin paylaştığı `Frame4K` üzerinde `&mut` / `&` referansı oluşturulmasının Rust bellek modelinde veri yarışı UB üretmesi; (c) `1280x800x32` (`4.096.000 B`) GOP alanı için tek `2 MiB` arka tampon varsayımının `y >= 410` satırlarında OOB bellek erişimine yol açması.
- **Uygulanan Savunma:** (a) Tek `dry-input` denetleyici sahipliği (`BusyActiveObject` ile çift `IRQ12` bağlama reddi) + `status & 0x20 (AUX_OBF)` ayrımı + `bit 3 == 1` paket senkron kilidi (`desync = 0`); (b) sıfır-`&mut` atomik `SharedWindowSurface` (`AtomicU64`/`AtomicU32`/`AtomicU16` tek/çift `seq` çiti) + compositor-özel `SurfaceSnapshot` + `USER_RO` eşleme (`InsufficientRights` ile `RW` yükseltme reddi); (c) `2 x Frame2M = 4.194.304 B` (`>= 4.096.000 B`) çalışma zamanı denetimi ve `fb_backing_oob_rejected` çekirdek sınır kapısı.

---

## P1 — Yüksek Şiddetli Mühendislik, Ürün ve Hukuk Riskleri

### R-05: QEMU/KVM Mikrobenchmarkları ve Emüle IOMMU ile Gerçek Fiziksel Donanım Farkı
- **Durum:** `OPEN / PLANNED` (`ADR-008`, `P5`).
- **Teknik Mekanizma:** QEMU/KVM içindeki `SYSCALL` (`231 cyc`), `IPC` (`1.952 cyc`) ve emüle `intel-iommu` testleri gerçek silikon önbellek/TLB/IOTLB gecikmelerini, fiziksel PCIe *No-Snoop* davranışını ve bütün-sistem G/Ç darboğazlarını temsil etmez; `4C/8T` konakta `-smp 20` koşusu `2.5x` konak CFS aşırı-aboneliğidir.
- **Azaltma:** KVM mikro-ölçütleri hiçbir belgede "Linux'tan hızlı" veya "gerçek 20 çekirdek kanıtı" olarak sunulmaz; gerçek donanım iddiaları `P5` bare-metal A/B benchmark kapısına bağlanmıştır.

### R-06 & R-07: Sürücü Adası `PREEMPT_RT` Semantiği ve `DmaArena` Dışı Scatter-Gather Pointer'ları
- **Durum:** `OPEN / PLANNED` (Faz 3 `H2` entegrasyonunda zorunlu derleme ve çalışma zamanı kapıları).
- **Azaltma:** `IRQF_ONESHOT` zorlaması, `raw_spinlock_t`/`IRQF_NO_THREAD` derleme reddi ve tüm `dry_lx_dma_map_*` girişlerinde açık `[arena_base, arena_end)` sınır doğrulaması.

### R-08: `GPL-2.0-only` Sürücü Adaları ile `Apache-2.0` / `GPL-3.0` Lisans Çatışması
- **Durum:** `BOUNDED BY DESIGN` (`ADR-005`).
- **Azaltma:** Çekirdek ve native servisler `MIT OR GPL-2.0-only`; Linux sürücü adaları ayrı süreç sınırında `GPL-2.0-only`; saf `Apache-2.0` ve `GPL-3.0` bağımlılıkları yasak.

### R-12: `DRY Glass` Görsel Efekt / Hibrit WM Karmaşıklığının Düşük Donanımı veya Mikroçekirdek Sınırını Zorlaması (Güncelleme — 14 Kasım 2026)
- **Durum:** `MITIGATED (QEMU — Ascension 3.0 Track B)` / `OPEN (Bare-Metal iGPU)` (`ADR-009`, `ADR-020`, `P2`).
- **Teknik Mekanizma:** Buzlu cam arka plan bulanıklığı, yuvarlatılmış yarı saydam kartlar ve `Floating + Tiling + Stacking/Tabbed` geçişlerinin yazılımsal GOP veya düşük uç donanımda kare süresini uzatması ya da UI mantığının çekirdeğe sızması.
- **Uygulanan Savunma:** Tüm görsel/cam/WM mantığı Ring 3 `dry-display` (`userspace/src/glass/*`, `compositor.rs`) içinde tutuldu (`0` çekirdek UI satırı); `1024x768`..`1920x1080` arası `DirtyRegion` + `expand_blur_damage` kırpma, canlı `Quick Settings` / `Settings [9]` **`low_overhead_glass` (`Solid Fallback`)** anahtarı ve donanım `VirtIO-GPU` `#UD` -> anlık `UEFI GOP` fallback (`--p9-only`) ile doğrulandı (`10` PNG kanıtı).

### R-13: `drypkg` Runtime Şişmesi ve Linux/Windows Uyumluluğunda Aşırı Vaat Riski (Yeni)
- **Durum:** `BOUNDED BY DESIGN` (`ADR-011`, `P3/P4`).
- **Teknik Mekanizma:** Yabancı uyumluluk çalışma zamanlarının varsayılan kurulumu şişirmesi (`DRY Server` saflığını bozması) veya kararsız paket formatı/gerçekçi olmayan "%100 Windows/Linux uyumu" beklentisi.
- **Azaltma:** Kodlama öncesi zorunlu paket/ABI tasarım kapısı, isteğe bağlı (*on-demand*) salt-okunur (`READ_ONLY_EXPORT`) runtime kurulumu, kesin öncelik sırası (`Native DRY -> Linux ELF -> Seçilmiş Win32`) ve `DRY Store` üzerinde görünür uyumluluk/kısıtlama rozetleri.

### R-16: Oturum Kilidi Doğrulaması (`DRY Vault`), Kalıcı `/persist` vs Uçucu `/ram` Ayrımı ve UEFI `ACPI S3` Uyanma Sınırı (Güncelleme — 12 Kasım 2026)
- **Durum:** `DRY Vault` & Kalıcı `/persist` `MITIGATED (QEMU — Milestone 2B)` / `ACPI S3` `OPEN — P5` (`ADR-014`, `ADR-015`).
- **Teknik Mekanizma:** (a) Oturum kilidinin düz metin veya sahte parola kontrolüyle aşılması; (b) `/persist` üzerinde güç kesintisinde veri bozulması veya `/ram` uçucu alanının kalıcı sanılması; (c) `< 1 MiB` 16-bit gerçek-mod uyanma trampolini yokken `Suspend-to-RAM (S3)` işleminin sahte ekran karartmasıyla taklit edilmesi.
- **Uygulanan Savunma:** (a) `Milestone 2B` ile `Session Lock` kalkanı `dry-vault.elf` (`0x0007`) tuzlu `PBKDF2-HMAC-SHA256` doğrulayıcısına (`/persist/vault.sec` `RootSystem | NO_DELEGATE`, `ct_eq_32`, `0` düz metin) ve $O(1)$ `GLOBAL_LEASE_EPOCH` ilerletmesine bağlandı (`docs/p4_vault_auth_lock.png`); (b) `/persist` çift A/B `CRC32` süperblok + 4 KiB CoW + `VIRTIO_BLK_T_FLUSH` ile güç kesintisi (`SIGKILL`) altında doğrulandı (`p4b`), `/ram` ise açıkça uçucu etiketlendi; (c) `POWER_OP_SUSPEND_S3` sahte uyku yapmaz ve deterministik `-ENOSYS` döner (`INV-APP-06`).

---

## P2 — Ortam ve Araç Zinciri Riskleri

### R-09: Yerel Geliştirme Makinesinin `4C/8T` (`8` Donanım İzleği) Sınırı
- **Durum:** `OPEN / PLANNED` (`P5` çok-çekirdekli fiziksel sunucu testine kadar geçerli).
- **Azaltma:** Yerel ortamda 1, 2, 4, 8 vCPU gerçek paralelliği temsil eder; `-smp 20` yalnızca eşzamanlılık doğruluğu ve yarış avı içindir.

### R-10: Çevrimdışı Araç Zinciri ve Önyükleyici Tekrarlanabilirliği
- **Durum:** `MITIGATED` (`tools/bootstrap_phase1.py` + `tools/phase1-toolchain-lock.json` ile SHA-256 kilitli `.toolchains/` kurulumu tamamlandı).

### R-17: Hibrit Dizüstü (Optimus) GPU IOMMU Grup Paylaşımı, `RTD3` Pil Tüketimi ve VFIO Bellek Sabitleme Maliyeti (Yeni — 12 Kasım 2026)
- **Durum:** `BOUNDED BY DESIGN` / `OPEN — P5 Bare-Metal` (`ADR-016`, `docs/COMPATIBILITY_ISLANDS_RESEARCH.md` Bölüm 8–9).
- **Teknik Mekanizma:** (a) Uyuyan bir GPU MicroVM'in vCPU'ları `HLT` durumunda `%1.3` CPU tüketse bile VFIO/IOMMU DMA eşlemesinin misafir RAM'ini kalıcı sabitlemesi (`Exp-B2`: `256 MiB` misafir için `309.41 MiB` `VmRSS`); (b) TigerLake-H dizüstünde `01:00.0` RTX 3050 Ti Mobile cihazının muxless (`0302`) olması, IOMMU Grup 13'ü `00:01.0` PCIe Kök Portuyla paylaşması ve `D0` boşta bırakıldığında `~8–15 W` pil tüketmesi.
- **Azaltma:** `Kademe 4` GPU MicroVM varsayılan açılışta kesinlikle çalıştırılmaz (*opt-in / on-demand*); birincil ekran taraması her zaman Intel `00:02.0` üzerinde yerel `dry-display` mülkiyetinde kalır; GPU kiralaması sona erdiğinde MicroVM kapatılıp cihaz `FLR` + ACPI `RTD3 D3cold` durumuna indirilir (`P5` fiziksel silikon kabul kapısı).

### R-18: Önyükleme Modül Tablosu Tavanı (`MAX_LATCHED_MODULES = 16/16`) ve `Mock` vs Gerçek Linux Sürücü Adası Sınırları (Yeni — 12 Kasım 2026)
- **Durum:** `OPEN / PLANNED` (`Milestone 2A-4a` #1 Önkoşulu -> `Milestone 2C-1` Gerçek Linux Island PoC).
- **Teknik Mekanizma:** (a) `Milestone 2C-0` ile eklenen `dry-broker.elf` (`15`) ve `dry-mock-drv.elf` (`16`) sonrasında Limine önyükleme modül dizisi `16/16` dolu kapasiteye ulaşmıştır (`kernel/src/limine.rs:429` `MAX_LATCHED_MODULES = 16`, `kernel/src/irq/session.rs:202` `[CAP_NULL; 16]`, `userspace/src/vfs_mem.rs:14` `MAX_BOOT_ENTRIES = 20`); tavan artırılmadan 17. ELF64 modülü eklenirse modül önyüklemede sessizce kırpılır; (b) `dry-mock-drv.elf` kontrol/veri düzlemi (`unauth_denied=4`, `stale_revoked=5`) kanıtı olup saf CPU dönüşümü yapar (`0` `libdry_lx_*`, `0` `PciDev`/DMA/MSI-X/FLR) ve `IoRing` kontrol kapı zilinde hatalı istemciye karşı `50 ms` bekçi zaman aşımı kullanır.
- **Azaltma:** Kesinleştirilen sırayla önce **`Milestone 2A-4a`** içinde `MAX_LATCHED_MODULES` (`16 -> 32`), `MAX_BOOT_ENTRIES` (`20 -> 36`) + önyükleme taşma assertion'ı, `XSAVEOPT` FPU/SIMD izolasyonu ve `FaultEndpoint` + `TCB`-bağlı birleşik `Endpoint + Notification` beklemesi tamamlanacak; ardından **`Milestone 2C-1`** içinde en küçük gerçek Linux sürücü bileşeni tamamen Ring 3 `libdry_lx_*` üzerinde (`0` çekirdek Linux ABI kodu) devreye alınacaktır.
