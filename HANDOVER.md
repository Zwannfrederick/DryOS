# DRY OS — Geliştirme Devir Teslim ve Uygulama Durumu (`HANDOVER`)

- **Son Güncelleme:** 11 Kasım 2026 (13:10 +03:00)
- **Mevcut Durum:** **Faz 0 (`H1` & `H2`), `ADR-007` güvenlik mimarisi ve Faz 1 `Milestone 1A` tek-CPU çekirdek önyükleme + `GDT/TSS/IDT` istisna işleme aşamaları (`Adım A..D`) eksiksiz tamamlandı ve makine tarafından doğrulandı.** İstenen ara kontrol noktası uyarınca **SMP (`2/4/20 vCPU`, `Adım E`) ve `H1` zamanlayıcı entegrasyonu (`Adım F`)** öncesinde kullanıcı değerlendirmesi beklenmektedir.

---

## 1. Kesinleşen Mimari Kararlar Özeti (`DECISIONS.md`)

1. **ADR-001 (Boot):** x86-64 önyükleme protokolü olarak **Limine** seçildi (UEFI `OVMF` + doğrudan 64-bit SMP AP başlatma + HHDM).
2. **ADR-002 (Syscall ABI):** x86-64 `SYSCALL` (`RDI, RSI, RDX, R10, R8, R9` kontrol + `R12..R15` kopyasız 4 mesaj yazmacı + sabitlenmiş 4 KiB IPC Buffer; negatif hata kodları). 12 aday çağrılık çekirdek yüzeyi (`dry_vspace_unmap`, `FaultEndpoint` ve `TCB`-bağlı `Notification` eklendi) Faz 1 deneyimine göre evrilebilir tutuldu.
3. **ADR-003 (CSpace):** Faz 1–2'de sabit boyutlu düz korumalı tablo (*Flat Guarded CNode* + `generation` + `iommu_pin_count`), Faz 3'te ABI kırmadan 2 seviyeli radiks ağacı ve epoch-tabanlı `Revoke`.
4. **ADR-004 (Scheduler):** TLC ve Rust `loom` ile doğrulanan `H1` (`AtomicU32(exec_state, park_token)` + açık `fence(Ordering::SeqCst)` + `CAS` tabanlı `post_switch_*`/`finish_waking_and_push`) + CPU başına öncelikli RR kuyruk + IPC Öncelik Bağışı (*Priority Inheritance*); Faz 3–4'te topoloji-farkındalıklı *work stealing* ve `NO_HZ_FULL`.
5. **ADR-005 (Lisans):** DRY Kernel ve native servisler `MIT OR GPL-2.0-only`; Linux *Driver Island* süreçleri ve `libdry_lx_*` katmanı `GPL-2.0-only` (saf `Apache-2.0` ve `GPL-3.0` bağımlılıkları sürücü adalarında yasak).
6. **ADR-006 (Sıra & Bağımsızlık):** **H1 ve H2 tamamlandı (`Gate 0` geçildi).** DRY Kernel gelişimi (Faz 1–2) H2'nin tüm sürücü sınıflarında veya fiziksel silikon IOMMU'da doğrulanmasını beklemeyecektir.
7. **ADR-007 (Güvenlik Etiketleri, Alan İzolasyonu & `DRY Vault`):** `USERSPACE`, `ROOT_SYSTEM` ve `KERNEL` 3-alan ayrımı, 8-bayt `SecLabel`, path-bağımsız `SecLabel + Capability` kesişim denetimi, kapsamlı `DEV_WORKSPACE`, süreli `DRY Vault` `lease_epoch` kiralaması, mutlak çekirdek belleği duvarı ve `INV-SEC-01..08` invariant'ları yürürlüğe alındı (`a443e6c`).

---

## 2. Tamamlanan Doğrulamalar (`H1` & `H2` — 11 Kasım 2026)

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

### Görev 3: `H2` Kaynak Edinimi, İkili Sembol Denetimi ve `libclang` AST Çıkarımı `[TAMAMLANDI]`
- **Üretilen Dosyalar:**
  - `vendor/linux-drivers/MANIFEST.json` (`v6.12` commit `adc218676eef25575469234709c2d87185ca223a`, `26` dosya, `17.845` satır; `v6.18` commit `7d0a66e4bb9081d75c82ec4957c50034cb0ea449`, `26` dosya, `18.528` satır; `0` değiştirilen satır)
  - `tools/h2-audit/fetch_igc_sources.py`, `tools/h2-audit/audit_symbols.py`, `tools/h2-audit/clang_ast_extractor.c`, `tools/h2-audit/clang_ast_deps.py`
  - `experiments/h2-island/manifests/igc-kmod-symbols.json`, `experiments/h2-island/manifests/igc-clang-ast-deps.json`
- **Doğrulanan Sonuçlar:**
  - İkili `igc.ko.zst`: `6.18.60-1-lts = 247` sembol, `7.2.14-arch1-1 = 244` sembol (`237` ortak = **`%96.0`**); `e1000e` ile `%72.5`, `igb` ile `%79.3` / `%83.8` örtüşme.
  - `libclang` `23.1.4` AST: `v6.18` 13 TU genelinde **`349` dış çekirdek çağrısı** (`195` adedi `nm -u`'dan gizli `static inline` yardımcı; 4'lü sınıflandırma: `auto_map=131`, `semantic_adapter=132`, `feature_prune=40`, `unsupported=46`, `sınıflandırılmamış=0`).

### Görev 4: `H2` `igc` Userspace Driver Island (`GPL-2.0-only`) ve Ayrı QEMU `vfio-pci` PoC `[TAMAMLANDI]`
- **Üretilen Dosyalar:**
  - `experiments/h2-island/Makefile`, `experiments/h2-island/include/dry_island.h`, `experiments/h2-island/include/lx_emul/*.h`, `experiments/h2-island/include/{linux,net}/*.h`
  - `experiments/h2-island/src/{dma_arena.c,lx_mmio_irq.c,lx_net_shim.c,igc_pruned_stubs.c}`
  - `experiments/h2-island/tests/test_h2_igc_island.c`, `experiments/h2-island/tests/vfio_e1000e_live.c`
- **Doğrulanan Sonuçlar:**
  - **Uyumluluk Runtime Boyutu:** Toplam **`3.468` satır (`3.028` SLOC)** (`<= 5.000 LOC` hedefi altında); upstream `igc` değişikliği **`%0.00`**; `v6.12 -> v6.18` taşıma farkı **`38` satır (`28` SLOC)** (`<= 100 LOC` hedefi altında).
  - **`make -C experiments/h2-island test`:** Hem `v6.18` hem `v6.12` gerçek 6 çekirdek `igc` C dosyası derlenip bağlandı; `DmaArena` + gölge `vmemmap`, BAR0 MMIO + `eventfd` MSI-X/NAPI ve uçtan uca `igc_probe -> igc_open -> 3x TX/DMA/MSIX/NAPI/IoRing RX -> igc_close` geçti (`dyn_iommu=0`).
  - **`make -C experiments/h2-island vfio-test` (Ayrı `e1000e` Deneyi):** KVM + emüle `intel-iommu` (`caching-mode=on`) + `vfio-pci` üzerinde iki `e1000e` (`8086:10d3`) arasında canlı DMA TX/RX + MSI `eventfd` ve eşlenmemiş `0xdead0000` IOVA için DMAR hatası doğrulandı (*Not: fiziksel silikon IOMMU güvenlik kanıtı değildir*).

---

## 3. Faz 1 `Milestone 1A` Durumu (`Adım A..D` Tamamlandı — `Adım E..F` Öncesi Ara Kontrol)

### Tamamlanan `Milestone 1A` Adımları (`Adım A..D`) `[TAMAMLANDI — 11 Kasım 2026]`
- **Adım A — Repo-Yerel Araç Zinciri (`0` Konak Paketi Değişikliği):** `tools/bootstrap_phase1.py` ve `tools/phase1-toolchain-lock.json` ile sistem `rustc 1.99.1` (`58cf8f9c3`) için resmi `rust-src-1.99.1` + Limine `10.8.5` UEFI (`7a9013f305de`) ve `limine.h` (`6e85ddd6f928`) SHA-256 kilitli olarak `.toolchains/` altına kuruldu.
- **Adım B & C — Minimal 1-CPU `dry-kernel` UEFI Boot & Otomatik QEMU Testi (`bc46907`):** `kernel/src/{main.rs,limine.rs,mem_verify.rs,security.rs,serial.rs,arch.rs}` ve `tools/qemu_boot_test.py` ile Limine HHDM + bellek haritası doğrulaması, `COM1` seri konsol, `ADR-007` `SecLabel`/`CapabilityMeta` öz-testleri ve deterministik panik işleyicisi (`exit=33` başarı / `exit=35` panik) makine tarafından doğrulandı.
- **Adım D — Per-CPU `GDT`, `TSS` (`IST1`) ve 256 Girişli `IDT` İstisna İşleme:** `kernel/src/{gdt_tss.rs,idt.rs}` ile `SYSCALL`/`SYSRETQ` uyumlu 64-bit GDT, 16 KiB ayrı `#DF` (`IST1`) yığınlı TSS ve assembly `TrapFrame` tabanlı IDT kuruldu; canlı `int3` (`#BP`) kurtarma, `#UD` (`ud2`) ve `#PF` (`CR2=0x7fffdeadbee0`) testleri otomatik QEMU koşusunda doğrulandı (`python3 tools/qemu_boot_test.py`).

### Sıradaki Adımlar (Kullanıcı Değerlendirmesi Sonrası `Adım E & F`)
- **Adım E (`SMP`):** Limine MP isteğiyle önce `-smp 2`, ardından `-smp 4` ve bunlar geçtikten sonra `-smp 20` vCPU üzerinde simetrik AP uyanışı, per-CPU `GS` alanı ve CPU başına bağımsız GDT/TSS/IDT yüklemesi.
- **Adım F (`H1` Zamanlayıcı Entegrasyonu):** TLC ve Loom ile doğrulanan `AtomicU32(exec_state, park_token)` + `PerCpuRunQueue` protokolünün, assembly `switch_to`, stack sahipliği ve `thread_entry_trampoline` / `post_switch_*` kurallarıyla `dry-kernel` içine taşınması ve çok-çekirdekli QEMU stres testleri.
