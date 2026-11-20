# DRY OS — Ascension 8.0 GPU Compatibility & NVIDIA Driver Island Status (`GPU_COMPATIBILITY_STATUS.md`)

**Branch:** `ascension-8.0`  
**Honesty Policy:** No unexecuted GPU initialization, modesetting, firmware loading, or NVML hardware telemetry is claimed. Capabilities reflect only verified QEMU execution and compile/audit contracts.

---

## 1. Per-Backend Status Matrix

| GPU / Display Path | Execution Status | Kernel Privilege / Isolation | Verified Scope (`ascension-8.0`) | Remaining Gaps |
| :--- | :--- | :--- | :--- | :--- |
| **UEFI GOP Framebuffer** | **`VERIFIED` (QEMU/OVMF)** | Kernel `dry-display` `2 MiB` mapping + GOP fallback preserved | Always active fallback in `safe`, `desktop_safe`, `recovery`, `nvidia_exp`, and on GPU crash rollback (`--gpu-only`, `--p17-only`) | Physical Lenovo `82K1` `1920x1080` panel visual verification (`operator-gated`) |
| **VirtIO-GPU 2D (`1af4:1050`)** | **`VERIFIED_QEMU`** | Ring-3 `dry-gpu` (`SecDomain::RootSystem` + VT-d `SLPT`) | `p7a` (`4.86s`, `--gpu-only`), M4 bounds hardening on BAR notify offsets, `submit_raw_cmd` response size, and scanout overflow (`userspace/src/virtio_gpu.rs`) | QEMU virtual device only |
| **VirtIO-GPU 3D / Virgl** | **`VERIFIED_QEMU`** | Ring-3 `dry-gpu` + `.toolchains/virgl-env` Mesa `llvmpipe` | `p17d` (`41.87s`, `--p17-only`, `capsets=2`, pixel `0xffff0000` readback + `-0x1205` negative parameter test, `leaked_frames=0`) | Host-accelerated Virgl hardware GL |
| **Intel Iris Xe (`8086:9a49`)** | **`PARTIAL` / Passive Contracts Ready** | Ring-3 `userspace/src/intel_tgl_prep.rs`; `ALLOW_PHYSICAL_GPU_WRITES = false` | BAR0/BAR2 validation, CSS firmware header + streaming SHA-256 verification, GGTT PTE & ring contracts verified (`[DRY-INTEL-PREP] PASS`); `hwdiag` `GPU-01-INTEL-XE` | Physical `8086:9a49` MMIO reads, GuC CTB, GGTT binding, and eDP-1 flip (`operator-gated`) |
| **NVIDIA RTX 3050 Ti (`10de:25a0`, GA107M)** | **`Level 1 — DRIVER ISLAND READY` (`BLOCKED`)** | `dry-nv-island.elf` (`0x0218`, slot `#29/32`) confined strictly to `SecDomain::Userspace`; zero kernel GPL/OpenRM code | Safe-mode non-spawn gate (`p18nv/safe_gating`), `nvidia_exp` Level-1 E2E (`p18f`, `9.91s`), 7 CLI commands, 7+1 `nvidia_exp/` logs + `fsck.fat -n`, `6/6` negative fault cases (`p18g`) | Prerequisites `#04`–`#12` (VT-d IR, VFIO, VT-x VMM, MicroVM, GA10x GSP-RM/WPR2, SPDM, full OpenRM `149/171`, PRIME SHM/fences) |

---

## 2. Mandatory NVIDIA Driver Island Architecture (M4)

To make NVIDIA readiness measurable, fail-closed, and diagnosable without bloating `dry-kernel` with GPL/vendor code or fabricating telemetry:

1. **Explicit Opt-In Gating (`userspace/src/sup_nv_island.rs`):**
   - `dry-nv-island.elf` spawns **only** under `hwcheck_mode=nvidia_exp` / `nvidia_exp=1` (`HWCHECK_MODE_NVIDIA_EXP` / `HWVAL_FLAG_NVIDIA_EXP`).
   - Standard `safe`, `desktop_safe`, `extended`, and `recovery` boot profiles never launch `dry-nv-island.elf` (verified by `p18nv/safe_gating`).
2. **Unprivileged Isolation (`userspace/src/bin/dry_nv_island.rs`):**
   - Runs in `SecDomain::Userspace` (`sys_proc_kill(1) == Err(EPERM)`), performs zero MMIO/PCI writes and zero DMA allocations, revokes any unused transferred capability (`revoke_unused_cap`), rejects unauthorized badges (`EPERM`), rejects malformed IPC payloads (`EINVAL`), and cleanly survives supervised `#UD` injection (`restarts=1`, `leaked_frames=0`).
3. **14 Fail-Closed Prerequisites (`userspace/src/nvidia_island/prereq.rs`):**
   - Tracks `#01 pci_device_10de`, `#02 pci_bar_layout`, `#03 iommu_dmar_enforcing`, `#04 iommu_int_remap`, `#05 vfio_pci_assign`, `#06 cpuid_vmx_cap` (`CPUID.1:ECX[5]`), `#07 vtx_hypervisor`, `#08 microvm_guest_img`, `#09 gsp_fw_ucodes`, `#10 spdm_cert_verify`, `#11 openrm_os_shim`, `#12 prime_shm_fence`, `#13 mac_island_confin`, `#14 nvidia_exp_optin`.
   - Under QEMU (`nvidia_exp=1` with Intel VT-d + VMX enabled and no `10de:25a0` passthrough), `4/14` prerequisites are `MET` (`#03`, `#06`, `#13`, `#14`) and `10/14` are `BLOCKED`.
4. **Honest `nvidia-smi` & Terminal Commands (`userspace/src/shell_gpu_cmds.rs`, `smi_format.rs`):**
   - `nvidia-smi`, `nvidia-smi -L`, `nvidia-smi -q`, `dry gpu status`, `dry gpu nvidia status`, `dry gpu nvidia prereqs`, and `dry gpu nvidia export` share one formatter with exported log artifacts.
   - Unavailable telemetry (temperature, clock speeds, power draw, VRAM allocation, driver version, CUDA version) prints explicitly as `N/A` / `BLOCKED`.
5. **USB Persistence Bundle (`userspace/src/nvidia_island/session_logs.rs`):**
   - Writes `nvidia_island_status.json`, `nvidia_prerequisites.json`, `nvidia_pci_inventory.json`, `nvidia_smi_output.txt`, `nvidia_smi_list.txt`, `nvidia_smi_query.txt`, `nvidia_ipc_audit.json`, and `manifest.sha256` under `hardware-tests/<sess>/nvidia_exp/`.
   - Repeated `dry gpu nvidia export` re-verifies `DRYOS_LOGS` ancestry/writable state and compares SHA-256 read-back via 3-segment `USB_OP_LOG_READ_FILE` (`userspace/src/usb/log_svc.rs`).

---

## 3. Verified Validation Evidence (`ascension-8.0` M4)

- `python3 tools/qemu_boot_test.py --p18-only`: **21/21 PASS** (`p18nv/safe_gating`, `p18f_nvidia_exp_level1_e2e` `9.91s`, `6/6 p18g` fault cases `nv1_no_gpu`..`nv6_unauth_cap`, `fsck.fat -n` clean, `leaked_frames=0`, canary NVMe untouched).
- `python3 tools/qemu_boot_test.py --p17-only`: **4/4 PASS** (`p17d` Virgl 3D `41.87s`).
- `python3 tools/qemu_boot_test.py --gpu-only`: **1/1 PASS** (`p7a` `4.86s`).
- `cargo clippy (-D warnings)`: **0 errors / 0 warnings** across all crates; `kernel/src/` contains zero C/GPL/OpenRM code.
