import importlib.util
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "package-cold-start-gate.py"
SPEC = importlib.util.spec_from_file_location("package_cold_start_gate", SCRIPT)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class ColdStartObservationTests(unittest.TestCase):
    def test_only_missing_session_pointer_is_absent(self):
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            self.assertIsNone(gate.read_session_run_id(workspace, 'session'))
            pointer = workspace / '.exomonad/sessions/session/run-id'
            pointer.parent.mkdir(parents=True)
            pointer.write_text('valid-run-123\n')
            self.assertEqual(gate.read_session_run_id(workspace, 'session'), 'valid-run-123')
            for content in (b'', b'\xff', b'../unknown'):
                pointer.write_bytes(content)
                with self.assertRaises(gate.GateError):
                    gate.read_session_run_id(workspace, 'session')
            for error in (PermissionError('denied'), OSError('I/O failure')):
                with mock.patch.object(Path, 'read_text', side_effect=error):
                    with self.assertRaisesRegex(gate.GateError, 'unreadable'):
                        gate.read_session_run_id(workspace, 'session')

    def test_sample_completion_is_bound_to_stopped_trace_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            trace = root / "compiler.jsonl"
            trace.write_text('{"fields":{"message":"compiler daemon ready"}}\n')
            sample = gate.seal_sample({"failure": None, "daemon_trace": str(trace)})
            self.assertTrue(sample["completed"])
            self.assertEqual(sample["daemon_trace_sha256"], gate.sha256(trace))
            trace.write_text("changed\n")
            self.assertNotEqual(sample["daemon_trace_sha256"], gate.sha256(trace))
            missing = gate.seal_sample({"failure": None, "daemon_trace": str(root / "missing")})
            self.assertFalse(missing["completed"])
            self.assertIn("trace unavailable", missing["failure"])

    def test_unverified_bundle_refuses_before_component_or_startup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workspace = root / "workspace"
            workspace.mkdir()
            output = root / "evidence"
            argv = [str(SCRIPT), "--descriptor", str(root / "qualification.json"),
                    "--workspace", str(workspace), "--output", str(output)]
            with mock.patch.object(gate.sys, "argv", argv), \
                    mock.patch.object(gate, "FrozenOperator", side_effect=gate.GateError("frozen refusal")), \
                    mock.patch.object(gate, "run_one") as run:
                self.assertEqual(gate.main(), 1)
            run.assert_not_called()
            runner = json.loads((output / "report.json").read_text())["runners"]["cold_start"]
            self.assertNotIn("native_qualification", runner)
            self.assertEqual(runner["process_execution_count"], 0)
            self.assertEqual(runner["exit_code"], 1)

    def schema_operator(self):
        operator = gate.FrozenOperator.__new__(gate.FrozenOperator)
        operator.descriptor = {"harness_revision": "a" * 40}
        return operator

    def test_schema_authority_comes_from_frozen_compiled_component(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            operator = self.schema_operator()
            def component(arguments, report):
                self.assertEqual(arguments, ["harness-store-schema"])
                report.write_text('{"exit_code":0,"command":["/frozen/host","harness-store-schema"]}')
                return subprocess.CompletedProcess(arguments, 0, '{"version":12}\n', '')
            with mock.patch.object(operator, "run", side_effect=component):
                authority = operator.store_schema(output)
            self.assertEqual(authority["version"], 12)
            self.assertEqual(authority["revision"], "a" * 40)
            self.assertEqual(authority["process_report_sha256"], gate.sha256(output / "store-schema.process.json"))
            self.assertEqual(json.loads((output / "store-schema.json").read_text()), {"version": 12})

    def test_schema_authority_refuses_malformed_component_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            operator = self.schema_operator()
            for content in ('not-json', '{"version":true}', '{"version":0}',
                            '{"version":4294967296}', '{"version":12,"extra":1}'):
                with self.subTest(content=content), mock.patch.object(operator, "run", return_value=
                        subprocess.CompletedProcess([], 0, content, '')):
                    with self.assertRaises(gate.GateError):
                        operator.store_schema(Path(temporary))
            with mock.patch.object(operator, "run", return_value=subprocess.CompletedProcess([], 9, '{"version":12}', '')):
                with self.assertRaisesRegex(gate.GateError, 'could not report'):
                    operator.store_schema(Path(temporary))

    def test_main_attempts_exactly_five_startups_after_compiled_schema_admission(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "host"
            binary.write_bytes(b'actual frozen bytes')
            workspace = root / "workspace"
            workspace.mkdir()
            output = root / "evidence"
            operator = mock.Mock()
            operator.provenance.return_value = {'profile':'fast-dev', 'descriptor':str(root / 'qualification.json')}
            operator.descriptor = {'programs':{'host':str(binary), 'host_elf':str(binary)}}
            operator.store_schema.return_value = {'version':12}
            def startup(index, args, output, report):
                self.assertEqual(args.harness_store_schema, 12)
                report['runners']['cold_start']['process_execution_count'] += 1
                return {'index':index, 'completed':True}
            argv = [str(SCRIPT), '--descriptor', str(root / 'qualification.json'),
                    '--workspace', str(workspace), '--output', str(output)]
            with mock.patch.object(gate.sys, 'argv', argv), \
                 mock.patch.object(gate, 'FrozenOperator', return_value=operator), \
                 mock.patch.object(gate, 'embedded_settings'), \
                 mock.patch.object(gate, 'run_one', side_effect=startup) as run:
                self.assertEqual(gate.main(), 0)
            self.assertEqual([call.args[0] for call in run.call_args_list], list(range(5)))
            retained = json.loads((output / 'report.json').read_text())
            self.assertEqual(retained['runners']['cold_start']['process_execution_count'], 5)
            self.assertEqual(retained['runners']['cold_start']['native_qualification']['profile'], 'fast-dev')

    def test_read_only_store_refuses_schema_other_than_projected_owner(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'store.sqlite'
            with sqlite3.connect(path) as connection:
                connection.executescript('CREATE TABLE schema_version(version INTEGER NOT NULL);'
                                         'INSERT INTO schema_version VALUES(8);'
                                         'CREATE TABLE decisions(hook TEXT NOT NULL);'
                                         'CREATE TABLE events(kind TEXT NOT NULL);')
            before = path.read_bytes()
            evidence = gate.provider_store_evidence(path, 8)
            self.assertEqual(evidence['store_schema_version'], 8)
            with self.assertRaisesRegex(gate.GateError, 'schema version'):
                gate.provider_store_evidence(path, 5)
            self.assertEqual(path.read_bytes(), before)

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

            evidence = gate.provider_store_evidence(path, 5)

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
                gate.provider_store_evidence(root / "missing.sqlite", 5)

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
                gate.provider_store_evidence(path, 5)

    def test_partial_init_run_pointer_is_retired_through_packaged_stop(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workspace = root / "workspace"
            output = root / "output"
            (workspace / ".exomonad" / "sessions").mkdir(parents=True)
            output.mkdir()
            args = argparse.Namespace(
                harness_store_schema=8,
                operator=mock.Mock(),
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
                return mock.Mock(process=process, command=argv, started_ns=gate.time.monotonic_ns())

            with mock.patch.object(args.operator, "launch", side_effect=partial_init), \
                    mock.patch.object(gate, "state_root", return_value=root / "state"), \
                    mock.patch.object(gate, "stop_run", return_value=(
                        ["/frozen/host", "stop", "--run-id", "run-partial-17",
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

    def test_startup_observation_retains_real_store_and_rejects_provider_attempts(self):
        for attempts in (0, 1):
            with self.subTest(attempts=attempts), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                workspace, output = root / 'workspace', root / 'output'
                workspace.mkdir()
                output.mkdir()
                binary, compiler, worker = root / 'host', root / 'compiler', root / 'worker'
                for path in (binary, compiler, worker):
                    path.write_text(path.name)
                run_root = root / 'state/tidepool/exomonad/runs/observed-run'
                (run_root / 'harness').mkdir(parents=True)
                connection = sqlite3.connect(run_root / 'harness/store.sqlite')
                with connection:
                    connection.executescript('CREATE TABLE schema_version(version INTEGER NOT NULL);'
                        'INSERT INTO schema_version VALUES(12); CREATE TABLE decisions(hook TEXT NOT NULL);'
                        'CREATE TABLE events(kind TEXT NOT NULL);')
                    if attempts:
                        connection.execute("INSERT INTO decisions VALUES('before-request')")
                connection.close()
                logs = workspace / '.exomonad/logs'
                logs.mkdir(parents=True)
                boot = {'message':'compiler daemon ready', 'run_id':'observed-run', 'daemon_epoch':'epoch',
                        'daemon_pid':123, 'producer':'producer', 'executable':str(compiler), 'worker':str(worker)}
                (logs / 'observed-run-compiler.jsonl').write_text(json.dumps({'fields':boot}) + '\n')
                operator = mock.Mock()
                operator.environment = {'TIDEPOOL_EXTRACT':str(compiler), 'TIDEPOOL_EXTRACT_WORKER':str(worker)}
                process = mock.Mock(returncode=0)
                process.poll.return_value = 0
                def launch(arguments, **kwargs):
                    session = arguments[arguments.index('--session') + 1]
                    pointer = workspace / '.exomonad/sessions' / session / 'run-id'
                    pointer.parent.mkdir(parents=True)
                    pointer.write_text('observed-run\n')
                    (run_root / 'status.json').write_text(json.dumps({'run_id':'observed-run', 'session':session,
                        'workspace':str(workspace), 'phase':{'state':'embedded_ready'}}))
                    return mock.Mock(command=[str(binary), *arguments], process=process, started_ns=gate.time.monotonic_ns())
                operator.launch.side_effect = launch
                args = argparse.Namespace(operator=operator, workspace=workspace, harness_store_schema=12)
                report = {'samples':[], 'runners':{'cold_start':{'process_execution_count':0, 'binary_sha256':gate.sha256(binary)}}}
                with mock.patch.object(gate, 'state_root', return_value=root / 'state'), \
                     mock.patch.object(gate, 'observe_ready', return_value=({}, {})) as browser, \
                     mock.patch.object(gate, 'host_pid', return_value=321), \
                     mock.patch.object(gate, 'retained_host_executable', return_value=binary), \
                     mock.patch.object(gate, 'live_executable_sha256', return_value=gate.sha256(binary)), \
                     mock.patch.object(gate, 'live_compiler_identity', return_value={
                         'compiler_executable_sha256':gate.sha256(compiler), 'compiler_worker_sha256':gate.sha256(worker)}), \
                     mock.patch.object(gate, 'stop_run', return_value=(['frozen-host', 'stop'], 0)) as stop:
                    if attempts:
                        with self.assertRaises(gate.GateError):
                            gate.run_one(0, args, output, report)
                        sample = report['samples'][0]
                        self.assertFalse(sample['completed'])
                    else:
                        sample = gate.run_one(0, args, output, report)
                        self.assertTrue(sample['completed'])
                    browser.assert_called_once()
                    stop.assert_called_once()
                self.assertEqual(sample['provider_requests'], attempts)
                self.assertTrue(sample['provider_request_evidence']['verified'])
                self.assertEqual(report['runners']['cold_start']['process_execution_count'], 1)
                self.assertEqual(sample['daemon_trace_sha256'], gate.sha256(logs / 'observed-run-compiler.jsonl'))

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
                harness_store_schema=8,
                operator=mock.Mock(),
                workspace=workspace,
            )
            report = {"samples": [], "runners": {"cold_start": {"process_execution_count": 0}}}
            with mock.patch.object(args.operator, "launch", side_effect=OSError("exec failed")):
                with self.assertRaises(gate.GateError):
                    gate.run_one(0, args, output, report)
            self.assertEqual(report["runners"]["cold_start"]["process_execution_count"], 0)
            self.assertEqual(len(report["samples"]), 1)


if __name__ == "__main__":
    unittest.main()
