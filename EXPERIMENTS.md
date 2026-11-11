# DRY OS — Faz 0 Doğrulama ve Fizibilite Deney Planı (`H1` & `H2`)

> **Tarih:** 10 Kasım 2026  
> **Kural:** Önceki `H1 v2` ve `H2` tasarımları **henüz doğrulanmış kabul edilmez**. Her deney önce kendi negatif testini (bilinen hatalı varyantı yakalama) kanıtlamak zorundadır.

---

## Bölüm A: `H1` — Zamanlayıcı Durum Makinesi ve Bellek Sıralaması Doğrulaması

### A.1. Kod Öncesi İncelenmesi Zorunlu 10 Kritik Yarış ve Açık Nokta

1. **`EXEC_READY` Geçişi ile Runqueue Yayınlama (`Publication`) Yarışı `[TEST EDİLMEDİ]`:**
   - **Risk:** Bir uyandırıcı `rq.push(T)` işlemini `exec_state = EXEC_READY` yapmadan *önce* gerçekleştirirse, başka bir CPU görevi kuyruktan çeker ancak `exec_state != EXEC_READY` görerek görevi düşürür veya kilitlenir.
   - **Zorunlu Kural:** Her kuyruklayıcı (`post_switch` ve `WAKING` kazananı) önce `Release` ile `EXEC_READY` yazmalı, **ardından** `Release` ile `rq.push(T)` yapmalıdır; okuyucu `Acquire` ile çekmelidir.
2. **`EXEC_WAKING` Sırasında Eşzamanlı İkinci `wakeup` `[KRİTİK AÇIK — TEST EDİLECEK]`:**
   - **Risk:** Görev `BLOCKED` iken `Waker 2` (ör. timeout) `CAS(BLOCKED -> WAKING)` kazanır. Henüz `READY` yapmadan `Waker 1` (gerçek olay) `wakeup(T)` çağırıp jetonu `(EXEC_WAKING, TOK_NOTIFIED)` yapar. Eğer `Waker 2` `READY`'ye geçerken `CAS` döngüsü yerine düz `store((EXEC_READY, TOK_NONE))` kullanırsa **`TOK_NOTIFIED` silinir**!
   - **Doğrulama:** Hem TLA+ hem Loom modelinde `WAKING -> READY` geçişi zorunlu olarak `CAS((WAKING, tok) -> (READY, tok))` olacak; düz `store` negatif test olarak koşturulup kayıp uyandırma ürettiği kanıtlanacaktır.
3. **Preemption ve CPU Göçü (`Migration`):**
   - `prepare_park()` sonrası gelen zamanlayıcı kesmesi `EXEC_RUNNING -> EXEC_SW_PREEMPT` geçişinde `park_token` bitlerine dokunmamalıdır; görev başka CPU'ya göç edip `DispatchRun` ile `EXEC_RUNNING` olurken de `CAS((READY, tok) -> (RUNNING, tok))` ile jeton korunmalıdır.
4. **Context Switch Tamamlanmadan Stack Kullanımı (`Assembly` Sınırı):**
   - `switch_to(prev, next)` çağrısında kontrol akışı yeni oluşturulmuş bir görevin ilk başlangıç fonksiyonuna (`thread_entry_trampoline`) sıçradığında da **ilk iş olarak `post_switch(prev)` çağrılmak zorundadır**; aksi halde `prev` sonsuza dek `EXEC_SW_PARK`/`EXEC_SW_PREEMPT` içinde asılı kalır. Derleyici yeniden sıralamasını önlemek için `RSP` değişimi ile `post_switch` arasına `compiler_fence(SeqCst)` konur.
5. **Timeout, Cancellation ve Spurious Wakeup:**
   - Zaman aşımıyla uyanan görev, `TCB.timer_node` düğümünü zamanlayıcı yığınından güvenli biçimde sökmelidir. `commit_park()` içindeki hızlı yol (`TOK_NOTIFIED` tüketimi) sahte uyandırmaya karşı çağıranı her zaman koşul döngüsünün başına (`while !cond`) döndürmelidir.
6. **Atomik Bellek Sıralaması — x86-64 TSO vs. AArch64 Zayıf Bellek `[KRİTİK]`:**
   - `[DOĞRULANDI: C11/Rust Bellek Modeli]` `T.sched` üzerindeki bir `SeqCst` RMW işlemi, tek başına **farklı bir atomik değişken olan** `cond` / `Notification.word` üzerindeki `Acquire`/`Release` erişimleriyle tam sıralama (`StoreLoad` engeli) **kurmaz**! x86-64'te `LOCK CMPXCHG` donanımsal tam bariyer olduğu için bu hatayı gizler; AArch64 ve Rust `loom` ise yakalar.
   - **Zorunlu Kural:** `prepare_park` ile `cond` okuması arasına ve `cond` yazması ile `wakeup` arasına açık `atomic::fence(Ordering::SeqCst)` (`dmb ish`) konacaktır.
7. **`TCB` Yaşam Döngüsü, Görev Sonlanma (`Termination`) ve Capability `Revoke`:**
   - `Notification.bound_tcb` üzerinden `wakeup(T)` çağrılırken `TCB` belleğinin eşzamanlı `Revoke` ile silinip başka nesneye dönüştürülmesi *Use-After-Free* yaratır.
   - **Kural:** `exec_state`'e `EXEC_EXITING` ve `EXEC_DEAD` eklenir; `TCB` fiziksel çerçevesi tüm aktif CPU'lar en az 1 çekirdek *quiescent epoch* geçirmeden yeniden tiplenemez.
8. **Model Soyutlaması vs. Gerçek Runqueue:**
   - TLA+ modelindeki soyut sayaç yerine gerçek kodda sabit boyutlu taşma riski olmayan intrüzif `TCB.rq_next` listesi + CPU-yerel `rq_lock` (Faz 1) kullanılacak; Loom testinde gerçek kuyruk veri yapısı modellenecektir.
9. **Safety ve Liveness Özelliklerinin Ayrı Doğrulanması:**
   - *Safety* (`ExclusiveOwnership`, `StackSafeOnSteal`, `RunqueueUniqueness`, `NoWakeAfterDead`) hiçbir adillik varsayımı olmadan (`Init /\ [][Next]_vars`) doğrulanır.
   - *Liveness* (`NoLostWakeup`) ayrı bir TLC yapılandırmasıyla test edilir.
10. **Fairness Varsayımlarının Gerçekçiliği:**
    - TLA+ modelinde `CAS` yarışlarına *Strong Fairness (`SF`)* **verilmeyecektir** (gerçek donanım `CAS` açlığını engellemez). Yalnızca CPU ilerlemesine *Weak Fairness (`WF`)* verilecek; sonsuz `preempt` veya sahte `wakeup` döngüsünün liveness'ı bozup bozmadığı açıkça raporlanacaktır.

### A.2. `H1` Test Araçları, Komutları ve Kabul Kriterleri (11 Kasım 2026)

- **Ortam Notu `[DOĞRULANDI]`:** Konak makinede `/usr/bin/java` ve `cargo 1.99.1` kurulu; ancak `tla2tools.jar` henüz diskte yok (Yarın Görev 1 başında indirilecek). Konak işlemci 4 fiziksel çekirdek / 8 mantıksal thread (`CPU 0..7`) barındırıyor; yerel stres testi 8 donanım thread'i ve 20 aşırı-abone (*oversubscribed*) thread ile koşturulacak, gerçek 20 fiziksel/SMT çekirdek performans testi Faz 4 donanım kapısına yazılacaktır.
- **Çalıştırma Komutları:**
  - Negatif TLC testi (hatalı varyantlar — **ihlal bulmak zorunda**): `java -XX:+UseParallelGC -jar tools/tla2tools.jar -config formal/h1/DrySchedBuggy.cfg formal/h1/DrySched.tla`
  - Pozitif TLC Safety & Liveness testi: `java -XX:+UseParallelGC -jar tools/tla2tools.jar -workers auto -config formal/h1/DrySched.cfg formal/h1/DrySched.tla`
  - Rust `loom` zayıf bellek permütasyon testi: `RUSTFLAGS="--cfg loom" cargo test --manifest-path experiments/h1-sched/Cargo.toml --release`
  - Native çok-çekirdek stres testi (60 sn hızlı kapı + uzun koşu): `cargo test --manifest-path experiments/h1-sched/Cargo.toml --release -- --ignored stress_20_threads`
- **Başarı / Başarısızlık Kriterleri:**
  - **Kabul:** Negatif testler beklenen 4 karşı-örneği üretir; pozitif TLC ve Loom testleri **0 ihlal** ile biter; native stres testinde >10 milyon uyuma/uyanma/göç turunda 0 kayıp uyandırma, 0 stack kanarya bozulması.
  - **Red (Falsification):** Pozitif modelde tek bir durum ihlali veya gerçek kuyruk bağlantısında kilitlenme çıkması.

---

## Bölüm B: `H2` — Linux Driver Island (`igc`) Fizibilite Hazırlığı

### B.1. Tekrar Üretilebilir Sembol ve Sürüm Kayması Bulguları `[DOĞRULANDI]`

10 Kasım 2026 tarihinde bu makinedeki kurulu çekirdek modülleri üzerinde doğrudan ölçülen değerler:

- **Kullanılan Çekirdek Paketleri:** `6.18.60-1-lts` ve `7.2.14-arch1-1` (`/lib/modules/` altında mevcut).
- **Tekrar Üretim Komutu:**
  ```bash
  zstd -dc /lib/modules/6.18.60-1-lts/kernel/drivers/net/ethernet/intel/igc/igc.ko.zst > /tmp/igc.ko
  nm -u /tmp/igc.ko | awk '{print $2}' | sort -u | wc -l
  rm -f /tmp/igc.ko
  ```
- **Doğrulanmış Ölçümler:**
  - `igc.ko` (`6.18.60-1-lts`): **247 dış sembol** (`472 KiB` ELF).
  - `igc.ko` (`7.2.14-arch1-1`): **244 dış sembol** — 4 ana sürüm boyunca **237 sembol (%96.0) birebir aynı kalmıştır**; yalnızca 10 sembol kalkmış (`skb_add_rx_frag_netmem`, `__mutex_init` vb.) ve 7 sembol eklenmiştir (`mutex_init_generic`, `__napi_schedule_irqoff` vb.).
  - **Sınıflar Arası Karşılaştırma (`6.18.60-1-lts`):**
    - `e1000e.ko` (`200` sembol): `igc` ile **%72 ortaklık** (`145` ortak, `+55` ek sembol).
    - `r8169.ko` (`186` sembol): `%59` ortaklık — ancak harici `libphy.ko` (`141` dış / `444` ihraç sembol) ve `clk_*` (*Common Clock Framework*) bağımlılığı taşır!
    - `iwlwifi` yığını: `iwlwifi.ko` (`254`) + `iwlmvm.ko` (`353`) + `mac80211.ko` (`408`) + `cfg80211.ko` (`229`) — çok katmanlı geçişli bağımlılık.
    - `amdgpu.ko`: **1.107 dış sembol**, **30.574 KiB (~30.5 MiB) ELF** -> otomatik çeviri için ekonomik değildir.

### B.2. Yerel Ortam, Kaynak Erişilebilirliği ve Donanım Gerçekliği `[DOĞRULANDI]`

1. **Linux 6.12 ve 6.18 Kaynak Kod Durumu:**
   - `/usr/src/linux-lts` (`6.18.60-1-lts`) yalnızca modül derleme başlıklarını (`include/`, `Makefile`, `Kconfig`) içerir; `drivers/net/ethernet/intel/igc/*.c` kaynak dosyaları **yerel diskte yoktur**. Yarın Görev 3'te `kernel.org` üzerinden yalnızca ilgili sürücü dizini (`v6.12` ve `v6.18` etiketleri) indirilecektir.
2. **Konak Donanımı, VFIO ve IOMMU Durumu:**
   - **ACPI `DMAR` Tablosu:** `/sys/firmware/acpi/tables/DMAR` (`136 bayt`) **mevcut** (donanım Intel VT-d destekliyor). `/dev/vfio/vfio` ve `/dev/kvm` aktif.
   - **Aktif IOMMU Grupları:** `/sys/kernel/iommu_groups/` şu an **`0`** çünkü çalışan `linux-zen` çekirdek komut satırında `intel_iommu=on` parametresi bulunmuyor.
   - **Fiziksel NIC:** Makinede Intel `igc` (I225/I226) kartı **yoktur**; kablolu ağ `0000:02:00.0` Realtek `r8169`, kablosuz ağ `0000:00:14.3` Intel `iwlwifi` (AX201) üzerindedir.
   - **QEMU 11.1.3 Cihaz Desteği:** Kurulu `qemu-system-x86_64` ikilisi `-device intel-iommu`, `-device igb` (Intel 82576 MSI-X GbE), `-device e1000e` (Intel 82574L MSI-X GbE) ve `-device nvme` cihazlarını **destekliyor**, ancak `-device igc` modelini **içermiyor**.

### B.3. Gerçek IOMMU vs. Emüle IOMMU ve Alternatif Test Ortamı Kanıt Sınırları

Fiziksel `igc` kartı bulunmadığından H2 deneyi iki katmanlı yürütülecek ve kanıt sınırları birbirine karıştırılmayacaktır:

- **Katman 1 (Statik Derleme & Semantik Bağlama Hedefi — `igc`):** Upstream Linux 6.12 ve 6.18 `igc` C dosyaları, `libdry_lx_net` gölge başlıkları ve çalışma zamanına karşı **0 tanımsız sembol** kalacak şekilde derlenip bağlanacaktır (API kayması ve LOC bütçesi doğrudan `igc` üzerinde ölçülür).
- **Katman 2 (Canlı Paket + VFIO/MSI-X Çalışma Hedefi — QEMU L1 VM içinde `-device igb` / `-device e1000e`):** Aynı `libdry_lx_net` çalışma zamanı, QEMU KVM içinde `-machine q35,kernel-irqchip=split -device intel-iommu,intremap=on,caching-mode=on` ile başlatılan konuk ortamda `vfio-pci`'a bağlı emüle Intel `e1000e` / `igb` (multi-queue MSI-X mimarisi `igc`'nin doğrudan atasıdır) üzerinde gerçek ARP/ICMP/TCP çerçevesi geçirecektir.
- **Kesin Kanıt Sınırı:**
  - *Emüle QEMU `intel-iommu` Neyi Kanıtlar?* Sürücünün yalnızca izin verilen IOVA aralığına DMA yaptığını ve sınır dışı bir DMA adresinin anında IOMMU hatasıyla yakalandığını (*fonksiyonel izolasyon*).
  - *Emüle IOMMU Neyi KANITLAYAMAZ?* Gerçek silikon IOTLB invalidation gecikmesini (~2.000 döngü), PCIe *No-Snoop* TLP önbellek tutarlılığı hatalarını, gerçek PHY link-eğitimi zamanlamasını ve donanım FLR kilitlenmelerini. (Bunlar yalnızca konak `intel_iommu=on` ile yeniden başlatılıp ikincil bir fiziksel PCIe NIC bağlandığında Faz 3/4'te kanıtlanmış sayılacaktır.)

### B.4. `PREEMPT_RT` ve Kalıcı IOVA Arena + Gölge `vmemmap` Risk Analizi

1. **`PREEMPT_RT` Otomatik Dönüşüm Garantisi Değildir:** `request_threaded_irq` içindeki `primary_handler` atomiktir; seviye/paylaşımlı kesmelerde `IRQF_ONESHOT` zorunludur; `local_irq_save` / `local_bh_disable` çağrıları kilit sağlamaz (`local_lock_t` / `bh_lock` gerekir); `raw_spinlock_t` kullanan sürücüler derleme anında reddedilmelidir.
2. **Ada-İçi DMA Sahipliği (Security):** Kalıcı haritalı `DmaArena` içinde NIC, `dma_unmap` sonrasında bile o arenadaki baytlara fiziksel olarak yazabilir. Bu yüzden **kontrol halkaları (`IoRing` descriptor'ları) kesinlikle `DmaArena` dışındaki CPU-özel paylaşımlı bellekte tutulur**; TX arenası `IOMMU_READ`, RX arenası `IOMMU_WRITE` olarak ayrılır.
3. **Önbellek Tutarlılığı (Cache Coherency):** x86-64'te VT-d *Snoop Control* biti zorlanır; AArch64 portunda `dry_lx_dma_sync_*` işlevleri donanım önbellek temizleme (`DC CIVAC`/`CVAC`) buyruklarına inmek zorundadır.
4. **Scatter-Gather ve Arena Dışı Pointer'lar:** `dry_lx_dma_map_*` kendisine gelen her adresi `[arena_base, arena_end)` aralığında denetler; kontrol yolundan gelen arena dışı `kmalloc` tamponları için küçük bir *bounce* dilimi ayırır (aksi halde gölge `vmemmap` aritmetiği geçersiz bellek okur).
5. **Çökme ve Reset (FLR):** Sürücü adası çöktüğünde `DmaArena` sayfaları sıfırlanıp geri alınmadan **önce** PCI Command yazmacında *Bus Master Enable (BME)* kapatılmalı, PCIe *Function Level Reset (FLR)* uygulanmalı ve IOMMU domain eşlemesi sökülmelidir.

### B.5. `H2` Ölçülebilir Kabul ve Red Kriterleri

- **Kabul:** (1) `libdry_lx_net` toplam boyutu **<= 5.000 LOC C** (kesin üst sınır: `< 10.000 LOC`), (2) Upstream sürücü C kodunda **<= %1** manuel değişiklik, (3) 6.12 -> 6.18 `igc` adaptasyon farkı **<= 100 LOC**, (4) Emüle `intel-iommu` (`caching-mode=on`) altında sıcak yolda **0 dinamik `VFIO_IOMMU_MAP_DMA`** çağrısı ve 0 IOMMU fault ile kesintisiz paket alışverişi, (5) Tüm ada bağımlılıklarının `GPL-2.0-only` uyumlu olması.

