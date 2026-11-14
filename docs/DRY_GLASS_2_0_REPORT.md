# DRY OS — DRY Glass 2.0 Technical & Verification Report

> **Milestone:** `DRY Glass 2.0` (`G2-1..G2-5`) | **Date:** 14 Kasım 2026 | **Branch:** `main`  
> **Scope:** Tracks A–E (`no_std` TrueType AA, SDF Vector Canvas, Native UI Toolkit, Compositor & Overlays, 6 Redesigned Ring-3 Apps, Multi-Res/Multi-DPI E2E Verification)

---

## 1. Executive Summary & Architectural Invariants

DRY Glass 2.0 replaces retro 8x16 bitmap UI rendering across the Ring-3 desktop and native applications with an in-tree `#![no_std]` scalable TrueType + SDF vector typography/UI stack while preserving strict capability isolation and zero kernel UI bloat:

- **Zero kernel bloat (`0` kernel UI lines, `Syscall ABI v0.1` `0..=31` preserved):** Release disassembly confirms `_RNvNtCsnw1t7cmLuE_10dry_kernel7syscall21rust_syscall_dispatch` allocates `sub rsp, 0xf8` (**`248 B` <= `256 B`** budget).
- **Zero new Rust crate dependencies & zero HTML engines:** Everything in `userspace/src/ttf/*` and `userspace/src/ui/*` compiles `#![no_std]` against `core`, with bounded `.bss` scratch/atlas pools (`0` dynamic heap allocations in the rasterizer).
- **Zero regressions (`101/101 PASS` + `5/5 PASS` Matrix, `leaked_frames = 0`):** Every native desktop app, `DRYFS_v1` `/persist`, `DRY Vault`, `VT-d`, `MSI/MSI-X`, Linux `v6.18` `virtio-rng.c` island (`sha256=720433a8...`, `0` modified upstream files), and live `#UD` `VirtIO-GPU` -> `GOP` -> `gen=2` recovery passes deterministically.

---

## 2. Track A & B — Typography (`ttf/`), Fonts (`OFL-1.1`) & UI Toolkit (`ui/`)

### 2.1. Pure `#![no_std]` TrueType Engine (`userspace/src/ttf/`)

| File | LOC | Responsibility | Key Bounded Invariants |
| :--- | :---: | :--- | :--- |
| `userspace/src/ttf/sfnt.rs` | 511 | SFNT/TTF table parser (`head`, `hhea`, `maxp`, `hmtx`, `cmap`, `loca`, `glyf`) | `MAX_OUTLINE_POINTS = 512`, `MAX_OUTLINE_CONTOURS = 96`, simple + compound glyphs |
| `userspace/src/ttf/raster.rs` | 340 | `24.8` fixed-point 256-level analytical grayscale rasterizer | `.bss` `RasterScratch`, `MAX_RASTER_W/H = 96`, `MAX_EDGES = 2048` |
| `userspace/src/ttf/atlas.rs` | 265 | Open-addressed LRU/bump glyph cache (`FontId` 4 variants) | `ATLAS_POOL_BYTES = 96 KiB`, `512` slots, `1024` hash buckets |
| `userspace/src/ttf/layout.rs` | 470 | UTF-8 decoding, baseline alignment, ellipsis, selection, hit-test | Clipping, selection highlight, `byte_index_at_x` / `x_at_byte_index` |
| `userspace/src/ttf/mod.rs` | 44 | `TextStyle` and HiDPI integer pixel scaling | `scale_px(base_px, dpi_pct)` across `100%` / `125%` / `150%` (clamped `8..=64 px`) |

Legacy `glyph_8x16` (`userspace/src/gfx.rs`) remains solely for low-level diagnostic/emergency text output.

### 2.2. Font Provenance & Licensing (`assets/fonts/`)

Subsets are reproducibly extracted offline from host Noto Sans / Noto Sans Mono via `tools/build_font_subsets.py` under **SIL Open Font License 1.1** (`assets/fonts/OFL.txt`, `assets/fonts/ATTRIBUTION.md`), covering Basic Latin, Latin-1 Supplement, Latin Extended-A (`ĞğÜüŞşİıÖöÇç`), and UI punctuation:

- `assets/fonts/DrySans-Regular.ttf`: `21,732 B` (`NotoSans-Regular.ttf`)
- `assets/fonts/DrySans-SemiBold.ttf`: `21,720 B` (`NotoSans-SemiBold.ttf`)
- `assets/fonts/DryMono-Regular.ttf`: `22,216 B` (`NotoSansMono-Regular.ttf`)
- `assets/fonts/DryMono-SemiBold.ttf`: `21,964 B` (`NotoSansMono-SemiBold.ttf`)
- Total bundled font payload: `87,632 B` (`~85.6 KiB`).

### 2.3. Reusable Native UI Toolkit (`userspace/src/ui/`)

- **`canvas.rs` (`611` LOC) & `icons.rs` (`336` LOC):** Integer SDF anti-aliased rounded rectangles (`rrect_cov`), alpha compositing, clipping, hairline borders, and scalable vector UI icons (`IconKind`).
- **`theme.rs` (`238` LOC):** `UiTheme` tokens for `Obsidian Dark`, `Frosted Daylight` (light), and `High Contrast` with DPI-scaled spacing, radii (`r_sm`, `r_md`), typography scale, and semantic `StatusKind` pills.
- **`layout.rs` (`240` LOC), `widgets.rs` (`416` LOC), `text_edit.rs` (`429` LOC), `containers.rs` (`398` LOC):** Shared immediate-mode layout primitives (`split_top/bot/left/right`, `h_cell`), interactive controls (`button`, `icon_button`, `toggle_switch`, `checkbox`, `slider`, `segmented_control`, `scrollbar_v`), single/multi-line editors (`text_field`, `multiline_viewport`), and containers (`sidebar_nav`, `tab_bar`, `card`, `metric_card`, `list_row`, `table_header`, `progress_bar`, `breadcrumb_bar`, `status_badge`, `popover_menu`, `modal_dialog_frame`).

---

## 3. Track C & D — DRY Glass Compositor & 6 Redesigned Native Apps

- **Compositor, Chrome & Overlays (`df7b141`):** `userspace/src/compositor.rs` (`623` LOC) maintains a cursor-free `WB` scene backbuffer (`<= 384 px` touched on pointer move before `WC` blit) while `userspace/src/glass/{blur,chrome,overlays,dock_bar,theme,typography,icons,app_kit,mod}.rs` render retained procedural wallpaper, dual-pass frosted backdrop blur (`BLUR_R = 4`, `TILE_W = 64`, `TILE_H = 32`), AA window frames/shadows, compact/standard top bar, running-indicator dock, Spotlight Launcher (`Alt+Space`), Quick Settings, Notification Center, and `DRY Vault` Lock Shield.
- **6 Isolated Ring-3 Applications (`c69439c`, `b27bc32`):** All 6 native compartments share `PixelAppWindow` (`userspace/src/apps/mod.rs`) and `ui/`:
  1. `dry-shell` (`userspace/src/bin/dry_shell.rs` + `apps/shell_ui.rs`): Terminal + live telemetry badges.
  2. `dry-sysmon` (`userspace/src/bin/dry_sysmon.rs` + `apps/sysmon_ui.rs`): `Life Station` KPI cards, PMM bar, syscall sparkline, compartment table.
  3. `dry-files` (`userspace/src/bin/dry_files.rs` + `apps/files_ui.rs`): Breadcrumb VFS path (`/boot`, `/ram`, `/persist`), `CoW` badge, file actions.
  4. `dry-settings` (`userspace/src/bin/dry_settings.rs` + `apps/settings_ui.rs`): Category `sidebar_nav`, DPI switcher (`100%`/`125%`/`150%`), theme/contrast/blur/layout/lock/GPU fault controls, and `Help` dialog. Commit `b27bc32` fixed split-brain state sync so external `WindowConfig` updates (e.g. Quick Settings toggling Daylight) refresh `app.query_live_prefs()` before subsequent Settings edits.
  5. `dry-editor` (`userspace/src/bin/dry_editor.rs` + `apps/editor_ui.rs`): `DRY Notes` multi-line UTF-8 editor persisting to `/persist/notes.txt` (`120 B` verified round-trip).
  6. `dry-calc` (`userspace/src/bin/dry_calc.rs` + `apps/calc_ui.rs`): Responsive grid keypad, expression readout, `DEC`/`HEX`/`BIN` programmers view.

---

## 4. Verification Results & Release Binary Audit

### 4.1. Release ELF Footprint Audit (`size`, `< 170` 4 KiB Pages = `696,320 B` Cap)

Measured after final code edit (`b27bc32`) via `python3 tools/qemu_boot_test.py --build-only`:

| Binary (`x86_64-unknown-none/release`) | `.text` (B) | `.data` (B) | `.bss` (B) | Total `dec` (B) | 4 KiB Pages |
| :--- | ---: | ---: | ---: | ---: | :---: |
| `dry-kernel` | 618,496 | 4,918,344 | 1,808,768 | 7,345,608 | — |
| `dry-display` | 331,776 | 211,944 | 0 | 543,720 | 132.7 (< 170) |
| `dry-shell` | 200,704 | 187,656 | 0 | 388,360 | 94.8 (< 170) |
| `dry-settings` | 167,936 | 165,888 | 0 | 333,824 | 81.5 (< 170) |
| `dry-editor` | 163,840 | 165,888 | 0 | 329,728 | 80.5 (< 170) |
| `dry-files` | 155,648 | 167,944 | 0 | 323,592 | 79.0 (< 170) |
| `dry-sysmon` | 147,456 | 165,888 | 0 | 313,344 | 76.5 (< 170) |
| `dry-calc` | 110,592 | 165,888 | 0 | 276,480 | 67.5 (< 170) |
| `dry-init` | 126,976 | 24 | 0 | 127,000 | 31.0 (< 170) |
| `dry-vfs` | 40,960 | 80,608 | 0 | 121,568 | 29.7 (< 170) |
| `dry-lx-rng` (`GPL-2.0-only`) | 20,480 | 88 | 4,096 | 24,664 | 6.0 (< 170) |
| Remaining 13 services/tests | 4,096..24,576 | 0..16 | 0..4,096 | 4,112..24,592 | 1.0..6.0 |

### 4.2. Deterministic QEMU/KVM & Multi-Res Matrix (`results_dry_glass_2_0.json`)

| Suite / Command | Result | Key Measured Evidence |
| :--- | :---: | :--- |
| `python3 tools/qemu_boot_test.py` | **101/101 PASS** | `58` legacy/2A-4a + `9` P1 + `14` P2 + `6` P3 + `3` P4 + `2` P5 + `1` P6 + `1` GPU + `1` 2C-2 + `3` P9 + `3` P10, `leaked_frames=0` |
| `python3 tools/qemu_boot_test.py --p3-matrix` | **5/5 PASS** | `1cpu/256M` (`2.98s`), `2cpu/256M` (`5.09s`), `4cpu/512M` (`4.16s`), `8cpu/1G` (`5.67s`), `20cpu/2G` (`10.26s`, `free_4k=512317`), `leaked_frames=0` |
| `p10a_glass2_1280x800_showcase` (`--p10-only`) | **PASS (`11.751s`)** | `back_2m=2`, `Alt+Space`, `6*7=42`, `/persist/notes.txt` (`120 B`), `QS` Daylight toggle, `100->125->150->100` DPI, High Contrast, `Notif`, `Vault` unlock (`irq12=30`, `leaked_frames=0`) |
| `p10b_glass2_1920x1080_tiled_and_recovery` | **PASS (`5.169s`)** | `back_2m=4`, 4-app Tiled workspace + `#UD` `gpufault` -> `GPU_FALLBACK_TO_GOP` -> `gen=2 vec=6` (`gpu_generations=2`, `pci_quiesce=3`, `leaked_frames=0`) |
| `p10c_glass2_1024x768_all_apps` | **PASS (`3.869s`)** | `back_2m=2`, `running_apps=6` concurrent Stacked mode + `8*9=72` (`leaked_frames=0`) |
| `python3 tools/h2-audit/fetch_virtio_rng_sources.py --verify` | **PASS** | `sha256=720433a8...`, `0` modified upstream Linux `v6.18` `virtio-rng.c` files |

---

## 5. Inspected Visual Screenshot Gallery (`12/12` PNGs Verified)

All 12 QMP PNGs captured under `docs/screenshots/` were visually inspected and verified via `read_file` for smooth TrueType AA glyphs, baseline/icon alignment, zero clipping/overlap, contrast, and multi-DPI scaling:

1. [`glass2_1280x800_dark_desktop.png`](screenshots/glass2_1280x800_dark_desktop.png) — Obsidian Dark desktop (`Terminal` + `Life Station`, `1,023,687` non-bg px)
2. [`glass2_1280x800_launcher.png`](screenshots/glass2_1280x800_launcher.png) — Frosted `Alt+Space` Spotlight App Launcher (`1,023,687` non-bg px)
3. [`glass2_1280x800_calc_editor.png`](screenshots/glass2_1280x800_calc_editor.png) — 4-app Tiled workspace with `Calculator` (`6*7=42`) & `DRY Notes` (`1,023,690` non-bg px)
4. [`glass2_1280x800_files_settings.png`](screenshots/glass2_1280x800_files_settings.png) — `DRY Files` (`/persist`) & `Settings` (`sidebar_nav`, switches, pills, `1,023,689` non-bg px)
5. [`glass2_1280x800_quick_settings.png`](screenshots/glass2_1280x800_quick_settings.png) — Quick Settings frosted popover (`1,023,689` non-bg px)
6. [`glass2_1280x800_daylight_125dpi.png`](screenshots/glass2_1280x800_daylight_125dpi.png) — `Frosted Daylight` light theme at **`125% DPI`** scale (`1,023,296` non-bg px)
7. [`glass2_1280x800_high_contrast.png`](screenshots/glass2_1280x800_high_contrast.png) — WCAG `High Contrast` theme (`1,023,339` non-bg px)
8. [`glass2_1280x800_notifications.png`](screenshots/glass2_1280x800_notifications.png) — Live Notification Center popover (`1,023,689` non-bg px)
9. [`glass2_1280x800_lock_shield.png`](screenshots/glass2_1280x800_lock_shield.png) — Authenticated `DRY Vault` Lock Shield with masked passphrase (`1,022,468` non-bg px)
10. [`glass2_1920x1080_tiled_workspace.png`](screenshots/glass2_1920x1080_tiled_workspace.png) — `1920x1080` 4-app Tiled workspace (`2,073,136` non-bg px)
11. [`glass2_1920x1080_gpu_recovered_gen2.png`](screenshots/glass2_1920x1080_gpu_recovered_gen2.png) — `1920x1080` post-`#UD` `VirtIO-GPU` `gen=2` live recovery (`2,073,136` non-bg px)
12. [`glass2_1024x768_all_apps.png`](screenshots/glass2_1024x768_all_apps.png) — `1024x768` compact topbar & all 6 concurrent apps in Stacked layout (`786,188` non-bg px)
