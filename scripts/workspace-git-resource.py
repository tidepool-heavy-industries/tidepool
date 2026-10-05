#!/usr/bin/env python3
"""Retain the recorded workspace commit as a self-contained Git bundle."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def provision(source: Path, output: Path, git: str) -> str:
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)

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
