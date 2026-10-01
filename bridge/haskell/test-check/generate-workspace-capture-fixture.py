#!/usr/bin/env python3
"""Render the narrow Workspace interface needed by source-capture smoke.

This is a source-capture fixture, not a frozen workspace selection: the
identity, module list, and prompt catalog are intentionally not represented.
The selected recipe only consumes workspaceRoot, which is derived from the
same config value and template inputs used by the production workspace owner.
"""

from __future__ import annotations

import json
import pathlib
import sys
import tomllib


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit(
            "usage: generate-workspace-capture-fixture.py TEMPLATE CONFIG OUTPUT"
        )

    template_path, config_path, output_path = map(pathlib.Path, sys.argv[1:])
    config = tomllib.loads(config_path.read_text())
    roots = config.get("haskell", {}).get("source_roots", [])
    selected_root = pathlib.PurePosixPath(roots[-1] if roots else ".")
    workspace_root = pathlib.PurePosixPath(".exomonad").joinpath(selected_root)
    workspace_root = pathlib.PurePosixPath(
        *(part for part in workspace_root.parts if part != ".")
    )

    # Workspace.hs is the production template; this source-capture-only module
    # fills just the bindings required for typechecking Project.Checks.
    source = template_path.read_text().rstrip()
    source += "\n"
    source += 'workspaceIdentity = "source-capture-fixture"\n'
    source += f"workspaceRoot = {json.dumps(workspace_root.as_posix(), ensure_ascii=False)}\n"
    source += "workspaceModules = []\n"
    source += "workspacePrompts = []\n"
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(source)
    print(f"generated source-capture Workspace fixture for {workspace_root}")


if __name__ == "__main__":
    main()
