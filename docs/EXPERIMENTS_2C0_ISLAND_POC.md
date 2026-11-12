# DRY OS — `Milestone 2C-0` Deneysel Ölçümler: Minimal Adaptive Driver Island PoC (`docs/EXPERIMENTS_2C0_ISLAND_POC.md`)

> **Tarih:** 12 Kasım 2026  
> **Kapsam:** `Milestone 2C-0` (`Minimal Adaptive Driver Island PoC: Capability-Mediated Device Broker + Isolated Ring 3 Mock Driver Island` — [`ADR-016`](../DECISIONS.md))  
> **Ham JSON Çıktısı:** [`tools/compat_bench/results_poc_2c0_island.json`](../tools/compat_bench/results_poc_2c0_island.json)  
> **İlişkili Araştırma Verisi:** [`docs/COMPATIBILITY_ISLANDS_RESEARCH.md`](COMPATIBILITY_ISLANDS_RESEARCH.md), [`docs/COMPATIBILITY_EXPERIMENTS_DATA.md`](COMPATIBILITY_EXPERIMENTS_DATA.md), [`docs/EXPERIMENTS_2B.md`](EXPERIMENTS_2B.md)

---

## 1. Ölçüm Ortamı ve Metodoloji

- **Konak Donanım & İşletim Sistemi:** Intel Core i5-11300H (`4C/8T`, Tiger Lake-H, VT-x + EPT, `~3.113..3.114 GHz` değişmez TSC), `16 GiB` RAM, Arch Linux `7.2.14-zen1-1-zen x86_64`, `/dev/kvm` aktif.
- **Emülatör, Derleyici ve Önyükleyici:** `QEMU emulator version 11.1.3` (`-machine q35,accel=kvm -cpu host,+invtsc -smp 2 -m 256M`), EDK2 UEFI `OVMF_CODE.4m.fd`, `rustc 1.99.1 (58cf8f9c3 2026-10-13)` + repo-içi SHA-256 kilitli `rust-src 1.99.1`, Limine `10.8.5` (`7a9013f305de0bea1f6310e76e7baba30499fef0`).
- **Mimari Kurulum (`0` Yeni Syscall):**
  - **`Device Broker` (`userspace/src/bin/dry_broker.rs`, `SecDomain::RootSystem`, kompartman `0x0008`):** Yalnızca mevcut `Syscall ABI v0.1` (`0..=31`) ilkellerini kullanır; istemci rozetini (`BADGE_BROKER_CLIENT` / `BADGE_BROKER_ADMIN`) doğrular, `Mock Driver Island` sürecini başlatır/izler, nesil-damgalı (`island_session_badge(gen)`) `Endpoint`, `IoRingSharedPage` (`Frame4K`) ve `Notification` (`sq_notif`, `cq_notif`) kiralamaları türetir, çökme sonrası `sys_cap_revoke` ile CDT alt-ağacını yıkarak bayat yetkileri geçersiz kılar.
  - **`Mock Driver Island` (`userspace/src/bin/dry_mock_drv.rs`, `SecDomain::Userspace`, kompartman `0x0209`):** Kendi `VSpace` + `CSpace` alanında çalışır, başlangıçta `sys_proc_kill(1) == Err(EPERM)` ile MAC yalıtımını doğrular, nesle bağlı deterministik `mock_island_transform(gen, op_arg, payload)` (`SplitMix64` + IEEE 802.3 `CRC32`) yürütür ve iş yokken `timeout_tsc = 0` ile sonsuz olay beklemesinde (`0 Hz` yoklama) uyur.
- **Tekrarlanabilir Komutlar:**
  ```bash
  # Yalnızca Milestone 2C-0 yaşam döngüsü, stale capability reddi ve performans/batch paketi (2/2)
  python3 tools/qemu_boot_test.py --island-only

  # Tam deterministik regresyon paketi (56 Faz 0-1C + 9 P1 + 14 P2 + 6 P3 + 3 P4 + 2 P5/2C-0 = 90/90)
  python3 tools/qemu_boot_test.py

  # SMP (1/2/4/8/20 vCPU) & RAM (256M/512M/1G/2G) masaüstü matrisi (5/5)
  python3 tools/qemu_boot_test.py --p3-matrix
  ```

> **Epistemik Dürüstlük Sınırı (`Zorunlu Uyarı`):** Bu belgedeki ölçümler DRY OS üzerinde **Driver Island kontrol/veri düzlemi, uyku/uyanma, yetki iptali ve süreç kurtarma mekanizmasını** doğrular. `Mock Driver Island` saf CPU üzerinde deterministik `SplitMix64 + CRC32` dönüşümü çalıştırır; gerçek bir PCIe/USB donanım sürücüsünün MMIO yazmaç gecikmesini, DMA veri aktarım süresini, `IOAPIC`/MSI-X kesme gecikmesini veya fiziksel PCIe `FLR` (`~10..100 ms`) donanım sıfırlama süresini **temsil etmez**.

---

## 2. Fonksiyonel ve Güvenlik Yaşam Döngüsü Doğrulaması (`p5a` — `PASS`)

| Alt Denetim | Test Edilen Akış | Ölçülen Kanıt (`[DRY-ISLAND-POC]`) | Sonuç |
| :--- | :--- | :--- | :---: |
| **MAC, Yetki & Kontrol-Düzlemi İzolasyonu** | `Mock Driver Island` (`0x0209`) içinden `sys_proc_kill(1)` + sahte/rozetsiz (`BADGE_BROKER_ROGUE`) çağrılar + sıradan istemcinin (`BADGE_BROKER_CLIENT`) `CRASH` ve **Broker üzerinden veri yönlendirme (`FORWARD_TRANSFORM`)** denemesi | `mac=1` (`-EPERM`), **`unauth=4` (`denied_ops=4` — normal veri trafiği Broker'dan geçemez)** | **`PASS`** |
| **Nesil 1 Doğrudan Veri Düzlemi** | Yalnızca doğrudan kiralanan `Endpoint` RPC (`ACQUIRE_EP`) + `IoRingSharedPage` 8'li grup işleme (`ACQUIRE_RING`, `gen=1`) | 64-bit `SplitMix64+CRC32` özetleri birebir eşleşti (`gen1_ok=1`, `batch=1`) | **`PASS`** |
| **Kontrollü `#UD` Çöküş & Kurtarma** | `BADGE_BROKER_ADMIN` -> `BADGE_SUPERVISOR` kapısından `ud2` (`vec=6`) tetikleme -> `sys_proc_wait` -> tam CDT `sys_cap_revoke` (`op=0/2`) -> `gen=2` yeniden başlatma | `vec=6`, `restarts=1`, `gen2_ok=1` (`gen=2` çıktıları `gen=1`'den farklı ve deterministik) | **`PASS`** |
| **Tam Önceki-Nesil Capability & Durum Reddi** | Çöküş öncesi (`gen=1`) kiralanan **`drv_ep`, `sq_notif`, `cq_notif`, `ring_cap` (`VSPACE_MAP`)** ve `expected_gen=1` token'larının `gen=2` üzerinde yeniden kullanımı | **`5/5` reddedildi** (`-EREVOKED` `-110` / `-EBADCAP` `-9` / `IPC_OP_ERR_STALE_GEN`): `stale_revoked=5` | **`PASS`** |
| **Depolama & `DRY Vault` Kararlılığı** | Ada çöküşü sonrasında `RootSystem` `/persist/island-guard.txt` doğrulaması + canlı `Userspace` `Terminal` üzerinden `write /persist/shell-post.txt` -> `sync` -> `cat` -> `vault-status` | `sv_ok=1`, `tx_seq=7`, `unlocked=true`, `leaked_frames=0` (`4.35 s`) | **`PASS`** |

---

## 3. Deneysel Performans, Uyku/Uyanma, Kurtarma ve Bellek Ölçümleri (`p5b` — `3.113 GHz` TSC)

### 3.1. Kontrol ve Veri Düzlemi IPC Gecikmesi (`8` Isınma + `64` Örneklem)

| İletişim Yolu (`2 vCPU` SMP, Çapraz `VSpace` + Çapraz Kompartman) | `p50` (`cyc` / `µs`) | `p95` (`cyc` / `µs`) | `p99` (`cyc` / `µs`) | Mimari Yorum |
| :--- | :---: | :---: | :---: | :--- |
| **Doğrudan Kiralık `Endpoint`** (`Client -> Mock Island -> Client`) | **`5.270` (`1.69 µs`)** | **`5.298` (`1.70 µs`)** | **`5.328` (`1.71 µs`)** | Normal istemci veri düzlemi (`Broker` sıcak yolda yok) |
| **Yönetici Tanılama 2-Sıçrama RPC** (`Admin -> Broker -> Island -> Broker -> Admin`) | **`10.165` (`3.27 µs`)** | **`10.214` (`3.28 µs`)** | **`10.443` (`3.35 µs`)** | Çift sıçrama `1.93x` maliyet üretir; normal istemci trafiğine `-EPERM` ile kapatılmasının kanıtıdır |

### 3.2. Donanım `HLT` Boşta Durumundan Uyanma Gecikmesi (`32` Örneklem, Örnekler Arası `1.5 ms` Uyku)

Her örnekten önce istemci karşı vCPU'nun gerçek donanım `sti; hlt` durumuna girdiğini `SYS_POWER_CTL` `idle_hlt_calls` artışıyla doğrulamıştır (`hlt_ok = 32/32`):

| Uyanma Metriği | `p50` (`cyc` / `µs`) | `p95` (`cyc` / `µs`) | `p99` (`cyc` / `µs`) | Kapsanan Donanım/Çekirdek Yolu |
| :--- | :---: | :---: | :---: | :--- |
| **Tek Yön Uyanma** (`Client rdtsc -> Island uyanış rdtsc`) | **`13.707` (`4.40 µs`)** | **`30.167` (`9.69 µs`)** | **`32.580` (`10.47 µs`)** | `SYSCALL` + `VEC_RESCHED_WAKE` IPI + KVM `HLT` çıkışı + `CR3` `VSpace` geçişi + Ring 3 dönüşü |
| **Tam Tur Uyanma RTT** (`Client -> HLT Island -> Transform -> Client`) | **`24.243` (`7.79 µs`)** | **`38.973` (`12.52 µs`)** | **`39.291` (`12.62 µs`)** | Soğuk `HLT` uyanışı + `SplitMix64/CRC32` işleme + yanıt teslimi |

### 3.3. `#UD` Servis Çökmesi Sonrası Kurtarma Süresi (`4` Ardışık Çökme/Yeniden Başlatma)

| Metrik (`ud2` Enjeksiyonundan `gen + 1` Yeni Ada Hazır Yanıtına Kadar) | Döngü (`cyc`) | Süre (`µs` / `ms`) | Kapsanan Adımlar |
| :--- | :---: | :---: | :--- |
| **Minimum (`min_cyc`)** | `4.094.050` | `1.315 µs` (`1.32 ms`) | `#UD` tuzağı -> `ProcessSlot` yıkımı -> `sys_proc_wait` -> CDT `sys_cap_revoke` + SMP TLB shootdown -> `dry-mock-drv.elf` yükleme -> `Frame4K` halka eşleme -> hazır el sıkışması |
| **Medyan (`p50_cyc`)** | **`4.430.738`** | **`1.423 µs` (`1.42 ms`)** | Saf mikroçekirdek süreç + capability kurtarma tabanı (`0` donanım FLR gecikmesi) |
| **Maksimum (`max_cyc`)** | `4.481.327` | `1.440 µs` (`1.44 ms`) | `4` ardışık tur sonunda `pmm_leak = 0` |

### 3.4. Boşta CPU / Syscall Etkinliği (`100 ms` Sessiz Pencere, `2 vCPU`) ve Bellek Ayak İzi

| Gözlem Alanı | Ölçülen Değer | Açıklama |
| :--- | :---: | :--- |
| **Boşta Servis Syscall Sayısı (`100 ms`)** | **`brk_sys = 0`, `isl_sys = 0`** | Her iki servis `timeout_tsc = 0` ile çekirdekte bloklu park eder; `0 Hz` meşgul döngü / yoklama |
| **Boşta Donanım `HLT` İkameti (`2 vCPU`)** | **`hlt_calls = 199`**, `hlt_tsc = 623.719.702` / `2 x 311.431.266` (**`> %99.8`**) | Kalan `< %0.2` yalnızca periyodik APIC zamanlayıcı kesmesi muhasebesidir |
| **`Device Broker` Bellek Maliyeti (`0x0008`)** | **`8` eşli sayfa + `5` sayfa tablosu çerçevesi (`52 KiB`)** | ELF `.text/.rodata/.bss`, kullanıcı yığını ve 4-seviye `VSpace` tabloları |
| **`Mock Driver Island` Bellek Maliyeti (`0x0209`)** | **`9` eşli sayfa + `6` sayfa tablosu çerçevesi (`60 KiB`)** | `8` ELF/yığın sayfası + `1` paylaşımlı `IoRingSharedPage` (`0x2400_0000`) + `VSpace` tabloları |
| **Toplam Aktif PMM Çerçeve Deltası & Sızıntı** | **`pmm_act = 31` çerçeve (`124 KiB`)**, **`pmm_leak = 0`** | Süreç sonlandırma ve `revoke` sonrasında `31/31` çerçeve eksiksiz PMM'e döner |

### 3.5. `IoRingSharedPage` (`CAP = 32`) Gruplu vs Grupsuz İletişim Karşılaştırması (`64` Grup / Kip)

| İletişim Kipi (`DRY OS Ring 3`, `2 vCPU`) | Grup Boyutu (`batch`) | `p50` (`cyc/op`) | `p99` (`cyc/op`) | `batch=1` `IoRing`'e Göre Kazanç | Broker 2-Sıçrama RPC'ye (`10.165 cyc`) Göre Kazanç |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Tanılama Broker 2-Sıçrama RPC** (`Yalnızca Admin`) | `1` | `10.165` | `10.443` | `0.45x` | `1.00x` |
| **Doğrudan Kiralık `Endpoint` RPC** | `1` | `5.270` | `5.328` | `0.86x` | **`1.93x`** |
| **`IoRingSharedPage` + `Notification`** | `1` | `4.556` | `164.377` | `1.00x` | **`2.23x`** |
| **`IoRingSharedPage` + `Notification`** | `16` | **`911`** | **`3.308`** | **`5.00x`** | **`11.16x`** |
| **`IoRingSharedPage` + `Notification`** | **`32`** | **`804` (`0.26 µs/op`)** | **`1.603`** | **`5.67x`** | **`12.64x`** |

---

## 4. `EXP-B1` ve `EXP-B4` İle Teknik Olarak Geçerli Karşılaştırmalar

1. **`EXP-B1` (`call_reply = 6.624 cyc` @ `2 vCPU`, `sched_wakeup = 3.455 cyc`) ile Karşılaştırma:**
   - Doğrudan kiralık `Endpoint` gecikmesi (`5.270 cyc` `p50`), `EXP-B1`'deki `2 vCPU` çapraz-süreç `call_reply` bandıyla (`4.876..6.624 cyc`) tam uyumludur; `Broker` üzerinden yönlendirme ise tam beklendiği gibi iki senkron RPC turuna (`10.165 cyc`, `1.93x`) karşılık gelir ve normal istemci trafiği için `-EPERM` ile kapatılmıştır (`INV-ISL-01`).
   - `EXP-B1` `sched_wakeup` (`3.455 cyc`) uyanık çekirdek bağlamında hazır iş parçacığı uyandırmasını ölçerken, buradaki `w1_p50 = 13.707 cyc` (`4.40 µs`) karşı vCPU gerçek `HLT` uykusundayken `VEC_RESCHED_WAKE` IPI + KVM `HLT` çıkışı + `CR3` değişimini içerir ve `EXP-B2` KVM MicroVM `HLT` uyanışından (`199.8..222.5 µs`) **`~45x..50x` daha düşüktür** (ancak ikisi farklı izolasyon katmanlarıdır: Ring 3 süreç vs VMX misafir çekirdeği).
2. **`EXP-B4` (Konak Linux SPSC `IoRing`: `batch=1` `26.351 cyc/op` -> `batch=32` `886 cyc/op`) ile Karşılaştırma:**
   - `EXP-B4` konak Linux üzerinde `eventfd` sistem çağrısıyla (`~26k cyc` tekil uyandırma) koşarken, DRY OS'nin hafif `Notification` kapı zili `batch=1` maliyetini **`4.556 cyc/op`** seviyesinden başlatır.
   - Kapı zili `batch=32` üzerinde amorti edildiğinde maliyet DRY OS üzerinde **`804 cyc/op` (`5.67x` hızlanma, Broker RPC'ye göre `12.64x`)** değerine inerek `EXP-B4`'ün temel mimari sonucunu (`886 cyc/op`) doğrudan `dry-kernel` üzerinde kanıtlar.

---

## 5. Geliştirme ve Mimari Denetim Sırasında Bulunup Çözülen 5 Gerçek Kök Neden

1. **`SYS_SYSINFO` İçinde Per-CPU PMM Şarjör Muhasebesi (`kernel/src/pmm/mod.rs`):** `free_global_frames_snapshot()` yalnızca küresel `pool.free_count` sayacını okuduğu için, süreç yıkımı sonrası CPU-yerel şarjörlere (`PER_CPU_MAGAZINES`, CPU başına `<= 32` çerçeve) iade edilen serbest sayfalar geçici sızıntı gibi görünüyordu. `free_global_frames_snapshot(online_cpus)` küresel havuz + tüm aktif CPU şarjörlerindeki serbest çerçeveleri toplayacak şekilde güncellenerek tam deterministik `pmm_leak = 0` muhasebesi sağlandı.
2. **`SlotState::EmptyRevoked` Hata Kodu Ayrımı (`EREVOKED` `-110` vs `EBADCAP` `-9`):** `sys_cap_revoke` ile iptal edilen bir capability yuvası yeniden tahsis edilmeden önce sorgulandığında/kullanıldığında çekirdek `CapError::Revoked -> EREVOKED (-110)` döndürür (`EBADCAP` `-9` ise sahte tutamaç veya yuva yeniden kullanıldıktan sonraki ABA uyuşmazlığı içindir). Denetim hem `-110` hem `-9` retlerini doğrulayacak şekilde keskinleştirildi.
3. **`LineBuilder` `160 B` Seri Satır Kırpılması:** `userspace/src/shell.rs` içindeki sabit `160 B` satır tamponu uzun telemetri satırını kırptığı için çıktı `[DRY-ISLAND-POC-DIGEST]` ve `[DRY-ISLAND-POC]` (`118 B`) olarak iki atomik satıra ayrıldı.
4. **`INV-SEC-05` `/persist` Sahip İzolasyonu ve QMP ASCII Kısıtı:** `RootSystem` süpervizörünün oluşturduğu `/persist/island-guard.txt` dosyasının `Userspace` `Terminal` tarafından okunmasının `ADR-007` gereği `-EPERM` ile reddedildiği doğrulandı; `RootSystem` doğrulaması `island_audit.rs` içinde tutuldu ve `p5a` testine ayrıca canlı `Userspace` `Terminal` `/persist/shell-post.txt` yazma + `sync` + okuma + `vault-status` adımı eklendi.
5. **Kontrol-Düzlemi Zorlaması (`INV-ISL-01`) ve `RIGHT_REVOKE` ile `IoRing` Alt-Ağaç İptali (`INV-ISL-03`, `dry_broker.rs` & `dry_mock_drv.rs`):** Son mimari denetimde (a) `BROKER_OP_FORWARD_TRANSFORM` çağrısının normal `BADGE_BROKER_CLIENT` istemcilerine `-EPERM` ile kapatılması zorunlu hale getirildi (`unauth=4`), (b) `island_session_badge` 8-bit sarmalanmaya karşı 32-bit nesil alanına (`0x0209_1000_0000_0000 | gen32`) genişletildi ve (c) `dry_mock_drv` tarafından Broker'a verilen `ring_cap` üzerine `RIGHT_REVOKE` eklenip `revoke_generation_caps()` içinde `sys_cap_revoke(ring_cap, 0)` (`cspace_revoke_children` + istemci `VSpace` `unmap` + SMP TLB shootdown) ardından `sys_cap_revoke(ring_cap, 1)` uygulanarak ada süreci sonlanmadan yapılan nesil dönüşümlerinde dahi `-EBUSY` kalıntısı bırakmadan tam iptal sağlandı (`stale_revoked=5`).
