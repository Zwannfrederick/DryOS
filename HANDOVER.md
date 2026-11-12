# DRY OS — Kapsamlı Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

- **Tarih & Son Güncelleme:** **12 Kasım 2026** (`Milestone 2A-2` / `DRY Desktop Preview` Kapanışı)
- **Mevcut Durum (`P0 — DONE`, `P1 — IN PROGRESS [2A-1 & 2A-2: DONE]`, `P2 — IN PROGRESS [Preview 2A-2: DONE]`):** Faz 0 (`H1` & `H2`), Faz 1 (`1A..1C`: `1df3842`), `Optimization Pass v1` (`69522a3`), Faz 2 `Milestone 2A-1` (`9dab7a6`) ve **`Milestone 2A-2` (`DRY Desktop Preview`)** `%100` tamamlandı; `0` Clippy uyarısı ve **`79/79` otomatik QEMU/KVM senaryosu** (`56/56` Faz 0–1C + `9/9` P1 klavye/kabuk + **`14/14` yeni P2 gerçek PS/2 fare, çok-pencereli Ring 3 WM, çökme izolasyonu, QMP `screendump` ve negatif güvenlik testi**; `leaked_frames = 0`, `stale_tlb_reads = 0`, `uaf_errors = 0`, `desync = 0`) ile doğrulandı.
- **Bu Belgenin Amacı:** İzleyen oturumda görevi devralacak mühendis veya coding agent'ın sıfır bağlam kaybıyla, mevcut güvenlik/yığın/eşzamanlılık değişmezlerini kırmadan doğrudan **`Milestone 2A-3` / `2B`** (`VirtIO-Input`/USB HID, `TurkishQ`, piksel-tamponlu `IoRing` yüzeyler, `TCB`-bağlı `Notification`, `FaultEndpoint`, `XSAVEOPT` ve `DRY Vault` / etiketli `VFS`) aşamasına başlamasını sağlamak.

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
| `2A-1 / P1` (`9dab7a6`) | ACPI MADT IOAPIC, `IrqCap`/`IoPortCap` (`IOPL=0`), `dry-input` (`IRQ1`), `dry-shell`, dinamik GOP compositor | `65/65` QEMU + PNG |
| **`2A-2 / P2 Preview`** *(12 Kasım 2026)* | **Birleşik PS/2 fare (`IRQ12`) + klavye (`IRQ1`), `4 MiB` arka tampon denetimi, yazılımsal imleç, Ring 3 çok-pencereli WM (`Floating`/`SideBySideTiled`), `dry-shell` + `dry-sysmon`** | **`79/79` QEMU + PNG** |

> **Önemli Çalışma Ağacı Notu:** Faz 0'dan kalma `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` dosyalarındaki önceden mevcut yerel değişiklikler kasıtlı olarak korunmuş ve commit dışında bırakılmıştır; **bu iki dosyayı `git add` yapma, silme veya geri alma (`git checkout`).**

---

## 2. Depo ve Alt-Sistem Haritası (Tüm Dosyalar `< 1.000` Satır Tavanı Altında)

### 2.1. Çekirdek (`kernel/src/` — `0` Satır Pencere/UI Kodu)
- **Önyükleme, ACPI, Donanım ve Per-CPU:** `main.rs`, `limine.rs` (Base Rev 3 HHDM/SMP/Modül/GOP/RSDP), `acpi.rs` (`PML4[384]` `RO+NX` 3-sayfa penceresiyle `RSDP -> XSDT/RSDT -> MADT` `IOAPIC` & `ISO` keşfi), `arch.rs`, `gdt_tss.rs`, `percpu.rs` (`256 B` `PerCpuData`), `idt.rs` (`0x30..=0x3F` harici IRQ vektörleri dahil 256-girişli `TrapFrame`), `apic.rs` (`x2APIC`/`xAPIC`, `uptime_telemetry`), `smp.rs`, `serial.rs`, `security.rs` (`ADR-007`), `uaccess.rs` (`CR4.SMAP = 1` + `extable`), `display.rs` (`CapObjectKind::Framebuffer`).
- **Donanım Kesme ve Port G/Ç (`kernel/src/irq/`):** `mod.rs` (`IRQ_KEYBOARD_PS2 = 1 -> vec 0x31`, `IRQ_MOUSE_PS2 = 12 -> vec 0x3c`, `IoApicHwCache`, `irq_line_delivered`, `handle_external_irq`, `sys_irq_bind`, `sys_irq_ack`, fırtına kelepçesi), `ioport.rs` (`RFLAGS.IOPL == 0` altında beyaz/kara listeli `sys_ioport_rw`), `bench.rs`, `tests.rs` (`interactive=1` P1 ve `desktop=1`/`desktop_sec=1`/`desktop_bench=1` P2 oturumları + 8 negatif `irq_fault=` testi).
- **Zamanlayıcı, Bellek, Yetki, Süreç, Syscall ve IPC:** `sched/{state,runqueue,context,preempt,donate,mod,tests,preempt_tests}.rs`; `pmm/{meta,mod,huge_reclaim,untyped,tests}.rs`; `vspace/{pte,mod,huge,tlb,tlb_debt,kernel_handover,tests}.rs`; `cspace/{cap,mod,revoke,objects,tests,bench,fault_tests}.rs` (`cspace_vspace_unmap_2m` dahil); `proc/{elf,mod,supervisor,tests,bench}.rs`; `syscall/{entry,mod,cold}.rs` (`rust_syscall_dispatch = 248 B`, `sys_cold_dispatch = 216 B`, çağrılar `0..=25`, `VSPACE_MAP_OP_UNMAP_2M = 5`, `104 B` `UserSysInfo`); `ipc/{mod,endpoint_state,endpoint,notif,tests,bench}.rs` (64 B hizalı per-CPU `total_ipc_msgs`).

### 2.2. Kullanıcı Alanı (`userspace/`) ve Test Araçları (`tools/`)
- **Kütüphane Modülleri (`userspace/src/`):** `lib.rs` (Syscall `0..=25`, `104 B` `UserSysInfo`, `RootInitBootInfo`, WM/girdi/süpervizör IPC sabitleri ve rozetleri), `input.rs` (`InputEvent`: klavye + `MouseMove`/`MouseButtonDown`/`MouseButtonUp`/`MouseScroll`, 48-bit `timestamp_tsc48`, `InputRingQueue::push_coalescing`), `keymap.rs` (`UsQwerty` + `TurkishQ` genişletme arayüzü), `ps2.rs` (`i8042` klavye + `init_ps2_aux_mouse` + 3-bayt `Ps2MouseDecoder`), `gfx.rs` (`Rect`, 6-kutulu `DirtyRegion`, `CURSOR_SPRITE_12X16`, 8x16 ASCII font), `surface.rs` (sıfır-UB atomik `SharedWindowSurface` `3.904 B` + `SurfaceSnapshot::capture_from_ro`), `wm.rs` (`WindowManager`: `MAX_WINDOWS = 4`, Z-sırası, odak, isabet testi, sürükleme, `Floating`/`SideBySideTiled`), `compositor.rs` (`DesktopCompositor`: imleçsiz `WB` arka tampon + kırpılmış Z-sıralı çizim + tarama satırında yazılımsal imleç bindirme), `shell.rs`.
- **8 Ring 3 ELF64 İkilisi (`userspace/src/bin/`):** `user_hello.rs`, `user_peer.rs`, `dry_init.rs` (1C + P1 + P2 çok-pencereli süpervizör), `dry_echo.rs`, `dry_input.rs` (`RootSystem` `0x0003` tek `i8042` sahibi `IRQ1`+`IRQ12`), `dry_display.rs` (`RootSystem` `0x0004` `4 MiB` çift-tamponlu GOP compositor & WM), `dry_shell.rs` (`Userspace` `0x0102` Pencere A), `dry_sysmon.rs` (`Userspace` `0x0103` Pencere B canlı sistem izleyicisi).
- **Otomasyon (`tools/`):** `qemu_boot_test.py` (`79` senaryo, `--p2-only`, `--p1-only`, `--smp-only`, `--build-only`), `qemu_p2_validators.py` (QMP `input-send-event` fare/buton/klavye, 5 aşamalı `screendump` ve `docs/p2_desktop_preview.png`), `qemu_p1_validators.py`, `qemu_{log,user,ipc}_validators.py`.

---

## 3. `Milestone 2A-1` & `2A-2` İkili Sözleşmeleri ve ABI Referansı

### 3.1. Harici Kesmeler, Birleşik `i8042` Sahipliği ve Syscall Güncellemeleri
- **IDT Harici Vektörleri (`0x30..=0x3F`):** `ISA IRQ 1` (PS/2 klavye) -> `GSI 1` -> **`vec = 0x31`**; `ISA IRQ 12` (PS/2 AUX fare) -> `GSI 12` -> **`vec = 0x3c`**. Tek `dry-input` süreci her iki hattı tek `Notification` üzerinde (`BADGE_IRQ1_KBD = 1`, `BADGE_IRQ12_MOUSE = 2`) dinler ve `status & 0x20 (AUX_OBF)` bitiyle ayrıştırır (`INV-WM-01`).
- **`SYS_SYSINFO (23)` (`104 B` `UserSysInfo`):** `abi_version`, `online_cpus`, `apic_mode`, `ioapic_present`, `fb_present`, `tsc_mhz`, `uptime_ms`, `elapsed_tsc`, `timer_irqs`, `total_ext_irqs`, `total_syscalls`, `total_ctx_switches`, `usable_frames_4k`, `free_frames_4k`, **`irq1_delivered`**, **`irq12_delivered`**, **`total_ipc_msgs`** (`0` çekirdek adresi).
- **`SYS_VSPACE_MAP (25)` `map_op` Kodları:** `0 = MAP_4K_RO`, `1 = MAP_4K_RW`, `2 = UNMAP_4K`, `3 = MAP_2M_RO`, `4 = MAP_2M_RW`, **`5 = UNMAP_2M`** (`cspace_vspace_unmap_2m` ile tam SMP TLB shootdown).
- **`RootInitBootInfo` (`0x0000_7e00_0000_0000`, `11 x u64 = 88 B`):** `[0..7]` P1 yetkileri, `[8] session_mode` (`0` = 1C, `1` = P1, **`2` = P2 masaüstü**, **`3` = P2 WM güvenlik denetimi**, **`4` = P2 performans ölçümü**), **`[9] sysmon_mod_cap`**, **`[10] irq12_cap`**.

### 3.2. Pencere Yöneticisi IPC Opcode'ları, Rozetler ve Sıfır-UB Atomik Yüzey Sözleşmesi
- **Rozetli (`Badged`) `disp_ep` Yetkileri (`userspace/src/lib.rs`):**
  - `BADGE_SUPERVISOR = 0x0001_0001` (`dry-init`: `WM_OP_OWNER_REAPED = 28`), `BADGE_INPUT_SVC = 0x0003_0001` (`dry-input` -> `dry-display` itmeli girdi akışı: `WM_OP_INPUT_EVENT = 24`), `BADGE_WIN_SHELL = 0x0102_0001` (`Window A`), `BADGE_WIN_SYSMON = 0x0103_0002` (`Window B`), `BADGE_WIN_ROGUE_PROBE = 0x0109_0009` (negatif güvenlik testi).
- **WM İşlemleri (`dry_display.rs` & `wm.rs`):**
  - `20 = SURFACE_ATTACH / WIN_CREATE` (`Frame4K` `READ|MAP` yetki devri + geometri/rol -> dönüş `win_id`), `21 = SURFACE_PRESENT` (`owner_badge` zorunlu), `22 = SURFACE_MOVE`, `23 = SURFACE_FOCUS`, `24 = WM_OP_INPUT_EVENT` (yalnızca `BADGE_INPUT_SVC`), `25 = WM_OP_POLL_KEY` (odaktaki pencereye klavye/kapatma olayı teslimi; `mr0 = WM_POLL_CLOSE_REQ` ise uygulama temiz kapanır), `26 = WM_OP_CLOSE_WINDOW`, `27 = WM_OP_SET_LAYOUT`, `28 = WM_OP_OWNER_REAPED` (yalnızca `BADGE_SUPERVISOR`).
- **`InputEvent` 4-Yazmaçlı Paketleme (`userspace/src/input.rs`):**
  - Klavye (`kind = 1|2`, `DEV_ID_PS2_KBD = 1`): P1 ile `%100` geriye uyumlu.
  - Fare (`kind = 3 [Move] | 4 [BtnDown] | 5 [BtnUp] | 6 [Scroll]`, `DEV_ID_PS2_MOUSE = 2`): `mr0` içinde işaretli `dx: i16`, `dy: i16`, `buttons: u8`, `changed_btn: u8`; `mr1 = seq`; `mr2 = [47:0] timestamp_tsc48 | [63:48] wheel`; `mr3 = queue_overflow_count`.
- **Sıfır-UB Atomik `SharedWindowSurface` (`userspace/src/surface.rs`, `3.904 B <= Frame4K`):**
  - Tüm başlık ve hücre kelimeleri `AtomicU64`/`AtomicU32`/`AtomicU16` olarak tanımlıdır; paylaşımlı sayfa üzerinde hiçbir zaman `&mut` oluşturulmaz (`INV-WM-04`). `dry-display` kendi özel sanal pencerelerinde (`0x0000_7c80_0000_0000 + slot * 0x1000`, `USER_RO`) `SurfaceSnapshot::capture_from_ro` ile tutarlı anlık görüntü yakalar; `2 x Order2MiB = 4 MiB` (`WB`) arka tamponunda Z-sıralı kırpılmış sahneyi birleştirir ve `12x16` imleci yalnızca GOP MMIO tarama satırı yazımında anlık bindirir.

---

## 4. Tekrarlanabilir Derleme, Test, Ekran Görüntüsü ve Disassembly Komutları

1. **Derleme + Clippy (`-D warnings`, `dry-userspace` + `dry-kernel`):**
   `python3 tools/qemu_boot_test.py --build-only`
2. **Hızlı `Milestone 2A-2` QMP Fare + Çoklu Pencere + Çökme İzolasyonu + WM Güvenlik Paketi (`14/14`, `~35 sn`):**
   `python3 tools/qemu_boot_test.py --p2-only` *(canlı PNG çıktısı: `docs/p2_desktop_preview.png`)*
3. **Hızlı `Milestone 2A-1` P1 Klavye/Kabuk Paketi (`9/9`, `~30 sn`):**
   `python3 tools/qemu_boot_test.py --p1-only` *(canlı PNG çıktısı: `docs/p1_interactive_shell.png`)*
4. **Tam Regresyon + P1 + P2 Masaüstü Paketi (`79/79` = `56` Faz 0–1C + `9` P1 + `14` P2, `~155 sn`):**
   `python3 tools/qemu_boot_test.py`
5. **Release Çekirdek Yığın Çerçevesi Denetimi:**
   `objdump -d -M intel kernel/target/x86_64-unknown-none/release/dry-kernel > /tmp/dry_kernel.asm`

---

## 5. Doğrulanmış Başarılar ve Performans Özeti (`12 Kasım 2026`)

- **`79/79` QEMU Testi Yeşil (`0` Clippy Uyarısı, `0` Dosya `>= 1.000` Satır, `leaked_frames = 0`, `desync = 0`):**
  - `56/56` Faz 0–1C + `9/9` P1 testi sıfır regresyonla korundu.
  - `14/14` yeni P2 testi geçti: `p2_desktop_qmp_{1,2,4}cpu`, `p2_desktop_headless_2cpu` (`-vga none`), `p2_desktop_qmp_xapic_2cpu`, `p2_cursor_zorder_tiling_2cpu`, `p2_app_crash_isolation_2cpu`, `p2_irq_interleave_flood_2cpu`, `p2_wm_security_rejection_2cpu`, `p2_perf_bench_2cpu` ve 4 çekirdek negatif testi (`irq12_unauth_bind_rejected`, `irq12_split_owner_rejected`, `wm_surface_ro_escalation_rejected`, `fb_backing_oob_rejected`).
- **Ölçülen `Milestone 2A-2` Döngü ve Bellek Metrikleri (`2 vCPU`, `~3.111 MHz` KVM):**
  - **Arka Tampon Denetimi:** `1280x800x32` (`4.096.000 B`) için `2 x Frame2M = 4.194.304 B` (`4 MiB WB RAM`), kapanışta `UNMAP_2M` ile `leaked_frames = 0`.
  - **Fare ve Çoklu Pencere Gecikmeleri (`p50 / p95 / p99`):** `irq12_drain` **`248.119 / 420.407 / 455.106 cyc`** (`~79.8 µs`), `in2comp` **`67.862 / 92.741 / 169.404 cyc`** (`~21.8 µs`), yazılımsal imleç `cur` (`92 px`) **`10.872 / 16.101 / 18.326 cyc`** (`~3.5 µs`; kesme selinde **`5.528 cyc`** [`~1.8 µs`]), pencere sürükleme `drag` **`503.295 / 718.365 / 718.365 cyc`** (`~161.8 µs`), `pres1w` `p50 = 41.997 cyc`, `pres2w` `p50 = 71.525 cyc`.
  - **Çekirdek Yığın Çerçeveleri (`objdump`):** `rust_syscall_dispatch = 248 B` (`+0 B`), `sys_cold_dispatch = 216 B` (`+0 B`), `sys_irq_ack = 40 B` (`+0 B`).

---

## 6. Açık Eksikler (Dürüst Sınırlar) ve Sıradaki Geliştirme Görevi (`Milestone 2A-3` / `2B`)

### 6.1. Bilinen Sınırlar ve Henüz Tamamlanmamış Özellikler (`PLANNED`)
1. **Girdi Arka Uçları ve Klavye Haritası:** Şu an donanım girdisi `PS/2 i8042` klavye (`IRQ1`) + `PS/2 AUX` fare (`IRQ12`, 3-bayt protokol; tekerlek Z-ekseni alanı `InputEvent`'te ayrılmış olup IntelliMouse `4`-bayt uzantısı ve `VirtIO-Input` / USB `xHCI` HID henüz `PLANNED` durumdadır). `KeymapLayout::TurkishQ` arayüzü hazır olup `Err(LayoutNotImplemented)` döner.
2. **Yüzey Türü ve Pencere Kapasitesi:** Mevcut `SharedWindowSurface` tek `Frame4K` (`80x24` stilize hücre ızgarası) ve `MAX_WINDOWS = 4` ile sınırlandırılmıştır; keyfi çözünürlüklü piksel yüzeyleri (`IoRing` + `Frame2M`/`SharedPayloadDesc`), pencere yeniden boyutlandırma tutamaçları ve `Stacking/Tabbed` grupları sonraki dilimdedir.
3. **Birleşik Bekleme ve Çekirdek Faz 2 Özellikleri:** `TCB`-bağlı `Notification` (`seL4 MCS` birleşik `Endpoint + Notification` beklemesi), `CapObjectKind::IoRing`, `FaultEndpoint` userspace pager, FPU/SIMD `XSAVEOPT`, `DRY Vault` / etiketli `VFS` ve fiziksel `bare-metal` + VT-d IOMMU testleri `PLANNED` aşamasındadır.

### 6.2. Sıradaki Somut Mühendislik Görevi (`Milestone 2A-3` / `Milestone 2B`)
1. **`TCB`-Bağlı `Notification` + `CapObjectKind::IoRing`:** `dry-input` ve `dry-display` içindeki zaman aşımlı `SYS_NOTIFY_WAIT` + `MSG_FLAG_NON_BLOCKING` yoklama döngüsünü tek bloklanan çekirdek beklemesi ve sıfır-kopyalı olay/piksel halkalarıyla değiştirmek.
2. **`VirtIO-Input` / Piksel Yüzeyleri + `KeymapLayout::TurkishQ` + FPU `XSAVEOPT`:** Sabit hücre yüzeyi yanında keyfi piksel yüzeylerini, fare tekerleğini (`MouseScroll`), Türkçe Q klavye düzenini ve kullanıcı alanı FPU/SIMD bağlam izolasyonunu devreye almak.
3. **`Milestone 2B` (`DRY Vault`, `Policy Broker` & Etiketli `VFS`):** `ADR-007` `lease_epoch` oturum kiralamasını ve `INV-SEC-05` etiket miraslı dosya sistemini masaüstü uygulamalarına bağlamak.
