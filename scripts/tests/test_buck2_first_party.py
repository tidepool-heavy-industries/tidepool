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
PROFILE = GENERATOR.with_name("native-profile.toml")


class FirstPartySources(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "repo"
        (self.root / "scripts").mkdir(parents=True)
        shutil.copyfile(GENERATOR, self.root / "scripts/buck2-first-party.py")
        shutil.copyfile(FEATURES, self.root / "scripts/buck2_cargo_features.py")
        shutil.copyfile(PROFILE, self.root / "scripts/native-profile.toml")
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
        self.write("tidepool/bridge-derive/Cargo.toml", "[package]\nname = 'tidepool-bridge-derive'\n")
        self.write("tidepool/bridge-derive/src/lib.rs", "extern crate proc_macro;\n")
        self.write("tidepool/runtime/Cargo.toml", "[package]\nname = 'tidepool-runtime'\n")
        self.write("tidepool/runtime/src/lib.rs",
                   'const SESSION: &str = include_str!("../../../bridge/haskell/src/Tidepool/Session.hs");\n')
        self.write("bridge/haskell/src/Tidepool/Session.hs", "module Tidepool.Session where\n")
        self.write("exomonad/agent/Cargo.toml", "[package]\nname = 'exomonad-agent'\n")
        self.write("exomonad/agent/src/lib.rs", "mod backend;\n")
        self.write("exomonad/agent/src/backend/mod.rs", "#[cfg(feature = \"codex-compat\")] mod codex;\n")
        self.write("exomonad/agent/src/backend/codex/mod.rs", "pub fn backend() {}\n")
        self.write("third-party/rust/Cargo.toml", """
[dependencies]
codex-shoal-protocol = { package = "codex-shoal-protocol", version = "=0.1.0" }
sha2-0_10_9 = { package = "sha2", version = "=0.10.9" }
sha2-0_11_0 = { package = "sha2", version = "=0.11.0" }
""")
        fixtures = (
            "join-v2.cbor", "join-v2.json", "inventory-v2.cbor", "inventory-v2.json",
            "join-v3.cbor", "join-v3.json", "join-typed-v3.cbor", "join-typed-v3.json",
            "inventory-v3.cbor", "inventory-v3.json",
        )
        includes = "\n".join(
            f'const FIXTURE_{index}: &[u8] = include_bytes!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/{name}");'
            for index, name in enumerate(fixtures)
        )
        self.write("tidepool/toolchain/Cargo.toml", "[package]\nname = 'tidepool-toolchain'\n")
        self.write("tidepool/toolchain/src/lib.rs", includes + "\n")
        for name in fixtures:
            self.write(f"bridge/haskell/test-cell-splitter/fixtures/declaration-join/{name}", name)
        self.write("bridge/facade/Cargo.toml", "[package]\nname = 'tidepool'\n")
        self.write("bridge/testing/Cargo.toml", "[package]\nname = 'tidepool-testing'\n")
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
                ("tidepool-extract", "bin", "src/main.rs")]),
            self.package("tidepool-bridge-derive", "tidepool/bridge-derive", [
                ("tidepool_bridge_derive", "proc-macro", "src/lib.rs")]),
            self.package("tidepool-runtime", "tidepool/runtime", [
                ("tidepool_runtime", "lib", "src/lib.rs")]),
            self.package("tidepool-toolchain", "tidepool/toolchain", [
                ("tidepool_toolchain", "lib", "src/lib.rs")]),
            self.package("tidepool", "bridge/facade", [
                ("tidepool_build_script", "custom-build", "build.rs"),
                ("tidepool", "lib", "src/lib.rs"),
                ("tidepool", "bin", "src/main.rs"),
                ("exomonad", "bin", "src/bin/exomonad.rs"),
                ("exomonad-view-helper", "bin", "src/view_helper.rs"),
                ("tidepool-compile-report", "bin", "src/compile_report_main.rs"),
            ]),
            self.package("tidepool-testing", "bridge/testing", [
                ("tidepool_testing", "lib", "src/lib.rs"),
            ]),
            self.package("exomonad-agent", "exomonad/agent", [
                ("exomonad_agent", "lib", "src/lib.rs"),
            ])]
        for package in self.packages:
            if package["name"] in {"tidepool", "exomonad-agent"}:
                package["features"] = {"default": ["codex-compat"], "codex-compat": []}
        self.write("bridge/facade/build.rs", "fn main() {}\n")
        for source in ("src/lib.rs", "src/main.rs", "src/bin/exomonad.rs", "src/view_helper.rs",
                       "src/compile_report_main.rs"):
            self.write(f"bridge/facade/{source}", "fn main() {}\n")
        self.metadata = self.base / "metadata.json"
        self.metadata.write_text(json.dumps({"packages": self.packages,
            "workspace_members": [package["id"] for package in self.packages],
            "resolve": {"nodes": [{"id": package["id"], "deps": []}
                                  for package in self.packages]}}))

    def package(self, name, directory, targets):
        return {"id": name, "name": name, "manifest_path": str(self.root / directory / "Cargo.toml"),
                "version": "0.1.0", "source": None, "features": {"default": []}, "dependencies": [],
                "targets": [{"name": target_name, "kind": [kind],
                             "src_path": str(self.root / directory / source), "edition": "2021"}
                            for target_name, kind, source in targets]}

    def write(self, path, contents):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)

    def generate(self, *args):
        metadata = json.loads(self.metadata.read_text())
        node_ids = {node["id"] for node in metadata["resolve"]["nodes"]}
        metadata["resolve"]["nodes"].extend(
            {"id": package["id"], "deps": []}
            for package in metadata["packages"] if package["id"] not in node_ids
        )
        self.metadata.write_text(json.dumps(metadata))
        env = os.environ | {"PATH": f"{self.bin}:{os.environ['PATH']}",
                            "BUCK_TEST_METADATA": str(self.metadata)}
        return subprocess.run([sys.executable, str(self.root / "scripts/buck2-first-party.py"),
                               "--package", "tidepool-repr", "--package", "tidepool-atomic-write",
                               "--package", "tidepool-heap",
                               "--package", "tidepool-codegen",
                               "--package", "tidepool-extract-cmd",
                               "--package", "tidepool-bridge-derive",
                               "--package", "tidepool-runtime",
                               "--package", "tidepool-toolchain",
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

    def test_plain_profile_disables_both_roots_and_omits_codex_source_tree(self):
        result = self.generate("--package", "tidepool", "--package", "exomonad-agent")
        self.assertEqual(result.returncode, 0, result.stderr)
        facade = (self.root / "bridge/facade/BUCK").read_text()
        self.assertNotIn('"codex-compat"', facade)
        facade, facade_groups = self.groups("bridge/facade")
        self.assertIn("src/lib.rs", facade_groups["tidepool_sources"])
        self.assertNotIn("src/main.rs", facade_groups["tidepool_sources"])
        self.assertIn("src/main.rs", facade_groups["tidepool_bin_sources"])
        agent, groups = self.groups("exomonad/agent")
        self.assertNotIn('"codex-compat"', agent)
        self.assertIn("src/backend/mod.rs", groups["exomonad_agent_sources"])
        self.assertNotIn("src/backend/codex/mod.rs", groups["exomonad_agent_sources"])
        check = self.generate("--package", "tidepool", "--package", "exomonad-agent", "--check")
        self.assertEqual(check.returncode, 0, check.stderr)

    def test_binary_sources_preserve_module_layouts_and_their_includes(self):
        metadata = json.loads(self.metadata.read_text())
        facade = next(package for package in metadata["packages"] if package["name"] == "tidepool")
        binary = next(target for target in facade["targets"] if target["name"] == "exomonad")
        binary["src_path"] = str(self.root / "bridge/facade/src/bin/exomonad.rs")
        self.metadata.write_text(json.dumps(metadata))
        self.write("bridge/facade/src/main.rs", "pub mod helper;\nmod tree;\nfn main() {}\n")
        self.write("bridge/facade/src/helper.rs", "pub fn answer() -> i32 { 42 }\n")
        self.write("bridge/facade/src/tree/mod.rs", "mod child;\n")
        self.write("bridge/facade/src/tree/child.rs", 'const VALUE: &str = include_str!("value.txt");\n')
        self.write("bridge/facade/src/tree/value.txt", "nested module input\n")
        self.write("bridge/facade/src/bin/exomonad.rs", "mod sibling;\nfn main() {}\n")
        self.write("bridge/facade/src/bin/sibling.rs", 'const VALUE: &str = include_str!("sibling.txt");\n')
        self.write("bridge/facade/src/bin/sibling.txt", "binary module input\n")

        result = self.generate("--package", "tidepool")

        self.assertEqual(result.returncode, 0, result.stderr)
        _, groups = self.groups("bridge/facade")
        for group, sources in {
            "tidepool_bin_sources": (
                "src/helper.rs", "src/tree/mod.rs", "src/tree/child.rs", "src/tree/value.txt",
            ),
            "exomonad_sources": (
                "src/bin/exomonad.rs", "src/bin/sibling.rs", "src/bin/sibling.txt",
            ),
        }.items():
            for source in sources:
                self.assertIn(source, groups[group])

    def test_later_validation_failure_preserves_every_existing_graph(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        previous = {path: path.read_bytes() for path in self.root.rglob("BUCK")}
        self.assertNotIn(b"new_source.rs", previous[self.root / "tidepool/repr/BUCK"])
        self.write("tidepool/repr/src/new_source.rs", "pub fn added() {}\n")
        # Repr renders before atomic-write in this metadata. Its changed graph
        # must not publish if the later package has an invalid input closure.
        self.write("bridge/atomic-write/src/lib.rs", 'const MISSING: &str = include_str!("missing.txt");\n')

        result = self.generate()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing compile-time input", result.stderr)
        for path, contents in previous.items():
            self.assertEqual(path.read_bytes(), contents, path)
        self.assertEqual(set(self.root.rglob("BUCK")), set(previous))
        self.assertFalse(list(self.root.rglob(".BUCK.*.tmp")))

    def test_facade_buildscript_declares_embedded_sources_and_out_dir(self):
        metadata = json.loads(self.metadata.read_text())
        facade = next(p for p in metadata["packages"] if p["name"] == "tidepool")
        toolchain = next(p for p in metadata["packages"] if p["name"] == "tidepool-toolchain")
        facade["dependencies"].append({
            "name": "tidepool-toolchain", "rename": None, "kind": "build",
            "target": None, "optional": False, "uses_default_features": True,
            "features": [], "req": "*",
        })
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == facade["id"])
        node["deps"].append({
            "name": "tidepool_toolchain", "pkg": toolchain["id"],
            "dep_kinds": [{"kind": "build", "target": None}],
        })
        self.metadata.write_text(json.dumps(metadata))
        result = self.generate("--package", "tidepool", "--no-default-features", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, groups = self.groups("bridge/facade")
        self.assertIn('name = "facade_cargo_manifest"', buck)
        self.assertIn('src = "Cargo.toml"', buck)
        self.assertIn('name = "tidepool_build_script"', buck)
        build_rule = buck.split('name = "tidepool_build_script",', 1)[1].split('\n)\n', 1)[0]
        self.assertIn("//tidepool/toolchain:tidepool_toolchain", build_rule)
        library_rule = buck.split('tidepool_rust_library(\n    name = "tidepool",', 1)[1].split('\n)\n', 1)[0]
        self.assertNotIn("//tidepool/toolchain:tidepool_toolchain", library_rule)
        self.assertIn('crate_root = "bridge/facade/build.rs"', build_rule)
        self.assertEqual(groups["tidepool_build_script_sources"]["build.rs"], "bridge/facade/build.rs")
        self.assertIn('name = "tidepool_build_script_run"', buck)
        self.assertIn('"TIDEPOOL_EMBED_HASKELL": "1"', buck)
        self.assertIn('"TIDEPOOL_BUILD_SOURCE_ROOT": "."', buck)
        self.assertIn('haskell_sources = "//bridge/haskell:facade_embedded_sources"', buck)
        self.assertIn('workspace_sources = "//exomonad/examples/workspace:facade_scaffold_sources"', buck)
        self.assertEqual(buck.count('"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"'), 6)
        self.assertNotIn("codex-shoal-protocol", buck)
        self.assertIn("src/lib.rs", groups["tidepool_sources"])

    def test_facade_unit_root_uses_embedded_profile_support_and_browser_resources(self):
        metadata = json.loads(self.metadata.read_text())
        facade = next(package for package in metadata["packages"] if package["name"] == "tidepool")
        support = next(package for package in metadata["packages"] if package["name"] == "tidepool-testing")
        facade["dependencies"].append({
            "name": "tidepool-testing", "rename": None, "kind": "dev",
            "target": None, "optional": False, "uses_default_features": True,
            "features": [], "req": "*",
        })
        next(node for node in metadata["resolve"]["nodes"] if node["id"] == facade["id"])["deps"].append({
            "name": "tidepool_testing", "pkg": support["id"],
            "dep_kinds": [{"kind": "dev", "target": None}],
        })
        self.metadata.write_text(json.dumps(metadata))
        self.write("bridge/facade/src/actor_host.rs", "#[cfg(test)] mod m1_host_tests;\n")
        self.write("bridge/facade/src/actor_host/m1_host_tests.rs", "#[path = \"m1_browser_runner.rs\"] mod browser_runner; #[test] fn browser_gate() {}\n")
        self.write("bridge/facade/src/actor_host/m1_browser_runner.rs", "pub fn run() {}\n")
        self.write("bridge/facade/src/actor_host/test_campaign.rs", "pub struct TestCampaign;\n")
        self.write("bridge/facade/src/host_dynamic_tools.rs", "#[cfg(test)] mod tui_resource_tests;\n")
        self.write("bridge/facade/src/host_dynamic_tools/tui_resource_tests.rs", 'const OMITTED: &str = include_str!("../../../../.exomonad/workspace/skills/exomonad-command/SKILL.md");\n')
        self.write("bridge/facade/src/actor_host/documentation_tests.rs", 'const OMITTED: &str = include_str!("../../../../.exomonad/workspace/checks/not-in-profile.hs");\n')
        self.write("bridge/facade/src/actor_host/agent_spec_tests.rs", 'const OMITTED: &str = include_str!("../../../../exomonad/examples/workspace/.exomonad/AgentSpec.hs");\n')
        self.write("bridge/facade/src/exomonad.rs", '''
const AGENT_SPEC: &str = include_str!("../../../exomonad/examples/workspace/.exomonad/AgentSpec.hs");
const JEV_OPERATORS: &str = include_str!("../../../.exomonad/workspace/Jev/Operators.hs");
const REVIEW_PROMPT: &str = include_str!("../../../exomonad/examples/workspace/.exomonad/prompts/review.md");
''')
        self.write("bridge/testing/src/lib.rs", "pub struct TestSupport;\n")
        self.write(".exomonad/workspace/Jev/Operators.hs", "module Jev.Operators where\n")
        self.write("exomonad/examples/workspace/.exomonad/AgentSpec.hs", "module AgentSpec where\n")
        self.write("exomonad/examples/workspace/.exomonad/prompts/review.md", "review prompt\n")

        result = self.generate("--package", "tidepool", "--no-default-features", "tidepool")

        self.assertEqual(result.returncode, 0, result.stderr)
        facade_buck, groups = self.groups("bridge/facade")
        self.assertIn('name = "tidepool_unit_tests_sources"', facade_buck)
        unit_rule = facade_buck.split('tidepool_rust_binary(\n    name = "tidepool_unit_tests",', 1)[1].split("\n)\n", 1)[0]
        for source in (
            "src/actor_host/m1_host_tests.rs", "src/actor_host/m1_browser_runner.rs",
            "src/actor_host/test_campaign.rs", "src/exomonad.rs",
        ):
            self.assertIn(source, groups["tidepool_unit_tests_sources"])
        for source in ("src/actor_host/documentation_tests.rs", "src/actor_host/agent_spec_tests.rs"):
            self.assertNotIn(source, groups["tidepool_unit_tests_sources"])
        self.assertIn("//bridge/testing:tidepool_testing", unit_rule)
        self.assertIn('rustc_flags = ["--test"]', unit_rule)
        for runtime_only in ("TIDEPOOL_EXTRACT", "TIDEPOOL_BROWSER", "//web:dist", "resources ="):
            self.assertNotIn(runtime_only, unit_rule)
        cases = {}
        for name in ("facade_process_tests", "facade_host_tests", "facade_host_raw_test",
                     "facade_late_output_test", "facade_browser_test", "tidepool_unit_tests_all"):
            cases[name] = facade_buck.split(
                'tidepool_rust_test_cases(\n    name = "' + name + '",', 1
            )[1].split("\n)\n", 1)[0]
            self.assertIn('binary = ":tidepool_unit_tests"', cases[name])
            self.assertIn("jobs = 1", cases[name])
        process = cases["facade_process_tests"]
        self.assertIn("expected_count = 6", process)
        self.assertIn('"TIDEPOOL_TEST_BASH": "$(exe toolchains//:bash)"', process)
        self.assertIn('"TIDEPOOL_TEST_SLEEP": "$(exe toolchains//:sleep)"', process)
        for heavyweight in ("TIDEPOOL_EXTRACT", "TIDEPOOL_BROWSER", "//web:dist", "playwright"):
            self.assertNotIn(heavyweight, process)
        host = cases["facade_host_tests"]
        self.assertIn("expected_count = 3", host)
        self.assertIn("haskell_worker = True", host)
        self.assertIn('"TIDEPOOL_EXTRACT_WORKER"', host)
        self.assertIn('"TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib"', host)
        self.assertNotIn("TIDEPOOL_BROWSER_DRIVER", host)
        self.assertNotIn("playwright", host)
        raw_host = cases["facade_host_raw_test"]
        self.assertIn(
            '"actor_host::m1_host_tests::production_host_retains_http_haskell_commands_and_reconnects_without_replay"',
            raw_host,
        )
        self.assertIn("expected_count = 1", raw_host)
        self.assertIn('"TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)"', raw_host)
        self.assertIn('"EXOMONAD_EMBEDDED_ASSET_ROOT": "$(location //web:dist)/web"', raw_host)
        self.assertIn('"//bridge/haskell:facade_embedded_sources"', raw_host)
        self.assertIn('"//web:dist"', raw_host)
        self.assertIn('"toolchains//:test_tools_closure"', raw_host)
        self.assertNotIn("TIDEPOOL_BROWSER_DRIVER", raw_host)
        self.assertIn("expected_count = 1", cases["facade_late_output_test"])
        browser = cases["facade_browser_test"]
        self.assertIn("expected_count = 1", browser)
        self.assertIn("ignored = True", browser)
        self.assertIn('"EXOMONAD_EMBEDDED_ASSET_ROOT": "$(location //web:dist)/web"', browser)
        self.assertIn('"TIDEPOOL_BROWSER_DRIVER": "$(location //build/testing/browser:driver_bundle)/driver.mjs"', browser)
        self.assertIn('"TIDEPOOL_BROWSER_NODE": "$(exe toolchains//:browser_node)"', browser)
        self.assertIn('"PLAYWRIGHT_BROWSERS_PATH": "$(location toolchains//:playwright_browsers)"', browser)
        self.assertIn('"toolchains//:browser_test_closure"', browser)
        self.assertNotIn("codex-shoal-protocol", facade_buck)
        support_buck, support_groups = self.groups("bridge/testing")
        self.assertIn('name = "tidepool_testing"', support_buck)
        self.assertIn("src/lib.rs", support_groups["tidepool_testing_sources"])
        self.assertIn(
            "//:facade_test_jev_operators",
            groups["tidepool_unit_tests_sources"],
        )
        self.assertIn(
            "//exomonad/examples/workspace:facade_agent_spec",
            groups["tidepool_unit_tests_sources"],
        )
        self.assertIn(
            "//exomonad/examples/workspace:facade_review_prompt",
            groups["tidepool_unit_tests_sources"],
        )

    def test_transitive_reindeer_target_uses_unique_locked_source_identity(self):
        metadata = json.loads(self.metadata.read_text())
        runtime = next(p for p in metadata["packages"] if p["name"] == "tidepool-runtime")
        resolved_id = (
            "registry+https://github.com/rust-lang/crates.io-index"
            "#tokio-tungstenite@0.29.0"
        )
        runtime["dependencies"].append({
            "name": "tokio-tungstenite", "rename": None, "kind": None,
            "target": None, "optional": False, "uses_default_features": True,
            "features": [], "req": "=0.29.0",
        })
        metadata["packages"].append({
            "id": resolved_id, "name": "tokio-tungstenite", "version": "0.29.0",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
        })
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == runtime["id"])
        node["deps"].append({
            "name": "tokio_tungstenite", "pkg": resolved_id,
            "dep_kinds": [{"kind": None, "target": None}],
        })
        self.metadata.write_text(json.dumps(metadata))
        self.write("Cargo.lock", """version = 4

[[package]]
name = "tokio-tungstenite"
version = "0.29.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
""")
        self.write("third-party/rust/BUCK", 'cargo.rust_library(\n    name = "tokio-tungstenite-0.29",\n)\n')

        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        runtime_buck, _ = self.groups("tidepool/runtime")
        self.assertIn("//third-party/rust:tokio-tungstenite-0.29", runtime_buck)

        lock = self.root / "Cargo.lock"
        lock.write_text(lock.read_text() + """
[[package]]
name = "tokio-tungstenite"
version = "0.29.0"
source = "git+https://example.invalid/tokio-tungstenite?rev=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa#aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
""")
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("source-ambiguous Reindeer target", result.stderr)

    def test_runtime_tests_separate_native_admission_from_compiler_resources(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        runtime, groups = self.groups("tidepool/runtime")
        self.assertIn('name = "tidepool_runtime"', runtime)
        self.assertNotIn("bridge/haskell/src/Tidepool/Session.hs", groups["tidepool_runtime_sources"].values())
        self.assertEqual(
            groups["tidepool_runtime_unit_tests_sources"]["//bridge/haskell:session_source"],
            "bridge/haskell/src/Tidepool/Session.hs",
        )
        self.assertIn('tidepool_rust_binary(\n    name = "tidepool_runtime_unit_tests",', runtime)
        self.assertIn('binary = ":tidepool_runtime_unit_tests"', runtime)
        self.assertIn("expected_count = 3", runtime)
        admission = runtime.split('name = "runtime_admission_tests",', 1)[1].split("\n)\n", 1)[0]
        for heavyweight in ("haskell_worker", "TIDEPOOL_EXTRACT", "trybuild", "//web:"):
            self.assertNotIn(heavyweight, admission)
        for name in ("runtime_checked_cache_test", "runtime_checked_original_test"):
            checked = runtime.split(f'name = "{name}",', 1)[1].split("\n)\n", 1)[0]
            self.assertIn("expected_count = 1", checked)
            self.assertIn("haskell_worker = True", checked)
            self.assertIn('"TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)"', checked)
            self.assertIn('"TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib"', checked)
        fixture = runtime.split('name = "runtime_compiled_cell_fixture_test",', 1)[1].split("\n)\n", 1)[0]
        self.assertIn("expected_count = 8", fixture)
        self.assertIn('"TIDEPOOL_CELL_TEST_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)"', fixture)
        self.assertIn('"TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)"', fixture)
        self.assertIn('"//build/package:compiler_deployment"', fixture)
        derive, _ = self.groups("tidepool/bridge-derive")
        self.assertIn("proc_macro = True", derive)

    def test_toolchain_unit_target_declares_legacy_and_v3_join_fixtures(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        _, groups = self.groups("tidepool/toolchain")
        unit = groups["tidepool_toolchain_unit_tests_sources"]
        self.assertEqual(
            {source: mapped for source, mapped in unit.items() if source.startswith("//bridge/haskell:declaration_")},
            {
                "//bridge/haskell:declaration_join_v2_cbor_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.cbor",
                "//bridge/haskell:declaration_join_v2_json_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.json",
                "//bridge/haskell:declaration_inventory_v2_cbor_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.cbor",
                "//bridge/haskell:declaration_inventory_v2_json_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.json",
                "//bridge/haskell:declaration_join_v3_cbor_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.cbor",
                "//bridge/haskell:declaration_join_v3_json_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.json",
                "//bridge/haskell:declaration_join_typed_v3_cbor_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.cbor",
                "//bridge/haskell:declaration_join_typed_v3_json_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.json",
                "//bridge/haskell:declaration_inventory_v3_cbor_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.cbor",
                "//bridge/haskell:declaration_inventory_v3_json_fixture": "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.json",
            },
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
        codex_edge = {
            "name": "codex_shoal_protocol", "pkg": vendor_id,
            "dep_kinds": [{"kind": None, "target": None}],
        }
        node["deps"].append(codex_edge)
        self.metadata.write_text(json.dumps(metadata))

        node["deps"].remove(codex_edge)
        self.metadata.write_text(json.dumps(metadata))
        embedded = self.generate(
            "--no-default-features", "tidepool-repr",
            "--features", "tidepool-repr=embedded",
        )
        self.assertEqual(embedded.returncode, 0, embedded.stderr)
        buck = (self.root / "tidepool/repr/BUCK").read_text()
        self.assertNotIn("codex-shoal-protocol", buck)
        self.assertIn('features = [\n        "embedded",\n    ]', buck)

        node["deps"].append(codex_edge)
        self.metadata.write_text(json.dumps(metadata))
        previous = (self.root / "tidepool/repr/BUCK").read_bytes()
        activated = self.generate("--features", "tidepool-repr=default")
        self.assertNotEqual(activated.returncode, 0)
        self.assertIn("forbidden Codex package codex-shoal-protocol", activated.stderr)
        self.assertEqual((self.root / "tidepool/repr/BUCK").read_bytes(), previous)

    def test_registry_dependency_labels_follow_resolved_package_versions(self):
        metadata = json.loads(self.metadata.read_text())
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool-repr")
        versions = [("0.10.9", "sha2-old"), ("0.11.0", "sha2-new")]
        edges = []
        for index, (version, package_id) in enumerate(versions):
            metadata["packages"].append({
                "id": package_id, "name": "sha2", "version": version,
                "source": "registry+https://github.com/rust-lang/crates.io-index",
            })
            alias = None if index == 0 else "sha2_new"
            package["dependencies"].append({
                "name": "sha2", "rename": alias, "kind": None, "target": None,
                "optional": False, "uses_default_features": True, "features": [],
            })
            edges.append({
                "name": (alias or "sha2"), "pkg": package_id,
                "dep_kinds": [{"kind": None, "target": None}],
            })
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == package["id"])
        node["deps"].extend(edges)
        self.metadata.write_text(json.dumps(metadata))

        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, _ = self.groups("tidepool/repr")
        self.assertIn("//third-party/rust:sha2-0_10_9", buck)
        self.assertIn("//third-party/rust:sha2-0_11_0", buck)
        self.assertNotIn('"//third-party/rust:sha2"', buck)

    def test_workspace_default_can_be_suppressed_without_emitting_that_package(self):
        result = self.generate("--no-default-features", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "bridge/facade/BUCK").exists())


if __name__ == "__main__":
    unittest.main()
