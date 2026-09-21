#!/usr/bin/env python3
"""Choose affected Cargo targets from Cargo's workspace graph."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

# These packages' unit tests do not launch the extractor. Keep this explicit:
# default nextest filters constrain execution, not Cargo's build graph.
EXTRACTOR_FREE = (
    "tidepool-repr", "tidepool-heap", "tidepool-bignum", "tidepool-effect",
    "tidepool-bridge", "tidepool-bridge-derive", "tidepool-bridge-effects",
    "tidepool-codegen",
)


def select(metadata, changed, root):
    packages = {p["name"]: p for p in metadata["packages"]
                if p["id"] in metadata["workspace_members"]}
    roots = {name: Path(p["manifest_path"]).parent.relative_to(root)
             for name, p in packages.items()}
    selections = {}
    production = set()
    actions = set()
    reasons = set()

    def add(name, kind, target=""):
        selections.setdefault(name, set()).add((kind, target))

    def all_tests(name):
        for target in packages[name]["targets"]:
            if not target.get("test", True):
                continue
            kinds = target["kind"]
            if any(k in kinds for k in ("lib", "proc-macro")):
                add(name, "lib")
            elif "test" in kinds:
                add(name, "test", target["name"])
            elif "bin" in kinds:
                add(name, "bin", target["name"])

    for path in changed:
        file = Path(path)
        if path in ("Cargo.toml", "Cargo.lock", "flake.nix", "flake.lock", "rust-toolchain.toml", "justfile") or path.startswith((".cargo/", ".config/", "scripts/", "dev/")):
            reasons.add(path)
            continue
        if path.startswith("haskell/"):
            if file.suffix in (".hs", ".cabal", ".cbor", ".json") or file.name.startswith("cabal.project"):
                actions.add("haskell")
                if path.startswith(("haskell/src/", "haskell/app/", "haskell/test/", "haskell/test-prepared-stg/", "haskell/test-execution-corpus/")):
                    actions.add("fixtures")
                all_tests("tidepool-runtime")
            continue
        owner = next((n for n, directory in roots.items() if file.is_relative_to(directory)), None)
        if owner is None:
            if file.suffix in (".rs", ".hs", ".toml", ".cbor"):
                reasons.add(path)
            continue
        relative = file.relative_to(roots[owner])
        if relative.parts[0] == "tests":
            matched = False
            for target in packages[owner]["targets"]:
                if "test" not in target["kind"]:
                    continue
                source = Path(target["src_path"])
                members = {source}
                if source.exists():
                    members.update((source.parent / p).resolve() for p in
                                   re.findall(r'#\[path\s*=\s*"([^"]+)"\]', source.read_text()))
                if (root / file).resolve() in members:
                    add(owner, "test", target["name"])
                    matched = True
            # Shared fixtures, helpers, deleted/new unregistered leaves: check
            # registration and conservatively select this package's test targets.
            if not matched:
                all_tests(owner)
            actions.add("registration")
        elif relative.parts[0] in ("src", "examples", "benches") or relative.name in ("Cargo.toml", "build.rs"):
            all_tests(owner)
            production.add(owner)
        elif file.suffix not in (".md", ".txt", ".png", ".svg"):
            # Build inputs and generated contracts belong to their package.
            all_tests(owner)
            production.add(owner)

    downstream = set(production)
    while True:
        consumers = {name for name, p in packages.items() if any(
            dep["name"] in downstream
            for dep in p["dependencies"])}
        expanded = downstream | consumers
        if expanded == downstream:
            break
        downstream = expanded
    return selections, downstream, actions, reasons


def commands(selections, downstream, actions):
    result = []
    if selections:
        result.append(["cargo", "fmt", *[arg for name in sorted(selections) for arg in ("-p", name)], "--", "--check"])
    if downstream:
        result.append(["cargo", "check", "--all-targets", *[arg for name in sorted(downstream) for arg in ("-p", name)]])
    if "registration" in actions:
        result.append(["scripts/test-suite-check.sh"])
    if "haskell" in actions:
        result.append(["bash", "-c", "cd haskell && cabal build all"])
    for name, targets in sorted(selections.items()):
        # One invocation per package, deduplicating unit and suite targets.
        prefix = (["cargo", "nextest", "run", "--profile", "battery", "--no-fail-fast"]
                  if name in EXTRACTOR_FREE else ["scripts/battery.sh"])
        flags = [arg for kind, target in sorted(targets)
                 for arg in (["--lib"] if kind == "lib" else ["--" + kind, target])]
        result.append([*prefix, "-p", name, *flags])
    if "fixtures" in actions:
        result.append(["scripts/fixtures.sh", "check"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="HEAD")
    parser.add_argument("--list", action="store_true", help="print commands without executing")
    parser.add_argument("--quick-packages", action="store_true")
    args = parser.parse_args()
    if args.quick_packages:
        print("\n".join(EXTRACTOR_FREE))
        return 0
    root = Path.cwd().resolve()
    # check=True matters: an invalid revision must never become a clean tree.
    paths = subprocess.check_output(["git", "diff", "--name-only", "-z", args.base]).split(b"\0")
    paths += subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"]).split(b"\0")
    changed = sorted({p.decode() for p in paths if p})
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"]))
    selections, downstream, actions, reasons = select(metadata, changed, root)
    if reasons:
        print("Shared build or unmapped executable inputs changed; run just verify at integration:\n  " + "\n  ".join(sorted(reasons)), file=sys.stderr)
        return 2
    work = commands(selections, downstream, actions)
    if not work:
        print("No executable checks selected (clean tree or documentation-only changes).")
        return 0
    import shlex
    status = 0
    for command in work:
        print("==> " + shlex.join(command), flush=True)
        if not args.list:
            status = max(status, subprocess.run(command).returncode != 0)
    return status


if __name__ == "__main__":
    sys.exit(main())
