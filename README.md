<p align="center">
  <img src="assets/brand/dryos-lockup-boot.svg" alt="DRY OS — Project Ascension 5.0" width="540">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Architecture-x86--64%20Capability%20Microkernel-1d4ed8?style=flat-square" alt="Architecture">
  <img src="https://img.shields.io/badge/Language-no__std%20Rust-0f766e?style=flat-square" alt="Language">
  <img src="https://img.shields.io/badge/Desktop-DRY%20Glass%202.1-334155?style=flat-square" alt="Desktop">
  <img src="https://img.shields.io/badge/Release-Project%20Ascension%205.0%20(v0.5.0)-1e293b?style=flat-square" alt="Release">
  <a href="LICENSING.md"><img src="https://img.shields.io/badge/License-Apache--2.0%20OR%20MIT%20%7C%20GPL--2.0%20Islands-475569?style=flat-square" alt="License"></a>
</p>

# DRY OS — Project Ascension 5.0 (`v0.5.0`)

**A clean-slate x86-64 capability microkernel OS in Rust with an anti-aliased vector desktop (`DRY Glass 2.1`), VT-d DMA isolation, Ring-3 xHCI USB HID/BOT + Read-Only NVMe storage, a 160 MiB 2-partition GPT UEFI Live USB, and process-isolated Linux Driver Islands (`VirtIO-RNG`, `DRM Bochs`, `8139cp`).**

> **Verification Scope (`v0.5.0`):** Gates 1–2 (builds, Clippy `-D warnings`, `118`-SVG vector asset compiler, QEMU/OVMF emulation [`135/135` deterministic checks PASS], `160 MiB` GPT Live USB `--verify-boot` [`5.491s`], and read-only Lenovo `82K1` inventory/firmware validators) are verified in CI/QEMU/read-only host audits (`27/32` Ring-3 modules, `leaked_frames=0`). Physical bare-metal Gates 3–7 on target Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`) hardware remain strictly **`READY FOR USER PHYSICAL EXECUTION`** — see [docs/ASCENSION_5_0_VALIDATION_MATRIX.md](docs/ASCENSION_5_0_VALIDATION_MATRIX.md).

---

## Desktop & Brand Showcase

<p align="center">
  <img src="docs/screenshots/brand_icons/p15_boot_banner.png" alt="DRY OS Iso-Kernel Prism Boot Lockup" width="48%">
  <img src="docs/screenshots/brand_icons/p15_1280x800_gallery_dark.png" alt="DRY Glass 2.1 with 21 Custom App Icons and System Iconography" width="48%">
</p>

*Left: Ring-3 UEFI GOP boot lockup (`Iso-Kernel Prism` brand identity). Right: `DRY Glass 2.1` (`1280x800`) showcasing all 21 custom app icons, 91 system icons, `DRY Files` (`/persist` `DRYFS_v1`), and adaptive window strip (`--p15-only`).*

---

## Core Architecture

DRY (**Minimal Mechanisms, Maximum Useful Parallelism**) enforces strict privilege and failure isolation. Ring 0 (`dry-kernel`) contains **zero** filesystems, **zero** block/USB/NVMe/network/display drivers, **zero** UI/font logic, and **zero** lines of Linux or GPL code.

```mermaid
flowchart TB
    subgraph R3_APPS["Ring 3: Userspace Apps (SecDomain::Userspace, Isolated VSpace + CSpace)"]
        TERM["Terminal (dry-shell, 12+ Linux CLI cmds)"]
        SYSMON["Life Station (dry-sysmon)"]
        FILES["Files (dry-files)"]
        SETTINGS["Settings (dry-settings + Identity Gallery)"]
        EDITOR["Notes (dry-editor)"]
        CALC["Calculator (dry-calc)"]
    end

    subgraph R3_SYS["Ring 3: Native System Services (SecDomain::RootSystem)"]
        INIT["dry-init Supervisor"]
        DISP["dry-display (DRY Glass 2.1 + Hybrid WM)"]
        INPUT["dry-input (PS/2 i8042 IRQ1/IRQ12)"]
        VFS["dry-vfs (/boot RO, /ram, /persist DRYFS_v1 RW/RO)"]
        VAULT["dry-vault (PBKDF2 Auth + Lease Epoch)"]
        BROKER["dry-broker (Control-Plane + 15-Step Lifecycle)"]
    end

    subgraph R3_DRV["Ring 3: Native & Isolated Linux Driver Islands (27/32 Modules)"]
        NDRV["Native Rust: dry-blk, dry-gpu, dry-usb (xHCI HID+BOT), dry-nvme (RO)"]
        LX_RNG["Linux v6.18 Island: dry-lx-rng (virtio-rng.c, GPL-2.0-only)"]
        LX_DRM["Linux v6.18 Island: dry-lx-drm-bochs (bochs.c, GPL-2.0-or-later)"]
        LX_NET["Linux v6.18 Island: dry-lx-net (8139cp.c + mii.c, GPL-2.0-only)"]
    end

    subgraph R0["Ring 0: dry-kernel (Apache-2.0 OR MIT, Syscall ABI v0.1 0..=31, 0 GPL Lines)"]
        K["CSpace CDT | VSpace W^X | 32G PMM | H1 SMP | VT-d SLPT/RMRR | GOP Panic + DIAG_RING"]
    end

    R3_APPS <--> DISP
    R3_APPS <--> VFS
    VFS <-- "Lock-Free IoRingSharedPage" --> NDRV
    DISP <-- "USB HID InputEvent / RO FrameCap" --> NDRV
    DISP <-- "RO PRIME FrameCap" --> LX_DRM
    BROKER -- "Control-Plane Leases & FaultEndpoint" --> R3_DRV
    R3_SYS -- "Capability Handles (248 B Hot Syscall Frame)" --> K
    R3_DRV -- "SYS_DEV_MAP (Sub-Page BAR) / SYS_DMA_ALLOC / SYS_IRQ_BIND" --> K
```

### Architectural Highlights

- **Ring-0 Capability Microkernel (`kernel/`):**
  - Frozen `Syscall ABI v0.1` (`32` calls: `0..=31`) with a `248 B` (`<= 256 B`) `rust_syscall_dispatch` hot stack frame, `CR4.SMAP = 1` + `extable`, `RFLAGS.IOPL = 0`, `XSAVEOPT64`/`FXSAVE64` FPU/SIMD isolation, `IST1..3` (`#DF`, `NMI`, `#MC`), and `FaultEndpoint` supervision.
  - Guarded `CSpace` (`MAX_CSPACES = 32`, `256` slots/`CNode`, $O(k)$ CDT `revoke`, $O(1)$ `GLOBAL_LEASE_EPOCH`), 4-level `W^X` `VSpace` with targeted SMP TLB shootdown, `16 B` `FrameMeta` `PMM` scaling up to `32 GiB` host RAM, and `H1` SMP scheduler (`1..20 vCPU` QEMU-tested; TLA+ `84,351`/`54,095` states, Loom `39,135` permutations).
  - Bare-metal boot hardening: COM1 scratch/LSR probe (`probe_com1_uart`) prevents stalls on UART-less laptops (`8250.nr_uarts=0`), `16 KiB` in-memory `DIAG_RING` feeds `SYS_SYSINFO` `dmesg` + `/persist/boot.log`, `panic_fb.rs` renders high-contrast fatal diagnostics directly to UEFI GOP, and `live_usb=1` skips synthetic burn-ins for fast interactive boot.
- **VT-d / IOMMU SLPT Isolation & PCIe Foundations (`kernel/src/iommu/`, `kernel/src/pci*.rs`):**
  - ACPI `DMAR` parsing (`dmar_unit=5`, `39-bit` 3-level & `48-bit` 4-level `SLPT`), RMRR pre-mapping (`premap_unit_rmrrs`), multi-DRHD active-GOP deferral (`dmar.drhd_count > 1 && is_dedicated_gop_drhd`) + `preserve_active_gop` BAR protection, `PCI` `MSI`/`MSI-X` (`0x40..=0x5F`) bound to `TCB` `Notification`s, `>4 GiB` `WC`/`UC` BAR windows, `< 4 KiB` sub-page PCI MMIO BARs (`page_suboff`), and crash quiesce (`BME=0` + `SLPT/IOTLB` scrub).
- **USB xHCI, Read-Only NVMe, `DRYFS_v1` & `DRY Vault` (`userspace/src/`):**
  - `dry-usb.elf` drives xHCI Boot HID keyboards/mice alongside PS/2 `dry-input` and USB Mass Storage BOT Partition 2 (`DRYOS_PERSIST`) with strict `INV-DISK-SAFE-02` enforcement (rejects writes outside Partition 2 or under `storage=ro`).
  - `dry-nvme.elf` (`INV-DISK-SAFE-01`) compiles **only** Admin `IDENTIFY (0x06)`, `CREATE_IO_CQ (0x05)`, `CREATE_IO_SQ (0x01)`, and NVM `READ (0x02)` — zero write/format/trim opcodes exist; Intel VMD (`8086:9a0b`) domains remain safely gated read-only.
  - `dry-vfs` mounts `/boot` (RO), `/ram` (compartment-isolated volatile), and `/persist` (`DRYFS_v1` CoW dual A/B `CRC32` superblocks, RW or RO), persists `/persist/boot.log`, and pairs with `dry-vault` (`PBKDF2-HMAC-SHA256`, `Session Lock`) plus 12+ Linux-grade shell commands (`dmesg`, `lspci`, `lsusb`, `lsblk`, `nvme`, `drvctl`, `uname`, `free`, `mounts`, `cat`, `write`, `sync`).
- **`DRY Glass 2.1` Desktop & Original Vector Identity (`userspace/src/{ttf,ui,glass,apps}/`, `assets/`):**
  - Pure `#![no_std]` TrueType AA (`DrySans` / `DryMono`, `OFL-1.1`), SDF vector canvas, dual-pass frosted blur, `100% / 125% / 150% / 200%` DPI scaling, `Obsidian Dark` / `Frosted Daylight` / `High Contrast` themes, `Adaptive Window Strip`, `Alt+W` Window Overview, and 6 native apps.
  - Original **Iso-Kernel Prism** visual identity (`118` SVGs: `6` brand, `21` app icons, `91` system icons) compiled deterministically by `tools/build_vector_assets.py` into `#![no_std]` Rust POD tables (`vector_assets_sys.rs`, `vector_assets_apps.rs`).
- **Universal Linux Driver Islands (`drivers/`, `tools/lx_compat/`):**
  - Binary `.ko` modules are strictly rejected (`KO_BINARY_REJECTED`). `56` SHA-256-verified upstream Linux `v6.18` sources link inside isolated `CPL=3` processes over modular 6-file `libdry-lx-compat` (`2,164` C LOC) and a 15-step broker lifecycle (`native-driver-island`, `lx-compat-island`, `linux-microvm`), running `virtio-rng.c`, `bochs.c`, and Realtek `8139cp.c` + `mii.c` (`dry-lx-net.elf`) with `#UD` `gen=2` fault recovery.

---

## Hardware Readiness Matrix

| Target / Subsystem | PCI / Hardware Scope | Current Verification Status | Honest Scope & Remaining Gap |
| :--- | :--- | :--- | :--- |
| **QEMU x86-64 Core, SMP & Storage** | OVMF UEFI, Limine, `i8042`, `VirtIO-Blk`, `DRYFS_v1`, `DRY Vault` | **DONE (QEMU/OVMF)** | `1..20 vCPU`, `256M..4G` RAM, `SIGKILL` CoW recovery |
| **QEMU `VT-d`, `MSI/MSI-X`, `>4G` / Sub-Page BAR** | `intel-iommu` (`DMAR`), `edu` MSI, `ivshmem` `>4 GiB`, `< 4 KiB` BAR | **DONE (QEMU/OVMF)** (`--p8-only`) | Physical silicon `VT-d`/IOMMU awaits user Gate 5–7 |
| **QEMU USB xHCI HID/BOT & RO NVMe** | `qemu-xhci`, `usb-kbd/mouse/storage` Part 2, `nvme` (`INV-DISK-SAFE`) | **DONE (QEMU/OVMF)** (`--p13-only`, `25` checks) | Physical `82K1` xHCI/HID/VMD awaits user Gate 3–5 |
| **QEMU `DRY Glass 2.1` + 118-SVG Identity** | `VirtIO-GPU`, `UEFI GOP`, `100%..200%` DPI, 21 App + 91 Sys Icons | **DONE (QEMU/OVMF)** (`--p11/p15-only`, `15` PNGs) | `2D` scanout + GOP fallback; `Virgl`/`Venus` 3D planned |
| **Linux Islands: `virtio-rng.c`, `bochs.c`** | `1AF4:1044`, `1234:1111` (`libdry-lx-compat`, `GPL-2.0-*`) | **DONE (QEMU/OVMF)** (`--lx-island-only`, `--p12-only`) | Unmodified `v6.18` C sources; `#UD` -> `gen=2` recovery |
| **Linux Island: `8139cp.c` + `mii.c`** | `10EC:8139` (`dry-lx-net.elf`, sub-page BAR1, SLIRP UDP echo) | **DONE (QEMU/OVMF)** (`--p14-only`, `1/1` PASS) | `56` SHA-256 files + `5/5` unit tests; `udp_echo=1`, `#UD gen=2`, `leaked_frames=0` |
| **UEFI Live USB (`160 MiB` GPT)** | `96 MiB` `DRYOS_ESP` + `DRYOS_PERSIST` `DRYFS_V1` (`27` modules) | **DONE (QEMU xHCI)** (`--verify-boot`, `5.491s`) | Physical USB flash & boot **`READY FOR USER PHYSICAL EXECUTION`** |
| **Lenovo 15IHU6: Iris Xe & RTX 3050 Ti** | `8086:9a49` (`dmc`/`guc`/VBT/EDID), `10de:25a0` (`gsp_ga10x` WPR2/PRIME) | **Read-Only Audit + Offline FW Validators PASS** | Native modeset **`BLOCKED`**; physical GOP/dGPU Gate 5–7 user-run |

---

## Quickstart & Reproducible Verification

### 1. Prerequisites, Toolchain & Vector Asset Verification

Requires Linux `x86_64` with `rustc`/`cargo` (locked in `tools/phase1-toolchain-lock.json`), `clang` + `lld`, `python3`, `qemu-system-x86_64`, OVMF (`/usr/share/edk2/x64/OVMF_{CODE,VARS}.4m.fd`), and `mtools` (`mformat`, `mmd`, `mcopy`, `mdir`).

```bash
git clone https://github.com/Zwannfrederick/DryOS.git
cd DryOS

# One-time offline/locked sysroot + Limine UEFI bootstrap (writes only to .toolchains/)
python3 tools/bootstrap_phase1.py

# Verify 118 original SVG assets, manifest SHA-256 digests, and generated no_std Rust tables
python3 tools/build_vector_assets.py --verify
```

### 2. Offline Release Build & QEMU Verification Gates

```bash
# Offline release build + Clippy (-D warnings) for kernel, 27 userspace modules, and GPL islands
python3 tools/qemu_boot_test.py --build-only

# Individual subsystem & Project Ascension 5.0 QEMU/OVMF gates
python3 tools/qemu_boot_test.py --p8-only            # VT-d DMAR/SLPT, MSI/MSI-X, >4 GiB & sub-page BAR
python3 tools/qemu_boot_test.py --p11-only           # DRY Glass 2.1 adaptive strip, Alt+W, HiDPI
python3 tools/qemu_boot_test.py --p12-only           # Linux v6.18 bochs.c DRM island + RO PRIME + GOP coexist
python3 tools/qemu_boot_test.py --p13-only           # M2 USB xHCI HID/BOT Part 2, RO NVMe, /persist/boot.log, 12 cmds (25 checks)
python3 tools/qemu_boot_test.py --p14-only           # M3 libdry-lx-compat v2, 15-step lifecycle, 8139cp.c+mii.c SLIRP UDP + #UD gen=2
python3 tools/qemu_boot_test.py --p15-only           # 118 SVGs & 100%/125%/150%/200% HiDPI DRY Glass integration (4 checks, 15 PNGs)
python3 tools/qemu_boot_test.py --smp-only 2 --mem 4G # 4 GiB RAM PFN/PMM boot validation
python3 tools/qemu_boot_test.py --p3-matrix          # 1..20 vCPU / 256M..2G RAM SMP matrix

# Full deterministic QEMU/OVMF suite (135/135 PASS, leaked_frames=0)
python3 tools/qemu_boot_test.py
```

### 3. Live USB Builder & Read-Only Host / Physical Bare-Metal Checks

```bash
# Build 160 MiB 2-partition GPT Live USB (96 MiB FAT32 DRYOS_ESP + DRYOS_PERSIST DRYFS_V1, 27 ELFs)
# Output: kernel/build/dryos-ascension5-live-usb.img (verified under QEMU xHCI)
python3 tools/build_live_usb_image.py --verify-boot

# Read-only Lenovo 15IHU6 (82K1) hardware inventory & Iris Xe / RTX 3050 Ti firmware/WPR2/PRIME validators
python3 tools/hw_inventory_lenovo_15ihu6.py
python3 tools/gpu_fw_validators.py --self-test

# Optional opt-in local firmware staging (NEVER committed to git):
# python3 tools/provision_local_firmware.py --stage --staging-dir /tmp/dryos-fw-stage
```

**Bare-Metal Physical USB Note:** QEMU/OVMF `--verify-boot` does not test real silicon IOMMU or laptop ACPI/xHCI quirks. To execute **Physical Gates 3–7** safely on real hardware, follow [docs/LIVE_USB_OPERATOR_GUIDE.md](docs/LIVE_USB_OPERATOR_GUIDE.md): verify target disks via `lsblk` and `/dev/disk/by-id/usb-*` first, write `kernel/build/dryos-ascension5-live-usb.img` only to a removable USB drive, and inspect Secure Boot non-destructively (`mokutil --sb-state`).

---

## Repository Layout & Licensing

```text
DryOS/
|-- kernel/                  # Ring-0 no_std x86-64 capability microkernel (Apache-2.0 OR MIT, 0 GPL lines)
|-- userspace/               # 24 native ELFs: DRY Glass 2.1, usb/nvme RO, 6 native apps (Apache-2.0 OR MIT)
|-- drivers/
|   |-- libdry-lx-compat/    # 6-file modular Ring-3 C compat runtime, 2,164 LOC (MIT OR GPL-2.0-only)
|   |-- lx-virtio-rng-island/# Linux v6.18 virtio-rng.c Ring-3 island (GPL-2.0-only)
|   |-- lx-drm-bochs-island/ # Linux v6.18 bochs.c Ring-3 DRM island (GPL-2.0-or-later)
|   |-- lx-net-8139cp-island/# Linux v6.18 8139cp.c + mii.c Ring-3 NIC island (GPL-2.0-only)
|   `-- lx-nv-open-shim/     # Compile-only NVIDIA os-interface.h validator (MIT)
|-- vendor/linux-drivers/    # Tracked SHA-256 MANIFEST.json; v*/ upstream C trees fetched on demand
|-- assets/                  # 6 brand SVGs, 21 app SVGs, 91 system SVGs (Apache-2.0 OR MIT) + OFL-1.1 fonts
|-- tools/                   # Offline bootstrap, vector compiler, QEMU/QMP validators, 160 MiB GPT Live USB builder
|-- formal/ & experiments/   # TLA+ H1 scheduler specs, Loom stress tests, H2/H3 research probes
`-- docs/                    # Design system, hardware/GPU audits, validation matrix, operator guide
```

DRY OS maintains a strict **galvanic license boundary** ([LICENSING.md](LICENSING.md)): original DRY OS code (`kernel/`, `userspace/`, `assets/brand/`, `assets/icons/`, `tools/`, `docs/`) is **`Apache-2.0 OR MIT`**, bundled font subsets are **`OFL-1.1`**, `libdry-lx-compat/` is **`MIT OR GPL-2.0-only`**, and isolated Ring-3 driver islands are **`GPL-2.0-only`** / **`GPL-2.0-or-later`**.

---

## Documentation Index

- [ARCHITECTURE.md](ARCHITECTURE.md) — Privilege boundaries, syscall ABI, `27` Ring-3 compartments, VT-d RMRR/GOP deferral, `INV-DISK-SAFE-01/02`
- [HANDOVER.md](HANDOVER.md) & [ROADMAP.md](ROADMAP.md) — Engineering handover guide, milestone ledger, and next phase queue
- [DECISIONS.md](DECISIONS.md) — Architecture Decision Records (`ADR-001` through `ADR-023`)
- [RISK_REGISTER.md](RISK_REGISTER.md) & [LICENSING.md](LICENSING.md) — Risk mitigations and dual-license isolation architecture
- [docs/ASCENSION_5_0_VALIDATION_MATRIX.md](docs/ASCENSION_5_0_VALIDATION_MATRIX.md) — Complete Gate 1–7 matrix (Verified QEMU/Host vs User Physical Execution)
- [docs/LIVE_USB_OPERATOR_GUIDE.md](docs/LIVE_USB_OPERATOR_GUIDE.md) — Safe USB flashing, Secure Boot check, 5 Limine profiles, `DRYFS_V1` diagnostics
- [docs/DESIGN_SYSTEM.md](docs/DESIGN_SYSTEM.md) & [docs/ASSET_LICENSING_AND_INVENTORY.md](docs/ASSET_LICENSING_AND_INVENTORY.md) — `Iso-Kernel Prism`, UI tokens, and `118`-SVG inventory
- [docs/HARDWARE_INVENTORY_LENOVO_15IHU6.md](docs/HARDWARE_INVENTORY_LENOVO_15IHU6.md) & [docs/GPU_COMPATIBILITY_REPORT.md](docs/GPU_COMPATIBILITY_REPORT.md) — Read-only `82K1` audit & Iris Xe / RTX 3050 Ti validation
- [docs/DRY_GLASS_2_1_REPORT.md](docs/DRY_GLASS_2_1_REPORT.md) & [docs/LINUX_COMPAT_RUNTIME_REPORT.md](docs/LINUX_COMPAT_RUNTIME_REPORT.md) — `DRY Glass 2.1` + `200%` DPI and `libdry-lx-compat` v2 reports
