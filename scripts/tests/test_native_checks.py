"""Native check diagnostic contracts, without a compiler invocation."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native_checks", Path(__file__).parents[2] / "build/rust/checks.py")
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)


class NativeCheckDiagnostics(unittest.TestCase):
    def test_nested_warning_and_error_are_failures_but_notes_are_not(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "diagnostics.json"
            path.write_text("\n".join(json.dumps({"message": {"level": level, "message": level, "rendered": level}})
                for level in ("note", "warning", "error")))
            self.assertEqual(checks.check_diagnostics("//pkg:consumer", path), ["//pkg:consumer: warning", "//pkg:consumer: error"])

    def test_top_level_fatal_and_malformed_diagnostics_cannot_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "diagnostics.json"
            path.write_text(json.dumps({"level": "fatal", "message": "compile refused"}))
            self.assertEqual(checks.check_diagnostics("//pkg:consumer", path), ["//pkg:consumer: compile refused"])
            path.write_text("not diagnostic JSON")
            with self.assertRaises(json.JSONDecodeError):
                checks.check_diagnostics("//pkg:consumer", path)


if __name__ == "__main__":
    unittest.main()
