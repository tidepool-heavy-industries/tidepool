#!/usr/bin/env python3
"""Checkout ownership and dev-shell admission without compiling or entering Nix."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("cargo_target", ROOT / "scripts/cargo-target.py")
target = importlib.util.module_from_spec(spec)
spec.loader.exec_module(target)


class TargetOwnership(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.checkout = self.base / "checkout"
        self.checkout.mkdir()

    def test_default_and_owned_targets_preserve_layout(self):
        self.assertEqual(target.resolve_target(self.checkout), self.checkout / "target")
        self.assertEqual(target.resolve_target(self.checkout, "build", self.checkout),
                         self.checkout / "build")

    def test_external_base_separates_checkouts_and_is_idempotent(self):
        external = self.base / "build"
        first = target.resolve_target(self.checkout, external)
        second_checkout = self.base / "second"
        second_checkout.mkdir()
        second = target.resolve_target(second_checkout, external)
        self.assertNotEqual(first, second)
        self.assertEqual(first.parent, external)
        self.assertEqual(target.resolve_target(self.checkout, first), first)
        self.assertFalse(external.exists(), "resolution must not claim or mutate output")

    def test_checkout_aliases_have_one_canonical_identity(self):
        alias = self.base / "alias"
        alias.symlink_to(self.checkout, target_is_directory=True)
        external = self.base / "build"
        self.assertEqual(target.resolve_target(alias, external),
                         target.resolve_target(self.checkout, external))

    def test_target_in_another_git_checkout_is_refused(self):
        other = self.base / "other"
        subprocess.run(["git", "init", "-q", str(other)], check=True)
        with self.assertRaisesRegex(ValueError, "belongs to another checkout"):
            target.resolve_target(self.checkout, other / "target" / "not-created")

    def test_nested_git_checkout_keeps_its_own_target(self):
        nested = self.checkout / "nested"
        subprocess.run(["git", "init", "-q", str(nested)], check=True)
        with self.assertRaisesRegex(ValueError, "belongs to another checkout"):
            target.resolve_target(self.checkout, nested / "target")

    def test_symlink_into_another_checkout_cannot_hide_target_ownership(self):
        other = self.base / "other"
        subprocess.run(["git", "init", "-q", str(other)], check=True)
        (self.checkout / "target").symlink_to(other / "target", target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "belongs to another checkout"):
            target.resolve_target(self.checkout, self.checkout / "target")
        with self.assertRaisesRegex(ValueError, "belongs to another checkout"):
            target.resolve_target(self.checkout)

    def test_fast_dev_shell_path_still_enforces_and_exports_target(self):
        fake_tools = self.base / "bin"
        fake_tools.mkdir()
        for tool in ("ghc", "rustc"):
            executable = fake_tools / tool
            executable.write_text("#!/bin/sh\nexit 0\n")
            executable.chmod(0o755)
        env = os.environ | {
            "PATH": str(fake_tools) + os.pathsep + os.environ["PATH"],
            "TIDEPOOL_DEV_FLAKE": "/nix/store/test-only-toolchain",
            "TIDEPOOL_DEV_SHELL": "/nix/store/test-only-toolchain#default",
            "TIDEPOOL_DEV_GHC": str(fake_tools / "ghc"),
            "TIDEPOOL_DEV_RUSTC": str(fake_tools / "rustc"),
            "CARGO_TARGET_DIR": str(self.base / "external"),
        }
        command = ["bash", "scripts/dev-shell.sh", sys.executable, "-c",
                   'import os; print(os.environ["CARGO_TARGET_DIR"])']
        result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = str(target.resolve_target(ROOT, self.base / "external"))
        self.assertEqual(result.stdout.strip(), expected)
        self.assertIn(expected, result.stderr)
        other = self.base / "other"
        subprocess.run(["git", "init", "-q", str(other)], check=True)
        env["CARGO_TARGET_DIR"] = str(other / "target")
        result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("belongs to another checkout", result.stderr)

        # The supported env prefix runs after the shell entrypoint unless its
        # assignments are normalized there. The last assignment wins.
        external = self.base / "prefix-external"
        env["CARGO_TARGET_DIR"] = str(other / "target")
        prefix_command = ["bash", "scripts/dev-shell.sh", "env", "--",
                          "CARGO_TARGET_DIR=" + str(other / "target"),
                          "CARGO_TARGET_DIR=" + str(external), "PRESERVED=value",
                          sys.executable, "-c",
                          'import os; print(os.environ["CARGO_TARGET_DIR"]); '
                          'print(os.environ["PRESERVED"])']
        result = subprocess.run(prefix_command, cwd=ROOT, env=env,
                                capture_output=True, text=True)
        expected = str(target.resolve_target(ROOT, external))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [expected, "value"])
        self.assertIn(expected, result.stderr)

        prefix_command[4] = "CARGO_TARGET_DIR=" + str(other / "target")
        prefix_command.pop(5)
        result = subprocess.run(prefix_command, cwd=ROOT, env=env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("belongs to another checkout", result.stderr)

        for option in (["-i"], ["-u", "CARGO_TARGET_DIR"],
                       ["--unset=CARGO_TARGET_DIR"]):
            command = ["bash", "scripts/dev-shell.sh", "env", *option,
                       sys.executable, "-c",
                       'import os; print(os.environ["CARGO_TARGET_DIR"])']
            result = subprocess.run(command, cwd=ROOT, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(ROOT / "target"))

        command = ["bash", "scripts/dev-shell.sh", "env", "-C", str(self.base),
                   "CARGO_TARGET_DIR=relative", sys.executable, "-c", "pass"]
        result = subprocess.run(command, cwd=ROOT, env=env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("unsupported env option -C", result.stderr)


if __name__ == "__main__":
    unittest.main()
