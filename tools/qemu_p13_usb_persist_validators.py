#!/usr/bin/env python3
"""Milestone M2 End-to-End USB xHCI (HID + BOT Partition 2) & Read-Only NVMe Validators (--p13-only)."""

import hashlib
import pathlib
import struct
import time
import uuid
import zlib
from collections.abc import Callable

from qemu_log_validators import _parse_kv_ints
from qemu_p2_validators import _DesktopQmpClient, _DesktopSessionHarness

DRYFS_MAGIC = b"DRYFS_V1"
PERSIST_README_TEXT = (
    b"DRY OS /persist Crash-Safe VirtIO-Blk Filesystem (DRYFS_v1)\n"
    b"-----------------------------------------------------------\n"
    b"STATUS: Persistent CoW Block Storage (Dual A/B Superblocks + CRC32)\n"
    b"BARRIER: Every commit enforces VIRTIO_BLK_T_FLUSH durability.\n"
    b"SECURITY: Mandatory ADR-007 SecLabel & INV-SEC-05 link rules active.\n"
)


def _sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _format_dryfs_v1_bytes(part_sectors: int) -> tuple[bytes, bytes]:
    """Return `(superblock_4k, data_block2_4k)` matching `userspace/src/persist_fs.rs` byte-for-byte."""
    max_blocks = max(16, min(4096, part_sectors // 8))
    data_blk2 = bytearray(4096)
    data_blk2[: len(PERSIST_README_TEXT)] = PERSIST_README_TEXT

    readme_crc = zlib.crc32(PERSIST_README_TEXT) & 0xFFFF_FFFF
    name_bytes = b"README.txt".ljust(36, b"\x00")
    entry0 = struct.pack(
        "<BBBBIIIIHH36sI",
        1,  # used
        1,  # sec_domain
        1,  # SEC_FLAG_READ_ONLY_EXPORT = 1 << 0
        1,  # nlink
        1000,  # inode
        len(PERSIST_README_TEXT),
        2,  # DATA_START_BLOCK
        readme_crc,
        2,  # VFS_FLAG_READONLY = 1 << 1
        0,  # owner_comp
        name_bytes,
        0,
    )
    assert len(entry0) == 64

    sb = bytearray(1600)
    struct.pack_into(
        "<8sIIQIIII24s",
        sb,
        0,
        DRYFS_MAGIC,
        1,  # version
        max_blocks,
        1,  # tx_seq
        1001,  # next_inode
        3,  # alloc_rover (DATA_START_BLOCK + 1)
        1,  # active_entries
        0,  # crc32 zeroed during hash
        b"\x00" * 24,
    )
    sb[64:128] = entry0
    sb_crc = zlib.crc32(sb) & 0xFFFF_FFFF
    struct.pack_into("<I", sb, 36, sb_crc)

    sb_4k = bytes(sb) + b"\x00" * (4096 - len(sb))
    return sb_4k, bytes(data_blk2)


def _write_gpt_image(
    img_path: pathlib.Path,
    total_sectors: int,
    part1_range: tuple[int, int],
    part2_range: tuple[int, int],
    part2_name: str,
    seed_part1: bytes,
    format_part2_dryfs: bool,
) -> None:
    """Create a valid GPT disk image with Protective MBR, Primary/Backup GPT, and 2 partitions."""
    assert total_sectors >= 4096
    disk_bytes = total_sectors * 512
    with img_path.open("wb") as f:
        f.truncate(disk_bytes)

    def gpt_entry(type_guid: uuid.UUID, unique_guid: uuid.UUID, first: int, last: int, name: str) -> bytes:
        u16_name = name.encode("utf-16-le")[:72].ljust(72, b"\x00")
        return struct.pack("<16s16sQQQ72s", type_guid.bytes_le, unique_guid.bytes_le, first, last, 0, u16_name)

    esp_guid = uuid.UUID("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
    data_guid = uuid.UUID("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")
    e1 = gpt_entry(esp_guid, uuid.uuid5(uuid.NAMESPACE_OID, "dry-p13-part1"), part1_range[0], part1_range[1], "EFI")
    e2 = gpt_entry(
        data_guid,
        uuid.uuid5(uuid.NAMESPACE_OID, "dry-p13-part2"),
        part2_range[0],
        part2_range[1],
        part2_name,
    )
    entries = bytearray(128 * 128)
    entries[0:128] = e1
    entries[128:256] = e2
    entries_crc = zlib.crc32(entries) & 0xFFFF_FFFF

    disk_guid = uuid.uuid5(uuid.NAMESPACE_OID, f"dry-p13-disk-{part2_name}").bytes_le
    backup_lba = total_sectors - 1
    backup_entries_lba = backup_lba - 32
    first_usable = 34
    last_usable = backup_entries_lba - 1

    def make_hdr(cur_lba: int, alt_lba: int, pe_lba: int) -> bytes:
        hdr = bytearray(512)
        struct.pack_into(
            "<8sIIIIQQQQ16sQIII",
            hdr,
            0,
            b"EFI PART",
            0x0001_0000,
            92,
            0,
            0,
            cur_lba,
            alt_lba,
            first_usable,
            last_usable,
            disk_guid,
            pe_lba,
            128,
            128,
            entries_crc,
        )
        hcrc = zlib.crc32(hdr[:92]) & 0xFFFF_FFFF
        struct.pack_into("<I", hdr, 16, hcrc)
        return bytes(hdr)

    pmbr = bytearray(512)
    struct.pack_into(
        "<BBBBBBBBII",
        pmbr,
        446,
        0x00,
        0x00,
        0x02,
        0x00,
        0xEE,
        0xFF,
        0xFF,
        0xFF,
        1,
        min(0xFFFF_FFFF, total_sectors - 1),
    )
    pmbr[510:512] = b"\x55\xaa"

    with img_path.open("r+b") as f:
        f.seek(0)
        f.write(pmbr)
        f.seek(512)
        f.write(make_hdr(1, backup_lba, 2))
        f.seek(2 * 512)
        f.write(entries)
        f.seek(backup_entries_lba * 512)
        f.write(entries)
        f.seek(backup_lba * 512)
        f.write(make_hdr(backup_lba, 1, backup_entries_lba))

        if seed_part1:
            f.seek(part1_range[0] * 512)
            f.write(seed_part1)

        if format_part2_dryfs:
            p2_secs = part2_range[1] - part2_range[0] + 1
            sb_4k, data_blk2_4k = _format_dryfs_v1_bytes(p2_secs)
            p2_off = part2_range[0] * 512
            f.seek(p2_off)
            f.write(sb_4k)
            f.seek(p2_off + 4096)
            f.write(sb_4k)
            f.seek(p2_off + 8192)
            f.write(data_blk2_4k)


class _UsbQmpClient(_DesktopQmpClient):
    """QMP helper verifying QEMU HID/USB Mouse active route and driving keyboard/mouse input."""

    def verify_usb_devices(self) -> None:
        mice = self.cmd("query-mice").get("return", [])
        if not any(("HID" in m.get("name", "") or "USB" in m.get("name", "")) and m.get("current") for m in mice):
            raise RuntimeError(f"Expected active QEMU HID/USB Mouse in query-mice, got {mice!r}")

    def run_usb_cmd(self, cmd_text: str) -> None:
        self.type_ascii(cmd_text)
        self.send_qcodes(["ret"], hold_ms=12)
        time.sleep(0.03)

    def usb_mouse_move_click(self) -> None:
        self.move_in_steps(108, 54, steps=3, step_s=0.025)
        self.click_left(hold_s=0.025)
        time.sleep(0.03)


def _usb_nvme_qemu_args(usb_img: pathlib.Path, nvme_img: pathlib.Path) -> list[str]:
    return [
        "-device",
        "qemu-xhci,id=xhci0",
        "-device",
        "usb-kbd,id=usbkbd0,bus=xhci0.0",
        "-device",
        "usb-mouse,id=usbmouse0,bus=xhci0.0",
        "-drive",
        f"if=none,id=usbdisk0,format=raw,file={usb_img}",
        "-device",
        "usb-storage,id=usbmsc0,bus=xhci0.0,drive=usbdisk0",
        "-drive",
        f"if=none,id=nvmedisk0,format=raw,file={nvme_img}",
        "-device",
        "nvme,serial=DRYNVME01,drive=nvmedisk0",
    ]


def _boot_p13_session(
    name: str,
    esp_img: pathlib.Path,
    usb_img: pathlib.Path,
    nvme_img: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> tuple[_DesktopSessionHarness, _UsbQmpClient]:
    h = _DesktopSessionHarness(
        name=name,
        img_path=esp_img,
        ovmf_code=ovmf_code,
        ovmf_vars=ovmf_vars,
        build_dir=build_dir,
        smp=2,
        expect_fb=True,
        extra_qemu_args=_usb_nvme_qemu_args(usb_img, nvme_img),
    )
    try:
        h.wait_for("[DRY-SHELL] Interactive shell ready in CPL3 Userspace", wait_s=45.0)
        h.wait_for("[DRY-SYSMON] Ready in CPL3 Userspace", wait_s=15.0)
        h.wait_for("active=2", wait_s=15.0)
        qmp = _UsbQmpClient(h.qmp_sock)
        h.qmp = qmp
        qmp.verify_usb_devices()
        return h, qmp
    except Exception:
        if h.proc.poll() is None:
            h.proc.kill()
        raise


def run_p13_usb_persist_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    _root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[..., None],
) -> int:
    """Verify M2 3-boot xHCI USB HID/BOT Partition 2 persistence, RO EROFS, and zero NVMe writes."""
    t0 = time.monotonic()
    esp_rw = build_dir / "esp_p13_rw.img"
    esp_ro = build_dir / "esp_p13_ro.img"
    usb_img = build_dir / "usb_p13_storage.img"
    nvme_img = build_dir / "nvme_p13_ro.img"

    write_mbr_esp_image(esp_rw, kernel_elf, "console=com1 desktop4=1 dry_qemu_test_no_iommu=1 usb_persist_test=1 usb_persist_prefer=1")
    write_mbr_esp_image(
        esp_ro,
        kernel_elf,
        "console=com1 desktop4=1 dry_qemu_test_no_iommu=1 usb_persist_test=1 usb_persist_prefer=1 storage=ro",
    )

    usb_secs = 65536
    p1_range = (2048, 34815)
    p2_range = (34816, usb_secs - 34)
    p1_sentinel = b"DRYOS-USB-ESP-SENTINEL-KEEP-INTACT-0001".ljust(4096, b"\xa5")
    _write_gpt_image(usb_img, usb_secs, p1_range, p2_range, "DRYOS_PERSIST", p1_sentinel, True)

    nvme_secs = 16384
    nvme_seed = b"DRYOS-NVME-RO-SENTINEL-MUST-NOT-CHANGE".ljust(4096, b"\x5a")
    _write_gpt_image(nvme_img, nvme_secs, (2048, 8191), (8192, nvme_secs - 34), "NVME_DATA", nvme_seed, False)

    nvme_sha_init = _sha256_file(nvme_img)
    with usb_img.open("rb") as f:
        usb_gpt_init = f.read(34 * 512)
        f.seek(p1_range[0] * 512)
        usb_p1_init = f.read(4096)

    checks = 0
    try:
        # Boot 1: storage=rw -> USB HID input + CLI telemetry + /persist write & desktop.cfg save.
        h1, qmp1 = _boot_p13_session("p13_boot1_rw", esp_rw, usb_img, nvme_img, ovmf_code, ovmf_vars, build_dir)
        try:
            usb_ln = h1.wait_for("[DRY-USB] xHCI ready", wait_s=10.0)
            if "kbd=1" not in usb_ln or "mouse=1" not in usb_ln or "msc=1" not in usb_ln or "dryfs=1" not in usb_ln:
                raise RuntimeError(f"[P13-B1] Unexpected xHCI ready line: {usb_ln!r}")
            h1.wait_for("[DRY-NVME] Read-Only controller ready", wait_s=10.0)
            h1.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=false ro=false", wait_s=10.0)
            checks += 1

            qmp1.usb_mouse_move_click()
            for cmd, expect in (
                ("uname", "[DRY-SHELL] CMD=uname sys=DryOS arch=x86_64 smp=2"),
                ("free", "[DRY-SHELL] CMD=free total_kib="),
                ("dmesg", "[DRY-SHELL] CMD=dmesg bytes="),
                ("lspci", "[DRY-SHELL] CMD=lspci count="),
                ("lsblk", "[DRY-SHELL] CMD=lsblk backend=2 ro=0"),
                ("nvme", "[DRY-SHELL] CMD=nvme status=READY"),
                ("drvctl", "[DRY-SHELL] CMD=drvctl procs="),
                ("mounts", "[DRY-SHELL] CMD=mounts persist=1 backend=usb-msc-p2 ro=0"),
                ("lsusb", "[DRY-SHELL] CMD=lsusb devs=3 kbd=1 mouse=1 msc=1 dryfs=1"),
                ("cat /persist/boot.log", "[DRY-SHELL] CMD=cat path=/persist/boot.log bytes="),
                ("write /persist/notes.txt usb-persist-m2-hello", "[DRY-SHELL] CMD=write path=/persist/notes.txt bytes=20"),
            ):
                idx = len(h1.serial_lines)
                qmp1.run_usb_cmd(cmd)
                ln = h1.wait_for(expect, start_idx=idx, wait_s=8.0)
                if cmd == "cat /persist/boot.log" and "bytes=0 " in ln:
                    raise RuntimeError(f"[P13-B1] Expected non-empty boot.log: {ln!r}")
                if cmd == "lsusb":
                    kv = _parse_kv_ints(ln)
                    if kv.get("kbd_ev", 0) <= 0 or kv.get("mouse_ev", 0) <= 0:
                        raise RuntimeError(f"[P13-B1] Expected non-zero USB HID kbd_ev & mouse_ev: {ln!r}")
                checks += 1

            # Open Settings (`Alt+4`) over USB HID and apply `Tiling` (`layout=1`) so `/persist/desktop.cfg` is written.
            idx = len(h1.serial_lines)
            qmp1.send_qcodes(["alt", "4"], hold_ms=18)
            h1.wait_for("[DRY-SETTINGS] Ready in CPL3 Userspace", start_idx=idx, wait_s=8.0)
            idx = len(h1.serial_lines)
            qmp1.send_qcodes(["right"], hold_ms=12)
            h1.wait_for("[DRY-SETTINGS] APPLY layout=1", start_idx=idx, wait_s=8.0)
            idx = len(h1.serial_lines)
            qmp1.send_qcodes(["alt", "1"], hold_ms=18)
            h1.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx, wait_s=8.0)
            idx = len(h1.serial_lines)
            qmp1.run_usb_cmd("sync")
            h1.wait_for("[DRY-SHELL] CMD=sync tx_seq=", start_idx=idx, wait_s=8.0)
            checks += 2

            qmp1.run_usb_cmd("shutdown")
            _, s1 = h1.finish_and_verify(require_mouse_irqs=False)
            if s1.get("leaked_frames", -1) != 0:
                raise RuntimeError(f"[P13-B1] Frame leak detected: {s1!r}")
            checks += 1
        except Exception as exc:
            time.sleep(0.3)
            rc_val = h1.proc.poll()
            h1.rd_thread.join(timeout=1.0)
            tail = "".join(h1.serial_lines[-60:])
            raise RuntimeError(f"[P13-B1] {type(exc).__name__}: {exc} rc={rc_val} serial tail:\n{tail}")
        finally:
            if h1.proc.poll() is None:
                h1.proc.kill()

        if _sha256_file(nvme_img) != nvme_sha_init:
            raise RuntimeError("[P13-B1] NVMe disk image modified! INV-DISK-SAFE-01 violated!")
        with usb_img.open("rb") as f:
            if f.read(34 * 512) != usb_gpt_init:
                raise RuntimeError("[P13-B1] USB GPT LBA 0..33 modified! INV-DISK-SAFE-02 violated!")
            f.seek(p1_range[0] * 512)
            if f.read(4096) != usb_p1_init:
                raise RuntimeError("[P13-B1] USB Partition 1 modified! INV-DISK-SAFE-02 violated!")
        checks += 1

        # Boot 2: storage=rw -> verify /persist/notes.txt + /persist/desktop.cfg survived; NVMe SHA unchanged.
        h2, qmp2 = _boot_p13_session("p13_boot2_rw", esp_rw, usb_img, nvme_img, ovmf_code, ovmf_vars, build_dir)
        try:
            h2.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=false ro=false", wait_s=10.0)
            h2.wait_for("[DRY-INIT] Restored desktop.cfg layout=1", wait_s=10.0)
            idx = len(h2.serial_lines)
            qmp2.run_usb_cmd("cat /persist/notes.txt")
            h2.wait_for('[DRY-SHELL] CMD=cat path=/persist/notes.txt bytes=20 text="usb-persist-m2-hello"', start_idx=idx, wait_s=8.0)
            qmp2.run_usb_cmd("shutdown")
            _, s2 = h2.finish_and_verify(require_mouse_irqs=False)
            if s2.get("leaked_frames", -1) != 0:
                raise RuntimeError(f"[P13-B2] Frame leak detected: {s2!r}")
            checks += 2
        except Exception:
            tail = "".join(h2.serial_lines[-60:])
            raise RuntimeError(f"[P13-B2] Failed with rc={h2.proc.poll()} serial tail:\n{tail}")
        finally:
            if h2.proc.poll() is None:
                h2.proc.kill()

        if _sha256_file(nvme_img) != nvme_sha_init:
            raise RuntimeError("[P13-B2] NVMe disk image modified on Boot 2!")
        usb_sha_before_ro = _sha256_file(usb_img)
        checks += 1

        # Boot 3: storage=ro -> readable /persist/notes.txt, write rejected EROFS, USB & NVMe SHA-256 unchanged.
        h3, qmp3 = _boot_p13_session("p13_boot3_ro", esp_ro, usb_img, nvme_img, ovmf_code, ovmf_vars, build_dir)
        try:
            h3.wait_for("[DRY-INIT] Mounted /persist (DRYFS_v1) fresh=false ro=true", wait_s=10.0)
            idx = len(h3.serial_lines)
            qmp3.run_usb_cmd("mounts")
            h3.wait_for("[DRY-SHELL] CMD=mounts persist=1 backend=usb-msc-p2 ro=1", start_idx=idx, wait_s=8.0)

            idx = len(h3.serial_lines)
            qmp3.run_usb_cmd("cat /persist/notes.txt")
            h3.wait_for('[DRY-SHELL] CMD=cat path=/persist/notes.txt bytes=20 text="usb-persist-m2-hello"', start_idx=idx, wait_s=8.0)

            idx = len(h3.serial_lines)
            qmp3.run_usb_cmd("write /persist/notes.txt denied-ro")
            h3.wait_for("[DRY-SHELL] CMD=write create_err=-30", start_idx=idx, wait_s=8.0)

            idx = len(h3.serial_lines)
            qmp3.run_usb_cmd("write /persist/new-ro.txt denied-ro")
            h3.wait_for("[DRY-SHELL] CMD=write create_err=-30", start_idx=idx, wait_s=8.0)

            qmp3.run_usb_cmd("shutdown")
            _, s3 = h3.finish_and_verify(require_mouse_irqs=False)
            if s3.get("leaked_frames", -1) != 0:
                raise RuntimeError(f"[P13-B3] Frame leak detected: {s3!r}")
            checks += 4
        except Exception:
            tail = "".join(h3.serial_lines[-60:])
            raise RuntimeError(f"[P13-B3] Failed with rc={h3.proc.poll()} serial tail:\n{tail}")
        finally:
            if h3.proc.poll() is None:
                h3.proc.kill()

        if _sha256_file(usb_img) != usb_sha_before_ro:
            raise RuntimeError("[P13-B3] USB disk image modified during storage=ro session!")
        if _sha256_file(nvme_img) != nvme_sha_init:
            raise RuntimeError("[P13-B3] NVMe disk image modified on Boot 3!")
        checks += 2
    finally:
        for p in (esp_rw, esp_ro, usb_img, nvme_img):
            p.unlink(missing_ok=True)

    print(
        f"[PASS] P13 USB xHCI + BOT Persist + NVMe RO (3-boot RW/RO + SHA-256) verified "
        f"({checks} assertions, nvme_sha={nvme_sha_init[:12]}, elapsed={time.monotonic() - t0:.2f}s)."
    )
    return checks
