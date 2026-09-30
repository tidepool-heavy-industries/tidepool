import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'buck-cache-gate.py'
spec = importlib.util.spec_from_file_location('buck_cache_gate', SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class BuckCacheGateParserTests(unittest.TestCase):
    def test_summary_distinguishes_no_action_cache_hit_and_execution(self):
        no_action = gate.parse_action_summary(
            'Build Summary\nActions\n- Local actions: 0\n- Remote actions: 0\n'
            '- Cached actions: 0\n- Other actions: 0\n'
        )
        self.assertEqual(no_action.classification, 'no-action')

        cached = gate.parse_action_summary(
            '- Local actions: 0\n- Remote actions: 0\n'
            '- Cached actions: 2\n- Other actions: 0\n'
        )
        self.assertEqual(cached.classification, 'action-cache-hit')

        local = gate.parse_action_summary(
            '- Local actions: 1\n- Remote actions: 0\n'
            '- Cached actions: 0\n- Other actions: 0\n'
        )
        self.assertEqual(local.classification, 'actions-executed')

        mixed = gate.parse_action_summary(
            '- Local actions: 1\n- Remote actions: 0\n'
            '- Cached actions: 2\n- Other actions: 0\n'
        )
        self.assertEqual(mixed.classification, 'mixed-cache-hit-and-execution')

    def test_summary_requires_all_action_categories(self):
        with self.assertRaisesRegex(gate.ProbeError, 'missing action counts'):
            gate.parse_action_summary('- Local actions: 1\n- Cached actions: 0\n')

    def test_what_ran_json_extracts_identity_sources_and_build_id(self):
        record = {
            'identity': 'root//tidepool/runtime:tidepool_runtime (platform#abc) (rustc)',
            'reason': 'build',
            'reproducer': {
                'executor': 'Local',
                'details': {
                    'env': {
                        'BUCK_BUILD_ID': 'build-42',
                        'SRCS': 'src/lib.rs src/session/workbench.rs',
                    }
                },
            },
        }
        parsed = gate.parse_action_records(json.dumps(record), 'build-42')
        self.assertEqual(len(parsed), 1)
        self.assertEqual(parsed[0].identity, 'root//tidepool/runtime:tidepool_runtime')
        self.assertEqual(parsed[0].reason, 'build')
        self.assertEqual(parsed[0].executor, 'Local')
        self.assertIn('workbench.rs', parsed[0].sources)

    def test_what_ran_rejects_bad_json_identity_and_cross_build_records(self):
        with self.assertRaisesRegex(gate.ProbeError, 'invalid what-ran JSON'):
            gate.parse_action_records('{broken')
        with self.assertRaisesRegex(gate.ProbeError, 'lacks an action identity'):
            gate.parse_action_records(json.dumps({'reason': 'build'}))

        record = {
            'identity': 'root//pkg:target',
            'reason': 'build',
            'reproducer': {'details': {'env': {'BUCK_BUILD_ID': 'other-build'}}},
        }
        with self.assertRaisesRegex(gate.ProbeError, 'expected build-42'):
            gate.parse_action_records(json.dumps(record), 'build-42')

    def test_what_ran_accepts_json_document_and_rejects_non_object_records(self):
        record = {'identity': 'root//pkg:target', 'reason': 'build'}
        self.assertEqual(len(gate.parse_action_records(json.dumps([record]))), 1)
        with self.assertRaisesRegex(gate.ProbeError, 'not a JSON object'):
            gate.parse_action_records(json.dumps([record, 'not-an-action']))

    def test_cli_requires_a_nonempty_mutation_and_unique_targets(self):
        with self.assertRaises(SystemExit):
            gate.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--probe-input', 'source.rs',
                '--append-hex', 'not-hex',
            ])
        with self.assertRaises(SystemExit):
            gate.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--target', '//pkg:target',
                '--probe-input', 'source.rs', '--append-text', '// probe',
            ])
        with self.assertRaises(SystemExit):
            gate.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--probe-input', 'source.rs',
                '--append-text', '',
            ])
        with self.assertRaises(SystemExit):
            gate.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '', '--probe-input', 'source.rs', '--append-text', '// probe',
            ])
        with self.assertRaises(SystemExit):
            gate.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--probe-input', 'source.rs',
                '--append-text', '// probe', '--build-timeout', 'nan',
            ])

    def test_restore_recovers_probe_bytes_mode_and_timestamp(self):
        baseline = b'original source\n'
        suffix = b'// cache probe\n'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'input.rs'
            path.write_bytes(baseline + suffix)
            os.chmod(path, 0o640)
            timestamp = 1_700_000_000_123_456_789
            os.utime(path, ns=(timestamp, timestamp))

            restored = gate.restore_input(
                path, baseline, gate.sha256(baseline + suffix), 0o640, timestamp,
            )

            self.assertTrue(restored)
            self.assertEqual(path.read_bytes(), baseline)
            self.assertEqual(path.stat().st_mode & 0o777, 0o640)
            self.assertEqual(path.stat().st_mtime_ns, timestamp)

    def test_restore_repairs_metadata_when_baseline_bytes_already_exist(self):
        baseline = b'original source\n'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'input.rs'
            path.write_bytes(baseline)
            os.chmod(path, 0o600)
            timestamp = 1_700_000_000_123_456_789

            restored = gate.restore_input(
                path, baseline, gate.sha256(baseline + b'probe'), 0o640, timestamp,
            )

            self.assertTrue(restored)
            self.assertEqual(path.read_bytes(), baseline)
            self.assertEqual(path.stat().st_mode & 0o777, 0o640)
            self.assertEqual(path.stat().st_mtime_ns, timestamp)

    def test_restore_preserves_concurrent_content_instead_of_overwriting(self):
        baseline = b'original source\n'
        suffix = b'// cache probe\n'
        concurrent = b'concurrent edit\n'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'input.rs'
            path.write_bytes(concurrent)

            restored = gate.restore_input(
                path, baseline, gate.sha256(baseline + suffix), 0o644, 1_700_000_000_123_456_789,
            )

            self.assertFalse(restored)
            self.assertEqual(path.read_bytes(), concurrent)

    def test_run_command_does_not_wait_on_pipe_held_by_escaped_descendant(self):
        with tempfile.TemporaryDirectory() as directory:
            pid_path = Path(directory) / 'descendant.pid'
            child = (
                'import os,time; '
                f'open({str(pid_path)!r}, "w").write(str(os.getpid())); '
                'time.sleep(30)'
            )
            leader = f'''import os,subprocess,sys,time
subprocess.Popen([sys.executable, "-c", {child!r}], start_new_session=True)
deadline=time.monotonic()+2
while not os.path.exists({str(pid_path)!r}) and time.monotonic()<deadline:
    time.sleep(.01)
assert os.path.exists({str(pid_path)!r})
print("leader finished")
'''

            started = time.monotonic()
            status, output, timed_out = gate.run_command(
                [sys.executable, '-c', leader], Path(directory), 3,
            )

            self.assertEqual(status, 0, output)
            self.assertFalse(timed_out)
            self.assertIn('leader finished', output)
            self.assertLess(time.monotonic() - started, 2)
            self.assertTrue(pid_path.exists())
            os.kill(int(pid_path.read_text()), signal.SIGKILL)

    def test_run_command_signal_cleanup_is_idempotent_and_reaps_child(self):
        old_handler = signal.signal(signal.SIGTERM, gate._interrupt_probe)
        previous_interrupt = gate.INTERRUPTED_BY
        gate.INTERRUPTED_BY = None
        timer = threading.Timer(0.1, lambda: os.kill(os.getpid(), signal.SIGTERM))
        timer.start()
        try:
            with self.assertRaises(gate.ProbeInterrupted) as raised:
                gate.run_command(
                    [sys.executable, '-c', 'import time; time.sleep(30)'],
                    Path.cwd(), 5,
                )
            self.assertEqual(raised.exception.signum, signal.SIGTERM)
            gate._interrupt_probe(signal.SIGINT, None)
            self.assertEqual(gate.INTERRUPTED_BY, signal.SIGTERM)
        finally:
            timer.join(timeout=2)
            gate.INTERRUPTED_BY = previous_interrupt
            signal.signal(signal.SIGTERM, old_handler)

    def test_signal_during_restored_build_returns_interrupted_after_restore(self):
        previous_interrupt = gate.INTERRUPTED_BY
        gate.INTERRUPTED_BY = None
        self.addCleanup(setattr, gate, 'INTERRUPTED_BY', previous_interrupt)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'checkout'
            root.mkdir()
            probe = root / 'input.rs'
            baseline = b'baseline source\n'
            probe.write_bytes(baseline)
            evidence = Path(directory) / 'evidence'
            relative = 'input.rs'
            options = SimpleNamespace(
                target=['//pkg:target'], mutation_bytes=b'// probe\n',
                build_timeout=60, buck2='buck2', expect_mutated_action=[],
                expect_unaffected_action=[],
            )
            snapshot = {
                'head': 'baseline-head', 'full_status_porcelain': '',
                'probe_status_porcelain': '',
            }

            def run_phase(_options, _root, _evidence, _executable, phase):
                if phase == 'restored':
                    gate._interrupt_probe(signal.SIGTERM, None)
                    gate.check_interrupted()
                return {'phase': phase, 'what_ran_records': []}

            completed = SimpleNamespace(stdout='mock')
            with (
                patch.object(gate, 'checked_paths', return_value=(root, evidence, probe, relative)),
                patch.object(gate, 'validate_checkout', return_value='/bin/true'),
                patch.object(gate, 'git_snapshot', return_value=snapshot),
                patch.object(gate, 'input_status', return_value=''),
                patch.object(gate, 'run_build_phase', side_effect=run_phase),
                patch.object(
                    subprocess, 'run',
                    side_effect=lambda command, **_kwargs: SimpleNamespace(
                        stdout='baseline-head' if 'rev-parse' in command else completed.stdout,
                    ),
                ),
            ):
                with self.assertRaises(gate.ProbeInterrupted) as raised:
                    gate.run_probe(options)

            self.assertEqual(raised.exception.signum, signal.SIGTERM)
            self.assertEqual(probe.read_bytes(), baseline)
            report = json.loads((evidence / 'gate-report.json').read_text())
            self.assertTrue(report['probe_restored'])
            self.assertTrue(report['restored_build_skipped'])
        gate.INTERRUPTED_BY = previous_interrupt


if __name__ == '__main__':
    unittest.main()
