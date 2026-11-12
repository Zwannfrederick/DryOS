# DRY OS — `Milestone 2A-3` Deneysel Ölçümler, SMP/RAM Matrisi ve ACPI S3 Boşluk Denetimi (`docs/EXPERIMENTS_2A3.md`)

> **Tarih:** 12 Kasım 2026  
> **Referans Commit Zinciri:** `0da9d9d` (`P3-A`), `ce61fea` (`P3-B`), `58ccda6` (`P3-C`)  
> **Ortam:** QEMU/KVM `q35` + OVMF UEFI (`~3.111..3.115 MHz` TSC, `4C/8T` konak; `-smp 20` koşusu konak üzerinde `2.5x` CFS aşırı-aboneliğidir)

---

## 1. Tekrarlanabilir Doğrulama Komutları

```bash
# Yalnızca Milestone 2A-3 uçtan uca QMP + güvenlik + güç yaşam döngüsü paketi (6/6)
python3 tools/qemu_boot_test.py --p3-only

# Milestone 2A-3 SMP (1/2/4/8/20 vCPU) ve RAM (256M/512M/1G/2G) donanım matrisi (5/5)
python3 tools/qemu_boot_test.py --p3-matrix

# Tam deterministik regresyon paketi (56 Faz 0-1C + 9 P1 + 14 P2 + 6 P3 = 85/85)
python3 tools/qemu_boot_test.py
```

---

## 2. Masaüstü Açılış, Uygulama Başlatma, Çizim ve Çekirdek Bellek/Yığın Bütçesi (`2 vCPU`)

| Metrik / Sembol | Ölçülen Değer | Mimari Açıklama |
| :--- | :---: | :--- |
| **Boot -> Interactive Desktop (`boot_ms`)** | **`229 .. 235 ms`** | Limine -> `dry-kernel` öz-testleri -> `dry-init` -> `dry-vfs`, `dry-power`, `dry-display`, `dry-input`, `dry-shell`, `dry-sysmon` (`7` canlı Ring 3 süreci) |
| **`in2comp` (`dry-input` TSC -> `dry-display` WM)** | **`48.981 / 57.237 / 63.310 cyc`** (`p50/p95/p99`) | **`~15.7 µs` (`p50`)**: 4-yazmaçlı IPC + `WindowManager::handle_input_event` |
| **`cur` (`12x16` / `24x32` yazılımsal imleç MMIO)** | **`11.646 .. 20.637 cyc`** (`p50`) | **`~3.7 .. 6.6 µs`**: `WB` arka tampon imleçsiz tutulur; yalnızca kirli dikdörtgen GOP MMIO'ya kopyalanır |
| **Tek satır Terminal güncellemesi (`ROW_PRESENT`)** | **`38.925 .. 43.384 cyc`** | **`~12.5 .. 13.9 µs`** (`25.312 px` kirli şerit) |
| **Tam pencere / çoklu pencere sunumu (`pres2w`)** | **`42.677` (`p50` satır) / `362.416 .. 407.521` (`tam`)** | **`~13.7 µs` satır / `~116 .. 131 µs` tam pencere** (`213.472 px`) |
| **Çekirdek `.bss` tasarrufu (`pmm/tests.rs`)** | **`-512 KiB` (`1.782 -> 1.654` meta çerçevesi)** | Sabit `OOM_HANDLES[65_536]` kaldırılıp intrüzif HHDM sayfa listesine taşındı |
| **Çekirdek yığın çerçeveleri (`objdump -d`)** | **`248 B` (`0xf8`) / `216 B` (`0xd8`) / `40 B` (`0x28`)** | `rust_syscall_dispatch` / `sys_cold_dispatch` / `sys_irq_ack` (**`+0 B` büyüme**) |

---

## 3. SMP (`1..20 vCPU`) ve Fiziksel RAM (`256 MiB .. 2 GiB`) Masaüstü Matrisi (`--p3-matrix`)

| Senaryo | vCPU | RAM | Serbest `4 KiB` Çerçeve (`free_4k`) | `HLT` Boşta Çağrısı (`hlt_calls`) | Süreç (`ps`) | `leaked_frames` | Süre |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| `p3_matrix_1cpu_256m` | `1` | `256 MiB` | `60.891` (`~237.8 MiB`) | `622` | `7` | **`0`** | `4.94 s` |
| `p3_matrix_2cpu_256m` | `2` | `256 MiB` | `60.883` (`~237.8 MiB`) | `1.266` | `7` | **`0`** | `3.97 s` |
| `p3_matrix_4cpu_512m` | `4` | `512 MiB` | `126.019` (`~492.3 MiB`) | `2.427` | `7` | **`0`** | `2.89 s` |
| `p3_matrix_8cpu_1g` | `8` | `1 GiB` | `256.291` (`~1.001 MiB`) | `4.857` | `7` | **`0`** | `4.23 s` |
| `p3_matrix_20cpu_2g` | `20` (`*`) | `2 GiB` | `516.802` (`~2.018 MiB`) | `12.325` | `7` | **`0`** | `8.35 s` |

> `(*)` `4C/8T` konakta `-smp 20` koşusu eşzamanlılık, `VSpace` TLB shootdown, `PMM` `2 GiB` OOM geri kazanımı ve `VEC_RESCHED_WAKE` `HLT` uyandırma doğruluğunu kanıtlar; fiziksel 20 çekirdek performans iddiası değildir (`R-05`, `R-09`).

---

## 4. Güç ve Oturum Yaşam Döngüsü Doğrulaması (`P3-C`)

| Alt Sistem | Kanıt / Seri & QMP Gözlemi | Sonuç |
| :--- | :--- | :---: |
| **`CPU Idle` (`sti; hlt` + `VEC_RESCHED_WAKE = 0xed`)** | `p3c_idle_dpms_lock_s3_e2e` oturumunda **`16.209` gerçek `HLT` girişi**; çapraz-CPU IPC/IRQ uyandırmalarında `lost_wakeups = 0` | **`PASS`** |
| **Manuel `DPMS` Karartma & Anlık Uyanma** | `blank` komutu sonrası QMP `screendump`: **`1.024.000 / 1.024.000` saf `0x000000` piksel**; tek fare hareketinde `DPMS_WAKE wakes=1` ve **`1.024.000 px`** tam sahne geri dönüşü | **`PASS`** |
| **Otomatik `5 sn` Hareketsizlik `DPMS` Karartması** | `Life Station` arka planda her `250 ms`'de `SURFACE_PRESENT` gönderirken dahi `last_input_tsc` etkilenmeden **`5.0 sn`** sonunda `DPMS_BLANK reason=idle px=1024000` | **`PASS`** |
| **`Session Lock` Kalkanı (`docs/p3c_lock_shield.png`)** | `lock` sonrası arka plan pencereleri gizlendi; yazılan gizli tuşlar uygulama kuyruğuna sızmadı (`LOCK_BLOCKED count=3`); `[ UNLOCK SESSION ]` tıklaması sonrası `echo clean after lock` doğrulandı | **`PASS`** |
| **Gerçek Donanım `ACPI S5` Kapanış (`isa-debug-exit` KAPALI)** | Kontrollü süreç kapatma (`OWNER_REAPED active=0`) -> `dry-power` -> `SYS_POWER_CTL` -> `PM1a_CNT_BLK = 0x604`, `SLP_TYPa = 0`, `SLP_EN = 1 << 13` -> **QEMU donanım kapanışı `rc=0` (`1.88 s`)** | **`PASS`** |
| **Gerçek Donanım `FADT` Yeniden Başlatma (`isa-debug-exit` KAPALI, `-no-reboot`)** | Kontrollü süreç kapatma -> `FADT.RESET_REG = 0xcf9` <- `RESET_VALUE = 0x0f` -> **QEMU q35 yonga seti sıfırlaması `rc=0` (`3.61 s`)** | **`PASS`** |
| **Yetkisiz Güç & Malformed IPC Reddi (`p3a_security_and_malformed_ipc_e2e`)** | `Userspace` doğrudan `SYS_POWER_CTL` -> `EPERM` (`pwr_eperm=true`); sahte süpervizör aksiyonları `sup_unauth=4`; `/boot` yazma `vfs_rofs=1`; çapraz kompartman `/ram` okuma/silme `vfs_unauth=2` | **`PASS`** |

---

## 5. `Suspend-to-RAM (ACPI S3)` Teknik Boşluk Denetimi (`ADR-014` & `INV-APP-06`)

Önyükleme sırasında `acpi.rs` gerçek OVMF/q35 tablolarını denetlemiştir:
- **`[DRY-ACPI-PWR]`:** `fadt=true`, `pm1a=0x604`, `s5=true (slp_typ_a=0)`, `reset_port=0xcf9 (val=0x0f)`, `s3_aml=true`, `facs=0xfeca000` (`256M`) / `0x1feb2000` (`512M`).
- **Neden `S3` Henüz Etkinleştirilmedi (`SysError::NoSys = -38`):**
  1. **16-Bit Gerçek Mod Uyanma Trampolini (`FACS.FirmwareWakingVector`):** ACPI S3 uyanışında yonga seti BSP'yi 64-bit Long Mode'da değil, `< 1 MiB` fiziksel adresteki (`CS:IP`) **16-bit Real Mode**'da başlatır. `LOW_MEM_GUARD_END_PFN` (`< 64 KiB`) altında korumalı alan mevcuttur; ancak `16-bit -> 32-bit Protected -> 64-bit Long Mode` sayfa tablosu (`KERNEL_VSPACE CR3`), `EFER.LME/NXE`, `CR4.PAE/SMEP/SMAP`, `GDT/TSS/IDT` ve `GS_BASE` geri yükleme assembly trampolini henüz yazılmamıştır.
  2. **SMP İkincil Çekirdek (`AP`) Yeniden Girişi:** Limine'in önyüklemede kullandığı AP başlatma bağlamı `BootloaderReclaimable` geri kazanımıyla serbest bırakılmıştır; S3 dönüşünde çekirdeğin kendi `INIT-SIPI-SIPI` dizisini yürütüp her AP'de `x2APIC`/`xAPIC`, `SYSCALL` MSR'ları ve `PerCpuData`'yı yeniden kurması gerekir.
  3. **Cihaz & UEFI GOP Durumu:** OVMF `S3 BootScript` olmadan doğrudan `PM1a_CNT_BLK` S3 yazımı sonrasında `IOAPIC` yönlendirme girişleri, `i8042` PS/2 fare akış kipi ve doğrusal GOP çerçeve tamponu yeniden ilklendirme gerektirir.
- **Karar:** Siyah ekran çizerek veya sanal makineyi dondurarak sahte `S3` göstermek mühendislik ilkelerine aykırı olduğundan, `suspend` komutu ve `POWER_OP_SUSPEND_S3` bu boşluğu açıkça raporlayarak deterministik `-ENOSYS` döner.

---

## 6. Doğrulama Sırasında Yakalanıp Kökten Çözülen 3 Gerçek Sistem Hatası

1. **`apic.rs` PIT `0x61` PIO VM-Exit Preemption Alt-Sayımı (`20 vCPU`):** `outb(0x61, GATE2=1)` çağrısından *sonra* `TIMER_INIT` ve `rdtsc()` okunması, konak CFS zamanlayıcısı `outb(0x61)` PIO çıkışında BSP'yi ~4.9 ms askıya aldığında PIT sayacının erken tükenmesine (`tsc_mhz=18`) yol açıyordu. `TIMER_INIT = u32::MAX` ve `t0 = rdtsc()` işlemleri `GATE2=1` yükseltilmeden *önceye* alınarak alt-sayım matematiksel olarak imkânsız hâle getirildi.
2. **`vspace/tlb.rs` 20-vCPU Aşırı-Abonelik `TLB_REQ_LOCK` Bekçisi:** Katı FIFO ticket kilidin KVM aşırı-aboneliğinde vCPU konvoy gecikmesi ürettiği deneysel olarak doğrulandı ve geri alındı; yerine önbellek-dostu **TTAS `AtomicBool` + `NEXT_REQ_GEN` ilerleme-duyarlı zaman aşımı** konularak hem konvoy gecikmesi hem de sahte zaman aşımı panikleri elendi.
3. **`pmm/tests.rs` `> 256 MiB` RAM OOM Öz-Test Tavanı:** `--p3-matrix` `512M/1G/2G` koşusunda sabit `MAX_OOM_FRAMES = 65_536` dizisi panik üretti. Dizi tamamen silinip (`-512 KiB` `.bss`) tahsis edilen her `4 KiB` HHDM sayfasının ilk 8 baytında intrüzif bağlantı kurularak `2 GiB` (`516.802` çerçeve) dahil keyfi RAM boyutlarında `%100` geri kazanım doğrulandı.
