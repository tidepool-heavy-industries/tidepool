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

    def test_failure_and_timeout_preserve_preexecution_case_evidence(self):
        for timed_out in (False, True):
            with self.subTest(timed_out=timed_out):
                root = Path(self.tmp.name) / ('timeout-artifacts' if timed_out else 'failure-artifacts')
                self.binary.write_text('#!' + sys.executable + '\n' +
                    'import os, pathlib, time\n' +
                    'root = pathlib.Path(os.environ["TIDEPOOL_TEST_ARTIFACT_ROOT"])\n' +
                    'assert os.environ["TIDEPOOL_TEST_DIAGNOSTIC_SCOPE"] == "1"\n' +
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
                self.assertEqual(record['status'], 'timeout' if timed_out else 'finished')
                self.assertEqual(record['executed_test_count'], None if timed_out else 1)

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

    def test_launch_hashes_capture_actual_binary_and_compiler_files_before_execution(self):
        frontend, worker = [Path(self.tmp.name) / name for name in ('frontend', 'worker')]
        frontend.write_bytes(b'frontend at launch')
        worker.write_bytes(b'worker at launch')
        record = {}
        def run(args, timeout):
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

    def test_owned_control_symlink_and_incomplete_markers_do_not_discover_foreign_roots(self):
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
        self.assertEqual(summary['owned_compiler_roots'], [])
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
        def result(args, timeout, service_slice, service_record):
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
        def refused(args, timeout, service_slice, service_record):
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

        def child_then_empty_parent(argv, timeout):
            discovered = self.discover(argv)
            if discovered is not None:
                return discovered
            return completed_process(
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
            return completed_process(
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
