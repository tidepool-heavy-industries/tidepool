import importlib.util
from pathlib import Path
import tempfile
import json
import sys
import unittest

spec = importlib.util.spec_from_file_location("profile_compiler", Path(__file__).parents[1] / "profile-compiler.py")
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


class ProfileCompilerTests(unittest.TestCase):
    def test_perf_headers_select_one_leaf_per_event(self):
        rows, lost = profile.parse_samples([
            "2188542/2188542 277584.749911562: ",
            "\t 7d376afdeb31 [unknown] (/nix/store/sha.so)",
            "\t 7d376afdf839 hash+0x1d9 (/nix/store/sha.so)",
            "2188542/2188542 277587.271918601: 830258 eq_entry+0x48 (/worker)",
            "281110.100000000: 830258 entry+0x1 (/worker)",
        ])
        self.assertEqual(len(rows), 3)
        self.assertEqual(rows[0]["monotonic_ns"], 277584749911562)
        self.assertIn("[unknown]", rows[0]["line"])
        self.assertIn("eq_entry", rows[1]["line"])
        self.assertEqual(lost, 0)

    def test_unknown_loss_is_not_zero(self):
        self.assertEqual(profile.parse_samples(["PERF_RECORD_LOST lost 7"])[1], 7)
        self.assertIsNone(profile.parse_samples(["PERF_RECORD_LOST unexpected grammar"])[1])

    def test_completed_command_output_is_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "output"
            path.write_bytes(b"x" * (profile.LIMIT + 1))
            report = profile.cap_output(path)
            self.assertEqual(path.stat().st_size, profile.LIMIT)
            self.assertEqual(report["original_bytes"], profile.LIMIT + 1)
            self.assertIsNone(profile.cap_output(path))

    def test_derived_output_limit_reports_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            output, errors = Path(directory) / "output", Path(directory) / "errors"
            result = profile.run_bounded_output([sys.executable, "-c", "print('x' * 1000)"], output, errors, limit=32)
            self.assertNotEqual(result, 0)
            self.assertEqual(output.stat().st_size, 32)

    def test_hash_bytes_separate_total_and_unique_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timing"
            path.write_text("tidepool-count name=hash_bytes.package_root.abc count=7\n" * 2 +
                            "tidepool-count name=hash_bytes.package_root.def count=5\n")
            counters = profile.hash_byte_counts(path)["package_root"]
            self.assertEqual(counters["calls"], 3)
            self.assertEqual(counters["total_bytes"], 19)
            self.assertEqual(counters["unique_bytes"], 12)
            self.assertFalse(counters["size_conflict"])

    def test_trace_worker_filter_preserves_request_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timing"
            line = "tidepool-timing-detail parent=exact_scope phase=decode start_ns=1 end_ns=2"
            path.write_text("\n".join(json.dumps({"fields": {"line": line, "compile_request": request},
                                                "span": {"worker_pid": pid, "daemon_epoch": "epoch"}})
                                     for pid, request in [(7, "selected"), (8, "other")]))
            rows = profile.timing_rows(path, worker_pid=7)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["trace"]["compile_request"], "selected")
            self.assertEqual(rows[0]["trace"]["worker_identity"], "trace_pid")

    def test_nested_timing_json_uses_only_completed_spans(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timing"
            path.write_text('{"fields":{"line":"tidepool-timing-detail parent=exact_scope phase=decode start_ns=1 end_ns=2 allocated_bytes=3"}}\n' +
                            'tidepool-timing-detail parent=exact_scope phase=decode start_ns=3\n')
            rows = profile.timing_rows(path)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["allocated_bytes"], "3")


if __name__ == "__main__":
    unittest.main()
