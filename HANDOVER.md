# DRY OS — Geliştirme Devir Teslim ve Yarınki Uygulama Planı (`HANDOVER`)

- **Kayıt Tarihi:** 10 Kasım 2026 (23:50 +03:00)
- **Hedef Uygulama Başlangıcı:** 11 Kasım 2026
- **Mevcut Durum:** Geliştirme öncesi hazırlık tamamlandı; hiçbir çekirdek/sürücü/prototip kodu yazılmadı. Temel mimari, ADR'ler, deney planı, yol haritası ve risk kaydı kök dizinde donduruldu.

---

## 1. Kesinleşen Mimari Kararlar Özeti (`DECISIONS.md`)

1. **ADR-001 (Boot):** x86-64 önyükleme protokolü olarak **Limine** seçildi (UEFI `OVMF` + doğrudan 64-bit SMP AP başlatma + HHDM).
2. **ADR-002 (Syscall ABI):** x86-64 `SYSCALL` (`RDI, RSI, RDX, R10, R8, R9` kontrol + `R12..R15` kopyasız 4 mesaj yazmacı + sabitlenmiş 4 KiB IPC Buffer; negatif hata kodları). 12 aday çağrılık çekirdek yüzeyi (`dry_vspace_unmap`, `FaultEndpoint` ve `TCB`-bağlı `Notification` eklendi) Faz 1 deneyimine göre evrilebilir tutuldu.
3. **ADR-003 (CSpace):** Faz 1–2'de sabit boyutlu düz korumalı tablo (*Flat Guarded CNode* + `generation` + `iommu_pin_count`), Faz 3'te ABI kırmadan 2 seviyeli radiks ağacı ve epoch-tabanlı `Revoke`.
4. **ADR-004 (Scheduler):** Faz 0–1'de TLC/Loom ile doğrulanacak `H1 v2` (`AtomicU32(exec_state, park_token)`) + CPU başına intrüzif öncelikli RR kuyruk + IPC Öncelik Bağışı (*Priority Inheritance*); Faz 3–4'te topoloji-farkındalıklı *work stealing* ve `NO_HZ_FULL`.
5. **ADR-005 (Lisans):** DRY Kernel ve native servisler `MIT OR GPL-2.0-only`; Linux *Driver Island* süreçleri ve `libdry_lx_*` katmanı `GPL-2.0-only` (saf `Apache-2.0` ve `GPL-3.0` bağımlılıkları sürücü adalarında yasak).
6. **ADR-006 (Sıra & Bağımsızlık):** Önce **H1**, ardından **H2** doğrulanacak; DRY Kernel gelişimi (Faz 1–2) H2'nin tüm sürücülerde başarılı olmasına bağımlı kılınmayacak.

---

## 2. Doğrulanmış Ortam Durumu ve Açık Ön-Koşullar / Blokajlar

### Kurulu ve Hazır Olanlar `[DOĞRULANDI]`
- `rustc 1.99.1`, `cargo 1.99.1`, `clang 23.1.4`, `java` (`/usr/bin/java`), `python3`, `zstd`, `nm`
- `qemu-system-x86_64 11.1.3` (`intel-iommu`, `igb`, `e1000e`, `nvme`, `virtio-net-pci` destekli; `/dev/kvm` ve `/dev/vfio/vfio` erişilebilir)
- UEFI Firmware: `/usr/share/edk2/x64/OVMF_CODE.4m.fd` ve `OVMF_VARS.4m.fd`
- Kurulu Çekirdek Modülleri (sembol analizi için): `/lib/modules/6.18.60-1-lts/` ve `/lib/modules/7.2.14-arch1-1/`
- ACPI `DMAR` tablosu mevcut (`/sys/firmware/acpi/tables/DMAR`)

### Yarın Çözülecek Açık Ön-Koşullar / Blokajlar
1. **`git` Deposu Henüz İlklendirilmedi:** Yarın ilk iş `git init` çalıştırılıp temel belgeler ilk commit olarak kaydedilecek.
2. **Görev 1 Ön-Koşulu (`tla2tools.jar` Eksik):** Diskte TLA+ `tla2tools.jar` bulunmuyor; Görev 1 başında GitHub TLA+ sürümlerinden `tools/tla2tools.jar` indirilecek (ağ izni gerekir).
3. **Görev 3–4 Ön-Koşulu (Linux `.c` Kaynakları ve Fiziksel `igc` Yokluğu):** `/usr/src/linux-lts` yalnızca başlıkları içeriyor (`*.c` yok); ayrıca makinede fiziksel Intel `igc` NIC yok ve QEMU 11.1.3 `-device igc` içermiyor (`igb` ve `e1000e` içeriyor). Bu nedenle Görev 3–4'te `kernel.org` üzerinden `v6.12` / `v6.18` `drivers/net/ethernet/intel/{igc,e1000e,igb}` dosyaları çekilecek; `igc` tam derleme/bağlama hedefi, QEMU `intel-iommu` + `e1000e`/`igb` ise canlı paket/VFIO hedefi olarak kullanılacak.
4. **Görev 5 Ön-Koşulu (`rust-src` / `x86_64-unknown-none` ve `limine` Eksik):** Sistem Rust kurulumunda yalnızca `x86_64-unknown-linux-gnu` hedef kütüphanesi kurulu ve `limine` paketi henüz yüklü değil. Görev 5'e geçmeden önce `rust-src` ve `limine` kurulması gerekecek.

---

## 3. Yarınki (11 Kasım 2026) 5 Adımlı Uygulama Planı

> **Yarınki İlk İş:** Doğrudan aşağıda **Görev 1 (`formal/h1/DrySched.tla` yürütülebilir TLC doğrulaması)** ile başlanacaktır.

### Görev 1: `H1` Modelinin Yürütülebilir TLC Doğrulaması
- **Dosya Konumları:** `formal/h1/DrySched.tla`, `formal/h1/DrySchedSafety.cfg`, `formal/h1/DrySchedLiveness.cfg`, `formal/h1/DrySchedBuggy.cfg`
- **Bağımlılıklar:** `/usr/bin/java` + `tools/tla2tools.jar` (indirilecek).
- **Kapsam:** `READY` yayın sırası, `WAKING` sırasında eşzamanlı `wakeup`, preemption/migration, `EXEC_DEAD` sonlanma ve Safety vs. Liveness ayrımı (`WF`, `SF` yok).
- **Çalıştırma Komutları:**
  ```bash
  java -XX:+UseParallelGC -jar tools/tla2tools.jar -config formal/h1/DrySchedBuggy.cfg formal/h1/DrySched.tla
  java -XX:+UseParallelGC -jar tools/tla2tools.jar -workers auto -config formal/h1/DrySchedSafety.cfg formal/h1/DrySched.tla
  java -XX:+UseParallelGC -jar tools/tla2tools.jar -workers auto -config formal/h1/DrySchedLiveness.cfg formal/h1/DrySched.tla
  ```
- **Tamamlanma Kriteri (DoD):** Hatalı yapılandırma beklenen karşı-örnek izini üretir; Safety ve Liveness yapılandırmaları **0 hata** ile tamamlanır.

### Görev 2: Rust `loom` Zamanlayıcı Prototipi ve Regresyon Testleri
- **Dosya Konumları:** `Cargo.toml` (workspace), `experiments/h1-sched/Cargo.toml`, `experiments/h1-sched/src/lib.rs`, `experiments/h1-sched/tests/loom_sched.rs`, `experiments/h1-sched/tests/stress_native.rs`
- **Bağımlılıklar:** `cargo 1.99.1`, `loom` crate (`x86_64-unknown-linux-gnu`).
- **Kapsam:** Gerçek `AtomicU32` `(exec_state, park_token)` + açık `fence(SeqCst)` + intrüzif runqueue + negatif regresyon testleri (`StoreLoad` bariyersiz varyantın `loom` tarafından yakalandığının kanıtı) + 8/20-thread stres koşusu.
- **Çalıştırma Komutları:**
  ```bash
  RUSTFLAGS="--cfg loom" cargo test -p dry-h1-sched --test loom_sched --release
  cargo test -p dry-h1-sched --test stress_native --release
  ```
- **Tamamlanma Kriteri (DoD):** Tüm `loom` permütasyonları ve `>= 10_000_000` native uyuma/uyanma/çalma turu 0 panik, 0 kayıp uyandırma ve 0 çift sahiplik ile geçer.

### Görev 3: `H2` Bağımlılık ve Sürüm Kayması Analizinin Otomatik Tekrar Üretimi
- **Dosya Konumları:** `tools/h2-audit/audit_symbols.py`, `experiments/h2-island/manifests/igc-6.18-symbols.json`, `vendor/linux-drivers/` (yalnızca `igc`/`e1000e` kaynak dilimi)
- **Bağımlılıklar:** Yerel `/lib/modules/{6.18.60-1-lts,7.2.14-arch1-1}` + `v6.12`/`v6.18` `igc` kaynak dosyaları + `clang 23.1.4`.
- **Çalıştırma Komutları:**
  ```bash
  python3 tools/h2-audit/audit_symbols.py --kmod-lts 6.18.60-1-lts --kmod-curr 7.2.14-arch1-1 --out experiments/h2-island/manifests/igc-6.18-symbols.json
  ```
- **Tamamlanma Kriteri (DoD):** 247 (`6.18`) ve 244 (`7.2`) sembol listesi deterministik JSON şeması olarak üretilir; her sembol `direct_shim`, `dma_arena`, `rt_sync`, `dead_stub` veya `manual_net` sınıfına atanır (sınıflandırılmamış sembol = `0`).

### Görev 4: `H2` VFIO / Driver Island PoC'sinin İlk Uygulanabilir Parçası
- **Dosya Konumları:** `experiments/h2-island/Makefile`, `experiments/h2-island/src/dma_arena.c`, `experiments/h2-island/src/lx_rt_sync.c`, `experiments/h2-island/src/vfio_probe.c`, `experiments/h2-island/include/lx_emul/`
- **Bağımlılıklar:** `clang 23.1.4`, `GPL-2.0-only` lisans başlığı, Görev 3 sembol manifestosu.
- **Kapsam:** (1) 64-bit userspace'de sabit `DmaArena` + doğrusal gölge `vmemmap` (`struct page[]`) + sınır denetimli `dry_lx_dma_map_page` modülü ve birim testleri; (2) `vfio-pci` BAR MMIO / MSI-X `eventfd` bağlayıcısı ve `igc_hw.c`/`igc_main.c` ilk derleme hedefi.
- **Çalıştırma Komutları:**
  ```bash
  make -C experiments/h2-island test-dma-arena
  make -C experiments/h2-island build-igc-object
  ```
- **Tamamlanma Kriteri (DoD):** Gölge `vmemmap` `page_to_virt`/`virt_to_page` birim testleri (arena dışı *bounce/trap* dahil) geçer ve ilk gerçek upstream `igc` C birimi gölge başlıklarla hatasız `.o` nesnesine derlenir.

### Görev 5: Deney Sonuçlarına Göre Minimal `dry-kernel` QEMU Boot İskeleti
- **Dosya Konumları:** `kernel/Cargo.toml`, `kernel/linker.ld`, `kernel/src/main.rs`, `tools/qemu-run.sh`
- **Bağımlılıklar:** `rust-src` (`-Z build-std=core` veya `x86_64-unknown-none`), `limine` ISO aracı, `qemu-system-x86_64`, `/usr/share/edk2/x64/OVMF_CODE.4m.fd`.
- **Çalıştırma Komutları:**
  ```bash
  cargo build --manifest-path kernel/Cargo.toml --target x86_64-unknown-none --release
  ./tools/qemu-run.sh --smp 20 --check-boot
  ```
- **Tamamlanma Kriteri (DoD):** Limine UEFI ile QEMU `q35` (`intel-iommu` + `-smp 20`) üzerinde çekirdek önyüklenir, HHDM + ACPI `DMAR`/`MADT` adreslerini ve uyanan 20 mantıksal CPU kimliğini seri porta (`COM1`) deterministik olarak yazar ve temiz çıkış koduyla kapanır.

