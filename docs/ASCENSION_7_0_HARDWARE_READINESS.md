# DRY OS Ascension 7.0 — Hardware Readiness Audit & Milestone A Fixes (`ASCENSION_7_0_HARDWARE_READINESS.md`)

**Branch:** `ascension-7.0` (from `ascension-6.0` @ `f104e0e`)  
**Target Hardware (Not Yet Bare-Metal Verified):** Lenovo IdeaPad Gaming 3 15IHU6 (`82K1`), Intel Core Tiger Lake-H, Iris Xe (`8086:9a49`), NVIDIA GeForce RTX 3050 Ti Laptop GPU (`10de:25a0`, GA107M muxless Optimus)  
**Physical Execution Claimed Here:** **NONE** (`VERIFIED_HARDWARE = 0` — all dynamic checks below are `VERIFIED_QEMU`)

---

## 1. Honesty Status Legend

| Label | Meaning |
| :--- | :--- |
| `VERIFIED_QEMU` | Executed and asserted end-to-end inside QEMU/KVM + OVMF (`x86_64`) with real emulated devices (`intel-iommu`, `virtio-gpu-pci`, `qemu-xhci`, `nvme`, `rtl8139`, `ivshmem-plain`, `edu`, `bochs-display`) or host unit tests. |
| `VERIFIED_HARDWARE` | **Not used anywhere in Milestone A.** Reserved exclusively for observed bare-metal boots/tests on physical silicon. |
| `PARTIAL` | Mechanism implemented for bounded subsets or QEMU hardware; missing bare-metal paths remain. |
| `UNSUPPORTED` | Explicitly fail-closed or not implemented (e.g., AMD-Vi SLPT translation, NVIDIA GSP/OpenRM runtime). |

---

## 2. Independent 10-Area Code Audit of Ascension 6.0

An independent line-by-line audit of `ascension-6.0` (`f104e0e`) before physical bring-up surfaced 12 concrete correctness, isolation, and hardware-compatibility defects across kernel and userspace:

| # | Area Audited | Key Ascension 6.0 Strength | Hardware-Readiness / Security Gap Discovered | Milestone A Fix & Status |
| :--- | :--- | :--- | :--- | :--- |
| 1 | **Intel VT-d & DMA Isolation** (`kernel/src/iommu/`) | Per-BDF SLPT (`48/39-bit`), RMRR pre-mapping, `FRCD` fault polling, active GOP DRHD deferral | (a) Hardcoded `BOOT_RMRR_DID = 65` rejected real VT-d units with `ND = 16` or `64` (`nd <= 65` -> `FailedClosed`). (b) `DMAR` bridge hop reading `0xff` bricked entire `DMAR` instead of skipping absent bridges; `scope_type` (`3` IOAPIC / `4` HPET) matched as PCI BDFs; type `2` bridge downstream buses unhandled. (c) `init_vtd_units` lacked multi-unit rollback and took caps from unit 0 only. (d) `map_dma_range_iova` lacked `checked_add`/max-AW check and IOTLB-fail SLPT rollback; `vtd.rs` at `878` lines neared ceiling. | Fixed (`A-01`, `A-02`, `A-04`): `BOOT_RMRR_DID = 1`, `pick_did(2..max_domains)`, split `vtd_domain.rs`, multi-DRHD `rollback_all_units`, `min` AW & `AND` coherency, `overlaps_rmrr`, `is_device_coherent`. `VERIFIED_QEMU` |
| 2 | **PCIe Discovery, MCFG/ECAM & BARs** (`kernel/src/{acpi,pci_cfg,pci_window,pci}.rs`) | `MCFG` ECAM mapping, 64-slot registry, non-RAM BAR verification, `DEV_MAP_OP_CFG_RW` whitelist | (a) `acpi.rs` parsed `DMAR` before `MCFG` (disabling ECAM during DMAR bridge reads). (b) `pci_cfg.rs` destructively probed BARs & cleared `BME=0` on unclaimed classes `(0x01\|0x02\|0x0c\|0xff)` (VMD, SMBus, SPI) and misclassified `(0x03, 0x02)` muxless 3D controller into `PCI_OBJ_DISPLAY_AUX`. (c) `pci_window.rs` protected standard header + MSI/MSI-X, omitting PCIe `pcie_cap_off`, ReBAR, SR-IOV, ACS caps. (d) `pci.rs` legacy `sys_dev_map` leaked `CapMapRecord` on `copy_to_user` `EFAULT`. | Fixed (`A-02`, `A-03`, `A-05`): `MCFG` parsed before `DMAR`; narrow class matching (`(0x03,0x00\|_\|0x80)`, xHCI, NVMe, Net only); ECAM write protection on PCIe/ReBAR/SR-IOV/ACS; `detach_matching_cap_mapping_record` on `EFAULT`. `VERIFIED_QEMU` |
| 3 | **Ring-3 Device Broker & MSI** (`kernel/src/irq/msi.rs`, `userspace/src/sup_hw_services.rs`) | One-shot `IRQF_ONESHOT` masking before LAPIC EOI, PciDev single-owner claiming | (a) `bind_pci_msi` could claim a slot before `preserve_active_gop` rejection or `notif_cap` lookup failure without releasing it. (b) `sup_hw_services::spawn_pci_driver` leaked `pci_deleg` / `sup_cap` / `ep` on error paths and retained `pci_deleg` after spawn. | Fixed (`A-05`, `A-11`): `preserve_active_gop` -> `EPERM` before claim; `release_claimed_pci_slot` rollback on `bind_pci_msi` error; strict cleanup in `spawn_pci_driver`. `VERIFIED_QEMU` |
| 4 | **Driver Island Capability Model** (`kernel/src/cspace/`, `drivers/`) | Strict `RootSystem` vs `Userspace` macOS-free MAC, `NO_DELEGATE` on DMA/Vault | `dry_lx_rust_dma_sg_alloc` in `drivers/lx-net-8139cp-island/src/main.rs` pre-allocated and mapped frames via `sys_frame_alloc`+`sys_vspace_map` BEFORE calling `sys_dma_alloc_sg` (which already allocates & maps!). | Fixed (`A-12`): removed redundant pre-mapping loop; single capability-gated `sys_dma_alloc_sg` call. `VERIFIED_QEMU` |
| 5 | **DMA & GPU Service Buffer/Fence APIs** (`kernel/src/dma.rs`, `userspace/src/gpu_svc/`) | `IOVA != HPA` SG separation, `2 MiB` `WC`/`UC` MMIO, 4-backend `GpuBackend` trait | (a) `dma_op_alloc_sg` allocated 1 CSpace `Frame4K` slot per SG page (exhausting `256`-slot CNodes after large/repeated allocations), didn't wrap/recycle `iova_next` or check `pinned_frames`/RMRR collisions, and `dma_op_sync` lacked `clflush` when `!coherent`. (b) `backend_*.rs` `wait_fence` captured stale `cur_sig` snapshot in poll closure. | Fixed (`A-06`, `A-07`, `A-08`): direct PMM `alloc_pages` + `map_4k_cap_authorized` (`0` CSpace slots per SG page), outside-lock `release_sg_frames`, IOVA recycle/wrap & RMRR/pin collision checks, `clflush` on `!is_device_coherent`, explicit `|_\| false` unsignaled fence poll returning `-ETIMEDOUT`. `VERIFIED_QEMU` |
| 6 | **GOP-to-KMS Handover** (`kernel/src/display.rs`, `userspace/src/wm_ipc.rs`, `gpu_svc/handover.rs`) | 7-state FSM preserving GOP framebuffer on driver crash | (a) `sys_fb_map` left `CapMapRecord` attached on `copy_to_user` `EFAULT`. (b) `wm_ipc.rs` ignored `promote_to_kms` error on re-bind instead of entering `FallbackGop`. (c) `virtio_gpu.rs` leaked `vq_cap`/`cmd_cap` on init error `194`/`195`. | Fixed (`A-05`, `A-08`, `A-11`). `VERIFIED_QEMU` |
| 7 | **Firmware Integrity Store** (`userspace/src/fw_store.rs`) | Constant-time chunked SHA-256 verification | `register()` allowed duplicate entries silently shadowing lookups, and fixed `16 MiB` limit prevented validating `27 MiB` NVIDIA `gsp_ga10x.bin` / `58 MiB` `ucodes_ga10x.bin`. | Fixed (`A-09`): reject duplicate names with `EEXIST`; added `FwStreamVerifier` incremental SHA-256 verifier. `VERIFIED_QEMU` |
| 8 | **Linux Driver Compat Layer** (`drivers/libdry-lx-compat/`, `lx-drm-bochs-island/`, `lx-net-8139cp-island/`) | Unmodified upstream `v6.18` C sources SHA-256 verified (`bochs` decoupled from `libdry-lx-compat`) | See Area 4 (`dry_lx_rust_dma_sg_alloc` pre-mapping bug); `os-interface.h` NVIDIA audit covers `22/162` APIs statically (`nv_os_shim_check.c`). | Fixed `dry_lx_rust_dma_sg_alloc`; NVIDIA full `162`-API audit queued for Milestone D. `PARTIAL` |
| 9 | **MicroVM GPU IPC Prototype** (`tools/microvm_gpu_proto/`) | Functional 2-process SHM + CRC32 benchmark + KVM check | Single-frame `1920x1080` copy takes `~8.10 ms` p50 (exceeding 165 Hz `6.06 ms` frame budget); Python IPC overhead masks native C SHM/dirty-tile speedups. | Quantified in `--p16-only`; multi-tier C optimization scheduled in Milestone F. `PROTOTYPE` |
| 10 | **Scheduler, IRQ & IoRing Recovery** (`kernel/src/sched/`, `userspace/src/ioring.rs`) | Vyukov `StaticRunQueue` preemption fix, bounded xHCI timeouts | `IoRingSharedPage::pop_cqe` lacked `AUD-U5` `tail.wrapping_sub(head) > IORING_RING_CAP` corrupted-tail check (allowing spin/OOB index on hostile `dry-blk`), and `IoRingClient::connect` leaked frames/notifs on error. | Fixed (`A-10`). `VERIFIED_QEMU` |

---

## 3. Honest Audit of the 139 Ascension 6.0 Tests (`tools/qemu_boot_test.py`)

Reporting `139/139 PASS` without separating *what* each test exercises obscures real hardware confidence. Based on direct inspection of `tools/qemu_*.py`, the 139 suite checks break down as follows:

| Group (Count) | Validator Module(s) | What Runs Against Real/Emulated QEMU Devices | What Is Synthetic Unit / In-Memory State | What Is Serial/Log Marker Only |
| :--- | :--- | :--- | :--- | :--- |
| **Legacy Kernel + SMP (`58`)** | `qemu_boot_test.py`, `qemu_log_validators.py`, `qemu_user_validators.py` | x86-64 UEFI Limine boot, GDT/IDT/TSS, APIC/x2APIC SMP (`1..20` CPUs), CR3/VSpace NX/SMEP/SMAP page faults, `#UD`/`#GP`/`#PF` trap frames, `uaccess` SMAP/canonical negative faults (`16/16` security scenarios), `PMM` `leaked_frames=0` | In-kernel synthetic sched stress (`yield`/`park`/`recycle`), synthetic IPC ping-pong, untyped split/revoke unit tests | Structured `[DRY-*] PASS:` line parsing |
| **P1–P4 (`32`: `9` P1 + `14` P2 + `6` P3 + `3` P4)** | `qemu_p1..p4_validators.py`, `qemu_neg_validators.py`, `qemu_p3c_validators.py` | Live PS/2 IRQ1/IRQ12 QMP `send-key` & mouse movement (`query-mice` + pixel-diff `screendump`), ACPI MADT/IOAPIC/FADT S5 poweroff & reset, VirtIO-Blk `DRYFS_v1` CoW multi-boot disk persistence + PBKDF2 Vault passphrase lock/unlock, `#UD` shell crash recovery | Compositor layout/IPC negative fuzzing (`unauth_rej`), S3 `-ENOSYS` audit check | `[DRY-WM]` / `[DRY-SHELL]` / `[DRY-P2-SESSION]` marker checks |
| **2C-0 / 2C-1 / P7 GPU / 2C-2 (`5`: `2`+`1`+`1`+`1`)** | `qemu_p5..p8_validators.py` | Genuine Linux `v6.18` `virtio-rng.c` island reading real QEMU `virtio-rng-pci` + `#UD` hot-respawn (`gen=1->2`); real `virtio-gpu-pci` 2D DMA scanout + pixel-diff cursor + `#UD` recovery; `ivshmem-plain` BAR64 >4 GiB `WC`/`UC` + `qemu-edu` MSI `IRQF_ONESHOT` + live Intel `VT-d` SLPT DMA round-trip & unmapped rogue DMA `FRCD` fault; ECAM `DEV_MAP_OP_CFG_RW` & SG `IOVA != HPA` hardware DMA | `Mock Driver Island` SplitMix64/CRC32 mock payload (`p5a`), `verify_surface_pool_invariants()` in-memory pool check (`p8a`) | `[DRY-HW-PASS]`, `[DRY-HW-A6-PASS]` telemetry gates |
| **Glass / 2.0 / 2.1 / Brand (`13`: `9` + `4`)** | `qemu_p9..p11`, `qemu_p15_brand_icons_validators.py` | Multi-resolution QMP screendumps (`1024x768`, `1280x800`, `1920x1080`), mouse window dragging/tiling/scaling, `#UD` GPU recovery preserving desktop windows (`gen=2`) | Vector icon & theme math rendering inside desktop | PNG visual artifacts + layout bounds markers |
| **LxDRM / USB+NVMe / LxNet (`27`: `1`+`25`+`1`)** | `qemu_p12..p14_validators.py` | Genuine Linux `v6.18` `bochs.c` modesetting/color fills on `bochs-display` + `#UD` respawn (`p12a`); 3-boot QEMU `qemu-xhci` USB BOT mass-storage GPT/DRYFS RW + `nvme` RO SHA-256 verification (`p13`: 25 sub-checks); Linux `8139cp.c`+`mii.c` UDP/ARP packet loopback on QEMU `rtl8139` (`p14a`) | C compat shim self-checks when hardware optional | Per-boot `[DRY-USB]` / `[DRY-LX-NET]` markers |
| **Ascension 6.0 (`4`: `p16a..d`)** | `qemu_p16_ascension6_validators.py` | `p16a`: real QEMU boot without IOMMU -> fail-closed `exit=35` + real `amd-iommu` (`IVRS`) boot -> `FailClosed` non-DMA safety; `p16b`: live QEMU desktop + VirtIO-GPU 3D probe (`virgl=0`); `p16d`: host 2-process POSIX SHM `mmap` CRC32 IPC benchmark + real host `qemu-system-x86_64` probe | `p16b`: 4-backend `dry-gpu-svc` state machine (`UefiGop`, `VirtioGpu`, `LxBochs`, `SyntheticGpu`), `dry-fw-store`, `15`-stage `BrokerLifecycle` in-memory tests; `p16c`: Python `unittest` (`6/6`), `nv_os_shim_check.c` compile check, static header validators | `[DRY-GPU-SVC] PASS` / `[GPU-FW-VALIDATORS] PASS` |

**Key Audit Takeaway:** Roughly ~105 of the 139 sub-checks validate genuine QEMU-emulated hardware paths (x86-64 traps/SMP/VT-d/MSI/ECAM/xHCI/NVMe/VirtIO/rtl8139/bochs/QMP screendumps), while ~34 sub-checks are in-memory state machines (`SyntheticGpu`, `BrokerLifecycle`, `SurfaceBufferSet`), host Python/C unit checks, or firmware metadata parsers. **None validate physical Intel Iris Xe or NVIDIA GA107 silicon.**

---

## 4. Missing Negative Tests Added & Verified in Milestone A

| Test Added | File | Condition Exercised | Verification Status (`VERIFIED_QEMU`) |
| :--- | :--- | :--- | :--- |
| Dynamic DID exhaustion & 64-DID allocation | `kernel/src/iommu/tests.rs` | `pick_did(16, &used14) == None`; `pick_did(64, &used14) == Some(16)` | Ran in kernel boot self-test (`--p8-only`, `--p16-only`) — **PASS** |
| DMAR non-PCI scope rejection & bridge `type 2` routing | `kernel/src/iommu/tests.rs` | `scope_type` `3` (IOAPIC) / `4` (HPET) return `false` on `matches_pci_bdf`; `scope_type = 2` matches `sec_bus..=sub_bus` and rejects outside | Ran in kernel boot self-test (`--p8-only`, `--p16-only`) — **PASS** |
| IOVA overflow / zero-len / unaligned / `SLPT39` out-of-range clear | `kernel/src/iommu/tests.rs` | `map_dma_range_iova` rejects `u64::MAX - 0xfff`, `len=0`, unaligned `iova`; `clear_slpt_4k(..., 1<<39, false)` safely no-ops | Ran in kernel boot self-test (`--p8-only`, `--p16-only`) — **PASS** |
| Protected & unaligned PCI/ECAM writes | `userspace/src/bin/dry_hw_test.rs` | `sys_dev_cfg_write(edu_cap, 0x3c, 2, 0) == Err(EPERM)`; `sys_dev_cfg_write(edu_cap, 0x05, 2, 0) == Err(EINVAL)` | Ran in live QEMU `intel-iommu` (`--p8-only`) — **PASS** |
| `SYS_DEV_MAP` `EFAULT` bad user pointer check | `userspace/src/bin/dry_hw_test.rs` | `DEV_MAP_OP_QUERY_BARS` with `0xdead_0000` returns `EFAULT` (`-14`) without leak/crash | Ran in live QEMU (`--p8-only`) — **PASS** |
| 20x 16-page SG alloc/write/free (`320` pages > `256` CNode slots) & IOVA reuse | `userspace/src/bin/dry_hw_test.rs` | Verifies zero CSpace exhaustion (`leaked_frames=0`), IOVA deterministic recycle to `0xc000_0000`, `active_sg_bufs == 0` | Ran in live QEMU (`--p8-only`) — **PASS** |
| Corrupted `IoRingSharedPage` `cq_tail` delta guard | `userspace/src/ioring.rs` (`verify_ioring_corrupt_tail_guard`) | `cq_tail = 100 > IORING_RING_CAP` returns `None` without advancing `cq_head` | Invoked from `dry_hw_test` (`--p8-only`) — **PASS** |
| Duplicate firmware registration & `FwStreamVerifier` negative cases | `userspace/src/fw_store.rs` | Duplicate `register` -> `Err(EEXIST)`; empty/tampered stream -> `EmptyBlob`/`DigestMismatch`; chunked `chunks(7)` -> `Ok` | Executed in `--p16-only` (`p16b`) — **PASS** |
| Unsignaled GPU fence timeout (`-ETIMEDOUT`) | `userspace/src/gpu_svc/mod.rs` | `synth.wait_fence(9999, 128) == Err(ETIMEDOUT)` | Executed in `--p16-only` (`p16b`) — **PASS** |

---

## 5. Observed Milestone A Verification Results

- `python3 tools/qemu_boot_test.py --build-only`: **PASS** (`cargo clippy -D warnings` + release builds clean across `dry-userspace`, `dry-lx-rng`, `dry-lx-drm-bochs`, `dry-lx-net`, `dry-kernel`).
- `python3 tools/qemu_boot_test.py --p8-only`: **PASS** (`p8a_hw_foundations_vtd_msi_bar64_e2e`, `bar64=1`, `msi_vec=0x40`, `msi_p50=22.22us`, `vtd_faults=1`, `quiesce=5`, `leaked_frames=0`).
- `python3 tools/qemu_boot_test.py --p16-only`: **PASS (4/4)** (`p16a..p16d`, including `fw_store` + `gpu_svc` negative checks & `gpu_fw_validators` 6/6 + `proto_bench` + `virtio3d` probe).
- `python3 tools/qemu_boot_test.py --p12-only`: **PASS** (`p12a_linux_v618_bochs_drm_island`, `gen=2`, `quiesce=3`).
- `python3 tools/qemu_boot_test.py --p14-only`: **PASS** (`p14a_lx_net_8139cp`, `tx=2 rx=2 udp_echo=1 arp=1 restarts=1 leaked_frames=0`).
- Kernel syscall dispatcher frame check (`objdump -d ... dry-kernel`): `rust_syscall_dispatch` allocates `sub $0xf8,%rsp` = **`248 B <= 256 B`**.
- Source file ceiling (`wc -l`): all 21 modified/created Rust files strictly `< 1,000` lines (largest touched file: `kernel/src/dma.rs` at `887` lines; `kernel/src/iommu/vtd.rs` split into `573` + `419` lines).
