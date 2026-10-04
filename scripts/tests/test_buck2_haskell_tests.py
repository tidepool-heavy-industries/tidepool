"""Check native Haskell package closure generation without invoking Buck or GHC."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "buck2-haskell-tests.py"
SPEC = importlib.util.spec_from_file_location("buck2_haskell_tests", SCRIPT)
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)


class HaskellInstalledPackageClosure(unittest.TestCase):
    def roster(self):
        return GENERATOR.components((GENERATOR.PACKAGE / "tidepool-extract.cabal").read_text())

    def test_encoder_links_archive_packages_without_exposing_them_to_compilation(self):
        roster = self.roster()
        component = roster["execution-schema-encode"]
        direct, libraries = GENERATOR.dependencies_for(component, roster)
        self.assertEqual(libraries, ["tidepool-extract-internal"])
        self.assertNotIn("ghc", direct)
        self.assertNotIn("cryptohash-sha256", direct)
        linked = GENERATOR.installed_link_packages(component, roster)
        self.assertIn("ghc", linked)
        self.assertIn("cryptohash-sha256", linked)
        self.assertIn("tasty", linked)
        self.assertNotIn("tidepool-extract-internal", linked)

    def test_production_corpus_links_its_library_without_test_packages(self):
        roster = self.roster()
        linked = GENERATOR.installed_link_packages(roster["execution-corpus-producer"], roster)
        self.assertIn("ghc", linked)
        self.assertIn("cryptohash-sha256", linked)
        self.assertNotIn("tasty", linked)
        self.assertNotIn("tasty-hunit", linked)

    def test_component_without_project_libraries_keeps_direct_package_roots(self):
        roster = self.roster()
        component = roster["model-turn-test"]
        direct, libraries = GENERATOR.dependencies_for(component, roster)
        self.assertEqual(libraries, [])
        self.assertEqual(GENERATOR.installed_link_packages(component, roster), direct)
        self.assertNotIn("ghc", direct)

    def test_nested_libraries_include_common_dependencies_and_deduplicate_roots(self):
        roster = GENERATOR.components("""common library-packages
  build-depends: base >= 4.20 && < 5, text, cryptohash-sha256
library assignment-internal
  import: library-packages
library tidepool-extract-internal
  build-depends: assignment-internal, ghc, text
test-suite consumer
  build-depends: tidepool-extract-internal, assignment-internal, tasty
""")
        self.assertEqual(GENERATOR.installed_link_packages(roster["consumer"], roster),
                         ["cryptohash-sha256", "ghc", "tasty", "text"])
        self.assertEqual(GENERATOR.dependencies_for(roster["consumer"], roster),
                         (["tasty"], ["assignment-internal", "tidepool-extract-internal"]))

    def test_cyclic_internal_library_dependency_fails_generation(self):
        roster = GENERATOR.components("""library assignment-internal
  build-depends: tidepool-extract-internal
library tidepool-extract-internal
  build-depends: assignment-internal
test-suite consumer
  build-depends: tidepool-extract-internal
""")
        with self.assertRaisesRegex(ValueError, "cyclic Cabal internal library dependency"):
            GENERATOR.installed_link_packages(roster["consumer"], roster)

    def test_missing_internal_library_declaration_fails_generation(self):
        roster = GENERATOR.components("""test-suite consumer
  build-depends: tidepool-extract-internal
""")
        with self.assertRaisesRegex(ValueError, "missing Cabal internal library tidepool-extract-internal"):
            GENERATOR.installed_link_packages(roster["consumer"], roster)

    def test_new_internal_library_requires_a_declared_buck_target(self):
        roster = GENERATOR.components("""library new-library
  build-depends: text
test-suite consumer
  build-depends: new-library
""")
        with self.assertRaisesRegex(ValueError, "new-library needs a native Buck target"):
            GENERATOR.installed_link_packages(roster["consumer"], roster)

    def test_library_package_change_updates_only_consumers_link_flags(self):
        cabal = """library tidepool-extract-internal
  build-depends: base, text
test-suite consumer
  main-is: Main.hs
  hs-source-dirs: consumer
  build-depends: base, tidepool-extract-internal, tasty
executable standalone
  main-is: Main.hs
  hs-source-dirs: standalone
  build-depends: base, bytestring
"""
        with tempfile.TemporaryDirectory() as directory:
            package = Path(directory)
            for component in ("consumer", "standalone"):
                (package / component).mkdir()
                (package / component / "Main.hs").write_text("module Main where\nmain = pure ()\n")
            path = package / "tidepool-extract.cabal"
            path.write_text(cabal)
            with patch.object(GENERATOR, "PACKAGE", package):
                before = GENERATOR.render()
                path.write_text(cabal.replace("base, text", "base, text, cryptohash-sha256"))
                after = GENERATOR.render()
            self.assertNotIn('name = "tidepool_extract_internal"', after)
            self.assertEqual(before.split('name = "standalone",')[1],
                             after.split('name = "standalone",')[1])
            before_compile = before.split("compiler_flags = ")[1].split("linker_flags = ")[0]
            after_compile = after.split("compiler_flags = ")[1].split("linker_flags = ")[0]
            self.assertEqual(before_compile, after_compile)
            self.assertNotIn("cryptohash-sha256", before)
            linked, _ = json.JSONDecoder().raw_decode(
                after.split("linker_flags = haskell_component_link_flags(", 1)[1])
            self.assertEqual(linked, ["cryptohash-sha256", "tasty", "text"])

    def test_checked_in_components_match_cabal_graph(self):
        self.assertEqual((GENERATOR.PACKAGE / "tests.bzl").read_text(), GENERATOR.render())

    def test_workspace_module_uses_its_source_owner_export(self):
        path, target = GENERATOR.source("Project.BrowserScenario", [
            "../../exomonad/examples/workspace/.exomonad"])
        self.assertEqual(path, "Project/BrowserScenario.hs")
        self.assertEqual(target,
                         "//exomonad/examples/workspace:authored_haskell_Project_BrowserScenario_hs")

    def test_pinned_contract_uses_actual_facade_fixture(self):
        self.assertEqual(GENERATOR.source("Project.Checks", ["generated/pinned"]),
                         ("Project/Checks.hs", "//bridge/facade:workspace_pinned_check_source"))

    def test_pinned_jev_modules_are_declared_artifact_projections(self):
        for module, path in (("Jev.Core", "core/Jev/Core.hs"),
                             ("Jev.Core.Schema", "core/Jev/Core/Schema.hs")):
            self.assertEqual(GENERATOR.source(module, ["generated/jev/core"]),
                             (path.removeprefix("core/"),
                              "toolchains//:jev_sources[" + GENERATOR.normalized_path(path) + "]"))
        self.assertFalse(GENERATOR.is_jev_core("Jev.CoreSomethingElse"))

    def test_jev_projection_roster_comes_from_cabal_module_declarations(self):
        rendered = GENERATOR.render()
        paths, _ = json.JSONDecoder().raw_decode(rendered.split("JEV_SOURCE_PATHS = ", 1)[1])
        self.assertEqual(paths, ["core/Jev/Core.hs", "core/Jev/Core/Contract.hs",
                                 "core/Jev/Core/Json.hs", "core/Jev/Core/Schema.hs"])
        roster = self.roster()
        modules = GENERATOR.words(roster["automation-helper-contract"]["fields"]["other-modules"])
        self.assertEqual(paths, sorted("core/" + module.replace(".", "/") + ".hs"
                                       for module in modules if GENERATOR.is_jev_core(module)))

    def test_compiler_profile_fixtures_are_runtime_inputs_without_host_module_edges(self):
        roster = self.roster()
        component = roster["tool-profiles-contract"]
        _, srcs, _, _, _ = GENERATOR.fields_for(component, roster)
        fixtures = sorted((GENERATOR.PACKAGE / "test-check/tool-profiles").glob("*.hs"))
        self.assertEqual(len(fixtures), 9)
        rendered = GENERATOR.render()
        resources = rendered.split('name = "tool_profiles_contract_resources",', 1)[1].split(")", 1)[0]
        for fixture in fixtures:
            relative = fixture.relative_to(GENERATOR.PACKAGE).as_posix()
            self.assertNotIn(relative, srcs.values())
            self.assertIn(relative, resources)

    def test_unowned_external_source_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Unowned.hs"
            path.write_text("module Unowned where\n")
            with self.assertRaisesRegex(ValueError, "needs an owning native source export"):
                GENERATOR.source_location(path)


if __name__ == "__main__":
    unittest.main()
