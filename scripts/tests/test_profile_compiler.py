import importlib.util
from pathlib import Path
import tempfile
import json
import shutil
import sys
import subprocess
import unittest

spec = importlib.util.spec_from_file_location("profile_compiler", Path(__file__).parents[1] / "profile-compiler.py")
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


class ProfileCompilerTests(unittest.TestCase):
    def test_early_sampling_exit_does_not_cancel_admitted_workload(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            perf = base / "perf"
            perf.write_text(f"#!{sys.executable}\n" + '''
import json, os, sys, time
from pathlib import Path
args = sys.argv[1:]
if args[0] == "version":
    print("test perf")
elif args[0] == "record":
    control = args[args.index("--control") + 1]
    read_fd, ack_fd = map(int, control.removeprefix("fd:").split(","))
    os.read(read_fd, 128)
    os.write(ack_fd, b"ack\\n")
    output = Path(args[args.index("-o") + 1])
    output.write_text(json.dumps({"pid": args[args.index("-p") + 1],
                                  "time": time.monotonic()}))
    time.sleep(.03)
elif args[0] == "script":
    data = json.loads(Path(args[args.index("-i") + 1]).read_text())
    print(f"{data['pid']}/{data['pid']} {data['time']:.9f}: 100 leaf (/test-worker)")
elif args[0] == "report":
    print("test leaf report")
''')
            perf.chmod(0o700)
            marker = base / "settled"
            worker = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
            try:
                outcome = subprocess.run([
                    sys.executable, str(Path(profile.__file__)), "--pid", str(worker.pid),
                    "--perf", str(perf), "--output", str(base / "capture"), "--duration", "5",
                    "--", sys.executable, "-c",
                    "import sys,time; from pathlib import Path; time.sleep(.2); Path(sys.argv[1]).write_text('settled')",
                    str(marker),
                ], capture_output=True, text=True, timeout=15)
                self.assertEqual(outcome.returncode, 1, outcome.stderr)
                self.assertTrue(marker.exists(), outcome.stderr)
                metadata = json.loads((base / "capture/capture.json").read_text())
                summary = json.loads((base / "capture/summary.json").read_text())
                self.assertEqual(metadata["command_exit"], 0)
                self.assertTrue(metadata["sampling_ended_before_workload"])
                self.assertTrue(metadata["capture_error"])
                self.assertFalse(summary["capture_complete"])
                self.assertTrue(summary["workload_success"])
            finally:
                worker.terminate()
                worker.wait(timeout=5)

    def make_cgroup_tree(self, directory, worker_path="/parent/worker"):
        proc_root = Path(directory) / "proc"
        cgroup_root = Path(directory) / "sys" / "fs" / "cgroup"
        worker = cgroup_root / worker_path.lstrip("/")
        worker.mkdir(parents=True)
        (proc_root / "self").mkdir(parents=True)
        (proc_root / "123").mkdir()
        (proc_root / "self" / "mountinfo").write_text(
            f"29 20 0:30 / {cgroup_root} rw,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw\n")
        (proc_root / "123" / "cgroup").write_text(f"0::{worker_path}\n")
        (cgroup_root / "cpu.stat").write_text("usage_usec 10\nuser_usec 7\nsystem_usec 3\n")
        (cgroup_root / "memory.events").write_text("low 0\nhigh 1\nmax 2\n")
        (cgroup_root / "memory.current").write_text("4096\n")
        (cgroup_root / "memory.max").write_text("max\n")
        (cgroup_root / "memory.high").write_text("8192\n")
        (cgroup_root / "memory.pressure").write_text("some avg10=0.00 avg60=0.10 avg300=0.20 total=100\n")
        (cgroup_root / "cpu.pressure").write_text("some avg10=1.00 avg60=1.00 avg300=1.00 total=200\n")
        (cgroup_root / "io.pressure").write_text("some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n")
        (worker / "cpu.stat").write_text("usage_usec 5\n")
        return proc_root, cgroup_root, worker

    def test_cgroup_context_reports_ancestor_deltas_and_shared_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            proc_root, cgroup_root, worker = self.make_cgroup_tree(directory)
            start = profile.capture_cgroup_context(123, proc_root)
            self.assertEqual(start["status"], "ok")
            self.assertEqual(start["worker_path"], "/parent/worker")
            (cgroup_root / "cpu.stat").write_text("usage_usec 25\nuser_usec 15\nsystem_usec 10\n")
            (cgroup_root / "memory.events").write_text("low 1\nhigh 3\nmax 2\n")
            (cgroup_root / "memory.current").write_text("6144\n")
            (cgroup_root / "memory.pressure").write_text("some avg10=0.50 avg60=0.10 avg300=0.20 total=150\n")
            end = profile.capture_cgroup_context(123, proc_root)
            compared = profile.compare_cgroup_context(start, end)
            self.assertEqual(compared["status"], "ok")
            self.assertEqual(compared["deltas"]["."]["cpu.stat"]["counters"]["usage_usec"]["value"], 15)
            self.assertEqual(compared["deltas"]["."]["memory.events"]["counters"]["high"]["value"], 2)
            self.assertEqual(compared["deltas"]["."]["memory.current"]["end"]["value"], 6144)
            self.assertEqual(compared["deltas"]["."]["memory.pressure"]["counters"]["some"]["total"]["value"], 50)
            self.assertIn("shared", compared["scope"])
            self.assertEqual(start["ancestors"][0]["path"], "parent/worker")

    def test_cgroup_context_marks_unreadable_and_missing_files(self):
        with tempfile.TemporaryDirectory() as directory:
            proc_root, _, worker = self.make_cgroup_tree(directory)
            (worker / "memory.events").mkdir()
            snapshot = profile.capture_cgroup_context(123, proc_root)
            files = snapshot["ancestors"][0]["files"]
            self.assertEqual(files["memory.events"]["status"], "unreadable")
            self.assertEqual(files["memory.high"]["status"], "missing")

    def test_cgroup_directory_disappearance_is_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            proc_root, _, worker = self.make_cgroup_tree(directory)
            start = profile.capture_cgroup_context(123, proc_root)
            shutil.rmtree(worker)
            end = profile.capture_cgroup_context(123, proc_root)
            compared = profile.compare_cgroup_context(start, end)
            self.assertEqual(end["ancestors"][0]["directory_status"], "missing")
            self.assertEqual(compared["deltas"]["parent/worker"]["cpu.stat"]["end_directory_status"], "missing")

    def test_cgroup_counter_reset_and_migration_are_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            proc_root, cgroup_root, _ = self.make_cgroup_tree(directory)
            start = profile.capture_cgroup_context(123, proc_root)
            (cgroup_root / "cpu.stat").write_text("usage_usec 2\n")
            end = profile.capture_cgroup_context(123, proc_root)
            delta = profile.compare_cgroup_context(start, end)
            self.assertEqual(delta["deltas"]["."]["cpu.stat"]["counters"]["usage_usec"],
                             {"status": "reset", "start": 10, "end": 2})
            (proc_root / "123" / "cgroup").write_text("0::/parent/other\n")
            moved = profile.capture_cgroup_context(123, proc_root)
            comparison = profile.compare_cgroup_context(start, moved)
            self.assertEqual(comparison["status"], "migrated")
            self.assertEqual(comparison["deltas"], {})

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


class RequestAccountingTests(unittest.TestCase):
    def span(self, ordinal=1, worker=7):
        return {"daemon_epoch": "epoch", "worker_pid": worker, "admission_id": 4,
                "request_ordinal": ordinal, "compile_request": "same-input", "transaction": True}

    def event(self, decision="work", cycle=1, stage="source_frontend", **changes):
        terminal = decision == "complete"
        return {"schema": 1, "cycle": cycle, "purpose": "cell_program", "observed_ns": 1,
                "stage": stage, "decision": decision, "reason": "stage_complete" if terminal else "absent",
                "unit": None if terminal else "main", "module": None if terminal else "Support",
                "version_kind": None if terminal else "source_fingerprint",
                "version": None if terminal else "abc", "items": 0 if terminal else 1,
                "bytes": None, **changes}

    def history(self, packets, ordinal=1, terminal="compiler request finished"):
        span = self.span(ordinal)
        rows = [{"fields": {"message": "compiler request started"}, "span": span}]
        rows += [{"fields": {"line": "tidepool-reuse " + json.dumps(packet)}, "span": span} for packet in packets]
        if terminal:
            rows.append({"fields": {"message": terminal, "exit_code": 0, "elapsed_ms": 5}, "span": span})
        return rows

    def analyze(self, rows, suffix="", rss=None, log_complete=True):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timing.log"
            path.write_text("\n".join(json.dumps(row) for row in rows) + "\n" + suffix)
            return profile.request_accounting(path, 7, rss or [], log_complete)

    def test_missing_completion_and_cancelled_request_are_not_zero(self):
        missing = self.analyze(self.history([self.event()]))
        request = missing["requests"][0]
        self.assertEqual(request["stages"]["source_frontend"]["status"], "UNKNOWN")
        self.assertIsNone(request["source_frontend_work_items"])
        cancelled = self.analyze(self.history([self.event(), self.event("complete")],
                                             terminal="compiler request abandoned by client"))
        self.assertEqual(cancelled["requests"][0]["status"], "UNKNOWN")
        self.assertIsNone(cancelled["requests"][0]["source_frontend_work_items"])
        self.assertIn("Actually linked bytecode: UNKNOWN", profile.render_request_accounting(cancelled))

    def test_truncated_envelope_and_absent_terminal_are_incomplete(self):
        report = self.analyze(self.history([self.event(), self.event("complete")], terminal=None),
                              suffix='{"fields":')
        self.assertEqual(report["status"], "incomplete")
        self.assertEqual(report["requests"][0]["status"], "UNKNOWN")
        self.assertTrue(any("invalid trace" in problem for problem in report["problems"]))

    def test_duplicate_stage_and_request_completion_are_refused(self):
        stage = self.analyze(self.history([self.event(), self.event("complete"), self.event("complete")]))
        self.assertEqual(stage["requests"][0]["cycle_stages"][0]["stages"]["source_frontend"]["status"], "UNKNOWN")
        rows = self.history([self.event(), self.event("complete")])
        report = self.analyze(rows + [rows[-1]])
        self.assertEqual(report["requests"][0]["status"], "UNKNOWN")

    def test_interleaved_same_digest_requests_and_cycles_stay_separate(self):
        first = self.history([self.event(cycle=1), self.event("complete", cycle=1)])
        second = self.history([self.event(cycle=2), self.event("complete", cycle=2)], ordinal=2)
        rows = [first[0], second[0], first[1], second[1], first[2], second[2], second[3], first[3]]
        report = self.analyze(rows)
        self.assertEqual(report["status"], "observed")
        self.assertEqual([request["identity"]["request_ordinal"] for request in report["requests"]], [1, 2])
        self.assertEqual([request["source_frontend_work_items"] for request in report["requests"]], [1, 1])
        self.assertEqual([request["cycle_stages"][0]["cycle"] for request in report["requests"]], [1, 2])

    def test_successful_output_without_decisions_does_not_prove_reuse(self):
        report = self.analyze(self.history([]))
        self.assertEqual(report["requests"][0]["status"], "UNKNOWN")
        self.assertTrue(all(stage["status"] == "UNKNOWN" for stage in report["requests"][0]["stages"].values()))
        self.assertIsNone(report["requests"][0]["bytecode"]["reconstructed"])

    def test_resources_remain_nested_and_rss_is_scoped_to_observed_windows(self):
        rows = self.history([self.event(), self.event("complete")])
        lines = ["tidepool-timing-detail parent=compile phase=ghc_load start_ns=10 end_ns=30 wall_ns=20 allocated_bytes=100 gcs=2",
                 "tidepool-timing-detail parent=ghc_load phase=decode start_ns=15 end_ns=20 wall_ns=5 allocated_bytes=70 gcs=1",
                 "tidepool-count name=exact_execution_original_load_owners count=3",
                 "tidepool-timing phase=retained_finalized_bytecode ms=2"]
        rows[-1:-1] = [{"fields": {"line": line}, "span": self.span()} for line in lines]
        report = self.analyze(rows, rss=[{"monotonic_ns": 9, "VmRSS": 999}, {"monotonic_ns": 16, "VmRSS": 25}])
        request = report["requests"][0]
        self.assertEqual([span["allocated_bytes"] for span in request["resource_spans"]], [100, 70])
        self.assertEqual(request["sampled_rss"]["peak_bytes"], 25)
        self.assertEqual(request["sampled_rss"]["sample_count"], 1)
        self.assertEqual(request["bytecode"]["selected"], {"exact_execution_original_load_owners": 3})
        self.assertEqual(request["bytecode"]["reconstructed"]["retained_finalized_bytecode"]["completed_operations"], 1)
        self.assertIsNone(request["bytecode"]["actually_linked"])
        self.assertNotIn("allocated_bytes_total", request)

    def test_offline_recovery_retains_request_boundaries_and_reuse(self):
        with tempfile.TemporaryDirectory() as directory:
            source, target = Path(directory) / "source", Path(directory) / "target"
            rows = self.history([self.event(), self.event("complete")])
            rows.insert(1, {"fields": {"line": "tidepool-timing-detail parent=compile phase=ghc_load start_ns=10 end_ns=20"}, "span": self.span()})
            source.write_text("\n".join(json.dumps(row) for row in rows) + "\n")
            profile.recover_timing_window(source, target, {"pid": 7,
                "sampling_enabled": {"monotonic_ns": 9}, "sampling_end": {"monotonic_ns": 21}})
            report = profile.request_accounting(target, 7, [])
            self.assertEqual(report["status"], "observed")
            self.assertEqual(report["requests"][0]["source_frontend_work_items"], 1)
            self.assertEqual(len(target.read_text().splitlines()), len(rows))

    def test_text_envelopes_and_nested_json_spans_retain_physical_identity(self):
        rows = self.history([self.event(), self.event("complete")])
        for row in rows:
            row["spans"] = [row.pop("span")]
        report = self.analyze(rows)
        self.assertEqual(report["requests"][0]["source_frontend_work_items"], 1)
        envelope = 'compile_request{daemon_epoch=epoch worker_pid=7 admission_id=4 request_ordinal=1 compile_request=same-input transaction=true}: '
        lines = [envelope + "compiler request started",
                 envelope + "compiler timing line=" + json.dumps("tidepool-reuse " + json.dumps(self.event())),
                 envelope + "compiler timing line=" + json.dumps("tidepool-reuse " + json.dumps(self.event("complete"))),
                 envelope + "compiler request finished elapsed_ms=5 exit_code=0"]
        parsed = profile.REUSE_REPORT.analyze(list(profile.trace_rows_from_lines(lines, 7)))
        self.assertEqual(parsed["status"], "observed")

    def test_capture_overflow_is_separate_from_completed_request(self):
        report = self.analyze(self.history([self.event(), self.event("complete")]), log_complete=False)
        self.assertEqual(report["status"], "incomplete")
        self.assertEqual(report["requests"][0]["status"], "observed")
        self.assertTrue(any("capture/recovery incomplete" in problem for problem in report["problems"]))

    def test_selected_activation_and_reconstruction_totals_require_enclosing_boundaries(self):
        lines = ["tidepool-count name=activation_preview_frontends count=9",
                 "tidepool-count name=exact_execution_original_load_owners count=9",
                 "tidepool-timing phase=retained_finalized_bytecode ms=2"]
        for placement in ("before", "after", "missing_terminal", "duplicate_terminal"):
            with self.subTest(placement=placement):
                rows = self.history([self.event(), self.event("complete")])
                diagnostics = [{"fields": {"line": line}, "span": self.span()} for line in lines]
                if placement == "before":
                    rows[0:0] = diagnostics
                elif placement == "after":
                    rows.extend(diagnostics)
                else:
                    rows[1:1] = diagnostics
                    if placement == "missing_terminal":
                        rows.pop()
                    else:
                        rows.append(rows[-1])
                report = self.analyze(rows)
                request = report["requests"][0]
                self.assertEqual(report["status"], "incomplete")
                self.assertEqual(request["legacy_status"], "UNKNOWN")
                self.assertIsNone(request["activation_preview_frontends"])
                self.assertIsNone(request["bytecode"]["selected"])
                self.assertIsNone(request["bytecode"]["reconstructed"])

    def test_valid_interleaved_legacy_totals_remain_per_physical_request(self):
        first = self.history([self.event(cycle=1), self.event("complete", cycle=1)])
        second = self.history([self.event(cycle=2), self.event("complete", cycle=2)], ordinal=2)
        one = {"fields": {"line": "tidepool-count name=activation_preview_frontends count=1"}, "span": self.span()}
        two = {"fields": {"line": "tidepool-count name=activation_preview_frontends count=2"}, "span": self.span(2)}
        report = self.analyze([first[0], second[0], first[1], second[1], one, two,
                               first[2], second[2], first[3], second[3]])
        self.assertEqual(report["status"], "observed")
        self.assertEqual([request["activation_preview_frontends"] for request in report["requests"]], [1, 2])

    def test_unavailable_rts_preserves_wall_cpu_and_nullable_counter_observations(self):
        rows = self.history([self.event(), self.event("complete")])
        detail = ("tidepool-timing-detail parent=compile phase=ghc_load start_ns=10 end_ns=20 "
                  "wall_ns=10 cpu_ns=7 rts=unavailable rts_scope=process_delta "
                  "allocated_bytes=unavailable gc_cpu_ns=unavailable gc_elapsed_ns=unavailable gcs=unavailable "
                  "major_gcs=unavailable minor_gcs=unavailable last_gc_epoch_before=unavailable "
                  "last_gc_live_bytes_after=unavailable process_highwater_major_gc_live_bytes=unavailable")
        rows.insert(1, {"fields": {"line": detail}, "span": self.span()})
        report = self.analyze(rows)
        self.assertEqual(report["status"], "observed")
        resource_span = report["requests"][0]["resource_spans"][0]
        self.assertEqual(resource_span["wall_ns"], 10)
        self.assertEqual(resource_span["cpu_ns"], 7)
        self.assertEqual(resource_span["rts"], "unavailable")
        for name in ("allocated_bytes", "gc_cpu_ns", "gc_elapsed_ns", "gcs", "major_gcs", "minor_gcs"):
            self.assertIsNone(resource_span[name])
        human = profile.render_request_accounting(report)
        self.assertIn("wall_ns=10 cpu_ns=7 allocated_bytes=UNKNOWN gcs=UNKNOWN", human)
        for required_field in ("start_ns", "end_ns", "wall_ns", "cpu_ns"):
            with self.subTest(required_field=required_field):
                invalid = detail.replace(f"{required_field}={resource_span[required_field]}", f"{required_field}=unavailable")
                bad = self.history([self.event(), self.event("complete")])
                bad.insert(1, {"fields": {"line": invalid}, "span": self.span()})
                rejected = self.analyze(bad)
                self.assertEqual(rejected["status"], "incomplete")
                self.assertEqual(rejected["requests"][0]["resource_spans"], [])

    def test_invalid_or_unenclosed_resource_span_does_not_become_request_measurement(self):
        for line in (
            "tidepool-timing-detail parent=compile phase=ghc_load start_ns=10 end_ns=20 wall_ns=99",
            "tidepool-timing-detail parent=compile phase=ghc_load start_ns=10 end_ns=20 wall_ns=10"):
            rows = self.history([self.event(), self.event("complete")])
            rows.append({"fields": {"line": line}, "span": self.span()})
            report = self.analyze(rows)
            self.assertEqual(report["status"], "incomplete")
            self.assertEqual(report["requests"][0]["resource_spans"], [])
            self.assertIsNone(report["requests"][0]["sampled_rss"]["peak_bytes"])

    def test_unknown_bytecode_and_resource_fields_remain_explicit_in_human_report(self):
        report = self.analyze(self.history([self.event(), self.event("complete")]))
        human = profile.render_request_accounting(report)
        self.assertIn("Executable selection observations: UNKNOWN", human)
        self.assertIn("Bytecode reconstruction observations: UNKNOWN", human)
        self.assertIn("RTS allocation/GC and resource spans: UNKNOWN", human)
        self.assertIn("peak_bytes=UNKNOWN", human)

    def test_request_detail_overflow_preserves_an_incomplete_bounded_summary(self):
        report = self.analyze(self.history([self.event(), self.event("complete")]))
        report["requests"][0]["resource_spans"] = [{"large": "x" * 30000}]
        summary = {"compile_reuse": report, "phase_samples": [], "phase_leaf_groups": [], "phase_analysis_complete": True}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "summary"
            profile.write_summary(path, summary, limit=20000)
            retained = json.loads(path.read_text())
            self.assertEqual(retained["compile_reuse"]["status"], "incomplete")
            self.assertEqual(retained["compile_reuse"]["requests"][0]["omitted_resource_spans"], 1)
            self.assertFalse(retained["phase_analysis_complete"])
            self.assertIn("request detail omitted", profile.render_request_accounting(retained["compile_reuse"]))


if __name__ == "__main__":
    unittest.main()
