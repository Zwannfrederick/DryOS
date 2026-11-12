# DRY OS — Kapsamlı Mühendislik Devir Teslim Kılavuzu (`HANDOVER.md`)

- **Tarih & Son Güncelleme:** **12 Kasım 2026** (`Milestone 2B` / `Persistent VirtIO-Blk Storage, Lock-Free IoRing, Crash-Safe DRYFS_v1 & DRY Vault` + `Track B Compatibility Research` Kapanışı)
- **Mevcut Durum (`P0 — DONE`, `P1 — IN PROGRESS [2A-1..2B: DONE]`, `P2 — IN PROGRESS [2A-2..2B: DONE]`):** Faz 0 (`H1` & `H2`), Faz 1 (`1A..1C`: `1df3842`), `Optimization Pass v1` (`69522a3`), `Milestone 2A-1` (`9dab7a6`), `Milestone 2A-2` (`85ec902`), `Milestone 2A-3` (`0da9d9d..58ccda6`) ve **`Milestone 2B` (`ADR-015`)** `%100` tamamlandı; `0` Clippy uyarısı, **`88/88` otomatik QEMU/KVM senaryosu** (`56/56` Faz 0–1C + `9/9` P1 + `14/14` P2 + `6/6` P3 + **`3/3` yeni P4 kalıcı blok depolama, `SIGKILL` güç kesintisi kurtarması ve `DRY Vault` / DMA / `DEV_WORKSPACE` güvenlik testi**) ve **`5/5` SMP (`1/2/4/8/20 vCPU`) & RAM (`256M/512M/1G/2G`) masaüstü matrisi** (`leaked_frames = 0`, `stale_tlb_reads = 0`, `uaf_errors = 0`, `desync = 0`, `pci_quiesce >= 1`) ile doğrulandı.
- **Paralel Araştırma Çıktısı (`Track B`):** Linux/Windows uygulama uyumluluğu ve uyarlamalı sürücü adaları araştırması tamamlanarak kanonik `docs/COMPATIBILITY_ISLANDS_RESEARCH.md` ve ham deney verileri `docs/COMPATIBILITY_EXPERIMENTS_DATA.md` (`tools/compat_bench/`) üretildi.
- **Bu Belgenin Amacı:** İzleyen oturumda görevi devralacak mühendis veya coding agent'ın sıfır bağlam kaybıyla doğrudan **`Milestone 2A-4` / `2C` / `Faz 3`** (`FaultEndpoint` userspace pager, FPU `XSAVEOPT`, `liquid-glass` piksel yüzeyleri, native `VirtIO-Net`/`NVMe` ve `Track B` Kademe 1 `dry-linux-abi` PoC) aşamasına başlamasını sağlamak.

---

## 1. Git Commit Zinciri ve Kilometre Taşı Özeti

| Aşama | Referans / Kapsam | Doğrulama |
| :--- | :--- | :---: |
| `ADR-007` (`a443e6c`) | `KERNEL (0)` / `ROOT_SYSTEM (1)` / `USERSPACE (2)` izolasyonu, `SecLabel` (8 B), `DRY Vault` kiralama | Öz-test |
| `1A-B..1A-G` (`bc46907..fad423b`) | Limine UEFI SMP (`1..20 vCPU`), per-CPU `GDT`/`TSS`/`IST1`, preemptive `H1`, `x2APIC`/`xAPIC` kalibrasyonu | `21/21` QEMU |
| `1B-A..1B-D` (`78ac64b..29002d3`) | `PMM` (`16 B` `FrameMeta`), 4-seviye `VSpace` (`W^X`), SMP TLB shootdown, `CSpace` CDT `revoke`, `CPL=3` ELF64 + `SMAP` | `50/50` QEMU |
| `1C` (`1df3842`) | `Endpoint`/`Notification` IPC, 4-sıçrama Öncelik Bağışı, `dry-init`/`dry-echo` ve ilk Ring 3 GOP ekranı | `56/56` QEMU |
| `Opt v1` (`69522a3`) | `null_sys 231 cyc`, birleşik `ep_reply_recv` (`call_reply 1.952 cyc` [`12.8x`]), `248 B` sıcak syscall çerçevesi | `56/56` QEMU |
| `2A-1 / P1` (`9dab7a6`) | ACPI MADT IOAPIC, `IrqCap`/`IoPortCap` (`IOPL=0`), `dry-input` (`IRQ1`), `dry-shell`, dinamik GOP compositor | `65/65` QEMU + PNG |
| `2A-2 / P2 Preview` (`85ec902`) | Birleşik PS/2 fare (`IRQ12`) + klavye (`IRQ1`), `4 MiB` arka tampon denetimi, yazılımsal imleç, çok-pencereli WM | `79/79` QEMU + PNG |
| `2A-3 P3-A..C` (`0da9d9d..58ccda6`) | `Launcher` + 4 yerel uygulama, `dry-vfs` (`RO /boot` + uçucu `/ram`), `TurkishQ`, PS/2 tekerlek, `dry-power` (ACPI S5/Reset, `HLT`, `DPMS`) | `85/85` + `5/5` Matris |
| **`2B / P4`** (`ADR-015`) | `PciDev`/`SYS_DMA_ALLOC` (`BME=0` susturma), `dry-blk` (`VirtIO 1.0`) + `IoRing`, çökme-güvenli CoW `DRYFS_v1` (`/persist`), `dry-vault` (`PBKDF2-SHA256` `Session Lock` + $O(1)$ `lease_epoch` + `RO_EXPORT`) | **`88/88` + `5/5` Matris** |

> **Önemli Çalışma Ağacı Notu:** Faz 0'dan kalma `experiments/h1-sched/src/runqueue.rs` ve `experiments/h1-sched/src/sync.rs` dosyalarındaki önceden mevcut yerel değişiklikler kasıtlı olarak korunmuş ve commit dışında bırakılmıştır; **bu iki dosyayı `git add` yapma, silme veya geri alma (`git checkout`).**

---

## 2. Depo ve Alt-Sistem Haritası (Tüm Dosyalar `< 1.000` Satır Tavanı Altında)

### 2.1. Çekirdek (`kernel/src/` — `0` Satır Pencere/UI/VFS/Kripto Politikası)
- **Önyükleme, PCI, Vault, ACPI ve Güç:** `main.rs`, `limine.rs`, `pci.rs` (`CapObjectKind::PciDev = 14`, `SYS_DEV_MAP = 29`, `SYS_DMA_ALLOC = 30`, `iommu_pin_count == 1` ve sürücü çöküşünde zorunlu `PCI Command.BME = 0` donanım susturma), `vault_ctrl.rs` (`CapObjectKind::VaultCtrl = 15`, `SYS_VAULT_CTL = 31`: `QUERY_EPOCH`, $O(1)$ `ADVANCE_EPOCH`, `SEAL_RO_EXPORT`), `acpi.rs`, `power.rs`, `arch.rs` (`outl`/`inl`/`outw`/`inw`), `gdt_tss.rs`, `percpu.rs`, `idt.rs`, `apic.rs`, `smp.rs`, `serial.rs`, `security.rs` (`ADR-007`), `uaccess.rs` (`CR4.SMAP = 1`), `display.rs`.
- **Zamanlayıcı, Bellek, Yetki, Süreç, Syscall ve IPC:** `sched/mod.rs`; `pmm/{meta,mod,huge_reclaim,untyped,tests}.rs`; `vspace/{pte,mod,huge,tlb,tlb_debt,kernel_handover,tests}.rs` (`map_mmio_4k`); `cspace/{cap,mod,revoke,objects,tests,bench,fault_tests}.rs` (`attach_cap_mapping_range_record`, `cspace_create_readonly_export_from_user`); `proc/{elf,mod,supervisor,tests,bench}.rs`; `syscall/{entry,mod,cold}.rs` (`rust_syscall_dispatch = 248 B`, `sys_cold_dispatch = 264 B`, `sys_irq_ack = 40 B`, çağrılar `0..=31`, `144 B` `UserSysInfo`); `ipc/{mod,endpoint_state,endpoint,notif,tests,bench}.rs`; `irq/{mod,session,tests}.rs`.

### 2.2. Kullanıcı Alanı (`userspace/`) ve Test/Araştırma Araçları (`tools/`)
- **Kütüphane Modülleri (`userspace/src/`):** `lib.rs` + `abi_ext.rs` (`SYS_DEV_MAP`, `SYS_DMA_ALLOC`, `SYS_VAULT_CTL`, `UserVirtioPciInfo`), `ioring.rs` (`IoRingSharedPage` `SQ`/`CQ` + `Notification` kapı zilleri + `BLK_OP_DETACH_RING`), `persist_fs.rs` (`DRYFS_v1` çift A/B `CRC32` süperblok + 4 KiB CoW + `INV-SEC-05` `SecLabel` denetimi), `vault_crypto.rs` (`SHA-256`, `HMAC-SHA256`, `PBKDF2-HMAC-SHA256`, `ct_eq_32`, `CRC32`), `vault_proto.rs`, `vfs_proto.rs`, `vfs_mem.rs`, `sup_ctx.rs`, `sup_audit.rs`, `supervisor.rs` + `sup_services.rs`, `action.rs`, `input.rs`, `keymap.rs`, `ps2.rs`, `gfx.rs`, `surface.rs`, `wm_types.rs`, `wm.rs`, `wm_ipc.rs`, `compositor.rs`, `shell.rs`, `shell_ext.rs`.
- **14 Ring 3 ELF64 İkilisi (`userspace/src/bin/`):** `user_hello.rs`, `user_peer.rs`, `dry_init.rs`, `dry_echo.rs`, `dry_input.rs` (`0x0003`), `dry_display.rs` (`0x0002`), `dry_vfs.rs` (`0x0004`: `/boot` + `/ram` + kalıcı `/persist`), `dry_power.rs` (`0x0005`), `dry_blk.rs` (`0x0006` VirtIO 1.0 Modern blok sürücüsü), `dry_vault.rs` (`0x0007` kimlik doğrulama ve kiralama servisi), `dry_shell.rs` (`0x0102`), `dry_sysmon.rs` (`0x0103`), `dry_files.rs` (`0x0104`), `dry_settings.rs` (`0x0105`).
- **Otomasyon ve Uyumluluk Deneyleri (`tools/`):** `qemu_boot_test.py` (`88` senaryo + `5` matris; `--p4-only`, `--p3-only`, `--p3-matrix`), `qemu_p4_validators.py`, `qemu_p3*.py`, `qemu_p2_validators.py`, `qemu_p1_validators.py`, `compat_bench/{microvm_dormant_bench.py,audit_driver_classes.py,ioring_batch_bench.rs,results_*.json}`.

---

## 3. Tekrarlanabilir Derleme, Test ve Denetim Komutları

1. **Derleme + Clippy (`-D warnings`, `dry-userspace` + `dry-kernel`):** `python3 tools/qemu_boot_test.py --build-only`
2. **Hızlı `Milestone 2B` Paketi (`3/3`, `~22 sn`):** `python3 tools/qemu_boot_test.py --p4-only` *(PNG kanıtları: `docs/p4_persist_vault_desktop.png`, `docs/p4_vault_auth_lock.png`)*
3. **SMP (`1..20 vCPU`) & RAM (`256M..2G`) Masaüstü Matrisi (`5/5`, `~24 sn`):** `python3 tools/qemu_boot_test.py --p3-matrix`
4. **Tam Regresyon Paketi (`88/88`, `~230 sn`):** `python3 tools/qemu_boot_test.py`
5. **Çekirdek Yığın Çerçevesi Denetimi (`248 B` / `264 B` / `40 B`):**
   `objdump -d -M intel kernel/target/x86_64-unknown-none/release/dry-kernel | grep -A8 -E "(rust_syscall_dispatch|sys_cold_dispatch|sys_irq_ack).*:"`

---

## 4. Açık Eksikler ve Sıradaki Geliştirme Görevleri (`Milestone 2A-4` / `2C` / `Faz 3`)

1. **Çekirdek Ön Koşulları (`Milestone 2A-4`):** `FaultEndpoint` userspace sayfalayıcı yönlendirmesi ve `SYSCALL` tuzak yönlendirmesi (`dry-linux-abi` için zorunlu), FPU/SIMD `XSAVEOPT`/`XRSTOR` bağlam izolasyonu ve piksel-tamponlu `IoRing` yüzeyleri.
2. **Ağ/Depolama ve Bare-Metal (`Milestone 2C` & `P5`):** Native Rust `VirtIO-Net` / temel `NVMe`, PCIe MSI-X kesme yönlendirmesi ve fiziksel silikon Intel VT-d / AMD-Vi IOMMU doğrulaması (`R-03`, `R-05`).
3. **Uyumluluk PoC Yol Haritası (`Faz 3`):** `docs/COMPATIBILITY_ISLANDS_RESEARCH.md` Bölüm 18'de tanımlanan kademeli `dry-linux-abi` (musl statik/dinamik ELF64) ve `H2` `igc`/`e1000e` sürücü adası entegrasyonu.
