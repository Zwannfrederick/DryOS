# DRY OS — Kapsamlı Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

- **Tarih & Son Güncelleme:** **12 Kasım 2026** (`Milestone 2A-3` / `Functional Desktop Foundation, Native Applications & Power Lifecycle` Kapanışı)
- **Mevcut Durum (`P0 — DONE`, `P1 — IN PROGRESS [2A-1..2A-3: DONE]`, `P2 — IN PROGRESS [2A-2 & 2A-3: DONE]`):** Faz 0 (`H1` & `H2`), Faz 1 (`1A..1C`: `1df3842`), `Optimization Pass v1` (`69522a3`), `Milestone 2A-1` (`9dab7a6`), `Milestone 2A-2` (`85ec902`) ve **`Milestone 2A-3` (`0da9d9d` `P3-A`, `ce61fea` `P3-B`, `58ccda6` `P3-C`)** `%100` tamamlandı; `0` Clippy uyarısı, **`85/85` otomatik QEMU/KVM senaryosu** (`56/56` Faz 0–1C + `9/9` P1 + `14/14` P2 + **`6/6` yeni P3 yerel uygulama yaşam döngüsü, VFS/Settings/WM, güvenlik ve donanım güç testi**) ve **`5/5` SMP (`1/2/4/8/20 vCPU`) & RAM (`256M/512M/1G/2G`) masaüstü matrisi** (`leaked_frames = 0`, `stale_tlb_reads = 0`, `uaf_errors = 0`, `desync = 0`) ile doğrulandı.
- **Bu Belgenin Amacı:** İzleyen oturumda görevi devralacak mühendis veya coding agent'ın sıfır bağlam kaybıyla, mevcut güvenlik/yığın/eşzamanlılık değişmezlerini kırmadan doğrudan **`Milestone 2A-4` / `2B`** (`liquid-glass` görsel masaüstü katmanı, piksel-tamponlu `IoRing` yüzeyler, `TCB`-bağlı `Notification`, `FaultEndpoint`, `XSAVEOPT` ve `DRY Vault` kimlik doğrulamalı oturum kilidi + kalıcı etiketli `VFS`) aşamasına başlamasını sağlamak.

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
| `2A-2 / P2 Preview` (`85ec902`) | Birleşik PS/2 fare (`IRQ12`) + klavye (`IRQ1`), `4 MiB` arka tampon denetimi, yazılımsal imleç, çok-pencereli WM | `79/79` QEMU + PNG |
| **`2A-3 P3-A`** (`0da9d9d`) | Birleşik uygulama yaşam döngüsü, `Launcher`, gelişmiş `Terminal`, `Life Station` (`SYS_PROC_LIST`/`SYS_PROC_KILL`) | `81/81` QEMU + PNG |
| **`2A-3 P3-B`** (`ce61fea`) | Ring 3 `dry-vfs` (`RO /boot` + uçucu `/ram`), `Files`, canlı `Settings`, `TurkishQ`, PS/2 tekerlek, `Stacked`/`Tabbed` WM | `82/82` QEMU + PNG |
| **`2A-3 P3-C`** (`58ccda6`) | `dry-power` (`CapObjectKind::PowerCtrl`), `HLT` boşta + `VEC_RESCHED_WAKE`, `DPMS`, `Session Lock`, gerçek ACPI S5/Reset, `-512 KiB` `.bss` | **`85/85` + `5/5` Matris** |

> **Önemli Çalışma Ağacı Notu:** Faz 0'dan kalma `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` dosyalarındaki önceden mevcut yerel değişiklikler kasıtlı olarak korunmuş ve commit dışında bırakılmıştır; **bu iki dosyayı `git add` yapma, silme veya geri alma (`git checkout`).**

---

## 2. Depo ve Alt-Sistem Haritası (Tüm Dosyalar `< 1.000` Satır Tavanı Altında)

### 2.1. Çekirdek (`kernel/src/` — `0` Satır Pencere/UI/VFS Politikası)
- **Önyükleme, ACPI, Güç ve Per-CPU:** `main.rs`, `limine.rs`, `acpi.rs` (`MADT` `IOAPIC`/`ISO` + `FADT` `PM1a_CNT_BLK`/`RESET_REG` + `FACS` + `DSDT` `\_S5_`/`\_S3_` AML keşfi), `power.rs` (`CapObjectKind::PowerCtrl = 13`, `sys_power_ctl`, gerçek ACPI S5 & FADT Reset, dürüst S3 `-ENOSYS`), `arch.rs` (`outw`/`inw`), `gdt_tss.rs`, `percpu.rs`, `idt.rs` (`0x30..=0x3F` harici IRQ + `VEC_RESCHED_WAKE = 0xed` + `VEC_TLB_SHOOTDOWN = 0xee`), `apic.rs` (`GATE2=1` öncesi `TIMER_INIT`+`rdtsc` PIT kalibrasyonu), `smp.rs`, `serial.rs`, `security.rs` (`ADR-007`), `uaccess.rs` (`CR4.SMAP = 1`), `display.rs`.
- **Zamanlayıcı, Bellek, Yetki, Süreç, Syscall ve IPC:** `sched/mod.rs` (yarışsız `idle_park_or_spin` `sti; hlt` + `wake_thread` `VEC_RESCHED_WAKE` IPI + `idle_hlt_telemetry`); `pmm/{meta,mod,huge_reclaim,untyped,tests}.rs` (intrüzif HHDM OOM listesiyle `-512 KiB` `.bss` ve `256M..64G+` ölçeklenmesi); `vspace/{pte,mod,huge,tlb,tlb_debt,kernel_handover,tests}.rs` (TTAS `TLB_REQ_LOCK` + `NEXT_REQ_GEN` ilerleme bekçisi); `cspace/{cap,mod,revoke,objects,tests,bench,fault_tests}.rs`; `proc/{elf,mod,supervisor,tests,bench}.rs`; `syscall/{entry,mod,cold}.rs` (`rust_syscall_dispatch = 248 B`, `sys_cold_dispatch = 216 B`, çağrılar `0..=28`, `120 B` `UserSysInfo`, `24 B` `UserProcEntry`); `ipc/{mod,endpoint_state,endpoint,notif,tests,bench}.rs`.

### 2.2. Kullanıcı Alanı (`userspace/`) ve Test Araçları (`tools/`)
- **Kütüphane Modülleri (`userspace/src/`):** `lib.rs` (Syscall `0..=28`, `120 B` `UserSysInfo`, `RootInitBootInfo` `[0..15]`, WM/VFS/Power/Supervisor IPC sabitleri ve rozetleri), `action.rs` (`AppId`, `DesktopAction`), `supervisor.rs` + `sup_services.rs` (`dry-init` masaüstü orkestratörü), `init_legacy.rs` (1C/P1/P2 oturumları), `input.rs`, `keymap.rs` (`UsQwerty` + tam `TurkishQ`), `ps2.rs` (`i8042` klavye + IntelliMouse 4-bayt tekerlekli fare), `gfx.rs` (`1x`/`2x` imleç), `surface.rs`, `wm.rs` + `wm_ipc.rs` (`MAX_WINDOWS = 6`, `Floating`/`SideBySideTiled`/`Stacked`/`Tabbed`, `WindowRole::DialogTransient`, `DPMS` & `Session Lock`), `compositor.rs` (`Standard Dark` / `High Contrast`, DPMS `0x000000` ve kilit kalkanı çizimi), `shell.rs` (satır içi düzenleme, geçmiş, kaydırma ve komut ayrıştırıcı).
- **12 Ring 3 ELF64 İkilisi (`userspace/src/bin/`):** `user_hello.rs`, `user_peer.rs`, `dry_init.rs`, `dry_echo.rs`, `dry_input.rs` (`0x0003`), `dry_display.rs` (`0x0002`), `dry_vfs.rs` (`0x0004` `RO /boot` + uçucu `/ram`), `dry_power.rs` (`0x0005` güç servisi), `dry_shell.rs` (`0x0102` `Terminal`), `dry_sysmon.rs` (`0x0103` `Life Station`), `dry_files.rs` (`0x0104` `Files`), `dry_settings.rs` (`0x0105` `Settings`).
- **Otomasyon (`tools/`):** `qemu_boot_test.py` (`85` senaryo + `5` matris senaryosu; `--p3-only`, `--p3-matrix`, `--p2-only`, `--p1-only`, `--smp-only`, `--build-only`), `qemu_p3_validators.py`, `qemu_p3c_validators.py`, `qemu_p2_validators.py`, `qemu_p1_validators.py`, `qemu_{log,user,ipc}_validators.py`.

---

## 3. `Milestone 2A-3` Yeni Sistem Çağrıları ve IPC Sözleşmeleri Özeti

- **Yeni Soğuk Syscall'lar (`kernel/src/syscall/cold.rs`):**
  - `26 = SYS_PROC_LIST(buf_ptr, max_entries)` -> canlı süreç sayısı (`UserProcEntry`: `pid`, `state`, `domain`, `compartment_id`, `vspace_id`, `cspace_id`, `thread_count`, `name_tag`).
  - `27 = SYS_PROC_KILL(proc_cap)` -> süpervizör sahipliğinde süreç sonlandırma ve geri kazanım.
  - `28 = SYS_POWER_CTL(power_cap, op)` -> `CapObjectKind::PowerCtrl (13)` zorunlu (`1 = SHUTDOWN_S5`, `2 = REBOOT`, `3 = SUSPEND_S3 [-ENOSYS]`).
- **Yeni IPC Opcode Grupları (`userspace/src/lib.rs`):**
  - **WM (`disp_ep`):** `20..28` (P2) + `29 = WM_OP_POP_ACTION` (`BADGE_SUPERVISOR`), `30 = WM_OP_SET_PREFS`, `31 = WM_OP_SESSION_CTL` (`0 = Active`, `1 = Blanked`, `2 = Locked`).
  - **Input (`in_ep`):** `35 = INPUT_OP_SET_PREFS` (`keymap`, `cursor_speed`).
  - **Supervisor (`sup_ep`):** `40 = SUP_OP_EXEC_ACTION` (rozet doğrulamalı `DesktopAction` yürütme).
  - **VFS (`vfs_ep`):** `45 = VFS_OP_LIST`, `46 = VFS_OP_STAT`, `47 = VFS_OP_READ`, `48 = VFS_OP_CREATE_WRITE`, `49 = VFS_OP_RENAME`, `50 = VFS_OP_UNLINK` (4 KiB paylaşımlı aktarım penceresi + kompartman sahiplik denetimi).
  - **Power (`pwr_ep`):** `55 = POWER_OP_REQUEST`, `56 = POWER_OP_QUERY_CAPS` (yalnızca `BADGE_PWR_SUPERVISOR`).

---

## 4. Tekrarlanabilir Derleme, Test ve Denetim Komutları

1. **Derleme + Clippy (`-D warnings`, `dry-userspace` + `dry-kernel`):** `python3 tools/qemu_boot_test.py --build-only`
2. **Hızlı `Milestone 2A-3` Paketi (`6/6`, `~36 sn`):** `python3 tools/qemu_boot_test.py --p3-only` *(PNG kanıtları: `docs/p3a_apps_lifecycle.png`, `docs/p3b_files_settings_wm.png`, `docs/p3c_lock_shield.png`)*
3. **SMP (`1..20 vCPU`) & RAM (`256M..2G`) Masaüstü Matrisi (`5/5`, `~25 sn`):** `python3 tools/qemu_boot_test.py --p3-matrix`
4. **Tam Regresyon Paketi (`85/85`, `~210 sn`):** `python3 tools/qemu_boot_test.py`
5. **Çekirdek Yığın Çerçevesi Denetimi (`248 B` / `216 B` / `40 B`):**
   `objdump -d -M intel kernel/target/x86_64-unknown-none/release/dry-kernel | grep -A8 -E "(rust_syscall_dispatch|sys_cold_dispatch).*:"`

---

## 5. Açık Eksikler (Dürüst Sınırlar) ve Sıradaki Geliştirme Görevi (`Milestone 2A-4` / `2B`)

1. **Görsel Arayüz & Piksel Yüzeyleri (`Milestone 2A-4`):** Mevcut masaüstü bilinçli olarak işlevsel ve yüksek kontrastlı stilize hücre yüzeyi (`SharedWindowSurface` `Frame4K`) üzerine kuruludur; nihai `liquid-glass` görsel estetiği, animasyonlar ve keyfi çözünürlüklü piksel yüzeyleri (`CapObjectKind::IoRing` + `Frame2M`/`SharedPayloadDesc` + `XSAVEOPT`) bir sonraki görsel/veri-düzlemi aşamasındadır.
2. **Kimlik Doğrulama & Kalıcı Depolama (`Milestone 2B`):** Mevcut `Session Lock` girdi-engelleyici bir kalkandır (`UNAUTHENTICATED LOCK SHIELD`) ve `dry-vfs` `/ram` alanı uçucudur; kriptografik parola/oturum doğrulaması `DRY Vault` ile, kalıcı dosya sistemi ise `VirtIO-Blk`/NVMe sürücüsüyle eklenecektir.
3. **`Suspend-to-RAM (ACPI S3)` (`Faz 4 / P5`):** `< 1 MiB` 16-bit gerçek-mod -> 64-bit Long Mode uyanma trampolini, AP `INIT-SIPI-SIPI` yeniden girişi ve GOP yeniden başlatma zinciri tamamlanana dek `suspend` dürüstçe `-ENOSYS` döner (ayrıntılı teknik analiz: `docs/EXPERIMENTS_2A3.md` Bölüm 5).
