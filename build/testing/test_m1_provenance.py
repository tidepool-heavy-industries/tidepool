"""Extractor-free rejection tests for the M1 acceptance admission boundary."""
import importlib.util
import json
import shutil
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('m1_provenance', Path(__file__).with_name('m1_provenance.py'))
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.root = self.base / 'repo'
        self.root.mkdir()
        self.record = self.base / 'record'
        self.record.mkdir()
        self.retain = self.base / 'retained'
        self.retain.mkdir()
        (self.root / 'bridge/haskell/src').mkdir(parents=True)
        (self.root / 'bridge/haskell/app').mkdir(parents=True)
        paths = [key for key in p.worker_roots(self.root) if key not in ('bridge/haskell/src', 'bridge/haskell/app')] + ['bridge/haskell/src/Worker.hs', 'bridge/haskell/app/Main.hs']
        rows = []
        for key in paths:
            current = self.root / key
            saved = self.record / 'sources' / key
            current.parent.mkdir(parents=True, exist_ok=True)
            saved.parent.mkdir(parents=True, exist_ok=True)
            current.write_text(key + '\n')
            saved.write_bytes(current.read_bytes())
            rows.append({'path': key, 'bytes': current.stat().st_size, 'sha256': p.digest(current)})
        manifest = self.record / 'sources.json'
        manifest.write_text(json.dumps({'head': 'a' * 40, 'files': rows}))
        self.packet = {'git_oid': 'a' * 40, 'all_haskell_source_hashes': 'sources.json',
                       'source_manifest_sha256': p.digest(manifest), 'all_haskell_source_bytes': 'sources'}
        self.frontend = self.base / 'frontend'
        self.worker = self.base / 'worker'
        self.frontend.write_bytes(b'frontend baseline')
        self.worker.write_bytes(b'worker binary')
        self.packet.update(frontend_sha256=p.digest(self.frontend), worker_sha256=p.digest(self.worker), build={'exit_status': 0})
        self.packet_path = self.record / 'packet.json'
        self.save_packet()

    def save_packet(self):
        self.packet_path.write_text(json.dumps(self.packet))
        return p.digest(self.packet_path)

    def validate_packet(self, expected=None):
        return p.validate_packet(self.packet_path, expected or p.digest(self.packet_path), self.frontend, self.worker,
                                 p.digest(self.frontend), p.digest(self.worker))

    def validate_sources(self):
        return p.validate_sources(self.root, self.record, self.packet, self.retain)

    def admitted_fixture(self):
        output = self.base / 'campaign'
        (output / 'frozen-bin').mkdir(parents=True)
        shutil.copyfile(self.frontend, output / 'frozen-bin/tidepool-extract')
        shutil.copyfile(self.worker, output / 'frozen-bin/tidepool-extract-bin')
        tools = {name: {'path': str(self.worker), 'sha256': p.digest(self.worker), 'version': 'fixture'}
                 for name in ('ghc', 'rustc', 'cabal')}
        self.packet['build'].update(flake='pinned-flake', tools=tools,
                                  tool_record_origin='post_build_same_pinned_shell_audit',
                                  log='build.log')
        (self.record / 'build.log').write_text('successful recorded build')
        self.packet['build']['log_sha256'] = p.digest(self.record / 'build.log')
        original = {'schema': 1, 'ghc_libdir': '/fixture/lib', 'producer_identity': [1] * 32,
                    'consumed_worker_identity': [2] * 32, 'frontend_path': str(self.frontend),
                    'worker_path': str(self.worker)}
        (self.record / 'deployment.json').write_text(json.dumps(original))
        self.packet.update(manifest='deployment.json', producer_identity='01' * 32,
                           consumed_worker_identity='02' * 32)
        node = self.base / 'node'
        node.write_text('node binary')
        browsers = self.base / 'browsers'
        browsers.mkdir()
        browser = browsers / 'chromium'
        browser.write_text('browser binary')
        dependencies = self.base / 'node_modules'
        package = dependencies / 'playwright-core/package.json'
        package.parent.mkdir(parents=True)
        package.write_text(json.dumps({'name': 'playwright-core', 'version': '1.0'}))
        assets = self.base / 'gui'
        assets.mkdir()
        (assets / 'index.html').write_text('GUI')
        lock = self.base / 'package-lock.json'
        lock.write_text(json.dumps({'packages': {'node_modules/playwright-core': {'version': '1.0'}}}))
        env = {'M1_PRODUCER_EVIDENCE': str(self.packet_path), 'M1_PRODUCER_EVIDENCE_SHA256': self.save_packet(),
               'M1_FRONTEND_SHA256': p.digest(self.frontend), 'M1_WORKER_SHA256': p.digest(self.worker),
               'TIDEPOOL_DEV_FLAKE': 'pinned-flake', 'TIDEPOOL_DEV_SHELL': 'pinned-flake#default',
               'TIDEPOOL_BROWSER_NODE': str(node), 'PLAYWRIGHT_BROWSERS_PATH': str(browsers),
               'M1_BROWSER_NODE_MODULES': str(dependencies), 'M1_BROWSER_PACKAGE_LOCK': str(lock),
               'EXOMONAD_EMBEDDED_ASSET_ROOT': str(assets), 'CARGO_TARGET_DIR': '/fixture/target'}
        real_run = p.subprocess.run
        def execute(args, **kwargs):
            if args[0] == 'bash':
                return real_run(args, **kwargs)
            self.assertEqual(args[1], '--compiler-deployment-manifest')
            manifest = dict(original, frontend_path=str(output / 'frozen-bin/tidepool-extract'),
                            worker_path=str(output / 'frozen-bin/tidepool-extract-bin'))
            Path(args[2]).write_text(json.dumps(manifest))
        def command(args):
            if args[-1] == '--print-libdir':
                return '/fixture/lib'
            if '-e' in args:
                return str(browser)
            return 'fixture version'
        return output, env, tools, execute, command

    def test_complete_admission_retains_and_audits_exact_evidence(self):
        output, env, tools, execute, command = self.admitted_fixture()
        with patch.object(p, 'active_tools', return_value=tools), \
             patch.object(p, 'immutable', side_effect=lambda path: str(Path(path).resolve())), \
             patch.object(p, 'command', side_effect=command), \
             patch.object(p.subprocess, 'run', side_effect=execute) as deployment:
            p.admit(self.root, output, env)
        self.assertEqual(sum(call.args[0][0] != 'bash' for call in deployment.call_args_list), 1)
        self.assertTrue(p.audit(output))
        self.assertTrue((output / 'producer-record/build.log').is_file())
        self.assertTrue((output / 'provenance-inputs.sha256').is_file())
        report = p.read_json(output / 'provenance-admission.json')
        self.assertIn('not independent binary reproduction', report['authority'])
        self.assertEqual(report['worker_sources']['files'], 10)

    def test_wrong_recorded_tool_refuses_before_deployment_or_compile(self):
        output, env, tools, execute, command = self.admitted_fixture()
        self.packet['build']['tools']['ghc']['sha256'] = '0' * 64
        env['M1_PRODUCER_EVIDENCE_SHA256'] = self.save_packet()
        tools = dict(tools, ghc=dict(tools['ghc'], sha256=p.digest(self.worker)))
        with patch.object(p, 'active_tools', return_value=tools), \
             patch.object(p.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'tool audit differs'):
                p.admit(self.root, output, env)
        run.assert_not_called()
        self.assertFalse((output / 'provenance-admission.json').exists())

    def binding_fixture(self):
        output = self.base / 'binding'
        retained = output / 'producer-record'
        retained.mkdir(parents=True)
        sources = p.validate_sources(self.root, self.record, self.packet, retained)
        report = {'worker_sources': sources, 'worker_source_owner_sha256': p.digest(p.worker_source_owner()),
                  'producer_packet_sha256': p.digest(self.packet_path)}
        (output / 'provenance-admission.json').write_text(json.dumps(report))
        (output / 'source-before').mkdir()
        return output, sources

    def capture_worker_subset(self, output, sources):
        entries = []
        keys = p.worker_inputs(self.root, sources['roots'])
        for key in sorted(keys):
            path = self.root / key
            entries.append({'path': key, 'kind': 'file', 'bytes': path.stat().st_size, 'sha256': p.digest(path)})
        entries.append({'path': 'scripts/lib-extract.sh', 'kind': 'file',
                        'bytes': p.worker_source_owner().stat().st_size, 'sha256': p.digest(p.worker_source_owner())})
        snapshot = {'version': 1, 'capture_changes': [], 'source': {'entries': entries}}
        (output / 'source-before/manifest.json').write_text(json.dumps(snapshot))
        return snapshot

    def test_snapshot_binding_uses_captured_baseline_and_retained_bytes(self):
        output, sources = self.binding_fixture()
        self.capture_worker_subset(output, sources)
        # Binding must inspect captured bytes rather than silently moving to live source.
        (self.root / 'bridge/haskell/src/Worker.hs').write_text('later live change')
        binding = p.bind_snapshot(self.root, output)
        self.assertEqual(binding['worker_inputs'], 10)
        self.assertTrue((output / 'worker-snapshot-binding.json').is_file())
        self.assertIn('worker-snapshot-binding.json', (output / 'provenance-inputs.sha256').read_text())

    def test_mutation_between_admission_and_capture_refuses(self):
        output, sources = self.binding_fixture()
        (self.root / 'bridge/haskell/src/Worker.hs').write_text('new stable compiler source')
        self.capture_worker_subset(output, sources)
        with self.assertRaisesRegex(ValueError, 'captured worker input differs'):
            p.bind_snapshot(self.root, output)
        self.assertFalse((output / 'worker-snapshot-binding.json').exists())

    def test_addition_between_admission_and_capture_refuses(self):
        output, sources = self.binding_fixture()
        (self.root / 'bridge/haskell/src/Added.hs').write_text('new compiled module')
        self.capture_worker_subset(output, sources)
        with self.assertRaisesRegex(ValueError, 'inventory differs'):
            p.bind_snapshot(self.root, output)

    def test_deleted_missing_or_symlink_captured_input_refuses(self):
        output, sources = self.binding_fixture()
        original = self.capture_worker_subset(output, sources)
        path = output / 'source-before/manifest.json'
        for kind in ('absent', 'missing', 'symlink'):
            snapshot = json.loads(json.dumps(original))
            rows = snapshot['source']['entries']
            if kind == 'absent':
                snapshot['source']['entries'] = [row for row in rows if row['path'] != 'bridge/haskell/src/Worker.hs']
            else:
                next(row for row in rows if row['path'] == 'bridge/haskell/src/Worker.hs')['kind'] = kind
            path.write_text(json.dumps(snapshot))
            with self.assertRaisesRegex(ValueError, 'captured worker input'):
                p.bind_snapshot(self.root, output)

    def test_changed_captured_enumerator_and_retained_input_refuse(self):
        output, sources = self.binding_fixture()
        snapshot = self.capture_worker_subset(output, sources)
        next(row for row in snapshot['source']['entries'] if row['path'] == 'scripts/lib-extract.sh')['sha256'] = '0' * 64
        (output / 'source-before/manifest.json').write_text(json.dumps(snapshot))
        with self.assertRaisesRegex(ValueError, 'captured worker source enumerator differs'):
            p.bind_snapshot(self.root, output)
        self.capture_worker_subset(output, sources)
        (output / 'producer-record/worker-source/bridge/haskell/src/Worker.hs').write_text('changed retained bytes')
        with self.assertRaisesRegex(ValueError, 'retained worker input changed'):
            p.bind_snapshot(self.root, output)

    def test_original_sources_and_frontend_baseline_are_independent(self):
        self.validate_packet()
        result = self.validate_sources()
        self.assertEqual(result['files'], 10)
        # A later Rust implementation cannot invalidate an explicitly frozen frontend.
        (self.root / 'new-rust-source.rs').write_text('later source')
        self.validate_packet()
        self.assertEqual(self.validate_sources(), result)

    def test_packet_substitution_and_binary_disagreement_refuse(self):
        admitted = self.save_packet()
        self.packet['build']['exit_status'] = 1
        self.save_packet()
        with self.assertRaisesRegex(ValueError, 'admitted packet'):
            self.validate_packet(admitted)
        with self.assertRaisesRegex(ValueError, 'did not succeed'):
            self.validate_packet()
        self.packet['build']['exit_status'] = 0
        self.save_packet()
        self.worker.write_bytes(b'different actual worker')
        with self.assertRaisesRegex(ValueError, 'worker digest'):
            self.validate_packet()

    def test_source_addition_deletion_and_change_refuse(self):
        source = self.root / 'bridge/haskell/src/Worker.hs'
        original = source.read_bytes()
        source.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'current worker source mismatch'):
            self.validate_sources()
        source.write_bytes(original)
        added = source.parent / 'Added.hs'
        added.write_text('new compiler input')
        with self.assertRaisesRegex(ValueError, 'inventory differs'):
            self.validate_sources()
        added.unlink()
        source.unlink()
        with self.assertRaisesRegex(ValueError, 'inventory differs'):
            self.validate_sources()

    def test_embedded_library_input_change_refuses_worker_admission(self):
        source = self.root / 'bridge/haskell/lib/Tidepool/Double.hs'
        source.write_text('changed compile-time embedded implementation')
        with self.assertRaisesRegex(ValueError, 'current worker source mismatch'):
            self.validate_sources()

    def test_inherited_compiler_and_wrapper_overrides_refuse(self):
        for key in ('RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC'):
            with self.assertRaisesRegex(ValueError, 'compiler override'):
                p.active_tools({key: '/different/compiler'})

    def test_retained_snapshot_tamper_and_manifest_substitution_refuse(self):
        retained = self.record / 'sources/bridge/haskell/src/Worker.hs'
        retained.write_bytes(b'wrong retained bytes')
        with self.assertRaisesRegex(ValueError, 'retained worker source mismatch'):
            self.validate_sources()
        (self.record / 'sources.json').write_text('{}')
        with self.assertRaisesRegex(ValueError, 'source manifest digest mismatch'):
            self.validate_sources()

    def test_unsafe_duplicate_and_symlink_source_paths_refuse(self):
        for value in ('../escape', '/absolute', 'a\\b'):
            with self.assertRaises(ValueError):
                p.relative(value)
        outside = self.base / 'outside'
        outside.mkdir()
        (self.record / 'escape').symlink_to(outside)
        with self.assertRaisesRegex(ValueError, 'escapes'):
            p.referenced(self.record, 'escape/file')
        manifest = self.record / 'sources.json'
        data = json.loads(manifest.read_text())
        data['files'].append(data['files'][0])
        manifest.write_text(json.dumps(data))
        self.packet['source_manifest_sha256'] = p.digest(manifest)
        with self.assertRaisesRegex(ValueError, 'duplicate producer source path'):
            self.validate_sources()

    def test_symlink_asset_root_inventories_served_bytes_and_additions(self):
        actual = self.base / 'actual-assets'
        actual.mkdir()
        (actual / 'index.html').write_text('page')
        linked = self.base / 'assets'
        linked.symlink_to(actual, target_is_directory=True)
        before = p.inventory(linked)
        self.assertEqual(before['entries'][0]['symlink'], str(actual))
        self.assertTrue(any(row.get('sha256') == p.digest(actual / 'index.html') for row in before['entries']))
        (actual / 'new.js').write_text('new asset')
        self.assertNotEqual(p.inventory(linked), before)
        (actual / 'new.js').unlink()
        (actual / 'index.html').write_text('changed page')
        self.assertNotEqual(p.inventory(linked), before)

    def test_symlink_served_external_bytes_and_retarget_are_visible(self):
        assets = self.base / 'assets'
        assets.mkdir()
        first, second = self.base / 'first', self.base / 'second'
        first.write_text('same bytes')
        second.write_text('same bytes')
        link = assets / 'served.js'
        link.symlink_to(first)
        before = p.inventory(assets)
        link.unlink()
        link.symlink_to(second)
        self.assertNotEqual(p.inventory(assets), before)
        second.write_text('changed externally served bytes')
        self.assertNotEqual(p.inventory(assets)['entries'][-1]['sha256'], before['entries'][-1]['sha256'])

    def test_empty_cyclic_and_over_budget_inventory_refuse(self):
        tree = self.base / 'tree'
        tree.mkdir()
        with self.assertRaisesRegex(ValueError, 'empty'):
            p.inventory(tree)
        (tree / 'loop').symlink_to(tree)
        with self.assertRaisesRegex(ValueError, 'cyclic'):
            p.inventory(tree)
        (tree / 'loop').unlink()
        (tree / 'file').write_text('payload')
        with patch.object(p, 'MAX_BYTES', 2):
            with self.assertRaisesRegex(ValueError, 'byte count'):
                p.inventory(tree)

    def test_forged_unpinned_or_mismatched_active_shell_refuse(self):
        with self.assertRaisesRegex(ValueError, 'immutable'):
            p.active_tools({'TIDEPOOL_DEV_FLAKE': 'path:.'})
        with self.assertRaisesRegex(ValueError, 'shell differs'):
            p.active_tools({'TIDEPOOL_DEV_FLAKE': 'git+file:///repo?rev=' + 'a' * 40,
                            'TIDEPOOL_DEV_SHELL': 'different#default'})
        with self.assertRaisesRegex(ValueError, 'immutable Nix'):
            p.immutable(self.worker)

    def test_oversized_and_duplicate_json_refuse(self):
        file = self.base / 'json'
        file.write_text('{"build":0,"build":1}')
        with self.assertRaisesRegex(ValueError, 'duplicate JSON'):
            p.read_json(file)
        with patch.object(p, 'MAX_JSON', 2):
            with self.assertRaisesRegex(ValueError, 'oversized JSON'):
                p.read_json(file)

    def test_post_audit_detects_added_assets_and_dependency_changes(self):
        asset = self.base / 'assets'
        dep = self.base / 'dependencies'
        for directory in (asset, dep):
            directory.mkdir()
            (directory / 'file').write_text('stable')
        report = {'assets': p.inventory(asset), 'dependencies': p.inventory(dep)}
        for key in ('node', 'browser', 'playwright_core', 'package_lock'):
            report[key] = {'path': str(self.worker), 'sha256': p.digest(self.worker)}
        (self.retain / 'provenance-admission.json').write_text(json.dumps(report))
        self.assertTrue(p.audit(self.retain))
        (dep / 'added').write_text('extra dependency')
        with self.assertRaisesRegex(ValueError, 'dependencies inventory changed'):
            p.audit(self.retain)


if __name__ == '__main__':
    unittest.main()
