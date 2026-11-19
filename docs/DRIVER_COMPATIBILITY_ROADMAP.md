# DRY OS — Pragmatic Driver Compatibility Roadmap (`DRIVER_COMPATIBILITY_ROADMAP.md`)

**Branch:** `ascension-8.0`  
**Strategy Principle:** *Minimum Overhead. Maximum Useful Work.* Never replicate all of Linux KPI or Windows WDM in `dry-kernel`. Choose the lightest isolation boundary that safely matches device complexity.

---

## 1. Three-Tier Compatibility Strategy

| Tier | Target Hardware Class | Execution Boundary | Licensing / TCB Impact | Status on `ascension-8.0` |
| :--- | :--- | :--- | :--- | :--- |
| **Tier 1: Native Rust Driver Islands (`dry-driver-sdk`)** | Core bus/display/input/storage (`dry-usb`, `dry-nvme`, `dry-blk`, `dry-gpu`, `dry-sdk-demo`, future Tiger Lake eDP plane-flipper & LPSS I2C-HID) | Isolated Ring-3 ELF (`SecDomain::Userspace` or scoped `RootSystem`) using `DRY Native Syscall ABI v0.2` + `Island ABI v1` | Dual MIT/Apache-2.0 (`no_std`); tiny binary footprint (`< 100 KiB`); zero C shim bloat | **Production baseline in QEMU** (`dry-sdk-demo.elf` `p18e` verified; `5/5` SDK host tests) |
| **Tier 2: Bounded Linux Source-Compat Islands (`libdry-lx-compat`)** | Self-contained Linux PCI drivers with small kernel-API surface (`virtio-rng`, `bochs-drm`, `8139cp`) | Ring-3 GPL-isolated ELF per driver supervised by `dry-broker` + `FaultEndpoint` + `BrokerLifecycle` (`15` stages) | Upstream `.c` verified untouched by SHA-256 (`56` files verified); zero GPL inside `dry-kernel` | **Verified in QEMU** (`p6`, `p12`, `p14`) |
| **Tier 3: MicroVM / VFIO Isolated Islands (Future)** | Multi-million-LOC subsystems requiring complex firmware/scheduler/VM runtime (NVIDIA GA107 `10de:25a0` OpenRM + GSP-RM, Wi-Fi `iwlwifi`, Vulkan/Mesa 3D offload) | Hardware-isolated `VT-x` + `VT-d IR` + VFIO guest island + read-only compositor/IVSHMEM transport | Zero GPL/proprietary vendor code in `dry-kernel`; crashes/hangs confined to disposable guest VM | **Level 1 (`DRIVER ISLAND READY`)** control/prerequisite/telemetry island (`dry-nv-island.elf` in M4); execution **BLOCKED** pending VT-x/VT-d IR/VFIO |

---

## 2. Sequenced Execution Roadmap (Post-Ascension 8.0)

### Phase A — Real Hardware Measurement & Laptop Ergonomics (Next after M5)
1. **Bare-Metal Lenovo `82K1` Boot Telemetry:** Capture actual session bundle (`hardware-tests/sess-XXXX/`) from physical `DRYOS_LOGS` partition to confirm real ACPI (`DMAR`, `MCFG`, `MADT`), `8086:9a49` BAR0/BAR2, `10de:25a0` BAR0/BAR1/BAR3, VMD (`8086:9a0b`) topology, and UEFI GOP `1920x1080` layout.
2. **Native LPSS I2C-HID Touchpad Island (`dry-driver-sdk`):** Use detected Intel LPSS I2C controller BDFs to bring up internal laptop touchpad input in `SecDomain::Userspace` alongside external USB HID (`dry-usb`).
3. **USB Hub Cascade Support (`dry-usb`):** Extend xHCI slot management cleanly beyond root-hub ports to external 1-level USB hubs for laptops with shared internal hubs.

### Phase B — Native Intel Iris Xe Scanout & NVMe Non-VMD Read/Write (Scoped)
1. **Native Tiger Lake `eDP-1` Page Flip (`dry-intel-xe` on `dry-driver-sdk`):** On hardware where UEFI GOP already initialized `eDP-1`, validate `validate_tgl_pci_bars`, take over `PLANE_SURF_1_A` flipping and `DE_MASTER_IRQ` vsync without destructive blind PLL resets.
2. **Read/Write NVMe Namespace Fences:** Extend `dry-nvme` only after explicit GPT partition UUID whitelist + opt-in boot policy (`storage=rw` must NEVER auto-mount host Windows/Linux OS partitions).

### Phase C — Minimal `VT-x` + `VT-d IR` + VFIO MicroVM Substrate (Prerequisite for Tier 3)
1. **`VT-d` Interrupt Remapping (`IRTA`) + Posted MSI:** Upgrade `kernel/src/iommu/vtd.rs` from DMA remapping (`SLPT`) alone to Interrupt Remapping (`iommu_audit_flags |= IOMMU_AUDIT_INT_REMAP`).
2. **Scoped `VT-x` Hypervisor Capability & VFIO Group Assign:** Allow an isolated Ring-3 VMM (`SecDomain::Userspace` + dedicated `VmxCap`) to bind one VFIO-eligible secondary PCI function (such as muxless `10de:25a0`) while keeping `8086:9a49` host GOP/iGPU protected.
3. **NVIDIA Level 2 -> Level 3 Promotion:** Mount read-only `ucodes_ga10x.bin` verified via `FwStreamVerifier` into the isolated MicroVM, run upstream OpenRM inside the disposable guest, and project rendered offscreen frames back to `dry-display` over explicit read-only `READ_ONLY_EXPORT` frames + fences.

---

## 3. Explicit Non-Goals (Guardrails)

- **No Ring-0 Loadable `.ko` / `.sys` Subsystem:** Violates microkernel fault isolation and license hygiene.
- **No Unbounded Fake Linux Symbol Fabrication (`400–600+` symbols):** Any driver requiring > ~40–60 kernel primitives belongs on Tier 3 (MicroVM), not an inflated Ring-3 C shim.
- **No Unverified Hardware Writes:** No phase skips read-only contract verification or automatic disk/GPU mutations without opt-in flags and verified device ownership.
