#!/usr/bin/env python3
"""Independent interval/identity controls; these are not native qualification."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import json
import subprocess
import sys
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location("contention", Path(__file__).with_name("compiler-warm-contention.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def envelope(index, workload, start, end):
    base = {"physical_execution": f"epoch:{index + 1}:1", "compile_request": f"request-{index}",
            "compiler_workload": workload, "worker": 0 if workload == "foreground" else 1,
            "admission_id": index + 1, "daemon_epoch": "epoch", "request_ordinal": 1,
            "compiler_jobs": 2, "compiler_capabilities": 2}
    return [{**base, "message": "compiler request started", "observed_timestamp": start},
            {**base, "message": "compiler request finished", "observed_timestamp": end, "exit_code": 0}]


def workload():
    rows = []
    for index in range(102):
        second = 0 if index < 2 else 2
        rows.extend(envelope(index, "foreground", f"2026-10-06T00:00:0{second}.100Z",
                             f"2026-10-06T00:00:0{second}.200Z"))
    return rows


def service_report(owner, *, complete=True):
    def observation(status, code=None):
        return {"status": status, "exit_code": code,
                "success": code == 0 if code is not None else None,
                "signal": None, "failure": None}
    return {"schema": 2, "kind": "owned-compiler-service", "owner_directory": str(owner),
            "operation": "01" * 32, "daemon_epoch": "02" * 32, "daemon_pid": 73,
            "producer": "03" * 32, "endpoint": "04" * 32, "socket_path": str(owner / "socket"),
            "action": observation("exited", 0), "acquisition": {"status": "not_configured"},
            "daemon_reap": observation("reaped", 0), "service_terminal": {
                "epoch": [2] * 32, "workers_retired": True, "socket_retired": True, "failures": []},
            "source_settlement": observation("not_configured"),
            "artifact_retention": observation("retained"),
            "settlement_failures": [], "publication_failures": [], "complete": complete}


def write_report(output, report):
    owner = output / "compiler"
    owner.mkdir(mode=0o700, exist_ok=True)
    path = owner / "owned-service.json"
    path.write_text(json.dumps(report))
    path.chmod(0o600)


def successful_measurement(output):
    owner = output / "compiler"
    write_report(output, service_report(owner))
    rows = workload() + envelope(200, "preparation", "2026-10-06T00:00:02.150Z",
                                  "2026-10-06T00:00:02.250Z")
    (owner / "compiler.jsonl").write_text("".join(json.dumps({
        "timestamp": row["observed_timestamp"], "fields": row}) + "\n" for row in rows))
    capture = output / "authored-sources"
    capture.mkdir()
    (capture / "capture.json").write_text(json.dumps({"observed_distinct_measured_source_sha256": 50}))


class EvidenceControls(unittest.TestCase):
    def test_campaign_consumes_complete_report_and_preserves_failed_obligations(self):
        for kind in ("complete", "incomplete", "missing", "malformed", "legacy", "crossed_owner",
                     "wrong_endpoint", "failed_action", "startup_failed", "action_failure",
                     "reap_failure", "terminal_failure"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary)
                successful_measurement(output)
                owner = output / "compiler"
                report = service_report(owner)
                if kind in ("missing", "legacy"):
                    (owner / "owned-service.json").unlink()
                    if kind == "legacy":
                        (owner / "lifecycle.json").write_text(json.dumps({"cleanup_confirmed": True}))
                elif kind == "malformed":
                    (owner / "owned-service.json").write_text("{partial")
                else:
                    if kind == "incomplete":
                        report["complete"] = False
                    elif kind == "crossed_owner":
                        report["owner_directory"] = str(output / "other-owner")
                    elif kind == "wrong_endpoint":
                        report["endpoint"] = "04" * 96
                    elif kind == "failed_action":
                        report["action"].update(exit_code=17, success=False)
                    elif kind == "startup_failed":
                        report["action"].update(status="failed", exit_code=None, success=None,
                                                failure="startup refused")
                        report["daemon_reap"].update(exit_code=1, success=False)
                        report["service_terminal"]["failures"] = ["startup refused"]
                    elif kind == "action_failure":
                        report["action"]["failure"] = "workload observation failed"
                    elif kind == "reap_failure":
                        report["daemon_reap"].update(exit_code=1, success=False)
                    elif kind == "terminal_failure":
                        report["service_terminal"]["failures"] = ["worker startup refused"]
                    write_report(output, report)
                result = MODULE.summarize_campaign(output, 0, {"owner": {"exit_code": 0}}, None)
                self.assertEqual(result, 0 if kind == "complete" else 1)
                summary = json.loads((output / "summary.json").read_text())
                observed = summary["compiler_service"]
                if kind in ("missing", "malformed", "legacy", "crossed_owner", "wrong_endpoint"):
                    self.assertEqual(observed["status"], "unknown")
                    self.assertIsNone(observed["report"])
                    self.assertTrue(observed["error"]["type"])
                else:
                    self.assertEqual(observed["status"], "incomplete" if kind == "incomplete" else "complete")
                    self.assertEqual(observed["report"], report)
                if kind != "complete":
                    self.assertEqual(summary["status"], "failed")

    def test_primary_failure_preserves_independent_service_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            report = service_report(output / "compiler", complete=False)
            write_report(output, report)
            result = MODULE.summarize_campaign(output, 17, {"owner": {"exit_code": 17}}, "primary failure")
            self.assertEqual(result, 1)
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(summary["exception"], "primary failure")
            self.assertEqual(summary["compiler_service"]["status"], "incomplete")
            self.assertEqual(summary["compiler_service"]["report"], report)

    def test_stop_uses_valid_live_owner_without_claiming_retirement(self):
        for stop_code in (0, 1):
            with self.subTest(stop_code=stop_code), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary)
                report = service_report(output / "compiler", complete=False)
                report["action"].update(status="running", exit_code=None, success=None)
                report["daemon_reap"].update(status="pending", exit_code=None, success=None)
                report["service_terminal"] = None
                write_report(output, report)
                owner = Mock()
                owner.poll.return_value = None
                owner.wait.side_effect = [subprocess.TimeoutExpired("owner", 1), 0]
                with patch.object(MODULE.subprocess, "run", return_value=subprocess.CompletedProcess(
                        [], stop_code, b"stop observation", b"")) as stop:
                    settlement = MODULE.settle_compiler_owner(owner, output, {"TIDEPOOL_EXTRACT": "/frontend"}, 1)
                self.assertEqual(stop.call_args.args[0], ["/frontend", "--stop-daemon", "--socket", report["socket_path"]])
                self.assertFalse(settlement["forced_kill"])
                self.assertEqual(settlement["stop_control"]["status"], "exited")
                self.assertEqual(settlement["stop_control"]["compiler_service"]["status"], "incomplete")
                self.assertEqual(settlement.get("owner_forced_termination", False), stop_code != 0)
                if stop_code:
                    owner.terminate.assert_called_once()
                    self.assertEqual(settlement["stop_control"]["exit_code"], stop_code)
                self.assertIsNone(report["service_terminal"])

    def test_missing_owner_report_cannot_address_an_ambient_socket(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            owner = Mock()
            owner.wait.side_effect = [subprocess.TimeoutExpired("owner", 1), 0]
            owner.poll.return_value = None
            with patch.object(MODULE.subprocess, "run") as stop:
                settlement = MODULE.settle_compiler_owner(owner, output, {"TIDEPOOL_EXTRACT": "/frontend"}, 1)
            stop.assert_not_called()
            owner.terminate.assert_called_once()
            self.assertTrue(settlement["owner_forced_termination"])
            self.assertEqual(settlement["stop_control"]["status"], "unavailable")
            self.assertEqual(settlement["stop_control"]["compiler_service"]["status"], "unknown")

    def test_actual_intersection_and_reserved_slot(self):
        rows = workload() + envelope(200, "preparation", "2026-10-06T00:00:02.150Z",
                                      "2026-10-06T00:00:02.250Z")
        report = MODULE.analyze(rows)
        self.assertEqual(report["actual_foreground_requests"], 102)
        self.assertEqual(report["actual_preparation_requests"], 1)
        self.assertEqual(len(report["overlap"]), 100)

    def test_no_overlap_is_inconclusive(self):
        rows = workload() + envelope(200, "preparation", "2026-10-06T00:00:03.100Z",
                                      "2026-10-06T00:00:03.200Z")
        self.assertEqual(MODULE.analyze(rows)["status"], "inconclusive_no_overlap")

    def test_missing_null_duplicate_and_changed_identity_refuse(self):
        for change in (lambda rows: rows[0].pop("compile_request"),
                       lambda rows: rows[0].update(compile_request=None),
                       lambda rows: rows.append(rows[0]),
                       lambda rows: rows[1].update(compiler_jobs=4),
                       lambda rows: rows[1].update(exit_code=1),
                       lambda rows: rows.pop()):
            rows = workload()
            change(rows)
            with self.assertRaises(ValueError):
                MODULE.analyze(rows)

    def test_preparation_cannot_occupy_reserved_slot(self):
        rows = workload() + envelope(200, "preparation", "2026-10-06T00:00:02.100Z",
                                      "2026-10-06T00:00:02.200Z")
        rows[-2]["worker"] = rows[-1]["worker"] = 0
        with self.assertRaises(ValueError):
            MODULE.analyze(rows)

    def test_paired_malformed_ownership_and_tuple_refuse(self):
        for field in ("worker", "admission_id", "request_ordinal", "daemon_epoch"):
            for malformed in (None, False, [], ""):
                rows = workload()
                rows[0][field] = rows[1][field] = malformed
                with self.assertRaises(ValueError):
                    MODULE.analyze(rows)
        rows = workload()
        rows[0]["physical_execution"] = rows[1]["physical_execution"] = "different:1:1"
        with self.assertRaises(ValueError):
            MODULE.analyze(rows)

    def test_local_helper_is_reaped_after_stop_and_forced_fallback(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(10)"])
        row = MODULE.settle(child, grace=1, stop=True)
        self.assertIsNotNone(child.poll())
        self.assertFalse(row["forced_kill"])
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(10)"])
        row = MODULE.settle(child, grace=.01)
        self.assertIsNotNone(child.poll())
        self.assertTrue(row["forced_kill"])

    def test_partial_live_line_waits_but_final_line_refuses(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "trace.jsonl"
            line = json.dumps({"timestamp": "2026-10-06T00:00:00Z", "fields": workload()[0]})
            path.write_text(line)
            trace = MODULE.Trace(path)
            trace.poll()
            self.assertEqual(trace.offset, 0)
            with self.assertRaises(ValueError):
                trace.poll(final=True)
            path.write_text(line + "\n")
            trace.poll(final=True)
            self.assertEqual(len(trace.rows), 1)


if __name__ == "__main__":
    unittest.main()
