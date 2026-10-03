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
            for name in ("test-suite-check.sh", "test-suite-check.py", "test_source_ownership.py"):
                shutil.copy(SCRIPTS / name, root / "scripts")
            tests = root / "nested" / "different-directory-name" / "tests"
            (tests / "suites").mkdir(parents=True)
            suite = tests / "suites" / "integration.rs"
            suite.write_text('#[path = "../registered.rs"]\nmod registered;\n')
            (tests / "registered.rs").touch()
            metadata = {"workspace_members": ["example"], "packages": [{
                "id": "example", "name": "example",
                "manifest_path": str(tests.parent / "Cargo.toml"),
                "targets": [{"kind": ["test"], "name": "integration", "src_path": str(suite)}]}]}
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
            (tests / "forgotten.rs").write_text("#[test]\nfn forgotten_case() {}\n")
            bad = check()
            self.assertNotEqual(bad.returncode, 0)
            self.assertIn("forgotten.rs", bad.stdout)

    def test_transitive_target_graph_resolves_nested_path_modules(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            suite = root / "tests/suites/actor.rs"
            actor = root / "tests/resident_local_actor.rs"
            reload = root / "tests/resident_local_actor/reload_uncertainty.rs"
            suite.parent.mkdir(parents=True)
            reload.parent.mkdir(parents=True)
            suite.write_text('#[path = "../resident_local_actor.rs"]\nmod resident_local_actor;\n')
            actor.write_text('#[path = "resident_local_actor/reload_uncertainty.rs"]\nmod reload_uncertainty;\n')
            reload.write_text("#[test]\nfn reload_uncertainty_case() {}\n")
            graph, unknown = finder.integration_target_sources([
                {"kind": ["test"], "name": "actor", "src_path": str(suite)}])
            self.assertEqual(unknown, set())
            self.assertEqual(graph[reload.resolve()], {"actor"})
            package = {"name": "example", "manifest_path": str(root / "Cargo.toml"),
                       "targets": [{"kind": ["test"], "name": "actor", "src_path": str(suite)}]}
            self.assertEqual(finder.command(package, str(reload), "reload_uncertainty_case"),
                             "just test-target example actor 'test(reload_uncertainty_case)'")

    def test_ordinary_modules_and_path_after_cfg_resolve_from_module_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            entry = root / "tests/odd_root.rs"
            support = root / "tests/support.rs"
            nested = root / "tests/support/nested.rs"
            leaf = root / "tests/support/nested/leaf.rs"
            nested.parent.mkdir(parents=True)
            leaf.parent.mkdir()
            entry.write_text("mod support;\n")
            support.write_text("mod nested;\n")
            nested.write_text('#[cfg(test)]\n#[path = "nested/leaf.rs"]\nmod leaf;\n')
            leaf.write_text("#[test]\nfn case() {}\n")
            graph, unknown = finder.integration_target_sources([
                {"kind": ["test"], "name": "odd", "src_path": str(entry)}])
            self.assertEqual(unknown, set())
            self.assertEqual(graph[leaf.resolve()], {"odd"})


if __name__ == "__main__":
    unittest.main()
