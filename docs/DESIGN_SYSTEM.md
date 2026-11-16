# DRY OS Visual Identity & Glass 2.1 Design System

> **License:** `Apache-2.0 OR MIT` (Original DRY OS Design Assets)  
> **Scope:** Brand Identity (`Iso-Kernel Prism`), Color & Surface Tokens, Typography, Icon Grids (`24x24` / `64x64`), Optical Scaling, Motion Bounds, and `#![no_std]` Vector Pipeline.

---

## 1. Brand Identity: The Iso-Kernel Prism

DRY OS (`Project Ascension 5.0`) uses an original geometric mark—the **Iso-Kernel Prism**—built without external logos, third-party icon packs, system-font `<text>` elements, or pictographs:

- **Architectural Metaphor:** Three interlocked capability blades (`KERNEL`, `ROOT_SYSTEM`, `USERSPACE`) arranged at 120-degree symmetry around a faceted hexagonal microkernel core, separated by parallel 15-degree precision isolation channels.
- **Brand Assets (`assets/brand/`):**
  - [dryos-symbol.svg](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/assets/brand/dryos-symbol.svg) (`64x64`): Primary mark with faceted outer blades, inner bevel highlights (`LargeOnly`), and 3-facet central core.
  - [dryos-symbol-small-16.svg](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/assets/brand/dryos-symbol-small-16.svg) (`16x16`): Optically simplified variant with 1.5px-1.6px strokes, crisp outer canted hexagon, 3 capability ribs, and solid core pip.
  - [dryos-wordmark-dark.svg](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/assets/brand/dryos-wordmark-dark.svg), [dryos-wordmark-light.svg](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/assets/brand/dryos-wordmark-light.svg), [dryos-wordmark-hc.svg](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/assets/brand/dryos-wordmark-hc.svg) (`240x48`): Pure geometric vector letterforms (`D R Y | O S`) drawn exclusively from `<path>`, `<rect>`, `<polyline>`, `<line>`, and `<circle>` primitives alongside the prism and 3-domain triad pips.
  - [dryos-lockup-boot.svg](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/assets/brand/dryos-lockup-boot.svg) (`320x80`): Clean Boot, Login Shield, and repository hero lockup framing the Iso-Kernel Prism, wordmark, and semantic capability bar inside an Obsidian glass pill.

---

## 2. Theme Modes & Color Tokens

Tokens align directly with `UiTheme` ([theme.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/ui/theme.rs)) and `GlassTokens` ([glass/theme.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/glass/theme.rs)):

### 2.1. Surface, Border, Typography & Semantic Tokens

| Token Role | `ObsidianDark` | `FrostedDaylight` | `HighContrast` | Usage |
| :--- | :--- | :--- | :--- | :--- |
| `bg_app` | `#0F141E` | `#F3F6FA` | `#000000` | Window client root canvas |
| `bg_sidebar` | `#131926` | `#E9EEF6` | `#080A0F` | Navigation rail & inspector pane |
| `bg_card` | `#171E2E` | `#FFFFFF` | `#0C1018` | Elevated card & icon tile surface |
| `bg_card_alt` | `#1C2436` | `#F0F4FA` | `#161B26` | Secondary badge / nested row |
| `bg_input` | `#0B1018` | `#FFFFFF` | `#000000` | Text field & terminal viewport |
| `bg_hover` / `bg_pressed` | `#232E44` / `#2B3852` | `#E2E8F2` / `#D2DCED` | `#1F2937` / `#374151` | Pointer hover & active press |
| `bg_selected` | `#1E3456` | `#DBEAFE` | `#1E3A8A` | Active row / launcher selection |
| `border_subtle` / `border_strong` | `#263147` / `#3B4B6B` | `#DAE2EE` / `#B0BECE` | `#64748B` / `#FFFFFF` | Card hairlines & focus framing |
| `focus_ring` | `#60A5FA` | `#2563EB` | `#FFE600` | Keyboard focus outline (`>= 2px`) |
| `fg_primary` | `#F1F5F9` | `#0F172A` | `#FFFFFF` | Primary headings & body copy |
| `fg_secondary` | `#CBD5E1` | `#334155` | `#F1F5F9` | Secondary labels & descriptions |
| `fg_muted` | `#8E9CB0` | `#526075` | `#CBD5E1` | Captions & metadata (`WCAG AA >= 4.5:1`) |
| `accent` / `accent_fg` | `#2563EB` / `#60A5FA` | `#1D4ED8` / `#1D4ED8` | `#FFE600` / `#FFE600` | Primary actions & active indicators |
| `ok` / `ok_fg` | `#10B981` / `#34D399` | `#059669` / `#047857` | `#00FF88` / `#4ADE80` | Verified capability, online, healthy |
| `warn` / `warn_fg` | `#D97706` / `#FBBF24` | `#B45309` / `#92400E` | `#FFB703` / `#FDE047` | Degraded fallback, storage/caution |
| `danger` / `danger_fg` | `#DC2626` / `#F87171` | `#B91C1C` / `#991B1B` | `#FF4D4D` / `#FCA5A5` | Crashed service, fault, destructive |

### 2.2. Vector Color Slots (`ColorSlot`)

System icons (`24x24`) map SVG hex tokens into dynamic `ColorSlot` values so a single compiled POD asset adapts automatically across `ObsidianDark`, `FrostedDaylight`, and `HighContrast`:

- `FgPrimary`: `#E2E8F0` / `#F1F5F9` -> theme `fg_primary`
- `FgMuted`: `#94A3B8` / `#8E9CB0` -> theme `fg_muted`
- `Accent`: `#38BDF8` / `#60A5FA` / `#2563EB` -> theme cyan/cobalt or HC `#FFE600`
- `Ok` / `Warn` / `Danger`: semantic status slots (`#34D399`, `#FBBF24`, `#F87171` in Dark; deepened `#047857`, `#B45309`, `#B91C1C` in Daylight)
- `BgSurface`: `#171E2E` -> occluding card background (used by overlapping icons such as `tile-float`)
- `RimHighlight`: `#FFFFFF` -> subtle `0.20` alpha top edge rim on app badges (elevated to `#FFE600` border in High Contrast)
- `Custom`: preserves multi-tone brand and application domain gradients

### 2.3. Application Semantic Domain Palettes (`64x64` Squircles)

| Domain Family | Gradient Stops (Top -> Bottom) | Foreground Accents | Applications |
| :--- | :--- | :--- | :--- |
| **Cobalt / Cyan Core** | `#1E293B -> #0B1120`, `#2563EB -> #132A6B`, `#1D4ED8 -> #0E2258` | `#38BDF8`, `#E0F2FE`, `#F8FAFC` | `terminal`, `launcher`, `network-manager`, `display-settings`, `system-info`, `text-editor` |
| **Emerald Health / Power** | `#065F46 -> #032D24`, `#047857 -> #053326`, `#0F4C5C -> #07232C` | `#34D399`, `#ECFDF5`, `#FBBF24` | `life-station`, `power-manager`, `system-monitor`, `help-diagnostics` |
| **Amber / Bronze Storage** | `#B45309 -> #6B2F08`, `#D97706 -> #78350F`, `#92400E -> #451A03` | `#FBBF24`, `#FDE68A`, `#FFFBEB` | `files`, `notes`, `storage-manager` |
| **Violet / Indigo Security & Apps** | `#5B21B6 -> #270E59`, `#4338CA -> #211A66`, `#4F46E5 -> #241E72`, `#6B21A8 -> #311054` | `#DDD6FE`, `#38BDF8`, `#34D399` | `security-vault`, `software-store`, `update-manager`, `audio-settings` |
| **Steel / Slate Hardware** | `#334155 -> #162032`, `#374151 -> #171F2C` | `#E2E8F0`, `#38BDF8`, `#F59E0B` | `settings`, `driver-manager`, `device-manager`, `calculator` |

---

## 3. Typography Hierarchy & DPI Scaling

DRY OS embeds four static TrueType subsets (`DrySans-Regular`, `DrySans-SemiBold`, `DryMono-Regular`, `DryMono-SemiBold`) under `assets/fonts/` (`SIL OFL 1.1`), rasterized purely in `#![no_std]` Ring 3 userspace (`userspace/src/ttf/`).

| Role | Font Family | Base (`100%`) | `125%` DPI | `150%` DPI | Line Measure & Usage |
| :--- | :--- | :---: | :---: | :---: | :--- |
| `title_px` | `DrySans-SemiBold` | `18 px` | `22 px` | `27 px` | Modal & page titles (`45-75ch` cap) |
| `heading_px` | `DrySans-SemiBold` | `14 px` | `18 px` | `21 px` | Card headers, settings sections |
| `body_px` | `DrySans-Regular` | `13 px` | `16 px` | `20 px` | Primary UI copy, lists, buttons |
| `mono_px` | `DryMono-Regular` / `SemiBold` | `13 px` | `16 px` | `20 px` | Terminal, hex IDs, tabular counters |
| `caption_px` | `DrySans-Regular` / `Mono` | `11 px` | `14 px` | `17 px` | Status pills, metadata (`>= 4.5:1` contrast) |

- **Numeric Discipline:** Telemetry metrics, process tables (`Life Station`), and clock/status pills use `DryMono` or tabular alignment to avoid layout jitter on live refresh.
- **Text Contrast & Wrapping:** All muted copy (`fg_muted`) maintains `>= 4.5:1` contrast against `bg_card` and `bg_app`; no transparency fading for hierarchy. Card containers apply `sub_clip(inner)` to prevent text bleed at `150%` DPI.

---

## 4. Icon Grids, Optical Alignment & Stroke Rules

### 4.1. Coordinate System & Normalization

All SVG sources are compiled deterministically by [build_vector_assets.py](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/tools/build_vector_assets.py) into `Q12` integer fixed-point coordinates (`[0..=4096]` normalized against `viewBox`) targeting the `Canvas` Q4 SDF rasterizer (`1 px = 16 Q4 units` in [canvas.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/ui/canvas.rs)):

- **24x24 System Icon Grid:**
  - Safe Margin: `2 px` all sides (`live zone = x: 2..22, y: 2..22`).
  - Optical Core: `16x16` centered (`4..20`) for dense silhouettes; circular glyphs target `r = 7.5..8.0` (`15-16 px` diameter) to match visual weight of `14-16 px` rectangular glyphs (`r = 1.5..2.5`).
  - Stroke Weights: `1.8 px` (`1.7-2.0 px`) primary outlines, `1.5-1.6 px` secondary/muted strokes, `stroke-linecap="round"`, `stroke-linejoin="round"`.
- **64x64 Application Icon Grid:**
  - Safe Margin: `4 px` all sides (`badge = 56x56` at `(4, 4)`).
  - Squircle Corner Radius: `rx = 14` (`25%` of badge dimension).
  - Top Specular Rim: `1 px` inner border (`#FFFFFF`, `alpha = 51`) plus `LargeOnly` top edge highlight bar (`x=16..48, y=5.5`).
  - Foreground Glyph Zone: `x=12..52, y=12..52` (`40x40` inner envelope) with `2.4-4.2 px` bold strokes.

### 4.2. Optical Small-Size Switching (`OpticalFlag`)

Every compiled `VectorShape` carries `OpticalFlag::{Always, SmallOnly, LargeOnly}`:

- **`<= 20 px` (`16 px`, `20 px`):** Shapes tagged `LargeOnly` (fine bezel pips, inner facet hairlines, secondary CPU pins, subtle specular bars, outer dock ring halos) are omitted so small icons stay uncluttered. Brand marks automatically switch from `dryos-symbol` (`64x64`) to `dryos-symbol-small-16` (`16x16`).
- **`>= 24 px` (`24..256 px`):** `SmallOnly` overrides are omitted and full `Always + LargeOnly` geometry renders with proportional SDF anti-aliasing.

---

## 5. Corner Geometry, Elevation, Shadows & Glass

- **Concentric Radius System:**
  - `UiTheme::r_sm = 6 px` (buttons, topbar pills, compact inputs)
  - `UiTheme::r_md = 8 px` (cards, list rows, popovers)
  - `UiTheme::r_lg = 12 px` (floating dock, modal frames, window corners via `GLASS_CORNER_R`)
  - Nested elements follow `r_inner = max(2, r_outer - padding)` so corners remain concentrically parallel.
- **Glass Backdrop & Shadows:**
  - `ObsidianDark` / `FrostedDaylight`: frosted backdrop blur (`backdrop_frosted_rect`) + soft multi-pixel shadow (`blend_soft_rrect_shadow`, `shadow_rgb`) + 1px `glass_rim` top highlight.
  - Low-Overhead / GOP Fallback & `HighContrast`: disables multi-pass blur and soft shadows (`ThemeMode::HighContrast`), switching to crisp high-contrast `1-2 px` solid borders (`#FFE600` / `#FFFFFF`) and opaque surfaces.

---

## 6. Motion, Accessibility & Touch/Pointer Targets

- **Animation Bounds:** Compositor transitions (launcher open, overview toggle, snap preview, toast fade) are strictly GPU/composited (`opacity` and `transform` bounds only) within **`200-300 ms`** ease-out timing (`cubic-bezier(0.2, 0.8, 0.2, 1)` equivalent in fixed-point).
- **Reduced Motion:** `low_overhead_glass` and user accessibility toggles enforce **`0 ms`** instant state transitions with zero animation blocking. Boot splash progress (`Clean Boot` / `Diagnostic Boot`) strictly reflects real capability/service readiness events (`ADR-010`)—never fabricated timers.
- **Interactive Target Sizing:**
  - Standard desktop controls (`ctrl_h = 28 px` at `100%`, `35 px` at `125%`, `42 px` at `150%`) and dock slots (`36 px` at `100%`) expand hit-testing rectangles with padding (`>= 44 px` logical touch/accessibility target floor for primary launcher, quick-settings, lock-screen, and modal controls at `150%` DPI or touch-assist mode).
  - All primary labels avoid truncation bleed and preserve keyboard navigation focus rings (`focus_ring`).
