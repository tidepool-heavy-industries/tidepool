import contextlib
import importlib.util
import io
import itertools
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


def completed_process(*args, **kwargs):
    """Test executions explicitly acknowledge their mocked envelope teardown."""
    result = subprocess.CompletedProcess(*args, **kwargs)
    result.cleanup_confirmed = True
    return result


class IsolatedLibtestTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.binary = Path(self.tmp.name) / 'test-binary'
        self.binary.write_text('placeholder')
        self.all_tests = 'suite::works: test\nsuite::also_works: test\nsuite::ignored: test\n'
        self.ignored_tests = 'suite::ignored: test\n'

    def tearDown(self):
        self.tmp.cleanup()

    def test_concurrent_cases_keep_candidate_sources_and_cache_reuse_in_their_own_scope(self):
        for diagnostic in (False, True):
            with self.subTest(diagnostic=diagnostic):
                rendezvous = Path(self.tmp.name) / f'cache-race-{diagnostic}'
                rendezvous.mkdir()
                ambient = rendezvous / 'ambient-cache'
                ambient.mkdir()
                (ambient / 'candidate').write_text('ambient must remain untouched')
                self.binary.write_text('#!' + sys.executable + '\n' +
                    'import os, pathlib, subprocess, sys, time\n' +
                    f'root = pathlib.Path({str(rendezvous)!r})\n' +
                    'name = sys.argv[2]\n' +
                    'cache = pathlib.Path(os.environ["TIDEPOOL_COMPILE_CACHE_DIR"])\n' +
                    'source = root / (name + ".hs")\n' +
                    'source.write_text(name)\n' +
                    '(cache / "candidate").write_text(str(source))\n' +
                    '(root / (name + ".ready")).touch()\n' +
                    'deadline = time.monotonic() + 3\n' +
                    'while len(list(root.glob("*.ready"))) != 2:\n' +
                    '    assert time.monotonic() < deadline, "other case did not arrive"\n' +
                    '    time.sleep(0.005)\n' +
                    'assert (cache / "candidate").read_text() == str(source)\n' +
                    'if name == "suite::first":\n' +
                    '    source.unlink()\n' +
                    '    (root / "first-released").touch()\n' +
                    'else:\n' +
                    '    while not (root / "first-released").exists():\n' +
                    '        assert time.monotonic() < deadline, "first case did not release"\n' +
                    '        time.sleep(0.005)\n' +
                    '    assert pathlib.Path((cache / "candidate").read_text()).read_text() == name\n' +
                    'observed = subprocess.check_output([sys.executable, "-c",\n' +
                    '    "import os,pathlib;print((pathlib.Path(os.environ[\\\"TIDEPOOL_COMPILE_CACHE_DIR\\\"])/\\\"candidate\\\").read_text())"], text=True)\n' +
                    'assert observed.strip() == str(source), "same case lost its cache reuse"\n' +
                    'print("test result: ok. 1 passed; 0 failed; 0 ignored;")\n')
                self.binary.chmod(0o700)
                records = [{}, {}]
                def run(index):
                    root = rendezvous / f'artifacts-{index}' if diagnostic else None
                    return runner.run_one(str(self.binary), ('suite::first', 'suite::second')[index],
                        False, 5, records[index], artifact_root=root, retain_artifacts=diagnostic)
                with patch.dict(os.environ, {'TIDEPOOL_COMPILE_CACHE_DIR': str(ambient)}), \
                     runner.concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    outcomes = list(pool.map(run, range(2)))
                for outcome in outcomes:
                    self.assertTrue(outcome[0], outcome[1:])
                caches = [Path(record['compile_cache_root']) for record in records]
                self.assertNotEqual(caches[0], caches[1])
                self.assertNotIn(ambient, caches)
                self.assertEqual((ambient / 'candidate').read_text(), 'ambient must remain untouched')
                for cache, record in zip(caches, records):
                    self.assertEqual(cache.is_dir(), diagnostic)
                    self.assertEqual(record['compile_cache_disposition'],
                                     'retained' if diagnostic else 'removed')

    def test_case_cache_selection_crosses_delegation_exactly(self):
        selected = '/case/artifacts/compile-cache'
        record = {}
        command, _ = runner.delegated_command(['/libtest'], 10, 'app.slice', record,
            environment={'TIDEPOOL_COMPILE_CACHE_DIR': selected, 'UNDECLARED_CACHE': '/ambient'})
        self.assertIn(f'--setenv=TIDEPOOL_COMPILE_CACHE_DIR={selected}', command)
        self.assertIn('TIDEPOOL_COMPILE_CACHE_DIR', record['environment_names'])
        self.assertFalse(any('UNDECLARED_CACHE' in word for word in command))

    def test_missing_case_cache_is_not_reported_as_runner_cleanup(self):
        root = Path(self.tmp.name) / 'missing-cache-artifacts'
        record = {}
        def run(args, timeout, environment=None):
            Path(environment['TIDEPOOL_COMPILE_CACHE_DIR']).rmdir()
            return completed_process(args, 101,
                'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', '')
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::failure', False,
                5, record, artifact_root=root)
        self.assertFalse(passed)
        self.assertTrue(root.is_dir())
        self.assertEqual(record['compile_cache_disposition'], 'missing')

    def test_failure_and_timeout_preserve_preexecution_case_evidence(self):
        for timed_out in (False, True):
            with self.subTest(timed_out=timed_out):
                root = Path(self.tmp.name) / ('timeout-artifacts' if timed_out else 'failure-artifacts')
                self.binary.write_text('#!' + sys.executable + '\n' +
                    'import os, pathlib, time\n' +
                    'root = pathlib.Path(os.environ["TIDEPOOL_TEST_ARTIFACT_ROOT"])\n' +
                    'assert os.environ["TIDEPOOL_TEST_DIAGNOSTIC_SCOPE"] == "1"\n' +
                    'cache = pathlib.Path(os.environ["TIDEPOOL_COMPILE_CACHE_DIR"])\n' +
                    '(cache / "candidate").write_text("retained failed input")\n' +
                    '(root / "completed-compile.txt").write_text("retained before execution")\n' +
                    ('time.sleep(30)\n' if timed_out else
                     'print("test result: FAILED. 0 passed; 1 failed; 0 ignored;")\nraise SystemExit(101)\n'))
                self.binary.chmod(0o700)
                record = {}
                passed, _, _ = runner.run_one(str(self.binary), 'suite::runtime_failure',
                    False, 0.5 if timed_out else 10, record, artifact_root=root)
                self.assertFalse(passed)
                self.assertTrue((root / 'case.json').is_file())
                self.assertEqual((root / 'completed-compile.txt').read_text(), 'retained before execution')
                self.assertEqual((root / 'compile-cache/candidate').read_text(), 'retained failed input')
                self.assertEqual(record['compile_cache_disposition'], 'retained')
                self.assertEqual(record['status'], 'timeout' if timed_out else 'finished')
                self.assertEqual(record['executed_test_count'], None if timed_out else 1)
                self.assertEqual(record['process_cleanup_status'], 'confirmed')
                self.assertEqual(record['hosted_cleanup_status'], 'not_observed')
                self.assertNotIn('cleanup_confirmed', record)

    def test_transaction_capture_failures_retain_passing_scratch_with_or_without_record(self):
        controls = {
            'artifact capture issue': {'phase': 'compiler_completed', 'files': [],
                                       'issues': ['source grew beyond diagnostic bound']},
            'issue array corrupt': {'phase': 'compiler_completed', 'files': [], 'issues': 'lost'},
            'file array corrupt': {'phase': 'compiler_completed', 'files': None, 'issues': []},
            'incomplete phase': {'phase': 'compiler_started', 'files': [], 'issues': []},
            'invalid json': '{',
            'missing marker': None,
        }
        for index, (label, report) in enumerate(controls.items()):
            for returned_record in (False, True):
                with self.subTest(label=label, returned_record=returned_record):
                    root = Path(self.tmp.name) / f'capture-{index}-{returned_record}'
                    record = {} if returned_record else None
                    def run(args, timeout, environment=None):
                        transaction = root / 'compiler-transactions/one'
                        transaction.mkdir(parents=True)
                        # The producer issues a request marker before writing
                        # transaction.json; absence after issuance is a failure.
                        (transaction / 'compiler-request.bin').write_bytes(b'issued request')
                        if report is not None:
                            (transaction / 'transaction.json').write_text(
                                report if isinstance(report, str) else json.dumps(report))
                        return completed_process(args, 0,
                            'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                    with patch.object(runner, 'execute', side_effect=run):
                        passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                            10, record, artifact_root=root)
                    self.assertTrue(passed)
                    self.assertTrue(root.exists())
                    if returned_record:
                        self.assertFalse(record['diagnostic_evidence_complete'])
                        self.assertTrue(record['diagnostic_summaries']['issues'])
                        self.assertNotIn('artifacts_removed_after_success', record)
                        self.assertTrue(record['artifacts_retained_after_success'])
                        self.assertEqual(record['artifact_disposition'], 'retained')

    def test_artifact_disposition_matches_observed_outcomes_across_operation_combinations(self):
        # Independently classify filesystem disposition after each run. Reusing
        # the outcome record must not carry a previous case's retention claim.
        record = {}
        for index, (libtest_passes, cleanup, complete, diagnostic, retain) in enumerate(
                itertools.product((False, True), repeat=5)):
            with self.subTest(libtest_passes=libtest_passes, cleanup=cleanup,
                              complete=complete, diagnostic=diagnostic, retain=retain):
                root = Path(self.tmp.name) / f'disposition-{index}'
                result = completed_process([], 0 if libtest_passes else 101,
                    'test result: ok. 1 passed; 0 failed; 0 ignored;\n' if libtest_passes else
                    'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', '')
                result.cleanup_confirmed = cleanup
                with patch.object(runner, 'execute', return_value=result), \
                     patch.object(runner, 'case_artifact_evidence',
                                  return_value=(cleanup, False, complete, '')), \
                     patch.dict(os.environ):
                    if diagnostic:
                        os.environ['TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS'] = '1'
                    else:
                        os.environ.pop('TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS', None)
                    passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                        10, record, artifact_root=root, retain_artifacts=retain)
                expected_pass = libtest_passes and cleanup
                expected_retained = not (expected_pass and complete and not diagnostic and not retain)
                self.assertEqual(passed, expected_pass)
                self.assertEqual(root.is_dir(), expected_retained)
                self.assertEqual(record['artifact_disposition'], 'retained' if expected_retained else 'removed')
                self.assertEqual(record.get('artifacts_retained_after_success', False),
                                 expected_retained and expected_pass)
                self.assertEqual(record.get('artifacts_retained_after_failure', False),
                                 expected_retained and not expected_pass)
                self.assertEqual(record.get('artifacts_removed_after_success', False), not expected_retained)

    def test_missing_artifact_root_never_claims_retention(self):
        root = Path(self.tmp.name) / 'missing-artifacts'
        def run(args, timeout, environment=None):
            import shutil
            shutil.rmtree(root)
            return completed_process(args, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root, retain_artifacts=True)
        self.assertTrue(passed)
        self.assertEqual(record['artifact_disposition'], 'missing')
        self.assertNotIn('artifacts_retained_after_success', record)

    def test_scratch_without_capture_markers_has_no_transaction_report_obligation(self):
        root = Path(self.tmp.name) / 'unstarted-scratch'
        directory = root / 'compiler-transactions/scratch'
        directory.mkdir(parents=True)
        # Real pre-admission/cached scratch retains these inputs without ever
        # calling CompilerDiagnosticCapture.start.
        (directory / 'turn.txt').write_text('pure ()')
        (directory / 'template-0.hs').write_text('module Main where')
        (directory / 'module-candidates.cbor').write_bytes(b'candidate inputs')
        summary = runner.diagnostic_summaries(root)
        self.assertEqual(summary['transaction_count'], 0)
        self.assertEqual(summary['scratch_without_capture_marker_count'], 1)
        self.assertEqual(summary['issues'], [])
        self.assertNotIn('physical_compiler_timing', summary)

    def test_partial_capture_markers_require_the_issued_transaction_report(self):
        for marker in ('compiler-request.bin', 'compiler-cwd.bin', 'transaction.json.pending',
                       'compiler.stderr', 'consumed-sources.json'):
            with self.subTest(marker=marker):
                root = Path(self.tmp.name) / marker
                directory = root / 'compiler-transactions/issued'
                directory.mkdir(parents=True)
                (directory / marker).write_bytes(b'partial issued diagnostic')
                summary = runner.diagnostic_summaries(root)
                self.assertEqual(summary['transaction_count'], 1)
                self.assertEqual(summary['scratch_without_capture_marker_count'], 0)
                self.assertTrue(summary['issues'])

    def test_transaction_discovery_and_capture_retention_are_bounded_separately(self):
        root = Path(self.tmp.name) / 'bounded-transactions'
        for index in range(5):
            directory = root / f'compiler-transactions/{index}'
            directory.mkdir(parents=True)
            (directory / 'transaction.json').write_text(json.dumps({
                'phase': 'compiler_completed', 'files': [], 'issues': []}))
        with patch.object(runner, 'TRANSACTION_CAPTURE_LIMIT', 2):
            summary = runner.diagnostic_summaries(root)
        self.assertEqual(summary['transaction_count'], 5)
        self.assertEqual(len(summary['transactions']), 2)
        self.assertTrue(summary['transactions_truncated'])
        self.assertTrue(summary['transaction_discovery_complete'])
        with patch.object(runner, 'TRANSACTION_DIRECTORY_LIMIT', 3):
            summary = runner.diagnostic_summaries(root)
        self.assertEqual(summary['transaction_count'], 3)
        self.assertFalse(summary['transaction_discovery_complete'])
        self.assertTrue(summary['issues'])

    def test_transaction_capture_issue_summary_preserves_total_and_truncation(self):
        root = Path(self.tmp.name) / 'capture-issues'
        transaction = root / 'compiler-transactions/one'
        transaction.mkdir(parents=True)
        (transaction / 'transaction.json').write_text(json.dumps({
            'phase': 'compiler_completed', 'files': [], 'issues': ['x' * 3000] * 12}))
        summary = runner.diagnostic_summaries(root)
        self.assertTrue(summary['issues'])
        transaction = summary['transactions'][0]
        self.assertEqual(transaction['issue_count'], 12)
        self.assertTrue(transaction['issues_truncated'])
        self.assertEqual(len(transaction['issues']), 8)
        self.assertEqual(len(transaction['issues'][0]), 2048)

    def test_exception_paths_capture_hosted_cleanup_and_keep_process_fact_separate(self):
        for index, error in enumerate((subprocess.TimeoutExpired([], 1),
                                      runner.RunnerInterrupted(signal.SIGTERM), OSError('spawn failed'))):
            with self.subTest(exception=type(error).__name__):
                root = Path(self.tmp.name) / f'exception-cleanup-{index}'
                record = {}
                error.cleanup_confirmed = True
                def run(args, timeout, environment=None):
                    campaign = root / 'hosted-campaign-1'
                    campaign.mkdir()
                    (campaign / 'hosted-outcome.json').write_text(json.dumps({
                        'scenario': {'status': 'failed'}, 'cleanup': {'status': 'unknown'}}))
                    raise error
                with patch.object(runner, 'execute', side_effect=run):
                    passed, _, errors = runner.run_one(str(self.binary), 'suite::works', False,
                        10, record, artifact_root=root)
                self.assertFalse(passed)
                self.assertTrue(root.exists())
                self.assertEqual(record['process_cleanup_status'], 'confirmed')
                self.assertEqual(record['hosted_cleanup_status'], 'unknown')
                self.assertEqual(record['compiler_cleanup_status'], 'not_observed')
                self.assertIn('cleanup remains unknown', errors)
                self.assertNotIn('cleanup_confirmed', record)

    def test_missing_or_corrupt_hosted_cleanup_never_becomes_process_cleanup(self):
        for index, report in enumerate((None, '{', {'cleanup': {'status': []}})):
            with self.subTest(report=report):
                root = Path(self.tmp.name) / f'corrupt-hosted-{index}'
                record = {}
                def run(args, timeout, environment=None):
                    campaign = root / 'hosted-campaign-1'
                    campaign.mkdir()
                    if report is not None:
                        (campaign / 'hosted-outcome.json').write_text(
                            report if isinstance(report, str) else json.dumps(report))
                    return completed_process(args, 0,
                        'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                with patch.object(runner, 'execute', side_effect=run):
                    passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                        10, record, artifact_root=root)
                self.assertFalse(passed)
                self.assertTrue(root.exists())
                self.assertEqual(record['process_cleanup_status'], 'confirmed')
                self.assertEqual(record['hosted_cleanup_status'], 'unknown')

    def test_failed_process_creation_and_prelaunch_interruption_are_not_teardown_receipts(self):
        with patch.object(runner.subprocess, 'Popen', side_effect=OSError('creation refused')):
            record = {}
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, record)
        self.assertFalse(passed)
        self.assertEqual(record['process_cleanup_status'], 'not_started')
        self.assertEqual(record['hosted_cleanup_status'], 'not_observed')
        self.assertEqual(record['compiler_cleanup_status'], 'not_observed')
        with patch.object(runner, 'INTERRUPT_SIGNAL', signal.SIGTERM), \
             patch.object(runner.subprocess, 'Popen') as launch:
            record = {}
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, record)
        self.assertFalse(passed)
        launch.assert_not_called()
        self.assertEqual(record['process_cleanup_status'], 'not_started')

    def test_corrupt_owned_compiler_cleanup_cannot_certify_evidence(self):
        root = Path(self.tmp.name) / 'corrupt-compiler-cleanup'
        record = {}
        def run(args, timeout, environment=None):
            compiler = root / 'compiler'
            compiler.mkdir()
            (compiler / 'owned-compiler-outcome.json').write_text(json.dumps({'cleanup': {'status': []}}))
            (compiler / 'lifecycle.json').write_text('{}')
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root)
        self.assertFalse(passed)
        self.assertTrue(root.exists())
        self.assertFalse(record['diagnostic_evidence_complete'])
        self.assertEqual(record['process_cleanup_status'], 'confirmed')
        self.assertEqual(record['compiler_cleanup_status'], 'unknown')

    def test_nested_owned_compiler_receipts_gate_cleanup_independently_of_timing(self):
        for status in ('confirmed', 'unconfirmed', 'failed'):
            with self.subTest(status=status):
                root = Path(self.tmp.name) / f'nested-owner-{status}'
                record = {}
                def run(args, timeout, environment=None):
                    compiler = root / 'owned-compiler-control-1/compiler'
                    compiler.mkdir(parents=True)
                    (compiler / 'owned-compiler-outcome.json').write_text(json.dumps({
                        'cleanup': {'status': status}}))
                    (compiler / 'lifecycle.json').write_text('{}')
                    return completed_process(args, 0,
                        'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                with patch.object(runner, 'execute', side_effect=run):
                    passed, _, errors = runner.run_one(str(self.binary), 'suite::works', False,
                        10, record, artifact_root=root)
                self.assertEqual(passed, status == 'confirmed')
                self.assertTrue(root.exists(), 'missing timing retains diagnostics even after confirmed cleanup')
                self.assertEqual(record['compiler_cleanup_status'], status)
                self.assertEqual(record['process_cleanup_status'], 'confirmed')
                self.assertEqual(record['executed_test_count'], 1)
                if status != 'confirmed':
                    self.assertIn(f'owned compiler cleanup remains {status}', errors)

    def test_owned_lifecycle_with_missing_or_dangling_outcome_retains_unknown_cleanup(self):
        for nested in (False, True):
            for marker in ('lifecycle', 'dangling_outcome', 'dangling_lifecycle'):
                with self.subTest(nested=nested, marker=marker):
                    root = Path(self.tmp.name) / f'missing-owned-outcome-{nested}-{marker}'
                    record = {}
                    def run(args, timeout, environment=None):
                        compiler = root / ('owned-compiler-control-1/compiler' if nested else 'compiler')
                        compiler.mkdir(parents=True)
                        if marker == 'dangling_lifecycle':
                            (compiler / 'lifecycle.json').symlink_to(compiler / 'absent.json')
                        else:
                            (compiler / 'lifecycle.json').write_text('{}')
                        if marker == 'dangling_outcome':
                            (compiler / 'owned-compiler-outcome.json').symlink_to(compiler / 'absent.json')
                        return completed_process(args, 0,
                            'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                    with patch.object(runner, 'execute', side_effect=run):
                        passed, _, errors = runner.run_one(str(self.binary), 'suite::works', False,
                            10, record, artifact_root=root)
                    self.assertFalse(passed)
                    self.assertTrue(root.exists())
                    self.assertEqual(record['executed_test_count'], 1)
                    self.assertEqual(record['process_cleanup_status'], 'confirmed')
                    self.assertEqual(record['compiler_cleanup_status'], 'unknown')
                    self.assertIn('owned compiler cleanup remains unknown', errors)

    def test_compiler_cleanup_discovery_bounds_and_unsafe_paths_are_not_confirmed_subsets(self):
        def confirmed_owner(path):
            path.mkdir(parents=True)
            (path / 'owned-compiler-outcome.json').write_text(json.dumps({'cleanup': {'status': 'confirmed'}}))
            (path / 'lifecycle.json').write_text('{}')
        for kind in ('complete', 'root_limit', 'control_limit', 'child_limit',
                     'unsafe_control', 'unsafe_child', 'unsafe_compiler_root', 'unsafe_compiler_file'):
            with self.subTest(kind=kind):
                root = Path(self.tmp.name) / f'owner-discovery-{kind}'
                record = {}
                def run(args, timeout, environment=None):
                    if kind in ('complete', 'root_limit'):
                        for index in range(8 if kind == 'complete' else 9):
                            confirmed_owner(root / f'owned-compiler-control-{index}/compiler')
                    elif kind == 'control_limit':
                        confirmed_owner(root / 'owned-compiler-control-0/compiler')
                        for index in range(1, 65):
                            (root / f'owned-compiler-control-{index}').mkdir()
                    elif kind == 'child_limit':
                        control = root / 'owned-compiler-control-0'
                        confirmed_owner(control / 'compiler')
                        for index in range(32):
                            (control / f'empty-{index}').mkdir()
                    elif kind == 'unsafe_compiler_file':
                        (root / 'compiler').write_text('not an owner directory')
                    else:
                        foreign = Path(self.tmp.name) / f'foreign-{kind}'
                        confirmed_owner(foreign / 'compiler')
                        if kind == 'unsafe_control':
                            (root / 'owned-compiler-control-link').symlink_to(foreign, target_is_directory=True)
                        elif kind == 'unsafe_child':
                            control = root / 'owned-compiler-control-0'
                            control.mkdir()
                            (control / 'compiler').symlink_to(foreign / 'compiler', target_is_directory=True)
                        else:
                            (root / 'compiler').symlink_to(foreign / 'compiler', target_is_directory=True)
                    return completed_process(args, 0,
                        'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                with patch.object(runner, 'execute', side_effect=run):
                    passed, _, errors = runner.run_one(str(self.binary), 'suite::works', False,
                        10, record, artifact_root=root)
                self.assertEqual(passed, kind == 'complete')
                self.assertTrue(root.exists())
                self.assertEqual(record['executed_test_count'], 1)
                self.assertEqual(record['process_cleanup_status'], 'confirmed')
                self.assertEqual(record['compiler_cleanup_observation_complete'], kind == 'complete')
                self.assertEqual(record['compiler_cleanup_status'], 'confirmed' if kind == 'complete' else 'unknown')
                discovery = record['diagnostic_summaries']['owned_compiler_discovery']
                self.assertEqual(discovery['complete'], kind == 'complete')
                if kind == 'complete':
                    self.assertEqual(len(record['diagnostic_summaries']['owned_compiler_roots']), 8)
                    self.assertFalse(record['diagnostic_evidence_complete'], 'timing absence is independent')
                else:
                    self.assertIn('unsafe_compiler_root' if kind == 'unsafe_compiler_file' else kind,
                                  [issue['kind'] for issue in discovery['issues']])
                    self.assertIn('owned compiler cleanup observation is incomplete', errors)

    def test_invalid_utf8_native_output_keeps_counts_and_cleanup_evidence(self):
        root = Path(self.tmp.name) / 'invalid-output'
        self.binary.write_text('#!' + sys.executable + '\n' +
            'import json, os, pathlib\n' +
            'campaign = pathlib.Path(os.environ["TIDEPOOL_TEST_ARTIFACT_ROOT"]) / "hosted-campaign-1"\n' +
            'campaign.mkdir()\n' +
            '(campaign / "hosted-outcome.json").write_text(json.dumps({"cleanup": {"status": "unknown"}}))\n' +
            'os.write(1, b"\\xff\\ntest result: ok. 1 passed; 0 failed; 0 ignored;\\n")\n' +
            'os.write(2, b"\\xfe\\n")\n')
        self.binary.chmod(0o700)
        record = {}
        passed, stdout, stderr = runner.run_one(str(self.binary), 'suite::works', False,
            10, record, artifact_root=root)
        self.assertFalse(passed)
        self.assertTrue(root.exists())
        self.assertEqual(record['executed_test_count'], 1)
        self.assertEqual(record['process_cleanup_status'], 'confirmed')
        self.assertEqual(record['hosted_cleanup_status'], 'unknown')
        self.assertIn('\ufffd', stdout)
        self.assertIn('\ufffd', stderr)

    def test_unexpected_observation_error_settles_artifacts_without_hiding_interrupts(self):
        root = Path(self.tmp.name) / 'unexpected-observation'
        record = {}
        with patch.object(runner, 'execute', side_effect=ValueError('observation failed')):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root)
        self.assertFalse(passed)
        self.assertTrue(root.exists())
        self.assertEqual(record['status'], 'runner_failed')
        self.assertEqual(record['process_cleanup_status'], 'unknown')
        self.assertIsNone(record['process_execution_count'])
        for error in (KeyboardInterrupt(), SystemExit(1)):
            with self.subTest(exception=type(error).__name__), \
                 patch.object(runner, 'execute', side_effect=error):
                with self.assertRaises(type(error)):
                    runner.run_one(str(self.binary), 'suite::works', False, 10, {})

    def test_case_environment_isolated_and_success_removes_diagnostics(self):
        root = Path(self.tmp.name) / 'successful-artifacts'
        ambient = os.environ.get('TIDEPOOL_TEST_ARTIFACT_ROOT')
        def run(args, timeout, environment=None):
            self.assertEqual(environment['TIDEPOOL_TEST_ARTIFACT_ROOT'], str(root))
            self.assertEqual(environment['TIDEPOOL_TEST_DIAGNOSTIC_SCOPE'], '1')
            self.assertEqual(os.environ.get('TIDEPOOL_TEST_ARTIFACT_ROOT'), ambient)
            (root / 'completed-compile.txt').write_text('diagnostics')
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root)
        self.assertTrue(passed)
        self.assertFalse(root.exists())
        self.assertTrue(record['artifacts_removed_after_success'])

    def test_retain_artifacts_option_controls_passing_case_cleanup(self):
        self.binary.write_text(
            '#!' + sys.executable + '\n'
            'import os, pathlib, sys\n'
            'args = sys.argv[1:]\n'
            'if "--list" in args:\n'
            '    print("suite::works: test\\nsuite::ignored: test" if "--ignored" not in args else "suite::ignored: test")\n'
            'elif "--exact" in args:\n'
            '    root = pathlib.Path(os.environ["TIDEPOOL_TEST_ARTIFACT_ROOT"])\n'
            '    (root / "host.jsonl").write_text("{\\\"phase\\\":\\\"retained\\\"}\\n")\n'
            '    print("test result: ok. 1 passed; 0 failed; 0 ignored;")\n'
        )
        self.binary.chmod(0o700)

        for retain in (False, True):
            with self.subTest(retain=retain):
                output_dir = Path(self.tmp.name) / f'counted-retain-{retain}'
                arguments = [
                    str(self.binary), '--exact', 'suite::works', '--expected-count', '1',
                    '--output-dir', str(output_dir),
                ]
                if retain:
                    arguments.append('--retain-artifacts')
                output = io.StringIO()
                errors = io.StringIO()
                with patch.dict(os.environ):
                    os.environ.pop('TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS', None)
                    with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                        result = runner.main(arguments)

                key = hashlib.sha256(b'suite::works').hexdigest()
                artifact_root = output_dir / key / 'artifacts'
                case_record = json.loads((output_dir / f'{key}.json').read_text())
                self.assertEqual(result, 0, errors.getvalue())
                self.assertIn('Isolated libtest: 1 passed; 0 failed;', output.getvalue())
                self.assertEqual(artifact_root.exists(), retain)
                if retain:
                    self.assertTrue(case_record['execution']['artifacts_retained_after_success'])
                    self.assertNotIn('artifacts_removed_after_success', case_record['execution'])
                else:
                    self.assertTrue(case_record['execution']['artifacts_removed_after_success'])
                    self.assertNotIn('artifacts_retained_after_success', case_record['execution'])

    def test_retain_artifacts_option_requires_output_directory(self):
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors), self.assertRaises(SystemExit) as raised:
            runner.parse_args([str(self.binary), '--retain-artifacts'])

        self.assertEqual(raised.exception.code, 2)
        self.assertIn('--retain-artifacts requires --output-dir', errors.getvalue())

    def test_owned_compiler_receives_case_root_without_changing_libtest_selection(self):
        root = Path(self.tmp.name) / 'resident-artifacts'
        frontend = Path(self.tmp.name) / 'declared-frontend'
        frontend.write_text('declared compiler frontend')
        def run(args, timeout, environment=None):
            self.assertEqual(args, [str(frontend), '--owned-daemon-run',
                str(root / 'compiler'), '--', str(self.binary), '--exact', 'suite::works', '--nocapture'])
            self.assertEqual(environment['TIDEPOOL_TEST_ARTIFACT_ROOT'], str(root))
            (root / 'compiler').mkdir()
            (root / 'compiler/owned-compiler-outcome.json').write_text(json.dumps({'cleanup': {'status': 'confirmed'}}))
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        with patch.dict(os.environ, {'TIDEPOOL_EXTRACT': str(frontend)}), \
             patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, artifact_root=root, compiler_mode='owned-resident')
        self.assertTrue(passed)

    def test_owned_compiler_allowances_forward_without_changing_child_or_worker_count(self):
        frontend = Path(self.tmp.name) / 'allowance-frontend'
        frontend.write_text('declared compiler frontend')
        for width in (2, 8, 16):
            root = Path(self.tmp.name) / f'allowance-{width}'
            record = {}
            def run(args, timeout, environment=None):
                self.assertEqual(args, [str(frontend), '--owned-daemon-run', str(root / 'compiler'),
                    '--foreground-jobs', str(width), '--preparation-jobs', str(width + 1),
                    '--', str(self.binary), '--exact', 'suite::works', '--nocapture'])
                case = json.loads((root / 'case.json').read_text())
                self.assertEqual(case['compiler_allowances'], record['compiler_allowances'])
                (root / 'compiler').mkdir()
                (root / 'compiler/owned-compiler-outcome.json').write_text(json.dumps({'cleanup': {'status': 'confirmed'}}))
                return completed_process(args, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
            with patch.dict(os.environ, {'TIDEPOOL_EXTRACT': str(frontend)}), \
                 patch.object(runner, 'execute', side_effect=run):
                passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, record,
                    artifact_root=root, compiler_mode='owned-resident', foreground_jobs=width, preparation_jobs=width + 1)
            self.assertTrue(passed)
            self.assertEqual(record['compiler_allowances'], {'requested_foreground_jobs': width,
                'requested_preparation_jobs': width + 1, 'worker_processes': 1})

    def test_compiler_allowance_parser_refuses_invalid_values_and_direct_mode(self):
        for flag in ('--foreground-jobs', '--preparation-jobs'):
            for value in ('0', '-1', 'invalid', str(1 << 32), str(1 << 64)):
                with self.subTest(flag=flag, value=value), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    runner.parse_args([str(self.binary), '--compiler-mode', 'owned-resident',
                                       '--output-dir', self.tmp.name, flag, value])
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                runner.parse_args([str(self.binary), flag, '2'])
            options = runner.parse_args([str(self.binary), '--compiler-mode', 'owned-resident',
                                         '--output-dir', self.tmp.name, flag, '16', '--jobs', '3'])
            self.assertEqual(options.jobs, 3)
            self.assertEqual(getattr(options, flag[2:].replace('-', '_')), 16)
            options = runner.parse_args([str(self.binary), '--compiler-mode', 'owned-resident',
                                         '--output-dir', self.tmp.name, flag, str((1 << 32) - 1)])
            self.assertEqual(getattr(options, flag[2:].replace('-', '_')), (1 << 32) - 1)

    def test_trace_profile_reaches_child_and_is_recorded(self):
        root = Path(self.tmp.name) / 'minimal-trace-case'
        observed = {}

        def run(args, timeout, environment=None):
            observed.update(environment or {})
            return completed_process(args, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')

        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, {},
                artifact_root=root, retain_artifacts=True, trace_profile='minimal')
        self.assertTrue(passed)
        self.assertEqual(observed['TIDEPOOL_TEST_TRACE_PROFILE'], 'minimal')
        self.assertEqual(observed['TIDEPOOL_TIMING'], '0')
        self.assertEqual(observed['TIDEPOOL_TIMING_SUMMARY'], '1')
        case = json.loads((root / 'case.json').read_text())
        self.assertEqual(case['trace_profile'], 'minimal')

    def test_trace_profile_parser_requires_retained_case_artifacts(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            runner.parse_args([str(self.binary), '--trace-profile', 'minimal'])
        options = runner.parse_args([str(self.binary), '--trace-profile', 'full',
            '--output-dir', self.tmp.name, '--retain-artifacts'])
        self.assertEqual(options.trace_profile, 'full')

    def test_compiler_allowance_domain_refuses_before_root_identity_or_launch(self):
        for flag in ('foreground_jobs', 'preparation_jobs'):
            root = Path(self.tmp.name) / flag
            with self.subTest(flag=flag), patch.object(runner, 'capture_launch_inputs') as identity, \
                 patch.object(runner, 'execute') as execute, self.assertRaisesRegex(ValueError, 'positive u32'):
                runner.run_one(str(self.binary), 'suite::works', False, 10, {},
                    artifact_root=root, compiler_mode='owned-resident', **{flag: 1 << 32})
            self.assertFalse(root.exists())
            identity.assert_not_called()
            execute.assert_not_called()

    def test_compiler_grant_summary_preserves_capacity_caps_and_missing_evidence(self):
        requested = {'requested_foreground_jobs': 16, 'requested_preparation_jobs': 8}
        rows = [{'daemon_epoch': 'epoch-a', 'admission_id': i + 1, 'queue_ms': 0,
                 'row': {'fields': {'compiler_workload': workload, 'compiler_jobs': jobs,
                                    'compiler_capabilities': capabilities}}}
                for i, (workload, jobs, capabilities) in enumerate(
                    [('foreground', 4, 4), ('foreground', 16, 12), ('preparation', 8, 6)])]
        summary = runner.compiler_job_grants({'records': rows, 'complete': True}, requested)
        self.assertTrue(summary['complete'])
        self.assertEqual(summary['observed_jobs'], [4, 8, 16])
        self.assertEqual([row['below_requested_maximum'] for row in summary['admissions']], [True, False, False])
        self.assertEqual([row['requested_jobs'] for row in summary['admissions']], [16, 16, 8])
        for key, values in (('compiler_jobs', [None, 0, True, '16']),
                            ('compiler_capabilities', [None, 0, False, '8']),
                            ('compiler_workload', [None, 'Foreground', 'typo'])):
            for value in values:
                with self.subTest(key=key, value=value):
                    altered = json.loads(json.dumps(rows))
                    altered[0]['row']['fields'][key] = value
                    invalid = runner.compiler_job_grants({'records': altered, 'complete': True}, requested)
                    self.assertFalse(invalid['complete'])
                    self.assertEqual(invalid['invalid_grant_count'], 1)
                    self.assertIsNone(invalid['admissions'][0]['below_requested_maximum'])
        self.assertFalse(runner.compiler_job_grants({'records': rows, 'complete': False}, requested)['complete'])
        self.assertFalse(runner.compiler_job_grants({}, requested)['complete'])

    def test_launch_hashes_capture_actual_binary_and_compiler_files_before_execution(self):
        frontend, worker = [Path(self.tmp.name) / name for name in ('frontend', 'worker')]
        frontend.write_bytes(b'frontend at launch')
        worker.write_bytes(b'worker at launch')
        record = {}
        def run(args, timeout, environment=None):
            captured = record['launch_inputs']
            self.assertEqual(captured['executable']['sha256'], hashlib.sha256(self.binary.read_bytes()).hexdigest())
            self.assertEqual(captured['resources']['TIDEPOOL_EXTRACT']['sha256'], hashlib.sha256(frontend.read_bytes()).hexdigest())
            self.assertEqual(captured['resources']['TIDEPOOL_EXTRACT_WORKER']['sha256'], hashlib.sha256(worker.read_bytes()).hexdigest())
            # A later relink cannot replace the already captured launch identity.
            self.binary.write_bytes(b'new link after launch')
            return completed_process(args, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        launch_hash = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        with patch.dict(os.environ, {'TIDEPOOL_EXTRACT': str(frontend), 'TIDEPOOL_EXTRACT_WORKER': str(worker)}), \
             patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, record)
        self.assertTrue(passed)
        self.assertEqual(record['launch_inputs']['executable']['sha256'], launch_hash)
        self.assertTrue(record['launch_inputs']['resource_hashes_complete'])

    def test_launch_input_disappearance_or_replacement_refuses_execution(self):
        digest = runner.hashlib.file_digest
        for replacement in (False, True):
            with self.subTest(replacement=replacement):
                self.binary.write_bytes(b'before launch')
                record = {}
                def change(stream, algorithm):
                    result = digest(stream, algorithm)
                    self.binary.unlink()
                    if replacement:
                        self.binary.write_bytes(b'replaced before launch')
                    return result
                with patch.object(runner.hashlib, 'file_digest', side_effect=change), \
                     patch.object(runner, 'execute') as execute:
                    passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, record)
                self.assertFalse(passed)
                execute.assert_not_called()
                self.assertEqual(record['status'], 'launch_identity_failed')
                self.assertEqual(record['launch_inputs']['status'], 'UNKNOWN')
                self.assertEqual(record['process_execution_count'], 0)

    def test_missing_declared_file_refuses_and_directory_resource_is_explicitly_unknown(self):
        directory = Path(self.tmp.name) / 'resources'
        directory.mkdir()
        record = {}
        with patch.dict(os.environ, {'DECLARED': str(directory)}), patch.object(runner, 'execute',
                return_value=completed_process([], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, record, declared_resources=('DECLARED',))
        self.assertTrue(passed)
        self.assertEqual(record['launch_inputs']['resources']['DECLARED']['status'], 'UNKNOWN')
        self.assertFalse(record['launch_inputs']['resource_hashes_complete'])
        with patch.dict(os.environ, {'DECLARED': str(directory / 'missing')}), patch.object(runner, 'execute') as execute:
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, {}, declared_resources=('DECLARED',))
        self.assertFalse(passed)
        execute.assert_not_called()

    def test_binary_change_during_compiler_hash_capture_is_refused(self):
        worker = Path(self.tmp.name) / 'worker'
        worker.write_bytes(b'worker')
        digest = runner.hashlib.file_digest
        def change(stream, algorithm):
            result = digest(stream, algorithm)
            if Path(stream.name) == worker:
                self.binary.write_bytes(b'changed while hashing worker')
            return result
        with patch.dict(os.environ, {'TIDEPOOL_EXTRACT_WORKER': str(worker)}), \
             patch.object(runner.hashlib, 'file_digest', side_effect=change), patch.object(runner, 'execute') as execute:
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False, 10, {})
        self.assertFalse(passed)
        execute.assert_not_called()

    def test_passing_diagnostic_startup_retains_marked_campaign_evidence(self):
        root = Path(self.tmp.name) / 'diagnostic-startup'
        startup = {'policy': {'mode': 'diagnostic', 'seconds': 600},
                   'elapsed_ms': 310000, 'over_baseline_budget': True,
                   'outcome': {'status': 'ready'}}
        def run(args, timeout, environment=None):
            self.assertEqual(environment['TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS'], '600')
            campaign = root / 'hosted-campaign-1'
            campaign.mkdir()
            (campaign / 'hosted-outcome.json').write_text(json.dumps({
                'schema': 2, 'startup': startup,
                'scenario': {'status': 'passed'}, 'cleanup': {'status': 'confirmed'}}))
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.dict(os.environ, {'TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS': '600'}), \
             patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                                         900, record, artifact_root=root)
        self.assertTrue(passed)
        self.assertTrue(root.exists())
        self.assertEqual(record['startup_diagnostic_seconds'], '600')
        self.assertEqual(record['cleanup_reports'][0]['startup'], startup)
        self.assertNotIn('artifacts_removed_after_success', record)

    def test_passing_libtest_with_unknown_campaign_cleanup_keeps_evidence(self):
        root = Path(self.tmp.name) / 'unconfirmed-artifacts'
        def run(args, timeout, environment=None):
            campaign = root / 'hosted-campaign-1'
            campaign.mkdir()
            (campaign / 'hosted-outcome.json').write_text(json.dumps({
                'scenario': {'status': 'passed'}, 'cleanup': {'status': 'unknown'}}))
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, errors = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root)
        self.assertFalse(passed)
        self.assertEqual(record['executed_test_count'], 1)
        self.assertEqual(record['exit_code'], 0)
        self.assertTrue(root.exists())
        self.assertIn('cleanup remains unknown', errors)

    def test_startup_non_admission_is_distinct_from_host_or_process_cleanup(self):
        valid = {
            'schema': 2,
            'startup': {'policy': {'mode': 'standard'}},
            'scenario': {'status': 'failed', 'phase': 'startup'},
            'cleanup': {'status': 'not_started', 'domain': 'host_runtime',
                        'owner_admission': 'not_admitted', 'executor_joined': True},
        }
        controls = [
            ('observed refusal', {}, True, 0, True),
            ('admitted host', {'owner_admission': 'admitted'}, True, 0, False),
            ('executor not joined', {'executor_joined': False}, True, 0, False),
            ('executor evidence not boolean', {'executor_joined': 1}, True, 0, False),
            ('foreign domain', {'domain': 'compiler'}, True, 0, False),
            ('unknown teardown', {'status': 'unknown'}, True, 0, False),
            ('process cleanup unknown', {}, False, 0, False),
            ('test failed', {}, True, 101, False),
        ]
        for index, (label, changes, process_cleanup, exit_code, expected) in enumerate(controls):
            with self.subTest(label=label):
                root = Path(self.tmp.name) / f'refusal-{index}'
                outcome = json.loads(json.dumps(valid))
                outcome['cleanup'].update(changes)
                def run(args, timeout, environment=None):
                    campaign = root / 'hosted-campaign-1'
                    campaign.mkdir()
                    (campaign / 'hosted-outcome.json').write_text(json.dumps(outcome))
                    result = completed_process(args, exit_code,
                        'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                    result.cleanup_confirmed = process_cleanup
                    return result
                record = {}
                with patch.object(runner, 'execute', side_effect=run):
                    passed, _, _ = runner.run_one(str(self.binary), 'suite::refusal', False,
                        10, record, artifact_root=root)
                self.assertEqual(passed, expected)
                self.assertTrue(root.exists())
                self.assertEqual(record['process_cleanup_status'],
                                 'confirmed' if process_cleanup else 'unconfirmed')
                self.assertEqual(record['hosted_cleanup_status'],
                    'not_started' if record['cleanup_reports'][0]['host_runtime_not_started'] else
                    'unknown' if changes.get('status') == 'unknown' else 'unconfirmed')
                self.assertNotIn('artifacts_removed_after_success', record)
                self.assertEqual(record['cleanup_reports'][0]['status'], outcome['cleanup']['status'])

    def test_host_runtime_non_admission_requires_integer_schema_version(self):
        for index, schema in enumerate((True, 1.0)):
            with self.subTest(schema=schema, type=type(schema).__name__):
                root = Path(self.tmp.name) / f'invalid-schema-{index}'
                def run(args, timeout, environment=None):
                    campaign = root / 'hosted-campaign-1'
                    campaign.mkdir()
                    (campaign / 'hosted-outcome.json').write_text(json.dumps({
                        'schema': schema,
                        'scenario': {'status': 'failed', 'phase': 'startup'},
                        'cleanup': {'status': 'not_started', 'domain': 'host_runtime',
                                    'owner_admission': 'not_admitted', 'executor_joined': True},
                    }))
                    return completed_process(args, 0,
                        'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                record = {}
                with patch.object(runner, 'execute', side_effect=run):
                    passed, _, errors = runner.run_one(str(self.binary), 'suite::refusal',
                        False, 10, record, artifact_root=root)
                self.assertFalse(passed)
                self.assertFalse(record['cleanup_reports'][0]['host_runtime_not_started'])
                self.assertIn('cleanup remains not_started', errors)
                self.assertTrue(root.exists())

    def test_standard_property_campaign_controls_cross_delegation_explicitly(self):
        campaign = {
            'PROPTEST_CASES': '4000', 'PROPTEST_RNG_SEED': '781231',
            'PROPTEST_RNG_ALGORITHM': 'cc', 'PROPTEST_MAX_SHRINK_ITERS': '32',
            'PROPTEST_MAX_LOCAL_REJECTS': '64', 'PROPTEST_MAX_GLOBAL_REJECTS': '64',
            'PROPTEST_MAX_FLAT_MAP_REGENS': '64', 'PROPTEST_MAX_SHRINK_TIME': '100',
            'PROPTEST_MAX_DEFAULT_SIZE_RANGE': '64', 'PROPTEST_FORK': 'true',
            'PROPTEST_TIMEOUT': '1000', 'PROPTEST_VERBOSE': '1',
            'PROPTEST_DISABLE_FAILURE_PERSISTENCE': '1',
        }
        unrelated = {'PROPTEST_PRIVATE_TOKEN': 'private', 'OPENAI_API_KEY': 'private'}
        command, _ = runner.delegated_command(['/libtest'], 10, 'app.slice', {},
            environment={**campaign, **unrelated})
        for name, value in campaign.items():
            self.assertIn(f'--setenv={name}={value}', command)
        for name in unrelated:
            self.assertFalse(any(word.startswith(f'--setenv={name}=') for word in command))

    def test_declared_resource_paths_cross_delegation_without_exporting_ambient_state(self):
        with patch.dict(os.environ, {'TIDEPOOL_HASKELL_ACTORS_DIR': '/declared/actors',
                                     'DECLARED_FIXTURE': '/declared/fixture',
                                     'UNDECLARED_FIXTURE': '/ambient/fixture'}):
            command, _ = runner.delegated_command(['/libtest'], 10, 'app.slice', {},
                declared_resources=('TIDEPOOL_HASKELL_ACTORS_DIR', 'DECLARED_FIXTURE'))
        self.assertIn('--setenv=TIDEPOOL_HASKELL_ACTORS_DIR=/declared/actors', command)
        self.assertIn('--setenv=DECLARED_FIXTURE=/declared/fixture', command)
        self.assertFalse(any('UNDECLARED_FIXTURE' in word for word in command))

    def test_actor_layout_diagnostics_cross_delegation_explicitly(self):
        command, _ = runner.delegated_command(['/libtest'], 10, 'app.slice', {},
            environment={'TIDEPOOL_ASYNC_LAYOUT_DIAGNOSTICS': '1', 'UNDECLARED_TRACE': '1'})
        self.assertIn('--setenv=TIDEPOOL_ASYNC_LAYOUT_DIAGNOSTICS=1', command)
        self.assertFalse(any('UNDECLARED_TRACE' in word for word in command))

    def test_success_retains_physical_timing_and_outcome_before_scratch_deletion(self):
        root = Path(self.tmp.name) / 'timing-artifacts'
        def run(args, timeout, environment=None):
            compiler = root / 'compiler'
            compiler.mkdir()
            (compiler / 'owned-compiler-outcome.json').write_text(json.dumps({'cleanup': {'status': 'confirmed'}}))
            (compiler / 'lifecycle.json').write_text(json.dumps({'pid': 73, 'producer': 'matched'}))
            rows = [
                {'fields': {'execution_layer': 'transaction_wrapper', 'elapsed_ms': 10}},
                {'fields': {'execution_layer': 'physical', 'physical_execution': '73:1', 'elapsed_ms': 9}},
            ]
            (compiler / 'compiler.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in rows))
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root)
        self.assertTrue(passed)
        self.assertFalse(root.exists())
        summary = record['diagnostic_summaries']
        self.assertEqual(summary['owned_compiler_lifecycle']['pid'], 73)
        timing = summary['physical_compiler_timing']
        self.assertTrue(timing['complete'])
        self.assertEqual(timing['physical_record_count'], 1)
        self.assertEqual(timing['records'][0]['fields']['physical_execution'], '73:1')

    def test_full_trace_streaming_crosses_old_byte_cap_with_bounded_samples(self):
        root = Path(self.tmp.name) / 'large-trace'
        compiler = root / 'compiler'
        compiler.mkdir(parents=True)
        padding = json.dumps({'fields': {'padding': 'x' * (64 << 10)}}).encode() + b'\n'
        physical = json.dumps({'fields': {'execution_layer': 'physical',
                                         'physical_execution': 'after-old-cap'}}).encode() + b'\n'
        trace = compiler / 'compiler.jsonl'
        with trace.open('wb') as stream:
            for _ in range(513):
                stream.write(padding)
            stream.write(physical * 600)
        summary = runner.diagnostic_summaries(root)
        scan, timing = summary['compiler_trace_scan'], summary['physical_compiler_timing']
        self.assertTrue(scan['complete'])
        self.assertEqual(scan['malformed_lines'], 0)
        self.assertEqual(scan['examined_bytes'], trace.stat().st_size)
        self.assertEqual(scan['sha256'], hashlib.sha256(trace.read_bytes()).hexdigest())
        self.assertEqual(timing['physical_record_count'], 600)
        self.assertEqual(timing['physical_request_count'], 1)
        self.assertTrue(timing['aggregate_complete'])
        self.assertTrue(timing['records_truncated'])
        self.assertFalse(timing['complete'], 'aggregate completeness does not upgrade sampled evidence')
        self.assertLessEqual(len(timing['records']), runner.TRACE_RECORD_LIMIT)
        self.assertLessEqual(len(json.dumps(timing['records'])), 2 * runner.TRACE_RECORD_BYTE_LIMIT)

    def test_trace_limits_and_partial_tail_are_not_malformed_json(self):
        physical = json.dumps({'fields': {'execution_layer': 'physical',
                                         'physical_execution': 'request'}}).encode() + b'\n'
        controls = {
            'oversized-valid': (json.dumps({'padding': 'x' * 200}).encode() + b'\n', 0, 1, 0),
            'oversized-malformed': (b'{' + b'x' * 200 + b'\n', 0, 1, 0),
            'malformed': (b'{invalid}\n', 1, 0, 0),
            'partial-tail': (b'{"fields":', 0, 0, 1),
        }
        for name, (line, malformed, oversized, incomplete) in controls.items():
            with self.subTest(name=name):
                root = Path(self.tmp.name) / name
                compiler = root / 'compiler'
                compiler.mkdir(parents=True)
                # A later valid row proves oversized lines are drained fully.
                (compiler / 'compiler.jsonl').write_bytes(
                    physical + line + (b'' if incomplete else physical))
                with patch.object(runner, 'TRACE_LINE_LIMIT', 128):
                    summary = runner.diagnostic_summaries(root)
                scan = summary['compiler_trace_scan']
                self.assertEqual(scan['malformed_lines'], malformed)
                self.assertEqual(scan['oversized_lines'], oversized)
                self.assertEqual(scan['incomplete_lines'], incomplete)
                self.assertFalse(scan['complete'])
                self.assertEqual(summary['physical_compiler_timing']['physical_record_count'],
                                 1 if incomplete else 2)
                self.assertFalse(summary['physical_compiler_timing']['aggregate_complete'])

    def test_trace_growth_keeps_snapshot_finite_and_marks_scan_incomplete(self):
        trace = Path(self.tmp.name) / 'growing.jsonl'
        row = b'{"fields": {"message": "first"}}\n'
        trace.write_bytes(row * 2)
        scan = {}
        rows = runner.compiler_trace_rows(trace, scan)
        first = next(rows)
        with trace.open('ab') as stream:
            stream.write(row)
        self.assertEqual(len([first, *rows]), 2)
        self.assertEqual(scan['examined_bytes'], len(row) * 2)
        self.assertTrue(scan['changed_during_scan'])
        self.assertFalse(scan['complete'])
        self.assertEqual(scan['malformed_lines'], 0)
        self.assertEqual(scan['sha256'], hashlib.sha256(row * 2).hexdigest())

    def test_identity_and_queue_bounds_make_aggregation_explicitly_incomplete(self):
        root = Path(self.tmp.name) / 'bounded-identities'
        compiler = root / 'compiler'
        compiler.mkdir(parents=True)
        rows = []
        for index in range(5):
            rows.extend([
                {'fields': {'phase': 'compiler_queue', 'daemon_epoch': 'epoch',
                            'admission_id': index, 'queue_ms': index}},
                {'fields': {'execution_layer': 'physical', 'physical_execution': f'epoch:{index}:1',
                            'daemon_epoch': 'epoch', 'admission_id': index}},
            ])
        (compiler / 'compiler.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in rows))
        with patch.object(runner, 'TRACE_IDENTITY_LIMIT', 2), patch.object(runner, 'TRACE_QUEUE_LIMIT', 2):
            summary = runner.diagnostic_summaries(root)
        timing, queue = summary['physical_compiler_timing'], summary['compiler_job_queue']
        self.assertTrue(summary['compiler_trace_scan']['complete'])
        self.assertEqual(timing['malformed_lines'], 0)
        self.assertEqual(timing['physical_record_count'], 5)
        self.assertEqual(timing['physical_request_count'], 2)
        self.assertTrue(timing['request_identities_truncated'])
        self.assertFalse(timing['request_count_complete'])
        self.assertFalse(timing['aggregate_complete'])
        self.assertEqual(queue['physical_job_count'], 2)
        self.assertTrue(queue['physical_job_identities_truncated'])
        self.assertTrue(queue['records_truncated'])
        self.assertEqual(queue['unclassified_record_count'], 0)
        self.assertFalse(queue['complete'])

    def test_trace_aggregation_matches_exhaustive_small_histories(self):
        root = Path(self.tmp.name) / 'trace-histories'
        compiler = root / 'compiler'
        compiler.mkdir(parents=True)
        trace = compiler / 'compiler.jsonl'
        # Independent oracle: tokens name two physical identities or a wrapper.
        # Enumerating all length-four histories exposes repeated/retired IDs and
        # ordering effects without reproducing the consumer's context merging.
        for history in itertools.product(('one', 'two', None), repeat=4):
            with self.subTest(history=history):
                rows = [{'fields': ({'execution_layer': 'physical', 'physical_execution': token}
                                   if token else {'execution_layer': 'transaction_wrapper'})}
                        for token in history]
                trace.write_text(''.join(json.dumps(row) + '\n' for row in rows))
                with patch.object(runner, 'TRACE_RECORD_LIMIT', 1):
                    summary = runner.diagnostic_summaries(root)
                timing = summary['physical_compiler_timing']
                physical = [token for token in history if token]
                self.assertEqual(timing['physical_record_count'], len(physical))
                self.assertEqual(timing['physical_request_count'], len(set(physical)))
                self.assertTrue(summary['compiler_trace_scan']['complete'])
                self.assertEqual(timing['aggregate_complete'], bool(physical))
                self.assertEqual(timing['records_truncated'], len(physical) > 1)

    def test_actual_older_compiler_span_trace_is_explicitly_unclassified(self):
        # Four unmodified queue/start/timing/close rows from the retained 2026-10-05
        # lookup-owned-daemon/compiler.jsonl; this producer predates layer tags.
        root = Path(self.tmp.name) / 'older-trace-artifacts'
        compiler = root / 'compiler'
        compiler.mkdir(parents=True)
        fixture = Path(__file__).with_name('fixtures') / 'compiler-span-trace.jsonl'
        (compiler / 'compiler.jsonl').write_bytes(fixture.read_bytes())
        summary = runner.diagnostic_summaries(root)
        timing = summary['physical_compiler_timing']
        self.assertEqual(timing['physical_request_count'], 0)
        self.assertEqual(timing['unclassified_request_records'], 3)
        self.assertEqual(timing['data_status'], 'no_identified_physical_requests')
        self.assertFalse(timing['complete'])
        queue = summary['compiler_job_queue']
        self.assertEqual(queue['retained_job_count'], 1)
        self.assertEqual(queue['records'][0]['queue_ms'], 0)
        self.assertTrue(queue['complete'])

    def test_conflicting_duplicate_admission_grants_cannot_be_complete(self):
        root = Path(self.tmp.name) / 'conflicting-grants'
        compiler = root / 'compiler'
        compiler.mkdir(parents=True)
        fields = {'phase': 'compiler_queue', 'queue_ms': 0, 'daemon_epoch': 'epoch-a',
                  'admission_id': 7, 'compiler_workload': 'foreground',
                  'compiler_jobs': 2, 'compiler_capabilities': 2}
        rows = [{'fields': fields}, {'fields': {**fields, 'compiler_jobs': 8}}]
        (compiler / 'compiler.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in rows))
        summary = runner.diagnostic_summaries(root)
        self.assertFalse(summary['compiler_job_queue']['complete'])
        self.assertEqual(summary['compiler_job_queue']['unclassified_record_count'], 1)
        self.assertFalse(runner.compiler_job_grants(summary['compiler_job_queue'], {})['complete'])

    def test_nested_owned_control_preserves_job_queue_separately_from_request_service(self):
        root = Path(self.tmp.name) / 'nested-control-artifacts'
        fixture = Path(__file__).with_name('fixtures') / 'compiler-span-trace.jsonl'
        actual_queue = json.loads(fixture.read_text().splitlines()[0])
        epoch = actual_queue['fields']['daemon_epoch']
        def run(args, timeout, environment=None):
            control = root / 'owned-compiler-control-73'
            for name in ('compiler', 'failed-compiler'):
                compiler = control / name
                compiler.mkdir(parents=True)
                (compiler / 'owned-compiler-outcome.json').write_text(json.dumps({
                    'schema': 1, 'cleanup': {'status': 'confirmed'}}))
                (compiler / 'lifecycle.json').write_text(json.dumps({
                    'schema': 1, 'daemon_epoch': epoch, 'cleanup_confirmed': True}))
                rows = [actual_queue, actual_queue]
                for ordinal in (1, 2):
                    span = {'name': 'compile_request', 'execution_layer': 'physical',
                            'physical_execution': f'{epoch}:1:{ordinal}', 'daemon_epoch': epoch,
                            'admission_id': 1, 'request_ordinal': ordinal}
                    rows.append({'fields': {'message': 'close', 'time.busy': '31ns'},
                                 'span': span, 'spans': []})
                (compiler / 'compiler.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in rows))
            unrelated = control / 'source-tree'
            unrelated.mkdir()
            (unrelated / 'compiler.jsonl').write_text('not owned compiler evidence\n')
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root)
        self.assertTrue(passed)
        self.assertFalse(root.exists())
        roots = record['diagnostic_summaries']['owned_compiler_roots']
        self.assertEqual(len(roots), 2)
        for compiler in roots:
            self.assertEqual(compiler['role'], 'native_test_control')
            self.assertFalse(compiler['authority'])
            self.assertEqual(compiler['physical_compiler_timing']['physical_request_count'], 2)
            queue = compiler['compiler_job_queue']
            self.assertEqual(queue['observed_record_count'], 2)
            self.assertEqual(queue['retained_job_count'], 1)
            self.assertEqual(queue['physical_job_count'], 1)
            self.assertEqual(queue['records'][0]['queue_ms'], 0)
            self.assertTrue(queue['complete'])

    def test_nested_owned_control_missing_queue_retains_successful_case(self):
        root = Path(self.tmp.name) / 'missing-queue-artifacts'
        def run(args, timeout, environment=None):
            compiler = root / 'owned-compiler-control-73' / 'compiler'
            compiler.mkdir(parents=True)
            (compiler / 'owned-compiler-outcome.json').write_text(json.dumps({'cleanup': {'status': 'confirmed'}}))
            (compiler / 'lifecycle.json').write_text(json.dumps({'daemon_epoch': 'epoch'}))
            row = {'fields': {'message': 'close'}, 'span': {
                'name': 'compile_request', 'execution_layer': 'physical',
                'physical_execution': 'epoch:1:1', 'daemon_epoch': 'epoch', 'admission_id': 1}}
            (compiler / 'compiler.jsonl').write_text(json.dumps(row) + '\n')
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
        record = {}
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                10, record, artifact_root=root)
        self.assertTrue(passed)
        self.assertTrue(root.exists())
        queue = record['diagnostic_summaries']['owned_compiler_roots'][0]['compiler_job_queue']
        self.assertEqual(queue['physical_jobs_without_queue'], 1)
        self.assertEqual(queue['data_status'], 'no_observed_job_queue')
        self.assertFalse(queue['complete'])

    def test_owned_control_symlink_is_refused_and_partial_local_owner_remains_unknown(self):
        root = Path(self.tmp.name) / 'safe-root'
        root.mkdir()
        foreign = Path(self.tmp.name) / 'foreign'
        compiler = foreign / 'compiler'
        compiler.mkdir(parents=True)
        (compiler / 'owned-compiler-outcome.json').write_text('{}')
        (compiler / 'lifecycle.json').write_text('{}')
        (root / 'owned-compiler-control-link').symlink_to(foreign, target_is_directory=True)
        incomplete = root / 'owned-compiler-control-73' / 'compiler'
        incomplete.mkdir(parents=True)
        (incomplete / 'owned-compiler-outcome.json').write_text('{}')
        summary = runner.diagnostic_summaries(root)
        self.assertEqual(len(summary['owned_compiler_roots']), 1)
        self.assertEqual(summary['owned_compiler_roots'][0]['path'], str(incomplete))
        self.assertEqual(summary['owned_compiler_roots'][0]['owned_cleanup_status'], 'unknown')
        self.assertTrue(any('unsafe' in issue for issue in summary['issues']))
        self.assertTrue(any('incomplete' in issue for issue in summary['issues']))

    def test_span_and_span_list_physical_context_retains_timing_once_per_row(self):
        root = Path(self.tmp.name) / 'span-artifacts'
        compiler = root / 'compiler'
        compiler.mkdir(parents=True)
        physical = {'name': 'compile_request', 'execution_layer': 'physical',
                    'physical_execution': 'epoch:1:2', 'request_mode': 'compile'}
        rows = [
            {'fields': {'message': 'compiler timing', 'line': 'wall_ns=31'},
             'spans': [physical, {'name': 'worker_detail'}]},
            {'fields': {'message': 'close', 'time.busy': '31ns'},
             'span': physical, 'spans': []},
            {'fields': {'message': 'wrapper event'}, 'span': {
                'name': 'compile_request', 'execution_layer': 'transaction_wrapper'}},
        ]
        (compiler / 'compiler.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in rows))
        timing = runner.diagnostic_summaries(root)['physical_compiler_timing']
        self.assertTrue(timing['complete'])
        self.assertEqual(timing['physical_record_count'], 2)
        self.assertEqual(timing['physical_request_count'], 1)
        self.assertEqual(timing['records'][0]['physical_context']['request_mode'], 'compile')
        self.assertEqual(timing['records'][1]['fields']['time.busy'], '31ns')

    def test_zero_or_missing_compiler_timing_retains_successful_case_scratch(self):
        for trace in (None, [], [{'fields': {'message': 'daemon ready'}}]):
            with self.subTest(trace=trace):
                root = Path(self.tmp.name) / ('absent' if trace is None else 'trace-' + str(len(trace)))
                def run(args, timeout, environment=None):
                    compiler = root / 'compiler'
                    compiler.mkdir()
                    (compiler / 'owned-compiler-outcome.json').write_text(json.dumps({'cleanup': {'status': 'confirmed'}}))
                    if trace is not None:
                        (compiler / 'compiler.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in trace))
                    return completed_process(args, 0,
                        'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')
                record = {}
                with patch.object(runner, 'execute', side_effect=run):
                    passed, _, _ = runner.run_one(str(self.binary), 'suite::works', False,
                        10, record, artifact_root=root)
                self.assertTrue(passed)
                self.assertTrue(root.exists())
                timing = record['diagnostic_summaries']['physical_compiler_timing']
                self.assertFalse(timing['complete'])
                self.assertEqual(timing['data_status'], 'trace_absent' if trace is None else 'no_identified_physical_requests')

    def test_regeneration_modes_refuse_before_discovery_or_execution(self):
        for name in ('TIDEPOOL_REGEN_BRIDGED', 'TIDEPOOL_REGEN_PROTOCOL_GOLDENS'):
            with self.subTest(name=name), patch.dict(os.environ, {name: '1'}), \
                 patch.object(runner, 'execute') as execute, \
                 contextlib.redirect_stderr(io.StringIO()) as errors:
                self.assertEqual(runner.main([str(self.binary)]), 2)
                execute.assert_not_called()
                self.assertIn(name, errors.getvalue())

    def test_native_compile_regression_path_uses_declared_package_and_target(self):
        calls = []
        definitions = SCRIPT.with_name('defs.bzl').read_text()
        namespace = {
            'rust_optimization_level': lambda *_: '1',
            'rust_test': lambda **kwargs: calls.append(('test', kwargs)),
            'rust_binary': lambda **kwargs: calls.append(('binary', kwargs)),
            'sh_test': lambda **kwargs: calls.append(('runner', kwargs)),
            'test_source_requirements': lambda **kwargs: None,
        }
        exec('\n'.join(line for line in definitions.splitlines()
                       if not line.startswith('load(')), namespace)
        declared_source = 'owned/component/src/lib.rs'
        arguments = {
            'name': 'property_case', 'package_name': 'component',
            'package_dir': 'owned/component', 'version': '0.1.0',
            'crate_root': declared_source,
        }
        namespace['tidepool_rust_test'](**arguments)
        namespace['tidepool_rust_isolated_test'](**arguments)
        self.assertEqual([kind for kind, _ in calls], ['test', 'binary', 'runner'])
        for _, rule in calls[:2]:
            self.assertEqual(rule['env']['TIDEPOOL_PROPTEST_REGRESSIONS'],
                             'owned/component/proptest-regressions/property_case.txt')
            self.assertEqual(rule['crate_root'], declared_source)
        self.assertNotIn('TIDEPOOL_PROPTEST_REGRESSIONS', calls[-1][1]['env'])

    def test_shared_binary_regression_path_only_applies_to_libtest(self):
        calls = []
        definitions = SCRIPT.with_name('defs.bzl').read_text()
        namespace = {
            'rust_optimization_level': lambda *_: '1',
            'rust_binary': lambda **kwargs: calls.append(kwargs),
            'test_source_requirements': lambda **kwargs: None,
        }
        exec('\n'.join(line for line in definitions.splitlines()
                       if not line.startswith('load(')), namespace)
        common = {
            'package_name': 'component', 'package_dir': 'owned/component',
            'version': '0.1.0', 'crate_root': 'owned/component/src/lib.rs',
        }

        namespace['tidepool_rust_binary'](
            name='shared_case', rustc_flags=['--test'], **common)
        namespace['tidepool_rust_binary'](
            name='ordinary_tool', rustc_flags=[], **common)

        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0]['env']['TIDEPOOL_PROPTEST_REGRESSIONS'],
                         'owned/component/proptest-regressions/shared_case.txt')
        self.assertIn('--test', calls[0]['rustc_flags'])
        self.assertNotIn('TIDEPOOL_PROPTEST_REGRESSIONS', calls[1]['env'])

    def test_declared_resources_are_absolute_for_discovery_and_execution(self):
        assets = Path(self.tmp.name) / 'web assets'
        assets.mkdir()
        (assets / 'index.html').write_text('declared web resource')
        link = Path(self.tmp.name) / 'Buck web link'
        link.symlink_to(assets, target_is_directory=True)
        relative = os.path.relpath(link)
        calls = []
        definitions = SCRIPT.with_name('defs.bzl').read_text()
        namespace = {'sh_test': lambda **kwargs: calls.append(kwargs)}
        exec('\n'.join(line for line in definitions.splitlines()
                       if not line.startswith('load(')), namespace)
        namespace['tidepool_rust_test_cases'](
            name='resource_control', binary=':libtest',
            exact_tests=['suite::works'], expected_count=1,
            env={'ORDINARY_FLAG': 'relative command or flag'},
            resource_env={'EXOMONAD_EMBEDDED_ASSET_ROOT': relative},
        )
        rule = calls.pop()
        self.assertIn('toolchains//:exomonad_runtime_tools', rule['resources'])
        for key in ('TIDEPOOL_TEST_SYSTEMD_RUN', 'TIDEPOOL_TEST_SYSTEMCTL'):
            self.assertIn(key, rule['args'])
            # This rule-shape control models Buck's location expansion with an
            # existing executable before exercising the resource binding owner.
            rule['env'][key] = sys.executable
        observed = []

        def run(args, timeout, environment=None):
            value = os.environ['EXOMONAD_EMBEDDED_ASSET_ROOT']
            self.assertTrue(Path(value).is_absolute())
            self.assertTrue(Path(value).is_symlink(), 'Buck artifact link must remain selected')
            self.assertTrue(Path(value).samefile(assets))
            self.assertTrue(Path(value).joinpath('index.html').is_file())
            self.assertEqual(os.environ['ORDINARY_FLAG'], 'relative command or flag')
            observed.append(args)
            discovered = self.discover(args)
            if discovered is not None:
                return discovered
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')

        with patch.dict(os.environ, rule['env']):
            result, _, _ = self.invoke(rule['args'][1:], run)
        self.assertEqual(result, 0)
        self.assertEqual(len([args for args in observed if '--list' in args]), 2)
        self.assertEqual(len([args for args in observed if '--exact' in args]), 1)

    def test_missing_declared_resource_refuses_before_discovery(self):
        for value in ('', str(Path(self.tmp.name) / 'missing web')):
            with self.subTest(value=value), \
                 patch.dict(os.environ, {'EXOMONAD_EMBEDDED_ASSET_ROOT': value}), \
                 patch.object(runner, 'execute') as execute, \
                 contextlib.redirect_stderr(io.StringIO()) as errors:
                self.assertEqual(runner.main([str(self.binary), '--resource-env',
                    'EXOMONAD_EMBEDDED_ASSET_ROOT']), 1)
                execute.assert_not_called()
                self.assertIn('declared resource', errors.getvalue())

    def test_compiler_selection_is_equal_for_direct_and_delegated_libtest(self):
        catalog = Path(self.tmp.name) / 'declared-catalog.json'
        catalog.write_text('{}')
        root_entry = Path(self.tmp.name) / 'declared-root-entry'
        root_entry.mkdir()
        daemon = Path(self.tmp.name) / 'declared-daemon.sock'
        daemon.touch()
        declared = {
            'TIDEPOOL_COMPILER_MODULES': str(catalog),
            'TIDEPOOL_PREPARED_ROOT_ENTRY': str(root_entry),
            'TIDEPOOL_EXTRACT_DAEMON_SOCKET': str(daemon),
        }
        poisoned = {
            'TIDEPOOL_COMPILER_MODULES': '/ambient/catalog',
            'TIDEPOOL_PREPARED_ROOT_ENTRY': '/ambient/root-entry',
            'TIDEPOOL_EXTRACT_DAEMON_SOCKET': '/ambient/daemon.sock',
            'TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT': 'ambient-endpoint',
            'TIDEPOOL_EXTRACT_NO_DAEMON': '1',
        }
        self.binary.write_text(
            '#!' + sys.executable + '\n'
            'import json, os, pathlib, sys\n'
            'if "--list" in sys.argv:\n'
            '    print("suite::ignored: test" if "--ignored" in sys.argv else "suite::works: test\\nsuite::ignored: test")\n'
            'elif "--exact" in sys.argv:\n'
            '    keys = ' + repr(sorted(poisoned)) + '\n'
            '    output = pathlib.Path(os.environ["TIDEPOOL_TEST_ARTIFACT_ROOT"])\n'
            '    (output / "compiler-selection.json").write_text(json.dumps({key: os.environ.get(key) for key in keys}))\n'
            '    print("test result: ok. 1 passed; 0 failed; 0 ignored;")\n'
        )
        self.binary.chmod(0o700)
        outputs = {}
        original_execute = runner.execute

        def execute(args, timeout, service_slice=None, service_record=None,
                    environment=None, declared_resources=()):
            if service_slice is None:
                return original_execute(args, timeout, environment=environment)
            command, _ = runner.delegated_command(
                args, timeout, service_slice, service_record,
                environment=environment, declared_resources=declared_resources)
            child_environment = {}
            for value in command:
                if value.startswith('--setenv='):
                    key, selected = value[len('--setenv='):].split('=', 1)
                    child_environment[key] = selected
            result = subprocess.run(args, capture_output=True, text=True,
                                    errors='replace', env=child_environment, check=False)
            result.cleanup_confirmed = True
            service_record.update(cleanup_confirmed=True, manager_wait_success=True,
                                  admission_observer_stopped=True)
            return result

        for bind_resources in (False, True):
            for delegated in (False, True):
                with self.subTest(bind_resources=bind_resources, delegated=delegated), \
                     tempfile.TemporaryDirectory() as directory:
                    output = Path(directory) / 'runner-output'
                    arguments = [str(self.binary), '--exact', 'suite::works',
                                 '--expected-count', '1', '--output-dir', str(output),
                                 '--retain-artifacts']
                    selected_resources = declared if bind_resources else {}
                    for name in selected_resources:
                        arguments.extend(['--resource-env', name])
                    if delegated:
                        arguments.append('--delegated-service')
                    stdout, stderr = io.StringIO(), io.StringIO()
                    with patch.dict(os.environ, clear=True):
                        os.environ.update({'PATH': os.environ.get('PATH', ''),
                                           **poisoned, **selected_resources})
                        with patch.object(runner, 'execute', side_effect=execute), \
                             contextlib.redirect_stdout(stdout), \
                             contextlib.redirect_stderr(stderr):
                            result = runner.main(arguments)
                    self.assertEqual(result, 0, stderr.getvalue() + stdout.getvalue())
                    key = hashlib.sha256(b'suite::works').hexdigest()
                    captured = output / key / 'artifacts/compiler-selection.json'
                    outputs[(bind_resources, delegated)] = json.loads(captured.read_text())

        self.assertEqual(outputs[(False, False)], outputs[(False, True)])
        self.assertEqual(outputs[(False, False)], {name: None for name in poisoned})
        self.assertEqual(outputs[(True, False)], outputs[(True, True)])
        self.assertEqual(outputs[(True, False)], declared | {
            'TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT': None,
            'TIDEPOOL_EXTRACT_NO_DAEMON': None,
        })

    def test_delegated_command_forwards_bounded_startup_and_existing_compiler_diagnostics(self):
        environment = {
            'TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS': '600',
            'TIDEPOOL_TIMING': '1',
            'TIDEPOOL_MEMO_TRACE': '1',
            'UNRELATED_VARIABLE': 'not-forwarded',
        }
        record = {}
        command, _ = runner.delegated_command(['/libtest'], 900, 'app.slice', record,
                                              environment=environment)
        for name, value in environment.items():
            if name != 'UNRELATED_VARIABLE':
                self.assertIn('--setenv=' + name + '=' + value, command)
                self.assertIn(name, record['environment_names'])
        self.assertNotIn('UNRELATED_VARIABLE', record['environment_names'])
        self.assertIn('--property=RuntimeMaxSec=900s', command)

    def test_delegated_command_exports_declared_inputs_and_only_test_process(self):
        record = {}
        with patch.dict(os.environ, {'TIDEPOOL_EXTRACT': '/qualified/compiler',
                                     'TIDEPOOL_KEEP_TEST_LOGS': '1',
                                     'TIDEPOOL_TEST_ARTIFACT_ROOT': '/retained/compiler failures',
                                     'EXOMONAD_WORKSPACE_GITLINK': '/qualified/workspace-pin',
                                     'EXOMONAD_WORKSPACE_GIT_BUNDLE': '/qualified/workspace.bundle',
                                     'EXOMONAD_NIX_BIN': '/declared/runtime-tools/bin/nix',
                                     'EXOMONAD_NIX_OFFLINE': '1',
                                     'OPENAI_API_KEY': 'never-forward',
                                     'UNRELATED_VARIABLE': 'never-forward'}):
            command, unit = runner.delegated_command(
                ['/declared/libtest', '--exact', 'suite::works'], 12, 'app.slice', record)
        split = command.index('--')
        self.assertEqual(command[split + 1:], ['/declared/libtest', '--exact', 'suite::works'])
        self.assertIn('--property=Delegate=yes', command)
        self.assertIn('--property=KillMode=control-group', command)
        self.assertIn('--setenv=TIDEPOOL_EXTRACT=/qualified/compiler', command)
        self.assertIn('--setenv=TIDEPOOL_KEEP_TEST_LOGS=1', command)
        self.assertIn('--setenv=TIDEPOOL_TEST_ARTIFACT_ROOT=/retained/compiler failures', command)
        self.assertIn('TIDEPOOL_TEST_ARTIFACT_ROOT', record['environment_names'])
        self.assertIn('--setenv=EXOMONAD_WORKSPACE_GITLINK=/qualified/workspace-pin', command)
        self.assertIn('--setenv=EXOMONAD_WORKSPACE_GIT_BUNDLE=/qualified/workspace.bundle', command)
        self.assertIn('--setenv=EXOMONAD_NIX_BIN=/declared/runtime-tools/bin/nix', command)
        self.assertIn('--setenv=EXOMONAD_NIX_OFFLINE=1', command)
        self.assertFalse(any('never-forward' in word for word in command))
        self.assertIn('--unit=' + unit, command)
        self.assertFalse(record['cleanup_confirmed'])

    def test_delegated_cleanup_failure_refuses_pass_without_erasing_actual_count(self):
        def result(args, timeout, service_slice, service_record, environment=None):
            self.assertEqual(service_slice, 'app.slice')
            service_record.update(cleanup_confirmed=False, cleanup_error='service still active')
            return completed_process(args, 0,
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
        self.assertEqual(record['process_cleanup_scope'], 'delegated_service')
        self.assertEqual(record['process_cleanup_status'], 'unconfirmed')

    def test_declared_manager_tools_launch_observe_and_cleanup_with_poisoned_path(self):
        tools = Path(self.tmp.name) / 'declared-tools'
        tools.mkdir()
        calls = tools / 'calls.jsonl'
        systemd_run, systemctl = tools / 'systemd-run', tools / 'systemctl'
        systemd_run.write_text(f'#!{sys.executable}\n' +
            'import json, os, sys\n' +
            f'with open({str(calls)!r}, "a") as stream: stream.write(json.dumps(sys.argv) + "\\n")\n' +
            'child = sys.argv[sys.argv.index("--") + 1:]\n' +
            'os.execv(child[0], child)\n')
        systemctl.write_text(f'#!{sys.executable}\n' +
            'import json, sys\n' +
            f'with open({str(calls)!r}, "a") as stream: stream.write(json.dumps(sys.argv) + "\\n")\n' +
            'print("Id=" + sys.argv[-1])\n' +
            'print("LoadState=loaded\\nActiveState=inactive\\nTransient=yes")\n' +
            'print("InvocationID=" + "ab" * 16)\n')
        self.binary.write_text(f'#!{sys.executable}\n' +
            'import time\ntime.sleep(0.1)\n' +
            'print("test result: ok. 1 passed; 0 failed; 0 ignored;")\n')
        for executable in (systemd_run, systemctl, self.binary):
            executable.chmod(0o700)
        environment = {'PATH': '/nonexistent-poisoned-path',
                       'TIDEPOOL_TEST_SYSTEMD_RUN': str(systemd_run),
                       'TIDEPOOL_TEST_SYSTEMCTL': str(systemctl)}
        record = {}
        result = runner.execute([str(self.binary)], 3, 'app.slice', record,
                                environment=environment)
        self.assertEqual(result.returncode, 0)
        self.assertTrue(result.cleanup_confirmed)
        self.assertTrue(record['manager_admission'])
        self.assertEqual(record['systemd_tools']['systemctl'], str(systemctl.resolve()))
        commands = [json.loads(line) for line in calls.read_text().splitlines()]
        self.assertEqual(sum(command[0] == str(systemd_run.resolve()) for command in commands), 1)
        self.assertTrue(all(command[0] in (str(systemd_run.resolve()), str(systemctl.resolve()))
                            for command in commands))
        self.assertTrue(any('stop' in command for command in commands))
        for bad in ({'TIDEPOOL_TEST_SYSTEMD_RUN': str(systemd_run)},
                    {**environment, 'TIDEPOOL_TEST_SYSTEMCTL': 'relative-systemctl'},
                    {**environment, 'TIDEPOOL_TEST_SYSTEMCTL': str(tools / 'missing')}):
            with self.subTest(environment=bad), patch.object(runner.subprocess, 'Popen') as spawn:
                with self.assertRaises(OSError):
                    runner.execute([str(self.binary)], 3, 'app.slice', {}, environment=bad)
                spawn.assert_not_called()

    def test_delegated_launch_failure_keeps_test_execution_unknown(self):
        def refused(args, timeout, service_slice, service_record, environment=None):
            service_record.update(cleanup_confirmed=True)
            return completed_process(args, 1, '', 'service admission refused')
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
             patch.object(runner, 'observe_delegated_admission'), \
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
            return completed_process(args, 0, output, '')
        record = {'manager_admission': {'Id': unit, 'Transient': 'yes',
                                        'InvocationID': 'ab' * 16}}
        with patch.object(runner.subprocess, 'run', side_effect=control):
            runner.stop_delegated_service(unit, record)
        self.assertTrue(record['cleanup_confirmed'])
        self.assertEqual([args[2] for args in calls], ['stop', 'kill', 'stop', 'show'])
        self.assertTrue(all(args[-1] == unit for args in calls))
        with patch.object(runner.subprocess, 'run', return_value=completed_process(
                [], 0, 'LoadState=loaded\nActiveState=active\n', '')):
            runner.stop_delegated_service(unit, record)
        self.assertFalse(record['cleanup_confirmed'])
        record = {'manager_admission': {'Id': unit, 'Transient': 'yes',
                                        'InvocationID': 'ab' * 16}}
        def collected_after_stop(args, **_kwargs):
            output = 'LoadState=not-found\nActiveState=inactive\n' if 'show' in args else ''
            return completed_process(args, 0, output, '')
        with patch.object(runner.subprocess, 'run', side_effect=collected_after_stop):
            runner.stop_delegated_service(unit, record)
        self.assertTrue(record['cleanup_confirmed'])
        self.assertIsNone(record['reset_failed_exit_code'])

    def test_delegated_options_do_not_apply_to_discovery(self):
        calls = []
        def run(args, timeout, service_slice=None, service_record=None, environment=None):
            calls.append((args, service_slice))
            discovered = self.discover(args)
            if discovered is not None:
                return discovered
            service_record.update(cleanup_confirmed=True)
            return completed_process(args, 0,
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
            return completed_process(args, 5 if 'stop' in args else 1,
                                                'LoadState=not-found\nActiveState=inactive\n', '')
        with patch.object(runner.subprocess, 'run', side_effect=absent):
            runner.stop_delegated_service(unit, record)
        self.assertFalse(record['cleanup_confirmed'])
        self.assertIn('queued start', record['cleanup_error'])
        late = f'Id={unit}\nLoadState=loaded\nTransient=yes\nInvocationID={"ab" * 16}\n'
        with patch.object(runner.subprocess, 'run', return_value=completed_process(
                [], 0, late, '')):
            runner.observe_delegated_admission(unit, record, runner.threading.Event())
        self.assertEqual(record['manager_admission']['Id'], unit)
        self.assertFalse(record['cleanup_confirmed'], 'late registration requires a new exact stop')
        with patch.object(runner.subprocess, 'run', return_value=completed_process(
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

    def test_successful_wait_fences_fast_collected_unit_without_invocation_poll(self):
        for exit_code, query_exit, expected in ((0, 0, True), (1, 0, False), (0, 1, False)):
            with self.subTest(exit_code=exit_code, query_exit=query_exit):
                class Process:
                    pid = 781237
                    stdout = None
                    stderr = None
                    returncode = exit_code
                    def communicate(self, timeout=None):
                        return 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', ''
                def control(args, **kwargs):
                    return completed_process(args, 5 if 'stop' in args else query_exit,
                        'LoadState=not-found\nActiveState=inactive\n', '')
                record = {'manager_wait_success': True, 'launcher_wait_exit_code': 0,
                          'manager_admission': {'Id': 'previous-unit.service'}}
                with patch.object(runner.subprocess, 'Popen', return_value=Process()), \
                     patch.object(runner, 'observe_delegated_admission'), \
                     patch.object(runner.subprocess, 'run', side_effect=control):
                    result = runner.execute(['fake-libtest'], 2, 'app.slice', record)
                self.assertNotIn('manager_admission', record)
                self.assertEqual(record['manager_wait_success'], exit_code == 0)
                self.assertEqual(record['launcher_wait_exit_code'], exit_code)
                self.assertEqual(result.cleanup_confirmed, expected)
        # Timeout/reap may race with a successful launcher exit, but incomplete
        # wait communication is not evidence that the manager's job settled.
        record = {}
        process = Process()
        process.returncode = 0
        with patch.object(runner.subprocess, 'Popen', return_value=process), \
             patch.object(process, 'communicate', side_effect=[
                 subprocess.TimeoutExpired(['fake-libtest'], 2), ('', '')]), \
             patch.object(runner, 'observe_delegated_admission'), \
             patch.object(runner.subprocess, 'run', side_effect=control), \
             self.assertRaises(subprocess.TimeoutExpired):
            runner.execute(['fake-libtest'], 2, 'app.slice', record)
        self.assertNotIn('manager_wait_success', record)
        self.assertFalse(record['cleanup_confirmed'])

    def test_fast_failed_service_stays_observable_until_exact_cleanup(self):
        admitted = runner.threading.Event()
        state_queries = 0
        calls = []

        class Process:
            pid = 781238
            stdout = None
            stderr = None
            returncode = 101

            def communicate(self, timeout=None):
                if not admitted.wait(2):
                    raise AssertionError('failed unit was collected before admission observation')
                return 'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', ''

        def control(args, **_kwargs):
            nonlocal state_queries
            calls.append(args)
            if '--property=InvocationID' in args:
                unit = args[-1]
                admitted.set()
                return completed_process(args, 0,
                    f'Id={unit}\nLoadState=loaded\nTransient=yes\nInvocationID={"ab" * 16}\n', '')
            if 'stop' in args:
                return completed_process(args, 0, '', '')
            if 'reset-failed' in args:
                return completed_process(args, 0, '', '')
            state_queries += 1
            state = ('LoadState=loaded\nActiveState=failed\n' if state_queries == 1
                     else 'LoadState=not-found\nActiveState=inactive\n')
            return completed_process(args, 0, state, '')

        record = {}
        with patch.object(runner.subprocess, 'Popen', return_value=Process()) as spawn, \
             patch.object(runner.subprocess, 'run', side_effect=control):
            result = runner.execute(['fake-libtest'], 2, 'app.slice', record)

        command = spawn.call_args.args[0]
        self.assertIn('--wait', command)
        self.assertNotIn('--collect', command)
        self.assertEqual(result.returncode, 101)
        self.assertTrue(result.cleanup_confirmed)
        self.assertFalse(record['manager_wait_success'])
        self.assertEqual(record['launcher_wait_exit_code'], 101)
        self.assertEqual(record['manager_admission']['InvocationID'], 'ab' * 16)
        self.assertEqual(record['reset_failed_exit_code'], 0)
        self.assertEqual(record['state'], {'LoadState': 'not-found', 'ActiveState': 'inactive'})
        unit = record['unit']
        self.assertTrue(all(args[-1] == unit for args in calls))

    def test_final_admission_observation_catches_failure_after_initial_not_found(self):
        first_query = runner.threading.Event()
        observer_finished = []
        calls = []
        admission_queries = 0
        state_queries = 0
        final_observations = []

        class Process:
            pid = 781239
            stdout = None
            stderr = None
            returncode = 101

            def communicate(self, timeout=None):
                if not first_query.wait(2):
                    raise AssertionError('observer did not perform its initial query')
                return 'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', ''

        def control(args, **_kwargs):
            nonlocal admission_queries
            nonlocal state_queries
            calls.append(args)
            if '--property=InvocationID' in args:
                admission_queries += 1
                if admission_queries == 1:
                    first_query.set()
                    if not observer_finished[0].wait(2):
                        raise AssertionError('polling query was not stopped before its reply')
                    return completed_process(args, 1,
                        'LoadState=not-found\nActiveState=inactive\n', '')
                unit = args[-1]
                return completed_process(args, 0,
                    f'Id={unit}\nLoadState=loaded\nTransient=yes\nInvocationID={"ab" * 16}\n', '')
            if 'stop' in args or 'reset-failed' in args:
                return completed_process(args, 0, '', '')
            state_queries += 1
            if state_queries == 1:
                return completed_process(args, 0,
                    'LoadState=loaded\nActiveState=failed\n', '')
            return completed_process(args, 0,
                'LoadState=not-found\nActiveState=inactive\n', '')

        observe = runner.observe_delegated_admission
        def observe_with_completion(unit, record, finished, final=False):
            if not final:
                observer_finished.append(finished)
            final_observations.append(final)
            return observe(unit, record, finished, final)

        record = {}
        with patch.object(runner.subprocess, 'Popen', return_value=Process()), \
             patch.object(runner, 'observe_delegated_admission', side_effect=observe_with_completion), \
             patch.object(runner.subprocess, 'run', side_effect=control):
            result = runner.execute(['fake-libtest'], 2, 'app.slice', record)

        unit = record['unit']
        final_query = next(index for index, args in enumerate(calls)
                           if '--property=InvocationID' in args and index > 0)
        stop = next(index for index, args in enumerate(calls) if 'stop' in args)
        self.assertEqual(admission_queries, 2)
        self.assertEqual(final_observations, [False, True])
        self.assertLess(final_query, stop)
        self.assertEqual(record['manager_admission']['Id'], unit)
        self.assertTrue(result.cleanup_confirmed)
        self.assertEqual(record['reset_failed_exit_code'], 0)

    def test_interrupted_launched_future_retains_unconfirmed_service_receipt(self):
        retained = Path(self.tmp.name) / 'interrupted'
        starts = []
        def run(args, timeout, service_slice=None, service_record=None, environment=None):
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
        def run(args, timeout, service_slice=None, service_record=None, environment=None):
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
        result = completed_process([], 101,
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
        result = completed_process([], 0,
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
            return completed_process(argv, 0, self.ignored_tests, '')
        return completed_process(argv, 0, self.all_tests, '')

    def test_passing_output_is_retained_with_explicit_truncation(self):
        retained = Path(self.tmp.name) / 'retained'
        payload = 'witness' * 8 + '\n'
        summary = 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'

        def run(argv, timeout, environment=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return completed_process(argv, 0, payload + summary, 'phase evidence')

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

        def run(argv, timeout, environment=None):
            calls.append((argv, timeout))
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return completed_process(
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

        def run(argv, timeout, environment=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return completed_process(
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

        def run(argv, timeout, environment=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            launched.append(argv[2])
            together.wait(timeout=2)
            return completed_process(
                argv, 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', '')

        result, _, errors = self.invoke(
            ['--exact', 'suite::works', '--exact', 'suite::also_works',
             '--expected-count', '2', '--jobs', '2'], run)
        self.assertEqual(result, 0, errors)
        self.assertCountEqual(launched, ['suite::works', 'suite::also_works'])

    def test_case_timeouts_apply_to_process_and_delegated_watchdog_and_receipt(self):
        retained = Path(self.tmp.name) / 'case-timeouts'
        limits = {}

        def run(argv, timeout, service_slice=None, service_record=None, environment=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            command, _ = runner.delegated_command(argv, timeout, service_slice, service_record)
            self.assertIn(f'--property=RuntimeMaxSec={timeout:g}s', command)
            limits[argv[2]] = timeout
            service_record.update(cleanup_confirmed=True)
            return completed_process(
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

        def run(argv, timeout, environment=None):
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

        def run(argv, timeout, environment=None):
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

        def run(argv, timeout, environment=None):
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

        def run(argv, timeout, environment=None):
            calls.append(argv)
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return completed_process(
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

        def run(argv, timeout, environment=None):
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
        def run(argv, timeout, environment=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return completed_process(
                argv, 0, 'test result: ok. 0 passed; 0 failed; 0 ignored;\n', ''
            )

        result, output, _ = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1'], run
        )
        self.assertEqual(result, 1)
        self.assertIn('FAIL suite::works', output)
        self.assertIn('0 passed; 1 failed', output)

        reached = []
        def child_then_empty_parent(argv, timeout, environment=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            reached.append(argv[2])
            return completed_process(
                argv,
                0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n'
                'test result: ok. 0 passed; 0 failed; 0 ignored; 1 measured; 1 filtered out;\n',
                '',
            )

        retained = Path(self.tmp.name) / 'parent-summary'
        result, output, _ = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1',
             '--output-dir', str(retained)], child_then_empty_parent
        )
        self.assertEqual(result, 1)
        self.assertIn('FAIL suite::works', output)
        self.assertEqual(reached, ['suite::works'])
        records = [json.loads(path.read_text()) for path in retained.glob('*.json')]
        self.assertEqual(len(records), 1)
        execution = records[0]['execution']
        self.assertFalse(records[0]['passed'])
        self.assertEqual(execution['status'], 'finished')
        self.assertEqual(execution['exit_code'], 0)
        self.assertEqual(execution['executed_test_count'], 0)
        self.assertEqual(execution['passed_test_count'], 0)
        self.assertEqual(execution['failed_test_count'], 0)

    def test_contradictory_child_and_terminal_summaries_refuse_with_terminal_counts(self):
        record = {}
        reached = []
        def run(args, timeout, environment=None):
            reached.append(args[2])
            return completed_process(args, 0,
                'test result: ok. 1 passed; 0 failed; 0 ignored;\n'
                'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', '')
        with patch.object(runner, 'execute', side_effect=run):
            passed, _, _ = runner.run_one(str(self.binary), 'suite::contradiction', False,
                5, record, artifact_root=Path(self.tmp.name) / 'contradictory-summary')
        self.assertFalse(passed)
        self.assertEqual(reached, ['suite::contradiction'])
        self.assertEqual(record['status'], 'finished')
        self.assertEqual(record['exit_code'], 0)
        self.assertEqual(record['executed_test_count'], 1)
        self.assertEqual(record['passed_test_count'], 0)
        self.assertEqual(record['failed_test_count'], 1)

    def test_generated_terminal_summaries_agree_with_independent_sequence_oracle(self):
        # Each producer model explicitly says whether it owns the terminal
        # result. The oracle folds these models, without parsing output text.
        rows = [
            ('ok', True, (True, 1, 0, 0), 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'),
            ('failed', True, (False, 0, 1, 0), 'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n'),
            ('zero', True, (True, 0, 0, 0), 'test result: ok. 0 passed; 0 failed; 0 ignored;\n'),
            ('multiple', True, (True, 2, 0, 0), 'test result: ok. 2 passed; 0 failed; 0 ignored;\n'),
            ('ignored', True, (True, 0, 0, 1), 'test result: ok. 0 passed; 0 failed; 1 ignored;\n'),
            ('ok-with-failure', True, (True, 1, 1, 0), 'test result: ok. 1 passed; 1 failed; 0 ignored;\n'),
            ('failed-with-success', True, (False, 1, 0, 0), 'test result: FAILED. 1 passed; 0 failed; 0 ignored;\n'),
            ('prefixed-child', False, None, 'child output: test result: ok. 1 passed; 0 failed; 0 ignored;\n'),
            ('prefixed-child-failure', False, None, 'child output: test result: FAILED. 0 passed; 1 failed; 0 ignored;\n'),
            ('diagnostic', False, None, 'diagnostic after result\n'),
            ('invalid-tag', True, None, 'test result: UNKNOWN. 1 passed; 0 failed; 0 ignored;\n'),
            ('invalid-count', True, None, 'test result: ok. missing passed; 0 failed; 0 ignored;\n'),
        ]
        histories = itertools.chain.from_iterable(itertools.product(rows, repeat=n) for n in range(3))
        for index, history in enumerate(histories):
            expected = None
            for _, owns_terminal, result, _ in history:
                if owns_terminal:
                    expected = result
            stdout = ''.join(row[3] for row in history)
            self.assertEqual(runner.terminal_summary(stdout), expected)
            self.assertEqual(runner.terminal_summary(stdout.encode()), expected)
            for exit_code, cleanup in itertools.product((0, 101), (False, True)):
                with self.subTest(history=[row[0] for row in history], exit_code=exit_code, cleanup=cleanup):
                    record = {}
                    reached = []
                    def run(args, timeout, environment=None):
                        reached.append(args[2])
                        result = completed_process(args, exit_code, stdout, '')
                        result.cleanup_confirmed = cleanup
                        return result
                    root = Path(self.tmp.name) / f'summary-{index}-{exit_code}-{cleanup}'
                    with patch.object(runner, 'execute', side_effect=run):
                        passed, _, _ = runner.run_one(str(self.binary), 'suite::summary', False,
                            5, record, artifact_root=root)
                    self.assertEqual(reached, ['suite::summary'])
                    self.assertEqual(record['status'], 'finished')
                    self.assertEqual(record['exit_code'], exit_code)
                    self.assertEqual(passed, exit_code == 0 and cleanup and expected == (True, 1, 0, 0))
                    self.assertEqual(record['executed_test_count'],
                                     None if expected is None else expected[1] + expected[2])
                    self.assertEqual(record['passed_test_count'], None if expected is None else expected[1])
                    self.assertEqual(record['failed_test_count'], None if expected is None else expected[2])
                    self.assertEqual(record.get('ignored_test_count'), None if expected is None else expected[3])

    def test_test_failure_and_timeout_are_counted(self):
        def fail(argv, timeout, environment=None):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return completed_process(
                argv, 101, 'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', 'panic\n'
            )

        result, output, errors = self.invoke(
            ['--exact', 'suite::works', '--expected-count', '1'], fail
        )
        self.assertEqual(result, 1)
        self.assertIn('FAIL suite::works', output)
        self.assertIn('panic', errors)

        def timeout(argv, timeout, environment=None):
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
                return completed_process(argv, 0, 'suite::absent: test\n', '')
            return completed_process(argv, 0, '2 tests, 0 benchmarks\n', '')

        result, _, errors = self.invoke([], malformed)
        self.assertEqual(result, 1)
        self.assertIn('unexpected libtest discovery output', errors)

        def unmatched_ignored(argv, timeout):
            if '--ignored' in argv:
                return completed_process(argv, 0, 'suite::absent: test\n', '')
            return completed_process(argv, 0, self.all_tests, '')

        result, _, errors = self.invoke([], unmatched_ignored)
        self.assertEqual(result, 1)
        self.assertIn('ignored tests missing from discovery', errors)


if __name__ == '__main__':
    unittest.main()
