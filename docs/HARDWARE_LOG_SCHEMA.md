# DRY OS Project Ascension 8.0 — USB Persistence & Machine-Readable Log Schema (`HARDWARE_LOG_SCHEMA.md`)

> **Design Goal:** Crash-safe, deterministic, structured validation bundles written to dedicated Partition 3 (`DRYOS_LOGS`, FAT32) only after strict physical USB ancestry verification, readable both by human operators and automated analysis agents.

---

## 1. Directory & Session Layout (`DRYOS_LOGS` FAT32)

```text
/hardware-tests/
  sess-0001/
    summary.json
    hardware_inventory.json
    cpu_memory.json
    acpi_uefi.json
    pci_topology.json
    iommu_dma.json
    intel_gpu.json
    nvidia_gpu.json
    usb_storage.json
    driver_compatibility.json
    kernel.log
    test_results.json
    manifest.sha256
    nvidia_exp/                    (present when nvidia_exp=1 / dry gpu nvidia export runs)
      nvidia_island_status.json
      nvidia_prerequisites.json
      nvidia_pci_inventory.json
      nvidia_smi_output.txt
      nvidia_smi_list.txt
      nvidia_smi_query.txt
      nvidia_ipc_audit.json
      manifest.sha256
```

Each boot increments the monotonically unique session directory (`sess-0001`, `sess-0002`, ...) after scanning existing `/hardware-tests/` entries via `USB_OP_LOG_LIST_SESSIONS`, ensuring prior sessions are preserved across reboots.

---

## 2. 5-Gate USB Ancestry Verification & Fail-Closed Persistence Rules

Implemented in [userspace/src/usb/msc.rs](../userspace/src/usb/msc.rs) (`ANCESTRY_ALL_GATES = 0x1F`) and [userspace/src/usb/log_svc.rs](../userspace/src/usb/log_svc.rs):

| Gate Bit | Constant | Requirement Enforced |
| :---: | :--- | :--- |
| `bit 0` (`0x01`) | `ANCESTRY_GATE_XHCI_MSC` | Storage device is a live USB Mass Storage Bulk-Only Transport (`08:06:50`) endpoint behind `dry-usb` xHCI (never VirtIO-Blk or NVMe). |
| `bit 1` (`0x02`) | `ANCESTRY_GATE_GPT_CRC` | Protective MBR (`0xEE`) + Primary GPT Header CRC32 and Partition Entry Array CRC32 match on disk. |
| `bit 2` (`0x04`) | `ANCESTRY_GATE_DISK_GUID` | On-disk GPT `DiskGUID` matches the kernel boot `usb_disk_guid=<hex>` command-line token. |
| `bit 3` (`0x08`) | `ANCESTRY_GATE_ESP_SAME_DISK` | Partition 1 on the *same* physical USB disk carries the UEFI ESP type GUID (`C12A7328-F81F-11D2-BA4B-00A0C93EC93B`). |
| `bit 4` (`0x10`) | `ANCESTRY_GATE_LOGS_PART` | Partition 3 (`DRYOS_LOGS`) carries Basic Data GUID, lies cleanly after Partition 2, and passes FAT32 BPB + FSInfo bounds checks. |

**Crash-Safe Commit Protocol ([userspace/src/fat32_log.rs](../userspace/src/fat32_log.rs) & [userspace/src/hwdiag/mgr.rs](../userspace/src/hwdiag/mgr.rs)):**
1. All diagnostics buffer in RAM (`DiagSessionReport`) first — missing/unverified USB never fails diagnostics (`persist_status = BLOCKED` / `RAM_ONLY`).
2. Writes require `gate_mask == 0x1F`, valid writable FAT32 (`fat_mounted != 0 && fat_corrupt == 0 && read_only == 0`), and `hwcheck_mode != recovery`.
3. Each file is written to a staging name first, bounded-retried on transient errors, finalized to its target LFN path, flushed (`SCSI SYNCHRONIZE CACHE`), and **read back via `USB_OP_LOG_READ_FILE` with full SHA-256 comparison** against its in-RAM digest before `manifest.sha256` is committed and read-back verified.
4. No disk failure (`LOG_BLOCK_DISCONNECTED`, `LOG_BLOCK_WRITE_FAILED`, full FAT32, corrupt BPB) ever triggers writes to any fallback disk.

---

## 3. Session File Schemas (`12` Data Files + `manifest.sha256`)

Defined in [userspace/src/hwdiag/report_json.rs](../userspace/src/hwdiag/report_json.rs) and [userspace/src/hwdiag/mod.rs](../userspace/src/hwdiag/mod.rs):

| # | Filename | Top-Level JSON Keys / Content |
| :- | :--- | :--- |
| 1 | `summary.json` | `kernel_version`, `git_commit`, `session_id`, `boot_profile`, `environment` (`qemu_or_hypervisor` \| `bare_metal`), `hardware_verified` (`false`), `redacted_ids_by_default` (`true`), `pass`, `fail`, `skip`, `blocked`, `persistence_status` (`VERIFIED`/`BLOCKED`/`FAILED`/`RECOVERY`/`RAM_ONLY`), `usb_ancestry_gates`, `storage_ro` |
| 2 | `hardware_inventory.json` | `cpus_online`, `tsc_khz`, `usable_mib`, `pci_functions`, `gop_width`, `gop_height`, `vmd_detected`, `dgpu_detected` |
| 3 | `cpu_memory.json` | `cpus_online`, `tsc_khz`, `cpuid_features_hex`, `mem_usable_mib`, `mem_regions` |
| 4 | `acpi_uefi.json` | `acpi_audit_flags`, `acpi_oem_id`, `gop_width`, `gop_height`, `gop_pitch`, `secure_boot_state` (`"post_ebs_ro_skip"`) |
| 5 | `pci_topology.json` | `pci_passive_mode`, `devices[]` -> `{bdf, vid, did, class, subclass, passive, bme, msi, msix}` |
| 6 | `iommu_dma.json` | `iommu_audit_flags`, `dmar_present`, `vtd_hw_enforcing`, `int_remap`, `drhd_count`, `rmrr_count`, `max_aw_bits` |
| 7 | `intel_gpu.json` | `present`, `vendor` (`"0x8086"`), `device`, `allow_physical_gpu_writes` (`false`), `gop_handover`, `missing_native` (`["guc_upload", "ggtt_ppgtt", "rcs_ring", "edp_165hz_aux"]`), `status` (`"PARTIAL"`) |
| 8 | `nvidia_gpu.json` | `present`, `vendor` (`"0x10de"`), `device`, `driver_level` (`"Level 1 - DRIVER ISLAND READY"`), `native_execution_supported` (`false`), `microvm_requirements` (`["vtx_vmx", "vtd_int_remap", "vfio_dmar", "gsp_fw"]`) |
| 9 | `usb_storage.json` | `boot_disk_guid_redacted` (`redacted-XX..YY` via `redact_guid_hex`), `ancestry_gates`, `block_reason`, `dryos_logs_mounted`, `free_kib`, `internal_storage_ro`, `vmd_gated` |
| 10 | `driver_compatibility.json` | Driver island & compatibility baseline summary (`driver_compatibility` schema validated as valid JSON by `retrieve_dryos_logs.py`) |
| 11 | `kernel.log` | Kernel ring buffer capture read via `sys_dmesg_read` (ASCII/UTF-8 log lines) |
| 12 | `test_results.json` | Contains `results[]` array of structured diagnostic records (`id`, `category`, `status` [`PASS`/`FAIL`/`SKIP`/`BLOCKED`], `observed`, `expected`, `recovery`, `duration_us`, `error_code`, `hardware_verified`) |
| 13 | `manifest.sha256` | Standard SHA-256 lines `<64-hex-sha256>  <filename>` covering all `12` session files |

**Default Redaction Guarantee:** Full disk UUIDs/GUIDs and personal identifiers are redacted by default (`redact_guid_hex`: `"redacted-XX..YY"` exposing only prefix/tail hex bytes); `tools/retrieve_dryos_logs.py` enforces this check (`forbidden_raw_guid_hex`).

---

## 4. NVIDIA Level-1 Experimental Schema (`<session-id>/nvidia_exp/`)

Generated via [userspace/src/nvidia_island/session_logs.rs](../userspace/src/nvidia_island/session_logs.rs) when `nvidia_exp=1` or `dry gpu nvidia export` runs with valid `DRYOS_LOGS`:

1. `nvidia_island_status.json`: `session_id`, `level`, `level_name` (`"DRIVER_ISLAND_READY"`), `status` (`"BLOCKED"`), `hardware_verified` (`false`), `spawned`, `mac_isolated`, `island_alive`, `pid`, `generation`, `met_count`, `blocked_count`, `reason`, `nvml_telemetry` (`"N/A (BLOCKED)"`)
2. `nvidia_prerequisites.json`: `met`, `total` (`14`), `prerequisites[]` -> `{id, name, met, injected_fail, err_code, detail}`
3. `nvidia_pci_inventory.json`: `nvidia_present`, `bar_layout_ok`, `gpu_devices[]` -> `{bdf, vendor, device, bar0_bytes, extra_flags}`
4. `nvidia_smi_output.txt`: Identical output to `nvidia-smi` terminal command
5. `nvidia_smi_list.txt`: Identical output to `nvidia-smi -L` terminal command
6. `nvidia_smi_query.txt`: Identical output to `nvidia-smi -q` terminal command
7. `nvidia_ipc_audit.json`: `hwval_flags`, `vm_restarts`, `corrupt_ipc_rejected`, `unauth_cap_rejected`, `timeout_handled`, `last_ipc_err`
8. `manifest.sha256`: `<64-hex-sha256>  <filename>` for all `7` files above (`export_files = 8`).
