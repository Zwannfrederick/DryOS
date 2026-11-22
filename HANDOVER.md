# DRY OS — Sıfır-Bağlam Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

> **Son Güncelleme:** 22 Kasım 2026 (`Project Ascension 9.0` on `ascension-9.0`: `30/32` Modül [`29` ELF + `dry-bin.tar`], M1 `5187395` Stabilizasyon & `-j` Paralel Koşucu, M2 `88441d5` Frame2M VFS Dinamik Exec & `/bin` USTAR, M3 `233a26d` `dry-net` TCP/IP & `dry-linux-abi` Statik ELF64, M4 `35ea9cb` xHCI Hub Cascade, `i2c-hid` SDK Adası, Pano IPC, Kabuk Boru/Yönlendirme & Editör Yolları, M5 HWVAL v0.9.0 Live USB & Raporlar — **`170/170` QEMU `-j 4` + `--p19-only` `6/6` + `--p18-only` `21/21` + `--p17-only` `4/4` + `--p3-matrix` `5/5` + `7/7` SDK PASS**)  
> **İlke:** *Minimum Overhead. Maximum Useful Work. Correctness first. Security always. Performance measured.*

---

## SONRAKİ OTURUM — YARINKİ AJANLAR (GPT / CLAUDE / KODLAMA AJANI) BURADAN BAŞLA

1. **Projenin Mevcut Durumu ve Kesinleşmiş Mimari Karar (`ADR-016` .. `ADR-027`):**
   - DRY OS; `x86-64` üzerinde çalışan, yetenek (*capability*) yönelimli, `no_std` Rust mikroçekirdeğidir (`dry-kernel` + **`30/32`** Limine modülü: `29` statik `CPL=3` ELF64 ikilisi + `/bin` `dry-bin.tar` USTAR arşivi [`112,640 B`, `5` ELF]; `MAX_LATCHED_MODULES = 32`).
   - `Ascension 8.0` ve **`Project Ascension 9.0` (`ascension-9.0`, M1..M5)** tamamlanmıştır: `256 MiB` 3-bölümlü HWVAL v0.9.0 GPT USB (`DRYOS_ESP` + `DRYOS_PERSIST` + `DRYOS_LOGS` FAT32), 5-kapılı xHCI USB MSC köken doğrulaması (`0x1f`) ve SHA-256 read-back doğrulamalı FAT32+LFN günlük yazıcısı, ortak `hwdiag` çerçevesi, Driver Island ABI v1 SDK + `dry-nv-island.elf` (fail-closed Seviye 1 `BLOCKED`/`N/A`), `Frame2M` VFS dinamik ELF yürütme + hiyerarşik VFS (`pwd/cd/mkdir/rmdir/cp/mv/exec`), kullanıcı alanı `dry-net.elf` TCP/UDP/DHCP/DNS/HTTP/1.0 yığını (`ifconfig/ping/nslookup/httpget`), çekirdek `SYSCALL` yönlendirmesi + `dry-linux-abi.elf` (statik `ET_EXEC` `lx-hello.elf`, `13` Linux syscall, `rc=42`), xHCI 1-seviye hub zinciri (`userspace/src/usb/hub.rs`), dış-ağaç korumalı `drivers/i2c-hid-island/` (`ALLOW_PHYSICAL_I2C_WRITES = false`, `i2c-hid-test`, `#UD gen=2`), compositor pano IPC (`182/183`, `clip set/get`, `Ctrl+C`/`Ctrl+V`), kabuk boru/yönlendirme (`>`, `>>`, `| grep`/`head`/`wc`) ve editör özel yol desteği (`open editor <path>`, `Ctrl+O`).
   - **Kesin Epistemik Dürüstlük & Mimari Kural (`ADR-005`, `ADR-016..027`):** **`kernel/src/` (`Ring 0`) içinde `0` satır C/Linux ABI/GPL/OpenRM veya UI kodu vardır** (`0` `.c`/`.h`/`.S`/`.s`); `0` yeni çekirdek syscall eklenmiştir (`rust_syscall_dispatch = 248 B <= 256 B`); hiçbir kaynak dosya `1,000` satıra ulaşmaz (en yüksek `963` satır); `0` yeni harici Cargo paketi eklenmiştir. Tüm çalışma anı testleri `VERIFIED_QEMU` (`llvmpipe` dahildir) veya `PROTOTYPE` düzeyindedir; **`VERIFIED_HARDWARE` sayısı `0`dır**. Fiziksel Lenovo `82K1` boot (`IMPLEMENTED_UNTESTED`), fiziksel Wi-Fi (`AX201`) / `r8168` (`UNSUPPORTED`), fiziksel LPSS I2C-HID MMIO yazımları (`PARTIAL`, `ALLOW_PHYSICAL_I2C_WRITES = false`), dinamik `ET_DYN`/glibc Linux ELF'leri (`UNSUPPORTED`), Intel `Iris Xe` (`PARTIAL`), NVIDIA `RTX 3050 Ti` (`BLOCKED`/`UNSUPPORTED`) ve AMD-Vi SLPT (`UNSUPPORTED`) dürüstçe sınırlandırılmıştır.
2. **Son Doğrulanmış Commit Zinciri (`ascension-9.0`, taban `ascension-8.0`):**
   - **`5187395`** — `Ascension 9.0 M1`: `900+` LOC sıcak dosya sadeleştirme, paralel QEMU koşucusu (`-j N`) & `p19a` stabilite soak testi
   - **`88441d5`** — `Ascension 9.0 M2`: `Frame2M` VFS dinamik ELF spawning (`SPAWN_FLAG_VFS_ELF`), `/bin` USTAR arşivi (`dry-bin.tar`) & hiyerarşik VFS (`p19b`)
   - **`233a26d`** — `Ascension 9.0 M3`: `dry-net.elf` TCP/UDP/DNS/HTTP yığını (`p19c`) ve çekirdek `SYSCALL` yönlendirme + `dry-linux-abi.elf` statik ELF64 çalışma zamanı (`p19d`)
   - **`35ea9cb`** — `Ascension 9.0 M4`: xHCI 1-seviye hub zinciri, korumalı `drivers/i2c-hid-island/` SDK adası (`p19e`), compositor pano IPC (`182/183`), kabuk boru/yönlendirme ve editör özel yolları (`p19f`)
   - **`HEAD`** — `Ascension 9.0 M5`: HWVAL v0.9.0 Live USB (`dryos-ascension9-hardware-validation.img`, SHA-256 `f3614385...`), `170/170` `-j 4` regresyonu, `ASCENSION_9_0_{FINAL_REPORT,VALIDATION_MATRIX}.md`, `ADR-027`, `R-27` ve güncel operatör belgeleri
3. **Son Doğrulanmış Test Kanıtları (`22 Kasım 2026`, `ascension-9.0`):**
   - `python3 tools/qemu_boot_test.py -j 4` (**`170/170 PASS`** in **`342.79 s`**, `p19` `6/6`, `p18` `21/21`, `p17` `4/4`, `p16` `4/4` dahil, `leaked_frames=0`).
   - `python3 tools/qemu_boot_test.py --p3-matrix` (**`5/5 PASS`**: `1cpu/256M` `5.67 s`, `2cpu/256M` `5.58 s`, `4cpu/512M` `5.51 s`, `8cpu/1G` `5.92 s`, `20cpu/2G` `9.96 s`, `leaked_frames=0`).
   - `python3 tools/qemu_boot_test.py --p19-only` (**`6/6 PASS`**: `p19a` `11.153 s`, `p19b` `15.382 s`, `p19c` `7.799 s`, `p19d` `6.816 s`, `p19e` `5.541 s`, `p19f` `11.441 s`, `leaked_frames=0`).
   - `python3 tools/build_live_usb_image.py --build --hwval --verify-boot` (**`PASS`**: `kernel/build/dryos-ascension9-hardware-validation.img`, `268,435,456 B` / `256 MiB`, `30/32` modül, `DRYOS_LOGS` `194,527` sektör FAT32, `6.254 s`, `13` dosya + `manifest.sha256` + `fsck.fat -n`, SHA-256 `f3614385538a3ba953b3fdec3317504254b3d2a679b1b954f77315f0108bf463`; eski `ascension{4..8}` imajları korundu).
   - `cargo test --offline` (`dry-driver-sdk` **`5/5 PASS`** + `i2c-hid-island --lib` **`2/2 PASS`**) & hijyen (`max_loc=963 < 1000`, `rust_syscall_dispatch=248 B <= 256 B`, `0` C/GPL dosyası `kernel/src/`).
4. **Kesinleştirilmiş Sonraki Milestone Sırası:**
   1. **Fiziksel Lenovo `82K1` İlk Boot Doğrulaması (`READY FOR USER PHYSICAL EXECUTION`):** Kullanıcı tarafından [docs/HARDWARE_VALIDATION_GUIDE.md](docs/HARDWARE_VALIDATION_GUIDE.md) ile `dryos-ascension9-hardware-validation.img` (`f3614385...`) USB testi ve ardından `python3 tools/retrieve_dryos_logs.py` ile `/hardware-tests/<session-id>/` raporlarının incelenmesi.
   2. **`Faz 3A–3B Devamı` (`PLANNED`):** Dinamik `ET_DYN` PIE/glibc `dry-linux-abi` desteği ve fiziksel `r8168`/`AX201`/LPSS I2C donanım sürücü adaları.
   3. **`Faz 4 / P5` (`PLANNED`):** Intel Iris Xe GGTT/ring/DPCD `165 Hz` modesetting, AMD-Vi SLPT/IOTLB ve cross-VM `DMA-BUF` / VFIO üzerinden RTX 3050 Ti GPU MicroVM.
5. **Açık Riskler (Özet):**
   - **`R-27`:** Fiziksel `AX201` Wi-Fi / `r8168` NIC, fiziksel LPSS I2C-HID MMIO yazımları (`ALLOW_PHYSICAL_I2C_WRITES = false`), dinamik `ET_DYN` Linux ELF sınırı ve gerçek `82K1` bare-metal ölçüm ihtiyacı.
   - **`R-22 / R-25 / R-26`:** Fiziksel Lenovo `82K1` çoklu-DRHD `VT-d`, xHCI zamanlaması, eDP `165 Hz`, NVIDIA Seviye 2–3 önkoşulları ve cross-VM `DMA-BUF` sıfır-kopya sınırı (`T5` yalnızca konak POSIX SHM içindedir).
   - **`R-23`:** Fiziksel `Iris Xe` (`8086:9a49` `PARTIAL`), `RTX 3050 Ti` (`10de:25a0` Seviye 1 `BLOCKED`) ve AMD `IVRS` SLPT (`UNSUPPORTED`).
6. **İlk Okunması Gereken Belgeler:**
   1. `docs/ASCENSION_9_0_FINAL_REPORT.md`, `docs/ASCENSION_9_0_VALIDATION_MATRIX.md`, `docs/HARDWARE_VALIDATION_GUIDE.md`, `docs/HARDWARE_DIAGNOSTIC_COMMANDS.md`
   2. `docs/ASCENSION_8_0_FINAL_REPORT.md`, `docs/GPU_COMPATIBILITY_STATUS.md`, `docs/PERFORMANCE_COMPARISON.md`
   3. `HANDOVER.md`, `ARCHITECTURE.md`, `DECISIONS.md` (`ADR-001..027`), `ROADMAP.md`, `RISK_REGISTER.md`
7. **İlk Çalıştırılması Gereken Komutlar:**
   ```bash
   git status -s && git log --oneline -n 10
   python3 tools/qemu_boot_test.py -j 4
   python3 tools/qemu_boot_test.py --p19-only
   python3 tools/qemu_boot_test.py --p3-matrix
   cargo test --offline --manifest-path drivers/dry-driver-sdk/Cargo.toml
   cargo test --offline --manifest-path drivers/i2c-hid-island/Cargo.toml --lib
   python3 tools/build_live_usb_image.py --build --hwval --verify-boot
   ```
   *(Kritik Kural: `experiments/h1-sched/src/{runqueue,sync}.rs`, `docs/*.png`, `docs/screenshots/**`, ilgisiz `tools/compat_bench/*.json` ve `drivers/*/Cargo.lock` içindeki önceden mevcut değişikliklere **kesinlikle dokunma**, `git add` veya `git checkout` uygulama.)*

### Yarınki Ajanlar İçin Başlangıç Talimatları

- **Geliştirme Ajanı İçin:**
  > DRY OS geliştirmesine devam etmeden önce `docs/ASCENSION_7_0_FINAL_REPORT.md`, `docs/ASCENSION_7_0_VALIDATION_MATRIX.md`, `docs/PHYSICAL_USB_TEST_OPERATOR_GUIDE.md` ve `DECISIONS.md` (`ADR-024`, `ADR-025`) belgelerini oku. `git status -s` (`experiments/h1-sched/` ve `docs/*.png` korunacak) ile depo durumunu, ardından `qemu_boot_test.py` (`143/143`), `--p17-only` (`4/4`), `--p3-matrix` (`5/5`), `probe_virtio_gpu_3d.py --live-qemu` ve `build_live_usb_image.py --verify-boot` ile `Ascension 7.0` tabanını doğrula. Sıradaki adımlar operatör eşliğindeki fiziksel `82K1` boot doğrulaması ile `Faz 3A–3B` (`dry-linux-abi` + `H2` NIC adaları) / `Faz 4` (GGTT+eDP / AMD-Vi SLPT / cross-VM `DMA-BUF`) hedefleridir.
- **Bağımsız Kod İnceleme Ajanı İçin:**
  > Salt-okunur denetim yap. `kernel/src/iommu/{dmar,slpt,vtd,vtd_domain}.rs`, `userspace/src/{virtio_gpu,virtio_gpu_3d,intel_tgl_prep,gpu_svc/*}.rs`, `drivers/lx-nv-openrm-probe/nv_os_shim_check.c`, `tools/microvm_gpu_proto/ipc_bench_a7.c`, `tools/qemu_p17_ascension7_validators.py` ve `docs/ASCENSION_7_0_FINAL_REPORT.md` dosyalarını güvenlik sınırları (`0` host müdahalesi, `0` sahte `VERIFIED_HARDWARE`, `KERNEL_GPL_ZERO_OK`, `leaked_frames=0`) açısından satır satır incele.

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
13. **`Milestone 2C-2` Donanım Temelleri & `2A-4a` P1 Kapanışı (`8d230df`):**
    - `VT-d` & `DMAR`: `kernel/src/iommu/{mod,dmar,slpt,vtd,tests}.rs`
    - `MSI/MSI-X`, `>4 GiB` `BAR` & `PAT4` `WC`: `kernel/src/irq/msi.rs`, `kernel/src/pci_bar.rs`, `kernel/src/pci_window.rs`, `kernel/src/vspace/pte.rs`
    - `TCB`-bağlı `Notification` & donanım E2E: `kernel/src/ipc/bound_notif.rs`, `userspace/src/bin/dry_hw_test.rs`, `tools/qemu_p8_hw_validators.py`, `tools/compat_bench/p8_hw_foundations_latest.json`
14. **`Track B` (`DRY Glass` v1) & `DRY Glass 2.0` (`--p9-only`, `--p10-only`):**
    - Saf `#![no_std]` TrueType AA & OFL-1.1 yazıtipleri: `userspace/src/ttf/{mod,sfnt,raster,atlas,layout}.rs`, `assets/fonts/{DrySans-*,DryMono-*,OFL.txt,ATTRIBUTION.md}`, `tools/build_font_subsets.py`
    - Ortak `ui/` bileşen kiti & `DRY Glass` compositor: `userspace/src/ui/{mod,canvas,icons,theme,layout,widgets,text_edit,containers}.rs`, `userspace/src/glass/{mod,theme,typography,icons,blur,chrome,overlays,dock_bar,app_kit}.rs`, `userspace/src/apps/*`
    - Çoklu çözünürlük/DPI E2E doğrulama: `tools/qemu_p9_glass_validators.py`, `tools/qemu_p10_glass2_validators.py`, `tools/compat_bench/{results_p9_glass,results_dry_glass_2_0}.json`, `docs/screenshots/glass_*.png` (`10` PNG) + `docs/screenshots/glass2_*.png` (`12` PNG), `docs/DRY_GLASS_2_0_REPORT.md`
15. **`Project Ascension 4.0` (`DRY Glass 2.1`, `bochs.c` DRM Adası & GPU/Live USB):**
    - `userspace/src/glass/{window_strip,overview}.rs`, `tools/qemu_p11_glass21_validators.py` (`9` PNG `glass21_*.png`), `docs/DRY_GLASS_2_1_REPORT.md`, `drivers/lx-drm-bochs-island/`, `userspace/src/{lx_compat_meta,lx_drv_proto,lx_drm_audit}.rs`, `tools/qemu_p12_lx_compat_validators.py` (`--p12-only`), `docs/LINUX_COMPAT_RUNTIME_REPORT.md`
16. **`Project Ascension 5.0` (`v0.5.0` — M1..M5, Team A & OSS Lisanslama):**
    - **M1 Çekirdek Sertleştirme (`9ca5e41`):** `kernel/src/{serial,panic_fb,pmm/meta,pci,pci_cfg,pci_slots,pci_bar,pci_window}.rs`, `kernel/src/iommu/{slpt,vtd,tests}.rs`, `kernel/src/irq/session.rs`, `userspace/src/abi_ext.rs`
    - **M2 (`dry-usb` + `dry-nvme` RO + CLI):** `userspace/src/usb/{mod,xhci,hid,msc}.rs`, `userspace/src/bin/{dry_usb,dry_nvme,dry_vfs}.rs`, `userspace/src/{nvme_ro,sup_hw_services,sup_services,supervisor,shell_sys_cmds,shell_ext,shell}.rs`, `tools/qemu_p13_usb_persist_validators.py` (`--p13-only`)
    - **M3 (`libdry-lx-compat` v2 & `8139cp.c+mii.c`):** 6 modül `drivers/libdry-lx-compat/src/lx_{core_sync_wq,pci_dma_irq,firmware_pm,dmabuf_fence,drm_compat,net_compat}.c`, `drivers/lx-net-8139cp-island/src/main.rs`, `vendor/linux-drivers/{v6.18/8139cp,MANIFEST.json}`, `tools/lx_compat/{COMPAT_CATALOG.json,lx_driver_manager.py,fetch_8139cp_sources.py}`, `userspace/src/{broker_lifecycle,lx_island_audit,bin/dry_broker}.rs`, `tools/qemu_p14_lx_net_validators.py` (`--p14-only`)
    - **Team A & OSS Lisanslama (`1e15333`, `3a878b7`):** `assets/{brand,icons/apps,icons/system,ASSET_MANIFEST.json}`, `tools/build_vector_assets.py`, `userspace/src/ui/{mod,brand,app_icons,theme,icons,canvas,vector_assets_sys,vector_assets_apps}.rs`, `userspace/src/display_banner.rs`, `tools/qemu_p15_brand_icons_validators.py` (`--p15-only`, `15` PNG), `docs/{DESIGN_SYSTEM,ASSET_LICENSING_AND_INVENTORY}.md`, `LICENSE-{APACHE,MIT,GPL-2.0}`, `LICENSING.md`
    - **M4 (`82K1` Envanter/FW) & M5 (`160 MiB` GPT Live USB):** `tools/{hw_inventory_lenovo_15ihu6,gpu_fw_validators,provision_local_firmware,build_live_usb_image}.py`, `tools/compat_bench/{lenovo_15ihu6_inventory,results_live_usb_boot}.json`, `docs/{HARDWARE_INVENTORY_LENOVO_15IHU6,INTEL_IRIS_XE_VALIDATION,NVIDIA_RTX_3050TI_VALIDATION,LIVE_USB_OPERATOR_GUIDE,ASCENSION_5_0_VALIDATION_MATRIX}.md`

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
| **`Priority C` (`Ring-3 VirtIO-GPU 2D, GOP Fallback & H3 Audit`)** | **`1879128`..`479674b`** | İzole Ring-3 `dry-gpu.elf` (`1AF4:1050`) `1280x800` `B8G8R8X8_UNORM` (`present_p50=1.097 ms`), `#UD` (`vec=6`) çöküşünde canlı `[DRY-WM] GPU_FALLBACK_TO_GOP`, `gen=2` (`8.727 ms` kurtarma, `docs/p7_virtio_gpu_scanout.png`, `leaked_frames=0`), `H3` konak GPU/UND/firmware/PRIME fence denetimi | **`94/94` QEMU + `5/5` Matris + `H3`** | `VirtIO-GPU` 2D kapsamındadır (`Virgl` 3D yok); bare-metal GPU `BLOCKED`. |
| **`Milestone 2C-2` (`Track A: VT-d, MSI/MSI-X, >4 GiB BAR & TCB Notif`)** | **`8d230df`** | ACPI `DMAR` (`dmar_unit=5`) + `VT-d` 4-seviye `SLPT` (`vtd_rogue_faults=1`), PCI `MSI/MSI-X` (`0x40..=0x5F`), 64-bit `>4 GiB` `BAR` + `PAT4` `WC`, `TCB`-bağlı `Notification` (`msi_bound_p50_cyc=76,037` / `24.42 us`) ve `AUD-K6/K7/K8`, `AUD-U3/U4/U8` kapanışı | **`95/95` QEMU + `5/5` Matris (`--p8-only`)** | QEMU `-device intel-iommu` üzerinde doğrulandı; fiziksel bare-metal henüz test edilmedi. |
| **`Track B` (`DRY Glass` v1)** | **`6078bff`** | `userspace/src/glass/*`, `SharedPixelSurfaceHeader`, `dry_editor.rs` + `dry_calc.rs` (`23/32` modül), `1024x768..1920x1080` `VirtIO-GPU` & canlı `GOP` kurtarma | **`98/98` QEMU + `5/5` Matris + `10` PNG** | bitmap 8x16 UI ve uygulama-özel çizim içeriyordu (`DRY Glass 2.0`'da değiştirildi). |
| **`DRY Glass 2.0` (`G2-1..G2-5`)** | **`a0bab8c..20f0561`** | Saf `#![no_std]` TrueType AA (`DrySans`/`DryMono`, SIL OFL 1.1), SDF vektör tuval + `ui/` ortak bileşen kiti (`100%/125%/150%` DPI), `Obsidian`/`Daylight`/`HC` buzlu compositor, `6` yenilenmiş yerel uygulama & `--p10-only` | **`101/101` QEMU + `5/5` Matris + `12` PNG (`leaked_frames=0`)** | `Clean/Diagnostic Boot` ve `VirtIO-GPU` 3D/Virgl `PLANNED` kapsamındadır. |
| **`Project Ascension 4.0`** | **`2ed966b` .. `a1de3a6`** | `DRY Glass 2.1` (`Adaptive Window Strip`, `Alt+W`, `IconRail`), `libdry-lx-compat` statik C DRM runtime (`.ko` reddi) + `v6.18` `bochs.c` (`0x0212`), `8086:9a49`/`10de:25a0` ön-denetim + `nv_os_shim_check.c` | **`105/105` QEMU (`--p11/p12-only`) + `11` PNG (`leaked_frames=0`)** | Fiziksel `Iris Xe`/`RTX 3050 Ti` modeset `BLOCKED`. |
| **`Project Ascension 5.0` (`v0.5.0`)** | **`9ca5e41..3a878b7` + Wave 2/3** | M1 (`32G` PMM, UART-sız `probe_com1_uart` + `DIAG_RING`, `panic_fb`, `< 4 KiB` BAR, çoklu-DRHD `VT-d` erteleme + RMRR, `live_usb=1`), M2 (`dry-usb` HID/BOT, salt-okunur `dry-nvme`, `12+` CLI), M3 (6-dosyalı `libdry-lx-compat`, 15-adımlı broker, `8139cp.c+mii.c`), M4 (`82K1` envanter + FW/WPR2/PRIME), Team A (`118` SVG, `200%` DPI), `Apache-2.0 OR MIT`, M5 (`160 MiB` GPT Live USB, `27/32` modül) | **`135/135` QEMU/OVMF + `5/5` Matris + `4 GiB` RAM (`8.361s`) + Live USB (`5.491s`) & Host RO PASS** | Fiziksel `82K1` Gates 3–7 **`READY FOR USER PHYSICAL EXECUTION`**; `i915`/`xe`/OpenRM modeset `BLOCKED`. |

*(Tamamlanmamış **`Faz 3A–3B` (`dry-linux-abi` `musl` ELF64 & `H2` `igc`/`e1000e`/`r8168`)** ve **`Faz 4..5` (`Bare-Metal` Gates 3–7 / `Win32`)** aşamaları `PLANNED` / `READY FOR USER PHYSICAL EXECUTION` durumundadır.)*

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

### Tekrarlanabilir Komutlar ve `Ascension 5.0` (`17 Kasım 2026`, `v0.5.0`) Doğrulanmış Sonuçları

```bash
# 1. Araç zinciri + Clippy (-D warnings: userspace, 3 GPL adası & dry-kernel) + 27 modül Release ELF64
python3 tools/qemu_boot_test.py --build-only
python3 tools/build_vector_assets.py --verify             # 118 SVGs + rustc -D warnings PASS

# 2. Upstream Linux v6.18/v6.12 kaynak bütünlüğü & M3 birim testleri
python3 tools/lx_compat/lx_driver_manager.py verify --all # 56/56 dosya SHA-256 OK
python3 tools/lx_compat/test_lx_driver_manager.py         # 5/5 OK

# 3. Project Ascension 5.0 QEMU/OVMF kapıları & 160 MiB GPT Live USB
python3 tools/qemu_boot_test.py --p8-only                 # VT-d DMAR/SLPT, MSI, >4G & <4K BAR PASS
python3 tools/qemu_boot_test.py --p13-only                # USB xHCI HID/BOT, NVMe RO, 12 CLI: 25/25 PASS
python3 tools/qemu_boot_test.py --p15-only                # 118 SVGs, 100..200% DPI: 4/4 PASS (15 PNG)
python3 tools/qemu_boot_test.py --p14-only                # M3 8139cp.c+mii.c SLIRP UDP + gen=2: 1/1 PASS (3.95s)
python3 tools/qemu_boot_test.py --smp-only 2 --mem 4G     # 4 GiB RAM PMM boot: PASS (8.361s, 4020 MiB)
python3 tools/build_live_usb_image.py --verify-boot       # 160 MiB GPT (27 modül): PASS (5.491s)

# 4. Salt-okunur Lenovo 15IHU6 (82K1) donanım & GPU FW/WPR2/PRIME doğrulayıcıları
python3 tools/hw_inventory_lenovo_15ihu6.py               # PASS (24 PCI, 32 GiB, 8250.nr_uarts=0)
python3 tools/gpu_fw_validators.py --self-test            # PASS (dmc 2.12, guc 70.1.1, gsp WPR2, 22/162)

# 5. Tam QEMU/OVMF paketi (135/135 deterministik kontrol PASS, leaked_frames=0)
python3 tools/qemu_boot_test.py
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

1. **`[ÇÖZÜLDÜ]` `R-18` — Önyükleme Modül Tablosu Tavanı (`27/32`):** `MAX_LATCHED_MODULES = 32` (`27/32` modül aktif: `dry-usb`, `dry-nvme`, `dry-lx-net` eklendi; `MAX_BOOT_ENTRIES = 36`).
2. **`[QEMU DOĞRULANDI -> Gate 6 Kullanıcı]` `R-03` & `R-22` — `VT-d`, `RMRR`, `MSI/MSI-X`, `>4 GiB` & `<4 KiB` `BAR`:** `slpt.rs` (`406`) + `vtd.rs` (`768`), çoklu-DRHD aktif-GOP ertelemesi (`dmar.drhd_count > 1`), `premap_unit_rmrrs`, 39-bit SLPT, `preserve_active_gop` ve alt-sayfa `page_suboff` QEMU'da doğrulandı (`--p8-only`); fiziksel `82K1` Gate 6 kullanıcı yürütmesine hazırdır.
3. **`[QEMU/Statik -> Gate 3–5 Kullanıcı]` `R-20` & `R-21` — `INV-DISK-SAFE-01/02` & `8250.nr_uarts=0`:** `dry-nvme` `0` yazma opkodu, `dry-usb` Bölüm 2/`storage=ro` çiti (`--p13-only` `25/25`), `probe_com1_uart` + `16 KiB` `DIAG_RING` + `panic_fb.rs` + `limine.conf` `serial: yes` kaldırma uygulandı; fiziksel USB flash/boot Gates 3–5 kullanıcı yürütmesine hazırdır.
4. **`[P0 -> P1 KAPATILDI]` `R-01 / R-02 / R-11`:** `rust_syscall_dispatch = 248 B` (`<= 256 B`), `boot_stack = 32 KiB`, `NMI` `IST2` / `#MC` `IST3`.
5. **`[P1]` `R-05, R-16 & R-19` — Bare-Metal, `ACPI S3` & Fiziksel GPU (`Iris Xe` / `RTX 3050 Ti`):** `160 MiB` GPT Live USB `--verify-boot` (`5.491s`) ve `82K1` salt-okunur M4 envanter/FW doğrulayıcıları (`tgl_dmc`/`tgl_guc`/`gsp_ga10x` WPR2, `22/162` `os-interface.h`) `PASS`; fiziksel modeset **`BLOCKED`**, `S3 = -ENOSYS`, `H2` `igc`/`e1000e`/`r8168` ve `dry-linux-abi` `PLANNED`.

---

## 4.8 Git ve Çalışma Ağacı Durumu

- **Mevcut Branch:** `ascension-8.0` (`ascension-7.0` @ `b0387ab` temelinden çatallandı)
- **Tamamlanan Commit Zinciri (Kronolojik):**
  1. `fc557ba` .. `9f9c575` — `Milestone 2A-1..2C-0` (`90/90` QEMU + `5/5` Matris)
  2. `8572da3` .. `479674b` — `Ascension 2.0`: `2A-4a`, `2C-1` (`v6.18 virtio-rng.c`) & `Priority C` (`94/94` QEMU)
  3. **`8d230df` .. `a1de3a6`** — `Ascension 3.0 & 4.0`: `Milestone 2C-2` (`--p8-only`), `DRY Glass` v1/2.0/2.1 (`--p9..p11-only`), `libdry-lx-compat` + `bochs.c` (`--p12-only`, `105/105` QEMU + `5/5` Matris)
  4. **`9ca5e41`..`3c823f4`** — `Ascension 5.0` (`v0.5.0`): M1 çekirdek sertleştirme, M2 `dry-usb`/`dry-nvme` (`--p13-only`), M3 `libdry-lx-compat` v2 + `8139cp` (`--p14-only`), Team A `118` SVG/`200%` DPI (`--p15-only`), `Apache-2.0 OR MIT`, `160 MiB` GPT Live USB (`46c96b3c...`).
  5. **`cba0d9f`..`f104e0e`** — `Ascension 6.0` (`139/139` QEMU + `--p16-only` `4/4`): Fail-closed `IommuOps` + `IVRS`, `MCFG`/`ECAM` `64` PCI, `SYS_DMA_CTL` SG (`IOVA != HPA`) & `2 MiB WC`, `dry-gpu-svc`, decoupled `libdry-lx-compat`, `fw_store`, MicroVM IPC prototipi, Vyukov/xHCI düzeltmeleri, `dryos-ascension6-live-usb.img` (`2c85fee6...`).
  6. **`5673c9c`..`b0387ab`** — `Ascension 7.0` (`SILICON REALITY`, `143/143` QEMU + `--p17-only` `4/4`): M-A 12-maddeli sertleştirme, `82K1` profil/rehber + `[DRY-IOMMU] UNIT`/`[DRY-PCI-TOPO]`, `intel_tgl_prep.rs`, `171`-API NVIDIA GA107 + `53.53 MiB` GSP denetimi, `.toolchains/virgl-env/` (`llvmpipe`) VirtIO-GPU 3D, yatay dirty-rect kırpma (`-54.4%` px) + `T1..T5` GPU IPC paketi, `--p17-only` `4/4`, `dryos-ascension7-live-usb.img` (`6165c54f...`).
  7. **`c7faf46`..`HEAD` (`ascension-8.0`):** M1 (`c7faf46`: `256 MiB` 3-bölümlü HWVAL USB + `DRYOS_LOGS` FAT32 + 5-kapılı xHCI/GUID + `hwdiag` + `15` `dry` komutu + `retrieve_dryos_logs.py` + `p18a`/`p18b`/`9/9 p18c`), M2 (`30bec64`: mimari/güvenlik denetimleri & çekirdek/userspace düzeltmeleri + `p18d`), M3 (`a883d1a`: Driver Island ABI v1 + `drivers/dry-driver-sdk/` + `dry-sdk-demo.elf` `#28/32` + `p18e`), M4 (`af957da`: `ALLOW_PHYSICAL_GPU_WRITES=false` + zorunlu fail-closed NVIDIA Seviye-1 `dry-nv-island.elf` `#29/32` + `nvidia-smi` / `dry gpu ...` + `nvidia_exp/` 7+1 + `p18nv`/`p18f`/`6/6 p18g` + `ADR-026`), M5 (`HEAD`: HWVAL USB `c267a74e...` + `PERFORMANCE_COMPARISON.md` §4 + `HARDWARE_*` & `ASCENSION_8_0_*` belgeleri + `164/164` QEMU + `21/21` `--p18-only`).
- **Beklenen `git status -s` ve Bilerek Korunan Dosyalar:** `experiments/h1-sched/src/runqueue.rs`, `experiments/h1-sched/src/sync.rs` ve `docs/*.png` / `docs/screenshots/` ön-mevcut kirli dosyalar ile `drivers/{dry-driver-sdk,sdk-example-pci}/Cargo.lock` kilit dosyaları **asla silinmez veya commit'e dahil edilmez.**

---

## 4.9 Kesinleştirilmiş Sonraki Milestone Sırası (`Bare Metal` -> `Faz 3–4`)

1. **Tamamlanan (`Ascension 8.0`):** `164/164` birleşik QEMU, `--p18-only` `21/21`, `--p17-only` `4/4`, `--p3-matrix` `5/5` (`20cpu/2G` `6.63s`), `dry-driver-sdk` `5/5` ve `256 MiB` 3-bölümlü HWVAL USB (`dryos-ascension8-hardware-validation.img`, `--hwval --verify-boot` `7.316s`, SHA-256 `c267a74e...`; `ascension{4..7}` imajları korundu).
2. **Sırada #1 (`READY FOR USER PHYSICAL EXECUTION`):** Kullanıcı tarafından [docs/HARDWARE_VALIDATION_GUIDE.md](docs/HARDWARE_VALIDATION_GUIDE.md) doğrultusunda Lenovo `82K1` üzerinde manuel USB doğrulama ve ardından `python3 tools/retrieve_dryos_logs.py` ile günlük incelemesi.
3. **Sırada #2 (`PLANNED` — `Faz 3A–3B` & `Faz 4 / P5`):** AMD-Vi SLPT/IOTLB desteği, VT-d Interrupt Remapping + VFIO/VT-x MicroVM (`NVIDIA Level 2–3`), cross-VM `DMA-BUF`, `H2` `igc`/`e1000e`/`r8168` NIC adaları, `dry-linux-abi` (`musl` ELF64) ve dar `Xe/GuC` + eDP `165 Hz` KMS.

---

## 4.10 Bağımsız Kod İncelemesi İçin Kritik Dosyalar (`< 1.000` Satır/Dosya)

| Alt Sistem | Kritik Dosyalar (Doğrulanmış Yol) | Bağımsız Denetçinin İncelemesi Gereken Noktalar |
| :--- | :--- | :--- |
| **1. `ADR-007` Güvenlik & `CSpace` CDT `Revoke`** | `kernel/src/security.rs`, `kernel/src/cspace/{cap,mod,revoke,objects}.rs` | `authorize_cspace_fast`, `slot.in_flight_ops` (`SeqCst`) Dekker pini, çift-nesil ABA kalkanı, root-only destroy (`AUD-K3`) ve `reserve/commit_root_slot` (`AUD-K9`). |
| **2. `VSpace`, `W^X`, SMP TLB & `PMM`** | `kernel/src/vspace/{mod,pte,tlb}.rs`, `kernel/src/pmm/mod.rs`, `kernel/src/uaccess.rs` | `W^X`, `PAT4 WC`/`UC`, `active_cpus` TLB shootdown, `PER_CPU_MAGAZINES` + `iommu_pin_count`, `CR4.SMAP`. |
| **3. `H1` SMP Zamanlayıcı, `IST` & FPU/SIMD** | `kernel/src/sched/{mod,state,context,runqueue,donate}.rs`, `kernel/src/fpu.rs`, `kernel/src/gdt_tss.rs` | `dry_switch_to`, `NMI` `IST2` / `#MC` `IST3`, `#NM` sıfırlaması (`CLEAN_FCW`/`CLEAN_MXCSR`), `XSAVEOPT64`/`FXSAVE64` geçişi. |
| **4. Syscall, `TCB`-Bağlı `Notif` & `FaultEndpoint`** | `kernel/src/syscall/{entry,mod,cold}.rs`, `kernel/src/ipc/{endpoint,endpoint_state,notif,bound_notif}.rs`, `kernel/src/proc/fault.rs` | `rust_syscall_dispatch` (`248 B`), `sys_notify_bind_tcb` / `sys_ipc_recv_ext`, `FAULT_MSG_LABEL` koruması & `FAULT_BUDGET = 16`. |
| **5. `VT-d` (`DMAR`/`SLPT`), `MSI/MSI-X` & `>4 GiB` `BAR`** | `kernel/src/iommu/{mod,dmar,slpt,vtd,tests}.rs`, `kernel/src/irq/msi.rs`, `kernel/src/pci_{bar,window}.rs`, `kernel/src/pci.rs`, `kernel/src/pci_cfg.rs` | 4-seviye `SLPT` (`IOVA -> HPA`), `quiesce_slot_for_cspace` (`MSI` maske + `BME=0` + `SLPT/IOTLB` scrub), `PCI_OBJ_DISPLAY_AUX = 6` + GOP BAR0 guard, `>4 GiB BAR` penceresi & `dry_hw_test.rs`. |
| **6. Depolama (`IoRing`, `DRYFS_v1`), `Vault` & `Broker`** | `userspace/src/{ioring,persist_fs,vault_crypto,island_proto}.rs`, `userspace/src/bin/{dry_blk,dry_vault,dry_broker}.rs`, `userspace/src/supervisor.rs` | `AUD-U1..U9` kapanmaları, SPSC `Acquire/Release`, `Device Broker` kontrol/veri ayrımı ve `RIGHT_REVOKE` alt-ağaç iptali. |
| **7. Linux `v6.18` `virtio-rng` Driver Island (`2C-1`)** | `drivers/lx-virtio-rng-island/{Cargo.toml,src/main.rs}`, `drivers/lx-virtio-rng-island/cshim/lx_virtio_shim.c`, `vendor/linux-drivers/v6.18/virtio_rng/virtio-rng.c` | `GPL-2.0-only` izolasyonu (`0` çekirdek Linux satırı), `kzalloc` `DEVICE_DMA`, `sg_init_one` sınırları ve `#UD` -> `gen=2` kurtarma. |
| **8. `DRY Glass 2.0 / 2.1`, `VirtIO-GPU` 2D & 6 Uygulama** | `userspace/src/ttf/*`, `userspace/src/ui/*`, `userspace/src/glass/*`, `userspace/src/apps/*`, `userspace/src/{surface,wm,wm_ipc,compositor,virtio_gpu}.rs`, `userspace/src/bin/{dry_gpu,dry_display,dry_shell,dry_sysmon,dry_files,dry_settings,dry_editor,dry_calc}.rs` | `DrySans`/`DryMono` (`OFL-1.1`), `Adaptive Window Strip`, `Alt+W` önizleme, `IconRail`, `100%/125%/150%` `scale_px`, `WindowConfig` eşitlemesi, `GPU_FALLBACK_TO_GOP` -> `gen=2` kurtarması ve `qemu_p10_glass2_validators.py` / `qemu_p11_glass21_validators.py`. |
| **9. `Ascension 4.0` `libdry-lx-compat`, `bochs` Adası, GPU & `Live USB`** | `drivers/libdry-lx-compat/{include,src}/*`, `drivers/lx-drm-bochs-island/`, `drivers/lx-nv-open-shim/nv_os_shim_check.c`, `userspace/src/{lx_compat_meta,lx_drv_proto,lx_drm_audit}.rs`, `tools/lx_compat/*` | `.ko` reddi, GOP BAR0 izolasyonu (`--p12-only` `bochs` + GOP), `nv_os_shim_check.c` `-Werror` ve `COMPAT_CATALOG.json` doğrulaması. |
| **10. `Ascension 5.0` M1/M2/M4/M5 (`9ca5e41`, `7d72cd3`, USB/NVMe)** | `kernel/src/{serial,panic_fb,pmm/meta,pci_bar,pci_window,pci_slots}.rs`, `kernel/src/iommu/{slpt,vtd}.rs`, `userspace/src/usb/*`, `userspace/src/{nvme_ro,sup_hw_services,shell_sys_cmds}.rs`, `tools/{build_live_usb_image,hw_inventory_lenovo_15ihu6,gpu_fw_validators,provision_local_firmware,qemu_p13_usb_persist_validators}.py` | `probe_com1_uart` + `DIAG_RING` + GOP panik, `page_suboff` `<4K` BAR, çoklu-DRHD `VT-d` + `premap_unit_rmrrs`, `INV-DISK-SAFE-01/02` (`--p13-only`), `160 MiB` GPT Live USB (`5` profil) ve salt-okunur `82K1` FW/WPR2/PRIME denetimleri. |
| **11. `Ascension 5.0` M3 (`8139cp`), Team A (`118` SVG) & OSS Lisans** | `drivers/libdry-lx-compat/src/lx_*.c` (`6` dosya), `drivers/lx-net-8139cp-island/`, `userspace/src/broker_lifecycle.rs`, `assets/{brand,icons,ASSET_MANIFEST.json}`, `userspace/src/ui/{brand,app_icons,vector_assets_sys,vector_assets_apps}.rs`, `tools/{build_vector_assets,qemu_p14_lx_net_validators,qemu_p15_brand_icons_validators}.py`, `LICENSING.md`, `docs/{LIVE_USB_OPERATOR_GUIDE,ASCENSION_5_0_VALIDATION_MATRIX}.md` | `2,207` LOC C runtime + 15-adımlı yaşam döngüsü + `56/56` SHA-256 `8139cp+mii`, `118` SVG (`200%` DPI, `--p15-only` `15` PNG), `Apache-2.0 OR MIT` + `GPL-2.0-only` sınırı. |
| **12. `Ascension 6.0` Evrensel GPU & Fail-Closed IOMMU (`--p16-only`)** | `kernel/src/{iommu/{ops,ivrs,vtd,dmar,slpt,mod},pci_ecam,pci_slots,dma,sched/runqueue}.rs`, `userspace/src/{gpu_svc/*,fw_store,ioring,usb/{xhci,msc}}.rs`, `drivers/{libdry-lx-compat/{include/dry_lx_drm.h,src/lx_*.c},lx-drm-bochs-island/src/bochs_island_glue.c,lx-nv-open-shim/nv_os_shim_check.c}`, `tools/{qemu_p16_ascension6_validators,microvm_gpu_proto/proto_bench,probe_virtio_gpu_3d,build_live_usb_image}.py`, `docs/{ASCENSION_6_0_VALIDATION_MATRIX,LINUX_MICROVM_GPU_FEASIBILITY}.md` | Fail-closed `IommuOps` + `IVRS` `FailClosed`, `MCFG`/`ECAM` `64` PCI, `SYS_DMA_CTL` SG (`IOVA != HPA`), `dry-gpu-svc` (`4` backend + `0x7f` FSM + `broker_data_path=0`), decoupled `libdry-lx-compat` (`0` `bochs` hit), dürüst `nv_os_shim_check.c` (`4/4/9/5`), `fw_store` SHA-256, `MicroVM` kopyalı IPC, `Virgl` keşfi, Vyukov/xHCI düzeltmeleri, `139/139` QEMU + `5/5` Matris + `VTD_ACTIVE` Live USB. |
| **13. `Ascension 7.0` (`SILICON REALITY`, `--p17-only`)** | `kernel/src/iommu/{vtd,vtd_domain,slpt}.rs`, `userspace/src/{intel_tgl_prep,virtio_gpu_3d,virtio_gpu}.rs`, `userspace/src/bin/dry_gpu.rs`, `tools/{hw_profile_lenovo_82k1,nvidia_ga107_audit,bootstrap_qemu_virgl_env,probe_virtio_gpu_3d,qemu_p17_ascension7_validators}.py`, `tools/microvm_gpu_proto/{ipc_bench_a7.c,run_a7_perf_suite.py}`, `docs/{ASCENSION_7_0_FINAL_REPORT,ASCENSION_7_0_VALIDATION_MATRIX,PERFORMANCE_COMPARISON}.md` | 12 donanım sertleştirmesi, `[DRY-IOMMU] UNIT`/`[DRY-PCI-TOPO]`, `intel_tgl_prep` (`PARTIAL`), `171`-API OpenRM + `ucodes_ga10x.bin`, izole `Virgl` (`llvmpipe`), yatay dirty-rect kırpma (`-54.4%` px) + `T0..T5`, `--p17-only` `4/4` kurtarma/izole `143/143` QEMU. |
| **14. `Ascension 8.0` HWVAL, SDK v1 & NVIDIA Seviye-1 (`--p18-only`, `29/32` modül)** | `kernel/src/{ipc/endpoint,pci,dma,irq/session}.rs`, `userspace/src/{abi_ext,fat32_log,island_proto,island_audit,intel_tgl_prep,virtio_gpu,sup_nv_island,shell_diag_cmds,shell_gpu_cmds}.rs`, `userspace/src/hwdiag/*`, `userspace/src/nvidia_island/*`, `userspace/src/usb/{log_svc,msc}.rs`, `userspace/src/bin/{dry_nv_island,dry_broker,dry_usb}.rs`, `drivers/{dry-driver-sdk,sdk-example-pci}/*`, `tools/{build_live_usb_image,retrieve_dryos_logs,qemu_p18_hwval_validators,qemu_p18_nvidia_validators}.py`, `docs/{ASCENSION_8_0_*,HARDWARE_*,ARCHITECTURE_AUDIT,SECURITY_AUDIT,LINUX_WINDOWS_GAP_ANALYSIS,DRIVER_COMPATIBILITY_ROADMAP,DRIVER_ISLAND_SDK,GPU_COMPATIBILITY_STATUS}.md` | `256 MiB` 3-bölümlü HWVAL USB, 5-kapılı xHCI/GUID/FAT32 (`0x1f`) + read-back SHA-256, `15` `dry` komutu, M2 çekirdek/userspace düzeltmeleri (`p18d`), Driver Island ABI v1 `dry-driver-sdk` (`5/5`) + `dry-sdk-demo.elf` (`p18e`), `ALLOW_PHYSICAL_GPU_WRITES=false`, `dry-nv-island.elf` (`#29/32`) fail-closed `14` önkoşul + `nvidia-smi` / `nvidia_exp/` (`p18f`/`p18nv`/`6/6 p18g`, `BLOCKED`/`N/A`), `164/164` QEMU + `5/5` Matris. |
