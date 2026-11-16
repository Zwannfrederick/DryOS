#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""DRY OS Linux Driver Compatibility Runtime Manager: fetch, SHA-256 verify, match, cache, lifecycle, drvctl."""

import argparse
import hashlib
import json
import pathlib
import subprocess
import sys
import urllib.request
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
CATALOG_PATH = ROOT / "tools" / "lx_compat" / "COMPAT_CATALOG.json"
MANIFEST_PATH = ROOT / "vendor" / "linux-drivers" / "MANIFEST.json"
DEFAULT_CACHE_DIR = ROOT / "kernel" / "build" / "lx_cache"
DEFAULT_LIBDRY_DIR = ROOT / "drivers" / "libdry-lx-compat"

LIFECYCLE_STEPS: tuple[str, ...] = (
    "DETECTED",
    "MATCHED",
    "ASSESSED",
    "SOURCE_RESOLVED",
    "DEPS_FIRMWARE_OK",
    "BUILT_VERIFIED",
    "ISLAND_CREATED",
    "CAPS_ASSIGNED",
    "RESOURCES_MAPPED",
    "INITIALIZED",
    "PUBLISHED",
    "MONITORING",
    "FAULT_RECOVERY",
    "UNLOADED",
    "ROLLED_BACK",
)

VALID_TRANSITIONS: dict[str, tuple[str, ...]] = {
    "DETECTED": ("MATCHED", "ROLLED_BACK"),
    "MATCHED": ("ASSESSED", "UNLOADED", "ROLLED_BACK"),
    "ASSESSED": ("SOURCE_RESOLVED", "UNLOADED", "ROLLED_BACK"),
    "SOURCE_RESOLVED": ("DEPS_FIRMWARE_OK", "ROLLED_BACK"),
    "DEPS_FIRMWARE_OK": ("BUILT_VERIFIED", "ROLLED_BACK"),
    "BUILT_VERIFIED": ("ISLAND_CREATED", "ROLLED_BACK"),
    "ISLAND_CREATED": ("CAPS_ASSIGNED", "FAULT_RECOVERY", "UNLOADED"),
    "CAPS_ASSIGNED": ("RESOURCES_MAPPED", "FAULT_RECOVERY", "UNLOADED"),
    "RESOURCES_MAPPED": ("INITIALIZED", "FAULT_RECOVERY", "UNLOADED"),
    "INITIALIZED": ("PUBLISHED", "FAULT_RECOVERY", "UNLOADED"),
    "PUBLISHED": ("MONITORING", "FAULT_RECOVERY", "UNLOADED"),
    "MONITORING": ("FAULT_RECOVERY", "UNLOADED"),
    "FAULT_RECOVERY": ("ISLAND_CREATED", "UNLOADED", "ROLLED_BACK"),
    "UNLOADED": ("ROLLED_BACK", "DETECTED"),
    "ROLLED_BACK": ("SOURCE_RESOLVED", "DETECTED"),
}

BASE_CFLAGS = [
    "clang", "-std=gnu11", "-target", "x86_64-unknown-none-elf",
    "-ffreestanding", "-fno-builtin", "-fno-stack-protector",
    "-fno-pic", "-fno-pie", "-mno-red-zone", "-mno-sse", "-mno-sse2", "-mno-avx",
    "-mcmodel=small", "-O2", "-Wall", "-Wextra", "-Wno-unused-parameter", "-Werror",
]


class DriverLifecycle:
    """Explicit 15-step driver lifecycle state machine with transition validation."""

    def __init__(self, driver: str, pci_id: str, backend: str) -> None:
        self.driver = driver
        self.pci_id = pci_id
        self.backend = backend
        self.state = "DETECTED"
        self.generation = 1
        self.restarts = 0
        self.history: list[str] = ["DETECTED"]

    def advance(self, next_state: str) -> str:
        if next_state not in LIFECYCLE_STEPS:
            raise ValueError(f"Unknown lifecycle state: {next_state!r}")
        allowed = VALID_TRANSITIONS.get(self.state, ())
        if next_state not in allowed:
            raise ValueError(f"Illegal transition {self.state} -> {next_state} (allowed={allowed})")
        self.state = next_state
        self.history.append(next_state)
        return self.state

    def run_bringup_to_monitoring(self) -> list[str]:
        for step in LIFECYCLE_STEPS[1:12]:
            self.advance(step)
        return list(self.history)

    def handle_fault_and_respawn(self) -> int:
        self.advance("FAULT_RECOVERY")
        self.restarts += 1
        self.generation += 1
        for step in ("ISLAND_CREATED", "CAPS_ASSIGNED", "RESOURCES_MAPPED", "INITIALIZED", "PUBLISHED", "MONITORING"):
            self.advance(step)
        return self.generation

    def unload_and_rollback(self) -> list[str]:
        if self.state != "UNLOADED":
            self.advance("UNLOADED")
        self.advance("ROLLED_BACK")
        return list(self.history)

    def to_dict(self) -> dict[str, Any]:
        return {
            "driver": self.driver,
            "pci_id": self.pci_id,
            "backend": self.backend,
            "state": self.state,
            "generation": self.generation,
            "restarts": self.restarts,
            "visited_steps": sorted(set(self.history), key=LIFECYCLE_STEPS.index),
            "history": list(self.history),
        }


def load_catalog(catalog_path: pathlib.Path = CATALOG_PATH) -> dict[str, Any]:
    return json.loads(catalog_path.read_text(encoding="utf-8"))


def sha256_file(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _http_get(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "dryos-lx-compat/0.1"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read()


def fetch_driver(driver_name: str, catalog_path: pathlib.Path = CATALOG_PATH) -> dict[str, Any]:
    cat = load_catalog(catalog_path)
    drv = cat["drivers"].get(driver_name)
    if not drv or not drv.get("local_path") or not drv.get("upstream_path"):
        raise ValueError(f"Driver {driver_name!r} has no single-file upstream fetch source in catalog")

    commit = cat["linux_commit"]
    tag = cat["linux_tag"]
    up_path = drv["upstream_path"]
    dest = ROOT / drv["local_path"]
    urls = [
        f"https://raw.githubusercontent.com/torvalds/linux/{commit}/{up_path}",
        f"https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/{up_path}?id={commit}",
    ]
    raw = None
    last_err: Exception | None = None
    for url in urls:
        try:
            raw = _http_get(url)
            break
        except Exception as exc:  # noqa: BLE001
            last_err = exc
    if raw is None:
        raise RuntimeError(f"Failed to download {up_path}@{commit}: {last_err}")

    digest = hashlib.sha256(raw).hexdigest()
    expected = drv.get("sha256", "")
    if expected and digest != expected:
        raise RuntimeError(f"Downloaded SHA-256 mismatch for {driver_name}: {digest} != {expected}")

    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(raw)
    lines = raw.decode("utf-8", errors="replace").count("\n")

    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8")) if MANIFEST_PATH.exists() else {}
    manifest[driver_name] = {
        "upstream_path": up_path,
        "tag": tag,
        "commit_sha": commit,
        "license": drv["license"],
        "local_path": drv["local_path"],
        "bytes": len(raw),
        "lines": lines,
        "sha256": digest,
    }
    MANIFEST_PATH.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return manifest[driver_name]


def verify_all(
    root: pathlib.Path = ROOT,
    catalog_path: pathlib.Path = CATALOG_PATH,
    manifest_path: pathlib.Path = MANIFEST_PATH,
) -> list[dict[str, str]]:
    cat = load_catalog(catalog_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8")) if manifest_path.exists() else {}
    checked: list[dict[str, str]] = []

    for name, drv in cat["drivers"].items():
        local_rel = drv.get("local_path", "")
        expected = drv.get("sha256", "")
        if local_rel and expected:
            p = root / local_rel
            if not p.exists():
                raise RuntimeError(f"Missing vendored driver source: {p}")
            actual = sha256_file(p)
            if actual != expected:
                raise RuntimeError(f"SHA-256 tamper/mismatch for {name} ({local_rel}): {actual} != {expected}")
            checked.append({"driver": name, "path": local_rel, "sha256": actual})

        for idx, comp in enumerate(drv.get("companion_sources", [])):
            crel = comp.get("local_path", "")
            cexp = comp.get("sha256", "")
            if not crel or not cexp:
                continue
            cp = root / crel
            if not cp.exists():
                raise RuntimeError(f"Missing companion driver source: {cp}")
            cact = sha256_file(cp)
            if cact != cexp:
                raise RuntimeError(f"SHA-256 tamper/mismatch for {name}[{idx}] ({crel}): {cact} != {cexp}")
            checked.append({"driver": f"{name}:companion{idx}", "path": crel, "sha256": cact})

    for mkey in ("net_8139cp", "net_mii"):
        mrec = manifest.get(mkey)
        if isinstance(mrec, dict) and mrec.get("local_path") and mrec.get("sha256"):
            mp = root / mrec["local_path"]
            if not mp.exists():
                raise RuntimeError(f"Missing manifest source {mp}")
            mact = sha256_file(mp)
            if mact != mrec["sha256"]:
                raise RuntimeError(f"Manifest SHA-256 mismatch for {mkey}: {mact} != {mrec['sha256']}")

    for ver, meta in manifest.get("versions", {}).items():
        base_dir = root / "vendor" / "linux-drivers" / ver / "igc"
        for fentry in meta.get("files", []):
            fp = base_dir / fentry["name"]
            if not fp.exists():
                raise RuntimeError(f"Missing igc file: {fp}")
            actual = sha256_file(fp)
            if actual != fentry["sha256"]:
                raise RuntimeError(f"SHA-256 mismatch for {fp}: {actual} != {fentry['sha256']}")
            checked.append({"driver": f"igc:{ver}/{fentry['name']}", "path": str(fp.relative_to(root)), "sha256": actual})

    return checked


def validate_backend_contract(backend_name: str, catalog_path: pathlib.Path = CATALOG_PATH) -> dict[str, Any]:
    cat = load_catalog(catalog_path)
    backends = cat.get("backends", {})
    if backend_name not in backends:
        raise ValueError(f"Unknown backend contract: {backend_name!r}")
    spec = backends[backend_name]
    if not spec.get("ring0_ko_forbidden", False):
        raise RuntimeError(f"Backend {backend_name!r} violates ring0_ko_forbidden contract")
    if spec.get("igpu_passthrough_assumed", True):
        raise RuntimeError(f"Backend {backend_name!r} illegally assumes physical iGPU passthrough")
    return {
        "backend": backend_name,
        "ring0_ko_forbidden": True,
        "igpu_passthrough_assumed": False,
        "isolation": spec["isolation"],
        "fault_ep_supervised": bool(spec.get("fault_ep_supervised", False)),
    }


def assess_driver(driver_name: str, catalog_path: pathlib.Path = CATALOG_PATH) -> dict[str, Any]:
    cat = load_catalog(catalog_path)
    drv = cat["drivers"].get(driver_name)
    if not drv:
        raise ValueError(f"Unknown driver {driver_name!r}")
    backend_name = drv.get("backend", "lx-compat-island")
    contract = validate_backend_contract(backend_name, catalog_path)
    return {
        "driver": driver_name,
        "license": drv["license"],
        "status": drv["status"],
        "runtime_island": drv.get("runtime_island", ""),
        "compartment_id": drv.get("compartment_id", ""),
        "backend": contract,
        "capabilities": drv.get("capabilities", []),
        "firmware_blobs": drv.get("firmware_blobs", []),
    }


def match_pci(vendor_device: str, catalog_path: pathlib.Path = CATALOG_PATH) -> dict[str, Any]:
    norm = vendor_device.strip().lower()
    if norm.startswith("0x"):
        norm = norm[2:]
    parts = norm.split(":")
    if len(parts) != 2:
        raise ValueError(f"Expected vendor:device hex pair, got {vendor_device!r}")
    norm = f"{int(parts[0], 16):04x}:{int(parts[1], 16):04x}"

    cat = load_catalog(catalog_path)
    matches = []
    for name, drv in cat["drivers"].items():
        ids = [x.lower() for x in drv.get("pci_ids", [])]
        if norm in ids:
            matches.append(
                {
                    "driver": name,
                    "pci_id": norm,
                    "status": drv["status"],
                    "backend": drv.get("backend", "lx-compat-island"),
                    "runtime_island": drv.get("runtime_island", ""),
                    "compartment_id": drv.get("compartment_id", ""),
                    "capabilities": drv.get("capabilities", []),
                }
            )
    return {"query": norm, "matched": bool(matches), "matches": matches}


def check_load_module(path: pathlib.Path) -> tuple[bool, str, str]:
    """Reject Linux `.ko` binaries or ELF ET_REL objects by design."""
    name = path.name.lower()
    if name.endswith((".ko", ".ko.zst", ".ko.xz", ".ko.gz")):
        return (
            False,
            "KO_BINARY_REJECTED",
            f"Refusing Linux .ko binary {path.name}: DRY OS uses source-level compilation against libdry-lx-compat in isolated Ring-3 islands, never kernel .ko loading.",
        )
    if path.exists() and path.is_file():
        hdr = path.read_bytes()[:18]
        if len(hdr) >= 18 and hdr[:4] == b"\x7fELF":
            e_type = int.from_bytes(hdr[16:18], "little")
            if e_type == 1:
                return (
                    False,
                    "KO_BINARY_REJECTED",
                    f"Refusing ELF ET_REL object {path.name}: binary .ko relocatables cannot load into DRY OS.",
                )
    return (False, "UNSUPPORTED_MODULE_PATH", f"Direct dynamic module loading disabled for {path}")


def _compute_build_key(c_files: list[pathlib.Path], h_files: list[pathlib.Path], flags: list[str]) -> str:
    h = hashlib.sha256()
    for f in flags:
        h.update(f.encode("utf-8"))
        h.update(b"\0")
    for src in sorted(c_files + h_files, key=lambda p: p.name):
        h.update(src.name.encode("utf-8"))
        h.update(sha256_file(src).encode("ascii"))
    return h.hexdigest()


def build_cache(
    driver_name: str,
    cache_dir: pathlib.Path = DEFAULT_CACHE_DIR,
    extra_c_sources: list[pathlib.Path] | None = None,
    include_dirs: list[pathlib.Path] | None = None,
    catalog_path: pathlib.Path = CATALOG_PATH,
) -> dict[str, Any]:
    cat = load_catalog(catalog_path)
    drv = cat["drivers"].get(driver_name)
    if not drv or not drv.get("local_path"):
        raise ValueError(f"Driver {driver_name!r} is not configured for source build")

    up_src = ROOT / drv["local_path"]
    if not up_src.exists():
        raise RuntimeError(f"Missing upstream C source {up_src}; run fetch first")
    expected = drv.get("sha256", "")
    if expected and sha256_file(up_src) != expected:
        raise RuntimeError(f"SHA-256 mismatch on {up_src}")

    comp_sources: list[pathlib.Path] = []
    for comp in drv.get("companion_sources", []):
        cp = ROOT / comp["local_path"]
        if not cp.exists():
            raise RuntimeError(f"Missing companion C source {cp}")
        cexp = comp.get("sha256", "")
        if cexp and sha256_file(cp) != cexp:
            raise RuntimeError(f"SHA-256 mismatch on {cp}")
        comp_sources.append(cp)

    if extra_c_sources is None:
        compat_srcs = sorted((DEFAULT_LIBDRY_DIR / "src").glob("*.c"))
        c_sources = [up_src] + comp_sources + compat_srcs
    else:
        c_sources = [up_src] + comp_sources + list(extra_c_sources)

    inc_dirs = include_dirs if include_dirs is not None else [DEFAULT_LIBDRY_DIR / "include"]
    headers: list[pathlib.Path] = []
    for inc in inc_dirs:
        if inc.exists():
            headers.extend(sorted(inc.rglob("*.h")))

    flags = BASE_CFLAGS + drv.get("cflags", []) + [f"-I{inc}" for inc in inc_dirs]
    key = _compute_build_key(c_sources, headers, flags)
    entry_dir = cache_dir / key
    archive_path = entry_dir / f"liblx_{driver_name}.a"
    meta_path = entry_dir / "meta.json"

    if archive_path.exists() and meta_path.exists():
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        if sha256_file(archive_path) == meta.get("archive_sha256"):
            meta["cache_hit"] = True
            return meta

    entry_dir.mkdir(parents=True, exist_ok=True)
    objs: list[pathlib.Path] = []
    for c_file in c_sources:
        obj = entry_dir / f"{c_file.stem}.o"
        subprocess.check_call(flags + ["-c", str(c_file), "-o", str(obj)])
        objs.append(obj)
    if archive_path.exists():
        archive_path.unlink()
    subprocess.check_call(["llvm-ar", "rcs", str(archive_path)] + [str(o) for o in objs])

    meta = {
        "driver": driver_name,
        "build_key": key,
        "cache_hit": False,
        "archive_path": str(archive_path),
        "archive_sha256": sha256_file(archive_path),
        "upstream_sha256": sha256_file(up_src),
        "objects": [o.name for o in objs],
    }
    meta_path.write_text(json.dumps(meta, indent=2) + "\n", encoding="utf-8")
    return meta


def pin_version(driver: str, version: str, state_path: pathlib.Path) -> dict[str, Any]:
    state = json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}
    rec = state.get(driver, {"current": None, "history": []})
    if rec["current"] is not None and rec["current"] != version:
        rec["history"].append(rec["current"])
    rec["current"] = version
    state[driver] = rec
    state_path.parent.mkdir(parents=True, exist_ok=True)
    state_path.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    return rec


def rollback_version(driver: str, state_path: pathlib.Path) -> dict[str, Any]:
    if not state_path.exists():
        raise RuntimeError("No pin history exists")
    state = json.loads(state_path.read_text(encoding="utf-8"))
    rec = state.get(driver)
    if not rec or not rec.get("history"):
        raise RuntimeError(f"No prior pinned version to roll back for {driver}")
    rec["current"] = rec["history"].pop()
    state[driver] = rec
    state_path.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    return rec


def drvctl_inspect(driver_name: str | None = None, pci_id: str | None = None) -> dict[str, Any]:
    cat = load_catalog()
    if pci_id:
        m = match_pci(pci_id)
        if not m["matched"]:
            return {"matched": False, "query": pci_id, "lifecycle_steps": list(LIFECYCLE_STEPS)}
        driver_name = m["matches"][0]["driver"]
    if driver_name:
        assessment = assess_driver(driver_name)
        drv = cat["drivers"][driver_name]
        pids = drv.get("pci_ids", [])
        lc = DriverLifecycle(driver_name, pids[0] if pids else "0000:0000", assessment["backend"]["backend"])
        lc.run_bringup_to_monitoring()
        return {"assessment": assessment, "lifecycle": lc.to_dict()}
    return {
        "lifecycle_steps": list(LIFECYCLE_STEPS),
        "backends": {k: validate_backend_contract(k) for k in cat.get("backends", {})},
        "drivers": {name: assess_driver(name) for name in cat["drivers"]},
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="DRY OS Linux Compatibility Runtime Manager")
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_fetch = sub.add_parser("fetch")
    p_fetch.add_argument("--driver", required=True)

    p_ver = sub.add_parser("verify")
    p_ver.add_argument("--all", action="store_true", required=True)

    p_match = sub.add_parser("match-pci")
    p_match.add_argument("pci_id")

    p_load = sub.add_parser("load-module")
    p_load.add_argument("path")

    p_cache = sub.add_parser("build-cache")
    p_cache.add_argument("--driver", required=True)
    p_cache.add_argument("--cache-dir", default=str(DEFAULT_CACHE_DIR))

    p_pin = sub.add_parser("pin")
    p_pin.add_argument("--driver", required=True)
    p_pin.add_argument("--version", required=True)
    p_pin.add_argument("--state-file", default=str(DEFAULT_CACHE_DIR / "pins.json"))

    p_rb = sub.add_parser("rollback")
    p_rb.add_argument("--driver", required=True)
    p_rb.add_argument("--state-file", default=str(DEFAULT_CACHE_DIR / "pins.json"))

    p_ctl = sub.add_parser("drvctl")
    p_ctl.add_argument("--driver", default=None)
    p_ctl.add_argument("--pci", default=None)

    args = parser.parse_args()
    if args.cmd == "fetch":
        print(json.dumps({"fetched": fetch_driver(args.driver)}, indent=2))
    elif args.cmd == "verify":
        checked = verify_all()
        print(f"[LX-COMPAT-VERIFY] OK: {len(checked)} upstream files SHA-256 verified (0 modified).")
    elif args.cmd == "match-pci":
        print(json.dumps(match_pci(args.pci_id), indent=2))
    elif args.cmd == "load-module":
        ok, code, msg = check_load_module(pathlib.Path(args.path))
        print(json.dumps({"ok": ok, "code": code, "message": msg}))
        if not ok:
            sys.exit(2)
    elif args.cmd == "build-cache":
        print(json.dumps(build_cache(args.driver, pathlib.Path(args.cache_dir)), indent=2))
    elif args.cmd == "pin":
        print(json.dumps(pin_version(args.driver, args.version, pathlib.Path(args.state_file))))
    elif args.cmd == "rollback":
        print(json.dumps(rollback_version(args.driver, pathlib.Path(args.state_file))))
    elif args.cmd == "drvctl":
        print(json.dumps(drvctl_inspect(args.driver, args.pci), indent=2))


if __name__ == "__main__":
    main()
