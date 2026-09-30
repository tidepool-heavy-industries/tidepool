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
FEATURES = GENERATOR.with_name("buck2_cargo_features.py")


class FirstPartySources(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "repo"
        (self.root / "scripts").mkdir(parents=True)
        shutil.copyfile(GENERATOR, self.root / "scripts/buck2-first-party.py")
        shutil.copyfile(FEATURES, self.root / "scripts/buck2_cargo_features.py")
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
                   '#[path = "../execution_schema_contract.rs"] mod contract;\n')
        for module in ("execution_schema_codec", "extend_checked_equivalence",
                       "metadata_strictness", "strict_jsonl_directory"):
            self.write(f"tidepool/repr/tests/{module}.rs", "")
        self.write("tidepool/repr/tests/execution_schema_contract.rs",
                   'const SCHEMA: &[u8] = include_bytes!("../../../bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor");\n'
                   'const FAULT: &str = include_str!("../../../bridge/atomic-write/tests/fixtures/directory_fault.c");\n')
        self.write("bridge/atomic-write/src/lib.rs", "pub fn write() {}\n")
        self.write("tidepool/heap/Cargo.toml", "[package]\nname = 'tidepool-heap'\n")
        self.write("tidepool/heap/src/lib.rs", "pub fn heap() {}\n")
        self.write("tidepool/heap/tests/gc_unit.rs", "#[test] fn gc() {}\n")
        self.write("tidepool/heap/tests/raw_scan_validation.rs", "#[test] fn scan() {}\n")
        self.write("tidepool/codegen/Cargo.toml", "[package]\nname = 'tidepool-codegen'\n")
        self.write("tidepool/codegen/src/lib.rs", "pub mod prepared_program { pub fn session_var_id(_: &str, _: &str) -> u64 { 0 } }\n")
        self.write("tidepool/codegen/tests/native_md5_link.rs", "#[test] fn linked() {}\n")
        self.write("tidepool/codegen/tests/prepared_control.rs", "#[test] fn full_suite() {}\n")
        for source in ("apply_tests.rs", "bytes_tests.rs", "caller_result_tests.rs",
                      "double_to_int_tests.rs", "entry_tests.rs", "foreign_apply_tests.rs",
                      "freer_boundary_tests.rs", "lifetime_tests.rs", "no_success_tests.rs",
                      "retention_tests.rs", "settlement_tests.rs", "tests.rs"):
            self.write(f"tidepool/codegen/src/prepared_program/{source}", "")
        self.write("bridge/atomic-write/tests/strict_directory.rs", "#[test] fn write() {}\n")
        self.write("tidepool/extract-cmd/Cargo.toml", "[package]\nname = 'tidepool-extract-cmd'\n")
        self.write("tidepool/extract-cmd/src/lib.rs", "pub fn frontend() {}\n")
        self.write("tidepool/extract-cmd/src/main.rs", "fn main() {}\n")
        self.packages = [self.package("tidepool-repr", "tidepool/repr", [
            ("tidepool_repr", "lib", "src/lib.rs"), ("repr", "test", "tests/suites/repr.rs")]),
            self.package("tidepool-atomic-write", "bridge/atomic-write", [
                ("tidepool_atomic_write", "lib", "src/lib.rs"),
                ("strict_directory", "test", "tests/strict_directory.rs")]),
            self.package("tidepool-heap", "tidepool/heap", [
                ("tidepool_heap", "lib", "src/lib.rs"),
                ("gc_unit", "test", "tests/gc_unit.rs"),
                ("raw_scan_validation", "test", "tests/raw_scan_validation.rs")]),
            self.package("tidepool-codegen", "tidepool/codegen", [
                ("tidepool_codegen", "lib", "src/lib.rs"),
                ("native_md5_link", "test", "tests/native_md5_link.rs"),
                ("prepared_control", "test", "tests/prepared_control.rs")]),
            self.package("tidepool-extract-cmd", "tidepool/extract-cmd", [
                ("tidepool_extract_cmd", "lib", "src/lib.rs"),
                ("tidepool-extract", "bin", "src/main.rs")])]
        self.metadata = self.base / "metadata.json"
        self.metadata.write_text(json.dumps({"packages": self.packages,
            "workspace_members": [package["id"] for package in self.packages],
            "resolve": {"nodes": [{"id": package["id"], "deps": []}
                                  for package in self.packages]}}))

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
                               "--package", "tidepool-heap",
                               "--package", "tidepool-codegen",
                               "--package", "tidepool-extract-cmd",
                               *args], cwd=self.root, env=env, capture_output=True, text=True)

    def groups(self, path):
        buck = (self.root / path / "BUCK").read_text()
        matches = re.findall(r'rust_filegroup\(\n    name = "([^"]+)",\n    mapped_srcs = (\{.*?\}),\n\)', buck, re.S)
        self.assertTrue(matches, buck)
        return buck, {name: ast.literal_eval(mapping) for name, mapping in matches}

    def test_codegen_emits_native_units_and_only_the_md5_integration_test(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, groups = self.groups("tidepool/codegen")
        self.assertIn('name = "native_md5_link"', buck)
        self.assertIn('crate_root = "tidepool/codegen/tests/native_md5_link.rs"', buck)
        self.assertIn('deps = [\n        ":tidepool_codegen",', buck)
        self.assertIn("tests/native_md5_link.rs", groups["native_md5_link_sources"])
        self.assertNotIn("prepared_control", buck)
        self.assertIn("tidepool_codegen_unit_tests", buck)
        self.assertIn("tidepool_codegen_unit_tests_sources", groups)

    def test_extract_frontend_binary_is_generated_from_its_cargo_target(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, groups = self.groups("tidepool/extract-cmd")
        self.assertIn('name = "tidepool-extract"', buck)
        self.assertIn('crate_root = "tidepool/extract-cmd/src/main.rs"', buck)
        self.assertEqual(
            groups["tidepool-extract_sources"]["src/main.rs"],
            "tidepool/extract-cmd/src/main.rs",
        )

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
        self.assertNotIn("tests/execution_schema_contract.rs", lib)
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

    def test_integration_roots_have_independent_source_closures(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        _, heap_groups = self.groups("tidepool/heap")
        gc = heap_groups["gc_unit_sources"]
        raw = heap_groups["raw_scan_validation_sources"]
        self.assertEqual(set(gc), {"Cargo.toml", "tests/gc_unit.rs"})
        self.assertEqual(set(raw), {"Cargo.toml", "tests/raw_scan_validation.rs"})
        _, atomic_groups = self.groups("bridge/atomic-write")
        self.assertEqual(set(atomic_groups["strict_directory_sources"]),
                         {"Cargo.toml", "tests/strict_directory.rs"})
        _, repr_groups = self.groups("tidepool/repr")
        self.assertEqual(set(repr_groups["repr_sources"]), {
            "Cargo.toml", "tests/suites/repr.rs", "tests/execution_schema_codec.rs",
            "tests/execution_schema_contract.rs", "tests/extend_checked_equivalence.rs",
            "tests/metadata_strictness.rs", "tests/strict_jsonl_directory.rs",
            "//bridge/haskell:schema6_intrinsic_fixture",
            "//bridge/atomic-write:directory_fault_fixture",
        })
        # An unlisted future #[path] module is absent from the Buck source
        # tree, so Rust compilation must fail visibly rather than silently
        # widening the target to every tests/*.rs file.
        self.write("tidepool/heap/tests/future.rs", "#[test] fn future() {}\n")
        self.write("tidepool/heap/tests/gc_unit.rs", '#[path = "future.rs"] mod future;\n')
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        _, heap_groups = self.groups("tidepool/heap")
        self.assertNotIn("tests/future.rs", heap_groups["gc_unit_sources"])
        self.assertNotIn("tests/future.rs", heap_groups["raw_scan_validation_sources"])
        (self.root / "tidepool/repr/tests/metadata_strictness.rs").unlink()
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing integration source", result.stderr)

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

    def test_unsupported_target_selector_fails_even_for_shared_resolved_package(self):
        metadata = json.loads(self.metadata.read_text())
        package = metadata["packages"][0]
        dependency = {"name": "libc", "rename": None, "kind": None,
                      "target": "cfg(windows)", "req": "0.2"}
        package["dependencies"].append(dependency)
        metadata["packages"].append({"id": "libc-id", "name": "libc"})
        metadata["resolve"]["nodes"][0]["deps"].append({
            "name": "libc", "pkg": "libc-id", "dep_kinds": [
                {"kind": None, "target": "cfg(windows)"},
                {"kind": None, "target": "cfg(unix)"},
            ]})
        self.metadata.write_text(json.dumps(metadata))
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported Linux dependency selector", result.stderr)

    def test_feature_selection_keeps_optional_vendor_protocol_out_of_embedded_graph(self):
        metadata = json.loads(self.metadata.read_text())
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool-repr")
        package["features"] = {
            "default": ["codex-compat"],
            "codex-compat": ["dep:codex-shoal-protocol"],
            "embedded": [],
        }
        package["dependencies"].append({
            "name": "codex-shoal-protocol", "rename": None, "kind": None,
            "target": None, "optional": True, "uses_default_features": True,
            "features": [], "req": "=0.1.0",
        })
        vendor_id = "registry+https://github.com/rust-lang/crates.io-index#codex-shoal-protocol@0.1.0"
        metadata["packages"].append({
            "id": vendor_id, "name": "codex-shoal-protocol", "version": "0.1.0",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
        })
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == package["id"])
        node["deps"].append({
            "name": "codex_shoal_protocol", "pkg": vendor_id,
            "dep_kinds": [{"kind": None, "target": None}],
        })
        self.metadata.write_text(json.dumps(metadata))

        embedded = self.generate(
            "--no-default-features", "tidepool-repr",
            "--features", "tidepool-repr=embedded",
        )
        self.assertEqual(embedded.returncode, 0, embedded.stderr)
        buck = (self.root / "tidepool/repr/BUCK").read_text()
        self.assertNotIn("codex-shoal-protocol", buck)
        self.assertIn('features = [\n        "embedded",\n    ]', buck)

        default = self.generate()
        self.assertEqual(default.returncode, 0, default.stderr)
        buck = (self.root / "tidepool/repr/BUCK").read_text()
        self.assertIn("//third-party/rust:codex-shoal-protocol", buck)
        self.assertIn('"codex-compat"', buck)


if __name__ == "__main__":
    unittest.main()
