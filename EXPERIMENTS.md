# DRY OS — Faz 0 Doğrulama ve Fizibilite Deney Raporu (`H1` & `H2`)

> **Son Güncelleme:** 11 Kasım 2026  
> **Durum:** **Bölüm A (`H1` Scheduler Doğrulaması) ve Bölüm B (`H2` Linux Driver Island Fizibilite Deneyi) TAMAMLANDI ve DOĞRULANDI (`[DOĞRULANDI]`).** Fiziksel silikon IOMMU güvenlik izolasyonu açık sınır olarak Faz 3/4 bare-metal aşamasına ayrılmıştır.

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

## Bölüm B: `H2` — Linux Driver Island (`igc`) Fizibilite Doğrulaması `[DOĞRULANDI — 11 Kasım 2026]`

### B.1. Upstream `v6.12` ve `v6.18` `igc` Kaynak Edinimi ve SHA Kayıtları (`vendor/linux-drivers/MANIFEST.json`)

Resmi `git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git` deposundan `drivers/net/ethernet/intel/igc` dizininin tamamı (`GPL-2.0-only`) `tools/h2-audit/fetch_igc_sources.py` ile indirilmiş ve tüm dosyaların SHA-256 özetleri kilitlenmiştir:

| Sürüm Etiketi | Tag Nesnesi SHA-1 | Commit SHA-1 | Dosya Sayısı | Toplam Satır | Upstream Değişiklik |
| :--- | :--- | :--- | ---: | ---: | ---: |
| **`v6.12`** | `06090c9b622a7e1f797e775db4c035e0d779b76e` | `adc218676eef25575469234709c2d87185ca223a` | `26` (`13` `.c`, `11` `.h`, `Makefile`, `Kconfig`) | `17.845` | **`0` satır (`%0.00`)** |
| **`v6.18`** | `f7b88edb52c8dd01b7e576390d658ae6eef0e134` | `7d0a66e4bb9081d75c82ec4957c50034cb0ea449` | `26` (`13` `.c`, `11` `.h`, `Makefile`, `Kconfig`) | `18.528` | **`0` satır (`%0.00`)** |

```bash
# Çevrimdışı SHA-256 bütünlük doğrulaması (52/52 upstream dosya)
python3 tools/h2-audit/fetch_igc_sources.py --verify
# Çıktı: [H2-VERIFY] OK: 52/52 upstream files match SHA-256 (0 modified)
```

---

### B.2. İkili ELF Sembol Denetimi vs. `libclang` AST Bağımlılık Çıkarımı (`tools/h2-audit/`)

#### 1. Çalıştırılan Denetim Komutları

```bash
# 1) Kurulu çekirdek modüllerinden (6.18.60-1-lts vs 7.2.14-arch1-1) ikili ELF sembol denetimi
python3 tools/h2-audit/audit_symbols.py \
  --kmod-lts 6.18.60-1-lts --kmod-curr 7.2.14-arch1-1 \
  --out experiments/h2-island/manifests/igc-kmod-symbols.json

# 2) libclang 23.1.4 AST tabanlı kaynak bağımlılık çıkarımı (v6.12 ve v6.18 tüm 26 çeviri birimi)
python3 tools/h2-audit/clang_ast_deps.py \
  --kdir /lib/modules/6.18.60-1-lts/build \
  --out experiments/h2-island/manifests/igc-clang-ast-deps.json
```

#### 2. `247` / `244` İkili Sembol Karşılaştırması ve Çapraz Sürücü Örtüşmesi

- **`igc.ko.zst` Sürüm Kayması:** `6.18.60-1-lts` modülünde **`247`**, `7.2.14-arch1-1` modülünde **`244`** tanımsız (`U`) sembol yeniden üretildi. **`237` sembol (`%96.0`) birebir aynıdır**; `10` sembol kalkmış (`__mutex_init`, `hrtimer_init`, `skb_add_rx_frag_netmem`, `pci_enable_pcie_error_reporting` vb.) ve `7` sembol eklenmiştir (`mutex_init_generic`, `hrtimer_setup`, `skb_add_rx_frag`, `__napi_schedule_irqoff`, `__vlan_get_protocol_offset` vb.).
- **Çapraz Sürücü Ortaklığı (`6.18.60-1-lts`):**
  - `e1000e.ko` (`200` sembol): `igc` ile **`145` ortak sembol** (`e1000e` yüzeyinin **`%72.5`'i**).
  - `igb.ko` (`261` sembol): `igc` ile **`207` ortak sembol** (`igb` yüzeyinin **`%79.3`'ü**, `igc` yüzeyinin **`%83.8`'i**).

#### 3. Kritik Mimari Bulgu: `nm -u` İkili Analizi ile `libclang` Kaynak AST Analizi Arasındaki Fark

`tools/h2-audit/clang_ast_extractor.c` (`libclang` `23.1.4`) ile `v6.18` `igc`'nin 13 `.c` dosyasının tamamı AST düzeyinde tarandığında **`349` benzersiz dış çekirdek fonksiyon/yardımcı çağrısı** bulundu. Bunların **`195` tanesi (`%55.9`) çekirdek başlıklarındaki `static inline` yardımcılarıdır** (`wr32`/`rd32`/`readl`/`writel`, `dma_map_single`, `dma_map_page_attrs`, `page_address`, `skb_frag_off`, `netdev_tx_sent_queue`, `spin_lock_irqsave`, `rcu_read_lock` vb.) ve ikili `nm -u` çıktısında **görünmezler**. Tersine, derlenmiş `.ko` modülünde `static inline` genişlemeleri yüzünden `vmemmap_base`, `page_offset_base`, `softnet_data`, `this_cpu_off` ve `__preempt_count` gibi **15 monolitik çekirdek globali** sızmaktadır. Kaynak düzeyinde temiz gölge başlıklarla (`include/lx_emul/`) derleme yapıldığında bu 15 ikili global bağımlılığın tamamı ortadan kalkmaktadır.

#### 4. Dörtlü Bağımlılık Sınıflandırması (`sınıflandırılmamış = 0`)

| Kategori | `6.18` İkili ELF (`247`) | `v6.18` Kaynak AST — Tüm 13 TU (`349`) | `v6.18` Kaynak AST — Çekirdek 6 TU (`287`) | Temsili Arayüzler ve Mimari Karşılık |
| :--- | ---: | ---: | ---: | :--- |
| **`auto_map`** | `61` (`%24.7`) | `131` (`%37.5`) | `101` (`%35.2`) | `kzalloc`/`kfree`, `mutex_*`, `spin_lock_*`, `usleep_range`, `readl`/`writel`, `ether_addr_copy`, bit/endian yardımcıları |
| **`semantic_adapter`** | `95` (`%38.5`) | `132` (`%37.8`) | `128` (`%44.6`) | `dma_alloc_coherent`, `dma_map_single`/`page` (`DmaArena` + gölge `vmemmap`), `pci_enable_msix_range`/`request_irq` (`eventfd`), `napi_schedule`/`napi_gro_receive` (`IoRing`) |
| **`feature_prune`** | `76` (`%30.8`) | `40` (`%11.5`) | `21` (`%7.3`) | `ptp_clock_*`, `ethtool_*`, `led_classdev_*`, `pm_runtime_*`, `__traceiter_*` (`igc_ptp.c`, `igc_ethtool.c`, `igc_leds.c`, `igc_diag.c`, `igc_dump.c`, `igc_tsn.c` budandı) |
| **`unsupported`** | `15` (`%6.1`) | `46` (`%13.2`) | `37` (`%12.9`) | Çekirdek-içi eBPF/XDP/AF_XDP UMEM (`bpf_*`, `xdp_*`, `xsk_*`, `xp_*` — `igc_xdp.c` budandı, çekirdek içi kancalar kapalı), `copy_from_user`/`copy_to_user` (`-EOPNOTSUPP`), `synchronize_rcu`, jump-label `static_key_*` |

- **Çıkarılan Çekirdek Geri-Çağırım Tabloları (`AST VarDecl`):** `igc_driver` (`struct pci_driver`), `igc_netdev_ops` (`struct net_device_ops`), `igc_ethtool_ops` (`struct ethtool_ops`), `igc_pm_ops` (`struct dev_pm_ops`).
- **Yakalanan LTS-İçi (`6.18.0 -> 6.18.60`) ABI Kırılması:** Saf `v6.18` etiketli `igc_main.c`, yerel `6.18.60-1-lts` çekirdek başlıklarına karşı parse edildiğinde **`2` derleme hatası** verdi (`xsk_buff_get_metadata` 2->3 argüman, `xsk_tx_metadata_request` 3->4 argüman). `v6.12` `igc_main.c` ise `6.18.60-1-lts` başlıklarına karşı **`5` hata** verdi (`del_timer_sync` -> `timer_delete_sync`, `hrtimer_init` -> `hrtimer_setup` + `xsk_*`). Bu bulgu, DRY OS'un dağıtım çekirdek başlıkları yerine sürümlenmiş kendi gölge başlık setini (`include/lx_emul/`) kullanmasının zorunlu olduğunu kanıtladı.

---

### B.3. `igc` Userspace Driver Island (`GPL-2.0-only`) Derleme ve Adaptör/Loopback Doğrulaması (`experiments/h2-island/`)

Gerçek, **0 satır değiştirilmiş** upstream `igc` çekirdek dosyalarının 6'sı (`igc_base.c`, `igc_mac.c`, `igc_nvm.c`, `igc_phy.c`, `igc_i225.c`, `igc_main.c` — `v6.18`'de `11.039` satır, `v6.12`'de `~10.700` satır) hem **`v6.18`** (`build/test_igc_618`) hem **`v6.12`** (`build/test_igc_612`) için userspace'de **0 derleme hatası ve 0 uyarıyla** derlenip bağlanmıştır:

```bash
make -C experiments/h2-island clean && make -C experiments/h2-island test
```

```text
./build/test_igc_618
[PASS] Test 1: DmaArena + shadow vmemmap bijection, permissions, & OOB rejection
[PASS] Test 2: BAR0 MMIO interceptor + eventfd MSI-X -> NAPI poll adapter
[PASS] Test 3: Upstream igc probe+open+3x(TX->DMA->MSIX->NAPI->IoRing RX)+close (coherent=8 rx_pages=1020 tx_bounce=3 mmio_r=751 mmio_w=841 dyn_iommu=0)
./build/test_igc_612
[PASS] Test 1: DmaArena + shadow vmemmap bijection, permissions, & OOB rejection
[PASS] Test 2: BAR0 MMIO interceptor + eventfd MSI-X -> NAPI poll adapter
[PASS] Test 3: Upstream igc probe+open+3x(TX->DMA->MSIX->NAPI->IoRing RX)+close (coherent=8 rx_pages=1020 tx_bounce=3 mmio_r=751 mmio_w=841 dyn_iommu=0)
```

- **Test 1 (`dma_arena.c`):** Kalıcı `DmaArena` tahsisi, doğrusal gölge `vmemmap` (`struct page * <-> IOVA <-> VA` $O(1)$ bijeksiyonu), TX (`IOMMU_READ`) / RX (`IOMMU_WRITE`) izin ayrımı, `DmaArena` dışı `kmalloc` TX tamponları için otomatik *bounce* kopyası (`tx_bounce_copies=1`) ve arena dışı RX / sınır dışı işaretçi reddi (`rejected_oob_maps=2`, `dynamic_iommu_syscalls=0`).
- **Test 2 (`lx_mmio_irq.c`):** BAR0 MMIO okuma/yazma kancası ve gerçek Linux `eventfd` üzerinden MSI-X kesme tetiklemesi -> arka plan IRQ iş parçacığı -> `napi_schedule` -> `poll` uyarlayıcısı.
- **Test 3 (`test_h2_igc_island.c`):** Gerçek upstream `igc_init_module` -> `igc_probe` (PCI ID `8086:15f3` I225-V, NVM checksum, MAC okuma, 1 TX + 1 RX kuyruk + MSI-X kurulumu) -> `igc_open` (`8` tutarlı descriptor halkası + `1.020` RX sayfa havuzu tahsisi, `IGC_RCTL`/`IGC_TCTL`/`IGC_RETA` programlama) -> `3` adet 60-bayt Ethernet çerçevesinin gerçek `igc_xmit_frame` ile TX descriptor halkasına yazılıp `IGC_TDT(0)` kapı ziline basılması -> simüle I225 DMA motorunun TX descriptor'dan okuyup RX descriptor'daki `DmaArena` sayfasına yazması ve MSI-X `eventfd` tetiklemesi -> gerçek `igc_msix_ring` -> `igc_poll` -> `napi_gro_receive` -> `dry_io_ring` üzerinden `3/3` çerçevenin bit-birebir doğrulanması -> `igc_close` -> `igc_remove` (`dyn_iommu=0`).

---

### B.4. Ayrı Sürücü/Cihaz Modeli Deneyi: Canlı QEMU KVM + Emüle `intel-iommu` + `vfio-pci` `e1000e` (`tests/vfio_e1000e_live.c`)

> **AÇIK AYRIM VE GÜVENLİK SINIRI:** Konak makinede fiziksel Intel `igc` (I225/I226) kartı yoktur, konak çekirdekte `intel_iommu=on` aktif değildir (`/sys/kernel/iommu_groups` = `0`) ve QEMU `11.1.3` ikilisi `-device igc` modeli içermez. Bu nedenle aşağıdaki canlı VFIO deneyi **`igc` üzerinde değil, farklı bir PCIe NIC modeli olan emüle Intel 82574L `e1000e` (`8086:10d3`) üzerinde yürütülen AYRI bir donanım-arayüz deneyidir**. Ayrıca QEMU `intel-iommu` yazılımsal bir emülasyon olduğundan **fiziksel silikon IOMMU / PCIe DMA güvenlik doğrulaması yapıldığı İDDİA EDİLMEZ** (PCIe ATS/ACS kaçakları, *No-Snoop* önbellek tutarsızlıkları, silikon errata'ları ve gerçek IOTLB zamanlaması yalnızca bare-metal VT-d donanımında doğrulanabilir).

```bash
make -C experiments/h2-island vfio-test
```

```text
=== [H2-VFIO] Separate Live QEMU intel-iommu + vfio-pci e1000e Experiment ===
[PASS] VFIO-1 MMIO & IOMMU Groups: 0000:01:00.0(grp=5 mac=02:00:00:00:00:01 status=0x80283) <-> 0000:02:00.0(grp=6 mac=02:00:00:00:00:02 status=0x80283)
[PASS] VFIO-2 MSI eventfd IRQ Adapter: hits=1 icr=0x80000080
[PASS] VFIO-3 Live Wire TX->RX DMA + MSI: len=64 payload="DRYOS-H2-LIVE-VFIO-DMA-OK" dyn_iommu_maps=0
[INFO] VFIO-4 Submitting unmapped IOVA 0xdead0000 to verify emulated DMAR fault...
qemu-system-x86_64: vtd_iova_to_sspte: detected sspte permission error (iova=0xdead0000, level=0x3, sspte=0x0, write=0, pasid=0x0)
[    0.911318] DMAR: [DMA Read NO_PASID] Request device [01:00.0] fault addr 0xdead0000 [fault reason 0x06] PTE Read access is not set
[PASS] H2-VFIO-SEPARATE-POC COMPLETE (NOTE: emulated QEMU intel-iommu != physical silicon security proof)
```

Konuk Linux çekirdeğinde **hiçbir NIC sürücüsü yüklü olmadan**, salt userspace `/init` süreci `vfio.ko` + `vfio-pci.ko` üzerinden iki ayrı `pcie-root-port` (`grp=5` ve `grp=6`) üzerindeki iki `e1000e` kartını sahiplenmiş; `VFIO_IOMMU_MAP_DMA` ile 3 sayfalık sabit `DmaArena` (`0x40000000` RW halka, `0x40001000` salt-okunur TX, `0x40002000` salt-yazılır RX) kurmuş; sıcak yolda **`0` dinamik IOMMU çağrısıyla** `NIC0 -> QEMU hub -> NIC1` üzerinden gerçek 64-bayt Ethernet DMA paketini ve MSI `eventfd` kesmesini geçirmiş; son adımda eşlenmemiş `0xdead0000` IOVA adresine yapılan DMA okumasının VT-d ikinci seviye sayfa tablosu tarafından `[fault reason 0x06] PTE Read access is not set` hatasıyla engellendiğini göstermiştir.

---

### B.5. Ölçülen Uyumluluk Runtime LOC, Sürüm Taşıma Maliyeti ve Kabul Kriteri Karşılaştırması

| Metrik | `H2` Kabul Hedefi (`§B.5`) | Gerçek Ölçüm (11 Kasım 2026) | Sonuç |
| :--- | :--- | :--- | :--- |
| **`libdry_lx` Runtime Boyutu (`include/` + `src/`)** | `<= 5.000 LOC` (`< 10.000` tavan) | **`3.468` toplam satır (`3.028` SLOC)** (`914` satır `.c` + `2.554` satır `.h`) | **GEÇTİ** |
| **Upstream `igc` Kaynak Kodu Değişikliği** | `<= %1` | **`0` satır / `%0.00`** (`v6.12` ve `v6.18` 52/52 SHA-256 korundu) | **GEÇTİ** |
| **`v6.12 -> v6.18` Runtime Adaptasyon Farkı** | `<= 100 LOC` | **`38` toplam satır (`28` SLOC)** | **GEÇTİ** |
| **Sıcak Yolda Dinamik IOMMU Haritalama** | `0` çağrı | **`0` çağrı** (`dyn_iommu=0` hem `igc` adasında hem `vfio-pci` testinde) | **GEÇTİ** |
| **Lisans Uyumu (`ADR-005`)** | Tümü `GPL-2.0-only` uyumlu | Tüm `experiments/h2-island/` ve `tools/h2-audit/` dosyaları `GPL-2.0-only` | **GEÇTİ** |

- **Dosya Bazında `libdry_lx` Satır Dağılımı:** `dry_island.h` (`147`), `lx_types.h` (`662`), `lx_mm_dma.h` (`192`), `lx_pci_irq.h` (`255`), `lx_subsys.h` (`465`), `lx_net.h` (`639`), `lx_xdp_stubs.h` (`142`), `26` yönlendirici başlık (`52`), `dma_arena.c` (`263`), `lx_mmio_irq.c` (`252`), `lx_net_shim.c` (`284`), `igc_pruned_stubs.c` (`115`).
- **`v6.12 -> v6.18` Taşıma Farkının Tam Dökümü (`38` satır):** `del_timer_sync` / `timer_delete_sync` ve `hrtimer_init` / `hrtimer_setup` (`lx_types.h`), `pci_is_pcie` ve `pcie_capability_read/write_word` (`lx_pci_irq.h`), `struct hwtstamp_config` (`lx_subsys.h`), `net_device_ops.ndo_eth_ioctl` (`lx_net.h`), `copy_from_user` / `copy_to_user` (`lx_xdp_stubs.h`) ve `igc_ptp_{get,set}_ts_config` vs `igc_ptp_hwtstamp_{get,set}` + `igc_fpe_*` (`igc_pruned_stubs.c`).

---

### B.6. Karşılaşılan Semantik Problemler, Hatalar ve Uygulanan Çözümler (Gizlenen Hata = `0`)

1. **`audit_symbols.py` İlk Çalıştırmada Sınıflandırılmamış Sembol Hatası:** `7.2.14` modülünde yeni eklenen `__vlan_get_protocol_offset` sembolü ilk kural setinde eşleşmeyip `RuntimeError: Unclassified symbols remain (1)` fırlattı -> `"__vlan_"` öneki `semantic_adapter` kurallarına eklendi (`0` sınıflandırılmamış).
2. **`clang_ast_extractor.c` Derleme Hatası:** İlk derlemede var olmayan `clang_createCXString` API'si çağrıldığı için `-Werror` altında derleme kırıldı -> `if (decl_file)` korumasıyla standart `clang_getFileName` akışına düzeltildi.
3. **Çevrimdışı `fetch_igc_sources.py` Ağ Erişimi Hatası:** Sandbox içinde doğrulama çağrıldığında betik `MANIFEST.json` kontrolünden önce `git ls-remote` çalıştırıp `HTTP 403` aldı -> çevrimdışı SHA-256 doğrulaması için `--verify` modu eklendi.
4. **Glibc `<pthread.h>` ile Gölge `<linux/types.h>` Ad Alanı Çarpışması:** `-Iexperiments/h2-island/include` bayrağı aktifken sistem `<pthread.h>` -> `<bits/sched.h>` -> `/usr/include/linux/sched/types.h` zinciri bizim gölge `linux/types.h` dosyamızı erken dahil etti ve `__u32` henüz tanımlanmadığı için derleme kırıldı -> `lx_types.h` içinde `__u8..__s64` typedef blokları `<pthread.h>` öncesine taşındı.
5. **`7.854` Satırlık `igc_main.c` İlk Derlemesinde 28 Eksik Çekirdek Makrosu:** `to_pci_dev`, `dev_kfree_skb`, `le16_to_cpup`/`le32_to_cpup`, `list_for_each_entry_reverse`, `MSEC_PER_SEC`, `tc_mqprio_qopt_offload.extack` ve `system_state` eksikleri yakalandı -> gölge başlıklara eklendi (`0` hata).
6. **Özellik Budama (`feature_prune`) Sırasında Yakalanan Gizli Donanım Bağımlılığı:** Budanan `igc_ethtool.c` dosyasının içindeki `igc_write_rss_indir_tbl()` fonksiyonunun normal RX kurulumunda (`igc_configure_rx`) `IGC_RETA(0..31)` donanım yazmaçlarını doldurmak için çağrıldığı tespit edildi -> boş no-op yerine `igc_pruned_stubs.c` içinde gerçek 12 satırlık `IGC_RETA` yazma döngüsü uygulandı.
7. **`v6.12` Bağlama (`ld`) Hatası (`igc_ptp_get/set_ts_config`):** `IGC_TXDCTL_QUEUE_ENABLE` makrosunun `v6.12` `igc_regs.h` içinde de tanımlı olması nedeniyle `#ifdef` korumasının `v6.12` sembollerini gizlediği görüldü -> her iki sürümün PTP saplama sembolleri koşulsuz ihraç edildi.
8. **Test 3 İlk Koşuda `igc_probe == -IGC_ERR_NVM` Başarısızlığı:** `igc_i225.c:82-86` (`igc_get_hw_semaphore_i225`), `IGC_SWSM` yazmacına `IGC_SWSM_SWESMBI` yazdıktan sonra okuduğunda bitin mandallanmasını (*latch*) bekliyordu; ilk simülatör `IGC_SWSM` okumasında sabit `0` döndürdüğü için probe `-1` ile reddedildi -> `IGC_SWSM` / `IGC_SW_FW_SYNC` yazmaçlarının yazılan değeri mandallaması ve `IGC_EERD` okumasının `IGC_NVM_RW_REG_DONE` bitini dönmesi sağlandı; hem `v6.18` hem `v6.12` tek seferde geçti.

---

### B.7. Ayrı Başarı Seviyeleri Özeti (`ADR-006` Kapı Değerlendirmesi)

1. **Seviye 1 — Doğrulanmış Bağımlılık ve Sürüm Kayması Analizi `[TAM BAŞARI]`:** İkili ELF (`247`/`244`, `%96` kararlılık) + `libclang` AST (`349` çağrı, `%55.9` satır-içi gizli yüzey) tam sınıflandırıldı (`0` belirsiz sembol) ve `v6.12 -> v6.18` taşıma farkının yalnızca `38` satır olduğu ölçüldü.
2. **Seviye 2 — Derlenebilir ve Çalışan `igc` (`v6.12` & `v6.18`) Userspace Driver Island `[FONKSİYONEL PoC BAŞARILI — SİMÜLE DONANIM]`:** `18.528` satırlık upstream `igc` sürücüsü `%0` değişiklik ve `3.468` satırlık `GPL-2.0-only` runtime ile derlenip simüle I225 DMA/MSI-X geri-döngüsünde uçtan uca TX/RX paket geçirdi.
3. **Seviye 3 — Canlı VFIO + IOMMU Donanım-Arayüz PoC (`e1000e` Ayrı Deneyi) `[FONKSİYONEL PoC BAŞARILI — FARKLI CİHAZ MODELİ & EMÜLE IOMMU]`:** Gerçek Linux `vfio-pci` + KVM `intel-iommu` üzerinde 0 dinamik IOMMU çağrısıyla canlı Ethernet DMA TX/RX ve `0xdead0000` DMAR sayfa hatası yakalama doğrulandı.
4. **Doğrulanmamış Alan (Açık Sınır):** Fiziksel Intel I225/I226 silikonu üzerinde gerçek zamanlama/link eğitimi ve fiziksel VT-d silikon IOMMU güvenlik izolasyonu **henüz doğrulanmamıştır** (Faz 3 giriş kriteri olarak korunmaktadır; Faz 1 `dry-kernel` geliştirmesini engellemez).
