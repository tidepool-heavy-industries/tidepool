"""Selection contracts for thin native frontends, without invoking Buck."""
import importlib.util
import contextlib
import copy
import io
import os
import shutil
import subprocess
import tempfile
import sys
import types
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("native_workflow", Path(__file__).parents[1] / "native-workflow.py")
workflow = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workflow)

ROSTER = {"example": {"libraries": {"example": {"build": "//pkg:example", "test_build": "//pkg:unit", "test": "//pkg:unit_all"}},
                      "binaries": {}, "integration": {"scenario": {"test_build": "//pkg:scenario_binary", "test": "//pkg:scenario"}}}}


class NativeWorkflowTests(unittest.TestCase):
    def invoke(self, arguments, result=0):
        with patch.object(workflow, "roster", return_value=ROSTER), patch.object(workflow, "buck", return_value=result) as buck:
            status = workflow.main(arguments)
            return status, buck.call_args_list

    def test_focused_execution_uses_declared_runinfo_and_retains_exact_count(self):
        status, calls = self.invoke(["test-target", "example", "scenario", "--", "--exact", "scenario::valid", "--expected-count", "1"])
        self.assertEqual(status, 0)
        self.assertEqual(calls[0].args, ("run", ["//pkg:scenario"], ["--exact", "scenario::valid", "--expected-count", "1"]))

    def test_default_library_runs_counted_wrapper(self):
        status, calls = self.invoke(["test-lib", "example"])
        self.assertEqual(calls[0].args, ("test", ["//pkg:unit_all"], []))

    def test_unknown_target_and_retired_expression_refuse_before_buck(self):
        for arguments in (["test-target", "example", "unknown"], ["test-lib", "example", "test(valid)"]):
            with self.subTest(arguments=arguments), patch.object(workflow, "roster", return_value=ROSTER), patch.object(workflow, "buck") as buck:
                with self.assertRaises(ValueError):
                    workflow.main(arguments)
                buck.assert_not_called()

    def test_compile_check_does_not_execute_harnesses(self):
        _, calls = self.invoke(["check"])
        self.assertEqual(calls[0].args, ("build", ["//pkg:example", "//pkg:scenario_binary", "//pkg:unit"]))

    def test_package_check_compiles_all_consumer_kinds_without_unselected_packages(self):
        packages = copy.deepcopy(ROSTER)
        packages["example"]["binaries"]["tool"] = {
            "build": "//pkg:tool", "test": "//pkg:tool_tests", "test_build": "//pkg:tool_tests_binary"}
        packages["example"]["binaries"]["without_harness"] = {"build": "//pkg:without_harness"}
        packages["unselected"] = {"libraries": {"other": {"build": "//other:other"}}, "binaries": {}, "integration": {}}
        expected = ["//pkg:example", "//pkg:scenario_binary", "//pkg:tool", "//pkg:tool_tests_binary", "//pkg:unit", "//pkg:without_harness"]
        with patch.object(workflow, "roster", return_value=packages), patch.object(workflow, "buck", return_value=0) as buck, contextlib.redirect_stderr(io.StringIO()) as report:
            self.assertEqual(workflow.main(["check", "example", "example"]), 0)
        self.assertEqual(buck.call_args.args, ("build", expected))
        self.assertEqual(report.getvalue().splitlines()[1:], expected)

    def test_compile_plan_prints_the_same_selection_without_launching_buck(self):
        with patch.object(workflow, "roster", return_value=ROSTER), patch.object(workflow, "buck") as buck, contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(workflow.main(["check-plan", "example"]), 0)
        buck.assert_not_called()
        self.assertEqual(output.getvalue().splitlines(), workflow.compile_labels(ROSTER, ["example"]))

    def test_unknown_package_and_kind_refuse_before_compile(self):
        packages = copy.deepcopy(ROSTER)
        packages["example"]["invented"] = {"hidden_consumer": {"build": "//pkg:hidden"}}
        for roster, arguments in ((ROSTER, ["check", "missing"]), (ROSTER, ["check-plan", "missing"]), (packages, ["check", "example"])):
            with self.subTest(arguments=arguments), patch.object(workflow, "roster", return_value=roster), patch.object(workflow, "buck") as buck:
                with self.assertRaises(ValueError):
                    workflow.main(arguments)
                buck.assert_not_called()
        with self.assertRaisesRegex(ValueError, "target kind"):
            workflow.package_target(ROSTER, "example", "unit_tests")

    def test_missing_or_invalid_compile_labels_cannot_silently_skip_a_consumer(self):
        for kind, name, key in (("libraries", "example", "build"), ("libraries", "example", "test_build"), ("integration", "scenario", "test_build"), ("integration", "scenario", "test")):
            for invalid in (None, "", "//pkg:", "unit_tests"):
                packages = copy.deepcopy(ROSTER)
                if invalid is None:
                    del packages["example"][kind][name][key]
                else:
                    packages["example"][kind][name][key] = invalid
                with self.subTest(kind=kind, key=key, invalid=invalid), patch.object(workflow, "roster", return_value=packages), patch.object(workflow, "buck") as buck:
                    with self.assertRaises(ValueError):
                        workflow.main(["check", "example"])
                    buck.assert_not_called()

    def test_fixture_action_failure_preserved_and_cohort_still_attempted(self):
        with patch.object(workflow, "roster", return_value=ROSTER), patch.object(workflow, "buck", side_effect=[7, 0]) as buck:
            self.assertEqual(workflow.main(["fixtures-check", "containers-contract"]), 7)
            self.assertEqual(buck.call_args_list[1].args, ("test", ["//bridge/haskell:corpus_containers_contract_test"]))

    def test_duplicate_cohorts_and_missing_suite_refuse_without_fallback(self):
        for arguments in (["fixtures-check", "suite", "suite"], ["suite", "missing"]):
            with self.subTest(arguments=arguments), patch.object(workflow, "roster", return_value=ROSTER), patch.object(workflow, "buck") as buck:
                with self.assertRaises(ValueError):
                    workflow.main(arguments)
                buck.assert_not_called()

    def test_verify_discovers_haskell_suite_targets_from_the_native_graph(self):
        discovery = types.SimpleNamespace(stdout="root//bridge/haskell:control\n")
        with patch.object(workflow, "roster", return_value=ROSTER), patch.object(workflow, "buck", return_value=0) as buck, patch.object(workflow.subprocess, "run", return_value=discovery) as query:
            self.assertEqual(workflow.main(["verify"]), 0)
        self.assertIn("haskell_component_suite", query.call_args.args[0][-1])
        self.assertIn("root//bridge/haskell:control", buck.call_args_list[-1].args[1])

    def test_verify_refuses_empty_haskell_graph_selection(self):
        with patch.object(workflow, "roster", return_value=ROSTER), patch.object(workflow, "buck") as buck, patch.object(workflow.subprocess, "run", return_value=types.SimpleNamespace(stdout="")):
            with self.assertRaisesRegex(ValueError, "no valid owning targets"):
                workflow.main(["verify"])
            buck.assert_not_called()

    def test_standard_unittest_adapter_executes_control_and_refuses_zero(self):
        adapter_spec = importlib.util.spec_from_file_location("native_unittest", Path(__file__).parents[1] / "unittest-main.py")
        adapter = importlib.util.module_from_spec(adapter_spec)
        adapter_spec.loader.exec_module(adapter)
        control = types.ModuleType("native_control")
        empty = types.ModuleType("native_empty")
        class Control(unittest.TestCase):
            def test_control(self):
                self.assertEqual(2 + 2, 4)
        control.Control = Control
        with patch.dict(os.environ, TIDEPOOL_SCRIPT_TEST_ROOT=str(Path(__file__).parents[2])), patch.dict(sys.modules, native_control=control, native_empty=empty), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(adapter.main(["native_control"]), 0)
            with self.assertRaises(ValueError):
                adapter.main(["native_empty"])

    def test_standard_unittest_imports_declared_package_from_resource_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts/tests").mkdir(parents=True)
            shutil.copyfile(Path(__file__).parents[1] / "unittest-main.py", root / "scripts/unittest-main.py")
            (root / "scripts/tests/import_control.py").write_text(
                "import unittest\nclass DeclaredControl(unittest.TestCase):\n"
                "    def test_declared_control(self):\n        self.assertTrue(True)\n")
            environment = dict(os.environ, TIDEPOOL_SCRIPT_TEST_ROOT=str(root), PYTHONPATH="")
            result = subprocess.run([sys.executable, str(root / "scripts/unittest-main.py"),
                                     "scripts.tests.import_control"], cwd=root,
                                    env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Ran 1 test", result.stderr)
            environment.pop("TIDEPOOL_SCRIPT_TEST_ROOT")
            refused = subprocess.run([sys.executable, str(root / "scripts/unittest-main.py"),
                                      "scripts.tests.import_control"], cwd=root,
                                     env=environment, capture_output=True, text=True)
            self.assertEqual(refused.returncode, 2, refused.stderr)
            self.assertIn("declare TIDEPOOL_SCRIPT_TEST_ROOT", refused.stderr)

    def test_buck_run_separates_runner_arguments_from_buck_options(self):
        with patch.object(workflow.subprocess, "call", return_value=0) as call:
            workflow.buck("run", ["//pkg:scenario"], ["--exact", "scenario::valid"])
        self.assertEqual(call.call_args.args[0][-4:], ["//pkg:scenario", "--", "--exact", "scenario::valid"])

    def resolved_command(self, arguments):
        with patch.object(workflow, "roster", return_value=ROSTER), \
             patch.object(workflow.subprocess, "call", return_value=0) as call, \
             contextlib.redirect_stderr(io.StringIO()) as report:
            self.assertEqual(workflow.main(arguments), 0)
        return call.call_args.args[0], report.getvalue()

    def test_profile_is_explicit_and_reported_in_actual_focused_buck_argv(self):
        for profile in workflow.PROFILES:
            with self.subTest(profile=profile):
                command, report = self.resolved_command([
                    "--profile", profile, "test-target", "example", "scenario",
                    "--", "--exact", "scenario::valid", "--expected-count", "1",
                ])
                self.assertEqual(command[2:9], [
                    "run", "--local-only", "-c", "remote.enabled=false",
                    "-c", f"tidepool.profile={profile}", "//pkg:scenario",
                ])
                self.assertEqual(command[9:], [
                    "--", "--exact", "scenario::valid", "--expected-count", "1",
                ])
                self.assertIn(f"Tidepool native profile: {profile}", report)

    def test_library_default_ignores_ambient_profile_requests(self):
        with patch.dict(os.environ, {"TIDEPOOL_NATIVE_PROFILE": "production"}):
            command, report = self.resolved_command(["test-lib", "example"])
        self.assertIn("tidepool.profile=fast-dev", command)
        self.assertNotIn("tidepool.profile=production", command)
        self.assertIn("Tidepool native profile: fast-dev", report)

    def test_native_target_retains_runner_arguments_without_roster_discovery(self):
        with patch.object(workflow, "roster") as roster, \
             patch.object(workflow.subprocess, "call", return_value=0) as call, \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(workflow.main([
                "--profile", "production", "test-native", "//native:case",
                "--", "--pattern", "one leaf",
            ]), 0)
            roster.assert_not_called()
        command = call.call_args.args[0]
        self.assertEqual(command[-4:], ["//native:case", "--", "--pattern", "one leaf"])
        self.assertIn("tidepool.profile=production", command)

    def test_invalid_profile_refuses_before_native_launch(self):
        with patch.object(workflow.subprocess, "call") as call, \
             contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as refused:
                workflow.main(["--profile", "unknown", "test-lib", "example"])
        self.assertEqual(refused.exception.code, 2)
        call.assert_not_called()

    def test_discovery_and_compile_only_selection_use_the_explicit_profile(self):
        for arguments in (["test-list", "example", "libraries"], ["check"]):
            with self.subTest(arguments=arguments):
                command, _ = self.resolved_command(["--profile", "debug", *arguments])
                self.assertIn("tidepool.profile=debug", command)
        with patch.object(workflow, "roster", return_value=ROSTER), \
             patch.object(workflow.subprocess, "call", return_value=0) as call, \
             patch.object(workflow.subprocess, "run", return_value=types.SimpleNamespace(
                 stdout="root//bridge/haskell:control\n")) as query, \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(workflow.main(["--profile", "production", "verify"]), 0)
        self.assertIn("tidepool.profile=production", query.call_args.args[0])
        for launch in call.call_args_list:
            self.assertIn("tidepool.profile=production", launch.args[0])


if __name__ == "__main__":
    unittest.main()
