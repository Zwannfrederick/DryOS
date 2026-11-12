#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Milestone 2A-3 Native Applications, Supervisor Lifecycle, Launcher, Terminal, Life Station,
Files/VFS, Settings, Window Manager & Power/Session Lifecycle E2E Validators."""

import pathlib
import shutil
import time
from typing import Callable

from qemu_ipc_validators import _ppm_to_png_and_verify
from qemu_log_validators import _parse_kv_ints
from qemu_p1_validators import _count_diff_pixels
from qemu_p2_validators import _DesktopSessionHarness
from qemu_p3c_validators import run_p3c_suite

ARTIFACT_DIR = pathlib.Path(
    "/home/zwannfrederick/.gemini/antigravity/brain/fcac1153-5c72-4469-803e-6ceaebf4f0cb"
)


def run_p3a_apps_lifecycle_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    """Phase P3-A E2E: Native Terminal (editing, history, scrollback, POSIX rejection, ps, apps),
    Life Station live `SYS_PROC_LIST` telemetry, Topbar Launcher mouse launch/focus, `#UD` crash
    isolation & relaunch, and Terminal `[x]` window close + Launcher relaunch."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    ppm_pre_scroll = build_dir / f"{name}_pre_scroll.ppm"
    ppm_scrolled = build_dir / f"{name}_scrolled.ppm"
    ppm_final = build_dir / f"{name}_final.ppm"

    try:
        qmp = h.boot_to_desktop_ready()
        bench_ln = h.wait_for("[DRY-P3-BENCH] boot_to_interactive_ms=", wait_s=10.0)
        bkv = _parse_kv_ints(bench_ln)
        if bkv.get("boot_to_interactive_ms", 99999) > 15000:
            raise RuntimeError(f"[{name}] Excessive boot_to_interactive_ms: {bench_ln!r}")

        sysmon_ln = h.wait_for("mode=life_station procs=", wait_s=10.0)
        mkv = _parse_kv_ints(sysmon_ln)
        if mkv.get("procs", 0) < 5:
            raise RuntimeError(f"[{name}] Expected >=5 live compartments in Life Station: {sysmon_ln!r}")

        # 1. Native Terminal commands wired to real kernel/supervisor APIs (`help`, `sysinfo`, `ps`, `apps`, `echo`).
        idx = len(h.serial_lines)
        qmp.type_ascii("help")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=help ok=true", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("sysinfo")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=sysinfo cpus=2", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("ps")
        qmp.send_qcodes(["ret"])
        ps_ln = h.wait_for("[DRY-SHELL] CMD=ps count=", start_idx=idx)
        if _parse_kv_ints(ps_ln).get("count", 0) < 5:
            raise RuntimeError(f"[{name}] Expected ps count >= 5, got {ps_ln!r}")

        idx = len(h.serial_lines)
        qmp.type_ascii("apps")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=apps running=2", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("echo p3 native terminal")
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="p3 native terminal"', start_idx=idx)

        # 2. Command History recall (`Up` arrow + `Enter`) re-executes previous command cleanly.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["up"])
        time.sleep(0.03)
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="p3 native terminal"', start_idx=idx)

        # 3. Epistemic honesty: unsupported POSIX pipe (`|`) is explicitly rejected, never faked.
        idx = len(h.serial_lines)
        qmp.type_ascii("echo a ")
        qmp.send_qcodes(["shift", "backslash"])
        qmp.type_ascii(" cat")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SHELL] CMD=unsupported_posix op=0x7c", start_idx=idx)

        # 4. Scrollback buffer (`PgUp` / `PgDn`) updates terminal viewport pixels deterministically.
        qmp.screendump(ppm_pre_scroll)
        qmp.send_qcodes(["pgup"])
        time.sleep(0.08)
        qmp.screendump(ppm_scrolled)
        scroll_diff = _count_diff_pixels(ppm_pre_scroll, ppm_scrolled)
        if scroll_diff < 200:
            raise RuntimeError(f"[{name}] Expected visible scrollback viewport change on PgUp, diff={scroll_diff}")
        qmp.send_qcodes(["pgdn"])
        time.sleep(0.05)

        # 5. Topbar Launcher mouse click on `[2:Life]` (`(230, 14)` from `(640, 400)` -> `dx=-410, dy=-386`).
        idx = len(h.serial_lines)
        qmp.move_in_steps(-410, -386, steps=6)
        qmp.click_left()
        h.wait_for("[DRY-SUP] APP_FOCUS app=lifestation", start_idx=idx)

        # 6. Trigger `#UD` fault in focused `Life Station` (`'x'`) -> verify crash isolation & window cleanup!
        idx = len(h.serial_lines)
        qmp.type_ascii("x")
        reap_ln = h.wait_for("[DRY-SUP] APP_REAPED app=lifestation", start_idx=idx)
        if "kind=1 vec=6 window_cleaned=true" not in reap_ln:
            raise RuntimeError(f"[{name}] Unexpected Life Station crash reap line: {reap_ln!r}")

        # 7. Click `[2:Life]` Launcher pill (`(230, 14)`) again to relaunch crashed `Life Station`!
        idx = len(h.serial_lines)
        qmp.click_left()
        spawn_ln = h.wait_for("[DRY-SUP] APP_SPAWNED app=lifestation", start_idx=idx)
        if "launches=2" not in spawn_ln:
            raise RuntimeError(f"[{name}] Expected launches=2 on Life Station relaunch: {spawn_ln!r}")
        h.wait_for("[DRY-SYSMON] Ready in CPL3 Userspace", start_idx=idx)

        # 8. Move cursor from `(230, 14)` to `Terminal` `[x]` close button `(593, 68)` (`dx=+363, dy=+54`) and click!
        idx = len(h.serial_lines)
        qmp.move_in_steps(363, 54, steps=5)
        qmp.click_left()
        term_reap = h.wait_for("[DRY-SUP] APP_REAPED app=terminal", start_idx=idx)
        if "kind=0 vec=0 window_cleaned=true" not in term_reap:
            raise RuntimeError(f"[{name}] Closing Terminal window should reap cleanly without OS exit: {term_reap!r}")

        # 9. Relaunch `Terminal` via Topbar Launcher `[1:Term]` pill `(152, 14)` (`dx=-441, dy=-54`)!
        idx = len(h.serial_lines)
        qmp.move_in_steps(-441, -54, steps=6)
        qmp.click_left()
        term_spawn = h.wait_for("[DRY-SUP] APP_SPAWNED app=terminal", start_idx=idx)
        if "launches=2" not in term_spawn:
            raise RuntimeError(f"[{name}] Expected launches=2 on Terminal relaunch: {term_spawn!r}")
        h.wait_for("[DRY-SHELL] Interactive shell ready in CPL3 Userspace", start_idx=idx)

        # 10. Restart `Life Station` from relaunched `Terminal` (`restart lifestation`) & capture screenshot!
        idx = len(h.serial_lines)
        qmp.type_ascii("restart lifestation")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-SUP] APP_RESTARTED app=lifestation", start_idx=idx)
        h.wait_for("[DRY-SYSMON] Ready in CPL3 Userspace", start_idx=idx)
        time.sleep(0.15)

        qmp.screendump(ppm_final)
        docs_png = root_dir / "docs" / "p3a_apps_lifecycle.png"
        docs_png.parent.mkdir(parents=True, exist_ok=True)
        _ppm_to_png_and_verify(ppm_final, docs_png)
        if ARTIFACT_DIR.exists():
            shutil.copyfile(docs_png, ARTIFACT_DIR / "p3a_apps_lifecycle.png")

        # 11. Refocus `Terminal` via `Alt+1` keyboard Launcher shortcut, then run controlled shutdown!
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=True)
        if "[DRY-SUP-STATS] launch_cyc=" not in serial_out:
            raise RuntimeError(f"[{name}] Missing [DRY-SUP-STATS] telemetry line")

        print(
            f"[PASS] {name}: boot_ms={bkv['boot_to_interactive_ms']} scroll_diff={scroll_diff} "
            f"irq1={skv['irq1']} irq12={skv['irq12']} leaked_frames=0 ({time.monotonic() - h.t0:.2f}s)."
        )
    finally:
        h.close()
        for p in (ppm_pre_scroll, ppm_scrolled, ppm_final):
            p.unlink(missing_ok=True)


def run_p3a_security_and_malformed_ipc_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
) -> None:
    """Phase P3-A Negative Security Suite: verify unprivileged `SYS_PROC_KILL` `-EPERM`,
    badge-enforced `IPC_OP_SUP_ACTION` (`IPC_OP_ERR_PERM`), malformed action `IPC_OP_ERR_INVAL`,
    and cross-window spoofing rejection."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    try:
        qmp = h.boot_to_desktop_ready()
        h.wait_for("[DRY-P3-SEC] PASS: badge_gate_verified=true bad_action_rejected=true", wait_s=10.0)

        idx = len(h.serial_lines)
        qmp.type_ascii("sec-audit")
        qmp.send_qcodes(["ret"])
        h.wait_for(
            "[DRY-SHELL] CMD=sec-audit kill_eperm=true unauth_perm=true bad_ipc_inval=true "
            "vfs_rofs=true vfs_eperm=true pwr_eperm=true",
            start_idx=idx,
        )

        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])
        serial_out, skv = h.finish_and_verify(require_mouse_irqs=False)
        stats_ln = [ln for ln in serial_out.splitlines() if "[DRY-SUP-STATS]" in ln]
        if not stats_ln:
            raise RuntimeError(f"[{name}] Missing [DRY-SUP-STATS]")
        st_kv = _parse_kv_ints(stats_ln[0])
        if st_kv.get("unauth_rej", 0) < 4 or st_kv.get("inval_rej", 0) < 2:
            raise RuntimeError(f"[{name}] Expected supervisor unauth/inval rejection counters: {stats_ln[0]!r}")

        vfs_ln = [ln for ln in serial_out.splitlines() if "[DRY-VFS-STATS]" in ln]
        if not vfs_ln:
            raise RuntimeError(f"[{name}] Missing [DRY-VFS-STATS]")
        vfs_kv = _parse_kv_ints(vfs_ln[0])
        if vfs_kv.get("rofs_rej", 0) < 1 or vfs_kv.get("unauth_rej", 0) < 2:
            raise RuntimeError(f"[{name}] Expected VFS rofs/unauth rejection counters: {vfs_ln[0]!r}")

        print(
            f"[PASS] {name}: sup_unauth={st_kv['unauth_rej']} vfs_rofs={vfs_kv['rofs_rej']} "
            f"vfs_unauth={vfs_kv['unauth_rej']} leaked_frames={skv['leaked_frames']} ({time.monotonic() - h.t0:.2f}s)."
        )
    finally:
        h.close()


def run_p3b_files_settings_wm_e2e(
    name: str,
    img_path: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
) -> None:
    """Phase P3-B E2E: `dry-vfs` + `Files` (`/boot` RO + `/ram` volatile CRUD + mouse wheel scroll),
    `Settings` live runtime categories (`Floating`/`Tiling`/`Stacked`/`Tabbed`, clickable tab strip,
    auto-floating `DialogTransient` helper dialog, `High Contrast`, `2x` cursor, `Turkish Q` keymap)."""
    h = _DesktopSessionHarness(name, img_path, ovmf_code, ovmf_vars, build_dir, smp=2, expect_fb=True)
    ppm_tiling_dlg = build_dir / f"{name}_tiling_dlg.ppm"
    ppm_std = build_dir / f"{name}_std.ppm"
    ppm_hc = build_dir / f"{name}_hc.ppm"

    try:
        qmp = h.boot_to_desktop_ready()
        vfs_ready = h.wait_for("[DRY-INIT] VFS service ready pid=", wait_s=10.0)
        if _parse_kv_ints(vfs_ready).get("boot_mods", 0) < 11:
            raise RuntimeError(f"[{name}] Expected >=11 seeded boot modules in /boot: {vfs_ready!r}")

        # 1. Launch `Files` (`Alt+3`), verify `/boot` read-only protection, view `/boot/os-release`, and wheel-scroll.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "3"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_SPAWNED app=files", start_idx=idx)
        files_rdy = h.wait_for("[DRY-FILES] Ready in CPL3 Userspace", start_idx=idx)
        if _parse_kv_ints(files_rdy).get("boot_entries", 0) < 13:
            raise RuntimeError(f"[{name}] Expected >=13 /boot entries: {files_rdy!r}")

        idx = len(h.serial_lines)
        qmp.type_ascii("n")
        h.wait_for("[DRY-FILES] ROFS_DISABLED mount=/boot", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-FILES] VIEW path=/boot/os-release", start_idx=idx)

        # Move cursor from `(640, 400)` onto `Files` (`(300, 220)` -> `dx=-340, dy=-180`) and emit IntelliMouse wheel-down.
        qmp.move_in_steps(-340, -180, steps=5)
        idx = len(h.serial_lines)
        qmp.mouse_button("wheel-down", True, settle_s=0.012)
        qmp.mouse_button("wheel-down", False, settle_s=0.012)
        h.wait_for("[DRY-FILES] SCROLL wheel=-1 sel=1", start_idx=idx)

        # 2. Switch `Files` to `/ram` (`2`) and exercise real create, write, rename, and delete.
        idx = len(h.serial_lines)
        qmp.type_ascii("2")
        h.wait_for("[DRY-FILES] MOUNT path=/ram ro=0", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("n")
        time.sleep(0.03)
        qmp.type_ascii("draft.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-FILES] CREATE path=/ram/draft.txt", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("e")
        time.sleep(0.03)
        qmp.type_ascii("dryos vfs live note")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-FILES] WRITE path=/ram/draft.txt bytes=20", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("r")
        time.sleep(0.03)
        qmp.type_ascii("memo.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-FILES] RENAME old=draft.txt new=memo.txt", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("n")
        time.sleep(0.03)
        qmp.type_ascii("tmp.txt")
        qmp.send_qcodes(["ret"])
        h.wait_for("[DRY-FILES] CREATE path=/ram/tmp.txt", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("d")
        h.wait_for("[DRY-FILES] UNLINK path=/ram/tmp.txt", start_idx=idx)

        # 3. Launch `Settings` (`Alt+4`) and cycle Window Mode (`Tiling` -> `Stacked` -> `Tabbed`).
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_SPAWNED app=settings", start_idx=idx)
        h.wait_for("[DRY-SETTINGS] Ready in CPL3 Userspace", start_idx=idx)

        for exp_mode in (1, 2, 3):
            idx = len(h.serial_lines)
            qmp.send_qcodes(["right"])
            h.wait_for(f"[DRY-WM] LAYOUT mode={exp_mode}", start_idx=idx)

        # 4. In `Tabbed` mode (`mode=3`), click tab 0 (`(98, 48)` from `(300, 220)` -> `dx=-202, dy=-172`) to focus Terminal!
        idx = len(h.serial_lines)
        qmp.move_in_steps(-202, -172, steps=5)
        qmp.click_left()
        h.wait_for("[DRY-WM] FOCUS id=1", start_idx=idx)

        # Refocus `Settings` (`Alt+4`), switch to `Side-by-Side Tiling` (`1` twice: `3->0->1`), and open `DialogTransient` (`h`).
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_FOCUS app=settings", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("1")
        h.wait_for("[DRY-WM] LAYOUT mode=0", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.type_ascii("1")
        h.wait_for("[DRY-WM] LAYOUT mode=1", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("h")
        h.wait_for("[DRY-SETTINGS] DIALOG open=1", start_idx=idx)
        time.sleep(0.12)

        qmp.screendump(ppm_tiling_dlg)
        docs_png = root_dir / "docs" / "p3b_files_settings_wm.png"
        docs_png.parent.mkdir(parents=True, exist_ok=True)
        _ppm_to_png_and_verify(ppm_tiling_dlg, docs_png)
        if ARTIFACT_DIR.exists():
            shutil.copyfile(docs_png, ARTIFACT_DIR / "p3b_files_settings_wm.png")

        # Close `DialogTransient` (`Esc`), then test High Contrast + 2x Large Cursor.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["esc"])
        h.wait_for("[DRY-SETTINGS] DIALOG open=0", start_idx=idx)

        qmp.screendump(ppm_std)
        idx = len(h.serial_lines)
        qmp.type_ascii("4")
        h.wait_for("[DRY-WM] PREFS contrast=1", start_idx=idx)
        time.sleep(0.08)
        qmp.screendump(ppm_hc)
        hc_diff = _count_diff_pixels(ppm_std, ppm_hc)
        if hc_diff < 50000:
            raise RuntimeError(f"[{name}] Expected full-scene High Contrast pixel change >= 50000, got {hc_diff}")

        idx = len(h.serial_lines)
        qmp.type_ascii("c")
        h.wait_for("[DRY-WM] PREFS contrast=1 cur_large=1", start_idx=idx)

        # 5. Apply `2x` pointer speed (`3`) and `Turkish Q` keymap (`2`), then prove Set-1 `equal`+`slash` -> `-.` in Terminal!
        idx = len(h.serial_lines)
        qmp.type_ascii("3")
        h.wait_for("[DRY-INPUT] PREFS keymap=0 speed=2", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("2")
        h.wait_for("[DRY-INPUT] PREFS keymap=1 speed=2", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.type_ascii("echo ")
        qmp.send_qcodes(["equal"])
        qmp.send_qcodes(["slash"])
        qmp.send_qcodes(["ret"])
        h.wait_for('[DRY-SHELL] CMD=echo text="-."', start_idx=idx)

        # Restore `US QWERTY` via `Settings` (`Alt+4` -> `2`) and shut down cleanly.
        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "4"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_FOCUS app=settings", start_idx=idx)
        idx = len(h.serial_lines)
        qmp.type_ascii("2")
        h.wait_for("[DRY-INPUT] PREFS keymap=0 speed=2", start_idx=idx)

        idx = len(h.serial_lines)
        qmp.send_qcodes(["alt", "1"], hold_ms=18)
        h.wait_for("[DRY-SUP] APP_FOCUS app=terminal", start_idx=idx)
        qmp.type_ascii("shutdown")
        qmp.send_qcodes(["ret"])

        serial_out, skv = h.finish_and_verify(require_mouse_irqs=True)
        if "[DRY-VFS-STATS]" not in serial_out:
            raise RuntimeError(f"[{name}] Missing [DRY-VFS-STATS] clean teardown line")

        print(
            f"[PASS] {name}: hc_diff={hc_diff} irq1={skv['irq1']} irq12={skv['irq12']} "
            f"leaked_frames={skv['leaked_frames']} ({time.monotonic() - h.t0:.2f}s)."
        )
    finally:
        h.close()
        for p in (ppm_tiling_dlg, ppm_std, ppm_hc):
            p.unlink(missing_ok=True)


def run_p3_suite(
    kernel_elf: pathlib.Path,
    build_dir: pathlib.Path,
    root_dir: pathlib.Path,
    ovmf_code: pathlib.Path,
    ovmf_vars: pathlib.Path,
    write_mbr_esp_image: Callable[[pathlib.Path, pathlib.Path, str], None],
) -> int:
    """Execute all Milestone 2A-3 hardware-backed QEMU/KVM desktop scenarios and return scenario count."""
    esp_p3_std = build_dir / "esp_p3_desktop_std.img"
    write_mbr_esp_image(esp_p3_std, kernel_elf, "console=com1 desktop3=1")

    esp_p3_sec = build_dir / "esp_p3_desktop_sec.img"
    write_mbr_esp_image(esp_p3_sec, kernel_elf, "console=com1 desktop3=1 p3_sec=1")

    run_p3a_apps_lifecycle_e2e(
        "p3a_apps_lifecycle_launcher_terminal_e2e",
        esp_p3_std,
        ovmf_code,
        ovmf_vars,
        build_dir,
        root_dir,
    )
    run_p3a_security_and_malformed_ipc_e2e(
        "p3a_security_and_malformed_ipc_e2e",
        esp_p3_sec,
        ovmf_code,
        ovmf_vars,
        build_dir,
    )
    run_p3b_files_settings_wm_e2e(
        "p3b_files_settings_wm_e2e",
        esp_p3_std,
        ovmf_code,
        ovmf_vars,
        build_dir,
        root_dir,
    )
    n_p3c = run_p3c_suite(esp_p3_std, ovmf_code, ovmf_vars, build_dir, root_dir)
    return 3 + n_p3c
