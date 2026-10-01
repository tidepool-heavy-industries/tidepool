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


def phase_report(phase, local=0, records=()):
    return {
        'phase': phase,
        'configuration': {'profile': 'production'},
        'primary_outputs': [{'path': '/output', 'sha256': 'same', 'bytes_hashed': 4}],
        'action_summary': {'local': local, 'remote': 0, 'cached': 0, 'other': 0},
        'what_ran_records': [{'identity': label} for label in records],
    }


class BuckCacheGateParserTests(unittest.TestCase):
    def parse_args(self, arguments):
        return gate.parse_args(arguments + [
            '--profile', 'production', '--expect-mutated-action', '//pkg:target',
            '--expect-unaffected-action', '//pkg:control',
        ])

    def test_finite_probe_commands_match_their_explicit_matrix(self):
        matrix = json.loads((SCRIPT.parent / 'buck-cache-probes.json').read_text())
        self.assertEqual([row['id'] for row in matrix['probes']], [
            'rust-production', 'test-fixture', 'haskell-helper', 'web', 'shared-schema',
        ])
        for row in matrix['probes']:
            with self.subTest(probe=row['id']):
                options = gate.parse_args(row['command_argv'][2:])
                self.assertEqual(str(options.root), row['checkout'])
                self.assertEqual(options.profile, row['profile'])
                self.assertEqual(str(options.probe_input), row['probe_input'])
                self.assertEqual(options.target, row['targets'])
                self.assertEqual(options.expect_mutated_action, row['expected_affected'])
                self.assertEqual(options.expect_unaffected_action, row['expected_unaffected'])
                self.assertEqual(options.mutation_bytes, row['append_text'].encode())

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
            self.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--probe-input', 'source.rs',
                '--append-hex', 'not-hex',
            ])
        with self.assertRaises(SystemExit):
            self.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--target', '//pkg:target',
                '--probe-input', 'source.rs', '--append-text', '// probe',
            ])
        with self.assertRaises(SystemExit):
            self.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--probe-input', 'source.rs',
                '--append-text', '',
            ])
        with self.assertRaises(SystemExit):
            self.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '', '--probe-input', 'source.rs', '--append-text', '// probe',
            ])
        with self.assertRaises(SystemExit):
            self.parse_args([
                '--root', '/tmp', '--evidence-dir', '/tmp/evidence',
                '--target', '//pkg:target', '--probe-input', 'source.rs',
                '--append-text', '// probe', '--build-timeout', 'nan',
            ])

    def test_cli_requires_profile_and_disjoint_affected_unaffected_expectations(self):
        arguments = [
            '--root', '/tmp', '--evidence-dir', '/tmp/evidence', '--target', '//pkg:target',
            '--probe-input', 'source.rs', '--append-text', '// probe',
        ]
        with self.assertRaises(SystemExit):
            gate.parse_args(arguments)
        with self.assertRaises(SystemExit):
            gate.parse_args(arguments + ['--profile', 'production'])
        with self.assertRaises(SystemExit):
            self.parse_args(arguments + ['--expect-unaffected-action', 'root//pkg:target'])
        self.assertEqual(self.parse_args(arguments).profile, 'production')

    def test_what_ran_accepts_only_the_known_buck_heading(self):
        record = json.dumps({'identity': 'root//pkg:target', 'reason': 'build'})
        self.assertEqual(len(gate.parse_action_records('Showing commands from: buck2 build\n' + record)), 1)
        self.assertEqual(gate.parse_action_records('Showing commands from: buck2 build\n'), [])
        with self.assertRaises(gate.ProbeError):
            gate.parse_action_records('unrecognized diagnostic\n' + record)

    def test_mutation_requires_execution_and_exact_affected_unaffected_labels(self):
        options = SimpleNamespace(expect_mutated_action=['//pkg:target'], expect_unaffected_action=['//pkg:control'])
        for report in (
            phase_report('mutated'),
            phase_report('mutated', local=1, records=['root//pkg:unrelated']),
            phase_report('mutated', local=1, records=['root//pkg:target', 'root//pkg:control']),
        ):
            with self.subTest(report=report), self.assertRaisesRegex(gate.ProbeError, 'mutation action mismatch'):
                gate.validate_mutation(options, report)
        gate.validate_mutation(options, phase_report('mutated', local=1, records=['root//pkg:target']))

    def test_phase_comparison_pins_configuration_and_outputs_but_allows_identical_mutation_outputs(self):
        baseline = phase_report('baseline')
        mutated = phase_report('mutated', local=1, records=['root//pkg:target'])
        gate.compare_phase(baseline, mutated)
        self.assertTrue(mutated['outputs_match_baseline'])
        mutated['primary_outputs'][0]['sha256'] = 'changed'
        gate.compare_phase(baseline, mutated)
        self.assertFalse(mutated['outputs_match_baseline'])
        for phase in ('warm', 'restored'):
            changed = phase_report(phase)
            changed['primary_outputs'][0]['sha256'] = 'changed'
            with self.assertRaisesRegex(gate.ProbeError, 'primary outputs differ'):
                gate.compare_phase(baseline, changed)
        changed = phase_report('mutated', local=1)
        changed['configuration']['profile'] = 'fast-dev'
        with self.assertRaisesRegex(gate.ProbeError, 'configuration differs'):
            gate.compare_phase(baseline, changed)
        with self.assertRaisesRegex(gate.ProbeError, 'warm build executed'):
            gate.compare_phase(baseline, phase_report('warm', local=1))

    def test_primary_output_digest_handles_files_directories_and_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / 'buck-out' / 'artifact'
            output.mkdir(parents=True)
            (output / 'file').write_bytes(b'content')
            (output / 'alias').symlink_to('file')
            report = gate.primary_outputs(str(output) + '\n', root)
            self.assertEqual(report[0]['bytes_hashed'], 7)
            self.assertEqual(report, gate.primary_outputs(str(output) + '\n', root))
            (output / 'file').write_bytes(b'changed')
            self.assertNotEqual(report, gate.primary_outputs(str(output) + '\n', root))
            with self.assertRaisesRegex(gate.ProbeError, 'did not report primary outputs'):
                gate.primary_outputs('BUILD SUCCEEDED\n', root)

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

    def test_restore_rechecks_concurrent_content_before_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'input.rs'
            path.write_bytes(b'baseline plus probe')
            original_chmod = os.chmod

            def edit_during_restore(temporary, mode):
                original_chmod(temporary, mode)
                path.write_bytes(b'concurrent edit')

            with patch.object(os, 'chmod', side_effect=edit_during_restore):
                restored = gate.restore_input(path, b'baseline', gate.sha256(b'baseline plus probe'), 0o644, 123)
            self.assertFalse(restored)
            self.assertEqual(path.read_bytes(), b'concurrent edit')

    def test_build_phase_records_pinned_profile_primary_outputs_and_command(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evidence = root / 'evidence'
            evidence.mkdir()
            for name in ('.buckconfig', '.buckconfig.local'):
                (root / name).write_text('configured')
            output = root / 'buck-out' / 'artifact'
            output.parent.mkdir()
            output.write_bytes(b'artifact')
            options = SimpleNamespace(profile='production', target=['//pkg:target'], build_timeout=60)
            commands = []

            def run(command, _root, _timeout):
                commands.append(command)
                if command[1] == 'build':
                    (evidence / 'baseline.build-id').write_text('build-id')
                    return 0, str(output) + '\n', False
                if 'summary' in command:
                    return 0, '- Local actions: 1\n- Remote actions: 0\n- Cached actions: 0\n- Other actions: 0\n', False
                return 0, 'Showing commands from: buck2 build\n' + json.dumps({
                    'identity': 'root//pkg:target', 'reason': 'build',
                }), False

            with patch.object(gate, 'run_command', side_effect=run):
                report = gate.run_build_phase(options, root, evidence, '/pinned/buck2', 'baseline')
            self.assertEqual(report['command'], commands[0])
            self.assertIn('tidepool.profile=production', report['command'])
            self.assertIn('remote.enabled=false', report['command'])
            self.assertIn('--local-only', report['command'])
            self.assertEqual(report['primary_outputs'][0]['bytes_hashed'], 8)
            self.assertEqual(report['configuration']['profile'], 'production')

    def test_missing_mutation_action_still_restores_exact_input_and_keeps_unrelated_wip(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'checkout'
            root.mkdir()
            probe = root / 'input.rs'
            baseline = b'baseline source\n'
            probe.write_bytes(baseline)
            os.chmod(probe, 0o640)
            timestamp = 1_700_000_000_123_456_789
            os.utime(probe, ns=(timestamp, timestamp))
            unrelated = root / 'other.rs'
            unrelated.write_bytes(b'unrelated dirty work')
            evidence = Path(directory) / 'evidence'
            options = SimpleNamespace(
                profile='production', target=['//pkg:target'], mutation_bytes=b'// probe\n',
                buck2='buck2', expect_mutated_action=['//pkg:target'], expect_unaffected_action=['//pkg:control'],
            )
            snapshot = {'head': 'baseline-head', 'full_status_porcelain': ' M other.rs\n', 'probe_status_porcelain': ''}
            phases = []

            def run_phase(_options, _root, _evidence, _executable, phase):
                phases.append(phase)
                return phase_report(phase)

            with (
                patch.object(gate, 'checked_paths', return_value=(root, evidence, probe, 'input.rs')),
                patch.object(gate, 'validate_checkout', return_value='/bin/true'),
                patch.object(gate, 'git_snapshot', return_value=snapshot),
                patch.object(gate, 'input_status', return_value=''),
                patch.object(gate, 'run_build_phase', side_effect=run_phase),
                patch.object(subprocess, 'run', return_value=SimpleNamespace(stdout='baseline-head')),
            ):
                with self.assertRaisesRegex(gate.ProbeError, 'mutation action mismatch'):
                    gate.run_probe(options)
            self.assertEqual(phases, ['baseline', 'warm', 'mutated', 'restored'])
            self.assertEqual(probe.read_bytes(), baseline)
            self.assertEqual(probe.stat().st_mode & 0o777, 0o640)
            self.assertEqual(probe.stat().st_mtime_ns, timestamp)
            self.assertEqual(unrelated.read_bytes(), b'unrelated dirty work')
            report = json.loads((evidence / 'gate-report.json').read_text())
            self.assertTrue(report['probe_restored'])

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
                build_timeout=60, buck2='buck2', profile='production',
                expect_mutated_action=['//pkg:target'], expect_unaffected_action=['//pkg:control'],
            )
            snapshot = {
                'head': 'baseline-head', 'full_status_porcelain': '',
                'probe_status_porcelain': '',
            }

            def run_phase(_options, _root, _evidence, _executable, phase):
                if phase == 'restored':
                    gate._interrupt_probe(signal.SIGTERM, None)
                    gate.check_interrupted()
                return phase_report(phase, local=1 if phase == 'mutated' else 0,
                                    records=['root//pkg:target'] if phase == 'mutated' else [])

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
