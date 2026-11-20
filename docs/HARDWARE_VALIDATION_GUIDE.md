# DRY OS Project Ascension 8.0 — Hardware Validation & Live USB Operator Guide (`HARDWARE_VALIDATION_GUIDE.md`)

> [!IMPORTANT]
> **Strict Honesty Disclaimer (`hardware_verified: false`):** All Live USB boots and tests in this repository (`tools/build_live_usb_image.py --hwval --verify-boot` and `tools/qemu_boot_test.py --p18-only`) ran inside **QEMU/KVM + OVMF (`intel-iommu`, `qemu-xhci`, `usb-storage`)**. **QEMU validation does not establish bare-metal hardware success.** Physical execution on the Lenovo `82K1` target remains a manual operator task (**0 `VERIFIED_HARDWARE` claims**).

---

## 1. Image Artifact & 3-Partition GPT Layout

Built and QEMU-verified via:

```bash
python3 tools/build_live_usb_image.py --build --hwval --verify-boot \
  --output kernel/build/dryos-ascension8-hardware-validation.img
```

| Field | Verified Value |
| :--- | :--- |
| **Output Path** | `kernel/build/dryos-ascension8-hardware-validation.img` |
| **Image Size** | `268,435,456` bytes (`256 MiB`, `524,288` x 512 B sectors) |
| **Full SHA-256** | `c267a74e7c0620f15018651134dfe2ae82518cc7d866515488b2fce1056e0a8f` |
| **Partition 1 (`DRYOS_ESP`)** | FAT32, `96 MiB` (`LBA 2048..198655`), holds `BOOTX64.EFI`, `limine.conf`, `dry-kernel`, `29` `.elf` modules (`29/32` slots), `DRYOS_LIVE_MANIFEST.json`. Untouched during diagnostics (`esp_and_persist_untouched=true`). |
| **Partition 2 (`DRYOS_PERSIST`)** | `DRYFS_V1`, `64 MiB` (`LBA 198656..329727`). Left read-only in safe validation profiles (`storage=ro`). |
| **Partition 3 (`DRYOS_LOGS`)** | Dedicated Linux-readable FAT32 (`4` sectors/cluster = `2 KiB/cluster`, `LBA 329728..524254`, `194,527` sectors) for `/hardware-tests/<session-id>/` reports. |
| **Disk GUID** | Embedded into `limine.conf` (`usb_disk_guid=<hex>`) so DRY OS verifies physical boot-disk ancestry before any log write. |

*Preserved historical baselines (`dryos-ascension4-live-usb.img`, `dryos-ascension5-live-usb.img`, `dryos-ascension6-live-usb.img`, `dryos-ascension7-live-usb.img`) remain untouched.*

---

## 2. Target Reference Hardware & Non-Invasive Policy (`Lenovo 82K1`)

Reference target hardware documented from passive host inventory ([PHYSICAL_USB_TEST_OPERATOR_GUIDE.md](PHYSICAL_USB_TEST_OPERATOR_GUIDE.md) and [HARDWARE_INVENTORY_LENOVO_15IHU6.md](HARDWARE_INVENTORY_LENOVO_15IHU6.md)):

- **CPU / Memory:** Intel Core i5-11300H (Tiger Lake-H), `32 GiB` RAM (UART-less chassis: `8250.nr_uarts=0`, `limine.conf` omits `serial: yes`, `DIAG_RING` + GOP panic screen active).
- **Integrated GPU:** Intel Iris Xe (`8086:9a49`), internal eDP `1920x1080` (`165 Hz` nominal). UEFI GOP kept passive/read-only (`ALLOW_PHYSICAL_GPU_WRITES = false`, `PARTIAL`).
- **Discrete GPU:** NVIDIA RTX 3050 Ti Laptop (`10de:25a0`, GA107M muxless). Kept passive in default profiles; explicit `nvidia_exp=1` launches isolated `dry-nv-island.elf` (`Level 1 — DRIVER ISLAND READY`, fail-closed `BLOCKED`, `0` MMIO/DMA).
- **Internal Storage:** Intel VMD (`8086:9a0b`) + internal NVMe (`vmd_passive=1`). **Never mounted or written during default hardware validation (`STOR-01-INTERNAL-RO`).**
- **Boot Media & IOMMU:** UEFI firmware + Intel VT-d `DMAR` + PCH xHCI USB (`DRYOS_LOGS` writes gated on all `5` xHCI/GPT/GUID ancestry gates `0x1F`).

---

## 3. Limine Boot Profiles & Kernel Command-Line Flags

| # | Limine Entry Title | Resolution | Kernel Cmdline Flags | Behavior |
| :- | :--- | :---: | :--- | :--- |
| 1 | `DRY OS v0.8.0 - SAFE AUTO CHECK (Default, Passive)` | `1280x800x32` | `live_usb=1 desktop=1 storage=ro hwcheck=auto hwcheck_mode=safe usb_disk_guid=<hex>` | Default passive diagnostics at boot, RAM buffering, verified FAT32 write to `DRYOS_LOGS` Partition 3 (`20 pass, 0 fail, 2 skip, 2 blocked` on QEMU). `dry-nv-island` stays unspawned. |
| 2 | `DRY OS v0.8.0 - DESKTOP SAFE MODE (GOP Only, No GPU Driver Init)` | `1280x800x32` | `live_usb=1 desktop=1 storage=ro hwcheck_mode=desktop_safe usb_disk_guid=<hex>` | Normal UEFI GOP desktop without running GPU driver initialization or automatic export. |
| 3 | `DRY OS v0.8.0 - EXTENDED DIAGNOSTICS (Bounded, Non-Destructive)` | `1280x800x32` | `live_usb=1 desktop=1 storage=ro hwcheck=auto hwcheck_mode=extended usb_disk_guid=<hex>` | Runs additional bounded non-destructive checks upon explicit operator selection. |
| 4 | `DRY OS v0.8.0 - RECOVERY / LOG VIEWER (No Hardware Init Tests)` | `1280x800x32` | `live_usb=1 desktop=1 storage=ro hwcheck_mode=recovery usb_disk_guid=<hex>` | Read-only inspection of existing `DRYOS_LOGS` sessions (`[DRY-HWDIAG] RECOVERY_SESSIONS`); executes zero hardware init tests and zero USB writes. |
| 5 | `DRY OS v0.8.0 - NVIDIA DRIVER EXPERIMENTAL (MicroVM Level 1 Gated)` | `1280x800x32` | `live_usb=1 desktop=1 storage=ro hwcheck=auto hwcheck_mode=nvidia_exp nvidia_exp=1 usb_disk_guid=<hex>` | Opt-in boot profile that spawns isolated `dry-nv-island.elf` (`SecDomain::Userspace`), evaluates `14` fail-closed prerequisites, and exports `nvidia_exp/` logs when safe. |

Backward compatibility with existing Ascension `5.0`/`6.0`/`7.0` flags (`live_usb=1`, `desktop=1`, `desktop4=1`, `storage=ro|rw`, `diag_selftest=1`, `lx_drm=1`, `console=com1`) is preserved.

---

## 4. Safe Manual USB Flashing Procedure (Operator Execution Only)

> [!CAUTION]
> **NEVER** flash automatically in scripts and **NEVER** target `/dev/nvme*` or `/dev/sda`. Always verify `RM=1` and `TRAN=usb` and write strictly via `/dev/disk/by-id/usb-<EXACT-DEVICE>`.

1. **Verify image checksum before writing:**
   ```bash
   sha256sum kernel/build/dryos-ascension8-hardware-validation.img
   # Must match: c267a74e7c0620f15018651134dfe2ae82518cc7d866515488b2fce1056e0a8f
   ```
2. **Inspect USB stick identity & Secure Boot read-only:**
   ```bash
   lsblk -o NAME,SIZE,RM,TRAN,MODEL,SERIAL,MOUNTPOINTS
   ls -l /dev/disk/by-id/usb-*
   mokutil --sb-state 2>/dev/null || journalctl -k -b | grep -i "Secure boot"
   ```
3. **Unmount auto-mounted partitions and write image synchronously:**
   ```bash
   lsblk -nr -o PATH /dev/disk/by-id/usb-<EXACT-USB-ID> | tail -n +2 | xargs -r umount
   sudo dd if=kernel/build/dryos-ascension8-hardware-validation.img \
           of=/dev/disk/by-id/usb-<EXACT-USB-ID> \
           bs=4M status=progress oflag=sync
   sync
   ```
4. **Boot Target Machine:**
   - Because `BOOTX64.EFI` is unsigned, temporarily set Secure Boot to **Disabled** and ensure **Intel VT-d = Enabled** in Lenovo UEFI Setup (`F2` / Novo button) if required — do **not** run `efibootmgr`.
   - Select the USB stick (`DRYOS_ESP`) from the One-Time Boot Menu (`F12`).
   - Start with Entry #1 (`SAFE AUTO CHECK`) or Entry #2 (`DESKTOP SAFE MODE`).

---

## 5. Retrieving Hardware Validation Reports on Linux

After shutting down DRY OS cleanly and rebooting into Linux, inspect and extract reports read-only using [tools/retrieve_dryos_logs.py](../tools/retrieve_dryos_logs.py):

```bash
# 1. Discover attached DRYOS_LOGS partition read-only without mounting anything
python3 tools/retrieve_dryos_logs.py --list-candidates

# 2. Option A: Extract & SHA-256 verify directly from an image file or raw USB partition (no mount needed)
python3 tools/retrieve_dryos_logs.py \
  --image /dev/disk/by-id/usb-<EXACT-USB-ID> \
  --out ./dryos_hw_report_bundle

# 3. Option B: Mount DRYOS_LOGS strictly read-only manually, then verify & copy
sudo mkdir -p /mnt/dryos_logs
sudo mount -o ro,umask=022 /dev/disk/by-id/usb-<EXACT-USB-ID>-part3 /mnt/dryos_logs
python3 tools/retrieve_dryos_logs.py --mount-dir /mnt/dryos_logs --out ./dryos_hw_report_bundle
sudo umount /mnt/dryos_logs
```

`retrieve_dryos_logs.py` runs `fsck.fat -n` (read-only), validates all `13` required session files, verifies every SHA-256 digest in `manifest.sha256`, confirms raw disk GUID redaction in `usb_storage.json`, and writes `bundle_verification.json`.

---

## 6. Expected Failure Behavior & Instant Rollback

- **5-Gate USB Ancestry Block:** If any ancestry gate (`0x1F`) fails (missing xHCI MSC, invalid GPT CRC, wrong `usb_disk_guid`, missing ESP on same disk, missing Partition 3) or if FAT32 is corrupt/full/RO, persistence stays **`BLOCKED`** (`LOG_BLOCK_*`) and diagnostic results remain safely buffered in RAM and viewable via `dry report` without ever falling back to NVMe/SATA.
- **No-IOMMU / Bad Firmware:** DRY OS stays fail-closed (`IOMMU-01-VTD-DMA` `BLOCKED` or kernel panic to GOP `panic_fb.rs` without unsafe DMA).
- **Zero-Damage Rollback:** Hold power button for `4 s` -> unplug USB stick -> power on normally. Because internal disks (`NVMe`/`VMD`), host ESP, and UEFI NVRAM are never written, the host OS boots back unchanged.
