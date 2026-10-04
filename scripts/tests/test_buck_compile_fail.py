"""Native compile contract driver refuses unqualified and unrelated failures."""
import importlib.util
import json
from pathlib import Path
import subprocess
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


if __name__ == "__main__":
    unittest.main()
