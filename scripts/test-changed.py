#!/usr/bin/env python3
"""Choose affected Cargo targets from Cargo's workspace graph."""
import argparse
from enum import IntEnum
import json
from pathlib import Path
import re
import subprocess
import sys

# Also support importlib-based unit tests without changing their caller cwd.
sys.path.insert(0, str(Path(__file__).parent))
from fixture_dependencies import affected as affected_fixtures

# These packages' unit tests do not launch the extractor. Keep this explicit:
# default nextest filters constrain execution, not Cargo's build graph.
EXTRACTOR_FREE = (
    "tidepool-repr", "tidepool-heap", "tidepool-bignum", "tidepool-effect",
    "tidepool-bridge", "tidepool-bridge-derive", "tidepool-bridge-effects",
    "tidepool-codegen",
)

# Retained reference source has no supported build or test obligation.
RETIRED_SOURCES = (
    Path("tidepool-harness"), Path("tidepool-web"),
    Path("tidepool/src/bin/tidepool-selfharness.rs"),
    Path("tidepool/src/bin/tidepool-selfharness"),
)


class CheckObligation(IntEnum):
    """How a changed package reaches a Cargo consumer.

    A normal or build dependency changes the consumer's production build and
    therefore propagates to that consumer's own production users. A dev
    dependency only changes that package's test/build obligation; it must be
    checked, but must not turn its users into downstream production consumers.
    """

    DEVELOPMENT = 1
    PRODUCTION = 2


def cabal_components(root, changed):
    """Derive source ownership from Cabal's component declarations."""
    manifest = root / "haskell/tidepool-extract.cabal"
    try:
        text = manifest.read_text()
    except OSError:
        return ["all"]
    if any(path.endswith(".cabal") or Path(path).name.startswith("cabal.project") for path in changed):
        return ["all"]
    if cabal_embedded_library_source_changes(text, changed):
        return ["all"]
    selected = set()
    components = {}
    for component in re.split(r"(?m)^(?=(?:library|executable|test-suite) )", text)[1:]:
        name = component.splitlines()[0].split()[1]
        components[name] = component
        directories = re.search(r"(?m)^  hs-source-dirs:\s*([^\n]+)", component)
        if directories is None:
            return ["all"]
        roots = [root / "haskell" / directory for directory in re.split(r"[,\s]+", directories[1].strip())]
        if any((root / path).is_relative_to(directory) for path in changed for directory in roots):
            selected.add(name)
    while True:
        consumers = {name for name, body in components.items()
                     if any(re.search(r"(?<![\w-])" + re.escape(dependency) + r"(?![\w-])", body)
                            for dependency in selected)}
        expanded = selected | consumers
        if expanded == selected:
            break
        selected = expanded
    return sorted(selected) or ["all"]


def cabal_embedded_library_source_changes(manifest, changed):
    """Whether a library source embedded into the worker changed.

    The manifest's library entries under extra-source-files are Template
    Haskell inputs to the internal compiler library. Other extra sources are
    distribution fixtures and retain their ordinary component ownership.
    """
    package_fields = re.split(r"(?m)^(?=(?:library|executable|test-suite) )", manifest)[0]
    extra_sources = re.search(
        r"(?ms)^extra-source-files:\s*(.*?)(?=^\S|\Z)", package_fields)
    if extra_sources is None:
        return False
    patterns = [entry for line in extra_sources[1].splitlines()
                for entry in re.split(r"[,\s]+", line.split("--", 1)[0].strip()) if entry]
    haskell_changes = [Path(path).relative_to("haskell") for path in changed
                       if Path(path).is_relative_to("haskell/lib")]
    return any(path.match(pattern) for path in haskell_changes for pattern in patterns)


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
        if any(file.is_relative_to(retired) for retired in RETIRED_SOURCES):
            continue
        if path in ("Cargo.toml", "Cargo.lock", "flake.nix", "flake.lock", "rust-toolchain.toml", "justfile") or path.startswith((".cargo/", ".config/", "scripts/", "dev/")):
            reasons.add(path)
            continue
        if path.startswith("haskell/"):
            if file.suffix in (".hs", ".cabal", ".cbor", ".json") or file.name.startswith("cabal.project"):
                actions.add("haskell")
                if path.startswith(("haskell/src/", "haskell/app/", "haskell/lib/", "haskell/test/", "haskell/test-prepared-stg/", "haskell/test-execution-corpus/")):
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

    # Compiler and schema boundaries require the full structural corpus. An
    # ordinary library edit can use the worker's consumed-source evidence.
    try:
        cabal_manifest = (root / "haskell/tidepool-extract.cabal").read_text()
    except OSError:
        cabal_manifest = ""
    structural = (any(path.startswith(("haskell/src/", "haskell/app/", "tidepool-repr/src/", "tidepool-protocol/src/")) and Path(path).suffix != ".md" for path in changed)
                  or cabal_embedded_library_source_changes(cabal_manifest, changed))
    if structural:
        actions.add("fixtures")
    elif "fixtures" in actions and all(path.startswith("haskell/lib/") or Path(path).suffix == ".md" for path in changed):
        cohorts = affected_fixtures(root / "target/prepared-corpus/dependencies.json", changed, root)
        if cohorts is not None:
            actions.remove("fixtures")
            actions.update("fixture:" + cohort for cohort in cohorts)

    obligations = {name: CheckObligation.PRODUCTION for name in production}
    while True:
        updated = False
        for name, package in packages.items():
            required = None
            for dependency in package["dependencies"]:
                upstream = obligations.get(dependency["name"])
                # A development obligation ends at its direct consumer. Its
                # users consume that package's production surface, which did
                # not change merely because a test-only dependency did.
                if upstream is not CheckObligation.PRODUCTION:
                    continue
                # Cargo's metadata uses null for a normal dependency. A build
                # dependency changes the consumer's production build too;
                # dev-dependencies only require this consumer's all-target
                # check and deliberately do not propagate further.
                edge = (CheckObligation.DEVELOPMENT
                        if dependency.get("kind") == "dev"
                        else CheckObligation.PRODUCTION)
                required = edge if required is None else max(required, edge)
            if required is not None and required > obligations.get(name, 0):
                obligations[name] = required
                updated = True
        if not updated:
            break
    return selections, obligations, actions, reasons


def commands(selections, obligations, actions, components=None):
    result = []
    if selections:
        result.append(["cargo", "fmt", *[arg for name in sorted(selections) for arg in ("-p", name)], "--", "--check"])
    if obligations:
        result.append(["cargo", "check", "--all-targets", *[arg for name in sorted(obligations) for arg in ("-p", name)]])
    if "registration" in actions:
        result.append(["scripts/test-suite-check.sh"])
    if "haskell" in actions:
        result.append(["bash", "-c", 'cd haskell && cabal build "$@"', "cabal-components", *(components or ["all"])])
    for name, targets in sorted(selections.items()):
        # One invocation per package, deduplicating unit and suite targets.
        prefix = (["cargo", "nextest", "run", "--profile", "battery", "--no-fail-fast"]
                  if name in EXTRACTOR_FREE else ["scripts/battery.sh"])
        flags = [arg for kind, target in sorted(targets)
                 for arg in (["--lib"] if kind == "lib" else ["--" + kind, target])]
        result.append([*prefix, "-p", name, *flags])
    if "fixtures" in actions:
        result.append(["scripts/fixtures.sh", "check"])
    else:
        cohorts = sorted(action.removeprefix("fixture:") for action in actions if action.startswith("fixture:"))
        if cohorts:
            result.append(["scripts/fixtures.sh", "check", *cohorts])
    return result


def requires_compiler(work, actions):
    """Whether this selected work will actually invoke extractor-backed code."""
    return bool(actions.intersection({"fixtures"})
                or any(action.startswith("fixture:") for action in actions)
                or any(command[0] == "scripts/battery.sh" for command in work))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="HEAD")
    parser.add_argument("--list", action="store_true", help="print commands without executing")
    parser.add_argument("--quick-packages", action="store_true")
    parser.add_argument("--in-compiler-run", action="store_true", help=argparse.SUPPRESS)
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
    selections, obligations, actions, reasons = select(metadata, changed, root)
    if reasons:
        print("Shared build or unmapped executable inputs changed; run just verify at integration:\n  " + "\n  ".join(sorted(reasons)), file=sys.stderr)
        return 2
    work = commands(selections, obligations, actions, cabal_components(root, changed))
    if not work:
        print("No executable checks selected (clean tree or documentation-only changes).")
        return 0
    # One owner keeps the daemon alive across every selected target. Nested
    # battery wrappers inherit its endpoint and do not retire it themselves.
    needs_compiler = requires_compiler(work, actions)
    if not args.list and not args.in_compiler_run and needs_compiler:
        return subprocess.run([
            "bash", "-c",
            'source scripts/lib-extract.sh\n'
            'resolve_tidepool_extract\n'
            'trap teardown_battery_daemon EXIT INT TERM\n'
            'start_battery_daemon\n'
            '"$@"',
            "selected-checks", sys.executable, str(Path(__file__).resolve()),
            *sys.argv[1:], "--in-compiler-run",
        ]).returncode
    import shlex
    status = 0
    for command in work:
        print("==> " + shlex.join(command), flush=True)
        if not args.list:
            status = max(status, subprocess.run(command).returncode != 0)
    return status


if __name__ == "__main__":
    sys.exit(main())
