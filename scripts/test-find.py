#!/usr/bin/env python3
"""Print the exact `just` command that runs a named test, without building.

Usage: scripts/test-find.py NAME

NAME matches a test function name (substring). Unit tests under a crate's
src/ run through `just test-lib`; integration tests under tests/ run through
`just test-target` with the Cargo target (a suite root or a standalone file)
that compiles them. Nothing is built: this reads `cargo metadata` and the
sources, so it answers in about a second.
"""
import json
import os
import re
import subprocess
import sys

TEST_ATTR = re.compile(r"#\[(tokio::)?test\b|#\[test_case\b|#\[rstest\b")


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    needle = sys.argv[1]
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root
        )
    )
    members = set(meta["workspace_members"])
    packages = [p for p in meta["packages"] if p["id"] in members]
    # Longest manifest-dir prefix owns a file.
    packages.sort(key=lambda p: len(os.path.dirname(p["manifest_path"])), reverse=True)

    fn_pattern = re.compile(r"\bfn\s+(\w*" + re.escape(needle) + r"\w*)\s*[<(]")
    hits = []
    for package in packages:
        pkg_dir = os.path.dirname(package["manifest_path"])
        for dirpath, dirnames, filenames in os.walk(pkg_dir):
            dirnames[:] = [d for d in dirnames if d not in ("target", ".git")]
            for filename in filenames:
                if not filename.endswith(".rs"):
                    continue
                path = os.path.join(dirpath, filename)
                if owner(packages, path) is not package:
                    continue
                try:
                    lines = open(path, encoding="utf-8").read().splitlines()
                except (OSError, UnicodeDecodeError):
                    continue
                for index, line in enumerate(lines):
                    match = fn_pattern.search(line)
                    if not match:
                        continue
                    window = lines[max(0, index - 4) : index]
                    if not any(TEST_ATTR.search(prior) for prior in window):
                        continue
                    hits.append((package, path, index + 1, match.group(1)))

    if not hits:
        print(f"no test function matching {needle!r}", file=sys.stderr)
        return 1
    for package, path, line, name in hits:
        rel = os.path.relpath(path, root)
        print(f"{rel}:{line}  {name}")
        print(f"  {command(package, path, name)}")
    return 0


def owner(packages, path):
    for package in packages:
        pkg_dir = os.path.dirname(package["manifest_path"]) + os.sep
        if path.startswith(pkg_dir):
            return package
    return None


def command(package, path, name) -> str:
    crate = package["name"]
    pkg_dir = os.path.dirname(package["manifest_path"])
    tests_dir = os.path.join(pkg_dir, "tests") + os.sep
    if not path.startswith(tests_dir):
        return f"just test-lib {crate} 'test({name})'"
    targets = [t for t in package["targets"] if t["kind"] == ["test"]]
    for target in targets:
        if os.path.abspath(target["src_path"]) == path:
            return f"just test-target {crate} {target['name']} 'test({name})'"
    # Suite roots include leaves with #[path = "../leaf.rs"] (or support dirs).
    relative_to_suites = os.path.relpath(path, os.path.join(pkg_dir, "tests", "suites"))
    for target in targets:
        try:
            source = open(target["src_path"], encoding="utf-8").read()
        except OSError:
            continue
        if f'"{relative_to_suites}"' in source:
            return f"just test-target {crate} {target['name']} 'test({name})'"
    return f"just test {crate} 'test({name})'  # no single target found; selects across targets"


if __name__ == "__main__":
    sys.exit(main())
