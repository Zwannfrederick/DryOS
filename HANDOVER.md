# DRY OS — Kapsamlı Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

- **Son Güncelleme:** 11 Kasım 2026 (`Milestone 1C` + `Low-Level Optimization Pass v1` Kapanışı)
- **Mevcut Durum:** **Faz 0 (`H1` & `H2`), `ADR-007` güvenlik mimarisi, Faz 1'in tüm kilometre taşları (`1A`, `1B-A`, `1B-B`, `1B-C`, `1B-D`, `1C`) ve `Low-Level Optimization Pass v1` `%100` tamamlandı, `0` Clippy uyarısı ve `56/56` otomatik QEMU/KVM senaryosu (`leaked_frames = 0`, `stale_tlb_reads = 0`, `uaf_errors = 0`, canlı QMP `screendump` piksel kanıtı) ile doğrulandı.**
- **Bu Belgenin Amacı:** Gelecek sohbetlerde Faz 2 (`IoRing`, `FaultEndpoint`, Ring 3 `DRY Vault`/`VFS`, `VirtIO` ve `H2` Sürücü Adası entegrasyonu) veya `Optimization Pass v2` (`PCID/INVPCID`, Doğrudan IPC Switch, `TSC-Deadline`) çalışmalarına sıfır bağlam kaybıyla, hiçbir gizli tuzağa düşmeden doğrudan devam edilmesini sağlamak.

---

## 1. Git Commit Zinciri ve Kilometre Taşı Özeti

| Aşama | Commit Hash | Kapsam Özeti | Doğrulama |
| :--- | :---: | :--- | :---: |
| `ADR-007` | `a443e6c` | `USERSPACE` / `ROOT_SYSTEM` / `KERNEL` 3-alan izolasyonu, `SecLabel` (8B), `DRY Vault` kiralama | Mimari + Öz-test |
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
| `Opt v1` | *HEAD* | `Low-Level Optimization Pass v1` (`null_sys 231 cyc`, `call_reply 1.952 cyc` [`12.8x`], `-16 KiB` meta) | `56/56` QEMU + Asm |

---

## 2. Kesinleşen Mimari Kararlar (`DECISIONS.md`) ve Faz 0 Kanıtları

1. **ADR-001 (Boot):** Limine Base Revision 3 (UEFI `OVMF` + doğrudan 64-bit SMP AP başlatma + HHDM + Boot Modules + GOP Framebuffer).
2. **ADR-002 (Syscall ABI):** `x86-64` `SYSCALL`/`SYSRETQ` (`RDI, RSI, RDX, R10, R8, R9` kontrol + `R12..R15` kopyasız 4 mesaj yazmacı + isteğe bağlı kullanıcı ek tamponu; `RAX` birincil dönüş / negatif `SysError`, `RDX` ikincil dönüş).
3. **ADR-003 (CSpace):** Sabit boyutlu korumalı düz tablo (`MAX_CSPACES = 16`, `CNODE_SLOTS = 256`, `64 B/slot`); `CapPtr` 64-bit düzeni (`guard:8 | radix:8 | slot_gen:16 | index:32`) Faz 3'te ABI kırmadan 2-seviyeli radiks ağacına genişleyebilir.
4. **ADR-004 (Scheduler):** TLC (`formal/h1/DrySched.tla`, `84.351` durum güvenlik + `54.095` durum canlılık, `0` ihlal) ve Rust `loom` (`39.135` C11 zayıf bellek permütasyonu + `>12M` 20-CPU stres geçişi) ile kanıtlanan `H1` protokolü + 8 öncelik seviyeli (`active_mask` `tzcnt`) `PerCpuRunQueue` + 4-sıçramalı IPC Öncelik Bağışı.
5. **ADR-005 (Lisans):** DRY Kernel ve native kullanıcı servisleri `MIT OR GPL-2.0-only`; Linux *Driver Island* ikilileri ve `experiments/h2-island` uyumluluk katmanı `GPL-2.0-only` (saf `Apache-2.0` ve `GPL-3.0` sürücü adalarında yasaktır).
6. **ADR-006 (Bağımsızlık):** `Gate 0` (`H1` + `H2` `igc` `v6.12`/`v6.18` `%0.00` upstream farkla `3.028 SLOC` userspace PoC + `vfio-pci` canlı `e1000e` DMA testi) geçilmiştir; çekirdek gelişimi fiziksel silikon IOMMU doğrulamasını beklemez.
7. **ADR-007 (Güvenlik & `DRY Vault`):** `USERSPACE (0)`, `ROOT_SYSTEM (1)` ve `KERNEL (2)` alan duvarları; 8-bayt `SecLabel`; path-bağımsız `CapRights + SecLabel + lease_epoch` ortak denetimi (`authorize_cspace_fast`); `ROOT_SYSTEM` süreçlerinin bile `KERNEL` belleğine erişememesi; `READ_ONLY_EXPORT` için `CSpace` + `VSpace` + `PMM` 3-katmanlı salt-okunur kalkanı.

---

## 3. Depo ve Alt-Sistem Haritası (Tüm Dosyalar `< 1.000` Satır Tavanı Altında)

### 3.1. Çekirdek (`kernel/src/`)
- **Önyükleme, Donanım ve Per-CPU:**
  - `main.rs`: `bsp_kmain` orkestrasyonu, faz geçişleri ve deterministik `QEMU isa-debug-exit` (`0xf4`) kapanışı.
  - `limine.rs`: Limine Base Rev 3 HHDM, bellek haritası, SMP, modül (`get_boot_module_by_name`) ve GOP (`BootFramebufferInfo`) sorguları; `cmdline` `.bss` mandallama.
  - `arch.rs`: `CLI/STI`, `RDMSR/WRMSR`, `CR0/CR2/CR3/CR4`, `RDTSC`, `INVLPG`, `port I/O`.
  - `gdt_tss.rs`: CPU başına özel 7-girişli `GDT`, `TSS` (`RSP0` + `IST1` `#DF`), `set_cpu_tss_rsp0`, `verify_cpu_tss_and_canaries` (tam) ve `verify_cpu_stack_canaries_fast` (`STR` VM-exit'siz sıcak yol).
  - `percpu.rs`: `256 B` (`4 x 64 B` önbellek hattı) `PerCpuData`, `IA32_GS_BASE` / `IA32_KERNEL_GS_BASE` / `IA32_FS_BASE` bağlaması.
  - `idt.rs`: 256-girişli assembly `TrapFrame` IDT, MSR-doğrulamalı koşullu `NMI/#DF` `swapgs`, CPL=3 istisna izolasyonu (`terminate_current_user_thread_fault`), `#PF` `uaccess` `extable` kurtarması, Local APIC Timer (`0xEC`), TLB Shootdown (`0xEE`) ve Spurious (`0xFF`) kesme kapıları.
  - `apic.rs`: `8259 PIC` maskeleme, `x2APIC` MSR (`0x800+`) / `xAPIC` UC MMIO otomatik seçimi, 3-pencereli PIT Kanal 2 kalibrasyonu, `arm_default_oneshot_tick` (tam kurulum) ve `rearm_oneshot_tick_count_only` (`0x838` tek-yazmaç hızlı yeniden kurma).
  - `smp.rs`: Limine MP AP başlatma, `Release -> Acquire` senkronizasyon bariyerleri (`ApEpochKind`).
  - `serial.rs`: `16550 UART` (`COM1 0x3F8`) kilitli ve panik-güvenli (`emergency_print`) seri çıktı.
  - `security.rs`: `ADR-007` `SecDomain`, `SecFlags`, `CapRights`, `SecLabel` (8B paketleme) ve `authorize_cspace_fast`.
  - `uaccess.rs`: `CPUID` kapılı `CR4.SMAP = 1`, alt-yarı `U/S=0` denetimi, `STAC`/`CLAC` daraltılmış assembly kopyası ve `[__dry_uaccess_fault_start, __dry_uaccess_fault_end)` istisna tablosu.
  - `display.rs`: UEFI GOP doğrulaması (`ReservedOrMmio`), tekil kök `CapObjectKind::Framebuffer` üretimi ve piksel bütünlüğü denetimi.
- **Zamanlayıcı (`kernel/src/sched/`):**
  - `state.rs`: `H1` `AtomicU32(exec_state:2, park_token:2, _pad:2, gen26:26)`, `SeqCst` Dekker çitleri ve `stack_owner`.
  - `runqueue.rs`: `StaticRunQueue` (128 hücreli Vyukov MPMC halka) + `PerCpuRunQueue` (`active_mask: AtomicU8` `tzcnt` taraması, `prio 0` CPU-sabit, `prio 1..7` iş-çalma uyumlu).
  - `context.rs`: `SwitchFrame`, `dry_switch_to` ve `dry_thread_entry_trampoline` (`global_asm!`).
  - `preempt.rs`: `on_lapic_timer_irq`, `preempt_disable`/`preempt_enable`, `timer_needs_rearm` takibi ve kesme fırtınası bekçisi.
  - `donate.rs`: 4-sıçramalı (`MAX_DONATION_DEPTH = 4`) geçişli IPC Öncelik Bağışı, `-EDEADLK` döngü tespiti ve taban önceliğe geri dönüş.
  - `mod.rs`, `tests.rs`, `preempt_tests.rs`: `TCB` havuzu (`24` idle + `36` worker = `60` yuva, her biri özel 16 KiB `.bss` yığını), `complete_post_switch_from_asm`, `yield_now`, `prepare_park_current`/`commit_park_current`, `wake_thread`.
- **Fiziksel Bellek (`kernel/src/pmm/`):**
  - `meta.rs`, `mod.rs`, `huge_reclaim.rs`, `untyped.rs`, `tests.rs`: `BootstrapAllocator`, `16 B` `FrameMeta` dizisi (`%0.39` ek yük), `PerCpuFrameCache` (`MAGAZINE_CAP = 64`), `Order4KiB`/`Order2MiB` tahsis/iade, kompartmanlar arası `rep stosq` kazıma, `Untyped` `split / retype / revoke`.
- **Sanal Bellek (`kernel/src/vspace/`):**
  - `pte.rs`, `mod.rs`, `huge.rs`, `tlb.rs`, `tlb_debt.rs`, `kernel_handover.rs`, `tests.rs`: `MAX_VSPACES = 16`, 4-seviye sayfa tabloları, `W^X`, `map_4k` / `map_mmio_4k` / `unmap_4k` / `unmap_4k_batch` / `protect_4k`, `2 MiB` büyük sayfa desteği, `KERNEL_VSPACE` devri ve `active_cpus` hedefli SMP TLB shootdown.
- **Yetki Uzayı (`kernel/src/cspace/`):**
  - `cap.rs`, `mod.rs`, `revoke.rs`, `objects.rs`, `tests.rs`, `bench.rs`, `fault_tests.rs`: Korumalı `CNode` (`256` yuva), kilitsiz seqlock `cspace_lookup_fast` + `with_pinned_cap` (`lookup_slot_unchecked_bounds`), monotonik `derive / copy / mint / transfer`, intrüzif çapraz-`CSpace` CDT `revoke` + otomatik `VSpace` `unmap`.
- **Süreçler, Sistem Çağrıları ve IPC (`kernel/src/proc/`, `syscall/`, `ipc/`):**
  - `proc/{elf.rs,mod.rs,supervisor.rs,tests.rs,bench.rs}`: Statik `ET_EXEC` ELF64 yükleyici, `align(64)` `ProcessSlot` (`MAX_PROCESSES = 24`), `SYS_PROC_SPAWN` / `SYS_PROC_WAIT` süpervizör yaşam döngüsü, sıralı/idempotent `reclaim_process`.
  - `syscall/{entry.rs,mod.rs}`: `STAR/LSTAR/FMASK`, `dry_syscall_entry`, `248 B` çerçeveli `rust_syscall_dispatch`, `SYS_ABI_PING`/`SYS_GET_TID` `CLI` hızlı yolu ve `20` sistem çağrısı (`0..=19`).
  - `ipc/{mod.rs,endpoint_state.rs,endpoint.rs,notif.rs,tests.rs,bench.rs}`: `MAX_ENDPOINTS = 32`, `MAX_NOTIFICATIONS = 32`, `align(64)` `TcbIpcSlot`, `ep_send_or_call`, `ep_recv`, `ep_reply`, birleşik `ep_reply_recv` (tek capability pini + tek spinlock), `notif_signal`/`notif_wait`.

### 3.2. Kullanıcı Alanı (`userspace/`) ve Araçlar (`tools/`)
- `userspace/src/lib.rs`: `no_std` `Syscall ABI v0.1` sarmalayıcıları (`sys_ipc_call`, `sys_ipc_reply_recv`, `sys_proc_spawn`, `sys_fb_map` vb.).
- `userspace/src/bin/`: `user_hello.rs` (ilk CPL=3 duman testi), `user_peer.rs` (`Stage F` güvenlik/izolasyon/hijyen/`DF=1` ajanı), `dry_init.rs` (`RootSystem` kök süpervizörü), `dry_echo.rs` (izole `Userspace` IPC sunucusu + çökme testi), `dry_display.rs` (`1280x800x32` Ring 3 GOP grafik çizici).
- `tools/`: `bootstrap_phase1.py` (SHA-256 kilitli `rust-src` + Limine), `qemu_boot_test.py` (`56` senaryoluk orkestratör + QMP `screendump`), `qemu_log_validators.py`, `qemu_user_validators.py`, `qemu_ipc_validators.py`.

---

## 4. Kritik İkili Düzenler ve Donanım Sözleşmeleri ( Değiştirirken Dikkat! )

### 4.1. `PerCpuData` (`256 B`, `#[repr(C, align(64))]` — `kernel/src/percpu.rs`)
Assembly kodları (`syscall/entry.rs`, `idt.rs`) aşağıdaki bayt ofsetlerine doğrudan `gs:[ofset]` ile erişir; derleme zamanı `const assert!` ifadeleriyle kilitlenmiştir:
- `gs:[0]` (`self_ptr: u64`): `PerCpuData` sanal adresi.
- `gs:[8]` (`cpu_idx: u64`): Mantıksal CPU kimliği (`0..MAX_CPUS`).
- `gs:[16]` (`lapic_id: u32`), `gs:[24]` (`magic: u64 = 0x4452_595f_5043_5055`).
- `gs:[32..64]`: `boot_stack_base`, `boot_stack_top`, `df_ist1_base`, `df_ist1_top` (`verify_cpu_stack_canaries_fast` tarafından `STR` VM-exit'siz okunur).
- `gs:[64..128]`: `tss_base`, `gdt_base`, `bp_trap_count`, `last_bp_rip`, `exec_ticks`, `current_slot`, `ctx_switches` (`CLI` altında `LOCK`'sız güncellenir), `in_df_handler`, `need_resched`, `timer_armed`, **`timer_needs_rearm` (ofset `104`)**.
- `gs:[128..192]`: `timer_irqs` (`128`), `preemptions`, `deferred_rescheds`, `last_progress_seq`, `preempt_count`, `irq_depth`, `quantum_rem`, `storm_streak`, `storm_slot`, `nmi_count`.
- **`gs:[192]` (`PCPU_OFF_KERNEL_RSP0`)**: Aktif iş parçacığının `stack_top` değeri (`TSS.RSP0` ile her `switch_to_slot` içinde senkron tutulur).
- **`gs:[200]` (`PCPU_OFF_USER_RSP_SCRATCH`)**: `SYSCALL` girişinde kullanıcı `RSP` geçici saklama alanı.
- `gs:[208..256]`: `cur_fs_base`, `cur_user_gs_base`, `cr3_noops_local`, `cr3_switches_local`, `syscall_count` (`CLI` altında `LOCK`'sız `inc`), `user_fault_count`.

### 4.2. `DRY Native Syscall ABI v0.1` (`0x0001_0000` — `syscall/entry.rs` & `syscall/mod.rs`)
- **Yazmaç Sözleşmesi:**
  - Giriş: `RAX = nr`, `RDI = a0`, `RSI = a1`, `RDX = a2`, `R10 = a3`, `R8 = a4`, `R9 = a5`; satır-içi kopyasız IPC mesaj yazmaçları `R12 = mr0`, `R13 = mr1`, `R14 = mr2`, `R15 = mr3`.
  - Dönüş: `RAX` birincil sonuç (`>= 0` başarı veya negatif `SysError`: `-1 EPERM`, `-9 EBADCAP`, `-11 EAGAIN`, `-12 ENOMEM`, `-14 EFAULT`, `-16 EBUSY`, `-22 EINVAL`, `-32 EPIPE`, `-35 EDEADLK`, `-38 ENOSYS`, `-110 ETIMEDOUT`, `-125 EREVOKED`, `-126 EIRQCTX`), `RDX` ikincil çıktı (`reply_token` / `badge` / `pid` / aktarılan `CapPtr`).
  - Hijyen: `RBX, RBP, R12..R15` korunur (IPC alım çağrılarında `R12..R15` gelen mesajı taşır); `SYSRETQ` dönüşünde `RDI, RSI, R8, R9, R10` sıfırlanır (`IRETQ` yedek yolunda ek olarak `RCX, R11` sıfırlanır).
  - Donanım Bayrakları: `IA32_FMASK` her `SYSCALL` girişinde `TF | IF | DF | IOPL | NT | AC` bitlerini donanım mikrokodunda temizler (`cld` veya `clac` gerekmez; `debug_assert_eq!` ile doğrulanır).
- **Mevcut Sistem Çağrısı Tablosu (`0..=19`, `kernel/src/syscall/mod.rs`):**
  - `0: SYS_ABI_PING`, `1: SYS_WRITE_DEBUG`, `2: SYS_YIELD`, `3: SYS_EXIT`, `4: SYS_GET_TID`, `5: SYS_CAP_QUERY`
  - `6: SYS_EP_CREATE`, `7: SYS_IPC_SEND`, `8: SYS_IPC_RECV`, `9: SYS_IPC_CALL`, `10: SYS_IPC_REPLY`, `11: SYS_IPC_REPLY_RECV`
  - `12: SYS_NOTIFY_CREATE`, `13: SYS_NOTIFY_SIGNAL`, `14: SYS_NOTIFY_WAIT`, `15: SYS_CAP_DERIVE`, `16: SYS_CAP_REVOKE`
  - `17: SYS_PROC_SPAWN`, `18: SYS_PROC_WAIT`, `19: SYS_FB_MAP`.

### 4.3. Sanal Adres Alanı Haritası (`VSpace` — `kernel/src/vspace/pte.rs`)
- `0x0000_0000_0000_0000 .. 0x0000_0000_0000_1000`: Kalıcı eşlenmemiş `NULL` koruma sayfası.
- `0x0000_0000_0040_0000+`: Statik `ET_EXEC` kullanıcı ELF64 segmentleri (`.text` `USER_RX`, `.rodata` `USER_RO`, `.data/.bss` `USER_RW`, `W^X` zorunlu).
- `0x0000_7d00_0000_0000+` (`USER_FB_MMIO_BASE`): Yetenek-kapılı Ring 3 GOP Çerçeve Tamponu (`PTE_USER | PTE_WRITABLE | PTE_PWT | PTE_PCD | PTE_NX | PTE_SW_MMIO`; normal RAM iade yoluna girmez).
- `0x0000_7fff_f000_0000 .. 0x0000_7fff_f001_2000`: Kullanıcı yığını (`4 KiB` alt koruma + `64 KiB` `USER_RW+NX` yığın + `4 KiB` üst koruma).
- `0xffff_8000_0000_0000+` (`PML4[256..]`): Limine HHDM doğrudan fiziksel RAM eşlemesi (`U/S = 0`, `NX = 1`; `.text/.rodata` HHDM takma adları `RO+NX`, `ReservedOrMmio` delikleri eşlenmemiş).
- `0xffff_ffff_8000_0000+` (`PML4[511]`): Çekirdek yüksek-yarı imajı (`.text` `RX`, `.rodata` `RO+NX`, `.data/.bss` `RW+NX`).

---

## 5. Asla İhlal Edilmeyecek Güvenlik ve Mimari Değişmezler

1. **`H1` Bellek Sıralaması ve Yığın Sahipliği (`sched/state.rs`, `sched/mod.rs`):**
   - `prepare_park` ve `Notification::signal` içindeki açık `fence(Ordering::SeqCst)` çağrılarını **asla kaldırma** (`TLC` Bug 2 ve `loom` ile kanıtlandı: x86-64'te tek başına `Release`/`Acquire` store-load yeniden sıralamasını engellemez).
   - Bir `TCB` yığını yalnızca `stack_owner == NO_CPU` ve `ExecState::Dead` iken yeniden tahsis edilebilir; `CSpace` sahibi `TCB` yuvaları yalnızca `cspace_destroy_object -> release_dead_tcb_slot` tamamlandığında havuza döner.
2. **Koşullu Local APIC One-Shot Yeniden Kurma Sözleşmesi (`percpu.rs`, `apic.rs`, `sched/preempt.rs`):**
   - `on_lapic_timer_irq` tetiklendiğinde `pcpu.timer_needs_rearm = true` olarak işaretlenir. Gönüllü geçişlerde (`yield_now`, bloklanan `IPC`) APIC sayacı hâlâ geri saydığı için `wrmsr` **yapılmaz** (`quantum_rem` yazılımda sıfırlanır); `rearm_oneshot_tick_count_only()` yalnızca `timer_needs_rearm == true` iken veya kesme dönüşünde çağrılır. Bu kural bloklanan IPC gecikmesini `12.8x` düşüren ana mekanizmadır.
3. **`16 KiB` Çekirdek Yığını Bütçesi Kuralı (Kritik!):**
   - Her CPU önyükleme/boşta yığını ve her worker TCB yığını tam **`16 KiB`** boyutundadır ve tabanında 8-bayt kanarya (`verify_cpu_stack_canaries_fast` / `verify_slot_canary`) bulunur.
   - `bsp_kmain` veya derin sistem çağrısı zincirlerine çağrılan test/benchmark yardımcılarında `>= 1 KiB` dizi/tampon varsa fonksiyon mutlaka `#[inline(never)]` ile işaretlenmelidir (bkz. `proc/bench.rs::run_stage_g_benchmarks`). `ipc/endpoint.rs` içindeki `[WakeBatchItem; 64]` (`~1.8 KiB`) yapılarını özyinelemeli zincirlerde büyütme.
4. **Kilit Sıralaması (Deadlock-Free DAG):**
   - `CDT_LOCK` -> `VSpace::lock` -> `TLB_REQ_LOCK` ve `Endpoint::lock` -> `DONATE_LOCK` sıralamaları tek yönlüdür. `Endpoint::lock` veya `TLB_REQ_LOCK` tutulurken asla uyuyan/bloklanan işlem veya `PMM` tahsisi yapma; `CLI` bekleme döngülerinde `poll_and_ack_pending_shootdown` çağrısını koru.
5. **`CSpace` `in_flight_ops` ve `SMAP` `extable` Sınırları:**
   - Çekirdek nesnesi üzerinde işlem yapan her sistem çağrısı `with_pinned_cap` kullanmalıdır (`slot.in_flight_ops` `SeqCst` pini `revoke` ile Dekker bariyeri kurar).
   - Kullanıcı belleğine doğrudan Rust referansı oluşturmak **kesinlikle yasaktır** (`CR4.SMAP = 1` anında Ring 0 `#PF` üretir); yalnızca `uaccess::copy_from_user` / `copy_to_user` kullanılmalıdır.

---

## 6. Güncel Performans Referans Tablosu (`Optimization Pass v1` Sonrası, `~3.11 GHz` KVM)

| Metrik (`p50` döngü) | `1 vCPU` | `2 vCPU` | `4 vCPU` | `8 vCPU` (`1.0x`) | `20 vCPU` (`2.5x`*) |
| :--- | :---: | :---: | :---: | :---: | :---: |
| `null_sys` (`SYSCALL -> SYSRETQ`) | **`231`** (`74 ns`) | **`231`** | **`257`** | **`257`** | **`257`** |
| `k2k_sw` / `u2u_same_vs` / `u2u_cross_vs` | `451 / 670 / 988` | `453 / 670 / 990` | `521 / 760 / 1.077` | `526 / 760 / 1.085` | `523 / 759 / 1.081` |
| `cspace_lookup_fast` / `sec_check` | `99` / `53` | `99..122` / `53..56` | `101` / `53` | `119` / `61` | `121` / `59` |
| `ep_lookup` / `notif_sig_wait` / `prio_donate` | `93 / 233 / 177` | `98 / 233 / 178` | `124 / 318 / 218` | `107 / 278 / 200` | `109 / 279 / 202` |
| `sched_wakeup` (Preemptive epoch) | **`1.037`** | **`3.315`** | **`2.766`** | **`3.107`** | **`3.576`** |
| `send_recv` (Bloklanan `Endpoint`) | **`2.070`** | **`3.657`** | **`2.175`** | **`2.555`** | **`2.087`** |
| `call_reply` (`CALL` + birleşik `REPLY_RECV`) | **`1.952`** (`0.63 µs`) | **`6.381`** (`4.173` xapic) | **`4.876`** | **`6.012`** | **`6.535`** |
| `cross_cpu` / `contended (4->1)` / `cap_transfer` | `1.952 / 6.507 / 2.416` | `6.278 / 9.809 / 7.624` | `5.283 / 8.655 / 6.435` | `5.651 / 9.770 / 7.347` | `6.694 / 8.586 / 6.614` |
| `PMM alloc_4k` / `free_4k` / `zero_4k` | `426 / 99 / 141` | `447 / 117 / 142` | `427 / 101 / 141` | `480 / 118 / 158` | `447 / 117 / 158` |

*(`*` `4C/8T` konakta `-smp 20` koşusu `2.5x` KVM aşırı-aboneliğidir; fiziksel 20 çekirdek performansını kanıtlamaz.)*

---

## 7. Tekrarlanabilir Derleme, Doğrulama ve Disassembly Komutları

1. ** Sadece Derleme + Clippy (`-D warnings`, hem `dry-userspace` hem `dry-kernel`):**
   ```bash
   python3 tools/qemu_boot_test.py --build-only
   ```
   *(Not: Repo-yerel `.toolchains/sysroot` ve `-Z build-std=core,compiler_builtins` bayrakları `qemu_boot_test.py` tarafından otomatik geçirilir; çıplak `cargo clippy` çalıştırma.)*
2. **Hızlı Tekil SMP Doğrulaması (`1`, `2`, `4`, `8` veya `20` vCPU, `~1.8..3.5 sn`):**
   ```bash
   python3 tools/qemu_boot_test.py --smp-only 2
   ```
3. **Tam Regresyon, Negatif Arıza ve Canlı QMP `screendump` Paketi (`56/56` senaryo, `~95 sn`):**
   ```bash
   python3 tools/qemu_boot_test.py
   ```
4. **Release Çekirdek Disassembly İncelemesi (`Intel` sözdizimi):**
   ```bash
   objdump -d -M intel target/x86_64-unknown-none/release/dry-kernel > /tmp/dry_kernel.asm
   ```
5. **Faz 0 `H1` (`loom`/stres) ve `H2` (`igc` Driver Island) Testleri:**
   ```bash
   cargo test -p h1-sched --release
   make -C experiments/h2-island test
   ```

---

## 8. Gelecek Sohbetler İçin Operasyonel Uyarılar (Tuzak Önleme)

1. **Dokunulmayacak Önceden Kirli Dosyalar:** `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` çalışma ağacında Faz 0'dan kalma yerel değişiklikler içerir. Kullanıcı açıkça istemedikçe bu iki dosyayı **asla `git add` yapma, düzenleme veya geri alma (`git checkout`)**.
2. **Zorunlu `Soft-Float` Derleme Hedefi:** Hem `kernel` hem `userspace` şu anda `-C target-feature=-sse,-sse2,-avx,+soft-float` ile derlenmektedir (`CR0.EM`/`CR4.OSFXSR` FPU bağlam değişimi henüz kurulmadı). Kullanıcı ikililerinde kayan nokta veya SIMD üretilecekse önce Faz 2'de per-TCB `XSAVEOPT`/`XRSTOR` bağlam alanı eklenmelidir; aksi hâlde `#UD`/`#NM` oluşur.
3. **1.000 Satır Dosya Tavanı:** Depodaki hiçbir dosya `1.000` satırı aşamaz (`800+` satıra yaklaşan `ipc/endpoint.rs` [`895`], `syscall/mod.rs` [`878`], `sched/mod.rs` [`863`], `EXPERIMENTS.md` [`804`] dosyalarına yeni özellik eklerken alt modüllere ayır!).
4. **AST Bilgi Grafı Senkronizasyonu:** Kod keşfinde önce `codebase-memory-mcp` (`search_graph`, `trace_path`, `get_code_snippet`) araçlarını kullan ve düzenleme turlarının sonunda `index_repository` ile grafı senkronize et.

---

## 9. Önerilen Faz 2 & `Optimization Pass v2` Yol Haritası

1. **Milestone 2A — `IoRing` Sıfır-Kopyalı Veri Düzlemi & `FaultEndpoint` Userspace Pager:**
   - Yüksek hacimli blok/ağ/ekran G/Ç için `CapObjectKind::IoRing` (sabitlenmiş paylaşımlı `SQ/CQ` halkaları + `Notification` kapı zili, veri yolunda sıfır sistem çağrısı kopyası).
   - Sayfa hatası (`#PF`) ve istisnaları çekirdekten güvenli biçimde kullanıcı alanı bellek yöneticisine yönlendiren `FaultEndpoint` protokolü.
2. **Milestone 2B — Ring 3 `DRY Vault`, `Policy Broker` & Yetenek-Etiketli `VFS`:**
   - `ADR-007` kiralama (`lease_epoch`), kullanıcı onayı ve etiket mirası kurallarını yürüten izole `RootSystem` `dry-vault` ve `dry-vfs` servisleri (`rename`/`symlink`/`hardlink`/`mount` yetki yükseltmeme kanıtları).
3. **Milestone 2C — Native Ring 3 `VirtIO` Sürücüleri & `H2` `igc` Sürücü Adası Entegrasyonu:**
   - `CapObjectKind::DmaArena` (`iommu_pin_count` kilitli) ve IRQ `Notification` bağlaması ile Ring 3 `virtio-blk` / `virtio-net` servisleri ve Faz 0 `H2` `igc` (`GPL-2.0-only`) sürücü adasının DRY native sistem çağrıları üzerine taşınması.
4. **Low-Level Optimization Pass v2 (Ölçümlü Sıradaki Kazançlar):**
   - **(1) `CR4.PCIDE = 1` + `INVPCID` 12-bit ASID nesilleri** (`u2u_cross_vs` `214..231 cyc` `CR3` yıkımını eleme), **(2) `SYS_IPC_CALL` -> `SYS_IPC_REPLY_RECV` doğrudan iş parçacığı el değiştirme** (`<1.000 cyc` hedefi), **(3) `IA32_TSC_DEADLINE` (`0x6E0`) + tickless boşta kalma (`NO_HZ`)**, **(4) `PMM` arka-plan toplu sıfırlama + kilit bölümleme**.
