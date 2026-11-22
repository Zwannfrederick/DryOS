# DRY OS Project Ascension 8.0 — Diagnostic Terminal Commands Reference (`HARDWARE_DIAGNOSTIC_COMMANDS.md`)

> **Shared Framework:** All boot-time automatic checks, `dry-sysmon` status-strip summaries, `dry` shell commands ([userspace/src/shell_diag_cmds.rs](../userspace/src/shell_diag_cmds.rs)), and NVIDIA Level-1 GPU commands ([userspace/src/shell_gpu_cmds.rs](../userspace/src/shell_gpu_cmds.rs)) invoke one shared Ring-3 diagnostic framework ([userspace/src/hwdiag/](../userspace/src/hwdiag/mod.rs) and [userspace/src/nvidia_island/](../userspace/src/nvidia_island/mod.rs)) over supervisor IPC (`IPC_OP_SUP_DIAG_RUN`, `IPC_OP_SUP_DIAG_QUERY`, `IPC_OP_SUP_NV_QUERY`, `IPC_OP_SUP_NV_EXPORT`). No parallel dummy test engines exist.

---

## 1. Status & Safety Semantics

Every check emits a structured `DiagTestRecord` (`128 B`) with one of four strict verdicts:

| Status | Meaning | Example Condition |
| :--- | :--- | :--- |
| **`PASS`** | Bounded passive check ran and satisfied expectation. | `CPU-01-CPUID` (`sse2=1 nx=1`), `MEM-03-ALLOC-RW` (allocated 4 KiB scratch page RW verified + freed). |
| **`FAIL`** | Real probe detected a concrete fault or mismatch. | ACPI checksum failure (`ACPI-01-TABLES-CRC`), corrupt `DRYOS_LOGS` FAT32 (`USB-02-LOGS-ANCESTRY` `-74`). |
| **`SKIP`** | Non-fatal safe omission under post-EBS or policy constraints. | `UEFI-02-SECUREBOOT` (`post_ebs_nvram_untouched` — avoids mutating or probing post-ExitBootServices NVRAM when state wasn't exported). |
| **`BLOCKED`** | Hardware prerequisite missing or safety fence forbids execution. | `IOMMU-01-VTD-DMA` when `VT-d` absent, `IOMMU-02-INTREMAP` when interrupt remapping absent (`-38`), `USB-02-LOGS-ANCESTRY` when ancestry gates `< 0x1f`, or all NVIDIA `Level 1` GPU runtime actions. |

---

## 2. Core `dry` Diagnostic Commands (`15` + `help`)

| Command | Scope / Behavior | Notes & Safety |
| :--- | :--- | :--- |
| `dry help` | Prints supported `dry ...` subcommand usage line. | Terminal convenience help (`[DRY-SHELL] CMD=dry help ok=true`). |
| `dry doctor` | Runs full passive suite (`SCOPE_ALL`, `force_safe=true`). | Summarizes `pass`/`fail`/`skip`/`blocked` and USB `DRYOS_LOGS` persistence status. |
| `dry check --safe` | Runs all passive/safe checks (`SCOPE_ALL`, `force_safe=true`). | Zero destructive BAR writes, zero MMIO GPU pokes, zero internal disk mounts. |
| `dry check --all` | Runs all checks permitted by the active boot profile (`SCOPE_ALL`, `force_safe=false`). | **Profile-fenced:** Under `hwcheck_mode=safe`, `desktop_safe`, `recovery`, or `pci_passive_mode=1`, `--all` never authorizes mutating tests. |
| `dry check --cpu` | `SCOPE_CPU` checks: `CPU-01-CPUID`, `CPU-02-APIC-SMP`, `CPU-03-TSC-MONO`, `CPU-04-COMPUTE` (CRC32 deterministic math), `CPU-05-SCHED-CTX` (`8x sys_yield`). | Read-only CPUID/TSC/sysinfo + bounded computation. |
| `dry check --memory` | `SCOPE_MEM` checks: `MEM-01-MEMMAP` (usable MiB & regions), `MEM-02-PMM-INV` (`free <= usable`), `MEM-03-ALLOC-RW` (4 KiB allocated scratch frame pattern + unmap/revoke), `MEM-04-CAP-GUARD` (reject null cap & KVA map). | Uses strictly newly allocated `sys_frame_alloc(0)` scratch memory; never touches unknown physical RAM. |
| `dry check --acpi` | `SCOPE_ACPI` checks: `ACPI-01-TABLES-CRC` (`RSDP`/`MADT`/`MCFG`/`FACP`/checksums), `UEFI-01-GOP-FB` (framebuffer pitch/geometry), `UEFI-02-SECUREBOOT` (read-only/post-EBS skip). | Never modifies UEFI variables (`efivars`). |
| `dry check --pci` | `SCOPE_PCI` checks: `PCI-01-ENUM-BARS` (`funcs>0`, `bad_bar=0`), `PCI-02-CAPS-VMD` (MSI/MSI-X/PCIe capability flags & Intel `VMD` detection). | Purely read-only ECAM/boot inventory (`pci_passive_mode=1` respects passive devices). |
| `dry check --iommu` | `SCOPE_IOMMU` checks: `IOMMU-01-VTD-DMA` (`DMAR`, `vtd`, `DRHD`, `RMRR`), `IOMMU-02-INTREMAP` (`int_remap`, `IVRS`, address width). | Fail-closed: reports `BLOCKED` if HW IOMMU or IR is absent/unimplemented. |
| `dry check --gpu` | `SCOPE_GPU` checks: Intel Iris Xe (`8086`) / NVIDIA (`10de`) / VirtIO GPU detection, BAR & GOP handover readiness, and native feature gaps. | Read-only; honors `ALLOW_PHYSICAL_GPU_WRITES = false`. |
| `dry check --usb` | `SCOPE_USB` checks: xHCI controller readiness and `USB-02-LOGS-ANCESTRY` (`gates=0x1f`, FAT32 mount/corruption/block reason). | Verifies all 5 ancestry gates before trusting `DRYOS_LOGS`. |
| `dry check --storage` | `SCOPE_STORAGE` checks: `STOR-01-INTERNAL-RO` (`storage=ro`, NVMe presence, VMD passive status, VirtIO-Blk readiness). | Confirms internal NVMe/SATA drives remain unmounted and unmodified. |
| `dry drivers` | `SCOPE_DRIVERS` checks: `DRV-01-ISLAND-RING3` (live USB/Blk/NVMe island PIDs & `zero_kernel_gpl_drivers` check). | Verifies Ring-3 driver compartment isolation. |
| `dry compat` | `SCOPE_COMPAT` checks: `COMPAT-01-BASELINE` (ABI version, active boot profile, `gop+xhci+vtd+island_v1` baseline). | Reports compatibility tier honestly. |
| `dry report` | Queries cached RAM `DiagSessionReport` via `IPC_OP_SUP_DIAG_QUERY` without re-running hardware initialization tests. | Safe under `RECOVERY` mode (`persist=RECOVERY`). |
| `dry report --export-usb` | Forces verified export of the `13`-file session bundle (`12` data files + `manifest.sha256`) to `DRYOS_LOGS` FAT32 Partition 3 (when all 5 ancestry gates + RW pass). | Performs SHA-256 read-back verification before reporting `persist=VERIFIED`. |

---

## 3. NVIDIA Level-1 & Cross-Vendor GPU Commands (`7` Commands)

Backed by [userspace/src/nvidia_island/](../userspace/src/nvidia_island/prereq.rs) and `dry-nv-island.elf` (`SecDomain::Userspace`, spawned only in explicit `hwcheck_mode=nvidia_exp` / `nvidia_exp=1` boots):

| Command | Output & Honest Telemetry Rules |
| :--- | :--- |
| `nvidia-smi` | Renders honest `nvidia-smi`-style status table from live `NvIslandStatusWire` IPC: reports NVIDIA PCI `VID:DID` / BDF when present, driver level `Level 1 - DRIVER ISLAND READY`, status **`BLOCKED`**, reason string, and **`N/A` / `[Not Supported]`** for unavailable NVML metrics (temperature, power draw, clocks, VRAM usage, ECC, P-state). Zero fabricated telemetry. |
| `nvidia-smi -L` | Lists discovered NVIDIA GPU (`10de:*`) or reports no supported/active NVIDIA device (`BLOCKED`, zero MMIO bind). |
| `nvidia-smi -q` | Prints detailed query section including PCI/BAR metadata, island PID/generation, and all `14` prerequisites (`PASS` vs `BLOCKED`/`FAIL`). |
| `dry gpu status` | Summarizes cross-vendor GPU view: GOP framebuffer state, Intel iGPU (`8086`), VirtIO-GPU, and NVIDIA (`10de`) Level-1 island status. |
| `dry gpu nvidia status` | Queries `NvIslandStatusWire` (same live IPC status backing as `nvidia-smi`). |
| `dry gpu nvidia prereqs` | Displays itemized breakdown of all `14` fail-closed Level-1 prerequisites (`pci_device_10de`, `pci_bar_layout`, `iommu_dmar_enforcing`, `iommu_int_remap`, `vfio_pci_assign`, `cpuid_vmx_cap`, `vtx_hypervisor`, `microvm_guest_img`, `gsp_fw_ucodes`, `spdm_cert_verify`, `openrm_os_shim`, `prime_shm_fence`, `mac_island_confin`, `nvidia_exp_optin`). |
| `dry gpu nvidia export` | Triggers `IPC_OP_SUP_NV_EXPORT` to write and read-back verify the `7+1` `nvidia_exp/` file bundle to `DRYOS_LOGS` (`VERIFIED files=8 (7 data + manifest.sha256)` or `BLOCKED`). |

Note: Under default `safe` mode (`p18nv/safe_gating`), `nvidia_exp_optin` (`#14`) is unmet, `dry-nv-island.elf` remains **unspawned**, and all NVIDIA commands honestly return `BLOCKED` (`spawned=0`).

---

## 4. Ascension 9.0 Interactive Shell, VFS, Network, Linux ABI & Ergonomics Commands

| Command | Scope / Behavior | Notes & Safety |
| :--- | :--- | :--- |
| `pwd`, `cd`, `mkdir`, `rmdir`, `cp`, `mv` | Hierarchical VFS path navigation and file manipulation on `/ram` and writable `/persist`. | Enforces `owner_comp` write isolation (`INV-SEC`); `/bin` and `/boot` stay read-only. |
| `exec <path>` | Dynamically spawns an ELF64 binary from `/bin` (`dry-bin.tar`) or `/persist` via `Frame2M` (`SPAWN_FLAG_VFS_ELF`). | Zero new kernel syscalls; validated in `p19b` (`exec /bin/dry-probe.elf`). |
| `linux-run <path>` | Launches `dry-linux-abi.elf` (`SPAWN_FLAG_SYSCALL_REDIRECT`) to run a static `ET_EXEC` x86-64 Linux ELF (e.g., `linux-run /bin/lx-hello.elf`). | Supports static `ET_EXEC` ELFs (`p19d`: `13` syscalls, `rc=42`); dynamic `ET_DYN`/glibc ELFs unsupported. |
| `ifconfig`, `ping`, `nslookup`, `httpget` | Userspace TCP/UDP/DHCP/ARP/ICMP/DNS/HTTP/1.0 commands backed by `dry-net.elf` over `dry-lx-net` (`8139cp`). | `VERIFIED_QEMU` (`p19c`); physical `AX201` Wi-Fi / `r8168` Ethernet remain `UNSUPPORTED`. |
| `i2c-hid-test` | Spawns out-of-tree SDK island `dry-i2c-hid.elf` (`ABI v1`), validates report parsing and `#UD` `gen=2` recovery. | Safe passive island (`ALLOW_PHYSICAL_I2C_WRITES = false`, zero physical LPSS I2C MMIO writes). |
| `clip set <text>`, `clip get` | Reads/writes the `dry-display` compositor clipboard buffer (`IPC_OP_WM_CLIP_SET=182` / `IPC_OP_WM_CLIP_GET=183`, max `512 B`). | Shared with `dry-editor` (`Ctrl+C` / `Ctrl+V`). |
| `>`, `>>`, `\| grep`, `\| head`, `\| wc` | Shell output redirection and bounded single-stage pipelines (`userspace/src/shell_pipe.rs`). | Unsupported RHS verbs (e.g. `\| cat`) keep strict `UnsupportedPosix(b'\|')` rejection (`--p3-only`). |
| `open editor [<path>]` | Opens `dry-editor` on a specific path (or default notes); `Ctrl+O` cycles `/persist/notes.txt`, `/ram/notes.txt`, `/ram/welcome.txt`. | Respects VFS compartment write isolation (`p19f`). |

