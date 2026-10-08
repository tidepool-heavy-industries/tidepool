import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "harness-usecase-perf-report.py"
spec = importlib.util.spec_from_file_location("harness_usecase_perf_report", SCRIPT)
reporter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reporter)


class HarnessUsecasePerfReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def write_json(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value) + "\n")

    def write_jsonl(self, path, rows):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("".join(json.dumps(row) + "\n" for row in rows))

    def make_case(self, *, queue=True, submissions=True, service_records=1, cell_span=True):
        artifact_root = self.root / "artifacts"
        host_path = artifact_root / "host.jsonl"
        compiler_path = artifact_root / "compiler/compiler.jsonl"
        phase_path = artifact_root / "phases.jsonl"
        phase_records = [
            {"schema": 1, "phase": "environment", "prepared_root_entry_supplied": False,
             "host_trace": str(host_path), "compiler_trace": str(compiler_path),
             "phase_trace": str(phase_path),
             "deployment": {"TIDEPOOL_COMPILER_DEPLOYMENT": "/test/compiler"}},
            {"schema": 1, "phase": "activation", "completed": True,
             "wall_ns": 9_000, "logical_compiler_requests": 1, "context_items": 1},
            {"schema": 1, "phase": "reuse-retained", "sequence": 3,
             "call_id": "usecase-3", "wall_ns": 20_000, "first_successor_ns": 8_000,
             "logical_compiler_requests": 1, "invocation_execution": "synchronous",
             "completed": True, "source": "value <- perfAction\n_ <- display value"},
        ]
        stdout = self.root / "case.stdout.log"
        stdout.write_text("running 1 test\ntest actor_host::perf ...\n")
        self.write_jsonl(phase_path, phase_records)
        request = {
            "daemon_epoch": "epoch-a", "admission_id": 7,
            "request_ordinal": 2, "compile_request": "digest-a",
        }
        host = [
            {"target": "exomonad_actor::resident_tools", "fields": {
                "message": "workbench cell dispatched to its actor", "context_call_id": "usecase-3",
                "execution": "exec-3"}},
            {"target": "exomonad_actor::call_timing", "fields": {
                "message": "call timing", "execution": "exec-3", "total_ms": "17",
                "checkout_wait_ms": "2", "compile_ms": "9", "exec_ms": "3"}},
        ]
        if submissions:
            host.append({"target": "tidepool_extract_cmd::endpoint", "fields": {
                "message": "compiler request identified", "transport": "daemon", **request},
                "span": ({"name": "cell", "execution": "exec-3"} if cell_span else
                         {"name": "compile_request", "compile_request": "digest-a"})})
        service = {"target": "tidepool_extract_cmd::daemon", "fields": {
            "message": "compiler request finished", "phase": "compiler_service",
            "elapsed_ms": 5, "exit_code": 0},
            "span": {"name": "compile_request", **request}}
        daemon = [service.copy() for _ in range(service_records)]
        if queue:
            daemon.append({"target": "tidepool_extract_cmd::daemon", "fields": {
                "message": "compiler job dequeued", "phase": "compiler_queue",
                "queue_ms": 11, "daemon_epoch": "epoch-a", "admission_id": 7}})
        self.write_jsonl(host_path, host)
        self.write_jsonl(compiler_path, daemon)
        record_path = self.root / "abc.json"
        self.write_json(record_path, {
            "test": "actor_host::production_harness_notebook_usecase_phases",
            "passed": True,
            "streams": {"stdout": {"path": stdout.name, "truncated": False}},
            "execution": {"artifact_root": str(artifact_root), "compiler_mode": "owned-resident",
                          "executed_test_count": 1, "diagnostic_evidence_complete": True,
                          "artifacts_retained_after_success": True},
        })
        return record_path

    def test_joins_phase_to_host_and_physical_service_but_keeps_queue_at_admission_scope(self):
        report = reporter.analyze(self.make_case())
        phase = report["phases"][0]
        self.assertEqual(phase["dispatch_executions"], ["exec-3"])
        self.assertEqual(phase["compiler_attribution"], "complete")
        self.assertEqual(phase["call_timing_records"][0]["checkout_wait_ms"], "2")
        self.assertEqual(phase["client_compiler_submissions"][0]["service_records"][0]["elapsed_ms"], 5)
        self.assertEqual(phase["queue_admission_ids"][0]["admission_id"], 7)
        self.assertNotIn("queue_ms", phase["queue_admission_ids"][0])
        self.assertEqual(report["queue_trace_status"], "records_observed")
        self.assertEqual(report["queue_observations"][0]["related_phases"], ["reuse-retained"])
        self.assertEqual(report["queue_observations"][0]["attribution_scope"],
                         "daemon worker admission; do not add to per-request service")
        self.assertEqual(phase["wall_ns"], 20_000)
        self.assertEqual(phase["provider_model_latency_ms"], None)
        self.assertEqual(phase["store_projection_status"], "not_instrumented")
        self.assertEqual(report["activation"]["wall_ns"], 9_000)
        self.assertEqual(report["phase_coverage"]["expected_count"], 8)
        self.assertEqual(report["phase_coverage"]["completed_unique_phase_count"], 1)
        self.assertEqual(len(report["phase_coverage"]["missing"]), 7)
        self.assertFalse(report["phase_coverage"]["complete"])

    def test_absent_queue_observation_stays_unknown_and_is_not_zero(self):
        report = reporter.analyze(self.make_case(queue=False))
        self.assertEqual(report["queue_observations"], [])
        self.assertEqual(report["queue_trace_status"], "no_records_observed_unknown")
        self.assertTrue(report["timing_contract"]["missing_queue_records_mean_unknown_not_zero"])

    def test_missing_exact_compiler_join_is_incomplete_not_a_zero_compile_claim(self):
        report = reporter.analyze(self.make_case(submissions=False))
        phase = report["phases"][0]
        self.assertEqual(phase["logical_compiler_requests"], 1)
        self.assertEqual(phase["client_compiler_submissions"], [])
        self.assertEqual(phase["compiler_attribution"], "partial")

    def test_missing_or_ambiguous_physical_service_keeps_attribution_partial(self):
        for count, status_key in ((0, "daemon_service_missing_count"),
                                  (2, "daemon_service_ambiguous_count")):
            with self.subTest(service_records=count):
                phase = reporter.analyze(self.make_case(service_records=count))["phases"][0]
                self.assertEqual(phase["compiler_attribution"], "partial")
                self.assertEqual(phase[status_key], 1)

    def test_full_event_join_stays_partial_when_runner_trace_capture_is_incomplete(self):
        record = self.make_case()
        payload = json.loads(record.read_text())
        payload["execution"]["diagnostic_evidence_complete"] = False
        record.write_text(json.dumps(payload))
        phase = reporter.analyze(record)["phases"][0]
        self.assertEqual(phase["host_submission_event_count"], 1)
        self.assertEqual(phase["daemon_service_matched_count"], 1)
        self.assertEqual(phase["compiler_attribution"], "partial")

    def test_real_host_request_shape_without_cell_span_is_unattributed(self):
        report = reporter.analyze(self.make_case(cell_span=False))
        phase = report["phases"][0]
        self.assertEqual(phase["dispatch_executions"], ["exec-3"])
        self.assertEqual(phase["host_submission_event_count"], 0)
        self.assertEqual(phase["compiler_attribution"], "partial")
        self.assertEqual(len(report["unattributed_compiler_submissions"]), 1)
        self.assertEqual(len(report["unattributed_compiler_submissions"][0]["daemon_service_records"]), 1)

    def test_missing_environment_trace_path_is_a_controlled_report_error(self):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        row = rows[0]
        del row["host_trace"]
        rows[0] = row
        self.write_jsonl(phase_path, rows)
        with self.assertRaisesRegex(ValueError, "no host_trace path"):
            reporter.analyze(record)

    def test_missing_phase_trace_path_is_a_controlled_report_error(self):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        del rows[0]["phase_trace"]
        self.write_jsonl(phase_path, rows)
        with self.assertRaisesRegex(ValueError, "no phase_trace path"):
            reporter.analyze(record)

    def test_missing_activation_is_reported_in_coverage(self):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = [row for row in reporter.read_jsonl(phase_path) if row["phase"] != "activation"]
        self.write_jsonl(phase_path, rows)
        report = reporter.analyze(record)
        self.assertIsNone(report["activation"])
        self.assertEqual(report["phase_coverage"]["control_phases"]["missing"], ["activation"])

    def test_phase_coverage_reports_duplicates_and_wrong_sequence(self):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        phase_index = next(index for index, row in enumerate(rows)
                           if row["phase"] == "reuse-retained")
        duplicate = rows[phase_index]
        duplicate["sequence"] = 4
        rows[phase_index] = duplicate
        rows.append(duplicate)
        self.write_jsonl(phase_path, rows)
        coverage = reporter.analyze(record)["phase_coverage"]
        self.assertEqual(coverage["duplicates"], ["reuse-retained"])
        self.assertEqual(coverage["sequence_errors"], [
            {"phase": "reuse-retained", "observed": 4, "expected": 3},
            {"phase": "reuse-retained", "observed": 4, "expected": 3},
        ])
        self.assertFalse(coverage["complete"])

    def test_runner_stdout_prefix_and_truncation_do_not_affect_phase_jsonl(self):
        record = self.make_case()
        payload = json.loads(record.read_text())
        payload["streams"]["stdout"]["truncated"] = True
        record.write_text(json.dumps(payload))
        report = reporter.analyze(record)
        self.assertEqual(report["phase_coverage"]["observed_workload_record_count"], 1)
        self.assertTrue(report["runner"]["phase_trace"].endswith("/phases.jsonl"))

    def test_retained_producer_shapes_do_not_invent_execution_correlation(self):
        # Sanitized event shapes from the retained process-cleanup host JSONL
        # lines 190/205 documented in the private evidence README, plus the
        # owned-daemon compiler JSONL producer. The host request has identity,
        # but its persisted span list has no cell execution.
        dispatch = {"target": "exomonad_actor::resident_tools", "fields": {
            "message": "workbench cell dispatched to its actor", "context_call_id": "call-a",
            "execution": "exec-a"}}
        submission = {"target": "tidepool_extract_cmd::endpoint", "fields": {
            "message": "compiler request identified", "transport": "daemon",
            "daemon_epoch": "epoch-a", "admission_id": 6, "request_ordinal": 1,
            "compile_request": "digest-a"}, "span": {"name": "compile_request",
            "compile_request": "digest-a"}, "spans": [
                {"name": "Actor"}, {"name": "actor", "message": "Workbench"},
                {"name": "compile_request", "transport": "transaction"},
                {"name": "compile_request", "transport": "daemon"}]}
        physical = {"fields": {"phase": "compiler_service", "elapsed_ms": 26,
                               "message": "compiler request finished"},
                    "span": {"name": "compile_request", "compile_request": "digest-a",
                             "physical_execution": "epoch-a:6:1"}}
        dispatches, _, submissions, services, _ = reporter.compiler_events(
            [dispatch, submission], [physical])
        self.assertEqual(dispatches["call-a"], {"exec-a"})
        self.assertEqual(submissions[0]["identity"], ("epoch-a", 6, 1, "digest-a"))
        self.assertEqual(submissions[0]["executions"], [])
        self.assertEqual(services[0]["identity"], submissions[0]["identity"])


if __name__ == "__main__":
    unittest.main()
