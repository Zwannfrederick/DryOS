# DRY OS — Live USB Operator Guide (`Project Ascension 5.0`, `v0.5.0`)

> **Target Image:** `kernel/build/dryos-ascension5-live-usb.img` (`160 MiB` 2-partition GPT, `27` Ring-3 modules)  
> **Safety Contract:** Never write to internal NVMe/SATA drives (`/dev/nvme*`, `/dev/sda`). Write **only** to a verified removable USB block device (`/dev/disk/by-id/usb-*`).

---

## 1. Building and QEMU-Verifying the Live USB Image

From the repository root:

```bash
# Build dry-kernel + 27 Ring-3 ELFs (if missing) and assemble the 160 MiB GPT Live USB image
python3 tools/build_live_usb_image.py --build

# Or assemble and run automated QEMU OVMF + qemu-xhci + usb-storage RW boot verification
python3 tools/build_live_usb_image.py --verify-boot
```

`tools/build_live_usb_image.py` formats a standalone `96 MiB` FAT32 Partition 1 (`DRYOS_ESP`, LBA `2048..198655`) and a `DRYFS_V1` Partition 2 (`DRYOS_PERSIST`, LBA `198656..327646`, `128,991` sectors), verifies Protective MBR (`0xEE`), Primary/Backup GPT CRC32s, `mdir` contents (`BOOTX64.EFI`, `limine.conf`, `dry-kernel`, `27` `.elf` modules, `DRYOS_LIVE_MANIFEST.json`), and Partition 2 dual A/B `DRYFS_V1` superblocks.

---

## 2. Inspecting Removable USB Devices Safely on Linux

Always identify the exact removable USB drive **before** running any write command:

```bash
# 1. Inspect transport, removable flag (RM=1), model, size, and existing partitions
lsblk -o NAME,SIZE,RM,TRAN,MODEL,SERIAL,MOUNTPOINTS

# 2. Resolve persistent USB-only symlink (never guess /dev/sdX blindly)
ls -l /dev/disk/by-id/usb-*
```

Confirm all four checks before proceeding:
1. `TRAN` is `usb` and `RM` is `1`.
2. Size and `MODEL`/`SERIAL` match your physical USB flash drive (not your internal NVMe SSD such as `nvme0n1` or internal SATA SSD).
3. You are targeting the **whole raw USB device** (for example `/dev/disk/by-id/usb-Vendor_Model-0:0`), not a partition (`-part1`) and never `/dev/nvme*` or `/dev/sda`.
4. Unmount any auto-mounted partitions on that USB drive first:

```bash
lsblk -nr -o PATH /dev/disk/by-id/usb-<EXACT-USB-ID> | tail -n +2 | xargs -r umount
```

---

## 3. Writing the Image to Removable USB

Once the USB device path is verified:

```bash
sudo dd if=kernel/build/dryos-ascension5-live-usb.img \
        of=/dev/disk/by-id/usb-<EXACT-USB-ID> \
        bs=4M status=progress oflag=sync
sync
```

Do not unplug the USB drive until `sync` exits cleanly. No host UEFI variables or partition tables outside that removable USB stick are touched.

---

## 4. Non-Destructive Secure Boot Check and UEFI Boot

DRY OS uses Limine `BOOTX64.EFI`, which is currently **unsigned** for UEFI Secure Boot.

1. **Inspect Secure Boot state read-only** (never auto-modify BIOS or write to `/sys/firmware/efi/efivars` / `efibootmgr`):
   ```bash
   mokutil --sb-state
   # Read-only kernel log fallback if mokutil is not installed:
   journalctl -k -b | grep -i "Secure boot"
   ```
2. If Secure Boot is enabled, disable it manually inside your laptop's UEFI Setup utility (`F2` or Novo button on Lenovo IdeaPad Gaming 3 15IHU6 `82K1`).
3. Insert the USB drive, reboot, open the UEFI Boot Menu (`F12` on Lenovo `82K1`), and boot the UEFI USB device (`DRYOS_ESP`).
4. Note on UART-less laptops: `live_usb_limine.conf` intentionally omits `serial: yes` so Limine never stalls polling `0x3F8` on systems with `8250.nr_uarts=0`.

---

## 5. Limine Boot Profiles (`timeout: 2`)

| # | Profile Title | Resolution | Kernel Cmdline | Behavior & Storage Fence |
| :- | :--- | :---: | :--- | :--- |
| 1 | `DRY OS v0.5.0 - Live Desktop (1280x800, USB Persist RW, Default)` | `1280x800x32` | `live_usb=1 desktop4=1 storage=rw` | Default fast boot (`live_usb=1` skips synthetic burn-ins); mounts `DRYOS_PERSIST` Partition 2 RW as `/persist`. |
| 2 | `DRY OS v0.5.0 - Live FHD Desktop (1920x1080, USB Persist RW)` | `1920x1080x32` | `live_usb=1 desktop4=1 storage=rw` | Native `1920x1080` panel mode (matches `15IHU6` `eDP-1`) with RW Partition 2 `/persist`. |
| 3 | `DRY OS v0.5.0 - Safe UEFI GOP Desktop (Read-Only, No GPU Island)` | `1280x800x32` | `live_usb=1 desktop=1 storage=ro` | Minimal UEFI GOP compositor (`safe_gop`/desktop1 path); `storage=ro` blocks all `/persist` writes (`-EROFS`). |
| 4 | `DRY OS v0.5.0 - Diagnostic & Hardware Self-Test (Read-Only)` | `1280x800x32` | `console=com1 diag_selftest=1 desktop4=1 storage=ro` | Runs full kernel scheduler/PMM self-tests + COM1 serial output (`probe_com1_uart` safe); `storage=ro` protects USB. |
| 5 | `DRY OS v0.5.0 - Linux v6.18 Driver Island Audit (Read-Only)` | `1280x800x32` | `console=com1 desktop4=1 lx_drm=1 storage=ro` | Runs Linux `v6.18` Driver Island audit mode (`session_mode=29`) with read-only storage (`storage=ro`). |

---

## 6. `DRYOS_PERSIST` (`DRYFS_V1`) & Hardware Safety Fences

- **Partition 1 Protection (`INV-DISK-SAFE-02`):** `dry-usb` only serves LBA `198656..327646` (Partition 2 `DRYOS_PERSIST`, verified `*b"DRYFS_V1"` on block 0/1) over `IoRingSharedPage`. Writes outside Partition 2 or writes while `storage=ro` (`BOOT_FLAG_STORAGE_RO`) is set are rejected with `-EROFS` / `-EIO_RING`. LBA `0..33` (GPT) and Partition 1 (`DRYOS_ESP`) are never written at runtime.
- **Internal NVMe Protection (`INV-DISK-SAFE-01`):** `dry-nvme` compiles only Admin `IDENTIFY (0x06)`, `CREATE_IO_CQ (0x05)`, `CREATE_IO_SQ (0x01)`, and NVM `READ (0x02)` — zero write/format/trim opcodes exist. On Intel VMD systems (`8086:9a0b`, e.g. `15IHU6`), the VMD domain stays gated and internal NVMe disks remain untouched.
- **Persistence Behavior:** In Profiles 1–2 (`storage=rw`), `/persist` saves `/persist/boot.log` (`sys_dmesg_read`, up to `4,096 B`), `/persist/desktop.cfg` (layout/theme/DPI preferences), `/persist/vault.sec` (`PBKDF2-HMAC-SHA256`), and user files across reboots. In Profiles 3–5 (`storage=ro`), `/persist` mounts read-only and refuses mutations (`-30` `EROFS`).

---

## 7. Diagnostics: `/persist/boot.log`, `dmesg`, and GOP Panic Console

1. **In-Memory `16 KiB` `DIAG_RING` (`kernel/src/serial.rs`):** Every kernel log message is recorded into `DIAG_RING` regardless of whether a physical `COM1` UART exists (`probe_com1_uart` checks scratch register `0x5a`/`0xa5` and `LSR != 0xff` so UART-less laptops never stall).
2. **Reading Logs in Ring 3 (`Terminal`):**
   - Run `dmesg` to read `DIAG_RING` via `SYS_SYSINFO` (displays the newest 40 lines).
   - Run `cat /persist/boot.log` to inspect the persisted boot log saved by `dry-init` (`sup_hw_services.rs`).
   - Run `lspci`, `lsusb`, `lsblk`, `nvme`, `drvctl`, `mounts`, `free`, `uname` for live hardware and isolation telemetry.
3. **UEFI GOP Fatal Panic Console (`kernel/src/panic_fb.rs`):** If a kernel panic or fatal CPU exception occurs without a serial cable, `render_fatal_screen` paints a high-contrast slate/crimson diagnostic banner directly onto the UEFI GOP framebuffer (`hhdm_virt` early, `KERNEL_GOP_DIAG_BASE` post-`CR3` handover) with the panic header and `DIAG_RING` log tail.
