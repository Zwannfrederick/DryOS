# DRY OS — Sıfır-Bağlam Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

> **Son Güncelleme:** 14 Kasım 2026 (`Project Ascension 2.0`: `Milestone 2A-4a`, `Milestone 2C-1` Linux Island & `Priority C` `VirtIO-GPU` Kapanışı — `94/94` QEMU + `5/5` Matris)  
> **İlke:** *Minimum Overhead. Maximum Useful Work. Correctness first. Security always. Performance measured.*

---

## SONRAKİ OTURUM — YARINKİ AJANLAR (GPT / CLAUDE / KODLAMA AJANI) BURADAN BAŞLA

1. **Projenin Mevcut Durumu ve Kesinleşmiş Mimari Karar (`ADR-016` .. `ADR-018`):**
   - DRY OS; `x86-64` üzerinde çalışan, yetenek (*capability*) yönelimli, `no_std` Rust mikroçekirdeğidir (`dry-kernel` + `20` adet statik `CPL=3` ELF64 kullanıcı alanı ikilisi; `MAX_LATCHED_MODULES = 32`).
   - Faz 0 (`H1` + `H2`), Faz 1 (`1A..1C` + `Opt Pass v1`) ve Faz 2 alt aşamaları **`Milestone 2A-1..2A-3`**, **`Milestone 2B`** (`DRYFS_v1`, `DRY Vault`, `PCI Command.BME = 0`), **`Milestone 2C-0`** (`Device Broker`), **`Milestone 2A-4a`** (`MAX_LATCHED_MODULES = 32`, `XSAVEOPT64`/`FXSAVE64` FPU izolasyonu, `FaultEndpoint` & `AUD-K1..K15`), **`Milestone 2C-1`** (değiştirilmemiş upstream Linux `v6.18` `virtio-rng.c` `GPL-2.0-only` Ring 3 Driver Island `dry-lx-rng.elf` -> `1af4:1044`) ve **`Priority C`** (Ring 3 `VirtIO-GPU` 2D scanout `dry-gpu.elf` + canlı `UEFI GOP Fallback` + konak `Iris Xe`/`NVIDIA`/`PRIME` `H3` denetimi) **TAMAMLANMIŞTIR (`DONE`)**.
   - **Kesin Mimari Kural (`ADR-016..018`):** `Device Broker` (`0x0008`) yalnızca kontrol düzlemidir; veri trafiği doğrudan kiralanmış `Endpoint` veya `IoRingSharedPage` üzerindedir. **`dry-kernel` (`Ring 0`) içinde `0` satır Linux ABI/GPL kodu vardır**; `Syscall ABI v0.1` (`0..=31`) korunmuştur. Bare-metal `Iris Xe`/`NVIDIA` sürücüleri **`VT-d` (`DMAR`) + `MSI/MSI-X` + `>4 GiB BAR`** eksikliği nedeniyle dürüstçe **`BLOCKED`** durumundadır.
2. **Son Doğrulanmış Commit Zinciri (`main`):**
   - **`8572da3`** (`2A-4a` A1: `MAX_LATCHED_MODULES` `16->32`, taşma reddi, `R-18` kapatıldı)
   - **`b24adc7`** (`2A-4a` A2: per-thread `XSAVEOPT64`/`FXSAVE64` FPU/SIMD izolasyonu)
   - **`ac340da`** (`2A-4a` A3/A4: `FaultEndpoint`, `AUD-K15` kill-safe IPC & eşzamanlılık sertleştirmesi)
   - **`1345bcd`** (`Milestone 2C-1`: gerçek Linux `v6.18` `virtio-rng.c` Driver Island + 3-yuvalı PCI DMA)
   - **`1879128`** (`Priority C`: Ring-3 `VirtIO-GPU` 2D scanout, canlı GOP fallback + `H3` konak GPU/PRIME denetimi)
   - **`HEAD`** (`docs(ascension-2.0)`): `tools/qemu_boot_test.py` içine `P6`/`P7` entegrasyonu ve kanonik belge senkronizasyonu (`94/94` QEMU + `5/5` Matris).
3. **Son Doğrulanmış Test Kanıtları (`14 Kasım 2026`):**
   - `python3 tools/qemu_boot_test.py` (**`94/94 PASS`**): `58` legacy/2A-4a (`10` pozitif + `48` negatif) + `9` P1 + `14` P2 + `6` P3 + `3` P4 + `2` P5 (`2C-0`) + `1` P6 (`2C-1`) + `1` P7 (`GPU`) -> tüm `94` kontrol geçti (`leaked_frames = 0`).
   - `python3 tools/qemu_boot_test.py --p3-matrix` -> **`5/5 PASS`** (`1..20 vCPU`, `256M..2G`, `20 vCPU / 2G free_4k=516682`, `leaked_frames = 0`).
   - `python3 tools/qemu_boot_test.py --lx-island-only` -> `1af4:1044`, `vq_irqs=35`, `entropy=1792 B`, `dir_p50=15345 cyc` (`5.115 us`), `brk_p50=19906 cyc` (`6.635 us`), `batch16=56865.8 cyc/op`, `#UD` kurtarma `1.565 ms`, `quiesce=3`, `dma_denied=7`, `RSS=26` (`104 KiB`).
   - `python3 tools/qemu_boot_test.py --gpu-only` -> `1af4:1050` `1280x800`, `present_p50=3416216 cyc` (`1.097 ms`), `GPU_FALLBACK_TO_GOP`, `gen=2` (`8.727 ms`), `docs/p7_virtio_gpu_scanout.png` (`998,700` px), `leaked_frames=0`.
   - `make -C experiments/h3-gpu-compat test` -> `xe=1213`, `i915=1244`, `nvidia=1189` UND, `os_apis=162`, `prime_copy_p50=262582ns`, `prime_fence_p50=18ns` (`14587.9x`), `vtd_active=False`.
   - `python3 tools/h2-audit/fetch_virtio_rng_sources.py --verify` & `fetch_igc_sources.py --verify` + `make -C experiments/h2-island test` -> **`PASS` (`0` değiştirilmiş upstream kaynak)**.
   - `objdump` yığın çerçevesi: **`rust_syscall_dispatch = 248 B` (`0xf8`)**, `sys_cold_dispatch = 312 B` (`0x138`), `sys_irq_ack = 40 B` (`0x28`).
4. **Kesinleştirilmiş Sonraki Milestone Sırası:**
   1. **`Milestone 2C-2` (Sırada #1 — Bare-Metal/GPU Önkoşulları):** ACPI `DMAR` ayrıştırıcısı + `VT-d` per-BDF DMA sayfa tabloları, APIC `MSI/MSI-X` yönlendirmesi ve `>4 GiB` 64-bit BAR eşlemesi (`+ AUD-K6/K7/K8`, `AUD-U3/U4/U8` açık denetim maddeleri).
   2. **`Milestone 2C-3` / `Faz 3A–3B`:** `H2` `igc`/`e1000e` NIC ve `TCB`-bağlı `Endpoint+Notification` ile Ring 3 `dry-linux-abi` (`musl` ELF64) kişilik sunucusu.
   3. **`Faz 4 / P5`:** Fiziksel donanım (`Bare-Metal` USB boot), dar `Xe/GuC` KMS ve isteğe bağlı ayrık GPU MicroVM (`Venus` / `RTD3`).
5. **Açık Riskler (Özet):**
   - **`R-18` (ÇÖZÜLDÜ):** `MAX_LATCHED_MODULES = 32` (`20/32` kullanılıyor).
   - **`R-11` (`P0`):** `16 KiB` yığın bütçesi (`rust_syscall_dispatch = 248 B`).
   - **`R-03` & `R-05` (`P0/P1`):** Donanım `VT-d` / `MSI-X` / `>4 GiB BAR1` olmadan bare-metal `Iris Xe`/`RTX 3050 Ti` Ring 3'e açılamaz (`QEMU/KVM != bare-metal`).
   - **`R-16`, `R-17` & Açık Denetim (`docs/AUDIT_2A4A.md`):** `ACPI S3 = -ENOSYS`; `AUD-K6..K12` & `AUD-U3/U4/U8` sonraki sertleştirme hedefleridir.
6. **İlk Okunması Gereken Belgeler:**
   1. `HANDOVER.md` & `DECISIONS.md` (`ADR-001..018`)
   2. `docs/LINUX_DRIVER_ISLAND_2C1.md`, `docs/GPU_COMPATIBILITY_REPORT.md`, `docs/EXPERIMENTS_ASCENSION_2_0.md`, `docs/AUDIT_2A4A.md`
   3. `ROADMAP.md` & `RISK_REGISTER.md`
7. **İlk Çalıştırılması Gereken Komutlar:**
   ```bash
   git status -s && git log --oneline -n 6
   python3 tools/qemu_boot_test.py --build-only
   python3 tools/qemu_boot_test.py --lx-island-only
   python3 tools/qemu_boot_test.py --gpu-only
   ```
   *(Kritik Kural: `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` içindeki önceden mevcut kullanıcı değişikliklerine **kesinlikle dokunma**, `git add` veya `git checkout` uygulama.)*

### Yarınki Ajanlar İçin Başlangıç Talimatları

- **Geliştirme Ajanı İçin:**
  > DRY OS geliştirmesine devam etmeden önce `HANDOVER.md`, `DECISIONS.md` (`ADR-017`, `ADR-018`), `docs/GPU_COMPATIBILITY_REPORT.md` ve `docs/AUDIT_2A4A.md` dosyalarını oku. `git status -s` (`experiments/h1-sched/` korunacak) ve `--build-only` / `--lx-island-only` / `--gpu-only` ile `94/94` tabanını doğrula. Sıradaki adım **`Milestone 2C-2`**: kernel ACPI `DMAR` + `VT-d` IOMMU izolasyonu, `MSI/MSI-X` ve `>4 GiB` BAR desteği ile `docs/AUDIT_2A4A.md` açık P1 maddelerinin (`AUD-K6/K7/K8`, `AUD-U3/U4/U8`) kapatılmasıdır.
- **Bağımsız Kod İnceleme Ajanı İçin:**
  > Salt-okunur denetim yap. `HANDOVER.md` §4.10'daki kritik dosyaları (`fpu.rs`, `proc/fault.rs`, `pci.rs`, `cspace/`, `drivers/lx-virtio-rng-island/`, `userspace/src/{virtio_gpu,gpu_audit,wm_ipc}.rs`) bellek sıralaması, CDT `revoke`, `BME=0` quiesce, FPU `CR0.TS`/eager switch ve `VT-d` eksikliği sınırları açısından satır satır incele.

---

## 4.1 Projenin Kimliği ve Tasarım Felsefesi

- **DRY OS Hedefi (`ADR-008`):** Tek `dry-kernel` ikilisi üzerinde hem etkileşimli masaüstü (`DRY Desktop`) hem sıfır-GUI başsız sunucu (`DRY Server`) deneyimi sunan; yerel Ring 3 uygulamalarını birincil, Linux ELF64 (`Faz 3`) ve seçilmiş Win32 PE64 (`Faz 5`) iş yüklerini ikincil uyumluluk katmanlarıyla çalıştırmayı hedefleyen bağımsız bir işletim sistemidir.
- **Rust `no_std` Mikroçekirdek Mimarisi:** Ring 0 çekirdeği içinde dosya sistemi, blok/ağ/ekran sürücüsü, pencere yöneticisi, yazıtipi, kriptografik parola kasası, POSIX veya Win32 politikası **bulunmaz (`0` satır)**. Tüm sürücüler ve sistem servisleri ayrı `VSpace` ve ayrı `CSpace` içindeki izole `CPL=3` süreçleridir.
- **Capability Tabanlı Güvenlik Modeli (`ADR-003`, `ADR-007`):**
  - Ortam yetkisi (*ambient authority*), global `uid == 0` süper-kullanıcısı veya `/dev/mem` yoktur. Her işlem çağıranın `CSpace` tablosundaki 64-bit `CapPtr` (`guard | radix | slot_gen | slot_idx`), hak maskesi (`CapRights`), 64-bit `SecLabel { domain, flags, compartment_id, policy_tag }` ve kiralama dönemi (`lease_epoch`) kesişimiyle denetlenir (`authorize_cspace_fast`, `p50 = 53..59 cyc`).
  - Üç kesin güvenlik alanı vardır: `SecDomain::Kernel (0)` (Ring 3'e asla eşlenemez), `SecDomain::RootSystem (1)` (sistem servisleri ve kapsamlı `DEV_WORKSPACE`) ve `SecDomain::Userspace (2)` (uygulamalar ve izole sürücü adaları).
- **Ring 0 vs Ring 3 Sorumluluk Ayrımı ve `Syscall ABI v0.1` (`32` Çağrı: `0..=31`):**
  - **Ring 0 (`dry-kernel`):** `H1` zamanlayıcı + bağlam değişimi, `PMM` + `Untyped`, 4-seviye `W^X` `VSpace` + hedefli SMP TLB shootdown, `CSpace` + intrüzif CDT $O(k)$ `revoke` + $O(1)$ `GLOBAL_LEASE_EPOCH`, `Endpoint`/`Notification` IPC + 4-sıçramalı Öncelik Bağışı, `IOAPIC` `IRQF_ONESHOT` kesme maskeleme, dar beyaz-listeli `IoPortCap` (`IOPL == 0`), `PciDev` BAR eşleme + çöküşte `PCI Command.BME = 0` susturma ve ACPI `S5`/`FADT` güç kapısı.
  - **Mevcut Syscall Tablosu (`kernel/src/syscall/{mod,cold}.rs`):** `0: ABI_PING`, `1: WRITE_DEBUG`, `2: YIELD`, `3: EXIT`, `4: GET_TID`, `5: CAP_QUERY`, `6: EP_CREATE`, `7: IPC_SEND`, `8: IPC_RECV`, `9: IPC_CALL`, `10: IPC_REPLY`, `11: IPC_REPLY_RECV`, `12: NOTIF_CREATE`, `13: NOTIF_SIGNAL`, `14: NOTIF_WAIT`, `15: CAP_DERIVE`, `16: CAP_REVOKE_OR_DEL`, `17: PROC_SPAWN`, `18: PROC_WAIT`, `19: FB_MAP`, `20: IRQ_BIND`, `21: IRQ_ACK`, `22: IOPORT_RW`, `23: SYSINFO`, `24: FRAME_ALLOC`, `25: VSPACE_MAP`, `26: PROC_LIST`, `27: PROC_KILL`, `28: POWER_CTL`, `29: DEV_MAP`, `30: DMA_ALLOC`, `31: VAULT_CTL`.
- **Minimum Overhead Yaklaşımı:**
  - Sıcak `SYSCALL` girişinde kullanıcı belleğinden pointer okunmaz; kontrol argümanları `RDI, RSI, RDX, R10, R8, R9`, satır-içi `32 B` IPC mesaj yükü `R12..R15` yazmaçlarında taşınır (`null_sys = 231 cyc`, birleşik `ep_reply_recv` `1 vCPU = 1.952..1.971 cyc`).
  - Sıcak `rust_syscall_dispatch` yığın çerçevesi `248 B` (`0xf8`) sabit tutulur; soğuk yönetim çağrıları `#[inline(never)]` `sys_cold_dispatch` (`264 B`) içine izole edilir.
  - Yüksek bant genişlikli G/Ç (`dry-vfs <-> dry-blk` ve `Driver Island` veri yolu) paylaşımlı bellek kilitsiz SPSC `IoRingSharedPage` (`CAP = 32`) + `Notification` kapı zilleri üzerinden gruplanarak (`batch=32` `859 cyc/op`) taşınır.
- **Vazgeçilmez Mimari Invariant'lar (Test Geçirmek İçin Asla Gevşetilemez):**
  1. **`INV-SEC-01..08` (`ADR-007`):** Ring 3 `Kernel` eşleme yasağı; `Userspace -> RootSystem` yazma yasağı; `READ_ONLY_EXPORT` mutlak `W^X`; monotonik yetki daraltma (`NO_DELEGATE` ve `DEV_WORKSPACE` için zorunlu `lease_epoch > 0`); path/`link` yetki yükseltmeme; sahtelenemez çekirdek `(badge, sender_label)` damgası; deterministik CDT `revoke` + TLB yıkımı (`-EREVOKED`); kapatılamaz MAC denetimi.
  2. **`H1` Zamanlayıcı Güvenliği (`ADR-004`, `R-01`):** `dry_switch_to -> finish_post_switch` yeni yığına geçmeden önceki yığın asla serbest bırakılmaz; `prepare_park` ve `notif_signal` arasında açık `fence(Ordering::SeqCst)` zorunludur.
  3. **Donanım Sınırları (`INV-IRQ-01..05`, `INV-P4-01`, `R-03`, `R-11`):** Tüm Ring 3 iş parçacıklarında `RFLAGS.IOPL == 0` ve `CR4.SMAP = 1`; DMA çerçeveleri `iommu_pin_count == 1` taşır ve sürücü çöküşünde bellek iadesinden **önce** `PCI Command.BME = 0` temizlenir; hiçbir dosya `1.000` satırı ve hiçbir çekirdek yığını `16 KiB` tavanını aşamaz.

---

## 4.2 Güncel Mimari Haritası (Doğrulanmış Gerçek Dosya Yolları)

Depodaki tüm kaynak dosyalar `< 1.000` satır pragmatik tavanı altındadır:

1. **Çekirdek Girişi, UEFI Önyükleme, FPU, CPU/IDT ve ACPI (`kernel/src/`):**
   - Önyükleme & Limine modül kenetleme (`MAX_LATCHED_MODULES = 32`, `MAX_BOOT_ENTRIES = 36`): `kernel/src/main.rs`, `kernel/src/limine.rs`
   - Per-thread FPU/SIMD izolasyonu (`XSAVEOPT64`/`XRSTOR64` & `FXSAVE64` fallback): `kernel/src/fpu.rs` (`userspace/src/bin/dry_fpu_test.rs`)
   - Mimari yazmaçlar, GDT/TSS (`IST1` `#DF`), Per-CPU (`GS_BASE`) ve 256-vektör IDT (`swapgs` NMI/#DF koruması): `kernel/src/arch.rs`, `kernel/src/gdt_tss.rs`, `kernel/src/percpu.rs`, `kernel/src/idt.rs`, `kernel/src/serial.rs`
   - Donanım `SMAP` + `#PF` istisna tablosu (`copy_from_user`/`copy_to_user`): `kernel/src/uaccess.rs`
   - ACPI `MADT` (`IOAPIC`), `FADT`/`FACS`/`DSDT` güç tabloları, `x2APIC`/`xAPIC` kalibrasyonu ve SMP AP başlatma: `kernel/src/acpi.rs`, `kernel/src/apic.rs`, `kernel/src/smp.rs`
2. **`CSpace`, Güvenlik Etiketleri (`ADR-007`) ve `VSpace` (`kernel/src/{security.rs,cspace/,vspace/}`):**
   - `SecDomain`/`SecLabel`/`GLOBAL_LEASE_EPOCH` kuralları: `kernel/src/security.rs`
   - Korumalı `CNode` (`256` yuva, `64 B/slot`), kilitsiz seqlock arama, CDT $O(k)$ `revoke` ve nesne yıkımı: `kernel/src/cspace/cap.rs`, `kernel/src/cspace/mod.rs`, `kernel/src/cspace/revoke.rs`, `kernel/src/cspace/objects.rs` (`tests.rs`, `fault_tests.rs`, `bench.rs`)
   - 4-seviye `W^X` `VSpace`, `KERNEL_VSPACE` devri, `2 MiB` büyük sayfa, `map_mmio_4k` ve `active_cpus` SMP TLB shootdown: `kernel/src/vspace/pte.rs`, `kernel/src/vspace/mod.rs`, `kernel/src/vspace/huge.rs`, `kernel/src/vspace/tlb.rs`, `kernel/src/vspace/tlb_debt.rs`, `kernel/src/vspace/kernel_handover.rs` (`tests.rs`)
3. **`PMM` ve Fiziksel Bellek Yönetimi (`kernel/src/{pmm/,mem_verify.rs}`):**
   - `16 B` `FrameMeta` (`%0.39` ek yük, `iommu_pin_count`, nesil sayacı), per-CPU 32'li şarjör (`PER_CPU_MAGAZINES`), `free_global_frames_snapshot(online_cpus)`, `Order2MiB` birleştirme ve `Untyped` `split/retype`: `kernel/src/pmm/meta.rs`, `kernel/src/pmm/mod.rs`, `kernel/src/pmm/huge_reclaim.rs`, `kernel/src/pmm/untyped.rs`, `kernel/src/pmm/tests.rs`, `kernel/src/mem_verify.rs`
4. **SMP `H1` Zamanlayıcı (`kernel/src/sched/` & `formal/h1/`):**
   - `AtomicU32(exec_state, park_token)` durum makinesi, `dry_switch_to` bağlam geçişi, `tzcnt` `active_mask` CPU-yerel öncelik kuyrukları, yarışsız `sti; hlt` + `VEC_RESCHED_WAKE (0xed)` ve 4-sıçramalı Öncelik Bağışı: `kernel/src/sched/mod.rs`, `kernel/src/sched/state.rs`, `kernel/src/sched/context.rs`, `kernel/src/sched/runqueue.rs`, `kernel/src/sched/donate.rs`, `kernel/src/sched/preempt.rs` (`tests.rs`, `preempt_tests.rs`), resmi TLA+ modeli `formal/h1/DrySched.tla`. *(Not: Topoloji-farkındalıklı work-stealing Faz 3 `PLANNED` aşamasındadır; mevcut zamanlayıcı CPU-başına intrüzif kuyruk + spawn yük dengelemesi kullanır.)*
5. **Syscall Kapısı, `Endpoint`/`Notification` IPC ve `IoRing` (`kernel/src/{syscall/,ipc/}`, `userspace/src/`):**
   - Ring 0 `SYSCALL`/`SYSRETQ` girişi ve dağıtımı: `kernel/src/syscall/entry.rs`, `kernel/src/syscall/mod.rs`, `kernel/src/syscall/cold.rs`
   - Senkron `Endpoint` (`ReplyToken`, tek-kazananlı uyandırma) ve asenkron `Notification`: `kernel/src/ipc/mod.rs`, `kernel/src/ipc/endpoint_state.rs`, `kernel/src/ipc/endpoint.rs`, `kernel/src/ipc/notif.rs` (`tests.rs`, `bench.rs`)
   - Ring 3 syscall sarmalayıcıları ve kilitsiz SPSC `IoRingSharedPage` (`SQ`/`CQ` `CAP = 32`): `userspace/src/lib.rs`, `userspace/src/abi_ext.rs`, `userspace/src/ioring.rs`
6. **Süreç Yükleyici, Harici IRQ/Oturum ve Kök Süpervizör (`kernel/src/{proc/,irq/}`, `userspace/src/`):**
   - Statik `ET_EXEC` ELF64 yükleyici, `FaultEndpoint` (`SPAWN_FLAG_FAULT_EP`) ve `ProcessSlot` geri kazanımı: `kernel/src/proc/elf.rs`, `kernel/src/proc/fault.rs`, `kernel/src/proc/mod.rs`, `kernel/src/proc/supervisor.rs` (`tests.rs`, `bench.rs`), `kernel/src/audit_tests.rs`, `userspace/src/bin/dry_fault_test.rs`
   - `IrqCap` (`IoApicHwCache`, fırtına kelepçesi), `IoPortCap` beyaz listesi ve önyükleme oturum seçicisi (`session_mode`): `kernel/src/irq/mod.rs`, `kernel/src/irq/ioport.rs`, `kernel/src/irq/session.rs` (`tests.rs`, `bench.rs`)
   - Ring 3 kök süpervizör (`dry-init.elf`) ve servis orkestrasyonu: `userspace/src/bin/dry_init.rs`, `userspace/src/supervisor.rs`, `userspace/src/sup_services.rs`, `userspace/src/sup_ctx.rs`, `userspace/src/sup_audit.rs`, `userspace/src/init_legacy.rs`, temel test ikilileri `userspace/src/bin/{user_hello,user_peer,dry_echo}.rs`
7. **3-Yuvalı PCI Keşfi ve Sabitlenmiş DMA Denetimi (`kernel/src/{pci,pci_cfg}.rs`):**
   - VirtIO 1.0 Modern PCI (`VIRTIO_BLK=0`, `VIRTIO_RNG=1`, `VIRTIO_GPU=2`) yetenek taraması (`COMMON`/`NOTIFY`/`ISR`/`DEVICE` MMIO BAR eşleme), `SYS_DMA_ALLOC` (`DEVICE_DMA | NO_DELEGATE`, `4K`/`2M`, `iommu_pin_count == 1`) ve süreç yıkımında `quiesce_pci_for_cspace` (`PCI Command.BME = 0`): `kernel/src/pci.rs`, `kernel/src/pci_cfg.rs`
8. **VirtIO-Blk Sürücüsü, `DRYFS_v1` (`/persist`) ve `dry-vfs` (`userspace/src/`):**
   - Ring 3 VirtIO 1.0 blok sürücüsü (`0x0006`): `userspace/src/bin/dry_blk.rs`
   - Çift A/B `CRC32` süperblok + 4 KiB Copy-on-Write + çift `FLUSH` kalıcı dosya sistemi ve `/boot` + `/ram` + `/persist` VFS sunucusu (`0x0004`): `userspace/src/persist_fs.rs`, `userspace/src/vfs_mem.rs`, `userspace/src/vfs_proto.rs`, `userspace/src/bin/dry_vfs.rs`
9. **`DRY Vault` ve Güç Yaşam Döngüsü (`kernel/src/{vault_ctrl,power}.rs`, `userspace/src/`):**
   - Çekirdek `GLOBAL_LEASE_EPOCH` / `SEAL_RO_EXPORT` ve ACPI `S5`/`FADT` kapıları: `kernel/src/vault_ctrl.rs`, `kernel/src/power.rs`
   - Ring 3 `PBKDF2-HMAC-SHA256` kimlik doğrulama kasası (`0x0007`) ve güç servisi (`0x0005`): `userspace/src/vault_crypto.rs`, `userspace/src/vault_proto.rs`, `userspace/src/bin/dry_vault.rs`, `userspace/src/bin/dry_power.rs`
10. **Compositor, Hibrit Pencere Yöneticisi, Girdi ve 4 Yerel Uygulama (`kernel/src/display.rs`, `userspace/src/`):**
    - GOP çerçeve tamponu doğrulaması: `kernel/src/display.rs`
    - Birleşik `i8042` klavye (`IRQ1`, `UsQwerty`/`TurkishQ`) + IntelliMouse fare (`IRQ12`) sürücüsü (`0x0003`): `userspace/src/ps2.rs`, `userspace/src/input.rs`, `userspace/src/keymap.rs`, `userspace/src/bin/dry_input.rs`
    - `4 MiB` (`2 x Frame2M` WB) arka tamponlu compositor, sıfır-UB atomik `SharedWindowSurface` ve `Floating`/`SideBySideTiled`/`Stacked`/`Tabbed` WM (`0x0002`): `userspace/src/gfx.rs`, `userspace/src/surface.rs`, `userspace/src/wm_types.rs`, `userspace/src/wm_layout.rs`, `userspace/src/wm.rs`, `userspace/src/wm_ipc.rs`, `userspace/src/compositor.rs`, `userspace/src/display_banner.rs`, `userspace/src/action.rs`, `userspace/src/bin/dry_display.rs`
    - 4 izole `Userspace` uygulaması: `Terminal` (`userspace/src/shell.rs`, `userspace/src/shell_ext.rs`, `userspace/src/bin/dry_shell.rs`), `Life Station` (`userspace/src/bin/dry_sysmon.rs`), `Files` (`userspace/src/bin/dry_files.rs`), `Settings` (`userspace/src/bin/dry_settings.rs`)
11. **`Device Broker`, `Mock` & Gerçek Linux `v6.18` `virtio-rng.c` Driver Island (`Milestone 2C-0`, `2C-1`):**
    - Protokol, rozetler, kontrol-düzlemi `Device Broker` (`0x0008`) ve `Mock Driver Island` (`0x0209`): `userspace/src/island_proto.rs`, `userspace/src/bin/dry_mock_drv.rs`, `userspace/src/bin/dry_broker.rs`, `userspace/src/island_audit.rs`, `tools/qemu_p5_island_validators.py`
    - İzole `GPL-2.0-only` Linux `v6.18` `virtio-rng.c` adası (`dry-lx-rng.elf`): `drivers/lx-virtio-rng-island/{Cargo.toml,src/main.rs,cshim/lx_virtio_shim.c,cshim/include/linux/*.h}`, değiştirilmemiş upstream kaynak `vendor/linux-drivers/v6.18/virtio_rng/virtio-rng.c`, doğrulayıcılar `tools/h2-audit/fetch_virtio_rng_sources.py` ve `tools/qemu_p6_lx_island_validators.py`
12. **Ring 3 `VirtIO-GPU` 2D Scanout, GOP Fallback ve Konak GPU Denetimi (`Priority C`):**
    - Sürücü & denetim: `userspace/src/gpu_proto.rs`, `userspace/src/virtio_gpu.rs`, `userspace/src/bin/dry_gpu.rs`, `userspace/src/gpu_audit.rs`, `tools/qemu_p7_gpu_validators.py`, `tools/bootstrap_qemu_gpu_modules.py`, `tools/qemu_boot_test.py`
    - Konak `Iris Xe` / `RTX 3050 Ti` / `PRIME` fence denetimi: `experiments/h3-gpu-compat/{Makefile,audit_gpu_stacks.py,prime_fence_probe.c,results_h3_gpu.json}`

---

## 4.3 Tamamlanan Milestone'lar (Kronolojik Özet ve Bilinen Sınırlar)

| Milestone | Commit(ler) | Tamamlanan Özellikler | Doğrulama | Bilinen Sınırlamalar |
| :--- | :--- | :--- | :---: | :--- |
| **`Faz 0` (`H1` & `H2`)** | İlk seri (`11 Kas`) | `H1` zamanlayıcı TLA+ (`84.351` durum) + Loom (`39.135` permütasyon); `H2` Linux `v6.12`/`v6.18` `igc` (`3.468` LOC C shim) + QEMU VFIO `e1000e` PoC | TLA+, Loom, 20-CPU stres | `H2` henüz `dry-kernel` üzerine bağlanmadı (`Faz 3`); fiziksel silikon IOMMU test edilmedi. |
| **`Milestone 1A..1C`** | `bc46907` .. `1df3842` | Limine UEFI SMP (`1..20 vCPU`), `H1`, `PMM`, `W^X` `VSpace` + TLB shootdown, `CSpace` CDT `revoke` + `ADR-007`, `CPL=3` ELF64 + `SMAP`, `Endpoint`/`Notification` + PI, `dry-init`/`dry-echo` ve ilk Ring 3 GOP ekranı | `56/56` QEMU + QMP PNG | Statik `ET_EXEC` ELF64 (dinamik PIE/`mmap` yok); FPU/SIMD `XSAVE` yok. |
| **`Opt Pass v1`** | `69522a3` | `null_sys = 231 cyc`, birleşik `ep_reply_recv` (`1.952 cyc`, `12.8x`), `248 B` sıcak çerçeve, koşullu APIC timer, `-16 KiB` meta | `56/56` QEMU | `CR4.PCIDE`/`INVPCID` ve doğrudan IPC thread switch `Opt Pass v2`'ye bırakıldı. |
| **`Milestone 2A-1` (`P1`)** | `fc557ba` .. `9dab7a6` | ACPI MADT `IOAPIC`, `IrqCap` (`IoApicHwCache` + fırtına kelepçesi), dar `IoPortCap` (`IOPL=0`), `dry-input` (`IRQ1`), `dry-shell`, çift-tamponlu `DirtyRegion` GOP compositor | `65/65` QEMU + `p1_interactive_shell.png` | Yalnızca PS/2 klavye (`UsQwerty`); tek pencere. |
| **`Milestone 2A-2` (`P2 Preview`)** | `ff31908` .. `85ec902` | Birleşik `IRQ1` + `IRQ12` PS/2 fare, `4 MiB` (`2 x Frame2M`) arka tampon + `UNMAP_2M`, izsiz yazılımsal imleç, sıfır-UB atomik `SharedWindowSurface`, `Floating`/`SideBySideTiled` WM, `dry-sysmon` | `79/79` QEMU + `p2_desktop_preview.png` | İki sabit pencere; dosya sistemi ve güç yönetimi yok. |
| **`Milestone 2A-3` (`Desktop & Power`)** | `0da9d9d`, `ce61fea`, `58ccda6`, `2a361c3` | Birleşik uygulama yaşam döngüsü (`Launcher`, `DesktopAction`, `PROC_LIST`/`PROC_KILL`), 4 yerel uygulama (`Terminal`, `Life Station`, `Files`, `Settings` + `DialogTransient`), `dry-vfs` (`RO /boot` + uçucu `/ram`), `TurkishQ`, PS/2 tekerlek, `Stacked`/`Tabbed` WM, `dry-power` (`HLT` + `VEC_RESCHED_WAKE`, `DPMS`, `Session Lock`, gerçek `ACPI S5` & `FADT Reset`), `-512 KiB` `.bss` | `85/85` QEMU + `5/5` SMP/RAM Matrisi | `/ram` uçucudur; `Session Lock` bu aşamada parolasız kalkan idi (`2B`'de giderildi); `ACPI S3 = -ENOSYS`. |
| **`Milestone 2B` + `Track B`** | `8c9bc25`, `57d213d` | `PciDev` (`SYS_DEV_MAP`), sabitlenmiş `SYS_DMA_ALLOC` + çöküşte `PCI Command.BME = 0`, Ring 3 `dry-blk` (`VirtIO 1.0`) + kilitsiz `IoRing`, çift A/B `CRC32` CoW kalıcı `DRYFS_v1` (`/persist`, `SIGKILL` kurtarma), `dry-vault` (`PBKDF2-HMAC-SHA256` parola doğrulamalı `Session Lock` + $O(1)$ `GLOBAL_LEASE_EPOCH` + `SEAL_RO_EXPORT`), `DEV_WORKSPACE` kısıtlaması; `ADR-016` 4-kademeli uyumluluk araştırması (`EXP-B1..B4`) | `88/88` QEMU + `5/5` Matris + `p4_*.png` | `DRYFS_v1` düz 24-girişli dizin tablosu kullanır (alt-dizin ağacı yok); donanım VT-d IOMMU yerine QEMU'da `BME=0` + pin kullanılır. |
| **`Milestone 2C-0` (`Island PoC` & Kontrol-Düzlemi Kilidi)** | **`a6fe26b`**, **`49b6682`** | `0` yeni syscall ile yalnızca kontrol düzlemi olan `Device Broker` (`dry-broker.elf` `0x0008`, istemci veri yönlendirmesi `-EPERM`, `unauth_denied=4`) + izole `Mock Driver Island` (`dry-mock-drv.elf` `0x0209`), 32-bit nesil-damgalı `Endpoint`/`IoRing`/`Notification` kiralaması (`d_p50=5.270 cyc`, `batch=32` `804 cyc/op` vs Broker `10.165 cyc`), `#UD` -> `RIGHT_REVOKE` tam CDT `revoke` (`stale_revoked=5/5`) -> `1.42 ms` kurtarma ve `PMM` şarjör muhasebesi düzeltmesi | **`90/90` QEMU + `5/5` Matris** | Saf CPU mock dönüşümüdür; gerçek Linux sürücüsü, PCIe MSI-X, FLR veya hypervisor içermez; önyükleme modül dizisi `16/16` dolmuştur (`R-18`). |
| **`Milestone 2A-4a` (`Kernel Hardening, FPU & FaultEndpoint`)** | **`8572da3`**, **`b24adc7`**, **`ac340da`** | `MAX_LATCHED_MODULES` `16->32` (`R-18` kapatıldı), `XSAVEOPT64`/`XRSTOR64` + `fpu_mode=fxsave` (`512 B`, `corrupt=0`), `FaultEndpoint` (`delivered=20`, `resumed=17`, `storm_budget=16`), `AUD-K1..K15` + `AUD-U1..U9` (`cases=6/6`, `200.000` notif turu), `rust_syscall_dispatch = 248 B` | **`92/92` QEMU + `5/5` Matris** | `TCB`-bağlı birleşik `Endpoint+Notification` beklemesi ile `docs/AUDIT_2A4A.md` açık P1 maddeleri (`AUD-K6/K7/K8`, `AUD-U3/U4/U8`) sonraki aşamaya bırakıldı. |
| **`Milestone 2C-1` (`Genuine Linux v6.18 virtio-rng.c Island`)** | **`1345bcd`** | Değiştirilmemiş upstream Linux `v6.18` `virtio-rng.c` (`sha256=720433a8...`, `GPL-2.0-only`) izole `dry-lx-rng.elf` adasında clang freestanding + minimal C shim ile QEMU `1AF4:1044` üzerinde `35` `vq_irqs`, `1,792 B` (`ones=49%`), `dir_p50=15,345 cyc` (`5.115 us`), `batch16=56,865.8 cyc/op`, `#UD` -> `1.565 ms` kurtarma (`quiesce=3`, `dma_denied=7`, `RSS=26` sayfa) | **`93/93` QEMU (`--lx-island-only`)** | `completed` yoklama (`used->idx` + `PAUSE`) kullanır (donanım MSI-X kesmesi yok); `0` çekirdek satırı Linux ABI/GPL kodu. |
| **`Priority C` (`Ring-3 VirtIO-GPU 2D, GOP Fallback & H3 Audit`)** | **`1879128`** + `HEAD` | İzole Ring-3 `dry-gpu.elf` (`1AF4:1050`) `1280x800` `B8G8R8X8_UNORM` (`present_p50=1.097 ms`), `#UD` (`vec=6`) çöküşünde canlı `[DRY-WM] GPU_FALLBACK_TO_GOP`, `gen=2` (`8.727 ms` kurtarma, `docs/p7_virtio_gpu_scanout.png`, `leaked_frames=0`), `H3` konak GPU/UND/firmware/PRIME fence denetimi (`xe=1213`, `i915=1244`, `nvidia=1189`, `14,587.9x`) | **`94/94` QEMU + `5/5` Matris + `H3`** | `VirtIO-GPU` 2D kapsamındadır (`Virgl` 3D yok); fiziksel `Iris Xe`/`RTX 3050 Ti` `VT-d` (`DMAR`), `MSI-X` ve `>4 GiB BAR` yokluğu nedeniyle `BLOCKED`. |

*(Tamamlanmamış **`Milestone 2C-2` (`VT-d` / `MSI-X`)**, **`Faz 3A/3B` (`dry-linux-abi` ELF64 & `igc`/`e1000e` NIC Adası)** ve **`Faz 4..5`** aşamaları kesin olarak `PLANNED`/`BLOCKED` durumundadır.)*

---

## 4.4 Bugünkü Çalışmanın Teknik Özeti (Doğrulanmış Kök Nedenler ve Kalıcı Çözümler)

Sonraki oturumda aynı hataların tekrarlanmaması için bugün kökten yakalanıp çözülen **8 gerçek mühendislik problemi**:

1. **`VirtIO-Blk` Konak `fdatasync` Kuyruk Gecikmesi Zaman Aşımı (`userspace/src/bin/dry_blk.rs`):**
   - *Kök Neden:* `DRYFS_v1` her atomik yazmada iki `VIRTIO_BLK_T_FLUSH` bariyeri gönderir; konak Linux I/O yükü altında sabit `5.000.000` spin (`~30 ms`) erken zaman aşımına düşebiliyordu.
   - *Çözüm:* `VirtioBlkHw::submit_sync`, ilk `4.096` hızlı `spin_loop()` sonrasında `5.0 sn` `rdtsc_user()` tavanlı ve her `512` adımda `sys_yield()` yapan iki-kademeli beklemeye geçirildi.
2. **`IoRing` Kapanış Yarışı (`BLK_OP_DETACH_RING`, `userspace/src/{ioring.rs,bin/dry_blk.rs}`):**
   - *Kök Neden:* Oturum kapanışında istemci (`dry-vfs`) sonlanıp paylaşımlı `IoRing` sayfalarını iptal ettiğinde, sunucu (`dry-blk`) bayat sanal adrese erişebiliyordu.
   - *Çözüm:* Açık `BLK_OP_DETACH_RING (123)` kontrol mesajı ve uyandırma döngüsüne `sys_cap_query` canlılık bekçisi eklendi (`leaked_frames = 0`).
3. **`DRYFS_v1` Güç Kesintisi Kurtarması ve `INV-SEC-05` `/persist` Sahip İzolasyonu (`userspace/src/persist_fs.rs`, `island_audit.rs`):**
   - *Kök Neden:* (a) Yerinde (*in-place*) blok yazımı güç kesintisinde dosya sistemini bozar; (b) `RootSystem` süpervizörünün oluşturduğu `/persist/island-guard.txt` dosyasının `Userspace` `Terminal` (`0x0102`) tarafından okunması `ADR-007` `INV-SEC-02/05` gereği `-EPERM` (`-1`) ile reddedilir.
   - *Çözüm:* (a) `DRYFS_v1` 4 KiB CoW veri blokları + çift A/B `CRC32` süperblok (`tx_seq & 1`) ile yazıldı ve `SIGKILL` altında kurtarma kanıtlandı (`p4b`); (b) güvenlik kuralı gevşetilmeyip `RootSystem` bütünlük kontrolü süpervizörde (`sv_ok=1`), `Userspace` kalıcılık kontrolü ise ayrı `/persist/shell-post.txt` dosyası üzerinden doğrulandı (`p5a`).
4. **`DEV_WORKSPACE` Zorunlu Kiralama Dönemi (`INV-SEC-07`, `kernel/src/security.rs`, `userspace/src/sup_audit.rs`):**
   - *Kök Neden:* `validate_narrowing_and_delegation`, `SecFlags::DEV_WORKSPACE` kompartmanına `lease_epoch == 0` (süresiz) capability devrini tasarım gereği `-EPERM` ile reddeder.
   - *Çözüm:* `sup_audit.rs` önce bu `-EPERM` reddini negatif güvenlik testi olarak doğrular, ardından `sys_cap_derive` ile aktif `cur_epoch` dönemli kiralık yetki türeterek kompartman testini yürütür (`p4c`).
5. **Süpervizör Rozet Kapısı (`BADGE_SUPERVISOR`) ve Çökme/Yeniden Başlatma (`dry_blk.rs`, `dry_broker.rs`, `dry_mock_drv.rs`):**
   - *Kök Neden:* Hata enjeksiyon kapısı (`IPC_OP_CRASH_UD2`) sıradan istemcilere açık bırakılırsa hizmet reddi (DoS) oluşur; rozetsiz istemci yetkisiyle çağrıldığında ise sunucu isteği reddeder.
   - *Çözüm:* `#UD` tetiklemesi sıkı biçimde `BADGE_SUPERVISOR` (ve `Broker` üzerinde `BADGE_BROKER_ADMIN`) rozetine kilitlendi; `dry-blk` çöküşünde `PCI Command.BME = 0` (`pci_quiesce=2`) ve `dry-mock-drv` çöküşünde CDT `sys_cap_revoke` + `gen + 1` yeniden başlatma (`restarts=1`) canlı kanıtlandı.
6. **`SYS_SYSINFO` İçinde Per-CPU PMM Şarjör Muhasebesi (`kernel/src/pmm/mod.rs`, `kernel/src/syscall/cold.rs`):**
   - *Kök Neden:* `free_global_frames_snapshot()` yalnızca küresel `pool.free_count` değerini okuduğu için, süreç kapanışı sonrası `PER_CPU_MAGAZINES` önbelleklerinde (`<= 32` çerçeve/CPU) tutulan serbest sayfalar `SYS_SYSINFO`'da sahte çerçeve sızıntısı gibi görünüyordu.
   - *Çözüm:* `free_global_frames_snapshot(online_cpus)` küresel havuz + tüm çevrimiçi CPU şarjörlerinin doluluğunu toplayacak şekilde güncellendi (`pmm_leak = 0`).
7. **İptal Edilmiş Capability Hata Semantiği (`EREVOKED` `-110` vs `EBADCAP` `-9`, `kernel/src/cspace/`):**
   - *Kök Neden:* `sys_cap_revoke` sonrası henüz yeniden tahsis edilmemiş yuva `SlotState::EmptyRevoked` durumundadır ve `CapError::Revoked -> EREVOKED (-110)` döner (`EBADCAP` `-9` ise sahte indeks veya yuva yeniden kullanıldıktan sonraki ABA uyuşmazlığı içindir).
   - *Çözüm:* Bayat capability doğrulamaları `Err(EREVOKED | EBADCAP)` sözleşmesine bağlandı.
8. **`LineBuilder` `160 B` Kırpılması ve QMP ASCII Tuş Eşlemesi (`userspace/src/shell.rs`, `tools/qemu_p5_island_validators.py`):**
   - *Kök Neden:* `LineBuilder` sabit `160 B` tamponu aşan seri günlük satırlarını kırpar; ayrıca `qmp.type_ascii` yardımcısı `_` karakterini desteklemez.
   - *Çözüm:* Telemetri satırları `< 120 B` atomik satırlara bölündü ve QMP kabuk dosya adlarında `-` kullanıldı.
9. **Kontrol-Düzlemi Sızıntısı ve `IoRing` Alt-Ağaç İptal Eksikliği (`49b6682`, `dry_broker.rs`, `dry_mock_drv.rs`, `island_proto.rs`):**
   - *Kök Neden:* (a) `BROKER_OP_FORWARD_TRANSFORM` başlangıçta `BADGE_BROKER_CLIENT` çağrılarına açıktı ve `Device Broker`'ın sıcak veri yoluna girmesine izin veriyordu; (b) `dry_mock_drv` tarafından Broker'a verilen `ring_cap` üzerinde `RIGHT_REVOKE` bulunmadığından ada süreci sonlanmadan çağrılan `sys_cap_revoke(ring_cap, 1)` (`cspace_delete_cap`) canlı istemci çocukları varken `CapError::BusyActiveObject (-EBUSY)` ile başarısız olabiliyordu; (c) `island_session_badge` 8-bit (`0xff`) maske kullanıyordu.
   - *Çözüm:* (a) `BROKER_OP_FORWARD_TRANSFORM` normal istemcilere `-EPERM` ile kapatıldı (`unauth_denied = 4`), yalnızca yönetici A/B tanılama ölçümüne izin verildi; (b) `ring_cap` Broker'a `RIGHT_REVOKE` ile verildi (istemci kiralamaları `GRANT`/`REVOKE` olmadan daraltıldı) ve `revoke_generation_caps()` içinde önce `sys_cap_revoke(ring_cap, 0)` (`cspace_revoke_children` + istemci `VSpace` `unmap` + SMP TLB shootdown), ardından `sys_cap_revoke(ring_cap, 1)` koşturuldu (`stale_revoked = 5/5`); (c) oturum rozeti 32-bit nesil alanına (`0x0209_1000_0000_0000 | gen32`) genişletildi.

---

## 4.5 Test Durumu ve Yeniden Üretim

### Doğrulanmış Ortam, Sürümler ve Ön Koşullar
- **Konak Makine:** Intel Core i5-11300H (`4C/8T`, `~3.114 GHz` TSC), `16 GiB` RAM, `Linux 7.2.14-zen1-1-zen x86_64`, `/dev/kvm` okuma/yazma izni açık.
- **Araç Zinciri (`tools/phase1-toolchain-lock.json` ile SHA-256 Kilitli):**
  - `rustc 1.99.1 (58cf8f9c3 2026-10-13)` & `cargo 1.99.1` + `.toolchains/sysroot` (`rust-src 1.99.1`)
  - `QEMU emulator version 11.1.3` (`qemu-system-x86_64`) + UEFI OVMF `/usr/share/edk2/x64/OVMF_{CODE,VARS}.4m.fd`
  - `Limine 10.8.5` (`BOOTX64.EFI` commit `7a9013f305de0bea1f6310e76e7baba30499fef0`, protokol `6e85ddd6f928`)
  - `Python 3.14.10` (`xorriso` **gerekmez**; `qemu_boot_test.py` doğrudan QEMU UEFI VVFAT `fat:rw:...` sürücüsü kullanır)

### Tekrarlanabilir Komutlar ve `Ascension 2.0` (`14 Kasım 2026`) Doğrulanmış Sonuçları

```bash
# 1. Araç zinciri + Clippy (-D warnings: userspace, dry-lx-rng & kernel) + Release ELF64
python3 tools/qemu_boot_test.py --build-only
# -> SONUÇ (HEAD): PASS (0 uyarı, 20 userspace/driver ELF64 + dry-kernel derlendi)

# 2. Upstream Linux v6.18 kaynak bütünlüğü ve H2 / H3 konak GPU uyumluluk denetimleri
python3 tools/h2-audit/fetch_virtio_rng_sources.py --verify
python3 tools/h2-audit/fetch_igc_sources.py --verify
make -C experiments/h2-island test
make -C experiments/h3-gpu-compat test
# -> SONUÇ: 0 değiştirilmiş upstream dosya; xe=1213, i915=1244, nvidia=1189 UND, prime=18ns (14587.9x)

# 3. Milestone 2C-1 Gerçek Linux v6.18 virtio-rng.c Driver Island paketi
python3 tools/qemu_boot_test.py --lx-island-only
# -> SONUÇ: 1/1 PASS (1af4:1044, vq_irqs=35, entropy=1792B, dir_p50=15345 cyc, batch16=56865.8 cyc/op, 1.565 ms, leaked=0)

# 4. Priority C Ring-3 VirtIO-GPU 2D Scanout + Canlı GOP Fallback paketi
python3 tools/qemu_boot_test.py --gpu-only
# -> SONUÇ: 1/1 PASS (1af4:1050 1280x800, p50=1.097ms, GPU_FALLBACK_TO_GOP, gen=2 8.727ms, docs/p7_virtio_gpu_scanout.png)

# 5. SMP (1/2/4/8/20 vCPU) & RAM (256M/256M/512M/1G/2G) Masaüstü Matrisi
python3 tools/qemu_boot_test.py --p3-matrix
# -> SONUÇ: 5/5 PASS (leaked_frames=0, 20 vCPU / 2 GiB free_4k=516682)

# 6. Tam Deterministik QEMU/KVM Regresyon Paketi
python3 tools/qemu_boot_test.py
# -> SONUÇ: 94/94 PASS (0 FAIL, leaked_frames=0)

# 7. Çekirdek sıcak/soğuk syscall yığın çerçevesi denetimi
objdump -d -M intel kernel/target/x86_64-unknown-none/release/dry-kernel | \
  grep -A10 -E "(rust_syscall_dispatch|sys_cold_dispatch|sys_irq_ack)>:"
# -> SONUÇ: rust_syscall_dispatch=248 B (0xf8), sys_cold_dispatch=312 B (0x138), sys_irq_ack=40 B (0x28)
```

---

## 4.6 Kesinleşmiş `ADR-016` Mimari Sözleşmesi ve Adaptive Driver Islands Durumu

> **Kesin Dürüstlük Beyanı:** DRY OS üzerinde değiştirilmemiş Linux `v6.18` C sürücüsü (`virtio-rng.c` `Milestone 2C-1`) ve Ring 3 `VirtIO-GPU` 2D scanout (`Priority C`) QEMU/KVM üzerinde doğrulanmıştır; **ancak** genel Linux ELF64 kullanıcı-alanı uygulamaları (`dry-linux-abi`), Windows `PE32+` uygulamaları (`dry-ntos`) ve fiziksel bare-metal `Iris Xe`/`NVIDIA` GPU çalıştırma aşamaları **`VT-d` (`DMAR`) / `MSI-X` / `>4 GiB BAR`** eksikliği nedeniyle **henüz uygulanmamış (`PLANNED` / `BLOCKED`)** durumdadır (`QEMU/KVM != bare-metal`). Tamamlanan belgeler: `docs/COMPATIBILITY_ISLANDS_RESEARCH.md`, `docs/COMPATIBILITY_EXPERIMENTS_DATA.md`, `docs/EXPERIMENTS_2C0_ISLAND_POC.md`, `docs/LINUX_DRIVER_ISLAND_2C1.md`, `docs/GPU_COMPATIBILITY_REPORT.md` ve `docs/EXPERIMENTS_ASCENSION_2_0.md`.

### Kesin Kontrol / Veri Düzlemi Ayrımı, Ring 3 Konumu ve Invariant'lar (`INV-ISL-01..06`)

- **Gerekçe ve Ölçüm Kanıtı (`p5b`, `2 vCPU`, `3.113 GHz` TSC):** Veri trafiğini `Device Broker` üzerinden çift sıçramalı geçirmek `b_p50 = 10.165 cyc` (`3.27 µs`) gecikmeye yol açarken, doğrudan kiralanan `Endpoint` **`d_p50 = 5.270 cyc` (`1.69 µs`, `1.93x` daha hızlı)** ve `batch=32` `IoRingSharedPage` **`804 cyc/op` (`0.26 µs/op`, Broker RPC'ye göre `12.64x`, `batch=1`'e göre `5.67x`)** sağlar. Bu nedenle `Device Broker` yalnızca kontrol düzlemidir; normal `BADGE_BROKER_CLIENT` veri yönlendirme çağrıları `-EPERM` ile reddedilir (`INV-ISL-01`).
- **Linux API Uyumluluk Katmanının Konumu (`%100 Ring 3`, `0` Çekirdek Satırı — `INV-ISL-05`):**
  - *Sürücü uyumluluğu (`Kademe 2`):* `libdry_lx_<sınıf>` (`< 5.000` LOC C çalışma zamanı) doğrudan ilgili izole `GPL-2.0-only` Ring 3 Driver Island sürecinin içine bağlanır (ortak `LinuxKPI` yasaktır).
  - *Uygulama uyumluluğu (`Kademe 3A`):* `dry-linux-abi` kişilik sunucusu + `libdry_posix` (`SEAL_RO_EXPORT`) tamamen Ring 3'te çalışır. **`dry-kernel` (`Ring 0`) Linux ABI/POSIX bağımlılıklarıyla asla kirletilmez.**
- **`Mock Driver Island` (`2C-0`) ile Gerçek `Linux Driver Island` (`2C-1` / `Faz 3B`) Arasındaki Fark:**

| Boyut | `Mock Driver Island` (`2C-0` — `DONE`) | Gerçek `Linux Driver Island` (`2C-1` / `Faz 3B` — `PLANNED`) |
| :--- | :--- | :--- |
| **Dil & Kaynak** | Saf `no_std` Rust (`dry-mock-drv.elf`, `305` satır) | Değiştirilmemiş upstream Linux C sürücü bileşeni (`H2`: örn. `e1000e` `200` sembol / `igc` `247` sembol, `GPL-2.0-only`) + Ring 3 `libdry_lx_net` |
| **Donanım & DMA** | `0` `PciDev` BAR, `0` DMA çerçevesi, `0` donanım kesmesi | `CapObjectKind::PciDev` (`SYS_DEV_MAP` UC BAR) + sabitlenmiş `DmaArena` (`SYS_DMA_ALLOC`, gölge `vmemmap` `struct page[]`, çöküşte `BME=0` + `FLR`) + `IrqCap`/MSI-X |
| **Olay Döngüsü** | Çift-kip (`drv_ep` veya `sq_notif` + `50 ms` bekçi) | `Milestone 2A-4a` `TCB`-bağlı birleşik `Endpoint + Notification` beklemesi (`0` zaman aşımı bekçisi) |
| **Kanıtladığı Şey** | Kontrol/veri ayrımı (`INV-ISL-01..04`), `HLT` uyanışı (`4.40 µs`), `#UD` sonrası tam CDT `revoke` (`5/5`) ve `1.42 ms` kurtarma | Gerçek C sürücüsünün Ring 3 uyumluluk katmanı üzerinde donanım MMIO/DMA/IRQ ile paket/komut taşıması |

### 7 Yaklaşımın Ayrı Ayrı Teknik Değerlendirmesi

1. **`Native DRY Driver` (`Kademe 1`):**
   - *Kanıtlanmış:* `PS/2 i8042` (`dry-input`), UEFI `GOP` (`dry-display`) ve `VirtIO 1.0 Blk` (`dry-blk` `783` satır + `IoRing` + `BME=0`) `90/90` QEMU testiyle çalışmaktadır.
   - *Deneysel Umut Vadeden:* Temel `VirtIO-Net`, `NVMe`, `VirtIO-GPU` ve Intel `00:02.0` için dar `dry-xe-gem` (yalnızca GuC/PPGTT gönderimi).
   - *Henüz Doğrulanmayan:* Fiziksel donanımda MSI-X + VT-d IOMMU altında `NVMe` / `Iris Xe` tarama çıkışı.
   - *Teknik Engeller & Bakım/Lisans:* Karmaşık PHY/Wi-Fi/GPU yığınlarını sıfırdan Rust ile yazmak imkânsızdır; ancak önyükleme-kritik sürücülerde `MIT OR GPL-2.0-only` tam egemenlik sağlar.
   - *Önerilen Entegrasyon:* Önyükleme, kök depolama (`VirtIO`/`NVMe`) ve birincil ekran taraması (`dry-display`) her zaman Kademe 1 Native Rust kalır.
2. **`DDE-Linux Driver Island` (`Kademe 2`, `GPL-2.0-only`):**
   - *Kanıtlanmış:* Faz 0 `H2`'de `18.528` satırlık Linux `igc` sürücüsü `%0` değişiklik ve `3.468` satırlık `libdry_lx_net` C çalışma zamanıyla (`v6.12 -> v6.18` farkı `38` satır) derlendi; `Exp-B3` denetiminde NIC/NVMe sürücülerinin `200–358` dış sembolle sınırlı kaldığı (`e1000e=200`, `igc=247`, `nvme=358`) ölçüldü; `2C-0` ise DRY üzerinde Broker + `IoRing` + `revoke` yaşam döngüsünü kanıtladı.
   - *Deneysel Umut Vadeden:* USB `xHCI` ve seçilmiş PCIe Wi-Fi sürücüleri.
   - *Henüz Doğrulanmayan:* `H2` `igc`/`e1000e` ikilisinin doğrudan `dry-kernel` `PciDev` + `DmaArena` üzerinde canlı paket taşıması (`Faz 3B`).
   - *Teknik Engeller & Bakım/Lisans:* Modern GPU yığınları (`i915=1.244`, `xe=1.213`, `amdgpu=1.108` sembol; `nvidia-uvm` `__folio_lock`/`HMM`) `< 10.000` LOC `ADR-006` tavanını yıkar. Ayrı süreç sınırı `GPL-2.0-only` lisansını izole eder (`ADR-005`).
   - *Önerilen Entegrasyon:* Yalnızca `< 400` dış sembollü, `0` DRM/HMM bağımlılıklı PCIe NIC/USB cihazlarda `Device Broker` + `IoRing` altında kullanılır.
3. **`Linux ABI / POSIX Uyumluluğu` (`Kademe 3A` — `dry-linux-abi`):**
   - *Kanıtlanmış:* `Exp-B1` DRY `Notification` (`233 cyc`) ve `Endpoint` (`1.971 cyc` `1-CPU` / `5.309 cyc` `2C-0` çapraz-kompartman) ilkellerinin VM çıkışından `40x–200x` ucuz olduğunu gösterdi.
   - *Deneysel Umut Vadeden:* Sanal makinesiz userspace `dry-linux-abi` kişilik sunucusu + `libdry_posix.so` (`futex -> Notification`, `epoll -> toplu rozet`, `SEAL_RO_EXPORT` paylaşımlı kütüphaneler).
   - *Henüz Doğrulanmayan:* `dry-kernel` üzerinde `XSAVEOPT`, `FaultEndpoint`, `ET_DYN` PIE, `ARCH_SET_FS` ve `fork()` CoW sayfalama.
   - *Teknik Engeller & Bakım/Lisans:* `fork`/`clone`/`mmap`/sinyal uç durumları; Linux çekirdek modülü veya `io_uring` çekirdek içi uzantıları isteyen yazılımlar kapsam dışıdır.
   - *Önerilen Entegrasyon (`ADR-011` Öncelik #1):* `Milestone 2A-4` çekirdek ilkellerinden hemen sonra `musl` statik/dinamik ELF64 ikilileriyle (`Faz 3A`) başlatılır.
4. **`Wine / Proton / Windows ABI Uyumluluğu` (`Kademe 3B` — `dry-ntos`):**
   - *Kanıtlanmış:* `dry-kernel` `swapgs` mimarisi kullanıcı `GS_BASE` yazmacını Win64 `TEB` için serbest bırakmaktadır; modern Wine + `DXVK`/`VKD3D-Proton` Win32/D3D11/12'yi userspace NT semantiği + Vulkan'a indirger.
   - *Deneysel Umut Vadeden:* `dry-linux-abi` olgunlaştıktan sonra Wine `ntdll.so` (`unixlib`) + Vulkan ICD bağlantısı (`Faz 5`).
   - *Henüz Doğrulanmayan:* DRY üzerinde PE32+ yükleme, `USER_GS` bağlam değişimi ve Vulkan/Venus boru hattı.
   - *Teknik Engeller & Bakım/Lisans:* Windows `.sys` (WDM/WDDM) çekirdek sürücüleri ve Ring 0 anti-cheat kesin olarak **reddedilmiştir** (`R-13`); Wine `LGPL-2.1+` ayrı süreçte uyumludur.
   - *Önerilen Entegrasyon:* Kesinlikle `dry-linux-abi` ve Vulkan yolu kararlılığa ulaşmadan **başlatılmaz** (`Faz 5`).
5. **`Dormant Linux microVM` (`Kademe 4` — `dry-vmm`):**
   - *Kanıtlanmış (`Exp-B2` KVM Deneyi):* `nohz=on` Linux `6.18.62` misafiri `HLT` uykusunda konak CPU'sunu **`%1.333`** seviyesine düşürür ve kapı ziliyle **`199.8–222.5 µs` (`p50`)** sürede uyanır; **ancak** VFIO DMA sabitlemesi (`-mem-prealloc`) `256 MiB` misafir için dahi **`309.41 MiB` `VmRSS`** fiziksel RAM'i kalıcı kilitler.
   - *Deneysel Umut Vadeden:* `NVIDIA RTX 3050 Ti Mobile` (`01:00.0`, IOMMU Grup 13, muxless `0302`) için değiştirilmemiş `nvidia-open 615.x` + kapalı kaynak Linux `libcuda.so`/`libnvidia-glcore.so` çiftini sürüm-kilitli çalıştırmak.
   - *Henüz Doğrulanmayan:* `dry-kernel` içinde minimal `VcpuCap` (`VMX`/EPT), fiziksel `VT-d` SLPT ve dizüstü `RTD3 D3cold` güç döngüsü (`Exp-B5` ve `Exp-B6` bilerek çalıştırılmadı).
   - *Teknik Engeller & Bakım/Lisans:* Her zaman açık bir VM bellek bütçesini ezer; bu yüzden varsayılan açılışta **kapalı (`0 B` RAM)** olmalı ve yalnızca dGPU kiralandığında açılmalıdır.
   - *Önerilen Entegrasyon:* Yalnızca `> 1.000` sembollü / `HMM` ayrık GPU'lar için Faz 4/`P5`'te isteğe bağlı (*opt-in*) etkinleştirilir.
6. **`GPU API Forwarding`:**
   - *Kanıtlanmış (`Exp-B4` & `2C-0`):* Tekil senkron RPC yönlendirme (`GVirtuS` tarzı işlem-başı çağrı) `0.1263 Mops/s` (`22.158 cyc/op`) darboğazına takılırken; `CAP = 32` `IoRingSharedPage` üzerinde 32'li gruplama konakta `3.1074 Mops/s` (`886 cyc/op`, `24.61x`), doğrudan DRY OS `2C-0` üzerinde ise **`859 cyc/op` (`6.14x`)** hızlanma sağlar.
   - *Deneysel Umut Vadeden:* **VirtIO-GPU `Venus`** (serileştirilmiş toplu Vulkan komut akışı + paylaşımlı `Order2MiB` `HOST3D` tarama tamponları).
   - *Henüz Doğrulanmayan / Engeller:* İnce taneli CUDA API yönlendirmesi kapalı sürücü güncellemelerinde kırılgan olduğundan tek başına reddedilmiştir; grafik için yalnızca toplu `Venus` protokolü benimsenmiştir.
7. **`Hybrid Adaptive Driver Islands` (`ADR-016` Seçilen Mimari):**
   - Tek tip DDE (`GPU`'da çöker) ve tek tip VM (`309 MiB+` sabitlenmiş RAM vergisi) uçlarını eleyerek tüm kademeleri ortak **`Device Broker` + `Endpoint` + `IoRingSharedPage` + `SharedSurface` + `PciDev`** sözleşmesi altında birleştirir (`Milestone 2C-0` ile kontrol/veri yaşam döngüsü doğrulandı).

---

## 4.7 Bilinen Riskler ve Teknik Borçlar (Önem Sırasıyla + Sonraki Doğrulama Adımları)

1. **`[ÇÖZÜLDÜ]` `R-18` — Önyükleme Modül Tablosu Tavanı (`16 -> 32`):** `MAX_LATCHED_MODULES = 32` (`kernel/src/limine.rs`), `MAX_BOOT_ENTRIES = 36` (`userspace/src/vfs_mem.rs`) yükseltildi ve taşma panik/red kapısı eklendi (`8572da3`; şu an `20/32` yuva kullanılıyor).
2. **`[P0]` `R-03` — Fiziksel IOMMU (`VT-d` / `AMD-Vi`), `MSI/MSI-X` ve `>4 GiB` BAR Eksikliği:** QEMU/KVM'de DMA güvenliği `iommu_pin_count == 1` + çöküşte `PCI Command.BME = 0` ile sağlanıyor; ancak çalışırken kötücül/hatalı bir fiziksel cihazın (`8086:9a49` veya `10de:25a0`) keyfi RAM'e DMA yazmasını yalnızca donanım IOMMU ikinci-seviye sayfa tabloları engeller. Konakta `DMAR` mevcut olsa da `/sys/class/iommu` boştur (`vtd_active = false`), ayrıca `10de:25a0` BAR1 `4 GiB` boyutundadır.
   - *Sonraki Doğrulama Adımı (`Milestone 2C-2`):* ACPI `DMAR` ayrıştırıcısı + `VT-d` BDF DMA sayfa tabloları, APIC `MSI/MSI-X` kesme yönlendirmesi ve 64-bit `>4 GiB` BAR eşlemesini yazıp QEMU `-device intel-iommu,intremap=on` ile negatif DMA kaçış testi koştur.
3. **`[P0]` `R-01 / R-02 / R-11` — Çekirdek `16 KiB` Yığın Bütçesi, FPU & Eşzamanlı Denetim Açıkları (`docs/AUDIT_2A4A.md`):** Sıcak çerçeve `rust_syscall_dispatch = 248 B` (`<= 256 B`) korunmuştur; ancak `docs/AUDIT_2A4A.md` içindeki açık `P1/P2` maddeleri (`AUD-K6` PI geri-hesaplama, `AUD-K7` NMI/#MC IST ayrımı, `AUD-K8` PCI FLR/BAR yeniden etkinleştirme, `AUD-K10..K12`, `AUD-U3/U4/U8`) sonraki döngüde kapatılmalıdır.
4. **`[P1]` `R-05 & R-16` — Gerçek Donanım (`Bare-Metal`) ve `ACPI S3` Açığı:** Tüm `94/94` testler QEMU/KVM üzerinde koşmaktadır (`QEMU/KVM != bare-metal`, `20 vCPU` `4C/8T` konakta aşırı-aboneliktir); `POWER_OP_SUSPEND_S3` `-ENOSYS` döner.
5. **`[P1]` `DRYFS_v1` & `H2` NIC Bağlantısı:** `DRYFS_v1` hiyerarşik alt-dizin/çok-yazıcılı stres testi henüz eksiktir; `H2` `igc`/`e1000e` NIC adası konak testinden DRY `PciDev` + `VT-d` + `MSI-X` üzerine taşınmayı beklemektedir (`Milestone 2C-3`).
6. **`[P1]` `R-17` — Hibrit Dizüstü GPU (`Iris Xe` + `RTX 3050 Ti`) Entegrasyonu:** `xe` (`1,213` UND) ve `nvidia` (`1,189` UND + `162` `os_*` + `81.31 MiB` GSP firmware) yığınları doğrudan Ring-3 shim içinde `VT-d` + `MSI-X` + DRM/GEM/TTM olmadan `BLOCKED` durumundadır (`docs/GPU_COMPATIBILITY_REPORT.md`).

---

## 4.8 Git ve Çalışma Ağacı Durumu

- **Mevcut Branch:** `main`
- **Tamamlanan Commit Zinciri (Kronolojik):**
  1. `fc557ba` .. `9dab7a6` — `Milestone 2A-1` (`P1`: `65/65`) | `ff31908` .. `85ec902` — `Milestone 2A-2` (`79/79`) | `0da9d9d` .. `2a361c3` — `Milestone 2A-3` (`85/85` + `5/5`)
  2. `8c9bc25` & `57d213d` — `Milestone 2B` (`88/88` + `5/5`) & `Track B` Uyumluluk Araştırması (`ADR-016`)
  3. `a6fe26b`, `812fddc`, `49b6682`, `9f9c575` — `Milestone 2C-0` (`90/90` + `5/5`) & Devir Teslimi
  4. **`8572da3`** — `Milestone 2A-4a` A1: `MAX_LATCHED_MODULES` `16 -> 32` & taşma reddi (`R-18`)
  5. **`b24adc7`** — `Milestone 2A-4a` A2: per-thread `XSAVEOPT64`/`XRSTOR64` + `FXSAVE64` FPU/SIMD izolasyonu
  6. **`ac340da`** — `Milestone 2A-4a` A3/A4: `FaultEndpoint`, `AUD-K1..K15` / `AUD-U1..U9` sertleştirme
  7. **`1345bcd`** — `Milestone 2C-1`: Gerçek Linux `v6.18` `virtio-rng.c` Driver Island + 3-yuvalı PCI DMA (`docs/LINUX_DRIVER_ISLAND_2C1.md`)
  8. **`1879128`** — `Priority C`: Ring-3 `VirtIO-GPU` 2D scanout + canlı GOP fallback + `H3` konak GPU/PRIME denetimi (`docs/GPU_COMPATIBILITY_REPORT.md`)
  9. **`HEAD` (`docs(ascension-2.0)`)** — `tools/qemu_boot_test.py` P6+P7 tam regresyon (`94/94` QEMU + `5/5` Matris), `docs/EXPERIMENTS_ASCENSION_2_0.md` ve kanonik belge senkronizasyonu (`ADR-017`, `ADR-018`)
- **Beklenen `git status -s` Çıktısı ve Bilerek Korunan Dosyalar:**
  ```text
   M experiments/h1-sched/src/runqueue.rs
   M experiments/h1-sched/src/sync.rs
  ```
  - **Neden Temiz Değil?** Yukarıdaki iki dosya kullanıcı tarafından korunması emredilen ön-mevcut çalışma ağacı değişiklikleridir. `dry-kernel`, `userspace`, `drivers`, `experiments/h3-gpu-compat`, `tools` ve `docs` altındaki tüm Ascension 2.0 değişiklikleri temiz biçimde commit edilmiştir. Bu iki dosyayı **asla silme, sıfırlama veya commit'e dahil etme.**

---

## 4.9 Kesinleştirilmiş Sonraki Milestone Sırası (`2C-2` -> `Faz 4`)

1. **Aşama 1 (`DONE` — `8572da3`, `b24adc7`, `ac340da`):** `Milestone 2A-4a` tamamlandı (`MAX_LATCHED_MODULES=32`, `XSAVEOPT64`/`FXSAVE64`, `FaultEndpoint`, `AUD-K15`).
2. **Aşama 2 (`DONE` — `1345bcd` & `1879128`):** `Milestone 2C-1` (`v6.18` `virtio-rng.c` Driver Island) ve `Priority C` (`VirtIO-GPU` 2D + `GOP Fallback` + `H3` konak GPU/PRIME denetimi) tamamlandı (`94/94` QEMU + `5/5` Matris).
3. **Aşama 3 (`PLANNED` — `Milestone 2C-2`, Sırada #1):**
   - Çekirdek ACPI `DMAR` ayrıştırıcısı + donanım **`VT-d` per-BDF DMA sayfa tabloları**, APIC **`MSI/MSI-X`** kesme yönlendirmesi, **64-bit `>4 GiB` BAR** eşleme desteği ve `TCB`-bağlı birleşik `Endpoint+Notification` beklemesi (`dry-mock-drv`/`dry-lx-rng` bekleme yoklamasının kaldırılması) + `docs/AUDIT_2A4A.md` açık P1 maddelerinin kapatılması.
4. **Aşama 4 (`PLANNED` — `Milestone 2C-3` / `Faz 3A–3B`):**
   - `H2` `igc`/`e1000e` NIC adası, `VirtIO-GPU` `VIRGL`/`Venus` + `dry-display` `PRIME` swapchain ve `FaultEndpoint` + `ET_DYN` PIE üzerinde Ring 3 `dry-linux-abi` (`musl` ELF64).
5. **Aşama 5 (`PLANNED` — `Faz 4 / P5` Bare-Metal & GPU):**
   - Fiziksel donanımda `VT-d` altında `Xe/GuC` KMS dar shim veya IOMMU-izole MicroVM (`Venus` / `RTD3`).

---

## 4.10 Bağımsız Kod İncelemesi İçin Kritik Dosyalar (`< 1.000` Satır/Dosya)

| Alt Sistem | Kritik Dosyalar (Doğrulanmış Yol) | Bağımsız Denetçinin İncelemesi Gereken Noktalar |
| :--- | :--- | :--- |
| **1. `ADR-007` Güvenlik & `CSpace` CDT `Revoke`** | `kernel/src/security.rs`, `kernel/src/cspace/{cap,mod,revoke,objects}.rs` | `authorize_cspace_fast`, `slot.in_flight_ops` (`SeqCst`) Dekker pini, çift-nesil ABA kalkanı, root-only destroy (`AUD-K3`) ve `reserve/commit_root_slot` (`AUD-K9`). |
| **2. `VSpace`, `W^X`, SMP TLB & `PMM`** | `kernel/src/vspace/{mod,tlb}.rs`, `kernel/src/pmm/mod.rs`, `kernel/src/uaccess.rs` | `W^X` ve `SEAL_RO_EXPORT`, `UNMAP_4K` kind denetimi (`AUD-K2`), `active_cpus` TLB shootdown, `PER_CPU_MAGAZINES` + `iommu_pin_count`, `CR4.SMAP`. |
| **3. `H1` SMP Zamanlayıcı & FPU/SIMD İzolasyonu** | `kernel/src/sched/{mod,state,context,runqueue}.rs`, `kernel/src/fpu.rs` (`351`), `userspace/src/bin/dry_fpu_test.rs` (`234`) | `dry_switch_to` yığın sahipliği, `SeqCst` çiti, `#NM` ilk kullanım sıfırlaması (`CLEAN_FCW`/`CLEAN_MXCSR`), `CR0.TS` + `XSAVEOPT64`/`FXSAVE64` geçişi. |
| **4. Syscall Girişi, IPC & `FaultEndpoint`** | `kernel/src/syscall/{entry,mod,cold}.rs`, `kernel/src/ipc/{endpoint,endpoint_state,notif}.rs`, `kernel/src/proc/fault.rs` (`193`), `kernel/src/audit_tests.rs` (`350`), `userspace/src/bin/dry_fault_test.rs` (`251`) | `rust_syscall_dispatch` (`248 B`), `AUD-K4`/`K5`/`K15` notif & kill-safe IPC beklemeleri, `FAULT_MSG_LABEL` sahtecilik koruması, `FAULT_BUDGET = 16` ve `Reply` `RIP` doğrulaması. |
| **5. 3-Yuvalı `PciDev`, DMA `BME=0` & `VaultCtrl`** | `kernel/src/irq/mod.rs`, `kernel/src/pci.rs` (`543`), `kernel/src/pci_cfg.rs` (`288`), `kernel/src/vault_ctrl.rs` | `owner_cspace` CAS (`AUD-K13b`), `PCI_OBJ_VIRTIO_{BLK,RNG,GPU}` izolasyonu, `quiesce_slot_for_cspace` `BME=0` -> DMA unpin sırası (`R-03`), `IRQF_ONESHOT`. |
| **6. Depolama (`IoRing`, `DRYFS_v1`), `Vault` & `Broker`** | `userspace/src/{ioring,persist_fs,vault_crypto,island_proto}.rs`, `userspace/src/bin/{dry_blk,dry_vault,dry_broker}.rs` (`dry_broker`: `708`), `userspace/src/supervisor.rs` (`690`) | `AUD-U1/U2/U5/U6/U7/U9` kapanmaları, SPSC `Acquire/Release`, `Device Broker` kontrol/veri ayrımı ve `RIGHT_REVOKE` alt-ağaç iptali. |
| **7. Linux `v6.18` `virtio-rng` Driver Island (`2C-1`)** | `drivers/lx-virtio-rng-island/{Cargo.toml,src/main.rs}` (`434`), `drivers/lx-virtio-rng-island/cshim/lx_virtio_shim.c` (`523`), `vendor/linux-drivers/v6.18/virtio_rng/virtio-rng.c` | `GPL-2.0-only` crate izolasyonu (`0` çekirdek Linux satırı), `kzalloc` `DEVICE_DMA` sıfır-kopya arenası, `sg_init_one` sınırları ve `#UD` -> `gen=2` kurtarma. |
| **8. `VirtIO-GPU` 2D, GOP Fallback & Konak GPU (`Priority C`)** | `userspace/src/{gpu_proto,virtio_gpu,gpu_audit,wm,wm_ipc,compositor}.rs` (`15`/`541`/`385`/`852`/`511`/`892`), `userspace/src/bin/{dry_gpu,dry_display}.rs` (`363`/`768`), `experiments/h3-gpu-compat/{audit_gpu_stacks.py,prime_fence_probe.c}` | `back_caps` `RIGHT_READ\|RIGHT_MAP` salt-okunur eşleme (`VSPACE_MAP_OP_2M_RO`), `GPU_FALLBACK_TO_GOP` kurtarması, `unauth_ipc_rejects=7` rozet kapıları ve `VT-d`/`>4 GiB BAR1` engelleri. |
