#!/usr/bin/env python3
"""Behavior checks for affected-target selection, without Cargo or GHC builds."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("changed", Path(__file__).parents[1] / "test-changed.py")
changed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(changed)


class Selection(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.packages = []
        self.package("a")
        self.package("b", ["a"])
        self.package("c", ["b"])
        self.package("unrelated")

    def package(self, name, dependencies=()):
        self.packages.append(dict(
            name=name, id=name, manifest_path=str(self.root / name / "Cargo.toml"),
            dependencies=[dict(name=d, kind=None) for d in dependencies],
            targets=[dict(name=name, kind=["lib"], src_path=str(self.root / name / "src/lib.rs")),
                     dict(name="suite", kind=["test"], src_path=str(self.root / name / "tests/suite.rs"))]))

    def select(self, *paths):
        return changed.select(dict(packages=self.packages, workspace_members=[p["id"] for p in self.packages]), paths, self.root)

    def test_production_change_checks_transitive_consumers_but_runs_owner_once(self):
        selection, checks, _, _ = self.select("a/src/lib.rs", "a/src/other.rs")
        self.assertEqual(checks, {"a", "b", "c"})
        self.assertEqual(selection, {"a": {("lib", ""), ("test", "suite")}})

    def test_test_support_change_checks_dev_consumers(self):
        self.packages[1]["dependencies"][0]["kind"] = "dev"
        _, checks, _, _ = self.select("a/src/lib.rs")
        self.assertEqual(checks, {"a", "b", "c"})

    def test_suite_leaf_does_not_rebuild_unrelated_test_targets(self):
        source = self.root / "a/tests/suite.rs"
        source.parent.mkdir(parents=True)
        source.write_text('#[path = "cases/a.rs"]\nmod a;\n')
        selection, checks, _, _ = self.select("a/tests/cases/a.rs")
        self.assertEqual(selection, {"a": {("test", "suite")}})
        self.assertEqual(checks, set())

    def test_shared_fixture_selects_all_owning_tests(self):
        selection, checks, actions, _ = self.select("a/tests/fixtures/input.hs")
        self.assertEqual(selection["a"], {("lib", ""), ("test", "suite")})
        self.assertIn("registration", actions)
        self.assertFalse(checks)

    def test_shared_build_change_requires_explicit_integration(self):
        _, _, _, reasons = self.select("Cargo.lock", "scripts/battery.sh")
        self.assertEqual(reasons, {"Cargo.lock", "scripts/battery.sh"})

    def test_haskell_corpus_change_cannot_disappear(self):
        self.package("tidepool-runtime")
        selection, _, actions, _ = self.select("haskell/test-execution-corpus/Case.hs")
        self.assertIn("tidepool-runtime", selection)
        self.assertEqual(actions, {"haskell", "fixtures"})

    def test_haskell_library_change_checks_fixtures(self):
        self.package("tidepool-runtime")
        _, _, actions, _ = self.select("haskell/lib/Tidepool/Data/Time.hs")
        self.assertEqual(actions, {"haskell", "fixtures"})

    def test_documentation_only_is_explicit_empty_selection(self):
        self.assertEqual(self.select("a/README.md", "docs/GUIDE.md"), ({}, set(), set(), set()))


if __name__ == "__main__":
    unittest.main()
