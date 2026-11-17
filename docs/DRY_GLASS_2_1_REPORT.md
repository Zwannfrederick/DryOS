# DRY OS — DRY Glass 2.1 Technical & Verification Report

> **Milestone:** Project Ascension 4.0 Tracks A & B (`DRY Glass 2.1`) | **Date:** 2026-11-14 | **Branch:** `main`  
> **Scope:** Adaptive Window Strip, Grouped Searchable Window Overview, HiDPI & Narrow-Tile Polish (`userspace/src/glass/*`, `userspace/src/apps/*`, `userspace/src/ui/containers.rs`, `tools/qemu_p11_glass21_validators.py`)

---

## 1. Executive Summary

Tracks A and B evolve DRY Glass 2.0 into a scalable, window-count-independent desktop shell while eliminating HiDPI and narrow-tile visual overlap bugs without modifying the microkernel or `Syscall ABI v0.1` (`0..=31`):

- **Scalable Adaptive Window Strip (`userspace/src/glass/window_strip.rs`, `439` LOC):** Replaces per-window tab expansion in the top panel with the focused app pill, dynamically packed recent-app icons bounded by `max_strip_pct = 35` (`[DRY-WM-SCALABILITY] pass=1 counts=1,5,10,25,50 res=3 dpi=3 layouts=4 max_strip_pct=35`), application-level grouping with count badges (verified `Notes 2`), and a compact `+N` overflow trigger (`+2` with 7 open windows at `1280x800`).
- **Searchable Window Overview (`userspace/src/glass/overview.rs`, `895` LOC):** `Alt+W` or overflow trigger opens a modal grouped overview (`Up`/`Down` navigate, live query filter e.g. `note`, `Enter` focus, `Alt+M` minimize/restore, `Del` close, `Esc` exit).
- **HiDPI, Compact Sidebar & Card Clipping Polish:** Dynamic `sidebar_preferred_w` icon-rail collapse (`sb_w < 104`) in [settings_ui.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/apps/settings_ui.rs) (`574` LOC) and [files_ui.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/apps/files_ui.rs) (`441` LOC), hover tooltips via `sidebar_tooltip_overlay`, dynamic breadcrumb threshold formatting (`bc_need`), `rows_area` sub-clipping against footer bleed, bounded `metric_card` sub-clipping in [containers.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/ui/containers.rs) (`469` LOC), `Ctrl+N` scratchpad toggle in [dry_editor.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/bin/dry_editor.rs), and `Live DRY Glass 2.1 Settings` status string alignment in [dry_settings.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/bin/dry_settings.rs#L501).
- **Strict Hygiene:** `0` Unicode pictographs/emojis across `userspace/src/`, `tools/`, `docs/`, and `kernel/src/`; every source/tool/report file strictly under `1,000` lines.

---

## 2. Architecture & Component Summary

| Component | Path | LOC | Verified Behavior |
| :--- | :--- | :---: | :--- |
| Adaptive Window Strip | [window_strip.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/glass/window_strip.rs) | 439 | Focused pill, app-grouped badges, dynamic icon slots, `+N` overflow within `<= 35%` strip width |
| Window Overview Overlay | [overview.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/glass/overview.rs) | 895 | Grouped window list (`7 open`), live text filter (`note`), focus/min/close actions |
| Glass Chrome & Strip Host | [chrome.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/glass/chrome.rs) | 512 | Compact top-bar slot coordination with WM state (`TILED` / `FLOAT`, alerts, theme, `QS`) |
| Settings Adaptive UI | [settings_ui.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/apps/settings_ui.rs) | 574 | Icon-rail collapse on narrow panes, compact category header fallback, DPI/theme/contrast controls |
| Files Adaptive UI | [files_ui.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/apps/files_ui.rs) | 441 | Compact mount badge when rail-collapsed, dynamic `bc_need`, clipped `rows_area` (`h >= 12`) |
| UI Toolkit Containers | [containers.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/ui/containers.rs) | 469 | `metric_card` `sub_clip(inner)` + font-metric guards preventing KPI card text overlap at `150%` DPI |
| Glass 2.1 E2E Validator | [qemu_p11_glass21_validators.py](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/tools/qemu_p11_glass21_validators.py) | 421 | 3 QEMU/QMP desktop suites (`p11a`, `p11b`, `p11c`) + 9 PNG captures |

---

## 3. Verification Matrix & Results

### 3.1. Automated QEMU Regression & SMP/RAM Matrix

- **Full QEMU Suite (`python3 tools/qemu_boot_test.py`):** **104/104 PASS** (`58` legacy Milestone `0..=1C+2A-4a`, `9` P1, `14` P2, `6` P3, `3` P4/2B, `2` 2C-0, `1` 2C-1, `1` GPU, `1` 2C-2 HW, `3` DRY Glass, `3` DRY Glass 2.0, `3` DRY Glass 2.1). `cargo clippy -- -D warnings` clean across all Rust crates.
- **SMP/RAM Matrix (`python3 tools/qemu_boot_test.py --p3-matrix`):** **5/5 PASS**, all `leaked_frames = 0`:
  - `1 vCPU / 256 MiB` (`5.04s`, `hlt_calls=570`)
  - `2 vCPU / 512 MiB` (PASS)
  - `4 vCPU / 512 MiB` (`5.04s`, `hlt_calls=2572`)
  - `8 vCPU / 1 GiB` (`5.66s`, `hlt_calls=5200`)
  - `20 vCPU / 2 GiB` (`8.80s`, `hlt_calls=13413`)

### 3.2. DRY Glass 2.1 Scenarios (`tools/compat_bench/results_dry_glass_2_1.json`)

| Case | Resolution & Mode | Key Assertions Verified | `leaked_frames` | Duration |
| :--- | :--- | :--- | :---: | :---: |
| `p11a_glass21_1280x800_grouped_overview_dpi` | `1280x800` Floating -> Tiled, `100%/125%/150%` DPI, Daylight + HC | `windows_opened=7` (`Notes` x2 grouped), `Alt+W` overview + `note` filter, `irq12=5` | `0` | `8.164s` |
| `p11b_glass21_1024x768_narrow_150dpi` | `1024x768` Narrow Tiled, `150%` DPI | Icon-rail sidebars, non-overlapping `metric_card`s, clipped file table | `0` | `3.62s` |
| `p11c_glass21_1920x1080_overview_tiled` | `1920x1080` Tiled + Overview | Full 5-app layout + `Alt+W` overview overlay | `0` | `5.128s` |

### 3.3. Visual Inspection Ledger (`docs/screenshots/glass21/`)

All 9 captured screenshots were visually inspected directly:
1. [glass21_1280x800_grouped_strip.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1280x800_grouped_strip.png) — Grouped `Notes` badge (`2`) and `+2` overflow chip fit cleanly inside the top bar.
2. [glass21_1280x800_overview_grouped.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1280x800_overview_grouped.png) — `Alt+W` overview shows `7 open` windows grouped by app with minimize/close buttons.
3. [glass21_1280x800_overview_search_note.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1280x800_overview_search_note.png) — Search query `note` filters cleanly to the `Notes` (`2`) group.
4. [glass21_1280x800_daylight_125dpi.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1280x800_daylight_125dpi.png) — Daylight `125%` DPI Settings and `DRY Notes #2` render crisply with `IconRail` sidebars.
5. [glass21_1280x800_daylight_150dpi_tiled.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1280x800_daylight_150dpi_tiled.png) — 7 tiled windows at `150%` DPI show zero footer bleed or widget overlap.
6. [glass21_1280x800_hc_150dpi_tiled.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1280x800_hc_150dpi_tiled.png) — High-Contrast `150%` DPI borders and text remain legible across all 7 tiles.
7. [glass21_1024x768_narrow_150dpi_tiled.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1024x768_narrow_150dpi_tiled.png) — Narrow `1024x768` `150%` DPI workspace renders readable KPI cards, `/persist` status badge, and Settings rail.
8. [glass21_1920x1080_tiled_workspace.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1920x1080_tiled_workspace.png) — Full `1920x1080` workspace displays `Live DRY Glass 2.1 Settings` and crisp tiled proportions.
9. [glass21_1920x1080_overview.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/glass21/glass21_1920x1080_overview.png) — Centered Window Overview over blurred `1920x1080` workspace (`5 open`).

---

## 4. Ascension 5.0 (`v0.5.0`) Visual Identity, 118 SVGs & `200%` DPI (`--p15-only`)

Project Ascension 5.0 replaces ad-hoc placeholder glyphs with a complete, original DRY OS vector design system (`Apache-2.0 OR MIT`) integrated directly into `DRY Glass 2.1`:

- **Original Vector Inventory (`118` SVGs, `0` third-party packs, `0` `<text>` tags, `0` emojis):**
  - `assets/brand/` (`6` SVGs): `Iso-Kernel Prism` primary emblem (`64x64`), optically simplified `16x16` mark (`dryos-symbol-small-16.svg`), pure-path dark/light/HC wordmarks, and `dryos-lockup-boot.svg`.
  - `assets/icons/apps/` (`21` SVGs): `64x64` rounded-squircle badges with 2-stop vertical gradients across 5 semantic domain palettes (`terminal`, `files`, `settings`, `life-station`, `notes`, `calculator`, `text-editor`, `system-monitor`, `launcher`, `storage-manager`, `network-manager`, `audio-settings`, `display-settings`, `security-vault`, `software-store`, `update-manager`, `driver-manager`, `device-manager`, `power-manager`, `system-info`, `help-diagnostics`).
  - `assets/icons/system/` (`91` SVGs): `24x24` UI icons spanning navigation/actions, files/storage, hardware, system status, and desktop controls.
- **Deterministic Asset Pipeline & UI Integration:**
  - `tools/build_vector_assets.py` (`686` lines) compiles all `118` SVGs into `#![no_std]` Rust `Q12` integer POD tables: `userspace/src/ui/vector_assets_sys.rs` (`577` lines, `1,227` shapes) and `userspace/src/ui/vector_assets_apps.rs` (`661` lines, `1,712` shapes), verified with `python3 tools/build_vector_assets.py --verify` and `rustc -D warnings`, plus `4` multi-scale contact sheets (`contact_sheet_{dark,light,hc,scales}.png` across `16..256 px`).
  - Integrated into `userspace/src/ui/{mod,brand,app_icons,theme,icons,canvas}.rs`, `userspace/src/glass/{chrome,dock_bar,overlays,overview,window_strip}.rs`, `userspace/src/apps/{files_ui,settings_ui,sysmon_ui}.rs`, `userspace/src/display_banner.rs`, and `userspace/src/bin/dry_settings.rs`.
  - **`200%` HiDPI Support:** `UiTheme::new` scales `100% / 125% / 150% / 200%` (`dpi_pct >= 185 => 200`), bound to key `[9]` and Settings UI controls while preserving key `[0]` (`100 -> 125 -> 150 -> 100`) backward compatibility for `--p11-only`. Key `[V]` / `Identity` renders a live 21-app + system icon gallery inside Settings.
- **QEMU/QMP E2E Verification (`--p15-only`, `tools/qemu_p15_brand_icons_validators.py`, `384` lines):**
  - **`4/4` PASS (`leaked_frames=0`):** `p15_boot_banner` (`1280x800`, `non_bg_px=255880`, `colors=32`), `p15a_1280x800_brand_themes_dpi` (`Dark`/`Daylight`/`HC` + `200%` DPI gallery/settings + overview + lock screen), `p15b_1024x768_125_150dpi` (`125%`/`150%` tiled gallery), and `p15c_1920x1080_showcase`.
  - All **`15`** captured PNG screenshots reside in `docs/screenshots/brand_icons/` (`p15_boot_banner.png`, `p15_1280x800_{dark_desktop,launcher,files,gallery_dark,gallery_light,gallery_hc,gallery_200dpi,settings_200dpi,overview,lock_screen}.png`, `p15_1024x768_tiled_gallery_{125dpi,150dpi}.png`, `p15_1920x1080_{tiled_gallery,overview}.png`).
  - **Combined Post-Merge Verification:** `python3 tools/qemu_boot_test.py` passed **`135/135`** deterministic checks (`leaked_frames=0`), and [`tools/compat_bench/results_p15_brand_icons.json`](../tools/compat_bench/results_p15_brand_icons.json) records `p15a` (`1280x800`, `10` PNGs, `9.925s`), `p15b` (`1024x768`, `2` PNGs, `4.408s`), and `p15c` (`1920x1080`, `2` PNGs, `5.291s`) alongside `p15_boot_banner.png` (all `leaked_frames=0`).
