"""Merge declared source trees, refusing contradictory bytes or unsafe paths."""
import argparse
import os
from pathlib import Path, PurePosixPath


def capture(trees, output, exclude_names=(), reject_symlinks=False):
    excluded = set(exclude_names)
    if any(not name or name in {".", ".."} or "/" in name for name in excluded):
        raise ValueError("excluded source names must be basenames")
    def walk_error(error):
        raise error

    captured = {}
    for prefix, tree in trees:
        prefix = PurePosixPath(prefix)
        if prefix.is_absolute() or ".." in prefix.parts:
            raise ValueError(f"unsafe source prefix {prefix}")
        tree = Path(tree)
        if not tree.is_dir():
            raise ValueError(f"source input is not a directory: {tree}")
        for directory, directories, names in os.walk(tree, onerror=walk_error):
            directories[:] = sorted(name for name in directories if name not in excluded)
            if reject_symlinks:
                for name in directories + [name for name in names if name not in excluded]:
                    source = Path(directory) / name
                    if source.is_symlink():
                        raise ValueError(f"declared source tree contains a symlink: {source}")
            for name in sorted(names):
                if name in excluded:
                    continue
                source = Path(directory) / name
                if not source.is_file():
                    raise ValueError(f"declared source is not a readable file: {source}")
                relative = prefix / source.relative_to(tree).as_posix()
                if relative.is_absolute() or ".." in relative.parts:
                    raise ValueError(f"unsafe source path {relative}")
                contents = source.read_bytes()
                if relative in captured and captured[relative] != contents:
                    raise ValueError(f"contradictory declared bytes for {relative}")
                captured[relative] = contents
    if not captured:
        raise ValueError("qualification source capture is empty")
    output.mkdir()
    for relative, contents in sorted(captured.items()):
        destination = output / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(contents)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", action="append", nargs=2, default=[])
    parser.add_argument("--output", required=True)
    parser.add_argument("--exclude-name", action="append", default=[])
    parser.add_argument("--reject-symlinks", action="store_true")
    args = parser.parse_args()
    capture(args.tree, Path(args.output), args.exclude_name, args.reject_symlinks)


if __name__ == "__main__":
    main()
