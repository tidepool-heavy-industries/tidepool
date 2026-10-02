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

    def test_real_text_envelope_decodes_payload_and_filters_worker(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timing"
            payload = "tidepool-timing-detail parent=exact_scope phase=decode start_ns=1 end_ns=2 gcs=0"
            path.write_text("\n".join(
                f'2026-10-02T12:03:13Z DEBUG compile_request{{run_id="run" compile_request=req admission_id=4 '
                f'request_ordinal=1 transaction=true worker_pid={pid} daemon_epoch=epoch}}: '
                f'compiler timing line={json.dumps(payload)}' for pid in [7, 8]))
            rows, invalid = profile.detail_rows(path, 7)
            self.assertEqual(invalid, 0)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["gcs"], "0")
            self.assertNotIn("worker_pid", rows[0])
            self.assertEqual(rows[0]["trace"]["compile_request"], "req")
            self.assertEqual(rows[0]["trace"]["admission_id"], 4)
            self.assertTrue(rows[0]["trace"]["transaction"])

    def test_invalid_numeric_detail_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timing"
            path.write_text('tidepool-timing-detail parent=p phase=x start_ns=1 end_ns=2 gcs=0"\n')
            rows, invalid = profile.detail_rows(path)
            self.assertEqual(rows, [])
            self.assertEqual(invalid, 1)

    def test_phase_leaf_groups_union_intervals_per_invocation(self):
        phases = [{"parent": "p", "phase": "x", "start_ns": str(start), "end_ns": str(end),
                   "trace": {"worker_pid": 7, "compile_request": "req", "admission_id": admission}}
                  for start, end, admission in [(1, 3, 1), (2, 4, 1), (1, 4, 2)]]
        samples = [{"monotonic_ns": stamp, "line": f"7/7 0.00000000{stamp}: 830258 hot+0x1 (/worker)"}
                   for stamp in [1, 2, 3, 4]]
        spans, groups, omissions = profile.phase_attribution(phases, samples)
        self.assertEqual([row["cpu_samples"] for row in spans], [3, 3, 4])
        self.assertEqual([group["cpu_samples"] for group in groups], [4, 4])
        self.assertEqual(groups[0]["completed_spans"], 2)
        self.assertEqual(groups[0]["top_leaf_symbols"][0]["symbol"], "hot")
        self.assertFalse(any(omissions.values()))

    def test_timing_prefix_and_summary_bounds_mark_incomplete(self):
        with tempfile.TemporaryDirectory() as directory:
            source, target = Path(directory) / "source", Path(directory) / "target"
            source.write_bytes(b"x" * 33)
            result = profile.bounded_copy(source, target, limit=32, retain_prefix=True)
            self.assertEqual(target.stat().st_size, 32)
            self.assertTrue(result["truncated"])
            self.assertEqual(result["omitted_bytes"], 1)
            summary = {"phase_samples": [{"large": "x" * 1000}], "phase_leaf_groups": [],
                       "phase_analysis_complete": True}
            profile.write_summary(target, summary, limit=512)
            parsed = json.loads(target.read_text())
            self.assertFalse(parsed["phase_analysis_complete"])
            self.assertEqual(parsed["omitted_summary_spans"], 1)
            self.assertLessEqual(target.stat().st_size, 512)

    def test_offline_recovery_selects_recorded_window_and_worker(self):
        with tempfile.TemporaryDirectory() as directory:
            source, target = Path(directory) / "source", Path(directory) / "target"
            lines = []
            for start, pid, admission in [(1, 7, 1), (10, 7, 2), (10, 8, 3)]:
                lines.append(json.dumps({"fields": {"line": f"tidepool-timing-detail parent=p phase=x start_ns={start} end_ns={start + 1}"},
                                         "span": {"worker_pid": pid, "compile_request": "req", "admission_id": admission}}))
            source.write_text("\n".join(lines) + "\n")
            result = profile.recover_timing_window(source, target, {"pid": 7,
                "sampling_enabled": {"monotonic_ns": 9}, "sampling_end": {"monotonic_ns": 12}})
            rows = profile.timing_rows(target, 7)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["trace"]["admission_id"], 2)
            self.assertFalse(result["scan_truncated"])
            self.assertEqual(result["invalid_source_rows"], 0)

    def test_hash_timestamps_attribute_union_and_leave_old_counts_unassigned(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timing"
            identity = {"worker_pid": 7, "compile_request": "req", "admission_id": 1}
            path.write_text("\n".join(json.dumps({"fields": {"line": line}, "span": identity}) for line in [
                "tidepool-count name=hash_bytes.package_root.abc count=7 count_ns=3",
                "tidepool-count name=hash_bytes.package_root.abc count=7 count_ns=4",
                "tidepool-count name=hash_bytes.package_root.abc count=7",
            ]))
            phases = [{"parent": "p", "phase": "x", "start_ns": str(start), "end_ns": str(end), "trace": identity}
                      for start, end in [(1, 3), (2, 5)]]
            groups, coverage = profile.phase_hash_counts(path, 7, phases)
            counts = groups[0]["hash_byte_counts"]["package_root"]
            self.assertEqual(counts["calls"], 2)
            self.assertEqual(counts["total_bytes"], 14)
            self.assertEqual(counts["unique_bytes"], 7)
            self.assertEqual(coverage["untimestamped_hash_count_rows"], 1)
            self.assertEqual(coverage["unassigned_timestamped_hash_count_rows"], 0)


if __name__ == "__main__":
    unittest.main()
