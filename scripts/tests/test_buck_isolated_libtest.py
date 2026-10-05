import contextlib
import importlib.util
import io
import json
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'build/rust/isolated-libtest.py'
spec = importlib.util.spec_from_file_location('isolated_libtest', SCRIPT)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class IsolatedLibtestTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.binary = Path(self.tmp.name) / 'test-binary'
        self.binary.write_text('placeholder')
        self.all_tests = 'suite::works: test\nsuite::also_works: test\nsuite::ignored: test\n'
        self.ignored_tests = 'suite::ignored: test\n'

    def tearDown(self):
        self.tmp.cleanup()

    def test_regeneration_modes_refuse_before_discovery_or_execution(self):
        for name in ('TIDEPOOL_REGEN_BRIDGED', 'TIDEPOOL_REGEN_PROTOCOL_GOLDENS'):
            with self.subTest(name=name), patch.dict(os.environ, {name: '1'}), \
                 patch.object(runner, 'execute') as execute, \
                 contextlib.redirect_stderr(io.StringIO()) as errors:
                self.assertEqual(runner.main([str(self.binary)]), 2)
                execute.assert_not_called()
                self.assertIn(name, errors.getvalue())

    def test_delegated_command_exports_declared_inputs_and_only_test_process(self):
        record = {}
        with patch.dict(os.environ, {'TIDEPOOL_EXTRACT': '/qualified/compiler',
                                     'EXOMONAD_WORKSPACE_GITLINK': '/qualified/workspace-pin',
                                     'OPENAI_API_KEY': 'never-forward',
                                     'UNRELATED_VARIABLE': 'never-forward'}):
            command, unit = runner.delegated_command(
                ['/declared/libtest', '--exact', 'suite::works'], 12, 'app.slice', record)
        split = command.index('--')
        self.assertEqual(command[split + 1:], ['/declared/libtest', '--exact', 'suite::works'])
        self.assertIn('--property=Delegate=yes', command)
        self.assertIn('--property=KillMode=control-group', command)
        self.assertIn('--setenv=TIDEPOOL_EXTRACT=/qualified/compiler', command)
        self.assertIn('--setenv=EXOMONAD_WORKSPACE_GITLINK=/qualified/workspace-pin', command)
        self.assertFalse(any('never-forward' in word for word in command))
        self.assertIn('--unit=' + unit, command)
        self.assertFalse(record['cleanup_confirmed'])

    def test_delegated_cleanup_failure_refuses_pass_without_erasing_actual_count(self):
        def result(args, timeout, service_slice, service_record):
            self.assertEqual(service_slice, 'app.slice')
            service_record.update(cleanup_confirmed=False, cleanup_error='service still active')
            return subprocess.CompletedProcess(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.object(runner, 'execute', side_effect=result):
            passed, _, errors = runner.run_one(str(self.binary), 'suite::works', False,
                                               12, record, 'app.slice')
        self.assertFalse(passed)
        self.assertEqual(record['exit_code'], 0)
        self.assertEqual(record['executed_test_count'], 1)
        self.assertEqual(record['process_execution_count'], 1)
        self.assertIn('service still active', errors)

    def test_delegated_launch_failure_keeps_test_execution_unknown(self):
        def refused(args, timeout, service_slice, service_record):
            service_record.update(cleanup_confirmed=True)
            return subprocess.CompletedProcess(args, 1, '', 'service admission refused')
        record = {}
        with patch.object(runner, 'execute', side_effect=refused):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                                          12, record, 'app.slice')
        self.assertFalse(passed)
        self.assertIsNone(record['executed_test_count'])
        self.assertIsNone(record['process_execution_count'])
        self.assertEqual(record['exit_code'], 1)

    def test_delegated_registration_signal_and_timeout_stop_exact_service(self):
        class Process:
            pid = 781235
            stdout = None
            stderr = None
            returncode = -9
            def communicate(self, timeout=None):
                return '', ''
        def spawn(*_args, **_kwargs):
            runner._signal_active_processes(signal.SIGTERM, None)
            return Process()
        runner.INTERRUPT_SIGNAL = None
        with patch.object(runner.subprocess, 'Popen', side_effect=spawn), \
             patch.object(runner.os, 'killpg'), \
             patch.object(runner, 'stop_delegated_service') as stop, \
             self.assertRaises(runner.RunnerInterrupted):
            runner.execute(['fake-test'], 1, 'app.slice', {})
        self.assertEqual(stop.call_count, 1)
        self.assertRegex(stop.call_args.args[0], r'^tidepool-libtest-[a-f0-9]{32}\.service$')
        runner.INTERRUPT_SIGNAL = None
        process = Process()
        with patch.object(runner.subprocess, 'Popen', return_value=process), \
             patch.object(process, 'communicate', side_effect=[
                 subprocess.TimeoutExpired(['fake-test'], 1), ('', '')]), \
             patch.object(runner.os, 'killpg'), \
             patch.object(runner, 'stop_delegated_service') as stop, \
             patch.object(runner, 'observe_delegated_admission'), \
             self.assertRaises(subprocess.TimeoutExpired):
            runner.execute(['fake-test'], 1, 'app.slice', {})
        self.assertEqual(stop.call_count, 1)
        self.assertEqual(runner.ACTIVE_PROCESSES, {})

    def test_delegated_stop_escalates_only_exact_service_and_verifies_end_state(self):
        unit = 'tidepool-libtest-exact.service'
        calls = []
        def control(args, **_kwargs):
            calls.append(args)
            if len(calls) == 1:
                raise subprocess.TimeoutExpired(args, 10)
            output = 'LoadState=loaded\nActiveState=inactive\n' if 'show' in args else ''
            return subprocess.CompletedProcess(args, 0, output, '')
        record = {'manager_admission': {'Id': unit, 'Transient': 'yes',
                                        'InvocationID': 'ab' * 16}}
        with patch.object(runner.subprocess, 'run', side_effect=control):
            runner.stop_delegated_service(unit, record)
        self.assertTrue(record['cleanup_confirmed'])
        self.assertEqual([args[2] for args in calls], ['stop', 'kill', 'stop', 'show'])
        self.assertTrue(all(args[-1] == unit for args in calls))
        with patch.object(runner.subprocess, 'run', return_value=subprocess.CompletedProcess(
                [], 0, 'LoadState=loaded\nActiveState=active\n', '')):
            runner.stop_delegated_service(unit, record)
        self.assertFalse(record['cleanup_confirmed'])

    def test_delegated_options_do_not_apply_to_discovery(self):
        calls = []
        def run(args, timeout, service_slice=None, service_record=None):
            calls.append((args, service_slice))
            discovered = self.discover(args)
            if discovered is not None:
                return discovered
            service_record.update(cleanup_confirmed=True)
            return subprocess.CompletedProcess(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        result, _, _ = self.invoke(['--exact', 'suite::works', '--expected-count', '1',
                                  '--delegated-service'], run)
        self.assertEqual(result, 0)
        self.assertTrue(all(service is None for args, service in calls if '--list' in args))
        self.assertEqual([service for args, service in calls if '--list' not in args], ['app.slice'])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            runner.parse_args([str(self.binary), '--service-slice', 'app.slice'])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            runner.parse_args([str(self.binary), '--delegated-service', '--service-slice', '../unsafe'])

    def test_absent_unknown_launch_cannot_confirm_before_late_manager_registration(self):
        unit = 'tidepool-libtest-late.service'
        record = {}
        def absent(args, **_kwargs):
            return subprocess.CompletedProcess(args, 5 if 'stop' in args else 1,
                                                'LoadState=not-found\nActiveState=inactive\n', '')
        with patch.object(runner.subprocess, 'run', side_effect=absent):
            runner.stop_delegated_service(unit, record)
        self.assertFalse(record['cleanup_confirmed'])
        self.assertIn('queued start', record['cleanup_error'])
        late = f'Id={unit}\nLoadState=loaded\nTransient=yes\nInvocationID={"ab" * 16}\n'
        with patch.object(runner.subprocess, 'run', return_value=subprocess.CompletedProcess(
                [], 0, late, '')):
            runner.observe_delegated_admission(unit, record, runner.threading.Event())
        self.assertEqual(record['manager_admission']['Id'], unit)
        self.assertFalse(record['cleanup_confirmed'], 'late registration requires a new exact stop')
        with patch.object(runner.subprocess, 'run', return_value=subprocess.CompletedProcess(
                [], 0, 'LoadState=loaded\nActiveState=inactive\n', '')):
            runner.stop_delegated_service(unit, record)
        self.assertTrue(record['cleanup_confirmed'])

    def test_successful_delegated_launcher_is_not_signalled_after_reaping(self):
        class Process:
            pid = 781236
            stdout = None
            stderr = None
            returncode = 0
            def communicate(self, timeout=None):
                return 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', ''
        with patch.object(runner.subprocess, 'Popen', return_value=Process()), \
             patch.object(runner, 'observe_delegated_admission'), \
             patch.object(runner, 'stop_delegated_service'), \
             patch.object(runner.os, 'killpg') as kill:
            runner.execute(['fake-libtest'], 2, 'app.slice', {})
        kill.assert_not_called()

    def test_interrupted_launched_future_retains_unconfirmed_service_receipt(self):
        retained = Path(self.tmp.name) / 'interrupted'
        starts = []
        def run(args, timeout, service_slice=None, service_record=None):
            discovered = self.discover(args)
            if discovered is not None:
                return discovered
            starts.append(args[2])
            service_record.update(unit='tidepool-libtest-pending.service',
                                  cleanup_confirmed=False, cleanup_error='launch admission unknown')
            runner._signal_active_processes(signal.SIGTERM, None)
            raise runner.RunnerInterrupted(signal.SIGTERM)
        result, _, _ = self.invoke(['--exact', 'suite::works', '--exact', 'suite::also_works',
            '--expected-count', '2', '--jobs', '1', '--delegated-service',
            '--output-dir', str(retained)], run)
        self.assertEqual(result, 128 + signal.SIGTERM)
        self.assertEqual(starts, ['suite::works'])
        records = [json.loads(path.read_text()) for path in retained.glob('*.json')]
        self.assertEqual(len(records), 1)
        self.assertFalse(records[0]['passed'])
        execution = records[0]['execution']
        self.assertEqual(execution['status'], 'interrupted')
        self.assertIsNone(execution['executed_test_count'])
        self.assertEqual(execution['delegated_service']['unit'], 'tidepool-libtest-pending.service')
        self.assertFalse(execution['delegated_service']['cleanup_confirmed'])
        runner.INTERRUPT_SIGNAL = None

    def test_timeout_preserves_actual_partial_libtest_count_without_passing(self):
        record = {}
        with patch.object(runner, 'execute', side_effect=subprocess.TimeoutExpired(
                ['fake-libtest'], 2, output='test result: ok. 1 passed; 0 failed; 0 ignored;\n')):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                                          2, record, 'app.slice')
        self.assertFalse(passed)
        self.assertEqual(record['status'], 'timeout')
        self.assertIsNone(record['exit_code'])
        self.assertEqual(record['executed_test_count'], 1)
        self.assertEqual(record['process_execution_count'], 1)

    def test_timeout_bytes_retain_output_and_exact_cleanup_receipt(self):
        retained = Path(self.tmp.name) / 'timeout-bytes'
        def run(args, timeout, service_slice=None, service_record=None):
            discovered = self.discover(args)
            if discovered is not None:
                return discovered
            service_record.update(unit='tidepool-libtest-timeout.service',
                                  cleanup_confirmed=False, cleanup_error='launch admission unknown')
            raise subprocess.TimeoutExpired(args, timeout,
                output=b'test result: ok. 1 passed; 0 failed; 0 ignored;\n',
                stderr=b'inherited pipe stalled\xff')
        result, _, _ = self.invoke(['--exact', 'suite::works', '--expected-count', '1',
            '--delegated-service', '--output-dir', str(retained)], run)
        self.assertEqual(result, 1)
        records = [json.loads(path.read_text()) for path in retained.glob('*.json')]
        self.assertEqual(len(records), 1)
        self.assertFalse(records[0]['passed'])
        execution = records[0]['execution']
        self.assertEqual(execution['status'], 'timeout')
        self.assertEqual(execution['executed_test_count'], 1)
        self.assertIsNone(execution['exit_code'])
        self.assertEqual(execution['delegated_service']['unit'], 'tidepool-libtest-timeout.service')
        self.assertFalse(execution['delegated_service']['cleanup_confirmed'])
        self.assertIn('test result: ok. 1 passed',
            (retained / records[0]['streams']['stdout']['path']).read_text())
        self.assertEqual((retained / records[0]['streams']['stderr']['path']).read_text(),
                         'inherited pipe stalled\ufffd')

    def test_actual_failed_execution_count_is_retained_without_changing_pass_rule(self):
        record = {}
        result = subprocess.CompletedProcess([], 101,
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n', '')
        with patch.object(runner, 'execute', return_value=result):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::fails', False, 2, record)
        self.assertFalse(passed)
        self.assertEqual(record['exit_code'], 101)
        self.assertEqual(record['executed_test_count'], 1)
        self.assertEqual(record['failed_test_count'], 1)
        self.assertGreaterEqual(record['elapsed_ns'], 0)

    def test_zero_execution_and_unknown_timeout_remain_distinct(self):
        record = {}
        result = subprocess.CompletedProcess([], 0,
            'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n', '')
        with patch.object(runner, 'execute', return_value=result):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::absent', False, 2, record)
        self.assertFalse(passed)
        self.assertEqual(record['executed_test_count'], 0)
        with patch.object(runner, 'execute', side_effect=subprocess.TimeoutExpired([], 2)):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::hung', False, 2, record)
        self.assertFalse(passed)
        self.assertIsNone(record['exit_code'])
        self.assertIsNone(record['executed_test_count'])
        self.assertEqual(record['status'], 'timeout')

    def invoke(self, arguments, run):
        output = io.StringIO()
        errors = io.StringIO()
        with patch.object(runner, 'execute', side_effect=run), \
             contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            result = runner.main([str(self.binary), *arguments])
        return result, output.getvalue(), errors.getvalue()

    def discover(self, argv):
        if '--list' not in argv:
            return None
        if '--ignored' in argv:
            return subprocess.CompletedProcess(argv, 0, self.ignored_tests, '')
        return subprocess.CompletedProcess(argv, 0, self.all_tests, '')

    def test_passing_output_is_retained_with_explicit_truncation(self):
        retained = Path(self.tmp.name) / 'retained'
        payload = 'witness' * 8 + '\n'
        summary = 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'

        def run(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return subprocess.CompletedProcess(argv, 0, payload + summary, 'phase evidence')

        with patch.object(runner, 'OUTPUT_LIMIT', 16):
            result, output, _ = self.invoke([
                '--exact', 'suite::works', '--expected-count', '1',
                '--output-dir', str(retained),
            ], run)
        self.assertEqual(result, 0)
        self.assertIn('PASS suite::works', output)
        records = list(retained.glob('*.json'))
        self.assertEqual(len(records), 1)
        record = json.loads(records[0].read_text())
        self.assertTrue(record['passed'])
        self.assertEqual(record['test'], 'suite::works')
        witness = record['streams']['stdout']
        self.assertTrue(witness['truncated'])
        self.assertEqual(witness['sha256'], hashlib.sha256((payload + summary).encode()).hexdigest())
        self.assertEqual((retained / witness['path']).read_bytes(), (payload + summary).encode()[:16])
        self.assertFalse(record['streams']['stderr']['truncated'])
        self.assertEqual((retained / record['streams']['stderr']['path']).read_text(), 'phase evidence')

    def test_exact_selection_requires_and_enforces_expected_count(self):
        calls = []

        def run(argv, timeout):
            calls.append((argv, timeout))
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return subprocess.CompletedProcess(
                argv, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', ''
            )

        result, output, errors = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1'], run
        )
        self.assertEqual(result, 0)
        self.assertIn('PASS suite::works', output)
        self.assertIn('1 passed; 0 failed', output)
        self.assertEqual(errors, '')
        test_calls = [argv for argv, _ in calls if '--list' not in argv]
        self.assertEqual(len(test_calls), 1, 'one fresh process must run per selected test')
        self.assertEqual(test_calls[0][1:], ['--exact', 'suite::works', '--nocapture'])

    def test_focused_jobs_default_to_one_and_can_be_overridden(self):
        class Executor:
            def __init__(self, max_workers):
                workers.append(max_workers)

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def submit(self, function, *args):
                future = runner.concurrent.futures.Future()
                future.set_result(function(*args))
                return future

            def shutdown(self, wait, cancel_futures):
                self.assert_shutdown = (wait, cancel_futures)

        def run(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return subprocess.CompletedProcess(
                argv, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', ''
            )

        workers = []
        with patch.object(runner.concurrent.futures, 'ThreadPoolExecutor', Executor):
            result, _, _ = self.invoke(
                ['--exact', 'suite::works', '--exact', 'suite::also_works',
                 '--expected-count', '2'], run
            )
        self.assertEqual(result, 0)
        self.assertEqual(workers, [1])

        workers.clear()
        with patch.object(runner.concurrent.futures, 'ThreadPoolExecutor', Executor):
            result, _, _ = self.invoke(
                ['--exact', 'suite::works', '--exact', 'suite::also_works',
                 '--expected-count', '2', '--jobs', '2'], run
            )
        self.assertEqual(result, 0)
        self.assertEqual(workers, [2])

    def test_jobs_execute_distinct_cases_concurrently(self):
        together = threading.Barrier(2)
        launched = []

        def run(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            launched.append(argv[2])
            together.wait(timeout=2)
            return subprocess.CompletedProcess(
                argv, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')

        result, _, errors = self.invoke(
            ['--exact', 'suite::works', '--exact', 'suite::also_works',
             '--expected-count', '2', '--jobs', '2'], run)
        self.assertEqual(result, 0, errors)
        self.assertCountEqual(launched, ['suite::works', 'suite::also_works'])

    def test_case_timeouts_apply_to_process_and_delegated_watchdog_and_receipt(self):
        retained = Path(self.tmp.name) / 'case-timeouts'
        limits = {}

        def run(argv, timeout, service_slice=None, service_record=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            command, _ = runner.delegated_command(argv, timeout, service_slice, service_record)
            self.assertIn(f'--property=RuntimeMaxSec={timeout:g}s', command)
            limits[argv[2]] = timeout
            service_record.update(cleanup_confirmed=True)
            return subprocess.CompletedProcess(
                argv, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')

        result, _, errors = self.invoke(
            ['--exact', 'suite::works', '--exact', 'suite::also_works',
             '--expected-count', '2', '--timeout', '600',
             '--case-timeout', 'suite::works=900', '--delegated-service',
             '--output-dir', str(retained)], run)
        self.assertEqual(result, 0, errors)
        self.assertEqual(limits, {'suite::works': 900, 'suite::also_works': 600})
        records = [json.loads(path.read_text()) for path in retained.glob('*.json')]
        self.assertEqual({record['test']: record['execution']['timeout_seconds'] for record in records}, limits)

    def test_invalid_case_timeouts_and_nonfinite_default_refuse_before_discovery(self):
        invalid = [
            ['--case-timeout', value] for value in
            ('missing-separator', '=12', 'suite::works=no', 'suite::works=0',
             'suite::works=-1', 'suite::works=nan', 'suite::works=inf')
        ]
        invalid.extend([
            ['--case-timeout', 'suite::works=1', '--case-timeout', 'suite::works=2'],
            ['--timeout', 'nan'], ['--timeout', 'inf'],
        ])
        for arguments in invalid:
            with self.subTest(arguments=arguments), patch.object(runner, 'execute') as execute, \
                 contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                runner.main([str(self.binary), *arguments])
            execute.assert_not_called()

    def test_case_timeout_requires_selected_exact_name_before_execution(self):
        launched = []

        def run(argv, timeout):
            if '--list' not in argv:
                launched.append(argv)
            return self.discover(argv)

        result, _, errors = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1',
             '--case-timeout', 'suite::also_works=900'], run)
        self.assertEqual(result, 1)
        self.assertIn('case timeout names are not selected', errors)
        self.assertEqual(launched, [])

    def test_overridden_timeout_never_qualifies_a_partial_success(self):
        retained = Path(self.tmp.name) / 'overridden-timeout'

        def run(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            self.assertEqual(timeout, 900)
            raise subprocess.TimeoutExpired(argv, timeout,
                output='test result: ok. 1 passed; 0 failed; 0 ignored;\n')

        result, _, _ = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1',
             '--case-timeout', 'suite::works=900', '--output-dir', str(retained)], run)
        self.assertEqual(result, 1)
        record = json.loads(next(retained.glob('*.json')).read_text())
        self.assertFalse(record['passed'])
        self.assertEqual(record['execution']['timeout_seconds'], 900)
        self.assertEqual(record['execution']['status'], 'timeout')
        self.assertEqual(record['execution']['executed_test_count'], 1)
        self.assertIsNone(record['execution']['exit_code'])

    def test_zero_and_wrong_expected_counts_fail_before_execution(self):
        launched = []

        def run(argv, timeout):
            if '--list' not in argv:
                launched.append(argv)
            return self.discover(argv)

        result, _, errors = self.invoke(
            ['--exact', 'suite::absent', '--expected-count', '1'], run
        )
        self.assertEqual(result, 1)
        self.assertIn('exact test names not discovered', errors)
        self.assertEqual(launched, [])

        result, _, errors = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '2'], run
        )
        self.assertEqual(result, 1)
        self.assertIn('expected 2 selected tests, discovered 1', errors)
        self.assertEqual(launched, [])

    def test_zero_expected_count_is_rejected(self):
        with patch.object(sys, 'stderr', io.StringIO()), self.assertRaises(SystemExit):
            runner.parse_args([
                str(self.binary), '--exact', 'suite::works', '--expected-count', '0'
            ])

    def test_ignored_mode_runs_exact_ignored_test_explicitly(self):
        calls = []

        def run(argv, timeout):
            calls.append(argv)
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return subprocess.CompletedProcess(
                argv, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', ''
            )

        result, output, _ = self.invoke(
            ['--exact', 'suite::ignored', '--expected-count', '1', '--ignored'], run
        )
        self.assertEqual(result, 0)
        test_calls = [argv for argv in calls if '--list' not in argv]
        self.assertEqual(len(test_calls), 1)
        self.assertIn('--ignored', test_calls[0])
        self.assertIn('PASS suite::ignored', output)

    def test_ignored_test_cannot_be_selected_without_ignored_mode(self):
        launched = []

        def run(argv, timeout):
            if '--list' not in argv:
                launched.append(argv)
            return self.discover(argv)

        result, _, errors = self.invoke(
            ['--exact', 'suite::ignored', '--expected-count', '1'], run
        )
        self.assertEqual(result, 1)
        self.assertIn('not all nonignored', errors)
        self.assertEqual(launched, [])

    def test_wrong_libtest_summary_fails_even_with_success_exit(self):
        def run(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return subprocess.CompletedProcess(
                argv, 0, 'test result: ok. 0 passed; 0 failed; 0 ignored;\n', ''
            )

        result, output, _ = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1'], run
        )
        self.assertEqual(result, 1)
        self.assertIn('FAIL suite::works', output)
        self.assertIn('0 passed; 1 failed', output)

        def child_then_empty_parent(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return subprocess.CompletedProcess(
                argv,
                0,
                'child output: test result: ok. 1 passed; 0 failed; 0 ignored;\n'
                'test result: ok. 0 passed; 0 failed; 0 ignored; 1 measured; 1 filtered out;\n',
                '',
            )

        result, output, _ = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1'], child_then_empty_parent
        )
        self.assertEqual(result, 1)
        self.assertIn('FAIL suite::works', output)

    def test_test_failure_and_timeout_are_counted(self):
        def fail(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return subprocess.CompletedProcess(
                argv, 101, 'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', 'panic\n'
            )

        result, output, errors = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1'], fail
        )
        self.assertEqual(result, 1)
        self.assertIn('FAIL suite::works', output)
        self.assertIn('panic', errors)

        def timeout(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            self.assertEqual(timeout, 0.01)
            raise subprocess.TimeoutExpired(argv, timeout)

        result, output, _ = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1', '--timeout', '0.01'],
            timeout,
        )
        self.assertEqual(result, 1)
        self.assertIn('timed out after 0.01s', output)

    def test_real_timeout_kills_and_reaps_the_test_process(self):
        pid_file = Path(self.tmp.name) / 'child.pid'
        code = (
            'import os,time; '
            f"open({str(pid_file)!r}, 'w').write(str(os.getpid())); "
            'time.sleep(60)'
        )
        with self.assertRaises(subprocess.TimeoutExpired):
            runner.execute([sys.executable, '-c', code], 0.1)
        pid = int(pid_file.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_registration_race_and_repeated_signal_do_not_miss_process(self):
        runner.INTERRUPT_SIGNAL = None

        class Process:
            pid = 781234
            stdout = None
            stderr = None

            def communicate(self, timeout=None):
                return '', ''

        def spawn(*_args, **_kwargs):
            process = Process()
            # Simulate delivery after Popen created the child but before the
            # caller could register its process-group cleanup handle.
            runner._signal_active_processes(signal.SIGTERM, None)
            runner._signal_active_processes(signal.SIGTERM, None)
            return process

        with patch.object(runner.subprocess, 'Popen', side_effect=spawn), \
             patch.object(runner.os, 'killpg') as killpg, \
             self.assertRaises(runner.RunnerInterrupted):
            runner.execute(['fake-test'], 1)
        self.assertGreaterEqual(killpg.call_count, 1)
        runner.INTERRUPT_SIGNAL = None
        runner.ACTIVE_PROCESSES.clear()
        runner.ACTIVE_PROCESS_SNAPSHOT = ()

    def test_runner_signal_during_discovery_kills_process_group(self):
        binary = Path(self.tmp.name) / 'discovery-libtest'
        child_pid_file = Path(self.tmp.name) / 'discovery-child.pid'
        binary.write_text(
            '#!/usr/bin/env python3\n'
            'import subprocess, sys, time\n'
            "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
            f"open({str(child_pid_file)!r}, 'w').write(str(child.pid))\n"
            'time.sleep(60)\n'
        )
        binary.chmod(0o755)
        helper = subprocess.Popen(
            [sys.executable, str(SCRIPT), str(binary)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 5
            while not child_pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(child_pid_file.exists(), 'discovery process never started')
            child_pid = int(child_pid_file.read_text())
            os.kill(helper.pid, signal.SIGTERM)
            stdout, stderr = helper.communicate(timeout=5)
            self.assertEqual(helper.returncode, 128 + signal.SIGTERM, (stdout, stderr))
            self.assertIn('interrupted by signal', stderr)
            self.assert_process_gone(child_pid)
        finally:
            if helper.poll() is None:
                helper.kill()
            helper.communicate(timeout=5)

    def test_runner_signal_with_single_job_does_not_start_queued_case(self):
        binary = Path(self.tmp.name) / 'fake-libtest'
        child_pid_file = Path(self.tmp.name) / 'descendant.pid'
        starts_file = Path(self.tmp.name) / 'starts.txt'
        binary.write_text(
            '#!/usr/bin/env python3\n'
            'import subprocess, sys, time\n'
            "if '--list' in sys.argv and '--ignored' in sys.argv:\n"
            '    raise SystemExit(0)\n'
            "if '--list' in sys.argv:\n"
            "    print('suite::first: test')\n"
            "    print('suite::second: test')\n"
            '    raise SystemExit(0)\n'
            f"open({str(starts_file)!r}, 'a').write(sys.argv[2] + '\\n')\n"
            "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
            f"open({str(child_pid_file)!r}, 'w').write(str(child.pid))\n"
            'time.sleep(60)\n'
        )
        binary.chmod(0o755)
        helper = subprocess.Popen(
            [
                sys.executable,
                str(SCRIPT),
                str(binary),
                '--exact',
                'suite::first',
                '--exact',
                'suite::second',
                '--expected-count',
                '2',
                '--jobs',
                '1',
                '--timeout',
                '30',
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 5
            while not child_pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(child_pid_file.exists(), 'selected test process never started')
            child_pid = int(child_pid_file.read_text())
            os.kill(helper.pid, signal.SIGTERM)
            time.sleep(0.01)
            os.kill(helper.pid, signal.SIGTERM)
            stdout, stderr = helper.communicate(timeout=5)
            self.assertEqual(helper.returncode, 128 + signal.SIGTERM, (stdout, stderr))
            self.assertIn('interrupted by signal', stderr)
            self.assertEqual(starts_file.read_text().splitlines(), ['suite::first'])
            self.assert_process_gone(child_pid)
        finally:
            if helper.poll() is None:
                helper.kill()
            helper.communicate(timeout=5)

    def test_successful_test_leader_kills_pipe_closed_descendant(self):
        binary = Path(self.tmp.name) / 'passing-libtest'
        child_pid_file = Path(self.tmp.name) / 'passing-child.pid'
        binary.write_text(
            '#!/usr/bin/env python3\n'
            'import subprocess, sys\n'
            "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'], "
            'stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n'
            f"open({str(child_pid_file)!r}, 'w').write(str(child.pid))\n"
            "print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')\n"
        )
        binary.chmod(0o755)
        passed, output, errors = runner.run_one(str(binary), 'suite::pass', False, 2)
        self.assertTrue(passed, (output, errors))
        self.assert_process_gone(int(child_pid_file.read_text()))

    def assert_process_gone(self, pid):
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.02)
        self.fail(f'process {pid} survived cleanup')

    def test_discovery_timeout_fails_closed(self):
        def timeout(argv, timeout):
            raise subprocess.TimeoutExpired(argv, timeout)

        result, _, errors = self.invoke([], timeout)
        self.assertEqual(result, 1)
        self.assertIn('libtest selection failed', errors)

    def test_discovery_and_ignored_inventory_are_validated(self):
        def malformed(argv, timeout):
            if '--ignored' in argv:
                return subprocess.CompletedProcess(argv, 0, 'suite::absent: test\n', '')
            return subprocess.CompletedProcess(argv, 0, '2 tests, 0 benchmarks\n', '')

        result, _, errors = self.invoke([], malformed)
        self.assertEqual(result, 1)
        self.assertIn('unexpected libtest discovery output', errors)

        def unmatched_ignored(argv, timeout):
            if '--ignored' in argv:
                return subprocess.CompletedProcess(argv, 0, 'suite::absent: test\n', '')
            return subprocess.CompletedProcess(argv, 0, self.all_tests, '')

        result, _, errors = self.invoke([], unmatched_ignored)
        self.assertEqual(result, 1)
        self.assertIn('ignored tests missing from discovery', errors)


if __name__ == '__main__':
    unittest.main()
