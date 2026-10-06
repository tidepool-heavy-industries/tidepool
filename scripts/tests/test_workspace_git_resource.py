"""Exercise resource admission with the actual declared workspace commit."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


resource = load("workspace_git_resource", ROOT / "scripts/workspace-git-resource.py")
qualification = load("workspace_qualification", ROOT / "build/package/qualification.py")


class WorkspaceGitResourceTests(unittest.TestCase):
    def setUp(self):
        self.storage = tempfile.TemporaryDirectory()
        self.addCleanup(self.storage.cleanup)
        self.root = Path(self.storage.name)
        self.bundle = Path(os.environ["EXOMONAD_WORKSPACE_GIT_BUNDLE"]).resolve(strict=True)
        self.record = qualification.workspace_gitlink(Path(os.environ["EXOMONAD_WORKSPACE_GITLINK"]))
        self.git = Path(shutil.which("git"))

    def run_git(self, directory, *arguments):
        return subprocess.check_output([str(self.git), "-C", str(directory), *arguments], text=True).strip()

    def source(self):
        source = self.root / "source"
        source.mkdir()
        self.run_git(source, "init", "--quiet")
        descriptor = source / "build/native-workspace-gitlink.json"
        descriptor.parent.mkdir()
        qualification.write_json(descriptor, self.record)
        self.run_git(source, "add", "build/native-workspace-gitlink.json")
        self.run_git(source, "update-index", "--add", "--cacheinfo",
                     f'160000,{self.record["revision"]},.exomonad/workspace')
        self.run_git(source, "-c", "user.name=Resource test", "-c", "user.email=resource@example.invalid",
                     "commit", "--quiet", "-m", "Record original workspace Gitlink")
        workspace = source / ".exomonad/workspace"
        workspace.parent.mkdir()
        self.run_git(source, "-c", "protocol.file.allow=always", "clone", "--quiet", "--",
                     str(self.bundle), str(workspace))
        return source, workspace

    def test_declared_bundle_has_the_real_gitlink_commit_and_complete_objects(self):
        qualification.verify_workspace_bundle(self.bundle, self.record, self.git)

    def test_missing_bundle_refuses(self):
        with self.assertRaises(subprocess.CalledProcessError):
            qualification.verify_workspace_bundle(self.root / "missing.bundle", self.record, self.git)

    def test_different_gitlink_refuses(self):
        with self.assertRaisesRegex(ValueError, "differs from its exact recorded Gitlink"):
            qualification.verify_workspace_bundle(self.bundle, self.record | {"revision": "0" * 40}, self.git)

    def test_provision_preserves_original_objects_and_excludes_dirty_working_bytes(self):
        source, workspace = self.source()
        original_tree = self.run_git(workspace, "rev-parse", self.record["revision"] + "^{tree}")
        tracked = self.run_git(workspace, "ls-files").splitlines()[0]
        (workspace / tracked).write_text("uncommitted bytes cannot enter the resource\n")
        output = self.root / "resource"
        resource.provision(source, output, str(self.git))
        qualification.verify_workspace_bundle(output / "workspace.bundle", self.record, self.git)
        restored = self.root / "restored"
        self.run_git(self.root, "-c", "protocol.file.allow=always", "clone", "--quiet", "--",
                     str(output / "workspace.bundle"), str(restored))
        self.assertEqual(self.run_git(restored, "rev-parse", "HEAD^{tree}"), original_tree)
        self.assertNotEqual((restored / tracked).read_bytes(), (workspace / tracked).read_bytes())
        self.assertEqual(json.loads((output / "workspace-gitlink.json").read_text()), self.record)

    def test_provision_refuses_a_descriptor_outside_the_committed_gitlink(self):
        source, _ = self.source()
        qualification.write_json(source / "build/native-workspace-gitlink.json", self.record | {"revision": "0" * 40})
        with self.assertRaisesRegex(ValueError, "differs from the committed source Gitlink"):
            resource.provision(source, self.root / "resource", str(self.git))
        self.assertFalse((self.root / "resource").exists())

    def test_provision_refuses_an_uninitialized_submodule(self):
        source, workspace = self.source()
        shutil.rmtree(workspace)
        workspace.mkdir()
        with self.assertRaisesRegex(ValueError, "initialize the recorded workspace submodule"):
            resource.provision(source, self.root / "resource", str(self.git))
        self.assertFalse((self.root / "resource").exists())

    def test_provision_refuses_an_index_gitlink_outside_the_committed_source(self):
        source, workspace = self.source()
        previous = self.run_git(workspace, "rev-parse", self.record["revision"] + "^")
        self.run_git(source, "update-index", "--cacheinfo", f'160000,{previous},.exomonad/workspace')
        with self.assertRaisesRegex(ValueError, "differs from the source index"):
            resource.provision(source, self.root / "resource", str(self.git))
        self.assertFalse((self.root / "resource").exists())

    def test_frozen_inventory_seals_bundle_bytes_and_carries_its_explicit_path(self):
        frozen = self.root / "frozen"
        shared = frozen / "share/exomonad"
        shared.mkdir(parents=True)
        copied = shared / "workspace.bundle"
        shutil.copyfile(self.bundle, copied)
        (shared / "ghc-libdir.txt").write_text(str(self.root) + "\n")
        tools = Path(os.environ["TIDEPOOL_RUNTIME_TOOLS"]).resolve(strict=True)
        (shared / "runtime-tools").symlink_to(tools)
        environment = qualification.native_environment(frozen)
        self.assertEqual(environment["EXOMONAD_WORKSPACE_GIT_BUNDLE"], str(copied))
        search = self.root / "search"
        search.mkdir()
        (search / "input.txt").write_text("packaged search witness\n")
        command_environment = {"PATH": environment["PATH"], "LC_ALL": "C"}
        self.assertEqual(subprocess.check_output(
            [str(tools / "bin/bash"), "-c", "rg --fixed-strings --line-number 'packaged search witness' ."],
            cwd=search, env=command_environment, text=True).strip(), "./input.txt:1:packaged search witness")
        self.assertEqual(subprocess.check_output(
            [str(tools / "bin/bash"), "-c", "find . -type f -name input.txt"],
            cwd=search, env=command_environment, text=True).strip(), "./input.txt")
        descriptor = {"inventory": qualification.inventory(frozen)}
        descriptor["inventory_sha256"] = qualification.digest_inventory(descriptor["inventory"])
        qualification.verify_frozen_inventory(frozen, descriptor)
        with copied.open("ab") as altered:
            altered.write(b"changed object bytes")
        with self.assertRaisesRegex(ValueError, "inventory changed"):
            qualification.verify_frozen_inventory(frozen, descriptor)
