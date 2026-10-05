"""Artifact mutation and runtime selection checks for native qualification."""
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import subprocess
import shutil
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'build/package/qualification.py'
SPEC = importlib.util.spec_from_file_location('native_qualification', SCRIPT)
qualification = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qualification)


class NativeQualificationTests(unittest.TestCase):
    def test_run_forwards_scheduling_and_sealed_watchdogs_and_records_them(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            descriptor_path = root / 'qualification.json'
            descriptor_path.write_text('sealed descriptor')
            descriptor = {'programs': {'runner': '/frozen/runner', 'libtest': '/frozen/libtest'},
                          'cohorts': qualification.cohorts(), 'environment': {},
                          'source_oid': 'a' * 40, 'harness_revision': 'b' * 40,
                          'profile': 'fast-dev', 'stdlib_mode': 'source-backed'}

            def execute(command, **kwargs):
                tests = root / 'evidence/tests'
                tests.mkdir()
                cohort = descriptor['cohorts']['m2']
                for index, name in enumerate(cohort['tests']):
                    qualification.write_json(tests / f'{index}.json', {
                        'test': name, 'passed': True,
                        'execution': {'executed_test_count': 1, 'exit_code': 0}})
                self.assertEqual(kwargs['env'], qualification.execution_environment(descriptor))
                return subprocess.CompletedProcess(command, 0)

            with patch.object(qualification, 'verify', return_value=descriptor), \
                 patch.object(qualification.subprocess, 'run', side_effect=execute):
                code = qualification.main(['run', str(descriptor_path), '--cohort', 'm2',
                    '--output', str(root / 'evidence'), '--jobs', '4', '--delegated-service',
                    '--service-slice', 'tidepool-completion-build.slice'])
            self.assertEqual(code, 0)
            report = json.loads((root / 'evidence/report.json').read_text())
            command = report['command']
            self.assertEqual(command[command.index('--jobs') + 1], '4')
            self.assertIn('--delegated-service', command)
            self.assertEqual(command[command.index('--service-slice') + 1], 'tidepool-completion-build.slice')
            self.assertEqual([command[index + 1] for index, value in enumerate(command) if value == '--exact'], qualification.M2_TESTS)
            self.assertEqual([command[index + 1] for index, value in enumerate(command) if value == '--case-timeout'],
                             [f'{name}=900' for name in sorted([qualification.M2_SURVIVAL_TEST, qualification.M2_NOMINAL_JOIN_TEST, qualification.M2_CHECKPOINT_RELEASE_TEST])])
            self.assertEqual(report['scheduling'], {
                'jobs': 4, 'effective_jobs': 4, 'delegated_service': True,
                'service_slice': 'tidepool-completion-build.slice', 'timeout_seconds': 600,
                'case_timeout_seconds': {name: 900 for name in [qualification.M2_SURVIVAL_TEST, qualification.M2_NOMINAL_JOIN_TEST, qualification.M2_CHECKPOINT_RELEASE_TEST]}})
            self.assertEqual(report['executed_test_count'], 6)
            self.assertTrue(report['completed'])

    def test_invalid_run_scheduling_refuses_before_verification_or_launch(self):
        invalid = (['--jobs', '0'], ['--jobs', '-1'], ['--service-slice', 'app.slice'],
                   ['--delegated-service', '--service-slice', '../unsafe.slice'],
                   ['--delegated-service', '--service-slice', 'not-a-slice'])
        for options in invalid:
            with self.subTest(options=options), patch.object(qualification, 'verify') as verify, \
                 patch.object(qualification.subprocess, 'run') as execute, \
                 contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(qualification.main(['run', '/missing/descriptor', '--cohort', 'm2',
                                                     '--output', '/missing/evidence', *options]), 1)
            verify.assert_not_called()
            execute.assert_not_called()

    def test_shared_execution_provider_records_actual_process_and_declared_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            descriptor_path = root / 'qualification.json'
            descriptor_path.write_text('retained descriptor bytes')
            admitted_digest = qualification.sha256(descriptor_path)
            descriptor = {'programs':{'host':sys.executable}, 'environment':{'TIDEPOOL_EXTRACT':'/frozen/compiler'},
                          'source_oid':'a' * 40, 'harness_revision':'b' * 40,
                          'profile':'fast-dev', 'stdlib_mode':'source-backed'}
            code = "import os,json; print(json.dumps([os.environ['TIDEPOOL_EXTRACT'],os.environ.get('TIDEPOOL_COMPILER_MODULES'),os.environ['XDG_CACHE_HOME']]))"
            with patch.object(qualification, 'verify', return_value=descriptor), \
                 patch.dict(os.environ, {'TIDEPOOL_EXTRACT':'/ambient/compiler', 'TIDEPOOL_COMPILER_MODULES':'/ambient/catalog'}):
                execution = qualification.launch_execution(descriptor_path, ['-c', code],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, cache_root=root)
                stdout, stderr = execution.process.communicate(timeout=5)
                descriptor_path.write_text('changed after admission')
                execution.record(root / 'process.json')
            self.assertEqual(execution.process.returncode, 0, stderr)
            self.assertEqual(json.loads(stdout), ['/frozen/compiler', None, str(root)])
            receipt = json.loads((root / 'process.json').read_text())
            self.assertEqual(receipt['command'], [sys.executable, '-c', code])
            self.assertEqual(receipt['process_execution_count'], 1)
            self.assertEqual(receipt['descriptor_sha256'], admitted_digest)
            self.assertEqual(receipt['exit_code'], 0)

    def test_shared_execution_provider_refuses_before_launch(self):
        with patch.object(qualification, 'verify', side_effect=ValueError('changed bundle')), \
             patch.object(qualification.subprocess, 'Popen') as spawn:
            with self.assertRaisesRegex(ValueError, 'changed bundle'):
                qualification.launch_execution(Path('/missing/qualification.json'), ['init'])
        spawn.assert_not_called()
        with patch.object(qualification, 'verify', return_value={}), \
             patch.object(qualification.subprocess, 'Popen') as spawn:
            with self.assertRaisesRegex(ValueError, 'actual package operation'):
                qualification.launch_execution(Path('/missing/qualification.json'), ['--help'])
        spawn.assert_not_called()

    def test_workspace_descriptor_must_match_the_actual_committed_gitlink(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            (source / 'tracked.txt').write_text('first commit\n')
            subprocess.run(['git', '-C', str(source), 'add', 'tracked.txt'], check=True)
            commit = ['git', '-C', str(source), '-c', 'user.name=Qualification test',
                      '-c', 'user.email=qualification@example.invalid', 'commit', '-qm']
            subprocess.run([*commit, 'fixture commit'], check=True)
            revision = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
            subprocess.run(['git', '-C', str(source), 'update-index', '--add', '--cacheinfo',
                            f'160000,{revision},.exomonad/workspace'], check=True)
            subprocess.run([*commit, 'record workspace Gitlink'], check=True)
            descriptor = source / 'workspace-gitlink.json'
            record = {'schema': 1, 'path': '.exomonad/workspace', 'mode': '160000', 'revision': revision}
            qualification.write_json(descriptor, record)
            self.assertEqual(qualification.verify_workspace_gitlink(source, descriptor), record)
            record['revision'] = '0' * 40
            qualification.write_json(descriptor, record)
            with self.assertRaisesRegex(ValueError, 'differs from the source HEAD recorded submodule'):
                qualification.verify_workspace_gitlink(source, descriptor)

    def test_owned_build_contract_rejects_mixed_revision_profile_and_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, bundle = root / 'source', root / 'bundle'
            source.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            (source / 'native.rs').write_text('fn shipped() {}\n')
            subprocess.run(['git', '-C', str(source), 'add', 'native.rs'], check=True)
            commit = ['git', '-C', str(source), '-c', 'user.name=Qualification test',
                      '-c', 'user.email=qualification@example.invalid', 'commit', '-qm']
            subprocess.run([*commit, 'first revision'], check=True)
            inputs = {'native.rs': qualification.sha256(source / 'native.rs')}
            contract = {'profile': 'fast-dev', 'source_inputs': inputs,
                        'source_inputs_sha256': qualification.digest_inventory(inputs), 'artifacts': {}}
            for relative, target in qualification.ARTIFACT_TARGETS.items():
                path = bundle / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('owned build bytes: ' + relative)
                contract['artifacts'][relative] = {'target': target, 'sha256': qualification.sha256(path)}
            qualification.verify_build_contract(source, bundle, contract, 'fast-dev')
            with self.assertRaisesRegex(ValueError, 'profile differs'):
                qualification.verify_build_contract(source, bundle, contract, 'production')
            libtest = bundle / 'bin/tidepool-tests'
            original = libtest.read_bytes()
            libtest.write_bytes(b'libtest from a different build')
            with self.assertRaisesRegex(ValueError, 'mixed native build artifact: bin/tidepool-tests'):
                qualification.verify_build_contract(source, bundle, contract, 'fast-dev')
            libtest.write_bytes(original)
            (source / 'native.rs').write_text('fn shipped() { broken_revision(); }\n')
            subprocess.run(['git', '-C', str(source), 'add', 'native.rs'], check=True)
            subprocess.run([*commit, 'different revision'], check=True)
            with self.assertRaisesRegex(ValueError, 'different source bytes: native.rs'):
                qualification.verify_build_contract(source, bundle, contract, 'fast-dev')

    def test_native_build_contract_rejects_untracked_source_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            (source / 'undeclared.rs').write_text('fn undeclared() {}\n')
            inputs = {'undeclared.rs': qualification.sha256(source / 'undeclared.rs')}
            contract = {'profile': 'fast-dev', 'source_inputs': inputs,
                        'source_inputs_sha256': qualification.digest_inventory(inputs)}
            with self.assertRaisesRegex(ValueError, 'not tracked in the recorded source'):
                qualification.verify_build_contract(source, source, contract, 'fast-dev')

    def test_untracked_haskell_cannot_enter_a_bundle_with_clean_tracked_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, bundle = root / 'source', root / 'bundle'
            source.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            for folder, packaged in (('lib', 'stdlib'), ('actors', 'actors')):
                declared = source / 'bridge/haskell' / folder / 'Declared.hs'
                declared.parent.mkdir(parents=True)
                declared.write_text('module Declared where\n')
                copied = bundle / 'share/exomonad' / packaged / 'Declared.hs'
                copied.parent.mkdir(parents=True)
                copied.write_bytes(declared.read_bytes())
            subprocess.run(['git', '-C', str(source), 'add', 'bridge'], check=True)
            qualification.declared_haskell_sources(source, bundle)
            (source / 'bridge/haskell/lib/Undeclared.hs').write_text('module Undeclared where\n')
            (bundle / 'share/exomonad/stdlib/Undeclared.hs').write_text('module Undeclared where\n')
            with self.assertRaisesRegex(ValueError, 'tracked declared Haskell source bytes'):
                qualification.declared_haskell_sources(source, bundle)

    def test_runtime_environment_rejects_ambient_catalog_and_daemon_selection(self):
        with patch.dict(os.environ, {
            'TIDEPOOL_EXTRACT_DAEMON_SOCKET': '/tmp/unqualified.sock',
            'TIDEPOOL_COMPILER_MODULES': '/nix/store/older-project/catalog.json',
            'TIDEPOOL_EXTRACT_NO_DAEMON': '1',
            'TIDEPOOL_EXTRACT': '/tmp/older-extract',
            'EXOMONAD_WORKSPACE_GITLINK': '/tmp/older-workspace-gitlink.json',
        }):
            environment = qualification.execution_environment({'environment': {
                'TIDEPOOL_EXTRACT': '/frozen/bin/tidepool-extract',
            }})
        self.assertEqual(environment['TIDEPOOL_EXTRACT'], '/frozen/bin/tidepool-extract')
        for key in ('TIDEPOOL_EXTRACT_DAEMON_SOCKET', 'TIDEPOOL_COMPILER_MODULES', 'TIDEPOOL_EXTRACT_NO_DAEMON', 'EXOMONAD_WORKSPACE_GITLINK'):
            self.assertNotIn(key, environment)

    def test_frozen_bytes_may_not_change_and_descriptor_may_not_move(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'share/exomonad').mkdir(parents=True)
            binary = root / 'host'
            binary.write_bytes(b'original-host')
            descriptor_path = root / qualification.DESCRIPTOR
            descriptor = {
                'schema': 1, 'kind': 'native-runtime-qualification',
                'bundle_root': str(root), 'stdlib_mode': 'source-backed',
                'feature_profile': 'embedded-native', 'environment': {},
                'external_inputs': {}, 'elf_runtime': {},
            }
            descriptor['inventory'] = qualification.inventory(root, [qualification.DESCRIPTOR])
            descriptor['inventory_sha256'] = qualification.digest_inventory(descriptor['inventory'])
            qualification.write_json(descriptor_path, descriptor)
            qualification.verify_frozen_inventory(root, descriptor)
            binary.write_bytes(b'different-host')
            with self.assertRaisesRegex(ValueError, 'inventory changed'):
                qualification.verify_frozen_inventory(root, descriptor)
            moved = root / 'moved.json'
            moved.write_text(json.dumps(descriptor))
            with self.assertRaisesRegex(ValueError, 'relocated'):
                qualification.verify(moved)

    def test_source_symlinks_cannot_retain_mutable_checkout_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory).resolve()
            root = parent / 'bundle'
            root.mkdir()
            source = parent / 'checkout-source.hs'
            source.write_text('mutable source')
            (root / 'source.hs').symlink_to(source)
            with self.assertRaisesRegex(ValueError, 'escapes the bundle'):
                qualification.inventory(root)


class CatalogSourceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.sources = self.root / 'sources'
        self.effects = self.root / 'generated'
        self.snapshot = self.root / 'snapshot'
        self.modules = {
            'Library': 'lib/Library.hs', 'Actor': 'actors/Actor.hs',
            'Tidepool.Effects.Core': 'effects/Tidepool/Effects/Core.hs',
            'Tidepool.Effects.Authored': 'effects/Tidepool/Effects/Authored.hs',
        }
        for relative in ('lib/Library.hs', 'actors/Actor.hs'):
            path = self.sources / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('source ' + relative)
        for relative in ('Tidepool/Effects/Core.hs', 'Tidepool/Effects/Authored.hs', 'Tidepool/Effects.hs'):
            path = self.effects / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('generated ' + relative)
        self.cohort = self.root / 'cohort.json'
        qualification.write_json(self.cohort, {'component': 'native-helper-contract', 'modules': self.modules})
        self.args = SimpleNamespace(sources=self.sources, effects=self.effects,
                                    cohort=self.cohort, output=self.snapshot)

    def test_snapshot_keeps_library_roles_and_uses_declared_generated_bytes(self):
        source = self.sources / 'lib/Library.hs'
        original = self.root / 'buck-generated-source'
        source.rename(original)
        source.symlink_to(original)
        qualification.snapshot_catalog_sources(self.args)
        self.assertFalse((self.snapshot / 'lib/Library.hs').is_symlink())
        self.assertEqual((self.snapshot / 'lib/Library.hs').read_bytes(), original.read_bytes())
        self.assertEqual(qualification.catalog_source_metadata(self.snapshot)['modules'], self.modules)
        self.assertEqual((self.snapshot / 'effects/Tidepool/Effects/Core.hs').read_bytes(),
                         (self.effects / 'Tidepool/Effects/Core.hs').read_bytes())
        self.assertFalse((self.snapshot / 'effects/Tidepool/Effects.hs').exists())
        self.assertEqual((self.snapshot / 'TidepoolCatalog.hs').read_text().count('import '), 4)
        original.write_text('changed after source action')
        self.assertNotEqual((self.snapshot / 'lib/Library.hs').read_bytes(), original.read_bytes())

    def test_original_source_alias_and_cohort_probe_changes_refuse(self):
        qualification.snapshot_catalog_sources(self.args)
        path = self.snapshot / 'actors/Actor.hs'
        path.unlink()
        path.symlink_to(self.snapshot / 'lib/Library.hs')
        with self.assertRaisesRegex(ValueError, 'without aliases'):
            qualification.catalog_source_inventory(self.snapshot)
        path.unlink()
        path.write_text('actor')
        (self.snapshot / 'TidepoolCatalog.hs').write_text('module Different where')
        with self.assertRaisesRegex(ValueError, 'probe differs'):
            qualification.catalog_source_metadata(self.snapshot)

    def test_retention_returns_original_root_and_rechecks_nar_bytes_and_gc_root(self):
        qualification.snapshot_catalog_sources(self.args)
        retained = self.root / 'registered-source'
        tools = self.root / 'nix-tools'
        observed = []
        nar = {'roots': [str(retained)], 'closure': [str(retained)],
               'nar_hashes': {str(retained): 'sha256:original'}}

        def add(command, **kwargs):
            observed.append(command)
            self.assertEqual(command[1], '--add')
            staged = Path(command[2])
            self.assertEqual(staged.name, 'tidepool-catalog-sources')
            shutil.copytree(staged, retained)
            return str(retained) + '\n'

        def run(command, **kwargs):
            observed.append(command)
            if '--add-root' in command:
                Path(command[command.index('--add-root') + 1]).symlink_to(retained)
            else:
                self.assertEqual(command[1:], ['--verify-path', str(retained)])
            return subprocess.CompletedProcess(command, 0)

        with patch.object(qualification, 'nix_path', side_effect=lambda path: path.resolve(strict=True)), \
             patch.object(qualification, 'store_root', side_effect=lambda path: path.resolve(strict=True)), \
             patch.object(qualification, 'nix_metadata', return_value=nar) as metadata, \
             patch.object(qualification.subprocess, 'check_output', side_effect=add), \
             patch.object(qualification.subprocess, 'run', side_effect=run):
            tools.mkdir()
            record = qualification.retain_catalog_sources(SimpleNamespace(
                snapshot=self.snapshot, output=self.root / 'retention', runtime_tools=tools))
            self.assertEqual(qualification.verify_retained_catalog_sources(record, self.snapshot, tools), retained)
            self.assertGreaterEqual(sum('--verify-path' in command for command in observed), 3)
            metadata.return_value = nar | {'nar_hashes': {str(retained): 'sha256:changed'}}
            with self.assertRaisesRegex(ValueError, 'NAR registration changed'):
                qualification.verify_retained_catalog_sources(record, self.snapshot, tools)
            metadata.return_value = nar
            source = retained / 'lib/Library.hs'
            original = source.read_bytes()
            source.write_text('changed source')
            with self.assertRaisesRegex(ValueError, 'declared action snapshot'):
                qualification.verify_retained_catalog_sources(record, self.snapshot, tools)
            source.write_bytes(original)
            value = json.loads(record.read_text())
            Path(value['gc_roots'][0]['path']).unlink()
            with self.assertRaises(FileNotFoundError):
                qualification.verify_retained_catalog_sources(record, self.snapshot, tools)


if __name__ == '__main__':
    unittest.main()
