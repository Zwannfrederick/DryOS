# DRY OS & DRY Kernel — Sistem Mimarisi

> **Durum Tarihi:** 17 Kasım 2026 (`Project Ascension 5.0 — v0.5.0`: `27/32` Ring-3 Modülü, Bare-Metal Boot Sertleştirme, `dry-usb`/`dry-nvme` RO, `libdry-lx-compat` v2 & `8139cp.c+mii.c`, `118`-SVG/`200%` DPI, `160 MiB` GPT Live USB)  
> **Aşama:** Faz 1..2, `Ascension 2.0..5.0` Gate 1–2 (`QEMU/OVMF` & Salt-Okunur Denetim) Tamamlandı; Fiziksel `82K1` Gates 3–7 `READY FOR USER PHYSICAL EXECUTION` (`ADR-001` .. `ADR-023` Yürürlükte)  
> **Epistemik Etiketler:** Bu belgedeki her kritik iddia aşağıdakilerden biriyle işaretlenmiştir:  
> - `[DOĞRULANDI]` — Birincil kaynak, ikili (binary) sembol analizi veya otomatik testlerle kanıtlandı.  
> - `[HİPOTEZ]` — Teknik olarak tutarlı ancak deney/bare-metal ile yanlışlanmaya açık tasarım iddiası.  
> - `[TAHMİN]` — Mühendislik ölçek/satır sayısı öngörüsü.  
> - `[TEST EDİLMEDİ]` / `[PLANNED]` — Henüz yürütülebilir kod veya fiziksel donanım üzerinde uygulanmadı/sınanmadı.

---

## 1. Tasarım Felsefesi ve Ayrıcalık Sınırları

**DRY (Minimal Mechanisms, Maximum Useful Parallelism)**; çekirdek içinde sürücü, dosya sistemi, ağ protokolü, POSIX veya Win32 kavramı barındırmayan, yetenek (*capability*) tabanlı `no_std` Rust mikroçekirdeğidir.

- **Sıfır Yönetim Çekirdeği:** Tüm mantıksal CPU'lar simetrik işçi (*peer worker*) olarak çalışır; işletim sistemi koordinasyonu için CPU ayrılmaz.
- **Global Kilit Yasağı:** Sıcak sistem çağrısı, IPC, bildirim (*notification*) ve zamanlayıcı uyandırma yollarında *Big Kernel Lock (BKL)* bulunmaz `[HİPOTEZ — Faz 0/1'de ölçülecek]`.
- **Çift Düzlemli İletişim:**
  - **Kontrol Düzlemi:** Yetki devri, hata (*fault*) bildirimi ve yaşam döngüsü için senkron `Endpoint` IPC + Öncelik Bağışı (*Priority Donation*).
  - **Veri Düzlemi:** `sDDF` / `io_uring` tarzı paylaşımlı bellek kilitsiz halkalar (`IoRing`) + yalnızca karşı taraf uyuyorsa çekirdeğe giren `Notification` kapı zili (*doorbell*) `[DOĞRULANDI: Milestone 2B dry-vfs <-> dry-blk ve Milestone 2C-0 batch=32 859 cyc/op ile kanıtlandı]`.
- **Hibrit Sürücü Stratejisi:** Önyükleme ve çekirdek depolama/sanallaştırma yolları (`VirtIO`, `NVMe`, `GOP`) native Rust ile yazılır; geniş PCIe çevre birimi ekosistemi (Ethernet, USB xHCI, seçilmiş Wi-Fi) Linux kaynak kodundan izole `GPL-2.0-only` userspace *Driver Island* süreçlerine uyarlanır (`ADR-005`, `ADR-006`).

```
+-----------------------------------------------------------------------------------+
|                  USERSPACE DOMAIN (Ring 3, SecDomain::Userspace)                  |
|                                                                                   |
|  [Native DRY Apps]      [Linux ELF Personality]       [Future Win32/NT Subsys]    |
|         |                         |                              |                |
|         +------------+------------+------------------------------+                |
|                      | Kontrol: Endpoint IPC | Veri: IoRing + Notification        |
|                      | Zorunlu Denetim: Object SecLabel + Process Capability Set  |
+----------------------|------------------------------------------------------------+
|             ROOT / SYSTEM DOMAIN (Ring 3, SecDomain::RootSystem)                  |
|                                                                                   |
|  [Root/Cap Mgr] [DRY Vault (Auth/Lease)] [Policy Broker] [Pager] [VFS] [Net Mux]  |
|  [Scoped Developer Workspace (DEV_WORKSPACE, izole compartment_id)]               |
|                  |                                     | CPU-Only IoRing          |
|  +---------------v---------------+     +---------------v-----------------------+  |
|  | Native Rust Sürücüler         |     | Linux Driver Islands (GPL-2.0-only)   |  |
|  | VirtIO-Blk/Net, NVMe, GOP     |     | Sürücü C Kodu + libdry_lx_<sınıf>     |  |
|  +---------------+---------------+     | Kalıcı IOVA DmaArena + Gölge vmemmap  |  |
+------------------|---------------------+-------------------+-------------------+--+
                   | Syscall ABI (ADR-002) + SecLabel/Lease  | Strict IOMMU (VT-d)
+------------------v-----------------------------------------v----------------------+
|              KERNEL DOMAIN (Ring 0, SecDomain::Kernel — Mutlak Duvar)             |
|  Untyped Retype | CSpace + SecLabel | VSpace | IOMMU Bind | IPC/Notif | Scheduler |
+-----------------------------------------------------------------------------------+
```

### Ring 0 vs. Ring 3 Sorumluluk Çizgisi

| Mekanizma | Konum | Gerekçe |
| :--- | :--- | :--- |
| `CR3` / MMU sayfa tablosu yazımı ve hedefli TLB shootdown | **Ring 0 (`KERNEL`)** | Donanım bellek izolasyonu kökü; `SecDomain::Kernel` Ring 3'e asla eşlenmez (`INV-SEC-01`). |
| IOMMU kök/domain tabloları ve IOTLB invalidation kuyruğu | **Ring 0 (`KERNEL`)** | Userspace DMA sürücülerinin çekirdeği ezmesini önleyen tek donanım engeli `[DOĞRULANDI]`. |
| CPU bağlam değişimi (`RSP`, FPU/SIMD), APIC/MSI-X yönlendirme | **Ring 0 (`KERNEL`)** | Ayrıcalıklı yazmaçlar (`MSR`, `XCR0`, `LAPIC`). |
| `CSpace` yetki, `SecLabel` alan tavanı, `lease_epoch` ve `Revoke` | **Ring 0 (`KERNEL`)** | Sahtelenemez zorunlu erişim kontrolü ilkelleri (`ADR-007`, `INV-SEC-01..08`). |
| `DRY Vault` (kimlik/lease), `Policy Broker`, `VFS`, sürücüler, TCP/IP | **Ring 3 (`ROOT_SYSTEM`)** | Politika ile mekanizma ayrımı; hata izolasyonu ve TCB küçültme (`ADR-007`). |

---

## 2. Çekirdek Nesne Modeli ve Denetlenmiş Native Syscall Yüzeyi

> **Not:** Önceki 11 çağrılık taslak değişmez kabul edilmemiştir (`ADR-002`). Yapılan güvenlik ve yaşam döngüsü denetimi sonucunda 3 kritik eksiklik tespit edilerek yüzey **12 temel mekanizmaya** revize edilmiştir (Faz 1 uygulama bulgularına göre evrilebilir).

### 2.1. Tespit Edilen Taslak Açıkları ve Düzeltmeler
1. **`Map` ve `Unmap` Ayrımı + IOMMU Pin Sayacı:** Bir fiziksel çerçeve veya `DmaArena` hem `VSpace`'te hem `IommuDomain`'de haritalı olabilir. `dry_vspace_unmap` ayrı bir işlem olmalı ve nesne üzerindeki atomik `iommu_pin_count > 0` iken fiziksel belleğin `Revoke`/geri dönüşümü reddedilmelidir.
2. **`TCB`-Bağlı `Notification` ve `FaultEndpoint`:** Tek thread'li olay-güdümlü bir sunucunun hem senkron `Endpoint` mesajlarını hem de asenkron `IoRing`/IRQ zillerini tek noktadan bekleyebilmesi için `TCB`'ye bir `Notification` bağlanabilmelidir (seL4 MCS modeli). Ayrıca sayfa hatası (`#PF`), istisna ve yabancı `SYSCALL` tuzaklarının userspace pager/personality sunucusuna iletilmesi için her `TCB`'nin bir `fault_ep` yetkisi bulunmalıdır.
3. **Çekirdek İçi Kullanıcı Pointer'ı Yasağı:** Çekirdek sıcak yolunda kullanıcı belleğinden pointer okunmaz (`copy_from_user` sayfa hatası riski taşımaz). Tüm sıcak argümanlar donanım yazmaçlarında, soğuk yapılandırma verileri ise göreve ait önceden sabitlenmiş (*pinned*) tek sayfalık **IPC Buffer** çerçevesi üzerinden aktarılır.

### 2.2. Başlangıç Çekirdek Çağrıları (`v0.1` Aday Arayüz)

| # | Çağrı | Amaç | Yaşam Döngüsü & Güvenlik Kuralı |
| :- | :--- | :--- | :--- |
| 1 | `dry_obj_retype` | `Untyped` bellekten çekirdek nesnesi (`TCB`, `VSpace`, `CSpace`, `Endpoint`, `Notification`, `PageTable`, `DmaArena`) türetir. | Çekirdek içi `kmalloc` yoktur; bellek hizalaması ve kalan kapasite atomik doğrulanır. |
| 2 | `dry_cap_mutate` | `Copy`, `Move`, `Mint` (hak daraltma/badge), `Delete`, `Revoke`. | `Revoke`, aktif CPU epoch bariyeri ve `iommu_pin_count == 0` koşuluna tabidir. |
| 3 | `dry_vspace_map` | `Frame` veya `DmaArena` yetkisini hedef `VSpace` sanal adresine eşler. | Yalnızca capability haklarının (`R/W/X`) alt-kümesi haritalanabilir (`W^X` varsayılan). |
| 4 | `dry_vspace_unmap` | Sanal adres aralığını çözer ve yalnızca `vspace.active_cpus` maskesine TLB shootdown gönderir. | Eşleme kalkmadan fiziksel çerçeve yeniden tiplenemez. |
| 5 | `dry_iommu_bind` | Soğuk yolda `DmaArena` yetkisini belirli PCI BDF `IommuDomain` IOVA tabanına bağlar / çözer. | `IOMMU_READ` (TX) ve `IOMMU_WRITE` (RX) ayrımını ve VT-d Snoop Control bitini zorlar. |
| 6 | `dry_ipc_call` | Kontrol düzlemi senkron çağrı (4 mesaj yazmacı + 1 yetki aktarımı + Öncelik Bağışı). | Hedef sunucuya tek kullanımlık (*single-shot*) `Reply` yetkisi üretir. |
| 7 | `dry_ipc_reply_recv` | Tek çekirdek girişinde önceki istemciye yanıt verir (`reply_cap` varsa) ve yeni mesaj/bağlı `Notification` bekler. | `reply_cap` boş geçilirse saf `Recv`, `wait_ep` boş geçilirse bloklanmayan saf `Reply` olarak çalışır. |
| 8 | `dry_notify_signal` | `Notification` kelimesine atomik `fetch_or(badge)` uygular ve bağlı görevi uyandırır. | Asla bloklanmaz; kesme bağlamından ve sürücü halkalarından güvenle çağrılır. |
| 9 | `dry_notify_wait` | `H1 v2` protokolüyle bildirim bitlerini atomik tüketir veya `deadline_tsc` anına dek uyur. | Erken uyanma veya görev silinmesinde `TCB.timer_node` yerel zamanlayıcı yığınından sökülür. |
| 10 | `dry_thread_ctrl` | `TCB` başlatma/durdurma, `FaultEndpoint` ve `Notification` bağlama, öncelik/CPU yakınlığı ve yazmaç okuma/yazma. | Yalnızca `EXEC_BLOCKED` / askıdaki veya hata durumundaki görevin yazmaçları dışarıdan değiştirilebilir. |
| 11 | `dry_irq_ctrl` | Donanım MSI-X/IOAPIC vektörünü `Notification` nesnesine bağlar; `IRQF_ONESHOT` maskesini kaldırır (`Ack`). | Vektör başına tek aktif sahip; sürücü çökerse vektör otomatik maskelenir. |
| 12 | `dry_sys_ctl` | Yalnızca kök platform yetkisine açık CPU hotplug, güç geçişi ve donanım topoloji sorgusu. | Kullanıcı süreçlerine asla devredilemez. |

### 2.3. `Milestone 1B-D` Ring 3 (`CPL=3`) Mimarisi ve `Native Syscall ABI v0.1` (`[DOĞRULANDI — 50/50 QEMU/KVM]`)

- **Ayrıcalık ve Adres Alanı Sınırı:**
  - Çekirdek yüksek yarıda (`0xffff_8000_0000_0000..`, `PML4[256..512]`, `U/S = 0`) çalışır; her kullanıcı süreci (`ProcessSlot`, `PID = 1..24`) kendine ait özel `VSpace` (`PML4[0..256]`) ve özel `CSpace` (`CSpaceId = 1..31`, `CSPACE_TABLE[0]` çekirdek rezervi) bağlamına sahiptir.
  - CPU başına `GDT` (`KERNEL_CS=0x08`, `KERNEL_DS=0x10`, `USER_DS=0x1b`, `USER_CS=0x23`, `TSS=0x28`), `TSS.RSP0` ve `PerCpuData.kernel_rsp0` (`gs:[192]`) her bağlam değişiminde hedef iş parçacığının 16 KiB çekirdek yığını tepesine senkronize edilir.
  - **`GS_BASE` ve `NMI/#DF` Güvenliği:** Ring 0'da `IA32_GS_BASE = &PerCpuData`, `IA32_KERNEL_GS_BASE = user_gs_base`; Ring 3'te tersidir. `SYSCALL` giriş/çıkışı `swapgs` kullanır; `IDT` ortak girişinde `CPL == 3` ise doğrudan `swapgs`, `vec == 2 (NMI)` veya `vec == 8 (#DF)` ise `IA32_GS_BASE` yüksek-yarı kontrolü yapılarak (`swapgs` penceresi koruması, `ebx` kalıcı bayrağı) çift-`swapgs` hatası önlenir `[DOĞRULANDI: F15 int 2 testi]`.
- **Donanım `SMAP` ve `extable` Tabanlı `uaccess` (`kernel/src/uaccess.rs`):**
  - Çalışma zamanı `CPUID.(EAX=7,ECX=0):EBX[20]` algılamasıyla tüm çekirdeklerde `CR4.SMAP = 1` ve `RFLAGS.AC = 0` etkinleştirilir; `IA32_FMASK` ve `dry_isr_common` kesme/çağrı girişinde `AC` bitini temizler.
  - `copy_from_user` / `copy_to_user`: Kanonik alt-yarı sınır doğrulaması (`[0x1000 .. 0x0000_8000_0000_0000)`), alt-yarı `U/S = 0` supervisor sayfası reddi (*confused-deputy* engeli) ve yalnızca `[__dry_uaccess_fault_start, __dry_uaccess_fault_end)` aralığında `STAC -> rep movsq/movsb -> CLAC` yürütür. Kullanıcı kaynaklı `#PF` hataları `try_fixup_uaccess_pf` istisna tablosu ile `-14` (`EFAULT`) döndürür; bu aralık dışındaki her Ring 0 `#PF` deterministik çekirdek paniğidir `[DOĞRULANDI: user_kernel_pf_not_masked & user_smap_ring0_bypass]`.
- **Statik `ET_EXEC` ELF64 Yükleyici ve Süreç Yaşam Döngüsü (`kernel/src/proc/`):**
  - Yalnızca statik `ET_EXEC` `x86-64` ELF64 ikilileri (`PT_INTERP` ve dinamik relokasyon reddedilir) sıkı `W^X` kuralıyla (`PF_W && PF_X -> Err(WxViolation)`) yüklenir; `p_memsz > p_filesz` `.bss` kuyruğu sıfırlanır ve kullanıcı yığını taban/tavan eşlenmemiş koruma sayfaları (*guard pages*) arasına yerleştirilir (`userspace/` crate: `user-hello.elf`, `user-peer.elf`).
  - Sıralı ve idempotent geri kazanım: `in_flight_syscalls == 0` (`SeqCst`) -> `TCB` `Dead && stack_owner == NO_CPU` (`release_dead_tcb_slot`) -> `VSpace.active_cpus == 0` -> `Frame4K` / `CapMapRecord` temizliği -> `destroy_cspace` -> `ProcessState::Reclaimed` (`leaked_frames = 0`).
- **Uygulanan `DRY Native Syscall ABI v0.1` (`ABI_VERSION = 0x0001_0000`, `kernel/src/syscall/`):**
  - Giriş: `RAX = num`, `RDI, RSI, RDX, R10, R8, R9 = arg0..arg5`; Dönüş: `RAX` (birincil veya negatif `SysError`), `RDX` (ikincil); `RBX, RBP, R12..R15` korunur, `RDI, RSI, R8, R9, R10` çekirdek sızıntısını önlemek için `0`'lanır. Kanonik hızlı yol `SYSRETQ` (`Pass v1` sonrası `p50 = 231 cyc` @ `1-2 vCPU`, `1df3842` tabanı `326 cyc`), kanonik olmayan/özel dönüş çerçevesi `IRETQ` yedek yolunu (`Pass v1` `p50 = 522 cyc`, taban `619 cyc`) kullanır.
  - Başlangıç v0.1 çağrı tablosu: `0 = SYS_ABI_PING`, `1 = SYS_WRITE_DEBUG` (yalnızca seri konsol tanılama, <= 512 B), `2 = SYS_YIELD`, `3 = SYS_EXIT`, `4 = SYS_GET_TID`, `5 = SYS_CAP_QUERY` (`CSpace` + `SecLabel` + `lease_epoch` doğrulamalı), ve `Milestone 1C` ile eklenen `6..19` IPC, süreç yönetimi ve GOP çerçeve tamponu çağrıları. FPU/SIMD `XSAVE` izolasyonu sonraki aşamadadır (`[PLANNED]`).

### 2.4. `Milestone 1C` Yetenek Tabanlı IPC, Kök Servis Süpervizörü ve Ring 3 GOP Çerçeve Tamponu + `Optimization Pass v1` (`[DOĞRULANDI — 56/56 QEMU/KVM]`)

- **Çift İlkelli İletişim (`kernel/src/ipc/`):**
  - **Senkron `Endpoint` (`endpoint_state.rs`, `endpoint.rs`):** `SYS_EP_CREATE (6)`, `SYS_IPC_SEND (7)`, `SYS_IPC_RECV (8)`, `SYS_IPC_CALL (9)`, `SYS_IPC_REPLY (10)` ve tek capability pini + tek spinlock altında birleştirilmiş sunucu hızlı yolu `SYS_IPC_REPLY_RECV (11)` (`Pass v1` sonrası `1 vCPU call_reply p50 = 1.952 cyc` [`0.63 µs`], `2..20 vCPU = 4.876..6.535 cyc`). `MsgTag` (`RAX/RSI`) + `R12..R15` (`mr0..mr3` = 32 B kopyasız yazmaç aktarımı), opsiyonel `<= 256 B` kullanıcı dilimi ve geleceğe uyumlu sıfır-kopya `SharedPayloadDesc` desteklenir.
  - **Tek-Kazananlı (`Single-Winner`) Uyandırma ve Tek-Atımlık `ReplyToken`:** Her bekleyen iş parçacığı uç nokta kilidi altında `Waiting* -> Delivered | Failed(err)` geçişini en fazla bir kez yapar (`0` kayıp mesaj, `0` çift uyandırma). `Endpoint::call` çağrısı `(ep_idx, caller_slot, caller_gen26, call_seq)` bağlı tek kullanımlık `ReplyToken` üretir; çift yanıt, token tekrarı veya istemci zaman aşımı/çıkışı `-EBADCAP`/`-EPIPE` döner. `irq_depth > 0 || preempt_count > 0` iken bloklanan IPC `-EIRQCTX (-95)` ile reddedilir.
  - **Asenkron `Notification` (`notif.rs`):** `SYS_NOTIFY_CREATE (12)`, bloklanmayan `SYS_NOTIFY_SIGNAL (13)` (`fetch_or` + `fence(SeqCst)`) ve `SYS_NOTIFY_WAIT (14)`.
  - **Sınırlı 4-Sıçramalı Öncelik Bağışı (`sched/donate.rs`):** Senkron `call` sırasında `caller.effective_prio < server.prio` ise en fazla `MAX_DONATION_DEPTH = 4` sıçrama boyunca öncelik yükseltilir; döngüsel çağrı zinciri (`A -> ... -> A`) `-EDEADLK (-35)` ile yakalanır ve `reply`/zaman aşımı/`revoke`/çıkış anında taban önceliğe geri dönülür.
- **Kök Kullanıcı Alanı Süpervizörü ve İzole Servis Yaşam Döngüsü (`userspace/src/bin/{dry_init,dry_echo}.rs`, `proc/supervisor.rs`):**
  - Çekirdek içinde gömülü servis yöneticisi yoktur. `dry-init.elf` (`SecDomain::RootSystem`), Limine modüllerine ait açık `CapObjectKind::BootModule` yetkileriyle başlatılır; `SYS_PROC_SPAWN (17)` ile ayrı `VSpace` + `CSpace` ve daraltılmış `SecDomain::Userspace` kompartmanında `dry-echo.elf` servisini başlatır, yalnızca türetilmiş `Endpoint` yetkisini devreder, `ping -> pong` RPC turunu doğrular, `ud2` (`#UD`) çöküşünü `SYS_PROC_WAIT (18)` ve `-EPIPE` ile algılayıp kaynakları geri kazanarak (`leaked_frames = 0`) servisi yeniden başlatır.
- **Yetenek-Kapılı Ring 3 UEFI GOP Grafik Ekranı (`display.rs`, `dry_display.rs`):**
  - Limine GOP doğrusal çerçeve tamponu (`1280x800x32`, `pitch=5120`, `FrameState::ReservedOrMmio` doğrulaması ve `pitch * height` taşma kontrolü) Ring 0 HHDM dışında tutulur; yalnızca `CapObjectKind::Framebuffer` yetkisi ve `SYS_FB_MAP (19)` üzerinden `PTE_USER | PTE_WRITABLE | PTE_PWT | PTE_PCD | PTE_NX | PTE_SW_MMIO` bayraklarıyla `dry-display.elf` kullanıcı adres alanına eşlenir, `revoke` veya çıkışta SMP TLB shootdown ile tamamen sökülür ve `-vga none` başsız (*headless*) kipte sistem hatasız devam eder.

### 2.5. `Milestone 2A-1` (`P1 First Interaction`) Yetenek-Kapılı IOAPIC IRQ, Dar Port G/Ç ve Ring 3 Etkileşimli Boru Hattı (`[DOĞRULANDI — 65/65 QEMU/KVM]`)

- **ACPI MADT Keşfi ve `IrqCap` / `IoPortCap` Donanım Sınırı (`kernel/src/acpi.rs`, `kernel/src/irq/`):**
  - `acpi.rs`, Limine Base Revision 3 altında HHDM dışında kalabilen `ACPI_NVS` / `RESERVED` RSDP ve XSDT/RSDT tablolarını `PML4[384]` üzerinde geçici `RO + NX` 3-sayfalık önyükleme penceresiyle güvenli okur; `MADT` (`APIC`) içinden `IOAPIC` taban adresini (`0xfec0_0000`, `FrameState::ReservedOrMmio`) ve `Interrupt Source Override` kayıtlarını (ISA `IRQ 0..15 -> GSI`, polarite ve tetikleme kipi) uydurma varsayım olmadan çıkarır ve pencereyi `INVLPG` ile söker.
  - Harici donanım kesmeleri `IDT` vektör `0x30..=0x3F` (`GSI 0..15`) aralığına yönlendirilir. Üst-yarı kesme işleyicisi (`handle_external_irq`), `IoApicHwCache` üzerinden sayfa tablosu yürüyüşü yapmadan hattı anında maskeler (`IRQF_ONESHOT`), `LAPIC EOI` gönderir, kesme fırtınası sayacını (`>= 256 IRQ / ~20 ms` -> `storm_clamped`) denetler ve bağlı `Notification` nesnesine bloklanmadan `notif_signal` tetikler (`irq_top p50 = 13.014 cyc`, `irq_ack p50 = 7.788 cyc`, `irq_wake p50 = 15.981 cyc`).
  - `CapObjectKind::IoPort` (`12`) ve `SYS_IOPORT_RW (22)` (`op=0` `inb`, `op=1` `outb`): `RFLAGS.IOPL == 0` değişmezi asla gevşetilmez; yalnızca ayrık beyaz listedeki cihaz portları (`i8042` `0x60`/`0x64` ve `COM2` `0x2F8`) `RootSystem` sürücüsüne verilebilir; PIC (`0x20`/`0xA0`), PIT (`0x40..0x43`), PCI (`0xCF8..0xCFF`), CMOS (`0x70..0x71`) ve QEMU çıkış portları sert kara listededir.
  - Yeni P1 sistem çağrıları (`syscall/{mod,cold}.rs`, sıcak `rust_syscall_dispatch` çerçevesi `248 B` korundu): `SYS_IRQ_BIND (20)`, `SYS_IRQ_ACK (21)` (`40 B` çerçeve, çift-ack `-EALREADY` reddi), `SYS_IOPORT_RW (22)`, `SYS_SYSINFO (23)` (`UserSysInfo` 80 B canlı telemetri), `SYS_FRAME_ALLOC (24)` ve `SYS_VSPACE_MAP (25)`.
- **İzole Ring 3 `dry-input -> dry-shell -> dry-display` Zinciri (`userspace/src/{input,keymap,ps2,gfx,surface,shell}.rs`):**
  - **`dry-input.elf` (`SecDomain::RootSystem`, `0x0003`):** `i8042` denetleyici kurulumu ve Set-1 / `0xE0` tarama kodu çözümlemesi tamamen Ring 3'te çalışır; cihazdan bağımsız `InputEvent` yapısı 64-olaylık taşma-korumalı halkada (`InputRingQueue<64>`) tutulur ve 4 yazmaçlı (`R12..R15`) IPC ile aboneye aktarılır. `KeymapLayout::UsQwerty` tam uygulanmıştır; `KeymapLayout::TurkishQ` genişletme arayüzü tanımlı olup dürüstçe `Err(LayoutNotImplemented)` döner (`[PLANNED]`).
  - **`dry-shell.elf` (`SecDomain::Userspace`, `0x0102`):** Sıfır donanım ve sıfır `Framebuffer` yetkisiyle çalışır (`ADR-007`). Girdi düzenleyici (`InputEditor`), ayrıştırıcı (`parse_command`), 64-satırlık kaydırma tamponu (`ShellEngine`) ve `SharedTerminalSurface` (`3.904 B <= Frame4K`) projeksiyonu ayrıştırılmıştır; `help`, `clear`, `system`, `uptime`, `echo`, `overflow-test`, `crash-input` ve `shutdown` komutlarını gerçek telemetriyle yürütür.
  - **`dry-display.elf` (`SecDomain::RootSystem`, `0x0004`):** `Order2MiB` (`Frame2M`) önbellekli (`WB`) RAM arka tamponu tutar, `dry-shell`'in `SharedTerminalSurface` sayfasını salt-okunur (`USER_RO`) eşler ve yalnızca `DirtyRegion` dikdörtgenlerini 64-bit yazımlarla GOP MMIO'ya aktarır (tek-satır tuş vuruşu `10.240 px = ~15.3k cyc` [`~5 µs`]; tam pencere `276.224 px = ~463k cyc` [`~149 µs`]; `Alt+Ok` ile gerçek taşınabilir floating pencere doğrulandı).

### 2.6. `Milestone 2A-2` (`DRY Desktop Preview`): Birleşik PS/2 Fare (`IRQ12`), Yazılımsal İmleç, Ring 3 Pencere Yöneticisi ve Çok-Uygulamalı İzolasyon (`[DOĞRULANDI — 79/79 QEMU/KVM]`)

- **Donanım ve Çekirdek Sınırı (`kernel/src/irq/mod.rs`, `cspace/objects.rs`, `syscall/cold.rs` — `0` Çekirdek UI Kodu):**
  - `ACPI MADT` üzerinden keşfedilen `ISA IRQ 12` (`PS/2 AUX` fare -> `GSI 12` -> `vec = 0x3c`) `IOAPIC` tablosuna eklenmiştir (`[DRY-IOAPIC] PASS: ... irq12_gsi=12 irq12_vec=0x3c`).
  - `cspace_vspace_unmap_2m` ve `VSPACE_MAP_OP_UNMAP_2M (5)` eklenerek `Order2MiB` arka tamponların çıkışta tam SMP TLB shootdown ile sökülmesi sağlanmıştır (`leaked_frames = 0`).
  - `UserSysInfo` (`104 B`, sıfır çekirdek adresi) hat bazlı `irq1_delivered`, `irq12_delivered` ve 64 B önbellek hizalı CPU-başına `total_ipc_msgs` sayaçlarıyla genişletilmiştir; sıcak `rust_syscall_dispatch` çerçevesi `248 B`, soğuk `sys_cold_dispatch` çerçevesi `216 B` korunmuştur.
- **Tek `i8042` Denetleyici Sahibi `dry-input` (`userspace/src/{ps2,input}.rs`, `bin/dry_input.rs`):**
  - Klavye ve fare aynı `0x60`/`0x64` portlarını paylaştığından `IRQ1` ve `IRQ12` tek `dry-input` sürecinde birleştirilmiştir (`ADR-013`, `INV-WM-01`). `init_ps2_aux_mouse` yardımcı portu (`0xA8`), `CCB` `IRQ1|IRQ12` bitlerini ve `0xD4 -> 0xF6 / 0xF4` akış kipini `ACK (0xFA)` / `RESEND (0xFE)` doğrulaması ve başsız *fallback* ile kurar.
  - Kesme uyanışında `status & 0x01 (OBF)` döngüsü `status & 0x20 (AUX_OBF)` bitine göre baytları klavye veya 3-baytlık fare çözücüsüne (`bit 3 == 1` senkron kilidi + taşma elemesi) ayırır; ardışık göreli hareketler `InputRingQueue::push_coalescing` ile kayıpsız birleştirilir (`46+43` eşzamanlı kesme selinde `desync = 0`).
- **Arka Tampon Denetimi, Yazılımsal İmleç ve Ring 3 Pencere Yöneticisi (`userspace/src/{surface,wm,compositor}.rs`, `bin/dry_display.rs`):**
  - **Arka Tampon Denetimi:** `1280x800x32` (`pitch = 5120`, `mapped_bytes = 4.096.000 B` = `1.000 x Frame4K`) doğrusal GOP alanı için `2 x Frame2M = 4.194.304 B` (`4 MiB WB RAM`) arka tampon ayrılır ve kanonik/hizalama sınırları doğrulanır (`INV-WM-05`).
  - **Sıfır-İzli Yazılımsal İmleç:** `WB` arka tampon imleçsiz sahneyi saklar; `12x16` imleç yalnızca GOP MMIO tarama satırı aktarımında anlık bindirilir. Fare hareketi yalnızca eski ve yeni `12x16` dikdörtgenleri (`92` değişen piksel, `cur p50 = ~8.8k..10.8k cyc` [`~3 µs`]) günceller.
  - **Sıfır-UB Atomik Yüzey ve Çok-Pencereli WM:** Paylaşımlı `Frame4K` üzerinde ham `&mut` referansı yasaktır; atomik tek/çift `seq` çitiyle `SurfaceSnapshot::capture_from_ro` okunur (`INV-WM-04`). Ring 3 `WindowManager` (`MAX_WINDOWS = 4`) Z-sırası, tıklamayla odak/öne getirme, başlık çubuğundan sürükleme, `[x]` kapatma, `Alt+Tab` ve 2-sütun `SideBySideTiled` (`Alt+T` / üst bar düğmesi) kiplerini rozet (`owner_badge`) doğrulamasıyla yönetir.
- **Eşzamanlı İzole Ring 3 Uygulamaları (`dry-shell` `0x0102` + `dry-sysmon` `0x0103`):**
  - **Pencere A (`dry-shell.elf`):** Odak kazandığında klavye olaylarını `WM_OP_POLL_KEY` üzerinden alır ve komutları yürütür.
  - **Pencere B (`dry-sysmon.elf`):** Ayrı `SecDomain::Userspace` kompartmanı, ayrı `VSpace` ve ayrı `CSpace` içinde gerçek `SYS_SYSINFO` telemetrisini (CPU/APIC, PMM bellek çubuğu, `Syscalls`, `IPC`, `IRQ1`, `IRQ12`) canlı çizer; `#UD` çöküşü enjekte edildiğinde süpervizör ve WM yalnızca Pencere B'yi temizlerken Pencere A kesintisiz çalışmaya devam eder (`INV-WM-06`).

### 2.7. `Milestone 2A-3` (`Functional Desktop Foundation, Native Applications & Power Lifecycle`): Birleşik Uygulama Yaşam Döngüsü, Ring 3 `dry-vfs`, Hibrit WM ve Güç/Oturum Mimarisi (`[DOĞRULANDI — 85/85 QEMU/KVM + 5/5 SMP/RAM Matrisi]`)

- **Çekirdek Mekanizma Eklentileri (`kernel/src/{acpi,power,idt,apic}.rs`, `sched/mod.rs`, `syscall/cold.rs` — `0` Çekirdek UI/VFS Politikası):**
  - **3 Yeni Soğuk Sistem Çağrısı (`248 B` sıcak çerçeve, `216 B` soğuk çerçeve korundu):** `SYS_PROC_LIST (26)` (`UserProcEntry` 24 B canlı süreç özeti), `SYS_PROC_KILL (27)` (süpervizör kontrollü sonlandırma) ve `SYS_POWER_CTL (28)` (`CapObjectKind::PowerCtrl = 13` yetenek kapılı güç işlemi).
  - **Donanım `CPU Idle` (`HLT`) ve Çapraz-CPU Uyandırma (`VEC_RESCHED_WAKE = 0xed`):** `idle_park_or_spin`, masaüstü oturumunda boşta kalan vCPU'ları yarışsız `cli -> kuyruk kontrolü -> sti; hlt` gölgesinde park eder; uzak CPU'daki `wake_thread` hedef CPU'yu `VEC_RESCHED_WAKE` IPI ile uyandırır; `UserSysInfo` (`120 B`) `idle_hlt_calls` ve `idle_hlt_tsc` sayaçlarını dışa aktarır.
  - **Gerçek ACPI Güç Keşfi ve S5/Reset (`acpi.rs`, `power.rs`):** `FADT` (`PM1a_CNT_BLK = 0x604`, `RESET_REG = 0xcf9`/`0x0f`), `FACS` ve `DSDT` AML `\_S5_`/`\_S3_` paketleri önyüklemede doğrulanır. `POWER_OP_SHUTDOWN_S5` gerçek ACPI S5 donanım kapanışı (`rc=0`), `POWER_OP_REBOOT` gerçek FADT donanım sıfırlaması (`rc=0`) uygular; `POWER_OP_SUSPEND_S3` ise `< 1 MiB` gerçek-mod uyanma trampolini hazır olmadığından sahte uyku yapmayıp `-ENOSYS` döner (`ADR-014`).
  - **`PMM` ve SMP Kararlılık İyileştirmeleri (`pmm/tests.rs`, `vspace/tlb.rs`, `apic.rs`):** OOM öz-testi intrüzif HHDM sayfa bağlantısına taşınarak `-512 KiB` çekirdek `.bss` tasarrufu sağlandı ve `256M -> 2G+` (`516.802` serbest çerçeve) ölçeklenmesi doğrulandı; `PIT` kalibrasyonunda `GATE2=1` öncesi `TIMER_INIT`+`rdtsc` kurulumu ve `TLB_REQ_LOCK` TTAS + `NEXT_REQ_GEN` ilerleme bekçisiyle 20-vCPU aşırı-abonelik kararlılığı güçlendirildi.
- **12 Ring 3 ELF64 İkilisi, `Launcher`, Yerel Uygulamalar ve Servisler (`userspace/src/`):**
  - **Sistem Servisleri (`SecDomain::RootSystem`):** `dry-init.elf` (`supervisor.rs` + `sup_services.rs`), `dry-input.elf` (`0x0003`: PS/2 IntelliMouse 4-bayt tekerlek `MouseScroll` + canlı `UsQwerty`/`TurkishQ` geçişi + imleç hız çarpanı), `dry-display.elf` (`0x0002`: `MAX_WINDOWS = 6`, `Floating`/`SideBySideTiled`/`Stacked`/`Tabbed`, `WindowRole::DialogTransient`, `High Contrast`, `2x` büyük imleç, `DPMS` `0x000000` karartma + 5 sn hareketsizlik sayacı ve kimlik doğrulamasız `Session Lock` kalkanı), `dry-vfs.elf` (`0x0004`: salt-okunur `/boot` + kompartman-yalıtımlı uçucu `/ram`), `dry-power.elf` (`0x0005`: kontrollü servis/uygulama kapatma + `SYS_POWER_CTL`).
  - **Yerel Uygulamalar (`SecDomain::Userspace`, Ayrı `VSpace` + `CSpace` + Kompartman):**
    1. **`Terminal` (`dry-shell.elf` `0x0102`):** Satır içi imleç düzenleme (`Sol/Sağ/Home/End/Delete/Backspace`), komut geçmişi (`Yukarı/Aşağı`), `PageUp/PageDown` + fare tekerleği kaydırma ve gerçek API'ye bağlı `help`, `clear`, `echo`, `sysinfo`, `ps`, `ls`, `cat`, `write`, `rm`, `open`, `blank`, `lock`, `suspend`, `reboot`, `poweroff`, `shutdown` komutları.
    2. **`Life Station` (`dry-sysmon.elf` `0x0103`):** Gerçek `SYS_SYSINFO` + `SYS_PROC_LIST` üzerinden CPU/APIC, `HLT` boşta kalma çağrıları, PMM bellek çubuğu, `Syscalls`, `IPC`, `IRQ1/12` ve çalışan süreç tablosu.
    3. **`Files` (`dry-files.elf` `0x0104`):** `dry-vfs` üzerinden salt-okunur `/boot` modül inceleme ve uçucu `/ram` içinde dosya oluşturma (`[N]`), görüntüleme (`Enter`), yeniden adlandırma (`[R]`) ve silme (`[D]`).
    4. **`Settings` (`dry-settings.elf` `0x0105`):** Canlı uygulanan 7 tercih satırı (`WM Layout`, `Keymap`, `Cursor Speed`, `Theme`, `Cursor Size`, `Idle DPMS`, `Session Lock / Reboot`) ve `Tiling` düzenini bozmayan `[H]` `DialogTransient` hızlı yardım penceresi.

### 2.8. `Milestone 2B`: Kalıcı VirtIO-Blk Depolama, Kilitsiz `IoRing`, Çökme-Güvenli `DRYFS_v1` ve `DRY Vault` Güvenlik Zorlaması (`[DOĞRULANDI — 88/88 QEMU/KVM + 5/5 SMP/RAM Matrisi]`)

- **Çekirdek Yetenek Kapıları ve `BME=0` Donanım Susturma (`kernel/src/{pci,vault_ctrl}.rs`, `syscall/cold.rs` — `0` Çekirdek Blok/FS/Kripto Politikası):**
  - **3 Yeni Soğuk Sistem Çağrısı (`rust_syscall_dispatch = 248 B` sıcak çerçeve korundu):**
    1. `SYS_DEV_MAP (29)`: `CapObjectKind::PciDev (14)` doğrulamasıyla PCI VirtIO 1.0 Modern blok denetleyicisinin (`1AF4:1042`/`1001`) doğrulanmış MMIO BAR sayfalarını `VmProt::USER_MMIO_UC` olarak eşler ve fiziksel adres içermeyen 32 B `UserVirtioPciInfo` döndürür.
    2. `SYS_DMA_ALLOC (30)`: Yalnızca `DEV_WORKSPACE` olmayan `SecDomain::RootSystem` sürücüsüne `SecFlags::DEVICE_DMA | NO_DELEGATE` etiketli, `iommu_pin_count == 1` sabitlenmiş 4 KiB DMA çerçeveleri verir (`INV-CAP-08`). Sürücü çöktüğünde/sonlandığında `quiesce_pci_for_cspace` önce donanımda PCI `Command.BME = 0` bitini kapatır, ardından DMA pinlerini çözerek sayfaları sızıntısız geri kazanır (`R-03`, `pci_quiesce >= 1`).
    3. `SYS_VAULT_CTL (31)`: `CapObjectKind::VaultCtrl (15)` üzerinden `QUERY_EPOCH (0)`, $O(1)$ `GLOBAL_LEASE_EPOCH` ilerletme (`ADVANCE_EPOCH = 1`, eski kiralamalara `-EREVOKED`, `INV-SEC-07`) ve `READ_ONLY_EXPORT` çerçeve mühürleme (`SEAL_RO_EXPORT = 2`, `W^X` `INV-SEC-03`) sağlar.
- **Ring 3 `dry-blk.elf` (`0x0006`), Kilitsiz `IoRing` (`ioring.rs`) ve `DRYFS_v1` (`persist_fs.rs` `/persist`):**
  - `dry-blk.elf`, VirtIO 1.0 Modern `virtq` halkasını `SYS_DMA_ALLOC` çerçeveleri üzerinde çalıştırır; `dry-vfs.elf` ile paylaşımlı `IoRingSharedPage` (`SQ`/`CQ`, `CAP = 32`) + 4 KiB veri penceresi + `Notification` kapı zilleri üzerinden konuşur.
  - `DRYFS_v1`, çift A/B `CRC32` süperblok (`tx_seq`) ve 4 KiB Copy-on-Write veri blokları + çift `VIRTIO_BLK_T_FLUSH` bariyeriyle güç kesintilerine (`SIGKILL`) ve süperblok bozulmalarına karşı atomik kurtarma sağlar; her inode `ADR-007` `SecLabel` taşır ve çapraz bağlama/kompartman `link` yükseltmelerini (`-EXDEV` / `-EPERM`) reddeder (`INV-SEC-05`).
- **Ring 3 `DRY Vault` (`dry-vault.elf` `0x0007`), Kriptografik `Session Lock` ve `DEV_WORKSPACE` (`0x002a`):**
  - Saf `no_std` tuzlu `PBKDF2-HMAC-SHA256` (`vault_crypto.rs`) doğrulayıcısı `/persist/vault.sec` (`RootSystem | NO_DELEGATE`) üzerinde saklanır (`0` düz metin parola); masaüstü `Session Lock` kalkanı gerçek parola doğrulamasıyla açılır ve `GLOBAL_LEASE_EPOCH` sayacını döndürür; masaüstü tercihleri `/persist/desktop.cfg` içinde yeniden başlatmalar boyunca yaşar; `DEV_WORKSPACE` kompartmanının donanım/DMA/Vault yetkilerine erişimi çekirdekte `-EPERM` ile engellenir.

### 2.9. `Milestone 2C-0`: Minimal Adaptive Driver Island PoC — Yalnızca Kontrol Düzlemi `Device Broker` & `Mock Driver Island` (`[DOĞRULANDI — 90/90 QEMU/KVM + 5/5 Matris]`)

- **Sıfır Yeni Syscall ile Kesin Kontrol/Veri Düzlemi Ayrımı (`userspace/src/island_proto.rs`, `bin/{dry_broker,dry_mock_drv}.rs`, `ADR-016`):**
  - **`Device Broker` Yalnızca Kontrol Düzlemidir (`SecDomain::RootSystem`, `0x0008`):** Yalnızca mevcut `Syscall ABI v0.1` (`0..=31`) ilkelleriyle istemci rozetini (`BADGE_BROKER_CLIENT` / `BADGE_BROKER_ADMIN`) doğrular, `dry-mock-drv.elf` sürecini başlatır/izler ve daraltılmış nesil-damgalı (`island_session_badge(gen)` — 32-bit nesil) `Endpoint` (`READ|SEND|CALL`), `IoRingSharedPage` (`READ|WRITE|MAP`, `GRANT` yok) ve `Notification` (`sq_notif: READ|SIGNAL`, `cq_notif: READ|WAIT`) kiralamalarını devreder (`INV-ISL-04`). **Normal istemci veri trafiğinin `Broker` üzerinden geçmesi deterministik olarak `-EPERM` ile reddedilir (`INV-ISL-01`, `unauth_denied = 4`)**; ölçüm kanıtı olarak `Broker` sıçraması `b_p50 = 10.165 cyc` (`3.27 µs`) iken doğrudan kiralık `Endpoint` `d_p50 = 5.270 cyc` (`1.69 µs`, `1.93x`) ve `batch=32` `IoRing` **`804 cyc/op` (`0.26 µs/op`, `12.64x` kazanç)** sağlar (`docs/EXPERIMENTS_2C0_ISLAND_POC.md`).
  - **`Mock Driver Island` (`SecDomain::Userspace`, `0x0209`):** Ayrı donanım `VSpace` + `CSpace` içinde çalışır (`sys_proc_kill(1) == -EPERM`, `INV-ISL-02`), nesle bağlı deterministik `SplitMix64 + CRC32` dönüşümünü doğrudan kiralanmış `Endpoint` ve `IoRingSharedPage` üzerinden yürütür, iş yokken `timeout_tsc = 0` ile sonsuz olay beklemesinde (`0` boşta syscall, `> %99.8` donanım `HLT`) uyur ve kapı ziliyle uyanır (`w1_p50 = 13.707 cyc` [`4.40 µs`]).
  - **Tam Önceki-Nesil Yetki İptali ve Çökme Kurtarması (`INV-ISL-03`):** Ada `ring_cap` yetkisini `Device Broker`'a `RIGHT_REVOKE` ile devreder. `#UD` (`ud2`) çöküşünde veya nesil dönüşümünde `Device Broker` `sys_proc_wait` ile süreci toplar, `sys_cap_revoke(ring_cap, 0)` (`cspace_revoke_children` + istemci `VSpace` `unmap` + senkron SMP TLB shootdown) ve `sys_cap_revoke(..., 2)` ile önceki neslin **tüm** kiralık yetkilerini (`drv_ep`, `sq_notif`, `cq_notif`, `ring_cap`) ve bayat nesil damgalarını geçersiz kılar (`stale_revoked = 5/5`) ve `1.42 ms` (`p50`) içinde `gen + 1` adasını sızıntısız (`pmm_act = 31` çerçeve, `pmm_leak = 0`) başlatır.
- **`Ascension 2.0` Kapanışı (`DONE` — `8572da3..1879128`, `94/94` QEMU/KVM + `5/5` Matris):**
  1. **`Milestone 2A-4a`:** `MAX_LATCHED_MODULES = 32`, `XSAVEOPT64`/`XRSTOR64` + `FXSAVE64` (`512 B` fallback) per-thread FPU/SIMD izolasyonu (`kernel/src/fpu.rs`), `FaultEndpoint` (`kernel/src/proc/fault.rs`) ve `AUD-K1..K15` / `AUD-U1..U9` çekirdek sertleştirmesi (`rust_syscall_dispatch = 248 B`).
  2. **`Milestone 2C-1` & `Priority C`:** İzole Ring 3 `GPL-2.0-only` Linux `v6.18` `virtio-rng.c` adası (`dry-lx-rng.elf`, `drivers/lx-virtio-rng-island/`) ve Ring 3 `VirtIO-GPU` 2D scanout (`dry-gpu.elf`) + canlı `UEFI GOP Fallback`.

### 2.10. `Milestone 2C-2` (`8d230df`, `95/95` QEMU): Donanım Temelleri (`VT-d`, `MSI/MSI-X`, `>4 GiB` `BAR` & `TCB` `Notification`)

- **ACPI `DMAR` & Intel `VT-d` IOMMU İzolasyonu (`kernel/src/iommu/{mod,dmar,slpt,vtd,tests}.rs`):** Önyüklemede `DMAR` tabloları (`dmar_unit = 5`) ve BDF başına 4-seviyeli `SLPT` (`IOVA -> HPA`) kurulur; yetkisiz DMA girişimleri donanımda yakalanır (`vtd_rogue_faults = 1`), çıkışta/çöküşte `quiesce_slot_for_cspace` (`MSI` maske + `BME=0` + `SLPT/IOTLB` scrub) çalışır.
- **PCI `MSI`/`MSI-X`, `>4 GiB` `BAR` & `PAT4` `WC` (`kernel/src/irq/msi.rs`, `pci_bar.rs`, `pci_window.rs`, `vspace/pte.rs`):** Vektör `0x40..=0x5F` aralığında APIC `MSI/MSI-X` yönlendirmesi; 64-bit `>4 GiB` (`0x7000000000`+) `BAR` adres penceresi ve piksel yüzeyleri için `PAT4` `Write-Combining` (`WC`) / `UC` eşlemesi (`wc_64k_cyc = 27,060`).
- **`TCB`-Bağlı `Notification` & `2A-4a` Kalan 6 `P1` Kapanışı (`kernel/src/ipc/bound_notif.rs`):** `Syscall ABI v0.1` (`0..=31`, `248 B`) korunarak birleşik `IPC + IRQ` beklemesi (`msi_p50 = 22.95 us`) sağlandı ve `AUD-K6/K7/K8`, `AUD-U3/U4/U8` kapatıldı (`dry-hw-test.elf`).

### 2.11. `Track B` & `DRY Glass 2.0`: Saf `#![no_std]` TrueType AA, Ortak `ui/` Kiti & 6 Yeniden Tasarlanmış Uygulama (`--p9-only` / `--p10-only`)

- **Tipografi (`userspace/src/ttf/*`, `assets/fonts/`):** Dış sandık veya heap olmadan çalışan SFNT ayrıştırıcısı, `24.8` sabit-nokta 256-seviyeli analitik gri-ton rasterleştirici ve `96 KiB` `.bss` (`512` yuva) glif önbelleği (`DrySans`/`DryMono`, SIL OFL 1.1, Türkçe dahil Latin Extended-A + `100%/125%/150%` `scale_px`). Eski `glyph_8x16` (`gfx.rs`) yalnızca tanılama fallback'i olarak tutulur.
- **Ortak UI Kiti (`userspace/src/ui/*`) & Compositor (`userspace/src/glass/*`, `compositor.rs`):** `canvas.rs` SDF AA ilkelleri, `theme.rs` (`Obsidian Dark`/`Frosted Daylight`/`High Contrast`), `widgets.rs`, `text_edit.rs`, `containers.rs` (`sidebar_nav`, `tab_bar`, `metric_card`, `list_row`, vb.), prosedürel duvar kağıdı, çift-geçişli kutu bulanıklığı (`BLUR_R = 4`), topbar, `Dock` ve cam paneller (`Launcher`, `Quick Settings`, `Notification Center`, `Lock Shield`).
- **Çoklu Çözünürlük `VirtIO-GPU` & 6 Uygulama (`24/32` modül):** `MAX_BACKBUF_2M = 8` ve `MAX_GPU_SCANOUT_2M = 5` ile `1024x768`, `1280x800` ve `1920x1080` desteklenir; `PixelAppWindow` + `ui/` üzerindeki `6` uygulama (`Terminal`, `Life Station`, `Files`, `Settings`, `Editor`, `Calculator`), `DRY Glass 2.1` adaptif pencere şeridi + `Alt+W` genel bakış (`userspace/src/glass/{window_strip,overview}.rs`, `docs/DRY_GLASS_2_1_REPORT.md`, `9` PNG `--p11-only`) ve `DRY Glass 2.0` (`--p10-only`, `12` PNG) ile tam `105/105` regresyonda (`leaked_frames = 0`) kanıtlanmıştır (`ADR-021`, `ADR-022`).

### 2.12. `Project Ascension 4.0` (`Track C/D/E`): Kaynak-Düzeyi `libdry-lx-compat`, `bochs.c` DRM Adası & Live USB

- **Çoklu-GPU PCI Keşfi (`kernel/src/{pci,pci_cfg,iommu/vtd,irq/session}.rs`):** `PCI_DEV_COUNT = 7` (`PCI_OBJ_DISPLAY_AUX = 6` `"display-aux"`) birincil UEFI GOP BAR0 adresini atlayarak ikincil ekran denetleyicisini ayırır ve `BootPciFuncRecord` (`boot_info + 0x0800`) dizisini süpervizöre aktarır (`0` yeni syscall, `rust_syscall_dispatch = 248 B`).
- **Kaynak-Düzeyi `libdry-lx-compat` & 2. Linux `v6.18` Adası (`drivers/{libdry-lx-compat,lx-drm-bochs-island}/`, `tools/lx_compat/`):** `.ko`/`ET_REL` ikilileri reddedilir; değiştirilmemiş `v6.18` `bochs.c` (`829` satır, `sha256=93f5c4bc...`) `dry-lx-drm-bochs.elf` (`0x0212`) içinde `SYS_DEV_MAP` BAR0/BAR2, `VSPACE_MAP_OP_2M_RO` PRIME `FrameCap`, `IoRing` (`OP_DRM_FLUSH = 18`) ve `#UD` (`vec=6`, `BME=0`) `gen=2` kurtarması ile `800x600` tarama üretir (`--p12-only`, `docs/LINUX_COMPAT_RUNTIME_REPORT.md`).
- **Hedef Fiziksel GPU & Bootable Live USB:** `validate_target_gpus.py` (`8086:9a49` + `10de:25a0` `os-interface.h` `-Werror` shim) ve `build_live_usb_image.py` (`96 MiB` FAT32, `qemu-xhci + usb-storage` önyükleme doğrulamalı, `24` modül) eklendi.

### 2.13. `Project Ascension 5.0` (`v0.5.0`): `27` Ring-3 Kompartmanı, Bare-Metal Sertleştirme, `INV-DISK-SAFE-01/02` & `160 MiB` GPT Live USB

- **`27/32` Ring-3 Kompartmanı (`userspace/` & `drivers/`):** Önceki `24` modüle üç izole `CPL=3` modül eklendi: `dry-usb.elf` (`SecDomain::RootSystem`, `0x000a` — xHCI USB Boot HID + BOT MSC), `dry-nvme.elf` (`SecDomain::RootSystem` — salt-okunur NVMe 1.4 envanteri) ve `dry-lx-net.elf` (`SecDomain::RootSystem`, `0x0214` — `GPL-2.0-only` `v6.18` `8139cp.c + mii.c` adası). `BootInfo` (`0x0700` `RootInitBootExtInfo` + `0x0800` `BootPciFuncRecord`) genişletildi (`PCI_DEV_COUNT = 11`: `DISPLAY_AUX=6`, `XHCI=7`, `NVME=8`, `NET=9`, `IGPU=10`).
- **Bare-Metal Çekirdek Sertleştirmesi (`kernel/src/` — `0` Yeni Syscall, `248 B` Sıcak Çerçeve):**
  - **`32 GiB` `PMM` Ölçeklenmesi (`pmm/meta.rs`):** `max_ram_end` tabanlı `FrameMeta` (`16 B/sayfa`) `BootstrapAllocator` tahsisi ve HHDM doğrulaması `32 GiB` sistem belleğine (`15IHU6` `31.12 GiB` kullanılabilir) genişletildi (`--smp-only 2 --mem 4G` `PASS`: `8.361s`, `usable_mib=4020`, `max_pfn=1572864`, `usable_frames=1029293`, `meta_bytes=25165824`, `leaked_frames=0`).
  - **UART-sız Önyükleme, `DIAG_RING` (`dmesg`) & GOP Panik Konsolu (`serial.rs`, `panic_fb.rs`, `syscall/cold.rs`, `build_live_usb_image.py`):** `live_usb_limine.conf` `serial: yes` satırını çıkarır; `probe_com1_uart()` `COM1+7` (`0x5a/0xa5`) ve `LSR != 0xff` denetleyerek `8250.nr_uarts=0` dizüstülerde bekleme/kilitlenmeyi önler. Tüm çekirdek çıktıları `16 KiB` `DIAG_RING` tamponuna yazılır (`SYS_SYSINFO` `copy_diag_ring_to_user` -> `dmesg` ve `/persist/boot.log`), ölümcül panikler ise [`panic_fb.rs`](kernel/src/panic_fb.rs) üzerinden doğrudan UEFI GOP'a (`hhdm_virt` veya `KERNEL_GOP_DIAG_BASE`) çizilir. `live_usb=1` hızlı deterministik önyükleme yolunu (`[DRY-LIVEBOOT]`) açar.
  - **`< 4 KiB` Alt-Sayfa PCI MMIO BAR (`pci_bar.rs`, `pci_window.rs`, `abi_ext.rs`):** `UserPciBarInfo` (`16 B`) `page_suboff: u16` taşır (`base_phys & 0xfff` MMIO için); `sys_dev_map_window` `eff_bar_limit = bar.size_bytes.max(PAGE_SIZE)` ve `bar_page_base = bar.base_phys & !(PAGE_SIZE - 1)` kullanarak `rtl8139` `256 B` `BAR1` gibi `< 4 KiB` pencereleri güvenle eşler.
  - **Çoklu-DRHD Aktif-GOP `VT-d` Ertelemesi, `RMRR` Ön-Eşleme & `preserve_active_gop` (`iommu/{slpt,vtd,tests}.rs`, `pci_cfg.rs`):** `00:02.0` (`PCI_OBJ_IGPU`) üzerinde `preserve_active_gop` BAR yazma/boyutlandırma yoklamasını ve `SYS_DEV_MAP`'i `-EPERM` ile kapatır. `init_single_unit` yalnızca `dmar.drhd_count > 1 && is_dedicated_gop_drhd(src, gop_bdf)` koşulunda adanmış grafik `DRHD`'sini erteleyip kalan `DRHD`'lerde (`TE=1` öncesi `premap_unit_rmrrs` `BOOT_RMRR_DID` ile) `39-bit` 3-seviyeli veya `48-bit` 4-seviyeli `SLPT` zorlar (`0` aktif birim kalırsa `VtdStatus::FailedClosed`).
- **Depolama Güvenlik Değişmezleri (`INV-DISK-SAFE-01` & `INV-DISK-SAFE-02`):**
  - **`INV-DISK-SAFE-01` (`userspace/src/nvme_ro.rs`):** `dry-nvme` kaynak kodunda yalnızca Admin `IDENTIFY (0x06)`, `CREATE_IO_CQ (0x05)`, `CREATE_IO_SQ (0x01)` ve NVM `READ (0x02)` opkodları bulunur (`0` yazma/silme/biçimlendirme opkodu). Intel VMD (`8086:9a0b`) altındaki konak NVMe aygıtları `VMD_DOMAIN_GATED` olarak salt-okunur dokunulmadan bırakılır.
  - **`INV-DISK-SAFE-02` (`userspace/src/usb/msc.rs`, `bin/dry_vfs.rs`):** `dry-usb` her `IORING_OP_WRITE_SECTORS` isteğinde `rel_sector + count <= part2_sectors`, `dryfs_verified == true` (`*b"DRYFS_V1"`) ve `read_only_policy == false` (`storage=ro` -> `-EROFS`) şartlarını denetler; LBA `0..33` GPT ve Bölüm 1 `DRYOS_ESP` asla değiştirilmez (`--p13-only`, `25` kontrol).
- **Modüler `libdry-lx-compat` v2, 15-Adımlı Yaşam Döngüsü & `118`-SVG Vektör Kimliği:** `drivers/libdry-lx-compat/src/` 6 modüler C dosyasına (`2,164` LOC) bölündü; `broker_lifecycle.rs` 15 adımlı durum makinesini (`DETECTED..ROLLED_BACK`) ve `native-driver-island` / `lx-compat-island` / `linux-microvm` sözleşmelerini zorlar (`dry-lx-net.elf` `8139cp.c+mii.c`). `tools/build_vector_assets.py` `118` özgün SVGsini (`Iso-Kernel Prism`) `#![no_std]` `vector_assets_{sys,apps}.rs` tablolarına derler ve `DRY Glass 2.1` `100%..200%` DPI destekler (`--p15-only`, `15` PNG); `build_live_usb_image.py` `160 MiB` 2-bölümlü GPT imajında `5` Limine profili üretir.

### 2.14. `Project Ascension 6.0` (`ascension-6.0`, `139/139` QEMU + `5/5` Matris): Evrensel GPU Mimarisi & Fail-Closed IOMMU

- **Fail-Closed `IommuOps` & ACPI `IVRS` (`kernel/src/iommu/{ops,ivrs,vtd,dmar,mod}.rs`):** Donanım IOMMU yokken veya `live_usb=1` iken çekirdek `UnsupportedFailClosed`/`FailedClosed` durumuna geçer, untrusted DMA tahsisini ve PCI `BME=1` geçişini `-EPERM` ile reddeder (`--p16-only` `p16a`: `no_iommu_rc=35`, `code=106`). `QemuTestBypass` yalnızca `CPUID.01H:ECX[31] && dry_qemu_test_no_iommu=1 && !live_usb` üçlüsüyle etkinleşir. `IVRS` ayrıştırıcısı QEMU `amd-iommu` üzerinde doğrulanmıştır ve dürüstçe `IommuBackendKind::AmdVi` `FailClosed` bildirir.
- **ACPI `MCFG`/`ECAM`, 64-Yuvalı PCI, SG DMA & 2 MiB `WC`/`UC` MMIO (`pci_{ecam,slots}.rs`, `dma.rs`, `SYS_DMA_CTL`):** `64` yuvalı dinamik PCI envanteri, `DEV_MAP_OP_CFG_RW`/`DEV_MAP_OP_ENUM_MINT`, `IOVA != HPA` (`0xC000_0000`) scatter-gather DMA, `2 MiB` `WC`/`UC` MMIO (`PAT4`) ve `SurfaceBufferSet` eklendi (`0` yeni sıcak yol yükü; `rust_syscall_dispatch = 248 B`).
- **Satıcı-Bağımsız Ring-3 `dry-gpu-svc` & GOP-to-KMS FSM (`userspace/src/gpu_svc/*`):** `UefiGop`, `VirtioGpuNative`, `LxCompatDrm`, `LinuxMicroVm` (`4` arka uç), `DisplayHandoverMachine` (`0x7f`), sıralı fence halkası, `SyntheticGpuDriver` (`synth=2`, `rollbacks=3`) ve `broker_data_path=0` ile `dry-display`/`wm` ayrımı uygulandı.
- **Ayrıştırılmış `libdry-lx-compat` (`2,207` C LOC), `dry-fw-store`, MicroVM IPC & Eşzamanlılık:** `bochs` özel mantığı `bochs_island_glue.c` (`31` satır) içine taşındı (`0` `bochs` eşleşmesi); zayıf `CFG_RW`/`SYS_DMA_CTL` kancaları bağlandı; `nv_os_shim_check.c` dürüst `NV_ERR_NOT_SUPPORTED` (`4/4/9/5` sınıflandırma) kullanır; `fw_store.rs` SHA-256 doğrulama sağlar; `proto_bench.py` (`SHARED_MEMORY_IPC_COPIED`, `zero_copy_dmabuf=False`), `VIRTIO_GPU_F_VIRGL` capset yoklaması (`HOST_VIRGL_UNAVAILABLE`), `StaticRunQueue::push` Vyukov tur-çakışma düzeltmesi (`20cpu_2g`) ve `dry-usb` IRQ-bekleme/zaman-aşımı hizalaması (`--p13-only` `syscalls`: `6.48M -> 94.9K`) tamamlandı.

---

## 3. Zamanlayıcı Mimarisi (`H1` Özeti)

- **Durum Temsili:** Her `TCB` içinde tek bir `sched: AtomicU32` kelimesi iki ortogonal alanı paketler: `exec_state` (`READY`, `RUNNING`, `SW_PREEMPT`, `SW_PARK`, `BLOCKED`, `WAKING`, `EXITING`, `DEAD`) ve `park_token` (`NONE`, `ARMED`, `NOTIFIED`) `[DOĞRULANDI — Faz 0 TLC (84.351 durum) + Rust Loom (39.135 permütasyon) + 20-CPU stres testi]`.
- **Temel Güvenlik Kuralı:** Hiçbir görev `switch_to()` assembly geçişi bitip yeni görevin stack'i üzerinde `post_switch(prev)` çalışmadan `EXEC_READY` olamaz ve runqueue'ya yayınlanamaz (`ADR-004`).
- **Aşamalı Karmaşıklık:** Faz 1'de CPU başına intrüzif öncelik kuyruğu + Priority Inheritance kullanılır; topoloji-farkındalıklı iş çalma (*work stealing*) ve `NO_HZ_FULL` Faz 3/4'te ölçümle eklenir.

---

## 4. Güvenlik Etiketleri, Alan İzolasyonu ve `DRY Vault` Mimarisi (`ADR-007`)

- **Üç Güvenlik Alanı ve Mutlak Çekirdek Duvarı:** `SecDomain::Kernel (0)`, `SecDomain::RootSystem (1)` ve `SecDomain::Userspace (2)` alanları kesin olarak ayrılır. `USERSPACE` süreçleri yalnızca yetkili oldukları `USERSPACE` nesnelerine yazabilir; sistem kütüphaneleri kontrollü `READ_ONLY_EXPORT` (`W^X`) ile salt-okunur sunulur (`INV-SEC-02`, `INV-SEC-03`). `ROOT_SYSTEM` yetkisi dahil hiçbir Ring 3 bileşeni `KERNEL` belleğine doğrudan erişemez (`INV-SEC-01`).
- **Nesne Etiketleme ve Path-Bağımsız Denetim:** Her çekirdek nesnesi (`TCB`, `CSpace`, `VSpace`, `Frame`/`DmaArena`, `Endpoint`, `Notification`, `PciCap`) ve VFS inode'u 64-bit `SecLabel { domain, flags, compartment_id, policy_tag }` taşır. Erişim kararları path veya `cwd`'ye değil, **nesne `SecLabel` + süreç capability seti** kesişimine dayanır. Yeni nesneler/dosyalar varsayılan olarak ebeveyn etiketini miras alır; `rename`, `symlink`, `hardlink` ve `mount` işlemleri yetki yükseltemez (`INV-SEC-05`).
- **`DRY Vault`, `Policy Broker` ve Developer Workspace:** Kimlik doğrulama (`DRY Vault`), politika değerlendirmesi (`Policy Broker`) ve dosya sistemi (`VFS`) userspace servisleridir. `DRY Vault`, `ROOT_SYSTEM` içinde kapsamla sınırlandırılmış (`compartment_id` + `DEV_WORKSPACE`) ve süreli (`lease_epoch`) yetkiler üreterek sürekli izin sorma ihtiyacını ortadan kaldırır; ancak deny-by-default ilkesi ve güvenlik denetimleri hiçbir zaman kapatılamaz (`INV-SEC-04`, `INV-SEC-07`, `INV-SEC-08`).
- **Etkinleşme Yaşam Döngüsü:** Userspace servis, paket ve politika güncellemeleri atomik nesil değişimiyle anında canlı etkinleşir; çekirdek güncellemeleri ise A/B Limine önyükleme yuvasına yazılıp bir sonraki boot'ta etkinleşir ve kullanıcıya reboot önerilir.

---

## 5. Linux Sürücü Adası ve Ring 3 Uyumluluk Katmanı Mimarisi (`H2` & `ADR-016`)

- **Linux API Uyumluluk Katmanının Kesin Konumu (`%100 Ring 3`, `0` Çekirdek Satırı — `INV-ISL-05`):**
  - **`dry-kernel` (`Ring 0`) Linux ABI, POSIX veya `LinuxKPI` sembolleriyle asla kirletilmez.**
  - **Sürücü Uyumluluğu (`Kademe 2`):** Daraltılmış `libdry_lx_<sınıf>` (`libdry_lx_net`, `libdry_lx_usb`; `< 5.000` LOC C çalışma zamanı) doğrudan ilgili izole **`GPL-2.0-only` Ring 3 Driver Island sürecinin içine** bağlanır; monolitik paylaşımlı `LinuxKPI` yasaktır (`ADR-006`).
  - **Uygulama Uyumluluğu (`Kademe 3A`):** Linux ELF64 sistem çağrısı çevirisi tamamen Ring 3 **`dry-linux-abi`** kişilik sunucusu ve salt-okunur (`SEAL_RO_EXPORT`) **`libdry_posix`** kütüphanesinde tutulur; çekirdek yalnızca jenerik `FaultEndpoint`, `XSAVEOPT` ve `FS_BASE`/`GS_BASE` ilkellerini sunar.
- **`Mock Driver Island` (`Milestone 2C-0`) ile Gerçek `Linux Driver Island` (`Milestone 2C-1` / `Faz 3B`) Arasındaki Kesin Fark:**
  - **`Mock Driver Island` (`2C-0` — `DONE`):** Saf `no_std` Rust (`dry-mock-drv.elf`), `0` Linux C kodu, `0` `PciDev` BAR MMIO, `0` DMA ve `0` donanım kesmesi içerir; yalnızca `Device Broker` kontrol düzlemini, doğrudan `Endpoint` + `IoRing` veri düzlemini, `HLT` uyku/uyanmayı ve `#UD` sonrası tam nesil `revoke`/kurtarma mekanizmasını kanıtlar.
  - **Gerçek `Linux Driver Island` (`2C-1` / `Faz 3B` — `PLANNED`):** Değiştirilmemiş upstream Linux C sürücü bileşeni (`H2`: örn. `e1000e` `200` sembol veya `igc` `247` sembol, `GPL-2.0-only`) + aynı Ring 3 kompartmanı içinde `libdry_lx_net` C çalışma zamanı + `CapObjectKind::PciDev` (`SYS_DEV_MAP` `USER_MMIO_UC` BAR) + sabitlenmiş `DmaArena` (`SYS_DMA_ALLOC`, gölge `vmemmap` `struct page[]`, çöküşte `PCI Command.BME = 0` + PCIe `FLR`) + `IrqCap`/MSI-X -> `Notification` + `TCB`-bağlı olay döngüsü + `IoRing` paket halkalarından oluşur (`INV-ISL-06`).
- **Uygulama ve GPU Sınırı:**
  - *Linux ELF Uygulamaları (`Faz 3A`):* Ring 3 `dry-linux-abi` + `FaultEndpoint`/`SYSCALL` yönlendirmesi; *Windows PE/NT (`Faz 5`):* Ring 3 `dry-ntos` + Wine `ntdll` + `DXVK` (`0` çekirdek `.sys` sürücüsü / anti-cheat).
  - *GPU (`ADR-016` Kademe 1 & 4):* Birincil ekran taraması yerel `dry-display` (`GOP`/`VirtIO-GPU`) mülkiyetindedir; `> 1.000` sembollü / `HMM` ayrık GPU'lar (`RTX 3050 Ti` `01:00.0`) çevrilmez, yalnızca `P5` bare-metal doğrulaması sonrasında isteğe bağlı (*opt-in*) uyuyan `dry-vmm` + toplu `Venus` halkasıyla çalıştırılır.

---

## 6. Ürün Katmanı, Kurulum Profilleri, Açılış Hattı ve Paketleme Mimarisi (`ADR-008` .. `ADR-011`, `[PLANNED]`)

> **Kapsam Notu:** `DRY Glass 2.0` (`ADR-021`, `101/101` QEMU + `5/5` Matris) itibarıyla kök süpervizör (`dry-init.elf`), `dry-input.elf`, Ring 3 hibrit masaüstü compositor/WM (`dry-display.elf`), `dry-vfs.elf`, `dry-blk.elf` (`DRYFS_v1`), `dry-vault.elf`, `dry-gpu.elf` (`VirtIO-GPU` 2D + `GOP` fallback), `dry-power.elf`, saf `#![no_std]` TrueType AA + `ui/` ortak bileşen kiti (`userspace/src/{ttf,ui,glass}/*`, `assets/fonts/` OFL-1.1) ve 6 yerel `CPL=3` uygulaması (`Terminal`, `Life Station`, `Files`, `Settings`, `Editor`, `Calculator`) çalışır durumdadır (`[DOĞRULANDI]`). Aşağıdaki `drypkg`, `DRY Server` ve `DRY Store` öğeleri ise sonraki faz hedefleridir (`PLANNED`). Ayrıntılı yol haritası `docs/PRODUCT_ROADMAP.md`, masaüstü raporu ise `docs/DRY_GLASS_2_0_REPORT.md` içindedir.

- **Tek Çekirdek, İki Kurulum Profili (`ADR-008` — *One Microkernel, Multiple Experiences*):**
  - Aynı `dry-kernel` ikilisi, aynı `Endpoint`/`Notification`/`IoRing` IPC katmanı, aynı `ADR-007` güvenlik alanları ve aynı `drypkg` altyapısı üzerinde iki ayrı profil kurulur:
    1. **`DRY Desktop` (`[PLANNED]`):** Ring 3 `dry-compositor` + `DRY Shell`, ekran/girdi/ses servisleri, `DRY Store`, etkileşim gecikmesi ve güç verimliliği odaklı kaynak politikaları.
    2. **`DRY Server` (`[PLANNED]`):** Varsayılan başsız (*headless*, sıfır masaüstü/compositor bağımlılığı), CLI ve uzaktan yönetim servisleri, yüksek verimli ağ/depolama yığını, CPU yakınlığı (*affinity*), NUMA yerelliği, kaynak izolasyonu ve AI inference / yüksek eşzamanlı sunucu iş yüklerine uygun politikalar.
  - Kurulumdan sonra yeniden kurulum gerekmeden atomik `drypkg` nesilleriyle profil dönüşümü veya karşı profil bileşenlerinin eklenmesi desteklenir.
- **Compositor, `DRY Shell` ve Ortak `Action API` Sınırı (`ADR-009`, `[PLANNED]`):**
  - Pencere yönetimi (varsayılan `Floating`, tek tık `Tiling`, `Stacking`/`Tabbed`, `Tiling` düzenini bozmayan `Floating` diyaloglar), görsel efektler (`liquid-glass` üst çubuk, tema, `Normal`/`Compact UI`, düşük donanım *fallback*, hareket azaltma) ve fare/klavye/menü/komut paletini birleştiren **`Action API`** tamamen Ring 3 kullanıcı alanında çalışır; `dry-kernel` içine hiçbir pencere, yazıtipi veya efekt mantığı eklenmez.
- **Tek Olay Akışlı Çift Açılış (`Clean` / `Diagnostic`) ve Kimlik Sınırı (`ADR-010`, `[PLANNED]`):**
  - `dry-init` ve sistem servislerinin ürettiği yapılandırılmış önyükleme olay akışı iki Ring 3 görünüm kipini besler: varsayılan sade **`Clean Boot`** ve canlı tanılama/kurtarma panelli **`Diagnostic Boot`** (önyükleme sırasında `Esc` ile anlık geçiş; sahte yüzde göstergesi yasaktır).
  - Oturum doğrulayan **`Login / Authentication Manager`** (`SecDomain::RootSystem` + `DRY Vault` kiralama) ile kullanıcının kişisel uygulama parolalarını saklayan **`Password Manager`** (`SecDomain::Userspace` şifreli kompartman) kesin olarak ayrı güvenlik bileşenleridir.
- **`drypkg`, `DRY Store` ve İsteğe Bağlı Uyumluluk Katmanları (`ADR-011`, `[PLANNED]`):**
  - Ortak `drypkg` arka ucu CLI ve `DRY Store` arayüzlerini besler; imzalı paketleri, `ADR-007` capability izin şablonlarını ve atomik güncelleme/*rollback* nesillerini yönetir.
  - Uygulama desteği **(1) Native DRY -> (2) Linux ELF/ABI -> (3) Seçilmiş Windows PE/Win32** öncelik sırasıyla ilerler; yabancı çalışma zamanları yalnızca ihtiyaç duyulduğunda kurulur ve uyumluluk sınırları kullanıcıya şeffaf biçimde gösterilir.
- **Performans İddiası Sınırı:** KVM içindeki sentetik mikro-ölçütler (`null_sys = 231 cyc`, `call_reply = 1.952 cyc`) yalnızca çekirdek sıcak yol disiplininin kanıtıdır; gerçek fiziksel donanımda (`bare-metal`) Linux ile uçtan uca A/B karşılaştırması tamamlanana dek (`P5`) "Linux'tan hızlı" iddiasında bulunulmaz (`[TEST EDİLMEDİ]`).

