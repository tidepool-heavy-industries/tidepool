#!/usr/bin/env python3
"""Source-layout contracts for the native Buck Rust generator, without a build."""

import ast
import importlib.util
import json
import os
import pathlib
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
TEST_OWNERSHIP = GENERATOR.with_name("test_source_ownership.py")
TOOLCHAIN_COMPILER_FREE_TESTS = [
    "certified_products::tests::original_native_index_preserves_sparse_order_and_selected_identity",
    "certified_products::tests::original_native_index_issuer_rejects_duplicate_ordinals_and_wrong_owners",
    "certified_products::tests::original_native_index_matches_linear_scan_for_generated_sparse_groups",
    "certified_products::tests::original_native_witness_reuses_nonempty_cycle_and_checks_selected_closure",
    "certified_products::tests::original_native_witness_checks_package_drift_and_zero_group_downgrade",
    "certified_products::tests::repeated_retained_core_promotion_preserves_exact_original_membership",
    "certified_products::tests::nonempty_exact_source_recipe_preserves_original_versions_and_selected_subsets",
    "artifact_inventory::tests::native_requirement_index_refuses_interface_edges",
    "artifact_inventory::tests::native_requirement_index_preserves_empty_sparse_and_overlapping_edges",
    "artifact_inventory::tests::native_requirement_planning_visits_only_selected_group_edges",
    "artifact_inventory::native_history_properties::native_requirement_index_matches_flat_edge_oracle",
    "artifact_inventory::native_history_properties::staged_native_histories_match_raw_fact_model",
    "artifact_inventory::native_history_properties::recovery_selection_cannot_borrow_unrecorded_global_dependency",
    "artifact_inventory::tests::native_requirement_cannot_select_an_ambient_exact_native_key",
    "artifact_inventory::tests::recovery_restores_interface_edges_and_keeps_certified_native_facts",
    "certified_products::tests::artifact_view_group_index_properties::artifact_view_group_indexes_preserve_order_and_refusal_precedence",
    "certified_products::tests::artifact_view_group_index_properties::artifact_view_group_indexes_match_linear_wrapper_for_generated_selections",
    "artifact_inventory::view_read_properties::cached_read_projection_matches_exhaustive_graph_after_view_histories",
    "artifact_inventory::view_read_properties::warmed_read_getters_do_not_visit_graph_nodes",
    "artifact_inventory::view_read_properties::first_read_and_inventory_extension_settle_without_lock_recursion",
    "artifact_inventory::tests::retained_views_reclaim_after_last_reader_without_copying_payloads",
    "artifact_inventory::tests::reclamation_metrics_follow_last_reader_and_selected_closure",
    "artifact_inventory::tests::empty_admission_and_parent_only_release_do_not_scan_history",
    "artifact_inventory::tests::reused_graph_targets_must_match_the_selected_sealed_dependencies",
    "artifact_inventory::tests::independent_views_retain_existing_nodes_without_sibling_visibility",
    "artifact_inventory::tests::selected_root_keeps_hidden_requirements_and_reclaims_unselected_history",
    "artifact_inventory::tests::reclamation_preserves_incoming_cycles_and_does_not_visit_unrelated_history",
    "artifact_inventory::view_read_properties::zero_mask_empty_selection_boundary_releases_valid_fixture_views",
    "artifact_inventory::tests::metadata_snapshot_preserves_canonical_typed_edges_in_one_closure",
    "artifact_inventory::view_read_properties::linear_view_history_reports_retained_read_projection_cost",
]


class FirstPartySources(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "repo"
        (self.root / "scripts").mkdir(parents=True)
        subprocess.run(["git", "init", "--quiet", str(self.root)], check=True)
        workspace = self.root / ".exomonad/workspace"
        workspace.mkdir(parents=True)
        subprocess.run(["git", "init", "--quiet", str(workspace)], check=True)
        subprocess.run(["git", "-C", str(workspace), "-c", "user.name=fixture",
                        "-c", "user.email=fixture@invalid", "commit", "--quiet",
                        "--allow-empty", "-m", "workspace fixture"], check=True)
        self.workspace_revision = subprocess.check_output(
            ["git", "-C", str(workspace), "rev-parse", "HEAD"], text=True).strip()
        subprocess.run(["git", "-C", str(self.root), "update-index", "--add",
                        "--cacheinfo", "160000", self.workspace_revision,
                        ".exomonad/workspace"], check=True)
        shutil.copyfile(GENERATOR, self.root / "scripts/buck2-first-party.py")
        shutil.copyfile(FEATURES, self.root / "scripts/buck2_cargo_features.py")
        shutil.copyfile(PROFILE, self.root / "scripts/native-profile.toml")
        shutil.copyfile(TEST_OWNERSHIP, self.root / "scripts/test_source_ownership.py")
        self.bin = self.base / "bin"
        self.bin.mkdir()
        cargo = self.bin / "cargo"
        cargo.write_text("#!/usr/bin/env python3\nimport os, sys\n"
                         "from pathlib import Path\n"
                         "sys.stdout.write(Path(os.environ['BUCK_TEST_METADATA']).read_text())\n")
        cargo.chmod(0o755)
        self.write("tidepool/repr/Cargo.toml", "[package]\nname = 'tidepool-repr'\n")
        self.write("bridge/atomic-write/Cargo.toml", "[package]\nname = 'tidepool-atomic-write'\n")
        self.write("bridge/atomic-write/tests/fixtures/directory_fault.c", "fixture")
        self.write("tidepool/repr/src/lib.rs", "pub fn structural() {}\n")
        self.write("tidepool/repr/tests/suites/repr.rs", "\n".join(
            f'#[path = "../{name}.rs"] mod {name};'
            for name in ("execution_schema_contract", "execution_schema_codec",
                         "extend_checked_equivalence", "metadata_strictness", "strict_jsonl_directory")
        ))
        for module in ("execution_schema_codec", "extend_checked_equivalence",
                       "metadata_strictness", "strict_jsonl_directory"):
            self.write(f"tidepool/repr/tests/{module}.rs", "")
        self.write("tidepool/repr/tests/execution_schema_contract.rs",
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
        self.write("build/protocol/outputs.txt", "tidepool/runtime/src/generated/mod.rs\n")
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

    def arguments(self, expression):
        """Read emitted literal arguments; preserve configured expressions as AST."""
        if isinstance(expression, ast.Dict):
            return {self.arguments(key): self.arguments(value)
                    for key, value in zip(expression.keys, expression.values)}
        if isinstance(expression, (ast.List, ast.Tuple)):
            return [self.arguments(value) for value in expression.elts]
        if isinstance(expression, ast.Constant):
            return expression.value
        return expression

    def rules(self, path):
        tree = ast.parse((self.root / path / "BUCK").read_text())
        rules = {}
        for statement in tree.body:
            self.assertIsInstance(statement, ast.Expr)
            self.assertIsInstance(statement.value, ast.Call)
            call = statement.value
            self.assertIsInstance(call.func, ast.Name)
            if call.func.id == "load":
                continue
            arguments = {keyword.arg: self.arguments(keyword.value) for keyword in call.keywords}
            # A no-argument owning macro (for example tidepool_codegen_md5)
            # emits its fixed targets internally rather than declaring a name.
            if "name" not in arguments:
                self.assertFalse(call.args or call.keywords)
            name = arguments.get("name", call.func.id)
            self.assertNotIn(name, rules, "duplicate named Buck rule")
            rules[name] = (call.func.id, arguments)
        self.assertTrue(rules)
        return rules

    def rule(self, path, name, kind=None):
        rules = self.rules(path)
        self.assertIn(name, rules)
        actual_kind, arguments = rules[name]
        if kind is not None:
            self.assertEqual(actual_kind, kind)
        return arguments

    def groups(self, path):
        rules = self.rules(path)
        groups = {name: arguments["mapped_srcs"] for name, (kind, arguments) in rules.items()
                  if kind == "rust_filegroup"}
        self.assertTrue(groups)
        return rules, groups

    def test_runtime_haskell_reads_are_resources_outside_rust_source_groups(self):
        paths = ["bridge/facade/src/actor_host/runtime.hs",
                 "exomonad/examples/workspace/.exomonad/Runtime.hs",
                 "bridge/haskell/examples/Runtime.hs"]
        for path in paths:
            self.write(path, "module Runtime where\nvalue = 1\n")
        self.write("bridge/facade/src/actor_host/tests.rs", "\n".join(
            f'let source = tidepool_testing::fixture_source("{path}");' for path in paths))
        self.write("build/test-fixtures.json", json.dumps({
            "schema": 1, "kind": "haskell-test-fixtures", "files": paths,
        }))
        # The Haskell-owned export is generated by the complete graph owner.
        self.write("bridge/haskell/rust_inputs.bzl",
                   'export_file(name = "rust_input_examples_Runtime_hs", src = "examples/Runtime.hs",)')
        result = self.generate("--package", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        _, groups = self.groups("bridge/facade")
        before = groups["tidepool_unit_tests_sources"]
        self.assertIn("src/actor_host/tests.rs", before)
        self.assertFalse(set(paths) & set(before.values()))
        self.assertFalse(any(name.endswith(".hs") for name in before))
        host = self.rule("bridge/facade", "facade_host_tests")
        self.assertEqual(host["resource_env"]["TIDEPOOL_TEST_FIXTURE_ROOT"],
                         "$(location //bridge/testing:haskell_test_fixtures)")
        pure = self.rule("bridge/facade", "facade_process_tests")
        self.assertNotIn("TIDEPOOL_TEST_FIXTURE_ROOT", pure.get("resource_env", {}))
        self.assertNotIn("//bridge/testing:haskell_test_fixtures", pure.get("resources", []))
        self.write(paths[0], "module Runtime where\nvalue = 2\n")
        result = self.generate("--package", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.groups("bridge/facade")[1]["tidepool_unit_tests_sources"], before)

    def test_complete_fixture_projection_preserves_paths_and_owners(self):
        tree = ast.parse(GENERATOR.read_text())
        definition = next(node for node in tree.body
                          if isinstance(node, ast.FunctionDef) and node.name == "fixture_resource_outputs")
        files = {
            "bridge/facade/src/fixtures/a-b.hs": "//bridge/facade:haskell_test_fixture_6162",
            ".exomonad/workspace/checks/progress-route.hs": "//:facade_doc_progress",
            "bridge/haskell/examples/Runtime.hs": "//bridge/haskell:rust_input_examples_Runtime_hs",
        }
        outputs = {self.root / "bridge/facade/BUCK": "", self.root / "bridge/testing/BUCK": ""}
        environment = dict(HASKELL_TEST_FIXTURES=files, outputs=outputs,
                           ROOT=self.root, json=json, pathlib=pathlib)
        exec(compile(ast.Module(body=[definition], type_ignores=[]), str(GENERATOR), "exec"), environment)
        environment["fixture_resource_outputs"]()
        manifest = json.loads(outputs[self.root / "build/test-fixtures.json"])
        self.assertEqual(manifest, {"schema": 1, "kind": "haskell-test-fixtures", "files": sorted(files)})
        for path, label in files.items():
            self.assertIn(f'{json.dumps(path)}: {json.dumps(label)}', outputs[self.root / "bridge/testing/BUCK"])
        self.assertIn('src = "src/fixtures/a-b.hs"', outputs[self.root / "bridge/facade/BUCK"])
        self.assertIn('out = "a-b.hs"', outputs[self.root / "bridge/facade/BUCK"])

    def test_fixture_reads_refuse_missing_invalid_or_unregistered_runtime_inputs(self):
        for relative, expected in [
            ("../outside.hs", "invalid Haskell test fixture path"),
            ("bridge/facade/missing.hs", "missing or outside-repository Haskell test fixture"),
            ("bridge/facade/src/fixture.hs", "need complete native graph regeneration"),
        ]:
            with self.subTest(relative=relative):
                self.write("bridge/facade/src/fixture.hs", "value = 1\n")
                self.write("bridge/facade/src/actor_host/tests.rs",
                           f'let value = tidepool_testing::fixture_source("{relative}");')
                self.write("bridge/facade/BUCK", "retained graph\n")
                result = self.generate("--package", "tidepool")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected, result.stderr)
                self.assertEqual((self.root / "bridge/facade/BUCK").read_text(), "retained graph\n")

    def test_partial_projection_keeps_manifest_and_tree_coherent_for_new_includes(self):
        old = "bridge/facade/src/actor_host/old.hs"
        new = "bridge/facade/src/actor_host/new.hs"
        for path in (old, new):
            self.write(path, "value = 1\n")
        self.write("bridge/facade/src/actor_host/tests.rs",
                   'const SOURCE: &str = include_str!("new.hs");')
        manifest = json.dumps({"schema": 1, "kind": "haskell-test-fixtures", "files": [old]})
        self.write("build/test-fixtures.json", manifest)
        result = self.generate("--package", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        tree = self.rule("bridge/testing", "haskell_test_fixtures", "filegroup")
        self.assertEqual(set(tree["srcs"]), {old})
        self.assertEqual((self.root / "build/test-fixtures.json").read_text(), manifest)
        old_label = tree["srcs"][old].partition(":")[2]
        self.assertEqual(self.rule("bridge/facade", old_label, "export_file")["src"],
                         "src/actor_host/old.hs")
        # The unmigrated source remains an ordinary compiler input until the
        # complete projection can issue its matching runtime manifest entry.
        self.assertIn("src/actor_host/new.hs", self.groups("bridge/facade")[1]["tidepool_unit_tests_sources"])

    def test_fixture_loader_cohort_has_no_compiler_or_browser_resources(self):
        self.write("bridge/testing/src/lib.rs", "pub fn fixtures() {}\n")
        result = self.generate("--package", "tidepool-testing")
        self.assertEqual(result.returncode, 0, result.stderr)
        cohort = self.rule("bridge/testing", "testing_fixture_resource_tests", "tidepool_rust_test_cases")
        self.assertEqual(cohort["expected_count"], 3)
        self.assertEqual(cohort["binary"], ":tidepool_testing_unit_tests_binary")
        self.assertNotIn("haskell_worker", cohort)
        self.assertNotIn("resource_env", cohort)
        self.assertNotIn("resources", cohort)
        self.assertEqual(cohort["exact_tests"][-1],
                         "fixtures::tests::fixture_names_refuse_absolute_and_parent_components")

    def test_facade_source_fixture_cohort_declares_sources_without_compiler_inputs(self):
        result = self.generate("--package", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        cohort = self.rule("bridge/facade", "facade_source_fixture_tests", "tidepool_rust_test_cases")
        self.assertEqual(cohort["binary"], ":tidepool_unit_tests")
        # Workspace capture uses these embedded libraries, so the native test
        # cannot depend on the source-backed development checkout fallback.
        producer = self.rule("bridge/facade", "tidepool_build_script_run", "tidepool_buildscript_run")
        self.assertEqual(producer["env"]["TIDEPOOL_EMBED_HASKELL"], "1")
        binary = self.rule("bridge/facade", "tidepool_unit_tests", "tidepool_rust_binary")
        self.assertEqual(binary["env"]["OUT_DIR"], "$(location :tidepool_build_script_run[out_dir])")
        self.assertEqual(cohort["exact_tests"], [
            "actor_host::tests::driver_sources_keep_policy_private_and_preserve_explicit_public_modules",
        ])
        self.assertEqual(cohort["expected_count"], 1)
        self.assertEqual(cohort["resources"], [
            "//bridge/testing:haskell_test_fixtures", "//bridge/haskell:facade_embedded_sources",
        ])
        self.assertEqual(cohort["resource_env"], {
            "TIDEPOOL_TEST_FIXTURE_ROOT": "$(location //bridge/testing:haskell_test_fixtures)",
            "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",
        })
        self.assertNotIn("haskell_worker", cohort)
        self.assertNotIn("env", cohort)

    def test_original_source_proof_control_has_counted_worker_execution_owner(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        group = self.rule("tidepool/toolchain", "toolchain_source_proof_pairing_test",
                          "tidepool_rust_test_cases")
        self.assertEqual(group["binary"], ":tidepool_toolchain_unit_tests_binary")
        self.assertEqual(group["exact_tests"], [
            "artifacts::source_proof_pairing_tests::source_selected_receipt_pairs_prior_program_support_with_actual_original_proof",
        ])
        self.assertEqual(group["expected_count"], 1)
        self.assertIs(group["ignored"], True)
        self.assertIs(group["haskell_worker"], True)
        for resource in (
            "//build/package:compiler_deployment",
            "//bridge/haskell:facade_embedded_sources",
            "//build/package:tidepool_extract_runtime_libraries",
        ):
            self.assertIn(resource, group["resources"])
        for variable in ("TIDEPOOL_COMPILER_DEPLOYMENT", "TIDEPOOL_EXTRACT",
                         "TIDEPOOL_EXTRACT_WORKER", "TIDEPOOL_PRELUDE_DIR"):
            self.assertIn(variable, group["resource_env"])

    def test_toolchain_index_view_cohort_uses_shared_binary_without_compiler_resources(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        cohort = self.rule("tidepool/toolchain", "toolchain_compiler_free_index_view_tests",
                           "tidepool_rust_test_cases")
        self.assertEqual(cohort["binary"], ":tidepool_toolchain_unit_tests_binary")
        self.assertEqual(cohort["expected_count"], 30)
        self.assertEqual(cohort["exact_tests"], TOOLCHAIN_COMPILER_FREE_TESTS)
        self.assertEqual(len(set(cohort["exact_tests"])), 30)
        self.assertNotIn("env", cohort)
        self.assertNotIn("resources", cohort)
        self.assertNotIn("resource_env", cohort)
        self.assertNotIn("haskell_worker", cohort)

        broad = self.rule("tidepool/toolchain", "tidepool_toolchain_unit_tests",
                          "tidepool_rust_isolated_test")
        self.assertIs(broad["haskell_worker"], True)
        self.assertIn("//build/package:compiler_deployment", broad["resources"])

    def test_generated_haskell_source_exports_preserve_compiler_filenames(self):
        self.write("exomonad/actor/Cargo.toml", "[package]\nname = 'exomonad-actor'\n")
        self.write("exomonad/actor/src/lib.rs", "pub fn actor() {}\n")
        self.write("exomonad/actor/src/fixtures/quoted-agent-provider.hs", "module QuotedProvider where\n")
        package = self.package("exomonad-actor", "exomonad/actor", [
            ("exomonad_actor", "lib", "src/lib.rs"),
        ])
        metadata = json.loads(self.metadata.read_text())
        metadata["packages"].append(package)
        metadata["workspace_members"].append(package["id"])
        self.metadata.write_text(json.dumps(metadata))
        result = self.generate("--package", "tidepool", "--package", "exomonad-actor")
        self.assertEqual(result.returncode, 0, result.stderr)
        exports = [self.rule("bridge/facade", "workspace_pinned_check_source", "export_file"),
                   self.rule("exomonad/actor", "quoted_agent_provider_fixture", "export_file")]
        exports.extend(self.rule("tidepool/runtime", f"activation_input_{name}_fixture", "export_file")
                       for name in ("function", "receiver", "resident_original", "resident_request", "resident_shadow"))
        for export in exports:
            with self.subTest(source=export["src"]):
                self.assertTrue(export["out"].endswith(".hs"))
                self.assertEqual(Path(export["out"]).name, Path(export["src"]).name)

    def test_generated_include_inventory_preserves_source_and_data_names(self):
        tree = ast.parse(GENERATOR.read_text())
        owners = [node for node in tree.body if isinstance(node, ast.If) and any(
            isinstance(value, ast.Constant) and value.value == "def declare_rust_test_inputs():"
            for value in ast.walk(node))]
        self.assertEqual(len(owners), 1)
        inputs = {"lib/Original.lhs", "test/Cycle.hs-boot", "test/LiterateCycle.lhs-boot", "test/Data.json"}
        environment = dict(selected={"fixture"}, SUPPORTED_PACKAGES={"fixture"},
                           HASKELL_RUST_INPUTS=inputs, outputs={}, ROOT=self.root,
                           json=json, pathlib=pathlib)
        exec(compile(ast.Module(body=owners, type_ignores=[]), str(GENERATOR), "exec"), environment)
        exports = []
        emitted = environment["outputs"][self.root / "bridge/haskell/rust_inputs.bzl"]
        rules = {"load": lambda *args: None, "export_file": lambda **rule: exports.append(rule)}
        exec(compile(emitted, "rust_inputs.bzl", "exec"), rules)
        rules["declare_rust_test_inputs"]()
        self.assertEqual({rule["src"] for rule in exports}, inputs)
        for rule in exports:
            self.assertEqual(rule["out"], Path(rule["src"]).name)

    def test_codegen_emits_native_units_and_all_registered_integration_tests(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, groups = self.groups("tidepool/codegen")
        binary = self.rule("tidepool/codegen", "native_md5_link", "tidepool_rust_isolated_test")
        self.assertEqual(binary["crate_root"], "tidepool/codegen/tests/native_md5_link.rs")
        self.assertIn(":tidepool_codegen", binary["deps"])
        self.assertIn("tests/native_md5_link.rs", groups["native_md5_link_sources"])
        self.assertIn("prepared_control", buck)
        self.assertIn("tidepool_codegen_unit_tests", buck)
        self.assertIn("tidepool_codegen_unit_tests_sources", groups)
        for name in ("tidepool_codegen", "tidepool_codegen_unit_tests",
                     "native_md5_link", "prepared_control"):
            with self.subTest(target=name):
                target = self.rule("tidepool/codegen", name)
                self.assertEqual(target["named_deps"]["prepared_md5_native"],
                                 ":prepared_md5_native")

    def test_protocol_roster_owns_generated_module_inputs_even_without_source_copies(self):
        self.write("tidepool/runtime/src/generated/mod.rs", "// stale source copy\n")
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        _buck, groups = self.groups("tidepool/runtime")
        for name in ("tidepool_runtime_sources", "tidepool_runtime_unit_tests_sources"):
            mapping = groups[name]
            self.assertNotIn("src/generated/mod.rs", mapping)
            self.assertEqual(mapping["//bridge/protocol:generated[tidepool_runtime_src_generated_mod_rs]"],
                             "tidepool/runtime/src/generated/mod.rs")

    def test_all_integration_suites_use_existing_counted_isolated_runner(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, _groups = self.groups("tidepool/heap")
        for name in ("gc_unit", "raw_scan_validation"):
            wrapper = self.rule("tidepool/heap", name, "tidepool_rust_isolated_test")
            self.assertEqual(wrapper["crate_root"], "tidepool/heap/tests/" + name + ".rs")
            self.assertEqual(wrapper["srcs_filegroup"], ":" + name + "_sources")

    def test_checked_in_prepared_program_cannot_reenter_compile_time_inputs(self):
        control = self.generate()
        self.assertEqual(control.returncode, 0, control.stderr)
        self.write("bridge/haskell/test-prepared-stg/fixtures/future.cbor", "immutable program")
        self.write("tidepool/repr/src/lib.rs", 'const PROGRAM: &[u8] = include_bytes!("../../../bridge/haskell/test-prepared-stg/fixtures/future.cbor");\n')
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("prepared program bytes must be declared runtime resources", result.stderr)

    def test_extract_frontend_binary_is_generated_from_its_cargo_target(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, groups = self.groups("tidepool/extract-cmd")
        binary = self.rule("tidepool/extract-cmd", "tidepool-extract", "tidepool_rust_binary")
        self.assertEqual(binary["crate_root"], "tidepool/extract-cmd/src/main.rs")
        self.assertEqual(
            groups["tidepool-extract_sources"]["src/main.rs"],
            "tidepool/extract-cmd/src/main.rs",
        )

    def test_registered_runtime_test_sources_stay_out_of_production_inputs(self):
        self.write("tidepool/runtime/src/lib.rs", "mod session;\n")
        self.write("tidepool/runtime/src/session/mod.rs", "#[cfg(test)]\nmod turn_scaling_tests;\n")
        self.write("tidepool/runtime/src/session/turn_scaling_tests.rs",
                   'const SOURCE: &str = include_str!("fixtures/compiled-cell-simple.hs");\n')
        self.write("tidepool/runtime/src/session/fixtures/compiled-cell-simple.hs", "pure 42\n")
        self.write("tidepool/runtime/src/session/ordinary.rs",
                   'const SOURCE: &str = include_str!("fixtures/unreviewed.hs");\n')
        self.write("tidepool/runtime/src/session/fixtures/unreviewed.hs", "unknown input\n")
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        _, groups = self.groups("tidepool/runtime")
        production = groups["tidepool_runtime_sources"]
        unit = groups["tidepool_runtime_unit_tests_sources"]
        for path in ("src/session/turn_scaling_tests.rs",
                     "src/session/fixtures/compiled-cell-simple.hs"):
            self.assertNotIn(path, production)
            self.assertIn(path, unit)
        self.assertIn("src/session/ordinary.rs", production)
        self.assertIn("src/session/fixtures/unreviewed.hs", production)

    def test_registered_inline_toolchain_fixture_stays_in_unit_inputs(self):
        self.write("tidepool/toolchain/src/declaration_join.rs",
                   '#[cfg(test)]\nmod authored_tests {\n'
                   'const SOURCE: &str = include_str!("../tests/fixtures/owned-declaration/G1.hs");\n}\n')
        self.write("tidepool/toolchain/tests/fixtures/owned-declaration/G1.hs", "module G1 where\n")
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        _, groups = self.groups("tidepool/toolchain")
        path = "tests/fixtures/owned-declaration/G1.hs"
        self.assertNotIn(path, groups["tidepool_toolchain_sources"])
        self.assertIn(path, groups["tidepool_toolchain_unit_tests_sources"])

    def test_cancellation_measurement_fixture_is_kept_in_the_shared_test_binary(self):
        self.write("bridge/facade/src/actor_host.rs", "#[cfg(test)]\nmod m1_host_tests;\n")
        self.write("bridge/facade/src/actor_host/m1_host_tests.rs",
                   '#[path = "m1_cancel_performance.rs"]\nmod cancel_performance;\n')
        self.write("bridge/facade/src/actor_host/m1_cancel_performance.rs",
                   'const SOURCE: &str = include_str!("m1_cancel_cell.hs");\n')
        self.write("bridge/facade/src/actor_host/m1_cancel_cell.hs", "sleep (seconds 300)\n")
        result = self.generate("--package", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        _, groups = self.groups("bridge/facade")
        for path in ("src/actor_host/m1_cancel_performance.rs", "src/actor_host/m1_cancel_cell.hs"):
            self.assertIn(path, groups["tidepool_unit_tests_sources"])
            # Each real test target may own test-only inputs; no production
            # consumer may carry the measurement fixture into its source map.
            for name, (kind, arguments) in self.rules("bridge/facade").items():
                if "srcs_filegroup" not in arguments:
                    continue
                is_test = kind in ("tidepool_rust_test", "tidepool_rust_isolated_test") or "--test" in arguments.get("rustc_flags", [])
                if not is_test:
                    self.assertNotIn(path, groups[arguments["srcs_filegroup"].removeprefix(":")], name)

    def test_retired_agent_is_not_a_native_root(self):
        result = self.generate("--package", "exomonad-agent")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported: exomonad-agent", result.stderr)

    def test_facade_refuses_missing_or_malformed_gitlink_before_publication(self):
        control = self.generate("--package", "tidepool")
        self.assertEqual(control.returncode, 0, control.stderr)
        descriptor = self.root / "build/native-workspace-gitlink.json"
        self.assertEqual(json.loads(descriptor.read_text()), {
            "schema": 1, "path": ".exomonad/workspace", "mode": "160000",
            "revision": self.workspace_revision,
        })
        previous = {path: path.read_bytes() for path in self.root.rglob("BUCK")}
        previous[descriptor] = descriptor.read_bytes()
        blob = subprocess.check_output(["git", "hash-object", "-w", "--stdin"],
                                       cwd=self.root, input="regular file", text=True).strip()
        for case in ("missing", "regular_file", "unmerged"):
            with self.subTest(case=case):
                subprocess.run(["git", "update-index", "--force-remove", ".exomonad/workspace"],
                               cwd=self.root, check=True)
                if case == "regular_file":
                    subprocess.run(["git", "update-index", "--add", "--cacheinfo", "100644",
                                    blob, ".exomonad/workspace"], cwd=self.root, check=True)
                elif case == "unmerged":
                    subprocess.run(["git", "update-index", "--index-info"], cwd=self.root, check=True,
                                   input=f"160000 {self.workspace_revision} 1\t.exomonad/workspace\n", text=True)
                refused = self.generate("--package", "tidepool")
                self.assertNotEqual(refused.returncode, 0)
                self.assertIn("native facade needs one recorded .exomonad/workspace gitlink" if case == "missing"
                              else "native facade needs an unambiguous stage-0 workspace gitlink", refused.stderr)
                for path, contents in previous.items():
                    self.assertEqual(path.read_bytes(), contents, path)
        subprocess.run(["git", "update-index", "--force-remove", ".exomonad/workspace"],
                       cwd=self.root, check=True)
        subprocess.run(["git", "update-index", "--add", "--cacheinfo", "160000",
                        self.workspace_revision, ".exomonad/workspace"], cwd=self.root, check=True)
        recovered = self.generate("--package", "tidepool", "--check")
        self.assertEqual(recovered.returncode, 0, recovered.stderr)

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
        manifest = self.rule("bridge/facade", "facade_cargo_manifest", "export_file")
        self.assertEqual(manifest["src"], "Cargo.toml")
        build_rule = self.rule("bridge/facade", "tidepool_build_script", "tidepool_rust_binary")
        self.assertIn("//tidepool/toolchain:tidepool_toolchain", build_rule["deps"])
        library_rule = self.rule("bridge/facade", "tidepool", "tidepool_rust_library")
        self.assertNotIn("//tidepool/toolchain:tidepool_toolchain", library_rule.get("deps", []))
        self.assertEqual(build_rule["crate_root"], "bridge/facade/build.rs")
        self.assertEqual(groups["tidepool_build_script_sources"]["build.rs"], "bridge/facade/build.rs")
        run = self.rule("bridge/facade", "tidepool_build_script_run", "tidepool_buildscript_run")
        self.assertEqual(run["env"]["TIDEPOOL_EMBED_HASKELL"], "1")
        self.assertEqual(run["env"]["TIDEPOOL_BUILD_SOURCE_ROOT"], ".")
        inputs = self.rule("bridge/facade", "tidepool_build_source_tree", "tidepool_facade_build_inputs")
        self.assertEqual(inputs["haskell_sources"], "//bridge/haskell:facade_embedded_sources")
        self.assertEqual(inputs["workspace_sources"], "//exomonad/examples/workspace:facade_scaffold_sources")
        self.assertEqual(sum(arguments.get("env", {}).get("OUT_DIR") ==
                             "$(location :tidepool_build_script_run[out_dir])"
                             for _, arguments in buck.values()), 6)
        self.assertFalse(any("codex-shoal-protocol" in dependency
                             for _, arguments in buck.values()
                             for dependency in arguments.get("deps", []) + list(arguments.get("named_deps", {}).values())))
        self.assertIn("src/lib.rs", groups["tidepool_sources"])

    def test_facade_action_surface_keeps_nix_policy_out_of_path_resources(self):
        metadata = json.loads(self.metadata.read_text())
        facade = next(package for package in metadata["packages"] if package["name"] == "tidepool")
        facade["targets"].append({
            "name": "exomonad_action_surface", "kind": ["test"], "edition": "2021",
            "src_path": str(self.root / "bridge/facade/tests/exomonad_action_surface.rs"),
        })
        self.metadata.write_text(json.dumps(metadata))
        self.write("bridge/facade/tests/exomonad_action_surface.rs", "#[test] fn row() {}\n")
        result = self.generate("--package", "tidepool", "--no-default-features", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        group = self.rule("bridge/facade", "exomonad_action_surface", "tidepool_rust_isolated_test")
        self.assertEqual(group["env"], {"TIDEPOOL_KEEP_TEST_LOGS": "1", "EXOMONAD_NIX_OFFLINE": "1"})
        self.assertNotIn("EXOMONAD_NIX_OFFLINE", group["resource_env"])
        self.assertEqual(group["resource_env"]["EXOMONAD_NIX_BIN"],
                         "$(location toolchains//:exomonad_runtime_tools)/bin/nix")

    def test_actor_skill_discovery_consumes_the_recorded_workspace_bundle(self):
        actor = self.package('exomonad-actor', 'exomonad/actor', [
            ('exomonad_actor', 'lib', 'src/lib.rs'),
        ])
        metadata = json.loads(self.metadata.read_text())
        metadata['packages'].append(actor)
        metadata['workspace_members'].append(actor['id'])
        metadata['resolve']['nodes'].append({'id': actor['id'], 'deps': []})
        self.metadata.write_text(json.dumps(metadata))
        self.write('exomonad/actor/Cargo.toml', "[package]\nname = 'exomonad-actor'\n")
        self.write('exomonad/actor/src/lib.rs', '#[test] fn discovery() {}\n')
        result = self.generate('--package', 'exomonad-actor')
        self.assertEqual(result.returncode, 0, result.stderr)
        group = self.rule('exomonad/actor', 'exomonad_actor_unit_tests_all', 'tidepool_rust_test_cases')
        self.assertEqual(group['resource_env']['EXOMONAD_WORKSPACE_GITLINK'],
                         '$(location //build/rust:workspace_gitlink)')
        self.assertEqual(group['resource_env']['EXOMONAD_WORKSPACE_GIT_BUNDLE'],
                         '$(location //build/rust:workspace_git_bundle)')
        self.assertEqual(group['resource_env']['TIDEPOOL_WORKSPACE_TEST_GIT'],
                         '$(location toolchains//:exomonad_runtime_tools)/bin/git')
        self.assertNotIn('//exomonad/examples/workspace:shipped_skills', group['resources'])

    def test_facade_recipe_sources_are_declared_in_focused_and_aggregate_execution(self):
        result = self.generate("--package", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        compiler_sources = {
            "TIDEPOOL_EFFECTS_SOURCE_ROOT": "$(location //bridge/mcp:effects_generated)",
            "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",
            "TIDEPOOL_HASKELL_ACTORS_DIR": "$(location //bridge/haskell:facade_embedded_sources)/actors",
            "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",
            "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",
            "TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)",
            "TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES": "$(location //build/package:tidepool_extract_runtime_libraries)",
            "LD_LIBRARY_PATH": "$(location //build/package:tidepool_extract_runtime_libraries)",
        }
        ordinary_controls = {
            "TIDEPOOL_KEEP_TEST_LOGS": "1",
            "EXOMONAD_NIX_OFFLINE": "1",
        }
        wrappers = []
        definitions = GENERATOR.parents[1] / "build/rust/defs.bzl"
        namespace = {
            "sh_test": lambda **arguments: wrappers.append(arguments),
            "read_root_config": lambda *_: "/declared/compiler",
        }
        exec("\n".join(line for line in definitions.read_text().splitlines()
                       if not line.startswith("load(")), namespace)
        runner_path = GENERATOR.parents[1] / "build/rust/isolated-libtest.py"
        spec = importlib.util.spec_from_file_location("recipe_resource_runner", runner_path)
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        for name in ("facade_prepared_recipe_contract_test",
                     "facade_recipe_source_capture_test", "tidepool_unit_tests_all"):
            with self.subTest(target=name):
                group = self.rule("bridge/facade", name, "tidepool_rust_test_cases")
                self.assertEqual(group["binary"], ":tidepool_unit_tests")
                self.assertIs(group["haskell_worker"], True)
                self.assertEqual(group["env"], ordinary_controls)
                self.assertFalse(set(group["env"]) & set(group["resource_env"]))
                for variable, path in compiler_sources.items():
                    self.assertEqual(group["resource_env"][variable], path)
                for label in ("//bridge/mcp:effects_generated",
                              "//bridge/haskell:facade_embedded_sources",
                              "//build/package:compiler_deployment",
                              "//build/package:tidepool_extract_runtime_libraries"):
                    self.assertIn(label, group["resources"])
                if name == "facade_prepared_recipe_contract_test":
                    self.assertNotIn("TIDEPOOL_RECIPE_WORKSPACE", group["resource_env"])
                else:
                    self.assertEqual(group["resource_env"]["TIDEPOOL_RECIPE_WORKSPACE"],
                                     "$(location //exomonad/examples/workspace:facade_test_sources)/.exomonad")
                    self.assertIn("//exomonad/examples/workspace:facade_test_sources", group["resources"])
                if name == "tidepool_unit_tests_all":
                    self.assertNotIn("exact_tests", group)
                    self.assertIn("TIDEPOOL_BROWSER_DRIVER", group["resource_env"])
                else:
                    self.assertEqual(group["expected_count"], 1)
                    self.assertNotIn("TIDEPOOL_BROWSER_DRIVER", group["resource_env"])
                    self.assertNotIn("//web:dist", group["resources"])
                namespace["tidepool_rust_test_cases"](**group)
                wrapper = wrappers.pop()
                args = wrapper["args"]
                declared = [args[index + 1] for index, value in enumerate(args)
                            if value == "--resource-env"]
                environment = dict(wrapper["env"])
                expected = {key: str(self.root / key) for key in compiler_sources}
                if name != "facade_prepared_recipe_contract_test":
                    expected["TIDEPOOL_RECIPE_WORKSPACE"] = str(self.root / "workspace")
                environment.update(expected)
                environment.update({
                    "TIDEPOOL_TEST_SYSTEMD_RUN": sys.executable,
                    "TIDEPOOL_TEST_SYSTEMCTL": sys.executable,
                    "UNDECLARED_RESOURCE": "/ambient/resource",
                })
                record = {}
                command, _ = runner.delegated_command(
                    ["/declared/libtest"], 300, "app.slice", record,
                    environment=environment, declared_resources=declared)
                for key, value in expected.items():
                    self.assertIn(key, declared)
                    self.assertIn("--setenv=" + key + "=" + value, command)
                    self.assertIn(key, record["environment_names"])
                self.assertNotIn("UNDECLARED_RESOURCE", record["environment_names"])
        binary = self.rule("bridge/facade", "tidepool_unit_tests", "tidepool_rust_binary")
        self.assertFalse(set(binary.get("env", {})) & set(compiler_sources))
        self.assertNotIn("resources", binary)

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
        unit_rule = self.rule("bridge/facade", "tidepool_unit_tests", "tidepool_rust_binary")
        for source in (
            "src/actor_host/m1_host_tests.rs", "src/actor_host/m1_browser_runner.rs",
            "src/actor_host/test_campaign.rs", "src/exomonad.rs",
        ):
            self.assertIn(source, groups["tidepool_unit_tests_sources"])
        self.assertIn("//bridge/testing:tidepool_testing", unit_rule["deps"])
        self.assertEqual(unit_rule["rustc_flags"], ["--test"])
        self.assertNotIn("resources", unit_rule)
        for runtime_only in ("TIDEPOOL_EXTRACT", "TIDEPOOL_BROWSER"):
            self.assertNotIn(runtime_only, unit_rule.get("env", {}))
        self.assertNotIn("//web:dist", unit_rule["deps"])
        cases = {}
        for name in ("facade_process_tests", "facade_host_tests", "facade_host_raw_test",
                     "facade_late_output_test", "facade_browser_test", "tidepool_unit_tests_all"):
            cases[name] = self.rule("bridge/facade", name, "tidepool_rust_test_cases")
            self.assertEqual(cases[name]["binary"], ":tidepool_unit_tests")
            self.assertEqual(cases[name]["jobs"], 1)
        process = cases["facade_process_tests"]
        self.assertEqual(process["expected_count"], 6)
        self.assertEqual(process["resource_env"]["TIDEPOOL_TEST_BASH"], "$(exe toolchains//:bash)")
        self.assertEqual(process["resource_env"]["TIDEPOOL_TEST_SLEEP"], "$(exe toolchains//:sleep)")
        for heavyweight in ("TIDEPOOL_EXTRACT", "TIDEPOOL_BROWSER", "PLAYWRIGHT_BROWSERS_PATH"):
            self.assertNotIn(heavyweight, process["resource_env"])
        for browser_resource in ("//web:dist", "toolchains//:playwright_browsers",
                                 "//build/testing/browser:driver_bundle"):
            self.assertNotIn(browser_resource, process["resources"])
        host = cases["facade_host_tests"]
        self.assertEqual(host["expected_count"], 3)
        self.assertIs(host["haskell_worker"], True)
        self.assertIn("TIDEPOOL_EXTRACT_WORKER", host["resource_env"])
        self.assertEqual(host["resource_env"]["TIDEPOOL_PRELUDE_DIR"], "$(location //bridge/haskell:facade_embedded_sources)/lib")
        self.assertEqual(host["env"], {"TIDEPOOL_KEEP_TEST_LOGS": "1", "EXOMONAD_NIX_OFFLINE": "1"})
        self.assertEqual(host["resource_env"]["EXOMONAD_NIX_BIN"], "$(location toolchains//:exomonad_runtime_tools)/bin/nix")
        self.assertIn("toolchains//:exomonad_runtime_tools", host["resources"])
        for browser_variable in ("TIDEPOOL_BROWSER_DRIVER", "PLAYWRIGHT_BROWSERS_PATH"):
            self.assertNotIn(browser_variable, host["resource_env"])
        for browser_resource in ("toolchains//:playwright_browsers", "//build/testing/browser:driver_bundle"):
            self.assertNotIn(browser_resource, host["resources"])
        raw_host = cases["facade_host_raw_test"]
        self.assertEqual(raw_host["exact_tests"], [
            "actor_host::m1_host_tests::production_host_retains_http_haskell_commands_and_reconnects_without_replay",
        ])
        self.assertEqual(raw_host["expected_count"], 1)
        self.assertEqual(raw_host["resource_env"]["TIDEPOOL_EXTRACT_WORKER"], "$(exe //bridge/haskell:tidepool_extract_bin)")
        self.assertEqual(raw_host["resource_env"]["EXOMONAD_EMBEDDED_ASSET_ROOT"], "$(location //web:dist)/web")
        for resource in ("//bridge/haskell:facade_embedded_sources", "//web:dist", "toolchains//:test_tools_closure"):
            self.assertIn(resource, raw_host["resources"])
        self.assertNotIn("TIDEPOOL_BROWSER_DRIVER", raw_host["resource_env"])
        self.assertEqual(cases["facade_late_output_test"]["expected_count"], 1)
        browser = cases["facade_browser_test"]
        self.assertEqual(browser["expected_count"], 1)
        self.assertIs(browser["ignored"], True)
        self.assertEqual(browser["resource_env"]["EXOMONAD_EMBEDDED_ASSET_ROOT"], "$(location //web:dist)/web")
        self.assertEqual(browser["resource_env"]["TIDEPOOL_BROWSER_DRIVER"], "$(location //build/testing/browser:driver_bundle)/driver.mjs")
        self.assertEqual(browser["resource_env"]["TIDEPOOL_BROWSER_NODE"], "$(exe toolchains//:browser_node)")
        self.assertEqual(browser["resource_env"]["PLAYWRIGHT_BROWSERS_PATH"], "$(location toolchains//:playwright_browsers)")
        self.assertIn("toolchains//:browser_test_closure", browser["resources"])
        self.assertFalse(any("codex-shoal-protocol" in dependency
                             for _, arguments in facade_buck.values()
                             for dependency in arguments.get("deps", []) + list(arguments.get("named_deps", {}).values())))
        support_buck, support_groups = self.groups("bridge/testing")
        self.rule("bridge/testing", "tidepool_testing", "tidepool_rust_library")
        self.assertIn("src/lib.rs", support_groups["tidepool_testing_sources"])
        self.assertIn(
            "//:facade_test_jev_operators",
            groups["tidepool_unit_tests_sources"],
        )
        # Workspace fixtures arrive as one declared directory, preserving both
        # AgentSpec and prompt paths for their include_str relative layout.
        self.assertEqual(groups["tidepool_unit_tests_sources"]["//exomonad/examples/workspace:facade_test_sources"],
                         "exomonad/examples/workspace")
        for relative in (".exomonad/AgentSpec.hs", ".exomonad/prompts/review.md"):
            self.assertTrue((self.root / "exomonad/examples/workspace" / relative).is_file())

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
        self.assertIn("//third-party/rust:tokio-tungstenite-0.29", self.rule("tidepool/runtime", "tidepool_runtime")["deps"])

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

    def test_compile_fail_contract_registers_declared_metadata_and_valid_control(self):
        stem = "machine_lease_double_borrow"
        self.write(f"tidepool/runtime/tests/compile_fail/{stem}.rs", "fn main() {}\n")
        self.write(f"tidepool/runtime/tests/compile_fail/{stem}.stderr", "error[E0499]: borrow\n")
        self.write(f"tidepool/runtime/tests/compile_pass/{stem}.rs", "fn main() {}\n")
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, _ = self.groups("tidepool/runtime")
        contract = self.rule("tidepool/runtime", f"compile_fail_{stem}", "rust_compile_fail")
        self.assertEqual(contract["dependencies"]["tidepool_runtime"], ":tidepool_runtime")
        self.assertEqual(contract["control"], "tests/compile_pass/machine_lease_double_borrow.rs")
        self.assertFalse(any("trybuild" in value for value in contract["dependencies"].values()))
        (self.root / f"tidepool/runtime/tests/compile_pass/{stem}.rs").unlink()
        refused = self.generate()
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("needs pinned diagnostic and valid control", refused.stderr)

    def test_runtime_tests_separate_native_admission_from_compiler_resources(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        runtime, groups = self.groups("tidepool/runtime")
        self.rule("tidepool/runtime", "tidepool_runtime", "tidepool_rust_library")
        self.assertNotIn("bridge/haskell/src/Tidepool/Session.hs", groups["tidepool_runtime_sources"].values())
        self.assertEqual(
            groups["tidepool_runtime_unit_tests_sources"]["//bridge/haskell:session_source"],
            "bridge/haskell/src/Tidepool/Session.hs",
        )
        self.rule("tidepool/runtime", "tidepool_runtime_unit_tests", "tidepool_rust_binary")
        unit = self.rule("tidepool/runtime", "tidepool_runtime_unit_tests_all", "tidepool_rust_test_cases")
        self.assertEqual(unit["resource_env"]["TIDEPOOL_HASKELL_ACTORS_DIR"],
                         "$(location //bridge/haskell:facade_embedded_sources)/actors")
        self.assertIn("//bridge/haskell:facade_embedded_sources", unit["resources"])
        self.assertEqual(set(unit["resource_env"]), {
            "TIDEPOOL_CELL_TEST_EXTRACT", "TIDEPOOL_COMPILER_DEPLOYMENT",
            "TIDEPOOL_EXTRACT", "TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES", "TIDEPOOL_EXTRACT_WORKER",
            "TIDEPOOL_FREER_RESUME_FIXTURE_DIR", "TIDEPOOL_FREER_RETENTION_FIXTURE_DIR",
            "TIDEPOOL_HASKELL_ACTORS_DIR", "TIDEPOOL_M3_FIXTURE_DIR", "TIDEPOOL_PRELUDE_DIR",
            "LD_LIBRARY_PATH", "TIDEPOOL_GHC",
        })
        self.assertEqual(unit["env"], {
            "TIDEPOOL_KEEP_TEST_LOGS": "1",
        })
        self.assertFalse(set(unit["resource_env"]) & set(unit["env"]))
        admission = self.rule("tidepool/runtime", "runtime_admission_tests", "tidepool_rust_test_cases")
        self.assertEqual(admission["binary"], ":tidepool_runtime_unit_tests")
        self.assertEqual(admission["expected_count"], 3)
        self.assertNotIn("haskell_worker", admission)
        self.assertNotIn("TIDEPOOL_EXTRACT", admission.get("env", {}))
        self.assertFalse(any(label.startswith("//web:") for label in admission.get("resources", [])))
        for name in ("runtime_checked_cache_test", "runtime_checked_original_test"):
            checked = self.rule("tidepool/runtime", name, "tidepool_rust_test_cases")
            self.assertEqual(checked["expected_count"], 1)
            self.assertIs(checked["haskell_worker"], True)
            self.assertEqual(checked["resource_env"]["TIDEPOOL_COMPILER_DEPLOYMENT"], "$(location //build/package:compiler_deployment)")
            self.assertEqual(checked["resource_env"]["TIDEPOOL_PRELUDE_DIR"], "$(location //bridge/haskell:facade_embedded_sources)/lib")
        fixture = self.rule("tidepool/runtime", "runtime_compiled_cell_fixture_test", "tidepool_rust_test_cases")
        self.assertEqual(fixture["expected_count"], 8)
        self.assertEqual(fixture["resource_env"]["TIDEPOOL_CELL_TEST_EXTRACT"], "$(exe //tidepool/extract-cmd:tidepool-extract)")
        self.assertEqual(fixture["resource_env"]["TIDEPOOL_COMPILER_DEPLOYMENT"], "$(location //build/package:compiler_deployment)")
        self.assertIn("//build/package:compiler_deployment", fixture["resources"])
        for name, (kind, arguments) in runtime.items():
            if kind == "tidepool_rust_test_cases" and arguments.get("haskell_worker"):
                self.assertIn("TIDEPOOL_PRELUDE_DIR", arguments["resource_env"], name)
                self.assertIn("TIDEPOOL_COMPILER_DEPLOYMENT", arguments["resource_env"], name)
                self.assertFalse(set(arguments["env"]) & set(arguments["resource_env"]), name)
                self.assertEqual(arguments["env"]["TIDEPOOL_KEEP_TEST_LOGS"], "1", name)
        self.assertIs(self.rule("tidepool/bridge-derive", "tidepool_bridge_derive")["proc_macro"], True)

    def test_toolchain_unit_target_declares_legacy_and_v3_join_fixtures(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        runner = self.rule("tidepool/toolchain", "tidepool_toolchain_unit_tests", "tidepool_rust_isolated_test")
        self.assertEqual(runner['resource_env']['TIDEPOOL_CATALOG_TEST_PYTHON'], '$(exe toolchains//:python)')
        self.assertEqual(runner['resource_env']['TIDEPOOL_CATALOG_QUALIFICATION_SCRIPT'],
                         '$(location //build/package:qualification_script)')
        self.assertIn('toolchains//:python', runner['resources'])
        self.assertIn('//build/package:qualification_script', runner['resources'])
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

    def test_catalog_resources_reach_the_generated_delegated_child_command(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        generated = self.rule("tidepool/toolchain", "tidepool_toolchain_unit_tests",
                              "tidepool_rust_isolated_test")
        wrappers = []
        definitions = GENERATOR.parents[1] / "build/rust/defs.bzl"
        namespace = {
            "rust_optimization_level": lambda *_: "1",
            "rust_binary": lambda **_: None,
            "sh_test": lambda **arguments: wrappers.append(arguments),
            "read_root_config": lambda *_: "/declared/compiler",
        }
        exec("\n".join(line for line in definitions.read_text().splitlines()
                       if not line.startswith("load(")), namespace)
        namespace["tidepool_rust_isolated_test"](**generated)
        wrapper = wrappers.pop()
        args = wrapper["args"]
        declared = [args[index + 1] for index, value in enumerate(args)
                    if value == "--resource-env"]
        environment = dict(wrapper["env"])
        expected = {
            "TIDEPOOL_CATALOG_TEST_PYTHON": sys.executable,
            "TIDEPOOL_CATALOG_QUALIFICATION_SCRIPT": str(
                GENERATOR.parents[1] / "build/package/qualification.py"),
        }
        environment.update(expected)
        environment.update({
            "TIDEPOOL_TEST_SYSTEMD_RUN": sys.executable,
            "TIDEPOOL_TEST_SYSTEMCTL": sys.executable,
            "UNDECLARED_RESOURCE": "/ambient/resource",
        })
        runner_path = GENERATOR.parents[1] / "build/rust/isolated-libtest.py"
        spec = importlib.util.spec_from_file_location("catalog_resource_runner", runner_path)
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        record = {}
        command, _ = runner.delegated_command(
            ["/declared/libtest"], 300, "app.slice", record,
            environment=environment, declared_resources=declared)
        for key, value in expected.items():
            self.assertNotIn(key, generated.get("env", {}))
            self.assertIn(key, declared)
            self.assertIn("--setenv=" + key + "=" + value, command)
            self.assertIn(key, record["environment_names"])
        self.assertNotIn("UNDECLARED_RESOURCE", record["environment_names"])

    def test_source_tree_preserves_fixture_layout_and_target_inputs(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck, groups = self.groups("tidepool/repr")
        self.assertEqual(self.rule("tidepool/repr", "tidepool_repr")["crate_root"], "tidepool/repr/src/lib.rs")
        integration_rule = self.rule("tidepool/repr", "repr", "tidepool_rust_isolated_test")
        self.assertEqual(integration_rule["crate_root"], "tidepool/repr/tests/suites/repr.rs")
        self.assertEqual(integration_rule["srcs_filegroup"], ":repr_sources")
        lib = groups["tidepool_repr_sources"]
        unit = groups["tidepool_repr_unit_tests_sources"]
        integration = groups["repr_sources"]
        fault = "bridge/atomic-write/tests/fixtures/directory_fault.c"
        self.assertEqual(lib["src/lib.rs"], "tidepool/repr/src/lib.rs")
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
            "//bridge/atomic-write:directory_fault_fixture",
        })
        # A new #[path] sibling belongs only to the root that imports it.
        self.write("tidepool/heap/tests/future.rs", "#[test] fn future() {}\n")
        self.write("tidepool/heap/tests/gc_unit.rs", '#[path = "future.rs"] mod future;\n')
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        _, heap_groups = self.groups("tidepool/heap")
        self.assertIn("tests/future.rs", heap_groups["gc_unit_sources"])
        self.assertNotIn("tests/future.rs", heap_groups["raw_scan_validation_sources"])
        (self.root / "tidepool/repr/tests/metadata_strictness.rs").unlink()
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("native source ownership refused", result.stderr)
        self.assertIn("tidepool-repr: cannot resolve every module for target repr", result.stderr)

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

    def test_local_development_features_refuse_before_any_graph_publication(self):
        control = self.generate()
        self.assertEqual(control.returncode, 0, control.stderr)
        previous = (self.root / "tidepool/repr/BUCK").read_bytes()
        metadata = json.loads(self.metadata.read_text())
        package = next(p for p in metadata["packages"] if p["name"] == "tidepool-repr")
        package["dependencies"].append({
            "name": "tidepool-heap", "rename": "heap_fixture", "kind": "dev",
            "target": None, "optional": False, "uses_default_features": False,
            "features": ["test-support"],
        })
        self.metadata.write_text(json.dumps(metadata))
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported local Cargo feature(s) test-support", result.stderr)
        self.assertIn("package tidepool-heap; native root tidepool-repr", result.stderr)
        self.assertIn("tidepool-repr --dev:heap_fixture--> tidepool-heap", result.stderr)
        self.assertEqual((self.root / "tidepool/repr/BUCK").read_bytes(), previous)

    def test_workspace_resolve_features_do_not_change_first_party_configuration(self):
        metadata = json.loads(self.metadata.read_text())
        node = next(n for n in metadata["resolve"]["nodes"] if n["id"] == "tidepool-repr")
        node["features"] = ["test-support"]
        self.metadata.write_text(json.dumps(metadata))
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck = (self.root / "tidepool/repr/BUCK").read_text()
        self.assertNotIn("test-support", self.rule("tidepool/repr", "tidepool_repr")["features"])

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
        deps = self.rule("tidepool/repr", "tidepool_repr")["deps"]
        self.assertIn("//third-party/rust:sha2-0_10_9", deps)
        self.assertEqual(self.rule("tidepool/repr", "tidepool_repr")["named_deps"]["sha2_new"], "//third-party/rust:sha2-0_11_0")
        self.assertNotIn("//third-party/rust:sha2", deps)

    def test_workspace_default_can_be_suppressed_without_emitting_that_package(self):
        result = self.generate("--no-default-features", "tidepool")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "bridge/facade/BUCK").exists())


if __name__ == "__main__":
    unittest.main()
