"""Exercise retained-generation launch without starting Buck, Nix or a shell."""

import json
import os
from pathlib import Path
import shutil
import signal
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
        for script in ("buck2-run.sh", "toolchain-inputs.sh", "buck2-config-args.py"):
            shutil.copyfile(SCRIPT.parent / script, self.root / "scripts" / script)
        self.git = shutil.which("git")
        self.git_command("init", "-q")
        (self.root / "flake.nix").write_text("pinned toolchain\n")
        self.package = self.root / "bridge/haskell/resume"
        self.resume = self.package / "src/Tidepool/Internal/Resume.hs"
        self.resume.parent.mkdir(parents=True)
        self.resume.write_text("module Tidepool.Internal.Resume where\n")
        (self.package / "tidepool-resume.cabal").write_text("name: tidepool-resume\n")
        (self.root / "flake.lock").write_text("locked inputs\n")
        (self.root / "rust-toolchain.toml").write_text("pinned Rust\n")
        (self.root / "nix").mkdir()
        (self.root / "nix/ghc.patch").write_text("pinned GHC patch\n")
        self.commit(".")
        self.tools = self.root / "tools"
        self.tools.mkdir()
        # Launcher bootstrap receives only its declared Bash/coreutils inputs.
        # Optional host utilities must not hide a missing action dependency.
        self.bootstrap_tools = self.root / "bootstrap tools"
        self.bootstrap_tools.mkdir()
        for name in ("bash", "cat", "dirname", "id", "readlink"):
            executable = shutil.which(name)
            self.assertIsNotNone(executable, f"declared {name} executable")
            (self.bootstrap_tools / name).symlink_to(executable)
        self.buck_output = self.root / "outputs/buck"
        (self.buck_output / "bin").mkdir(parents=True)
        self.action_output = self.root / "outputs/action tools"
        self.action_output.mkdir()
        # Real declared utilities only serve the stub's Git/config checks.
        self.action_tools = self.action_output / "bin"
        self.action_tools.mkdir()
        for name in ("git", "mktemp", "rm", "python3"):
            (self.action_tools / name).symlink_to(shutil.which(name))
        self.shell_log = self.root / "dev-shell.log"
        self.buck_log = self.root / "buck.json"
        self.program(self.tools / "mountpoint", 'exit "$MOUNT_EXIT"')
        self.program(self.root / "scripts/dev-shell.sh", 'echo invoked > "$DEV_SHELL_LOG"; exit 96')
        self.program(self.tools / "nix", 'echo invoked > "$DEV_SHELL_LOG"; exit 97')
        self.program(self.tools / "buck2", "exit 95")
        self.buck = self.buck_output / "bin/buck2"
        self.buck.write_text(f"""#!{sys.executable}
import json, os, signal, sys
from pathlib import Path
Path(os.environ["BUCK_LOG"]).write_text(json.dumps({{
    "args": sys.argv[1:], "cwd": os.getcwd(), "path": os.environ["PATH"],
    "pid": os.getpid(), "cgroup": Path('/proc/self/cgroup').read_text(),
}}))
if os.environ.get('BUCK_SIGNAL'):
    os.kill(os.getpid(), signal.SIGTERM)
sys.exit(int(os.environ.get('BUCK_EXIT', '0')))
""")
        self.buck.chmod(0o755)
        self.generation = self.root / ".buck2-toolchains/generations/generation.test"
        (self.generation / "roots").mkdir(parents=True)
        self.selection = "git+file:///project?rev=" + "1" * 40 + "#default"
        tree = subprocess.check_output(
            ["bash", "-c", 'source scripts/toolchain-inputs.sh; toolchain_input_tree'],
            cwd=self.root, text=True,
        ).strip()
        (self.generation / "owner").write_text(
            f"checkout={self.root}\nuid={os.getuid()}\nselection={self.selection}\nselection_mode=checkout\ntoolchain_tree={tree}\n"
        )
        (self.generation / "status").write_text("configured\n")
        records = []
        for name, output in (("buck-buck2", self.buck_output), ("buck-test-git", self.action_output)):
            root = self.generation / "roots" / name
            root.symlink_to(output)
            records.append(f"{name}\t{self.selection.split('#')[0]}#packages.test.{name}\t{output}\t{root}\n")
        (self.generation / "outputs.tsv").write_text("".join(records))
        self.config = self.root / ".buckconfig.local"
        self.write_config(f"buck2 = {self.buck}\ngit = {self.action_tools}/git\naction_path = {self.action_tools}\n")
        self.env = dict(os.environ)
        self.env.pop("TIDEPOOL_BUCK_SHELL", None)
        self.env.update(
            PATH=str(self.tools) + os.pathsep + str(self.bootstrap_tools), MOUNT_EXIT="0",
            DEV_SHELL_LOG=str(self.shell_log), BUCK_LOG=str(self.buck_log),
        )

    def git_command(self, *args):
        return subprocess.run([self.git, "-C", str(self.root), *args], check=True, capture_output=True)

    def commit(self, path):
        self.git_command("add", path)
        self.git_command("-c", "user.name=test", "-c", "user.email=test@invalid", "commit", "-qm", "source")

    def write_config(self, values):
        content = f"# Retained toolchain generation: {self.generation}\n[nix]\n{values}"
        self.config.write_text(content)
        (self.generation / "config").write_text(content)

    def program(self, path, body):
        path.write_text(f"#!{shutil.which('bash')}\nset -euo pipefail\n" + body + "\n")
        path.chmod(0o755)

    def run_runner(self, *args, timeout_seconds=10, **environment):
        with subprocess.Popen(
            ["bash", str(self.root / "scripts/buck2-run.sh"), *args], cwd="/",
            env=dict(self.env, **environment), text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
        ) as process:
            self.launch_pid = process.pid
            try:
                stdout, stderr = process.communicate(timeout=timeout_seconds)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
                self.fail(f"Buck launcher did not finish within {timeout_seconds} seconds")
            return subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)

    def assert_no_shell_or_buck(self):
        self.assertFalse(self.shell_log.exists())
        self.assertFalse(self.buck_log.exists())

    def assert_refused(self, message):
        result = self.run_runner("build", "//pkg:target", TIDEPOOL_BUCK_SHELL="ready")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(message, result.stderr)
        self.assert_no_shell_or_buck()

    def test_missing_mount_fails_before_launch(self):
        result = self.run_runner("build", "//pkg:target", MOUNT_EXIT="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Buck requires the per-checkout buck-out bind mount", result.stderr)
        self.assert_no_shell_or_buck()

    def test_missing_config_fails_before_launch(self):
        self.config.unlink()
        result = self.run_runner("build", "//pkg:target")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Run scripts/buck2-configure.sh first", result.stderr)
        self.assert_no_shell_or_buck()

    def test_configured_launch_preserves_arguments_cwd_pid_cgroup_and_path(self):
        args = ["build", "--local-only", "-c", "remote.enabled=false", "//pkg:target", "with spaces", ""]
        result = self.run_runner(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.shell_log.exists())
        self.assertEqual(json.loads(self.buck_log.read_text()), {
            "args": args, "cwd": str(self.root),
            "path": f"{self.action_tools}:/run/current-system/sw/bin", "pid": self.launch_pid,
            "cgroup": Path("/proc/self/cgroup").read_text(),
        })

    def test_ready_marker_does_not_change_explicit_remote_arguments(self):
        args = ["test", "--remote-only", "-c", "remote.enabled=true", "//pkg:target"]
        result = self.run_runner(*args, TIDEPOOL_BUCK_SHELL="ready")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.shell_log.exists())
        self.assertEqual(json.loads(self.buck_log.read_text())["args"], args)

    def test_private_shared_configuration_is_added_for_every_buck_command(self):
        config_file = self.root / "retained-buck-config.json"
        config_file.write_text(json.dumps({
            "nix.native_catalog_source_root": "/nix/store/catalog-snapshot",
            "nix.native_catalog_retention_record": "/evidence/retained.json",
            "nix.native_catalog_retention": '{"gc_roots": []}',
        }))
        config_file.chmod(0o600)
        result = self.run_runner(
            "build", "--local-only", "-c", "remote.enabled=false",
            TIDEPOOL_BUCK_CONFIG_FILE=str(config_file),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(self.buck_log.read_text())["args"]
        self.assertEqual(args[0], "build")
        self.assertEqual(args.count("-c"), 4)
        self.assertIn("nix.native_catalog_source_root=/nix/store/catalog-snapshot", args)
        self.assertIn("nix.native_catalog_retention_record=/evidence/retained.json", args)
        self.assertIn('nix.native_catalog_retention={"gc_roots": []}', args)

    def test_shared_configuration_requires_matching_explicit_values(self):
        config_file = self.root / "retained-buck-config.json"
        config_file.write_text(json.dumps({"tidepool.profile": "fast-dev"}))
        config_file.chmod(0o600)
        result = self.run_runner(
            "build", "-c", "tidepool.profile=production",
            TIDEPOOL_BUCK_CONFIG_FILE=str(config_file),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("conflicts with command-line configuration", result.stderr)
        self.assertFalse(self.buck_log.exists())

    def test_shared_configuration_refuses_group_readable_files(self):
        config_file = self.root / "retained-buck-config.json"
        config_file.write_text(json.dumps({"tidepool.profile": "fast-dev"}))
        config_file.chmod(0o640)
        result = self.run_runner(
            "build", TIDEPOOL_BUCK_CONFIG_FILE=str(config_file),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("shared Buck configuration is invalid", result.stderr)
        self.assertFalse(self.buck_log.exists())

    def test_shared_configuration_refuses_a_private_fifo_without_blocking(self):
        config_file = self.root / "retained-buck-config.json"
        os.mkfifo(config_file, mode=0o600)
        config_file.chmod(0o600)
        result = self.run_runner(
            "build", TIDEPOOL_BUCK_CONFIG_FILE=str(config_file), timeout_seconds=2,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("shared Buck configuration is invalid", result.stderr)
        self.assertFalse(self.buck_log.exists())

    def test_shared_configuration_refuses_symlinks_to_private_regular_files(self):
        target = self.root / "actual-config.json"
        target.write_text(json.dumps({"tidepool.profile": "fast-dev"}))
        target.chmod(0o600)
        config_file = self.root / "retained-buck-config.json"
        config_file.symlink_to(target)
        result = self.run_runner(
            "build", TIDEPOOL_BUCK_CONFIG_FILE=str(config_file),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("shared Buck configuration is invalid", result.stderr)
        self.assertFalse(self.buck_log.exists())

    def test_buck_exit_status_and_signal_are_preserved(self):
        result = self.run_runner("build", BUCK_EXIT="37")
        self.assertEqual(result.returncode, 37, result.stderr)
        result = self.run_runner("build", BUCK_SIGNAL="1")
        self.assertEqual(result.returncode, -signal.SIGTERM, result.stderr)

    def test_legacy_config_requires_explicit_preparation(self):
        self.config.write_text("[nix]\naction_path = /somewhere\n")
        self.assert_refused("missing retained generation")

    def test_generation_status_and_published_copy_are_checked(self):
        (self.generation / "status").write_text("failed\n")
        self.assert_refused("generation was not configured")
        (self.generation / "status").write_text("configured\n")
        self.config.write_text(self.config.read_text() + "# changed\n")
        self.assert_refused("published configuration changed")

    def test_published_copy_compares_exact_bytes_including_trailing_newlines(self):
        original = self.config.read_bytes()
        for suffix in (b"\n", b"\n\n", b"\0", b"\0\n"):
            with self.subTest(suffix=suffix):
                self.config.write_bytes(original + suffix)
                self.assert_refused("published configuration changed")
        self.config.write_bytes(original.rstrip(b"\n"))
        self.assert_refused("published configuration changed")
        self.config.write_bytes(original)
        result = self.run_runner("build")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_checkout_and_user_ownership_are_checked(self):
        owner = self.generation / "owner"
        owner.write_text(owner.read_text().replace(f"uid={os.getuid()}", "uid=invalid"))
        self.assert_refused("another checkout or user")

    def test_root_unavailability_and_mismatch_are_checked(self):
        root = self.generation / "roots/buck-buck2"
        root.unlink()
        self.assert_refused("retained output buck-buck2")
        root.symlink_to(self.action_output)
        self.assert_refused("retained output buck-buck2")

    def test_missing_buck_does_not_fall_back_to_ambient_executable(self):
        self.buck.unlink()
        self.assert_refused("Buck executable does not match")

    def test_unrooted_and_empty_action_path_are_refused(self):
        for path in (str(self.tools), str(self.action_tools) + ":", ":" + str(self.action_tools)):
            with self.subTest(path=path):
                self.write_config(f"buck2 = {self.buck}\ngit = {self.action_tools}/git\naction_path = {path}\n")
                self.assert_refused("action PATH")

    def test_selection_reference_is_checked(self):
        roster = self.generation / "outputs.tsv"
        roster.write_text(roster.read_text().replace("1" * 40, "2" * 40))
        self.assert_refused("output selection")

    def test_changed_toolchain_pin_requires_reconfiguration(self):
        (self.root / "flake.nix").write_text("new toolchain\n")
        self.assert_refused("changed toolchain inputs")
        self.commit("flake.nix")
        self.assert_refused("changed toolchain input pin")

    def input_tree(self, revision="HEAD"):
        return subprocess.check_output(
            ["bash", "-c", 'source scripts/toolchain-inputs.sh; toolchain_input_tree git "$1"', "bash", revision],
            cwd=self.root, text=True,
        ).strip()

    def test_toolchain_snapshot_retains_nested_package_and_original_inputs(self):
        index = self.root / ".git/index"
        before_index = index.read_bytes()
        tree = self.input_tree()
        self.assertEqual(before_index, index.read_bytes())
        for path in ("flake.nix", "flake.lock", "rust-toolchain.toml", "nix/ghc.patch",
                     "bridge/haskell/resume/tidepool-resume.cabal",
                     "bridge/haskell/resume/src/Tidepool/Internal/Resume.hs"):
            with self.subTest(path=path):
                self.assertEqual(self.git_command("show", f"{tree}:{path}").stdout,
                                 (self.root / path).read_bytes())
        self.assertEqual(self.git_command("ls-tree", "--name-only", f"{tree}:bridge/haskell").stdout,
                         b"resume\n")

    def test_package_source_changes_refuse_until_reconfiguration(self):
        before = self.input_tree()
        old_revision = self.git_command("rev-parse", "HEAD").stdout.decode().strip()
        self.resume.write_text("module Tidepool.Internal.Resume where\nchanged = ()\n")
        self.assert_refused("changed toolchain inputs")
        self.git_command("add", "bridge/haskell/resume")
        self.assert_refused("changed toolchain inputs")
        self.commit("bridge/haskell/resume")
        self.assert_refused("changed toolchain input pin")
        self.assertNotEqual(before, self.input_tree())
        self.assertEqual(before, self.input_tree(old_revision))

    def test_untracked_package_sources_cannot_be_omitted_from_snapshot(self):
        (self.package / "src/New.hs").write_text("module New where\n")
        self.assert_refused("changed toolchain inputs")

    def test_unrelated_bridge_source_does_not_change_toolchain_snapshot(self):
        before = self.input_tree()
        unrelated = self.root / "bridge/haskell/src/Compiler.hs"
        unrelated.parent.mkdir(parents=True)
        unrelated.write_text("module Compiler where\n")
        self.assertEqual(before, self.input_tree())
        self.commit("bridge/haskell/src")
        self.assertEqual(before, self.input_tree())
        result = self.run_runner("build")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_explicit_selection_ignores_dirty_and_changed_local_toolchain_inputs(self):
        owner = self.generation / "owner"
        owner.write_text(owner.read_text().replace("selection_mode=checkout", "selection_mode=explicit"))
        (self.root / "flake.nix").write_text("uncommitted local experiment\n")
        result = self.run_runner("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.shell_log.exists())
        self.commit("flake.nix")
        result = self.run_runner("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(self.buck_log.read_text())["args"], ["build"])

    def test_missing_selection_mode_requires_configuration(self):
        owner = self.generation / "owner"
        owner.write_text(owner.read_text().replace("selection_mode=checkout\n", ""))
        self.assert_refused("generation selection mode")

    def test_unrelated_source_commit_reuses_exact_generation(self):
        (self.root / "program.rs").write_text("source change\n")
        self.commit("program.rs")
        result = self.run_runner("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.shell_log.exists())


if __name__ == "__main__":
    unittest.main()
