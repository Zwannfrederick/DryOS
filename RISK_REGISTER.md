# DRY OS — Teknik ve Ürün Risk Kaydı (`RISK_REGISTER.md`)

> **Son Güncelleme:** 12 Kasım 2026 (`Milestone 2A-3` / `Functional Desktop Foundation` Kapanışı)  
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
- **Durum:** `MITIGATED (QEMU BME=0 + Pin)` / `OPEN (Bare-Metal Fiziksel VT-d/AMD-Vi Silikon — P5)` (`Milestone 2B` `ADR-015` ile çekirdek mekanizması doğrulandı).
- **Azaltma:** `SYS_DMA_ALLOC` çerçeveleri `DEVICE_DMA | NO_DELEGATE` ve `iommu_pin_count == 1` ile sabitlenir (`INV-CAP-08`, erken `destroy` `-EBUSY`); `IoRing` kontrol sayfaları normal çerçevelerde tutulur; sürücü çöküşünde (`#UD` canlı testi: `p4c_vault_dma_devws_security_e2e`) veya çıkışında `quiesce_pci_for_cspace` bellek iadesinden **önce** donanımda `PCI Command.BME = 0` bitini temizler (`pci_quiesce=2`, `leaked_frames=0`). Fiziksel PCIe FLR ve silikon VT-d/AMD-Vi IOTLB temizliği `P5` bare-metal kapısındadır.

### R-04: Alt-Sistem Geçişli Bağımlılık Patlaması (*Monolithic Shim Creep*)
- **Durum:** `BOUNDED BY DESIGN` (`ADR-006`).
- **Kanıt & Azaltma:** `igc` (`247` sembol) ve `e1000e` (`200` sembol) dar bir ağ adasına (`3.028 SLOC` runtime) sığarken `amdgpu` (`1.107` sembol, `30.5 MiB` ELF) ve `nvidia-open` (`HMM`/`mmu_notifier`) sığmamaktadır. Ortak `LinuxKPI` yasaktır; `< 10.000 LOC` sınırını aşan sınıflar çevrilmez.

### R-11: `16 KiB` Sabit Çekirdek Yığını Bütçesi ve Derleyici Satır-İçi (*Inlining*) Yığın Taşması (Yeni — 11 Kasım 2026)
- **Durum:** `MITIGATED (QEMU)` — Yeni kod eklenirken aktif dikkat gerektirir!
- **Kanıt:** `Optimization Pass v1` sırasında `verify_cpu_stack_canaries_fast`, `bsp_kmain` (`5.448 B`) -> `verify_userspace_and_syscalls` içine gömülen `run_stage_g_benchmarks` (`8.192 B` tampon) + `on_process_terminating` (`1.816 B` `[WakeBatchItem; 64]`) zincirinin `16.384 B` yığın tavanını aştığını (`16.680 B`) yakaladı.
- **Azaltma:** Soğuk/test/benchmark fonksiyonları `#[inline(never)]` ile izole edilerek `CPU 0` yığınında `+7.704 B` boşluk açıldı (`proc/bench.rs`, `proc/tests.rs`); `Milestone 2A-1` (`P1`) çağrıları da ayrı `syscall/cold.rs` (`216 B` çerçeve) içine alınarak sıcak `rust_syscall_dispatch` (`248 B`) hiç büyütülmedi. **Kural:** Çekirdekte `>= 1 KiB` yığın dizisi ayırmak yasaktır veya mutlaka `#[inline(never)]` yaprak fonksiyon / per-CPU statik kazıma alanı kullanılmalıdır; Faz 2'de çekirdek yığınları koruma sayfalı (*guard-paged*) sanal aralıklara taşınacaktır.

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

### R-12: `DRY Desktop` Görsel Efekt / Hibrit WM Karmaşıklığının Düşük Donanımı veya Mikroçekirdek Sınırını Zorlaması (Yeni)
- **Durum:** `OPEN / PLANNED` (`ADR-009`, `P2`).
- **Teknik Mekanizma:** `liquid-glass` üst çubuk, bulanıklık/gölge efektleri ve `Floating + Tiling + Stacking/Tabbed` pencere geçişlerinin yazılımsal GOP veya giriş seviyesi iGPU'larda kare hızı düşüşü (*frame drop*) yaratması ya da UI mantığının çekirdeğe sızdırılması.
- **Azaltma:** Tüm görsel/WM mantığı Ring 3 compositor içinde tutulur (`0` çekirdek UI kodu); `Milestone 2A-1` ile `Order2MiB` (`WB`) RAM arka tamponu + `DirtyRegion` 64-bit GOP aktarımı devreye alınarak tek-satır tuş vuruşunun yalnızca `~15.3k cyc` (`~5 µs`) sürdüğü doğrulandı; `P2`'de bulanıklık/animasyon maliyetini donanım yeteneğine göre otomatik kapatan *Low-End Fallback* katmanı ve `Floating` diyalogların `Tiling` ağacını bozmamasını garanti eden ayrık sahne grafı zorunludur (`docs/DRY_DESKTOP_SPEC.md`).

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
