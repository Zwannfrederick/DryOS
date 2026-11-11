# DRY OS — Teknik Risk Kaydı (Risk Register)

> **Tarih:** 10 Kasım 2026  
> **Sıralama:** Teknik şiddet ve mimari yıkıcılık derecesine göre (`P0` Kritik/Varoluşsal -> `P3` Düşük).

---

## P0 — Varoluşsal / Mimariyi Kırabilecek Riskler

### R-01: Zamanlayıcıda `switch_to` veya Zayıf Bellek Sıralaması Kaynaklı Çift-CPU Stack Ezilmesi / Kayıp Uyandırma
- **Durum:** `[TEST EDİLMEDİ — Yarın Görev 1 & 2 hedefi]`
- **Teknik Mekanizma:** (a) Yeni oluşturulan bir görevin başlangıç trampolininde `post_switch(prev)` çağrısının unutulması veya derleyicinin `prev.sched` güncellemesini `RSP` değişiminin önüne alması; (b) `EXEC_WAKING -> EXEC_READY` geçişinde `CAS` yerine düz `store` kullanılarak eşzamanlı `TOK_NOTIFIED` bitinin ezilmesi; (c) AArch64/C11 modelinde iki farklı atomik değişken (`T.sched` ve `Notification.word`) arasında açık `fence(SeqCst)` konulmaması.
- **Erken Tespit:** Negatif/pozitif TLC durum taraması ve Rust `loom` permütasyon testleri.
- **Azaltma:** `post_switch` kancasını tek bir merkezi assembly çıkış noktasına bağlamak; tüm durum geçişlerinde `park_token` koruyan `fetch_update`/`CAS` zorunluluğu.

### R-02: `TCB` Silinmesi veya Capability `Revoke` Sırasında Eşzamanlı `wakeup` (*Use-After-Free*)
- **Durum:** `[HİPOTEZ]`
- **Teknik Mekanizma:** `dry_notify_signal` içindeki bir CPU `Notification.bound_tcb` adresini okuduktan hemen sonra başka bir CPU'nun o `TCB` çerçevesini iptal edip (`Revoke`) sayfa tablosu veya kullanıcı verisine dönüştürmesi ve `LOCK CMPXCHG` buyruğunun yabancı belleği bozması.
- **Azaltma:** `EXEC_EXITING` / `EXEC_DEAD` durumları, `iommu_pin_count` kilidi ve fiziksel çerçeve yeniden tiplemesi öncesinde zorunlu **Çekirdek Epoch Bekleme Süresi (*Quiescent Grace Period*)**.

### R-03: Kalıcı IOVA `DmaArena` İçinde DMA Sahiplik İhlali (*Intra-Arena Corruption*) ve Çökme Sonrası Kaçak DMA
- **Durum:** `[HİPOTEZ — Yarın Görev 4'te sınanacak]`
- **Teknik Mekanizma:** Sıcak yolda IOTLB shootdown yapmamak için kalıcı haritalanan `DmaArena` içinde hatalı/kötücül PCIe cihazının `dma_unmap` sonrasında da tamponlara yazabilmesi; ayrıca sürücü süreci çöktüğünde cihazın aktif DMA motorunun belleği ezmeye devam etmesi.
- **Azaltma:**
  1. `IoRing` kontrol/descriptor halkalarını **asla** `DmaArena` içine koymamak (yalnızca CPU-paylaşımlı bellek).
  2. TX (`IOMMU_READ`) ve RX (`IOMMU_WRITE`) arenalarını donanım IOMMU izinleriyle ayırmak; RX başlıklarını doğrulamadan önce kopyalamak (TOCTOU engeli).
  3. Sürücü çökmesinde bellek sıfırlamadan önce zorunlu PCI `Bus Master Enable = 0` + **PCIe FLR** + IOMMU domain temizliği.

### R-04: Alt-Sistem Geçişli Bağımlılık Patlaması (*Monolithic Shim Creep*)
- **Durum:** `[DOĞRULANDI — İkili sembol analiziyle kanıtlandı]`
- **Kanıt:** `igc` (`247` sembol) ve `e1000e` (`200` sembol, `%72` ortak) dar bir ağ adasına sığarken; `r8169` harici `libphy.ko` (`444` ihraç sembol) + `clk_*` çerçevesi istemekte, `iwlwifi` ise `iwlmvm` + `mac80211` + `cfg80211` katmanlarını çekmekte, `amdgpu` ise **1.107 dış sembol ve 30.5 MiB ELF** boyutuna ulaşmaktadır.
- **Azaltma:** Ortak `LinuxKPI` katmanı yasaktır. `libdry_lx_net` için sert üst sınır `< 10.000 LOC` (hedef `<= 5.000 LOC`) olarak sabitlenmiştir; bu sınırı aşan sınıflar çevrilmez (`ADR-006`).

---

## P1 — Yüksek Şiddetli Mühendislik ve Hukuk Riskleri

### R-05: Emüle QEMU IOMMU ile Gerçek Silikon VT-d Farkından Doğan Yanlış Güven
- **Durum:** `[DOĞRULANDI]`
- **Teknik Mekanizma:** Konak makinede fiziksel `igc` kartı yoktur ve `/sys/kernel/iommu_groups` şu an `0`'dır (`linux-zen` cmdline'da `intel_iommu=on` eksik). QEMU `intel-iommu,caching-mode=on` fonksiyonel sınır hatalarını yakalar ancak gerçek donanım IOTLB invalidation gecikmesini (~2.000 döngü), PCIe *No-Snoop* önbellek tutarsızlığını ve fiziksel PHY zamanlamasını simüle etmez.
- **Azaltma:** `EXPERIMENTS.md` içinde emüle test sonuçları yalnızca "fonksiyonel IOVA doğrulaması" olarak etiketlenecek; performans ve önbellek iddiaları gerçek VT-d açık donanım testine kadar `[TEST EDİLMEDİ]` kalacaktır.

### R-06: `PREEMPT_RT` Semantik Uyumsuzlukları (`primary_handler`, `local_lock_t`, `raw_spinlock_t`)
- **Durum:** `[DOĞRULANDI]`
- **Teknik Mekanizma:** `PREEMPT_RT`'de `spin_lock_irqsave` kesme maskelemez; `request_threaded_irq` birincil işleyicisi atomiktir ve `raw_spinlock_t` uyuyamaz. Sürücü CPU-yerel kesme kapatmaya güveniyorsa userspace'de veri yarışı oluşur.
- **Azaltma:** `IRQF_ONESHOT` zorlaması, `local_lock_t`/`bh_lock` eşlemesi ve `raw_spinlock_t` / `IRQF_NO_THREAD` kullanan sürücülerin derleme zamanında otomatik reddi.

### R-07: Gölge `vmemmap` Dışından Gelen Scatter-Gather / Kontrol Pointer'ları
- **Durum:** `[HİPOTEZ]`
- **Teknik Mekanizma:** `page_address()` ve `dma_map_single()` aritmetiğine `DmaArena` dışından (`kmalloc` veya yığın üzerinden) tahsis edilmiş bir adres gelirse geçersiz bellek erişimi oluşur.
- **Azaltma:** Tüm `dry_lx_dma_map_*` girişlerinde açık adres aralığı denetimi (`[arena_base, arena_end)`); soğuk kontrol tamponları için arena içi küçük *bounce buffer* havuzu.

### R-08: `GPL-2.0-only` Sürücü Adaları ile `Apache-2.0` / `GPL-3.0` Lisans Çatışması
- **Durum:** `[DOĞRULANDI]`
- **Teknik Mekanizma:** Saf `Apache-2.0` ve `GPL-3.0` kütüphaneleri `GPL-2.0-only` Linux sürücü koduyla aynı süreçte bağlanamaz (`ADR-005`).
- **Azaltma:** Çekirdek/native kodda `MIT OR GPL-2.0-only` ikili lisansı; sürücü adalarında `cargo-deny` / SPDX otomatik derleme kapısı.

---

## P2 & P3 — Orta/Düşük Ortam ve Ölçekleme Riskleri

### R-09: Geliştirme Makinesinin 8 Mantıksal CPU Sınırı (`CPU 0..7`)
- **Durum:** `[DOĞRULANDI]`
- **Etki:** Yerel donanım 4C/8T Tiger Lake işlemcidir; 20-CPU ölçekleme ve P/E hibrit ya da çoklu NUMA davranışları yerel bare-metal'de doğrudan ölçülemez (QEMU `-smp 20` yalnızca fonksiyonel doğruluk sağlar).
- **Azaltma:** Yerel bare-metal ölçümleri 1, 2, 4, 8 thread için alınacak; 16 ve 20+ çekirdek performans kapısı (Faz 4) adanmış çok-çekirdekli test sunucusunda koşturulacaktır.

### R-10: Eksik Yerel Araç ve Kaynak Paketleri (`tla2tools.jar`, `rust-src`, `limine`, Linux `.c` kaynakları)
- **Durum:** `[DOĞRULANDI — Çözüm komutları HANDOVER.md içinde tanımlandı]`
- **Etki:** Çevrimdışı ortamda bu 4 bileşen olmadan sırasıyla Görev 1, Görev 5 ve Görev 3 bloklanır.
- **Azaltma:** Yarınki oturum başında her görev öncesi bağımlılık indirme/paket kurulum adımları açıkça ayrılmıştır.

