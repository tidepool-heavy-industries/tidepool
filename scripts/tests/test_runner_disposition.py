"""Actual local processes exercise result publication and owned-tree disposition."""
import contextlib
import errno
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

from scripts.tests.test_buck_isolated_libtest import runner


class RunnerDispositionTests(unittest.TestCase):
    def setUp(self):
        self.files = tempfile.TemporaryDirectory()
        self.addCleanup(self.files.cleanup)
        self.root = Path(self.files.name)
        self.output = self.root / 'results'
        self.external = self.root / 'external'
        self.external.mkdir()
        self.sentinel = self.external / 'keep'
        self.sentinel.write_bytes(b'outside bytes\n')
        self.sentinel.chmod(0o444)
        self.external.chmod(0o755)
        self.baseline = (self.sentinel.read_bytes(), stat.S_IMODE(self.sentinel.stat().st_mode),
                         stat.S_IMODE(self.external.stat().st_mode))
        self.binary = self.root / 'local-libtest'
        self.binary.write_text('#!' + sys.executable + '\n' + '''import os,pathlib,sys,time
if '--list' in sys.argv:
    if '--ignored' not in sys.argv:
        print('suite::first: test\\nsuite::second: test\\nsuite::third: test')
else:
    root=pathlib.Path(os.environ.get('TIDEPOOL_TEST_ARTIFACT_ROOT',os.environ['TIDEPOOL_COMPILE_CACHE_DIR']))
    deployment=root/'prepared-deployment';deployment.mkdir()
    workspace=deployment/'workspace';workspace.mkdir()
    (workspace/'input').write_bytes(b'private prepared input')
    (workspace/'input').chmod(0o444)
    (workspace/'external-dir').symlink_to(EXTERNAL,target_is_directory=True)
    (workspace/'external-file').symlink_to(SENTINEL)
    os.link(SENTINEL,workspace/'hardlink')
    root.chmod(0o555) if os.environ.get('TEST_READONLY_SIDECAR') else None
    workspace.chmod(0o555);deployment.chmod(0o555)
    if os.environ.get('TEST_CONCURRENT') and sys.argv[2] != 'suite::third':
        shared=pathlib.Path(SHARED);(shared/sys.argv[2].split('::')[1]).touch()
        deadline=time.monotonic()+5
        while not all((shared/name).exists() for name in ('first','second')):
            assert time.monotonic()<deadline, 'other process missing'
            time.sleep(.005)
    print('body marker: '+sys.argv[2])
    print('test result: ok. 1 passed; 0 failed; 0 ignored;')
'''.replace('EXTERNAL', repr(str(self.external))).replace('SENTINEL', repr(str(self.sentinel)))
            .replace('SHARED', repr(str(self.root))))
        self.binary.chmod(0o700)
        self.addCleanup(self.restore_private_directories)

    def restore_private_directories(self):
        # Exact test-private directories only; never symlink targets or files.
        for directory, dirs, _ in os.walk(self.root, followlinks=False):
            path = Path(directory)
            if path != self.external:
                path.chmod(stat.S_IMODE(path.stat().st_mode) | 0o700)

    def external_unchanged(self):
        self.assertEqual((self.sentinel.read_bytes(), stat.S_IMODE(self.sentinel.stat().st_mode),
                          stat.S_IMODE(self.external.stat().st_mode)), self.baseline)

    def run_history(self, count=2, jobs=1, **environment):
        arguments = [str(self.binary), '--expected-count', str(count), '--jobs', str(jobs),
                     '--output-dir', str(self.output)]
        for name in ('first', 'second', 'third')[:count]:
            arguments.extend(['--exact', 'suite::' + name])
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.dict(os.environ, environment), contextlib.redirect_stdout(stdout), \
                contextlib.redirect_stderr(stderr):
            code = runner.main(arguments)
        records = [json.loads(p.read_text()) for p in self.output.glob('*.json')]
        self.external_unchanged()
        return code, {r['test']: r for r in records}, stdout.getvalue(), stderr.getvalue()

    def record(self, name):
        return json.loads((self.output / (hashlib.sha256(name.encode()).hexdigest() + '.json')).read_text())

    def assert_body_preserved(self, record):
        self.assertEqual(record['execution']['exit_code'], 0)
        self.assertEqual(record['execution']['executed_test_count'], 1)
        self.assertEqual(record['execution']['passed_test_count'], 1)
        self.assertTrue(record['execution']['libtest_passed'])
        stream = (self.output / record['streams']['stdout']['path']).read_bytes()
        self.assertIn(('body marker: ' + record['test']).encode(), stream)
        self.assertEqual(hashlib.sha256(stream).hexdigest(), record['streams']['stdout']['sha256'])

    def test_actual_runner_sealed_tree_publishes_before_disposal_and_runs_next(self):
        remove = runner.remove_owned_tree
        observed = []
        def check(custody):
            key = Path(custody['path']).parent.name
            record = json.loads((self.output / (key + '.json')).read_text())
            self.assert_body_preserved(record)
            self.assertFalse(record['passed'])
            self.assertEqual(record['execution']['disposal']['status'], 'pending')
            observed.append(record['test'])
            return remove(custody)
        with patch.object(runner, 'remove_owned_tree', side_effect=check):
            code, records, _, _ = self.run_history()
        self.assertEqual(code, 0)
        self.assertEqual(len(records), 2)
        self.assertCountEqual(observed, records)
        for record in records.values():
            self.assert_body_preserved(record)
            self.assertTrue(record['passed'])
            self.assertEqual(record['execution']['disposal']['status'], 'removed')
            self.assertFalse(Path(record['execution']['artifact_root']).exists())

    def fail_first_disposal(self, custody):
        if hashlib.sha256(b'suite::first').hexdigest() in custody['path']:
            raise PermissionError(errno.EACCES, 'injected exact disposal refusal', custody['path'])
        return self.remove(custody)

    def test_disposal_refusal_retains_actual_result_and_later_cases(self):
        self.remove = runner.remove_owned_tree
        with patch.object(runner, 'remove_owned_tree', side_effect=self.fail_first_disposal):
            code, records, _, _ = self.run_history()
        self.assertEqual(code, 1)
        self.assertEqual(len(records), 2)
        first = records['suite::first']
        self.assert_body_preserved(first)
        self.assertFalse(first['passed'])
        self.assertEqual(first['execution']['disposal']['status'], 'failed')
        self.assertEqual(first['execution']['disposal']['error']['errno'], errno.EACCES)
        self.assertEqual(first['execution']['artifact_disposition'], 'partially_retained')
        self.assertTrue(records['suite::second']['passed'])

    def test_disposal_and_sidecar_failures_preserve_both_causes_and_continue(self):
        self.remove = runner.remove_owned_tree
        with patch.object(runner, 'remove_owned_tree', side_effect=self.fail_first_disposal):
            code, records, _, _ = self.run_history(TEST_READONLY_SIDECAR='1')
        self.assertEqual(code, 1)
        self.assertEqual(len(records), 2)
        first = records['suite::first']
        self.assert_body_preserved(first)
        self.assertEqual(first['execution']['disposal']['error']['errno'], errno.EACCES)
        self.assertEqual(first['execution']['artifact_record_error']['type'], 'PermissionError')
        self.assertTrue(records['suite::second']['passed'])

    def test_concurrent_disposal_failure_drains_results_and_schedules_tail(self):
        self.remove = runner.remove_owned_tree
        with patch.object(runner, 'remove_owned_tree', side_effect=self.fail_first_disposal):
            code, records, _, _ = self.run_history(3, 2, TEST_CONCURRENT='1')
        self.assertEqual(code, 1)
        self.assertEqual(len(records), 3)
        for record in records.values():
            self.assert_body_preserved(record)
        self.assertEqual(sum(r['passed'] for r in records.values()), 2)

    def test_final_publication_failure_keeps_prior_count_and_drains_existing_case(self):
        replace = runner._replace_result_file
        writes = 0
        first = hashlib.sha256(b'suite::first').hexdigest() + '.json'
        def fail(path, data):
            nonlocal writes
            if path.name == first:
                writes += 1
                if writes == 2:
                    raise OSError(errno.ENOSPC, 'injected final publication failure', str(path))
            return replace(path, data)
        with patch.object(runner, '_replace_result_file', side_effect=fail):
            code, records, _, stderr = self.run_history(3, 2, TEST_CONCURRENT='1')
        self.assertEqual(code, 1)
        self.assertIn('publication failed', stderr)
        self.assertIn('suite::second', records)
        self.assertTrue(records['suite::second']['passed'])
        self.assert_body_preserved(records['suite::first'])
        self.assertFalse(records['suite::first']['passed'])
        self.assertEqual(records['suite::first']['execution']['disposal']['status'], 'pending')

    def owned_directory(self, name='private'):
        directory = self.root / name
        directory.mkdir(mode=0o700)
        return directory, runner.disposable_tree_custody(directory)

    def test_observed_root_replacement_refuses_without_touching_replacement(self):
        directory, custody = self.owned_directory()
        directory.rename(self.root / 'original')
        directory.mkdir(mode=0o700)
        (directory / 'keep').write_bytes(b'replacement')
        with self.assertRaises(PermissionError):
            runner.remove_owned_tree(custody)
        self.assertEqual((directory / 'keep').read_bytes(), b'replacement')

    def test_observed_parent_replacement_refuses(self):
        parent = self.root / 'parent'; parent.mkdir(mode=0o700)
        directory = parent / 'private'; directory.mkdir(mode=0o700)
        custody = runner.disposable_tree_custody(directory)
        parent.rename(self.root / 'original-parent')
        parent.mkdir(mode=0o700); directory.mkdir(mode=0o700)
        with self.assertRaises(PermissionError):
            runner.remove_owned_tree(custody)
        self.assertTrue(directory.is_dir())

    def test_root_symlink_and_missing_root_refuse(self):
        for kind in ('link', 'missing'):
            with self.subTest(kind=kind):
                directory, custody = self.owned_directory(kind)
                directory.rmdir()
                if kind == 'link':
                    directory.symlink_to(self.external, target_is_directory=True)
                with self.assertRaises(OSError):
                    runner.remove_owned_tree(custody)
                self.external_unchanged()

    def test_child_replaced_by_symlink_at_open_is_not_followed(self):
        directory, custody = self.owned_directory()
        child = directory / 'child'; child.mkdir(mode=0o555)
        original_open = runner.os.open
        replaced = False
        def replace(name, *args, **kwargs):
            nonlocal replaced
            if name == 'child' and not replaced:
                replaced = True
                child.rmdir(); child.symlink_to(self.external, target_is_directory=True)
            return original_open(name, *args, **kwargs)
        with patch.object(runner.os, 'open', side_effect=replace), self.assertRaises(OSError):
            runner.remove_owned_tree(custody)
        self.assertTrue(replaced)
        self.external_unchanged()

    def test_unowned_or_other_device_child_refuses_before_permission_change(self):
        for field in ('st_uid', 'st_dev'):
            with self.subTest(field=field):
                directory, custody = self.owned_directory(field)
                child = directory / 'child'; child.mkdir(mode=0o555)
                inode = child.stat().st_ino
                original = runner.os.fstat
                def different(fd):
                    metadata = original(fd)
                    if metadata.st_ino == inode:
                        values = list(metadata)
                        values[4 if field == 'st_uid' else 2] += 1
                        return os.stat_result(values)
                    return metadata
                with patch.object(runner.os, 'fstat', side_effect=different), self.assertRaises(OSError):
                    runner.remove_owned_tree(custody)
                self.assertEqual(stat.S_IMODE(child.stat().st_mode), 0o555)

    def test_uncertain_settlement_refuses_disposal_with_count_intact(self):
        # Actual local execution issues custody; then challenge its release facts.
        record = {}
        outcome = runner.run_one(str(self.binary), 'suite::first', False, 5, record,
                                 artifact_root=self.root / 'artifacts')
        record['process_cleanup_status'] = 'unknown'
        passed, _ = runner.retain_case_result(self.output, 'suite::first', *outcome, record)
        self.assertFalse(passed)
        retained = self.record('suite::first')
        self.assert_body_preserved(retained)
        self.assertEqual(retained['execution']['disposal']['status'], 'failed')
        self.assertTrue(Path(record['artifact_root']).is_dir())

    def test_no_artifact_cache_uses_same_disposition_owner(self):
        record = {}
        outcome = runner.run_one(str(self.binary), 'suite::first', False, 5, record)
        self.assertEqual(record['disposal']['kind'], 'compile_cache')
        passed, _ = runner.retain_case_result(self.output, 'suite::first', *outcome, record)
        self.assertTrue(passed)
        self.assertFalse(Path(record['compile_cache_root']).exists())
        self.assert_body_preserved(self.record('suite::first'))
        self.external_unchanged()
