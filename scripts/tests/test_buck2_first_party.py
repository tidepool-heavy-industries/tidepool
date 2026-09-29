#!/usr/bin/env python3
"""Source-layout contracts for the native Buck Rust generator, without a build."""

import ast
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


GENERATOR = Path(__file__).resolve().parents[1] / "buck2-first-party.py"


class FirstPartySources(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "repo"
        (self.root / "scripts").mkdir(parents=True)
        shutil.copyfile(GENERATOR, self.root / "scripts/buck2-first-party.py")
        self.bin = self.base / "bin"
        self.bin.mkdir()
        cargo = self.bin / "cargo"
        cargo.write_text("#!/usr/bin/env python3\nimport os, sys\n"
                         "from pathlib import Path\n"
                         "sys.stdout.write(Path(os.environ['BUCK_TEST_METADATA']).read_text())\n")
        cargo.chmod(0o755)
        self.write("tidepool/repr/Cargo.toml", "[package]\nname = 'tidepool-repr'\n")
        self.write("bridge/atomic-write/Cargo.toml", "[package]\nname = 'tidepool-atomic-write'\n")
        self.write("bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor", "m3")
        self.write("bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor", "schema6")
        self.write("bridge/atomic-write/tests/fixtures/directory_fault.c", "fixture")
        self.write("tidepool/repr/src/lib.rs",
                   'const M3: &[u8] = include_bytes!("../../../bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor");\n')
        self.write("tidepool/repr/tests/suites/repr.rs",
                   '#[path = "../contract.rs"] mod contract;\n')
        self.write("tidepool/repr/tests/contract.rs",
                   'const SCHEMA: &[u8] = include_bytes!("../../../bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor");\n'
                   'const FAULT: &str = include_str!("../../../bridge/atomic-write/tests/fixtures/directory_fault.c");\n')
        self.write("bridge/atomic-write/src/lib.rs", "pub fn write() {}\n")
        self.write("bridge/atomic-write/tests/strict_directory.rs", "#[test] fn write() {}\n")
        self.packages = [self.package("tidepool-repr", "tidepool/repr", [
            ("tidepool_repr", "lib", "src/lib.rs"), ("repr", "test", "tests/suites/repr.rs")]),
            self.package("tidepool-atomic-write", "bridge/atomic-write", [
                ("tidepool_atomic_write", "lib", "src/lib.rs"),
                ("strict_directory", "test", "tests/strict_directory.rs")])]
        self.metadata = self.base / "metadata.json"
        self.metadata.write_text(json.dumps({"packages": self.packages,
            "workspace_members": [package["id"] for package in self.packages]}))

    def package(self, name, directory, targets):
        return {"id": name, "name": name, "manifest_path": str(self.root / directory / "Cargo.toml"),
                "version": "0.1.0", "features": {"default": []}, "dependencies": [],
                "targets": [{"name": target_name, "kind": [kind],
                             "src_path": str(self.root / directory / source), "edition": "2021"}
                            for target_name, kind, source in targets]}

    def write(self, path, contents):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)

    def generate(self, *args):
        env = os.environ | {"PATH": f"{self.bin}:{os.environ['PATH']}",
                            "BUCK_TEST_METADATA": str(self.metadata)}
        return subprocess.run([sys.executable, str(self.root / "scripts/buck2-first-party.py"),
                               "--package", "tidepool-repr", "--package", "tidepool-atomic-write",
                               *args], cwd=self.root, env=env, capture_output=True, text=True)

    def groups(self, path):
        buck = (self.root / path / "BUCK").read_text()
        matches = re.findall(r'rust_filegroup\(\n    name = "([^"]+)",\n    mapped_srcs = (\{.*?\}),\n\)', buck, re.S)
        self.assertTrue(matches, buck)
        return buck, {name: ast.literal_eval(mapping) for name, mapping in matches}

    def test_source_tree_preserves_fixture_layout_and_target_inputs(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, groups = self.groups("tidepool/repr")
        self.assertIn('crate_root = "tidepool/repr/src/lib.rs"', buck)
        self.assertIn('crate_root = "tidepool/repr/tests/suites/repr.rs"', buck)
        self.assertIn('srcs_filegroup = ":repr_sources"', buck)
        lib = groups["tidepool_repr_sources"]
        unit = groups["tidepool_repr_unit_tests_sources"]
        integration = groups["repr_sources"]
        m3 = "bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor"
        schema = "bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor"
        fault = "bridge/atomic-write/tests/fixtures/directory_fault.c"
        self.assertEqual(lib["src/lib.rs"], "tidepool/repr/src/lib.rs")
        self.assertEqual(lib["//bridge/haskell:m3_vertical_fixture"], m3)
        self.assertEqual(unit["//bridge/haskell:m3_vertical_fixture"], m3)
        self.assertEqual(integration["//bridge/haskell:schema6_intrinsic_fixture"], schema)
        self.assertEqual(integration["//bridge/atomic-write:directory_fault_fixture"], fault)
        self.assertNotIn("src/lib.rs", integration)
        self.assertNotIn("tests/contract.rs", lib)
        for group in groups.values():
            for source, mapped in group.items():
                actual = self.root / (mapped if source.startswith("//") else "tidepool/repr/" + source)
                self.assertTrue(actual.is_file(), source)
                self.assertTrue((self.root / mapped).is_file(), mapped)
                if actual.suffix != ".rs":
                    continue
                for relative in re.findall(r'include_(?:str|bytes)!\s*\(\s*"([^"]+)"', actual.read_text()):
                    expected = (self.root / mapped).parent.joinpath(relative).resolve().relative_to(self.root)
                    self.assertIn(expected.as_posix(), group.values())

    def test_rejects_missing_and_outside_repository_inputs(self):
        source = self.root / "tidepool/repr/src/lib.rs"
        for literal, expected in [
            ("missing.cbor", "missing compile-time input"),
            (str(self.base / "outside.cbor"), "compile-time input outside repository"),
        ]:
            with self.subTest(literal=literal):
                (self.base / "outside.cbor").write_text("outside")
                source.write_text(f'const BAD: &[u8] = include_bytes!("{literal}");\n')
                result = self.generate()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected, result.stderr)

    def test_regeneration_is_deterministic_and_check_detects_drift(self):
        first = self.generate()
        self.assertEqual(first.returncode, 0, first.stderr)
        paths = [self.root / "tidepool/repr/BUCK", self.root / "bridge/atomic-write/BUCK"]
        original = [path.read_bytes() for path in paths]
        second = self.generate()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual([path.read_bytes() for path in paths], original)
        check = self.generate("--check")
        self.assertEqual(check.returncode, 0, check.stderr)
        paths[0].write_bytes(original[0] + b"\n# stale\n")
        check = self.generate("--check")
        self.assertNotEqual(check.returncode, 0)
        self.assertIn("stale or missing Buck target graph", check.stderr)


if __name__ == "__main__":
    unittest.main()
