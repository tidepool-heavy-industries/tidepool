"""Artifact mutation and runtime selection checks for native qualification."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'build/package/qualification.py'
SPEC = importlib.util.spec_from_file_location('native_qualification', SCRIPT)
qualification = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qualification)


class NativeQualificationTests(unittest.TestCase):
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
        }):
            environment = qualification.execution_environment({'environment': {
                'TIDEPOOL_EXTRACT': '/frozen/bin/tidepool-extract',
            }})
        self.assertEqual(environment['TIDEPOOL_EXTRACT'], '/frozen/bin/tidepool-extract')
        for key in ('TIDEPOOL_EXTRACT_DAEMON_SOCKET', 'TIDEPOOL_COMPILER_MODULES', 'TIDEPOOL_EXTRACT_NO_DAEMON'):
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


if __name__ == '__main__':
    unittest.main()
