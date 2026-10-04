"""Validate native component projection without compiling or parsing Cabal."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "buck2-haskell-components.py"
SPEC = importlib.util.spec_from_file_location("haskell_components", SCRIPT)
G = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(G)


def component(name, kind="library", phase="production", roots=("src",), modules=("Library",), dependencies=(), **extra):
    value = dict(name=name, kind=kind, phase=phase, main=None if kind == "library" else "Main.hs",
                 source_dirs=list(roots), modules=list(modules), language="GHC2024", extensions=[],
                 ghc_options=["-Wall"], dependencies=list(dependencies), tools=[])
    value.update(extra)
    return value


def dependency(package, library=None):
    return dict(package=package, version_range="-any", libraries=[library])


class ComponentProjectionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.package = Path(temporary.name)
        (self.package / "tidepool-extract.cabal").write_text("source bytes\n")
        for relative in ("src/Library.hs", "app/Main.hs", "test/Main.hs", "test/Negative.hs"):
            path = self.package / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("module Main where\n")
        self.addCleanup(patch.stopall)
        patch.object(G, "PACKAGE", self.package).start()

    def metadata(self, production=(), tests=(), benchmarks=()):
        return dict(schema=1, package="example", version="1.0", compiler="ghc-9.12.2", platform="x86_64-linux",
                    source_sha256=hashlib.sha256((self.package / "tidepool-extract.cabal").read_bytes()).hexdigest(),
                    configurations=[dict(phase=phase, flags={"test-tools": phase != "production", "benchmarks": phase == "benchmarks"}, components=list(values))
                                    for phase, values in zip(("production", "tests", "benchmarks"), (production, tests, benchmarks))])

    def test_project_libraries_are_edges_and_installed_packages_are_roots(self):
        library = component("internal", dependencies=[dependency("ghc"), dependency("cryptohash-sha256")])
        consumer = component("control", "test-suite", "tests", ("test",), (),
                             [dependency("example", "internal"), dependency("tasty")])
        package, roster = G.normalized_components(self.metadata([library], [consumer]))
        _, _, packages, edges, _ = G.fields_for(roster["control"], roster, package)
        self.assertEqual(packages, ["tasty"])
        self.assertEqual(edges, [":internal"])
        self.assertEqual(G.installed_link_packages(roster["control"], roster, package),
                         ["cryptohash-sha256", "ghc", "tasty"])

    def test_production_source_dependency_and_options_changes_update_one_output(self):
        library = component("internal", dependencies=[dependency("text")])
        worker = component("worker", "executable", "production", ("app",), (), [dependency("example", "internal")])
        metadata = self.metadata([library, worker])
        before = G.render(metadata)
        changed = copy.deepcopy(metadata)
        changed["configurations"][0]["components"][0]["dependencies"].append(dependency("cryptohash-sha256"))
        changed["configurations"][0]["components"][0]["ghc_options"].extend(["-optc", "a", "-optc", "b"])
        after = G.render(changed)
        self.assertIn('"cryptohash-sha256"', after)
        _, roster = G.normalized_components(changed)
        self.assertEqual(G.fields_for(roster["internal"], roster, "example")[-1][-4:], ["-optc", "a", "-optc", "b"])
        self.assertNotEqual(before, after)

    def test_changed_local_library_cannot_silently_unify_test_flag_variant(self):
        original = component("internal")
        variant = component("internal", phase="tests", dependencies=[dependency("tasty")])
        with self.assertRaisesRegex(ValueError, "unsupported native component variants"):
            G.normalized_components(self.metadata([original], [variant]))

    def test_duplicate_components_label_collisions_and_sources_are_rejected(self):
        for values, expected in (([component("a"), component("a")], "duplicate"),
                                 ([component("a-b"), component("a_b")], "collide")):
            with self.assertRaisesRegex(ValueError, expected):
                G.normalized_components(self.metadata(values))
        package, roster = G.normalized_components(self.metadata([component("a", modules=("Library", "Library"))]))
        with self.assertRaisesRegex(ValueError, "duplicate native source"):
            G.fields_for(roster["a"], roster, package)

    def test_missing_local_library_external_sublibrary_and_cycles_refuse(self):
        for dependencies, expected in (([dependency("example", "absent")], "missing native"),
                                       ([dependency("external", "part")], "sublibrary")):
            package, roster = G.normalized_components(self.metadata([component("a", dependencies=dependencies)]))
            with self.assertRaisesRegex(ValueError, expected):
                G.dependencies_for(roster["a"], roster, package)
        package, roster = G.normalized_components(self.metadata([
            component("a", dependencies=[dependency("example", "b")]),
            component("b", dependencies=[dependency("example", "a")])]))
        with self.assertRaisesRegex(ValueError, "cyclic native"):
            G.installed_link_packages(roster["a"], roster, package)

    def test_stale_source_identity_and_incomplete_configurations_refuse(self):
        value = self.metadata()
        value["source_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "current package source"):
            G.normalized_components(value)
        value = self.metadata()
        value["configurations"].pop()
        with self.assertRaisesRegex(ValueError, "explicit production"):
            G.normalized_components(value)
        value = self.metadata()
        value["platform"] = "unsupported"
        with self.assertRaisesRegex(ValueError, "pinned GHC/native platform"):
            G.normalized_components(value)

    def test_negative_compiler_fixture_is_runtime_resource_without_host_source_edge(self):
        metadata = self.metadata(tests=[component("control", "test-suite", "tests", ("test",), ())])
        package, roster = G.normalized_components(metadata)
        self.assertNotIn("test/Negative.hs", G.fields_for(roster["control"], roster, package)[1].values())
        rendered = G.render(metadata)
        self.assertIn('"test/Negative.hs": "test/Negative.hs"', rendered)
        self.assertIn('labels = ["haskell_component_suite"]', rendered)

    def test_generated_sources_and_workspace_sources_keep_owning_producers(self):
        self.assertEqual(G.source("Tidepool.Internal.ModelControl", ["generated/protocol"])[1],
                         "//bridge/protocol:generated[bridge_haskell_lib_Tidepool_Internal_ModelControl_hs]")
        self.assertEqual(G.source("Project.Checks", ["generated/pinned"])[1],
                         "//bridge/facade:workspace_pinned_check_source")
        self.assertEqual(G.source("Jev.Core.Schema", ["generated/jev/core"])[1],
                         "toolchains//:jev_sources[core_Jev_Core_Schema_hs]")
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            source = workspace / "Project/Control.hs"
            source.parent.mkdir()
            source.write_text("module Project.Control where\n")
            with patch.object(G, "WORKSPACE", workspace):
                self.assertEqual(G.source_location(source),
                                 "//exomonad/examples/workspace:authored_haskell_Project_Control_hs")

    def test_bootstrap_uses_declared_target_without_ambient_parser(self):
        with patch.object(G.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata())
            G.read_metadata()
        arguments = run.call_args.args[0]
        self.assertIn("//build/haskell/cabal-metadata:metadata", arguments)
        self.assertIn("remote.enabled=false", arguments)
        self.assertNotIn("runghc", arguments)


if __name__ == "__main__":
    unittest.main()
