import contextlib
import importlib.util
import io
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
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
