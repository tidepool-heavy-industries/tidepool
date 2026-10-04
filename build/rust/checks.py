"""Fail declared formatting or compiler diagnostics without invoking Cargo."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def check_diagnostics(label, path):
    failures = []
    for line in Path(path).read_text().splitlines():
        item = json.loads(line)
        diagnostic = item.get("message", item) if isinstance(item.get("message"), dict) else item
        if diagnostic.get("level") in ("warning", "error", "fatal"):
            failures.append(label + ": " + diagnostic.get("rendered", diagnostic["message"]))
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rustfmt", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--source-tree", action="append", default=[])
    parser.add_argument("--diagnostic", action="append", nargs=2, default=[])
    args = parser.parse_args()
    sources = {}
    failures = []
    for root in args.source_tree:
        root = Path(root)
        for path in sorted(root.rglob("*.rs")):
            relative = path.relative_to(root).as_posix()
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            if relative in sources and sources[relative][0] != digest:
                raise RuntimeError(f"conflicting declared Rust source bytes for {relative}")
            sources[relative] = (digest, path)
    if not sources or not args.diagnostic:
        parser.error("native checks require nonempty source and target inventories")
    # Each source group preserves repository paths. Child modules are checked
    # as declared files, so rustfmt never follows ambient module paths.
    result = subprocess.run([args.rustfmt, "--check", "--edition", "2021", "--config",
                             "style_edition=2021,skip_children=true",
                             *(str(row[1]) for row in sources.values())])
    for label, path in args.diagnostic:
        failures.extend(check_diagnostics(label, path))
    if failures:
        print("\n".join(failures))
    if result.returncode or failures:
        raise SystemExit(1)
    Path(args.output).write_text(json.dumps({"schema": 1, "sources": {key: row[0] for key, row in sorted(sources.items())},
        "targets": {label: hashlib.sha256(Path(path).read_bytes()).hexdigest() for label, path in args.diagnostic}}, indent=2) + "\n")


if __name__ == "__main__":
    main()
