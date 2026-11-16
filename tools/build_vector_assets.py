#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Deterministic DRY OS SVG vector asset compiler, manifest builder, and PNG contact-sheet verifier."""

from __future__ import annotations

import argparse, hashlib, json, math, pathlib, re, struct, sys, zlib
import xml.etree.ElementTree as ET
from typing import NamedTuple

ROOT = pathlib.Path(__file__).resolve().parents[1]
ASSETS_DIR = ROOT / "assets"
BRAND_DIR, APPS_DIR, SYS_DIR = ASSETS_DIR / "brand", ASSETS_DIR / "icons" / "apps", ASSETS_DIR / "icons" / "system"
MANIFEST_PATH = ASSETS_DIR / "ASSET_MANIFEST.json"
SYS_RS_PATH = ROOT / "userspace" / "src" / "ui" / "vector_assets_sys.rs"
APPS_RS_PATH = ROOT / "userspace" / "src" / "ui" / "vector_assets_apps.rs"
SHOT_DIR = ROOT / "docs" / "screenshots" / "brand_icons"

REQ_BRAND = ["dryos-symbol", "dryos-symbol-small-16", "dryos-wordmark-dark", "dryos-wordmark-light", "dryos-wordmark-hc", "dryos-lockup-boot"]
REQ_APPS = [
    "terminal", "files", "settings", "life-station", "notes", "calculator", "text-editor",
    "system-monitor", "launcher", "storage-manager", "network-manager", "audio-settings",
    "display-settings", "security-vault", "software-store", "update-manager",
    "driver-manager", "device-manager", "power-manager", "system-info", "help-diagnostics",
]
HEX_TO_SLOT = {
    0xE2E8F0: "FgPrimary", 0xF1F5F9: "FgPrimary", 0x0F172A: "FgPrimary", 0x94A3B8: "FgMuted",
    0x8E9CB0: "FgMuted", 0x526075: "FgMuted", 0x38BDF8: "Accent", 0x60A5FA: "Accent",
    0x2563EB: "Accent", 0x1D4ED8: "Accent", 0x34D399: "Ok", 0x10B981: "Ok", 0x059669: "Ok",
    0xFBBF24: "Warn", 0xF59E0B: "Warn", 0xD97706: "Warn", 0xF87171: "Danger",
    0xEF4444: "Danger", 0xDC2626: "Danger", 0x171E2E: "BgSurface",
}
THEME_PALETTES = {
    "dark": {"bg": 0x0B0F18, "card": 0x151C2B, "border": 0x2A364F, "label": 0xCBD5E1, "title": 0xF1F5F9,
             "FgPrimary": 0xF1F5F9, "FgMuted": 0x8E9CB0, "Accent": 0x38BDF8, "Ok": 0x34D399,
             "Warn": 0xFBBF24, "Danger": 0xF87171, "BgSurface": 0x171E2E, "RimHighlight": 0xFFFFFF},
    "light": {"bg": 0xEBF0F7, "card": 0xFFFFFF, "border": 0xCBD5E1, "label": 0x334155, "title": 0x0F172A,
              "FgPrimary": 0x0F172A, "FgMuted": 0x526075, "Accent": 0x1D4ED8, "Ok": 0x047857,
              "Warn": 0xB45309, "Danger": 0xB91C1C, "BgSurface": 0xFFFFFF, "RimHighlight": 0xFFFFFF},
    "hc": {"bg": 0x000000, "card": 0x0C1018, "border": 0xFFE600, "label": 0xF1F5F9, "title": 0xFFFFFF,
           "FgPrimary": 0xFFFFFF, "FgMuted": 0xCBD5E1, "Accent": 0xFFE600, "Ok": 0x4ADE80,
           "Warn": 0xFDE047, "Danger": 0xFCA5A5, "BgSurface": 0x0C1018, "RimHighlight": 0xFFE600},
}
FONT5X7_HEX = {
    " ": "00000000000000", "-": "0000000e000000", "_": "0000000000001f", ".": "00000000000c0c", "/": "01020204080810",
    ":": "000c0c000c0c00", "+": "0004041f040400", "(": "02040808080402", ")": "08040202020408", "0": "0e11131519110e",
    "1": "040c040404040e", "2": "0e11010608101f", "3": "1e01010e01011e", "4": "02060a121f0202", "5": "1f101e0101110e",
    "6": "0608101e11110e", "7": "1f010204080808", "8": "0e11110e11110e", "9": "0e11110f01020c", "A": "0e11111f111111",
    "B": "1e11111e11111e", "C": "0e11101010110e", "D": "1c12111111121c", "E": "1f10101e10101f", "F": "1f10101e101010",
    "G": "0e11101711110f", "H": "1111111f111111", "I": "0e04040404040e", "J": "0702020212120c", "K": "11121418141211",
    "L": "1010101010101f", "M": "111b1515111111", "N": "11191513111111", "O": "0e11111111110e", "P": "1e11111e101010",
    "Q": "0e11111115120d", "R": "1e11111e141211", "S": "0e11100e01110e", "T": "1f040404040404", "U": "1111111111110e",
    "V": "11111111110a04", "W": "11111115151b11", "X": "11110a040a1111", "Y": "11110a04040404", "Z": "1f01020408101f",
}
RUST_HEADER = """// SPDX-License-Identifier: Apache-2.0 OR MIT
//! {title} (generated deterministically by tools/build_vector_assets.py).
//! Pure `no_std` integer POD tables (`Q12` normalized `[0..=4096]`); zero heap/float.

#[repr(u8)] #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {{ RRect = 0, VGradRRect = 1, Circle = 2, Ring = 3, Line = 4 }}
#[repr(u8)] #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpticalFlag {{ Always = 0, SmallOnly = 1, LargeOnly = 2 }}
#[repr(u8)] #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSlot {{
    Custom = 0, FgPrimary = 1, FgMuted = 2, Accent = 3, Ok = 4,
    Warn = 5, Danger = 6, BgSurface = 7, RimHighlight = 8,
}}
#[repr(C)] #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VectorShape {{
    pub kind: ShapeKind, pub optical: OpticalFlag, pub color: ColorSlot, pub alpha: u8,
    pub x: i16, pub y: i16, pub w: i16, pub h: i16, pub radius: i16, pub thick: i16,
    pub rgb_top: u32, pub rgb_bot: u32,
}}
#[repr(C)] #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VectorAsset {{
    pub id: &'static [u8], pub category: &'static [u8],
    pub vb_w: u16, pub vb_h: u16, pub start: u16, pub len: u16,
}}
#[allow(unused_imports)]
use ColorSlot::*;
#[allow(unused_imports)]
use OpticalFlag::*;
#[allow(dead_code)]
const fn rr(o: OpticalFlag, c: ColorSlot, a: u8, r: (i16, i16, i16, i16), rad: i16, rgb: u32) -> VectorShape {{ VectorShape {{ kind: ShapeKind::RRect, optical: o, color: c, alpha: a, x: r.0, y: r.1, w: r.2, h: r.3, radius: rad, thick: 0, rgb_top: rgb, rgb_bot: 0 }} }}
#[allow(dead_code)]
const fn sr(o: OpticalFlag, c: ColorSlot, a: u8, r: (i16, i16, i16, i16), rad: i16, th: i16, rgb: u32) -> VectorShape {{ VectorShape {{ kind: ShapeKind::RRect, optical: o, color: c, alpha: a, x: r.0, y: r.1, w: r.2, h: r.3, radius: rad, thick: th, rgb_top: rgb, rgb_bot: 0 }} }}
#[allow(dead_code)]
const fn vg(o: OpticalFlag, c: ColorSlot, a: u8, r: (i16, i16, i16, i16), rad: i16, g: (u32, u32)) -> VectorShape {{ VectorShape {{ kind: ShapeKind::VGradRRect, optical: o, color: c, alpha: a, x: r.0, y: r.1, w: r.2, h: r.3, radius: rad, thick: 0, rgb_top: g.0, rgb_bot: g.1 }} }}
#[allow(dead_code)]
const fn ci(o: OpticalFlag, c: ColorSlot, a: u8, p: (i16, i16), rad: i16, rgb: u32) -> VectorShape {{ VectorShape {{ kind: ShapeKind::Circle, optical: o, color: c, alpha: a, x: p.0, y: p.1, w: rad, h: 0, radius: 0, thick: 0, rgb_top: rgb, rgb_bot: 0 }} }}
#[allow(dead_code)]
const fn rg(o: OpticalFlag, c: ColorSlot, a: u8, p: (i16, i16), rad: i16, th: i16, rgb: u32) -> VectorShape {{ VectorShape {{ kind: ShapeKind::Ring, optical: o, color: c, alpha: a, x: p.0, y: p.1, w: rad, h: 0, radius: 0, thick: th, rgb_top: rgb, rgb_bot: 0 }} }}
#[allow(dead_code)]
const fn ln(o: OpticalFlag, c: ColorSlot, a: u8, p0: (i16, i16), p1: (i16, i16), th: i16, rgb: u32) -> VectorShape {{ VectorShape {{ kind: ShapeKind::Line, optical: o, color: c, alpha: a, x: p0.0, y: p0.1, w: p1.0, h: p1.1, radius: 0, thick: th, rgb_top: rgb, rgb_bot: 0 }} }}"""


class Shape(NamedTuple):
    kind: str; optical: str; slot: str; alpha: int
    x: int; y: int; w: int; h: int; radius: int; thick: int; rgb_top: int; rgb_bot: int


class Asset(NamedTuple):
    slug: str; category: str; rel_path: str; sha256: str; vb_w: int; vb_h: int; shapes: list[Shape]


def parse_color(val: str | None) -> int | None:
    if not val or val.strip().lower() == "none":
        return None
    s = val.strip().lower()
    if s in ("white", "black"):
        return 0xFFFFFF if s == "white" else 0
    if s.startswith("#"):
        h = s[1:]
        return int("".join(c * 2 for c in h) if len(h) == 3 else h, 16)
    raise ValueError(f"Unsupported SVG color: {val}")


def parse_num(val: str | None, default: float = 0.0) -> float:
    return default if not val else float(re.sub(r"(px|%)$", "", val.strip()))


def mul_mat(m0: tuple[float, ...], m1: tuple[float, ...]) -> tuple[float, float, float, float, float, float]:
    a0, b0, c0, d0, e0, f0 = m0
    a1, b1, c1, d1, e1, f1 = m1
    return (a0 * a1 + c0 * b1, b0 * a1 + d0 * b1, a0 * c1 + c0 * d1, b0 * c1 + d0 * d1, a0 * e1 + c0 * f1 + e0, b0 * e1 + d0 * f1 + f0)


def parse_transform(tf: str | None) -> tuple[float, float, float, float, float, float]:
    m = (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    if not tf:
        return m
    for kind, args_s in re.findall(r"(translate|scale|rotate)\(([^)]*)\)", tf):
        nums = [float(x) for x in re.split(r"[\s,]+", args_s.strip()) if x]
        if kind == "translate":
            n = (1.0, 0.0, 0.0, 1.0, nums[0], nums[1] if len(nums) > 1 else 0.0)
        elif kind == "scale":
            n = (nums[0], 0.0, 0.0, nums[1] if len(nums) > 1 else nums[0], 0.0, 0.0)
        else:
            ang = math.radians(nums[0])
            cs, sn = math.cos(ang), math.sin(ang)
            cx, cy = (nums[1], nums[2]) if len(nums) > 2 else (0.0, 0.0)
            n = (cs, sn, -sn, cs, cx - cs * cx + sn * cy, cy - sn * cx - cs * cy)
        m = mul_mat(m, n)
    return m


def xform_pt(m: tuple[float, ...], x: float, y: float) -> tuple[float, float]:
    return (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])


def sample_arc(x0: float, y0: float, rx: float, ry: float, phi_deg: float, large: int, sweep: int, x1: float, y1: float) -> list[tuple[float, float]]:
    if (x0 == x1 and y0 == y1) or rx == 0 or ry == 0:
        return [(x1, y1)]
    rx, ry, phi = abs(rx), abs(ry), math.radians(phi_deg)
    cp, sp = math.cos(phi), math.sin(phi)
    dx, dy = (x0 - x1) * 0.5, (y0 - y1) * 0.5
    x1p, y1p = cp * dx + sp * dy, -sp * dx + cp * dy
    lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry)
    if lam > 1.0:
        s = math.sqrt(lam)
        rx, ry = rx * s, ry * s
    num = max(0.0, rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p)
    den = rx * rx * y1p * y1p + ry * ry * x1p * x1p
    fac = (-1.0 if large == sweep else 1.0) * (math.sqrt(num / den) if den > 1e-9 else 0.0)
    cxp, cyp = fac * (rx * y1p / ry), -fac * (ry * x1p / rx)
    cx, cy = cp * cxp - sp * cyp + (x0 + x1) * 0.5, sp * cxp + cp * cyp + (y0 + y1) * 0.5
    vang = lambda ux, uy, vx, vy: (-1.0 if ux * vy - uy * vx < 0 else 1.0) * math.acos(max(-1.0, min(1.0, (ux * vx + uy * vy) / max(1e-9, math.hypot(ux, uy) * math.hypot(vx, vy)))))
    th1 = vang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dth = vang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if sweep == 0 and dth > 0:
        dth -= 2 * math.pi
    elif sweep == 1 and dth < 0:
        dth += 2 * math.pi
    steps = max(6, int(abs(dth) / (math.pi / 12)) + 1)
    return [(cp * rx * math.cos(th1 + dth * k / steps) - sp * ry * math.sin(th1 + dth * k / steps) + cx,
             sp * rx * math.cos(th1 + dth * k / steps) + cp * ry * math.sin(th1 + dth * k / steps) + cy) for k in range(1, steps + 1)]


def parse_path_subpaths(d: str) -> list[tuple[list[tuple[float, float]], bool]]:
    tokens = re.findall(r"[A-Za-z]|[-+]?(?:\d*\.\d+|\d+)(?:[eE][-+]?\d+)?", d)
    i, cx, cy, sx, sy, cmd, subs, cur = 0, 0.0, 0.0, 0.0, 0.0, "", [], []
    while i < len(tokens):
        if tokens[i].isalpha():
            cmd, i = tokens[i], i + 1
            if cmd in ("Z", "z"):
                if cur:
                    subs.append((cur, True))
                    cur = []
                cx, cy = sx, sy
                continue
        rel, uc = cmd.islower(), cmd.upper()
        if uc in ("M", "L"):
            if uc == "M" and cur:
                subs.append((cur, False))
                cur = []
            nx, ny = float(tokens[i]), float(tokens[i + 1])
            i += 2
            cx, cy = (cx + nx, cy + ny) if rel else (nx, ny)
            if uc == "M":
                sx, sy, cmd = cx, cy, ("l" if rel else "L")
            cur.append((cx, cy))
        elif uc in ("H", "V"):
            v, i = float(tokens[i]), i + 1
            cx = (cx + v if rel else v) if uc == "H" else cx
            cy = (cy + v if rel else v) if uc == "V" else cy
            cur.append((cx, cy))
        elif uc == "Q":
            qx, qy, nx, ny = (float(tokens[i + k]) for k in range(4))
            i += 4
            if rel:
                qx, qy, nx, ny = cx + qx, cy + qy, cx + nx, cy + ny
            for s in range(1, 9):
                t, mt = s / 8.0, 1.0 - s / 8.0
                cur.append((mt * mt * cx + 2 * mt * t * qx + t * t * nx, mt * mt * cy + 2 * mt * t * qy + t * t * ny))
            cx, cy = nx, ny
        elif uc == "C":
            x1, y1, x2, y2, nx, ny = (float(tokens[i + k]) for k in range(6))
            i += 6
            if rel:
                x1, y1, x2, y2, nx, ny = cx + x1, cy + y1, cx + x2, cy + y2, cx + nx, cy + ny
            for s in range(1, 11):
                t, mt = s / 10.0, 1.0 - s / 10.0
                cur.append((mt**3 * cx + 3 * mt**2 * t * x1 + 3 * mt * t**2 * x2 + t**3 * nx,
                            mt**3 * cy + 3 * mt**2 * t * y1 + 3 * mt * t**2 * y2 + t**3 * ny))
            cx, cy = nx, ny
        elif uc == "A":
            rx, ry, rot, lg, swp, nx, ny = (float(tokens[i + k]) for k in range(7))
            i += 7
            if rel:
                nx, ny = cx + nx, cy + ny
            cur.extend(sample_arc(cx, cy, rx, ry, rot, int(lg), int(swp), nx, ny))
            cx, cy = nx, ny
        else:
            raise ValueError(f"Unsupported SVG path cmd: {cmd}")
    if cur:
        subs.append((cur, False))
    return subs


def polygon_to_lines(pts: list[tuple[float, float]], vb_w: float, vb_h: float, opt: str, slot: str, alpha: int, rgb: int) -> list[Shape]:
    if len(pts) < 3:
        return []
    if sum(pts[i][0] * pts[(i + 1) % len(pts)][1] - pts[(i + 1) % len(pts)][0] * pts[i][1] for i in range(len(pts))) < 0:
        pts = list(reversed(pts))
    edge_t = max(0.85, min(vb_w, vb_h) * 0.022)
    r, n, inset = edge_t * 0.5, len(pts), []
    for i in range(n):
        p0, p1, p2 = pts[(i - 1) % n], pts[i], pts[(i + 1) % n]
        e1x, e1y, e2x, e2y = p1[0] - p0[0], p1[1] - p0[1], p2[0] - p1[0], p2[1] - p1[1]
        l1, l2 = math.hypot(e1x, e1y), math.hypot(e2x, e2y)
        if l1 < 1e-6 or l2 < 1e-6:
            inset.append(p1)
        else:
            n1x, n1y, n2x, n2y = -e1y / l1, e1x / l1, -e2y / l2, e2x / l2
            bx, by = n1x + n2x, n1y + n2y
            bl = math.hypot(bx, by)
            if bl < 1e-6:
                inset.append((p1[0] + n1x * r, p1[1] + n1y * r))
            else:
                bx, by = bx / bl, by / bl
                d = min((r * 0.85) / max(0.45, bx * n1x + by * n1y), min(l1, l2) * 0.38)
                inset.append((p1[0] + bx * d, p1[1] + by * d))
    qx, qy, qs = lambda x: round((x / vb_w) * 4096), lambda y: round((y / vb_h) * 4096), lambda v: max(1, round((v / vb_h) * 4096))
    out, ys = [], [p[1] for p in inset]
    y, y_end, step, ft = min(ys) + r * 0.2, max(ys) - r * 0.2, edge_t * 0.55, edge_t * 1.25
    while y <= y_end:
        xs = sorted(inset[i][0] + (y - inset[i][1]) * (inset[(i + 1) % n][0] - inset[i][0]) / (inset[(i + 1) % n][1] - inset[i][1])
                    for i in range(n) if (inset[i][1] <= y < inset[(i + 1) % n][1]) or (inset[(i + 1) % n][1] <= y < inset[i][1]))
        for k in range(0, len(xs) - 1, 2):
            if xs[k + 1] - xs[k] > 0.05:
                out.append(Shape("Line", opt, slot, alpha, qx(xs[k]), qy(y), qx(xs[k + 1]), qy(y), 0, qs(ft), rgb, 0))
        y += step
    for i in range(n):
        p0, p1 = inset[i], inset[(i + 1) % n]
        out.append(Shape("Line", opt, slot, alpha, qx(p0[0]), qy(p0[1]), qx(p1[0]), qy(p1[1]), 0, qs(edge_t), rgb, 0))
    return out


def rect_rounded_pts(x: float, y: float, w: float, h: float, rx: float) -> list[tuple[float, float]]:
    r = max(0.0, min(rx, w * 0.5, h * 0.5))
    if r < 0.25:
        return [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
    corners = [(x + w - r, y + r, -math.pi / 2, 0.0), (x + w - r, y + h - r, 0.0, math.pi / 2),
               (x + r, y + h - r, math.pi / 2, math.pi), (x + r, y + r, math.pi, 1.5 * math.pi)]
    return [(cx + r * math.cos(a0 + (a1 - a0) * (k / 3.0)), cy + r * math.sin(a0 + (a1 - a0) * (k / 3.0)))
            for cx, cy, a0, a1 in corners for k in range(4)]


def validate_svg_text(path: pathlib.Path, raw: str) -> None:
    for idx, ch in enumerate(raw):
        if ch not in ("\n", "\r", "\t") and not (0x20 <= ord(ch) <= 0x7E):
            raise ValueError(f"{path}: non-ASCII char U+{ord(ch):04X} at {idx}")
    stripped = raw.lower().replace('xmlns="http://www.w3.org/2000/svg"', "")
    for bad in ("<text", "<tspan", "http://", "https://", "ftp://", "data:image", "xlink:href"):
        if bad in stripped:
            raise ValueError(f"{path}: forbidden SVG token {bad!r}")


def parse_svg_file(path: pathlib.Path, category: str) -> Asset:
    raw = path.read_text(encoding="ascii")
    validate_svg_text(path, raw)
    root = ET.fromstring(raw)
    vb = root.attrib.get("viewBox")
    if vb:
        parts = [float(x) for x in re.split(r"[\s,]+", vb.strip())]
        vb_w, vb_h = int(round(parts[2])), int(round(parts[3]))
    else:
        vb_w, vb_h = int(round(parse_num(root.attrib.get("width"), 24))), int(round(parse_num(root.attrib.get("height"), 24)))
    if vb_w <= 0 or vb_h <= 0:
        raise ValueError(f"{path}: invalid viewBox {vb_w}x{vb_h}")

    grads: dict[str, tuple[int, int]] = {}
    for g in root.iter():
        if g.tag.split("}")[-1] == "linearGradient" and "id" in g.attrib:
            stops = [c for c in g if c.tag.split("}")[-1] == "stop"]
            if len(stops) >= 2:
                c0 = parse_color(stops[0].attrib.get("stop-color")) or 0
                grads[g.attrib["id"]] = (c0, parse_color(stops[-1].attrib.get("stop-color")) or c0)

    shapes: list[Shape] = []
    qx, qy, qs = lambda x: round((x / vb_w) * 4096), lambda y: round((y / vb_h) * 4096), lambda v: max(1, round((v / vb_h) * 4096))
    slot_for = lambda rgb, exp: exp if exp else (HEX_TO_SLOT.get(rgb, "FgPrimary") if category == "system" else "Custom")
    emit_poly = lambda pts, cl, opt, sl, a, sw, rgb: shapes.extend(
        Shape("Line", opt, sl, a, qx(pts[k][0]), qy(pts[k][1]), qx(pts[(k + 1) % len(pts)][0]), qy(pts[(k + 1) % len(pts)][1]), 0, qs(sw), rgb, 0)
        for k in range(len(pts) if cl else len(pts) - 1)
    )

    def walk(node: ET.Element, state: dict[str, str], mat: tuple[float, ...]) -> None:
        st = {**state, **{k: node.attrib[k] for k in ("fill", "stroke", "stroke-width", "opacity", "fill-opacity", "stroke-opacity", "data-optical", "data-slot") if k in node.attrib}}
        cur_mat, tag = mul_mat(mat, parse_transform(node.attrib.get("transform"))), node.tag.split("}")[-1]
        if tag in ("defs", "linearGradient", "stop", "title", "desc"):
            return
        opt = {"always": "Always", "small-only": "SmallOnly", "large-only": "LargeOnly"}.get(st.get("data-optical", "always"), "Always")
        exp_slot, b_op = st.get("data-slot"), float(st.get("opacity", "1"))
        fa = max(0, min(255, round(255 * b_op * float(st.get("fill-opacity", "1")))))
        sa = max(0, min(255, round(255 * b_op * float(st.get("stroke-opacity", "1")))))
        sw, f_str, s_str = parse_num(st.get("stroke-width"), 1.5), st.get("fill", "#000000"), st.get("stroke", "none")

        if tag == "rect":
            x, y, w, h = parse_num(node.attrib.get("x")), parse_num(node.attrib.get("y")), parse_num(node.attrib.get("width")), parse_num(node.attrib.get("height"))
            rx, (tx, ty) = parse_num(node.attrib.get("rx"), parse_num(node.attrib.get("ry"), 0.0)), xform_pt(cur_mat, x, y)
            if f_str.startswith("url(#"):
                c0, c1 = grads[f_str[5:-1]]
                shapes.append(Shape("VGradRRect", opt, "Custom", fa, qx(tx), qy(ty), qx(w), qy(h), round((rx / vb_h) * 4096), 0, c0, c1))
            elif (fc := parse_color(f_str)) is not None:
                shapes.append(Shape("RRect", opt, slot_for(fc, exp_slot), fa, qx(tx), qy(ty), qx(w), qy(h), round((rx / vb_h) * 4096), 0, fc, 0))
            if (sc := parse_color(s_str)) is not None:
                sl = "RimHighlight" if (category == "apps" and sc == 0xFFFFFF and sw <= 1.25 and not exp_slot) else slot_for(sc, exp_slot)
                if sw <= 1.25:
                    shapes.append(Shape("RRect", opt, sl, sa, qx(tx), qy(ty), qx(w), qy(h), round((rx / vb_h) * 4096), qs(sw), sc, 0))
                else:
                    emit_poly(rect_rounded_pts(tx, ty, w, h, rx), True, opt, sl, sa, sw, sc)
        elif tag == "circle":
            cx, cy = xform_pt(cur_mat, parse_num(node.attrib.get("cx")), parse_num(node.attrib.get("cy")))
            r = parse_num(node.attrib.get("r"))
            if (fc := parse_color(f_str)) is not None:
                shapes.append(Shape("Circle", opt, slot_for(fc, exp_slot), fa, qx(cx), qy(cy), qs(r), 0, 0, 0, fc, 0))
            if (sc := parse_color(s_str)) is not None:
                shapes.append(Shape("Ring", opt, slot_for(sc, exp_slot), sa, qx(cx), qy(cy), qs(r), 0, 0, qs(sw), sc, 0))
        elif tag == "line":
            x0, y0 = xform_pt(cur_mat, parse_num(node.attrib.get("x1")), parse_num(node.attrib.get("y1")))
            x1, y1 = xform_pt(cur_mat, parse_num(node.attrib.get("x2")), parse_num(node.attrib.get("y2")))
            sc = parse_color(s_str) or parse_color(f_str) or 0xFFFFFF
            shapes.append(Shape("Line", opt, slot_for(sc, exp_slot), sa, qx(x0), qy(y0), qx(x1), qy(y1), 0, qs(sw), sc, 0))
        elif tag in ("polyline", "polygon"):
            nums = [float(v) for v in re.split(r"[\s,]+", node.attrib["points"].strip()) if v]
            pts = [xform_pt(cur_mat, nums[k], nums[k + 1]) for k in range(0, len(nums), 2)]
            if tag == "polygon" and (fc := parse_color(f_str)) is not None:
                shapes.extend(polygon_to_lines(pts, vb_w, vb_h, opt, slot_for(fc, exp_slot), fa, fc))
            if (sc := parse_color(s_str)) is not None:
                emit_poly(pts, tag == "polygon", opt, slot_for(sc, exp_slot), sa, sw, sc)
        elif tag == "path":
            for raw_pts, closed in parse_path_subpaths(node.attrib["d"]):
                pts = [xform_pt(cur_mat, px, py) for px, py in raw_pts]
                if (fc := parse_color(f_str)) is not None and len(pts) >= 3:
                    shapes.extend(polygon_to_lines(pts, vb_w, vb_h, opt, slot_for(fc, exp_slot), fa, fc))
                if (sc := parse_color(s_str)) is not None and len(pts) >= 2:
                    emit_poly(pts, closed, opt, slot_for(sc, exp_slot), sa, sw, sc)
        for child in node:
            walk(child, st, cur_mat)

    walk(root, {"fill": "#000000", "stroke": "none"}, (1.0, 0.0, 0.0, 1.0, 0.0, 0.0))
    return Asset(path.stem, category, str(path.relative_to(ROOT)), hashlib.sha256(path.read_bytes()).hexdigest(), vb_w, vb_h, shapes)


def fmt_shape(s: Shape) -> str:
    rgb0 = f"0x00{s.rgb_top:06X}"
    if s.kind == "RRect":
        return f"rr({s.optical}, {s.slot}, {s.alpha}, ({s.x}, {s.y}, {s.w}, {s.h}), {s.radius}, {rgb0})" if s.thick == 0 else \
               f"sr({s.optical}, {s.slot}, {s.alpha}, ({s.x}, {s.y}, {s.w}, {s.h}), {s.radius}, {s.thick}, {rgb0})"
    if s.kind == "VGradRRect":
        return f"vg({s.optical}, {s.slot}, {s.alpha}, ({s.x}, {s.y}, {s.w}, {s.h}), {s.radius}, ({rgb0}, 0x00{s.rgb_bot:06X}))"
    if s.kind == "Circle":
        return f"ci({s.optical}, {s.slot}, {s.alpha}, ({s.x}, {s.y}), {s.w}, {rgb0})"
    if s.kind == "Ring":
        return f"rg({s.optical}, {s.slot}, {s.alpha}, ({s.x}, {s.y}), {s.w}, {s.thick}, {rgb0})"
    return f"ln({s.optical}, {s.slot}, {s.alpha}, ({s.x}, {s.y}), ({s.w}, {s.h}), {s.thick}, {rgb0})"


def generate_rust_module(assets: list[Asset], mod_title: str, arr_prefix: str) -> str:
    all_shapes: list[Shape] = []
    entries, shape_lines = [], []
    total_shapes = sum(len(a.shapes) for a in assets)
    per_line = 3 if total_shapes > 850 else (2 if total_shapes > 380 else 1)
    for a in assets:
        start = len(all_shapes)
        all_shapes.extend(a.shapes)
        entries.append(f'    VectorAsset {{ id: b"{a.slug}", category: b"{a.category}", vb_w: {a.vb_w}, vb_h: {a.vb_h}, start: {start}, len: {len(a.shapes)} }},')
        items = [fmt_shape(s) for s in a.shapes]
        for k in range(0, len(items), per_line):
            shape_lines.append("    " + ", ".join(items[k : k + per_line]) + ",")
    fn_pfx = arr_prefix.lower()
    return "\n".join([
        RUST_HEADER.format(title=mod_title),
        f"pub const {arr_prefix}_ASSETS: &[VectorAsset] = &[", *entries, "];", "",
        f"pub const {arr_prefix}_SHAPES: &[VectorShape] = &[", *shape_lines, "];", "",
        f"#[inline] pub fn find_{fn_pfx}_asset(id: &[u8]) -> Option<&'static VectorAsset> {{",
        f"    for a in {arr_prefix}_ASSETS {{ if a.id == id {{ return Some(a); }} }} None",
        "}",
        f"#[inline] pub fn {fn_pfx}_shapes(asset: &VectorAsset) -> &'static [VectorShape] {{",
        f"    let (s, e) = (asset.start as usize, asset.start as usize + asset.len as usize);",
        f"    {arr_prefix}_SHAPES.get(s..e).unwrap_or(&[])",
        "}", "",
    ])


class Rasterizer:
    def __init__(self, w: int, h: int, bg_rgb: int) -> None:
        self.w, self.h = w, h
        self.buf = bytearray([(bg_rgb >> 16) & 255, (bg_rgb >> 8) & 255, bg_rgb & 255]) * (w * h)

    def blend(self, x: int, y: int, rgb: int, alpha: int) -> None:
        if alpha <= 0 or x < 0 or y < 0 or x >= self.w or y >= self.h:
            return
        idx = (y * self.w + x) * 3
        sr, sg, sb = (rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255
        if alpha >= 255:
            self.buf[idx], self.buf[idx + 1], self.buf[idx + 2] = sr, sg, sb
        else:
            inv = 255 - alpha
            self.buf[idx] = (self.buf[idx] * inv + sr * alpha + 127) // 255
            self.buf[idx + 1] = (self.buf[idx + 1] * inv + sg * alpha + 127) // 255
            self.buf[idx + 2] = (self.buf[idx + 2] * inv + sb * alpha + 127) // 255

    def _rrect_cov(self, rx: int, ry: int, rw: int, rh: int, rad: int, px: int, py: int) -> int:
        if rad <= 0:
            return 255
        lx, ly = px - rx, py - ry
        il, ir, it, ib = lx < rad, lx >= rw - rad, ly < rad, ly >= rh - rad
        if not (il or ir) or not (it or ib):
            return 255
        rq4 = rad * 16
        dx, dy = (lx * 16 + 8) - (rq4 if il else (rw - rad) * 16), (ly * 16 + 8) - (rq4 if it else (rh - rad) * 16)
        return (max(0, min(20, 10 - (math.isqrt(dx * dx + dy * dy) - rq4))) * 255 + 10) // 20

    def fill_rrect(self, x: int, y: int, w: int, h: int, rad: int, top: int, bot: int | None, alpha: int) -> None:
        if w <= 0 or h <= 0 or alpha <= 0:
            return
        rad, den = min(rad, w // 2, h // 2), max(1, h - 1)
        for py in range(max(0, y), min(self.h, y + h)):
            t = py - y
            inv = den - t
            rgb = top if bot is None else (((((top >> 16) & 255) * inv + ((bot >> 16) & 255) * t + den // 2) // den << 16) |
                                           ((((top >> 8) & 255) * inv + ((bot >> 8) & 255) * t + den // 2) // den << 8) |
                                           (((top & 255) * inv + (bot & 255) * t + den // 2) // den))
            for px in range(max(0, x), min(self.w, x + w)):
                if (a := (self._rrect_cov(x, y, w, h, rad, px, py) * alpha + 127) // 255):
                    self.blend(px, py, rgb, a)

    def stroke_rrect(self, x: int, y: int, w: int, h: int, rad: int, rgb: int, alpha: int) -> None:
        if w < 2 or h < 2 or alpha <= 0:
            return
        rad = min(rad, w // 2, h // 2)
        irad = max(0, rad - 1)
        for py in range(max(0, y), min(self.h, y + h)):
            for px in range(max(0, x), min(self.w, x + w)):
                if (oc := self._rrect_cov(x, y, w, h, rad, px, py)):
                    ic = self._rrect_cov(x + 1, y + 1, w - 2, h - 2, irad, px, py) if (x + 1 <= px < x + w - 1 and y + 1 <= py < y + h - 1) else 0
                    if oc > ic:
                        self.blend(px, py, rgb, ((oc - ic) * alpha + 127) // 255)

    def circle(self, cxq4: int, cyq4: int, rq4: int, rgb: int, alpha: int) -> None:
        if rq4 <= 0 or alpha <= 0:
            return
        for py in range(max(0, (cyq4 - rq4 - 16) >> 4), min(self.h, (cyq4 + rq4 + 32) >> 4)):
            pyq4 = py * 16 + 8 - cyq4
            for px in range(max(0, (cxq4 - rq4 - 16) >> 4), min(self.w, (cxq4 + rq4 + 32) >> 4)):
                pxq4 = px * 16 + 8 - cxq4
                if (cov := (max(0, min(20, 10 - (math.isqrt(pxq4 * pxq4 + pyq4 * pyq4) - rq4))) * 255 + 10) // 20):
                    self.blend(px, py, rgb, (cov * alpha + 127) // 255)

    def ring(self, cxq4: int, cyq4: int, rq4: int, thq4: int, rgb: int, alpha: int) -> None:
        if rq4 <= 0 or thq4 <= 0 or alpha <= 0:
            return
        ht, pad = max(4, thq4 // 2), rq4 + max(4, thq4 // 2) + 20
        for py in range(max(0, (cyq4 - pad) >> 4), min(self.h, (cyq4 + pad + 16) >> 4)):
            pyq4 = py * 16 + 8 - cyq4
            for px in range(max(0, (cxq4 - pad) >> 4), min(self.w, (cxq4 + pad + 16) >> 4)):
                pxq4 = px * 16 + 8 - cxq4
                if (cov := (max(0, min(18, 9 - (abs(math.isqrt(pxq4 * pxq4 + pyq4 * pyq4) - rq4) - ht))) * 255 + 9) // 18):
                    self.blend(px, py, rgb, (cov * alpha + 127) // 255)

    def line(self, x0q4: int, y0q4: int, x1q4: int, y1q4: int, thq4: int, rgb: int, alpha: int) -> None:
        if thq4 <= 0 or alpha <= 0:
            return
        ht, pad = max(4, thq4 // 2), max(4, thq4 // 2) + 20
        vx, vy, lsq = x1q4 - x0q4, y1q4 - y0q4, (x1q4 - x0q4) ** 2 + (y1q4 - y0q4) ** 2
        for py in range(max(0, (min(y0q4, y1q4) - pad) >> 4), min(self.h, (max(y0q4, y1q4) + pad + 16) >> 4)):
            pyq4 = py * 16 + 8
            for px in range(max(0, (min(x0q4, x1q4) - pad) >> 4), min(self.w, (max(x0q4, x1q4) + pad + 16) >> 4)):
                wx, wy = (px * 16 + 8) - x0q4, pyq4 - y0q4
                if lsq == 0:
                    dq4 = math.isqrt(wx * wx + wy * wy)
                else:
                    dot = max(0, min(lsq, wx * vx + wy * vy))
                    qx, qy = wx - (dot * vx) // lsq, wy - (dot * vy) // lsq
                    dq4 = math.isqrt(qx * qx + qy * qy)
                if (cov := (max(0, min(18, 9 - (dq4 - ht))) * 255 + 9) // 18):
                    self.blend(px, py, rgb, (cov * alpha + 127) // 255)

    def draw_text(self, x: int, y: int, text: str, rgb: int, scale: int = 1) -> None:
        for idx, ch in enumerate(text.upper()):
            hx = FONT5X7_HEX.get(ch, FONT5X7_HEX[" "])
            for ry in range(7):
                bits = int(hx[ry * 2 : ry * 2 + 2], 16)
                for rx in range(5):
                    if (bits >> (4 - rx)) & 1:
                        for sy in range(scale):
                            for sx in range(scale):
                                self.blend(x + idx * 6 * scale + rx * scale + sx, y + ry * scale + sy, rgb, 255)

    def draw_asset(self, a: Asset, dx: int, dy: int, dw: int, dh: int, theme: str) -> None:
        pal, small = THEME_PALETTES[theme], min(dw, dh) <= 20
        mx, my, ms = lambda v: dx * 16 + (v * dw + 128) // 256, lambda v: dy * 16 + (v * dh + 128) // 256, lambda v: max(12, (v * dh + 128) // 256)
        for s in a.shapes:
            if (s.optical == "SmallOnly" and not small) or (s.optical == "LargeOnly" and small):
                continue
            rgb0 = s.rgb_top if s.slot == "Custom" else pal[s.slot]
            if s.kind in ("RRect", "VGradRRect"):
                rx, ry = dx + (s.x * dw + 2048) // 4096, dy + (s.y * dh + 2048) // 4096
                rw, rh = max(1, (s.w * dw + 2048) // 4096), max(1, (s.h * dh + 2048) // 4096)
                rrad = (s.radius * dh + 2048) // 4096
                if s.kind == "VGradRRect":
                    self.fill_rrect(rx, ry, rw, rh, rrad, rgb0, s.rgb_bot, s.alpha)
                elif s.thick == 0:
                    self.fill_rrect(rx, ry, rw, rh, rrad, rgb0, None, s.alpha)
                else:
                    self.stroke_rrect(rx, ry, rw, rh, rrad, rgb0, s.alpha)
            elif s.kind == "Circle":
                self.circle(mx(s.x), my(s.y), ms(s.w), rgb0, s.alpha)
            elif s.kind == "Ring":
                self.ring(mx(s.x), my(s.y), ms(s.w), ms(s.thick), rgb0, s.alpha)
            elif s.kind == "Line":
                self.line(mx(s.x), my(s.y), mx(s.w), my(s.h), ms(s.thick), rgb0, s.alpha)

    def png_bytes(self) -> bytes:
        stride = self.w * 3
        raw = b"".join(b"\x00" + bytes(self.buf[y * stride : (y + 1) * stride]) for y in range(self.h))
        chunk = lambda tag, d: struct.pack(">I", len(d)) + tag + d + struct.pack(">I", zlib.crc32(tag + d) & 0xFFFFFFFF)
        return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", self.w, self.h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")


def render_theme_sheet(theme: str, brand: list[Asset], apps: list[Asset], sys_icons: list[Asset]) -> bytes:
    pal, by_slug = THEME_PALETTES[theme], {a.slug: a for a in brand + apps + sys_icons}
    w, h = 1320, 1140
    r = Rasterizer(w, h, pal["bg"])
    r.draw_text(24, 16, f"DRY OS VISUAL IDENTITY + VECTOR SYSTEM ({theme.upper()})", pal["title"], scale=2)
    r.draw_text(24, 36, f"BRAND: {len(brand)}  APPS: {len(apps)}  SYSTEM: {len(sys_icons)}  TOTAL: {len(brand)+len(apps)+len(sys_icons)}", pal["label"])
    r.fill_rrect(24, 54, w - 48, 134, 12, pal["card"], None, 255)
    r.stroke_rrect(24, 54, w - 48, 134, 12, pal["border"], 255)
    r.draw_asset(by_slug["dryos-symbol"], 44, 72, 64, 64, theme)
    r.draw_text(42, 152, "SYMBOL 64", pal["label"])
    r.draw_asset(by_slug["dryos-symbol-small-16"], 156, 96, 16, 16, theme)
    r.draw_text(132, 152, "SMALL 16", pal["label"])
    wm_slug = {"dark": "dryos-wordmark-dark", "light": "dryos-wordmark-light", "hc": "dryos-wordmark-hc"}[theme]
    r.draw_asset(by_slug[wm_slug], 230, 78, 260, 52, theme)
    r.draw_text(230, 152, wm_slug.upper(), pal["label"])
    r.draw_asset(by_slug["dryos-lockup-boot"], 540, 66, 360, 90, theme)
    r.draw_text(540, 166, "DRYOS-LOCKUP-BOOT", pal["label"])

    r.draw_text(24, 202, "APPLICATION ICONS (21 CUSTOM 64X64 SQUIRCLES)", pal["title"])
    for idx, a in enumerate(apps):
        cx, cy = 24 + (idx % 7) * 182, 218 + (idx // 7) * 104
        r.fill_rrect(cx, cy, 172, 96, 10, pal["card"], None, 255)
        r.stroke_rrect(cx, cy, 172, 96, 10, pal["border"], 255)
        r.draw_asset(a, cx + 54, cy + 10, 64, 64, theme)
        if theme == "hc":
            r.stroke_rrect(cx + 58, cy + 14, 56, 56, 14, pal["border"], 255)
        r.draw_text(cx + 8, cy + 80, a.slug[:24], pal["label"])

    r.draw_text(24, 542, f"SYSTEM ICONOGRAPHY ({len(sys_icons)} X 24X24 GLYPHS)", pal["title"])
    for idx, a in enumerate(sys_icons):
        cx, cy = 24 + (idx % 13) * 98, 560 + (idx // 13) * 78
        r.fill_rrect(cx, cy, 92, 70, 8, pal["card"], None, 255)
        r.stroke_rrect(cx, cy, 92, 70, 8, pal["border"], 255)
        r.draw_asset(a, cx + 34, cy + 12, 24, 24, theme)
        r.draw_text(cx + 4, cy + 50, a.slug[:14], pal["label"])
    return r.png_bytes()


def render_scales_sheet(brand: list[Asset], apps: list[Asset], sys_icons: list[Asset]) -> bytes:
    pal, by_slug = THEME_PALETTES["dark"], {a.slug: a for a in brand + apps + sys_icons}
    scales = [16, 20, 24, 32, 48, 64, 128, 256]
    targets = ["dryos-symbol", "terminal", "life-station", "files", "security-vault", "driver-manager", "cpu", "status-success"]
    row_h, w = 304, 880
    r = Rasterizer(w, 72 + row_h * len(targets), pal["bg"])
    r.draw_text(24, 16, "DRY OS MULTI-SCALE OPTICAL SHEET (16..256 PX)", pal["title"], scale=2)
    r.draw_text(24, 40, "OPTICAL SMALL-ONLY/LARGE-ONLY SWITCHING AT 16-20 PX", pal["label"])
    y = 66
    for slug in targets:
        a = by_slug[slug]
        r.fill_rrect(24, y, w - 48, row_h - 10, 12, pal["card"], None, 255)
        r.stroke_rrect(24, y, w - 48, row_h - 10, 12, pal["border"], 255)
        r.draw_text(36, y + 10, slug.upper(), pal["title"])
        x = 36
        for sc in scales:
            use_a = by_slug["dryos-symbol-small-16"] if (slug == "dryos-symbol" and sc <= 20) else a
            r.draw_asset(use_a, x, y + 26 + (256 - sc) // 2, sc, sc, "dark")
            r.draw_text(x, y + row_h - 22, str(sc), pal["label"])
            x += sc + 22
        y += row_h
    return r.png_bytes()


def run_build(verify_only: bool) -> int:
    brand_files = sorted(BRAND_DIR.glob("*.svg"), key=lambda p: p.stem)
    app_files = sorted(APPS_DIR.glob("*.svg"), key=lambda p: p.stem)
    sys_files = sorted(SYS_DIR.glob("*.svg"), key=lambda p: p.stem)
    assert [p.stem for p in brand_files] == sorted(REQ_BRAND), f"Brand mismatch: {[p.stem for p in brand_files]}"
    assert [p.stem for p in app_files] == sorted(REQ_APPS), f"App mismatch: {[p.stem for p in app_files]}"
    assert len(sys_files) >= 80, f"Expected >= 80 system icons, got {len(sys_files)}"

    brand = [parse_svg_file(p, "brand") for p in brand_files]
    apps = [parse_svg_file(p, "apps") for p in app_files]
    sys_icons = [parse_svg_file(p, "system") for p in sys_files]

    sys_rs = generate_rust_module(sys_icons, "DRY OS System Vector Iconography", "SYS")
    apps_rs = generate_rust_module(brand + apps, "DRY OS Brand & Application Vector Iconography", "APP")
    assert len(sys_rs.splitlines()) < 750, f"vector_assets_sys.rs lines {len(sys_rs.splitlines())} >= 750"
    assert len(apps_rs.splitlines()) < 750, f"vector_assets_apps.rs lines {len(apps_rs.splitlines())} >= 750"

    manifest = {
        "schema_version": 1, "license": "Apache-2.0 OR MIT",
        "counts": {"brand": len(brand), "apps": len(apps), "system": len(sys_icons), "total_svg": len(brand) + len(apps) + len(sys_icons)},
        "generated_rust": {
            "userspace/src/ui/vector_assets_sys.rs": {"sha256": hashlib.sha256(sys_rs.encode("ascii")).hexdigest(), "lines": len(sys_rs.splitlines())},
            "userspace/src/ui/vector_assets_apps.rs": {"sha256": hashlib.sha256(apps_rs.encode("ascii")).hexdigest(), "lines": len(apps_rs.splitlines())},
        },
        "assets": [{"id": a.slug, "category": a.category, "path": a.rel_path, "viewBox": f"0 0 {a.vb_w} {a.vb_h}", "shapes": len(a.shapes), "sha256": a.sha256}
                   for a in (brand + apps + sys_icons)],
    }
    manifest_text = json.dumps(manifest, indent=2) + "\n"
    pngs = {
        "contact_sheet_dark.png": render_theme_sheet("dark", brand, apps, sys_icons),
        "contact_sheet_light.png": render_theme_sheet("light", brand, apps, sys_icons),
        "contact_sheet_hc.png": render_theme_sheet("hc", brand, apps, sys_icons),
        "contact_sheet_scales.png": render_scales_sheet(brand, apps, sys_icons),
    }

    if verify_only:
        assert SYS_RS_PATH.read_text(encoding="ascii") == sys_rs, "vector_assets_sys.rs is stale"
        assert APPS_RS_PATH.read_text(encoding="ascii") == apps_rs, "vector_assets_apps.rs is stale"
        assert MANIFEST_PATH.read_text(encoding="ascii") == manifest_text, "ASSET_MANIFEST.json is stale"
        for name, expected in pngs.items():
            assert (SHOT_DIR / name).read_bytes() == expected, f"Stale PNG contact sheet: {name}"
    else:
        SYS_RS_PATH.write_text(sys_rs, encoding="ascii")
        APPS_RS_PATH.write_text(apps_rs, encoding="ascii")
        MANIFEST_PATH.write_text(manifest_text, encoding="ascii")
        SHOT_DIR.mkdir(parents=True, exist_ok=True)
        for name, data in pngs.items():
            (SHOT_DIR / name).write_bytes(data)

    print(
        f"[VECTOR-ASSETS] OK | brand={len(brand)} apps={len(apps)} system={len(sys_icons)} "
        f"total_svg={len(brand)+len(apps)+len(sys_icons)} | sys_rs_lines={len(sys_rs.splitlines())} "
        f"apps_rs_lines={len(apps_rs.splitlines())} | sys_shapes={sum(len(a.shapes) for a in sys_icons)} "
        f"app_brand_shapes={sum(len(a.shapes) for a in brand + apps)}"
    )
    return 0


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description="Build and verify DRY OS vector assets")
    ap.add_argument("--verify", action="store_true", help="Verify SVGs, generated Rust tables, manifest, and contact sheets")
    sys.exit(run_build(ap.parse_args().verify))
