# DRY OS — Project Ascension 5.0 (`v0.5.0`) Validation Matrix (Gates 1–7)

> **Epistemic Honesty Rule:** DRY OS strictly separates **Gate 1–2** verification (offline builds, unit tests, QEMU/OVMF emulation, and strictly read-only host inspection/validators) from **Gates 3–7** (bare-metal physical execution on **Lenovo IdeaPad Gaming 3 15IHU6 `82K1`**). Emulated QEMU/OVMF passes never certify physical silicon IOMMU, physical xHCI quirks, or physical GPU modesetting.

---

## 1. Gate 1 & Gate 2 — Verified (`PASS`)

### Gate 1: Offline Builds, Static Hygiene, License Boundary & Asset Pipeline

| Check ID | Command / Artifact | Observed Evidence & Status |
| :--- | :--- | :--- |
| `G1-BUILD` | `python3 tools/qemu_boot_test.py --build-only` | **PASS** — `cargo clippy --offline -- -D warnings` + release ELF64 build clean for `dry-kernel`, `24` native userspace ELFs (`27/32` total modules), `dry-lx-rng`, `dry-lx-drm-bochs`, `dry-lx-net`. |
| `G1-FRAME` | `objdump -d kernel/.../dry-kernel` (`rust_syscall_dispatch`) | **PASS** — Hot syscall dispatch frame = **`248 B` (`sub $0xf8,%rsp` `<= 256 B`)**; `32` syscalls (`0..=31`); `0` GPL lines in `kernel/`. |
| `G1-ASSETS` | `python3 tools/build_vector_assets.py --verify` + `rustc -D warnings` | **PASS** — `118` SVGs (`6` brand, `21` apps, `91` system), `vector_assets_sys.rs` (`577` lines, `1227` shapes), `vector_assets_apps.rs` (`661` lines, `1712` shapes), `4` PNG contact sheets. |
| `G1-LX-HASH` | `python3 tools/lx_compat/lx_driver_manager.py verify --all` & `test_lx_driver_manager.py` | **PASS** — `56/56` upstream Linux `v6.18`/`v6.12` source files SHA-256 verified (`virtio-rng.c`, `bochs.c`, `8139cp.c` + `mii.c`, `52` `igc`); `5/5` Python unit tests OK; `.ko` rejected (`KO_BINARY_REJECTED`). |
| `G1-HYGIENE` | Pragmatic line limits & emoji/safety audit | **PASS** — `489` scanned files all `< 1,000` lines (`kernel/src/iommu/vtd.rs` `768`, `slpt.rs` `406`), `0` emojis, `0` GPL/Linux lines in `kernel/`, `0` NVMe write opcodes (`INV-DISK-SAFE-01`). |

### Gate 2: QEMU/OVMF Emulation, Live USB & Read-Only Host Audits

| Check ID | Command / Target | Observed Evidence & Status |
| :--- | :--- | :--- |
| `G2-BASE` | `--p3-matrix` + `--p8-only` | **PASS** (`leaked_frames=0`) — `--p3-matrix` `5/5 PASS` (`1 vCPU/256M 5.06s`, `2 vCPU/512M`, `4 vCPU/512M 3.95s`, `8 vCPU/1G 4.17s`, `20 vCPU/2G 7.18s`); `--p8-only` (`3.131s`, `p8_hw_foundations_latest.json`): `bar64_above4g=true`, `msi_vec=0x40`, `msi_bound_p50_us=22.73`, `vtd_rogue_faults=1`. |
| `G2-USB-NVME` | `python3 tools/qemu_boot_test.py --p13-only` | **PASS** (`25` assertions, `20.17s`, `leaked_frames=0`) — 3-boot QEMU `qemu-xhci` (`usb-kbd`, `usb-mouse`, `usb-storage` Part 2 `DRYFS_V1`) + `-device nvme`: Boot 1 HID + 12 CLI cmds + `/persist/boot.log` + `desktop.cfg`; Boot 2 reboot recovery; Boot 3 `storage=ro` (`-30 EROFS`); NVMe & Part 1/GPT SHA-256 byte-identical. |
| `G2-BRAND-DPI` | `python3 tools/qemu_boot_test.py --p15-only` (`results_p15_brand_icons.json`) | **PASS** (`4/4` checks, `leaked_frames=0`) — `p15_boot_banner.png` (`1280x800`) + `p15a` (`1280x800`, `10` PNGs, `9.925s`), `p15b` (`1024x768`, `2` PNGs, `4.408s`), `p15c` (`1920x1080`, `2` PNGs, `5.291s`) across `Dark`/`Daylight`/`HC` & `100%..200%` DPI (`15` PNGs total in `docs/screenshots/brand_icons/`). |
| `G2-LX-NET` | `python3 tools/qemu_boot_test.py --p14-only` (`qemu_p14_lx_net_validators.py`) | **PASS** (`1/1`, `3.95s` standalone / `3.85s` combined, `leaked_frames=0`) — `dry-lx-net.elf` (`8139cp+mii`, `mac=0x563412005452`, `bar1_suboff=0`, `tx=2`, `rx=2`, `arp=1`, `udp_echo=1`, `vec=6`, `restarts=1`, `drvctl=0x1fff`) over 6-file `libdry-lx-compat` (`2,164` C LOC). |
| `G2-RAM-4G` | `python3 tools/qemu_boot_test.py --smp-only 2 --mem 4G` | **PASS** (`8.361s`, `leaked_frames=0`) — `usable_mib=4020`, `max_pfn=1572864`, `usable_frames=1029293`, `meta_bytes=25165824`, `meta_overhead_bps=39`. |
| `G2-FULL-SUITE` | `python3 tools/qemu_boot_test.py` | **PASS (`135/135` deterministic checks, `leaked_frames=0`)** — `58` Legacy (`0..=1C+2A-4a`) + `9` P1 + `14` P2 + `6` P3 + `3` P4/2B + `2` 2C-0 + `1` 2C-1 + `1` GPU + `1` 2C-2 HW + `3` DRY Glass + `3` DRY Glass 2.0 + `3` DRY Glass 2.1 + `1` LX DRM + `25` M2 USB/NVMe + `1` M3 LX Net + `4` Brand/Icons. |
| `G2-LIVE-USB` | `python3 tools/build_live_usb_image.py --verify-boot` | **PASS** (`results_live_usb_boot.json`, `5.491s`, `exit=33`, `leaked_frames=0`) — `160 MiB` GPT (`96 MiB` `DRYOS_ESP` + `DRYOS_PERSIST` `DRYFS_V1` `LBA=198656..327646`, `128,991` sectors), `27` modules, `5` Limine profiles, `esp_untouched=true`, `image_sha256=46c96b3c...`. |
| `G2-HOST-15IHU6` | `hw_inventory_lenovo_15ihu6.py`, `gpu_fw_validators.py --self-test`, `provision_local_firmware.py --self-test` | **PASS (Read-Only)** — `82K1`, i5-11300H (`39b/48b`), `32 GiB`, `8250.nr_uarts=0`, `24` PCI funcs; `tgl_dmc` `2.12` + `tgl_guc` `70.1.1` + `$VBT` + `eDP-1` EDID + `preserve_active_gop` + `gsp_ga10x.bin` WPR2 + `os-interface.h` (`22/162`) + PRIME `RO` fence (`5` host blobs, `0` `/dev` or `/sys` writes). |

---

## 2. Gates 3–7 — Bare-Metal Physical Hardware (`READY FOR USER PHYSICAL EXECUTION`)

Target system: **Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`, BIOS `H4CN22WW(V1.07)`, Core i5-11300H, `32 GiB` RAM, `8250.nr_uarts=0`)**. None of Gates 3–7 were run by AI agents; they require physical operator execution following [docs/LIVE_USB_OPERATOR_GUIDE.md](LIVE_USB_OPERATOR_GUIDE.md).

| Gate | Target Hardware & Scope | Pass Criteria | Status |
| :--- | :--- | :--- | :--- |
| **Gate 3: Physical USB Flash & UEFI Limine Boot** | Removable USB stick + `82K1` UEFI (`mokutil --sb-state`, Secure Boot disabled manually) | USB flashed strictly via `/dev/disk/by-id/usb-*`; internal Micron NVMe `1344:5404` & SATA SSD untouched; Limine menu displays all `5` profiles without COM1 hang. | **`READY FOR USER PHYSICAL EXECUTION`** |
| **Gate 4: Physical xHCI HID & Partition 2 Persistence** | `00:14.0` Tiger Lake xHCI (`8086:a0ed`), USB keyboard/mouse, `DRYOS_PERSIST` Part 2 | USB HID input works in `DRY Glass 2.1`; `/persist/boot.log` & `desktop.cfg` persist across physical reboot (`storage=rw`); Profile 3/4/5 (`storage=ro`) rejects writes (`-EROFS`). | **`READY FOR USER PHYSICAL EXECUTION`** |
| **Gate 5: Physical Laptop Boot Diagnostics & HiDPI** | `8250.nr_uarts=0` `DIAG_RING`, `1920x1080` `AUO 0xb69b` `eDP-1`, `panic_fb.rs` | `dmesg` returns kernel boot ring; `1920x1080` Profile 2 renders `DRY Glass 2.1` + `118`-SVG assets cleanly across `100%..200%` DPI; `32 GiB` RAM inventoried by PMM. | **`READY FOR USER PHYSICAL EXECUTION`** |
| **Gate 6: Physical Intel Iris Xe GOP & Multi-DRHD VT-d** | `00:02.0` Iris Xe (`8086:9a49`, `preserve_active_gop`), `DMAR` dual-DRHD + `xHCI` RMRR, `00:0e.0` VMD (`8086:9a0b`) | Dedicated GOP DRHD deferred (`dmar.drhd_count > 1`) while catch-all DRHD + RMRR enforces `VT-d` without black-screening GOP; VMD domain stays gated & host NVMe untouched. | **`READY FOR USER PHYSICAL EXECUTION`** |
| **Gate 7: Physical NVIDIA RTX 3050 Ti & Hybrid GPU** | `01:00.0` GA107M (`10de:25a0`, muxless `0x030200`, `4 GiB` BAR1, `d3cold_allowed=1`) | Physical PCI enumeration & `>4 GiB` BAR decode observed via `lspci` (**`READY FOR USER PHYSICAL EXECUTION`**); native `i915`/`xe`/OpenRM modeset & `RTD3` remain **`BLOCKED`** pending GEM/TTM + `140` `os_*` + GSP-RM RPC. | **`READY FOR USER PHYSICAL EXECUTION`** (`Native Modeset BLOCKED`) |
