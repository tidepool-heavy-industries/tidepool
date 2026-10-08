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

    def make_case(self, *, queue=True, submissions=True):
        artifact_root = self.root / "artifacts"
        host_path = artifact_root / "host.jsonl"
        compiler_path = artifact_root / "compiler/compiler.jsonl"
        phase_records = [
            {"schema": 1, "phase": "environment", "prepared_root_entry_supplied": False,
             "host_trace": str(host_path), "compiler_trace": str(compiler_path),
             "deployment": {"TIDEPOOL_COMPILER_DEPLOYMENT": "/test/compiler"}},
            {"schema": 1, "phase": "reuse-retained", "sequence": 3,
             "call_id": "usecase-3", "wall_ns": 20_000, "first_successor_ns": 8_000,
             "logical_compiler_requests": 1, "invocation_execution": "synchronous",
             "source": "value <- perfAction\n_ <- display value"},
        ]
        stdout = self.root / "case.stdout.log"
        stdout.write_text("".join("harness-usecase " + json.dumps(row) + "\n"
                                  for row in phase_records))
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
                "span": {"name": "cell", "execution": "exec-3"}})
        daemon = [{"target": "tidepool_extract_cmd::daemon", "fields": {
            "message": "compiler request finished", "phase": "compiler_service",
            "elapsed_ms": 5, "exit_code": 0},
            "span": {"name": "compile_request", **request}}]
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
        self.assertEqual(phase["compiler_attribution"], "incomplete_submission_trace")

    def test_truncated_phase_log_is_rejected(self):
        record = self.make_case()
        payload = json.loads(record.read_text())
        payload["streams"]["stdout"]["truncated"] = True
        record.write_text(json.dumps(payload))
        with self.assertRaisesRegex(ValueError, "truncated"):
            reporter.analyze(record)


if __name__ == "__main__":
    unittest.main()
