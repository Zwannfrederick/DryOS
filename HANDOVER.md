# DRY OS — Kapsamlı Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

- **Tarih & Son Güncelleme:** **12 Kasım 2026** (`Milestone 2A-1` / `P1 First Interaction` Kapanışı)
- **Mevcut Durum (`P0 — DONE`, `P1 — IN PROGRESS [Milestone 2A-1: DONE]`):** Faz 0 (`H1` & `H2`), Faz 1 (`1A..1C`: `1df3842`), `Optimization Pass v1` (`69522a3`) ve **Faz 2 `Milestone 2A-1` (`P1 First Interaction`)** `%100` tamamlandı; `0` Clippy uyarısı ve **`65/65` otomatik QEMU/KVM senaryosu** (`56/56` eski regresyon + `9/9` yeni P1 gerçek QMP klavye E2E, canlı `screendump` piksel farkı ve negatif güvenlik testi; `leaked_frames = 0`, `stale_tlb_reads = 0`, `uaf_errors = 0`) ile doğrulandı.
- **Bu Belgenin Amacı:** İzleyen oturumda görevi devralacak mühendis veya coding agent'ın sıfır bağlam kaybıyla, mevcut güvenlik/yığın/eşzamanlılık değişmezlerini kırmadan doğrudan **`Milestone 2A-2`** (Fare/`VirtIO-Input`/USB HID, Türkçe Q klavye haritası, `IoRing` sıfır-kopya veri düzlemi, `FaultEndpoint` ve `XSAVEOPT`) aşamasına başlamasını sağlamak.

---

## 1. Git Commit Zinciri ve Kilometre Taşı Özeti

| Aşama | Referans / Kapsam | Doğrulama |
| :--- | :--- | :---: |
| `ADR-007` (`a443e6c`) | `KERNEL (0)` / `ROOT_SYSTEM (1)` / `USERSPACE (2)` izolasyonu, `SecLabel` (8 B), `DRY Vault` kiralama | Öz-test |
| `1A-B..1A-G` (`bc46907..fad423b`) | Limine UEFI SMP (`1..20 vCPU`), per-CPU `GDT`/`TSS`/`IST1`, preemptive `H1`, `x2APIC`/`xAPIC` kalibrasyonu | `21/21` QEMU |
| `1B-A..1B-D` (`78ac64b..29002d3`) | `PMM` (`16 B` `FrameMeta`), 4-seviye `VSpace` (`W^X`), SMP TLB shootdown, `CSpace` CDT `revoke`, `CPL=3` ELF64 + `SMAP` | `50/50` QEMU |
| `1C` (`1df3842`) | `Endpoint`/`Notification` IPC, 4-sıçrama Öncelik Bağışı, `dry-init`/`dry-echo` ve ilk Ring 3 GOP ekranı | `56/56` QEMU |
| `Opt v1` (`69522a3`) | `null_sys 231 cyc`, birleşik `ep_reply_recv` (`call_reply 1.952 cyc` [`12.8x`]), `248 B` sıcak syscall çerçevesi | `56/56` QEMU |
| `Docs` (`cdfb567`) | Ürün vizyonu, `ADR-008..011`, `PRODUCT_ROADMAP.md`, `DRY_DESKTOP_SPEC.md` başlangıç devir teslimi | Doküman |
| **`2A-1 / P1`** *(12 Kasım 2026)* | **ACPI MADT IOAPIC, `IrqCap`/`IoPortCap` (`IOPL=0`), `dry-input`, `dry-shell`, dinamik GOP compositor + taşınabilir pencere** | **`65/65` QEMU + PNG** |

> **Önemli Çalışma Ağacı Notu:** Faz 0'dan kalma `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` dosyalarındaki önceden mevcut yerel değişiklikler kasıtlı olarak korunmuş ve commit dışında bırakılmıştır; **bu iki dosyayı `git add` yapma, silme veya geri alma (`git checkout`).**

---

## 2. Depo ve Alt-Sistem Haritası (Tüm Dosyalar `< 1.000` Satır Tavanı Altında)

### 2.1. Çekirdek (`kernel/src/`)
- **Önyükleme, ACPI, Donanım ve Per-CPU:** `main.rs`, `limine.rs` (Base Rev 3 HHDM/SMP/Modül/GOP/**RSDP**), `acpi.rs` (geçici `PML4[384]` `RO+NX` 3-sayfa penceresiyle `RSDP -> XSDT/RSDT -> MADT` `IOAPIC` & `ISO` keşfi), `arch.rs`, `gdt_tss.rs`, `percpu.rs` (`256 B` `PerCpuData`), `idt.rs` (`0x30..=0x3F` harici IRQ vektörleri dahil 256-girişli `TrapFrame`), `apic.rs` (`x2APIC`/`xAPIC`, `uptime_telemetry`), `smp.rs`, `serial.rs`, `security.rs` (`ADR-007`), `uaccess.rs` (`CR4.SMAP = 1` + `extable`), `display.rs` (`CapObjectKind::Framebuffer`).
- **Donanım Kesme ve Port G/Ç (`kernel/src/irq/` — YENİ):** `mod.rs` (`IOAPIC` `UC+NX` eşleme, `IoApicHwCache`, `IrqSlotState`, `handle_external_irq`, `sys_irq_bind`, `sys_irq_ack`, fırtına kelepçesi), `ioport.rs` (`RFLAGS.IOPL == 0` altında beyaz/kara listeli `sys_ioport_rw`), `bench.rs`, `tests.rs` (öz-testler, 4 negatif `irq_fault=` testi ve `interactive=1` oturum başlatıcısı).
- **Zamanlayıcı, Bellek, Yetki, Süreç, Syscall ve IPC:** `sched/{state,runqueue,context,preempt,donate,mod,tests,preempt_tests}.rs`; `pmm/{meta,mod,huge_reclaim,untyped,tests}.rs`; `vspace/{pte,mod,huge,tlb,tlb_debt,kernel_handover,tests}.rs`; `cspace/{cap,mod,revoke,objects,tests,bench,fault_tests}.rs` (`CapObjectKind::IrqLine = 11`, `IoPort = 12`); `proc/{elf,mod,supervisor,tests,bench}.rs`; `syscall/{entry,mod,cold}.rs` (`rust_syscall_dispatch = 248 B`, `sys_cold_dispatch = 216 B`, çağrılar `0..=25`); `ipc/{mod,endpoint_state,endpoint,notif,tests,bench}.rs`.

### 2.2. Kullanıcı Alanı (`userspace/`) ve Test Araçları (`tools/`)
- **Kütüphane Modülleri (`userspace/src/`):** `lib.rs` (Syscall `0..=25`, `UserSysInfo`, `RootInitBootInfo`, IPC sabitleri), `input.rs` (`InputEvent`, `MouseEvent`, `InputRingQueue<64>`), `keymap.rs` (`KeymapLayout::UsQwerty` + `TurkishQ` genişletme arayüzü), `ps2.rs` (`i8042` kurulum + Set-1/`0xE0` çözücü), `gfx.rs` (`Rect`, `DirtyRegion`, 95-glif 8x16 ASCII font), `surface.rs` (`SharedTerminalSurface` `3.904 B <= Frame4K`), `shell.rs` (`InputEditor`, `parse_command`, 64-satır `ShellEngine`, `project_to_surface`).
- **Ring 3 Servis İkilileri (`userspace/src/bin/`):** `user_hello.rs`, `user_peer.rs`, `dry_init.rs` (Milestone 1C + P1 oturum ve çökme süpervizörü), `dry_echo.rs`, `dry_input.rs` (`RootSystem` `0x0003` PS/2 `IRQ1` sürücüsü), `dry_shell.rs` (`Userspace` `0x0102` etkileşimli kabuk), `dry_display.rs` (`RootSystem` `0x0004` `Order2MiB` çift-tamponlu GOP masaüstü compositor).
- **Otomasyon (`tools/`):** `qemu_boot_test.py` (`65` senaryo, `--p1-only`, `--smp-only`, `--build-only`), `qemu_p1_validators.py` (canlı QMP `send-key`, dinamik `screendump` piksel farkı ve `docs/p1_interactive_shell.png` üretimi), `qemu_{log,user,ipc}_validators.py`.

---

## 3. Tüm Yeni `Milestone 2A-1` (`P1`) İkili Sözleşmeleri ve ABI Referansı

### 3.1. IDT Harici Kesme Vektörleri ve `IOAPIC` / `IoApicHwCache` Sözleşmesi (`kernel/src/idt.rs`, `irq/mod.rs`)
- **IDT Haritası:**
  - `0x00..=0x1F`: Mimari x86-64 istisnaları (`#DF = 8` münhasır `IST1`; `#PF = 14` `SMAP`/`extable` kontrolü).
  - `0x20`: Yerel APIC One-Shot Preemptive Zamanlayıcı Kesmesi (`TIMER_VECTOR`).
  - **`0x30..=0x3F` (`EXT_IRQ_BASE_VEC .. EXT_IRQ_BASE_VEC + 15`):** Harici `IOAPIC` kesmeleri (`vec = 0x30 + gsi`, `MAX_EXT_GSIS = 16`). QEMU Q35/PC MADT tablosunda `ISA IRQ 1` (PS/2 klavye) -> `GSI 1` -> **`vec = 0x31`**.
  - `0xEE`: SMP TLB Shootdown IPI; `0xEF`: Zamanlayıcı Yeniden Planlama IPI; `0xFF`: Sahte (*Spurious*) APIC Vektörü.
- **`IRQF_ONESHOT` ve EOI Sözleşmesi:**
  1. Kesme anında `handle_external_irq(vec)` `IoApicHwCache` üzerinden tek `IOREGSEL + IOWIN` yazımıyla `GSI` hattını **anında maskeler** (`masked_pending_ack = true`), ardından yerel APIC `EOI` yazar.
  2. Pencere başına (`STORM_WINDOW_TSC = 64.000.000`, `~20 ms`) `>= 256` kesme gelirse hat `storm_clamped = true` ile kilitlenir.
  3. Bağlı `Notification` nesnesine bloklanmadan `notif_signal(badge)` gönderilir.
  4. Ring 3 sürücüsü cihaz FIFO'sunu boşalttıktan sonra `SYS_IRQ_ACK(irq_cap, IRQ_ACK_UNMASK=1)` çağırır; hat zaten açıksa `-114` (`EALREADY`) döner (çift-EOI/ack imkânsızdır). Sürücü maskeyi açtıktan hemen sonra `0x64` `OBF` bitini bir kez daha kontrol ederek kenar-tetiklemeli (*edge-triggered*) mandallanma yarışını kapatır.

### 3.2. `DRY Native Syscall ABI` (`0..=25` — `kernel/src/syscall/{mod,cold}.rs`)
- **Yazmaçlar:** `RAX = nr`, `RDI = a0`, `RSI = a1`, `RDX = a2`, `R10 = a3`, `R8 = a4`, `R9 = a5`; kopyasız IPC yazmaçları `R12..R15 = mr0..mr3`; dönüş `RAX` (`>= 0` veya negatif `SysError`: eklenen `-114 = EALREADY`), ikincil dönüş `RDX`.
- **Çağrı Tablosu (`0..=19` Faz 1 + `20..=25` P1):**
  - `0..=5`: `SYS_ABI_PING (0)`, `SYS_WRITE_DEBUG (1)`, `SYS_YIELD (2)`, `SYS_EXIT (3)`, `SYS_GET_TID (4)`, `SYS_CAP_QUERY (5)`.
  - `6..=14`: `SYS_EP_CREATE (6)`, `SYS_IPC_SEND (7)`, `SYS_IPC_RECV (8)`, `SYS_IPC_CALL (9)`, `SYS_IPC_REPLY (10)`, `SYS_IPC_REPLY_RECV (11)`, `SYS_NOTIFY_CREATE (12)`, `SYS_NOTIFY_SIGNAL (13)`, `SYS_NOTIFY_WAIT (14)`.
  - `15..=19`: `SYS_CAP_DERIVE (15)`, `SYS_CAP_REVOKE (16)`, `SYS_PROC_SPAWN (17)`, `SYS_PROC_WAIT (18)`, `SYS_FB_MAP (19)`.
  - **`20 = SYS_IRQ_BIND(irq_cap, notif_cap, badge, flags=0)`:** `IrqLine` (`WRITE | SIGNAL`) yetkisini `Notification` (`SIGNAL`) nesnesine bağlar ve `IOAPIC` yönlendirmesini kurar; dönüş `GSI`.
  - **`21 = SYS_IRQ_ACK(irq_cap, action)`:** `action = 1` (`UNMASK`, bekleyen kesmeyi onaylayıp maskeyi kaldırır; bekleyen kesme yoksa `-114 EALREADY`), `action = 0` (`MASK_DISABLE`, hattı güvenli maskeler).
  - **`22 = SYS_IOPORT_RW(ioport_cap, port, op, val)`:** `op = 0` (`inb(u16) -> u8`, `CapRights::READ`), `op = 1` (`outb(u16, u8) -> 0`, `CapRights::WRITE`). `port` değeri capability `[base_port, base_port + count)` içinde olmalıdır (`RFLAGS.IOPL == 0`).
  - **`23 = SYS_SYSINFO(user_out_ptr)`:** 8-bayt hizalı kullanıcı adresine 80-baytlık `UserSysInfo` (`abi_version`, `online_cpus`, `apic_mode` [`1=x2APIC`, `2=xAPIC`], `ioapic_present`, `fb_present`, `tsc_mhz`, `uptime_ms`, `elapsed_tsc`, `timer_irqs`, `total_ext_irqs`, `total_syscalls`, `total_ctx_switches`, `usable_frames_4k`, `free_frames_4k`) yazar.
  - **`24 = SYS_FRAME_ALLOC(order, flags=0)`:** Çağıran sürecin güvenlik etiketiyle `PMM`'den sıfırlanmış `Order4KiB (0)` veya `Order2MiB (1)` çerçeve ayırır ve yeni `Frame4K`/`Frame2M` `CapPtr` döner.
  - **`25 = SYS_VSPACE_MAP(frame_cap, vaddr, map_op)`:** Çağıran sürecin `VSpace`'inde `Frame4K`/`Frame2M` eşler veya çözer (`map_op`: `0 = MAP_4K_RO`, `1 = MAP_4K_RW`, `2 = UNMAP_4K`, `3 = MAP_2M_RW`, `4 = UNMAP_2M`; tümü `PTE_USER | PTE_NX`, `W^X` zorunlu).

### 3.3. Yeni Capability Türleri, `RootInitBootInfo` ve `ADR-007` Çapraz-Alan Kuralı
- **Yeni `CapObjectKind` Değerleri (`cspace/cap.rs`):**
  - `CapObjectKind::IrqLine = 11`: `obj_id = gsi`, `aux = vector (0x30 + gsi)`; yalnızca `SecDomain::RootSystem` (`Userspace`'e delege edilemez).
  - `CapObjectKind::IoPort = 12`: `obj_id = base_port`, `aux = port_count`; yalnızca beyaz listedeki cihaz portları (`0x60..=0x60`, `0x64..=0x64`, `0x2F8..=0x2F8`); yalnızca `SecDomain::RootSystem`.
- **`RootInitBootInfo` (`userspace/src/lib.rs`, `0x0000_7e00_0000_0000`, 9 x `u64` = 72 B):**
  `[0] echo_mod_cap`, `[1] display_mod_cap`, `[2] fb_cap`, `[3] input_mod_cap`, `[4] shell_mod_cap`, `[5] irq1_cap`, `[6] ioport_60_cap`, `[7] ioport_64_cap`, `[8] session_mode` (`0` = Milestone 1C regresyon akışı, `1` = P1 etkileşimli oturum).
- **`ADR-007` Çapraz-Alan IPC Uç Noktası Tasarımı (`security.rs:224`):** `SecDomain::Userspace` (`dry-shell`, `0x0102`) hiçbir `RootSystem` nesnesine `SEND | CALL` yapamaz; ancak `RootSystem` servisleri (`dry-init`, `dry-input`, `dry-display`) `Userspace` kompartman etiketli (`domain_spec(0, SHELL_COMP_ID)`) bir `Endpoint` üzerinde `RECV | REPLY` yapabilir. Bu nedenle paylaşılan RPC uç noktaları istemci kompartmanı etiketiyle oluşturulur (`0` çekirdek istisnası).

### 3.4. IPC Opcode'ları, `InputEvent`, `SharedTerminalSurface` ve Kabuk Sözleşmesi
- **IPC Etiketleri (`userspace/src/lib.rs`):**
  - Temel: `1 = PING`, `2 = PONG`, `3 = CRASH_UD2`, `4 = EXIT_CLEAN`.
  - Girdi (`dry-input` `srv_ep`): `10 = INPUT_WAIT`, `11 = INPUT_EVENT`, `12 = INPUT_POLL`, `13 = INPUT_OVERFLOW_TEST`.
  - Ekran (`dry-display` `disp_ep`): `20 = SURFACE_ATTACH` (`Frame4K` `READ|MAP` yetki devriyle), `21 = SURFACE_PRESENT`, `22 = SURFACE_MOVE` (`mr0 = dx as i64`, `mr1 = dy as i64`), `23 = SURFACE_FOCUS`.
  - Süpervizör (`dry-init` `sup_ep`): `30 = SUP_RECONNECT_INPUT` (`#UD` çöküşü sonrası `dry-input` yeniden başlatma ve taze `Endpoint` yetki devri), `31 = SUP_SHUTDOWN`.
- **`InputEvent` 4-Yazmaçlı (`R12..R15`) Kayıpsız Paketleme (`userspace/src/input.rs`):**
  - `mr0`: `[7:0] kind (1=KeyPress, 2=KeyRelease) | [15:8] device_id | [31:16] keycode | [47:32] scancode | [55:48] modifiers (Shift=1, Ctrl=2, Alt=4) | [63:56] ascii`
  - `mr1`: `seq` (monotonik olay sıra numarası)
  - `mr2`: `timestamp_tsc`
  - `mr3`: `queue_overflow_count` (`InputRingQueue<64>` taşma sayacı)
- **`SharedTerminalSurface` (`userspace/src/surface.rs`, `#[repr(C)]`, tam `3.904 B <= 4.096 B` `Frame4K`):**
  - `64 B` başlık (`magic = 0x4452_5953_5552_4631`, `version = 1`, `cols = 80`, `rows = 24`, `seq`, `cursor_col`, `cursor_row`, `cursor_visible`, `dirty_row_min`, `dirty_row_max`, `cmd_count`, `ext_irqs_snap`, `uptime_sec_snap`) + `1.920 x 2 B = 3.840 B` stilize karakter ızgarası (`SurfaceCell { ch, style }`).
  - `dry-shell` bu sayfayı `0x0000_7c00_0000_0000` adresinde `USER_RW`, `dry-display` ise `0x0000_7c80_0000_0000` adresinde **yalnızca `USER_RO`** eşler; `dry-display` arka tamponunu `0x0000_7b00_0000_0000` (`Order2MiB` `WB`), GOP MMIO'yu `0x0000_7d00_0000_0000` (`UC`) adresinde tutar.
- **`dry-shell` Yerleşik Komutları (`userspace/src/shell.rs`):**
  `help`, `clear` (`Ctrl+L`), `system` (`SYS_SYSINFO` canlı CPU/APIC/RAM/IRQ/IPC sayaçları), `uptime` (`SYS_SYSINFO` kalibre edilmiş TSC saniye/ms), `echo <metin>`, `overflow-test` (80 olay basıp `retained=64 dropped=16` doğrular), `crash-input` (`dry-input` içine `#UD` enjekte edip canlı süpervizör kurtarmasını doğrular), `shutdown` (tüm Ring 3 servislerini temiz kapatıp `leaked_frames = 0` ile çıkar). Kısayollar: `Backspace`, `Delete`, `Home`/`End`, `Sol`/`Sağ`, `Ctrl+U`, `Shift+Yukarı/Aşağı` (64-satır kaydırma), `Alt+Ok` (floating pencere taşıma).

---

## 4. Tekrarlanabilir Derleme, Test, Ekran Görüntüsü ve Disassembly Komutları

1. **Derleme + Clippy (`-D warnings`, `dry-userspace` + `dry-kernel`):**
   `python3 tools/qemu_boot_test.py --build-only`
2. **Hızlı P1 Donanım QMP `send-key` + `screendump` + Negatif Güvenlik Paketi (`9/9`, `~32 sn`):**
   `python3 tools/qemu_boot_test.py --p1-only` *(canlı PNG çıktısı: `docs/p1_interactive_shell.png`)*
3. **Hızlı Tekil SMP Duman Testi (`~2 sn`):**
   `python3 tools/qemu_boot_test.py --smp-only 2`
4. **Tam Regresyon + P1 E2E Paketi (`65/65` = `56` Faz 0–1C + `9` P1, `~125 sn`):**
   `python3 tools/qemu_boot_test.py`
5. **Release Çekirdek Yığın ve Sembol Denetimi:**
   `objdump -d -M intel kernel/target/x86_64-unknown-none/release/dry-kernel > /tmp/dry_kernel.asm`

---

## 5. Doğrulanmış Başarılar ve Önce/Sonra Performans Özeti (`12 Kasım 2026`)

- **`65/65` QEMU Testi Yeşil (`0` Clippy Uyarısı, `0` Dosya `>= 1.000` Satır, `leaked_frames = 0`):**
  - `56/56` Faz 0–1C testi (`1/2/4/8/20 vCPU`, `xAPIC`, `-vga none`, `dry_os_first_screen.png`, 48 negatif arıza testi) sıfır regresyonla geçti.
  - `9/9` yeni P1 testi geçti: `p1_interactive_{1,2,4}cpu_e2e`, `p1_interactive_2cpu_headless_e2e` (`-vga none`), `p1_interactive_2cpu_xapic_e2e`, `irq_unauth_ioport_rejected`, `irq_unauth_ack_rejected`, `irq_double_ack_rejected`, `irq_display_cap_leak_rejected`.
- **Ölçülen Döngü ve Yığın Metrikleri (`~3.111 MHz` KVM):**
  - `IoApicHwCache` Önce -> Sonra (`2 vCPU` `p50`): `irq_top` **`30.292 -> 13.014 cyc` (`-%57.0`)**, `irq_ack` **`24.867 -> 7.788 cyc` (`-%68.7`)**, `irq_wake` **`33.433 -> 15.981 cyc` (`-%52.2`)**; `ioport_inb = 8.618 cyc`; `null_sys = 231 cyc` korundu.
  - Ring 3 `DirtyRegion` GOP aktarımı: tek-satır tuş vuruşu (`10.240 px`) **`15.294..18.478 cyc` (`~5 µs`, `29x` hızlı)** vs tam pencere (`276.224 px`) **`449.164..538.855 cyc` (`~150 µs`)**.
  - Çekirdek yığın çerçeveleri (`objdump`): `rust_syscall_dispatch = 248 B` (`0 B` büyüme), `sys_cold_dispatch = 216 B`, `sys_irq_ack = 40 B`.

---

## 6. Açık Eksikler (Dürüst Sınırlar) ve Bir Sonraki Geliştirme Görevi (`Milestone 2A-2`)

### 6.1. Henüz Tamamlanmamış Özellikler (`PLANNED` — Tamamlanmış Gösterilmez)
1. **Türkçe Q Klavye Eşlemesi (`KeymapLayout::TurkishQ`):** `userspace/src/keymap.rs` içinde genişletme arayüzü hazır olup şu an dürüstçe `Err(LayoutNotImplemented)` döner (`UTF-8` çok-baytlı glif ve AltGr katmanı `Milestone 2A-2`'dedir).
2. **Gerçek Fare / İşaretçi Sürücüsü, USB HID ve `VirtIO-Input`:** `MouseEvent` veri modeli `userspace/src/input.rs` içinde tanımlanmıştır, ancak gerçek `PS/2 AUX (IRQ12)`, `VirtIO-Input` veya USB `xHCI` HID sürücüsü henüz yazılmamıştır (floating pencere taşıma şu an gerçek klavye `Alt + Ok` kısayoluyla çalışır).
3. **Birleşik `Endpoint + Notification` Beklemesi ve `CapObjectKind::IoRing`:** Şu an `dry-input` boşta `SYS_NOTIFY_WAIT` zaman aşımı ile `MSG_FLAG_NON_BLOCKING` `Endpoint` yoklamasını birleştirir; `TCB`-bağlı `Notification` ve sıfır-kopyalı `IoRing` veri düzlemi `Milestone 2A-2` hedefidir.
4. **Diğer Faz 2–5 Hedefleri:** `FaultEndpoint` userspace pager, FPU/SIMD `XSAVEOPT`/`XRSTOR`, koruma sayfalı sanal çekirdek yığınları, `DRY Vault` / `Policy Broker` / etiketli `VFS`, tam hibrit `DRY Desktop` WM (`Tiling`/`Tabbed` + `Action API`), `drypkg` ve fiziksel `bare-metal` + VT-d IOMMU doğrulaması (`-smp 20` KVM sonuçları `4C/8T` konak aşırı-aboneliğidir; fiziksel donanım ölçekleme kanıtı değildir).

### 6.2. Sıradaki Somut Mühendislik Görevi (`Milestone 2A-2`)
1. **`TCB`-Bağlı `Notification` (`seL4 MCS` birleşik sunucu beklemesi):** Tek iş parçacıklı Ring 3 sürücü ve compositor servislerinin `SYS_IPC_REPLY_RECV` içinde zaman aşımı yoklaması yapmadan hem `Endpoint` RPC'lerini hem `IrqCap`/`IoRing` bildirimlerini sıfır gecikmeyle beklemesi.
2. **`PS/2 AUX` (`IRQ12` -> `GSI 12`) Fare & `VirtIO-Input` Arka Ucu + İmleç / Sürükleme:** `dry-input` içine ikinci `IrqCap` (`IRQ12`) veya PCI `VirtIO-Input` arka ucu ekleyerek gerçek fare imlecini ve pencere başlık çubuğundan sürükleyerek taşımayı devreye almak; eşzamanlı olarak `KeymapLayout::TurkishQ` ve Latin-5/UTF-8 glif sayfasını tamamlamak.
3. **`CapObjectKind::IoRing` ve FPU/SIMD `XSAVEOPT`:** Yüksek frekanslı girdi/ekran/depolama halkaları ve zengin kullanıcı alanı çizimi için çekirdek veri düzlemi ve FPU bağlam izolasyonunu tamamlamak.
