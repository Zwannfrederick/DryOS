# DRY OS — Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`) Read-Only Hardware Inventory

> **Scope:** Project Ascension 5.0 Track C / M4 (`tools/hw_inventory_lenovo_15ihu6.py`)  
> **Raw JSON:** [`tools/compat_bench/lenovo_15ihu6_inventory.json`](../tools/compat_bench/lenovo_15ihu6_inventory.json)  
> **Host Safety Contract:** Strictly read-only sysfs/proc/lsblk/journalctl inspection (`0` `/dev/*` opens, `0` `/sys/*` writes, `0` `efivars`/`efibootmgr` touches, `0` `dd` calls).

---

## 1. Platform, CPU, Memory & Boot/Security State

| Component | Observed Host Value | DRY OS Relevance |
| :--- | :--- | :--- |
| Machine / Board | `LENOVO IdeaPad Gaming 3 15IHU6` (`82K1`, board `LNVNB161216`) | Target physical bare-metal laptop |
| BIOS / UEFI | `LENOVO H4CN22WW(V1.07)` (`11/18/2021`), `64-bit` UEFI | Limine `BOOTX64.EFI` Live USB boot target |
| CPU | `11th Gen Intel(R) Core(TM) i5-11300H @ 3.10GHz` (`4C/8T`, ucode `0xbe`) | `39-bit` phys / `48-bit` virt (`>4 GiB` BAR OK) |
| ISA / VMX Features | `x2apic, vmx, smap, smep, xsave, xsaveopt, avx2, avx512f, pdpe1gb, invpcid, constant_tsc, nonstop_tsc, umip` | `x2APIC`, `SMAP`, `XSAVEOPT`, `EPT/VPID` present |
| System RAM | `32634344 KiB` (`31.12 GiB`, `32 GiB` installed) | Sufficient for Ring-3 islands and `2 MiB` GOP/PRIME buffers |
| Serial / UART | `8250.nr_uarts=0` (`legacy_com1_absent = True`) | `kernel/src/serial.rs` LSR/scratch probe prevents UART hang |
| Secure Boot | `disabled` (via `journalctl -k -b (efivars and efibootmgr untouched)`; lockdown `[none] integrity confidentiality`) | Unsigned Limine `BOOTX64.EFI` can boot when disabled |
| Host `DMAR` / `VT-d` | ACPI `DMAR`: `present_root_only`, host `vtd_active = False` | DRY OS parses `DMAR` independently in Ring 0 (`kernel/src/iommu/`) |

---

## 2. Complete PCIe & VMD Topology (`len(pci_devices) = 24`)

| BDF | Vendor:Device | Class | Host Driver | Key BARs / >4G | Role / Notes |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `0000:00:00.0` | `0x8086:0x9a14` | `0x060000` | `unbound` | `none` | - |
| `0000:00:02.0` | `0x8086:0x9a49` | `0x030000` | `i915` | `BAR0:MMIO 16.0 MiB>4G, BAR2:MMIO 256.0 MiB>4G, BAR4:IO 64 B` | Intel Iris Xe iGPU (primary UEFI GOP console target; PCI_OBJ_IGPU) |
| `0000:00:04.0` | `0x8086:0x9a03` | `0x118000` | `proc_thermal` | `BAR0:MMIO 128.0 KiB>4G` | - |
| `0000:00:06.0` | `0x8086:0x09ab` | `0x088000` | `unbound` | `none` | - |
| `0000:00:08.0` | `0x8086:0x9a11` | `0x088000` | `unbound` | `BAR0:MMIO 4.0 KiB>4G` | - |
| `0000:00:0a.0` | `0x8086:0x9a0d` | `0x118000` | `intel_vsec` | `BAR0:MMIO 32.0 KiB>4G` | - |
| `0000:00:0e.0` | `0x8086:0x9a0b` | `0x010400` | `vmd` | `BAR0:MMIO 32.0 MiB>4G, BAR2:MMIO 32.0 MiB, BAR4:MMIO 1.0 MiB>4G` | Intel VMD controller (owns PCI domain 10000:e0..e1 NVMe + AHCI) |
| `0000:00:14.0` | `0x8086:0xa0ed` | `0x0c0330` | `xhci_hcd` | `BAR0:MMIO 64.0 KiB` | Intel Tiger Lake xHCI USB 3.2 controller (Live USB & USB HID; PCI_OBJ_XHCI) |
| `0000:00:14.2` | `0x8086:0xa0ef` | `0x050000` | `unbound` | `BAR0:MMIO 16.0 KiB>4G, BAR2:MMIO 4.0 KiB>4G` | - |
| `0000:00:14.3` | `0x8086:0xa0f0` | `0x028000` | `iwlwifi` | `BAR0:MMIO 16.0 KiB>4G` | Intel Wi-Fi 6 AX201 CNVi wireless adapter (iwlwifi) |
| `0000:00:15.0` | `0x8086:0xa0e8` | `0x0c8000` | `intel-lpss` | `BAR0:MMIO 4.0 KiB>4G` | Intel LPSS I2C controller #0 (owns Synaptics I2C-HID touchpad 06cb:ce78) |
| `0000:00:16.0` | `0x8086:0xa0e0` | `0x078000` | `mei_me` | `BAR0:MMIO 4.0 KiB>4G` | - |
| `0000:00:17.0` | `0x8086:0x09ab` | `0x088000` | `unbound` | `none` | - |
| `0000:00:1c.0` | `0x8086:0xa0bc` | `0x060400` | `pcieport` | `none` | - |
| `0000:00:1d.0` | `0x8086:0xa0b3` | `0x060400` | `pcieport` | `none` | - |
| `0000:00:1f.0` | `0x8086:0xa082` | `0x060100` | `unbound` | `none` | - |
| `0000:00:1f.3` | `0x8086:0xa0c8` | `0x040100` | `sof-audio-pci-intel-tgl` | `BAR0:MMIO 16.0 KiB>4G, BAR4:MMIO 1.0 MiB>4G` | Intel Smart Sound Technology HDA audio DSP (Realtek ALC257 + HDMI) |
| `0000:00:1f.4` | `0x8086:0xa0a3` | `0x0c0500` | `i801_smbus` | `BAR0:MMIO 256 B>4G, BAR4:IO 32 B` | - |
| `0000:00:1f.5` | `0x8086:0xa0a4` | `0x0c8000` | `intel-spi` | `BAR0:MMIO 4.0 KiB` | - |
| `0000:01:00.0` | `0x10de:0x25a0` | `0x030200` | `nvidia` | `BAR0:MMIO 16.0 MiB, BAR1:MMIO 4096.0 MiB>4G, BAR3:MMIO 32.0 MiB>4G, BAR5:IO 128 B` | NVIDIA RTX 3050 Ti Mobile GA107M (class 03:02 muxless dGPU; 4 GiB BAR1 >4G) |
| `0000:02:00.0` | `0x10ec:0x8168` | `0x020000` | `r8169` | `BAR0:IO 256 B, BAR2:MMIO 4.0 KiB, BAR4:MMIO 16.0 KiB` | Realtek RTL8111/8168 Gigabit Ethernet NIC (r8169; PCI_OBJ_NET target) |
| `10000:e0:06.0` | `0x8086:0x9a09` | `0x060400` | `pcieport` | `none` | - |
| `10000:e0:17.0` | `0x8086:0xa0d3` | `0x010601` | `ahci` | `BAR0:MMIO 8.0 KiB, BAR1:MMIO 256 B, BAR5:MMIO 2.0 KiB` | Intel Tiger Lake AHCI SATA controller (behind VMD 10000:e0:17.0) |
| `10000:e1:00.0` | `0x1344:0x5404` | `0x010802` | `nvme` | `BAR0:MMIO 16.0 KiB` | Micron MTFDHBA512QFD NVMe SSD (behind VMD domain 10000:e1:00.0) |

---

## 3. Storage, HID/I2C, Networking, Audio, Display & Firmware

- **Block `sda`:** model `KIOXIA-EXCERIA SATA SSD`, size `894,3G`, tran `sata`
- **Block `zram0`:** model `None`, size `4G`, tran `None`
- **Block `nvme0n1`:** model `Micron MTFDHBA512QFD`, size `476,9G`, tran `nvme`
- **I2C Node `i2c-MSFT0001:01`:** driver `i2c_hid_acpi`, modalias `acpi:MSFT0001:PNP0C50:`, path `/sys/devices/pci0000:00/0000:00:15.0/i2c_designware.0/i2c-1/i2c-MSFT0001:01`
- **HID `0003:048D:C963.0003`:** `ITE Tech. Inc. ITE Device(8295)` (`driver=hid-generic`, `phys=usb-0000:00:14.0-7/input1`)
- **HID `0003:048D:C966.0004`:** `ITE Tech. Inc. ITE Device(8176)` (`driver=hid-generic`, `phys=usb-0000:00:14.0-8/input0`)
- **HID `0003:1A81:2232.0001`:** `Lenovo Lenovo Gaming Mouse` (`driver=hid-generic`, `phys=usb-0000:00:14.0-1.2/input0`)
- **HID `0018:06CB:CE78.0005`:** `MSFT0001:01 06CB:CE78` (`driver=hid-multitouch`, `phys=i2c-MSFT0001:01`)
- **NIC `enp2s0`:** PCI `0000:02:00.0`, `operstate=down`
- **NIC `wlan0`:** PCI `0000:00:14.3`, `operstate=up`
- **Audio `card0/codec#0`:** `Realtek ALC257` (`0x10ec0257`)
- **Audio `card0/codec#2`:** `Intel Tigerlake HDMI` (`0x80862812`)
- **DRM `card2-eDP-1`:** status `connected`, mode `1920x1080`, EDID `128 B`
- **Firmware Blobs (Read-Only):** `15` Intel `tgl_*` files; `4` NVIDIA `615.78.08` blobs + `8` `ga107/gsp` blobs.

---

## 4. Follow-Up Subsystem Readiness Assessments

| Subsystem | Readiness Status | Finding | DRY OS Path / Next Action |
| :--- | :--- | :--- | :--- |
| `uart_serial` | `MITIGATED_IN_KERNEL` | Host cmdline sets 8250.nr_uarts=0; legacy COM1 0x3F8 is absent on 15IHU6. | kernel/src/serial.rs probes COM1 scratch/LSR != 0xff and buffers logs into 16 KiB DIAG_RING without stalling. |
| `display_and_gpu` | `GOP_READY_NATIVE_KMS_PLANNED` | 00:02.0 Iris Xe (8086:9a49, BAR2 256 MiB >4G) drives eDP-1 AUO 1920x1080; 01:00.0 RTX 3050 Ti (10de:25a0, class 03:02) is muxless with 4 GiB BAR1 >4G. | UEFI GOP + preserve_active_gop + 64-bit >4G WC BAR + VT-d SLPT verified in QEMU; native GuC/DMC/GSP modeset remains PLANNED (Gates 6-7). |
| `storage_vmd_and_usb` | `LIVE_USB_XHCI_READY_VMD_PLANNED` | 00:0e.0 Intel VMD (8086:9a0b) owns domain 10000:e0..e1 (10000:e1:00.0 Micron NVMe 1344:5404 + 10000:e0:17.0 AHCI). 00:14.0 xHCI (8086:a0ed) handles USB. | Live USB boots via UEFI + xHCI; native NVMe behind VMD requires VMD sub-bus ECAM remapping in pci_cfg.rs (PLANNED). |
| `i2c_hid_input` | `PS2_KBD_USB_HID_NEARTERM_I2C_PLANNED` | i8042 AT keyboard present; Synaptics touchpad 06cb:ce78 (MSFT0001:01) hangs off 00:15.0 LPSS I2C (8086:a0e8, 4 KiB >4G BAR, IRQ 27). | dry-input covers PS/2 i8042; Synaptics touchpad needs Ring-3 LPSS DesignWare I2C + I2C-HID island; xHCI USB mouse is immediate bare-metal path. |
| `networking` | `GBE_ISLAND_CANDIDATE_WIFI_DEFERRED` | 02:00.0 Realtek GbE (10ec:8168, r8169) has 256 B I/O + 4 KiB/16 KiB MMIO; 00:14.3 Intel Wi-Fi 6 AX201 (8086:a0f0, iwlwifi) requires CNVi + mac80211 stack. | 10ec:8168 fits Ring-3 Driver Island after 2C-3 igc/e1000e (PLANNED); 8086:a0f0 exceeds <10k LOC ADR-006 island ceiling (BLOCKED/LONG-TERM). |
| `audio` | `PLANNED_LONG_TERM` | 00:1f.3 Intel TGL SST (8086:a0c8) uses sof-audio-pci-intel-tgl with Realtek ALC257 and Intel HDMI codecs. | Requires SOF DSP firmware loader, NHLT/topology, and Ring-3 audio server (no audio stack in DRY OS yet). |
| `power_and_rtd3` | `S5_RESET_READY_S3_RTD3_PLANNED` | BAT1 (L20C3PC2), ACAD, s2idle/deep, and 01:00.0 d3cold_allowed=1 (currently D0 active on host). | ACPI S5 & FADT reset implemented; S3 returns -ENOSYS (R-16); 01:00.0 RTD3 D3cold power-off (~8-15W savings, R-17) is PLANNED for P5. |

---

## 5. QEMU/Validator Evidence vs. Bare-Metal Physical Gates (Gates 5-7)

| Gate | Scope | Current Verification State |
| :--- | :--- | :--- |
| **QEMU / Host Read-Only Prep** | `hw_inventory_lenovo_15ihu6.py`, `gpu_fw_validators.py --self-test`, `provision_local_firmware.py --self-test`, QEMU `--p8-only`/`--p12-only`, `build_live_usb_image.py --verify-boot` | **PASS** (read-only host inventory + synthetic/host firmware validators + QEMU/OVMF emulation) |
| **Gate 5 (Physical Live USB Boot)** | Boot `dryos-ascension4-live-usb.img` on physical `82K1` via `00:14.0` xHCI (`8250.nr_uarts=0`, GOP desktop + PS/2 or USB HID) | **`READY FOR USER PHYSICAL EXECUTION`** (not executed by agent) |
| **Gate 6 (Physical VT-d + Iris Xe GOP)** | Bare-metal `DMAR` `VT-d` enforcement with `00:02.0` `8086:9a49` `preserve_active_gop` untouched | **`READY FOR USER PHYSICAL EXECUTION`** (not executed by agent) |
| **Gate 7 (Physical dGPU RTD3 / PRIME)** | Bare-metal `01:00.0` `10de:25a0` `RTD3 D3cold` / GSP WPR2 / muxless PRIME `READ_ONLY_EXPORT` handoff | **`READY FOR USER PHYSICAL EXECUTION`** (`BLOCKED` for live OpenRM modeset until GSP RPC bring-up) |
