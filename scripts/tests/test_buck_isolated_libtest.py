import contextlib
import importlib.util
import io
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

    def tearDown(self):
        self.tmp.cleanup()

    def invoke(self, run):
        output = io.StringIO()
        errors = io.StringIO()
        with patch.object(sys, 'argv', ['isolated-libtest.py', str(self.binary)]), \
             patch.object(runner.subprocess, 'run', side_effect=run), \
             contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            result = runner.main()
        return result, output.getvalue(), errors.getvalue()

    def test_zero_runnable_tests_fails_closed(self):
        def run(argv, **kwargs):
            if '--list' in argv and '--ignored' in argv:
                return subprocess.CompletedProcess(argv, 0, 'suite::ignored: test\n', '')
            if '--list' in argv:
                return subprocess.CompletedProcess(argv, 0, 'suite::ignored: test\n', '')
            self.fail('no test process should launch when every test is ignored')
        with self.assertRaisesRegex(RuntimeError, 'no runnable tests discovered'):
            self.invoke(run)

    def test_unexpected_discovery_line_fails_closed(self):
        def run(argv, **kwargs):
            return subprocess.CompletedProcess(argv, 0, '2 tests, 0 benchmarks\n', '')
        with self.assertRaisesRegex(RuntimeError, 'unexpected libtest discovery output'):
            self.invoke(run)

    def test_duplicate_and_unmatched_ignored_discovery_fail_closed(self):
        def duplicate(argv, **kwargs):
            return subprocess.CompletedProcess(argv, 0, 'suite::same: test\nsuite::same: test\n', '')
        with self.assertRaisesRegex(RuntimeError, 'duplicate libtest names'):
            self.invoke(duplicate)

        def unmatched(argv, **kwargs):
            if '--ignored' in argv:
                out = 'suite::missing: test\n'
            else:
                out = 'suite::present: test\n'
            return subprocess.CompletedProcess(argv, 0, out, '')
        with self.assertRaisesRegex(RuntimeError, 'ignored tests missing from discovery'):
            self.invoke(unmatched)

    def test_exact_test_requires_one_pass_summary(self):
        calls = []
        def run(argv, **kwargs):
            calls.append(argv)
            if '--list' in argv and '--ignored' in argv:
                return subprocess.CompletedProcess(argv, 0, '', '')
            if '--list' in argv:
                return subprocess.CompletedProcess(argv, 0, 'suite::works: test\n', '')
            return subprocess.CompletedProcess(argv, 0,
                'test result: ok. 2 passed; 0 failed; 0 ignored;\n', '')
        result, output, errors = self.invoke(run)
        self.assertTrue(result, 'wrong selected-test count must fail despite exit status 0')
        self.assertIn('FAIL suite::works', output)
        self.assertIn('0 passed; 1 failed; 0 ignored', output)
        self.assertTrue(any('--exact' in call for call in calls))

    def test_test_failure_is_counted(self):
        def run(argv, **kwargs):
            if '--list' in argv and '--ignored' in argv:
                return subprocess.CompletedProcess(argv, 0, '', '')
            if '--list' in argv:
                return subprocess.CompletedProcess(argv, 0, 'suite::fails: test\n', '')
            return subprocess.CompletedProcess(argv, 101,
                'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', 'panic\n')
        result, output, errors = self.invoke(run)
        self.assertTrue(result)
        self.assertIn('FAIL suite::fails', output)
        self.assertIn('panic', errors)

    def test_timeout_is_a_hard_failure(self):
        def run(argv, **kwargs):
            if '--list' in argv and '--ignored' in argv:
                return subprocess.CompletedProcess(argv, 0, '', '')
            if '--list' in argv:
                return subprocess.CompletedProcess(argv, 0, 'suite::hangs: test\n', '')
            self.assertEqual(kwargs['timeout'], 300)
            raise subprocess.TimeoutExpired(argv, 300)
        with self.assertRaises(subprocess.TimeoutExpired):
            self.invoke(run)

    def test_discovery_nonzero_status_is_a_hard_failure(self):
        def run(argv, **kwargs):
            if kwargs.get('check'):
                raise subprocess.CalledProcessError(2, argv, '', 'discovery broken')
            return subprocess.CompletedProcess(argv, 2, '', 'discovery broken')
        with self.assertRaises(subprocess.CalledProcessError):
            self.invoke(run)


if __name__ == '__main__':
    unittest.main()
