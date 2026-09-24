#!/usr/bin/env python3
"""Regression check: scripts/dev-shell.sh must not leak nix's own temp dirs.

`nix develop --command CMD` execve's CMD in place of the shell it generated
to hold NIX_BUILD_TOP (a `mktemp -d -t nix-shell.XXXXXX` directory), so any
EXIT trap that shell installed to remove that directory never runs and every
invocation leaked a directory under /tmp. dev-shell.sh now points nix's own
mktemp at a private TMPDIR it creates and removes itself once the command
exits (skipping removal only while a live process still has that TMPDIR, for
a detached command such as `just daemon-start`'s persistent daemon keeper).
This test scopes TMPDIR to an isolated directory before running dev-shell.sh
with a command that exits immediately, then asserts nothing was left behind.
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
DEV_SHELL = REPO_ROOT / "scripts" / "dev-shell.sh"


class DevShellTmpCleanup(unittest.TestCase):
    def test_no_tmp_left_after_a_run_that_exits(self):
        scope = Path(tempfile.mkdtemp(prefix="dev-shell-tmp-scope-"))
        self.addCleanup(shutil.rmtree, scope, ignore_errors=True)
        env = os.environ | {"TMPDIR": str(scope)}

        result = subprocess.run(
            ["bash", str(DEV_SHELL), "true"],
            cwd=REPO_ROOT,
            env=env,
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

        leftover = sorted(p.name for p in scope.iterdir())
        self.assertEqual(
            leftover,
            [],
            "dev-shell.sh left temp entries behind: "
            f"{leftover}\nstderr:\n{result.stderr}",
        )

    def test_exit_status_of_the_command_is_preserved(self):
        scope = Path(tempfile.mkdtemp(prefix="dev-shell-tmp-scope-"))
        self.addCleanup(shutil.rmtree, scope, ignore_errors=True)
        env = os.environ | {"TMPDIR": str(scope)}

        result = subprocess.run(
            ["bash", str(DEV_SHELL), "bash", "-c", "exit 7"],
            cwd=REPO_ROOT,
            env=env,
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(sorted(scope.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
