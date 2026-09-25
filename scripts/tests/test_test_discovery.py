"""Test discovery regressions without compilation."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("test_find", SCRIPTS / "test-find.py")
finder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(finder)


class DiscoveryTests(unittest.TestCase):
    def test_binary_target(self):
        package = {"name": "example", "manifest_path": "/repo/pkg/Cargo.toml",
                   "targets": [{"kind": ["bin"], "name": "cli",
                                "src_path": "/repo/pkg/src/bin/cli.rs"}]}
        self.assertEqual(finder.command(package, "/repo/pkg/src/bin/cli.rs", "parses"),
                         "just test-bin example cli 'test(parses)'")
        self.assertEqual(finder.command(package, "/repo/pkg/src/lib.rs", "parses"),
                         "just test-lib example 'test(parses)'")

    def test_nested_suite_registration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copy(SCRIPTS / "test-suite-check.sh", root / "scripts")
            tests = root / "nested" / "different-directory-name" / "tests"
            (tests / "suites").mkdir(parents=True)
            suite = tests / "suites" / "integration.rs"
            suite.write_text('#[path = "../registered.rs"]\nmod registered;\n')
            (tests / "registered.rs").touch()
            metadata = {"workspace_members": ["example"], "packages": [{
                "id": "example", "name": "example",
                "manifest_path": str(tests.parent / "Cargo.toml"),
                "targets": [{"kind": ["test"], "src_path": str(suite)}]}]}
            (root / "metadata.json").write_text(json.dumps(metadata))
            (root / "bin").mkdir()
            cargo = root / "bin" / "cargo"
            cargo.write_text('#!/bin/sh\ncat metadata.json\n')
            cargo.chmod(0o755)
            env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])
            def check():
                return subprocess.run(["bash", str(root / "scripts/test-suite-check.sh")],
                                      env=env, text=True, capture_output=True)
            good = check()
            self.assertEqual(good.returncode, 0, good.stdout + good.stderr)
            (tests / "forgotten.rs").touch()
            bad = check()
            self.assertNotEqual(bad.returncode, 0)
            self.assertIn("forgotten.rs", bad.stdout)
            self.assertIn("example test files", bad.stderr)


if __name__ == "__main__":
    unittest.main()
