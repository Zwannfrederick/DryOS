# DRY OS — Kapsamlı Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

- **Son Güncelleme:** 11 Kasım 2026 (Gün Sonu Dokümantasyon Kapanışı — `Milestone 1C` + `Optimization Pass v1` + `ADR-008..011` Ürün/Mimari Devir Teslimi)
- **Mevcut Durum (`P0 — DONE`):** Faz 0 (`H1` & `H2`), `ADR-007` güvenlik mimarisi, Faz 1'in tüm kilometre taşları (`1A`, `1B-A`, `1B-B`, `1B-C`, `1B-D`, `1C`: `1df3842`) ve `Low-Level Optimization Pass v1` (`69522a3`) `%100` tamamlandı; `0` Clippy uyarısı ve `56/56` otomatik QEMU/KVM senaryosu (`leaked_frames = 0`, `stale_tlb_reads = 0`, `uaf_errors = 0`, canlı QMP `screendump` piksel kanıtı) ile doğrulandı.
- **Bu Belgenin Amacı:** 12 Kasım 2026 sabahı ve izleyen oturumlarda görevi devralacak coding agent'ın sıfır bağlam kaybıyla, mevcut değişmezleri ve yığın/eşzamanlılık sınırlarını kırmadan doğrudan `P1` (`Milestone 2A` — Etkileşimli DRY OS: IRQ -> Input -> IPC -> Ring 3 Shell -> Display) geliştirmesine başlamasını sağlamak.

---

## 1. Git Commit Zinciri ve Kilometre Taşı Özeti

| Aşama | Commit Hash | Kapsam Özeti | Doğrulama |
| :--- | :---: | :--- | :---: |
| `ADR-007` | `a443e6c` | `KERNEL (0)` / `ROOT_SYSTEM (1)` / `USERSPACE (2)` izolasyonu, `SecLabel` (8B), `DRY Vault` kiralama | Mimari + Öz-test |
| `1A-B/C` | `bc46907` | Limine UEFI 64-bit boot, HHDM, `COM1` seri konsol, deterministik `isa-debug-exit` (`33`/`35`) | QEMU Boot |
| `1A-D` | `7ecf5b2` | Per-CPU `GDT`, `TSS` (`IST1` `#DF` yığını), 256-girişli `TrapFrame` `IDT`, canlı `#BP`/`#UD`/`#PF` | `6/6` QEMU |
| `1A-E` | `0ae3a94` | Limine MP `1/2/4/20 vCPU` SMP, özel 16 KiB `.bss` yığınları, `IA32_GS_BASE`, 4 negatif SMP testi | `10/10` QEMU |
| `1A-F` | `b6ad2e9` | İşbirlikçi `H1` zamanlayıcı (`AtomicU32`, `SeqCst` çit, `PerCpuRunQueue`, `dry_switch_to`) | `15/15` QEMU |
| `1A-G` | `fad423b` | Preemptive `H1`, `x2APIC`/`xAPIC` 3-pencereli PIT kalibrasyonu, `preempt_count`, 5 yarış testi | `21/21` QEMU |
| `1B-A` | `78ac64b` | `BootstrapAllocator` mühürleme, `16 B` `FrameMeta` `PMM` (`Order4KiB`/`Order2MiB`), `Untyped` | `26/26` QEMU |
| `1B-B` | `6445641` | 4-seviye `VSpace`, `W^X`, `KERNEL_VSPACE` devri (`~53 MiB` geri kazanım), SMP TLB shootdown (`0xEE`) | `35/35` QEMU |
| `1B-C` | `b4ca6f8` | Korumalı `CSpace` (`64 B` `CapSlot`), çift-nesil ABA kalkanı, CDT $O(k)$ `revoke`, TLB borç onarımı | `46/46` QEMU |
| `1B-D` | `29002d3` | Gerçek `CPL=3` ELF64 süreçleri, `Syscall ABI v0.1`, `CR4.SMAP` + `extable` `uaccess`, sıralı `reclaim` | `50/50` QEMU |
| `1C` | `1df3842` | `Endpoint`/`Notification` IPC, 4-sıçrama Öncelik Bağışı, `dry-init`/`dry-echo`/`dry-display` GOP ekranı | `56/56` QEMU + PNG |
| `Opt v1` | `69522a3` | `Low-Level Optimization Pass v1` (`null_sys 231 cyc`, `call_reply 1.952 cyc` [`12.8x`], `-16 KiB` meta) | `56/56` QEMU + Asm |
| `Docs` | *HEAD* | Ürün vizyonu, `ADR-008..011`, `PRODUCT_ROADMAP.md`, `DRY_DESKTOP_SPEC.md` ve 12 Kasım Handover | Dokümantasyon |

---

## 2. Kesinleşen Mimari ve Ürün Kararları (`DECISIONS.md`)

1. **ADR-001 (Boot — `DONE`):** Limine Base Revision 3 (UEFI `OVMF` + doğrudan 64-bit SMP AP başlatma + HHDM + Boot Modules + GOP Framebuffer).
2. **ADR-002 (Syscall ABI — `DONE` v0.1/v0.2):** `x86-64` `SYSCALL`/`SYSRETQ` (`RDI, RSI, RDX, R10, R8, R9` kontrol + `R12..R15` kopyasız 4 mesaj yazmacı + isteğe bağlı `<= 256 B` ek tampon; `RAX` birincil dönüş / negatif `SysError`, `RDX` ikincil dönüş).
3. **ADR-003 (CSpace — `DONE` Düz Tablo, `PLANNED` Radiks):** Sabit boyutlu korumalı düz tablo (`MAX_CSPACES = 32` [`0` çekirdek rezervi, `1..=31` kullanıcı], `CNODE_SLOTS = 256`, `64 B/slot`); `CapPtr` 64-bit düzeni (`guard:8 | radix:8 | slot_gen:32 | slot_idx:16`) Faz 3'te ABI kırmadan 2-seviyeli radiks ağacına genişler.
4. **ADR-004 (Scheduler — `DONE` Temel `H1`, `PLANNED` Topolojik):** TLC (`formal/h1/DrySched.tla`, `84.351` güvenlik + `54.095` canlılık durumu, `0` ihlal) ve Rust `loom` (`39.135` permütasyon + `>24M` 20-CPU stres geçişi) ile kanıtlanan `H1` protokolü + 8 öncelik seviyeli (`active_mask` `tzcnt`) `PerCpuRunQueue` + 4-sıçramalı IPC Öncelik Bağışı.
5. **ADR-005 (Lisans — `DONE`):** DRY Kernel ve native servisler `MIT OR GPL-2.0-only`; Linux *Driver Island* ikilileri ve `experiments/h2-island` katmanı `GPL-2.0-only` (saf `Apache-2.0` ve `GPL-3.0` sürücü adalarında yasaktır).
6. **ADR-006 (Bağımsızlık — `DONE`):** `Gate 0` (`H1` + `H2` `igc` `v6.12`/`v6.18` `%0` upstream farkla `3.468` satır runtime + `vfio-pci` `e1000e` DMA PoC) geçilmiştir; çekirdek gelişimi `H2` çeviri kapsamına bağımlı değildir.
7. **ADR-007 (Güvenlik & `DRY Vault` — Çekirdek `DONE`, Ring 3 Servisler `PLANNED`):** `KERNEL (0)`, `ROOT_SYSTEM (1)` ve `USERSPACE (2)` alan duvarları; 8-bayt `SecLabel`; path-bağımsız `CapRights + SecLabel + lease_epoch` ortak denetimi (`authorize_cspace_fast`); `ROOT_SYSTEM` süreçlerinin bile `KERNEL` belleğine erişememesi; `READ_ONLY_EXPORT` için `CSpace` + `VSpace` + `PMM` 3-katmanlı salt-okunur kalkanı.
8. **ADR-008 (Ürün Felsefesi & Profiller — `PLANNED`):** Tek `dry-kernel` ikilisi üzerinde iki birinci sınıf kurulum profili: etkileşim/enerji odaklı **`DRY Desktop`** ve varsayılan başsız (*headless*), sıfır GUI bağımlılıklı, CPU yakınlığı/NUMA/*throughput*/AI inference odaklı **`DRY Server`** (çift yönlü kurulum sonrası geçiş destekli; KVM mikrobenchmarkları Linux'tan hızlı olunduğunun kanıtı sayılmaz).
9. **ADR-009 (`DRY Desktop` / `DRY Shell` & `Action API` — `PLANNED`):** Tek Ring 3 compositor üzerinde varsayılan `Floating` + tek tık `Tiling` + `Stacking/Tabbed` pencere yönetimi (`Tiling` düzenini bozmayan `Floating` diyaloglar ve kip geçişlerinde yerleşim korunumu), `liquid-glass` üst bar, `Normal`/`Compact UI`, düşük donanım *fallback* ve fare/klavye/menü/komut paletini birleştiren ortak **`Action API`** (`0` çekirdek UI kodu).
10. **ADR-010 (Çift Açılış & Kimlik Sınırı — `PLANNED`):** Tek yapılandırılmış `BootEvent` akışını kullanan iki görünüm (**`Clean Boot`** varsayılan ve **`Diagnostic Boot`**, `Esc` ile canlı geçiş, sahte yüzde yasağı) + `Login / Authentication Manager` (`RootSystem`) ile `User Password Manager` (`Userspace`) bileşenlerinin kesin TCB ayrımı.
11. **ADR-011 (`drypkg`, `DRY Store` & Kademeli Uyumluluk — `PLANNED`):** Ortak GUI+CLI paket motoru (*Bul -> Kur -> Çalıştır*), uygulama öncesi zorunlu tasarım kapısı, isteğe bağlı (*on-demand*) runtime kurulumu ve kesin öncelik sırası: **(1) Native DRY -> (2) Linux ELF/ABI -> (3) Seçilmiş Windows PE/Win32** (görünür uyumluluk sınırlarıyla; `%100` uyumluluk vaat edilmez).

---

## 3. Depo ve Alt-Sistem Haritası (Tüm Dosyalar `< 1.000` Satır Tavanı Altında)

### 3.1. Çekirdek (`kernel/src/`)
- **Önyükleme, Donanım ve Per-CPU:** `main.rs` (`bsp_kmain`), `limine.rs` (Base Rev 3 HHDM/SMP/Modül/GOP), `arch.rs`, `gdt_tss.rs` (per-CPU `GDT`/`TSS`/`IST1`, `verify_cpu_stack_canaries_fast`), `percpu.rs` (`256 B` `PerCpuData`), `idt.rs` (256-girişli `TrapFrame`, MSR-doğrulamalı `NMI/#DF` `swapgs`, `#PF` `extable`), `apic.rs` (`x2APIC`/`xAPIC`, `rearm_oneshot_tick_count_only`), `smp.rs`, `serial.rs`, `security.rs` (`ADR-007`), `uaccess.rs` (`CR4.SMAP = 1` + `extable`), `display.rs` (`CapObjectKind::Framebuffer`).
- **Zamanlayıcı (`kernel/src/sched/`):** `state.rs` (`H1` `AtomicU32` + `SeqCst` çitler), `runqueue.rs` (`active_mask` `tzcnt` `PerCpuRunQueue`), `context.rs` (`dry_switch_to`), `preempt.rs` (`timer_needs_rearm`), `donate.rs` (4-sıçramalı Öncelik Bağışı), `mod.rs` (`60` TCB yuvası, özel 16 KiB `.bss` yığınları), `tests.rs`, `preempt_tests.rs`.
- **Fiziksel ve Sanal Bellek (`kernel/src/pmm/`, `vspace/`):** `pmm/{meta.rs,mod.rs,huge_reclaim.rs,untyped.rs,tests.rs}` (`16 B` `FrameMeta`, `PerCpuFrameCache`, `Order4KiB`/`Order2MiB`, `Untyped`); `vspace/{pte.rs,mod.rs,huge.rs,tlb.rs,tlb_debt.rs,kernel_handover.rs,tests.rs}` (`MAX_VSPACES = 32`, `W^X`, `map_4k`/`map_mmio_4k`/`unmap_4k_batch`, `active_cpus` SMP TLB shootdown).
- **Yetki, Süreç, Syscall ve IPC (`cspace/`, `proc/`, `syscall/`, `ipc/`):** `cspace/{cap.rs,mod.rs,revoke.rs,objects.rs,tests.rs,bench.rs,fault_tests.rs}` (`cspace_lookup_fast`, `with_pinned_cap`, CDT `revoke`); `proc/{elf.rs,mod.rs,supervisor.rs,tests.rs,bench.rs}` (`MAX_PROCESSES = 24`, `ET_EXEC` ELF64, `reclaim_process`); `syscall/{entry.rs,mod.rs}` (`248 B` çerçeveli `rust_syscall_dispatch`, `0..=19`); `ipc/{mod.rs,endpoint_state.rs,endpoint.rs,notif.rs,tests.rs,bench.rs}` (`ep_send_or_call`, `ep_recv`, `ep_reply`, birleşik `ep_reply_recv`, `notif_signal`/`notif_wait`).

### 3.2. Kullanıcı Alanı (`userspace/`), Araçlar (`tools/`) ve Belgeler (`docs/`)
- `userspace/src/lib.rs` ve `userspace/src/bin/{user_hello,user_peer,dry_init,dry_echo,dry_display}.rs`.
- `tools/`: `bootstrap_phase1.py`, `qemu_boot_test.py` (`56` senaryo + QMP `screendump`), `qemu_{log,user,ipc}_validators.py`.
- Mimari & Ürün Belgeleri: `ARCHITECTURE.md`, `DECISIONS.md`, `EXPERIMENTS.md` (`804` satır — geçmiş ölçüm kaydı, dokunulmaz), `ROADMAP.md` (teknik), `RISK_REGISTER.md`, `docs/PRODUCT_ROADMAP.md` (`P0..P5` ürün yol haritası), `docs/DRY_DESKTOP_SPEC.md` (`DRY Desktop`/`DRY Shell` spesifikasyonu).

---

## 4. Kritik İkili Düzenler ve Donanım Sözleşmeleri

### 4.1. `PerCpuData` (`256 B`, `#[repr(C, align(64))]` — `kernel/src/percpu.rs`)
Assembly kodları (`syscall/entry.rs`, `idt.rs`) aşağıdaki bayt ofsetlerine doğrudan `gs:[ofset]` ile erişir (`const assert!` kilitli):
- `gs:[0]` `self_ptr`, `gs:[8]` `cpu_idx`, `gs:[16]` `lapic_id`, `gs:[24]` `magic = 0x4452_595f_5043_5055`.
- `gs:[32..64]`: `boot_stack_base`, `boot_stack_top`, `df_ist1_base`, `df_ist1_top` (`verify_cpu_stack_canaries_fast` `STR` VM-exit'siz okur).
- `gs:[64..128]`: `tss_base`, `gdt_base`, `bp_trap_count`, `last_bp_rip`, `exec_ticks`, `current_slot`, `ctx_switches` (`CLI` altında `LOCK`'sız), `in_df_handler`, `need_resched`, `timer_armed`, **`timer_needs_rearm` (ofset `104`)**.
- `gs:[128..192]`: `timer_irqs`, `preemptions`, `deferred_rescheds`, `last_progress_seq`, `preempt_count`, `irq_depth`, `quantum_rem`, `storm_streak`, `storm_slot`, `nmi_count`.
- **`gs:[192]` (`PCPU_OFF_KERNEL_RSP0`)**: Aktif iş parçacığının `stack_top` değeri (`TSS.RSP0` ile senkron).
- **`gs:[200]` (`PCPU_OFF_USER_RSP_SCRATCH`)**: `SYSCALL` girişinde kullanıcı `RSP` geçici saklama alanı.
- `gs:[208..256]`: `cur_fs_base`, `cur_user_gs_base`, `cr3_noops_local`, `cr3_switches_local`, `syscall_count`, `user_fault_count`.

### 4.2. `DRY Native Syscall ABI` (`0x0001_0000` — `syscall/entry.rs`, `syscall/mod.rs`, `uaccess.rs`)
- **Giriş:** `RAX = nr`, `RDI = a0`, `RSI = a1`, `RDX = a2`, `R10 = a3`, `R8 = a4`, `R9 = a5`; kopyasız IPC yazmaçları `R12..R15 = mr0..mr3`.
- **Dönüş & `SysError` Kodları (`uaccess.rs`):** `RAX >= 0` başarı veya negatif hata (`-1 EPERM`, `-9 EBADCAP`, `-10 ECHILD`, `-11 EAGAIN/EWOULDBLOCK`, `-12 ENOMEM`, `-14 EFAULT`, `-16 EBUSY`, `-22 EINVAL`, `-32 EPIPE`, `-35 EDEADLK`, `-38 ENOSYS`, `-90 EMSGSIZE`, `-95 EIRQCTX`, `-110 EREVOKED`, `-116 ETIMEDOUT`, `-125 ECANCELED`); `RDX` ikincil çıktı (`reply_token` / `badge` / `pid` / aktarılan `CapPtr`).
- **Hijyen & Bayraklar:** `RBX, RBP, R12..R15` korunur (IPC alım çağrılarında `R12..R15` mesaj taşır); dönüşte `RDI, RSI, R8, R9, R10` sıfırlanır; `IA32_FMASK` girişte `TF | IF | DF | IOPL | NT | AC` bitlerini donanımda temizler.
- **Mevcut Çağrı Tablosu (`0..=19`):** `0: SYS_ABI_PING`, `1: SYS_WRITE_DEBUG`, `2: SYS_YIELD`, `3: SYS_EXIT`, `4: SYS_GET_TID`, `5: SYS_CAP_QUERY`, `6: SYS_EP_CREATE`, `7: SYS_IPC_SEND`, `8: SYS_IPC_RECV`, `9: SYS_IPC_CALL`, `10: SYS_IPC_REPLY`, `11: SYS_IPC_REPLY_RECV`, `12: SYS_NOTIFY_CREATE`, `13: SYS_NOTIFY_SIGNAL`, `14: SYS_NOTIFY_WAIT`, `15: SYS_CAP_DERIVE`, `16: SYS_CAP_REVOKE`, `17: SYS_PROC_SPAWN`, `18: SYS_PROC_WAIT`, `19: SYS_FB_MAP`.

### 4.3. Sanal Adres Alanı Haritası (`VSpace` — `kernel/src/vspace/pte.rs`)
- `0x0000_0000_0000_0000 .. 0x1000`: Eşlenmemiş `NULL` koruma sayfası.
- `0x0000_0000_0040_0000+`: Statik `ET_EXEC` kullanıcı ELF64 segmentleri (`W^X` zorunlu).
- `0x0000_7d00_0000_0000+` (`USER_FB_MMIO_BASE`): Yetenek-kapılı Ring 3 GOP Çerçeve Tamponu (`PTE_USER | PTE_WRITABLE | PTE_PWT | PTE_PCD | PTE_NX | PTE_SW_MMIO`; normal RAM iade yoluna girmez).
- `0x0000_7fff_f000_0000 .. 0x0000_7fff_f001_2000`: Kullanıcı yığını (`4 KiB` alt koruma + `64 KiB` yığın + `4 KiB` üst koruma).
- `0xffff_8000_0000_0000+` (`PML4[256..]`): Limine HHDM (`U/S = 0`, `NX = 1`; `ReservedOrMmio` delikleri eşlenmemiş).
- `0xffff_ffff_8000_0000+` (`PML4[511]`): Çekirdek imajı (`.text` `RX`, `.rodata` `RO+NX`, `.data/.bss` `RW+NX`).

---

## 5. Tekrarlanabilir Derleme, Test ve Disassembly Komutları

1. **Derleme + Clippy (`-D warnings`, `dry-userspace` + `dry-kernel`):** `python3 tools/qemu_boot_test.py --build-only` *(çıplak `cargo clippy` çalıştırma; betik `.toolchains/sysroot` ve `-Z build-std` bayraklarını geçirir).*
2. **Hızlı Tekil SMP Duman Testi (`~2 sn`):** `python3 tools/qemu_boot_test.py --smp-only 2`
3. **Tam Regresyon + Negatif Arıza + QMP `screendump` Paketi (`56/56`, `~95 sn`):** `python3 tools/qemu_boot_test.py`
4. **Release Çekirdek Disassembly:** `objdump -d -M intel target/x86_64-unknown-none/release/dry-kernel > /tmp/dry_kernel.asm`
5. **Faz 0 `H1` ve `H2` Testleri:** `cargo test -p h1-sched --release` ve `make -C experiments/h2-island test`

---

## 6. 12 Kasım 2026 Sabahı Devam Edecek Coding Agent İçin Başlangıç Rehberi

### 6.1. Kesin Son Commit Zinciri ve Temiz Çalışma Ağacı Durumu
- **Son Kod Commit'leri (Dokunulmadan Korunacak):**
  - `Milestone 1C`: `1df3842e3ff1676c46349f072c623836a56f846a`
  - `Low-Level Optimization Pass v1`: `69522a3482d3000dc796a6717446dbc8bb1cb25e`
  - `Gün Sonu Dokümantasyon Kapanışı`: `69522a3` üzerindeki son `docs:` commit'i (`git log -n 3` ile kontrol et; `69522a3`'ten sonra hiçbir `.rs`/kod dosyası değiştirilmemiştir).
- **Çalışma Ağacı Durumu:** `kernel/`, `userspace/`, `tools/`, `docs/` ve kök dizindeki tüm izlenen kod/belge dosyaları temizdir. Yalnızca Faz 0'dan kalma `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` yerel değişiklik içerir; **bu iki dosyayı asla `git add` yapma, düzenleme veya geri alma (`git checkout`).**

### 6.2. `56/56` Doğrulama Sonucu ve Neleri Kanıtlayıp Kanıtlamadığı
- **Kanıtlanan (`DONE`):** `50` Faz 1 regresyon testi + `boot_display_screendump` (`1280x800`, `non_bg_px = 281.600`, `colors = 7`, sol-üst `#0A0E17`, merkez `#18E0B5`) + `boot_2cpu_headless_nofb` (`-vga none`) + `4` negatif IPC arıza testi (`ipc_irq_block`, `ipc_unauth_ep`, `ipc_double_reply`, `ipc_donate_cycle`) `%100` geçti (`0` Clippy uyarısı, `leaked_frames = 0`, `stale_tlb_reads = 0`, `uaf_errors = 0`).
- **Kanıtlanmayan Sınırlar (`PLANNED`):** Gerçek fiziksel x86-64 donanım (`bare-metal`) önyüklemesi, fiziksel Intel VT-d / AMD-Vi IOMMU silikon davranışı, harici IOAPIC/MSI-X kesmeleriyle canlı klavye/fare/disk/ağ G/Ç, FPU/SIMD izolasyonu ve Linux'a karşı tam sistem performansı henüz test edilmemiştir.

### 6.3. Ring 3 Framebuffer ve Capability IPC'nin Mevcut Durumu
- **Ring 3 Framebuffer (`DONE` Statik Kanıt / `PLANNED` Sürekli Sunucu):** `display.rs`, Limine GOP BAR bölgesini (`0x8000_0000`, `1280x800x32`, `ReservedOrMmio`) doğrular ve yalnızca `RootSystem` süpervizörüne tekil `CapObjectKind::Framebuffer` üretir. `dry-display.elf`, `SYS_FB_MAP (19)` ile çerçeve tamponunu `0x0000_7d00_0000_0000` adresine eşler, ilk DRY OS ekranını çizer ve temiz şekilde çıkar. **Eksik olanlar (`PLANNED`):** Sürekli çalışan ekran sunucusu döngüsü, RAM arka tamponu (*backbuffer*) + kirli dikdörtgen (*dirty-rect*) aktarımı, çoklu istemci yüzeyleri ve compositor.
- **Capability IPC (`DONE` Kontrol Düzlemi / `PLANNED` Veri Halkası):** Senkron `Endpoint` (`send`, `recv`, `call`, `reply`, tek-pin/tek-kilit birleşik `ep_reply_recv`), asenkron `Notification` (`signal`, `wait`), kopyasız `R12..R15` (`32 B`) + `<= 256 B` ek tampon, `cspace_transfer` yetki devri, tek-atımlık `ReplyToken` ve 4-sıçramalı Öncelik Bağışı tam çalışmaktadır. **Eksik olanlar (`PLANNED`):** Yüksek hacimli sıfır-kopya `CapObjectKind::IoRing` paylaşımlı bellek halkaları ve tek sunucu iş parçacığında `Endpoint + Notification` birleşik beklemesi (`TCB`-bağlı bildirim).

### 6.4. Optimizasyon Öncesi (`1df3842`) ve Sonrası (`69522a3`) Özet Performans Metrikleri

| Metrik (`p50` döngü, `~3.11 GHz` KVM) | `1df3842` (`1 vCPU`) | `69522a3` (`1 vCPU`) | `69522a3` (`2..20 vCPU`*) | Net Kazanç / Kök Mekanizma |
| :--- | :---: | :---: | :---: | :--- |
| `null_sys` (`SYSCALL -> SYSRETQ`) | `326` | **`231`** (`74 ns`) | **`231..257`** | **`-29.1%`** (`CLI` hızlı yol, `248 B` çerçeve, `STR` VM-exit elemesi) |
| `iretq_sys` (`SYSCALL -> IRETQ`) | `619` | **`522`** | **`521..548`** | **`-15.7%`** (`1.416 B -> 248 B` `rust_syscall_dispatch` yığını) |
| `k2k_sw` / `u2u_same_vs` / `u2u_cross_vs` | `533 / 818 / 1.015` | **`451 / 670 / 988`** | **`453..526 / 670..760 / 990..1.085`** | **`-15%..-18%`** (`tzcnt` `active_mask`, koşullu APIC rearm) |
| `cspace_lookup_fast` / `ep_lookup` | `128` / `139` | **`99`** / **`93`** | **`99..122`** / **`98..124`** | **`-22.7%` / `-33.1%`** (sınır-kontrollü sarmalayıcı soyma) |
| `sched_wakeup` (Preemptive epoch) | `12.593` | **`1.037`** | **`2.766..3.576`** | **`12.1x`** (gönüllü geçişte gereksiz APIC `wrmsr` VM-exit elemesi) |
| `send_recv` (Bloklanan `Endpoint`) | `24.760` | **`2.070`** | **`2.087..3.657`** | **`12.0x`** (`0` gönüllü APIC VM-exit + tek-kilit uyanma) |
| `call_reply` (`CALL` + `REPLY_RECV`) | `25.001` | **`1.952`** (`0.63 µs`) | **`4.876..6.535`** (`4.173` xapic) | **`12.8x`** (birleşik `ep_reply_recv` + koşullu APIC rearm) |
| `svc_startup` (`spawn + 1 RPC + wait`) | `70.857` | **`47.741`** | **`57.882..65.171`** | **`-32.6%`** (`unmap_4k_batch` tek-IPI TLB + hızlı IPC) |
| `rust_syscall_dispatch` & Yerleşik Meta | `11.352 B` / `1.622` fr | **`3.085 B` / `1.618` fr** | — | **`-72.8%` kod**, `76 -> 6` `lock`, **`-16 KiB` `.bss`** |

### 6.5. Güvenlik Invariant'ları ve Yarış Riski Bulunan Sıcak Bölgeler
- **Kırılmaz Güvenlik Kuralları:** `INV-SEC-01..08` (`ADR-007`: `KERNEL` çerçeveleri Ring 3'e asla eşlenmez; `READ_ONLY_EXPORT` 3 katmanda yazmaya kapalıdır; `authorize_cspace_fast` her nesne erişiminde koşar), `CR4.SMAP = 1` (kullanıcı belleğine ham Rust referansı oluşturmak yasaktır; yalnızca `copy_from_user`/`copy_to_user`), `W^X` ve `NMI/#DF` MSR-doğrulamalı koşullu `swapgs`.
- **Yarışa Hassas 5 Sıcak Bölge (Değişiklikte Negatif Test Zorunlu):**
  1. `sched/state.rs` & `ipc/notif.rs`: `prepare_park` ile `wake_thread`/`notif_signal` arasındaki açık `fence(Ordering::SeqCst)` bariyerlerini **asla kaldırma** (`R-01`).
  2. `sched/runqueue.rs`: `PerCpuRunQueue::pop_masked` içinde `active_mask` biti temizlendikten sonraki Dekker yeniden kontrolünü (`!rings[p].is_empty() -> fetch_or`) koru (`R-01`).
  3. `cspace/{mod,revoke}.rs`: Nesne kullanan her sistem çağrısı `with_pinned_cap` (`slot.in_flight_ops` `SeqCst` pini) içinden geçmelidir; `CSpace`-sahibi `TCB` yuvaları yalnızca `cspace_destroy_object -> release_dead_tcb_slot` sonrasında serbest kalır (`R-02`).
  4. `sched/preempt.rs` & `apic.rs`: `pcpu.timer_needs_rearm` bayrağı yalnızca gerçek timer IRQ'sunda `true` yapılır; gönüllü bloklanma/`yield` yoluna koşulsuz APIC `wrmsr` eklemek IPC gecikmesini `12x` bozar.
  5. `vspace/tlb.rs`: `CDT_LOCK -> VSpace::lock -> TLB_REQ_LOCK` ve `Endpoint::lock -> DONATE_LOCK` kilit sırasını bozma; `CLI` altında dönen her bekleme döngüsünde `poll_and_ack_pending_shootdown` çağrısını koru.

### 6.6. `16 KiB` Çekirdek Yığını Bütçesi ve `Pass v1`'de Yakalanan Stack Problemi (`R-11`)
- Her CPU önyükleme/idle yığını ve her worker TCB yığını `.bss` içinde sabit **`16.384 B` (`16 KiB`)** boyutundadır (henüz sanal koruma sayfası yoktur; tabanda 8-bayt kanarya bulunur).
- **Yakalanan Gerçek Taşma:** `Optimization Pass v1` sırasında `verify_cpu_stack_canaries_fast`, LLVM'in `proc/bench.rs::run_stage_g_benchmarks` (`8.192 B` örneklem dizileri) fonksiyonunu `verify_userspace_and_syscalls` içine satır-içi (*inline*) gömmesi sonucunda `bsp_kmain (5.448 B) + verify_userspace_and_syscalls (8.192 B) + verify_stage_f (1.224 B) + on_process_terminating ([WakeBatchItem; 64] = 1.816 B) = 16.680 B > 16.384 B` yığın taşmasını yakaladı.
- **Çözüm ve Zorunlu Kural:** Soğuk benchmark/test fonksiyonları `#[inline(never)]` ile izole edilerek çerçeve `8.192 B -> 488 B` boyutuna indirildi (`+7.704 B` tavan boşluğu). **Yarın yeni kod yazarken:** Çekirdek yığınında `>= 1 KiB` dizi/tampon ayırma (per-CPU statik veya nesne havuzu kullan), soğuk öz-test fonksiyonlarını `#[inline(never)]` işaretle, `soft-float` derleme bayraklarını FPU `XSAVE` eklenene dek koru ve Faz 2'de çekirdek yığınlarını koruma sayfalı sanal aralıklara taşı.

### 6.7. QEMU/KVM Ölçümü ile Gerçek Donanım (`Bare-Metal`) Farkı (`R-05`, `R-09`)
- Bu geliştirme makinesi `4C/8T` (`8` donanım izleği) kapasitelidir: `1, 2, 4, 8 vCPU` koşuları gerçek paralelliği yansıtırken `-smp 20` koşusu `2.5x` konak CFS aşırı-aboneliğidir (vCPU zamanlayıcı kuyruklanması içerir; fiziksel 20 çekirdek ölçeklemesini kanıtlamaz).
- KVM içindeki sanal APIC MSR yakalamaları, EPT TLB davranışı ve emüle GOP/IOMMU gecikmeleri gerçek silikon davranışından farklıdır. **DRY mikroçekirdeğinin Linux'tan hızlı olduğu henüz kanıtlanmamıştır;** bu iddia yalnızca `P5` aşamasında özdeş fiziksel donanımda adil A/B testleriyle sınanacaktır.

### 6.8. 12 Kasım 2026 Sabahı Başlanacak Gerçekçi İlk Teknik İş (`P1 — Dilim 1 / Milestone 2A-1`)
Tek adımda tüm terminali veya masaüstünü yazmaya çalışma; **`P1` (Etkileşimli DRY OS)** hedefinin ilk uçtan uca çalışan dilimi olan **"Donanım IRQ -> `Notification` Köprüsü + Ring 3 `PS/2 i8042` Klavye Sürücüsü (`dry-input.elf`)"** işiyle başla:
1. **Ring 0 `IOAPIC` + `IrqCap` (`kernel/src/ioapic.rs`, `idt.rs`, `cspace/objects.rs`):** IOAPIC MMIO eşlemesi (`ReservedOrMmio`), harici IRQ vektör kapısı (`0x20..0x3F`), kesme anında hattı maskeleyip (`IRQF_ONESHOT`) bağlı `Notification` nesnesine bloklanmadan `notif_signal` tetikleyen işleyici ve `SYS_IRQ_BIND` / `SYS_IRQ_ACK` çağrıları.
2. **Kısıtlı Donanım Erişim Yetkisi (`CapObjectKind::IoPort`):** Rastgele `IOPL=3` açmadan, yalnızca yetkili port aralığına (`0x60`/`0x64` `i8042`) izin veren çekirdek aracılı güvenli port okuma/yazma çağrısı.
3. **Ring 3 `dry-input.elf` (`SecDomain::RootSystem`) + Otomatik Test:** `SYS_NOTIFY_WAIT` ile IRQ bekleyen, tarama kodunu (`scancode`) `KeyEvent` yapısına çevirip IPC `Endpoint` üzerinden aboneye ileten sürücü ve `tools/qemu_boot_test.py` içinde QMP `send-key` ile uçtan uca doğrulama senaryosu.

### 6.9. `Input -> IPC -> Ring 3 Shell -> Display` Zinciri İçin Mimari Bağımlılıklar
```text
[1. IOAPIC + IrqCap(bind/ack) + IoPortCap]  (Ring 0 — Donanım kesme ve port izolasyonu)
                    |
                    v  Notification::signal (IRQ-safe, bloklanmaz)
[2. dry-input.elf (PS/2 Klavye -> Fare / VirtIO-Input)]  (Ring 3 RootSystem — Scancode -> InputEvent)
                    |
                    v  Endpoint IPC (kontrol/tuş) + Paylaşımlı Bellek / IoRing + TCB-Bağlı Notification
[3. dry-shell.elf / dry-term.elf]  (Ring 3 — Komut satırı düzenleme, yerleşik komutlar, karakter ızgarası)
                    |
                    v  SharedPayloadDesc / Frame4K arka tampon + kirli dikdörtgen (dirty_rect) bildirimi
[4. Sürekli Çalışan dry-display.elf GOP Sunucusu]  (Ring 3 RootSystem — RAM backbuffer -> GOP MMIO toplu aktarım)
```
- **Kritik Bağımlılık Notu:** Doğrudan `WC/UC` GOP MMIO belleğine karakter başına tek-piksel yazmak yavaştır; etkileşimli terminal (`P1`) ve compositor (`P2`) aşamasında çizimler mutlaka normal önbellekli (`WB`) paylaşımlı RAM çerçevesinde yapılıp yalnızca değişen dikdörtgenler (`dirty_rect`) GOP MMIO'ya toplu kopyalanmalıdır.

### 6.10. Teknik Roadmap (`ROADMAP.md`) ile Ürün Roadmap (`docs/PRODUCT_ROADMAP.md`) İlişkisi
- `Faz 0` + `Faz 1` (`Milestone 1A..1C` + `Opt Pass v1`) = **`P0` Mikroçekirdek Temeli (`DONE` — QEMU `56/56`)**
- `Faz 2 — Milestone 2A` + `2B` çekirdeği = **`P1` Etkileşimli DRY OS (`PLANNED` — 12 Kasım sabahı başlangıç)**
- `Faz 2 — Milestone 2B/2C` + Ring 3 UI servisleri = **`P2` `DRY Desktop` Alpha/Beta (`PLANNED` — `docs/DRY_DESKTOP_SPEC.md`)**
- `Faz 3` (Linux ELF + `H2` Sürücü Adaları) & `Faz 5` (Seçilmiş Win32) = **`P3` Linux & Windows Uyumluluğu (`PLANNED`)**
- `Faz 3` (`drypkg` çekirdeği) & `Faz 4` (`DRY Store` + Profiller) = **`P4` Paketleme ve Dağıtım (`PLANNED`)**
- `Faz 4` (Bare-Metal, Fiziksel IOMMU, Linux A/B, `>=72s` Soak/Fuzz) = **`P5` Production Readiness (`PLANNED`)**

### 6.11. Tamamlanmamış Özelliklerin Açık Listesi (`PLANNED` — Hiçbiri Tamamlanmış Gösterilmez)
1. **Donanım Girdi & Etkileşim (`P1`):** IOAPIC/MSI-X IRQ yönlendirme (`dry_irq_ctrl`), `PS/2` ve `VirtIO-Input` klavye/fare sürücüleri, IPC üzerinden etkileşimli Ring 3 terminal/kabuk, çift-tamponlu sürekli ekran sunucusu.
2. **Çekirdek Faz 2 İlkelleri (`P1/P2`):** `CapObjectKind::IoRing` paylaşımlı bellek veri düzlemi, `TCB`-bağlı `Notification`, `FaultEndpoint` userspace pager, dinamik ELF/`mmap`, FPU/SIMD (`XSAVEOPT`/`XRSTOR`) bağlam izolasyonu, koruma sayfalı sanal çekirdek yığınları ve `Optimization Pass v2` (`PCID/INVPCID`, doğrudan IPC switch, `TSC-Deadline`).
3. **Güvenlik, Depolama ve Ağ Servisleri (`P1/P2`):** Ring 3 `DRY Vault`, `Policy Broker`, `ADR-007` etiketli `VFS` (`INV-SEC-05`), native `VirtIO-Blk`, `VirtIO-Net`, `VirtIO-GPU` ve `NVMe` sürücüleri.
4. **Masaüstü, Açılış ve Oturum (`P2`):** Ring 3 hibrit compositor (`Floating`/`Tiling`/`Stacking`/`Tabbed`), `DRY Shell` üst bar ve `Compact UI`, ortak `Action API`, `Clean/Diagnostic Boot` (`Esc` geçişli), `DRY Login` ve ayrı `DRY PassVault` parola kasası, temel sistem uygulamaları.
5. **Uyumluluk, Paketleme ve Üretim (`P3..P5`):** Linux ELF/ABI personality, `H2` sürücü adalarının çekirdeğe bağlanması, seçilmiş Windows PE/Win32 katmanı, `drypkg` + `DRY Store`, `DRY Desktop` / `DRY Server` kurulum profilleri, gerçek fiziksel donanım (`bare-metal`) + USB live boot + fiziksel IOMMU doğrulaması ve adil Linux A/B benchmarkları.
