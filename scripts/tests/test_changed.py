#!/usr/bin/env python3
"""Behavior checks for affected-target selection, without Cargo or GHC builds."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import json
import hashlib
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
        self.assertEqual(checks, {
            "a": changed.CheckObligation.PRODUCTION,
            "b": changed.CheckObligation.PRODUCTION,
            "c": changed.CheckObligation.PRODUCTION,
        })
        self.assertEqual(selection, {"a": {("lib", ""), ("test", "suite")}})

    def test_test_support_change_checks_dev_consumers_without_propagating_them(self):
        self.packages[1]["dependencies"][0]["kind"] = "dev"
        _, checks, _, _ = self.select("a/src/lib.rs")
        self.assertEqual(checks, {
            "a": changed.CheckObligation.PRODUCTION,
            "b": changed.CheckObligation.DEVELOPMENT,
        })

    def test_production_path_upgrades_a_dev_obligation(self):
        self.packages[1]["dependencies"][0]["kind"] = "dev"
        self.packages[2]["dependencies"].append(dict(name="a", kind=None))
        _, checks, _, _ = self.select("a/src/lib.rs")
        self.assertEqual(checks["c"], changed.CheckObligation.PRODUCTION)

    def test_build_dependencies_propagate_production_checks(self):
        self.packages[1]["dependencies"][0]["kind"] = "build"
        _, checks, _, _ = self.select("a/src/lib.rs")
        self.assertEqual(checks, {
            "a": changed.CheckObligation.PRODUCTION,
            "b": changed.CheckObligation.PRODUCTION,
            "c": changed.CheckObligation.PRODUCTION,
        })

    def test_compiler_daemon_tracks_execution_not_cargo_checks(self):
        checks = changed.commands({}, {"a": changed.CheckObligation.PRODUCTION}, set())
        self.assertFalse(changed.requires_compiler(checks, set()))
        cabal = changed.commands({}, {}, {"haskell"}, ["cell-splitter-test"])
        self.assertFalse(changed.requires_compiler(cabal, {"haskell"}))
        self.assertTrue(changed.requires_compiler(
            [["scripts/battery.sh", "-p", "tidepool-runtime"]], set()))
        self.assertTrue(changed.requires_compiler([], {"fixture:containers-contract"}))

    def test_suite_leaf_does_not_rebuild_unrelated_test_targets(self):
        source = self.root / "a/tests/suite.rs"
        source.parent.mkdir(parents=True)
        source.write_text('#[path = "cases/a.rs"]\nmod a;\n')
        selection, checks, _, _ = self.select("a/tests/cases/a.rs")
        self.assertEqual(selection, {"a": {("test", "suite")}})
        self.assertEqual(checks, {})

    def test_shared_fixture_selects_all_owning_tests(self):
        selection, checks, actions, _ = self.select("a/tests/fixtures/input.hs")
        self.assertEqual(selection["a"], {("lib", ""), ("test", "suite")})
        self.assertIn("registration", actions)
        self.assertEqual(checks, {})

    def test_shared_build_change_requires_explicit_integration(self):
        _, _, _, reasons = self.select("Cargo.lock", "scripts/battery.sh")
        self.assertEqual(reasons, {"Cargo.lock", "scripts/battery.sh"})

    def test_retired_source_has_no_supported_build_obligation(self):
        self.package("tidepool")
        retired = (
            "tidepool-harness/Cargo.toml", "tidepool-harness/src/engine.rs",
            "tidepool-web/src/lib.rs", "tidepool/src/bin/tidepool-selfharness.rs",
            "tidepool/src/bin/tidepool-selfharness/prompt_catalog.rs",
        )
        self.assertEqual(self.select(*retired), ({}, {}, set(), set()))
        selection, checks, _, reasons = self.select(
            *retired, "tidepool/src/bin/shoal.rs", "Cargo.toml")
        self.assertIn("tidepool", selection)
        self.assertIn("tidepool", checks)
        self.assertEqual(reasons, {"Cargo.toml"})

    def test_haskell_corpus_change_cannot_disappear(self):
        self.package("tidepool-runtime")
        selection, _, actions, _ = self.select("haskell/test-execution-corpus/Case.hs")
        self.assertIn("tidepool-runtime", selection)
        self.assertEqual(actions, {"haskell", "fixtures"})

    def test_haskell_library_change_checks_fixtures(self):
        self.package("tidepool-runtime")
        _, _, actions, _ = self.select("haskell/lib/Tidepool/Data/Time.hs")
        self.assertEqual(actions, {"haskell", "fixtures"})

    def test_cabal_components_come_from_manifest_source_roots(self):
        manifest = self.root / "haskell/tidepool-extract.cabal"
        manifest.parent.mkdir()
        manifest.write_text("library compiler\n  hs-source-dirs: src\nexecutable worker\n  hs-source-dirs: app\ntest-suite parser\n  hs-source-dirs: test-parser\n")
        self.assertEqual(changed.cabal_components(self.root, ["haskell/src/A.hs"]), ["compiler"])
        self.assertEqual(changed.cabal_components(self.root, ["haskell/test-parser/A.hs"]), ["parser"])
        self.assertEqual(changed.cabal_components(self.root, ["haskell/tidepool-extract.cabal"]), ["all"])

    def fixture_index(self):
        self.package("tidepool-runtime")
        library = self.root / "haskell/lib/Library.hs"
        library.parent.mkdir(parents=True)
        library.write_text("module Library where")
        unrelated = self.root / "haskell/lib/Unrelated.hs"
        unrelated.write_text("module Unrelated where")
        candidate = self.root / "higher/Library.hs"
        evidence = dict(version=1, cache_safe=True, selection_complete=True,
                        sources=[dict(path=str(library), sha256=hashlib.sha256(library.read_bytes()).hexdigest())],
                        resolutions=[dict(module="Library", selected=str(library), candidates=[str(candidate), str(library)])], packages=[])
        index = self.root / "target/prepared-corpus/dependencies.json"
        index.parent.mkdir(parents=True)
        document = dict(
            version=1,
            repository=str(self.root),
            cohorts={"selected": evidence},
            cohort_count=1,
            complete=True,
            expected_cohorts=["selected"],
        )
        index.write_text(json.dumps(document))
        return index, document, library, candidate

    def test_complete_evidence_selects_consumers_and_ignores_unrelated_library(self):
        self.fixture_index()
        _, _, actions, _ = self.select("haskell/lib/Library.hs")
        self.assertIn("fixture:selected", actions)
        self.assertNotIn("fixtures", actions)
        _, _, actions, _ = self.select("haskell/lib/Unrelated.hs")
        self.assertFalse(any(a.startswith("fixture") for a in actions))

    def test_missing_incomplete_or_stale_evidence_falls_back(self):
        index, document, library, _ = self.fixture_index()
        document["cohorts"]["selected"]["selection_complete"] = False
        index.write_text(json.dumps(document))
        self.assertIn("fixtures", self.select("haskell/lib/Library.hs")[2])
        document["cohorts"]["selected"]["selection_complete"] = True
        index.write_text(json.dumps(document))
        library.write_text("an edit outside the selected diff")
        self.assertIn("fixtures", self.select("haskell/lib/Unrelated.hs")[2])
        index.unlink()
        self.assertIn("fixtures", self.select("haskell/lib/Library.hs")[2])

    def test_shadow_insertions_select_previous_consumers(self):
        index, _, _, candidate = self.fixture_index()
        candidate.parent.mkdir()
        candidate.write_text("module Library where")
        self.assertEqual(changed.affected_fixtures(index, ["higher/Library.hs"], self.root), ["selected"])
        self.assertIsNone(changed.affected_fixtures(index, [], self.root))

    def test_incomplete_inventory_cannot_reduce_coverage(self):
        index, document, _, _ = self.fixture_index()
        document["cohort_count"] = 2
        index.write_text(json.dumps(document))
        self.assertIn("fixtures", self.select("haskell/lib/Library.hs")[2])

        document["cohort_count"] = 1
        document["complete"] = False
        index.write_text(json.dumps(document))
        self.assertIn("fixtures", self.select("haskell/lib/Library.hs")[2])

        document["complete"] = True
        document["expected_cohorts"].append("missing")
        index.write_text(json.dumps(document))
        self.assertIn("fixtures", self.select("haskell/lib/Library.hs")[2])

    def test_documentation_only_is_explicit_empty_selection(self):
        self.assertEqual(self.select("a/README.md", "docs/GUIDE.md"), ({}, {}, set(), set()))


if __name__ == "__main__":
    unittest.main()
