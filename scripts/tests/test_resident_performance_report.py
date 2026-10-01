import importlib.util
from pathlib import Path
import unittest
import tempfile
import hashlib

SPEC = importlib.util.spec_from_file_location(
    "resident_performance", Path(__file__).parents[1] / "resident-performance-report.py"
)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class ResidentPerformanceReport(unittest.TestCase):
    def fixture(self):
        manifest = {key: "f" * 64 for key in (
            "source_oid", "command", "binary_sha256", "frontend_sha256",
            "worker_sha256", "compiler_producer",
        )}
        manifest["source_oid"] = "a" * 40
        manifest["command"] = ["frozen-fixture", "--exact"]
        manifest["exit_code"] = 0
        binary = tempfile.NamedTemporaryFile()
        binary.write(b"frozen pair bytes")
        binary.flush()
        self.addCleanup(binary.close)
        manifest["binary_path"] = binary.name
        for key in ("binary_sha256", "frontend_sha256", "worker_sha256"):
            manifest[key] = hashlib.sha256(b"frozen pair bytes").hexdigest()
        events = []
        for index in range(5):
            events.append({"fields": {
                "message": "compiler daemon ready", "daemon_epoch": str(index),
                "producer": "f" * 64, "daemon_pid": 200 + index,
                "executable": binary.name, "worker": binary.name,
            }})
        samples = []
        for index in range(50):
            events.append({"fields": {
                "message": "compiler request finished", "daemon_epoch": "0",
                "compile_request": str(index), "transport": "daemon", "exit_code": 0,
                "worker_pid": 300, "daemon_pid": 200, "served": index + 1,
                "followed_rotation": False, "elapsed_ms": 10,
            }})
            samples.append({
                "schema": 1, "composition": "engine-store", "kind": "warm_cell",
                "index": index, "elapsed_ns": 100_000_000, "completed": True,
                "displayed": True, "workload": str(index % 10),
                "source_digest": f"{index % 10:064x}",
                "daemon_epoch": "0", "compiler_requests": [str(index)],
            })
            samples.append({
                "schema": 1, "composition": "engine-store", "kind": "cancel_ack",
                "index": index, "elapsed_ns": 10_000_000, "completed": True,
                "daemon_epoch": "0", "started_ns": 20_000_000,
                "settled_ns": 30_000_000, "effect_active_ns": 10_000_000,
                "acknowledged": True, "operation_id": f"operation-{index}",
            })
        for index in range(5):
            samples.append({
                "schema": 1, "composition": "engine-store", "kind": "cold_start",
                "index": index, "elapsed_ns": 1_000_000_000, "completed": True,
                "daemon_epoch": str(index), "started_ns": 0,
                "settled_ns": 1_000_000_000, "packaged": True,
                "host_pid": 500 + index, "readiness": "workspace-ready",
                "host_sha256": manifest["binary_sha256"], "host_executable": binary.name,
            })
        return samples, events, manifest

    def test_matched_resident_evidence_is_accepted(self):
        self.assertTrue(REPORT.analyze(*self.fixture())["accepted"])

    def test_missing_worker_evidence_fails_even_with_fast_samples(self):
        samples, events, manifest = self.fixture()
        events[5]["fields"].pop("worker_pid")
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cold_worker_cannot_be_counted_as_warm(self):
        samples, events, manifest = self.fixture()
        events[5]["fields"]["served"] = 0
        events[5]["fields"]["followed_rotation"] = True
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_neighboring_epoch_or_private_session_cannot_close_product_gate(self):
        samples, events, manifest = self.fixture()
        samples[0]["daemon_epoch"] = "unobserved"
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples[0]["composition"] = "private-session"
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_each_cold_start_must_meet_its_limit(self):
        samples, events, manifest = self.fixture()
        samples[-1]["elapsed_ns"] = 10_000_000_001
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cancel_requires_actual_active_effect_and_unique_operation(self):
        samples, events, manifest = self.fixture()
        samples[1].pop("effect_active_ns")
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples[1]["effect_active_ns"] = 10_000_000
        samples[3]["operation_id"] = samples[1]["operation_id"]
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_cold_requires_exact_boundaries_and_packaged_host(self):
        samples, events, manifest = self.fixture()
        samples[-1]["settled_ns"] += 1
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples[-1]["settled_ns"] -= 1
        samples[-1]["packaged"] = False
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_actual_binary_bytes_and_source_variation_are_required(self):
        samples, events, manifest = self.fixture()
        manifest["frontend_sha256"] = "0" * 64
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])
        samples, events, manifest = self.fixture()
        for row in samples:
            if row["kind"] == "warm_cell":
                row["source_digest"] = "0" * 64
        self.assertFalse(REPORT.analyze(samples, events, manifest)["accepted"])

    def test_worker_phase_costs_and_io_counts_remain_separate_from_wall_clock(self):
        samples, events, manifest = self.fixture()
        for line in (
            "tidepool-timing phase=exact_iface_decode ms=12",
            "tidepool-timing phase=exact_iface_decode ms=23",
            "tidepool-count name=exact_iface_decode_reads.Session.Val.G1 count=2",
            "private diagnostic without a machine counter",
        ):
            events.append({"fields": {"message": "compiler timing", "line": line}})
        report = REPORT.analyze(samples, events, manifest)
        self.assertEqual(report["compiler_phases_ms"]["exact_iface_decode"], {
            "count": 2, "total_ms": 35, "p95_ms": 23,
        })
        self.assertEqual(report["compiler_counts"], {"exact_iface_decode_reads.Session.Val.G1": 2})
        self.assertIsNone(report["worker_peak_observed_rss_mb"])
        self.assertTrue(report["accepted"])

    def test_nearest_rank_and_empty_evidence(self):
        self.assertEqual(REPORT.percentile(list(range(1, 51)), 0.95), 48)
        self.assertIsNone(REPORT.percentile([], 0.95))
        self.assertFalse(REPORT.analyze([], [], {})["accepted"])


if __name__ == "__main__":
    unittest.main()
