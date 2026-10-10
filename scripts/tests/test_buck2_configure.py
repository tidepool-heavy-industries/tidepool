import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "buck2-configure.sh"
PIN = "git+file:///project?rev=" + "1" * 40 + "&submodules=1"


class ConfigureToolchainTests(unittest.TestCase):
    def setUp(self):
        self.storage = tempfile.TemporaryDirectory()
        self.addCleanup(self.storage.cleanup)
        self.root = Path(self.storage.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.root / "scripts/buck2-configure.sh")
        shutil.copyfile(SCRIPT.parent / "toolchain-inputs.sh", self.root / "scripts/toolchain-inputs.sh")
        (self.root / "scripts/workspace-git-resource.py").write_text("""import os, pathlib, sys
if os.environ.get('TEST_WORKSPACE_FAILURE'):
    sys.exit('workspace resource refused')
pathlib.Path(sys.argv[sys.argv.index('--output') + 1]).mkdir()
print('1' * 40)
""")
        (self.root / "flake.nix").write_text("pinned toolchain\n")
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "-C", str(self.root), "add", "flake.nix"], check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=test", "-c", "user.email=test@invalid", "commit", "-qm", "toolchain"], check=True)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.log = self.root / "nix.log"
        self.executable("mountpoint", "#!/bin/sh\nexit 0\n")
        self.executable("nix-store", f"""#!{sys.executable}
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
root = args[args.index('--add-root') + 1]
Path(root).symlink_to(args[-1])
with open(os.environ['TEST_NIX_LOG'], 'a') as log:
    log.write(json.dumps(['store-root', root, args[-1]]) + '\\n')
""")
        self.outputs = self.root / "outputs"
        self.executable("ghc", '#!/bin/sh\necho "$TEST_GHC_ROOT/lib"\n')
        self.executable("nix", f"""#!{sys.executable}
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with open(os.environ['TEST_NIX_LOG'], 'a') as log:
    log.write(json.dumps(args) + '\\n')
def output(name):
    if name == 'buck-ghc':
        return Path(os.environ['TEST_GHC_ROOT'])
    if name == 'buck-test-ghc':
        return Path(os.environ['TEST_TEST_GHC_ROOT'])
    if name == 'buck-python':
        return Path(sys.executable).resolve().parent.parent
    return Path(os.environ['TEST_OUTPUTS']) / name
if '--impure' in args:
    print('x86_64-linux')
elif args[0] == 'build':
    name = args[-1].split('.')[-1]
    if os.environ.get('TEST_BUILD_FAILURE') == name:
        sys.exit('selected output failed to build')
    target = output(name)
    target.mkdir(parents=True, exist_ok=True)
    if name == 'buck-buck2' and not os.environ.get('TEST_MISSING_BUCK'):
        (target / 'bin').mkdir(exist_ok=True)
        (target / 'bin/buck2').write_text('#!/bin/sh\\nexit 0\\n')
        (target / 'bin/buck2').chmod(0o755)
    if name == 'buck-browser-node' and not os.environ.get('TEST_MISSING_BROWSER_NODE'):
        (target / 'bin').mkdir(exist_ok=True)
        for tool in ('node', 'npm'):
            (target / 'bin' / tool).write_text('#!/bin/sh\\nexit 0\\n')
            (target / 'bin' / tool).chmod(0o755)
    if name == 'buck-lld' and not os.environ.get('TEST_MISSING_LLD'):
        (target / 'bin').mkdir(exist_ok=True)
        (target / 'bin/ld.lld').write_text('#!/bin/sh\\nexit 0\\n')
        (target / 'bin/ld.lld').chmod(0o755)
    link = Path(args[args.index('--out-link') + 1])
    link.symlink_to(target if os.environ.get('TEST_WRONG_ROOT') != name else target.parent)
elif args[0] == 'path-info':
    if not Path(args[-1]).is_dir():
        sys.exit('output unavailable')
    print(args[-1])
elif args[:2] == ['store', 'add-path']:
    target = Path(os.environ['TEST_OUTPUTS']) / args[args.index('--name') + 1]
    target.mkdir(parents=True, exist_ok=True)
    print(target)
else:
    print(output(args[-1].split('.')[-2]))
""")
        self.ghc = self.root / "ghc/bin"
        self.ghc.mkdir(parents=True)
        (self.ghc.parent / "lib").mkdir()
        shutil.copyfile(self.tools / "ghc", self.ghc / "ghc")
        (self.ghc / "ghc").chmod(0o755)

        self.test_ghc = self.root / "test-ghc/bin"
        self.test_ghc.mkdir(parents=True)
        (self.test_ghc.parent / "lib").mkdir()
        (self.test_ghc / "ghc").write_text('#!/bin/sh\necho "$TEST_TEST_GHC_ROOT/lib"\n')
        (self.test_ghc / "ghc").chmod(0o755)

    def executable(self, name, source):
        path = self.tools / name
        path.write_text(source)
        path.chmod(0o755)

    def run_configure(self, *args, pin=PIN, extra_env=None):
        environment = dict(os.environ)
        environment.update(
            PATH=str(self.tools) + os.pathsep + environment["PATH"],
            TIDEPOOL_DEV_SHELL=PIN + "#default",
            TIDEPOOL_DEV_FLAKE=pin,
            TIDEPOOL_BUCK_REMOTE="false",
            TEST_NIX_LOG=str(self.log),
            TEST_OUTPUTS=str(self.outputs),
            TEST_GHC_ROOT=str(self.ghc.parent),
            TEST_TEST_GHC_ROOT=str(self.test_ghc.parent),
        )
        environment.update(extra_env or {})
        return subprocess.run(
            ["bash", str(self.root / "scripts/buck2-configure.sh"), *args],
            env=environment, text=True, capture_output=True,
        )

    def test_unpinned_selection_refuses_before_nix(self):
        result = self.run_configure(extra_env={"TIDEPOOL_DEV_SHELL": str(self.root) + "#default"})
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("revision-pinned", result.stderr)
        self.assertFalse(self.log.exists())

    def test_uncommitted_toolchain_input_refuses_before_nix(self):
        (self.root / "flake.nix").write_text("changed toolchain\n")
        revision = subprocess.check_output(["git", "-C", str(self.root), "rev-parse", "HEAD"], text=True).strip()
        result = self.run_configure(extra_env={
            "TIDEPOOL_DEV_FLAKE": "", "TIDEPOOL_DEV_SHELL": f"git+file://{self.root}?rev={revision}#default",
        })
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("Commit changed toolchain inputs", result.stderr)
        self.assertFalse(self.log.exists())

    def test_explicit_immutable_selection_allows_local_toolchain_edits(self):
        (self.root / "flake.nix").write_text("uncommitted local experiment\n")
        result = self.run_configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        generation = next((self.root / ".buck2-toolchains/generations").iterdir())
        self.assertIn("selection_mode=explicit\n", (generation / "owner").read_text())
        self.assertIn("toolchain_tree=\n", (generation / "owner").read_text())
        self.assertIn(PIN, (generation / "owner").read_text())

    def test_inherited_default_shell_cannot_capture_a_new_pin_as_old_tools(self):
        revision = subprocess.check_output(["git", "-C", str(self.root), "rev-parse", "HEAD"], text=True).strip()
        (self.root / "flake.nix").write_text("new committed toolchain\n")
        subprocess.run(["git", "-C", str(self.root), "add", "flake.nix"], check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=test", "-c", "user.email=test@invalid", "commit", "-qm", "new pin"], check=True)
        result = self.run_configure(extra_env={
            "TIDEPOOL_DEV_FLAKE": "", "TIDEPOOL_DEV_SHELL": f"git+file://{self.root}?rev={revision}#default",
        })
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("inherited dev shell has stale toolchain inputs", result.stderr)
        self.assertFalse(self.log.exists())

    def test_matching_default_shell_selection_configures(self):
        revision = subprocess.check_output(["git", "-C", str(self.root), "rev-parse", "HEAD"], text=True).strip()
        result = self.run_configure(extra_env={
            "TIDEPOOL_DEV_FLAKE": "", "TIDEPOOL_DEV_SHELL": f"git+file://{self.root}?rev={revision}#default",
        })
        self.assertEqual(result.returncode, 0, result.stderr)
        generation = next((self.root / ".buck2-toolchains/generations").iterdir())
        self.assertIn("selection_mode=checkout\n", (generation / "owner").read_text())

    def test_missing_prepared_buck_preserves_prior_configuration(self):
        (self.root / ".buckconfig.local").write_text("previous configuration\n")
        result = self.run_configure(extra_env={"TEST_MISSING_BUCK": "1"})
        self.assert_failed_generation_preserves_config(result)
        self.assertIn("Prepared Buck executable is unavailable", result.stderr)

    def test_ordinary_configuration_does_not_capture_project_resources(self):
        result = self.run_configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        text = (self.root / ".buckconfig.local").read_text()
        self.assertIn("buck2 = " + str(self.outputs / "buck-buck2/bin/buck2"), text)
        self.assertNotIn("runtime_stdlib_", text)
        self.assertNotIn('"flake"', self.log.read_text())
        self.assertNotIn("runtime-stdlib-products", self.log.read_text())

    def test_host_test_closure_is_explicit_and_keeps_production_libdir(self):
        control = self.run_configure()
        self.assertEqual(control.returncode, 0, control.stderr)
        ordinary = (self.root / ".buckconfig.local").read_text()
        self.assertIn("test_ghc = \n", ordinary)
        self.assertNotIn("buck-test-ghc", self.log.read_text())
        result = self.run_configure("--tests")
        self.assertEqual(result.returncode, 0, result.stderr)
        configured = (self.root / ".buckconfig.local").read_text()
        self.assertIn("ghc_libdir = " + str(self.ghc.parent / "lib") + "\n", configured)
        self.assertIn("test_ghc_libdir = " + str(self.test_ghc.parent / "lib") + "\n", configured)
        self.assertIn("test_ghc = " + str(self.test_ghc / "ghc"), configured)
        self.assertIn("haskell_test_closure = " + str(self.outputs / "buck-haskell-test-closure"), configured)
        self.assertIn("jev_sources = " + str(self.outputs / "buck-jev-sources"), configured)

    def test_browser_selection_is_independent_from_host_test_selection(self):
        browser_outputs = ("browser-node", "browser-npm-cache", "playwright-browsers", "browser-test-closure")
        for flags in ((), ("--tests",), ("--browser",), ("--tests", "--browser")):
            with self.subTest(flags=flags):
                self.log.unlink(missing_ok=True)
                result = self.run_configure(*flags)
                self.assertEqual(result.returncode, 0, result.stderr)
                config = (self.root / ".buckconfig.local").read_text()
                calls = [json.loads(line) for line in self.log.read_text().splitlines()]
                selected = {call[-1].split(".")[-1] for call in calls if call[0] == "build"}
                for output in browser_outputs:
                    self.assertEqual("buck-" + output in selected, "--browser" in flags)
                self.assertEqual("buck-test-ghc" in selected, "--tests" in flags)
                if "--browser" in flags:
                    self.assertIn("browser_node = " + str(self.outputs / "buck-browser-node/bin/node"), config)
                    self.assertIn("browser_npm = " + str(self.outputs / "buck-browser-node/bin/npm"), config)
                else:
                    for key in ("browser_node", "browser_npm", "browser_npm_cache", "playwright_browsers", "browser_test_closure"):
                        self.assertIn(key + " = \n", config)

    def test_missing_selected_browser_executables_preserve_prior_configuration(self):
        (self.root / ".buckconfig.local").write_text("previous configuration\n")
        result = self.run_configure("--browser", extra_env={"TEST_MISSING_BROWSER_NODE": "1"})
        self.assert_failed_generation_preserves_config(result)
        self.assertIn("Prepared browser Node/npm executables are unavailable", result.stderr)

    def test_runtime_tools_are_pinned_without_project_catalog_capture(self):
        result = self.run_configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        configured = (self.root / ".buckconfig.local").read_text()
        self.assertIn("exomonad_runtime_tools = " + str(self.outputs / "buck-exomonad-runtime-tools"), configured)
        self.assertNotIn("runtime_stdlib_", configured)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertTrue(any(call[-1].endswith("buck-exomonad-runtime-tools.outPath") for call in calls))
        self.assertFalse(any(call[0] == "flake" for call in calls))

    def test_retired_project_catalog_option_refuses_before_nix_or_generation(self):
        config = self.root / ".buckconfig.local"
        config.write_text("previous configuration\n")
        result = self.run_configure("--runtime-stdlib")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("buck2-configure.sh [--tests]", result.stderr)
        self.assertEqual(config.read_text(), "previous configuration\n")
        self.assertFalse(self.log.exists())
        self.assertFalse((self.root / ".buck2-toolchains").exists())

    def generations(self):
        return sorted((self.root / ".buck2-toolchains/generations").iterdir())

    def test_selected_outputs_have_registered_roots_before_config_is_published(self):
        result = self.run_configure("--tests")
        self.assertEqual(result.returncode, 0, result.stderr)
        generation, = self.generations()
        records = [line.split("\t") for line in (generation / "outputs.tsv").read_text().splitlines()]
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        evaluated = {call[-1].removesuffix(".outPath") for call in calls if call[0] == "eval" and "--impure" not in call}
        packages = [row for row in records if not row[1].startswith('gitlink:')]
        resources = [row for row in records if row[1].startswith('gitlink:')]
        self.assertEqual({reference for _, reference, _, _ in packages}, evaluated)
        self.assertEqual(len(packages), len(evaluated))
        self.assertEqual(len(resources), 1)
        for _, reference, output, root in packages:
            self.assertTrue(Path(root).is_symlink())
            self.assertEqual(str(Path(root).resolve()), output)
            self.assertTrue(any(call[0] == "build" and call[-1] == reference and
                                call[call.index("--out-link") + 1] == root for call in calls))
            self.assertIn(["path-info", "--", output], calls)
        name, reference, output, root = resources[0]
        self.assertEqual(name, 'workspace-git-resource')
        self.assertEqual(reference, 'gitlink:' + '1' * 40)
        self.assertEqual(str(Path(root).resolve()), output)
        self.assertIn(['store-root', root, output], calls)
        self.assertIn('workspace_git_resource = ' + output, (generation / 'config').read_text())
        self.assertEqual((generation / "status").read_text(), "configured\n")
        self.assertEqual((generation / "config").read_bytes(), (self.root / ".buckconfig.local").read_bytes())
        self.assertIn(str(generation), result.stdout)
        self.assertIn("uid=", (generation / "owner").read_text())
        self.assertIn("toolchain_tree=", (generation / "owner").read_text())
        self.assertTrue((generation / "roots/buck-buck2").is_symlink())
        configured = (generation / "config").read_text()
        self.assertIn("test_tools = " + str(self.outputs / "buck-test-tools"), configured)
        self.assertTrue((generation / "roots/buck-test-tools").is_symlink())
        self.assertTrue((generation / "roots/buck-test-tools-closure").is_symlink())

    def test_new_selection_keeps_prior_generation_and_optional_outputs_separate(self):
        result = self.run_configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        old, = self.generations()
        old_config = (old / "config").read_bytes()
        old_records = (old / "outputs.tsv").read_text()
        self.assertNotIn("buck-test-ghc", old_records)
        self.assertNotIn("runtime-stdlib-products", old_records)
        result = self.run_configure("--tests")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.generations()), 2)
        self.assertEqual((old / "config").read_bytes(), old_config)
        for line in old_records.splitlines():
            self.assertTrue(Path(line.split("\t")[3]).is_symlink())
        self.assertNotEqual((self.root / ".buckconfig.local").read_bytes(), old_config)

    def assert_failed_generation_preserves_config(self, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.root / ".buckconfig.local").read_text(), "previous configuration\n")
        generation, = self.generations()
        self.assertTrue((generation / "status").read_text().startswith("failed"))
        self.assertIn(str(generation), result.stderr)
        self.assertFalse((generation / "config").exists())

    def test_build_failure_retains_evidence_without_switching_config(self):
        (self.root / ".buckconfig.local").write_text("previous configuration\n")
        self.assert_failed_generation_preserves_config(
            self.run_configure("--browser", extra_env={"TEST_BUILD_FAILURE": "buck-browser-npm-cache"}))
        generation, = self.generations()
        self.assertIn("buck-browser-npm-cache", (generation / "outputs.tsv").read_text())
        self.assertTrue((generation / "roots/buck-ghc").is_symlink())

    def test_workspace_refusal_preserves_configuration_and_failed_generation(self):
        (self.root / ".buckconfig.local").write_text("previous configuration\n")
        result = self.run_configure(extra_env={"TEST_WORKSPACE_FAILURE": "1"})
        self.assert_failed_generation_preserves_config(result)
        self.assertIn("workspace resource refused", result.stderr)

    def test_wrong_root_refuses_configuration(self):
        (self.root / ".buckconfig.local").write_text("previous configuration\n")
        result = self.run_configure(extra_env={"TEST_WRONG_ROOT": "buck-rust"})
        self.assert_failed_generation_preserves_config(result)
        self.assertIn("output/root mismatch", result.stderr)

    def test_pinned_lld_is_rooted_and_declared_in_action_search_path(self):
        result = self.run_configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        generation, = self.generations()
        configured = (generation / "config").read_text()
        lld = self.outputs / "buck-lld"
        self.assertIn("lld_bin = " + str(lld / "bin"), configured)
        action_path = next(line.split(" = ", 1)[1] for line in configured.splitlines()
                           if line.startswith("action_path = "))
        self.assertIn(str(lld / "bin"), action_path.split(":"))
        self.assertEqual((generation / "roots/buck-lld").resolve(), lld)
        self.assertIn(str(lld), (generation / "outputs.tsv").read_text())

    def test_missing_pinned_lld_refuses_without_ambient_linker_fallback(self):
        (self.root / ".buckconfig.local").write_text("previous configuration\n")
        self.executable("ld.lld", "#!/bin/sh\nexit 0\n")
        result = self.run_configure(extra_env={"TEST_MISSING_LLD": "1"})
        self.assert_failed_generation_preserves_config(result)
        self.assertIn("Prepared pinned LLD executable is unavailable", result.stderr)

    def test_unavailable_ghc_libdir_refuses_configuration(self):
        (self.root / ".buckconfig.local").write_text("previous configuration\n")
        (self.test_ghc.parent / "lib").rmdir()
        result = self.run_configure("--tests")
        self.assert_failed_generation_preserves_config(result)
        self.assertIn("GHC libdir is unavailable", result.stderr)


if __name__ == "__main__":
    unittest.main()
