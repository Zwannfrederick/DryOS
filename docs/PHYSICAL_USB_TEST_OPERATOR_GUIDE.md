# DRY OS Ascension 7.0 — Physical USB Boot & Bare-Metal Operator Guide (`PHYSICAL_USB_TEST_OPERATOR_GUIDE.md`)

> [!IMPORTANT]
> **Strict Honesty Disclaimer:** All Live USB tests performed by this repository (`tools/build_live_usb_image.py --verify-boot`) executed inside **QEMU/KVM + OVMF (`intel-iommu`, `qemu-xhci`, `usb-storage`)**, NOT on physical bare metal. **QEMU boot success is NOT physical hardware boot success.** Bare-metal execution on Lenovo `82K1` remains a manual operator step (`hardware_verified: false`).

---

## 1. Target Platform Profile (`Lenovo IdeaPad Gaming 3 15IHU6` / `82K1`)

Verified read-only on the host via `python3 tools/hw_profile_lenovo_82k1.py` (`tools/compat_bench/results_lenovo_82k1_profile.json`, zero `/dev/*` mutations):

| Component | PCI BDF / Identifier | Observed Details | DRY OS Ascension 7.0 Boot Policy |
| :--- | :--- | :--- | :--- |
| **Platform / BIOS** | `LENOVO` `82K1` | BIOS `H4CN22WW(V1.07)`, 24 PCI functions | Vendor-neutral ACPI/MCFG/DMAR discovery (zero model-specific kernel hacks) |
| **Primary Display GPU** | `0000:00:02.0` (`8086:9a49`, class `030000`) | Intel Tiger Lake Iris Xe; BAR0 `16 MiB` (`0x6104000000`), BAR2 `256 MiB` (`0x4000000000`); `card2-eDP-1` `1920x1080` (`EDID 128 B`) | UEFI GOP framebuffer validated read-only (`MAX_FB_PAGES = 4096` covers `2,025` FHD pages); dedicated GFX DRHD deferred cleanly |
| **Discrete Muxless GPU** | `0000:01:00.0` (`10de:25a0`, class `030200`) | NVIDIA RTX 3050 Ti Laptop (GA107M); BAR1 `4 GiB` (`0x6000000000`) | **Kept Passive:** recorded in `BootPciFuncRecord`, never BAR-probed or bound (`dgpu_3d_passive=1`) |
| **Intel VMD & Internal NVMe** | `0000:00:0e.0` (`8086:9a0b`, class `010400`) -> `10000:e1:00.0` (`1344:5404`) | Internal NVMe sits inside VMD domain `10000` | **Kept Passive (`vmd_passive=1`):** DRY OS never probes/writes VMD root or internal NVMe |
| **USB 3.x Host Controller** | `0000:00:14.0` (`8086:a0ed`, class `0c0330`) | BAR0 `64 KiB` (`0x55180000`), IRQ 122 | Managed xHCI host driving Live USB BOT mass-storage (`DRYOS_PERSIST`) |
| **ACPI & IOMMU** | `/sys/firmware/acpi/tables/{DMAR,MCFG,APIC,FACP,HPET}` | All present; `boot/acpi.rs` parses `MCFG` before `DMAR`; `BOOT_RMRR_DID = 1` + dynamic DID `2..max_domains` | Fail-closed VT-d (`NoIommu` never enabled on `live_usb=1`); RMRR preserved for xHCI |

---

## 2. Live USB Artifact & Pre-Flight Verification

| Artifact | Path | Size | SHA-256 |
| :--- | :--- | :---: | :--- |
| **Ascension 7.0 Live USB Image** | `kernel/build/dryos-ascension7-live-usb.img` | `160 MiB` (`327,680` sectors) | `e1e136a9d13feb686cfa7e5f5e15706139877590d8d886875762977ab4f2f604` |
| *Preserved Baselines (Untouched)* | `kernel/build/dryos-ascension5-live-usb.img` / `ascension6` | `160 MiB` | Preserved intact |

Before flashing any USB stick on the host:

```bash
# 1. Verify image SHA-256 matches the QEMU-verified build
sha256sum kernel/build/dryos-ascension7-live-usb.img

# 2. Check UEFI Secure Boot status read-only (Limine BOOTX64.EFI is currently unsigned)
mokutil --sb-state 2>/dev/null || journalctl -k -b | grep -i "Secure boot"

# 3. Inspect USB stick identity ONLY (confirm RM=1, TRAN=usb, size != internal NVMe)
lsblk -o NAME,SIZE,RM,TRAN,MODEL,SERIAL,MOUNTPOINTS
ls -l /dev/disk/by-id/usb-*
```

---

## 3. Manual USB Flashing & Safe Boot Procedure

> [!CAUTION]
> Never automate flashing and NEVER pass `/dev/nvme*` or `/dev/sda` to `dd`. Double-check the `/dev/disk/by-id/usb-<EXACT-DEVICE>` symlink (whole raw USB disk, NOT `-part1`) and confirm `RM=1`.

1. Unmount any auto-mounted partitions on the target removable USB drive:
   ```bash
   lsblk -nr -o PATH /dev/disk/by-id/usb-<EXACT-USB-ID> | tail -n +2 | xargs -r umount
   ```
2. Write the image manually with synchronous flush:
   ```bash
   sudo dd if=kernel/build/dryos-ascension7-live-usb.img \
           of=/dev/disk/by-id/usb-<EXACT-USB-ID> \
           bs=4M status=progress oflag=sync
   sync
   ```
3. Reboot the Lenovo `82K1` laptop:
   - Press `F2` (or Novo button) if needed to verify **Intel VT-d = Enabled** and **Secure Boot = Disabled** inside UEFI Setup (only temporary/manual; do NOT run `efibootmgr`).
   - Open One-Time UEFI Boot Menu (`F12`) and select the USB stick (`DRYOS_ESP`).
4. Select Limine Profile **in this safety order**:
   - **First Trial (Minimum Risk):** Entry #3 — `Safe UEFI GOP Desktop (Read-Only, No GPU Island)` (`live_usb=1 desktop=1 storage=ro`). Zero USB writes, zero GPU driver islands, pure read-only UEFI GOP compositor.
   - **Second Trial (FHD Read-Only / Diagnostic):** Entry #4 (`diag_selftest=1 storage=ro`) or Entry #1/`2` (`desktop4=1 storage=rw` on `DRYOS_PERSIST` Partition 2 ONLY; ESP & internal NVMe stay isolated).

---

## 4. Expected Checkpoints, Failure Modes & Rollback

### Expected On-Screen / Diagnostic Checkpoints
- **Success (GOP Desktop):** Limine splash -> DRY OS desktop wallpaper & topbar rendered via validated UEFI GOP framebuffer; `/persist` (if RW) or volatile RAM (if RO) accessible; internal NVMe untouched (`vmd_passive=1`).
- **Expected Safe Failure (`panic_fb.rs`):** If bare-metal `DMAR` fails strict validation (e.g., unhandled BIOS scope or MMIO reservation mismatch), the kernel stays **fail-closed** (`InitVtd` refuses DMA & `BME=1` rather than disabling VT-d) and renders a high-contrast `[DRY-FB]` panic diagnostic screen directly onto the GOP framebuffer with the tail of the kernel log ring (`[DRY-IOMMU] UNIT ...`, `[DRY-PCI-TOPO] ...`).

### Risks & Instant Rollback
- **Risks:** Laptop lacks exposed COM1 DB9 UART (no serial capture without AMT/USB debug); unsigned `BOOTX64.EFI` won't boot unless Secure Boot is disabled; unsupported USB 3.x xHCI controller quirks on Tiger Lake PCH could leave `/persist` falling back to volatile RAM or timing out in `dry-usb`.
- **Zero-Damage Rollback:** Hold power button for `4 s` -> unplug USB stick -> power on normally. Because neither internal disks (`/dev/nvme0n1`), Windows/Linux ESP, nor UEFI NVRAM variables (`BootOrder`) are modified by the Live USB test, the host boots directly back into its primary OS unchanged. Re-enable Secure Boot in `F2` Setup if previously enabled.
