#!/usr/bin/env python3
"""Independent interval/identity controls; these are not native qualification."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import json

SPEC = importlib.util.spec_from_file_location("contention", Path(__file__).with_name("compiler-warm-contention.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def envelope(index, workload, start, end):
    base = {"physical_execution": f"epoch:{index}:1", "compile_request": f"request-{index}",
            "compiler_workload": workload, "worker": 0 if workload == "foreground" else 1,
            "admission_id": index, "daemon_epoch": "epoch", "request_ordinal": 1,
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


class EvidenceControls(unittest.TestCase):
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
