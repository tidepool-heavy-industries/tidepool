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
        # exomonad's flake selection runs the real codex-source-preflight.sh;
        # this test repo carries no vendor/codex, so stub it out. It is only
        # ever invoked for --exomonad, never for the default-shell tests.
        (self.repo / "scripts").mkdir()
        preflight = self.repo / "scripts" / "codex-source-preflight.sh"
        preflight.write_text("#!/usr/bin/env bash\nexit 0\n")
        preflight.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("TIDEPOOL_DEV_")}
        self.env.update(PATH=f"{self.bin}:{os.environ['PATH']}", IN_NIX_SHELL="impure")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], text=True).strip()

    def run_shell(self, *args):
        return subprocess.run(["bash", str(SCRIPT), *args], cwd=self.repo, env=self.env, text=True, capture_output=True)

    def synthetic_default_revision(self):
        """Recompute the default shell's toolchain-input pin the same way dev-shell.sh does."""
        entries = subprocess.check_output(
            ["git", "-C", str(self.repo), "ls-tree", "HEAD", "--",
             "flake.nix", "flake.lock", "rust-toolchain.toml", "nix"],
            text=True,
        )
        tree = subprocess.run(
            ["git", "-C", str(self.repo), "mktree"], input=entries, text=True,
            capture_output=True, check=True,
        ).stdout.strip()
        env = dict(os.environ, GIT_AUTHOR_DATE="@0 +0000", GIT_COMMITTER_DATE="@0 +0000",
                   GIT_AUTHOR_NAME="dev-shell", GIT_AUTHOR_EMAIL="dev-shell@invalid",
                   GIT_COMMITTER_NAME="dev-shell", GIT_COMMITTER_EMAIL="dev-shell@invalid")
        return subprocess.run(
            ["git", "-C", str(self.repo), "commit-tree", tree, "-m", "dev-shell toolchain-input pin"],
            text=True, capture_output=True, check=True, env=env,
        ).stdout.strip()

    def test_pins_git_and_preserves_cwd_despite_false_shell_marker(self):
        artifacts = self.repo / ".exomonad/build/cargo"
        artifacts.mkdir(parents=True)
        (artifacts / "untracked").write_bytes(b"artifact")
        result = self.run_shell("--exomonad", "ghc", "--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        selection = json.loads(result.stdout)
        # exomonad forces the codex path input, which needs the real worktree, so it stays pinned to HEAD.
        self.assertEqual(selection["args"][:2], ["develop", f"git+file://{self.repo}/.git?rev={self.git('rev-parse', 'HEAD')}#exomonad"])
        self.assertEqual(selection["cwd"], str(self.repo))

    def test_default_shell_pins_toolchain_inputs_not_head(self):
        result = self.run_shell("ghc", "--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        selection = json.loads(result.stdout)
        revision = self.synthetic_default_revision()
        self.assertNotEqual(revision, self.git("rev-parse", "HEAD"))
        self.assertEqual(selection["args"][:2], ["develop", f"git+file://{self.repo}/.git?rev={revision}#default"])
        # Running it again reuses the same synthetic revision, and the ref persists.
        result2 = self.run_shell("ghc", "--version")
        selection2 = json.loads(result2.stdout)
        self.assertEqual(selection["args"][:2], selection2["args"][:2])
        self.assertEqual(self.git("rev-parse", f"refs/tidepool/dev-shell/{revision}"), revision)

    def test_dirty_toolchain_requires_explicit_selection(self):
        (self.repo / "flake.nix").write_text("changed inputs\n")
        self.assertEqual(self.run_shell("true").returncode, 2)
        self.env["TIDEPOOL_DEV_FLAKE"] = f"git+file://{self.repo}?rev={self.git('rev-parse', 'HEAD')}"
        self.assertEqual(self.run_shell("true").returncode, 0)

    def test_dirty_nix_directory_requires_explicit_selection_for_default(self):
        nix_dir = self.repo / "nix"
        nix_dir.mkdir()
        (nix_dir / "overlay.nix").write_text("committed\n")
        self.git("add", "nix/overlay.nix")
        self.git("commit", "-qm", "add nix overlay")
        (nix_dir / "overlay.nix").write_text("changed\n")
        self.assertEqual(self.run_shell("true").returncode, 2)

    def test_rejects_path_import(self):
        self.env["TIDEPOOL_DEV_FLAKE"] = "path:."
        self.assertEqual(self.run_shell("true").returncode, 2)

    def test_reuses_only_matching_effective_toolchain(self):
        for tool in ["ghc", "rustc"]:
            path = self.bin / tool
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        revision = self.synthetic_default_revision()
        self.env.update(TIDEPOOL_DEV_SHELL=f"git+file://{self.repo}/.git?rev={revision}#default", TIDEPOOL_DEV_GHC=str(self.bin / "ghc"), TIDEPOOL_DEV_RUSTC=str(self.bin / "rustc"))
        self.assertEqual(self.run_shell("printf", "reused").stdout, "reused")
        self.env["TIDEPOOL_DEV_GHC"] = "/stale/ghc"
        self.assertIn("develop", self.run_shell("true").stdout)


if __name__ == "__main__":
    unittest.main()
