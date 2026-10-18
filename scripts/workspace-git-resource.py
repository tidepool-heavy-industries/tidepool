#!/usr/bin/env python3
"""Retain the recorded workspace commit as a Git bundle and selected sources."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


# These skills have one authored source and must be published unchanged in the
# default workspace. This is a publication invariant, not a behavior check.
PUBLISHED_SKILLS = (
    "exomonad-agent-spec/SKILL.md",
    "exomonad-command/references/hosted-tools.md",
)


PROJECTED_SOURCE_SUFFIXES = frozenset({".hs", ".hs-boot", ".json", ".cbor", ".txt"})


def git_environment():
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    return environment


def recorded_workspace(source: Path, git: str):
    environment = git_environment()

    def run(directory, *arguments):
        return subprocess.check_output([git, "-C", str(directory), *arguments],
                                       env=environment, text=True).strip()

    descriptor = source / "build/native-workspace-gitlink.json"
    record = json.loads(descriptor.read_text())
    revision = record.get("revision")
    if (not isinstance(revision, str) or re.fullmatch(r"[0-9a-f]{40}", revision) is None
            or record != {"schema": 1, "path": ".exomonad/workspace", "mode": "160000", "revision": revision}):
        raise ValueError("workspace resource requires the generated exact Gitlink descriptor")
    if run(source, "ls-tree", "HEAD", "--", record["path"]) != f'160000 commit {revision}\t{record["path"]}':
        raise ValueError("workspace Gitlink descriptor differs from the committed source Gitlink")
    if run(source, "ls-files", "--stage", "--", record["path"]) != f'160000 {revision} 0\t{record["path"]}':
        raise ValueError("workspace Gitlink descriptor differs from the source index")
    repository = source / record["path"]
    if run(repository, "rev-parse", "--show-toplevel") != str(repository.resolve(strict=True)):
        raise ValueError("initialize the recorded workspace submodule before configuration")
    if run(repository, "cat-file", "-t", revision) != "commit":
        raise ValueError("recorded workspace object is not a commit")
    return record, repository


def selected_source_paths(source: Path, git: str) -> tuple[str, ...]:
    record, repository = recorded_workspace(source, git)
    return _source_paths(repository, record["revision"], git)


def _source_paths(repository: Path, revision: str, git: str) -> tuple[str, ...]:
    tree = subprocess.check_output([git, "-C", str(repository), "ls-tree", "-rz", revision],
                                   env=git_environment())
    selected = []
    for entry in tree.split(b"\0"):
        if not entry:
            continue
        header, encoded_path = entry.split(b"\t", 1)
        relative = encoded_path.decode("utf-8")
        if Path(relative).suffix not in PROJECTED_SOURCE_SUFFIXES:
            continue
        mode, kind, _ = header.decode("ascii").split(" ")
        if mode not in {"100644", "100755"} or kind != "blob":
            raise ValueError(f"workspace source is not an ordinary committed file: {relative}")
        selected.append(relative)
    return tuple(sorted(selected))


def provision(source: Path, output: Path, git: str) -> str:
    record, repository = recorded_workspace(source, git)
    revision = record["revision"]
    descriptor = source / "build/native-workspace-gitlink.json"
    environment = git_environment()

    def run(directory, *arguments):
        return subprocess.check_output([git, "-C", str(directory), *arguments],
                                       env=environment, text=True).strip()

    templates = source / "exomonad/examples/workspace/.exomonad/skills"
    for relative in PUBLISHED_SKILLS:
        published = subprocess.check_output(
            [git, "-C", str(repository), "show", f"{revision}:skills/{relative}"],
            env=environment,
        )
        if published != (templates / relative).read_bytes():
            raise ValueError(
                f"workspace skill publication differs from its authored template: {relative}; "
                "publish the template and record its reviewed workspace commit"
            )
    projected = {
        relative: subprocess.check_output(
            [git, "-C", str(repository), "show", f"{revision}:{relative}"],
            env=environment,
        )
        for relative in _source_paths(repository, revision, git)
    }
    tree = run(repository, "rev-parse", revision + "^{tree}")
    output.mkdir()
    # Import original objects into an isolated repository; never change source
    # refs or reconstruct a commit from working files.
    with tempfile.TemporaryDirectory(prefix="workspace-git-", dir=output.parent) as temporary:
        isolated = Path(temporary)
        run(isolated, "init", "--bare", "--quiet")
        run(isolated, "-c", "protocol.file.allow=always", "fetch", "--quiet", "--no-tags",
            str(repository.resolve()), revision)
        run(isolated, "update-ref", "refs/heads/workspace", revision)
        run(isolated, "symbolic-ref", "HEAD", "refs/heads/workspace")
        if run(isolated, "rev-parse", "refs/heads/workspace^{tree}") != tree:
            raise ValueError("workspace resource differs from the original committed tree")
        run(isolated, "-c", "pack.threads=1", "bundle", "create",
            str((output / "workspace.bundle").resolve()), "HEAD", "refs/heads/workspace")
        if run(isolated, "bundle", "list-heads", str(output / "workspace.bundle")) != f"{revision} HEAD\n{revision} refs/heads/workspace":
            raise ValueError("workspace bundle does not advertise its exact recorded commit")
        run(isolated, "bundle", "verify", str(output / "workspace.bundle"))
    # Publish only the declared original bytes, never the mutable checkout.
    for relative, original in projected.items():
        destination = output / "source" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(original)
    (output / "workspace-gitlink.json").write_bytes(descriptor.read_bytes())
    return revision


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--git", required=True)
    arguments = parser.parse_args()
    print(provision(arguments.source_root.resolve(strict=True), arguments.output.absolute(), arguments.git))


if __name__ == "__main__":
    main()
