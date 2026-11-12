# DRY OS — Sıfır-Bağlam Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

> **Son Güncelleme:** 12 Kasım 2026 (`Milestone 2C-0` Kapanışı — Eksiksiz Sıfır-Bağlam Devir Teslimi)  
> **İlke:** *Minimum Overhead. Maximum Useful Work. Correctness first. Security always. Performance measured.*

---

## SONRAKİ OTURUM — BURADAN BAŞLA

1. **Projenin Mevcut Durumu:** DRY OS; `x86-64` üzerinde çalışan, yetenek (*capability*) yönelimli, `no_std` Rust mikroçekirdeğidir (`dry-kernel` + `16` adet statik `CPL=3` ELF64 kullanıcı alanı ikilisi). Faz 0 (`H1` TLA+/Loom + `H2` `igc` PoC), Faz 1 (`1A..1C` + `Opt Pass v1`) ve Faz 2 alt aşamaları **`Milestone 2A-1`**, **`2A-2`**, **`2A-3`**, **`2B`** (Ring 3 `VirtIO-Blk`, kilitsiz `IoRing`, çökme-güvenli CoW `DRYFS_v1` `/persist`, `DRY Vault` `PBKDF2-SHA256` oturum kilidi + $O(1)$ `lease_epoch`, `PCI Command.BME = 0` çöküş susturması) ile **`Milestone 2C-0`** (`ADR-016` Minimal Adaptive Driver Island PoC: `Device Broker` + `Mock Driver Island`) tamamlanmış ve doğrulanmıştır. Gerçek Linux veya Windows ikili/sürücü uyumluluğu **henüz uygulanmamıştır** (`PLANNED`).
2. **Son Doğrulanmış Commit:** Kod, test ve deney verisi commit'i **`a6fe26bbacf72aea5cb827517fdab0bd28aae2e7`** (`a6fe26b` — `feat(island): add Milestone 2C-0 device broker and mock driver island PoC`); `main` üzerindeki `HEAD` ise bu devir belgesini mühürleyen doğrudan ardıl `docs(handover)` commit'idir (`HEAD~1 == a6fe26b`, kod farkı `0` satır).
3. **Son Doğrulanmış Testler (`12 Kasım 2026`, `a6fe26b` üzerinde bizzat çalıştırıldı):**
   - `cargo clippy -D warnings` (`dry-userspace` + `dry-kernel`): **`0` uyarı / `PASS`**.
   - Tam deterministik QEMU/KVM regresyon paketi (`python3 tools/qemu_boot_test.py`): **`90/90 PASS`** (`56` Faz 0–1C + `9` P1 + `14` P2 + `6` P3 + `3` P4 + `2` P5/`2C-0`; tüm senaryolarda `leaked_frames = 0`).
   - SMP (`1/2/4/8/20 vCPU`) & RAM (`256M/512M/1G/2G`) masaüstü matrisi (`python3 tools/qemu_boot_test.py --p3-matrix`): **`5/5 PASS`** (`leaked_frames = 0`).
   - Çekirdek yığın çerçevesi denetimi (`objdump -d`): `rust_syscall_dispatch = 248 B` (`+0 B`), `sys_cold_dispatch = 264 B`, `sys_irq_ack = 40 B`.
4. **Aktif Milestone:** `Milestone 2C-0` tamamlandı ve kapatıldı (`DONE`). Henüz başlatılmış yarım bir milestone yoktur; sıradaki sınırlı hedef **`Milestone 2A-4a`** (`MAX_LATCHED_MODULES` `16 -> 32` genişletmesi + FPU/SIMD `XSAVEOPT`/`XRSTOR` + `FaultEndpoint` altyapısı) olarak önerilmektedir (bkz. Bölüm `4.9`).
5. **Açık Riskler (Özet):**
   - **`R-18` (Acil Ön Koşul):** Limine önyükleme modül dizisi `16/16` doludur (`kernel/src/limine.rs:429`); 17. ELF64 ikilisi eklenmeden önce tavan `16 -> 32` yükseltilmelidir.
   - **`R-11` (`P0`):** Sabit `16 KiB` çekirdek yığını bütçesi; sıcak `rust_syscall_dispatch` çerçevesi `248 B` korunmalıdır.
   - **`R-03` & `R-05` (`P0/P1`):** Fiziksel Intel VT-d / AMD-Vi IOMMU ve gerçek silikon (`bare-metal`) doğrulaması henüz yapılmamıştır (`QEMU/KVM != bare-metal`).
   - **`R-16` & `R-17` (`P1/P2`):** `ACPI S3` uyku kipi dürüstçe `-ENOSYS` döner; ayrık GPU MicroVM (`RTX 3050 Ti` `01:00.0`) VFIO RAM sabitlemesi (`309.41 MiB+`) ve `RTD3` pil kısıtı nedeniyle varsayılan kapalı (*opt-in*) tutulmalıdır.
6. **İlk Okunması Gereken Belgeler (Sırasıyla):**
   1. `HANDOVER.md` (bu belge)
   2. `ROADMAP.md` ve `docs/PRODUCT_ROADMAP.md`
   3. `ARCHITECTURE.md` ve `DECISIONS.md` (`ADR-001` .. `ADR-016`)
   4. `RISK_REGISTER.md` (`R-01` .. `R-18`)
   5. `docs/EXPERIMENTS_2B.md` ve `docs/EXPERIMENTS_2C0_ISLAND_POC.md`
   6. `docs/COMPATIBILITY_ISLANDS_RESEARCH.md` ve `docs/COMPATIBILITY_EXPERIMENTS_DATA.md`
7. **İlk Çalıştırılması Gereken Komutlar:**
   ```bash
   git status -s && git log -n 5 --oneline
   python3 tools/qemu_boot_test.py --build-only
   python3 tools/qemu_boot_test.py --island-only
   python3 tools/qemu_boot_test.py --p4-only
   ```
   *(Not: `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` içindeki önceden mevcut yerel değişikliklere **kesinlikle dokunma**, `git add` veya `git checkout` uygulama.)*
8. **Bir Sonraki Mantıklı Geliştirme Adımı (`Milestone 2A-4a`):** Önce `MAX_LATCHED_MODULES` (`16 -> 32`) ve `MAX_BOOT_ENTRIES` (`20 -> 36`) tavanlarını genişlet; ardından hem `DRY Desktop` (`P2`) hem `dry-linux-abi` (`Faz 3`) için zorunlu çekirdek ön koşulları olan **FPU/SIMD `XSAVEOPT`/`XRSTOR` bağlam izolasyonunu** ve **`FaultEndpoint` (Ring 3 sayfalayıcı + yabancı `SYSCALL` yönlendirme kapısı)** mekanizmasını uygula.

### Yeni Oturum Ajan Başlangıç Talimatı (Doğrudan Kopyalanabilir)

> DRY OS geliştirmesine devam etmeden önce HANDOVER.md dosyasını tamamen oku. Referans verilen mimari, karar, risk ve deney belgelerini incele. Mevcut Git durumunu doğrula. Son tamamlanan milestone'u ve test sonuçlarını belirle. Daha önce çözülmüş problemleri yeniden ele alma; doğrulanmış mimari kararları gerekçesiz değiştirme. Önce mevcut durumun tutarlılığını doğrula, ardından belgelenmiş bir sonraki sınırlı milestone üzerinden ilerle. Eksik veya çelişkili bilgi bulursan varsayım üretmek yerine kod ve testlerle doğrula.

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

1. **Çekirdek Girişi, UEFI Önyükleme, CPU/IDT ve ACPI (`kernel/src/`):**
   - Önyükleme & Limine modül kenetleme (`MAX_LATCHED_MODULES = 16`): `kernel/src/main.rs`, `kernel/src/limine.rs`
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
   - Statik `ET_EXEC` ELF64 yükleyici, `ProcessSlot` geri kazanımı ve süpervizör çağrıları: `kernel/src/proc/elf.rs`, `kernel/src/proc/mod.rs`, `kernel/src/proc/supervisor.rs` (`tests.rs`, `bench.rs`)
   - `IrqCap` (`IoApicHwCache`, fırtına kelepçesi), `IoPortCap` beyaz listesi ve önyükleme oturum seçicisi (`session_mode`): `kernel/src/irq/mod.rs`, `kernel/src/irq/ioport.rs`, `kernel/src/irq/session.rs` (`tests.rs`, `bench.rs`)
   - Ring 3 kök süpervizör (`dry-init.elf`) ve servis orkestrasyonu: `userspace/src/bin/dry_init.rs`, `userspace/src/supervisor.rs`, `userspace/src/sup_services.rs`, `userspace/src/sup_ctx.rs`, `userspace/src/sup_audit.rs`, `userspace/src/init_legacy.rs`, temel test ikilileri `userspace/src/bin/{user_hello,user_peer,dry_echo}.rs`
7. **PCI Keşfi ve Sabitlenmiş DMA Denetimi (`kernel/src/pci.rs`):**
   - VirtIO 1.0 Modern PCI yetenek taraması (`COMMON`/`NOTIFY`/`ISR`/`DEVICE` MMIO BAR eşleme), `SYS_DMA_ALLOC` (`DEVICE_DMA | NO_DELEGATE`, `iommu_pin_count == 1`) ve süreç yıkımında `quiesce_pci_for_cspace` (`PCI Command.BME = 0`): `kernel/src/pci.rs`
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
11. **`Device Broker` ve `Mock Driver Island` (`Milestone 2C-0` PoC — `userspace/src/`, `tools/`):**
    - Protokol, rozetler, deterministik `SplitMix64 + CRC32` dönüşümü ve gecikme örnekleyici: `userspace/src/island_proto.rs`
    - İzole Ring 3 test sürücü adası (`0x0209`) ve yetenek-aracılı `Device Broker` (`0x0008`): `userspace/src/bin/dry_mock_drv.rs`, `userspace/src/bin/dry_broker.rs`
    - Yaşam döngüsü/güvenlik denetimi (`mode=25`), mikro-ölçüt motoru (`mode=26`) ve QEMU E2E doğrulayıcıları: `userspace/src/island_audit.rs`, `tools/qemu_p5_island_validators.py`, `tools/qemu_boot_test.py`

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
| **`Milestone 2C-0` (`Island PoC`)** | **`a6fe26b`** (`HEAD~1`) | `0` yeni syscall ile `Device Broker` (`dry-broker.elf` `0x0008`) + izole `Mock Driver Island` (`dry-mock-drv.elf` `0x0209`), nesil-damgalı `Endpoint`/`IoRing`/`Notification` kiralaması, `HLT` uyku/uyanma (`0` boşta syscall), `#UD` çöküşü -> CDT `revoke` -> `1.41 ms` kurtarma, bayat yetki reddi (`4/4`) ve `PMM` per-CPU şarjör muhasebesi düzeltmesi | **`90/90` QEMU + `5/5` Matris** | Saf CPU mock dönüşümüdür; gerçek Linux sürücüsü, PCIe MSI-X, FLR veya hypervisor içermez; önyükleme modül dizisi `16/16` dolmuştur (`R-18`). |

*(Tamamlanmamış `Milestone 2A-4`, `Milestone 2C` ve `Faz 3..5` aşamaları kesin olarak `PLANNED` durumundadır.)*

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
   - *Çözüm:* Telemetri satırları `< 115 B` atomik satırlara bölündü ve QMP kabuk dosya adlarında `-` kullanıldı.

---

## 4.5 Test Durumu ve Yeniden Üretim

### Doğrulanmış Ortam, Sürümler ve Ön Koşullar
- **Konak Makine:** Intel Core i5-11300H (`4C/8T`, `~3.114 GHz` TSC), `16 GiB` RAM, `Linux 7.2.14-zen1-1-zen x86_64`, `/dev/kvm` okuma/yazma izni açık.
- **Araç Zinciri (`tools/phase1-toolchain-lock.json` ile SHA-256 Kilitli):**
  - `rustc 1.99.1 (58cf8f9c3 2026-10-13)` & `cargo 1.99.1` + `.toolchains/sysroot` (`rust-src 1.99.1`)
  - `QEMU emulator version 11.1.3` (`qemu-system-x86_64`) + UEFI OVMF `/usr/share/edk2/x64/OVMF_{CODE,VARS}.4m.fd`
  - `Limine 10.8.5` (`BOOTX64.EFI` commit `7a9013f305de0bea1f6310e76e7baba30499fef0`, protokol `6e85ddd6f928`)
  - `Python 3.14.10` (`xorriso` **gerekmez**; `qemu_boot_test.py` doğrudan QEMU UEFI VVFAT `fat:rw:...` sürücüsü kullanır)

### Tekrarlanabilir Komutlar ve `a6fe26b` (`12 Kasım 2026`) Doğrulanmış Sonuçları

```bash
# 1. Araç zinciri doğrulaması + Clippy (-D warnings: userspace & kernel) + Release ELF64 derlemesi
python3 tools/qemu_boot_test.py --build-only
# -> SONUÇ (a6fe26b): PASS (0 uyarı, 16 userspace ELF64 + dry-kernel derlendi)

# 2. Yalnızca doğrudan cargo clippy kontrolleri
cargo clippy --manifest-path userspace/Cargo.toml --target x86_64-unknown-none --bins -- -D warnings
cargo clippy --manifest-path kernel/Cargo.toml --target kernel/x86_64-dry-kernel.json \
  -Zbuild-std=core,compiler_builtins -Zbuild-std-features=compiler-builtins-mem -- -D warnings
# -> SONUÇ (a6fe26b): PASS (0 uyarı)

# 3. Milestone 2C-0 Adaptive Driver Island PoC E2E + performans/batch paketi (~12 sn)
python3 tools/qemu_boot_test.py --island-only
# -> SONUÇ (a6fe26b): 2/2 PASS (p5a + p5b, leaked_frames=0; JSON: tools/compat_bench/results_poc_2c0_island.json)

# 4. Milestone 2B VirtIO-Blk kalıcılık, SIGKILL güç kesintisi kurtarma & DRY Vault/DMA güvenliği (~22 sn)
python3 tools/qemu_boot_test.py --p4-only
# -> SONUÇ (a6fe26b): 3/3 PASS (p4a + p4b + p4c, leaked_frames=0)

# 5. SMP (1/2/4/8/20 vCPU) & RAM (256M/512M/1G/2G) Masaüstü Matrisi (~25 sn)
python3 tools/qemu_boot_test.py --p3-matrix
# -> SONUÇ (a6fe26b): 5/5 PASS (leaked_frames=0, 20 vCPU / 2 GiB free_4k=516.741)

# 6. Tam Deterministik QEMU/KVM Regresyon Paketi (~240 sn)
python3 tools/qemu_boot_test.py
# -> SONUÇ (a6fe26b): 90/90 PASS (56 Faz 0-1C + 9 P1 + 14 P2 + 6 P3 + 3 P4 + 2 2C-0; 0 FAIL)

# 7. Çekirdek sıcak/soğuk syscall yığın çerçevesi denetimi
objdump -d -M intel kernel/target/x86_64-unknown-none/release/dry-kernel | \
  grep -A8 -E "(rust_syscall_dispatch|sys_cold_dispatch|sys_irq_ack).*:"
# -> SONUÇ (a6fe26b): rust_syscall_dispatch=248 B (0xf8), sys_cold_dispatch=264 B (0x108), sys_irq_ack=40 B (0x28)

# 8. Track B konak uyumluluk deneylerini yeniden üretme (isteğe bağlı)
python3 tools/compat_bench/audit_driver_classes.py
python3 tools/compat_bench/microvm_dormant_bench.py
rustc -O tools/compat_bench/ioring_batch_bench.rs -o /tmp/ioring_batch_bench && /tmp/ioring_batch_bench
```

---

## 4.6 Adaptive Driver Islands Araştırmasının Durumu (`ADR-016`)

> **Kesin Dürüstlük Beyanı:** DRY OS üzerinde henüz **gerçek Linux ELF uygulaması, Windows PE uygulaması veya gerçek yabancı çekirdek sürücüsü uyumluluğu çalıştırılmamıştır**. Tamamlanan çalışma; kanıta dayalı mimari araştırma (`docs/COMPATIBILITY_ISLANDS_RESEARCH.md`), 4 konak/çekirdek deneyi (`EXP-B1..B4`: `docs/COMPATIBILITY_EXPERIMENTS_DATA.md`) ve `Milestone 2C-0` yaşam döngüsü prototipidir (`docs/EXPERIMENTS_2C0_ISLAND_POC.md`).

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

1. **`[P0]` `R-18` — Önyükleme Modül Tablosu Tavanı (`16/16` Dolu):** `kernel/src/limine.rs:429` (`MAX_LATCHED_MODULES = 16`) ve `kernel/src/irq/session.rs:202` dizileri `dry-broker.elf` ve `dry-mock-drv.elf` ile tam kapasiteye ulaştı.
   - *Sonraki Doğrulama Adımı:* Yeni ELF eklemeden önce sabitleri `16 -> 32` (`vfs_mem.rs` `MAX_BOOT_ENTRIES` `20 -> 36`) yükselt ve `limine.rs` içine `module_count <= MAX_LATCHED_MODULES` açık panik/assertion kapısı ekle.
2. **`[P0]` `R-03` — Fiziksel IOMMU (`VT-d` / `AMD-Vi`) Eksikliği:** QEMU/KVM'de DMA güvenliği `iommu_pin_count == 1` + çöküşte `PCI Command.BME = 0` ile sağlanıyor; ancak çalışırken kötücül/hatalı bir cihazın rastgele fiziksel adrese DMA yazmasını yalnızca donanım IOMMU ikinci-seviye sayfa tabloları engeller.
   - *Sonraki Doğrulama Adımı:* ACPI `DMAR` tablosu ayrıştırıcısını yaz ve önce QEMU `-device intel-iommu,intremap=on`, ardından `P5` fiziksel TigerLake donanımında negatif DMA kaçış testi koştur.
3. **`[P0]` `R-01 / R-02 / R-11` — Çekirdek `16 KiB` Yığın Bütçesi ve Eşzamanlı `Revoke` Yarışları:** Sıcak yolda derleyici satır-içi (*inlining*) şişmesi `16 KiB` çekirdek yığınını taşırabilir; yeni capability türleri `in_flight_ops` ve TLB shootdown sırasına uymalıdır.
   - *Sonraki Doğrulama Adımı:* Her çekirdek değişikliğinde `objdump` ile `rust_syscall_dispatch == 248 B` ve önyükleme kanarya denetimini (`verify_cpu_stack_canaries_fast`) zorunlu koştur.
4. **`[P1]` `R-05 & R-16` — Gerçek Donanım (`Bare-Metal`) ve `ACPI S3` Açığı:** Tüm `90/90` testler QEMU/KVM üzerinde koşmaktadır (`20 vCPU` testi `4C/8T` konakta aşırı-aboneliktir); `POWER_OP_SUSPEND_S3` `-ENOSYS` döner.
   - *Sonraki Doğrulama Adımı:* `P5`'te USB Live Boot imajıyla fiziksel makinede önyükleme, gerçek APIC/IOAPIC/GOP doğrulaması ve A/B performans ölçümü yap.
5. **`[P1]` `DRYFS_v1` Kapsam Sınırı ve Çökme Tutarlılığı:** `persist_fs.rs` çift A/B süperblok + 4 KiB CoW ile tek-blok/küçük dosya atomikliğini ve `SIGKILL` kurtarmasını (`p4b`) kanıtladı; ancak henüz hiyerarşik alt-dizin ağacı, çok-bloklu büyük dosya CoW kapsamları ve çok-yazıcılı eşzamanlılık stres testi yoktur.
   - *Sonraki Doğrulama Adımı:* `dry-linux-abi` öncesinde çok-bloklu CoW extent yapısını ve rastgele kesme enjeksiyonlu 1.000-işlemlik güç kesintisi *fuzz* testini ekle.
6. **`[P1]` `R-17` — Hibrit Dizüstü GPU (`Iris Xe` + `RTX 3050 Ti`) Entegrasyonu:** `01:00.0` `RTX 3050 Ti` muxless olup IOMMU Grup 13'ü `00:01.0` Kök Portuyla paylaşır ve `D0` boşta `8–15 W` pil tüketir.
   - *Sonraki Doğrulama Adımı:* İkincil test makinesinde veya Live USB üzerinde `FLR` + ACPI `RTD3` `D3cold <-> D0` 50-döngü kararlılık testini (`Exp-B5`) çalıştır.
7. **`[P2]` `Milestone 2C-0` Deneysel Altyapı ve Test Hassasiyeti Sınırları:** `dry_mock_drv.rs` `IoRing` kipinde (`park_mode = 1`) kontrol kapı zili (`ISLAND_DOORBELL_CTL`) geldikten sonra mesaj gelmezse DoS önlemek için `50 ms` zaman aşımı kullanır; ayrıca `p5b` benchmark sayıları konak yüküne duyarlıdır.
   - *Sonraki Doğrulama Adımı:* `Milestone 2A-4`'te `TCB`-bağlı `Notification` (`Endpoint + Notification` tek bekleme noktası) eklendiğinde `dry_mock_drv` içindeki çift-kip beklemeyi tek birleşik olay döngüsüne indirge.

---

## 4.8 Git ve Çalışma Ağacı Durumu

- **Mevcut Branch:** `main`
- **Bugün (`12 Kasım 2026`) Yapılan İlgili Commit Zinciri (Kronolojik):**
  1. `fc557ba` .. `9dab7a6` — `Milestone 2A-1` (`P1 First Interaction`: IOAPIC, `IrqCap`, PS/2 klavye, kabuk, GOP compositor; `65/65`)
  2. `ff31908` .. `85ec902` — `Milestone 2A-2` (`DRY Desktop Preview`: PS/2 fare `IRQ12`, yazılımsal imleç, çok-pencereli WM; `79/79`)
  3. `0da9d9d`, `ce61fea`, `58ccda6`, `2a361c3` — `Milestone 2A-3` (`Launcher`, 4 yerel uygulama, `dry-vfs`, `dry-power` ACPI S5/Reset/`HLT`; `85/85` + `5/5`)
  4. `8c9bc257f68322b4d7c4a0cf8a7b2b254ae89d7f` (`8c9bc25`) — `Milestone 2B` (`VirtIO-Blk` `IoRing`, CoW `DRYFS_v1`, `DRY Vault`, `BME=0`; `88/88` + `5/5`)
  5. `57d213dd12323154e9e04cc57b717d00f2246fb0` (`57d213d`) — `Track B` Uyumluluk Araştırması (`ADR-016`, `EXP-B1..B4`)
  6. **`a6fe26bbacf72aea5cb827517fdab0bd28aae2e7` (`a6fe26b`) — Bu Görev (`Milestone 2C-0` Kod, Test ve Deney Verisi Commit'i)**
  7. **`HEAD` (`docs(handover)` Commit'i, `HEAD~1 == a6fe26b`)** — Yalnızca bu `HANDOVER.md` dosyasını son commit hash'leriyle mühürleyen devir commit'i.
- **Beklenen `git status -s` Çıktısı ve Bilerek Korunan Dosyalar:**
  ```text
   M experiments/h1-sched/src/runqueue.rs
   M experiments/h1-sched/src/sync.rs
  ```
  - **Neden Temiz Değil?** Yukarıdaki iki dosya Faz 0 deneylerinden kalma, kullanıcı tarafından korunması açıkça emredilen yerel çalışma ağacı değişiklikleridir. `dry-kernel`, `userspace`, `tools` ve `docs` altındaki **tüm** geliştirme değişiklikleri temiz biçimde commit edilmiştir. Bu iki dosyayı **asla silme, sıfırlama veya commit'e dahil etme.**
- **Oluşturulan / Güncellenen Rapor ve Deney Dosyalarının Yolları:**
  - `docs/EXPERIMENTS_2C0_ISLAND_POC.md` ve `tools/compat_bench/results_poc_2c0_island.json` (`Milestone 2C-0`)
  - `docs/EXPERIMENTS_2B.md`, `docs/EXPERIMENTS_2A3.md`, `EXPERIMENTS.md` (`Bölüm L..P`)
  - `docs/COMPATIBILITY_ISLANDS_RESEARCH.md` ve `docs/COMPATIBILITY_EXPERIMENTS_DATA.md` (`tools/compat_bench/results_exp_b1..b4*.json`)

---

## 4.9 Sonraki Oturumun Yol Haritası (Somut Uygulama Sırası)

1. **Adım 1 — Başlangıç Doğrulaması (`~1 dk`):**
   - `git status -s` (`yalnızca 2 experiments/h1-sched dosyası M olmalı`), `git log -n 3 --oneline` ve `python3 tools/qemu_boot_test.py --island-only` + `--p4-only` çalıştırarak `a6fe26b` tabanının yerel ortamda yeşil olduğunu teyit et.
2. **Adım 2 — Önerilen Küçük ve Sınırlı Milestone (`Milestone 2A-4a: Core Execution & Trap Prerequisites`):**
   - **Görev A (`R-18` Kapasite Artırımı):** `kernel/src/limine.rs` (`MAX_LATCHED_MODULES: 16 -> 32`), `kernel/src/irq/session.rs` (`[CAP_NULL; 32]`) ve `userspace/src/vfs_mem.rs` (`MAX_BOOT_ENTRIES: 20 -> 36`) sabitlerini güncelle.
   - **Görev B (`XSAVEOPT` / `XRSTOR` FPU/SIMD Bağlam İzolasyonu):** `CR4.OSFXSR | CR4.OSXMMEXCPT | CR4.OSXSAVE` etkinleştirerek `TCB` başına hizalı FPU/SSE/AVX kayıt alanı ekle (hem `DRY Desktop` vektör çizimi hem `dry-linux-abi` ELF64 ikilileri için zorunlu bağımlılık).
   - **Görev C (`TCB`-Bağlı `Notification` + `FaultEndpoint` Temeli):** Tek iş parçacıklı sunucuların `Endpoint + Notification` olaylarını birlikte beklemesini sağla ve Ring 3 `#PF` / yabancı `SYSCALL` (`RAX < 0x1000` kişilik bayrağı) tuzaklarını yetkili `FaultEndpoint`'e yansıt.
3. **Beklenen Testler ve Başarı Kriterleri:**
   - Mevcut `90/90` QEMU regresyon + `5/5` SMP/RAM matrisinin `%100` korunması (`leaked_frames = 0`, `rust_syscall_dispatch == 248 B`).
   - Çok-iş-parçacıklı SSE/XMM yazmaç izolasyon testi (`0` çapraz-süreç XMM bozulması) ve `FaultEndpoint` sayfa hatası / yabancı `syscall` yakalama-devam ettirme E2E testi (`+3..4` yeni deterministik QEMU testi).
4. **Kesinlikle Erken Başlanmaması Gereken İşler:**
   - `XSAVEOPT` ve `FaultEndpoint` bitmeden `glibc`/dinamik Linux ELF veya `Wine`/`Win32` çalıştırmaya kalkışma.
   - Fiziksel `VT-d` IOMMU ve `VcpuCap` (`VMX`/EPT) tasarlanmadan `dry-kernel` içine hypervisor, VFIO veya `NVIDIA` sürücü kodu ekleme.
   - `liquid-glass` görsel efektlerini Ring 0'a sızdırma veya düşük donanım *fallback* olmadan ağır alfa/bulanıklık döngüleri ekleme.
