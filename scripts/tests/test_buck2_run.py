"""Exercise the Buck runner boundary with stub tools, without starting Buck or Nix."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "buck2-run.sh"


class BuckRunnerTests(unittest.TestCase):
    def setUp(self):
        self.storage = tempfile.TemporaryDirectory()
        self.addCleanup(self.storage.cleanup)
        self.root = Path(self.storage.name) / "checkout with spaces"
        (self.root / "scripts").mkdir(parents=True)
        shutil.copyfile(SCRIPT, self.root / "scripts/buck2-run.sh")
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.action_tools = self.root / "action tools"
        self.action_tools.mkdir()
        self.shell_log = self.root / "dev-shell.log"
        self.buck_log = self.root / "buck.json"
        self.config = self.root / ".buckconfig.local"
        self.config.write_text(f"[nix]\naction_path = {self.action_tools}\n")
        self.program(self.tools / "mountpoint", 'exit "$MOUNT_EXIT"')
        self.program(
            self.root / "scripts/dev-shell.sh",
            'printf "%s\\n" "$@" > "$DEV_SHELL_LOG"\nexec "$@"',
        )
        buck = self.tools / "buck2"
        buck.write_text(f"""#!{sys.executable}
import json, os, sys
from pathlib import Path
Path(os.environ["BUCK_LOG"]).write_text(json.dumps({{
    "args": sys.argv[1:],
    "cwd": os.getcwd(),
    "path": os.environ["PATH"],
    "shell": os.environ.get("TIDEPOOL_BUCK_SHELL"),
}}))
""")
        buck.chmod(0o755)
        self.program(self.action_tools / "buck2", "exit 95")
        self.env = dict(os.environ)
        self.env.pop("TIDEPOOL_BUCK_SHELL", None)
        self.env.update(
            PATH=str(self.tools) + os.pathsep + self.env["PATH"],
            MOUNT_EXIT="0",
            DEV_SHELL_LOG=str(self.shell_log),
            BUCK_LOG=str(self.buck_log),
        )

    def program(self, path, body):
        path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + body + "\n")
        path.chmod(0o755)

    def run_runner(self, *args, **environment):
        return subprocess.run(
            ["bash", str(self.root / "scripts/buck2-run.sh"), *args],
            cwd="/",
            env=dict(self.env, **environment),
            text=True,
            capture_output=True,
        )

    def assert_no_shell_or_buck(self):
        self.assertFalse(self.shell_log.exists())
        self.assertFalse(self.buck_log.exists())

    def test_missing_mount_fails_before_entering_dev_shell(self):
        result = self.run_runner("build", "//pkg:target", MOUNT_EXIT="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Buck requires the per-checkout buck-out bind mount", result.stderr)
        self.assert_no_shell_or_buck()

    def test_missing_config_fails_before_entering_dev_shell(self):
        self.config.unlink()
        result = self.run_runner("build", "//pkg:target")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Run scripts/buck2-configure.sh first", result.stderr)
        self.assert_no_shell_or_buck()

    def test_valid_setup_forwards_arguments_and_declared_action_path(self):
        args = ["build", "--local-only", "-c", "remote.enabled=false", "//pkg:target", "with spaces", ""]
        result = self.run_runner(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.shell_log.read_text().splitlines(),
            ["env", "TIDEPOOL_BUCK_SHELL=ready", "bash", "scripts/buck2-run.sh", *args],
        )
        self.assertEqual(json.loads(self.buck_log.read_text()), {
            "args": args,
            "cwd": str(self.root),
            "path": f"{self.action_tools}:/run/current-system/sw/bin",
            "shell": "ready",
        })

    def test_ready_shell_preserves_explicit_remote_arguments(self):
        args = ["test", "--remote-only", "-c", "remote.enabled=true", "//pkg:target"]
        result = self.run_runner(*args, TIDEPOOL_BUCK_SHELL="ready")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.shell_log.exists())
        self.assertEqual(json.loads(self.buck_log.read_text())["args"], args)


if __name__ == "__main__":
    unittest.main()
