# DRY OS & DRY Kernel — Sistem Mimarisi

> **Durum Tarihi:** 12 Kasım 2026 (`Milestone 2A-2` / `DRY Desktop Preview` Kapanışı)  
> **Aşama:** Faz 1 (`1df3842`, `69522a3`) + Faz 2 `Milestone 2A-1` (`P1 First Interaction`) + **`Milestone 2A-2` (`DRY Desktop Preview`)** Tamamlandı (`79/79` QEMU/KVM Doğrulaması: Gerçek PS/2 Klavye [`IRQ1`] + PS/2 Fare [`IRQ12`] -> Birleşik Ring 3 `dry-input` -> Ring 3 Çok-Pencereli Compositor [`dry-display` + Yazılımsal İmleç + `Floating`/`SideBySideTiled` WM] -> Eşzamanlı İzole `dry-shell` & `dry-sysmon` Yeşil; `ADR-001` .. `ADR-013` Yürürlükte)  
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
  - **Veri Düzlemi:** `sDDF` / `io_uring` tarzı paylaşımlı bellek kilitsiz halkalar (`IoRing`) + yalnızca karşı taraf uyuyorsa çekirdeğe giren `Notification` kapı zili (*doorbell*) `[DOĞRULANDI: LionsOS/sDDF literatür kanıtı; DRY uyarlaması TEST EDİLMEDİ]`.
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

## 5. Linux Sürücü Adası ve Uyumluluk Mimarisi (`H2` Özeti)

- **İzolasyon Birimi (*Driver Island*):** Upstream C sürücüsü (`GPL-2.0-only`) + sürücü sınıfına özel daraltılmış `libdry_lx_<sınıf>` çalışma zamanı aynı userspace sürecinde derlenir; dış dünyayla yalnızca DRY `IoRing` (CPU paylaşımlı bellek) üzerinden konuşur `[DOĞRULANDI — Faz 0 H2: 18.528 satır igc %0 değişiklik ve 3.468 satır runtime ile derlendi/test edildi]`.
- **Gölge `vmemmap` ve Kalıcı IOVA Arena:** `igc.ko` ikili analizinde `[DOĞRULANDI]` görülen `vmemmap_base` ve `page_offset_base` satır-içi pointer aritmetiğini karşılamak ve paket başına IOTLB shootdown maliyetinden kaçınmak için 64-bit userspace adres alanında sabit bir `DmaArena` ve ona doğrusal indekslenen gölge `struct page[]` dizisi ayrılır `[FONKSİYONEL PoC DOĞRULANDI; fiziksel silikon IOMMU güvenliği TEST EDİLMEDİ]`.
- **Uygulama Uyumluluğu Ayrımı:**
  - *Linux ELF Uygulamaları:* Userspace personality sunucusu + `FaultEndpoint`/`SYSCALL` yönlendirme trampolini (çekirdek ABI'sine POSIX eklenmez).
  - *Windows PE/NT (Faz 5):* Userspace `ntdll` -> `dry-ntos` sunucusu + Wine DLL + `DXVK` (çekirdek modu anti-cheat kapsam dışıdır).
  - *GPU:* Faz 2–3'te yalnızca UEFI `GOP` ve `VirtIO-GPU`; `amdgpu` (~1.107 dış sembol, ~30.5 MiB ELF `[DOĞRULANDI]`) ve `nvidia-open` (`nvidia-uvm` `HMM`/`mmu_notifier` bağımlılığı `[DOĞRULANDI]`) çeviri kapsamı dışındadır.

---

## 6. Ürün Katmanı, Kurulum Profilleri, Açılış Hattı ve Paketleme Mimarisi (`ADR-008` .. `ADR-011`, `[PLANNED]`)

> **Kapsam Notu:** Aşağıdaki ürün ve kullanıcı alanı bileşenleri mimari olarak kesinleştirilmiş tasarım hedefleridir (`PLANNED`); `Milestone 2A-3` (`Functional Desktop Foundation, Native Applications & Power Lifecycle`) itibarıyla kök süpervizör (`dry-init.elf`), örnek IPC servisi (`dry-echo.elf`), birleşik PS/2 klavye+IntelliMouse tekerlekli fare sürücüsü (`dry-input.elf`, `UsQwerty`+`TurkishQ`), Ring 3 hibrit masaüstü compositor/WM (`dry-display.elf`: `Floating`/`SideBySideTiled`/`Stacked`/`Tabbed`, `DialogTransient`, `DPMS` ve `Session Lock`), uçucu `/ram` + salt-okunur `/boot` dosya sistemi (`dry-vfs.elf`), ACPI S5/Reset güç servisi (`dry-power.elf`) ve dört izole yerel `Userspace` uygulaması (`dry-shell`, `dry-sysmon` / `Life Station`, `dry-files`, `dry-settings`) uygulanıp `85/85` QEMU testi ve `5/5` SMP/RAM matrisiyle doğrulanmıştır (`[DOĞRULANDI]`). Ayrıntılı yol haritası `docs/PRODUCT_ROADMAP.md`, masaüstü/kabuk spesifikasyonu ise `docs/DRY_DESKTOP_SPEC.md` içindedir.

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

