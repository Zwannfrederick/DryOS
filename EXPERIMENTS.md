# DRY OS — Faz 0 Doğrulama ve Fizibilite Deney Raporu (`H1` & `H2`)

> **Son Güncelleme:** 11 Kasım 2026  
> **Durum:** **Bölüm A (`H1` Scheduler Doğrulaması) TAMAMLANDI ve DOĞRULANDI (`[DOĞRULANDI]`).** Bölüm B (`H2` Driver Island) sıradaki aşama olarak korunmaktadır.

---

## Bölüm A: `H1` — Zamanlayıcı Durum Makinesi ve Bellek Sıralaması Doğrulaması `[DOĞRULANDI — 11 Kasım 2026]`

### A.1. Doğrulanan Mimari Protokol (`AtomicU32(exec_state, park_token)`)

Her görev (`Task` / `TCB`) tek bir 32-bit atomik kelimede iki dikey (*orthogonal*) alanı paketler (`experiments/h1-sched/src/lib.rs`):

- **`exec_state` (`bits 0..=3`):** `Ready (0)`, `Running (1)`, `SwPreempt (2)`, `SwPark (3)`, `Blocked (4)`, `Waking (5)`, `Exiting (6)`, `Dead (7)`.
- **`park_token` (`bits 4..=5`):** `None (0)`, `Armed (1)`, `Notified (2)`.
- **Zorunlu Protokol Kuralları:**
  1. **Stack Sahipliği Bırakımı:** Giden CPU `switch_to` bitip kendi yığınından (`RSP`) ayrılmadan `SwPreempt` veya `SwPark` durumundan çıkamaz (`post_switch_*`).
  2. **Yayınlama Sırası (`Publication Order`):** Her kuyruklayıcı (`post_switch_preempt`, `post_switch_park`, `finish_waking_and_push`) önce `CAS` (`AcqRel`) ile `exec_state = Ready` yazar, **ancak ondan sonra** `rq.push(task_id)` (`Release`) çağırır.
  3. **Kör Yazma (`Blind Store`) Yasağı:** `SwPreempt -> Ready`, `SwPark -> Ready/Blocked` ve `Waking -> Ready` geçişlerinin tamamı `CAS` döngüsü kullanır; böylece hem eşzamanlı `ParkToken::Notified` jetonu korunur hem de `Exiting` durumundaki bir görevin yeniden canlandırılması (*Use-After-Free*) engellenir.
  4. **Zayıf Bellek `StoreLoad` Bariyeri:** `prepare_park()` sonrasında `cond` okumadan önce ve `Notification::signal()` içinde `signals.fetch_or` sonrasında `wakeup()` çağırmadan önce açık `atomic::fence(Ordering::SeqCst)` zorunludur.
  5. **Quiescent TCB Geri Kazanımı (`Exiting -> Dead`):** Görev `Exiting` olduktan sonra yalnızca `stack_owner == NO_CPU` ve `in_flight_refs == 0` (hiçbir `RunQueue` veya aktif uyandırıcı referansı kalmadığında) `Dead` durumuna geçebilir.

---

### A.2. TLA+ TLC Biçimsel Doğrulama Sonuçları (`formal/h1/`) `[DOĞRULANDI]`

- **Araç:** `tools/tla2tools.jar` — `TLC2 Version 2026.11.06.222357 (rev: d8fb231)` (`/usr/bin/java`, OpenJDK 21.0.13, Arch Linux `7.2.14-zen1-1-zen`).
- **Model Dosyası:** `formal/h1/DrySched.tla` (`578` satır; 3 mantıksal CPU `{"c0", "c1", "c2"}`, iki aşamalı runqueue yayınlama `MarkReady` + `PushRq`, iki aşamalı iş çalma `DequeueTask` + `TryDispatchPopped`, 2 eşzamanlı uyandırıcı `w_sig` + `w_spur`, gönüllü iptal `TaskCancelBeforeCommit`, asenkron sonlandırma `ExternalKill` -> `ReapDeadTask`).

#### 1. Çalıştırılan Gerçek TLC Komutları

```bash
# Negatif Test 1: Context switch bitmeden erken READY yayını
java -XX:+UseParallelGC -cp tools/tla2tools.jar tlc2.TLC -cleanup -noGenerateSpecTE -difftrace \
  -metadir /tmp/tlc-bug1 -config formal/h1/DrySchedBug1_EarlyReady.cfg formal/h1/DrySched.tla

# Negatif Test 2: Ters yayın sırası (EXEC_READY öncesi rq.push)
java -XX:+UseParallelGC -cp tools/tla2tools.jar tlc2.TLC -cleanup -noGenerateSpecTE -difftrace \
  -metadir /tmp/tlc-bug2 -config formal/h1/DrySchedBug2_PubOrder.cfg formal/h1/DrySched.tla

# Negatif Test 3: post_switch içinde kör store ile TOK_NOTIFIED ezilmesi
java -XX:+UseParallelGC -cp tools/tla2tools.jar tlc2.TLC -cleanup -noGenerateSpecTE -difftrace \
  -metadir /tmp/tlc-bug3 -config formal/h1/DrySchedBug3_PostSwitchClobber.cfg formal/h1/DrySched.tla

# Negatif Test 4: WAKING -> READY geçişinde kör store ile EXITING görevin diriltilmesi
java -XX:+UseParallelGC -cp tools/tla2tools.jar tlc2.TLC -cleanup -noGenerateSpecTE -difftrace \
  -metadir /tmp/tlc-bug4 -config formal/h1/DrySchedBug4_WakingClobber.cfg formal/h1/DrySched.tla

# Pozitif Safety Doğrulaması (Adillik varsayımı YOK — SpecSafety)
java -XX:+UseParallelGC -cp tools/tla2tools.jar tlc2.TLC -cleanup -noGenerateSpecTE -difftrace \
  -metadir /tmp/tlc-safety -config formal/h1/DrySchedSafety.cfg formal/h1/DrySched.tla

# Adillik Sınırı Analizi (Kuantum garantisi OLMADAN WF — Liveness karşı-örneği)
java -XX:+UseParallelGC -cp tools/tla2tools.jar tlc2.TLC -cleanup -noGenerateSpecTE -difftrace \
  -metadir /tmp/tlc-liveness-noq -config formal/h1/DrySchedLivenessNoQuantum.cfg formal/h1/DrySched.tla

# Pozitif Liveness Doğrulaması (Yalnızca WF_vars, SF YOK, RequireQuantumStep = TRUE)
java -XX:+UseParallelGC -cp tools/tla2tools.jar tlc2.TLC -cleanup -noGenerateSpecTE -difftrace \
  -metadir /tmp/tlc-liveness-3cpu -config formal/h1/DrySchedLiveness.cfg formal/h1/DrySched.tla
```

#### 2. Ölçülen TLC Durum Sayıları ve Sonuç Tablosu

| Yapılandırma (`.cfg`) | Mod / Adillik | Üretilen Durum | Ayrık Durum | Derinlik | Süre | Sonuç / İhlal |
| :--- | :--- | ---: | ---: | ---: | ---: | :--- |
| `DrySchedBug1_EarlyReady.cfg` | Hatalı Safety | 204 | 106 | 6 | `<1s` | **İHLAL:** `StackOwnershipSafe` (`stack_users = {"c0", "c1"}`) |
| `DrySchedBug2_PubOrder.cfg` | Hatalı Safety | 612 | 342 | 8 | `<1s` | **İHLAL:** `NoOrphanedReadyTask` (çalınan görev kuyruk dışı yetim kaldı) |
| `DrySchedBug3_PostSwitchClobber.cfg` | Hatalı Safety | 735 | 404 | 9 | `<1s` | **İHLAL:** `NoMissedSignalBlock` (`TOK_NOTIFIED` ezildi -> kalıcı `BLOCKED`) |
| `DrySchedBug4_WakingClobber.cfg` | Hatalı Safety | 2.247 | 1.088 | 10 | `<1s` | **İHLAL:** `NoDeadRequeue` (`EXITING` görev `READY` olarak diriltildi) |
| **`DrySchedSafety.cfg`** | **Düzeltilmiş Safety (`0` fairness)** | **84.351** | **31.279** | **31** | **`1s`** | **GEÇTİ (`0` hata — 8/8 Safety invariant doğrulandı)** |
| `DrySchedLivenessNoQuantum.cfg` | Liveness (`WF`, 0-buyruk kuantum) | 7.968 | 3.321 | 25 | `1s` | **İHLAL (Beklenen Sınır):** `NoLostWakeup` (22 adımlı 0-buyruk kesme kilitlenmesi) |
| **`DrySchedLiveness.cfg`** | **Düzeltilmiş Liveness (`WF`, `>=1` buyruk kuantum, `SF` YOK)** | **54.095** | **21.073** (`42.146` tableau) | **31** | **`6s`** | **GEÇTİ (`0` hata — `NoLostWakeup` & `CleanTermination` doğrulandı)** |

#### 3. Yeniden Üretilen Karşı Örneklerin Özeti

- **Karşı Örnek 1 (`BugEarlyReadyBeforeSwitch`):** `State 3` (`c0` görevi çalıştırır, `stack_users={"c0"}`) -> `State 4` (`PreemptTask("c0")` yığından çıkmadan `READY` ve `rq["c0"]` yayınlar) -> `State 5-6` (`c1` görevi `c0`'dan çalar ve çalıştırır -> `stack_users={"c0","c1"}`).
- **Karşı Örnek 2 (`BugPushBeforeReady`):** `State 5` (`PostSwitchFinishStack("c0")` görevi hala `SW_PREEMPT` iken `rq["c0"]` içine iter) -> `State 6-7` (`c1` görevi çalar, `exec_state != READY` gördüğü için `CAS` başarısız olur ve görevi düşürür) -> `State 8` (`PostSwitchLateReady("c0")` durumu `READY` yapar; görev tüm kuyruklardan düşmüş halde kalıcı yetim kalır).
- **Karşı Örnek 3 (`BugPostSwitchBlindStore`):** `State 6` (`TaskCommitPark("c0")` -> `SW_PARK`) -> `State 7-8` (`SignalSetCond` + `SignalWakeup` jetonu `TOK_NOTIFIED` yapar) -> `State 9` (`PostSwitchFinishStack("c0")` kör atama ile `(BLOCKED, TOK_NONE)` yazar; uyandırma tamamen kaybolur).
- **Karşı Örnek 4 (`BugWakingBlindStore`):** `State 8` (`SpuriousWakeup` -> `WAKING`) -> `State 9` (`ExternalKill` -> `EXITING`, `terminated=TRUE`) -> `State 10` (`WakerFinishWaking` kör atama ile `exec_state="READY"` yazar; öldürülmüş TCB yeniden kuyruğa girer).
- **Liveness Adillik Sınırı (`DrySchedLivenessNoQuantum.cfg`):** Görev `cond = 1` değerini okuduktan (`pc = "PC_SEEN_ONE"`) hemen sonra `State 18 -> 19 (PreemptTask) -> 20 (PostSwitch) -> 21 (PushRq) -> 22 (DequeueTask) -> State 18` döngüsüne girerek tek bir buyruk bile ilerleyemeden sonsuza dek kesmeye uğrar. Bu iz, **salt Weak Fairness (`WF`) altında neden çekirdek uyku döngüsünde kesmelerin maskelenmesi ve donanım zaman diliminin (`APIC timer quantum`) en az 1 buyruk ilerlemeye izin vermesi gerektiğini** matematiksel olarak kanıtlar. `RequireQuantumStep = TRUE` ile **hiçbir Strong Fairness (`SF`) kullanılmadan** `NoLostWakeup` ve `CleanTermination` 3 CPU üzerinde eksiksiz sağlanmıştır.

---

### A.3. Rust `loom` ve Native Çok-Çekirdek Stres Doğrulaması (`experiments/h1-sched/`) `[DOĞRULANDI]`

#### 1. Çalıştırılan Komutlar

```bash
# Lint kapısı (hem native hem cfg(loom) hedefleri, sıfır uyarı zorunlu)
cargo clippy --offline --manifest-path Cargo.toml --all-targets -- -D warnings
cargo clippy --offline --manifest-path Cargo.toml --config 'build.rustflags=["--cfg","loom"]' --all-targets -- -D warnings

# Rust Loom 0.7.3 C11 zayıf bellek doğrulaması (5 negatif + 5 pozitif test)
cargo test --offline --manifest-path Cargo.toml --config 'build.rustflags=["--cfg","loom"]' \
  --test loom_sched --release -- --nocapture --test-threads=1

# Native 8-CPU ve 20-CPU SMP stres doğrulaması
cargo test --offline --manifest-path Cargo.toml --test stress_native --release -- --nocapture
```

#### 2. Geliştirme Sırasında Yakalanan Gerçek Hatalar ve Düzeltmeler (Gizlenen Test = `0`)

1. **İlk `loom` Koşusunda Yakalanan Başarısız Test (`loom_preempt_and_steal_preserves_armed_token_and_stack`):**
   - **Belirti:** İlk `loom` çalıştırmasında `7 passed; 1 failed` alındı: `panicked at experiments/h1-sched/src/lib.rs:323:26: Invalid state in commit_park: (Running, None)`.
   - **Kök Neden:** Test senaryosunda `CPU 1` görevi `rq0`'dan çalıp `commit_park(1)` yavaş yoluna soktuğunda ve eşzamanlı `waker` görevi uyandırıp `rq1`'e (`(Ready, None)` durumunda, yani `commit_park` çağrısından *dönmüş* program sayacında) koyduğunda, ana test iş parçacığı `rq0` ile `rq1` kuyruklarını ayırt etmeyerek görevi `rq1`'den çekip `prepare_park()` çağırmadan ikinci kez `commit_park()` içine soktu.
   - **Mimari Ders & Düzeltme:** `Task::commit_park` içindeki sıkı `(Running, None) => panic!` koruması bu protokol ihlalini anında yakaladı. Çekirdek uyku döngüsünde `commit_park()` dönüşü her zaman `while !cond { prepare_park(); ... }` döngüsünün başına döner; hiçbir zaman `prepare_park()` çağrılmadan `commit_park()` tekrar çağrılamaz. Test senaryosu `rq0` (henüz `commit_park` öncesi) ve `rq1` (`commit_park` sonrası uyanmış görev) program sayaçlarını doğru ayıracak şekilde düzeltildi ve **21.804 permütasyonun tamamı `0` hatayla geçti**.
2. **RC11 / `loom 0.7.3` İçinde `Ordering::SeqCst` Operasyon vs. `fence(Ordering::SeqCst)` Bulgusu:**
   - `loom-0.7.3/src/rt/thread.rs:336` kaynak incelemesinde ve negatif testte (`loom_catches_missing_seqcst_fence_storeload_lost_wakeup`) doğrulandığı üzere, tekil atomik CAS/store üzerindeki `Ordering::SeqCst` iki farklı atomik değişken (`Task::sched` ve `Notification::signals`) arasındaki C11/RC11 `StoreLoad` yeniden sıralamasını **engellemez**. Açık `atomic::fence(Ordering::SeqCst)` (`seq_cst_fence()` / AArch64 `DMB ISH`) kullanıldığında **0 kayıp uyandırma** kanıtlanmıştır.
3. **TLA+ Modellemesinde Seviye-Tetiklemeli (`Sticky`) vs. Jeton-Tabanlı Uyandırma Ayrımı:**
   - TLA+ ve Loom analizleri, `WAKING -> READY` geçişindeki kör `store` hatasının yapışkan `cond=1` döngüsünden ziyade **(a) eşzamanlı `terminate()` (`EXITING`) durumunu ezerek ölü TCB'yi kuyruğa geri sokma (*Use-After-Free*)** ve **(b) zaman aşımıyla uyanan görevde eşzamanlı `TOK_NOTIFIED` bilgisini silme** hatalarına yol açtığını gösterdi; `CAS((Waking, tok) -> (Ready, tok))` her iki hatayı da kapattı.

#### 3. Doğrulanmış `loom` ve Native Stres Ölçümleri

- **Loom Negatif Kanıt Testleri (`5/5` beklenen paniği deterministik yakaladı):**
  - `loom_catches_missing_seqcst_fence_storeload_lost_wakeup` (`StoreLoad` kayıp uyandırma)
  - `loom_catches_post_switch_blind_store_lost_wakeup` (`SW_PARK` kör yazma kayıp uyandırma)
  - `loom_catches_push_before_ready_orphan_race` (`Ready` öncesi `rq.push` yetim görev yarışı)
  - `loom_catches_early_ready_stack_collision` (yığın bırakılmadan erken `Ready` çift-CPU stack çarpışması)
  - `loom_catches_waking_blind_store_resurrecting_exiting_task` (`Exiting` görevin diriltilmesi)
- **Loom Pozitif Zayıf Bellek Testleri (`5/5` geçti, toplam `39.135` C11 zayıf bellek permütasyonu, `0` hata):**
  - `loom_sleep_wakeup_no_lost_wakeup`: **140** permütasyon (`ok`)
  - `loom_cancel_park_races_with_wakeup`: **500** permütasyon (`ok`)
  - `loom_concurrent_dual_wakeup_during_waking`: **2.835** permütasyon (`ok`)
  - `loom_termination_races_with_wakeup_and_reap`: **13.856** permütasyon (`ok`)
  - `loom_preempt_and_steal_preserves_armed_token_and_stack`: **21.804** permütasyon (`ok`)
- **Native Çok-Çekirdek Stres Testleri (`3/3` geçti, `>24 Milyon` toplam geçiş, `0` hata):**
  - **8-CPU (Konak 4C/8T eşleşmeli, 24 görev):** `12.018.728` geçiş, `304.239` başarılı *work-steal*, `628.162` uyandırma, süre `278.76 ms` (**`43.12 Milyon işlem/sn`**), `24/24` görev temiz `Dead` geri kazanımı.
  - **20-CPU (DRY OS hedef topolojisi, 40 görev):** `12.037.985` geçiş, `312.295` başarılı *work-steal*, `619.059` uyandırma, süre `202.20 ms` (**`59.54 Milyon işlem/sn`**), `40/40` görev temiz `Dead` geri kazanımı.

---

### A.4. Modelin (TLC ve Loom) Kapsamadığı Gerçek Donanım Davranışları `[AÇIK SINIRLAR]`

TLC ve Rust `loom` algoritmik durum makinesini ve C11/RC11 nedensellik sıralamasını kanıtlar; ancak aşağıdaki fiziksel donanım davranışlarını **kapsamaz** (Faz 1 `dry-kernel` kodlamasında doğrudan donanım/assembly testleriyle doğrulanacaktır):

1. **Assembly `switch_to` Yazmaç / `RSP` Değişim Penceresi:** Gerçek çekirdekte `callee-saved` yazmaçların (`RBX, RBP, R12..R15`) yığına itilmesi ve `RSP` değişimi `global_asm!` / `naked` fonksiyon içinde gerçekleşir; derleyicinin `post_switch_*` çağrısını `RSP` değişiminin önüne taşımaması için assembly sınırı ve `compiler_fence(Ordering::SeqCst)` zorunludur.
2. **Yeni Görev İlk Giriş Trampolini (`thread_entry_trampoline`):** Hiç `switch_to` içinden çıkmamış yeni bir görev ilk kez CPU'ya alındığında normal dönüş yoluna düşmez; trampolin fonksiyonunun **ilk buyruk olarak `post_switch_*(prev)` çağırması** gerekir, aksi halde bir önceki görev `SwPreempt`/`SwPark` içinde asılı kalır.
3. **Donanım Kesme Maskeleme (`RFLAGS.IF` / `DAIF`) ve APIC/GIC Zamanlaması:** TLC `DrySchedLivenessNoQuantum.cfg` izinin kanıtladığı gibi, `prepare_park -> cond.load -> commit_park` çekirdek kritik bölgesi yerel kesmeler kapalıyken (`CLI`) çalışmalı ve `IRETQ`/`SYSRETQ` öncesi `APIC` zamanlayıcısı sıfır-buyruk kesme fırtınası yaratmayacak pozitif bir eşiğe kurulmalıdır.
4. **Sanal Adres Alanı (`CR3` / PCID / ASID) ve TLB Shootdown Eşzamanlılığı:** Görev `CPU 0`'dan `CPU 1`'e çalındığında (`steal`), `post_switch` öncesinde aktif sayfa tablosunun (`CR3`) ve `active_cpus` TLB maskesinin atomik devri bu modelde soyutlanmıştır.
5. **Fiziksel 20 Çekirdek Önbellek Hattı Sekmesi (*Cache-Line Bouncing*) ve NUMA Gecikmesi:** Konak makine 4 fiziksel çekirdek / 8 mantıksal thread (`Tiger Lake`) olduğundan 20-CPU testi işletim sistemi thread aşırı-aboneliğiyle (*oversubscription*) koşmuştur; gerçek çok-soketli / 20 fiziksel çekirdekli donanımda `Task` ve `RunQueue` yapılarının `#[repr(align(64))]` önbellek hattı izolasyonu Faz 4'te donanım sayaçlarıyla ölçülecektir.

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
   - `/usr/src/linux-lts` (`6.18.60-1-lts`) yalnızca modül derleme başlıklarını (`include/`, `Makefile`, `Kconfig`) içerir; `drivers/net/ethernet/intel/igc/*.c` kaynak dosyaları **yerel diskte yoktur**. Görev 3'te `kernel.org` üzerinden yalnızca ilgili sürücü dizini (`v6.12` ve `v6.18` etiketleri) indirilecektir.
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
