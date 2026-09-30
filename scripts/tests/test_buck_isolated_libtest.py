import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
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
        self.all_tests = 'suite::works: test\nsuite::ignored: test\n'
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
