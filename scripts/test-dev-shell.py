"""Focused regression tests for committed-source dev-shell selection."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("dev-shell.sh").resolve()


class DevShellTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        (self.repo / "flake.nix").write_text("committed inputs\n")
        self.git("add", "flake.nix")
        self.git("commit", "-qm", "seed")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        nix = self.bin / "nix"
        nix.write_text("#!/usr/bin/env python3\nimport json,os,sys\nprint(json.dumps({'args':sys.argv[1:],'cwd':os.getcwd()}))\n")
        nix.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("TIDEPOOL_DEV_")}
        self.env.update(PATH=f"{self.bin}:{os.environ['PATH']}", IN_NIX_SHELL="impure")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], text=True).strip()

    def run_shell(self, *args):
        return subprocess.run(["bash", str(SCRIPT), *args], cwd=self.repo, env=self.env, text=True, capture_output=True)

    def test_pins_git_and_preserves_cwd_despite_false_shell_marker(self):
        artifacts = self.repo / ".shoal/build/cargo"
        artifacts.mkdir(parents=True)
        (artifacts / "untracked").write_bytes(b"artifact")
        result = self.run_shell("--shoal", "ghc", "--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        selection = json.loads(result.stdout)
        self.assertEqual(selection["args"][:2], ["develop", f"git+file://{self.repo}/.git?rev={self.git('rev-parse', 'HEAD')}#shoal"])
        self.assertEqual(selection["cwd"], str(self.repo))

    def test_dirty_toolchain_requires_explicit_selection(self):
        (self.repo / "flake.nix").write_text("changed inputs\n")
        self.assertEqual(self.run_shell("true").returncode, 2)
        self.env["TIDEPOOL_DEV_FLAKE"] = f"git+file://{self.repo}?rev={self.git('rev-parse', 'HEAD')}"
        self.assertEqual(self.run_shell("true").returncode, 0)

    def test_rejects_path_import(self):
        self.env["TIDEPOOL_DEV_FLAKE"] = "path:."
        self.assertEqual(self.run_shell("true").returncode, 2)

    def test_reuses_only_matching_effective_toolchain(self):
        for tool in ["ghc", "rustc"]:
            path = self.bin / tool
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        self.env.update(TIDEPOOL_DEV_SHELL=f"git+file://{self.repo}/.git?rev={self.git('rev-parse', 'HEAD')}#default", TIDEPOOL_DEV_GHC=str(self.bin / "ghc"), TIDEPOOL_DEV_RUSTC=str(self.bin / "rustc"))
        self.assertEqual(self.run_shell("printf", "reused").stdout, "reused")
        self.env["TIDEPOOL_DEV_GHC"] = "/stale/ghc"
        self.assertIn("develop", self.run_shell("true").stdout)


if __name__ == "__main__":
    unittest.main()
