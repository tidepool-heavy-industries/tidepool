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
                             [f'{name}=900' for name in sorted([qualification.M2_SURVIVAL_TEST, qualification.M2_NOMINAL_JOIN_TEST, qualification.M2_CHECKPOINT_RELEASE_TEST, qualification.M2_SELECTED_CODING_TEST])])
            self.assertEqual(report['scheduling'], {
                'jobs': 4, 'effective_jobs': 4, 'delegated_service': True,
                'service_slice': 'tidepool-completion-build.slice', 'timeout_seconds': 600,
                'case_timeout_seconds': {name: 900 for name in [qualification.M2_SURVIVAL_TEST, qualification.M2_NOMINAL_JOIN_TEST, qualification.M2_CHECKPOINT_RELEASE_TEST, qualification.M2_SELECTED_CODING_TEST]}})
            self.assertEqual(report['executed_test_count'], 7)
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

    def test_catalog_source_provenance_checks_original_trees_without_unused_bundle_copies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, original, bundle = root / 'source', root / 'original', root / 'bundle'
            source.mkdir()
            bundle.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            for relative in ('lib/Library.hs', 'actors/Actor.hs'):
                tracked = source / 'bridge/haskell' / relative
                retained = original / relative
                tracked.parent.mkdir(parents=True, exist_ok=True)
                retained.parent.mkdir(parents=True, exist_ok=True)
                tracked.write_text('source ' + relative)
                retained.write_bytes(tracked.read_bytes())
            subprocess.run(['git', '-C', str(source), 'add', 'bridge'], check=True)
            evidence = qualification.declared_haskell_sources(source, bundle, original)
            self.assertEqual(set(evidence), {'stdlib', 'actors'})
            self.assertFalse((bundle / 'share/exomonad/stdlib').exists())
            (original / 'actors/Actor.hs').write_text('changed actor')
            with self.assertRaisesRegex(ValueError, 'tracked declared Haskell source bytes'):
                qualification.declared_haskell_sources(source, bundle, original)

    def test_runtime_environment_rejects_ambient_catalog_and_daemon_selection(self):
        with patch.dict(os.environ, {
            'TIDEPOOL_EXTRACT_DAEMON_SOCKET': '/tmp/unqualified.sock',
            'TIDEPOOL_COMPILER_MODULES': '/nix/store/older-project/catalog.json',
            'TIDEPOOL_EXTRACT_NO_DAEMON': '1',
            'TIDEPOOL_EXTRACT': '/tmp/older-extract',
            'EXOMONAD_WORKSPACE_GITLINK': '/tmp/older-workspace-gitlink.json',
            'EXOMONAD_NIX_BIN': '/tmp/hostile-nix',
            'EXOMONAD_NIX_OFFLINE': '1',
        }):
            environment = qualification.execution_environment({'environment': {
                'TIDEPOOL_EXTRACT': '/frozen/bin/tidepool-extract',
                'EXOMONAD_NIX_BIN': '/frozen/runtime-tools/bin/nix',
            }})
        self.assertEqual(environment['TIDEPOOL_EXTRACT'], '/frozen/bin/tidepool-extract')
        self.assertEqual(environment['EXOMONAD_NIX_BIN'], '/frozen/runtime-tools/bin/nix')
        for key in ('TIDEPOOL_EXTRACT_DAEMON_SOCKET', 'TIDEPOOL_COMPILER_MODULES', 'TIDEPOOL_EXTRACT_NO_DAEMON', 'EXOMONAD_WORKSPACE_GITLINK', 'EXOMONAD_NIX_OFFLINE'):
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

    def test_catalog_gate_rejects_source_mode_before_launch(self):
        with patch.object(qualification, 'verify', return_value={'stdlib_mode': 'source-backed'}), \
             patch.object(qualification.subprocess, 'run') as execute:
            with self.assertRaisesRegex(ValueError, 'qualified native catalog mode'):
                qualification.run_catalog_gate(SimpleNamespace(descriptor=Path('/missing/descriptor'), output=Path('/missing/evidence')))
        execute.assert_not_called()

    def test_catalog_gate_requires_one_executed_passing_case_and_retains_admitted_digest(self):
        cases = [('passing', 1, True, 0), ('zero', 0, True, 1), ('unknown', None, True, 1),
                 ('failed', 1, False, 1), ('missing', None, None, 1)]
        for label, count, passed, expected_code in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / 'qualification.json'
                path.write_text('admitted descriptor')
                admitted_digest = qualification.sha256(path)
                output = root / 'evidence'
                descriptor = {'bundle_root': str(root), 'stdlib_mode': 'catalog-backed',
                              'external_inputs': {'runtime_tools': {'path': '/pinned/runtime-tools'}},
                              'environment': {}, 'source_oid': 'a' * 40, 'harness_revision': 'b' * 40,
                              'profile': 'fast-dev', 'native_catalog': {'catalog_sha256': 'c' * 64}}
                def execute(command, **kwargs):
                    self.assertEqual(command, ['/pinned/runtime-tools/bin/bash', str(root / 'share/exomonad/packaged-catalog-consumer.sh'),
                        str(root), str(path), '/pinned/runtime-tools/bin/bwrap', str(output), '/pinned/runtime-tools/bin/python3'])
                    if passed is not None:
                        qualification.write_json(output / 'tests/case.json', {'test': qualification.CATALOG_TEST,
                            'passed': passed, 'execution': {'executed_test_count': count, 'exit_code': 0}})
                    path.write_text('changed after admission')
                    return subprocess.CompletedProcess(command, 0)
                with patch.object(qualification, 'verify', return_value=descriptor), \
                     patch.object(qualification.subprocess, 'run', side_effect=execute):
                    self.assertEqual(qualification.run_catalog_gate(SimpleNamespace(descriptor=path, output=output)), expected_code)
                report = json.loads((output / 'report.json').read_text())
                self.assertEqual(report['descriptor_sha256'], admitted_digest)
                self.assertEqual(report['selected_test_count'], 1)
                self.assertEqual(report['completed'], expected_code == 0)


class CatalogSourceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.sources = self.root / 'sources'
        self.effects = self.root / 'generated'
        self.snapshot = self.root / 'snapshot'
        self.jev_sources = self.root / 'jev'
        (self.jev_sources / 'core/Jev').mkdir(parents=True)
        (self.jev_sources / 'core/Jev/Core.hs').write_text('module Jev.Core where\n')
        self.modules = {
            'Library': 'lib/Library.hs', 'Actor': 'actors/Actor.hs', 'Jev.Core': 'jev/core/Jev/Core.hs',
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
        qualification.write_json(self.cohort, {'components': ['native-helper-contract'], 'modules': self.modules})
        self.args = SimpleNamespace(sources=self.sources, effects=self.effects,
                                    cohort=self.cohort, output=self.snapshot, jev_sources=self.jev_sources)

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
        self.assertEqual((self.snapshot / 'TidepoolCatalog.hs').read_text().count('import '), 5)
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
            if command[1:3] == ['--query', '--roots']:
                pins = list((self.root / 'retention/share/exomonad/gc-roots').iterdir())
                return '\n'.join(str(pin) + ' -> ' + str(retained) for pin in pins)
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
            selected = qualification.retain_catalog_sources(SimpleNamespace(
                snapshot=self.snapshot, output=self.root / 'retention', runtime_tools=tools))
            self.assertEqual(selected, retained)
            record = self.root / 'retention' / qualification.RETAINED_CATALOG_SOURCES
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

    def retained_fixture(self):
        qualification.snapshot_catalog_sources(self.args)
        original = self.root / 'registered-source'
        shutil.copytree(self.snapshot, original)
        tools = self.root / 'tools'
        (tools / 'bin').mkdir(parents=True)
        record = self.root / 'retention' / qualification.RETAINED_CATALOG_SOURCES
        pin = record.parent / 'gc-roots' / qualification.hashlib.sha256(str(original).encode()).hexdigest()
        pin.parent.mkdir(parents=True)
        pin.symlink_to(original)
        nar = {'roots': [str(original)], 'closure': [str(original)],
               'nar_hashes': {str(original): 'sha256:original'}}
        inventory = qualification.catalog_source_inventory(original)
        qualification.write_json(record, {
            'schema': 1, 'kind': 'native-catalog-source-retention', 'original_root': str(original),
            'inventory': inventory, 'inventory_sha256': qualification.digest_inventory(inventory),
            'nix_closure': nar, 'gc_roots': [{'path': str(pin), 'store_path': str(original),
                                             'command': ['fixture'], 'exit_code': 0}]})
        return original, tools, record, pin, nar

    @contextlib.contextmanager
    def nix_checks(self, nar, pins):
        def roots(command, **kwargs):
            self.assertEqual(command[1:3], ['--query', '--roots'])
            return '\n'.join(str(pin) + ' -> ' + nar['roots'][0] for pin in pins)
        with patch.object(qualification, 'nix_path', side_effect=lambda path: path.resolve(strict=True)), \
             patch.object(qualification, 'store_root', side_effect=lambda path: path.resolve(strict=True)), \
             patch.object(qualification, 'nix_metadata', return_value=nar), \
             patch.object(qualification.subprocess, 'check_output', side_effect=roots):
            yield

    def selection(self, original):
        return {'snapshot_root': str(original), 'roles': qualification.NATIVE_SOURCE_ROLES,
                'source_files': [[relative, 'b' * 64] for relative, item in
                    qualification.catalog_source_inventory(original).items()
                    if item['kind'] == 'file' and relative.endswith('.hs')]}

    def catalog_fixture(self, original, tools, record):
        bundle = self.root / 'bundle'
        shared = bundle / 'share/exomonad'
        catalog = shared / 'catalog/catalog.json'
        qualification.write_json(catalog, {'schema': 4, 'source_selection': self.selection(original),
                                          'producer_identity': [3] * 32, 'consumed_worker_identity': [4] * 32,
                                          'modules': []})
        shutil.copy2(record, catalog.parent / 'source-retention.json')
        selected = {'catalog_sha256': qualification.sha256(catalog),
                    'source_selection': self.selection(original),
                    'source_inventory_sha256': json.loads(record.read_text())['inventory_sha256'],
                    'product_inventory_sha256': qualification.digest_inventory(qualification.native_catalog_products(catalog.parent))}
        qualification.write_json(catalog.parent / qualification.NATIVE_CATALOG_BUILD, {
            'schema': 1, 'kind': 'native-catalog-build',
            'producer_target': '//tidepool/toolchain:tidepool-module-package',
            'retention_record_origin': str(record),
            'product_inventory': qualification.native_catalog_products(catalog.parent), **selected})
        contract = {'stdlib_mode': 'catalog-backed', 'native_catalog': selected}
        qualification.write_json(shared / 'native-build-contract.json', contract)
        (shared / 'runtime-tools').symlink_to(tools)
        (shared / 'ghc-libdir.txt').write_text(str(tools) + '\n')
        return bundle, contract, catalog

    def test_registered_collector_root_is_required_beyond_a_matching_symlink(self):
        original, tools, record, pin, nar = self.retained_fixture()
        with self.nix_checks(nar, []), patch.object(qualification.subprocess, 'run'):
            with self.assertRaisesRegex(ValueError, 'not registered'):
                qualification.verify_retained_catalog_sources(record, self.snapshot, tools)
        self.assertEqual(pin.resolve(), original)

    def test_declared_record_copy_keeps_the_original_gc_evidence_location(self):
        original, tools, record, pin, nar = self.retained_fixture()
        declared = self.root / 'declared.json'
        shutil.copy2(record, declared)
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run'):
            self.assertEqual(qualification.verify_retained_catalog_sources(
                declared, self.snapshot, tools, record_origin=record), original)
            with self.assertRaisesRegex(ValueError, 'GC root is missing'):
                qualification.verify_retained_catalog_sources(declared, self.snapshot, tools)

    def test_catalog_binds_exact_roles_manifest_and_unmodified_producer_bytes(self):
        original, tools, record, pin, nar = self.retained_fixture()
        bundle, contract, catalog = self.catalog_fixture(original, tools, record)
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run'):
            environment = qualification.native_environment(bundle)
            self.assertEqual(environment['TIDEPOOL_PRELUDE_DIR'], str(original / 'lib'))
            self.assertEqual(environment['TIDEPOOL_COMPILER_MODULES'], str(catalog))
            self.assertEqual(qualification.verify_native_catalog(bundle, tools), contract['native_catalog'])
            before = catalog.read_bytes()
            altered = json.loads(before)
            altered['source_selection']['roles'].reverse()
            qualification.write_json(catalog, altered)
            with self.assertRaisesRegex(ValueError, 'ordered source roles'):
                qualification.verify_native_catalog(bundle, tools)
            altered = json.loads(before)
            altered['source_selection']['source_files'].pop()
            qualification.write_json(catalog, altered)
            with self.assertRaisesRegex(ValueError, 'complete source manifest'):
                qualification.verify_native_catalog(bundle, tools)
            altered = json.loads(before)
            altered['consumed_worker_identity'][0] = 9
            qualification.write_json(catalog, altered)
            with self.assertRaisesRegex(ValueError, 'product inventory changed'):
                qualification.verify_native_catalog(bundle, tools)
            catalog.write_bytes(before)
            self.assertEqual(qualification.verify_native_catalog(bundle, tools), contract['native_catalog'])
            self.assertEqual(catalog.read_bytes(), before)

    def test_frozen_bundle_transfers_real_source_root_and_survives_old_pin_removal(self):
        original, tools, record, pin, nar = self.retained_fixture()
        bundle, contract, catalog = self.catalog_fixture(original, tools, record)
        before = catalog.read_bytes()
        new_pin = bundle / 'share/exomonad/gc-roots' / pin.name
        new_pin.parent.mkdir(parents=True)
        new_pin.symlink_to(original)
        pins = [{'path': str(new_pin), 'store_path': str(original), 'exit_code': 0, 'command': ['fixture']}]
        with self.nix_checks(nar, [pin, new_pin]), patch.object(qualification.subprocess, 'run'):
            qualification.transfer_catalog_retention(bundle, contract, pins, tools)
        pin.unlink()
        with self.nix_checks(nar, [new_pin]), patch.object(qualification.subprocess, 'run'):
            qualification.verify_native_catalog(bundle, tools)
            self.assertEqual(catalog.read_bytes(), before)
            new_pin.unlink()
            with self.assertRaises(FileNotFoundError):
                qualification.verify_native_catalog(bundle, tools)

    def test_native_action_refuses_changed_declared_snapshot_before_production(self):
        original, tools, record, pin, nar = self.retained_fixture()
        declared = self.root / 'declared'
        shutil.copytree(original, declared)
        (declared / 'actors/Actor.hs').write_text('substituted actor bytes')
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run') as execute:
            with self.assertRaisesRegex(ValueError, 'complete declared source snapshot'):
                qualification.build_native_catalog(SimpleNamespace(
                    runtime_tools=tools, retention_record=record, snapshot=self.snapshot,
                    retention_record_origin=record, source_root=original, declared_source_root=declared))
            self.assertEqual(len(execute.call_args_list), 1)
            self.assertIn('--verify-path', execute.call_args_list[0].args[0])

    def test_native_action_uses_original_probe_and_explicit_compiler_then_rechecks(self):
        original, tools, record, pin, nar = self.retained_fixture()
        paths = {}
        for name in ('producer', 'frontend', 'worker', 'deployment'):
            paths[name] = self.root / name
            paths[name].write_text(name)
        output = self.root / 'native-products'
        calls = []
        def execute(command, **kwargs):
            calls.append(command)
            if '--verify-path' in command:
                return subprocess.CompletedProcess(command, 0)
            self.assertEqual(command, [str(paths['producer']), 'build', '--source', str(original / 'TidepoolCatalog.hs'),
                '--target', 'catalogSentinel', '--source-root', str(original), '--output-root', str(output)])
            env = kwargs['env']
            self.assertEqual(env['TIDEPOOL_EXTRACT'], str(paths['frontend']))
            self.assertEqual(env['TIDEPOOL_EXTRACT_WORKER'], str(paths['worker']))
            self.assertEqual(env['TIDEPOOL_COMPILER_DEPLOYMENT'], str(paths['deployment']))
            self.assertNotIn('TIDEPOOL_COMPILER_MODULES', env)
            self.assertNotIn('TIDEPOOL_EXTRACT_DAEMON_SOCKET', env)
            qualification.write_json(output / 'catalog.json', {'schema': 4, 'source_selection': self.selection(original)})
            return subprocess.CompletedProcess(command, 0)
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run', side_effect=execute), \
             patch.dict(os.environ, {'TIDEPOOL_COMPILER_MODULES': '/ambient/catalog',
                                     'TIDEPOOL_EXTRACT_DAEMON_SOCKET': '/ambient/daemon'}):
            qualification.build_native_catalog(SimpleNamespace(
                runtime_tools=tools, retention_record=record, snapshot=self.snapshot, retention_record_origin=record,
                source_root=original, declared_source_root=original, ghc_libdir=tools, libraries=tools,
                output=output, **paths))
        self.assertEqual(sum('--verify-path' in command for command in calls), 2)
        self.assertEqual((output / 'source-retention.json').read_bytes(), record.read_bytes())
        receipt = json.loads((output / qualification.NATIVE_CATALOG_BUILD).read_text())
        self.assertEqual(receipt['catalog_sha256'], qualification.sha256(output / 'catalog.json'))

    def producer_args(self, original, tools, record, output):
        paths = {}
        for name in ('producer', 'frontend', 'worker', 'deployment'):
            paths[name] = self.root / name
            paths[name].write_text(name)
        return SimpleNamespace(runtime_tools=tools, retention_record=record, snapshot=self.snapshot,
            retention_record_origin=record, source_root=original, declared_source_root=original,
            ghc_libdir=tools, libraries=tools, output=output, timeout=900, **paths)

    def test_build_producer_refusal_still_rechecks_complete_retention(self):
        original, tools, record, pin, nar = self.retained_fixture()
        args = self.producer_args(original, tools, record, self.root / 'refused-products')
        calls = []
        def execute(command, **kwargs):
            calls.append(command)
            return subprocess.CompletedProcess(command, 0 if '--verify-path' in command else 7)
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run', side_effect=execute):
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                qualification.build_native_catalog(args)
        self.assertEqual(failure.exception.returncode, 7)
        self.assertEqual(sum('--verify-path' in command for command in calls), 2)
        self.assertFalse((args.output / qualification.NATIVE_CATALOG_BUILD).exists())
        self.assertFalse(args.output.with_name(args.output.name + '.invocation').exists())

    def test_inspection_preserves_refusal_and_secondary_retention_failure_distinctly(self):
        original, tools, record, pin, nar = self.retained_fixture()
        args = self.producer_args(original, tools, record, self.root / 'inspection')
        def execute(command, **kwargs):
            if '--verify-path' in command:
                return subprocess.CompletedProcess(command, 0)
            self.assertEqual(command[1], 'inspect')
            self.assertEqual(kwargs['cwd'], args.output.with_name(args.output.name + '.invocation'))
            self.assertFalse(args.output.exists())
            kwargs['stderr'].write(b'unsafe cache_safe refusal\n')
            (original / 'lib/Library.hs').write_text('source changed during failed inspection')
            return subprocess.CompletedProcess(command, 9)
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run', side_effect=execute):
            with self.assertRaisesRegex(ValueError, 'producer failed:.*retention recheck failed:'):
                qualification.inspect_native_catalog(args)
        evidence = args.output.with_name(args.output.name + '.invocation')
        report = json.loads((evidence / 'outcome.json').read_text())
        self.assertEqual(report['producer_exit_code'], 9)
        self.assertIn('returned non-zero exit status 9', report['producer_error'])
        self.assertFalse(report['retention_recheck']['passed'])
        self.assertIn('declared action snapshot', report['retention_recheck']['error'])
        self.assertFalse(report['catalog_qualified'])
        self.assertFalse(report['completed'])
        self.assertEqual((evidence / 'stderr.log').read_bytes(), b'unsafe cache_safe refusal\n')
        self.assertFalse((args.output / 'catalog.json').exists())
        self.assertFalse((args.output / qualification.NATIVE_CATALOG_BUILD).exists())

    def test_inspection_timeout_keeps_durable_logs_and_rechecks_retention(self):
        original, tools, record, pin, nar = self.retained_fixture()
        args = self.producer_args(original, tools, record, self.root / 'timed-out-inspection')
        calls = []
        def execute(command, **kwargs):
            calls.append(command)
            if '--verify-path' in command:
                return subprocess.CompletedProcess(command, 0)
            self.assertEqual(kwargs['timeout'], 900)
            self.assertEqual(kwargs['cwd'], args.output.with_name(args.output.name + '.invocation'))
            kwargs['stdout'].write(b'compiler started\n')
            kwargs['stderr'].write(b'partial compiler diagnostics\n')
            raise subprocess.TimeoutExpired(command, kwargs['timeout'])
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run', side_effect=execute), \
             patch.dict(os.environ, {'INSPECTION_AMBIENT_SECRET': 'private-ambient-credential'}):
            with self.assertRaises(subprocess.TimeoutExpired):
                qualification.inspect_native_catalog(args)
        evidence = args.output.with_name(args.output.name + '.invocation')
        report = json.loads((evidence / 'outcome.json').read_text())
        self.assertNotIn('INSPECTION_AMBIENT_SECRET', (evidence / 'invocation.json').read_text())
        self.assertNotIn('private-ambient-credential', (evidence / 'outcome.json').read_text())
        self.assertTrue(report['timed_out'])
        self.assertIsNone(report['producer_exit_code'])
        self.assertTrue(report['retention_recheck']['passed'])
        self.assertFalse(report['catalog_qualified'])
        self.assertFalse(report['completed'])
        self.assertEqual(sum('--verify-path' in command for command in calls), 2)
        self.assertEqual((evidence / 'stdout.log').read_bytes(), b'compiler started\n')
        self.assertEqual((evidence / 'stderr.log').read_bytes(), b'partial compiler diagnostics\n')
        self.assertFalse((args.output / qualification.NATIVE_CATALOG_BUILD).exists())

    def test_inspection_timeout_bounds_refuse_before_source_or_process_work(self):
        for timeout in (-1, 0, 599, 1801):
            with self.subTest(timeout=timeout), patch.object(qualification, 'nix_path') as selected, \
                 patch.object(qualification.subprocess, 'run') as execute:
                with self.assertRaisesRegex(ValueError, 'between 600 and 1800'):
                    qualification.inspect_native_catalog(SimpleNamespace(timeout=timeout))
            selected.assert_not_called()
            execute.assert_not_called()


if __name__ == '__main__':
    unittest.main()
