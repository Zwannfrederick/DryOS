#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR GPL-2.0-only
"""Unit tests for DRY OS Linux Compatibility Runtime Manager (`lx_driver_manager.py`)."""

import hashlib
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import lx_driver_manager as mgr  # noqa: E402


class TestLxDriverManager(unittest.TestCase):
    def test_match_pci_known_and_unknown(self) -> None:
        bochs = mgr.match_pci("1234:1111")
        self.assertTrue(bochs["matched"])
        self.assertEqual(bochs["matches"][0]["driver"], "drm_bochs")
        self.assertEqual(bochs["matches"][0]["compartment_id"], "0x0212")

        rtl = mgr.match_pci("10ec:8139")
        self.assertTrue(rtl["matched"])
        self.assertEqual(rtl["matches"][0]["driver"], "net_8139cp")
        self.assertEqual(rtl["matches"][0]["compartment_id"], "0x0214")
        self.assertEqual(rtl["matches"][0]["backend"], "lx-compat-island")

        intel = mgr.match_pci("0x8086:0x9A49")
        self.assertTrue(intel["matched"])
        self.assertEqual(intel["matches"][0]["driver"], "intel_i915_tgl")
        self.assertEqual(intel["matches"][0]["backend"], "linux-microvm")

        nv = mgr.match_pci("10de:25a0")
        self.assertTrue(nv["matched"])
        self.assertEqual(nv["matches"][0]["driver"], "nvidia_open_ga107")

        unknown = mgr.match_pci("ffff:0000")
        self.assertFalse(unknown["matched"])

    def test_15_step_lifecycle_and_backends(self) -> None:
        self.assertEqual(len(mgr.LIFECYCLE_STEPS), 15)
        lc = mgr.DriverLifecycle("net_8139cp", "10ec:8139", "lx-compat-island")
        self.assertEqual(lc.state, "DETECTED")
        with self.assertRaises(ValueError):
            lc.advance("MONITORING")

        lc.run_bringup_to_monitoring()
        self.assertEqual(lc.state, "MONITORING")
        self.assertEqual(lc.handle_fault_and_respawn(), 2)
        self.assertEqual(lc.state, "MONITORING")
        lc.unload_and_rollback()
        self.assertEqual(lc.state, "ROLLED_BACK")
        self.assertEqual(set(lc.history), set(mgr.LIFECYCLE_STEPS))

        for bname in ("native-driver-island", "lx-compat-island", "linux-microvm"):
            contract = mgr.validate_backend_contract(bname)
            self.assertTrue(contract["ring0_ko_forbidden"])
            self.assertFalse(contract["igpu_passthrough_assumed"])

        insp = mgr.drvctl_inspect(driver_name="net_8139cp")
        self.assertEqual(insp["lifecycle"]["state"], "MONITORING")
        self.assertEqual(insp["assessment"]["compartment_id"], "0x0214")

    def test_reject_ko_and_elf_rel(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            ko_path = pathlib.Path(td) / "bochs.ko"
            ko_path.write_bytes(b"\x00")
            ok, code, _ = mgr.check_load_module(ko_path)
            self.assertFalse(ok)
            self.assertEqual(code, "KO_BINARY_REJECTED")

            elf_rel = pathlib.Path(td) / "driver_blob.bin"
            elf_rel.write_bytes(b"\x7fELF" + b"\x00" * 12 + (1).to_bytes(2, "little"))
            ok2, code2, _ = mgr.check_load_module(elf_rel)
            self.assertFalse(ok2)
            self.assertEqual(code2, "KO_BINARY_REJECTED")

    def test_tamper_detection_on_temp_root(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            troot = pathlib.Path(td)
            src = troot / "bochs.c"
            src.write_bytes(b"int ok = 1;\n")
            digest = hashlib.sha256(src.read_bytes()).hexdigest()
            cat_path = troot / "catalog.json"
            cat_path.write_text(
                json.dumps(
                    {
                        "schema_version": "1.1",
                        "linux_tag": "v6.18",
                        "linux_commit": "0" * 40,
                        "drivers": {
                            "test_drv": {
                                "local_path": "bochs.c",
                                "sha256": digest,
                                "license": "GPL-2.0-only",
                                "pci_ids": ["1234:1111"],
                                "status": "active_verified",
                            }
                        },
                    }
                ),
                encoding="utf-8",
            )
            manifest_path = troot / "manifest.json"
            manifest_path.write_text("{}", encoding="utf-8")

            checked = mgr.verify_all(root=troot, catalog_path=cat_path, manifest_path=manifest_path)
            self.assertEqual(len(checked), 1)

            src.write_bytes(b"int ok = 2;\n")
            with self.assertRaises(RuntimeError):
                mgr.verify_all(root=troot, catalog_path=cat_path, manifest_path=manifest_path)

    def test_content_addressed_cache_hit_and_pin_rollback(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            troot = pathlib.Path(td)
            src = troot / "sample.c"
            src.write_text("int dry_sample_fn(int x) { return x + 1; }\n", encoding="utf-8")
            inc = troot / "inc"
            inc.mkdir()
            (inc / "sample.h").write_text("#define SAMPLE_VER 1\n", encoding="utf-8")
            cache_dir = troot / "cache"
            cat_path = troot / "cat.json"
            cat_path.write_text(
                json.dumps(
                    {
                        "schema_version": "1.1",
                        "linux_tag": "v6.18",
                        "linux_commit": "0" * 40,
                        "drivers": {
                            "sample": {
                                "local_path": str(src),
                                "sha256": hashlib.sha256(src.read_bytes()).hexdigest(),
                                "license": "GPL-2.0-only",
                                "cflags": [],
                                "status": "active_verified",
                            }
                        },
                    }
                ),
                encoding="utf-8",
            )
            orig_root = mgr.ROOT
            try:
                mgr.ROOT = pathlib.Path("/")
                m1 = mgr.build_cache(
                    "sample",
                    cache_dir=cache_dir,
                    extra_c_sources=[],
                    include_dirs=[inc],
                    catalog_path=cat_path,
                )
                self.assertFalse(m1["cache_hit"])
                self.assertTrue(pathlib.Path(m1["archive_path"]).exists())

                m2 = mgr.build_cache(
                    "sample",
                    cache_dir=cache_dir,
                    extra_c_sources=[],
                    include_dirs=[inc],
                    catalog_path=cat_path,
                )
                self.assertTrue(m2["cache_hit"])
                self.assertEqual(m1["build_key"], m2["build_key"])
            finally:
                mgr.ROOT = orig_root

            pins_file = troot / "pins.json"
            r1 = mgr.pin_version("drm_bochs", "v6.18", pins_file)
            self.assertEqual(r1["current"], "v6.18")
            r2 = mgr.pin_version("drm_bochs", "v6.19-rc1", pins_file)
            self.assertEqual(r2["current"], "v6.19-rc1")
            r3 = mgr.rollback_version("drm_bochs", pins_file)
            self.assertEqual(r3["current"], "v6.18")


if __name__ == "__main__":
    unittest.main()
