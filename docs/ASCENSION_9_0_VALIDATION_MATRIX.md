# DRY OS — Project Ascension 9.0 Validation Matrix (`ascension-9.0`)

**Date:** 22 Kasım 2026  
**Branch / Release Line:** `ascension-9.0` (`v0.9.0-dev`, continuing from `ascension-8.0`, Milestone 1 `5187395` .. Milestone 4 `35ea9cb` .. Milestone 5 `HEAD`)  
**Scope Boundary:** Every automated runtime result below is strictly **`VERIFIED_QEMU`** (QEMU/KVM + OVMF, and Mesa software `llvmpipe` for Virgl `p17d`) or **`PROTOTYPE`** (isolated host POSIX SHM IPC). **Physical bare-metal execution on Lenovo `82K1` remains `IMPLEMENTED_UNTESTED` (`0` `VERIFIED_HARDWARE` claims).**

---

## 1. Automated QEMU/OVMF, SDK & Image Verification Gates

| Suite / Command | Result | Key Observed Measurements & Guarantees | Label |
| :--- | :---: | :--- | :---: |
| `python3 tools/qemu_boot_test.py -j 4` *(Full Unified Regression)* | **170 / 170 PASS** | **`342.79 s`** (`jobs=4`): `58` Legacy + `9` P1 + `14` P2 + `6` P3 + `3` P4 + `2` 2C-0 + `1` 2C-1 + `1` GPU + `1` 2C-2 + `9` Glass/2.0/2.1 + `1` LxDRM + `25` USB/NVMe + `1` LxNet + `4` Brand + `4` Ascension 6.0 + `4` Ascension 7.0 + `21` Ascension 8.0 + **`6` Ascension 9.0 (`p19a..f`)**; zero failures, zero leaks. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p3-matrix` *(SMP/RAM Matrix)* | **5 / 5 PASS** | `1cpu/256M` (`5.67 s`, `hlt=548`), `2cpu/256M` (`5.58 s`, `hlt=1,216`), `4cpu/512M` (`5.51 s`, `hlt=2,588`), `8cpu/1G` (`5.92 s`, `hlt=4,547`), `20cpu/2G` (`9.96 s`, `hlt=7,573`); all `leaked_frames=0`. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p17-only` *(Ascension 7 Reliability & Virgl)* | **4 / 4 PASS** | Recorded under `-j 4`: `p17a` (`7.054 s`, `gen=3`, `pci_quiesce=4`), `p17b` (`5.197 s`, `persist_mounted=false`), `p17c` (`4.992 s`, `smp=4`, `vtd_faults=2`, `quiesces=5`), `p17d` (`65.007 s` under `-j 4`, `Virgl` `llvmpipe`, `px=0xffff0000`, `neg_err=0x1205`); all `leaked_frames=0`. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p18-only` *(Ascension 8 HWVAL & NVIDIA)* | **21 / 21 PASS** | Recorded under `-j 4`: `p18a` (`16.24 s`, `sess-0001..0002`), `p18b` (`15.02 s`, `sess-0003`), `9/9 p18c`, `p18d` (`9.27 s`), `p18e` (`7.76 s`), `p18nv/safe_gating` + `p18f` (`11.51 s`, `met=4`) + `6/6 p18g` (`77.71 s`); `fsck.fat -n` clean; `leaked_frames=0`. Output: `tools/compat_bench/results_p18_hwval.json`. | `VERIFIED_QEMU` |
| `python3 tools/qemu_boot_test.py --p19-only` *(Ascension 9 Living OS & Ergonomics)* | **6 / 6 PASS** | `p19a` (`11.153 s`), `p19b` (`15.382 s`), `p19c` (`7.799 s`), `p19d` (`6.816 s`), `p19e` (`5.541 s`), `p19f` (`11.441 s`); all `leaked_frames=0`. Output: `tools/compat_bench/results_p19_ascension9.json`. | `VERIFIED_QEMU` |
| `python3 tools/build_live_usb_image.py --build --hwval --verify-boot` | **PASS** | `kernel/build/dryos-ascension9-hardware-validation.img`: **`268,435,456 B`** (`256 MiB`) 3-part GPT (`DRYOS_ESP` 96 MiB + `DRYOS_PERSIST` 64 MiB + `DRYOS_LOGS` `194,527` sectors FAT32), **`30/32`** modules (`29` ELFs + `dry-bin.tar` `112,640 B`, `5` USTAR ELFs), `hwcheck=auto hwcheck_mode=safe` (`pass=20 fail=0 skip=2 blocked=2`), `sess-0001` `13` files + `manifest.sha256` + `fsck.fat -n` clean, `esp_and_persist_untouched=true`, `leaked_frames=0`, **`6.254 s`**. Full SHA-256: `f3614385538a3ba953b3fdec3317504254b3d2a679b1b954f77315f0108bf463`. | `VERIFIED_QEMU` |
| `cargo test --offline --manifest-path drivers/dry-driver-sdk/Cargo.toml` | **5 / 5 PASS** | `abi_hello`, `lifecycle_transitions`, `mmio_window_bounds`, `dma_region_bounds`, `ring_validation` (`5 passed; 0 failed`). | `VERIFIED_QEMU` |
| `cargo test --offline --manifest-path drivers/i2c-hid-island/Cargo.toml --lib` | **2 / 2 PASS** | `safety_gate_denies_physical_mmio_writes_by_default`, `touchpad_report_roundtrip_and_bounds` (`2 passed; 0 failed`). | `VERIFIED_QEMU` |

---

## 2. Ascension 9.0 Sub-Test Breakdown (`p19a` .. `p19f`)

- **`p19a_m1_stability_soak` (`11.153 s`):** Exercises repeated Ring-3 desktop application open/close cycles (`app_cycles=5`) and forced application crash-and-reap recoveries (`crash_cycles=2`) under `DRY Glass 2.1`, asserting responsive shell/compositor telemetry and `leaked_frames=0`.
- **`p19b_vfs_exec_and_hier_fs` (`15.382 s`):** Verifies read-only `/bin` USTAR archive (`dry-bin.tar`, `112,640 B`, `5` entries), hierarchical VFS commands (`pwd`, `cd`, `mkdir`, `rmdir`, `cp`, `mv`), `Frame2M` dynamic ELF spawning (`SPAWN_FLAG_VFS_ELF`) from `/bin/dry-probe.elf` (`probe_bytes=12792`), and copy-then-execute verification on `/persist` (`persist_exec_ok=true`, `leaked_frames=0`).
- **`p19c_net_stack_dns_http` (`7.799 s`):** Spawns `dry-net.elf` over `dry-lx-net.elf` (`8139cp.c` + `mii.c`) in QEMU SLIRP, completes DHCP/ARP configuration (`ifconfig`), ICMP `ping`, UDP DNS resolution against local host harness (`dns_port=35125`, `nslookup`), and TCP HTTP/1.0 fetch (`http_port=39347`, `httpget`) with `leaked_frames=0`.
- **`p19d_linux_abi_static_elf` (`6.816 s`):** Spawns `dry-linux-abi.elf` supervising unmodified static `ET_EXEC` Linux x86-64 ELF `/bin/lx-hello.elf` (`9,176 B`) via kernel `SYSCALL` redirect (`SPAWN_FLAG_SYSCALL_REDIRECT`, `0` new kernel syscalls); handles `13` Linux syscalls (`arch_prctl` TLS `%fs:0`, `brk`, `uname`, `openat`/`write`/`close`, `exit_group`), captures `stdout` (`DRY-OS`), and exits cleanly with `rc=42`, `leaked_frames=0`.
- **`p19e_usb_hub_and_i2c_hid_island` (`5.541 s`):** Boots QEMU `qemu-xhci` with an attached `usb-hub` + downstream `usb-kbd` and verifies 1-level hub cascade enumeration (`[DRY-USB] HUB_CASCADE root_port=5 hub_slot=1 ds_port=1 child_slot=2 speed=1`); dynamically spawns out-of-tree SDK island `dry-i2c-hid.elf` via `i2c-hid-test`, validates Driver Island ABI v1 handshake, triggers `#UD` fault recovery to `i2c_hid_gen=2`, and confirms `ALLOW_PHYSICAL_I2C_WRITES = false`, `leaked_frames=0`.
- **`p19f_desktop_clipboard_pipes_editor` (`11.441 s`):** Verifies compositor clipboard IPC (`IPC_OP_WM_CLIP_SET=182`, `IPC_OP_WM_CLIP_GET=183`) via `clip set`/`clip get`, shell redirection (`>`, `>>`) and pipes (`| grep` `grep_out_lines=4`, `| head`, `| wc`) while preserving `UnsupportedPosix(b'|')` on unsupported RHS verbs (`--p3-only` `6/6`), and `open editor /ram/pipe.txt` -> `Ctrl+C` -> `Ctrl+O` (`/persist/notes.txt`, honoring `owner_comp` write isolation) -> `Ctrl+V` -> `Ctrl+S` -> `cat /persist/notes.txt`, `leaked_frames=0`.

---

## 3. Code Quality, Binary & Source Hygiene Verification

| Constraint | Measured Result | Status |
| :--- | :--- | :---: |
| **`rust_syscall_dispatch` Stack Frame** (`objdump -d ... dry-kernel`) | `sub $0xf8,%rsp` = **`248 B`** (`<= 256 B`) | **PASS** |
| **New Kernel Syscalls / External Cargo Crates** | **`0`** new syscalls (`DRY Native Syscall ABI v0.2`, `0..=32` unchanged); **`0`** new Cargo dependencies | **PASS** |
| **RootInit Module Slots & `/bin` USTAR Archive** | **`30 / 32`** `MAX_LATCHED_MODULES` (`29` ELFs + `dry-bin.tar` `112,640 B`, `5` USTAR ELFs) | **PASS** |
| **Source File Line Ceiling (`< 1,000` LOC across `kernel/src`, `userspace/src`, `tools`, `drivers`)** | `LOC>=1000: []` (top 5: `kernel/src/ipc/tests.rs` `963`, `userspace/src/shell.rs` `955`, `userspace/src/lib.rs` `950`, `kernel/src/sched/mod.rs` `940`, `userspace/src/bin/dry_display.rs` `930`) | **PASS** |
| **Kernel Source Purity (`kernel/src/`)** | `0` `.c`/`.h`/`.S`/`.s` files; `0` GPL/Linux/OpenRM code | **PASS** |
| **`cargo clippy (-D warnings)`** | Clean across `dry-userspace`, `dry-lx-rng`, `dry-lx-drm-bochs`, `dry-lx-net`, `dry-sdk-demo`, `dry-i2c-hid`, `dry-kernel` | **PASS** |
| **Preserved Historical Images & Working Tree Safety** | `dryos-ascension{4..8}-*.img` untouched; pre-existing dirty/untracked files excluded from commits | **PASS** |

---

## 4. Honest Hardware & Scope Limitations

| Capability | Status Label | Honest Explanation |
| :--- | :---: | :--- |
| Physical Lenovo `82K1` Bare-Metal USB Execution | **`IMPLEMENTED_UNTESTED`** | Zero physical USB flashing or bare-metal runs permitted; `hardware_verified = false`, `0 VERIFIED_HARDWARE`. |
| Physical Intel Iris Xe (`8086:9a49`) Native Driver | **`PARTIAL`** | Read-only inventory, BAR audit, GOP handover preserve (`ALLOW_PHYSICAL_GPU_WRITES = false`); GuC/GGTT/eDP `165 Hz` silicon modeset unimplemented. |
| Physical Intel LPSS `I2C-HID` Touchpad Hardware Writes | **`PARTIAL`** | Out-of-tree `dry-i2c-hid.elf` SDK island, ABI v1 lifecycle, report parser, and `#UD` `gen=2` recovery are `VERIFIED_QEMU`; real LPSS I2C MMIO writes remain gated off (`ALLOW_PHYSICAL_I2C_WRITES = false`). |
| Physical Wi-Fi (`Intel AX201`) & Realtek `r8168` (`10ec:8168`) NIC | **`UNSUPPORTED`** | `dry-net.elf` is `VERIFIED_QEMU` over `dry-lx-net` (`8139cp.c`); physical laptop Wi-Fi and Realtek `r8168` drivers are not yet ported. |
| Dynamic-Linked (`ET_DYN` / `ld-linux` / glibc) Linux ELFs | **`UNSUPPORTED`** | `dry-linux-abi.elf` supports static `ET_EXEC` x86-64 Linux ELFs (`lx-hello.elf` `VERIFIED_QEMU`); dynamic ELF loading and multi-threaded `futex`/`clone` remain future work. |
| NVIDIA RTX 3050 Ti (`10de:25a0`) `dry-nv-island` | **`BLOCKED`** | Level-1 control island + `nvidia-smi` / `nvidia_exp/` `VERIFIED_QEMU`; real execution `BLOCKED` pending VT-d IR, VFIO, VT-x VMCS/VMM, GSP/SPDM, OpenRM guest, and PRIME zero-copy bridge (`4/14` met in QEMU). |
| VirtIO-GPU 3D / Virgl & `T5` Host SHM IPC | **`VERIFIED_QEMU`** *(Virgl `llvmpipe`)* / **`PROTOTYPE`** *(`T5`)* | Software `llvmpipe` in QEMU and host POSIX `MAP_SHARED` prototype only; cross-VM `DMA-BUF` and AMD-Vi SLPT remain `UNSUPPORTED`. |
