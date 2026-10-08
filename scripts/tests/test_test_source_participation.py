"""Compiler-action source obligations and dep-info decoding controls."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("test_source_rustc", ROOT / "build/rust/test-source-rustc.py")
wrapper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrapper)
spec = importlib.util.spec_from_file_location("test_source_ownership", ROOT / "scripts/test_source_ownership.py")
ownership = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ownership)


class DepInfoTests(unittest.TestCase):
    def test_multiple_rules_phony_targets_comments_and_escaped_paths(self):
        self.assertEqual(wrapper.depfile_dependencies(
            "binary: root.rs escaped\\ path.rs hash\\#name.rs dollar$$name.rs "
            "back\\\\slash.rs colon\\:name.rs \\\n\tleaf.rs\n"
            "metadata: root.rs second.rs\nroot.rs:\nsecond.rs:\n# env-dep:VALUE=text\n"
        ), {"root.rs", "escaped path.rs", "hash#name.rs", "dollar$name.rs",
            "back\\slash.rs", "colon:name.rs", "leaf.rs", "second.rs"})

    def test_empty_or_malformed_dep_info_cannot_certify_participation(self):
        for body in ("", "root.rs:\n", "not a Make rule\n"):
            with self.subTest(body=body), self.assertRaises(ValueError):
                wrapper.depfile_dependencies(body)

    def test_rust_fixture_source_has_an_explicit_fixture_role(self):
        fixture = "tidepool/extract-cmd/src/fixtures/build_products_worker.rs"
        self.assertIn(fixture, ownership.TEST_ONLY_FIXTURES["tidepool-extract-cmd"])
        self.assertNotIn(fixture, ownership.TEST_ONLY_MODULES["tidepool-extract-cmd"])
        self.assertEqual(ownership.TEST_ONLY_SOURCES["tidepool-extract-cmd"],
                         ownership.TEST_ONLY_MODULES["tidepool-extract-cmd"] |
                         ownership.TEST_ONLY_FIXTURES["tidepool-extract-cmd"])

    def test_native_macro_declares_module_and_fixture_manifest_as_compile_input(self):
        manifests = []
        binaries = []
        namespace = {
            "rust_optimization_level": lambda *_: "0",
            "test_source_requirements": lambda **kwargs: manifests.append(kwargs),
            "rust_binary": lambda **kwargs: binaries.append(kwargs),
        }
        definitions = (ROOT / "build/rust/defs.bzl").read_text()
        exec("\n".join(line for line in definitions.splitlines()
                       if not line.startswith("load(")), namespace)
        namespace["tidepool_rust_binary"](
            name="unit_tests", package_name="component", package_dir="owned/component",
            version="0.1.0", rustc_flags=["--test"],
            required_test_modules=["owned/component/src/required.rs"],
            test_fixtures=["owned/component/src/worker.rs"],
        )
        self.assertEqual(manifests[0]["modules"], ["owned/component/src/required.rs"])
        self.assertEqual(manifests[0]["fixtures"], ["owned/component/src/worker.rs"])
        self.assertEqual(binaries[0]["env"]["TIDEPOOL_TEST_SOURCE_REQUIREMENTS"],
                         "$(location :unit_tests_source_requirements)")
        self.assertNotIn("required_test_modules", binaries[0])
        self.assertNotIn("test_fixtures", binaries[0])


class CompilerActionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="test source participation ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.projection = self.root / "declared sources"
        self.package = self.projection / "owned/component"
        self.package.mkdir(parents=True)
        self.extras = self.root / "declared extras"
        self.requirements = self.root / "requirements.json"
        self.requirements.write_text(json.dumps({
            "version": 1, "target": "//owned/component:unit_tests_source_requirements",
            "package": "component", "package_dir": "owned/component",
            "modules": ["owned/component/src/lib.rs", "owned/component/src/required.rs"],
            "fixtures": ["owned/component/src/worker.rs", "owned/component/src/fixture.hs"],
        }))
        self.environment = {
            "TIDEPOOL_TEST_SOURCE_REQUIREMENTS": str(self.requirements),
            "CARGO_MANIFEST_DIR": str(self.package),
        }
        self.dependencies = {"owned/component/src/lib.rs", "owned/component/src/required.rs",
                             "owned/component/src/worker.rs"}
        self.calls = []

    def compiler(self, command):
        # This compiler-action control supplies dependency evidence directly.
        # Real Rust cfg/module participation is qualified by the native action.
        self.calls.append(command)
        arguments = wrapper.expanded_arguments(command[1:])
        modes = wrapper.emit_modes(arguments)
        depfile = Path(modes["dep-info"])
        depfile.parent.mkdir(parents=True, exist_ok=True)
        dependencies = [str(self.projection / path).replace(" ", "\\ ")
                        for path in sorted(self.dependencies)]
        depfile.write_text("artifact: " + " ".join(dependencies) + "\n" +
                           "\n".join(path + ":" for path in dependencies))
        return 0

    def arguments(self):
        return ["--test", "--crate-name", "component", "--out-dir", str(self.extras),
                "--emit=link=" + str(self.root / "test binary")]

    def proof(self):
        return json.loads((self.extras / "test-source-participation.json").read_text())

    def test_same_compiler_action_preserves_response_arguments_and_emit_outputs(self):
        response = self.root / "compiler response"
        response.write_text("\n".join([*self.arguments(), "--cfg", 'feature="selected"',
                                       "--emit=metadata=" + str(self.root / "metadata")]) + "\n")
        arguments = ["@" + str(response)]
        with patch.object(wrapper.subprocess, "call", side_effect=self.compiler):
            self.assertEqual(wrapper.compile_test("/pinned/rustc", arguments, self.environment), 0)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.calls[0][1], "@" + str(response))
        modes = wrapper.emit_modes(wrapper.expanded_arguments(self.calls[0][1:]))
        self.assertEqual(modes["link"], str(self.root / "test binary"))
        self.assertEqual(modes["metadata"], str(self.root / "metadata"))
        proof = self.proof()
        self.assertEqual(proof["status"], "complete")
        self.assertEqual(proof["cfg"], ['feature="selected"'])
        self.assertTrue(all(module["compiler_dependency"] for module in proof["modules"]))
        fixtures = {fixture["path"]: fixture for fixture in proof["fixtures"]}
        self.assertTrue(fixtures["owned/component/src/worker.rs"]["compiler_dependency"])
        self.assertFalse(fixtures["owned/component/src/fixture.hs"]["compiler_dependency"])

    def test_missing_required_module_refuses_action_even_when_fixture_is_consumed(self):
        self.dependencies.remove("owned/component/src/required.rs")
        with patch.object(wrapper.subprocess, "call", side_effect=self.compiler):
            with self.assertRaisesRegex(ValueError, "required.rs"):
                wrapper.compile_test("/pinned/rustc", self.arguments(), self.environment)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.proof()["missing_modules"], ["owned/component/src/required.rs"])
        self.assertEqual(self.proof()["status"], "refused")

    def test_existing_dep_info_is_preserved_and_retained_in_declared_extras(self):
        original = self.root / "already declared.d"
        arguments = self.arguments() + ["--emit=dep-info=" + str(original)]
        with patch.object(wrapper.subprocess, "call", side_effect=self.compiler):
            self.assertEqual(wrapper.compile_test("/pinned/rustc", arguments, self.environment), 0)
        self.assertEqual(self.calls, [["/pinned/rustc", *arguments]])
        self.assertEqual((self.extras / "test-source-participation.d").read_bytes(),
                         original.read_bytes())

    def test_non_test_and_unowned_compilations_forward_untouched(self):
        with patch.object(wrapper.subprocess, "call", return_value=0) as compiler:
            wrapper.compile_test("/pinned/rustc", ["--version"], self.environment)
            wrapper.compile_test("/pinned/rustc", self.arguments(), {})
        self.assertEqual([call.args[0] for call in compiler.call_args_list],
                         [["/pinned/rustc", "--version"], ["/pinned/rustc", *self.arguments()]])

    def test_failed_rustc_does_not_issue_source_participation_proof(self):
        with patch.object(wrapper.subprocess, "call", return_value=7):
            self.assertEqual(wrapper.compile_test("/pinned/rustc", self.arguments(), self.environment), 7)
        self.assertFalse((self.extras / "test-source-participation.json").exists())

    def test_missing_declared_extras_refuses_before_compiler_work(self):
        with patch.object(wrapper.subprocess, "call") as compiler:
            with self.assertRaisesRegex(ValueError, "declared extras"):
                wrapper.compile_test("/pinned/rustc", ["--test"], self.environment)
        compiler.assert_not_called()

    def test_response_cycle_and_outside_projection_cannot_issue_proof(self):
        response = self.root / "recursive"
        response.write_text("@" + str(response))
        with self.assertRaisesRegex(ValueError, "recursive"):
            wrapper.expanded_arguments(["@" + str(response)])
        self.environment["CARGO_MANIFEST_DIR"] = str(self.root / "unrelated")
        with patch.object(wrapper.subprocess, "call") as compiler:
            with self.assertRaisesRegex(ValueError, "source projection"):
                wrapper.compile_test("/pinned/rustc", self.arguments(), self.environment)
        compiler.assert_not_called()


if __name__ == "__main__":
    unittest.main()
