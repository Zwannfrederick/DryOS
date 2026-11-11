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

---

## Bölüm C — Faz 1 `Milestone 1A-E`: Limine SMP (`1/2/4/20 vCPU`), Per-CPU İzolasyon ve `#DF` `IST1` Doğrulaması

- **Tarih:** 11 Kasım 2026
- **Kapsam:** `dry-kernel` (`x86_64-unknown-none`, `no_std`, Limine `10.8.5` UEFI + `OVMF` + KVM) üzerinde `1`, `2`, `4` ve `20` vCPU kademeli SMP uyanışı, CPU başına bağımsız `GDT`, `TSS`, `IST1` (`#DF`), `IA32_GS_BASE` ve `PerCpuData` izolasyonu, negatif arıza testleri ve gerçek donanım `#PF -> #DF` yığın geçişi.
- **Tekrarlanabilir Komut:** `python3 tools/qemu_boot_test.py` (tekil SMP koşusu için `--smp-only {1|2|4|20}`).

### C.1. AP Başlangıç Eşzamanlılık ve Bellek Sıralaması Kuralları (`kernel/src/smp.rs`)

1. **Faz 1 — Tek Yazıcılı BSP Ön-Hazırlığı (`Single-Writer Pre-Publication`):** BSP (`cpu_idx = 0`), paylaşılan 256-girişli `IDT` tablosunu kurup `IDT_INITIALIZED.store(true, Release)` ile yayınlar; her AP için `prepare_cpu_boot_stack(cpu_idx)` ile kalıcı 16 KiB `boot_stack` ve 16 KiB `df_stack` taban kanaryalarını yazar, `AP_LAUNCH_SLOTS[cpu_idx]` kaydını doldurur ve `mp_info.extra_argument` alanına slot adresini yazar.
2. **Faz 2 — `LimineMpInfo::goto_address` Üzerinden `Release -> Acquire` Devri:** BSP, `fence(Ordering::Release)` sonrasında `mp_info.goto_address.store(dry_ap_trampoline, Ordering::Release)` yazar. Limine'in AP bekleme döngüsü bu adresi gözlemleyip `RFLAGS.IF = 0` (`CLI`), 64-bit Long Mode ve BSP ile özdeş `CR3` altında `dry_ap_trampoline(mp_info)` noktasına atlar.
3. **Faz 3 — Anında Kalıcı Yığın Devri (`dry_ap_trampoline` & `_start`):** Hem BSP (`_start`) hem her AP (`dry_ap_trampoline`), herhangi bir Rust yığın çerçevesi oluşturmadan önce assembly içinde Limine'in geçici `BOOTLOADER_RECLAIMABLE` yığınından çıkarak `'static` çekirdek `.bss` içindeki 16-bayt hizalı `CPU_DESCS[cpu_idx].boot_stack` tavanına geçer; `ap_main_rust` girişinde `fence(Ordering::Acquire)` ile BSP'nin tüm ön-yazımlarının görünürlüğü garanti edilir.
4. **Faz 4 — Per-CPU Donanım Bağlama Sırası ve Ayrıklık Denetimi:** x86-64'te `mov gs, ax` buyruğu gizli `IA32_GS_BASE` yazmacını sıfırladığı için her CPU **zorunlu olarak önce** `init_cpu_gdt_tss(cpu_idx)` (`lgdt`, uzak `retfq`, `ltr 0x28`), ardından `load_idt()` ve **sonrasında** `init_cpu_percpu_and_gs(...)` (`wrmsr(0xC000_0101)` + `0xC000_0102` + `gs:[0]`/`gs:[8]`/`gs:[24]` geri-okuma) çalıştırır. `register_cpu_identity_and_stacks` atomik bitmaske (`CLAIMED_CPU_MASK`, `AcqRel`) ve kilitli aralık tablosuyla (`STACK_REGISTRY`) yinelenen `cpu_idx`, yinelenen `lapic_id` ve kesişen `boot_stack`/`df_ist1` aralıklarını reddeder; her CPU canlı `int3` (`#BP`) tuzağını doğrulayıp `ONLINE_COUNT.fetch_add(1, Release)` ile hazır olduğunu bildirir.
5. **Faz 5 — Sınırlı Zaman Aşımı ve Simetrik Rendezvous (`Kontrolör Çekirdek Yok`):** BSP, `ONLINE_COUNT == cpu_count` koşulunu hem TSC (`3.0e9` çevrim) hem iterasyon (`2.0e8` spin) tavanıyla bekler; başka bir CPU paniklemişse `PANIC_OWNER` kilidi sayesinde sahte zaman aşımı basmadan `hlt` durumuna geçer. Tüm CPU'lar hazır olduğunda `RENDEZVOUS_GATE.store(true, Release)` açılır ve **BSP dahil tüm CPU'lar simetrik olarak** `4.096` tur boyunca `gs:[8] == cpu_idx` doğrulaması + `PerCpuData::exec_ticks` + `GLOBAL_WORK_COUNTER` (`AcqRel`) artışı yürütür.

### C.2. Per-CPU `TSS` Ömrü ve `#DF` (`IST1`) Sahiplik İncelemesi (`kernel/src/gdt_tss.rs`, `idt.rs`)

- **`TSS` Ömrü ve Donanım `Busy` Biti (`0x89 -> 0x8B`):** `ltr 0x28` buyruğu bellekteki GDT TSS tanımlayıcısının erişim baytını `0x89` (*Available*) değerinden `0x8B` (*Busy*) değerine çevirir ve `&CPU_DESCS[cpu_idx].tss` adresini çekirdeğin gizli `TR.base` yazmacına önbellekler. Donanım her istisna/kesme geçişinde doğrudan bu adresi okuduğu için:
  1. GDT ve TSS kesinlikle CPU başına özeldir (paylaşılan TSS ikinci çekirdeğin `ltr` çağrısında `#GP(0x28)` üretir ve `RSP0`/`IST1` yığınlarını birbirine karıştırır).
  2. `CPU_DESCS[MAX_CPUS]` dizisi geri kazanılamaz `'static` çekirdek `.bss` (`SecDomain::Kernel`) içinde yaşar; yığın üzerinde veya `BOOTLOADER_RECLAIMABLE` bellekte tutulamaz. Her CPU `ltr` sonrasında ve iş yükü sonunda `access_byte == 0x8b` ve `str == 0x28` kontrolünü doğrular.
- **`#DF` (`IST1`) Münhasır Sahipliği ve Tekrar-Girişsizlik (*Non-Reentrancy*):** IDT vektör `8` (`#DF`) `IST = 1` kullandığında donanım koşulsuz olarak `RSP = TSS.IST[0]` yükler. `IST1`'in `#PF`, `#GP`, `#BP` veya kesmelerle paylaşılması, iç içe bir hatada aktif `TrapFrame`'in ezilmesine yol açar. Bu nedenle `IST1` **yalnızca** ölümcül `#DF` (vektör `8`) kapısına atanmıştır (`vec != 8 => IST = 0`), taban kanaryası (`DF_CANARY_MAGIC ^ cpu_idx`) ve CPU-yerel `in_df_handler: AtomicBool` tekrar-giriş kilidiyle korunmuştur.

### C.3. Ölçülen QEMU KVM UEFI Test Sonuçları (`11/11` Senaryo)

| Senaryo | vCPU (`-smp`) | Cmdline / Enjekte Hata | Beklenen / Gerçek Çıkış | Doğrulanan Kanıt (`COM1` Seri Çıktısı) | Süre |
| :--- | :---: | :--- | :---: | :--- | :---: |
| `boot_1cpu_ok` | `1` | `console=com1` | `33` / **`33`** | `cpus_online=1 mask=0x1 total_bp_traps=1 symmetric_ops=4096 stacks_disjoint=true` | `1.240 s` |
| `boot_2cpu_ok` | `2` | `console=com1` | `33` / **`33`** | `cpus_online=2 mask=0x3 total_bp_traps=2 symmetric_ops=8192 stacks_disjoint=true` | `1.349 s` |
| `boot_4cpu_ok` | `4` | `console=com1` | `33` / **`33`** | `cpus_online=4 mask=0xf total_bp_traps=4 symmetric_ops=16384 stacks_disjoint=true` | `1.477 s` |
| `boot_20cpu_ok` | `20` | `console=com1` | `33` / **`33`** | `cpus_online=20 mask=0xfffff total_bp_traps=20 symmetric_ops=81920 stacks_disjoint=true` | `1.837 s` |
| `smp_ap_timeout` | `2` | `smp_fault=ap_timeout` | `35` / **`35`** | `[DRY-SMP-FAIL] AP timeout: online=1/2 online_mask=0x1 missing_mask=0x2` + `[DRY-PANIC] cpu=0` | `2.380 s` |
| `smp_dup_cpu_id` | `2` | `smp_fault=dup_cpu_id` | `35` / **`35`** | `[DRY-SMP-FAIL] Duplicate CPU ID detected: cpu_idx=0 lapic_id=1` + `[DRY-PANIC] cpu=1` | `1.415 s` |
| `smp_stack_overlap` | `2` | `smp_fault=stack_overlap` | `35` / **`35`** | `[DRY-SMP-FAIL] Stack/LAPIC conflict (stack_overlap) between cpu=1 and cpu=0` + `[DRY-PANIC] cpu=1` | `1.344 s` |
| `smp_df_ist1_ap` | `2` | `smp_fault=df_ist1_ap` | `35` / **`35`** | `#PF->#DF` AP 1 üzerinde `fault_rsp=0x7fffdead0008 -> IST1 rsp=0xffffffff80016110 [0xffffffff80012320, 0xffffffff80016320]` | `1.312 s` |
| `panic_handler_deterministic` | `1` | `panic_self_test=1` | `35` / **`35`** | `[DRY-PANIC] cpu=0 src/main.rs:139:9` | `1.299 s` |
| `exception_ud2_caught` | `1` | `fault_self_test=ud2` | `35` / **`35`** | `[DRY-EXCEPTION] cpu=0 vec=6 (#UD Invalid-Opcode)` + `[DRY-PANIC] cpu=0` | `1.239 s` |
| `exception_pf_caught` | `1` | `fault_self_test=pf` | `35` / **`35`** | `[DRY-EXCEPTION] cpu=0 vec=14 (#PF Page-Fault) ... cr2=0x7fffdeadbee0` | `1.291 s` |

### C.4. Fiziksel Donanım Sınırı ve Geliştirme Sırasında Yakalanan Hatalar (Gizlenen Hata = `0`)

- **20-vCPU Testinin Fiziksel Performans Sınırı (Zorunlu Kayıt):** Test konağı 4 fiziksel çekirdek / 8 donanım iş parçacıklı (*Tiger Lake*) bir işlemcidir. `-smp 20` koşusu KVM zaman-dilimlemeli *vCPU oversubscription* (`2.5x`) altında çalışır; **fonksiyonel SMP eşzamanlılığını, bellek bariyeri doğruluğunu ve 20 çekirdekli yığın/GDT/TSS/GS izolasyonunu kanıtlar**, ancak **fiziksel 20 çekirdekli donanım önbellek tutarlılığı gecikmesini veya doğrusal ölçeklenme performansını kanıtlamaz**.
- **Derleme/Lint Sırasında Yakalanıp Düzeltilen 2 Hata (`0` QEMU Başarısızlığı):**
  1. `kernel/src/main.rs:199`: `enter_fatal_or_halt()` içinde kullanılan `if let Err(owner) = ... && owner != token` ifadesi `edition = "2021"` altında derleyici tarafından reddedildi (`let chains are only allowed in Rust 2024 or later`) -> `matches!(..., Err(owner) if owner != token)` olarak düzeltildi.
  2. `kernel/src/smp.rs:373`: `cpu_count >= 1 && cpu_count <= MAX_CPUS` ifadesi `cargo clippy -D warnings` altında `clippy::manual_range_contains` uyarısı verdi -> `(1..=MAX_CPUS).contains(&cpu_count)` olarak düzeltildi.
  3. Bu iki statik düzeltme sonrasında `1`, `2`, `4`, `20` vCPU ve tüm negatif testler ilk QEMU koşularında `0` yeniden deneme ile geçti.

---

## Bölüm D — Faz 1 `Milestone 1A-F` (Aşama 1): İşbirlikçi (`Cooperative`) `H1` Zamanlayıcı Entegrasyonu

- **Tarih:** 11 Kasım 2026
- **Kapsam:** `dry-kernel` içinde yalnızca işbirlikçi (*cooperative*) çekirdek iş parçacığı (*kernel-thread*) zamanlaması: `AtomicU32(exec_state, park_token, gen26)` `H1` protokolü, lock-free `PerCpuRunQueue` + CPU'lar arası *work stealing*, bağımsız 16 KiB çekirdek yığınları ve `Dead` sonrası yığın/TCB geri dönüşümü, x86-64 assembly `dry_switch_to` + `dry_thread_entry_trampoline`, `post_switch_*` el değiştirme kuralları, kademeli `1 -> 2 -> 4 -> 20 vCPU` doğrulaması ve 4 `H1` hata sınıfı denetimi.
- **Tekrarlanabilir Komut:** `python3 tools/qemu_boot_test.py` (`15/15` senaryo; tekil koşu için `--smp-only {1|2|4|20}`).

### D.1. x86-64 Assembly Bağlam Değişimi (`dry_switch_to`) ve Giriş Trampolini Sözleşmesi (`kernel/src/sched/context.rs`)

1. **Yığın Hizalaması (`16`-Bayt System V AMD64 ABI & `-mno-red-zone`):**
   - `x86_64-unknown-none` hedefi 128-bayt *red-zone* ve çekirdek içi SSE/FPU yazmaç kullanımını devre dışı bırakır (`disable-redzone: true`, `+soft-float,-mmx,-sse`).
   - Çağıran fonksiyon `RSP % 16 == 0` iken `call dry_switch_to` çalıştırır (`RSP % 16 == 8`); `dry_switch_to` 7 adet 64-bit yazmacı (`56` bayt: `RFLAGS, RBP, RBX, R12, R13, R14, R15`) ittiğinde kaydedilen `saved_rsp % 16 == 0` olur.
   - `mov rsp, rsi` (`% 16 == 0`) ile hedef yığına geçilip 7 yazmaç çekildiğinde (`+56` bayt) `RSP % 16 == 8` durumundadır. `sub rsp, 8` ile `RSP % 16 == 0` hizalanarak `call finish_post_switch` çağrılır (System V kuralı uyarınca Rust fonksiyonuna girişte `RSP % 16 == 8`); dönüşte `add rsp, 8; ret` yürütülür.
   - Yeni oluşturulan iş parçacıklarında `init_new_thread_stack`, `[stack_top - 64]` adresine 64-baytlık başlangıç çerçevesi yazar (`[stack_top - 8] = dry_thread_entry_trampoline`). İlk `ret` sonrası `dry_thread_entry_trampoline` tam `RSP == stack_top` (`% 16 == 0`) ve `RBP == 0` ile başlar; `call thread_bootstrap` Rust girişine `RSP == stack_top - 8` (`% 16 == 8`) ile girer.
2. **Callee-Saved Yazmaçlar ve `TSS.RSP0` Senkronizasyonu:**
   - `RBX`, `RBP`, `R12`, `R13`, `R14`, `R15` giden yığında korunur; yeni thread'lerde `R12 = entry_fn`, `R13 = entry_arg`, `R14 = slot_idx`, `RBP = 0` tohumlanır.
   - Her `switch_to_slot` geçişinde `set_cpu_tss_rsp0(cpu_idx, next_top)` ile CPU'nun özel `TSS.RSP0` alanı aktif çekirdek yığınının tavanına güncellenir (`#DF` `IST1` yığını münhasır kalır).
3. **`RFLAGS` ve Kesme Durumu Sözleşmesi (`Aşama 1`):**
   - `pushfq` / `popfq` + `cld` ile bayraklar korunur ve `DF = 0` zorlanır.
   - Bu ilk aşamada yalnızca işbirlikçi zamanlama uygulandığından tüm çekirdek iş parçacıklarında `RFLAGS.IF == 0` (`CLI`, başlangıç değeri `0x0002`) değişmezdir; `Local APIC` zamanlayıcısı ve kesme güdümlü *preemption* **henüz etkinleştirilmemiştir**.
4. **Açık Mimari Sınır (`CR3` / TLB ve Adres Alanı Değişimi Yok):**
   - Tüm `Milestone 1A-F` çekirdek iş parçacıkları tekil çekirdek `VSpace` (`CR3`, `SecDomain::Kernel`) içinde çalışır; `dry_switch_to` `CR3` yazmacını değiştirmez ve TLB shootdown üretmez (`VSpace` + `active_cpus` TLB shootdown `Milestone 1B` kapsamındadır).

### D.2. Dört `H1` Hata Sınıfının Çekirdek Üzerinde Önlenmesi ve Negatif Doğrulaması

| `H1` Hata Sınıfı | Üretim Önleme Mekanizması (`sched/{state.rs,context.rs,mod.rs}`) | Negatif QEMU Testi (`-smp 2`) ve Yakalanan Kanıt (`exit=35`) |
| :--- | :--- | :--- |
| **1. Stack Ownership Violation** | `finish_post_switch` yalnızca `mov rsp, rsi` ile giden yığından **tamamen çıkıldıktan sonra** çalışır, donanım `RSP`'sinin giden `[prev_base, prev_top]` dışında olduğunu doğrular ve ancak ondan sonra `stack_owner.swap(NO_CPU, Release)` yapar. | `sched_stack_owner_conflict` (`sched_fault=stack_owner`): `[DRY-SCHED-FAIL] H1 Bug1 stack_owner violation on dispatch: slot=64 claimed by cpu=1 while owned by cpu=0` (`1.460 s`) |
| **2. Orphaned Runnable Thread** | `post_switch_yield`, `post_switch_park` ve `finish_waking_and_push` önce `AcqRel` CAS ile `ExecState::Ready` yazar, hemen ardından kesmeler kapalıyken `PerCpuRunQueue::push` çağırır; durgunluk denetçisi tüm TCB'leri tarar. | `sched_orphan_ready_detected` (`sched_fault=orphan_ready`): `[DRY-SCHED-FAIL] H1 Bug2 orphaned Ready thread detected: slot=64 gen=2 missing from all PerCpuRunQueues` (`1.387 s`) |
| **3. Lost Wakeup** | `prepare_park` ve `Notification::signal_raw` açık `fence(Ordering::SeqCst)` uygular; `commit_park` `Notified` jetonunu yerinde tüketir; `post_switch_park` `SwPark` sırasında gelen `Notified` jetonunu `Blocked` yerine doğrudan `Ready + push` yapar. | `sched_lost_wakeup_unarmed` (`sched_fault=lost_wakeup`): `[DRY-SCHED-FAIL] H1 Bug3 invalid park protocol state on slot=64: state=Running token=None` (`1.420 s`) |
| **4. Exiting Thread Resurrection** | `AtomicU32` üst 26 bitinde `gen26` taşınır (`ThreadId = (gen26 << 8) \| slot`); `Exiting`/`Dead` görevler `wakeup` çağrılarını `WakeOutcome::Terminated` ile reddeder; yığın yeniden tahsisi `Exiting -> Dead` CAS sonrasında `gen26 + 1` ile yapılır. | `sched_exit_resurrection` (`sched_fault=exit_resurrect`): `[DRY-SCHED-FAIL] H1 Bug4 exiting/dead thread resurrection rejected: slot=64 ticket_gen=2 cur_gen=2 state=Dead` (`1.398 s`) |

### D.3. Kademeli Pozitif QEMU KVM Sonuçları (`1 -> 2 -> 4 -> 20 vCPU`) ve Geliştirme Kayıtları

| Senaryo | `-smp` | Spawn / Exit | Yeniden Kullanılan Yığın (`36` Havuzda) | Assembly `ctx_switches` | CPU Göçü (`migrations`) | `park/wake` Turu | Tamamlanan İş (`work_units`) | Reddedilen Bayat Wake | Süre / Çıkış |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` | `64 / 64` | `28` | `1.088` | `0` | `128` | `896` | `48` | `1.223 s` / **`33`** |
| `boot_2cpu_ok` | `2` | `64 / 64` | `28` | `1.154` | `808` | `128` | `896` | `48` | `1.375 s` / **`33`** |
| `boot_4cpu_ok` | `4` | `64 / 64` | `28` | `1.249` | `669` | `128` | `896` | `48` | `1.374 s` / **`33`** |
| `boot_20cpu_ok` | `20` | `96 / 96` | `65` | `3.507` | `497` | `384` | `1.920` | `48` | `1.865 s` / **`33`** |

- **Doğrulanan Ek Değişmezler:** Her testte `current_thread_id()` (`slot + gen26`), `gs:[8]` (`cpu_idx`), callee-saved yerel değişken bütünlüğü, `sum(percpu.ctx_switches) == GLOBAL_CTX_SWITCHES` (tüm aktif vCPU'larda `ctx_switches > 0`), `orphan_ready == 0` ve tüm `boot_stack` / `df_ist1` / `36` worker yığın taban kanaryaları doğrulandı (*Not: 4C/8T konakta `-smp 20` fiziksel 20 çekirdek performans kanıtı değildir*).
- **Gizlenmeyen Derleme / Düzenleme Kayıtları (`0` QEMU Başarısızlığı):**
  1. `smp.rs` düzenlemesinde tek satırlık `halt_forever()` hedefi dosyada 4 yerde eşleştiği için 3 istenmeyen eşleşme anında geri alındı.
  2. İlk `cargo clippy -D warnings` kontrolünde yakalanan 7 uyarı (`unused_imports` x2, `current_thread_id` kullanılmaması -> `yield_worker` kimlik doğrulamasına bağlandı, `needless_range_loop` x2, `manual_range_contains` x2) düzeltildi; tüm `15/15` QEMU senaryosu ilk koşuda geçti.


---

## Bölüm E — Faz 1 `Milestone 1A-G`: Preemptive `H1` Zamanlayıcı ve `Local APIC`/`x2APIC` Kalibrasyonu

- **Tarih:** 11 Kasım 2026
- **Kapsam:** `b6ad2e9` (`Milestone 1A-F` işbirlikçi `H1` zamanlayıcı ve `15` regresyon testi) korunarak; `CPUID` + `IA32_APIC_BASE` (`0x1B`) donanım yetenek tespiti ve 3-kademeli geri-düşüş (*fallback*) politikası, dinamik i8254 PIT Kanal 2 zamanlayıcı kalibrasyonu, kesme/iş-parçacığı yığın sahipliği protokolü (`IST = 0`, `EOI`-önce-switch, `CLI` `finish_post_switch`, One-Shot yeniden kurma), `preempt_count` yuvalanması + `need_resched`, CPU'yu bırakmayan (*non-yielding*) iş parçacıklarının zorlayıcı (*involuntary*) kesilmesi, `IRQ` vs `yield`/`park`/`wake`/`exit` yarışları (`1 -> 2 -> 4 -> 20 vCPU`) ve 5 negatif preemptive arıza testi.
- **Tekrarlanabilir Komutlar:**
  - Yalnızca Aşama A (`IRQ` teslimi + `EOI` + `TrapFrame` bütünlüğü): `python3 tools/qemu_boot_test.py --smp-only {1|2|4|20} --irq-only`
  - Tam paket (`21/21` senaryo): `python3 tools/qemu_boot_test.py`

### E.1. Donanım Yetenek Tespiti, Geri-Düşüş Politikası ve Kesme/Yığın Sözleşmesi (`apic.rs`, `idt.rs`, `sched/{context.rs,preempt.rs}`)

1. **3-Kademeli Donanım Geri-Düşüş Politikası ve Eski `8259 PIC` Maskelemesi:**
   - `STI` öncesinde eski çift `8259A PIC` (`0x20/0x21`, `0xA0/0xA1`) `0x20..0x2F` vektörlerine yeniden eşlenip tüm hatları `0xFF` ile maskelenir; olası sahte donanım `IRQ7`/`IRQ15` ve APIC `0xFF` vektörleri zararsız sayaçlarla karşılanır (`spurious_irqs=0`).
   - **Kademe 1 (Birincil — `x2APIC` MSR):** `CPUID.01H:EDX[9] == 1` ve `ECX[21] == 1` ise her çekirdekte `IA32_APIC_BASE[11:10] = 0b11` (`EN | EXTD`) etkinleştirilir; seri serileştirme veya MMIO sayfa tablosu gerektirmeden `0x800..0x83F` MSR yüzeyi kullanılır.
   - **Kademe 2 (Yedek — `xAPIC` Uncached MMIO):** `x2APIC` yoksa veya `apic_mode=xapic` verilmişse `IA32_APIC_BASE & 0xFFFF_F000` (`0xFEE0_0000`) adresi HHDM içinde önbelleksiz (`PCD | PWT | RW | NX | P`) 4 KiB PTE ile eşlenir ve `boot_2cpu_xapic_fallback` senaryosuyla uçtan uca doğrulanır (`mode=xapic-mmio`).
   - **Kademe 3 (Deterministik Red):** `CPUID.01H:EDX[9] == 0` ise sahte zamanlayıcı uydurulmaz; `[DRY-APIC-FAIL]` ile deterministik panik (`exit=35`) üretilir.
2. **Dinamik Zamanlayıcı Ölçümü ve Sanallaştırma Sınırı (Sabit Frekans İddiası Yok):**
   - QEMU/KVM (`-cpu host`) altında `CPUID.15H` ve `CPUID.16H` yapraklarının `0` döndüğü deneysel olarak doğrulandı (`cpuid_crystal_hz=0 cpuid_base_mhz=0`).
   - BSP, `LVT_TIMER` maskeli ve bölücü `div=16` iken donanım **i8254 PIT Kanal 2** (`1.193182 MHz`, Kip 0, `5.966` darbe = `5,0008 ms`) referansına karşı 3 bağımsız pencere ölçer. PIT `OUT2` erken (`< 5 ms`) yükselemediği ve konak vCPU kesilmeleri yalnızca terminal sayım sonrası okuma gecikmesi eklediği için 3 örneğin minimumu seçilir (`ticks_per_ms ≈ 62.500..62.543` -> `~1,000 GHz` KVM APIC veri yolu saati; `tsc_mhz ≈ 3.110..3.112 MHz`; `QUANTUM_TICKS = 2` -> `~2.000 µs`).
   - **Açık Ölçüm Sınırı:** Ölçülen frekans sanallaştırılmış KVM zamanlayıcısına aittir; konak zamanlayıcı *jitter*'ına tabidir ve 4C/8T konakta `-smp 20` koşusu çıplak metal mikrosaniye-altı zamanlama doğruluğunu veya fiziksel 20 çekirdek performansını **kanıtlamaz**.
3. **Kesme Bağlamı ile Thread Bağlamı Arasındaki Yığın Sahipliği ve Bloklamasız ISR Kuralları:**
   - `VEC_LAPIC_TIMER` (`0xEF`) `Interrupt Gate` (`RFLAGS.IF = 0` girişte otomatik temizlenir) ve **`IST = 0`** kullanır (`IST1` yalnızca `#DF` vektör `8` için münhasırdır). Donanım `TrapFrame` (`176 B`) + `dry_switch_to` `SwitchFrame` (`56 B`), o anda `stack_owner == cpu_idx` olan iş parçacığının özel 16 KiB çekirdek yığınına itilir; ISR içinde yığın sınırları ve taban kanaryası doğrulanır.
   - `lapic_eoi()` çağrısı bağlam değişiminden **kesinlikle önce** yapılır (`ISR[0xEF] == 0` her faz sonunda donanımdan geri okunur); aksi hâlde kesilen thread uykudayken yeni thread o çekirdekte bir daha zamanlayıcı kesmesi alamaz.
   - `dry_switch_to`, `pushfq; cli` ile bayrakları kaydeder, `mov rsp, rsi` sonrası `finish_post_switch` çağrısını **`RFLAGS.IF == 0` iken** (`popfq` öncesinde) yürütür ve hedef göreve dönmeden hemen önce One-Shot APIC sayacını yeniden kurarak sıfır-buyruk kesme kilitlenmesini (*zero-instruction IRQ livelock*) önler.
   - `on_lapic_timer_irq` içinde **0 bloklayıcı kilit, 0 bellek tahsisi ve 0 uyuyabilen işlem** bulunur (`irq_depth > 0` iken `yield_now`/`prepare_park`/`commit_park`/`exit` çağrıları çalışma zamanında reddedilir). Aynı CPU üzerinde Vyukov MPMC halkasının kesme tekrar-girişinden (*IRQ reentrancy*) etkilenmemesi için tüm `StaticRunQueue::{push,pop}` çağrıları `RFLAGS.IF == 0` şartını çalışma zamanında zorlar.

### E.2. Kademeli Doğrulama Sonuçları (`1 -> 2 -> 4 -> 20 vCPU`, `xAPIC` Yedek ve 5 Negatif Test — `21/21` Geçti)

| Senaryo | `-smp` | APIC Kipi & Kalibrasyon (`ticks/ms`) | `IRQ-Only` (`total / min_cpu`) | Preemptive `timer_irqs / preempts / deferred` | Zorla Kesilen Spinner (`threads / preempts`) | Karma Yarış (`spawn=exit / recycle / sw / mig / park`) | Çıkış / Süre |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` | `x2apic-msr` (`62.522`) | `10 / 10` | `172 / 80 / 1` | `4 / 4` | `60 / 24 / 452 / 0 / 72` | **`33`** / `1.580 s` |
| `boot_2cpu_ok` | `2` | `x2apic-msr` (`62.539`) | `20 / 10` | `172 / 75 / 1` | `4 / 4` | `60 / 24 / 447 / 243 / 72` | **`33`** / `1.580 s` |
| `boot_4cpu_ok` | `4` | `x2apic-msr` (`62.543`) | `40 / 10` | `99 / 28 / 1` | `8 / 8` | `64 / 28 / 446 / 100 / 96` | **`33`** / `1.598 s` |
| `boot_20cpu_ok` | `20` | `x2apic-msr` (`62.501`) | `200 / 10` | `482 / 66 / 1` | `32 / 65` | `68 / 32 / 547 / 143 / 120` | **`33`** / `1.976 s` |
| `boot_2cpu_xapic_fallback` | `2` | `xapic-mmio` (`62.340`) | `20 / 10` | `172 / 75 / 1` | `4 / 4` | `60 / 24 / 447 / 247 / 72` | **`33`** / `1.411 s` |

- **5 Negatif Preemptive Arıza Testi (`-smp 2`, tamamı `exit=35` ile deterministik yakalandı):**
  1. `preempt_irq_storm` (`preempt_fault=irq_storm`): 1-tick (`~16 ns`) zamanlayıcı fırtınasında `[DRY-PREEMPT-FAIL] Timer interrupt storm / zero-progress livelock detected on cpu=0 slot=0 streak=256` (`1.298 s`).
  2. `preempt_nested_irq` (`preempt_fault=nested_irq`): ISR içinde erken `STI + int 0xef` enjeksiyonunda `[DRY-PREEMPT-FAIL] Nested timer interrupt violation (irq_depth=2) on cpu=1` (`1.270 s`).
  3. `preempt_lost_wakeup` (`preempt_fault=lost_wakeup`): Silahsızlandırılmış `ParkToken::None` ile `commit_park` çağrısında `[DRY-PREEMPT-FAIL] Preemptive lost-wakeup protocol violation on slot=64: state=Running token=None` (`1.272 s`).
  4. `preempt_double_schedule` (`preempt_fault=double_sched`): Halihazırda kuyrukta olan görev (`in_flight_refs=1`) ikinci kez kuyruklanmak istendiğinde `[DRY-PREEMPT-FAIL] Double scheduling / duplicate runqueue enqueue detected on slot=64 (in_flight_refs=2)` (`1.258 s`).
  5. `preempt_stack_reuse` (`preempt_fault=stack_reuse`): Aktif `stack_owner=0` tutulan görev yığını erken yeniden tahsis edildiğinde `[DRY-PREEMPT-FAIL] Premature stack reuse while stack_owner active on slot=64 cpu=0` (`1.254 s`).
- **Korunan Regresyon Paketi:** `Milestone 1A-D/E/F` kapsamındaki tüm `11` negatif istisna/SMP/işbirlikçi-H1 testi ve `4` işbirlikçi pozitif aşama (`[DRY-SCHED] PASS:`) tek bir değişiklik veya regresyon olmadan `21/21` koşuda geçti.

### E.3. Geliştirme Sırasında Yakalanıp Çözülen Gerçek Hatalar (Gizlenen Hata = `0`)

1. **Derleme / Clippy Düzeltmeleri:** `ApicCalibrationSummary` alan kısaltması (`has_arat`/`has_inv_tsc`) ve Rust `1.99.1`'de güvenli olan `__cpuid` çevresindeki gereksiz `unsafe` bloğu (`-D unused-unsafe`) düzeltildi (`0` uyarı).
2. **`init_scheduler` Çoklu-Faz Yığın Kanaryası Nesil (`gen26`) Uyumsuzluğu (`1-vCPU` Koşusunda Yakalandı):** İşbirlikçi `1A-F` fazından sonra ikinci kez çağrılan `init_scheduler()`, yığın tabanına mevcut `cur_gen` (`5`) yerine sabit `gen=1` yazdığı için kullanılmayan `slot=66` kanarya denetiminde panik üretti -> `w.sched.snapshot()` üzerinden gerçek `cur_gen` damgalanarak giderildi.
3. **Vyukov MPMC `StaticRunQueue` Aynı-CPU Kesme Tekrar-Girişi ve `-smp 20` `G4` Spawner Kilitlenmesi:**
   - İlk `-smp 20` koşusunda zaman aşımı gözlendi. İncelemede (a) `wake_thread()` çağrısının `RFLAGS.IF == 1` ile `StaticRunQueue::push` çağırdığı — aynı CPU'da `enqueue_pos` CAS ile `cell.seq` yazımı arasına giren bir zamanlayıcı kesmesinin üreticiyi kendi tamamlanmamış hücresinin arkasına kuyruklayabileceği — tespit edilip `wake_thread` `irq_save_cli()` altına alındı ve `push`/`pop` içine `IF == 0` doğrulaması eklendi; (b) CPU 0 zamanlayıcı ISR'ına eklenen `EPOCH_WATCHDOG_TSC` TCB dökümüyle, `G4` fazında 36 yuvanın dolduğu anda BSP'nin `spawn_preemptible_with_assist` içinde `slot=65` (`mixed_bg_spinner_worker`) görevini devraldığı, diğer 19 AP'nin kalan görevleri bitirip kuyrukları boşalttığı (`live=4 <= 20`) ve `slot=65` kesilemediği için BSP'nin kalan 32 görevi üretemediği kesin olarak kanıtlandı -> `>= 2` vCPU'da AP'ler yuvaları boşaltırken BSP üretici rolünde tutuldu ve `mixed_bg_spinner_worker` canlı görev sayısına (`live <= online`) bağlandı; art arda 4 `-smp 20` koşusu `1.84s..2.15s` içinde deterministik geçti.
4. **Idle Bağlamında (`slot=0`) 1-Tick (`16 ns`) Zamanlayıcı Fırtınası Canlı-Kilitlenmesi (`preempt_irq_storm` Negatif Testinde Yakalandı):** İlk tam paket koşusunda `preempt_irq_storm` testi, `STI` açılır açılmaz `slot=0` (idle dispatcher) üzerinde `1.965.401` kesme üreterek tek bir worker bile çalıştırmadan `EPOCH_WATCHDOG_TSC`'ye takıldı; çünkü fırtına bekçisi yalnızca `cur_slot >= MAX_CPUS` görevlerini izliyordu -> sıfır-ilerleme (`storm_streak >= 256`) denetimi aktif `idle_dispatch_once` döngüsünü de kapsayacak şekilde genişletildi ve fırtına `1.298 s` içinde deterministik olarak yakalandı.

---

## Bölüm F — Faz 1 `Milestone 1B-A`: Physical Memory Manager, `Untyped` Capability Temeli ve `20`-vCPU Liveness Analizi

- **Tarih:** 11 Kasım 2026
- **Kapsam:** `fad423b` (`Milestone 1A-G` Preemptive `H1` zamanlayıcı ve `21` regresyon testi) korunarak; Limine Base Rev 3 fiziksel bellek envanteri, tek yönlü `BootstrapAllocator` (`Uninit -> Active -> Sealed`) ile kalıcı `PMM` yaşam döngüsü ayrımı, `16 B` `FrameMeta` (`%0.39` ek yük) + genişletilebilir `PageOrder` (`Order4KiB = 0`, `Order2MiB = 9`), deterministik çift-tahsis / `double-free` / `UAF` / sahiplik-pin yakalama, `ADR-007` zorunlu alanlar/kompartmanlar arası `rep stosq` sıfırlama + `VerifiedZero`/aynı-kompartman yazım elemesi, tam `CSpace` radiks tablosu eklemeden `Untyped` `split / retype / revoke` sözleşmesi, 6 donanım/KVM performans ve parçalanma ölçümü (`1, 2, 4, 20 vCPU`), BSP-pinsiz `T_spawner` aşırı-yükleme (*oversubscription*) liveness regresyonu ve 5 negatif `pmm_*` arıza testi.
- **Tekrarlanabilir Komutlar:**
  - Tekil SMP doğrulaması: `python3 tools/qemu_boot_test.py --smp-only {1|2|4|20}`
  - Tam paket (`26/26` senaryo): `python3 tools/qemu_boot_test.py`

### F.1. Mimari Tasarım, Ponytail Maliyet Analizi ve `ADR-007` / `Untyped` Sözleşmesi (`pmm/{meta.rs,mod.rs,untyped.rs}`)

1. **Fiziksel Bellek Envanteri ve Tek Yönlü `BootstrapAllocator` -> `PMM` Geçişi:**
   - Limine bellek haritasındaki tüm türler (`USABLE`, `BOOTLOADER_RECLAIMABLE`, `EXECUTABLE_AND_MODULES`, `RESERVED`, `ACPI_RECLAIMABLE`, `ACPI_NVS`, `BAD_MEMORY`, `FRAMEBUFFER`) ayrıştırılır ve `free_init + reclaimable + kernel_meta + reserved_low == max_tracked_pfn` eşitliğiyle `%100` PFN korunumu çalışma zamanında doğrulanır.
   - `< 64 KiB` düşük BIOS/AP gerçek-kip SIPI bölgesi (`PFN 0..16`, hem `USABLE` hem `BOOTLOADER_RECLAIMABLE` üzerinde) ve `0xFEE0_0000` Local APIC MMIO adresi kalıcı olarak `FrameState::ReservedOrMmio` mühürlenir; aktif Limine `CR3` sayfa tabloları `Milestone 1B-B` kendi çekirdek `VSpace` geçişini tamamlayana dek `FrameState::BootloaderReclaimable` olarak kilitli tutulur.
   - `BootstrapAllocator` yalnızca tek iş parçacıklı BSP başlangıcında `FrameMeta` dizisini oymak için `Active` olur ve `PMM` serbest listesi yayınlanmadan önce geri dönüşsüz biçimde `BootstrapState::Sealed` durumuna geçer (`bootstrap_alloc_pages` sonrasında deterministik `Err(BootstrapSealed)` döner).
2. **Önbellek-Dostu `16 B` `FrameMeta` ve `CLI` Per-CPU Magazine (Ponytail Sadeleştirmesi):**
   - Her 4 KiB PFN için hizalı `16 B` (`word0: AtomicU64` + `word1: AtomicU64`, sabit **`%0.390625` = `39 bps`** RAM ek yükü; `256 MiB` RAM için `1.020 KiB`) tutulur. Serbest liste bağlantısı (`next_pfn`), veri sayfasının kendisine dokunmadan doğrudan `FrameMeta.word1` içinde taşınır; böylece tahsis/iade sırasında soğuk 4 KiB veri sayfası önbellek satırı kirletilmez.
   - Spekülatif ikili buddy birleştiricisi veya kilitli ağaçlar yerine `CLI` ile korunan CPU-yerel `PerCpuFrameCache` (`MAGAZINE_CAP = 64`, `MAGAZINE_BATCH = 32`) seçildi: hızlı yol global kilide hiç dokunmaz (`%99` isabet), toplu dolum/boşaltım global kilit çekişmesini `32x` amortize eder ve `cld; rep stosq` sıfırlaması kesme gecikmesini sınırlamak için hem `GlobalFreePool` kilidinin hem de `CLI` bölgesinin **dışında** yürütülür.
3. **`ADR-007` Zorunlu Sıfırlama, Gereksiz Veri Hareketi Elemesi ve `Untyped` Yaşam Döngüsü:**
   - `Kernel <-> RootSystem <-> Userspace` ve farklı `Userspace` `compartment_id` geçişlerinde (`UNINIT_SAME_DOMAIN_OK` istense dahi) `apply_mandatory_zero_policy` zorunlu olarak `rep stosq` çalıştırır. Buna karşılık `scrub_and_free_frame` / `untyped_revoke` sonrası `ZeroState::VerifiedZero` olan veya aynı `(SecDomain, compartment_id)` içinde `UNINIT_SAME_DOMAIN_OK` ile yeniden kullanılan çerçevelerde `4 KiB` bellek yazımı tamamen elenir (`zero_elided_p50 = 151 cyc` vs sıfırlamalı `alloc_p50 = 424 cyc` -> **`2.81x` hızlanma**).
   - `Untyped` sözleşmesi (`carve_untyped`, `untyped_split`, `untyped_retype`, `untyped_revoke`, `release_untyped_to_pmm`) `KernelObjHeader` (`SecLabel`, `generation`, `iommu_pin_count`, `map_ref_count`) üzerine kuruldu: `split` ebeveyn `SecLabel`'ını miras alır (`ADR-007` Kural 4), `retype` (`Frame4KiB`, `PageTable4KiB`, `TcbObj`, `EndpointObj`, `NotificationObj`, `CNodeSlotBlock`) monotonik `watermark_bytes` ve sıfırlama uygular, `revoke` ise 2-geçişli atomik denetimle tek bir çerçevede dahi `iommu_pin_count > 0` veya `map_ref_count > 0` varsa hiçbir alt nesneyi bozmadan `Err(SecError::BusyPinned)` döner; pinler kalktığında tüm alt `Untyped` ve çerçeve `generation` sayaçlarını artırıp belleği sıfırlayarak geri kazanır. Tam `CSpace` radiks yetenek tablosu bilinçli olarak `Milestone 1B-C`'ye bırakılmıştır.

### F.2. 6 Zorunlu Donanım/KVM Ölçümü ve 5 Negatif Arıza Testi (`26/26` Geçti)

| Senaryo | `-smp` | Envanter (`free_init` / `meta_bps`) | `alloc` `p50/p95/p99` (`cyc`) | `free` `p50/p95/p99` (`cyc`) | `zero_4k` `p50/p95/p99` (`elided p50`) | Verim: Magazine vs Global (`hit%`) | Global Spin: Direct -> Mag | `2 MiB` Blok (`önce->orta->sonra` / `frag_bps`) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` | `49.794` / `39 bps` (`%0.39`) | `424 / 462 / 905` | `99 / 119 / 125` | `152 / 181 / 306` (`151`) | `29.96` vs `28.18 Mops/s` (`99%`) | `0 -> 0` | `93 -> 77 -> 93` (`437 bps` = `%4.37`) |
| `boot_2cpu_ok` | `2` | `49.737` / `39 bps` (`%0.39`) | `478 / 606 / 903` | `128 / 158 / 184` | `179 / 196 / 209` (`204`) | `29.11` vs `9.50 Mops/s` (`99%`) | `9.928 -> 0` | `93 -> 77 -> 93` (`426 bps` = `%4.26`) |
| `boot_2cpu_xapic` | `2` | `49.737` / `39 bps` (`%0.39`) | `426 / 463 / 919` | `102 / 120 / 128` | `155 / 162 / 169` (`152`) | `37.57` vs `10.71 Mops/s` (`99%`) | `16.433 -> 0` | `93 -> 77 -> 93` (`426 bps` = `%4.26`) |
| `boot_4cpu_ok` | `4` | `49.670` / `39 bps` (`%0.39`) | `448 / 492 / 826` | `115 / 136 / 143` | `158 / 166 / 174` (`177`) | **`29.97` vs `3.73 Mops/s`** (`99%`, `8.03x`) | **`146.788 -> 58`** (`2.531x`) | `93 -> 77 -> 93` (`413 bps` = `%4.13`) |
| `boot_20cpu_ok` | `20` | `48.995` / `39 bps` (`%0.39`) | `449 / 488 / 857` | `115 / 132 / 142` | `158 / 164 / 166` (`176`) | **`8.92` vs `0.98 Mops/s`** (`99%`, `9.10x`) | **`6.972.056 -> 829`** (`8.410x`) | `92 -> 76 -> 92` (`385 bps` = `%3.85`) |

- **Ölçüm Notu ve OOM Korunumu:** Tüm koşularda tam-RAM OOM testi (`48.995..49.794` çerçeve `Err(OutOfMemory)` sınırına kadar tahsis edilip geri verildi) **`leaked_frames=0`** ile geçti. Konak makine 4 fiziksel çekirdek / 8 izlek (`tsc_mhz ≈ 3.110..3.113 MHz`) olduğundan `-smp 20` verimi konak zamanlayıcı paylaşımını yansıtır ve **fiziksel 20 çekirdek performans kanıtı değildir** (izole `-smp 20` koşusunda spin düşüşü `8.018.945 -> 468 = 17.134x` olarak ölçüldü).
- **5 Negatif PMM ve `Untyped` Arıza Testi (`-smp 2`, tamamı `exit=35` ile deterministik yakalandı):**
  1. `pmm_double_free` (`pmm_fault=double_free`): `[DRY-PMM-FAIL] Double free detected: pfn=0x2f handle_gen=1 cur_gen=2 state=FreePerCpu` (`1.634 s`).
  2. `pmm_use_after_free` (`pmm_fault=uaf`): `[DRY-PMM-FAIL] Use-after-free / stale generation detected: pfn=0x2f handle_gen=1 cur_gen=2 state=AllocatedFrame` (`1.614 s`).
  3. `pmm_double_alloc` (`pmm_fault=double_alloc`): `[DRY-PMM-FAIL] Double allocation / corrupt free-state detected: pfn=0x2f state=AllocatedFrame expected=FreePerCpu cpu_tag=0x0 pins=0 maps=0` (`1.529 s`).
  4. `pmm_ownership_violation` (`pmm_fault=owner_mismatch`): `[DRY-PMM-FAIL] Frame ownership / domain / pin violation: pfn=0x2f owner=(RootSystem,1) caller=(Userspace,200) pins=0 maps=0` (`1.595 s`).
  5. `pmm_untyped_revoke` (`pmm_fault=untyped_revoke`): `[DRY-PMM-FAIL] Revoked Untyped / stale capability access rejected: slot=1 expected_gen=2` (`1.484 s`).

### F.3. `1A-G` `20`-vCPU Spawner Açlık Vakası Değerlendirmesi ve BSP-Pinsiz `T_spawner` Liveness Regresyonu

| Senaryo | `-smp` | CPU-Bound Rakip Spinner (`yield = 0`) | BSP-Pinsiz `T_spawner` Dalga / Üretim (`slot >= MAX_CPUS`) | Aktif TCB Aşırı Yüklemesi (`/ 36` Havuz) | `spawner_preempts` | `max_wave_wait_us` | Açlık (`starved`) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` | `3` | `4` dalga (`48 / 48` PMM worker) | `16` görev / `1` vCPU (`16x`) | `4` | `8.139 µs` | **`0`** |
| `boot_2cpu_ok` | `2` | `4` | `4` dalga (`48 / 48` PMM worker) | `17` görev / `2` vCPU (`8.5x`) | `8` | `13.070 µs` | **`0`** |
| `boot_4cpu_ok` | `4` | `6` | `4` dalga (`48 / 48` PMM worker) | `19` görev / `4` vCPU (`4.75x`) | `6` | `6.068 µs` | **`0`** |
| `boot_20cpu_ok` | `20` | `22` | `4` dalga (`48 / 48` PMM worker) | **`35 / 36` görev / `20` vCPU (`1.75x`)** | `4` (tekrar: `5`) | `8.387 µs` (`17.112 µs`) | **`0`** |

- **Mimari Değerlendirme (BSP Pinleme Neden Genel Çözüm Değildir?):**
  1. `Milestone 1A-G` `G4` testindeki kilitlenme genel `H1` worker zamanlayıcısından değil, test koşumunun üreticiyi `slot 0` (CPU 0 `Idle` TCB'si) üzerinde çalıştırmasından kaynaklanıyordu: `PostSwitchKind::FromIdle` giden `slot 0` yığınını serbest bırakır ancak `slot 0`'ı `PerCpuRunQueue`'ya **geri eklemez**; dolayısıyla `slot 0` yalnızca CPU 0 kuyruğu tamamen boşaldığında yeniden çalışabilir.
  2. Buna karşılık gerçek bir `H1` çekirdek görevi (`slot >= MAX_CPUS`) olarak başlatılan BSP-pinsiz `T_spawner`, zamanlayıcı kesmesiyle kesildiğinde `PostSwitchKind::Yield` yoluyla FIFO `PerCpuRunQueue` sonuna eklenir ve boşta kalan çekirdeklerce çalınır (*work stealing*). Bu sayede aynı öncelik (`prio = 1`) düzeyinde `O(N_runnable * quantum)` üst sınırı altında `0` `yield_now()` ve `0` BSP pinleme ile `20` vCPU üzerinde `35/36` yuva doluyken dahi `starved=0` ile tamamlandığı kanıtlandı.
  3. **Kalan Açık Adillik ve Kabul Sınırları:** `1A-G`'deki BSP pinleme yalnızca bir önyükleme test-koşumu yamasıdır; genel işletim sistemi adillik garantisi **değildir**. İlerleyen aşamalarda ele alınacak iki açık sınır: (a) **Çapraz-öncelik açlığı:** Katı öncelik dizisi (`prio 0` > `prio 1` > `prio 2`) yüksek öncelikli sonsuz döngü karşısında alt önceliği aç bırakabilir (Faz 3 `ADR-004` yaşlandırma / EEVDF bütçesi kapsamındadır); (b) **Paylaşımlı TCB havuzu doygunluğu (`Admission Control`):** Sabit `WORKER_POOL_CAP = 36` çekirdek havuzu tek bir kompartman tarafından doldurulabilir; `Milestone 1B-C`'de küresel TCB havuzu kaldırılıp her güvenlik alanının kendi `Untyped::retype(TcbObj)` kotasından görev üretmesi zorunlu kılınacaktır.

### F.4. Geliştirme Sırasında Yakalanıp Çözülen Gerçek Hatalar (Gizlenen Hata = `0`)

1. **Derleme Öncesi Öz-Denetim (`carve_contiguous_for_untyped` Bağlantı Kaydı ve Pragmatik Dosya Tavanı):** İlk yazım sonrasında `carve_contiguous_for_untyped` serbest liste ayıklama döngüsünün `prev_kept.word1` güncellerken önceki düğümün kendi `(last_comp, cpu_tag)` bitleri yerine `cur` düğümünün bitlerini yazdığı fark edilip derleme öncesinde düzeltildi; ayrıca tek dosyada `1.394` satıra ulaşan modül pragmatik `< 800` satır standardına uygun olarak `pmm/meta.rs` (`593` satır) ve `pmm/mod.rs` (`732` satır) biçiminde ikiye ayrıldı.
2. **İlk `cargo clippy -- -D warnings` Derlemesi (`5` Uyarı/Hata):** Kullanılmayan `FrameMeta` yeniden-aktarımı kaldırıldı; tanısal `FragReport.free_cached_frames`, `PmmPerfCounters.zero_runs`, `PmmInventory.hhdm_offset` alanları ve `PageOrder::frame_count` yöntemi `pmm/tests.rs` içindeki somut değişmez doğrulamalarına bağlandı (`0` uyarı).
3. **İlk `1-vCPU` Koşusu (`thread_bootstrap` `RFLAGS.IF` Uyumsuzluğu ve Parçalanma Sonda Hassasiyeti):** `[DRY-PMM] PASS` ve `[DRY-UNTYPED] PASS` geçtikten sonra `pmm::tests` içindeki SMP zamanlayıcı epoch'u `src/sched/context.rs:142` (`Bootstrap RFLAGS.IF mismatch on slot 64`) paniği üretti; çünkü `verify_preemptive_scheduler` çıkışta `set_preemptive_mode_active(false)` yapmıştı -> `pmm::tests` içindeki preemptive epoch'lar `set_preemptive_mode_active(true/false)` ile sarmalandı. Ayrıca 64 LIFO tahsisinin aynı 2 MiB-altı kuyrukta kalması nedeniyle `contig_2m_mid == 93` çıktığı görülüp orta-aşama parçalanma sondası 16 ayrı `2 MiB` bloğundan 1'er çerçeve oyacak (`93 -> 77 -> 93`) şekilde keskinleştirildi.
4. **İlk `2-vCPU` Koşusu (Limine SMP Gerçek-Kip SIPI Trampolini `< 64 KiB` Sınıflandırması):** `--smp-only 2` koşusunda `< 64 KiB` koruma testi `left: BootloaderReclaimable, right: ReservedOrMmio` paniği verdi. İncelemede Limine'in `smp=1` iken `< 64 KiB` bölgesini `LIMINE_MEMMAP_USABLE` olarak bıraktığı, ancak `smp >= 2` iken 16-bit AP SIPI uyandırma trampolinini `< 64 KiB` içine yerleştirip `LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE` olarak işaretlediği kanıtlandı -> `pfn < LOW_MEM_GUARD_END_PFN` filtresi `BOOTLOADER_RECLAIMABLE` koluna da eklendi; böylece `Milestone 1B-B`'de önyükleyici belleği geri kazanıldığında bile fiziksel `0x0000..0xFFFF` aralığının kalıcı `ReservedOrMmio` kalması garanti altına alındı.


---

## Bölüm G — Faz 1 `Milestone 1B-B`: `VSpace`, 4-Seviye Sayfa Tabloları, `CR3` Context Switching ve SMP TLB Tutarlılığı

- **Tarih:** 11 Kasım 2026
- **Kapsam:** `78ac64b` (`Milestone 1B-A` PMM, `Untyped` temeli ve `26` regresyon testi) korunarak; 4-seviye (`PML4/PDPT/PD/PT`) sanal adres alanı yönetimi, izole kullanıcı `VSpace` nesneleri + tutarlı çekirdek üst-yarısı (`PML4[256..512]`, `U/S = 0`), varsayılan `W^X` (`CR0.WP = 1`, `IA32_EFER.NXE = 1`, `CR4.SMEP = 1`, `CR4.PCIDE = 0`), `H1` zamanlayıcı `CR3` entegrasyonu, Limine `BootloaderReclaimable` yaşam döngüsü ve aktif `boot_cr3` koruması, kilitlenmesiz (`deadlock-free`) `active_cpus` SMP TLB shootdown (`0xEE` Fixed IPI, hem `x2APIC` MSR hem `xAPIC` MMIO), ayrı ve bağımsız ölçülebilir `2 MiB` huge-page (`PageOrder::Order2MiB`, `PDE.PS`) alt aşaması, genişletilmiş `PMM` mikro-ölçümleri (`samples = 2.048`, `rounds_per_cpu = 512`) ve 9 negatif `vspace_*` arıza testi (`35/35` toplam QEMU senaryosu).
- **Tekrarlanabilir Komutlar:**
  - Tekil SMP doğrulaması: `python3 tools/qemu_boot_test.py --smp-only {1|2|4|20}`
  - Tam paket (`35/35` senaryo): `python3 tools/qemu_boot_test.py`

### G.1. Mimari Tasarım, Limine Sayfa Tablosu Yaşam Döngüsü ve Kilitlenmesiz SMP TLB Shootdown Protokolü

1. **İzole `VSpace` Nesneleri, Varsayılan `W^X` ve `CPUID`/`PCID` Dürüstlük Politikası (`vspace/{pte.rs,mod.rs}`):**
   - Her kullanıcı `VSpace` nesnesi `PML4[0..256]` (`[0x1000 .. 0x0000_8000_0000_0000)`, `0x0..0x1000` NULL koruma sayfası hariç) altında özel `PDPT/PD/PT` alt ağaçlarına sahiptir; `PML4[256..512]` ise `KERNEL_VSPACE` (`id = 0`) üst yarısını paylaşır (`U/S = 0` zorunlu). 48-bit kanoniklik, hizalama, aralık taşması ve `W + X` kombinasyonları varsayılan olarak reddedilir (`Err(WxViolation)`).
   - Her CPU'da `CPUID` tespiti yapılır (`pcid_hw=true`, `invpcid_hw=true`), `IA32_EFER.NXE = 1`, `CR0.WP = 1`, `CR4.PGE = 1` ve `CR4.SMEP = 1` etkinleştirilir; ancak `PCID`/`INVPCID` optimizasyonu varsayılmaz ve `CR4.PCIDE = 0` (`pcid_enabled=false`, `pcid_assumed=false`) tutularak ham donanım `CR3` ve `invlpg` maliyetleri dürüstçe raporlanır.
2. **Limine `BootloaderReclaimable` Yaşam Döngüsü (`limine.rs`, `vspace/kernel_handover.rs`):**
   - Önyükleme başında Limine `cmdline` dizgesi `.bss` içindeki kalıcı tampona kopyalanır (`latch_cmdline_to_static_bss`); böylece önyükleyici çerçeveleri kazınıp geri kazanıldığında sallanan işaretçi oluşmaz.
   - `KERNEL_VSPACE` (`id = 0`) tamamen `PMM` çerçeveleriyle (`136` PT çerçevesi) inşa edilir: çekirdek `.text` `RX` (`G=1, NX=0, RW=0`), `.rodata` `RO+NX`, `.data/.bss` (tüm CPU'ların `boot_stack`, worker yığınları, `GDT/TSS`, `#DF` `IST1`, `GS_BASE` ve `IDT` tabloları dahil) `RW+NX`, HHDM RAM `RW+NX` (çekirdek `.text/.rodata` fiziksel takma adları `W^X` sızıntısını önlemek üzere `RO+NX`; `ReservedOrMmio` delikleri eşlenmemiş) ve `0xFEE0_0000` Local APIC sayfası `UC+RW+NX`.
   - Geçiş öncesinde `reclaim_bootloader_reclaimable_frames` çağrısının aktif `boot_cr3` nedeniyle `Err(BootTableStillActive)` döndürdüğü kanıtlanır (`boot_reclaim_blocked_while_active=true`). Ancak BSP ve tüm AP'ler `CR3` yazmacını `KERNEL_VSPACE`'e geçirdikten sonra `13.051..13.684` adet (`~51..53.5 MiB`) `BootloaderReclaimable` çerçeve `PMM` havuzuna güvenle aktarılır.
3. **H1 `CR3` Geçişi, Kilitlenmesiz SMP TLB Shootdown ve Geciktirilmiş Geri Kazanım (`sched/mod.rs`, `vspace/tlb.rs`):**
   - `complete_post_switch_from_asm` içinde `switch_cpu_vspace_if_needed(cpu_idx, next_vs)`: `old_vs == next_vs` (tüm `Kernel -> Kernel` ve aynı-`VSpace` geçişleri) durumunda **sıfır `mov cr3` ve sıfır çapraz-CPU atomik yazma** yapar (`cr3_noop_p50 = 63..71 cyc`, `rdtsc` ölçüm sargısı dahil). Adres alanı değiştiğinde `next.active_cpus.fetch_or(SeqCst)` -> `mov cr3` -> `old.active_cpus.fetch_and(SeqCst)` + `ack_if_leaving_vspace` sıralaması uygulanır.
   - **4 Kilitlenme Vektörünün Tam Elemesi ve Intel SDM §4.10.2.3 Doğrusallaştırması:** (i) Kesme bağlamında (`irq_depth > 0`) shootdown yasaktır; (ii) Katı tek yönlü kilit DAG'ı `VSpace::lock` (CPU başına en fazla 1) -> `TLB_REQ_LOCK` (yaprak kilit) çalışma zamanında doğrulanır ve `shootdown_tlb` `VSpace::lock` bırakılmadan tamamlanır; böylece eşzamanlı `map_*` çağrılarının temizlenmemiş `PDE/PDPTE/PML4E` sayfalama önbelleklerine çarpması imkânsız kılınır (`pt_len > 0` dizin koparmalarında `FULL_FLUSH_SENTINEL` ile `CR3` tazelenir); (iii) `CLI` (`RFLAGS.IF == 0`) altında `VSpace::lock` veya `TLB_REQ_LOCK` bekleyen her CPU `poll_and_ack_pending_shootdown` çalıştırır ve `0xEE` ISR'ı `%100` kilitsizdir; (iv) `PendingReclaim::finalize_after_shootdown`, veri çerçevesi `map_ref_count` düşümünü ve boşalan `PT/PD/PDPT` çerçevelerinin `PMM`'e iadesini **yalnızca tüm hedef CPU ACK'ları tamamlandıktan ve kilitler bırakıldıktan sonra** yürütür; `IF == 0` ile kilitlenen uzak CPU varsa sınırlı `SHOOTDOWN_TIMEOUT_TSC` deterministik panik üretir (`exit=35`).

### G.2. Genişletilmiş `PMM` (`N=2.048`) ve `VSpace` / `2 MiB` / TLB Shootdown Ölçümleri (`35/35` Geçti)

#### Tablo G.2a — Genişletilmiş `PMM` Yeniden Doğrulaması (`samples = 2.048`, `rounds_per_cpu = 512`, Sentetik Mikro-Ölçüt)

| Senaryo | `-smp` | `alloc` `p50/p95/p99` (`cyc`) | `free` `p50/p95/p99` (`cyc`) | `zero_4k` `p50/p95/p99` (`elided p50`) | Verim: Magazine vs Global (`hit%`) | Global Spin: Direct -> Mag | `2 MiB` Blok (`önce->orta->sonra`) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` | `425 / 445 / 454` | `99 / 115 / 119` | `141 / 144 / 144` (`150`) | `34.99` vs `29.94 Mops/s` (`99%`) | `0 -> 0` | `93 -> 77 -> 93` |
| `boot_2cpu_ok` | `2` | `426 / 449 / 470` | `99 / 119 / 126` | `141 / 144 / 144` (`150`) | `33.54` vs `9.76 Mops/s` (`99%`, `3.44x`) | `18.823 -> 8` (`2.353x`) | `93 -> 77 -> 93` |
| `boot_2cpu_xapic` | `2` | `425 / 449 / 521` | `99 / 117 / 125` | `141 / 143 / 144` (`150`) | `36.07` vs `10.59 Mops/s` (`99%`, `3.41x`) | `19.712 -> 13` (`1.516x`) | `93 -> 77 -> 93` |
| `boot_4cpu_ok` | `4` | `424 / 449 / 498` | `101 / 126 / 141` | `141 / 144 / 144` (`150`) | **`31.22` vs `3.05 Mops/s`** (`99%`, **`10.24x`**) | **`319.143 -> 90`** (`3.546x`) | `91 -> 75 -> 91` |
| `boot_20cpu_ok` | `20` | `476 / 521 / 589` | `127 / 144 / 158` | `174 / 182 / 191` (`178`) | **`27.05` vs `1.90 Mops/s`** (`99%`, **`14.24x`**) | **`12.212.371 -> 459`** (`26.606x`) | `91 -> 75 -> 91` |

#### Tablo G.2b — `VSpace`, `CR3` (`PCIDE=0`), `2 MiB` Huge-Page ve SMP TLB Shootdown Maliyetleri (`samples = 512` yerel / `256` IPI)

| Senaryo | `-smp` | `CR3` Switch `p50/p95/p99` (`noop p50`) | `map_4k` `p50/p95/p99` | `protect_4k` `p50/p95/p99` | `unmap_local` `p50/p95/p99` | `shootdown_1peer` `p50/p95/p99` | `shootdown_all` `p50/p95/p99` | `2 MiB` vs `512x4K` Map (`cyc`) | SMP Stres (`shootdowns`: `ipi` + `coop_ack`) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` | `240 / 243 / 249` (`64`) | `208 / 212 / 218` | `411 / 417 / 423` | `296 / 299 / 300` | `0` (tek CPU) | `0` (tek CPU) | `15.615` vs `91.208` (**`5.84x`**) | `0` (`stale=0, leak=0`) |
| `boot_2cpu_ok` | `2` | `240 / 243 / 253` (`65`) | `207 / 212 / 229` | `411 / 416 / 417` | `300 / 304 / 308` | `5.522 / 5.607 / 6.181` | `5.562 / 5.611 / 5.838` | `16.324` vs `90.530` (**`5.55x`**) | `251` (`60` + `191`, `stale=0`) |
| `boot_2cpu_xapic` | `2` | `239 / 243 / 245` (`63`) | `210 / 214 / 215` | `409 / 415 / 417` | `296 / 299 / 300` | `7.090 / 7.149 / 8.353` | `7.104 / 7.180 / 8.622` | `15.548` vs `91.303` (**`5.87x`**) | `242` (`53` + `189`, `stale=0`) |
| `boot_4cpu_ok` | `4` | `249 / 255 / 257` (`65`) | `209 / 214 / 215` | `427 / 431 / 433` | `296 / 299 / 318` | `5.517 / 5.623 / 7.168` | `13.492 / 13.539 / 23.662` | `16.595` vs `89.877` (**`5.42x`**) | `118` (`27` + `90`, `stale=0`) |
| `boot_20cpu_ok` | `20` | `259 / 372 / 664` (`71`) | `251 / 261 / 275` | `466 / 581 / 1.227` | `350 / 364 / 379` | **`6.192 / 6.744 / 10.437`** | `37.223.945 / 68.526.083 / 77.106.996` | `23.519` vs `106.679` (**`4.54x`**) | `538` (`155` + `1.301`, `stale=0`) |

- **Sentetik Ölçüt ve Konak Sınır Beyanı:** Yukarıdaki tüm sayılar çekirdek içi sentetik mikro-ölçüt sonuçlarıdır; gerçek iş yükü/userspace uygulama performansı kanıtı değildir. `CR4.PCIDE = 0` tutulduğundan `CR3` maliyetleri `PCID`-siz tam TLB temizleme maliyetidir. 4 fiziksel çekirdek / 8 izlekli konakta `-smp 20` `shootdown_all` (`19` eş-CPU bariyeri) konak CFS zamanlayıcı paylaşımını (`PLE`) yansıtır ve **fiziksel 20 çekirdek performans kanıtı değildir**; öte yandan `active_cpus` hedef maskelemesinin tek-eşli (`shootdown_1peer = 6.192 cyc`) senaryoda gereksiz 19-CPU yayınına kıyasla gecikmeyi **`6.011x`** düşürdüğünü somut olarak gösterir. `x2APIC` MSR (`5.522 cyc`) ise `xAPIC` HHDM UC MMIO'ya (`7.090 cyc`) göre IPI tur-süresini **`%22.1`** kısaltmaktadır.
- **9 Negatif `VSpace` & TLB Arıza Testi (`-smp 2`, tamamı `exit=35` ile deterministik yakalandı):**
  1. `vspace_wrong_aspace`: Yanlış `VSpace` içinde `0x400000` okumasında donanım `#PF` (`err=0x0, cr2=0x400000`, `1.566 s`).
  2. `vspace_us_bypass`: `U/S = 0` sayfa üzerinde kullanıcı erişim denemesinde `[DRY-VSPACE-FAIL] U/S permission bypass rejected` (`1.502 s`).
  3. `vspace_write_ro`: `USER_RO` korunan sayfaya yazmada `CR0.WP=1` donanım `#PF` (`err=0x3, cr2=0x500000`, `1.503 s`).
  4. `vspace_exec_nx`: `NX=1` (`U/S=0`, `SMEP` maskelemesiz) sayfada buyruk getirmede donanım `I/D` `#PF` (`rip=0x501000, err=0x11, cr2=0x501000`, `1.504 s`).
  5. `vspace_double_map`: Aynı sanal adrese ikinci eşlemede `[DRY-VSPACE-FAIL] Double map detected on vspace=1 vaddr=0x420000` (`1.502 s`).
  6. `vspace_double_unmap`: Eşlenmemiş adreste ikinci `unmap_4k` çağrısında `[DRY-VSPACE-FAIL] Double unmap detected` (`1.470 s`).
  7. `vspace_stale_tlb`: Uzak `0xEE` IPI bastırıldığında CPU 1'de gerçek bayat donanım TLB okuması yakalandı (`hw_stale_hit=true ipis_handled=0`, `1.508 s`).
  8. `vspace_shootdown_timeout`: Uzak CPU `CLI` (`IF == 0`) ile kilitlendiğinde sınırlı TSC zaman aşımı (`targets=0x2 acked=0x0`, `1.610 s`).
  9. `vspace_active_frame_reuse`: `map_ref_count == 1` olan aktif çerçeve `PMM`'e iade edilmek istendiğinde `[DRY-PMM-FAIL] ... maps=1` (`1.545 s`).

### G.3. Geliştirme Sırasında Yakalanıp Çözülen Gerçek Hatalar (Gizlenen Hata = `0`)

1. **Pragmatik Dosya Tavanı ve `clippy -D warnings` Temizliği:** İlk birleşik `vspace/mod.rs` (`1.151` satır) derleme öncesinde `kernel_handover.rs` (`324` satır), `huge.rs` (`271` satır) ve `mod.rs` (`672` satır) olarak ayrıldı; `query_frame_state(u64)` imzası, Rust `1.99.1`'de güvenli `__cpuid`, kullanılmayan `PTE_ACCESSED/DIRTY`, `PagingHwCaps` ve `PmmError::OrderUnsupportedYet` ölü kodları temizlendi (`0` uyarı).
2. **`1-vCPU` Koşusu — LLVM Linker-Sembolü Örtüşmeme (`Non-Aliasing`) Sabit Katlama Hatası ve `begin_preemptive_epoch` Parametre Sırası:**
   - `extern "C" { static __kernel_virt_start: u8; static __kernel_text_start: u8; }` bildirimlerinde her iki sembol çalışma zamanında `0xffffffff80000000` adresini taşıdığı hâlde, LLVM `-O` derleyicisi iki ayrı `i8` nesnesinin aynı adreste olamayacağını varsayarak `assert_eq!(kvirt_start, text_start)` karşılaştırmasını derleme zamanında `false`'a katladı (`left == right: 18446744071562067968`). Sahte `extern static u8` bildirimleri kaldırılıp `lea {out}, [rip + sym]` (`linker_sym_addr!`) makrosuna geçilerek kökünden giderildi.
   - `vspace/tests.rs` içindeki `begin_preemptive_epoch` çağrılarında beklenen iş parçacığı sayısı yerine `online_cpus` geçildiği için `1` CPU'da `8` worker çıktığında `PHASE_PENDING_THREADS` alttan taştı (`usize::MAX - 6`); gerçek görev sayıları (`2`, `1 + peer_count`, `workers`) bağlanarak düzeltildi.
3. **`2-vCPU` Koşusu — `SMP_STRESS_VSPACES = 4` Modulo Eş-Dağılımı:** `w % 4` ataması `smp = 2` üzerinde çift `VSpace`'leri yalnızca CPU 0'a, tek `VSpace`'leri yalnızca CPU 1'e düşürdüğü için çapraz-CPU shootdown tetiklenmedi (`shootdowns=0`); `(w / 2) % 4` eşlemesiyle her `VSpace`'in çoklu CPU tarafından eşzamanlı paylaşılması sağlandı (`shootdowns=251`, `coop_acks=191`).
4. **`20-vCPU` Stres Koşusu — Gerçek Donanım Intel SDM §4.10.2.3 `PDE` Sayfalama-Yapısı Önbelleği Yarışı (`cr2=0xc14000, err=0x2`):**
   - İlk `-smp 20` koşusunda `cpu=9` üzerinde `map_4k(0xc14000)` hemen sonrasında `err=0x2` (`P=0` yazma) donanım `#PF` yakalandı. Kök neden analizinde: `CPU A`'nın boşalan `PT` çerçevesini `pd[i2] = 0` ile ayırıp `shootdown_tlb` öncesinde `VSpace::lock` kilidini bıraktığı; `CPU 9`'un ise `shootdown` IPI'ı ulaşmadan kilidi alıp yeni bir `PT` tahsis ederek `0xc14000`'a yazdığı, ancak `CPU 9` donanım `PDE` önbelleğinin hâlâ eski sıfırlanmış `PT`'yi gösterdiği kanıtlandı.
   - `shootdown_tlb` çağrısı tek yönlü `VSpace::lock -> TLB_REQ_LOCK` DAG'ı altında `VSpace::lock` içine alındı ve dizin koparmalarında (`pt_len > 0`) `FULL_FLUSH_SENTINEL` (`mov cr3`) zorunlu kılındı; düzeltme sonrasında art arda tüm `-smp 20` ve tam `35/35` paket koşuları `0` hata ile geçti.


---

## Bölüm H — Faz 1 `Milestone 1B-C`: Sabit Korumalı `CSpace`, CDT İptal Yaşam Döngüsü, `ADR-007` Çekirdek Zorlaması ve `TLB` Performans Borcu Analizi

- **Tarih:** 11 Kasım 2026
- **Kapsam:** `6445641` (`Milestone 1B-B` ve `35` regresyon senaryosu) korunarak; radiks-genişleyebilir `CapPtr` + `64 B/slot` sabit korumalı `CNode` (`256` slot, `MAX_CSPACES = 16`), kilitsiz seqlock `cspace_lookup_fast` (`slot_gen` + canlı nesne `generation` çift ABA kalkanı), monotonik `derive / copy / mint` + IPC-hazır `cspace_transfer` (`IpcCapTransferReceipt`), `TCB` / `VSpace` / `Frame4K` / `Frame2M` / `Untyped` nesnelerinin `CSpace`'e bağlanması, çapraz-`CSpace` intrüzif $O(k)$ CDT geçişli `revoke` + otomatik `CapMapRecord` `VSpace` `unmap` + senkron SMP TLB shootdown, `ADR-007` 3-alan duvarları + `READ_ONLY_EXPORT` + `DEV_WORKSPACE` `lease_epoch` kiralaması, `1/2/4/8/20` vCPU kademeli doğrulaması, 10 yeni negatif `cspace_*` arıza testi (`46/46` toplam QEMU senaryosu) ve `Stage F` bağımsız `TLB` performans borcu incelemesi.
- **Tekrarlanabilir Komutlar:**
  - Tekil SMP doğrulaması: `python3 tools/qemu_boot_test.py --smp-only {1|2|4|8|20}`
  - Tam paket (`46/46` senaryo): `python3 tools/qemu_boot_test.py`

### H.1. Tehdit Modeli (`T-CAP-01..06`), Biçimsel Değişmezler (`INV-CAP-01..08`) ve Yaşam Döngüsü Protokolü

1. **Tehdit Modeli (`T-CAP-01..06`):**
   - `T-CAP-01` (*Sahte/Tahmin Edilmiş Handle*): Yanlış `guard`, `radix_bits`, `slot_idx == 0` veya sınır dışı indeks -> `Err(InvalidHandle)`.
   - `T-CAP-02` (*Slot/Nesne Yeniden Kullanımı & ABA*): Silinmiş veya iptal edilmiş yuva/nesneye eski `CapPtr` ile erişim -> 32-bit `slot_gen` **ve** canlı nesne `generation` çift doğrulaması (`Err(StaleGenerationAba)`).
   - `T-CAP-03` (*Hak/Süre Yükseltme & `W^X` Delme*): `derive/mint/transfer/map/protect` adımlarında üst küme hak talebi, `W+X` birleşimi veya `lease_epoch` uzatma -> `Err(RightsAmplification | InsufficientRights)`.
   - `T-CAP-04` (*Çapraz Alan / Kompartman Sızıntısı*): `Userspace -> RootSystem/Kernel`, `RootSystem -> Kernel`, `GRANT` veya kiralama olmadan kompartmanlar arası devir, ya da `READ_ONLY_EXPORT` çerçeveyi `RW` eşleme -> `Err(DomainViolation | TransferEscalation)`.
   - `T-CAP-05` (*SMP Eşzamanlı `revoke` Yarışı & Bayat Eşleme*): `CPU A` `lookup/derive/transfer/map` yürütürken `CPU B`'nin `revoke` çalıştırması veya iptal sonrası TLB'de kalan sanal eşleme -> `slot.in_flight_ops` pini + seqlock yeniden doğrulaması + `EmptyRevoked` öncesi zorunlu `unmap_4k` & SMP TLB shootdown.
   - `T-CAP-06` (*Aktif Nesnenin Erken Geri Kazanımı*): `map_ref_count > 0`, `iommu_pin_count > 0`, `VSpace.active_cpus != 0` veya çalışan `TCB` (`ExecState != Dead || stack_owner != NO_CPU`) üzerinde `destroy` -> `Err(BusyActiveObject)`.
2. **Biçimsel Değişmezler (`INV-CAP-01..08`):**
   - `INV-CAP-01 (GuardRadixExact)`: Yalnızca `guard == cnode.guard && radix_bits == 8 && 1 <= slot_idx < 256` olan `CapPtr` çözümlenir (`slot 0 = CAP_NULL`).
   - `INV-CAP-02 (DualGenNoAba)`: Çözümleme hem `slot_gen == ptr.slot_gen()` hem de alt katmandaki canlı nesne (`PMM`, `VSpace`, `TCB`, `Untyped`, `CNode`) nesil sayacının eşleşmesini zorunlu tutar.
   - `INV-CAP-03 (MonotonicNarrowing)`: Her `derive / copy / mint / transfer` adımında `child.rights ⊆ parent.rights`, `!(W && X)` ve `(parent.lease == 0 || (0 < child.lease <= parent.lease))` korunur.
   - `INV-CAP-04 (JointSecEnforcement)`: Hiçbir işlem tek başına `CapRights` veya tek başına `SecLabel` ile geçmez; `authorize_cspace_fast` her ikisini ve `now_epoch <= lease_epoch` sınırını birlikte denetler.
   - `INV-CAP-05 (TransitiveRevokeBeforeReclaim)`: İptal kesin olarak **`Live -> Revoking` (`slot_gen + 1`, `SeqCst`) -> `in_flight_ops == 0` süzme -> `CapMapRecord` `VSpace` `unmap` + senkron SMP TLB shootdown -> `EmptyRevoked` / geri kazanım** sırasını izler.
   - `INV-CAP-06 (NoOrphanDescendant)`: Çapraz-`CSpace` intrüzif CDT listesi (`cdt_parent`, `cdt_first_child`, `cdt_next_sibling`, `cdt_prev_sibling`) sayesinde ata yetki iptal edildiğinde farklı `CSpace`'lere aktarılmış torunlar dahil hiçbir türetilmiş yetki canlı kalamaz.
   - `INV-CAP-07 (ImmutableRoExport)`: `READ_ONLY_EXPORT` çerçeveler `CSpace`, `VSpace::protect_4k` ve `PMM` `map_ref_inc_data_frame` katmanlarının üçünde de yalnızca `!writable && expected_owner.domain != Kernel` koşuluyla kabul edilir.
   - `INV-CAP-08 (ActivePinGuard)`: Donanım/CPU kullanımı sıfırlanmadan hiçbir `Frame`, `VSpace`, `TCB` veya `Untyped` nesnesi serbest bırakılamaz.

### H.2. `Stage E` — `CSpace` & `ADR-007` Sentetik Mikro-Ölçütleri (`samples = 512`, `64 B/slot`, `257 KiB` Havuz)

| Senaryo | `-smp` | `lookup` `p50/p95/p99` (`cyc`) | `sec` `p50/p95/p99` (`cyc`) | `derive` `p50/p95/p99` | `mint` `p50/p95/p99` | `revoke1` `p50/p95/p99` | CDT Çocuk Ölçekleme `1 / 8 / 32 / 64` (`cyc`) | Çekişmeli `lookup` `p50/p99` (`cyc`) | SMP Stres (`revoke` / `stale_rej` / `leak`) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` | `99 / 103 / 103` | `53 / 56 / 57` | `283 / 287 / 289` | `233 / 237 / 237` | `237 / 241 / 243` | `243 / 964 / 3.501 / 6.816` | `102 / 112` | `64 / 128 / 0` |
| `boot_2cpu_ok` | `2` | `103 / 106 / 107` | `53 / 56 / 57` | `281 / 285 / 287` | `233 / 237 / 245` | `237 / 241 / 249` | `241 / 964 / 3.496 / 6.829` | `100 / 286` | `64 / 177 / 0` |
| `boot_2cpu_xapic` | `2` | `132 / 142 / 184` | `65 / 77 / 110` | `350 / 436 / 640` | `280 / 367 / 559` | `289 / 361 / 549` | `343 / 1.244 / 4.613 / 8.633` | `114 / 362` | `64 / 203 / 0` |
| `boot_4cpu_ok` | `4` | `130 / 152 / 184` | `67 / 77 / 112` | `381 / 417 / 537` | `309 / 352 / 599` | `306 / 334 / 375` | `328 / 1.215 / 4.183 / 8.495` | `125 / 470` | `64 / 328 / 0` |
| `boot_8cpu_ok` | `8` | `113 / 119 / 122` | `59 / 65 / 67` | `330 / 348 / 375` | `273 / 289 / 300` | `280 / 298 / 322` | `288 / 1.140 / 4.182 / 8.208` | `120 / 400` | `128 / 599 / 0` |
| `boot_20cpu_ok` | `20` | `113 / 121 / 121` | `59 / 65 / 67` | `330 / 353 / 435` | `273 / 291 / 324` | `277 / 300 / 338` | `288 / 1.150 / 4.281 / 8.175` | `124 / 353` | `192 / 918 / 0` |

- **Bellek Ayak İzi ve Ölçekleme Yorumu:** Her `CapSlot` tam `64 B` (tek CPU önbellek satırı; komşu slotlar arası *false sharing* yok), `CNode` başına `256 * 64 B = 16 KiB` (`16` `CSpace` = `256 KiB` + `1 KiB` `CapMapRecord` = toplam **`257 KiB`** statik `.bss` havuzu). CDT iptal maliyeti çocuk sayısıyla tam doğrusal $O(k)$ ölçeklenmektedir (`6.816 / 64 ≈ 106.5 cyc/çocuk` `1 vCPU`'da; `8.175 / 64 ≈ 127.7 cyc/çocuk` `20 vCPU`'da). Kilitsiz okuma yolu `1..20 vCPU` çekişmesi altında dahi `p50 = 100..125 cyc` bandında kalmaktadır.

### H.3. `Stage F` — Ayrı `TLB` Performans Borcu, Konak Aşırı-Abonelik Ayrımı ve Toplu (`Batch`) Shootdown Analizi

| Senaryo | `-smp` (`Konak Oranı`) | `non_member_ipis` | Sayaç Bölüntüsü (`handled + coop == sent`) | `ipi_send` `p50 / p99` (`cyc`) | `remote_sched` `p50 / p99` (`cyc`) | `flush_exec` `p50 / p99` (`cyc`) | `ack_wait` `p50 / p99` (`cyc`) | `16x unmap_4k` vs `unmap_4k_batch` (`cyc`) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `boot_1cpu_ok` | `1` (`0.125x`) | **`0`** | `0 + 0 == 0` (`true`) | `0 / 0` | `0 / 0` | `231 / 531` | `0 / 0` | Yerel (IPI yok) |
| `boot_2cpu_ok` | `2` (`0.25x`) | **`0`** | `57 + 193 == 250` (`true`) | `4.088 / 10.807` | `4.650 / 12.567` | `233 / 300` | `1.097 / 7.634` | `92.759` -> `10.321` (**`8.99x`**) |
| `boot_2cpu_xapic` | `2` (`0.25x`, MMIO) | **`0`** | `48 + 201 == 249` (`true`) | `6.308 / 9.032` | `6.040 / 6.804` | `233 / 377` | `170 / 1.401` | `124.636` -> `12.970` (**`9.61x`**) |
| `boot_4cpu_ok` | `4` (`0.50x`) | **`0`** | `23 + 81 == 104` (`true`) | `11.889 / 14.203` | `12.535 / 15.074` | `257 / 312` | `1.233 / 1.379` | `221.410` -> `18.717` (**`11.83x`**) |
| `boot_8cpu_ok` | **`8` (`1.00x` tam sığma)** | **`0`** | `71 + 357 == 428` (`true`) | **`31.144` / `40.887`** | **`31.536` / `160.815`** | **`261` / `365`** | **`996` / `120.619`** | `528.452` -> `38.186` (**`13.84x`**) |
| `boot_20cpu_ok` | **`20` (`2.50x` aşırı-abone)** | **`0`** | `169 + 1.224 == 1.393` (`true`) | `99.721 / 189.426` | **`24.901.927` / `59.811.581`** | **`644` / `11.142`** | `36.846.509 / 74.898.278` | `559.793.636` -> `33.192.000` (**`16.87x`**) |

1. **`Milestone 1B-B` `538` vs `155 + 1.301 = 1.456` (`ipis_sent = 1.454`) Sayaç Farkının Kök Nedeni ve Kanıtlı Onarımı:**
   - **(a) Birim ayrımı:** `shootdowns` (`538`) çoklu-hedef yayın turlarını, `ipis_sent` (`1.454`) ise hedef CPU başına gönderilen tekil IPI adedini sayar (`19` hedefe giden tek tur `shootdowns += 1`, `ipis_sent += 19` üretir).
   - **(b) `+2` fazlalığın gerçek kök nedeni:** `1B-B`'deki `service_pending_shootdown_on_cpu`, `REQ_GEN` (`G`) okuduktan sonra KVM tarafından kesilip `REQ_TARGET_MASK`'ı bir sonraki `G + 1` turundan okuduğunda (yırtık okuma / *torn read*), `G` turunda hedef olmayan bir CPU'nun kendisini hedef sanıp `coop_acks` sayacını fazladan artırmasına yol açıyordu (`1.454` yerine `1.456`).
   - **(c) Onarım (`vspace/tlb.rs`):** Monotonik `REQ_GEN` seqlock yeniden doğrulaması, 64-bayt önbellek hizalı `CpuTlbSlot` (`CPU_TLB`) ve başlatıcı-tarafı ayrık bit sayımı eklendi; tüm `2, 4, 8, 20` vCPU koşularında **`ipis_handled + coop_acks == ipis_sent`** eşitliği bire bir doğrulandı.
2. **Konak KVM Aşırı-Aboneliği (`8` vs `20` vCPU) ve Alt-Küme (`active_cpus`) Kanıtı:**
   - `4C/8T` (`8` donanım izleği) konakta `8 vCPU` (`1.0x`) tam sığma noktasında `7` uzak çekirdeğin tamamına yayın yapan `shootdown_all p50` yalnızca **`33.103 cyc` (`~10.6 µs`)** ve `flush_exec p50 = 261 cyc` iken; `20 vCPU` (`2.5x` aşırı-abonelik) koşusunda `flush_exec p50` hâlâ **`644 cyc`** kaldığı hâlde `remote_sched p50` **`24.90M cyc` (`~8.0 ms`)** olmaktadır. Yani `20-vCPU` gecikmesinin **`>%99.7`'si** misafir çekirdekten değil, Linux konak CFS zamanlayıcısının uyuyan vCPU izleklerini sıra bekletmesinden kaynaklanmaktadır.
   - `vs_other` çalıştıran üye-olmayan çekirdeklerin tek bir IPI bile almadığı (`non_member_ipis = 0`) tüm SMP topolojilerinde doğrulandı.
3. **Toplu (`unmap_4k_batch`) vs Tembel (*Lazy*) Geçersiz Kılma Değerlendirmesi:**
   - `unmap_4k_batch` tek kilit ve tek `shootdown_tlb` bariyeriyle 16 sayfayı sökerek `2..20` vCPU genelinde **`8.99x .. 16.87x`** hızlanma sağladı (`MAX_SINGLE_INVLPG_PAGES = 16` eşiği üstünde otomatik tam `CR3` tazelemesine geçer).
   - Buna karşılık çerçeve geri kazanımı veya izin daraltma öncesinde *lazy* (ACK beklemeden geciktirilmiş) TLB geçersiz kılma, bayat TLB üzerinden *use-after-free* açacağı için (`INV-CAP-05`, `cspace_stale_mapping_after_revoke`) güvenlik gerekçesiyle **reddedilmiştir**; tembel nesil birleştirme yalnızca Faz 3 `PCID` bağlam değişimlerinde ve çerçeveler `PendingReclaim` kuyruğunda tutulduğu sürece uygulanabilir.

### H.4. 10 Negatif `CSpace` & `ADR-007` Arıza Testi ve Geliştirme Sırasında Çözülen Gerçek Bulgular

- **10 Negatif `cspace_*` Senaryosu (`-smp 2`, tamamı `exit=35` ile deterministik yakalandı, `0` gizlenen hata):**
  1. `cspace_forged_handle`: Sahte `guard` (`0x33 != 0x5a`) ile `CapPtr` kullanımında `[DRY-CSPACE-FAIL] Forged capability handle rejected` (`1.569 s`).
  2. `cspace_stale_aba`: Silinip aynı `slot_idx` üzerinde yeni çerçeveye tahsis edilen yuvaya eski `CapPtr` (`slot_gen` uyuşmazlığı) ile erişimde `[DRY-CSPACE-FAIL] Stale generation / ABA capability access rejected` (`1.573 s`).
  3. `cspace_rights_amp`: `RO` türetilmiş yetkiyle `RW` türetme (`Err(RightsAmplification)`) ve `USER_RW` eşleme denemesinde `[DRY-CSPACE-FAIL] Rights amplification rejected` (`1.598 s`).
  4. `cspace_cross_domain`: `Userspace` öznesinin özel `RootSystem` nesnesine erişiminde `[DRY-CSPACE-FAIL] Cross-domain unauthorized access rejected` (`1.647 s`).
  5. `cspace_concurrent_revoke`: `CPU 0` `cspace_revoke_children` yürütürken `CPU 1`'de eşzamanlı `cspace_lookup_fast` yarışında `[DRY-CSPACE-FAIL] Concurrent lookup on revoked capability rejected` (`1.728 s`).
  6. `cspace_stale_mapping_after_revoke`: Uzak CPU TLB'si ısıtıldıktan sonra `revoke` -> otomatik `unmap` + SMP shootdown sonrası `0x700000` okumasında gerçek donanım `#PF` (`vec=14, err=0x0, cr2=0x700000`, `1.618 s`).
  7. `cspace_parent_child_lifetime`: `CSpace A -> CSpace B` aktarılmış yetkiden türetilen torun (`grandchild`) yetkinin, ata iptal edildiğinde geçersizleşmesi (`rev_cnt=3`, `1.639 s`).
  8. `cspace_transfer_escalation`: `CapRights::GRANT` olmayan yetkiyle kompartmanlar arası `cspace_transfer` denemesinde `[DRY-CSPACE-FAIL] Capability transfer privilege escalation rejected` (`1.634 s`).
  9. `cspace_double_destroy_revoke`: Yıkılmış nesne üzerinde ikinci `cspace_destroy_object` çağrısında `[DRY-CSPACE-FAIL] Double destroy / double revoke detected` (`1.599 s`).
  10. `cspace_premature_active_reclaim`: AP 1 üzerinde aktif çalışan `TCB` için `cspace_destroy_object` çağrısında `[DRY-CSPACE-FAIL] Premature reclamation of active TCB/Frame blocked` (`1.613 s`).
- **Geliştirme Sırasında Yakalanıp Çözülen Gerçek Bulgular:**
  1. **`READ_ONLY_EXPORT` Donanım Sınırı (`pmm/huge_reclaim.rs`, `vspace/mod.rs`):** İlk `1-vCPU` koşusunda `RootSystem` `READ_ONLY_EXPORT` kütüphane çerçevesinin `Userspace` `VSpace` içine `USER_RX` eşlenmesi `PMM` sahiplik denetiminde takıldı; `PMM` ve `protect_4k` sınırları yalnızca `!writable && owner == RootSystem(READ_ONLY_EXPORT) && target != Kernel` geçişine izin verecek, her türlü `RW` eşleme/yükseltmeyi donanım katmanında da reddedecek şekilde sıkılaştırıldı.
  2. **`DEV_WORKSPACE` Kiralama Yetkilendirmesi ve Tekil Kaynak (`security.rs`):** `cspace/mod.rs` içindeki mükerrer alan kontrolü kaldırılıp tüm kiralama (`lease_epoch > 0 && !NO_DELEGATE`) kuralları tek `authorize_cspace_fast` fonksiyonunda birleştirildi.
  3. **`20-vCPU` Alt-Küme Testinde Zamanlayıcı İş-Çalma Göçü (`vspace/tlb_debt.rs`) ve Aşırı-Abonelik Epoch Bütçesi (`sched/preempt_tests.rs`):** Preemptive zamanlayıcının `vs_other` işçisini göç ettirip aynı çekirdeğe `vs_target` işçisini alması nedeniyle oluşan meşru IPI'lar `preempt_disable` ile izole edilerek saf alt-küme hedeflemesi (`non_member_ipis = 0`) kanıtlandı; ayrıca `online_cpus > 8` (`2.5x` konak aşırı-aboneliği) altında 500+ senkron shootdown içeren stres epoch'ları için `PREEMPT_TIMEOUT_TSC` / `EPOCH_WATCHDOG_TSC` `3x` ölçeklenirken `smp <= 8` negatif test süreleri (`3.85 s`) korundu.
  4. **Negatif Test Kurulumunda `W^X` Yakalaması (`cspace/fault_tests.rs`):** İlk tam paket koşusunda `cspace_stale_aba` testinin `executable=false` çerçeveden `FRAME_SHARED_RO` (`EXEC` bitli) türetmeye çalışması bizzat yeni `W^X` türetme kalkanı tarafından `Err(RightsAmplification)` ile yakalandı; test kurulumu `READ | MAP` maskesine çekilerek `46/46` senaryonun tamamı doğrulandı.
