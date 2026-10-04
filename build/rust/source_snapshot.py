"""Merge declared source trees, refusing contradictory bytes or unsafe paths."""
import argparse
from pathlib import Path, PurePosixPath


def capture(trees, output):
    captured = {}
    for prefix, tree in trees:
        prefix = PurePosixPath(prefix)
        if prefix.is_absolute() or ".." in prefix.parts:
            raise ValueError(f"unsafe source prefix {prefix}")
        tree = Path(tree)
        if not tree.is_dir():
            raise ValueError(f"source input is not a directory: {tree}")
        for source in sorted(tree.rglob("*")):
            if source.is_dir():
                continue
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
    args = parser.parse_args()
    capture(args.tree, Path(args.output))


if __name__ == "__main__":
    main()
