"""Disposable copies preserve input content and permit output cleanup."""

import importlib.util
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("directory_copy", ROOT / "scripts/copy-directory.py")
COPY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COPY)


class DirectoryCopy(unittest.TestCase):
    def setUp(self):
        self.cp = os.environ["TIDEPOOL_TEST_CP"]
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name)
        self.source = self.root / "source"
        (self.source / "nested").mkdir(parents=True)
        self.external = self.root / "external"
        self.external.mkdir()
        (self.source / "nested/data").write_bytes(b"immutable input\x00\xff")
        (self.source / "tool").write_text("#!/bin/sh\nexit 0\n")
        (self.external / "sentinel").write_text("external input")
        (self.source / "external-link").symlink_to(self.external, target_is_directory=True)
        (self.source / "nested/data").chmod(0o444)
        (self.source / "tool").chmod(0o555)
        (self.external / "sentinel").chmod(0o444)
        for directory in [self.source / "nested", self.source, self.external]:
            directory.chmod(0o555)

    def tearDown(self):
        for directory, _, _ in os.walk(self.root, followlinks=False):
            path = Path(directory)
            path.chmod(stat.S_IMODE(path.stat().st_mode) | stat.S_IWUSR)
        self.scratch.cleanup()

    def test_readonly_input_preserves_bytes_modes_and_links_with_removable_output(self):
        control = self.root / "archive-copy"
        subprocess.run([self.cp, "-a", str(self.source) + "/.", str(control)], check=True)
        with self.assertRaises(PermissionError):
            shutil.rmtree(control)
        output = self.root / "output"
        COPY.copy_directory(self.cp, self.source, output)
        self.assertEqual((output / "nested/data").read_bytes(), b"immutable input\x00\xff")
        self.assertEqual((output / "tool").read_bytes(), (self.source / "tool").read_bytes())
        self.assertEqual(stat.S_IMODE((output / "nested/data").stat().st_mode), 0o444)
        self.assertEqual(stat.S_IMODE((output / "tool").stat().st_mode), 0o555)
        self.assertEqual(os.readlink(output / "external-link"), str(self.external))
        for directory in [output, output / "nested"]:
            self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o755)
        for directory in [self.source, self.source / "nested", self.external]:
            self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o555)
        self.assertEqual(stat.S_IMODE((self.external / "sentinel").stat().st_mode), 0o444)
        shutil.rmtree(output)
        self.assertFalse(output.exists())
        self.assertEqual((self.external / "sentinel").read_text(), "external input")

    def test_symlink_output_refuses_before_mutating_its_target(self):
        valid = self.root / "valid"
        COPY.copy_directory(self.cp, self.source, valid)
        output = self.root / "output"
        output.symlink_to(self.external, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "output must not be a symlink"):
            COPY.copy_directory(self.cp, self.source, output)
        self.assertEqual(stat.S_IMODE(self.external.stat().st_mode), 0o555)
        self.assertEqual(list(self.external.iterdir()), [self.external / "sentinel"])


if __name__ == "__main__":
    unittest.main()
