"""Native compile contract driver refuses unqualified and unrelated failures."""
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest

DRIVER = Path(__file__).resolve().parents[2] / "build/rust/compile-fail.py"
spec = importlib.util.spec_from_file_location("native_compile_fail", DRIVER)
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class CompileContractDiagnostics(unittest.TestCase):
    expected = {"code": "E0499", "message": "cannot borrow twice",
                "file": "lease.rs", "line": 6, "column": 18}

    def diagnostic(self, code="E0499", line=6):
        return json.dumps({"level": "error", "code": {"code": code},
            "message": "cannot borrow twice", "spans": [{"is_primary": True,
            "file_name": "declared/lease.rs", "line_start": line, "column_start": 18}]})

    def result(self, status=1, **kwargs):
        return subprocess.CompletedProcess([], status, stderr=self.diagnostic(**kwargs))

    def test_exact_error_requires_the_pinned_primary_source_span(self):
        driver.check_refusal(self.result(), self.expected)
        with self.assertRaisesRegex(ValueError, "primary source span"):
            driver.check_refusal(self.result(line=7), self.expected)

    def test_unrelated_missing_dependency_error_cannot_prove_borrow_contract(self):
        driver.check_refusal(self.result(), self.expected)
        with self.assertRaisesRegex(ValueError, "pinned error code/message"):
            driver.check_refusal(self.result(code="E0463"), self.expected)

    def test_success_zero_errors_multiple_errors_and_interruption_refuse(self):
        driver.check_refusal(self.result(), self.expected)
        for result in (self.result(status=0), self.result(status=-9),
                       subprocess.CompletedProcess([], 1, stderr=""),
                       subprocess.CompletedProcess([], 1,
                           stderr=self.diagnostic() + "\n" + self.diagnostic())):
            with self.subTest(status=result.returncode, stderr=result.stderr):
                with self.assertRaisesRegex(ValueError, "expected one type refusal"):
                    driver.check_refusal(result, self.expected)


class CompileContractCommandVector(unittest.TestCase):
    expected_text = "error[E0499]: cannot borrow twice\n --> src/lease.rs:6:18\n"
    diagnostic = json.dumps({"level": "error", "code": {"code": "E0499"},
        "message": "cannot borrow twice", "spans": [{"is_primary": True,
        "file_name": "src/lease.rs", "line_start": 6, "column_start": 18}]})

    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="compile-fail-command-")
        self.addCleanup(self.root.cleanup)
        self.directory = Path(self.root.name)
        self.log = self.directory / "compiler.jsonl"
        self.compiler = self.directory / "rustc-fixture.py"
        self.compiler.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "args = sys.argv[1:]\n"
            "with open(os.environ['COMPILE_FAIL_TEST_LOG'], 'a') as log:\n"
            "    log.write(json.dumps(args) + '\\n')\n"
            "crate = args[args.index('--crate-name') + 1]\n"
            "if crate == 'contract_control':\n"
            "    sys.exit(0)\n"
            f"print({self.diagnostic!r}, file=sys.stderr)\n"
            "sys.exit(1)\n"
        )
        self.compiler.chmod(self.compiler.stat().st_mode | stat.S_IXUSR)
        self.control = self.directory / "control.rs"
        self.control.write_text("fn main() {}\n")
        self.source = self.directory / "refusal.rs"
        self.source.write_text("fn main() {}\n")
        self.expected = self.directory / "expected.stderr"
        self.expected.write_text(self.expected_text)
        self.inputs = self.directory / "inputs.json"

    def run_action(self, command, suffix):
        self.inputs.write_text(json.dumps({
            "rustc_command": command,
            "direct": {},
            "transitive": [],
        }))
        output = self.directory / suffix
        previous = os.environ.get("COMPILE_FAIL_TEST_LOG")
        os.environ["COMPILE_FAIL_TEST_LOG"] = str(self.log)
        try:
            driver.run_action(type("Args", (), {
                "output": output,
                "inputs": self.inputs,
                "control": self.control,
                "source": self.source,
                "expected": self.expected,
                "edition": "2021",
            })())
        finally:
            if previous is None:
                os.environ.pop("COMPILE_FAIL_TEST_LOG", None)
            else:
                os.environ["COMPILE_FAIL_TEST_LOG"] = previous
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual([call[call.index("--crate-name") + 1] for call in calls],
                         ["contract_control", "contract_refusal"])
        self.assertTrue((output / "control.jsonl").exists())
        self.assertTrue((output / "refusal.jsonl").exists())
        proof = json.loads((output / "proof.json").read_text())
        self.assertEqual((proof["control_compile_count"], proof["refusal_compile_count"],
                          proof["control_status"], proof["refusal_status"]), (1, 1, 0, 1))

    def test_direct_executable_command_compiles_control_and_pinned_refusal(self):
        self.run_action([str(self.compiler)], "direct-output")

    def test_multi_argument_runinfo_wrapper_is_invoked_as_a_vector(self):
        # Match the repository rust toolchain RunInfo: Python launcher, the
        # source-aware rustc wrapper, and rustc's executable separated by --.
        wrapper = Path(__file__).resolve().parents[2] / "build/rust/test-source-rustc.py"
        self.run_action([sys.executable, str(wrapper), "--rustc", str(self.compiler), "--"],
                        "wrapped-output")

    def test_missing_or_malformed_compiler_vector_is_refused(self):
        for command in (None, [], "rustc", ["python", ""]):
            with self.subTest(command=command):
                with self.assertRaisesRegex(ValueError, "non-empty argument vector"):
                    driver.compiler_command({"rustc_command": command})


if __name__ == "__main__":
    unittest.main()
