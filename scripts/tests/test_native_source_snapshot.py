"""Qualification source captures preserve identity and refuse incomplete input."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "build/rust/source_snapshot.py"
spec = importlib.util.spec_from_file_location("source_snapshot", SCRIPT)
owner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owner)


class SourceSnapshotTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.first = self.root / "first"
        self.second = self.root / "second"
        self.first.mkdir()
        self.second.mkdir()
        (self.first / "lib.rs").write_bytes(b"first source")
        self.output = self.root / "captured"

    def test_materializes_source_bytes_at_full_repository_paths(self):
        owner.capture([("crate/src", self.first)], self.output)
        self.assertEqual((self.output / "crate/src/lib.rs").read_bytes(), b"first source")

    def test_duplicate_identical_inputs_have_one_identity(self):
        (self.second / "lib.rs").write_bytes(b"first source")
        owner.capture([("crate/src", self.first), ("crate/src", self.second)], self.output)
        self.assertEqual(list(self.output.rglob("*.rs")), [self.output / "crate/src/lib.rs"])

    def test_conflicting_inputs_refuse_before_exporting_any_output(self):
        (self.second / "lib.rs").write_bytes(b"substituted source")
        with self.assertRaisesRegex(ValueError, "contradictory declared bytes"):
            owner.capture([("crate/src", self.first), ("crate/src", self.second)], self.output)
        self.assertFalse(self.output.exists())

    def test_absolute_and_parent_prefixes_refuse(self):
        for prefix in ("/absolute", "../outside", "crate/../outside"):
            with self.subTest(prefix=prefix), self.assertRaisesRegex(ValueError, "unsafe source prefix"):
                owner.capture([(prefix, self.first)], self.output)
        self.assertFalse(self.output.exists())

    def test_empty_inventory_refuses(self):
        with self.assertRaisesRegex(ValueError, "capture is empty"):
            owner.capture([("", self.second)], self.output)

    def test_missing_declared_source_refuses(self):
        (self.first / "missing.rs").symlink_to(self.root / "absent")
        with self.assertRaisesRegex(ValueError, "not a readable file"):
            owner.capture([("crate/src", self.first)], self.output)
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
