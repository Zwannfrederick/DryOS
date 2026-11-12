# DRY OS — `Milestone 2B` Deneysel Ölçümler, Kalıcı `DRYFS_v1` Kurtarma ve `DRY Vault` / DMA Güvenlik Denetimi (`docs/EXPERIMENTS_2B.md`)

> **Tarih:** 12 Kasım 2026  
> **Kapsam:** `Milestone 2B` (`Persistent VirtIO-Blk Storage, Lock-Free IoRing, Crash-Safe DRYFS_v1 & DRY Vault Security Enforcement` — `ADR-015`)  
> **Ortam:** QEMU/KVM `q35` + OVMF UEFI (`~3.114 MHz` TSC, `virtio-blk-pci,disable-legacy=on` 16 MiB ham disk imajı; `4C/8T` Intel Core i5-11300H konak)

---

## 1. Tekrarlanabilir Doğrulama Komutları

```bash
# Yalnızca Milestone 2B kalıcı disk, SIGKILL güç kesintisi kurtarma ve DRY Vault / DMA / DEV_WORKSPACE güvenlik paketi (3/3)
python3 tools/qemu_boot_test.py --p4-only

# SMP (1/2/4/8/20 vCPU) ve RAM (256M/512M/1G/2G) masaüstü matrisi (5/5)
python3 tools/qemu_boot_test.py --p3-matrix

# Tam deterministik regresyon paketi (56 Faz 0-1C + 9 P1 + 14 P2 + 6 P3 + 3 P4 = 88/88)
python3 tools/qemu_boot_test.py

# Çekirdek sıcak/soğuk syscall yığın çerçevesi denetimi (248 B / 264 B / 40 B)
objdump -d -M intel kernel/target/x86_64-unknown-none/release/dry-kernel | grep -A8 -E "(rust_syscall_dispatch|sys_cold_dispatch|sys_irq_ack).*:"
```

---

## 2. Uçtan Uca Senaryo Sonuçları (`3/3` Yeni `P4` + `88/88` Tam Paket + `5/5` Matris)

| Senaryo | Doğrulanan Mekanizma ve Invariant'lar | Ölçülen Çıktı & Kanıt | Sonuç |
| :--- | :--- | :--- | :---: |
| **`p4a_virtio_blk_persist_across_reboots_e2e`** | 2 ardışık soğuk önyükleme: Boot #1 taze `DRYFS_v1` kurulumu (`tx_seq=1`), `/persist/note.txt` CoW yazma/okuma, `/persist/alias.txt` `hardlink`, `vault-passwd dryos sovereign7` (`gen=2`), `Files` & `Settings` (`/persist/desktop.cfg` `layout=1 theme=0x1`), kriptografik `Session Lock` (`wrongpass` reddi `fails=1`, `sovereign7` açılışı `epoch=2`), ham disk düz-metin parola taraması (`0` eşleşme), Boot #2 yeniden bağlama (`fresh=false recovered=false`) | `2x_boot_persisted=true`, `pbkdf2_gen=2`, `lock_diff=868.726 px`, `pci_quiesce >= 1`, `leaked_frames = 0/0` (`11.09 s`) | **`PASS`** |
| **`p4b_powercut_and_superblock_recovery_e2e`** | `write /persist/atomic.txt cow-survived-powercut-77`, aktif Slot 0 süperblok bozma (`corrupt-sb0-test`) ve anında `SIGKILL` güç kesintisi; Boot #2'de `CRC32` kontrolüyle karşı yuvadan (`Slot 1`) otomatik kurtarma ve CoW veri doğrulaması | `sigkill_recovered=true` (`fresh=false tx_seq=5 recovered=true`), veri eksiksiz okundu, `leaked_frames = 0` (`6.40 s`) | **`PASS`** |
| **`p4c_vault_dma_devws_security_e2e`** | Kiralanmamış `DEV_WORKSPACE` yetki devri reddi (`-EPERM`), canlı `DEV_WORKSPACE` (`0x002a`) içinde `SYS_DMA_ALLOC` / `SYS_DEV_MAP` / `SYS_VAULT_CTL(ADVANCE_EPOCH)` / `/persist/vault.sec` okuma & `link` reddi (`-EPERM`), `GLOBAL_LEASE_EPOCH` $O(1)$ iptali (`-EREVOKED`), `SEAL_RO_EXPORT` `W^X` (`RW -> -EPERM`, `RO -> OK`), `INV-SEC-05` `ln /ram <-> /persist` (`-EXDEV`), canlı `dry-blk` `#UD` çöküşü -> `PCI Command.BME = 0` donanım susturma -> yeniden başlatma & remount | `dma_denied=2`, `pci_quiesce=2`, `vault_denied=1`, `lease_revoked=true`, `ro_export_wx=true`, `leaked_frames = 0` (`4.54 s`) | **`PASS`** |

---

## 3. Performans, Çekirdek Yığın Çerçevesi ve SMP/RAM Matrisi Ölçümleri

| Metrik / Sembol | Ölçülen Değer (`Milestone 2B`) | Mimari Açıklama |
| :--- | :---: | :--- |
| **`rust_syscall_dispatch` sıcak çerçeve (`objdump`)** | **`248 B` (`0xf8`, `+0 B` büyüme)** | Sıcak `SYSCALL` yolu (`send`/`recv`/`call`/`reply_recv`/`notify`) hiç değiştirilmedi (`R-11`) |
| **`sys_irq_ack` çerçevesi (`objdump`)** | **`40 B` (`0x28`, `+0 B` büyüme)** | Tek-atımlık `IOAPIC` maske açma yolu korundu |
| **`sys_cold_dispatch` soğuk çerçeve (`objdump`)** | **`264 B` (`0x108`)** | `SYS_DEV_MAP (29)`, `SYS_DMA_ALLOC (30)`, `SYS_VAULT_CTL (31)` ve `144 B` `UserSysInfo` ile 16 KiB yığın bütçesi içinde |
| **Masaüstü `in2comp` (`dry-input` -> `dry-display`)** | **`40.247 .. 49.198 cyc` (`p50`)** | `~12.9 .. 15.8 µs` (`9` eşzamanlı Ring 3 servisi aktifken: `+dry-blk`, `+dry-vault`) |
| **Tek satır Terminal sunumu (`ROW_PRESENT`)** | **`42.968 .. 50.761 cyc`** | `~13.8 .. 16.3 µs` (`25.312 px` kirli dikdörtgen) |
| **`--p3-matrix` `1..20 vCPU` / `256 MiB .. 2 GiB`** | **`5/5 PASS`, `leaked_frames = 0`** | `20 vCPU / 2 GiB`: `free_4k = 516.700`, `hlt_calls = 12.396` (`5.65 s`) |

---

## 4. Geliştirme Sırasında Kökten Yakalanıp Çözülen 4 Mühendislik Bulgusu

1. **Oturum Kapanışında `IoRing` Paylaşımlı Çerçeve Yaşam Döngüsü (`BLK_OP_DETACH_RING`):** `dry-init` oturumu sonlandırırken `dry-vfs` kapanıp paylaşımlı `IoRing` çerçevelerini iptal ettiğinde `dry-blk` bayat sanal adrese erişebiliyordu. Çözüm olarak açık `BLK_OP_DETACH_RING (123)` el sıkışması ve `dry-blk` uyandırma döngüsüne `sys_cap_query` canlılık bekçisi eklendi (`leaked_frames = 0`).
2. **`DEV_WORKSPACE` Kiralama Zorunluluğu (`INV-SEC-07` / `ADR-007` Kural 6):** `validate_narrowing_and_delegation`, `SecFlags::DEV_WORKSPACE` içine `lease_epoch == 0` (süresiz) yetki devrini `-EPERM` ile reddeder. `sup_audit.rs` önce bu reddi doğrular, ardından `sys_cap_derive` ile dönem-sınırlı (`lease_epoch = cur_epoch`) yetki türeterek kompartman kısıtlama testini yürütür.
3. **Konak `fdatasync` Gecikmesine Karşı 2-Kademeli `VirtioBlkHw::submit_sync` Beklemesi:** `DRYFS_v1` her taahhütte iki `VIRTIO_BLK_T_FLUSH` bariyeri gönderdiğinden konak Linux disk yazma yükü altında sabit `5.000.000` spin (~30 ms) zaman aşımına uğrayabiliyordu. İlk `4.096` hızlı `spin_loop()` sonrasında `5.0 sn` `rdtsc_user()` tavanlı periyodik `sys_yield()` beklemesine geçilerek hem düşük gecikme hem deterministik dayanıklılık sağlandı.
4. **Süpervizör Rozet Kapısı (`BADGE_SUPERVISOR`):** `dry-blk` içindeki `#UD` hata enjeksiyon kapısı yalnızca `BADGE_SUPERVISOR` rozetli uç noktadan kabul edildiğinden, denetim çağrısı rozetsiz istemci yetkisi yerine `*blk_sup_cap` üzerine bağlandı ve `quiesce_pci_for_cspace` (`BME=0`) canlı doğrulandı.
