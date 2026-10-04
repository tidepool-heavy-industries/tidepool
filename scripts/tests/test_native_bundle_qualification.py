"""Artifact mutation and runtime selection checks for native qualification."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'build/package/qualification.py'
SPEC = importlib.util.spec_from_file_location('native_qualification', SCRIPT)
qualification = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qualification)


class NativeQualificationTests(unittest.TestCase):
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
            with patch.object(qualification, 'loader_evidence', return_value={}):
                self.assertEqual(qualification.verify(descriptor_path)['bundle_root'], str(root))
                binary.write_bytes(b'different-host')
                with self.assertRaisesRegex(ValueError, 'inventory changed'):
                    qualification.verify(descriptor_path)
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
