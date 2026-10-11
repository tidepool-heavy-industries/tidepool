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

    def test_exclusions_prune_case_sensitive_names_at_every_depth(self):
        authored = self.first / ".exomonad/.hidden/.nested"
        authored.mkdir(parents=True)
        (authored / "extensionless").write_bytes(b"authored")
        for directory in (self.first, authored):
            (directory / "runtime").mkdir()
            (directory / "runtime/missing").symlink_to(self.root / "absent")
            (directory / "Runtime").write_bytes(b"case-sensitive authored name")
            (directory / "logs").write_bytes(b"excluded filename")
            (directory / "runtime.txt").write_bytes(b"basename must match exactly")
        owner.capture([("runtime/workspace", self.first)], self.output, ["runtime", "logs"])
        captured = {path.relative_to(self.output).as_posix(): path.read_bytes()
                    for path in self.output.rglob("*") if path.is_file()}
        self.assertEqual(captured, {
            "runtime/workspace/lib.rs": b"first source",
            "runtime/workspace/Runtime": b"case-sensitive authored name",
            "runtime/workspace/runtime.txt": b"basename must match exactly",
            "runtime/workspace/.exomonad/.hidden/.nested/extensionless": b"authored",
            "runtime/workspace/.exomonad/.hidden/.nested/Runtime": b"case-sensitive authored name",
            "runtime/workspace/.exomonad/.hidden/.nested/runtime.txt": b"basename must match exactly",
        })

    def test_empty_exclusions_preserve_hidden_and_runtime_contents(self):
        (self.first / "runtime/.hidden").mkdir(parents=True)
        (self.first / "runtime/.hidden/resource").write_bytes(b"unfiltered")
        owner.capture([("", self.first)], self.output)
        self.assertEqual((self.output / "runtime/.hidden/resource").read_bytes(), b"unfiltered")

    def test_exclusions_refuse_paths_and_empty_names_before_publication(self):
        for name in ("", ".", "..", "runtime/logs", "/runtime"):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "must be basenames"):
                owner.capture([("", self.first)], self.output, [name])
        self.assertFalse(self.output.exists())

    def test_authored_symlinks_refuse_before_publication(self):
        nested = self.first / ".exomonad/.hidden"
        nested.mkdir(parents=True)
        source = nested / "alias"
        for target in (self.second, self.first / "lib.rs", self.root / "absent", source):
            with self.subTest(target=target):
                source.symlink_to(target)
                try:
                    with self.assertRaisesRegex(ValueError, "contains a symlink"):
                        owner.capture([("", self.first)], self.output, reject_symlinks=True)
                    self.assertFalse(self.output.exists())
                finally:
                    source.unlink()

    def test_default_symlink_policy_preserves_readable_file_inputs(self):
        (self.first / "alias").symlink_to(self.first / "lib.rs")
        owner.capture([("", self.first)], self.output)
        self.assertEqual((self.output / "alias").read_bytes(), b"first source")

    def test_excluded_symlink_is_pruned_before_authored_refusal(self):
        (self.first / "runtime").symlink_to(self.root / "absent")
        owner.capture([("", self.first)], self.output, ["runtime"], reject_symlinks=True)
        self.assertEqual((self.output / "lib.rs").read_bytes(), b"first source")

    def test_inaccessible_subtree_refuses_before_publication(self):
        directory = self.first / "private"
        directory.mkdir()
        (directory / "resource").write_bytes(b"must not silently omit")
        directory.chmod(0)
        try:
            with self.assertRaises(PermissionError):
                owner.capture([("", self.first)], self.output)
            self.assertFalse(self.output.exists())
        finally:
            directory.chmod(0o700)


if __name__ == "__main__":
    unittest.main()
