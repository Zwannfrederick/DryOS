#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Generate compact static TrueType subsets from host Noto Sans / Noto Sans Mono."""

import pathlib
from fontTools import subset

ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC_DIR = pathlib.Path("/usr/share/fonts/noto")
OUT_DIR = ROOT / "assets" / "fonts"

FONTS = [
    ("NotoSans-Regular.ttf", "DrySans-Regular.ttf"),
    ("NotoSans-SemiBold.ttf", "DrySans-SemiBold.ttf"),
    ("NotoSansMono-Regular.ttf", "DryMono-Regular.ttf"),
    ("NotoSansMono-SemiBold.ttf", "DryMono-SemiBold.ttf"),
]

# Printable ASCII + Latin-1 Supplement + Latin Extended-A (Turkish GgUuSsIiOoCc)
UNICODES = list(range(0x0020, 0x007F)) + list(range(0x00A0, 0x0180)) + [
    0x2013,  # EN DASH
    0x2014,  # EM DASH
    0x2018,  # LEFT SINGLE QUOTATION MARK
    0x2019,  # RIGHT SINGLE QUOTATION MARK
    0x201C,  # LEFT DOUBLE QUOTATION MARK
    0x201D,  # RIGHT DOUBLE QUOTATION MARK
    0x2022,  # BULLET
    0x2026,  # HORIZONTAL ELLIPSIS
    0x2190,  # LEFTWARDS ARROW
    0x2191,  # UPWARDS ARROW
    0x2192,  # RIGHTWARDS ARROW
    0x2193,  # DOWNWARDS ARROW
    0x2212,  # MINUS SIGN
    0x25A0,  # BLACK SQUARE
    0x25C6,  # BLACK DIAMOND
    0x2713,  # CHECK MARK
]


def build_subset(src_name: str, dst_name: str) -> None:
    src_path = SRC_DIR / src_name
    dst_path = OUT_DIR / dst_name
    if not src_path.is_file():
        raise FileNotFoundError(f"Missing source font: {src_path}")

    opts = subset.Options()
    opts.hinting = False
    opts.drop_tables += ["DSIG", "GDEF", "GPOS", "GSUB", "cvt ", "fpgm", "gasp", "prep"]
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.recommended_glyphs = True

    font = subset.load_font(str(src_path), opts)
    subsetter = subset.Subsetter(opts)
    subsetter.populate(unicodes=UNICODES)
    subsetter.subset(font)
    font.save(str(dst_path))

    maxp = font["maxp"]
    head = font["head"]
    hhea = font["hhea"]
    size_kb = dst_path.stat().st_size / 1024.0
    print(
        f"{dst_name}: {size_kb:.1f} KiB | glyphs={maxp.numGlyphs} | "
        f"upm={head.unitsPerEm} | asc={hhea.ascent} desc={hhea.descent} gap={hhea.lineGap} | "
        f"maxPts={maxp.maxPoints} maxCont={maxp.maxContours} "
        f"maxCompPts={getattr(maxp, 'maxCompositePoints', 0)} "
        f"maxCompCont={getattr(maxp, 'maxCompositeContours', 0)}"
    )


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for src_name, dst_name in FONTS:
        build_subset(src_name, dst_name)


if __name__ == "__main__":
    main()
