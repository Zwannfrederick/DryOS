# DRY OS — Geliştirme Devir Teslim ve Uygulama Durumu (`HANDOVER`)

- **Son Güncelleme:** 11 Kasım 2026 (09:36 +03:00)
- **Mevcut Durum:** Git deposu (`main`) ilklendirildi; temel mimari ve ADR belgeleri korundu (`commit 5e7356e`). **Görev 1 (TLA+ TLC Biçimsel Doğrulaması) ve Görev 2 (Rust `loom` + Native Çok-Çekirdek Stres Doğrulaması) eksiksiz tamamlandı (`H1` kapısı geçildi).** `H2` ve çekirdek önyükleme (`Görev 3–5`) aşamalarına henüz geçilmedi.

---

## 1. Kesinleşen Mimari Kararlar Özeti (`DECISIONS.md`)

1. **ADR-001 (Boot):** x86-64 önyükleme protokolü olarak **Limine** seçildi (UEFI `OVMF` + doğrudan 64-bit SMP AP başlatma + HHDM).
2. **ADR-002 (Syscall ABI):** x86-64 `SYSCALL` (`RDI, RSI, RDX, R10, R8, R9` kontrol + `R12..R15` kopyasız 4 mesaj yazmacı + sabitlenmiş 4 KiB IPC Buffer; negatif hata kodları). 12 aday çağrılık çekirdek yüzeyi (`dry_vspace_unmap`, `FaultEndpoint` ve `TCB`-bağlı `Notification` eklendi) Faz 1 deneyimine göre evrilebilir tutuldu.
3. **ADR-003 (CSpace):** Faz 1–2'de sabit boyutlu düz korumalı tablo (*Flat Guarded CNode* + `generation` + `iommu_pin_count`), Faz 3'te ABI kırmadan 2 seviyeli radiks ağacı ve epoch-tabanlı `Revoke`.
4. **ADR-004 (Scheduler):** TLC ve Rust `loom` ile doğrulanan `H1` (`AtomicU32(exec_state, park_token)` + açık `fence(Ordering::SeqCst)` + `CAS` tabanlı `post_switch_*`/`finish_waking_and_push`) + CPU başına öncelikli RR kuyruk + IPC Öncelik Bağışı (*Priority Inheritance*); Faz 3–4'te topoloji-farkındalıklı *work stealing* ve `NO_HZ_FULL`.
5. **ADR-005 (Lisans):** DRY Kernel ve native servisler `MIT OR GPL-2.0-only`; Linux *Driver Island* süreçleri ve `libdry_lx_*` katmanı `GPL-2.0-only` (saf `Apache-2.0` ve `GPL-3.0` bağımlılıkları sürücü adalarında yasak).
6. **ADR-006 (Sıra & Bağımsızlık):** **H1 tamamlandı**; sıradaki adım **H2** fizibilite doğrulamasıdır. DRY Kernel gelişimi (Faz 1–2) H2'nin tüm sürücülerde başarılı olmasına bağımlı kılınmayacaktır.

---

## 2. Tamamlanan Doğrulamalar (`H1` — 11 Kasım 2026)

### Görev 1: `H1` Modelinin Yürütülebilir TLC Doğrulaması `[TAMAMLANDI]`
- **Üretilen Dosyalar:**
  - `formal/h1/DrySched.tla` (`578` satır)
  - Hatalı karşı-örnek yapılandırmaları: `formal/h1/DrySchedBug1_EarlyReady.cfg`, `DrySchedBug2_PubOrder.cfg`, `DrySchedBug3_PostSwitchClobber.cfg`, `DrySchedBug4_WakingClobber.cfg`
  - Adillik sınırı yapılandırması: `formal/h1/DrySchedLivenessNoQuantum.cfg`
  - Düzeltilmiş doğrulama yapılandırmaları: `formal/h1/DrySchedSafety.cfg`, `formal/h1/DrySchedLiveness.cfg`
- **Doğrulanan Sonuçlar:**
  - 4 hatalı yapılandırmanın tamamı beklenen karşı örnekleri (`StackOwnershipSafe`, `NoOrphanedReadyTask`, `NoMissedSignalBlock`, `NoDeadRequeue`) `<1 sn` içinde üretti.
  - `DrySchedSafety.cfg`: 3 CPU üzerinde hiçbir adillik varsayımı olmadan **84.351 durum / 31.279 ayrık durum (derinlik 31)** taranarak **0 ihlal** ile geçti.
  - `DrySchedLiveness.cfg`: 3 CPU üzerinde yalnızca `WF_vars` (`SF` yok) ve `RequireQuantumStep = TRUE` ile **54.095 durum / 21.073 ayrık durum (`42.146` tableau durumu)** taranarak `NoLostWakeup` ve `CleanTermination` özellikleri **0 ihlal** ile doğrulandı.

### Görev 2: Rust `loom` Zamanlayıcı Prototipi ve Native Stres Testleri `[TAMAMLANDI]`
- **Üretilen Dosyalar:**
  - `Cargo.toml`, `Cargo.lock`, `experiments/h1-sched/Cargo.toml`
  - `experiments/h1-sched/src/lib.rs` (`Task`, `ExecState`, `ParkToken`, `Notification`)
  - `experiments/h1-sched/src/runqueue.rs` (`RunQueue`, `PerCpuRunQueue`)
  - `experiments/h1-sched/src/sync.rs` (`std` / `loom` senkronizasyon katmanı)
  - `experiments/h1-sched/tests/loom_sched.rs` (`10` test: 5 negatif `#[should_panic]` + 5 pozitif zayıf bellek modeli)
  - `experiments/h1-sched/tests/stress_native.rs` (öncelikli RR/steal birim testi + 8-CPU ve 20-CPU `>12M` geçiş stres testleri)
- **Doğrulanan Sonuçlar:**
  - `cargo clippy --all-targets -- -D warnings` (hem native hem `--cfg loom`): **0 uyarı / 0 hata**.
  - `loom_sched`: 5 negatif test kusurları yakaladı; 5 pozitif test toplam **39.135 C11 zayıf bellek permütasyonunu** **0 hata** ile tamamladı.
  - `stress_native`: 8-CPU (`12.018.728` geçiş, `43.12M ops/s`) ve 20-CPU (`12.037.985` geçiş, `59.54M ops/s`) testleri **0 kayıp uyandırma, 0 stack çakışması ve %100 temiz `Dead` TCB geri kazanımı** ile geçti.
- **Faz 1 Çekirdek Uygulamasına Aktarılacak Zorunlu Kurallar:**
  1. `prepare_park` ve `Notification::signal` içinde açık `core::sync::atomic::fence(Ordering::SeqCst)` kullanılacak (tek başına `SeqCst` CAS yeterli değildir).
  2. `thread_entry_trampoline` ilk çalıştırmada mutlaka `post_switch_*(prev)` çağıracak; `RSP` değişimi assembly sınırıyla korunacak.
  3. Çekirdek içi `prepare_park -> cond kontrolü -> commit_park` penceresi yerel kesmeler kapalıyken (`CLI`) yürütülecek ve dispatch sonrası `APIC` zamanlayıcısı sıfır-buyruk kesme kilitlenmesine izin vermeyecek şekilde kurulacak.

---

## 3. Sıradaki Aşamalar ve Açık Ön-Koşullar (`Görev 3–5`)

### Açık Ön-Koşullar
1. **Görev 3–4 Ön-Koşulu (Linux `.c` Kaynakları ve Fiziksel `igc` Yokluğu):** `/usr/src/linux-lts` yalnızca başlıkları içeriyor (`*.c` yok); ayrıca makinede fiziksel Intel `igc` NIC yok ve QEMU 11.1.3 `-device igc` içermiyor (`igb` ve `e1000e` içeriyor). Görev 3–4'te `kernel.org` üzerinden `v6.12` / `v6.18` `drivers/net/ethernet/intel/{igc,e1000e,igb}` dosyaları çekilecek; `igc` tam derleme/bağlama hedefi, QEMU `intel-iommu` + `e1000e`/`igb` ise canlı paket/VFIO hedefi olarak kullanılacak.
2. **Görev 5 Ön-Koşulu (`rust-src` / `x86_64-unknown-none` ve `limine` Eksik):** Sistem Rust kurulumunda yalnızca `x86_64-unknown-linux-gnu` hedef kütüphanesi kurulu ve `limine` paketi henüz yüklü değil. Görev 5'e geçmeden önce `rust-src` ve `limine` kurulması gerekecek.

### Görev 3: `H2` Bağımlılık ve Sürüm Kayması Analizinin Otomatik Tekrar Üretimi `[SIRADAKİ ADIM]`
- **Dosya Konumları:** `tools/h2-audit/audit_symbols.py`, `experiments/h2-island/manifests/igc-6.18-symbols.json`, `vendor/linux-drivers/` (yalnızca `igc`/`e1000e` kaynak dilimi)
- **Çalıştırma Komutu:**
  ```bash
  python3 tools/h2-audit/audit_symbols.py --kmod-lts 6.18.60-1-lts --kmod-curr 7.2.14-arch1-1 --out experiments/h2-island/manifests/igc-6.18-symbols.json
  ```

### Görev 4: `H2` VFIO / Driver Island PoC'sinin İlk Uygulanabilir Parçası `[BEKLEMEDE]`
- **Dosya Konumları:** `experiments/h2-island/Makefile`, `experiments/h2-island/src/dma_arena.c`, `experiments/h2-island/src/lx_rt_sync.c`, `experiments/h2-island/src/vfio_probe.c`, `experiments/h2-island/include/lx_emul/`

### Görev 5: Deney Sonuçlarına Göre Minimal `dry-kernel` QEMU Boot İskeleti `[BEKLEMEDE]`
- **Dosya Konumları:** `kernel/Cargo.toml`, `kernel/linker.ld`, `kernel/src/main.rs`, `tools/qemu-run.sh`
