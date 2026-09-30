import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

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


if __name__ == '__main__':
    unittest.main()
