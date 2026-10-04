"""Exercise the actual pinned Cabal parser/finalizer through its native producer."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HEADER = "cabal-version: 3.4\nname: example\nversion: 1.0\nbuild-type: Simple\n"
LIBRARY = "library internal\n  exposed-modules: Library\n  hs-source-dirs: src\n  build-depends: base\n  default-language: GHC2024\n"


class CabalMetadataTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.producer = os.environ.get("TIDEPOOL_CABAL_METADATA")
        if not cls.producer:
            raise RuntimeError("missing declared native metadata producer TIDEPOOL_CABAL_METADATA")

    def invoke(self, source):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "example.cabal"
            path.write_text(HEADER + source)
            return subprocess.run([self.producer, str(path)], text=True, capture_output=True)

    def metadata(self, source):
        result = self.invoke(source)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def phase(self, metadata, name):
        return next(value for value in metadata["configurations"] if value["phase"] == name)

    def test_common_stanzas_expand_using_cabal(self):
        source = "common first\n  build-depends: text\ncommon second\n  import: first\n  ghc-options: -Wall\n" + LIBRARY.replace("  exposed-modules:", "  import: second\n  exposed-modules:")
        record = self.phase(self.metadata(source), "production")["components"][0]
        self.assertEqual({value["package"] for value in record["dependencies"]}, {"base", "text"})
        self.assertIn("-Wall", record["ghc_options"])

    def test_flag_conditionals_are_finalized_separately(self):
        source = "flag test-tools\n  default: False\n  manual: False\n" + LIBRARY + "  if flag(test-tools)\n    build-depends: tasty\n  else\n    build-depends: text\n"
        metadata = self.metadata(source)
        production = self.phase(metadata, "production")["components"][0]
        tests = self.phase(metadata, "tests")["components"][0]
        self.assertIn("text", [value["package"] for value in production["dependencies"]])
        self.assertNotIn("tasty", [value["package"] for value in production["dependencies"]])
        self.assertIn("tasty", [value["package"] for value in tests["dependencies"]])

    def test_unrelated_flag_retains_declared_default(self):
        metadata = self.metadata("flag unrelated\n  default: False\n  manual: False\n" + LIBRARY)
        self.assertTrue(all(value["flags"]["unrelated"] is False for value in metadata["configurations"]))

    def test_compiler_and_platform_conditions_use_the_pinned_identity(self):
        metadata = self.metadata(LIBRARY + "  if os(linux) && arch(x86_64) && impl(ghc >= 9.12.1)\n    build-depends: unix\n  else\n    build-depends: Win32\n")
        self.assertEqual(metadata["platform"], "x86_64-linux")
        self.assertTrue(metadata["compiler"].startswith("ghc-9.12."))
        dependencies = self.phase(metadata, "production")["components"][0]["dependencies"]
        self.assertIn("unix", [value["package"] for value in dependencies])
        self.assertNotIn("Win32", [value["package"] for value in dependencies])

    def test_qualified_project_library_is_a_typed_library_dependency(self):
        metadata = self.metadata(LIBRARY + "executable worker\n  main-is: Main.hs\n  build-depends: base, example:internal\n")
        record = next(value for value in self.phase(metadata, "production")["components"] if value["name"] == "worker")
        dependency = next(value for value in record["dependencies"] if value["package"] == "example")
        self.assertEqual(dependency["libraries"], ["internal"])

    def test_disabled_measurement_is_available_only_in_its_explicit_role(self):
        metadata = self.metadata("flag benchmarks\n  default: False\n  manual: True\n" + LIBRARY + "executable measurement\n  main-is: Main.hs\n  build-depends: base\n  if !flag(benchmarks)\n    buildable: False\n")
        for phase in ("production", "tests"):
            self.assertNotIn("measurement", [value["name"] for value in self.phase(metadata, phase)["components"]])
        self.assertIn("measurement", [value["name"] for value in self.phase(metadata, "benchmarks")["components"]])

    def test_unknown_syntax_and_unimplemented_native_inputs_refuse(self):
        for source, diagnostic in ((LIBRARY + "  invented-field: data\n", "parse warnings"),
                                   (LIBRARY + "  c-sources: foreign.c\n", "c-sources"),
                                   ("library internal\n  if (\n", "parse failure")):
            with self.subTest(diagnostic=diagnostic):
                result = self.invoke(source)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)


if __name__ == "__main__":
    unittest.main()
