# DRY OS — Project Ascension 8.0 Validation Matrix (`ascension-8.0`)

**Date:** 20 Kasım 2026  
**Branch / Release Line:** `ascension-8.0` (continuing from `ascension-7.0` @ `b0387ab`, Milestone 1 `c7faf46` .. Milestone 4 `af957da` .. Milestone 5)  
**Scope Boundary:** Every automated runtime result below is strictly **`VERIFIED_QEMU`** (QEMU/KVM + OVMF, and Mesa software `llvmpipe` for Virgl `p17d`) or **`PROTOTYPE`** (isolated host POSIX SHM IPC). **Physical bare-metal execution on Lenovo `82K1` remains `IMPLEMENTED_UNTESTED` (`0` `VERIFIED_HARDWARE` claims).**

---

## 1. Automated QEMU/OVMF, SDK & Image Verification Gates

| Suite / Command | Result | Key Observed Measurements & Guarantees | Label |
| :--- | :---: | :--- | :---: |
| `python3 tools/qemu_boot_test.py` *(Full Unified Regression)* | **164 / 164 PASS** | `58` Legacy + `9` P1 + `14` P2 + `6` P3 + `3` P4 + `2` 2C-0 + `1` 2C-1 + `1` GPU + `1` 2C-2 + `9` Glass/2.0/2.1 + `1` LxDRM + `25` USB/NVMe + `1` LxNet + `4` Brand + `4` Ascension 6.0 + `4` Ascension 7.0 + **`21` Ascension 8.0**; zero failures, zero leaks. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p3-matrix` *(SMP/RAM Matrix)* | **5 / 5 PASS** | `1cpu/256M` (`5.09s`, `hlt=567`), `2cpu/256M` (`4.17s`, `hlt=1,249`), `4cpu/512M` (`5.17s`, `hlt=2,578`), `8cpu/1G` (`5.26s`, `hlt=5,260`), `20cpu/2G` (`6.63s`, `hlt=13,447`); all `leaked_frames=0`. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p17-only` *(Ascension 7 Reliability & Virgl)* | **4 / 4 PASS** | `p17a` (`5.21s`, `gen=1->2->3`, `quiesce=4`), `p17b` (`5.04s`, volatile `persist=0`), `p17c` (`4.36s`, `smp=4`, `vtd_faults=2`, `quiesces=5`), `p17d` (`15.52s`, `Virgl` `capsets=2`, `px=0xffff0000`, `neg_err=0x1205`); all `leaked_frames=0`. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p18-only` *(Ascension 8 HWVAL & NVIDIA)* | **21 / 21 PASS** | `p18a` (`12.84s`), `p18b` (`12.79s`), `9/9 p18c` (`61.33s`), `p18d` (`10.02s`), `p18e` (`7.41s`), `p18nv/safe_gating`, `p18f` (`10.18s`), `6/6 p18g` (`nv1..nv6`); `fsck.fat -n` clean; `leaked_frames=0`. Output: `tools/compat_bench/results_p18_hwval.json`. | `VERIFIED_QEMU` |
| `python3 tools/build_live_usb_image.py --build --hwval --verify-boot --output kernel/build/dryos-ascension8-hardware-validation.img` | **PASS** | `268,435,456 B` (`256 MiB`) 3-part GPT (`DRYOS_ESP` 96 MiB + `DRYOS_PERSIST` 64 MiB + `DRYOS_LOGS` `194,527` sectors FAT32), `29/32` Ring-3 ELFs, `hwcheck=auto hwcheck_mode=safe` (`pass=20 fail=0 skip=2 blocked=2`), `sess-0001` `13` files + `manifest.sha256` + `fsck.fat -n` clean, `esp_and_persist_untouched=true`, `leaked_frames=0`, **`7.316s`**. Full SHA-256: `c267a74e7c0620f15018651134dfe2ae82518cc7d866515488b2fce1056e0a8f`. | `VERIFIED_QEMU` |
| `cargo test --manifest-path drivers/dry-driver-sdk/Cargo.toml` | **5 / 5 PASS** | `abi_hello_accepts_v1_and_rejects_mismatch`, `lifecycle_transitions_enforce_valid_states`, `mmio_window_bounds_and_alignment`, `dma_region_bounds_and_overflow`, `wire_sizes_and_ring_validation` (`5 passed; 0 failed`, `0.00s`). | `VERIFIED_QEMU` |
| `python3 tools/retrieve_dryos_logs.py --list-candidates` & `retrieve_and_verify` | **PASS** | Host `--list-candidates` safely inspects without mounting (`No DRYOS_LOGS partition currently attached.`); `retrieve_and_verify` verified inside `--hwval --verify-boot` (`13` files) and `p18a`/`p18b`/`p18f` (`manifest.sha256` + raw disk GUID redaction check). | `VERIFIED_QEMU` |

---

## 2. Ascension 8.0 Sub-Test Breakdown (`p18a` .. `p18g`)

- **`p18a_hwval_safe_auto_and_canary` (`12.84s`):** Boots with Intel VT-d (`hwcheck=auto hwcheck_mode=safe`), verifies passive PCI mode (`pci_passive=1`), passes all `5` xHCI/GPT/GUID ancestry gates (`0x1f`), completes `AUTO_COMPLETE` + 13-file FAT32+LFN write + read-back SHA-256, executes all `15` `dry` diagnostic CLI commands (`help` + `14` active commands), preserves 2 canary disks (`NVMe` & `VirtIO-Blk`) and `ESP`/`DRYOS_PERSIST` prefixes byte-identical, and extracts `sess-0001` & `sess-0002` via `retrieve_dryos_logs.py`.
- **`p18b_reboot_recovery_and_multisession` (`12.79s`):** Reboots into `RECOVERY / LOG VIEWER` (`persist=RECOVERY`, zero writes, Partition 3 SHA-256 unchanged, `RECOVERY_SESSIONS count=2`), then reboots into safe auto mode and appends `sess-0003` cleanly.
- **`p18c_negative_fault_matrix` (`9/9 PASS`, `61.33s`):** `neg1_missing_p3` (`BLOCKED`), `neg2_write_fail` (`LOG_BLOCK_WRITE_FAILED` / RAM buffer retained), `neg3_wrong_guid` (`LOG_BLOCK_UNVERIFIED_BOOT_GUID`), `neg4_corrupt_bpb` (corrupt FAT32 rejected), `neg5_full_fat32` (insufficient clusters safely handled), `neg6_usb_disconn` (`LOG_BLOCK_DISCONNECTED`, canary NVMe untouched), `neg7_bad_acpi` (`ACPI-01-TABLES-CRC` fail handling), `neg8_bad_pci_bar` (`PCI-01-ENUM-BARS` handling), `neg9_no_iommu` (fail-closed `IOMMU-01-VTD-DMA` `BLOCKED`).
- **`p18d_security_hardening` (`10.02s`):** Exercises Milestone 2 kernel/userspace IPC donation cleanup, `sys_dev_map` quiesce rollback, RMRR/DMAR DMA collision guards, unused capability revocation, `dry-usb` unprivileged fault containment (`EPERM`), `leaked_frames=0`, NVMe untouched.
- **`p18e_driver_sdk_lifecycle` (`7.41s`):** Verifies Driver Island ABI v1 (`ISLAND_ABI_VERSION_V1 = 0x0001_0000`, `dry-sdk-demo.elf` `#28/32`): protocol handshake `ISLAND_OP_HELLO (157)` / `BROKER_OP_ISLAND_HELLO (169)`, version mismatch rejection, `#UD` fault respawn to `gen=2`, clean `ISLAND_OP_QUIESCE_UNLOAD (158)` / `BROKER_OP_QUIESCE_UNLOAD (180)` teardown.
- **`p18nv/safe_gating` & `p18f_nvidia_exp_level1_e2e` (`10.18s`):** Proves `dry-nv-island.elf` (`#29/32`, `SecDomain::Userspace`, zero MMIO/DMA) stays **unspawned** under default `safe` mode (`p18nv/safe_gating`); when booted with `nvidia_exp=1`, runs all `7` NVIDIA/GPU commands (`nvidia-smi`, `-L`, `-q`, `dry gpu status`, `dry gpu nvidia status|prereqs|export`), reports honest `BLOCKED` / `N/A`, evaluates `14` prerequisites (`met=4`), and exports + read-back verifies `7+1` `nvidia_exp/` files (`fsck.fat -n` clean).
- **`p18g` NVIDIA Fault Injection Matrix (`6/6 PASS`, included in `60.87s` with `p18nv`):** `nv1_no_gpu`, `nv2_bad_bar`, `nv3_vm_crash`, `nv4_timeout`, `nv5_corrupt_ipc`, `nv6_unauth_cap` — all cleanly fail-closed as `BLOCKED` with zero leaked frames or unsafe MMIO/DMA.

---

## 3. Code Quality, Binary & Source Hygiene Verification

| Constraint | Measured Result | Status |
| :--- | :--- | :---: |
| **`rust_syscall_dispatch` Stack Frame** (`objdump -d ... dry-kernel`) | `sub $0xf8,%rsp` = **`248 B`** (`<= 256 B`) | **PASS** |
| **RootInit IPC Struct & Module Table** | `RootInitBootExtInfo = 192 B` (`<= 256 B`); `29 / 32` `MAX_LATCHED_MODULES` used | **PASS** |
| **Source File Line Ceiling (`< 1,000` LOC across all `.rs` & `.py`)** | `OVER_1000: 0 []` (highest: `kernel/src/ipc/tests.rs` `963`, `wm.rs` `962`, `dry_shell.rs` `960`, `qemu_boot_test.py` `950`, `lib.rs` `938`) | **PASS** |
| **Kernel Source Purity (`kernel/src/`)** | `0` `.c`/`.h`/`.S` files; `0` GPL/Linux/OpenRM markers (`CFILES: [] GPL_HITS: []`) | **PASS** |
| **`cargo clippy (-D warnings)`** | Clean across `dry-userspace`, `dry-lx-rng`, `dry-lx-drm-bochs`, `dry-lx-net`, `dry-sdk-demo`, `dry-kernel` | **PASS** |
| **Preserved Historical Images & Working Tree Safety** | `dryos-ascension{4,5,6,7}-live-usb.img` unchanged; `experiments/h1-sched/src/*`, pre-existing PNGs, and `Cargo.lock` excluded from commits | **PASS** |

---

## 4. Honest Hardware & Scope Limitations

| Capability | Status Label | Honest Explanation |
| :--- | :---: | :--- |
| Physical Lenovo `82K1` Bare-Metal USB Execution | **`IMPLEMENTED_UNTESTED`** | Zero physical USB flashing or bare-metal runs permitted; `hardware_verified = false`, `0 VERIFIED_HARDWARE`. |
| Physical Intel Iris Xe (`8086:9a49`) Native Driver | **`PARTIAL`** | Read-only inventory, BAR audit, GOP handover preserve (`ALLOW_PHYSICAL_GPU_WRITES = false`), `intel_tgl_prep.rs`; GuC/GGTT/eDP `165 Hz` silicon modeset unimplemented. |
| NVIDIA RTX 3050 Ti (`10de:25a0`) `dry-nv-island` | **`BLOCKED`** | Level 1 isolated control compartment + `14`-prereq evaluator + `nvidia-smi` / `nvidia_exp/` `VERIFIED_QEMU`; real execution `BLOCKED` pending VT-d IR, VFIO, VT-x VMCS/VMM, GSP/SPDM, OpenRM guest, and PRIME zero-copy bridge (`4/14` met in QEMU). |
| VirtIO-GPU 3D / Virgl | **`VERIFIED_QEMU`** *(Mesa `llvmpipe`)* | Software `llvmpipe` in QEMU `.toolchains/virgl-env/`, not physical GPU 3D hardware. |
| `T5` SHM Slot-Index Zero-Copy IPC | **`PROTOTYPE`** | Host Linux POSIX `MAP_SHARED` prototype only; cross-VM guest `DMA-BUF` remains `UNSUPPORTED`. |
| AMD-Vi SLPT / IOTLB Remapping | **`UNSUPPORTED`** | `IVRS` ACPI detection enforces `FailClosed`; page-table remapping unimplemented. |
