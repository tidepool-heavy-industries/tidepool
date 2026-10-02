"""Acceptance provenance checks using isolated Git repositories."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('source_snapshot', Path(__file__).with_name('source_snapshot.py'))
snapshot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(snapshot)


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], stderr=subprocess.PIPE)


class SourceSnapshotTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.root = self.base / 'repo'
        self.root.mkdir()
        git(self.root, 'init', '--quiet')
        git(self.root, 'config', 'user.email', 'source-test@example.invalid')
        git(self.root, 'config', 'user.name', 'Source Test')
        (self.root / 'tracked').write_text('original\n')
        git(self.root, 'add', 'tracked')
        git(self.root, 'commit', '--quiet', '-m', 'fixture')

    def test_complete_bytes_include_untracked_source_and_exact_exclusion(self):
        (self.root / 'new source λ.rs').write_text('production bytes\n')
        (self.root / 'link').symlink_to('new source λ.rs')
        (self.root / 'test-source-boot').mkdir()
        (self.root / 'test-source-boot/unrelated').write_text('excluded\n')
        included = self.root / 'bridge/haskell/test-source-boot/production.hs'
        included.parent.mkdir(parents=True)
        included.write_text('included\n')
        manifest = snapshot.capture(self.root, self.base / 'pre', ['test-source-boot'])
        with tarfile.open(self.base / 'pre/sources.tar') as archive:
            self.assertEqual(archive.extractfile('new source λ.rs').read(), b'production bytes\n')
            self.assertEqual(archive.getmember('link').linkname, 'new source λ.rs')
            self.assertEqual(archive.extractfile('bridge/haskell/test-source-boot/production.hs').read(), b'included\n')
            self.assertNotIn('test-source-boot/unrelated', archive.getnames())
        self.assertEqual(manifest['exclude_prefixes'], ['test-source-boot'])
        self.assertTrue(snapshot.audit(self.root, self.base / 'pre', self.base / 'post.json'))

    def test_post_audit_rejects_added_deleted_changed_and_mode_changed_source(self):
        (self.root / 'executable').write_text('retained\n')
        (self.root / 'deleted').write_text('retained\n')
        snapshot.capture(self.root, self.base / 'pre', [])
        (self.root / 'tracked').write_text('changed\n')
        (self.root / 'added.rs').write_text('new production\n')
        (self.root / 'deleted').unlink()
        (self.root / 'executable').chmod(0o755)
        self.assertFalse(snapshot.audit(self.root, self.base / 'pre', self.base / 'post.json'))
        report = json.loads((self.base / 'post.json').read_text())
        self.assertEqual({entry['path'] for entry in report['changes']}, {'tracked', 'added.rs', 'deleted', 'executable'})

    def test_tracked_deletion_and_changed_symlink_remain_visible(self):
        (self.root / 'tracked').unlink()
        (self.root / 'link').symlink_to('first')
        manifest = snapshot.capture(self.root, self.base / 'pre', [])
        self.assertIn({'path': 'tracked', 'kind': 'missing'}, manifest['source']['entries'])
        (self.root / 'link').unlink()
        (self.root / 'link').symlink_to('second')
        self.assertFalse(snapshot.audit(self.root, self.base / 'pre', self.base / 'post.json'))

    def test_initialized_submodule_bytes_include_its_untracked_source(self):
        child = self.base / 'child'
        child.mkdir()
        git(child, 'init', '--quiet')
        git(child, 'config', 'user.email', 'source-test@example.invalid')
        git(child, 'config', 'user.name', 'Source Test')
        (child / 'module.hs').write_text('original module\n')
        git(child, 'add', 'module.hs')
        git(child, 'commit', '--quiet', '-m', 'module')
        git(self.root, '-c', 'protocol.file.allow=always', 'submodule', 'add', '--quiet', str(child), 'vendor/module')
        (self.root / 'vendor/module/untracked.hs').write_text('untracked module\n')
        snapshot.capture(self.root, self.base / 'pre', [])
        with tarfile.open(self.base / 'pre/sources.tar') as archive:
            self.assertEqual(archive.extractfile('vendor/module/untracked.hs').read(), b'untracked module\n')
        (self.root / 'vendor/module/added.hs').write_text('late module\n')
        self.assertFalse(snapshot.audit(self.root, self.base / 'pre', self.base / 'post.json'))

    def test_capture_refuses_a_moving_source(self):
        original = snapshot.state

        def change_before_recheck(root, prefixes):
            (self.root / 'tracked').write_text('concurrent edit\n')
            return original(root, prefixes)

        with patch.object(snapshot, 'state', side_effect=change_before_recheck):
            with self.assertRaisesRegex(RuntimeError, 'source changed during capture'):
                snapshot.capture(self.root, self.base / 'pre', [])
        self.assertTrue(json.loads((self.base / 'pre/manifest.json').read_text())['capture_changes'])

    def test_snapshot_cannot_add_itself_to_the_source_inventory(self):
        with self.assertRaisesRegex(RuntimeError, 'must be Git-ignored'):
            snapshot.capture(self.root, self.root / 'evidence', [])
        (self.root / '.gitignore').write_text('target/\n')
        snapshot.capture(self.root, self.root / 'target/pre', [])
        self.assertTrue(snapshot.audit(self.root, self.root / 'target/pre', self.base / 'post.json'))

    def test_corrupted_retained_bytes_cannot_pass_audit(self):
        snapshot.capture(self.root, self.base / 'pre', [])
        with (self.base / 'pre/sources.tar').open('ab') as archive:
            archive.write(b'changed retained bytes')
        with self.assertRaisesRegex(RuntimeError, 'archive digest changed'):
            snapshot.audit(self.root, self.base / 'pre', self.base / 'post.json')

    def test_subdirectory_cannot_claim_a_complete_repository_capture(self):
        nested = self.root / 'subdirectory'
        nested.mkdir()
        with self.assertRaisesRegex(RuntimeError, 'complete repository root'):
            snapshot.capture(nested, self.base / 'pre', [])


if __name__ == '__main__':
    unittest.main()
