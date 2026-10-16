import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).parents[1]))

SPEC = importlib.util.spec_from_file_location("integration_status", Path(__file__).parents[1] / "integration-status.py")
STATUS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STATUS)


class IntegrationStatusTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "tidepool"
        self.repo.mkdir()
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.base = self.commit("base", "base")
        self.git("update-ref", "refs/remotes/origin/main", self.base)
        self.write_pins(self.base)

    def git(self, *args, repo=None):
        environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        return subprocess.check_output(["git", "-C", str(repo or self.repo), *args], text=True, stderr=subprocess.DEVNULL, env=environment).strip()

    def commit(self, name, contents):
        (self.repo / name).write_text(contents)
        self.git("add", name)
        self.git("commit", "-m", name)
        return self.git("rev-parse", "HEAD")

    def write_pins(self, revision, lock_revision=None, flake_revision=None, web_lock_revision=None):
        (self.repo / "exomonad/host").mkdir(parents=True, exist_ok=True)
        (self.repo / "exomonad/host/Cargo.toml").write_text('[package]\nname = "exomonad-host"\nversion = "0.1.0"\n[dependencies.harness]\nworkspace = true\n')
        git_url = "https://github.com/tidepool-heavy-industries/exomonad-harness.git"
        (self.repo / "Cargo.toml").write_text(f'[workspace.dependencies]\nharness = {{ git = "{git_url}", rev = "{revision}", package = "harness" }}\n')
        (self.repo / "Cargo.lock").write_text(f'[[package]]\nname = "harness"\nversion = "0.1.0"\nsource = "git+{git_url}?rev={revision}#{lock_revision or revision}"\n[[package]]\nname = "exomonad-host"\nversion = "0.1.0"\ndependencies = ["harness"]\n')
        (self.repo / "flake.nix").write_text(f'{{ inputs.harnessWeb = {{ url = "github:tidepool-heavy-industries/exomonad-harness/{flake_revision or revision}"; flake = false; }}; }}\n')
        (self.repo / "flake.lock").write_text(json.dumps({"root": "root", "nodes": {"root": {"inputs": {"harnessWeb": "harnessWeb"}}, "harnessWeb": {"locked": {"type": "github", "owner": "tidepool-heavy-industries", "repo": "exomonad-harness", "rev": web_lock_revision or revision}}}}))

    def invoke(self, *args):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            result = STATUS.main(["--repo", str(self.repo), "--json", *args])
        return result, json.loads(output.getvalue())

    def test_equal_main_and_cached_remote_are_strict_ok(self):
        code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertEqual(report["main_vs_cached_remote"]["relation"], "equal")
        self.assertEqual(report["remote_main"]["observation"], "cached_local_tracking_ref")
        self.assertFalse(report["remote_main"]["fetched"])

    def test_feature_branch_and_ahead_main_are_informational(self):
        self.commit("main-change", "one")
        self.git("checkout", "-b", "feature")
        self.commit("feature-change", "two")
        code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertEqual(report["checkout"]["branch"], "feature")
        self.assertEqual(report["main_vs_cached_remote"]["relation"], "ahead")
        self.assertEqual(report["checkout_vs_main"]["left_unique_commits"], 1)

    def test_behind_main_is_informational(self):
        newer = self.commit("remote-change", "one")
        self.git("update-ref", "refs/remotes/origin/main", newer)
        self.git("checkout", "--detach", self.base)
        self.git("branch", "-f", "main", self.base)
        code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertIsNone(report["checkout"]["branch"])
        self.assertEqual(report["main_vs_cached_remote"]["relation"], "behind")

    def test_true_main_divergence_fails_strict(self):
        self.commit("local-change", "one")
        self.git("checkout", "-b", "remote-side", self.base)
        other = self.commit("remote-change", "two")
        self.git("update-ref", "refs/remotes/origin/main", other)
        code, report = self.invoke("--strict")
        self.assertEqual(code, 1)
        self.assertEqual(report["main_vs_cached_remote"]["relation"], "diverged")
        self.assertEqual(report["main_vs_cached_remote"]["left_unique_commits"], 1)
        self.assertEqual(report["main_vs_cached_remote"]["right_unique_commits"], 1)

    def test_contained_candidate_and_unique_candidate(self):
        self.git("branch", "contained", self.base)
        self.git("checkout", "-b", "candidate")
        selected = self.commit("candidate-change", "one")
        _, report = self.invoke("--candidate", "contained", "--candidate", "candidate")
        first, second = report["candidates"]
        self.assertTrue(first["contained_in_main"])
        self.assertFalse(second["contained_in_main"])
        self.assertEqual(second["head"], selected)
        self.assertEqual(second["patch_unique_commits"], 1)

    def test_patch_equivalence_does_not_claim_containment(self):
        self.git("checkout", "-b", "candidate")
        selected = self.commit("same-patch", "same")
        self.git("checkout", "main")
        self.commit("unrelated", "other")
        self.git("cherry-pick", selected)
        _, report = self.invoke("--candidate", "candidate")
        row = report["candidates"][0]
        self.assertFalse(row["contained_in_main"])
        self.assertEqual(row["patch_unique_commits"], 0)
        self.assertEqual(row["vs_main"]["left_unique_commits"], 1)

    def test_pin_disagreements_each_fail(self):
        missing = "f" * 40
        for field in ("lock_revision", "flake_revision", "web_lock_revision"):
            with self.subTest(field=field):
                self.write_pins(self.base, **{field: missing})
                code, report = self.invoke("--strict")
                self.assertEqual(code, 1)
                self.assertIn("harness_pin_disagreement", report["errors"])

    def test_unselected_harness_inventory_cannot_satisfy_required_ancestry(self):
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        for edge in (None, "foreign-harness", "harness", "harness 0.1.0", "harness 0.1.0 (" + registry + ")"):
            with self.subTest(edge=edge):
                self.write_pins(self.base)
                path = self.repo / "Cargo.lock"
                contents = path.read_text().replace('dependencies = ["harness"]',
                    'dependencies = []' if edge is None else 'dependencies = [' + json.dumps(edge) + ']')
                # The correct Git record remains available but the host either
                # has no such edge, selects a foreign source, or is ambiguous.
                path.write_text(contents + '\n[[package]]\nname = "harness"\nversion = "0.1.0"\nsource = "' + registry + '"\n')
                code, report = self.invoke("--strict", "--harness-repo", str(self.repo),
                                           "--required-harness-ancestor", self.base)
                self.assertEqual(code, 1)
                self.assertFalse(report["strict_ok"])
                self.assertIn("harness_pins_unavailable", report["errors"])
                self.assertIsNone(report["harness_repository"]["selected_revision"])
                self.assertIsNone(report["harness_repository"]["selected_contains_required"])

    def test_exact_host_git_edge_survives_unrelated_registry_inventory(self):
        path = self.repo / "Cargo.lock"
        source = "git+https://github.com/tidepool-heavy-industries/exomonad-harness.git?rev=" + self.base + "#" + self.base
        contents = path.read_text().replace('dependencies = ["harness"]',
            'dependencies = [' + json.dumps("harness 0.1.0 (" + source + ")") + ']')
        path.write_text(contents + '\n[[package]]\nname = "harness"\nversion = "0.1.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n')
        code, report = self.invoke("--strict", "--harness-repo", str(self.repo),
                                  "--required-harness-ancestor", self.base)
        self.assertEqual(code, 0)
        self.assertTrue(report["harness_repository"]["selected_contains_required"])

    def test_same_revision_from_foreign_repository_is_not_selected(self):
        path = self.repo / "Cargo.lock"
        path.write_text(path.read_text().replace("https://github.com/tidepool-heavy-industries/exomonad-harness.git", "https://example.invalid/fork.git"))
        code, report = self.invoke("--strict", "--harness-repo", str(self.repo),
                                  "--required-harness-ancestor", self.base)
        self.assertEqual(code, 1)
        self.assertIn("harness_pins_unavailable", report["errors"])
        self.assertIsNone(report["harness_repository"]["selected_contains_required"])

    def test_native_revision_is_the_resolved_host_edge_even_during_pin_disagreement(self):
        actual = self.commit("selected-change", "selected")
        self.write_pins(self.base, lock_revision=actual)
        code, report = self.invoke("--strict", "--harness-repo", str(self.repo),
                                  "--required-harness-ancestor", actual)
        self.assertEqual(code, 1)
        self.assertIn("harness_pin_disagreement", report["errors"])
        self.assertEqual(report["harness_pins"]["selected_native_revision"], actual)
        self.assertTrue(report["harness_repository"]["selected_contains_required"])

    def test_required_harness_ancestor_uses_selected_commit_not_main(self):
        descendant = self.commit("harness-change", "one")
        self.write_pins(descendant)
        self.git("checkout", "--detach", descendant)
        self.git("branch", "-f", "main", self.base)
        code, report = self.invoke("--strict", "--harness-repo", str(self.repo), "--required-harness-ancestor", self.base)
        self.assertEqual(code, 0)
        self.assertTrue(report["harness_repository"]["selected_contains_required"])

    def test_noncontained_required_ancestor_fails(self):
        self.git("checkout", "-b", "unrelated")
        other = self.commit("other", "one")
        code, report = self.invoke("--strict", "--harness-repo", str(self.repo), "--required-harness-ancestor", other)
        self.assertEqual(code, 1)
        self.assertFalse(report["harness_repository"]["selected_contains_required"])

    def test_missing_selected_and_required_objects_fail(self):
        missing = "f" * 40
        self.write_pins(missing)
        code, report = self.invoke("--strict", "--harness-repo", str(self.repo), "--required-harness-ancestor", "e" * 40)
        self.assertEqual(code, 1)
        self.assertIn("selected_harness_object_missing", report["errors"])
        self.assertIn("required_harness_object_missing", report["errors"])
        self.assertIsNone(report["harness_repository"]["selected_contains_required"])

    def test_missing_cached_remote_is_unknown(self):
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        code, report = self.invoke("--strict")
        self.assertEqual(code, 1)
        self.assertIsNone(report["remote_main"]["head"])
        self.assertIn("cached_remote_main_unknown", report["errors"])
        self.assertEqual(self.invoke()[0], 0)

    def test_invalid_explicit_ref_fails_even_without_strict(self):
        code, report = self.invoke("--candidate=--not-an-option")
        self.assertEqual(code, 1)
        self.assertIsNone(report["candidates"][0]["head"])

    def test_source_repository_mismatch_and_credentials_are_not_printed(self):
        text = (self.repo / "Cargo.toml").read_text()
        (self.repo / "Cargo.toml").write_text(text.replace("https://github.com/tidepool-heavy-industries/exomonad-harness.git", "https://private-token@github.com/other/harness.git"))
        code, report = self.invoke("--strict")
        self.assertEqual(code, 1)
        self.assertNotIn("private-token", json.dumps(report))

    def test_configured_main_upstream_selects_its_cached_ref(self):
        self.git("config", "remote.mirror.url", "https://private-token@example.invalid/repo")
        self.git("config", "remote.mirror.fetch", "+refs/heads/*:refs/remotes/mirror/*")
        self.git("config", "branch.main.remote", "mirror")
        self.git("config", "branch.main.merge", "refs/heads/main")
        self.git("update-ref", "refs/remotes/mirror/main", self.base)
        code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertEqual(report["remote_main"]["ref"], "refs/remotes/mirror/main")
        self.assertEqual(report["remote_main"]["selection"], "main_upstream")
        self.assertNotIn("private-token", json.dumps(report))

    def test_missing_local_main_fails_strict(self):
        self.git("checkout", "-b", "feature")
        self.git("branch", "-d", "main")
        code, report = self.invoke("--strict")
        self.assertEqual(code, 1)
        self.assertIn("local_main_missing", report["errors"])

    def test_malformed_pin_data_is_unknown_without_leaking_text(self):
        (self.repo / "Cargo.toml").write_text("private-token unparseable")
        code, report = self.invoke("--strict")
        self.assertEqual(code, 1)
        self.assertIn("harness_pins_unavailable", report["errors"])
        self.assertNotIn("private-token", json.dumps(report))

    def test_nested_directory_resolves_checkout_root(self):
        nested = self.repo / "subdir"
        nested.mkdir()
        report = STATUS.observe(nested)
        self.assertEqual(report["checkout"]["path"], str(self.repo))
        self.assertTrue(report["strict_ok"])

    def test_non_repository_is_an_operational_failure(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = STATUS.main(["--repo", self.temp.name, "--json"])
        self.assertEqual(code, 1)
        self.assertEqual(json.loads(output.getvalue())["errors"], ["checkout_observation_unavailable"])

    def test_dirty_tracked_work_is_observed_not_rejected(self):
        (self.repo / "base").write_text("work in progress")
        code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertEqual(report["checkout"]["tracked_change_count"], 1)
        self.assertEqual(report["harness_pins"]["observation"], "working_tree_files")
        self.assertEqual((self.repo / "base").read_text(), "work in progress")

    def test_explicit_repository_ignores_ambient_git_location(self):
        other = Path(self.temp.name) / "other"
        other.mkdir()
        self.git("init", "-b", "other", repo=other)
        with patch.dict(os.environ, {"GIT_DIR": str(other / ".git"), "GIT_WORK_TREE": str(other)}):
            code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertEqual(report["checkout"]["head"], self.base)
        self.assertEqual(report["checkout"]["branch"], "main")

    def test_nix_comments_do_not_shadow_selected_input(self):
        source = (self.repo / "flake.nix").read_text()
        shadow = '# harnessWeb = { url = "github:other/harness/' + "f" * 40 + '"; };\n'
        (self.repo / "flake.nix").write_text(shadow + source)
        code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertEqual(report["harness_pins"]["sources"]["flake_manifest"]["revision"], self.base)

    def test_nix_expression_is_evaluated_without_outputs(self):
        (self.repo / "flake.nix").write_text(
            'let revision = "' + self.base + '"; in { inputs.harnessWeb.url = '
            '"github:tidepool-heavy-industries/exomonad-harness/" + revision; outputs = throw "outputs must not be evaluated"; }'
        )
        code, report = self.invoke("--strict")
        self.assertEqual(code, 0)
        self.assertTrue(report["harness_pins"]["agreement"])

    def test_unavailable_nix_evaluator_is_unknown_and_strict_failure(self):
        with patch.dict(os.environ, {"EXOMONAD_NIX_BIN": str(self.repo / "missing-nix")}):
            code, report = self.invoke("--strict")
        self.assertEqual(code, 1)
        self.assertIn("harness_pins_unavailable", report["errors"])

    def test_invalid_nix_expression_is_unknown_and_strict_failure(self):
        (self.repo / "flake.nix").write_text("invalid-private-token {")
        code, report = self.invoke("--strict")
        self.assertEqual(code, 1)
        self.assertIn("harness_pins_unavailable", report["errors"])
        self.assertNotIn("private-token", json.dumps(report))

    def test_observation_does_not_change_refs_or_source_files(self):
        before = self.git("show-ref")
        files = {p.name: p.read_bytes() for p in self.repo.iterdir() if p.is_file()}
        self.invoke("--strict", "--candidate", "main", "--harness-repo", str(self.repo), "--required-harness-ancestor", self.base)
        self.assertEqual(self.git("show-ref"), before)
        self.assertEqual({p.name: p.read_bytes() for p in self.repo.iterdir() if p.is_file()}, files)


if __name__ == "__main__":
    unittest.main()
