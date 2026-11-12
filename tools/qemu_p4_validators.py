#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone 2B Persistent Storage (`dry-blk` VirtIO 1.0 Modern + Lock-Free `IoRing` + `DRYFS_v1` CoW),
Power-Cut Superblock Recovery (`R-05`), and `DRY Vault` Security & DMA Fault Confinement (`ADR-007`,
`INV-SEC-01..08`, `INV-CAP-08`, `R-03`) QEMU/KVM E2E Validators."""

import pathlib
import shutil
import time
from typing import Callable

from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints
from qemu_p1_validators import _count_diff_pixels
from qemu_p2_validators import _DesktopSessionHarness

ARTIFACT_DIR = pathlib.Path(
    "/home/zwannfrederick/.gemini/antigravity/brain/fcac1153-5c72-4469-803e-6ceaebf4f0cb"
)


def _virtio_blk_args(disk_path: pathlib.Path) -> list[str]:
    return [
        "-drive",
        f"file={disk_path},if=none,format=raw,id=hd0",
        "-device",
        "virtio-blk-pci,drive=hd0,disable-legacy=on",
    ]


def _create_raw_disk(disk_path: pathlib.Path, size_mib: int = 4) -> None:
    disk_path.unlink(missing_ok=True)
    with disk_path.open("wb") as f:
        f.truncate(size_mib * 1024 * 1024)


def run_p4a_virtio_blk_persist_across_reboots_e2e(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    """Phase P4-A E2E (2 Sequential QEMU Boots on the Same Raw VirtIO-Blk Disk):
    Boot #1 formats `DRYFS_v1` (`/persist`), writes `/persist/note.txt` + hardlink `/persist/alias.txt`,
    rotates the `DRY Vault` PBKDF2-HMAC-SHA256 passphrase (`dryos` -> `sovereign7`), applies `Tiling` +
    `High Contrast` in `Settings` (persisted to `/persist/desktop.cfg`), verifies authenticated `SessionLocked`
    rejection of wrong passphrase + unlock with `sovereign7`, and shuts down cleanly.
    Boot #2 mounts the same raw disk (`fresh=false`), verifies zero plaintext secrets on disk, restores
    `desktop.cfg` + `vault.sec (gen=2)`, reads back persisted files, and unlocks with `sovereign7`."""
    disk_path = build_dir / "disk_p4a_persist.img"
    _create_raw_disk(disk_path, size_mib=4)
    ppm_desktop = build_dir / f"{name}_desktop.ppm"
    ppm_locked = build_dir / f"{name}_locked.ppm"
    t0 = time.monotonic()

    # --- BOOT #1: Fresh format, persist files + Settings + Vault verifier, verify authenticated lock ---
    h1 = _DesktopSessionHarness(
        f"{name}_boot1",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp1 = h1.boot_to_desktop_ready()
        h1.wait_for("[DRY-PCI] PASS: virtio-blk ", wait_s=5.0)
        h1.wait_for("[DRY-BLK] VirtIO 1.0 Modern ready sectors=8192", wait_s=5.0)
        h1.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=true", wait_s=5.0)
        h1.wait_for("[DRY-VAULT] Sealed /persist/vault.sec (NO_DELEGATE) gen=1", wait_s=5.0)

        idx = len(h1.serial_lines)
        qmp1.type_ascii("write /persist/note.txt hello-sovereign-disk-v2b")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SHELL] CMD=write path=/persist/note.txt bytes=24", start_idx=idx)

        idx = len(h1.serial_lines)
        qmp1.type_ascii("cat /persist/note.txt")
        qmp1.send_qcodes(["ret"])
        h1.wait_for(
            '[DRY-SHELL] CMD=cat path=/persist/note.txt bytes=24 text="hello-sovereign-disk-v2b"',
            start_idx=idx,
        )

        idx = len(h1.serial_lines)
        qmp1.type_ascii("ln /persist/note.txt /persist/alias.txt")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SHELL] CMD=ln ok=true ino=", start_idx=idx)

        idx = len(h1.serial_lines)
        qmp1.type_ascii("vault-passwd dryos sovereign7")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SHELL] CMD=vault-passwd ok=true gen=2", start_idx=idx)

        idx = len(h1.serial_lines)
        qmp1.type_ascii("sync")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SHELL] CMD=sync tx_seq=", start_idx=idx)

        # Open `Files` (`Alt+3` -> defaults to `/persist`) and `Settings` (`Alt+4`).
        idx = len(h1.serial_lines)
        qmp1.send_qcodes(["alt", "3"], hold_ms=18)
        h1.wait_for("[DRY-FILES] Ready in CPL3 Userspace", start_idx=idx)

        idx = len(h1.serial_lines)
        qmp1.send_qcodes(["alt", "4"], hold_ms=18)
        h1.wait_for("[DRY-SETTINGS] Ready in CPL3 Userspace", start_idx=idx)

        # Row 0 (`Layout`) -> `Tiling (1)`, Row 3 (`Contrast`) -> `High Contrast (1)` -> persisted to `/persist/desktop.cfg`!
        idx = len(h1.serial_lines)
        qmp1.send_qcodes(["right"])
        h1.wait_for("[DRY-SETTINGS] APPLY layout=1", start_idx=idx)

        for _ in range(3):
            qmp1.send_qcodes(["down"])
            time.sleep(0.015)
        idx = len(h1.serial_lines)
        qmp1.send_qcodes(["right"])
        h1.wait_for("[DRY-SETTINGS] APPLY contrast=1", start_idx=idx)

        # Focus `Terminal` (`Alt+1`) and capture persistent desktop screenshot!
        idx = len(h1.serial_lines)
        qmp1.send_qcodes(["alt", "1"], hold_ms=18)
        h1.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        time.sleep(0.12)

        qmp1.screendump(ppm_desktop)
        docs_desk_png = root_dir / "docs" / "p4_persist_vault_desktop.png"
        docs_desk_png.parent.mkdir(parents=True, exist_ok=True)
        _ppm_to_png_and_verify(ppm_desktop, docs_desk_png)
        if ARTIFACT_DIR.exists():
            shutil.copyfile(docs_desk_png, ARTIFACT_DIR / "p4_persist_vault_desktop.png")

        # Lock session -> advances `GLOBAL_LEASE_EPOCH` -> requires real passphrase to unlock!
        idx = len(h1.serial_lines)
        qmp1.type_ascii("lock")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SUP] VAULT_LOCK revoked_to_epoch=2", start_idx=idx)
        h1.wait_for("[DRY-WM] SESSION state=2", start_idx=idx)
        time.sleep(0.06)

        # Wrong passphrase MUST be rejected and keep session locked (`fails=1`)!
        idx = len(h1.serial_lines)
        qmp1.type_ascii("wrongpass")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SUP] VAULT_UNLOCK auth=denied fails=1", start_idx=idx)
        time.sleep(0.06)

        qmp1.screendump(ppm_locked)
        lock_diff = _count_diff_pixels(ppm_desktop, ppm_locked)
        if lock_diff < 50_000:
            raise RuntimeError(f"[{name}] Expected visible authenticated Vault lock shield, diff={lock_diff}")
        docs_lock_png = root_dir / "docs" / "p4_vault_auth_lock.png"
        _ppm_to_png_and_verify(ppm_locked, docs_lock_png)
        if ARTIFACT_DIR.exists():
            shutil.copyfile(docs_lock_png, ARTIFACT_DIR / "p4_vault_auth_lock.png")

        # Correct passphrase (`sovereign7`) unlocks session!
        idx = len(h1.serial_lines)
        qmp1.type_ascii("sovereign7")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SUP] VAULT_UNLOCK auth=ok epoch=2", start_idx=idx)
        h1.wait_for("[DRY-WM] SESSION state=0", start_idx=idx)

        qmp1.type_ascii("shutdown")
        qmp1.send_qcodes(["ret"])
        s1_out, s1_kv = h1.finish_and_verify(require_mouse_irqs=False)
        hlt1 = [ln for ln in s1_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        if not hlt1 or _parse_kv_ints(hlt1[0]).get("pci_quiesce", 0) < 1:
            raise RuntimeError(f"[{name}] Expected pci_quiesce>=1 on Boot #1 teardown: {hlt1!r}")
    finally:
        h1.close()
        for p in (ppm_desktop, ppm_locked):
            p.unlink(missing_ok=True)

    # Binary audit of raw disk image between boots: plaintext passphrases MUST NEVER appear on disk!
    raw_disk_bytes = disk_path.read_bytes()
    if b"sovereign7" in raw_disk_bytes:
        raise RuntimeError(f"[{name}] SECURITY VIOLATION: plaintext passphrase found on raw VirtIO disk!")
    if b"hello-sovereign-disk-v2b" not in raw_disk_bytes:
        raise RuntimeError(f"[{name}] Expected persisted file payload in raw VirtIO disk blocks!")

    # --- BOOT #2: Fresh QEMU process mounting the SAME `disk_p4a_persist.img` ---
    h2 = _DesktopSessionHarness(
        f"{name}_boot2",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp2 = h2.boot_to_desktop_ready()
        mnt_ln = h2.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=false", wait_s=5.0)
        if "recovered=false" not in mnt_ln:
            raise RuntimeError(f"[{name}] Expected clean non-recovery mount on Boot #2: {mnt_ln!r}")
        h2.wait_for("[DRY-INIT] Restored desktop.cfg layout=1 theme=0x1", wait_s=5.0)
        h2.wait_for("[DRY-VAULT] Loaded sealed /persist/vault.sec gen=2", wait_s=5.0)

        idx = len(h2.serial_lines)
        qmp2.type_ascii("cat /persist/note.txt")
        qmp2.send_qcodes(["ret"])
        h2.wait_for(
            '[DRY-SHELL] CMD=cat path=/persist/note.txt bytes=24 text="hello-sovereign-disk-v2b"',
            start_idx=idx,
        )

        idx = len(h2.serial_lines)
        qmp2.type_ascii("cat /persist/alias.txt")
        qmp2.send_qcodes(["ret"])
        h2.wait_for(
            '[DRY-SHELL] CMD=cat path=/persist/alias.txt bytes=24 text="hello-sovereign-disk-v2b"',
            start_idx=idx,
        )

        idx = len(h2.serial_lines)
        qmp2.type_ascii("lock")
        qmp2.send_qcodes(["ret"])
        h2.wait_for("[DRY-WM] SESSION state=2", start_idx=idx)

        idx = len(h2.serial_lines)
        qmp2.type_ascii("sovereign7")
        qmp2.send_qcodes(["ret"])
        h2.wait_for("[DRY-SUP] VAULT_UNLOCK auth=ok", start_idx=idx)
        h2.wait_for("[DRY-WM] SESSION state=0", start_idx=idx)

        qmp2.type_ascii("shutdown")
        qmp2.send_qcodes(["ret"])
        _, s2_kv = h2.finish_and_verify(require_mouse_irqs=False)
        print(
            f"[PASS] {name}: 2x_boot_persisted=true pbkdf2_gen=2 lock_diff={lock_diff} "
            f"leaked_frames={s1_kv['leaked_frames']}/{s2_kv['leaked_frames']} ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h2.close()
        disk_path.unlink(missing_ok=True)


def run_p4b_powercut_and_superblock_recovery_e2e(
    name: str,
    esp_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Phase P4-B E2E (`R-05` Abrupt Power-Loss + Corrupted Superblock Slot 0 Fallback Recovery):
    Writes `/persist/atomic.txt`, corrupts Superblock Slot 0 CRC on disk (`corrupt-sb0-test`), and
    immediately kills QEMU (`SIGKILL`, zero clean unmount). Boot #2 verifies CRC32 fallback recovery
    from Slot 1 (`recovered=true`) and intact CoW file contents."""
    disk_path = build_dir / "disk_p4b_powercut.img"
    _create_raw_disk(disk_path, size_mib=4)
    t0 = time.monotonic()

    h1 = _DesktopSessionHarness(
        f"{name}_kill",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp1 = h1.boot_to_desktop_ready()
        h1.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=true", wait_s=5.0)

        idx = len(h1.serial_lines)
        qmp1.type_ascii("write /persist/atomic.txt cow-survived-powercut-77")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SHELL] CMD=write path=/persist/atomic.txt bytes=24", start_idx=idx)

        idx = len(h1.serial_lines)
        qmp1.type_ascii("corrupt-sb0-test")
        qmp1.send_qcodes(["ret"])
        h1.wait_for("[DRY-SHELL] CMD=corrupt-sb0-test ok=true", start_idx=idx)

        # Simulate sudden power cord pull (`SIGKILL` without `shutdown` or `IPC_OP_EXIT_CLEAN`)!
        h1.proc.kill()
        h1.proc.wait(timeout=5.0)
    finally:
        h1.close()

    h2 = _DesktopSessionHarness(
        f"{name}_recover",
        esp_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp2 = h2.boot_to_desktop_ready()
        rec_ln = h2.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=false", wait_s=5.0)
        if "recovered=true" not in rec_ln:
            raise RuntimeError(f"[{name}] Expected recovered=true after SB0 CRC corruption + SIGKILL: {rec_ln!r}")

        idx = len(h2.serial_lines)
        qmp2.type_ascii("cat /persist/atomic.txt")
        qmp2.send_qcodes(["ret"])
        h2.wait_for(
            '[DRY-SHELL] CMD=cat path=/persist/atomic.txt bytes=24 text="cow-survived-powercut-77"',
            start_idx=idx,
        )

        qmp2.type_ascii("shutdown")
        qmp2.send_qcodes(["ret"])
        _, s2_kv = h2.finish_and_verify(require_mouse_irqs=False)
        print(
            f"[PASS] {name}: sigkill_recovered=true ({rec_ln.strip()}) "
            f"leaked_frames={s2_kv['leaked_frames']} ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h2.close()
        disk_path.unlink(missing_ok=True)


def run_p4c_vault_dma_devws_security_e2e(
    name: str,
    esp_sec_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Phase P4-C E2E (`ADR-007`, `INV-SEC-01..08`, `INV-CAP-08`, `R-03`):
    Verifies `DEV_WORKSPACE` hardware syscall & VFS confinement, O(1) `GLOBAL_LEASE_EPOCH` capability
    revocation (`-EREVOKED`), `READ_ONLY_EXPORT` `W^X` write-mapping rejection, `INV-SEC-05` cross-mount
    (`-EXDEV`) / cross-domain (`-EPERM`) link isolation, and `dry-blk` `#UD` crash -> PCI `BME=0`
    quiesce + DMA unpin + supervisor restart & `/persist` remount."""
    disk_path = build_dir / "disk_p4c_sec.img"
    _create_raw_disk(disk_path, size_mib=4)
    t0 = time.monotonic()

    h = _DesktopSessionHarness(
        name,
        esp_sec_img,
        ovmf_code,
        ovmf_vars,
        build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_virtio_blk_args(disk_path),
    )
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for(
            "[DRY-P4-DEVWS] PASS: dma_eperm=true devmap_eperm=true vault_sec_eperm=true ro_read_ok=true",
            wait_s=10.0,
        )
        h.wait_for(
            "[DRY-P4-SEC] PASS: devws_confined=true lease_revoked=true ro_export_wx=true "
            "link_isolated=true blk_crash_recovered=true",
            wait_s=10.0,
        )

        idx = len(h.serial_lines)
        qmp.type_ascii("vault-test")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=vault-test lease_revoked=true ro_export_wx=true", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("ln /boot/dry-init.elf /persist/bad")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=ln err=-18", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("ln /persist/vault.sec /persist/bad2")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=ln err=-1", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("cat /persist/vault.sec")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=cat path=/persist/vault.sec err=-1", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        hlt_lns = [ln for ln in serial_out.splitlines() if "[DRY-IDLE-HLT] PASS:" in ln]
        if not hlt_lns:
            raise RuntimeError(f"[{name}] Missing [DRY-IDLE-HLT] PASS telemetry line")
        hkv = _parse_kv_ints(hlt_lns[0])
        if (
            hkv.get("dma_denied", 0) < 2
            or hkv.get("pci_quiesce", 0) < 2
            or hkv.get("vault_denied", 0) < 1
        ):
            raise RuntimeError(f"[{name}] Expected dma_denied>=2 pci_quiesce>=2 vault_denied>=1: {hlt_lns[0]!r}")

        print(
            f"[PASS] {name}: dma_denied={hkv['dma_denied']} pci_quiesce={hkv['pci_quiesce']} "
            f"vault_denied={hkv['vault_denied']} leaked_frames={skv['leaked_frames']} ({time.monotonic() - t0:.2f}s)."
        )
    finally:
        h.close()
        disk_path.unlink(missing_ok=True)


def run_p4_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[[pathlib.Path, pathlib.Path, str], None],
) -> int:
    """Execute all Milestone 2B hardware-backed QEMU/KVM storage, recovery & Vault scenarios."""
    esp_p4_std = build_dir / "esp_p4_desktop_std.img"
    write_mbr_esp_image(esp_p4_std, kernel_elf, "console=com1 desktop4=1")

    esp_p4_sec = build_dir / "esp_p4_desktop_sec.img"
    write_mbr_esp_image(esp_p4_sec, kernel_elf, "console=com1 desktop4=1 p4_sec=1")

    run_p4a_virtio_blk_persist_across_reboots_e2e(
        "p4a_virtio_blk_persist_across_reboots_e2e",
        esp_p4_std,
        ovmf_code,
        ovmf_vars,
        build_dir,
        root_dir,
    )
    run_p4b_powercut_and_superblock_recovery_e2e(
        "p4b_powercut_and_superblock_recovery_e2e",
        esp_p4_std,
        ovmf_code,
        ovmf_vars,
        build_dir,
    )
    run_p4c_vault_dma_devws_security_e2e(
        "p4c_vault_dma_devws_security_e2e",
        esp_p4_sec,
        ovmf_code,
        ovmf_vars,
        build_dir,
    )
    return 3
