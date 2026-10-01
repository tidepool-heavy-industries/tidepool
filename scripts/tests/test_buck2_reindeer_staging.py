#!/usr/bin/env python3
"""Exercise staged Buck dependency generation with controlled tool boundaries."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import sys
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "buck2-reindeer.sh"
PROVENANCE = SCRIPT.parent / "embedded_web_provenance.py"


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
             ')\\n'
             'cargo.rust_library(\\n'
             '    name = "harness-0.1",\\n'
             '    srcs = [":harness-source.git"],\\n'
             '    crate_root = "harness-source/crates/harness/src/lib.rs",\\n'
             ')\\n')
else:
    value = 'generated buck' + chr(10)
Path('BUCK').write_text(value)
"""
        )
        reindeer.chmod(0o755)
        rev = "0123456789abcdef0123456789abcdef01234567"
        (self.root / "Cargo.lock").write_text(
            '[[package]]\nname = "harness"\nversion = "0.1.0"\n'
            f'source = "git+https://github.com/tidepool-heavy-industries/exomonad-harness.git?rev={rev}#{rev}"\n'
        )
        pin = {"type": "github", "owner": "tidepool-heavy-industries", "repo": "exomonad-harness", "rev": rev}
        (self.root / "flake.lock").write_text(json.dumps({"nodes": {
            "root": {"inputs": {"harnessWeb": "harnessWeb"}},
            "harnessWeb": {"flake": False, "original": pin, "locked": pin | {"narHash": "sha256-" + "A" * 43 + "="}},
        }}))
        self.expected_source = "/nix/store/11111111111111111111111111111111-tidepool-matched-harness-source"
        fake_git = self.root / "bin/git"
        fake_git.write_text(
            """#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
if args[:3] == ['diff', '--quiet', 'HEAD']:
    raise SystemExit(1 if os.environ.get('DIRTY_NIX') else 0)
if args[:2] == ['ls-tree', 'HEAD']:
    print('100644 blob deadbeef\\tflake.nix')
elif args[:1] == ['mktree']:
    print('b' * 40)
elif args[:1] == ['commit-tree']:
    print('a' * 40)
elif args[:2] == ['rev-parse', '-q']:
    print('a' * 40)
else:
    raise SystemExit('unexpected git invocation: ' + repr(args))
"""
        )
        fake_git.chmod(0o755)
        fake_nix = self.root / "bin/nix"
        fake_nix.write_text(
            """#!/usr/bin/env python3
import os, pathlib, sys
args = sys.argv[1:]
with open('received-nix-args', 'a') as stream:
    stream.write(repr(args) + '\\n')
if args[:1] == ['path-info']:
    if os.environ.get('MISSING_OVERRIDE'):
        raise SystemExit(1)
    print(args[1])
elif args[:3] == ['hash', 'path', '--sri']:
    print(os.environ.get('OVERRIDE_HASH', 'sha256-' + 'A' * 43 + '='))
elif args[:3] == ['store', 'ls', '--json']:
    import json
    print(json.dumps({'type': 'directory' if os.environ.get('BAD_SOURCE_LAYOUT') else 'regular'}))
elif '--expr' in args:
    print('x86_64-linux')
elif any('buck-matched-harness-source.outPath' in arg for arg in args):
    print(os.environ['EXPECTED_NIX_OUTPUT'])
else:
    raise SystemExit('unexpected nix invocation: ' + repr(args))
"""
        )
        fake_nix.chmod(0o755)
        self.env = os.environ | {
            "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}",
            "TIDEPOOL_REINDEER_SHELL": "ready",
            "EXPECTED_NIX_OUTPUT": self.expected_source,
        }

    def run_script(self, *args, **env):
        return subprocess.run(
            ["bash", "scripts/buck2-reindeer.sh", *args], cwd=self.root,
            env=self.env | env, capture_output=True, text=True, timeout=10,
        )

    def contents(self):
        rust = self.root / "third-party/rust"
        return {name: (rust / name).read_bytes() for name in ("Cargo.toml", "Cargo.lock", "BUCK")}

    def prepare_local_harness_fixture(self, **env):
        rev = "0123456789abcdef0123456789abcdef01234567"
        (self.root / "Cargo.lock").write_text(
            '[[package]]\nname = "harness"\nversion = "0.1.0"\n'
            f'source = "git+https://github.com/tidepool-heavy-industries/exomonad-harness.git?rev={rev}#{rev}"\n'
        )
        pin = {"type": "github", "owner": "tidepool-heavy-industries", "repo": "exomonad-harness", "rev": rev}
        (self.root / "flake.lock").write_text(
            json.dumps({"nodes": {"root": {"inputs": {"harnessWeb": "harnessWeb"}}, "harnessWeb": {"flake": False, "original": pin, "locked": pin | {"narHash": "sha256-" + "A" * 43 + "="}}}})
        )
        (self.root / ".buckconfig.local").write_text(
            f"[nix]\nmatched_harness_source = {self.expected_source}\n"
        )
        return self.run_script("--local-harness-source", MATCHED_HARNESS="1", **env)

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
        self.assertIn('load("@prelude//:rules.bzl", "alias", "filegroup")', buck)
        self.assertIn('name = "matched_harness_source"', buck)
        self.assertIn('source = ":harness-source.git"', buck)
        self.assertIn('srcs = [":harness-source"]', buck)
        self.assertIn('actual = ":harness-source[revision]"', buck)
        self.assertIn('visibility = ["PUBLIC"]', buck)

        result = self.run_script("--check", MATCHED_HARNESS="1")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_opt_in_source_mode_replaces_fetch_and_preserves_generated_labels(self):
        result = self.prepare_local_harness_fixture()
        self.assertEqual(result.returncode, 0, result.stderr)
        buck = (self.root / "third-party/rust/BUCK").read_text()
        self.assertNotIn("git_fetch(", buck)
        self.assertIn('load("@toolchains//:tidepool.bzl", "checked_harness_source")', buck)
        self.assertIn('name = "harness-source",', buck)
        self.assertIn(f'store_path = "{self.expected_source}"', buck)
        self.assertNotIn('read_root_config("nix", "matched_harness_source")', buck)
        self.assertIn('cargo_lock = "root//:workspace_cargo_lock"', buck)
        self.assertIn('flake_lock = "root//:workspace_flake_lock"', buck)
        self.assertNotIn('name = "harness-source.git",', buck)
        self.assertIn('srcs = [":harness-source"]', buck)
        self.assertNotIn('srcs = [":harness-source.git"]', buck)
        self.assertIn('crate_root = "harness-source/crates/harness/src/lib.rs"', buck)
        self.assertIn('name = "matched_harness_source"', buck)
        web_group = buck.split('name = "matched_harness_source",', 1)[1]
        self.assertIn('srcs = [":harness-source"]', web_group)
        self.assertNotIn('srcs = [":harness-source.git"]', web_group)
        self.assertIn('source = "git+https://github.com/tidepool-heavy-industries/exomonad-harness.git?rev=0123456789abcdef0123456789abcdef01234567#0123456789abcdef0123456789abcdef01234567"', (self.root / "Cargo.lock").read_text())

    def test_default_mode_retains_standard_git_fetch(self):
        result = self.run_script(MATCHED_HARNESS="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        buck = (self.root / "third-party/rust/BUCK").read_text()
        self.assertIn("git_fetch(", buck)
        self.assertIn('load("@toolchains//:tidepool.bzl", "checked_harness_source")', buck)
        self.assertIn('source = ":harness-source.git"', buck)
        self.assertNotIn('store_path =', buck)

    def test_immutable_override_preserves_the_locked_source_and_generated_graph(self):
        self.prepare_local_harness_fixture()
        lock_path = self.root / 'flake.lock'
        lock = json.loads(lock_path.read_text())
        lock['nodes']['harnessWeb']['locked']['narHash'] = 'sha256-' + 'A' * 43 + '='
        lock_path.write_text(json.dumps(lock))
        source = '/nix/store/22222222222222222222222222222222-source'
        before = self.contents()
        result = self.run_script('--local-harness-source', '--harness-source-override', source, MATCHED_HARNESS='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual(self.contents()['BUCK'], before['BUCK'])
        self.assertEqual(self.contents()['Cargo.toml'], before['Cargo.toml'])
        self.assertEqual(self.contents()['Cargo.lock'], before['Cargo.lock'])
        self.assertEqual(json.loads(lock_path.read_text()), lock)
        buck = (self.root / 'third-party/rust/BUCK').read_text()
        self.assertIn(f'store_path = "{source}"', buck)
        result = self.run_script('--check', '--local-harness-source', '--harness-source-override', source, MATCHED_HARNESS='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        invocations = (self.root / 'received-nix-args').read_text()
        self.assertIn(repr(['path-info', source]), invocations)
        self.assertIn(repr(['hash', 'path', '--sri', source]), invocations)
        self.assertIn(repr(['store', 'ls', '--json', source + '/crates/harness/src/lib.rs']), invocations)
        self.assertNotIn("'--override-input'", invocations)

    def test_verified_override_ignores_mutable_config_and_unrelated_dirty_nix(self):
        self.prepare_local_harness_fixture()
        (self.root / '.buckconfig.local').write_text('[nix]\nmatched_harness_source = /tmp/stale-source\n')
        (self.root / 'received-nix-args').unlink()
        source = '/nix/store/22222222222222222222222222222222-source'
        result = self.run_script('--local-harness-source', '--harness-source-override', source,
                                 MATCHED_HARNESS='1', DIRTY_NIX='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        buck = (self.root / 'third-party/rust/BUCK').read_text()
        self.assertIn(f'store_path = "{source}"', buck)
        invocations = (self.root / 'received-nix-args').read_text()
        self.assertNotIn("'eval'", invocations)
        result = self.run_script('--check', '--local-harness-source', '--harness-source-override', source,
                                 MATCHED_HARNESS='1', DIRTY_NIX='1')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_implicit_source_still_refuses_dirty_nix(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        result = self.run_script('--local-harness-source', MATCHED_HARNESS='1', DIRTY_NIX='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('commit Nix inputs before Reindeer generation', result.stderr)
        self.assertEqual(self.contents(), before)

    def test_verified_override_missing_path_or_wrong_source_layout_refuses(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        for environment in ({'MISSING_OVERRIDE': '1'}, {'BAD_SOURCE_LAYOUT': '1'}):
            with self.subTest(environment=environment):
                result = self.run_script('--local-harness-source', '--harness-source-override',
                    '/nix/store/22222222222222222222222222222222-source', MATCHED_HARNESS='1', **environment)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Cannot select matched harness source', result.stderr)
                self.assertEqual(self.contents(), before)

    def test_verified_override_refuses_orphan_current_pin(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        lock_path = self.root / 'flake.lock'
        lock = json.loads(lock_path.read_text())
        lock['nodes']['root']['inputs'].clear()
        lock_path.write_text(json.dumps(lock))
        result = self.run_script('--local-harness-source', '--harness-source-override',
            '/nix/store/22222222222222222222222222222222-source', MATCHED_HARNESS='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not connected to the root input graph', result.stderr)
        self.assertEqual(self.contents(), before)

    def test_override_hash_mismatch_refuses_without_publication(self):
        self.prepare_local_harness_fixture()
        lock_path = self.root / 'flake.lock'
        lock = json.loads(lock_path.read_text())
        lock['nodes']['harnessWeb']['locked']['narHash'] = 'sha256-' + 'A' * 43 + '='
        lock_path.write_text(json.dumps(lock))
        before = self.contents()
        result = self.run_script('--local-harness-source', '--harness-source-override',
                                 '/nix/store/22222222222222222222222222222222-source',
                                 MATCHED_HARNESS='1', OVERRIDE_HASH='sha256-foreign-source')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('override hash differs from the canonical locked narHash', result.stderr)
        self.assertEqual(self.contents(), before)

    def test_override_requires_immutable_store_source_and_explicit_source_mode(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        for args in [('--harness-source-override', '/tmp/mutable-source'),
                     ('--local-harness-source', '--harness-source-override', ''),
                     ('--local-harness-source', '--harness-source-override', '/tmp/mutable-source')]:
            result = self.run_script(*args, MATCHED_HARNESS='1')
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.contents(), before)

    def test_source_lock_mismatch_refuses_without_publishing(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        lock = json.loads((self.root / "flake.lock").read_text())
        lock["nodes"]["harnessWeb"]["locked"]["rev"] = "f" * 40
        (self.root / "flake.lock").write_text(json.dumps(lock))
        result = self.run_script("--local-harness-source", MATCHED_HARNESS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("flake.lock harnessWeb locked pin does not match", result.stderr)
        self.assertEqual(self.contents(), before)

    def test_generated_fetch_revision_mismatch_refuses_without_publishing(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        reindeer = self.root / "bin/reindeer"
        reindeer.write_text(reindeer.read_text().replace("0123456789abcdef0123456789abcdef01234567", "f" * 40))
        result = self.run_script("--local-harness-source", MATCHED_HARNESS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("generated git_fetch revision does not match", result.stderr)
        self.assertEqual(self.contents(), before)

    def test_missing_or_non_store_source_refuses_without_publishing(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        (self.root / ".buckconfig.local").write_text("[nix]\nmatched_harness_source = /tmp/harness\n")
        result = self.run_script("--local-harness-source", MATCHED_HARNESS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stale or differs from the current pinned Nix output", result.stderr)
        self.assertEqual(self.contents(), before)

    def test_stale_store_output_with_same_basename_refuses_without_publication(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        stale_source = "/nix/store/00000000000000000000000000000000-tidepool-matched-harness-source"
        (self.root / ".buckconfig.local").write_text(f"[nix]\nmatched_harness_source = {stale_source}\n")
        result = self.run_script(
            "--local-harness-source", MATCHED_HARNESS="1",
            TIDEPOOL_DEV_SHELL="git+file:///old-checkout?rev=" + "c" * 40 + "#default",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stale or differs from the current pinned Nix output", result.stderr)
        self.assertEqual(self.contents(), before)
        nix_args = (self.root / "received-nix-args").read_text()
        self.assertIn(f"git+file://{self.root}?rev=" + "a" * 40, nix_args)
        self.assertNotIn("old-checkout", nix_args)

    def test_local_mode_without_generated_candidate_refuses_without_publication(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        result = self.run_script('--local-harness-source')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('expected one generated git_fetch candidate, found 0', result.stderr)
        self.assertEqual(self.contents(), before)

    def test_multiple_matching_candidates_refuse_without_publishing(self):
        self.prepare_local_harness_fixture()
        before = self.contents()
        reindeer = self.root / "bin/reindeer"
        source = reindeer.read_text()
        source = source.replace("Path('BUCK').write_text(value)", "Path('BUCK').write_text(value + value[value.index('git_fetch('):value.index('cargo.rust_library(')])")
        reindeer.write_text(source)
        result = self.run_script("--local-harness-source", MATCHED_HARNESS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected one generated git_fetch candidate, found 2", result.stderr)
        self.assertEqual(self.contents(), before)
    def check_generated_source(self, revision, output):
        return subprocess.run([
            sys.executable, str(PROVENANCE),
            '--generated-source-revision', revision,
            '--generated-source-nar-hash', 'sha256-' + 'A' * 43 + '=',
            '--cargo-lock', str(self.root / 'Cargo.lock'),
            '--flake-lock', str(self.root / 'flake.lock'),
            '--output', str(output),
        ], capture_output=True, text=True, timeout=10)

    def test_stale_generated_source_cannot_be_consumed_after_pin_update(self):
        self.prepare_local_harness_fixture()
        generated = (self.root / 'third-party/rust/BUCK').read_text()
        old_revision = '0123456789abcdef0123456789abcdef01234567'
        new_revision = 'f' * 40
        (self.root / 'Cargo.lock').write_text((self.root / 'Cargo.lock').read_text().replace(old_revision, new_revision))
        (self.root / 'flake.lock').write_text((self.root / 'flake.lock').read_text().replace(old_revision, new_revision))
        output = self.root / 'checked-revision.txt'
        result = self.check_generated_source(old_revision, output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('generated harness source revision differs from Cargo.lock', result.stderr)
        self.assertFalse(output.exists())
        self.assertEqual((self.root / 'third-party/rust/BUCK').read_text(), generated)

    def test_mutable_config_drift_cannot_redirect_generated_source(self):
        self.prepare_local_harness_fixture()
        generated = (self.root / 'third-party/rust/BUCK').read_text()
        (self.root / '.buckconfig.local').write_text('[nix]\nmatched_harness_source = /tmp/unrelated-harness\n')
        output = self.root / 'checked-revision.txt'
        revision = '0123456789abcdef0123456789abcdef01234567'
        result = self.check_generated_source(revision, output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(), revision + '\n')
        self.assertIn(f'store_path = "{self.expected_source}"', generated)
        self.assertNotIn('read_root_config("nix", "matched_harness_source")', generated)

    def test_current_lock_mismatch_is_rejected_at_consumption(self):
        self.prepare_local_harness_fixture()
        lock = json.loads((self.root / 'flake.lock').read_text())
        lock['nodes']['harnessWeb']['locked']['rev'] = 'f' * 40
        (self.root / 'flake.lock').write_text(json.dumps(lock))
        output = self.root / 'checked-revision.txt'
        result = self.check_generated_source('0123456789abcdef0123456789abcdef01234567', output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('generated harness source revision differs from flake.lock locked pin', result.stderr)
        self.assertFalse(output.exists())

    def test_same_revision_with_changed_nar_hash_is_rejected_at_consumption(self):
        self.prepare_local_harness_fixture()
        lock_path = self.root / 'flake.lock'
        lock = json.loads(lock_path.read_text())
        lock['nodes']['harnessWeb']['locked']['narHash'] = 'sha256-' + 'B' * 43 + '='
        lock_path.write_text(json.dumps(lock))
        output = self.root / 'checked-revision.txt'
        result = self.check_generated_source('0123456789abcdef0123456789abcdef01234567', output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('generated harness source NAR hash differs from flake.lock', result.stderr)
        self.assertFalse(output.exists())

    def test_malformed_current_lock_is_rejected_at_consumption(self):
        self.prepare_local_harness_fixture()
        (self.root / 'Cargo.lock').write_text('[[package]')
        output = self.root / 'checked-revision.txt'
        result = self.check_generated_source('0123456789abcdef0123456789abcdef01234567', output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('embedded web provenance rejected', result.stderr)
        self.assertFalse(output.exists())

if __name__ == "__main__":
    unittest.main()
