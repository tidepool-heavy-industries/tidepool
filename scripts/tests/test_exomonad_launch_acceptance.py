"""Fixture configuration and refusal cleanup without live user-service changes."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

SCRIPT = Path(__file__).resolve().parents[2] / 'exomonad/scripts/tests/exomonad_launch_acceptance.py'
SPEC = importlib.util.spec_from_file_location('native_launch_acceptance', SCRIPT)
acceptance = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(acceptance)
CONFIG = '''[launch]
systemd_slice = "original.slice"
[launch.embedded]
listen = "127.0.0.1:0"
session_secret_file = "/private/session-secret"
credential_file = "/private/provider-credentials"
context_capacity_tokens = 32768
'''


class LaunchAcceptanceTests(unittest.TestCase):
    def test_fixture_override_changes_only_native_slice(self):
        original = acceptance.tomllib.loads(CONFIG)
        changed = acceptance.tomllib.loads(acceptance.slice_configuration(CONFIG, 'refusal.slice'))
        self.assertEqual(changed['launch']['systemd_slice'], 'refusal.slice')
        self.assertEqual(changed['launch']['embedded'], original['launch']['embedded'])
        for text, name in ((CONFIG, '../invalid.slice'), ('[launch]\n', 'valid.slice')):
            with self.assertRaises(acceptance.gate.GateError):
                acceptance.slice_configuration(text, name)

    def test_refusal_workspace_uses_production_new_and_external_native_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            work = output / 'refusal'
            operator = Mock()
            def create(arguments, report, timeout):
                self.assertEqual(arguments, ['new', str(work)])
                (work / '.exomonad').mkdir(parents=True)
                return subprocess.CompletedProcess(arguments, 0, '', '')
            operator.run.side_effect = create
            acceptance.refusal_workspace(operator, CONFIG, work, 'missing.slice', output)
            native = acceptance.tomllib.loads((work / '.exomonad/config.toml').read_text())
            self.assertEqual(native['launch']['embedded']['credential_file'], '/private/provider-credentials')
            self.assertFalse((output / 'codex-0').exists())

    def test_wrong_slice_refusal_must_leave_shared_service_unchanged(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            env = {'XDG_RUNTIME_DIR': str(output / 'runtime')}
            operator = Mock()
            operator.run.return_value = subprocess.CompletedProcess([], 1, '',
                'shared command service is outside the selected slice')
            with patch.object(acceptance, 'service', side_effect=['MainPID=123\n', 'MainPID=456\n']), \
                 patch.object(acceptance, 'refusal_workspace'), \
                 patch.object(acceptance, 'run') as control:
                with self.assertRaisesRegex(AssertionError, 'disturbed'):
                    acceptance.verify_wrong_service_slice(operator, CONFIG, output, env)
            self.assertTrue(any(call.args[:3] == ('systemctl', '--user', 'stop') for call in control.call_args_list))
            self.assertEqual(list((output / 'runtime/systemd/user').glob('*.slice')), [])

    def test_unexpected_wrong_slice_admission_is_retired_before_failure_returns(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            env = {'XDG_RUNTIME_DIR': str(output / 'runtime')}
            operator = Mock()
            operator.run.side_effect = [subprocess.CompletedProcess([], 0, '', ''),
                                        subprocess.CompletedProcess([], 0, '', '')]
            with patch.object(acceptance, 'service', return_value='MainPID=123\n'), \
                 patch.object(acceptance, 'refusal_workspace'), \
                 patch.object(acceptance.gate, 'read_session_run_id', return_value='unexpected-run'), \
                 patch.object(acceptance, 'run'):
                with self.assertRaisesRegex(AssertionError, 'admitted'):
                    acceptance.verify_wrong_service_slice(operator, CONFIG, output, env)
            self.assertEqual(operator.run.call_args_list[-1].args[0][:3], ['stop', '--run-id', 'unexpected-run'])

    def test_invalid_wrong_slice_identity_still_removes_owned_slice(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            env = {'XDG_RUNTIME_DIR': str(output / 'runtime')}
            operator = Mock()
            operator.run.return_value = subprocess.CompletedProcess([], 1, '',
                'shared command service is outside the selected slice')
            with patch.object(acceptance, 'service', return_value='MainPID=123\n'), \
                 patch.object(acceptance, 'refusal_workspace'), \
                 patch.object(acceptance.gate, 'read_session_run_id', side_effect=acceptance.gate.GateError('invalid identity')), \
                 patch.object(acceptance, 'run') as control:
                with self.assertRaisesRegex(acceptance.gate.GateError, 'invalid identity'):
                    acceptance.verify_wrong_service_slice(operator, CONFIG, output, env)
            self.assertEqual(list((output / 'runtime/systemd/user').glob('*.slice')), [])
            self.assertEqual(control.call_args.args[:3], ('systemctl', '--user', 'daemon-reload'))

    def test_invalid_partial_identity_does_not_skip_known_run_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            env = {'XDG_RUNTIME_DIR': str(output / 'runtime')}
            operator = Mock()
            operator.run.return_value = subprocess.CompletedProcess([], 0, '', '')
            with patch.object(acceptance.gate, 'read_session_run_id', side_effect=acceptance.gate.GateError('invalid identity')), \
                 patch.object(acceptance, 'run') as control, \
                 patch.object(acceptance, 'service', return_value='MainPID=0\n'):
                with self.assertRaisesRegex(acceptance.gate.GateError, 'identity is unconfirmed'):
                    acceptance.cleanup_runs(operator, [('known-run', 'known-session')], output, env,
                                            [(output / 'unknown-workspace', 'unknown-session')])
            self.assertEqual(operator.run.call_args.args[0][:3], ['stop', '--run-id', 'known-run'])
            self.assertEqual(control.call_args.args[:3], ('systemctl', '--user', 'stop'))
            self.assertFalse(json.loads((output / 'cleanup.json').read_text())['confirmed'])

    def test_cleanup_failure_is_retained_and_cannot_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            env = {'XDG_RUNTIME_DIR': str(output / 'runtime')}
            operator = Mock()
            operator.run.return_value = subprocess.CompletedProcess([], 7, '', '')
            with patch.object(acceptance, 'run'), \
                 patch.object(acceptance, 'service', return_value='MainPID=99\n'):
                with self.assertRaises(acceptance.gate.GateError):
                    acceptance.cleanup_runs(operator, [('owned-run', 'owned-session')], output, env)
            receipt = json.loads((output / 'cleanup.json').read_text())
            self.assertFalse(receipt['confirmed'])
            self.assertEqual(len(receipt['failures']), 2)
            self.assertFalse((output / 'result.json').exists())


if __name__ == '__main__':
    unittest.main()
