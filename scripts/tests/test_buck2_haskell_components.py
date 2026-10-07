"""Validate native component projection without compiling or parsing Cabal."""
import ast
import copy
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
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


class SourceExportFilenameTests(unittest.TestCase):
    def assert_compiler_sources(self, exports):
        self.assertTrue(exports)
        for export in exports:
            with self.subTest(source=export["src"]):
                output = export["src"] if export.get("mode") == "reference" else export.get("out", export["name"])
                self.assertTrue(output.endswith((".hs", ".lhs", ".hs-boot", ".lhs-boot")), output)
                self.assertEqual(Path(output).name, Path(export["src"]).name)

    def test_authored_projection_preserves_haskell_and_boot_source_filenames(self):
        exports = []
        environment = {"load": lambda *args: None, "export_file": lambda **rule: exports.append(rule)}
        path = SCRIPT.parents[1] / "exomonad/examples/workspace/authored_sources.bzl"
        exec(compile(path.read_text(), str(path), "exec"), environment)
        environment["declare_authored_haskell_sources"]([
            ".exomonad/Exomonad/Contrib/Types.hs", ".exomonad/Jev/Operators.hs",
            ".exomonad/Project/Literate.lhs", ".exomonad/Project/Cycle.hs-boot",
            ".exomonad/Project/LiterateCycle.lhs-boot",
        ])
        self.assert_compiler_sources(exports)

    def test_workspace_runtime_resources_have_owning_native_exports(self):
        workspace_package = G.WORKSPACE.parent
        exports = []
        environment = {
            "load": lambda *args: None,
            "export_file": lambda **rule: exports.append(rule),
            "filegroup": lambda **rule: None,
            "glob": lambda patterns: sorted({
                path.relative_to(workspace_package).as_posix()
                for pattern in patterns for path in workspace_package.glob(pattern)
                if path.is_file()
            }),
        }
        for filename in ("authored_sources.bzl", "BUCK"):
            path = workspace_package / filename
            exec(compile(path.read_text(), str(path), "exec"), environment)
        declared = {"//exomonad/examples/workspace:" + rule["name"]: rule for rule in exports}
        resources = [path for path in G.WORKSPACE.rglob("*")
                     if path.is_file() and path.suffix in {".hs", ".hs-boot", ".json", ".cbor", ".txt"}]
        self.assertTrue(any(path.name == "usage-examples.json" for path in resources))
        for path in resources:
            with self.subTest(resource=path.relative_to(G.WORKSPACE)):
                export = declared[G.source_location(path)]
                self.assertEqual(export["src"], path.relative_to(workspace_package).as_posix())
                self.assertEqual(export["out"], path.name)

    def test_static_source_exports_preserve_compiler_filenames(self):
        exports = []
        for relative in ("bridge/haskell/BUCK", "exomonad/examples/workspace/BUCK"):
            tree = ast.parse((SCRIPT.parents[1] / relative).read_text())
            for statement in tree.body:
                if not isinstance(statement, ast.Expr) or not isinstance(statement.value, ast.Call):
                    continue
                call = statement.value
                if not isinstance(call.func, ast.Name) or call.func.id != "export_file":
                    continue
                export = {argument.arg: ast.literal_eval(argument.value) for argument in call.keywords}
                if export["src"].endswith((".hs", ".lhs", ".hs-boot", ".lhs-boot")):
                    exports.append(export)
        self.assert_compiler_sources(exports)


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
        self.assertEqual(after.count('"-optc"'), 4)  # Two ordered options at compile and link.
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
        self.assertIn("PATH:search-list TIDEPOOL_GHC_LIBDIR:directory "
                      "TIDEPOOL_TEST_EFFECTS_DIR:directory TIDEPOOL_PRELUDE_DIR:directory "
                      "TIDEPOOL_TEST_PYTHON:executable", rendered)

    def test_prepared_products_use_declared_issuer_and_matched_compiler_inputs(self):
        metadata = self.metadata(tests=[component("prepared-stg-pipeline-test", "test-suite",
                                                 "tests", ("test",), ())])
        rendered = G.render(metadata)
        self.assertIn('"TIDEPOOL_CANDIDATE_FIXTURE_ISSUER": "$(exe //tidepool/toolchain:candidate_fixture_issuer)"', rendered)
        self.assertIn("TIDEPOOL_CANDIDATE_FIXTURE_ISSUER:executable", rendered)
        self.assertIn('"//tidepool/toolchain:candidate_fixture_issuer"', rendered)
        self.assertIn('"TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)"', rendered)
        self.assertIn("TIDEPOOL_COMPILER_DEPLOYMENT:file", rendered)
        self.assertIn('"TIDEPOOL_EXTRACT_WORKER": "$(exe :tidepool_extract_bin)"', rendered)
        self.assertIn("TIDEPOOL_EXTRACT_WORKER:executable", rendered)
        self.assertIn('"//build/package:tidepool_extract_runtime_libraries"', rendered)

    def test_generated_sources_and_workspace_sources_keep_owning_producers(self):
        self.assertEqual(G.source("Tidepool.Internal.ActorProfiles", ["generated/protocol"])[1],
                         "//bridge/protocol:generated[bridge_haskell_lib_Tidepool_Internal_ActorProfiles_hs]")
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

    def test_catalog_projects_all_runtime_source_owners_without_test_packages(self):
        for relative in ("lib/Library.hs", "actors/Actor.hs", "test-support/Runner.hs"):
            path = self.package / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("module Example where\n")
        helper = component("native-helper-contract", "test-suite", "tests",
                           ("lib", "actors", "test-support", "generated/effects"),
                           ("Library", "Actor", "Runner", "Tidepool.Effects.Core"),
                           [dependency("tasty"), dependency("text")])
        _, roster = G.normalized_components(self.metadata(tests=[helper]))
        cohort = G.native_catalog_cohort(roster)
        self.assertEqual(cohort, {"components": ["native-helper-contract"], "modules": {
            "Actor": "actors/Actor.hs", "Library": "lib/Library.hs",
            "Tidepool.Effects": "effects/Tidepool/Effects.hs",
            "Tidepool.Effects.Authored": "effects/Tidepool/Effects/Authored.hs",
            "Tidepool.Effects.Core": "effects/Tidepool/Effects/Core.hs",
        }})
        self.assertIn("Tidepool.Effects", cohort["modules"])
        self.assertNotIn("dependencies", cohort)
        helper["modules"].append("Tidepool.Effects")
        _, roster = G.normalized_components(self.metadata(tests=[helper]))
        self.assertEqual(G.native_catalog_cohort(roster), cohort)

        helper["modules"].pop()
        helper["source_dirs"].append("test")
        helper["modules"].append("Negative")
        _, roster = G.normalized_components(self.metadata(tests=[helper]))
        self.assertEqual(G.native_catalog_cohort(roster), cohort)

    def test_catalog_joins_runtime_owners_across_components_with_pinned_jev(self):
        for relative in ("lib/Library.hs", "actors/Actor.hs"):
            path = self.package / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("module Example where\n")
        helpers = component("native-helper-contract", "test-suite", "tests", ("lib",),
                            ("Library",), [dependency("tasty")])
        actors = component("actor-contract", "test-suite", "tests",
                           ("actors", "generated/jev/core"), ("Actor", "Jev.Core.Json"),
                           [dependency("tasty-hunit")], ghc_options=["-DTEST_ONLY"])
        _, roster = G.normalized_components(self.metadata(tests=[helpers, actors]))
        cohort = G.native_catalog_cohort(roster)
        self.assertEqual(cohort["components"], ["actor-contract", "native-helper-contract"])
        self.assertEqual(cohort["modules"]["Actor"], "actors/Actor.hs")
        self.assertEqual(cohort["modules"]["Jev.Core.Json"], "jev/core/Jev/Core/Json.hs")
        self.assertEqual(set(cohort), {"components", "modules"})


class FixtureInputBoundaryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.wrapper = SCRIPT.parents[1] / "bridge/haskell/test-with-fixtures.sh"
        self.fixtures = self.root / "fixture tree"
        self.fixtures.mkdir()
        (self.fixtures / "input").write_text("immutable fixture")
        self.directory = self.root / "declared directory"
        self.directory.mkdir()
        self.file = self.root / "declared file"
        self.file.write_text("original input")
        self.tool = self.root / "declared tool"
        self.tool.write_text("#!/usr/bin/env bash\nprintf 'declared tool result'\n")
        self.tool.chmod(0o755)
        self.record = self.root / "result.json"
        self.binary = self.root / "test binary"
        self.binary.write_text("""#!/usr/bin/env bash
set -euo pipefail
[[ "$("$DECLARED_EXECUTABLE")" == 'declared tool result' ]]
"$TIDEPOOL_TEST_PYTHON" - "$TEST_RECORD" "$@" <<'PYPROBE'
import json, os, pathlib, sys
names = ['DECLARED_DIRECTORY', 'DECLARED_FILE', 'DECLARED_EXECUTABLE',
         'ORDERED_SEARCH', 'ORDERED_LIBRARIES', 'SCALAR_VALUE', 'TASTY_PATTERN']
pathlib.Path(sys.argv[1]).write_text(json.dumps(dict(
    cwd=os.getcwd(), args=sys.argv[2:], producer=os.environ.get("TIDEPOOL_COMPILER_PRODUCER"), values={name: os.environ[name] for name in names})))
pathlib.Path('input').write_text('private mutation')
PYPROBE
""")
        self.binary.chmod(0o755)
        self.env = os.environ | {
            "DECLARED_DIRECTORY": self.directory.name,
            "DECLARED_FILE": self.file.name,
            "DECLARED_EXECUTABLE": self.tool.name,
            "ORDERED_SEARCH": "first::/absolute:last:$ORIGIN/lib:",
            "ORDERED_LIBRARIES": "libdir:$ORIGIN/lib::${LIB}/suffix:$PLATFORM",
            "SCALAR_VALUE": "retain this scalar: exactly",
            "TASTY_PATTERN": "/canonical current source/",
            "TIDEPOOL_TEST_PYTHON": sys.executable,
            "TIDEPOOL_TEST_INPUT_PATHS": "DECLARED_DIRECTORY:directory DECLARED_FILE:file "
                "DECLARED_EXECUTABLE:executable ORDERED_SEARCH:search-list "
                "ORDERED_LIBRARIES:library-search-list "
                "TIDEPOOL_TEST_PYTHON:executable",
            "TEST_RECORD": str(self.record),
        }
        # This is a runner boundary probe, independent of compiler issuance.
        self.env.pop("TIDEPOOL_CANDIDATE_FIXTURE_ISSUER", None)
        self.env.pop("TIDEPOOL_COMPILER_DEPLOYMENT", None)
        self.env.pop("TIDEPOOL_COMPILER_PRODUCER", None)

    def run_wrapper(self, env):
        return subprocess.run(['bash', str(self.wrapper), self.binary.name, self.fixtures.name,
                               '--pattern', '/canonical current source/'], cwd=self.root,
                              env=env, text=True, capture_output=True, timeout=10)

    def test_relative_declared_inputs_survive_private_cwd_and_preserve_search_order(self):
        result = self.run_wrapper(self.env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        record = json.loads(self.record.read_text())
        self.assertEqual(record['values']['DECLARED_DIRECTORY'], str(self.directory))
        self.assertEqual(record['values']['DECLARED_FILE'], str(self.file))
        self.assertEqual(record['values']['DECLARED_EXECUTABLE'], str(self.tool))
        self.assertEqual(record['values']['ORDERED_SEARCH'], ':'.join([
            str(self.root / 'first'), str(self.root) + '/.', '/absolute',
            str(self.root / 'last'), str(self.root / '$ORIGIN/lib'), str(self.root) + '/.']))
        self.assertEqual(record['values']['ORDERED_LIBRARIES'], ':'.join([
            str(self.root / 'libdir'), '$ORIGIN/lib', str(self.root) + '/.', '${LIB}/suffix', '$PLATFORM']))
        self.assertEqual(record['values']['SCALAR_VALUE'], self.env['SCALAR_VALUE'])
        self.assertEqual(record['values']['TASTY_PATTERN'], self.env['TASTY_PATTERN'])
        self.assertEqual(record['args'], ['--pattern', '/canonical current source/'])
        self.assertNotEqual(record['cwd'], str(self.root))
        self.assertFalse(Path(record['cwd']).exists())
        self.assertEqual((self.fixtures / 'input').read_text(), 'immutable fixture')

    def test_codec_issuer_does_not_require_or_inherit_compiler_authority(self):
        env = self.env | {
            "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER": self.tool.name,
            "TIDEPOOL_COMPILER_DEPLOYMENT": "undeclared ambient deployment",
            "TIDEPOOL_COMPILER_PRODUCER": "ambient producer",
            "TIDEPOOL_TEST_INPUT_PATHS": self.env["TIDEPOOL_TEST_INPUT_PATHS"]
                + " TIDEPOOL_CANDIDATE_FIXTURE_ISSUER:executable",
        }
        result = self.run_wrapper(env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNone(json.loads(self.record.read_text())["producer"])

    def test_genuine_issuer_uses_declared_deployment_identity(self):
        deployment = self.root / "compiler deployment.json"
        deployment.write_text(json.dumps(dict(schema=1, producer_identity=list(range(32)))))
        env = self.env | {
            "TIDEPOOL_COMPILER_DEPLOYMENT": deployment.name,
            "TIDEPOOL_COMPILER_PRODUCER": "ambient producer",
            "TIDEPOOL_TEST_INPUT_PATHS": self.env["TIDEPOOL_TEST_INPUT_PATHS"]
                + " TIDEPOOL_COMPILER_DEPLOYMENT:file",
        }
        result = self.run_wrapper(env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(self.record.read_text())["producer"], bytes(range(32)).hex())

    def test_declared_genuine_deployment_missing_or_wrong_role_refuses(self):
        for changed in [
            {}, {"TIDEPOOL_COMPILER_DEPLOYMENT": "missing"},
            {"TIDEPOOL_COMPILER_DEPLOYMENT": self.directory.name,
             "TIDEPOOL_TEST_INPUT_PATHS": self.env["TIDEPOOL_TEST_INPUT_PATHS"]
                 + " TIDEPOOL_COMPILER_DEPLOYMENT:directory"},
        ]:
            with self.subTest(changed=changed):
                env = self.env | {
                    "TIDEPOOL_TEST_INPUT_PATHS": self.env["TIDEPOOL_TEST_INPUT_PATHS"]
                        + " TIDEPOOL_COMPILER_DEPLOYMENT:file",
                } | changed
                result = self.run_wrapper(env)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.record.exists())

    def test_absolute_declared_inputs_keep_their_artifact_roles(self):
        env = self.env | {
            'DECLARED_DIRECTORY': str(self.directory), 'DECLARED_FILE': str(self.file),
            'DECLARED_EXECUTABLE': str(self.tool), 'ORDERED_SEARCH': '/first:/second',
        }
        result = self.run_wrapper(env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        values = json.loads(self.record.read_text())['values']
        self.assertEqual(values['ORDERED_SEARCH'], env['ORDERED_SEARCH'])
        for name in ('DECLARED_DIRECTORY', 'DECLARED_FILE', 'DECLARED_EXECUTABLE'):
            self.assertEqual(values[name], env[name])

    def test_absent_input_wrong_artifact_role_and_unknown_role_refuse_before_execution(self):
        for changed in [
            {'DECLARED_FILE': 'missing'}, {'DECLARED_FILE': self.directory.name},
            {'DECLARED_EXECUTABLE': self.file.name},
            {'TIDEPOOL_TEST_INPUT_PATHS': 'DECLARED_FILE:unknown'},
            {'TIDEPOOL_TEST_INPUT_PATHS': 'ABSENT_INPUT:file'},
        ]:
            with self.subTest(changed=changed):
                result = self.run_wrapper(self.env | changed)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.record.exists())


if __name__ == "__main__":
    unittest.main()
