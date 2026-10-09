import importlib.util
import itertools
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
            {"schema": 1, "phase": "environment", "workload_cohort": "harness-eight-phase",
             "workload_roster": list(reporter.EXPECTED_PHASES),
             "prepared_root_entry_supplied": False,
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
                "queue_ms": 11, "daemon_epoch": "epoch-a", "admission_id": 7,
                "compiler_workload": "foreground", "compiler_jobs": 2, "compiler_capabilities": 2}})
        self.write_jsonl(host_path, host)
        self.write_jsonl(compiler_path, daemon)
        record_path = self.root / "abc.json"
        self.write_json(record_path, {
            "test": "actor_host::context_transaction_acceptance_tests::harness_usecase_performance::production_harness_notebook_usecase_phases",
            "passed": True,
            "streams": {"stdout": {"path": stdout.name, "truncated": False}},
            "execution": {"artifact_root": str(artifact_root), "compiler_mode": "owned-resident",
                          "executed_test_count": 1, "diagnostic_evidence_complete": True,
                          "artifacts_retained_after_success": True,
                          "diagnostic_summaries": {
                              "compiler_trace_scan": {"complete": True},
                              "physical_compiler_timing": {"request_count_complete": True,
                                                            "complete": False, "records_truncated": True},
                              "compiler_job_queue": {"physical_job_count_complete": True},
                          }},
        })
        return record_path

    def test_report_preserves_requested_width_and_capacity_capped_admission(self):
        path = self.make_case()
        record = reporter.read_json(path)
        record['execution']['compiler_allowances'] = {'requested_foreground_jobs': 16,
                                                     'requested_preparation_jobs': 8,
                                                     'worker_processes': 1}
        self.write_json(path, record)
        trace = self.root / 'artifacts/compiler/compiler.jsonl'
        rows = reporter.read_jsonl(trace)
        rows[-1]['fields'].update(compiler_workload='foreground', compiler_jobs=4, compiler_capabilities=3)
        self.write_jsonl(trace, rows)
        report = reporter.analyze(path)
        self.assertEqual(report['compiler_allowances'], record['execution']['compiler_allowances'])
        grants = report['compiler_job_grants']
        self.assertEqual(grants['status'], 'observed')
        self.assertEqual(grants['observed_jobs'], [4])
        self.assertEqual(grants['observed_capabilities'], [3])
        self.assertEqual(grants['admissions'][0]['requested_jobs'], 16)
        self.assertTrue(grants['admissions'][0]['below_requested_maximum'])
        rows[-1]['fields'].pop('compiler_jobs')
        self.write_jsonl(trace, rows)
        report = reporter.analyze(path)
        self.assertEqual(report['compiler_job_grants']['status'], 'partial_or_unknown')
        self.assertIsNone(report['compiler_job_grants']['admissions'][0]['jobs'])

    def make_complete_case(self, cohort="harness-eight-phase"):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        environment, activation = rows[:2]
        environment.update(workload_cohort=cohort, workload_roster=list(reporter.KNOWN_ROSTERS[cohort]),
                           prepared_root_entry_supplied=True)
        environment["deployment"].update({
            "TIDEPOOL_PREPARED_ROOT_ENTRY": "/frozen/root-entry",
            "TIDEPOOL_COMPILER_MODULES": "/frozen/catalog/catalog.json",
            "TIDEPOOL_COMPILER_DEPLOYMENT": "/frozen/compiler/compiler-deployment.json",
        })
        activation["logical_compiler_requests"] = 0
        if cohort == "three-actor-capture":
            activation["sequence"] = 0
            activation["workspace_preparation_ns"] = 11_000
            activation["host_start_readiness_ns"] = 7_000
        workload = [
            {"schema": 1, "phase": name, "sequence": index, "completed": True,
             "call_id": "usecase-3" if name in ("reuse-retained", "root-fork-capture")
                        else f"case-{index}",
             "logical_compiler_requests": 1 if name in ("reuse-retained", "root-fork-capture") else 0,
             "wall_ns": 1000, "source": "retained source",
             "evidence": ({"cell_role": "first"} if name == "root-setup" else
                          {"cell_role": "repeated"} if name == "parent-publication-read" else {}),
             **({"scripted_response_hold_ns": 250}
                if cohort == "three-actor-capture" and name in reporter.PROVIDER_RESPONSE_PHASES else {})}
            for index, name in enumerate(reporter.KNOWN_ROSTERS[cohort]) if name != "activation"
        ]
        self.write_jsonl(phase_path, [environment, activation, *workload])
        payload = json.loads(record.read_text())
        payload["execution"].update({
            "exit_code": 0, "process_cleanup_status": "confirmed",
            "hosted_cleanup_status": "confirmed", "compiler_cleanup_status": "confirmed",
            "compiler_cleanup_observation_complete": True,
        })
        if cohort == "three-actor-capture":
            payload["test"] = "actor_host::scripted_three_actor_performance::production_harness_three_actor_capture_phases"
        self.write_json(record, payload)
        return record

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
        self.assertEqual(phase["store_projection_status"], "not_observed_unknown")
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

    def test_queue_rows_without_exact_admission_identity_keep_queue_evidence_unknown(self):
        record = self.make_case()
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        rows = reporter.read_jsonl(compiler_path)
        rows.append({"fields": {"phase": "compiler_queue", "message": "compiler job dequeued",
                                "queue_ms": 0}})
        self.write_jsonl(compiler_path, rows)
        report = reporter.analyze(record)
        self.assertEqual(report["queue_evidence"]["status"], "partial_or_unknown")
        self.assertEqual(report["queue_evidence"]["unidentified_queue_event_count"], 1)

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

    def test_raw_exact_joins_remain_complete_when_sampled_diagnostics_are_incomplete(self):
        record = self.make_case()
        payload = json.loads(record.read_text())
        payload["execution"]["diagnostic_evidence_complete"] = False
        record.write_text(json.dumps(payload))
        report = reporter.analyze(record)
        phase = report["phases"][0]
        self.assertEqual(phase["host_submission_event_count"], 1)
        self.assertEqual(phase["daemon_service_matched_count"], 1)
        self.assertEqual(phase["compiler_attribution"], "complete")
        stream = report["whole_physical_stream_reconciliation"]
        self.assertEqual(stream["raw_event_capture_status"], "complete")
        self.assertEqual(stream["detailed_compiler_sample_status"], "truncated_or_unknown")
        self.assertTrue(stream["detailed_compiler_sample_records_truncated"])

    def test_raw_trace_scan_remains_unknown_when_only_parseable_prefix_is_proven(self):
        record = self.make_case()
        payload = json.loads(record.read_text())
        payload["execution"]["diagnostic_summaries"]["compiler_trace_scan"]["complete"] = False
        record.write_text(json.dumps(payload))
        report = reporter.analyze(record)
        self.assertEqual(report["whole_physical_stream_reconciliation"]["status"], "complete")
        self.assertEqual(report["whole_physical_stream_reconciliation"]["raw_event_capture_status"],
                         "unknown_or_partial")

    def test_full_stream_and_workload_status_do_not_claim_startup_owner_from_counts(self):
        record = self.make_case()
        artifact_root = self.root / "artifacts"
        host_path = artifact_root / "host.jsonl"
        compiler_path = artifact_root / "compiler/compiler.jsonl"
        phase_path = artifact_root / "phases.jsonl"
        phase_rows = reporter.read_jsonl(phase_path)
        phase_rows[0]["workload_cohort"] = "test-roster"
        phase_rows[0]["workload_roster"] = ["reuse-retained"]
        phase_rows[2]["sequence"] = 0
        self.write_jsonl(phase_path, phase_rows)
        host = reporter.read_jsonl(host_path)
        service = reporter.read_jsonl(compiler_path)[0]
        request = {"daemon_epoch": "epoch-a", "admission_id": 8,
                   "request_ordinal": 1, "compile_request": "startup-digest"}
        host.append({"target": "tidepool_extract_cmd::endpoint", "fields": {
            "message": "compiler request identified", "transport": "daemon", **request},
            "span": {"name": "compile_request", "compile_request": "startup-digest"}})
        startup_service = json.loads(json.dumps(service))
        startup_service["span"].update(request)
        queue = {"target": "tidepool_extract_cmd::daemon", "fields": {
            "message": "compiler job dequeued", "phase": "compiler_queue",
            "queue_ms": 0, "daemon_epoch": "epoch-a", "admission_id": 8}}
        self.write_jsonl(host_path, host)
        self.write_jsonl(compiler_path, [service, startup_service, *reporter.read_jsonl(compiler_path)[1:], queue])
        report = reporter.analyze(record)
        self.assertEqual(report["phase_coverage"]["complete"], True)
        self.assertEqual(report["workload_request_service_joins"]["status"], "complete")
        self.assertEqual(report["whole_physical_stream_reconciliation"]["status"], "complete")
        self.assertEqual(report["startup_scope"]["owner_status"], "unknown_unowned_or_unmatched_requests")
        self.assertEqual(report["startup_scope"]["unowned_host_submission_count"], 1)
        self.assertEqual(report["queue_evidence"]["status"], "complete")
        self.assertEqual(report["startup_scope"]["assignment_policy"],
                         "exact_declared_phase_or_root_startup_owner; no count or timestamp assignment")

    def test_startup_owner_requires_and_accepts_explicit_phase_span(self):
        record = self.make_case()
        host_path = self.root / "artifacts/host.jsonl"
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        request = {"daemon_epoch": "epoch-a", "admission_id": 8,
                   "request_ordinal": 1, "compile_request": "startup-digest"}
        host = reporter.read_jsonl(host_path)
        host.append({"target": "tidepool_extract_cmd::endpoint", "fields": {
            "message": "compiler request identified", "transport": "daemon", **request},
            "span": {"name": "activation", "workload_phase": "activation"}})
        daemon = reporter.read_jsonl(compiler_path)
        service = json.loads(json.dumps(daemon[0]))
        service["span"].update(request)
        daemon.append(service)
        daemon.append({"target": "tidepool_extract_cmd::daemon", "fields": {
            "phase": "compiler_queue", "message": "compiler job dequeued", "queue_ms": 0,
            "daemon_epoch": "epoch-a", "admission_id": 8}})
        self.write_jsonl(host_path, host)
        self.write_jsonl(compiler_path, daemon)
        report = reporter.analyze(record)
        self.assertEqual(report["startup_scope"]["owner_status"], "complete")
        self.assertEqual(report["startup_scope"]["explicit_activation_owner_request_count"], 1)
        self.assertEqual(report["startup_scope"]["unowned_host_submission_count"], 0)

    def test_named_production_startup_span_owns_request_without_count_assignment(self):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        phases = reporter.read_jsonl(phase_path)
        phases[1]["logical_compiler_requests"] = 2
        self.write_jsonl(phase_path, phases)
        host_path = self.root / "artifacts/host.jsonl"
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        request = {"daemon_epoch": "epoch-a", "admission_id": 8,
                   "request_ordinal": 1, "compile_request": "startup-digest"}
        host = reporter.read_jsonl(host_path)
        host.append({"target": "tidepool_extract_cmd::endpoint", "fields": {
            "message": "compiler request identified", "transport": "daemon", **request},
            "spans": [{"name": "host_run"}, {"name": "compile_root", "actor_path": "root"}]})
        daemon = reporter.read_jsonl(compiler_path)
        service = json.loads(json.dumps(daemon[0]))
        service["span"].update(request)
        daemon.append(service)
        daemon.append({"target": "tidepool_extract_cmd::daemon", "fields": {
            "phase": "compiler_queue", "message": "compiler job dequeued", "queue_ms": 0,
            "daemon_epoch": "epoch-a", "admission_id": 8}})
        toolset_request = {"daemon_epoch": "epoch-a", "admission_id": 9,
                           "request_ordinal": 1, "compile_request": "toolset-digest"}
        host.append({"target": "tidepool_extract_cmd::endpoint", "fields": {
            "message": "compiler request identified", "transport": "daemon", **toolset_request},
            "spans": [{"name": "workspace_toolsets_prepare", "workspace": "/test/workspace",
                       "deployment": "/test/prepared"}]})
        toolset_service = json.loads(json.dumps(daemon[0]))
        toolset_service["span"].update(toolset_request)
        daemon.append(toolset_service)
        daemon.append({"target": "tidepool_extract_cmd::daemon", "fields": {
            "phase": "compiler_queue", "message": "compiler job dequeued", "queue_ms": 0,
            "daemon_epoch": "epoch-a", "admission_id": 9}})
        self.write_jsonl(host_path, host)
        self.write_jsonl(compiler_path, daemon)
        report = reporter.analyze(record)
        self.assertEqual(report["startup_scope"]["owner_status"], "complete")
        self.assertEqual(report["startup_scope"]["explicit_startup_span_owner_request_count"], 2)
        self.assertEqual({tuple(row["owner_spans"]) for row in
                          report["startup_scope"]["explicit_startup_span_requests"]},
                         {("compile_root",), ("workspace_toolsets_prepare",)})

    def test_nested_harness_cost_events_are_retained_without_adding_spans(self):
        phases = [{"phase": "reuse-retained", "call_id": "usecase-3"}]
        rows = [{"target": "harness::runtime_cost", "level": "DEBUG", "fields": {
            "message": "store projection", "elapsed_ns": 14},
            "spans": [{"name": "request", "call_id": "usecase-3"},
                      {"name": "portable_history_projection", "elapsed_ns": 14}]},
            {"target": "harness::runtime_cost", "fields": {"message": "unknown boundary"}}]
        events = reporter.runtime_cost_events(rows, phases)
        self.assertEqual(events[0]["phase_attribution"], "reuse-retained")
        self.assertEqual(events[0]["fields"]["elapsed_ns"], 14)
        self.assertEqual(events[0]["spans"][1]["elapsed_ns"], 14)
        self.assertEqual(events[1]["phase_attribution"], "unknown")
        coverage = reporter.runtime_cost_coverage(events)
        self.assertEqual(coverage["store_projection_status"], "events_observed")
        self.assertEqual(coverage["host_admission_status"], "not_observed_unknown")

    def test_unknown_or_source_backed_profile_is_never_reported_as_frozen_qualification(self):
        report = reporter.analyze(self.make_case())
        self.assertEqual(report["environment"]["execution_profile_observed"], "source-backed-or-unprepared")
        self.assertEqual(report["environment"]["frozen_catalog_qualification"],
                         "not-established-by-this-counted-test")

    def test_unknown_workload_roster_is_a_controlled_error(self):
        record = self.make_case()
        payload = json.loads(record.read_text())
        payload["test"] = "actor_host::fixture_without_workload_contract"
        self.write_json(record, payload)
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        del rows[0]["workload_roster"]
        self.write_jsonl(phase_path, rows)
        with self.assertRaisesRegex(ValueError, "must declare a nonempty workload_roster"):
            reporter.analyze(record)

    def test_three_actor_roster_is_explicit_and_counts_activation_in_order(self):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        rows[0]["workload_cohort"] = "three-actor-capture"
        rows[0]["workload_roster"] = list(reporter.THREE_ACTOR_PHASES)
        rows[1]["sequence"] = 0
        rows[1]["logical_compiler_requests"] = 0
        rows[1]["workspace_preparation_ns"] = 11_000
        rows[1]["host_start_readiness_ns"] = 7_000
        rows[2:] = [
            {"schema": 1, "phase": phase, "sequence": index, "completed": True,
             "call_id": "usecase-3" if phase == "root-fork-capture" else f"three-actor-{index}",
             "logical_compiler_requests": 1 if phase == "root-fork-capture" else 0,
             "wall_ns": 1000,
             "evidence": ({"cell_role": "first"} if phase == "root-setup" else
                          {"cell_role": "retained_action_reuse"}
                          if phase == "parent-publication-read" else {}),
             "boundary_kind": "native" if phase == "root-fork-capture" else "lifecycle",
             **({"scripted_response_hold_ns": 250}
                if phase in reporter.PROVIDER_RESPONSE_PHASES else {})}
            for index, phase in enumerate(reporter.THREE_ACTOR_PHASES[1:], 1)
        ]
        self.write_jsonl(phase_path, rows)
        report = reporter.analyze(record)
        self.assertEqual(report["phase_coverage"]["cohort"], "three-actor-capture")
        self.assertEqual(report["phase_coverage"]["expected_count"], 9)
        self.assertTrue(report["phase_coverage"]["complete"])
        self.assertEqual(report["phase_measurements"]["status"], "complete")
        self.assertEqual(report["activation_timing"]["workspace_preparation_ns"], 11_000)
        self.assertEqual(report["activation_timing"]["host_start_readiness_ns"], 7_000)
        root_fork = next(phase for phase in report["phases"] if phase["phase"] == "root-fork-capture")
        self.assertEqual(root_fork["phase_record"]["boundary_kind"], "native")
        self.assertEqual(root_fork["scripted_response_hold_ns"], 250)
        self.assertEqual(root_fork["scripted_response_hold_status"], "observed_test_coordination")
        self.assertIsNone(root_fork["provider_model_latency_ms"])
        self.assertEqual(root_fork["provider_model_latency_status"], "not_measured_scripted_provider")
        first = next(phase for phase in report["phases"] if phase["phase"] == "root-setup")
        repeated = next(phase for phase in report["phases"] if phase["phase"] == "parent-publication-read")
        self.assertEqual(first["behavior_role"], "first")
        self.assertEqual(repeated["behavior_role"], "retained_action_reuse")

    def test_recursive_measurement_requires_prep_start_and_scripted_response_timings(self):
        record = self.make_complete_case("three-actor-capture")
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        activation = next(row for row in rows if row["phase"] == "activation")
        del activation["workspace_preparation_ns"]
        root_setup = next(row for row in rows if row["phase"] == "root-setup")
        del root_setup["scripted_response_hold_ns"]
        self.write_jsonl(phase_path, rows)
        report = reporter.analyze(record)
        self.assertEqual(report["phase_measurements"]["status"], "partial_or_unknown")
        invalid = report["phase_measurements"]["invalid_records"]
        self.assertEqual({row["phase"] for row in invalid}, {"activation", "root-setup"})

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

    def test_conflicting_compiler_owners_are_ambiguous_and_never_credited_twice(self):
        for cohort in reporter.KNOWN_ROSTERS:
            with self.subTest(cohort=cohort):
                record = self.make_complete_case(cohort)
                phase_path = self.root / "artifacts/phases.jsonl"
                phases = reporter.read_jsonl(phase_path)
                second_phase = "first-arithmetic" if cohort == "harness-eight-phase" else "child-alpha-reply"
                next(phase for phase in phases if phase["phase"] == second_phase)["logical_compiler_requests"] = 1
                self.write_jsonl(phase_path, phases)
                host_path = self.root / "artifacts/host.jsonl"
                host = reporter.read_jsonl(host_path)
                host[-1]["fields"]["workload_phase"] = second_phase
                self.write_jsonl(host_path, host)
                report = reporter.analyze(record)
                self.assertEqual(report["whole_physical_stream_reconciliation"]["status"], "complete")
                self.assertEqual(report["workload_request_service_joins"]["status"], "partial_or_unknown")
                self.assertEqual(report["workload_request_service_joins"]["exact_phase_request_join_count"], 0)
                self.assertEqual(report["startup_scope"]["ambiguous_owner_submission_count"], 1)
                ambiguity = report["unattributed_compiler_submissions"][0]
                self.assertEqual(ambiguity["phase_attribution"], "ambiguous")
                self.assertEqual(len(ambiguity["owner_candidates"]), 2)

    def test_nested_executions_and_agreeing_owners_are_order_independent(self):
        record = self.make_complete_case("three-actor-capture")
        phase_path = self.root / "artifacts/phases.jsonl"
        phases = reporter.read_jsonl(phase_path)
        next(phase for phase in phases if phase["phase"] == "child-alpha-reply")["logical_compiler_requests"] = 1
        self.write_jsonl(phase_path, phases)
        host_path = self.root / "artifacts/host.jsonl"
        base = reporter.read_jsonl(host_path)
        base.append({"target": "exomonad_actor::resident_tools", "fields": {
            "message": "workbench cell dispatched to its actor", "context_call_id": "case-4",
            "execution": "exec-child"}})
        base[2]["spans"] = [{"name": "cell", "execution": "exec-child"}]
        for order in itertools.permutations(base):
            self.write_jsonl(host_path, order)
            result = reporter.analyze(record)
            self.assertEqual(result["workload_request_service_joins"]["exact_phase_request_join_count"], 0)
            self.assertEqual(result["startup_scope"]["ambiguous_owner_submission_count"], 1)
        # Independent observations agreeing on one owner retain one physical join.
        base[2].pop("spans")
        base[2]["fields"]["workload_phase"] = "root-fork-capture"
        self.write_jsonl(host_path, base)
        result = reporter.analyze(record)
        self.assertEqual(result["workload_request_service_joins"]["exact_phase_request_join_count"], 1)

    def test_all_queue_events_count_even_when_duplicate_timing_is_invalid(self):
        record = self.make_complete_case()
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        original = reporter.read_jsonl(compiler_path)
        for invalid in (None, -1, True, "invalid"):
            duplicate = json.loads(json.dumps(original[-1]))
            duplicate["fields"]["queue_ms"] = invalid
            for order in itertools.permutations([*original, duplicate]):
                self.write_jsonl(compiler_path, order)
                result = reporter.analyze(record)
                self.assertEqual(result["queue_evidence"]["status"], "partial_or_unknown")
                self.assertEqual(result["queue_observations"][0]["queue_event_count"], 2)
                self.assertEqual(result["queue_observations"][0]["invalid_timing_record_count"], 1)

    def test_phase_measurements_require_nonnegative_json_integers(self):
        record = self.make_complete_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        original = reporter.read_jsonl(phase_path)
        for field in ("wall_ns", "logical_compiler_requests"):
            for invalid in (None, -1, True, 1.5, "1000"):
                phases = json.loads(json.dumps(original))
                phases[2][field] = invalid
                self.write_jsonl(phase_path, phases)
                result = reporter.analyze(record)
                self.assertEqual(result["phase_measurements"]["status"], "partial_or_unknown")
                self.assertIn(field, result["phase_measurements"]["invalid_records"][0]["invalid_fields"])
        phases = json.loads(json.dumps(original))
        phases[1].pop("wall_ns")
        self.write_jsonl(phase_path, phases)
        self.assertEqual(reporter.analyze(record)["phase_measurements"]["invalid_records"][0]["phase"], "activation")
        self.write_jsonl(phase_path, original)
        self.assertEqual(reporter.analyze(record)["phase_measurements"]["status"], "complete")

    def test_actor_prepare_startup_owner_requires_canonical_root_path(self):
        record = self.make_case()
        host_path = self.root / "artifacts/host.jsonl"
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        original_host = reporter.read_jsonl(host_path)
        original_daemon = reporter.read_jsonl(compiler_path)
        request = {"daemon_epoch": "epoch-a", "admission_id": 8,
                   "request_ordinal": 1, "compile_request": "startup-digest"}
        service = json.loads(json.dumps(original_daemon[0]))
        service["span"].update(request)
        queue = {"fields": {"phase": "compiler_queue", "queue_ms": 0,
                            "daemon_epoch": "epoch-a", "admission_id": 8}}
        self.write_jsonl(compiler_path, [*original_daemon, service, queue])
        for name in ("compile_root", "actor_application_prepare"):
            for path in (None, "", "/root", "root/child", "root"):
                span = {"name": name, "actor_path": path}
                self.write_jsonl(host_path, [*original_host, {
                    "target": "tidepool_extract_cmd::endpoint",
                    "fields": {"message": "compiler request identified", **request}, "span": span}])
                status = reporter.analyze(record)["startup_scope"]["owner_status"]
                self.assertEqual(status == "complete", path == "root")

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
        dispatches, _, submissions, services, _, _ = reporter.compiler_events(
            [dispatch, submission], [physical])
        self.assertEqual(dispatches["call-a"], {"exec-a"})
        self.assertEqual(submissions[0]["identity"], ("epoch-a", 6, 1, "digest-a"))
        self.assertEqual(submissions[0]["executions"], [])
        self.assertEqual(services[0]["identity"], submissions[0]["identity"])


if __name__ == "__main__":
    unittest.main()
