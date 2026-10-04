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


class ConfigureRuntimeStdlibTests(unittest.TestCase):
    def setUp(self):
        self.storage = tempfile.TemporaryDirectory()
        self.addCleanup(self.storage.cleanup)
        self.root = Path(self.storage.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.root / "scripts/buck2-configure.sh")
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.capture = self.root / "captured-source"
        self.capture.mkdir()
        self.log = self.root / "nix.log"
        self.executable("mountpoint", "#!/bin/sh\nexit 0\n")
        self.executable("ghc", "#!/bin/sh\necho /nix/store/checked-ghc/lib\n")
        self.executable("nix", f"""#!{sys.executable}
import json, os, sys
from pathlib import Path
with open(os.environ['TEST_NIX_LOG'], 'a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
if sys.argv[1] == 'flake':
    print(json.dumps({{'path': os.environ['TEST_SOURCE_CAPTURE']}}))
elif '--impure' in sys.argv:
    print('x86_64-linux')
else:
    name = sys.argv[-1].split('.')[-2]
    if name == 'buck-ghc':
        print(os.environ['TEST_GHC_ROOT'])
    elif name == 'buck-test-ghc':
        print(os.environ['TEST_TEST_GHC_ROOT'])
    elif name == 'buck-python':
        print(str(Path(sys.executable).parent.parent))
    else:
        print('/nix/store/checked-' + name)
""")
        self.ghc = self.root / "ghc/bin"
        self.ghc.mkdir(parents=True)
        shutil.copyfile(self.tools / "ghc", self.ghc / "ghc")
        (self.ghc / "ghc").chmod(0o755)

        self.test_ghc = self.root / "test-ghc/bin"
        self.test_ghc.mkdir(parents=True)
        (self.test_ghc / "ghc").write_text("#!/bin/sh\necho /nix/store/checked-test-ghc/lib\n")
        (self.test_ghc / "ghc").chmod(0o755)

    def executable(self, name, source):
        path = self.tools / name
        path.write_text(source)
        path.chmod(0o755)

    def run_configure(self, *args, pin=PIN):
        environment = dict(os.environ)
        environment.update(
            PATH=str(self.tools) + os.pathsep + environment["PATH"],
            TIDEPOOL_DEV_SHELL=PIN + "#default",
            TIDEPOOL_DEV_FLAKE=pin,
            TIDEPOOL_BUCK_REMOTE="false",
            TEST_NIX_LOG=str(self.log),
            TEST_SOURCE_CAPTURE=str(self.capture),
            TEST_GHC_ROOT=str(self.ghc.parent),
            TEST_TEST_GHC_ROOT=str(self.test_ghc.parent),
        )
        return subprocess.run(
            ["bash", str(self.root / "scripts/buck2-configure.sh"), *args],
            env=environment, text=True, capture_output=True,
        )

    def test_ordinary_configuration_does_not_capture_project_resources(self):
        result = self.run_configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        text = (self.root / ".buckconfig.local").read_text()
        self.assertIn("runtime_stdlib_products = \n", text)
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
        self.assertIn("ghc_libdir = /nix/store/checked-ghc/lib\n", configured)
        self.assertIn("test_ghc_libdir = /nix/store/checked-test-ghc/lib\n", configured)
        self.assertIn("test_ghc = " + str(self.test_ghc / "ghc"), configured)
        self.assertIn("haskell_test_closure = /nix/store/checked-buck-haskell-test-closure", configured)
        self.assertIn("jev_sources = /nix/store/checked-buck-jev-sources", configured)

    def test_runtime_tools_are_pinned_without_project_catalog_capture(self):
        result = self.run_configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        configured = (self.root / ".buckconfig.local").read_text()
        self.assertIn("exomonad_runtime_tools = /nix/store/checked-buck-exomonad-runtime-tools", configured)
        self.assertIn("runtime_stdlib_products = \n", configured)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertTrue(any(call[-1].endswith("buck-exomonad-runtime-tools.outPath") for call in calls))
        self.assertFalse(any(call[0] == "flake" for call in calls))

    def test_reduced_toolchain_capture_refuses_project_resource_selection(self):
        result = self.run_configure("--runtime-stdlib")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("reduced toolchain flake", result.stderr)
        self.assertFalse((self.root / ".buckconfig.local").exists())

    def test_project_resource_selection_records_one_pinned_source(self):
        (self.capture / "bridge/haskell/lib").mkdir(parents=True)
        producer = self.capture / "tidepool/toolchain/src/bin/tidepool-module-package.rs"
        producer.parent.mkdir(parents=True)
        producer.write_text("fn main() {}\n")
        result = self.run_configure("--runtime-stdlib")
        self.assertEqual(result.returncode, 0, result.stderr)
        text = (self.root / ".buckconfig.local").read_text()
        self.assertIn("runtime_stdlib_sources = /nix/store/checked-runtime-stdlib-sources", text)
        self.assertIn("runtime_stdlib_products = /nix/store/checked-runtime-stdlib-products", text)
        self.assertIn("runtime_stdlib_extract = /nix/store/checked-tidepool-extract", text)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        selected = [call[-1] for call in calls if "runtime-stdlib" in call[-1]]
        self.assertTrue(selected)
        self.assertTrue(all(reference.startswith(PIN + "#") for reference in selected))

    def test_unpinned_worktree_cannot_supply_project_resources(self):
        result = self.run_configure("--runtime-stdlib", pin=str(self.root))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("revision-pinned", result.stderr)
        self.assertNotIn('"flake"', self.log.read_text())


if __name__ == "__main__":
    unittest.main()
