#!/usr/bin/env python3
"""Exercise staged Buck dependency generation with controlled tool boundaries."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "buck2-reindeer.sh"


class ReindeerStaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="buck-reindeer-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "third-party/rust/fixups").mkdir(parents=True)
        (self.root / "bin").mkdir()
        shutil.copyfile(SCRIPT, self.root / "scripts/buck2-reindeer.sh")
        (self.root / "scripts/buck2-reindeer.sh").chmod(0o755)
        (self.root / "scripts/buck2-dependencies.py").write_text(
            """import os, pathlib, sys
out = pathlib.Path(sys.argv[sys.argv.index('--output-dir') + 1])
pathlib.Path(__file__).resolve().parents[1].joinpath('received-feature-args').write_text(repr(sys.argv[3:]))
if os.environ.get('FAIL_GENERATOR'):
    raise SystemExit(17)
(out / 'Cargo.toml').write_text('staged manifest' + chr(10))
(out / 'Cargo.lock').write_text('staged lock' + chr(10))
"""
        )
        rust = self.root / "third-party/rust"
        for name, value in {
            "reindeer.toml": "config",
            "empty.rs": "",
            "Cargo.toml": "old manifest\n",
            "Cargo.lock": "old lock\n",
            "BUCK": "old buck\n",
        }.items():
            (rust / name).write_text(value)
        reindeer = self.root / "bin/reindeer"
        reindeer.write_text(
            """#!/usr/bin/env python3
import os
from pathlib import Path
if os.environ.get('FAIL_REINDEER'):
    raise SystemExit(19)
if os.environ.get('LEAK_STAGE'):
    value = str(Path.cwd())
elif os.environ.get('MATCHED_HARNESS'):
    value = ('load("@prelude//rust:cargo_package.bzl", "cargo")\\n'
             'git_fetch(\\n'
             '    name = "harness-source.git",\\n'
             '    repo = "https://github.com/tidepool-heavy-industries/exomonad-harness.git",\\n'
             '    rev = "0123456789abcdef0123456789abcdef01234567",\\n'
             '    visibility = [],\\n'
             ')\\n')
else:
    value = 'generated buck' + chr(10)
Path('BUCK').write_text(value)
"""
        )
        reindeer.chmod(0o755)
        self.env = os.environ | {
            "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}",
            "TIDEPOOL_REINDEER_SHELL": "ready",
        }

    def run_script(self, *args, **env):
        return subprocess.run(
            ["bash", "scripts/buck2-reindeer.sh", *args], cwd=self.root,
            env=self.env | env, capture_output=True, text=True, timeout=10,
        )

    def contents(self):
        rust = self.root / "third-party/rust"
        return {name: (rust / name).read_bytes() for name in ("Cargo.toml", "Cargo.lock", "BUCK")}

    def test_check_compares_staged_result_without_changing_outputs(self):
        result = self.run_script("--check")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Stale Buck dependency inputs", result.stderr)
        self.assertEqual(self.contents(), {
            "Cargo.toml": b"old manifest\n",
            "Cargo.lock": b"old lock\n",
            "BUCK": b"old buck\n",
        })

    def test_check_failure_after_generation_does_not_change_outputs(self):
        before = self.contents()
        result = self.run_script("--check", FAIL_REINDEER="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.contents(), before)

    def test_normal_failure_does_not_publish_partial_outputs(self):
        before = self.contents()
        result = self.run_script(FAIL_REINDEER="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.contents(), before)

    def test_staging_path_in_generated_input_is_rejected_without_publication(self):
        before = self.contents()
        result = self.run_script(LEAK_STAGE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Staging path leaked", result.stderr)
        self.assertEqual(self.contents(), before)

    def test_successful_generation_publishes_all_validated_outputs(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = {
            "Cargo.toml": b"staged manifest\n",
            "Cargo.lock": b"staged lock\n",
            "BUCK": b"generated buck\n",
        }
        self.assertEqual(self.contents(), expected)

        result = self.run_script("--check")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.contents(), expected)

    def test_workspace_feature_suppressions_are_forwarded_to_reindeer_metadata(self):
        result = self.run_script(
            "--no-default-features", "tidepool",
            "--no-default-features", "exomonad-agent",
            "--features", "tidepool=other",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        received = (self.root / "received-feature-args").read_text()
        self.assertEqual(
            received,
            "['--no-default-features', 'tidepool', '--no-default-features', 'exomonad-agent', '--features', 'tidepool=other']",
        )

    def test_locked_harness_fetch_is_exposed_as_a_public_source_input(self):
        result = self.run_script(MATCHED_HARNESS="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        buck = (self.root / "third-party/rust/BUCK").read_text()
        self.assertIn('load("@prelude//:rules.bzl", "filegroup")', buck)
        self.assertIn('name = "matched_harness_source"', buck)
        self.assertIn('srcs = [":harness-source.git"]', buck)
        self.assertIn('visibility = ["PUBLIC"]', buck)

        result = self.run_script("--check", MATCHED_HARNESS="1")
        self.assertEqual(result.returncode, 0, result.stderr)
if __name__ == "__main__":
    unittest.main()
