"""Check materialized Buck resources against the authored workspace contract."""
import os
from pathlib import Path
import unittest


EXCLUDED = {"logs", "sessions", "runtime", "build", ".git", "dist-newstyle", "target"}


def authored_files(root):
    files = {}
    for directory, directories, names in os.walk(root / ".exomonad"):
        directories[:] = [name for name in directories if name not in EXCLUDED]
        for name in names:
            if name not in EXCLUDED:
                path = Path(directory) / name
                files[path.relative_to(root).as_posix()] = path.read_bytes()
    for name in ("flake.nix", "flake.lock"):
        if (root / name).is_file():
            files[name] = (root / name).read_bytes()
    return files


def all_files(root):
    return {path.relative_to(root).as_posix(): path.read_bytes()
            for path in root.rglob("*") if path.is_file()}


class AuthoredWorkspaceInputTests(unittest.TestCase):
    def test_facade_resources_match_authored_copy(self):
        expected = authored_files(Path(os.environ["AUTHORED_TREE"]))
        self.assertGreater(len(expected), 0)
        self.assertIn("flake.lock", expected)
        self.assertGreater(len([path for path in expected if path.endswith(".sh")]), 0)
        actual = all_files(Path(os.environ["FACADE_TREE"]))
        self.assertEqual(set(actual), set(expected))
        for path in expected:
            self.assertEqual(actual[path], expected[path], path)

    def test_qualification_snapshot_preserves_authored_bytes_and_build_sources(self):
        expected = authored_files(Path(os.environ["AUTHORED_TREE"]))
        actual = all_files(Path(os.environ["QUALIFICATION_TREE"]))
        self.assertIn("BUCK", actual)
        build_sources = {path: contents for path, contents in actual.items()
                         if path in {"BUCK", "test-authored-inputs.sh", "test_authored_inputs.py"}
                         or ("/" not in path and path.endswith(".bzl"))}
        actual = {path: contents for path, contents in actual.items()
                  if path not in build_sources}
        self.assertEqual(set(actual), set(expected))
        for path in expected:
            self.assertEqual(actual[path], expected[path], path)


if __name__ == "__main__":
    unittest.main(verbosity=2)
