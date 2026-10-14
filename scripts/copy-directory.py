"""Copy an action input without inheriting read-only output directories."""

import argparse
import os
from pathlib import Path
import stat
import subprocess


def _fail_walk(error: OSError) -> None:
    raise error


def copy_directory(cp: str, source: Path, output: Path) -> None:
    if output.is_symlink():
        raise ValueError("directory output must not be a symlink")
    subprocess.run([cp, "-a", str(source) + "/.", str(output)], check=True)
    # Archive copying preserves executable files and links. Only real output
    # directories need owner-write so Buck can replace or remove the artifact.
    # Directory symlinks remain links; their targets are never traversed.
    for directory, _, _ in os.walk(output, followlinks=False, onerror=_fail_walk):
        path = Path(directory)
        path.chmod(stat.S_IMODE(path.stat().st_mode) | stat.S_IWUSR)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cp", required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    copy_directory(args.cp, args.source, args.output)


if __name__ == "__main__":
    main()
