"""Stubbed checks for Buck configuration publication."""

import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("buck2-configure.sh").resolve()


class BuckConfigureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "scripts").mkdir()
        (self.root / "buck-out").mkdir()
        self.script = self.root / "scripts/buck2-configure.sh"
        shutil.copy2(SCRIPT, self.script)

        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.nix_root = self.root / "nix"
        self.nix_root.mkdir()
        self.program(self.bin / "mountpoint", "exit 0")
        self.program(self.bin / "nix", f'''case "$*" in
  *builtins.currentSystem*) printf '%s\\n' x86_64-linux; exit 0 ;;
esac
for last do :; done
name=${{last##*.buck-}}; name=${{name%.outPath}}
printf '%s\\n' '{self.nix_root}/fake-'"$name"
''')
        ghc = self.nix_root / "fake-ghc/bin/ghc"
        ghc.parent.mkdir(parents=True)
        self.program(ghc, "printf '%s\\n' /fake/libdir")
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        TIDEPOOL_DEV_SHELL="git+file:///stub?rev=stub#default")

    @staticmethod
    def program(path, body):
        path.write_text("#!/usr/bin/env bash\nset -eu\n" + body + "\n")
        path.chmod(0o755)

    def configure(self):
        return subprocess.run([str(self.script)], cwd=self.root, env=self.env,
                              text=True, capture_output=True)

    def test_published_config_is_readable_by_group(self):
        result = self.configure()
        self.assertEqual(result.returncode, 0, result.stderr)
        mode = stat.S_IMODE((self.root / ".buckconfig.local").stat().st_mode)
        self.assertTrue(mode & stat.S_IRGRP, oct(mode))

    def test_failed_atomic_rename_preserves_previous_config_and_cleans_temp(self):
        destination = self.root / ".buckconfig.local"
        destination.write_bytes(b"previous config\n")
        self.program(self.bin / "mv", "exit 92")
        result = self.configure()
        self.assertEqual(result.returncode, 92, result.stderr)
        self.assertEqual(destination.read_bytes(), b"previous config\n")
        self.assertEqual(list(self.root.glob(".buckconfig.local.*")), [])


if __name__ == "__main__":
    unittest.main()
