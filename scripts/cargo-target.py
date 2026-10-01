#!/usr/bin/env python3
"""Resolve a Cargo target directory belonging to one canonical checkout."""

import argparse
import hashlib
from pathlib import Path
import subprocess
import sys


def resolve_target(checkout, requested=None, cwd=None):
    checkout = Path(checkout).resolve()
    if requested is None:
        requested = checkout / "target"
    target = Path(requested)
    if not target.is_absolute():
        target = Path(cwd or Path.cwd()) / target
    target = target.resolve()
    # Inspect existing ancestors without creating a directory or claiming any
    # output. A different Git checkout must retain its own build state.
    ancestor = target
    while not ancestor.is_dir() and ancestor != ancestor.parent:
        ancestor = ancestor.parent
    observed = subprocess.run(
        ["git", "-C", str(ancestor), "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=False,
    )
    if observed.returncode == 0:
        other = Path(observed.stdout.strip()).resolve()
        if other != checkout and target.is_relative_to(other):
            raise ValueError(
                f"Cargo target {target} belongs to another checkout ({other}); "
                "use this checkout's target or an external build base"
            )
    if target.is_relative_to(checkout):
        return target

    suffix = "tidepool-" + hashlib.sha256(str(checkout).encode()).hexdigest()[:16]
    # Nested dev shells inherit the resolved directory rather than adding a
    # second suffix. The identity remains tied to the canonical source path.
    return target if target.name == suffix else target / suffix


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkout")
    parser.add_argument("requested", nargs="?")
    options = parser.parse_args()
    try:
        target = resolve_target(options.checkout, options.requested)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2
    print(target)
    return 0


if __name__ == "__main__":
    sys.exit(main())
