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


def runtime_tools_fixture(root):
    """Permission fixtures only; these files are never compiler/tool executions."""
    tools = root / 'runtime-tools-fixture'
    (tools / 'bin').mkdir(parents=True)
    for name in qualification.REQUIRED_RUNTIME_EXECUTABLES:
        path = tools / 'bin' / name
        path.write_text('source-only executable permission fixture\n')
        path.chmod(0o755)
    return tools


def browser_driver_fixture(source, driver):
    """Small source/artifact identity fixture; no browser execution evidence."""
    inputs = {}
    for relative, original in qualification.BROWSER_DRIVER_SOURCES.items():
        path = source / original
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('declared browser input: ' + original + '\n')
        copied = driver / relative
        copied.parent.mkdir(parents=True, exist_ok=True)
        copied.write_bytes(path.read_bytes())
        inputs[original] = qualification.sha256(path)
    dependency = driver / 'node_modules/playwright/index.js'
    dependency.parent.mkdir(parents=True)
    dependency.write_text('locked dependency bytes\n')
    return inputs


def root_entry_source_fixture(root, modules):
    """Generated source declaration fixture, without compilation authority."""
    path = root / qualification.ROOT_ENTRY_SOURCE
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('module TidepoolPreparedDriver where\n')
    for relative in qualification.CATALOG_SOURCE_ROOTS.values():
        (root / relative).mkdir(parents=True, exist_ok=True)
    (root / 'TidepoolCatalog.hs').write_text(qualification.catalog_probe(modules))
    generated = qualification.root_entry_source_record(root)
    qualification.write_json(root / qualification.CATALOG_SOURCE_METADATA, {
        'schema': 1, 'kind': 'native-catalog-source-snapshot', 'components': ['fixture'],
        'modules': modules, 'roots': qualification.CATALOG_SOURCE_ROOTS,
        'probe': 'TidepoolCatalog.hs', 'targets': ['catalogSentinel'], 'root_entry': generated})
    return generated


class RegisteredGcRootTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.tools = self.root / 'tools'
        self.stores = [self.root / 'store-a', self.root / 'store-b']
        self.links = [self.root / 'root-a', self.root / 'root-b']
        for store, link in zip(self.stores, self.links):
            store.mkdir()
            link.symlink_to(store)
        self.pins = [{'path': str(link), 'store_path': str(store)}
                     for link, store in zip(self.links, self.stores)]

    def test_one_query_verifies_every_exact_pair_and_deduplicates_store_paths(self):
        alias = self.root / 'another-root-a'
        alias.symlink_to(self.stores[0])
        pins = self.pins + [{'path': str(alias), 'store_path': str(self.stores[0])}]
        output = (f'{self.links[1]} -> {self.stores[1]}\n'
                  f'{alias} -> {self.stores[0]}\n'
                  f'{self.root / "unrelated-root"} -> {self.stores[1]}\n'
                  f'{self.links[0]} -> {self.stores[0]}\n')
        with patch.object(qualification.subprocess, 'check_output', return_value=output) as query:
            qualification.verify_registered_gc_roots(self.tools, pins)
        query.assert_called_once_with(
            [str(self.tools / 'bin/nix-store'), '--query', '--roots',
             str(self.stores[0]), str(self.stores[1])], text=True, timeout=60)

    def test_missing_pair_refuses_despite_another_registered_root_for_its_store(self):
        output = (f'{self.links[0]} -> {self.stores[0]}\n'
                  f'{self.root / "other-root-b"} -> {self.stores[1]}\n')
        with patch.object(qualification.subprocess, 'check_output', return_value=output):
            with self.assertRaisesRegex(ValueError, 'not registered'):
                qualification.verify_registered_gc_roots(self.tools, self.pins)

    def test_wrong_pairs_refuse_despite_all_expected_root_and_store_names(self):
        output = (f'{self.links[0]} -> {self.stores[1]}\n'
                  f'{self.links[1]} -> {self.stores[0]}\n')
        with patch.object(qualification.subprocess, 'check_output', return_value=output):
            with self.assertRaisesRegex(ValueError, 'not registered'):
                qualification.verify_registered_gc_roots(self.tools, self.pins)

    def test_malformed_registration_does_not_establish_the_expected_pair(self):
        for invalid in ('', str(self.links[1]), f'{self.links[1]} -> ',
                        f'{self.links[1]} -> {self.stores[1]} trailing-junk'):
            with self.subTest(output=invalid):
                output = f'{self.links[0]} -> {self.stores[0]}\n{invalid}\n'
                with patch.object(qualification.subprocess, 'check_output', return_value=output):
                    with self.assertRaisesRegex(ValueError, 'not registered'):
                        qualification.verify_registered_gc_roots(self.tools, self.pins)

    def test_query_failure_or_timeout_cannot_establish_registration(self):
        for failure in (subprocess.CalledProcessError(1, ['nix-store']),
                        subprocess.TimeoutExpired(['nix-store'], 60)):
            with self.subTest(failure=type(failure).__name__):
                with patch.object(qualification.subprocess, 'check_output', side_effect=failure):
                    with self.assertRaises(type(failure)):
                        qualification.verify_registered_gc_roots(self.tools, self.pins)

    def test_changed_or_removed_symlink_refuses_before_the_query(self):
        self.links[1].unlink()
        with patch.object(qualification.subprocess, 'check_output') as query:
            with self.assertRaises(FileNotFoundError):
                qualification.verify_registered_gc_roots(self.tools, self.pins)
            self.links[1].symlink_to(self.stores[0])
            with self.assertRaisesRegex(ValueError, 'removed or changed'):
                qualification.verify_registered_gc_roots(self.tools, self.pins)
            query.assert_not_called()

    def test_relative_root_refuses_before_the_query(self):
        pins = [self.pins[0] | {'path': 'relative-root'}]
        with patch.object(qualification.subprocess, 'check_output') as query:
            with self.assertRaisesRegex(ValueError, 'removed or changed'):
                qualification.verify_registered_gc_roots(self.tools, pins)
            query.assert_not_called()

    def test_no_pins_requires_no_query(self):
        with patch.object(qualification.subprocess, 'check_output') as query:
            qualification.verify_registered_gc_roots(self.tools, [])
            query.assert_not_called()


class NativeQualificationTests(unittest.TestCase):
    def fixture_source(self, root, relative='workspace/fixtures/sample.hs', track_fixture=True):
        source = root / 'source'
        source.mkdir()
        subprocess.run(['git', 'init', '-q', str(source)], check=True)
        fixture = source / relative
        fixture.parent.mkdir(parents=True, exist_ok=True)
        fixture.write_text('module Sample where\n')
        resource_root = root / 'runtime-fixtures'
        resource = resource_root / relative
        resource.parent.mkdir(parents=True, exist_ok=True)
        resource.write_bytes(fixture.read_bytes())
        manifest = {'schema': 1, 'kind': 'haskell-test-fixtures', 'files': [relative]}
        manifest_path = source / qualification.TEST_FIXTURE_MANIFEST
        manifest_path.parent.mkdir(parents=True, exist_ok=True)
        manifest_path.write_text(json.dumps(manifest, sort_keys=True) + '\n')
        subprocess.run(['git', '-C', str(source), 'add', qualification.TEST_FIXTURE_MANIFEST], check=True)
        if track_fixture:
            subprocess.run(['git', '-C', str(source), 'add', relative], check=True)
        return source, fixture, manifest, resource_root

    def frozen_fixture(self, root):
        source, fixture, _, resource_root = self.fixture_source(root)
        bundle = root / 'bundle'
        (bundle / 'share/exomonad').mkdir(parents=True)
        record = qualification.copy_test_fixtures(source, source / qualification.TEST_FIXTURE_MANIFEST,
                                                  resource_root, bundle / 'share/exomonad')
        contract = {'source_inputs': {qualification.TEST_FIXTURE_MANIFEST:
                                     qualification.sha256(source / qualification.TEST_FIXTURE_MANIFEST),
                                     fixture.relative_to(source).as_posix(): qualification.sha256(fixture)},
                    'test_fixtures': record}
        qualification.write_json(bundle / 'share/exomonad/native-build-contract.json', contract)
        record = qualification.freeze_test_fixtures(source, bundle)
        return source, fixture, bundle, record

    def test_frozen_fixture_inventory_rejects_changed_missing_and_extra_files(self):
        for change in ('changed', 'missing', 'extra'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                _, _, bundle, record = self.frozen_fixture(Path(directory))
                fixture = bundle / qualification.TEST_FIXTURE_ROOT / 'workspace/fixtures/sample.hs'
                if change == 'changed':
                    fixture.write_text('module Changed where\n')
                elif change == 'missing':
                    fixture.unlink()
                else:
                    (fixture.parent / 'extra.hs').write_text('module Extra where\n')
                with self.assertRaisesRegex(ValueError, 'fixture'):
                    qualification.verify_frozen_test_fixtures(bundle, record)

    def test_fixture_snapshot_rejects_absent_and_symlinked_roots_or_directories(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            record = {'manifest': {'schema': 1, 'kind': 'haskell-test-fixtures', 'files': []},
                      'manifest_sha256': '0' * 64, 'files': {}}
            with self.assertRaises((FileNotFoundError, ValueError)):
                qualification.verify_frozen_test_fixtures(root / 'absent', record)
            source, fixture, bundle, frozen = self.frozen_fixture(root)
            fixture.unlink()
            fixture.parent.rmdir()
            outside = root / 'outside'
            outside.mkdir()
            (outside / 'sample.hs').write_text('module Sample where\n')
            (source / 'workspace/fixtures').symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'missing or aliased'):
                qualification.fixture_manifest(source)
            fixture_dir = bundle / qualification.TEST_FIXTURE_ROOT / 'workspace/fixtures'
            shutil.rmtree(fixture_dir)
            fixture_dir.symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'inventory|aliases'):
                qualification.verify_frozen_test_fixtures(bundle, frozen)
            fixture_dir.unlink()
            shutil.rmtree(bundle / qualification.TEST_FIXTURE_ROOT)
            (bundle / qualification.TEST_FIXTURE_ROOT).symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'root is missing or aliased'):
                qualification.verify_frozen_test_fixtures(bundle, frozen)

    def test_frozen_fixture_verifier_rejects_identical_manifest_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            _, _, bundle, record = self.frozen_fixture(Path(directory))
            manifest = bundle / 'share/exomonad/test-fixtures.json'
            external = Path(directory) / 'external-manifest.json'
            shutil.copyfile(manifest, external)
            manifest.unlink()
            manifest.symlink_to(external)
            with self.assertRaisesRegex(ValueError, 'manifest path.*aliased'):
                qualification.verify_frozen_test_fixtures(bundle, record)

    def test_frozen_fixture_verifier_rejects_identical_exomonad_ancestor_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, _, bundle, record = self.frozen_fixture(root)
            external_shared = root / 'external/exomonad'
            shutil.copytree(bundle / 'share/exomonad', external_shared)
            shutil.rmtree(bundle / 'share/exomonad')
            (bundle / 'share/exomonad').symlink_to(external_shared)
            with self.assertRaisesRegex(ValueError, 'manifest path.*aliased'):
                qualification.verify_frozen_test_fixtures(bundle, record)

    def test_fixture_manifest_rejects_noncanonical_traversal_and_absolute_names(self):
        invalid = ('./workspace/sample.hs', 'workspace//sample.hs', '../sample.hs', '/sample.hs')
        for relative in invalid:
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source, _, _, _ = self.fixture_source(root)
                manifest = {'schema': 1, 'kind': 'haskell-test-fixtures', 'files': [relative]}
                qualification.write_json(source / qualification.TEST_FIXTURE_MANIFEST, manifest)
                with self.assertRaisesRegex(ValueError, 'fixture'):
                    qualification.fixture_manifest(source)

    def test_fixture_manifest_rejects_untracked_source_file(self):
        with tempfile.TemporaryDirectory() as directory:
            source, _, _, _ = self.fixture_source(Path(directory), track_fixture=False)
            with self.assertRaisesRegex(ValueError, 'not tracked'):
                qualification.fixture_manifest(source)

    def test_fixture_manifest_accepts_a_tracked_file_in_initialized_source_submodule(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            submodule_source = root / 'fixture-source'
            submodule_source.mkdir()
            subprocess.run(['git', 'init', '-q', str(submodule_source)], check=True)
            subprocess.run(['git', '-C', str(submodule_source), 'config', 'user.name', 'fixture test'], check=True)
            subprocess.run(['git', '-C', str(submodule_source), 'config', 'user.email', 'fixture@example.invalid'], check=True)
            (submodule_source / 'fixtures').mkdir()
            (submodule_source / 'fixtures/sample.hs').write_text('module Sample where\n')
            subprocess.run(['git', '-C', str(submodule_source), 'add', '.'], check=True)
            subprocess.run(['git', '-C', str(submodule_source), 'commit', '-qm', 'fixture source'], check=True)

            source = root / 'source'; source.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            subprocess.run(['git', '-C', str(source), 'config', 'user.name', 'fixture test'], check=True)
            subprocess.run(['git', '-C', str(source), 'config', 'user.email', 'fixture@example.invalid'], check=True)
            subprocess.run(['git', '-C', str(source), '-c', 'protocol.file.allow=always',
                            'submodule', 'add', '-q', str(submodule_source), 'workspace'], check=True)
            manifest_path = source / qualification.TEST_FIXTURE_MANIFEST
            manifest_path.parent.mkdir(parents=True, exist_ok=True)
            manifest_path.write_text(json.dumps({'schema': 1, 'kind': 'haskell-test-fixtures',
                                                 'files': ['workspace/fixtures/sample.hs']}, sort_keys=True) + '\n')
            subprocess.run(['git', '-C', str(source), 'add', qualification.TEST_FIXTURE_MANIFEST], check=True)
            subprocess.run(['git', '-C', str(source), 'commit', '-qm', 'source manifest'], check=True)
            manifest, hashes = qualification.fixture_manifest(source)
            self.assertEqual(manifest['files'], ['workspace/fixtures/sample.hs'])
            self.assertEqual(hashes['workspace/fixtures/sample.hs'],
                             qualification.sha256(source / 'workspace/fixtures/sample.hs'))

    def test_fixture_descriptor_must_match_native_source_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            source, _, bundle, record = self.frozen_fixture(Path(directory))
            contract = {'test_fixtures': record, 'source_inputs': {
                qualification.TEST_FIXTURE_MANIFEST: qualification.sha256(source / qualification.TEST_FIXTURE_MANIFEST),
                **record['files']}}
            qualification.verify_fixture_source_contract(record, contract)
            contract['source_inputs'][next(iter(record['files']))] = '0' * 64
            with self.assertRaisesRegex(ValueError, 'source contract'):
                qualification.verify_fixture_source_contract(record, contract)
            self.assertTrue(bundle.exists())

    def test_bundle_assembly_refuses_runtime_fixture_drift_and_unlisted_files(self):
        for mutation in ('changed', 'missing', 'extra'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source, _, _, resource_root = self.fixture_source(root)
                if mutation == 'changed':
                    (resource_root / 'workspace/fixtures/sample.hs').write_text('module Changed where\n')
                elif mutation == 'missing':
                    (resource_root / 'workspace/fixtures/sample.hs').unlink()
                else:
                    (resource_root / 'workspace/fixtures/extra.hs').write_text('module Extra where\n')
                shared = root / 'bundle/share/exomonad'
                shared.mkdir(parents=True)
                with self.assertRaisesRegex(ValueError, 'fixture'):
                    qualification.copy_test_fixtures(source, source / qualification.TEST_FIXTURE_MANIFEST,
                                                     resource_root, shared)
                self.assertEqual(list(shared.iterdir()), [])

    def test_native_bundle_assembly_copies_and_records_declared_fixture_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, fixture, _, resource_root = self.fixture_source(root)
            resource_fixture = resource_root / fixture.relative_to(source)
            resource_fixture.unlink()
            resource_fixture.symlink_to(fixture)
            assets = root / 'assets/web'; assets.mkdir(parents=True)
            (assets / 'index.html').write_text('web')
            sources = root / 'sources'
            for name in ('lib', 'actors'):
                (sources / name).mkdir(parents=True)
            libraries = root / 'libraries'; libraries.mkdir()
            (libraries / 'libfixture.so').write_bytes(b'library')
            inputs = {}
            for name in ('host', 'view-helper', 'frontend', 'worker', 'libtest'):
                path = root / name; path.write_bytes(name.encode()); inputs[name] = path
            workspace_gitlink = root / 'workspace-gitlink.json'
            workspace_gitlink.write_text(json.dumps({
                'schema': 1, 'path': '.exomonad/workspace', 'mode': '160000', 'revision': 'a' * 40}))
            workspace_bundle = root / 'workspace.bundle'; workspace_bundle.write_bytes(b'bundle')
            harness_revision = root / 'harness-revision'; harness_revision.write_text('revision\n')
            entrypoint = root / 'entrypoint.sh'; entrypoint.write_text('#!/bin/bash\nexec "$@"\n')
            tools = root / 'runtime-tools'; tools.mkdir()
            ghc = root / 'ghc-libdir'; ghc.mkdir()
            output = root / 'assembled'
            browser_driver = root / 'browser-driver'
            browser_driver_fixture(source, browser_driver)
            args = SimpleNamespace(
                output=output, host=inputs['host'], view_helper=inputs['view-helper'],
                frontend=inputs['frontend'], worker=inputs['worker'], libtest=inputs['libtest'],
                build_sources=source, test_fixtures=resource_root,
                fixture_manifest=source / qualification.TEST_FIXTURE_MANIFEST,
                workspace_gitlink=workspace_gitlink, workspace_git_bundle=workspace_bundle,
                sources=sources, assets=root / 'assets', libraries=libraries,
                harness_revision=harness_revision, runtime_tools=tools, ghc_libdir=ghc,
                entrypoint_template=entrypoint, browser_driver=browser_driver, profile='fast-dev')
            with patch.object(qualification, 'native_runtime_tools', return_value=tools), \
                 patch.object(qualification, 'nix_path', side_effect=lambda path: path.resolve(strict=True)), \
                 patch.object(qualification, 'verify_workspace_bundle'):
                qualification.assemble(args)
            contract = json.loads((output / 'share/exomonad/native-build-contract.json').read_text())
            self.assertEqual(contract['test_fixtures']['files'], {
                fixture.relative_to(source).as_posix(): qualification.sha256(fixture)})
            self.assertEqual((output / qualification.TEST_FIXTURE_ROOT / fixture.relative_to(source)).read_bytes(),
                             fixture.read_bytes())
            self.assertFalse((output / qualification.TEST_FIXTURE_ROOT / fixture.relative_to(source)).is_symlink())
            self.assertEqual((output / 'share/exomonad/test-fixtures.json').read_bytes(),
                             (source / qualification.TEST_FIXTURE_MANIFEST).read_bytes())
            qualification.verify_browser_driver(output / qualification.BROWSER_DRIVER_ROOT, contract)
            self.assertEqual(qualification.inventory(output / qualification.BROWSER_DRIVER_ROOT),
                             qualification.inventory(browser_driver))

    def test_browser_driver_rejects_changed_authored_inputs_and_dependency_artifacts(self):
        for relative in ('driver.mjs', 'package.json', 'package-lock.json',
                         'node_modules/playwright/index.js'):
            for change in ('replace', 'delete'):
                with self.subTest(relative=relative, change=change), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    driver = root / 'driver'
                    inputs = browser_driver_fixture(root / 'source', driver)
                    contract = {'source_inputs': inputs,
                                'browser_driver': qualification.browser_driver_record(driver, inputs)}
                    qualification.verify_browser_driver(driver, contract)
                    path = driver / relative
                    if change == 'replace':
                        path.write_text('bytes from another build\n')
                    else:
                        path.unlink()
                    with self.assertRaisesRegex(ValueError, 'browser driver differs'):
                        qualification.verify_browser_driver(driver, contract)

    def test_browser_driver_requires_owned_source_and_complete_dependency_tree(self):
        for change in ('missing-source', 'missing-contract', 'wrong-target', 'missing-dependencies',
                       'extra-dependency', 'changed-mode'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                driver = root / 'driver'
                inputs = browser_driver_fixture(root / 'source', driver)
                contract = {'source_inputs': inputs,
                            'browser_driver': qualification.browser_driver_record(driver, inputs)}
                if change == 'missing-source':
                    del inputs['build/testing/browser/driver.mjs']
                elif change == 'missing-contract':
                    del contract['browser_driver']
                elif change == 'wrong-target':
                    contract['browser_driver']['target'] = '//other:driver'
                elif change == 'missing-dependencies':
                    shutil.rmtree(driver / 'node_modules')
                elif change == 'extra-dependency':
                    (driver / 'node_modules/extra.js').write_text('unowned code')
                else:
                    (driver / 'driver.mjs').chmod(0o755)
                with self.assertRaisesRegex(ValueError, 'browser driver'):
                    qualification.verify_browser_driver(driver, contract)

    def test_artifact_contract_reuses_the_verified_inventory_without_rehashing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / 'bundle'
            inputs = browser_driver_fixture(root / 'source', bundle / qualification.BROWSER_DRIVER_ROOT)
            contract = {'source_inputs': inputs, 'artifacts': {},
                        'browser_driver': qualification.browser_driver_record(
                            bundle / qualification.BROWSER_DRIVER_ROOT, inputs)}
            for relative, target in qualification.ARTIFACT_TARGETS.items():
                path = bundle / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('artifact: ' + relative)
                contract['artifacts'][relative] = {'target': target, 'sha256': qualification.sha256(path)}
            files = qualification.inventory(bundle)
            descriptor = {'inventory': files, 'inventory_sha256': qualification.digest_inventory(files)}
            observed = qualification.verify_frozen_inventory(bundle, descriptor)
            with patch.object(qualification, 'sha256', side_effect=AssertionError('duplicate file scan')):
                qualification.verify_artifact_contract(bundle, contract, observed)
                del observed['bin/tidepool-tests']
                with self.assertRaisesRegex(ValueError, 'mixed native build artifact'):
                    qualification.verify_artifact_contract(bundle, contract, observed)

    def test_frozen_roster_requires_shutdown_and_restored_settlement_regressions(self):
        cohorts = qualification.cohorts()
        self.assertIn(qualification.M2_SHUTDOWN_TEST, cohorts['m2']['tests'])
        self.assertIn(qualification.M2_COORDINATOR_TEST, cohorts['m2']['tests'])
        regressions = cohorts['unified-regressions']
        self.assertEqual(regressions['expected_count'], 5)
        self.assertEqual(len(set(regressions['tests'])), 5)
        self.assertFalse(regressions['ignored'])

    def test_old_qualification_schema_requires_its_own_frozen_reader(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / qualification.DESCRIPTOR
            qualification.write_json(path, {'schema': 1, 'kind': 'native-runtime-qualification',
                                             'bundle_root': str(root)})
            with self.assertRaisesRegex(ValueError, 'unsupported schema'):
                qualification.verify(path)

    def test_older_unsealed_cohorts_require_their_own_frozen_reader(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / qualification.DESCRIPTOR
            older = qualification.cohorts()
            older.pop('harness-performance')
            for name in ('m2', 'm1', 'unified-regressions'):
                older[name].pop('compiler_mode')
            qualification.write_json(path, {
                'schema': qualification.QUALIFICATION_SCHEMA, 'kind': 'native-runtime-qualification',
                'bundle_root': str(root), 'stdlib_mode': 'catalog-backed',
                'feature_profile': 'embedded-native', 'programs': qualification.programs(root),
                'cohorts': older,
            })
            with self.assertRaisesRegex(ValueError, 'mandatory test cohorts'):
                qualification.verify(path)

    def test_runtime_cohorts_refuse_direct_or_unsealed_modes_before_launch(self):
        for name in qualification.cohorts():
            for mode in ('direct', None, 'unrecognized'):
                with self.subTest(cohort=name, mode=mode), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    descriptor = {
                        'external_inputs': {'runtime_tools': {'path': '/frozen/runtime-tools'}},
                        'cohorts': qualification.cohorts(),
                    }
                    options = []
                    if mode == 'direct':
                        options = ['--compiler-mode', 'direct']
                    elif mode is None:
                        descriptor['cohorts'][name].pop('compiler_mode')
                    else:
                        descriptor['cohorts'][name]['compiler_mode'] = mode
                    output = root / 'evidence'
                    with patch.object(qualification, 'verify', return_value=descriptor), \
                         patch.object(qualification.subprocess, 'run') as execute, \
                         contextlib.redirect_stderr(io.StringIO()):
                        self.assertEqual(qualification.main([
                            'run', str(root / 'qualification.json'), '--cohort', name,
                            '--output', str(output), *options]), 1)
                    execute.assert_not_called()
                    self.assertFalse(output.exists())

    def test_catalog_resources_cross_frozen_cohort_delegation_without_ambient_selection(self):
        runner_spec = importlib.util.spec_from_file_location(
            'frozen_resource_runner', SCRIPT.parent.parent / 'rust/isolated-libtest.py')
        runner = importlib.util.module_from_spec(runner_spec)
        runner_spec.loader.exec_module(runner)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            resources = {}
            for name in ('TIDEPOOL_COMPILER_MODULES', 'TIDEPOOL_PREPARED_ROOT_ENTRY',
                         'TIDEPOOL_TEST_FIXTURE_ROOT'):
                resource = root / name
                resource.mkdir()
                resources[name] = str(resource)
            descriptor_path = root / 'qualification.json'
            descriptor_path.write_text('sealed descriptor')
            descriptor = {
                'programs': {'runner': '/frozen/runner', 'libtest': '/frozen/libtest'},
                'external_inputs': {'runtime_tools': {'path': '/frozen/runtime-tools'}},
                'cohorts': qualification.cohorts(), 'environment': resources,
                'source_oid': 'a' * 40, 'harness_revision': 'b' * 40,
                'profile': 'production', 'stdlib_mode': 'catalog-backed',
            }

            def execute(command, **kwargs):
                declared = [command[index + 1] for index, value in enumerate(command)
                            if value == '--resource-env']
                self.assertEqual(set(declared), set(resources))
                with patch.dict(os.environ, kwargs['env'], clear=True):
                    runner.resolve_resource_environment(declared)
                    delegated, _ = runner.delegated_command(
                        ['/frozen/libtest'], 1800, 'app.slice', {},
                        environment=os.environ, declared_resources=declared)
                    for name, path in resources.items():
                        self.assertIn(f'--setenv={name}={path}', delegated)
                    self.assertFalse(any('ambient-endpoint' in value for value in delegated))
                tests = root / 'evidence/tests'
                tests.mkdir()
                for index, test in enumerate(qualification.PREPARED_CHILD_TESTS):
                    qualification.write_json(tests / f'case-{index}.json', {
                        'test': test, 'passed': True,
                        'execution': {'executed_test_count': 1, 'exit_code': 0}})
                return subprocess.CompletedProcess(command, 0)

            with patch.object(qualification, 'verify', return_value=descriptor), \
                 patch.object(qualification.subprocess, 'run', side_effect=execute), \
                 patch.dict(os.environ, {
                     'TIDEPOOL_COMPILER_MODULES': '/ambient-catalog',
                     'TIDEPOOL_PREPARED_ROOT_ENTRY': '/ambient-entry',
                     'TIDEPOOL_EXTRACT_DAEMON_SOCKET': '/ambient-endpoint'}):
                self.assertEqual(qualification.main([
                    'run', str(descriptor_path), '--cohort', 'prepared-child',
                    '--output', str(root / 'evidence'), '--delegated-service']), 0)

    def test_prepared_child_cohort_selects_frozen_native_case_and_refuses_incomplete_execution(self):
        outcomes = [('complete', 1, True, 0), ('missing-default', 1, True, 1), ('empty', 0, True, 1),
                    ('unknown', None, True, 1), ('failed', 1, False, 1)]
        for label, count, passed, expected in outcomes:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                descriptor_path = root / 'qualification.json'
                descriptor_path.write_text('frozen descriptor')
                descriptor = {
                    'programs': {'runner': '/frozen/runner', 'libtest': '/frozen/libtest'},
                    'external_inputs': {'runtime_tools': {'path': '/frozen/runtime-tools'}},
                    'cohorts': qualification.cohorts(), 'environment': {},
                    'source_oid': 'a' * 40, 'harness_revision': 'b' * 40,
                    'profile': 'production', 'stdlib_mode': 'catalog-backed',
                }
                output = root / 'evidence'
                commands = []
                def execute(command, **kwargs):
                    commands.append(command)
                    selected = qualification.PREPARED_CHILD_TESTS
                    if label == 'missing-default':
                        selected = selected[:1]
                    for index, test in enumerate(selected):
                        qualification.write_json(output / f'tests/case-{index}.json', {
                            'test': test, 'passed': passed,
                            'execution': {'executed_test_count': count, 'exit_code': 0 if passed else 101}})
                    return subprocess.CompletedProcess(command, 0)
                with patch.object(qualification, 'verify', return_value=descriptor), \
                     patch.object(qualification.subprocess, 'run', side_effect=execute):
                    code = qualification.main(['run', str(descriptor_path), '--cohort', 'prepared-child',
                                               '--output', str(output)])
                self.assertEqual(code, expected)
                report = json.loads((output / 'report.json').read_text())
                self.assertEqual(report['completed'], expected == 0)
                self.assertEqual(report['expected_count'], len(qualification.PREPARED_CHILD_TESTS))
                command = commands[0]
                self.assertEqual(command[2], '/frozen/libtest')
                self.assertEqual(command[command.index('--compiler-mode') + 1], 'owned-resident')
                self.assertEqual(command[command.index('--expected-count') + 1],
                                 str(len(qualification.PREPARED_CHILD_TESTS)))
                self.assertEqual(command[command.index('--timeout') + 1], '1800')
                self.assertIn('--ignored', command)
                self.assertEqual([command[index + 1] for index, value in enumerate(command)
                                  if value == '--exact'], qualification.PREPARED_CHILD_TESTS)

    def test_prepared_child_cohort_refuses_other_compiler_mode_and_parallel_execution(self):
        for options in (['--compiler-mode', 'direct'], ['--jobs', '2']):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                descriptor = {'external_inputs': {'runtime_tools': {'path': '/frozen/runtime-tools'}},
                              'cohorts': qualification.cohorts()}
                output = root / 'evidence'
                with patch.object(qualification, 'verify', return_value=descriptor), \
                     patch.object(qualification.subprocess, 'run') as execute, \
                     contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(qualification.main(['run', str(root / 'qualification.json'),
                        '--cohort', 'prepared-child', '--output', str(output), *options]), 1)
                execute.assert_not_called()
                self.assertFalse(output.exists())

    def test_harness_performance_seals_resources_and_keeps_behavior_separate_from_measurement(self):
        for label, count, diagnostics, retained, expected in (
                ('complete', 1, True, True, 0),
                ('missing-phase-artifact', 1, True, True, 0),
                ('unjoined-physical-request', 1, True, True, 0),
                ('missing-timing', 1, False, True, 0),
                ('unknown-timing', 1, None, True, 0),
                ('discarded-artifacts', 1, True, False, 0),
                ('empty', 0, True, True, 1)):
            with self.subTest(outcome=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / 'qualification.json'
                path.write_text('frozen descriptor')
                descriptor = {
                    'programs': {'runner': '/frozen/runner', 'libtest': '/frozen/libtest'},
                    'external_inputs': {'runtime_tools': {'path': '/frozen/runtime-tools'}},
                    'cohorts': qualification.cohorts(),
                    'environment': {'TIDEPOOL_COMPILER_MODULES': '/frozen/catalog/catalog.json',
                                    'TIDEPOOL_PREPARED_ROOT_ENTRY': '/frozen/root-entry'},
                    'source_oid': 'a' * 40, 'harness_revision': 'b' * 40,
                    'profile': 'production', 'stdlib_mode': 'catalog-backed', 'startup_mode': 'prepared',
                }
                output = root / 'evidence'

                def execute(command, **kwargs):
                    self.assertIn('--retain-artifacts', command)
                    self.assertIn('--ignored', command)
                    self.assertEqual(command[command.index('--compiler-mode') + 1], 'owned-resident')
                    self.assertEqual([command[index + 1] for index, value in enumerate(command)
                                      if value == '--exact'], qualification.HARNESS_PERFORMANCE_TESTS)
                    self.assertEqual({command[index + 1] for index, value in enumerate(command)
                                      if value == '--resource-env'}, set(descriptor['environment']))
                    qualification.write_json(output / 'tests/case.json', {
                        'test': qualification.HARNESS_PERFORMANCE_TESTS[0], 'passed': True,
                        'execution': {'artifact_root': str(root / 'case/artifacts'),
                                      'executed_test_count': count, 'exit_code': 0,
                                      'diagnostic_evidence_complete': diagnostics,
                                      'artifacts_retained_after_success': retained}})
                    return subprocess.CompletedProcess(command, 0)

                def measurement(*args):
                    records = args[1]
                    paths = args[2]
                    return {'status': 'partial' if records else 'refused', 'completed': False,
                            'reporter': qualification.HARNESS_PERFORMANCE_REPORTER,
                            'runner_record': str(paths[0]) if paths else None,
                            'qualification': {'prerequisites': {
                                'artifacts_retained': retained, 'runner_passed': count == 1}}}

                with patch.object(qualification, 'verify', return_value=descriptor), \
                     patch.object(qualification.subprocess, 'run', side_effect=execute), \
                     patch.object(qualification, 'analyze_harness_performance', side_effect=measurement) as analyze:
                    code = qualification.main(['run', str(path), '--cohort', 'harness-performance',
                                               '--output', str(output)])
                self.assertEqual(code, expected)
                report = json.loads((output / 'report.json').read_text())
                self.assertEqual(report['behavioral_completed'], count == 1)
                self.assertFalse(report['measurement']['completed'])
                self.assertEqual(report['measurement']['status'], 'partial')
                self.assertEqual(report['measurement']['reporter'], qualification.HARNESS_PERFORMANCE_REPORTER)
                if count:
                    analyze.assert_called_once()
                    self.assertEqual(report['measurement']['runner_record'], str(output / 'tests/case.json'))
                self.assertEqual(report['completed'], expected == 0)
                self.assertTrue(report['tests'][0]['passed'])

    def test_harness_performance_refuses_unprepared_inputs_and_parallel_execution(self):
        mutations = (
            ('source-backed', {'stdlib_mode': 'source-backed'}, None, []),
            ('unprepared', {'startup_mode': 'unprepared'}, None, []),
            ('missing-catalog', {}, 'TIDEPOOL_COMPILER_MODULES', []),
            ('missing-entry', {}, 'TIDEPOOL_PREPARED_ROOT_ENTRY', []),
            ('parallel', {}, None, ['--jobs', '2']),
        )
        for label, changed, missing, options in mutations:
            with self.subTest(input=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                descriptor = {
                    'external_inputs': {'runtime_tools': {'path': '/frozen/runtime-tools'}},
                    'cohorts': qualification.cohorts(), 'stdlib_mode': 'catalog-backed',
                    'startup_mode': 'prepared',
                    'environment': {'TIDEPOOL_COMPILER_MODULES': '/frozen/catalog.json',
                                    'TIDEPOOL_PREPARED_ROOT_ENTRY': '/frozen/root-entry'},
                    **changed,
                }
                if missing is not None:
                    descriptor['environment'].pop(missing)
                output = root / 'evidence'
                with patch.object(qualification, 'verify', return_value=descriptor), \
                     patch.object(qualification.subprocess, 'run') as execute, \
                     contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(qualification.main([
                        'run', str(root / 'qualification.json'), '--cohort', 'harness-performance',
                        '--output', str(output), *options]), 1)
                execute.assert_not_called()
                self.assertFalse(output.exists())

    def test_frozen_harness_reporter_qualifies_only_complete_descriptor_bound_measurement(self):
        import shutil
        from scripts.tests.test_harness_usecase_perf_report import HarnessUsecasePerfReportTests, reporter

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = HarnessUsecasePerfReportTests()
            fixture.root = root / 'case'
            fixture.root.mkdir()
            runner_path = fixture.make_case()
            artifact_root = fixture.root / 'artifacts'
            phase_path = artifact_root / 'phases.jsonl'
            phases = reporter.read_jsonl(phase_path)
            env = phases[0]
            env['prepared_root_entry_supplied'] = True
            env['deployment']['TIDEPOOL_PREPARED_ROOT_ENTRY'] = '/frozen/root-entry'
            env['deployment']['TIDEPOOL_COMPILER_MODULES'] = '/frozen/catalog/catalog.json'
            env['deployment']['TIDEPOOL_COMPILER_DEPLOYMENT'] = '/frozen/compiler/compiler-deployment.json'
            phases[1]['logical_compiler_requests'] = 0
            workload = []
            for sequence, name in enumerate(reporter.EXPECTED_PHASES):
                workload.append({
                    'schema': 1, 'phase': name, 'sequence': sequence, 'completed': True,
                    'call_id': 'usecase-3' if sequence == 3 else f'usecase-{sequence}',
                    'logical_compiler_requests': 1 if sequence == 3 else 0,
                    'wall_ns': 1000, 'source': 'retained source',
                })
            phases = [env, phases[1], *workload]
            phase_path.write_text(''.join(json.dumps(row) + '\n' for row in phases))
            record = json.loads(runner_path.read_text())
            record['execution'].update({
                'exit_code': 0,
                'executed_test_count': 1,
                'process_cleanup_status': 'confirmed', 'hosted_cleanup_status': 'confirmed',
                'compiler_cleanup_status': 'confirmed', 'compiler_cleanup_observation_complete': True,
                'artifacts_retained_after_success': True,
            })
            runner_path.write_text(json.dumps(record) + '\n')
            descriptor_path = root / 'qualification.json'
            descriptor_path.write_text('verified by run_cohort')
            bundle = root / 'bundle'
            reporter_path = bundle / 'share/exomonad/harness-usecase-perf-report.py'
            reporter_path.parent.mkdir(parents=True)
            source_reporter = Path(__file__).resolve().parents[1] / 'harness-usecase-perf-report.py'
            shutil.copy2(source_reporter, reporter_path)
            descriptor = {
                'bundle_root': str(bundle), 'source_oid': 'a' * 40,
                'harness_revision': 'b' * 40, 'profile': 'production',
                'stdlib_mode': 'catalog-backed', 'startup_mode': 'prepared',
                'environment': {'TIDEPOOL_PREPARED_ROOT_ENTRY': '/frozen/root-entry',
                                'TIDEPOOL_COMPILER_MODULES': '/frozen/catalog/catalog.json',
                                'TIDEPOOL_COMPILER_DEPLOYMENT': '/frozen/compiler/compiler-deployment.json'},
            }
            measured = qualification.analyze_harness_performance(
                descriptor, [record], [runner_path],
                qualification.cohorts()['harness-performance'],
                True, descriptor_path)
            self.assertEqual(measured['status'], 'qualified')
            self.assertTrue(measured['completed'])
            self.assertTrue(measured['qualification']['prerequisites']['prepared_frozen_root_entry_matches_descriptor'])
            self.assertTrue(measured['qualification']['prerequisites']['startup_owner_complete'])
            self.assertTrue(measured['whole_physical_stream_reconciliation']['detailed_compiler_sample_records_truncated'])

            descriptor['environment']['TIDEPOOL_PREPARED_ROOT_ENTRY'] = '/different/root-entry'
            refused_match = qualification.analyze_harness_performance(
                descriptor, [record], [runner_path],
                qualification.cohorts()['harness-performance'],
                True, descriptor_path)
            self.assertEqual(refused_match['status'], 'partial')
            self.assertFalse(refused_match['qualification']['prerequisites']['prepared_frozen_root_entry_matches_descriptor'])

    def test_measurement_mutants_cannot_qualify_a_passing_frozen_case(self):
        from scripts.tests.test_harness_usecase_perf_report import HarnessUsecasePerfReportTests, reporter
        mutations = {
            "control": None,
            "short_unknown_roster": "workload_contract_matches_selected_cohort",
            "missing_wall": "phase_measurements_complete",
            "negative_wall": "phase_measurements_complete",
            "malformed_duplicate_queue": "queue_evidence_complete",
            "conflicting_owners": "workload_request_service_joins_complete",
            "wrong_compiler_mode": "resident_compiler_mode_matches_cohort",
        }
        for mutation, refused_dimension in mutations.items():
            with self.subTest(mutation=mutation):
                fixture = HarnessUsecasePerfReportTests()
                fixture.setUp()
                self.addCleanup(fixture.tearDown)
                runner_path = fixture.make_complete_case()
                phase_path = fixture.root / "artifacts/phases.jsonl"
                phases = reporter.read_jsonl(phase_path)
                if mutation == "short_unknown_roster":
                    phases[0].update(workload_cohort="typo-cohort", workload_roster=["reuse-retained"])
                    only_phase = next(phase for phase in phases if phase["phase"] == "reuse-retained")
                    only_phase["sequence"] = 0
                    phases = [*phases[:2], only_phase]
                elif mutation in ("missing_wall", "negative_wall"):
                    for phase in phases[1:]:
                        if mutation == "missing_wall":
                            phase.pop("wall_ns", None)
                        else:
                            phase["wall_ns"] = -1
                elif mutation == "malformed_duplicate_queue":
                    compiler_path = fixture.root / "artifacts/compiler/compiler.jsonl"
                    daemon = reporter.read_jsonl(compiler_path)
                    duplicate = json.loads(json.dumps(daemon[-1]))
                    duplicate["fields"].pop("queue_ms")
                    fixture.write_jsonl(compiler_path, [*daemon, duplicate])
                elif mutation == "conflicting_owners":
                    next(phase for phase in phases if phase["phase"] == "first-arithmetic")["logical_compiler_requests"] = 1
                    host_path = fixture.root / "artifacts/host.jsonl"
                    host = reporter.read_jsonl(host_path)
                    host[-1]["fields"]["workload_phase"] = "first-arithmetic"
                    fixture.write_jsonl(host_path, host)
                fixture.write_jsonl(phase_path, phases)
                record = json.loads(runner_path.read_text())
                if mutation == "wrong_compiler_mode":
                    record["execution"]["compiler_mode"] = "direct"
                    fixture.write_json(runner_path, record)
                bundle = fixture.root / "bundle"
                reporter_path = bundle / "share/exomonad/harness-usecase-perf-report.py"
                reporter_path.parent.mkdir(parents=True)
                shutil.copy2(Path(__file__).resolve().parents[1] / "harness-usecase-perf-report.py", reporter_path)
                descriptor_path = fixture.root / "qualification.json"
                descriptor_path.write_text("verified by run_cohort")
                descriptor = {
                    "bundle_root": str(bundle), "source_oid": "a" * 40,
                    "harness_revision": "b" * 40, "profile": "production",
                    "stdlib_mode": "catalog-backed", "startup_mode": "prepared",
                    "environment": phases[0]["deployment"],
                }
                measured = qualification.analyze_harness_performance(
                    descriptor, [record], [runner_path], qualification.cohorts()["harness-performance"],
                    True, descriptor_path)
                self.assertEqual(measured["completed"], mutation == "control")
                self.assertTrue(measured["qualification"]["prerequisites"]["counted_behavior_passed"])
                if refused_dimension:
                    self.assertFalse(measured["qualification"]["prerequisites"][refused_dimension])
                    self.assertEqual(measured["status"], "partial")

    def test_frozen_cohort_refuses_diagnostic_startup_override_before_execution(self):
        with patch.dict(os.environ, {'TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS': '600'}):
            with patch.object(qualification, 'verify') as verify:
                with self.assertRaisesRegex(ValueError, 'diagnostic startup overrides'):
                    qualification.run_cohort(SimpleNamespace())
                verify.assert_not_called()

    def test_frozen_cohort_rejects_otherwise_passing_diagnostic_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            descriptor_path = root / 'descriptor.json'
            descriptor_path.write_text('{}')
            descriptor = {
                'external_inputs': {'runtime_tools': {'path': str(root / 'tools')}},
                'cohorts': {'m2': {'tests': ['suite::works'], 'expected_count': 1,
                                  'timeout': 900, 'ignored': False,
                                  'compiler_mode': 'owned-resident'}},
                'programs': {'runner': '/declared/runner', 'libtest': '/declared/libtest'},
                'source_oid': 'recorded-source', 'harness_revision': 'recorded-harness',
                'profile': 'recorded-profile', 'stdlib_mode': 'source-backed',
                'environment': {},
            }
            for label, marker, expected in [('standard', None, 0), ('diagnostic', '600', 1)]:
                with self.subTest(label=label):
                    output = root / label
                    args = SimpleNamespace(jobs=1, service_slice=None, delegated_service=False,
                                           descriptor=descriptor_path, cohort='m2', output=output)
                    execution = {'executed_test_count': 1, 'exit_code': 0}
                    if marker is not None:
                        execution['startup_diagnostic_seconds'] = marker
                    receipt = {'test': 'suite::works', 'passed': True, 'execution': execution}
                    def run(command, **kwargs):
                        tests = output / 'tests'
                        tests.mkdir()
                        (tests / 'case.json').write_text(json.dumps(receipt))
                        return subprocess.CompletedProcess(command, 0)
                    with patch.dict(os.environ, {}, clear=True), \
                         patch.object(qualification, 'verify', return_value=descriptor), \
                         patch.object(qualification.subprocess, 'run', side_effect=run):
                        code = qualification.run_cohort(args)
                    report = json.loads((output / 'report.json').read_text())
                    self.assertEqual(code, expected)
                    self.assertEqual(report['completed'], expected == 0)
                    self.assertEqual(report['runner_exit_code'], 0)
                    self.assertEqual(report['executed_test_count'], 1)
                    self.assertTrue(report['tests'][0]['passed'])

    def test_runtime_tool_owner_checks_declared_executable_files(self):
        for name in ('nix-store', 'rg', 'find'):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                tools = runtime_tools_fixture(root)
                with patch.object(qualification, 'nix_path', side_effect=lambda path: path.resolve(strict=True)):
                    self.assertEqual(qualification.native_runtime_tools(tools), tools)
                    required = tools / 'bin' / name
                    required.unlink()
                    with self.assertRaisesRegex(ValueError, f'lack executable {name}'):
                        qualification.native_runtime_tools(tools)
                    required.mkdir()
                    with self.assertRaisesRegex(ValueError, f'lack executable {name}'):
                        qualification.native_runtime_tools(tools)

    def test_missing_or_nonexecutable_python_refuses_assembly_freeze_and_environment_before_work(self):
        for stage in ('assemble', 'freeze', 'environment'):
            for condition in ('missing', 'nonexecutable'):
                with self.subTest(stage=stage, condition=condition), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    tools = runtime_tools_fixture(root)
                    python = tools / 'bin/python3'
                    if condition == 'missing':
                        python.unlink()
                    else:
                        python.chmod(0o644)
                    bundle = root / 'bundle'
                    (bundle / 'share/exomonad').mkdir(parents=True)
                    (bundle / 'share/exomonad/runtime-tools').symlink_to(tools)
                    output = root / 'assembled'
                    args = SimpleNamespace(runtime_tools=tools, ghc_libdir=tools, output=output, bundle=bundle)
                    with patch.object(qualification, 'nix_path', side_effect=lambda path: path.resolve(strict=True)), \
                         patch.object(qualification.subprocess, 'run') as execute, \
                         patch.object(qualification.subprocess, 'check_output') as query:
                        with self.assertRaisesRegex(ValueError, 'lack executable python3'):
                            if stage == 'assemble':
                                qualification.assemble(args)
                            elif stage == 'freeze':
                                qualification.freeze(args)
                            else:
                                qualification.native_environment(bundle)
                    self.assertFalse(output.exists())
                    execute.assert_not_called()
                    query.assert_not_called()

    def test_run_forwards_scheduling_and_sealed_watchdogs_and_records_them(self):
        for compiler_mode in (None, 'owned-resident'):
            with self.subTest(compiler_mode=compiler_mode):
                self.assert_run_scheduling(compiler_mode)

    def assert_run_scheduling(self, compiler_mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            descriptor_path = root / 'qualification.json'
            descriptor_path.write_text('sealed descriptor')
            descriptor = {'programs': {'runner': '/frozen/runner', 'libtest': '/frozen/libtest'},
                          'cohorts': qualification.cohorts(),
                          'environment': {'PATH': '/frozen/runtime-tools/bin'},
                          'external_inputs': {'runtime_tools': {'path': '/frozen/runtime-tools'}},
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
                self.assertEqual(command[0], '/frozen/runtime-tools/bin/python3')
                self.assertEqual(kwargs['env']['PATH'], '/frozen/runtime-tools/bin')
                return subprocess.CompletedProcess(command, 0)

            with patch.object(qualification, 'verify', return_value=descriptor), \
                 patch.object(qualification.sys, 'executable', '/ambient/unqualified-python'), \
                 patch.dict(os.environ, {'PATH': '/ambient/unqualified-tools'}), \
                 patch.object(qualification.subprocess, 'run', side_effect=execute):
                arguments = ['run', str(descriptor_path), '--cohort', 'm2',
                    '--output', str(root / 'evidence'), '--jobs', '4', '--delegated-service',
                    '--service-slice', 'tidepool-completion-build.slice']
                if compiler_mode is not None:
                    arguments.extend(['--compiler-mode', compiler_mode])
                code = qualification.main(arguments)
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
                'service_slice': 'tidepool-completion-build.slice', 'compiler_mode': 'owned-resident', 'timeout_seconds': 600,
                'case_timeout_seconds': {name: 900 for name in [qualification.M2_SURVIVAL_TEST, qualification.M2_NOMINAL_JOIN_TEST, qualification.M2_CHECKPOINT_RELEASE_TEST, qualification.M2_SELECTED_CODING_TEST]}})
            self.assertEqual(command[command.index('--compiler-mode') + 1], 'owned-resident')
            self.assertEqual(report['executed_test_count'], len(descriptor['cohorts']['m2']['tests']))
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
            inputs = browser_driver_fixture(source, bundle / qualification.BROWSER_DRIVER_ROOT)
            subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
            commit = ['git', '-C', str(source), '-c', 'user.name=Qualification test',
                      '-c', 'user.email=qualification@example.invalid', 'commit', '-qm']
            subprocess.run([*commit, 'first revision'], check=True)
            inputs['native.rs'] = qualification.sha256(source / 'native.rs')
            contract = {'profile': 'fast-dev', 'startup_mode': 'unprepared', 'source_inputs': inputs,
                        'source_inputs_sha256': qualification.digest_inventory(inputs), 'artifacts': {},
                        'browser_driver': qualification.browser_driver_record(
                            bundle / qualification.BROWSER_DRIVER_ROOT, inputs)}
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
            generated = root_entry_source_fixture(original, {'Library': 'lib/Library.hs', 'Actor': 'actors/Actor.hs'})
            evidence = qualification.declared_haskell_sources(source, bundle, original, generated)
            self.assertEqual(set(evidence), {'stdlib', 'actors'})
            self.assertFalse((bundle / 'share/exomonad/stdlib').exists())
            (original / 'actors/Actor.hs').write_text('changed actor')
            with self.assertRaisesRegex(ValueError, 'tracked declared Haskell source bytes'):
                qualification.declared_haskell_sources(source, bundle, original, generated)

    def test_generated_driver_requires_declared_bytes_and_rejects_missing_contract_or_extra_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, original = root / 'source', root / 'original'
            source.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            for relative in ('lib/Library.hs', 'actors/Actor.hs'):
                tracked = source / 'bridge/haskell' / relative
                retained = original / relative
                tracked.parent.mkdir(parents=True, exist_ok=True)
                retained.parent.mkdir(parents=True, exist_ok=True)
                tracked.write_text('source ' + relative)
                retained.write_bytes(tracked.read_bytes())
            subprocess.run(['git', '-C', str(source), 'add', 'bridge'], check=True)
            generated = root_entry_source_fixture(original, {'Library': 'lib/Library.hs', 'Actor': 'actors/Actor.hs'})
            evidence = qualification.declared_haskell_sources(source, root, original, generated)
            self.assertEqual(evidence['actors'], {'Actor.hs': qualification.sha256(original / 'actors/Actor.hs')})
            self.assertEqual(generated['path'], 'TidepoolPreparedDriver.hs')
            self.assertEqual(qualification.catalog_source_inventory(original)[generated['path']]['sha256'],
                             generated['sha256'])
            self.assertFalse((original / 'actors/TidepoolPreparedDriver.hs').exists())
            with self.assertRaisesRegex(ValueError, 'absent from the owning native build contract'):
                qualification.declared_haskell_sources(source, root, original)
            extra = original / 'actors/Unexpected.hs'
            extra.write_text('module Unexpected where\n')
            with self.assertRaisesRegex(ValueError, 'tracked declared Haskell source bytes'):
                qualification.declared_haskell_sources(source, root, original, generated)
            extra.unlink()
            (original / qualification.ROOT_ENTRY_SOURCE).write_text('module DifferentDriver where\n')
            with self.assertRaisesRegex(ValueError, 'declared generated source bytes'):
                qualification.declared_haskell_sources(source, root, original, generated)

    def test_runtime_environment_rejects_ambient_catalog_and_daemon_selection(self):
        with patch.dict(os.environ, {
            'TIDEPOOL_EXTRACT_DAEMON_SOCKET': '/tmp/unqualified.sock',
            'TIDEPOOL_COMPILER_MODULES': '/nix/store/older-project/catalog.json',
            'TIDEPOOL_EXTRACT_NO_DAEMON': '1',
            'TIDEPOOL_EXTRACT': '/tmp/older-extract',
            'TIDEPOOL_TEST_SYSTEMD_RUN': '/tmp/hostile-systemd-run',
            'TIDEPOOL_TEST_SYSTEMCTL': '/tmp/hostile-systemctl',
            'TIDEPOOL_TEST_FIXTURE_ROOT': '/tmp/source-fixtures',
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
        for key in ('TIDEPOOL_EXTRACT_DAEMON_SOCKET', 'TIDEPOOL_COMPILER_MODULES', 'TIDEPOOL_EXTRACT_NO_DAEMON', 'EXOMONAD_WORKSPACE_GITLINK', 'EXOMONAD_NIX_OFFLINE', 'TIDEPOOL_TEST_SYSTEMD_RUN', 'TIDEPOOL_TEST_SYSTEMCTL', 'TIDEPOOL_TEST_FIXTURE_ROOT'):
            self.assertNotIn(key, environment)

    def test_frozen_bytes_may_not_change_and_descriptor_may_not_move(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'share/exomonad').mkdir(parents=True)
            binary = root / 'host'
            binary.write_bytes(b'original-host')
            descriptor_path = root / qualification.DESCRIPTOR
            descriptor = {
                'schema': qualification.QUALIFICATION_SCHEMA, 'kind': 'native-runtime-qualification',
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
        self.root_entry_source = self.root / 'TidepoolPreparedDriver.hs'
        self.root_entry_source.write_text('module TidepoolPreparedDriver where\n')
        self.args = SimpleNamespace(sources=self.sources, effects=self.effects,
                                    cohort=self.cohort, output=self.snapshot, jev_sources=self.jev_sources,
                                    root_entry_source=self.root_entry_source)

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
        self.assertEqual((self.snapshot / qualification.ROOT_ENTRY_SOURCE).read_bytes(),
                         self.root_entry_source.read_bytes())
        for relative in ('lib', 'actors'):
            inventories = [
                {path.relative_to(root).as_posix(): qualification.sha256(path)
                 for path in root.rglob('*') if path.is_file()}
                for root in (self.snapshot / relative, self.sources / relative)]
            self.assertEqual(*inventories)
        self.assertFalse((self.snapshot / 'actors/TidepoolPreparedDriver.hs').exists())
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
        tools = runtime_tools_fixture(self.root)
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
                'source_files': [{'path': relative, 'sha256': item['sha256']} for relative, item in
                    sorted(qualification.catalog_source_inventory(original).items())
                    if item['kind'] == 'file' and relative.endswith(('.hs', '.hs-boot', '.lhs', '.lhs-boot'))]}

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
        entry = shared / 'root-entry'
        qualification.write_json(entry / 'entry.json', {
            'schema': 2, 'purpose': 'original_source', 'target': '__prepared',
            'source': str(original / qualification.ROOT_ENTRY_SOURCE),
            'sources': {'kind': 'native_catalog', 'selection': self.selection(original)},
            'producer': [3] * 32, 'worker': [4] * 32, 'files': {}})
        shutil.copy2(record, entry / 'source-retention.json')
        entry_products = qualification.native_catalog_products(entry, qualification.NATIVE_ROOT_ENTRY_BUILD)
        selected_entry = {'manifest_sha256': qualification.sha256(entry / 'entry.json'),
                          'source_selection': self.selection(original),
                          'source_inventory_sha256': selected['source_inventory_sha256'],
                          'product_inventory_sha256': qualification.digest_inventory(entry_products)}
        qualification.write_json(entry / qualification.NATIVE_ROOT_ENTRY_BUILD, {
            'schema': 1, 'kind': 'native-root-entry-build',
            'producer_target': '//tidepool/toolchain:tidepool-module-package',
            'product_inventory': entry_products, **selected_entry})
        contract = {'stdlib_mode': 'catalog-backed', 'startup_mode': 'prepared', 'native_catalog': selected,
                    'native_root_entry': selected_entry,
                    'generated_root_source': qualification.catalog_source_metadata(original)['root_entry']}
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
            altered['source_selection']['source_files'] = [
                [file['path'], file['sha256']]
                for file in altered['source_selection']['source_files']]
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

    def test_root_entry_requires_schema_two_typed_source_selection(self):
        original, tools, record, _, _ = self.retained_fixture()
        bundle, _, _ = self.catalog_fixture(original, tools, record)
        path = bundle / 'share/exomonad/root-entry/entry.json'
        self.assertEqual(qualification.root_entry_selection(path, original), self.selection(original))
        manifest = json.loads(path.read_text())
        manifest['source'] = str(original / 'actors/TidepoolPreparedDriver.hs')
        qualification.write_json(path, manifest)
        with self.assertRaisesRegex(ValueError, 'declared original settled driver'):
            qualification.root_entry_selection(path, original)
        manifest['source'] = str(original / qualification.ROOT_ENTRY_SOURCE)
        manifest['schema'] = 1
        qualification.write_json(path, manifest)
        with self.assertRaisesRegex(ValueError, 'declared original settled driver'):
            qualification.root_entry_selection(path, original)

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

    def test_catalog_refuses_wrong_source_digest_even_when_bundle_envelopes_match(self):
        original, tools, record, pin, nar = self.retained_fixture()
        bundle, contract, catalog = self.catalog_fixture(original, tools, record)
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run'):
            self.assertEqual(qualification.verify_native_catalog(bundle, tools), contract['native_catalog'])
            altered = json.loads(catalog.read_text())
            file = altered['source_selection']['source_files'][0]
            file['sha256'] = ('0' if file['sha256'][0] != '0' else '1') + file['sha256'][1:]
            qualification.write_json(catalog, altered)
            products = qualification.native_catalog_products(catalog.parent)
            selected = {**contract['native_catalog'],
                        'source_selection': altered['source_selection'],
                        'catalog_sha256': qualification.sha256(catalog),
                        'product_inventory_sha256': qualification.digest_inventory(products)}
            receipt_path = catalog.parent / qualification.NATIVE_CATALOG_BUILD
            receipt = json.loads(receipt_path.read_text())
            receipt.update(selected, product_inventory=products)
            qualification.write_json(receipt_path, receipt)
            contract['native_catalog'] = selected
            qualification.write_json(bundle / 'share/exomonad/native-build-contract.json', contract)
            with self.assertRaisesRegex(ValueError, 'complete source manifest'):
                qualification.native_catalog_selection(catalog, original)
            with self.assertRaisesRegex(ValueError, 'complete source manifest'):
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

    def test_root_entry_producer_selects_retained_direct_target_outside_library_roots(self):
        original, tools, record, pin, nar = self.retained_fixture()
        args = self.producer_args(original, tools, record, self.root / 'entry-products')
        commands = []
        def execute(command, **kwargs):
            if '--verify-path' not in command:
                commands.append(command)
            return subprocess.CompletedProcess(command, 0)
        with self.nix_checks(nar, [pin]), patch.object(qualification.subprocess, 'run', side_effect=execute):
            self.assertEqual(qualification.invoke_retained_catalog_producer(
                args, qualification.CatalogProducerOperation.ENTRY), original)
        target = original / qualification.ROOT_ENTRY_SOURCE
        self.assertEqual(commands, [[str(args.producer), 'entry', '--source', str(target),
            '--target', '__prepared', '--source-root', str(original), '--output-root', str(args.output)]])
        self.assertTrue(target.is_file())
        self.assertTrue(all(not target.is_relative_to(original / relative)
                            for relative in qualification.CATALOG_SOURCE_ROOTS.values()))
        self.assertIn(qualification.ROOT_ENTRY_SOURCE, qualification.catalog_source_inventory(original))

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
