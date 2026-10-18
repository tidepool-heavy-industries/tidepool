import importlib.util
import contextlib
import io
from datetime import datetime, timedelta, timezone
import itertools
import json
from pathlib import Path
import random
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "harness-usecase-perf-report.py"
spec = importlib.util.spec_from_file_location("harness_usecase_perf_report", SCRIPT)
reporter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reporter)


def check_shrinking_samples(samples, check):
    """Shrink numeric inputs while preserving the generated fault or control."""
    for sample in samples:
        try:
            check(sample)
        except AssertionError:
            smallest = sample
            changed = True
            while changed:
                changed = False
                for index, value in enumerate(smallest):
                    if type(value) is not int or value <= 0:
                        continue
                    for reduced in sorted({0, value // 2}):
                        candidate = (*smallest[:index], reduced, *smallest[index + 1:])
                        try:
                            check(candidate)
                        except AssertionError:
                            smallest = candidate
                            changed = True
                            break
                    if changed:
                        break
            try:
                check(smallest)
            except AssertionError as error:
                raise AssertionError(f"sample={sample!r}; minimized={smallest!r}: {error}") from error


class HarnessUsecasePerfReportTests(unittest.TestCase):
    def test_case_wall_ledger_keeps_verification_shutdown_and_unpartitioned_work_separate(self):
        for scale in (1, 17, 1000000):
            phases = [
                {"phase": "activation", "start_ns": 2 * scale, "end_ns": 12 * scale, "wall_ns": 10 * scale,
                 "installation_verification_start_ns": 13 * scale,
                 "installation_verification_end_ns": 16 * scale, "installation_verification_ns": 3 * scale},
                {"phase": "cell", "start_ns": 20 * scale, "end_ns": 30 * scale, "wall_ns": 10 * scale},
                {"phase": "measurement-summary", "start_ns": 32 * scale, "end_ns": 40 * scale,
                 "wall_ns": 8 * scale, "case_wall_ns": 40 * scale},
            ]
            report = reporter.case_wall_ledger(phases, ["cell"])
            self.assertEqual(report["status"], "complete")
            self.assertEqual(report["measured_disjoint_ns"], 31 * scale)
            self.assertEqual(report["unpartitioned_ns"], 9 * scale)
            overlap = [*phases[:1], {**phases[1], "start_ns": 15 * scale,
                                   "wall_ns": 15 * scale}, phases[2]]
            self.assertEqual(reporter.case_wall_ledger(overlap, ["cell"])["status"], "partial_or_unknown")
            self.assertEqual(reporter.case_wall_ledger([*phases, phases[-1]], ["cell"])["status"], "partial_or_unknown")
        self.assertEqual(reporter.case_wall_ledger([], ["cell"])["status"], "not_collected")

    def test_catalog_acceptance_is_receipt_observation_not_unique_or_missing_zero(self):
        values = {"message": "deployment module products accepted", "deployment_accepted_modules": 2,
                  "deployment_original_product_bytes": 1024, "receipt_fresh_native_modules": 1,
                  "receipt_cached_native_modules": 3, "receipt_retained_core_native_modules": 0}
        row = {"target": "tidepool_toolchain::module_candidates::acceptance", "fields": values,
               "span": {"name": "cell", "execution": "issued-execution"}}
        report = reporter.catalog_acceptance_observations([row, row])
        self.assertEqual(report["status"], "observed")
        self.assertEqual(report["module_observations"]["deployment_accepted_modules"], 4)
        self.assertEqual(report["events"][0]["execution"], "issued-execution")
        self.assertIsNone(report["events"][0]["request_identity"])
        absent = reporter.catalog_acceptance_observations([])
        self.assertEqual(absent["status"], "not_observed_unknown")
        self.assertIsNone(absent["module_observations"]["receipt_fresh_native_modules"])
        old = {**row, "fields": {key: value for key, value in values.items()
                                 if key != "receipt_fresh_native_modules"}}
        self.assertEqual(reporter.catalog_acceptance_observations([old])["status"], "partial_or_unknown")

    def workbench_rows(self, scale=1):
        phases = [
            (0, "private_begin_started"), (1, "private_begin_completed"),
            (1, "cell_prepare_started"), (4, "cell_prepare_completed"),
            (4, "checked_item_admit_started"), (5, "checked_item_admitted"),
            (5, "native_item_snapshot_started"), (7, "native_item_snapshot_completed"),
            (8, "native_item_jit_completed"), (8, "native_item_run_started"),
            (9, "native_item_run_completed"), (9, "private_publish_started"),
            (11, "private_publish_completed"),
        ]
        origin = datetime(2026, 10, 15, tzinfo=timezone.utc)
        return [{"timestamp": (origin + timedelta(microseconds=tick * scale)).isoformat(),
                 "target": "exomonad_actor::workbench_phase",
                 "fields": {"message": "workbench phase", "phase": phase,
                            **({"install_attempt": 1} if phase.startswith("native_item_") else {})},
                 "span": {"name": "cell", "execution": "measured", "item": 0}}
                for tick, phase in phases]

    def test_workbench_partition_is_additive_and_does_not_count_nested_spans_twice(self):
        for scale in (1, 17, 1000, 1_000_000):
            rows = self.workbench_rows(scale)
            nested = {"timestamp": rows[3]["timestamp"],
                      "target": "exomonad_harness::timing",
                      "fields": {"message": "close", "time.busy": "2ms", "time.idle": "3µs"},
                      "span": {"name": "cell_program.offer", "inclusive": True},
                      "spans": [{"name": "cell", "execution": "measured"}]}
            result = reporter.workbench_phase_partition([*rows, nested], "measured")
            self.assertEqual(result["status"], "complete")
            self.assertEqual(sum(result["totals_ns"].values()), 11 * scale * 1000)
            self.assertEqual(result["counts"]["item_snapshot"], 1)
            self.assertEqual(result["nested_owner_spans"][0]["busy_ns"], 2_000_000)
            self.assertEqual(result["nested_owner_spans"][0]["idle_ns"], 3000)
            self.assertEqual(reporter.workbench_phase_partition(rows, "other")["status"], "not_collected")

    def test_workbench_partition_refuses_missing_duplicate_reversed_and_overlapping_boundaries(self):
        rows = self.workbench_rows()
        cases = [rows[:-1], [*rows[:1], rows[0], *rows[1:]]]
        reversed_rows = [dict(row) for row in rows]
        reversed_rows[3] = {**rows[3], "timestamp": rows[0]["timestamp"]}
        cases.append(reversed_rows)
        overlapping = [dict(row) for row in rows]
        overlapping[4] = {**rows[4], "timestamp": rows[2]["timestamp"]}
        cases.append(overlapping)
        cases.append([row for row in rows if row["fields"]["phase"].startswith(
            ("private_", "cell_prepare_"))])
        for case in cases:
            result = reporter.workbench_phase_partition(case, "measured")
            self.assertEqual(result["status"], "partial_or_unknown", result)
            self.assertTrue(result["problems"])

    def test_pre_post_preparation_never_treats_missing_owner_spans_as_zero(self):
        rows = self.workbench_rows()
        result = reporter.workbench_phase_partition(rows, "measured")
        for group in result["compiler_pre_post_sibling_spans"].values():
            self.assertEqual(group["status"], "partial_or_unknown")
            self.assertIsNone(group["wall_ns"])
        names = ["cell_program.setup", "cell_program.bind", "cell_program.offer",
                 "cell_program.admit", "cell_program.declaration_projections", "cell_program.seal"]
        owners = [{"target": "exomonad_harness::timing",
                   "fields": {"message": "close", "time.busy": "1ms", "time.idle": "0ns"},
                   "span": {"name": name}, "spans": [{"name": "cell", "execution": "measured"}]}
                  for name in names]
        result = reporter.workbench_phase_partition([*rows, *owners], "measured")
        for group in result["compiler_pre_post_sibling_spans"].values():
            self.assertEqual(group["status"], "complete")
            self.assertEqual(group["wall_ns"], 3_000_000)
        result = reporter.workbench_phase_partition([*rows, *owners, owners[0]], "measured")
        self.assertIsNone(result["compiler_pre_post_sibling_spans"]["pre_compiler"]["wall_ns"])

    def test_workbench_partition_requires_each_native_item_attempt(self):
        rows = self.workbench_rows()
        second = json.loads(json.dumps(rows[4:11]))
        for row in second:
            row["timestamp"] = (datetime.fromisoformat(row["timestamp"]) + timedelta(microseconds=5)).isoformat()
            row["span"]["item"] = 1
        publication = json.loads(json.dumps(rows[11:]))
        for row in publication:
            row["timestamp"] = (datetime.fromisoformat(row["timestamp"]) + timedelta(microseconds=5)).isoformat()
        complete = [*rows[:11], *second, *publication]
        self.assertEqual(reporter.workbench_phase_partition(complete, "measured")["status"], "complete")
        # Global phase presence and even equal totals cannot stand in for each
        # item's sequence: retain a missing run and a misattributed run control.
        missing_run = [*rows[:11], *second[:-2], *publication]
        wrong_item = json.loads(json.dumps(complete))
        wrong_item[16]["span"]["item"] = 0
        wrong_item[17]["span"]["item"] = 0
        for case in (missing_run, wrong_item):
            result = reporter.workbench_phase_partition(case, "measured")
            self.assertEqual(result["status"], "partial_or_unknown")
            self.assertIn("incomplete_or_reordered_native_attempt", [p["reason"] for p in result["problems"]])
        declaration = json.loads(json.dumps(second[:2]))
        self.assertEqual(reporter.workbench_phase_partition(
            [*rows[:11], *declaration, *publication], "measured")["status"], "complete")

    def test_workbench_partition_checks_event_clock_order_and_native_identity(self):
        rows = self.workbench_rows()
        backwards = json.loads(json.dumps(rows))
        for row in backwards[:2]:
            row["timestamp"] = (datetime.fromisoformat(row["timestamp"]) + timedelta(microseconds=20)).isoformat()
        result = reporter.workbench_phase_partition(backwards, "measured")
        self.assertEqual(result["status"], "partial_or_unknown")
        self.assertIn("clock_reversed_in_event_order", [p["reason"] for p in result["problems"]])
        for key, location in (("item", "span"), ("install_attempt", "fields")):
            missing = json.loads(json.dumps(rows))
            del missing[6][location][key]
            result = reporter.workbench_phase_partition(missing, "measured")
            self.assertEqual(result["status"], "partial_or_unknown")
            self.assertIn("missing_native_item_attempt_identity", [p["reason"] for p in result["problems"]])

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

    def make_case(self, *, queue=True, submissions=True, service_records=1, cell_span=True,
                  trace_profile=None):
        artifact_root = self.root / "artifacts"
        host_path = artifact_root / "host.jsonl"
        compiler_path = artifact_root / "compiler/compiler.jsonl"
        phase_path = artifact_root / "phases.jsonl"
        phase_records = [
            {"schema": 1, "phase": "environment", "workload_cohort": "harness-eight-phase",
             "workload_roster": list(reporter.EXPECTED_PHASES),
             "campaign_trace_profile": trace_profile or "full",
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
        if trace_profile is not None:
            phase_records[0]["campaign_trace_profile"] = trace_profile
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
                          "trace_profile": trace_profile or "full",
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

    def add_owner_summaries(self):
        host_path = self.root / "artifacts/host.jsonl"
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        host = reporter.read_jsonl(host_path)
        host += [{"target": "tidepool_codegen::prepared_compile", "fields": {
                    "message": "prepared compile", "process_id": 99, "image_instance": 7}},
                 {"target": "tidepool_codegen::image_install", "fields": {
                    "process_id": 99, "image_instance": 7, "outcome": "machine_published"}}]
        self.write_jsonl(host_path, host)
        daemon = reporter.read_jsonl(compiler_path)
        span = next(row["span"] for row in daemon
                    if row["fields"].get("message") == "compiler request finished")
        daemon += [{"span": span, "fields": {"line": "tidepool-validation " + json.dumps({
                    "schema": 1, "invocation_id": 1, "worker_pid": 123, "start_ns": 10,
                    "end_ns": 20, "wall_ns": 10, "cpu_ns": 9, "clock_domain": "ghc_monotonic_ns",
                    "outcome": "success", "counts": {"observed_file_count": 8}})}}]
        self.write_jsonl(compiler_path, daemon)

    def make_child_activation_case(self):
        record = self.make_complete_case("three-actor-capture")
        trace = self.root / "artifacts/host.jsonl"
        retained = reporter.read_jsonl(trace)
        rows = [next(row for row in retained if row["fields"].get("message") == message
                     and (row["fields"].get("execution") == "exec-3" or message == "compiler request identified"))
                for message in ("workbench cell dispatched to its actor", "call timing", "compiler request identified")]
        rows[0]["fields"]["actor"] = "1@1"
        origin = {"request": 11, "activation_actor": "2@1", "parent_actor": "1@1",
                  "parent_execution": "exec-3", "parent_attempt": "attempt-3"}
        rows.append({"target": "exomonad_actor::request", "fields": {
            "message": "request activation origin issued", **origin},
            "span": {"name": "cell", "actor": "1@1", "execution": "exec-3"}})
        rows[2]["span"] = {"name": "activation_input_prepare", "actor": "2@1",
                           "source_request": 11, "parent_actor": "1@1",
                           "parent_execution": "exec-3", "parent_attempt": "attempt-3"}
        self.write_jsonl(trace, rows)
        return record, trace, rows

    def test_child_activation_exact_origin_is_order_independent(self):
        record, trace, rows = self.make_child_activation_case()
        for permutation in itertools.permutations(rows):
            self.write_jsonl(trace, permutation)
            report = reporter.analyze(record)
            self.assertEqual(report["workload_request_service_joins"]["status"], "complete")
            self.assertEqual(report["startup_scope"]["owner_status"], "complete")
            self.assertEqual(report["activation_input_origins"][0]["status"], "verified")
            self.assertEqual(report["activation_input_origins"][0]["phase_attribution"],
                             "root-fork-capture")
            self.assertEqual(report["startup_scope"]["explicit_startup_span_owner_request_count"], 0)

    def test_child_activation_missing_mismatched_and_ambiguous_origins_refused(self):
        record, trace, original = self.make_child_activation_case()
        mutations = [
            ("missing issuance", lambda rows: rows.pop()),
            ("duplicate issuance", lambda rows: rows.append(rows[-1])),
            ("duplicate dispatch", lambda rows: rows.append(rows[0])),
            ("wrong child", lambda rows: rows[2]["span"].update(actor="3@1")),
            ("wrong incarnation", lambda rows: rows[2]["span"].update(actor="2@2")),
            ("wrong request", lambda rows: rows[2]["span"].update(source_request=12)),
            ("wrong parent", lambda rows: rows[2]["span"].update(parent_actor="4@1")),
            ("wrong execution", lambda rows: rows[2]["span"].update(parent_execution="exec-other")),
            ("wrong attempt", lambda rows: rows[2]["span"].update(parent_attempt="retry")),
            ("wrong issuer cell", lambda rows: rows[-1]["span"].update(execution="exec-other")),
            ("wrong dispatch actor", lambda rows: rows[0]["fields"].update(actor="4@1")),
            ("competing child receipt", lambda rows: rows[2].update(spans=[{
                **rows[2]["span"], "actor": "3@1"}])),
            ("competing issuer cell", lambda rows: rows[-1].update(spans=[{
                **rows[-1]["span"], "execution": "exec-other"}])),
            ("explicit phase cannot hide missing origin", lambda rows: rows[2]["span"].update(
                source_request=12, workload_phase="root-fork-capture")),
        ]
        mutations += [(f"absent {field}", lambda rows, field=field: rows[2]["span"].pop(field))
                      for field in ("source_request", "parent_actor", "parent_execution", "parent_attempt")]
        for label, mutation in mutations:
            with self.subTest(label=label):
                rows = json.loads(json.dumps(original))
                mutation(rows)
                self.write_jsonl(trace, rows)
                report = reporter.analyze(record)
                self.assertEqual(report["workload_request_service_joins"]["status"], "partial_or_unknown")
                self.assertEqual(report["startup_scope"]["owner_status"], "unknown_unowned_or_unmatched_requests")
                self.assertNotEqual(report["activation_input_origins"][0]["status"], "verified")
                self.assertEqual(report["activation_input_origins"][0]["phase_attribution"], "unknown")

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

    def test_logical_request_aggregate_preserves_missing_and_invalid_counts(self):
        for invalid in (None, -1, True, "1"):
            with self.subTest(invalid=invalid):
                record = self.make_complete_case()
                path = self.root / "artifacts/phases.jsonl"
                phases = reporter.read_jsonl(path)
                next(row for row in phases if row["phase"] == "first-arithmetic")["logical_compiler_requests"] = invalid
                self.write_jsonl(path, phases)
                aggregate = reporter.analyze(record)["workload_request_service_joins"]
                self.assertIsNone(aggregate["logical_request_count"])
                self.assertEqual(aggregate["unknown_logical_request_phase_count"], 1)
                self.assertEqual(aggregate["observed_logical_request_subtotal"], 1)
        record = self.make_complete_case()
        path = self.root / "artifacts/phases.jsonl"
        phases = reporter.read_jsonl(path)
        for row in phases:
            if row["phase"] != "environment":
                row["logical_compiler_requests"] = 0
        self.write_jsonl(path, phases)
        aggregate = reporter.analyze(record)["workload_request_service_joins"]
        self.assertEqual(aggregate["logical_request_count"], 0)
        self.assertEqual(aggregate["unknown_logical_request_phase_count"], 0)

    def test_source_timing_claim_does_not_require_frozen_preparation(self):
        record = self.make_complete_case(trace_profile="minimal")
        self.add_owner_summaries()
        path = self.root / "artifacts/phases.jsonl"
        phases = reporter.read_jsonl(path)
        phases[0]["prepared_root_entry_supplied"] = False
        self.write_jsonl(path, phases)
        report = reporter.analyze(record)
        assessment = reporter.require_claim(report, "workload_timing", scope="harness-eight-phase")
        self.assertEqual(assessment["status"], "valid_workload_timing")
        with self.assertRaises(ValueError):
            reporter.require_claim(report, "workload_timing", scope="persona-workbench")
        self.assertEqual(report["environment"]["frozen_catalog_qualification"], "not-established-by-this-counted-test")
        with self.assertRaises(ValueError):
            reporter.require_claim(report, "frozen_workload_timing", scope="harness-eight-phase")

    def test_claims_distinguish_failed_body_missing_observations_and_empty_phases(self):
        for mutation in ("failed_body", "failed_disposal", "failed_timeout", "unknown_cleanup", "no_phases"):
            with self.subTest(mutation=mutation):
                record = self.make_complete_case(trace_profile="minimal")
                self.add_owner_summaries()
                payload = json.loads(record.read_text())
                if mutation == "failed_body":
                    payload.update(passed=False)
                    payload["execution"]["exit_code"] = 101
                elif mutation == "failed_disposal":
                    payload.update(passed=False)
                    payload["execution"]["disposal"] = {"status": "failed"}
                elif mutation == "failed_timeout":
                    payload.update(passed=False)
                    payload["execution"].update(status="timeout", executed_test_count=None)
                elif mutation == "unknown_cleanup":
                    payload["execution"]["hosted_cleanup_status"] = "unknown"
                else:
                    path = self.root / "artifacts/phases.jsonl"
                    self.write_jsonl(path, reporter.read_jsonl(path)[:1])
                self.write_json(record, payload)
                report = reporter.analyze(record)
                expected = "failed_workload_diagnostics" if mutation.startswith("failed_") else "insufficient_observations"
                self.assertEqual(report["assessment"]["status"], expected)
                if mutation == "no_phases":
                    self.assertEqual(report["phase_measurements"]["status"], "partial_or_unknown")
                    self.assertIsNone(report["workload_request_service_joins"]["logical_request_count"])
                with self.assertRaises(ValueError):
                    reporter.require_claim(report, "workload_timing", scope="harness-eight-phase")

    def test_claim_consumption_never_promotes_historical_or_missing_assessment(self):
        for report in ({"schema": 1, "completed": True}, {"schema": True, "completed": True},
                       {"schema": 2, "completed": True},
                       {"schema": 2, "assessment": {"kind": "workload_timing", "status": "valid_workload_timing"}}):
            with self.subTest(report=report), self.assertRaises(ValueError):
                reporter.require_claim(report, "workload_timing", scope="harness-eight-phase")
        self.assertEqual(reporter.claim_assessment({"schema": 1}, "workload_timing")["status"], "unassessed")
        with self.assertRaises(ValueError):
            reporter.require_claim({"schema": 2}, "unknown-kind", scope="harness-eight-phase")

    def test_report_cli_serialization_and_requested_claim_have_distinct_outcomes(self):
        record = self.make_case()
        for options, expected in (([], 0), (["--require-claim", "workload_timing", "--workload-scope", "harness-eight-phase"], 1)):
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(reporter.main([str(record), *options]), expected)
            self.assertEqual(json.loads(stdout.getvalue())["assessment"]["status"], "insufficient_observations")
        (self.root / "artifacts/phases.jsonl").unlink()
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(reporter.main([str(record)]), 2)
        self.assertEqual(json.loads(stdout.getvalue())["assessment"]["status"], "insufficient_observations")

    def make_complete_case(self, cohort="harness-eight-phase", trace_profile=None):
        record = self.make_case(trace_profile=trace_profile)
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
        measured_phase = {"harness-eight-phase": "reuse-retained", "three-actor-capture": "root-fork-capture",
                          "persona-workbench": "warm-context-read-first",
                          "two-children-concurrent": "root-fork-capture", "notebook-a-a-b-a": "repeat-a",
                          "prepare-once-repeated-launch": "fresh-preparation"}[cohort]
        workload = [
            {"schema": 1, "phase": name, "sequence": index, "completed": True,
             "call_id": "usecase-3" if name == measured_phase
                        else f"case-{index}",
             "logical_compiler_requests": 1 if name == measured_phase else 0,
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
        self.add_execution_capture(record)
        return record

    def add_execution_capture(self, record):
        """Complete fixture facts use the production dispatch and terminal record shape."""
        phase_path = self.root / "artifacts/phases.jsonl"
        host_path = self.root / "artifacts/host.jsonl"
        daemon_path = self.root / "artifacts/compiler/compiler.jsonl"
        phases = reporter.read_jsonl(phase_path)
        profile = phases[0]["campaign_trace_profile"]
        host = reporter.read_jsonl(host_path)
        terminal_fields = {"actor": 1, "incarnation": 1, "process_id": 99,
            "clock_domain": "actor_process_monotonic", "start_ns": 10, "end_ns": 30,
            "wall_ns": 20, "total_ms": 0, "checkout_wait_ms": 0, "checkout_hold_ms": 0,
            "compile_ms": 0, "compile_count": 0, "jev_ms": 0, "jev_count": 0, "exec_ms": 0,
            "publication_attempts": 1, "semantic_rebases": 0, "metadata_restages": 0,
            "outcome": "Committed"}
        for row in host:
            if row["fields"].get("message") == "call timing":
                row["fields"] = {**terminal_fields, **row["fields"]}
            if row["fields"].get("message") == "workbench cell dispatched to its actor":
                row["fields"]["actor"] = "1@1"
        existing_calls = {row["fields"].get("context_call_id") for row in host}
        added = []
        for phase in phases:
            if phase["phase"] in ("environment", "activation", "async-yield", "host-cleanup",
                                   "fresh-preparation", "launch-1", "launch-2", "launch-3"):
                continue
            phase["actor_id"] = "1@1"
            calls = [phase["call_id"]]
            if phase["phase"] == "children-concurrent-replies":
                phase.pop("call_id", None)
                calls = ["three-actor-child-alpha", "three-actor-child-beta"]
                phase["call_ids"] = calls
                phase["evidence"]["replies"] = [{"call_id": call, "actor": f"{index + 2}@1"}
                                                  for index, call in enumerate(calls)]
            for index, call in enumerate(calls):
                if call in existing_calls:
                    continue
                execution = "execution-" + call
                actor = index + 2 if len(calls) == 2 else 1
                added.extend([{"target": "exomonad_actor::resident_tools", "fields": {
                    "message": "workbench cell dispatched to its actor", "actor": f"{actor}@1",
                    "execution": execution, "context_call_id": call}},
                    {"target": "exomonad_actor::call_timing", "fields": {
                        "message": "call timing", "execution": execution, **terminal_fields, "actor": actor}}])
        receipt = {"schema": 1, "status": "installed", "profile": profile,
            "span_events": "none" if profile == "minimal" else "new_close", "filter_contract": 1,
            "writer": str(host_path), "process_id": 99}
        phases[0]["campaign_trace_capture"] = receipt
        applied = {key: value for key, value in receipt.items() if key not in ("schema", "status")}
        applied["message"] = "campaign trace profile applied"
        self.write_jsonl(host_path, [{"target": "tidepool::actor_host::startup", "fields": applied}, *added, *host])
        daemon = reporter.read_jsonl(daemon_path)
        applied_compiler = {"message": "compiler trace profile applied", "profile": profile,
            "raw_detail": "off" if profile == "minimal" else "debug", "span_events": "close",
            "filter_contract": 1, "writer": str(daemon_path), "process_id": 123}
        self.write_jsonl(daemon_path, [*daemon[:1],
            {"target": "tidepool_extract_cmd::daemon", "fields": applied_compiler},
            {"target": "tidepool_extract_cmd::daemon", "fields": {
                "message": "compiler daemon ready", "daemon_pid": 123}}, *daemon[1:]])
        self.write_jsonl(phase_path, phases)

    def test_applied_capture_requires_owner_receipts_and_actual_writers(self):
        for profile in ("minimal", "full"):
            record = self.make_complete_case(trace_profile=profile)
            paths = [self.root / "artifacts/phases.jsonl", self.root / "artifacts/host.jsonl",
                     self.root / "artifacts/compiler/compiler.jsonl"]
            original = [reporter.read_jsonl(path) for path in paths]
            self.assertEqual(reporter.analyze(record)["trace_capture"]["status"], "complete")
            for mutation in ("missing_receipt", "existing_custom", "wrong_host_writer", "missing_host_event",
                             "duplicate_host_event", "wrong_host_filter", "wrong_compiler_profile",
                             "wrong_compiler_detail", "wrong_compiler_pid", "missing_compiler_boot",
                             "boolean_schema", "boolean_filter", "boolean_compiler_filter"):
                with self.subTest(profile=profile, mutation=mutation):
                    phases, host, daemon = json.loads(json.dumps(original))
                    receipt = phases[0]["campaign_trace_capture"]
                    compiler = next(row["fields"] for row in daemon if row["fields"].get("message") == "compiler trace profile applied")
                    if mutation == "boolean_schema": receipt["schema"] = True
                    elif mutation == "boolean_filter": receipt["filter_contract"] = True
                    elif mutation == "boolean_compiler_filter": compiler["filter_contract"] = True
                    elif mutation == "missing_receipt": phases[0].pop("campaign_trace_capture")
                    elif mutation == "existing_custom": receipt["status"] = "existing_unverified"
                    elif mutation == "wrong_host_writer": receipt["writer"] = "/another/host.jsonl"
                    elif mutation == "missing_host_event": host = [row for row in host if row["fields"].get("message") != "campaign trace profile applied"]
                    elif mutation == "duplicate_host_event": host.append(host[0])
                    elif mutation == "wrong_host_filter": receipt["filter_contract"] = 2
                    elif mutation == "wrong_compiler_profile": compiler["profile"] = "full" if profile == "minimal" else "minimal"
                    elif mutation == "wrong_compiler_detail": compiler["raw_detail"] = "debug" if profile == "minimal" else "off"
                    elif mutation == "wrong_compiler_pid": compiler["process_id"] = 777
                    elif mutation == "missing_compiler_boot": daemon = [row for row in daemon if row["fields"].get("message") != "compiler daemon ready"]
                    for path, rows in zip(paths, (phases, host, daemon)): self.write_jsonl(path, rows)
                    self.assertEqual(reporter.analyze(record)["trace_capture"]["status"], "unverified_or_partial")

    def test_missing_profile_is_unknown_and_never_inferred_full(self):
        record = self.make_complete_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        rows = reporter.read_jsonl(phase_path)
        rows[0].pop("campaign_trace_profile")
        self.write_jsonl(phase_path, rows)
        payload = json.loads(record.read_text())
        payload["execution"].pop("trace_profile")
        self.write_json(record, payload)
        report = reporter.analyze(record)
        self.assertIsNone(report["trace_profile"]["name"])
        self.assertEqual(report["trace_capture"]["status"], "unverified_or_partial")

    def test_execution_mutations_refuse_without_weakening_exact_compiler_joins(self):
        record = self.make_complete_case()
        host_path = self.root / "artifacts/host.jsonl"
        original = reporter.read_jsonl(host_path)
        original[-1]["fields"]["workload_phase"] = "reuse-retained"
        for mutation in ("missing_dispatch", "duplicate_dispatch", "wrong_dispatch_actor", "missing_terminal",
                         "duplicate_terminal", "wrong_terminal_actor", "missing_count", "cancelled", "error",
                         "rejected", "reversed_clock", "execution_alias", "wrong_terminal_process"):
            with self.subTest(mutation=mutation):
                rows = json.loads(json.dumps(original))
                dispatch = next(row for row in rows if row["fields"].get("message") == "workbench cell dispatched to its actor"
                                and row["fields"].get("execution") == "exec-3")
                terminal = next(row for row in rows if row["fields"].get("message") == "call timing"
                                and row["fields"].get("execution") == "exec-3")
                if mutation == "missing_dispatch": rows.remove(dispatch)
                elif mutation == "duplicate_dispatch": rows.append(dispatch)
                elif mutation == "wrong_dispatch_actor": dispatch["fields"]["actor"] = "2@1"
                elif mutation == "missing_terminal": rows.remove(terminal)
                elif mutation == "duplicate_terminal": rows.append(terminal)
                elif mutation == "wrong_terminal_actor": terminal["fields"]["incarnation"] = 2
                elif mutation == "wrong_terminal_process": terminal["fields"]["process_id"] = 777
                elif mutation == "missing_count": terminal["fields"].pop("publication_attempts")
                elif mutation in ("cancelled", "error", "rejected"): terminal["fields"]["outcome"] = mutation
                elif mutation == "reversed_clock": terminal["fields"].update(start_ns=31, end_ns=30)
                elif mutation == "execution_alias":
                    next(row for row in rows if row["fields"].get("context_call_id") == "case-4")["fields"]["execution"] = "exec-3"
                self.write_jsonl(host_path, rows)
                report = reporter.analyze(record)
                self.assertEqual(report["workload_request_service_joins"]["status"],
                                 "partial_or_unknown" if mutation == "execution_alias" else "complete")
                self.assertEqual(report["execution_observations"]["status"], "partial_or_unknown")

    def test_terminal_count_mutations_preserve_unknown_instead_of_zero(self):
        record = self.make_complete_case()
        host_path = self.root / "artifacts/host.jsonl"
        original = reporter.read_jsonl(host_path)
        for field, invalid in itertools.product(("publication_attempts", "semantic_rebases", "metadata_restages"),
                                                (None, -1, True, False, "invalid", 1.5)):
            rows = json.loads(json.dumps(original))
            terminal = next(row for row in rows if row["fields"].get("message") == "call timing"
                            and row["fields"].get("execution") == "exec-3")
            terminal["fields"][field] = invalid
            self.write_jsonl(host_path, rows)
            self.assertEqual(reporter.analyze(record)["execution_observations"]["status"], "partial_or_unknown")

    def test_intrinsic_lifecycle_and_parked_terminal_applicability(self):
        record = self.make_complete_case("three-actor-capture")
        host_path = self.root / "artifacts/host.jsonl"
        rows = reporter.read_jsonl(host_path)
        parent = next(row for row in rows if row["fields"].get("message") == "call timing"
                      and row["fields"].get("execution") == "exec-3")
        rows.remove(parent)
        parent["fields"].update(start_ns=10, end_ns=100_000, wall_ns=99_990)
        self.write_jsonl(host_path, [*rows, parent])
        observation = reporter.analyze(record)["execution_observations"]
        self.assertEqual(observation["status"], "complete")
        phases = {row["phase"]: row for row in observation["phases"]}
        self.assertEqual(phases["async-yield"]["reason"], "engine_intrinsic")
        self.assertEqual(phases["host-cleanup"]["status"], "not_applicable")
        self.assertEqual(phases["root-fork-capture"]["status"], "complete")
        record = self.make_complete_case("prepare-once-repeated-launch")
        observation = reporter.analyze(record)["execution_observations"]
        self.assertEqual(observation["status"], "complete")
        self.assertTrue(all(row["status"] == "not_applicable" for row in observation["phases"]))

    def test_concurrent_phase_owns_each_exact_dispatch_once(self):
        phases = [{"phase": "both-children", "call_ids": ["alpha", "beta"]}]
        submissions = [{"phase_owners": [], "executions": [execution], "startup_owner_spans": []}
                       for execution in ("exec-alpha", "exec-beta")]
        reporter.assign_submission_phases(submissions, {"alpha": {"exec-alpha"}, "beta": {"exec-beta"}},
                                          phases, ["both-children"])
        self.assertEqual([row["phase_attribution"] for row in submissions], ["both-children", "both-children"])
        for invalid in (["alpha", "alpha"], ["alpha", None], "alpha", []):
            with self.subTest(calls=invalid), self.assertRaises(ValueError):
                reporter.phase_call_ids({"call_ids": invalid})

    def test_grouped_dispatch_distinguishes_exact_and_shared_execution(self):
        record = self.make_complete_case("two-children-concurrent")
        phase_path = self.root / "artifacts/phases.jsonl"
        phases = reporter.read_jsonl(phase_path)
        phase = next(row for row in phases if row["phase"] == "children-concurrent-replies")
        phase.pop("call_id", None)
        phase["call_ids"] = ["alpha", "beta"]
        self.write_jsonl(phase_path, phases)
        host_path = self.root / "artifacts/host.jsonl"
        original = reporter.read_jsonl(host_path)
        events = [{"target": "exomonad_actor::resident_tools", "fields": {
                   "message": "workbench cell dispatched to its actor", "context_call_id": call,
                   "execution": execution}}
                  for call, execution in (("alpha", "exec-alpha"), ("beta", "exec-beta"))]
        self.write_jsonl(host_path, original + events)
        result = reporter.analyze(record)
        self.assertEqual(next(row for row in result["phases"] if row["phase"] == phase["phase"])["dispatch_attribution"], "exact_group")
        events[1]["fields"]["execution"] = "exec-alpha"
        self.write_jsonl(host_path, original + events)
        result = reporter.analyze(record)
        self.assertNotEqual(next(row for row in result["phases"] if row["phase"] == phase["phase"])["dispatch_attribution"], "exact_group")
        with self.assertRaises(ValueError):
            reporter.phase_call_ids({"call_id": "alpha", "call_ids": ["alpha", "beta"]})

    def test_preparation_request_uses_its_issuing_owner(self):
        submissions = [{"phase_owners": [], "executions": [],
                        "startup_owner_spans": ["workspace_toolsets_prepare"]}]
        reporter.assign_submission_phases(submissions, {}, [], ["fresh-preparation", "launch-1"])
        self.assertEqual(submissions[0]["phase_attribution"], "fresh-preparation")

    def test_group_compiler_completeness_requires_each_declared_call(self):
        record = self.make_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        phases = reporter.read_jsonl(phase_path)
        phases[-1].pop("call_id")
        phases[-1].update(call_ids=["usecase-3", "beta"], logical_compiler_requests=2)
        self.write_jsonl(phase_path, phases)
        host_path = self.root / "artifacts/host.jsonl"
        host = reporter.read_jsonl(host_path)
        host.append({"target": "exomonad_actor::resident_tools", "fields": {
            "message": "workbench cell dispatched to its actor", "context_call_id": "usecase-3",
            "execution": "exec-other"}})
        other = json.loads(json.dumps(host[2]))
        other["fields"].update(request_ordinal=3, compile_request="digest-b")
        other["span"]["execution"] = "exec-other"
        host.append(other)
        self.write_jsonl(host_path, host)
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        compiler = reporter.read_jsonl(compiler_path)
        service = json.loads(json.dumps(compiler[0]))
        service["span"].update(request_ordinal=3, compile_request="digest-b")
        self.write_jsonl(compiler_path, compiler + [service])
        phase = reporter.analyze(record)["phases"][0]
        self.assertEqual(len(phase["dispatch_executions"]), 2)
        self.assertEqual(phase["compiler_attribution"], "partial")

    def test_physical_request_identity_binds_one_digest_across_both_streams(self):
        for ordinal, digest in ((3, "digest-a"), (3, "digest-b"), (2, "digest-b")):
            with self.subTest(ordinal=ordinal, digest=digest):
                record = self.make_case()
                host_path = self.root / "artifacts/host.jsonl"
                compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
                phase_path = self.root / "artifacts/phases.jsonl"
                host = reporter.read_jsonl(host_path)
                compiler = reporter.read_jsonl(compiler_path)
                request = json.loads(json.dumps(host[-1]))
                request["fields"].update(request_ordinal=ordinal, compile_request=digest)
                service = json.loads(json.dumps(compiler[0]))
                service["span"].update(request_ordinal=ordinal, compile_request=digest)
                self.write_jsonl(host_path, host + [request])
                self.write_jsonl(compiler_path, compiler + [service])
                phases = reporter.read_jsonl(phase_path)
                phases[-1]["logical_compiler_requests"] = 2
                self.write_jsonl(phase_path, phases)
                if ordinal == 2:
                    with self.assertRaisesRegex(ValueError, "rebound"):
                        reporter.analyze(record)
                else:
                    result = reporter.analyze(record)
                    self.assertEqual(result["phases"][0]["compiler_attribution"], "complete")
                    self.assertEqual(result["whole_physical_stream_reconciliation"]["status"], "complete")
        record = self.make_case()
        compiler_path = self.root / "artifacts/compiler/compiler.jsonl"
        compiler = reporter.read_jsonl(compiler_path)
        compiler[0]["span"]["compile_request"] = "digest-other-stream"
        self.write_jsonl(compiler_path, compiler)
        with self.assertRaisesRegex(ValueError, "rebound"):
            reporter.analyze(record)

    def test_physical_alias_must_agree_with_each_explicit_coordinate(self):
        for name in ("daemon_epoch", "admission_id", "request_ordinal"):
            row = {"fields": {"daemon_epoch": "epoch-a", "admission_id": 7,
                              "request_ordinal": 2, "compile_request": "digest-a",
                              "physical_execution": "epoch-a:7:2"}}
            self.assertEqual(reporter.request_identity(row), ("epoch-a", 7, 2, "digest-a"))
            row["fields"][name] = "epoch-other" if name == "daemon_epoch" else 99
            for resolver in (reporter.request_identity, reporter.admission_identity):
                with self.subTest(name=name, resolver=resolver.__name__), self.assertRaisesRegex(ValueError, "contradicts"):
                    resolver(row)
        self.assertEqual(reporter.request_identity({"fields": {
            "physical_execution": "epoch-a:7:2", "compile_request": "digest-a"}}),
            ("epoch-a", 7, 2, "digest-a"))

    def test_repeat_launch_zero_requests_requires_complete_counter_and_stream_evidence(self):
        record = self.make_complete_case("prepare-once-repeated-launch")
        phase_path = self.root / "artifacts/phases.jsonl"
        phases = reporter.read_jsonl(phase_path)
        for phase in phases[1:]:
            phase["logical_compiler_requests"] = 0
        self.write_jsonl(phase_path, phases)
        self.write_jsonl(self.root / "artifacts/host.jsonl", [])
        self.write_jsonl(self.root / "artifacts/compiler/compiler.jsonl", [])
        report = reporter.analyze(record)
        self.assertEqual(report["queue_trace_status"], "no_admissions_verified")
        self.assertEqual(report["compiler_job_grants"]["status"], "not_applicable_no_compiler_requests")
        self.assertEqual(report["owned_artifact_observations"]["status"], "partial_or_unknown")
        phases[-1]["logical_compiler_requests"] = 1
        self.write_jsonl(phase_path, phases)
        self.assertEqual(reporter.analyze(record)["queue_evidence"]["status"], "partial_or_unknown")
        phases[-1]["logical_compiler_requests"] = 0
        self.write_jsonl(phase_path, phases[:-1])
        self.assertEqual(reporter.analyze(record)["compiler_job_grants"]["status"], "partial_or_unknown")

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
                         "exact_declared_phase_or_issued_child_origin_or_root_startup_owner; no count or timestamp assignment")

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
                second_phase = {"harness-eight-phase": "first-arithmetic", "three-actor-capture": "child-alpha-reply",
                                "persona-workbench": "warm-source-display-first",
                                "two-children-concurrent": "children-concurrent-replies", "notebook-a-a-b-a": "changed-b",
                                "prepare-once-repeated-launch": "launch-1"}[cohort]
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
        retained = reporter.read_jsonl(host_path)
        base = [next(row for row in retained if row["fields"].get("message") == message
                     and (row["fields"].get("execution") == "exec-3" or message == "compiler request identified"))
                for message in ("workbench cell dispatched to its actor", "call timing", "compiler request identified")]
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

    def test_minimal_trace_profile_keeps_bounded_joins_without_runtime_detail(self):
        record = self.make_complete_case(trace_profile="minimal")
        report = reporter.analyze(record)
        self.assertEqual(report["trace_profile"]["name"], "minimal")
        self.assertEqual(report["phase_measurements"]["status"], "complete")
        self.assertEqual(report["workload_request_service_joins"]["status"], "complete")
        self.assertTrue(report["runner"]["cleanup"]["complete"])
        self.assertEqual(report["harness_runtime_cost_trace"]["status"], "not_collected_by_design")
        self.assertIsNone(report["phases"][0]["harness_runtime_cost_events"])
        self.assertEqual(report["owned_artifact_observations"]["status"], "partial_or_unknown")
        self.assertIsNone(report["owned_artifact_observations"]["actual_native_image_compiles_observed"])

    def test_owner_summary_separates_native_compile_install_and_buffered_validation(self):
        identity = {"daemon_epoch": "epoch", "admission_id": 4, "request_ordinal": 1,
                    "compile_request": "digest", "worker_pid": 123}
        validation = {"schema": 1, "invocation_id": 1, "worker_pid": 123,
                      "start_ns": 10, "end_ns": 20, "wall_ns": 10, "cpu_ns": 9,
                      "clock_domain": "ghc_monotonic_ns", "outcome": "success",
                      "rts_scope": "process_delta", "allocated_bytes": 400,
                      "counts": {"observed_file_count": 8}}
        host = [{"target": "tidepool_codegen::prepared_compile", "fields": {
                    "message": "prepared compile", "process_id": 99, "image_instance": 7}},
                *[{"target": "tidepool_codegen::image_install", "fields": {
                    "process_id": 99, "image_instance": 7, "outcome": "machine_published"}} for _ in range(20)]]
        daemon = [{"fields": {"message": "compiler request finished", "elapsed_ms": 2, "exit_code": 0}, "span": identity},
                  {"fields": {"line": "tidepool-validation " + json.dumps(validation)}, "span": identity}]
        result = reporter.owned_artifact_observations(host, daemon)
        self.assertEqual(result["status"], "observed")
        self.assertEqual(result["actual_native_image_compiles_observed"], 1)
        self.assertEqual(result["machine_installations_observed"], 20)
        self.assertEqual(result["validation_intervals"][0]["wall_interval_union_ns"], 10)
        self.assertEqual(result["validation_intervals"][0]["process_allocated_bytes_sum"], 400)
        self.assertIsNone(result["registry_dispositions_observed"])
        result = reporter.owned_artifact_observations(host, daemon + [daemon[-1]])
        self.assertEqual(result["status"], "partial_or_unknown")
        self.assertIn("duplicate validation invocation", result["problems"])
        validation["invocation_id"] = 2
        validation["start_ns"] = 15
        validation["end_ns"] = 25
        overlapping = {"fields": {"line": "tidepool-validation " + json.dumps(validation)}, "span": identity}
        result = reporter.owned_artifact_observations(host, daemon + [overlapping])
        self.assertEqual(result["validation_intervals"][0]["wall_interval_union_ns"], 15)
        self.assertIsNone(result["validation_intervals"][0]["process_allocated_bytes_sum"])
        overlapping["span"] = {**identity, "worker_pid": 124}
        result = reporter.owned_artifact_observations(host, [overlapping])
        self.assertEqual(result["status"], "partial_or_unknown")
        self.assertIn("validation lacks matching physical request/worker identity", result["problems"])

    def test_registry_wait_sum_preserves_unknown_decision_durations(self):
        def observe(*waits):
            rows = [{"target": "tidepool_codegen::image_registry", "fields": {
                "message": "native image registry decision", "process_id": 99,
                "image_registry": 3, "image_entry": 7, "shared_wait_ns": wait}}
                for wait in waits]
            return reporter.owned_artifact_observations(rows, [])[
                "registry_wait_ns_observed"]

        self.assertEqual(observe(0, 0), 0)
        self.assertEqual(observe(0, 5), 5)
        self.assertIsNone(observe(0, None))
        for invalid in (-1, True, 1.5, "invalid"):
            with self.subTest(invalid=invalid):
                self.assertIsNone(observe(0, invalid))
        self.assertIsNone(observe())

    def observe_validation(self, validation):
        identity = {"daemon_epoch": "epoch", "admission_id": 4, "request_ordinal": 1,
                    "compile_request": "digest", "worker_pid": 123}
        host = [{"target": "tidepool_codegen::prepared_compile", "fields": {
                    "message": "prepared compile", "process_id": 99, "image_instance": 7}},
                {"target": "tidepool_codegen::image_install", "fields": {
                    "process_id": 99, "image_instance": 7, "outcome": "machine_published"}}]
        daemon = [{"fields": {"message": "compiler request finished", "elapsed_ms": 2,
                               "exit_code": 0}, "span": identity},
                  {"fields": {"line": "tidepool-validation " + json.dumps(validation)},
                   "span": identity}]
        return reporter.owned_artifact_observations(host, daemon)

    def validation_case(self, start=10, duration=10, resource=400):
        return {"schema": 1, "invocation_id": 1, "worker_pid": 123,
                "start_ns": start, "end_ns": start + duration, "wall_ns": duration,
                "cpu_ns": resource, "clock_domain": "ghc_monotonic_ns", "outcome": "success",
                "rts_scope": "process_delta", "allocated_bytes": resource,
                "gc_cpu_ns": resource, "gc_elapsed_ns": resource,
                "counts": {"observed_file_count": resource}}

    def test_validation_summary_rejects_contradictory_duration_and_counter_scope(self):
        for changes in ({"wall_ns": 999}, {"allocated_bytes": -400},
                        {"rts_scope": "unavailable"}, {"rts_scope": None}):
            with self.subTest(changes=changes):
                result = self.observe_validation({**self.validation_case(), **changes})
                self.assertEqual(result["status"], "partial_or_unknown")
                self.assertTrue(result["problems"])
                self.assertEqual(result["validation_events"], [])
                self.assertEqual(result["validation_intervals"], [])

    def test_validation_summary_preserves_unavailable_optional_counters(self):
        for scope in (None, "unavailable", "process_delta"):
            for outcome in ("success", "refused", "exception"):
                value = self.validation_case()
                value["outcome"] = outcome
                for name in ("allocated_bytes", "gc_cpu_ns", "gc_elapsed_ns"):
                    value.pop(name)
                if scope is None:
                    value.pop("rts_scope")
                else:
                    value["rts_scope"] = scope
                result = self.observe_validation(value)
                self.assertEqual(result["status"], "observed")
                self.assertEqual(result["validation_events"][0]["outcome"], outcome)
                self.assertIsNone(result["validation_intervals"][0]["process_allocated_bytes_sum"])

    def test_generated_validation_admission_preserves_owner_relations(self):
        seed = 0xC10C
        rng = random.Random(seed)
        numbers = [(10**6, 10**5, 400), (0, 0, 0), (1, 1, 1), (2**63, 0, 2**64),
                   *[(rng.randrange(10**12), rng.randrange(10**9), rng.randrange(10**12))
                     for _ in range(48)]]
        controls = ("measured", "unavailable", "absent", "refused", "exception")
        faults = ("duration", "scope", "clock", "counts")
        counter_faults = tuple((name, invalid) for name in
                              ("allocated_bytes", "gc_cpu_ns", "gc_elapsed_ns")
                              for invalid in (-1, True, None, 1.5, "1"))

        def check(sample):
            start, duration, resource, mode = sample
            value = self.validation_case(start, duration, resource)
            if mode in ("unavailable", "absent"):
                for name in ("allocated_bytes", "gc_cpu_ns", "gc_elapsed_ns"):
                    value.pop(name)
                if mode == "absent":
                    value.pop("rts_scope")
                else:
                    value["rts_scope"] = "unavailable"
            elif mode in ("refused", "exception"):
                value["outcome"] = mode
            elif mode == "duration":
                value["wall_ns"] = duration + 1
            elif mode == "scope":
                value["rts_scope"] = "unavailable"
            elif mode == "clock":
                value["clock_domain"] = "logger_arrival"
            elif mode == "counts":
                value["counts"]["observed_file_count"] = -1
            elif isinstance(mode, tuple):
                value[mode[0]] = mode[1]
            result = self.observe_validation(value)
            if mode in controls:
                self.assertEqual(result["status"], "observed")
                interval = result["validation_intervals"][0]
                self.assertEqual(interval["wall_interval_union_ns"], duration)
                self.assertEqual(interval["process_allocated_bytes_sum"],
                                 None if mode in ("unavailable", "absent") else resource)
            else:
                self.assertEqual(result["status"], "partial_or_unknown")
                self.assertTrue(result["problems"])
                self.assertEqual(result["validation_events"], [])
                self.assertEqual(result["validation_intervals"], [])

        check_shrinking_samples(((*values, mode) for values in numbers
                                 for mode in (*controls, *faults, *counter_faults)), check)

    def test_phase_first_successor_cannot_follow_its_observed_terminal_output(self):
        record = self.make_complete_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        original = reporter.read_jsonl(phase_path)
        for successor in (0, 1000, 1001):
            phases = json.loads(json.dumps(original))
            phases[2]["first_successor_ns"] = successor
            self.write_jsonl(phase_path, phases)
            result = reporter.analyze(record)
            measurements = result["phase_measurements"]
            self.assertEqual(measurements["status"],
                             "complete" if successor <= 1000 else "partial_or_unknown")
            if successor > 1000:
                self.assertIn("first_successor_ns", measurements["invalid_records"][0]["invalid_fields"])
            self.assertTrue(result["phases"][0]["completed"])
        self.write_jsonl(phase_path, original)
        result = reporter.analyze(record)
        self.assertEqual(result["phase_measurements"]["status"], "complete")
        self.assertIsNone(result["phases"][0]["first_successor_ns"])

    def test_generated_phase_endpoint_ordering(self):
        record = self.make_complete_case()
        phase_path = self.root / "artifacts/phases.jsonl"
        original = reporter.read_jsonl(phase_path)
        seed = 0x51CCE55
        rng = random.Random(seed)
        durations = (10**12, 0, 1, 2**64, *(rng.randrange(10**12) for _ in range(24)))

        def check(sample):
            duration, fraction, mode = sample
            phases = json.loads(json.dumps(original))
            phase = phases[2]
            phase["wall_ns"] = duration
            if mode == "ordered":
                phase["first_successor_ns"] = duration * fraction // 100
            elif mode == "reversed":
                phase["first_successor_ns"] = duration + fraction + 1
            self.write_jsonl(phase_path, phases)
            report = reporter.analyze(record)
            self.assertEqual(report["phase_measurements"]["status"],
                             "partial_or_unknown" if mode == "reversed" else "complete")
            self.assertTrue(report["phases"][0]["completed"])
            if mode == "absent":
                self.assertIsNone(report["phases"][0]["first_successor_ns"])

        check_shrinking_samples(((duration, fraction, mode) for duration in durations
                                for fraction in (0, 1, 99, 100)
                                for mode in ("ordered", "reversed", "absent")), check)

    def test_publication_summary_is_joined_to_its_unique_settlement(self):
        record = self.make_case()
        host_path = self.root / "artifacts/host.jsonl"
        rows = reporter.read_jsonl(host_path)
        counters = {"publication_attempts": 3, "semantic_rebases": 1,
                    "metadata_restages": 1, "latest_join_reservation": 17}
        rows[1]["fields"].update(counters)
        self.write_jsonl(host_path, rows)
        result = reporter.analyze(record)["phases"][0]["publication_work"]
        self.assertEqual(result["status"], "observed")
        self.assertEqual(result["counts"], {key: counters[key] for key in
                         ("publication_attempts", "semantic_rebases", "metadata_restages")})
        self.assertEqual(result["latest_join_reservation"], 17)
        self.write_jsonl(host_path, rows + [rows[1]])
        ambiguous = reporter.analyze(record)["phases"][0]["publication_work"]
        self.assertEqual(ambiguous, {"status": "ambiguous_settlement"})

    def test_publication_summary_distinguishes_zero_and_unobserved(self):
        counts = {"publication_attempts": 0, "semantic_rebases": 0, "metadata_restages": 0}
        self.assertEqual(reporter.publication_work_observation([])["status"], "not_observed_unknown")
        self.assertEqual(reporter.publication_work_observation([counts])["counts"], counts)
        for field, invalid in itertools.product(counts, (None, -1, True, 1.5, "invalid")):
            with self.subTest(field=field, invalid=invalid):
                row = {**counts, field: invalid}
                observed = reporter.publication_work_observation([row])
                self.assertEqual(observed["status"], "partial_or_unknown")
                self.assertIsNone(observed["counts"][field])

    def test_bound_group_work_retains_shared_inventory_totals_without_summing(self):
        observations = [{"target": "exomonad_harness::timing", "fields": {
            "phase": "products.bound_native_demand",
            "message": "bound native demand inventory observation",
            "issued_native_groups": issued, "reused_native_groups": reused,
            "canonical_hits_total": hits, "canonical_comparisons_total": 2,
            "canonical_bytes_offered_total": 1024, "issued_scope_nodes_total": installed,
            "graph_visits_total": installed, "entry_handle_copies_total": 5},
            "span": {"name": "cell", "execution": execution}}
            for execution, issued, reused, hits, installed in
            (("exec-a", 3, 0, 10, 30), ("exec-b", 0, 3, 18, 34))]
        observed = reporter.owned_artifact_observations(observations, [])["bound_native_demand"]
        self.assertEqual(observed["status"], "observed")
        self.assertEqual([row["local_group_work"] for row in observed["observations"]],
                         [{"issued_native_groups": 3, "reused_native_groups": 0},
                          {"issued_native_groups": 0, "reused_native_groups": 3}])
        self.assertEqual([row["inventory_totals"]["issued_scope_nodes_total"]
                          for row in observed["observations"]], [30, 34])
        self.assertEqual([row["execution"] for row in observed["observations"]], ["exec-a", "exec-b"])
        self.assertNotIn("counts", observed)
        observations[1]["fields"].pop("issued_native_groups")
        partial = reporter.bound_native_demand_observations(observations)
        self.assertEqual(partial["status"], "partial_or_unknown")
        self.assertIsNone(partial["observations"][1]["local_group_work"]["issued_native_groups"])
        self.assertEqual(reporter.bound_native_demand_observations([])["status"], "not_observed_unknown")

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
