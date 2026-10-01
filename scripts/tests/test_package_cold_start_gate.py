import importlib.util
import argparse
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "package-cold-start-gate.py"
SPEC = importlib.util.spec_from_file_location("package_cold_start_gate", SCRIPT)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class ColdStartObservationTests(unittest.TestCase):
    def test_status_waits_for_ready_phase_and_exact_run_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "status.json"
            status = {
                "run_id": "run-1",
                "session": "session-1",
                "workspace": temporary,
                "phase": {"state": "awaiting_binding"},
            }
            path.write_text(json.dumps(status))
            self.assertIsNone(gate.parse_status(path, "run-1", "session-1"))
            status["phase"] = {"state": "embedded_ready"}
            path.write_text(json.dumps(status))
            self.assertEqual(gate.parse_status(path, "run-1", "session-1"), status)
            self.assertIsNone(gate.parse_status(path, "another-run", "session-1"))
            self.assertIsNone(gate.parse_status(path, "run-1", "another-session"))
            path.write_text("{malformed")
            self.assertIsNone(gate.parse_status(path, "run-1", "session-1"))

    def test_snapshot_requires_the_exact_idle_root_and_conversation(self):
        actor = {"id": 9, "incarnation": 3}
        identity = {"actor": "/actors/a9_i3", "incarnation": "3"}
        ready = {
            "type": "snapshot",
            "snapshot": {
                "actors": [{"identity": identity, "parent": None, "lifecycle": "waiting"}],
                "conversations": [{"id": "/actors/a9_i3", "state": "idle"}],
            },
        }
        self.assertEqual(gate.validate_idle_snapshot(ready, actor), ready)
        for malformed in (
            {},
            {"type": "snapshot", "snapshot": {"actors": [], "conversations": []}},
            {"type": "snapshot", "snapshot": {
                "actors": [{"identity": identity, "parent": None, "lifecycle": "requesting"}],
                "conversations": [{"id": "/actors/a9_i3", "state": "idle"}],
            }},
            {"type": "snapshot", "snapshot": {
                "actors": [{"identity": identity, "parent": "/actors/parent", "lifecycle": "waiting"}],
                "conversations": [{"id": "/actors/a9_i3", "state": "idle"}],
            }},
            {"type": "snapshot", "snapshot": {
                "actors": [{"identity": identity, "parent": None, "lifecycle": "waiting"}],
                "conversations": [{"id": "/actors/a9_i3", "state": "running"}],
            }},
        ):
            with self.subTest(malformed=malformed), self.assertRaises(gate.GateError):
                gate.validate_idle_snapshot(malformed, actor)

    def test_observation_requires_authenticated_idle_root_snapshot(self):
        status = {
            "run_id": "run-2",
            "session": "session-2",
            "workspace": "/frozen/workspace",
            "phase": {
                "state": "embedded_ready",
                "root_actor": {"id": 9, "incarnation": 3},
                "browser_address": "127.0.0.1:4321",
            },
        }
        probe = mock.Mock(return_value={"type": "snapshot"})
        with mock.patch.object(gate, "parse_status", return_value=status), \
                mock.patch.object(gate, "embedded_settings", return_value=("private-secret", "127.0.0.1:0", "https")):
            observed_status, snapshot = gate.observe_ready(Path("status.json"), "run-2", "session-2", probe)
        self.assertEqual(observed_status, status)
        self.assertEqual(snapshot, {"type": "snapshot"})
        probe.assert_called_once_with("127.0.0.1:4321", "private-secret",
                                      {"id": 9, "incarnation": 3}, "https")

    def test_awaiting_binding_and_ready_are_not_embedded_readiness(self):
        for state in ("awaiting_binding", "ready"):
            status = {
                "run_id": "run-3",
                "session": "session-3",
                "workspace": "/frozen/workspace",
                "phase": {"state": state},
            }
            with self.subTest(state=state), \
                    mock.patch.object(gate, "parse_status", return_value=status), \
                    mock.patch.object(gate, "embedded_settings") as settings:
                with self.assertRaises(gate.GateError):
                    gate.observe_ready(Path("status.json"), "run-3", "session-3")
                settings.assert_not_called()

    def test_compiler_boot_requires_one_actual_ready_trace_row(self):
        fields = {
            "message": "compiler daemon ready",
            "run_id": "run-4",
            "daemon_epoch": "f00d",
            "daemon_pid": 1004,
            "producer": "producer-hash",
            "executable": "/pkg/bin/tidepool-extract",
            "worker": "/pkg/bin/tidepool-extract-worker",
        }
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "compiler.jsonl"
            path.write_text(json.dumps({"fields": fields}) + "\n")
            self.assertEqual(gate.compiler_boot(path, "run-4"), fields)
            path.write_text(json.dumps({"fields": {**fields, "daemon_epoch": ""}}) + "\n")
            with self.assertRaises(gate.GateError):
                gate.compiler_boot(path, "run-4")

    def test_provider_attempt_count_comes_from_read_only_store_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "store.sqlite"
            connection = sqlite3.connect(path)
            connection.executescript("""
                CREATE TABLE schema_version(version INTEGER NOT NULL);
                INSERT INTO schema_version VALUES (5);
                CREATE TABLE decisions(hook TEXT NOT NULL);
                CREATE TABLE events(kind TEXT NOT NULL);
                INSERT INTO decisions VALUES ('before-request');
                INSERT INTO decisions VALUES ('after-request');
                INSERT INTO events VALUES ('model_turn');
                INSERT INTO events VALUES ('responses_usage');
            """)
            connection.commit()
            connection.close()

            evidence = gate.provider_store_evidence(path)

            self.assertTrue(evidence["verified"])
            self.assertEqual(evidence["method"], "harness-store-decisions-before-request")
            self.assertEqual(evidence["provider_request_attempts"], 1)
            self.assertEqual(evidence["completed_model_turns"], 1)
            self.assertEqual(evidence["responses_usage_events"], 1)
            self.assertTrue(evidence["files"])

    def test_provider_evidence_fails_closed_for_missing_or_inconsistent_store(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaises(gate.GateError):
                gate.provider_store_evidence(root / "missing.sqlite")

            path = root / "store.sqlite"
            connection = sqlite3.connect(path)
            connection.executescript("""
                CREATE TABLE schema_version(version INTEGER NOT NULL);
                INSERT INTO schema_version VALUES (5);
                CREATE TABLE decisions(hook TEXT NOT NULL);
                CREATE TABLE events(kind TEXT NOT NULL);
                INSERT INTO events VALUES ('model_turn');
            """)
            connection.commit()
            connection.close()
            with self.assertRaises(gate.GateError):
                gate.provider_store_evidence(path)

    def test_partial_init_run_pointer_is_retired_through_packaged_stop(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workspace = root / "workspace"
            output = root / "output"
            (workspace / ".exomonad" / "sessions").mkdir(parents=True)
            output.mkdir()
            args = argparse.Namespace(
                package_entrypoint=Path("/frozen/package/bin/exomonad"),
                workspace=workspace,
            )
            report = {"samples": [], "runners": {"cold_start": {"process_execution_count": 0}}}
            process = mock.Mock()
            process.poll.return_value = 17
            process.wait.return_value = 17

            def partial_init(argv, **kwargs):
                session = argv[argv.index("--session") + 1]
                pointer = workspace / ".exomonad" / "sessions" / session / "run-id"
                pointer.parent.mkdir(parents=True)
                pointer.write_text("run-partial-17\n")
                return process

            with mock.patch.object(gate.subprocess, "Popen", side_effect=partial_init), \
                    mock.patch.object(gate, "state_root", return_value=root / "state"), \
                    mock.patch.object(gate, "stop_run", return_value=(
                        [str(args.package_entrypoint), "stop", "--run-id", "run-partial-17",
                         "--session", "placeholder"], 0)) as stop:
                with self.assertRaises(gate.GateError):
                    gate.run_one(0, args, output, report)

            stop.assert_called_once()
            self.assertEqual(stop.call_args.args[1], "run-partial-17")
            self.assertTrue(stop.call_args.args[2].startswith("cold-start-0-"))
            self.assertEqual(stop.call_args.args[3].name, "sample-0")
            sample = report["samples"][0]
            self.assertEqual(report["runners"]["cold_start"]["process_execution_count"], 1)
            self.assertEqual(sample["run_id"], "run-partial-17")
            self.assertEqual(sample["stop_exit_code"], 0)
            self.assertIsNone(sample["provider_requests"])
            self.assertFalse(sample["provider_request_evidence"]["verified"])

    def test_live_executable_hash_reads_the_running_process_inode(self):
        executable = Path(os.readlink("/proc/self/exe")).resolve(strict=True)
        self.assertEqual(gate.live_executable_sha256(os.getpid(), executable), gate.sha256(executable))

    def test_live_compiler_identity_checks_pid_against_trace_executable(self):
        executable = Path(os.readlink("/proc/self/exe")).resolve(strict=True)
        worker = Path(__file__).resolve(strict=True)
        identity = gate.live_compiler_identity({
            "daemon_pid": os.getpid(),
            "executable": str(executable),
            "worker": str(worker),
        })
        self.assertEqual(identity["daemon_pid"], os.getpid())
        self.assertEqual(identity["compiler_executable"], str(executable))
        self.assertEqual(identity["compiler_executable_sha256"], gate.sha256(executable))
        self.assertEqual(identity["compiler_worker_sha256"], gate.sha256(worker))
        with self.assertRaises(gate.GateError):
            gate.live_compiler_identity({
                "daemon_pid": os.getpid(),
                "executable": str(worker),
                "worker": str(worker),
            })

    def test_lingering_init_observer_is_terminated_and_reaped(self):
        class HangingObserver:
            def __init__(self):
                self.exit_code = None
                self.waits = 0
                self.terminated = False

            def poll(self):
                return self.exit_code

            def wait(self, timeout):
                self.waits += 1
                if self.exit_code is None:
                    raise gate.subprocess.TimeoutExpired("init", timeout)
                return self.exit_code

            def terminate(self):
                self.terminated = True
                self.exit_code = -15

        observer = HangingObserver()
        exit_code, failure = gate.reap_init_observer(observer, None)
        self.assertTrue(observer.terminated)
        self.assertEqual(observer.waits, 2)
        self.assertEqual(exit_code, -15)
        self.assertEqual(failure, "packaged init observer remained live after readiness")

    def test_failed_process_spawn_does_not_count_as_init_execution(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workspace = root / "workspace"
            output = root / "output"
            workspace.mkdir()
            output.mkdir()
            args = argparse.Namespace(
                package_entrypoint=Path("/frozen/package/bin/exomonad"),
                workspace=workspace,
            )
            report = {"samples": [], "runners": {"cold_start": {"process_execution_count": 0}}}
            with mock.patch.object(gate.subprocess, "Popen", side_effect=OSError("exec failed")):
                with self.assertRaises(gate.GateError):
                    gate.run_one(0, args, output, report)
            self.assertEqual(report["runners"]["cold_start"]["process_execution_count"], 0)
            self.assertEqual(len(report["samples"]), 1)


if __name__ == "__main__":
    unittest.main()
