# DRY Glass 2.0 Font Attribution & Provenance

All bundled `.ttf` files in `assets/fonts/` are compact glyph subsets generated reproducibly offline via `tools/build_font_subsets.py` from upstream **Noto Sans** and **Noto Sans Mono** packages installed on the build host (`/usr/share/fonts/noto/`).

- **Upstream Project:** Noto Fonts (`https://github.com/notofonts/latin-greek-cyrillic`)
- **Copyright:** Copyright 2022 The Noto Project Authors
- **License:** SIL Open Font License, Version 1.1 (see `OFL.txt`)
- **Reserved Font Names:** None
- **Included Character Coverage:**
  - Printable Basic Latin (`U+0020..U+007E`)
  - Latin-1 Supplement (`U+00A0..U+00FF`)
  - Latin Extended-A (`U+0100..U+017F`, full Turkish `ĞğÜüŞşİıÖöÇç` coverage)
  - UI Typography Punctuation & Symbols (`U+2013`, `U+2014`, `U+2018`, `U+2019`, `U+201C`, `U+201D`, `U+2022`, `U+2026`, `U+2212`; arrows/diamonds also included in monospace fonts)

| File | Source Font | Role in DRY Glass 2.0 | Subset Size |
| :--- | :--- | :--- | :--- |
| `DrySans-Regular.ttf` | `NotoSans-Regular.ttf` | Proportional UI body, lists, labels, inputs | 21.2 KiB |
| `DrySans-SemiBold.ttf` | `NotoSans-SemiBold.ttf` | Proportional headings, buttons, window titles, badges | 21.2 KiB |
| `DryMono-Regular.ttf` | `NotoSansMono-Regular.ttf` | Terminal output, hex/octal views, code/editor content | 21.7 KiB |
| `DryMono-SemiBold.ttf` | `NotoSansMono-SemiBold.ttf` | Terminal headings, bold ANSI-like highlights, numeric readouts | 21.4 KiB |

Re-generate subsets deterministically at any time with:

```bash
python3 tools/build_font_subsets.py
```
