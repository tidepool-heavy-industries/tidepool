#!/usr/bin/env python3
"""Select declared Buck targets; test discovery and execution belong to their runners."""
import argparse
from functools import partial
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
COHORTS = (
    "suite", "project-work-candidate", "agent-watch-await-settled",
    "recovered-base-contract", "formatting-execution-contract",
    "formatting-dependency-shadow", "fingerprint-execution-contract",
    "time-intrinsic-contract", "containers-contract", "bignum-contract",
    "usertypes-contract", "text-contract",
)
QUICK_PACKAGES = (
    "tidepool-repr", "tidepool-heap", "tidepool-bignum", "tidepool-effect",
    "tidepool-bridge", "tidepool-bridge-derive", "tidepool-bridge-effects",
    "tidepool-codegen",
)
PROFILES = ("fast-dev", "debug", "production")


def roster():
    data = json.loads((ROOT / "build/native-targets.json").read_text())
    if data.get("schema") != 1 or not isinstance(data.get("packages"), dict):
        raise ValueError("unsupported native target roster; regenerate the complete graph")
    return data["packages"]


def package_target(packages, package, kind, target=None):
    if package not in packages:
        raise ValueError(f"unsupported native package {package!r}")
    targets = packages[package][kind]
    if target is None:
        if len(targets) != 1:
            raise ValueError(f"{package}: expected one {kind} target; available: {sorted(targets)}")
        target = next(iter(targets))
    if target not in targets:
        raise ValueError(f"{package}: unsupported {kind} target {target!r}; available: {sorted(targets)}")
    return targets[target]


def buck(command, labels, extra=(), *, profile="fast-dev"):
    if not labels:
        raise ValueError("no declared targets selected")
    if profile not in PROFILES:
        raise ValueError(f"unsupported native profile {profile!r}")
    args = ["bash", str(ROOT / "scripts/buck2-run.sh"), command,
            "--local-only", "-c", "remote.enabled=false",
            "-c", f"tidepool.profile={profile}", *labels]
    if extra:
        # sh_test RunInfo preserves its declared environment and resources.
        # `buck2 test -- ...` instead supplies arguments to the test executor.
        args.extend(["--", *extra])
    print(f"Tidepool native profile: {profile}", file=sys.stderr, flush=True)
    return subprocess.call(args, cwd=ROOT)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=PROFILES, default="fast-dev",
                        help="native compiler profile (default: fast-dev)")
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("test-lib", "test-target", "test-bin", "test-list"):
        child = commands.add_parser(name)
        child.add_argument("package")
        if name in ("test-target", "test-bin"):
            child.add_argument("target")
        if name == "test-list":
            child.add_argument("kind", choices=("libraries", "binaries", "integration"))
            child.add_argument("target", nargs="?")
        else:
            child.add_argument("arguments", nargs=argparse.REMAINDER)
    native = commands.add_parser("test-native")
    native.add_argument("target")
    native.add_argument("arguments", nargs=argparse.REMAINDER)
    for name in ("suite", "suite-plan"):
        commands.add_parser(name).add_argument("package")
    commands.add_parser("quick")
    commands.add_parser("check")
    commands.add_parser("lint")
    commands.add_parser("verify")
    commands.add_parser("fixtures-check").add_argument("cohorts", nargs="*")
    options = parser.parse_args(argv)
    execute = partial(buck, profile=options.profile)
    if options.command == "test-native":
        arguments = options.arguments
        if arguments[:1] == ["--"]:
            arguments = arguments[1:]
        return execute("run", [options.target], arguments)
    packages = roster()
    if options.command in ("test-lib", "test-target", "test-bin", "test-list"):
        kind = getattr(options, "kind", {"test-lib": "libraries", "test-target": "integration", "test-bin": "binaries"}.get(options.command))
        target = package_target(packages, options.package, kind, getattr(options, "target", None))
        if options.command == "test-list":
            return execute("run", [target["test_build"]], ["--list", "--format", "terse"])
        extra = options.arguments
        if extra[:1] == ["--"]:
            extra = extra[1:]
        if any(value.startswith("test(") for value in extra):
            raise ValueError("nextest expressions are retired; use --exact FULL_NAME --expected-count N")
        return execute("run" if extra else "test", [target["test"]], extra)
    if options.command in ("suite", "suite-plan"):
        if options.package not in packages:
            raise ValueError(f"unsupported native package {options.package!r}")
        labels = sorted(target["test"] for target in packages[options.package]["integration"].values())
        if not labels:
            raise ValueError(f"{options.package}: no registered integration targets")
        if options.command == "suite-plan":
            print("\n".join(labels))
            return 0
        return execute("test", labels)
    if options.command == "quick":
        return execute("test", [package_target(packages, name, "libraries")["test"] for name in QUICK_PACKAGES])
    if options.command == "fixtures-check":
        selected = options.cohorts or list(COHORTS)
        unknown = set(selected) - set(COHORTS)
        if unknown or len(set(selected)) != len(selected):
            raise ValueError(f"unknown or duplicate cohorts {selected!r}; available: {', '.join(COHORTS)}")
        labels = ["//bridge/haskell:corpus_" + cohort.replace("-", "_") + "_test" for cohort in selected]
        probe_status = execute("build", ["//bridge/haskell:probe_opacity"])
        test_status = execute("test", labels)
        return probe_status or test_status
    build_labels = set()
    test_labels = set()
    for package in packages.values():
        for kind in ("libraries", "binaries", "integration"):
            for target in package[kind].values():
                if "build" in target:
                    build_labels.add(target["build"])
                if "test_build" in target:
                    build_labels.add(target["test_build"])
                if "test" in target:
                    test_labels.add(target["test"])
    if options.command == "lint":
        return execute("build", ["//build/rust:native_checks"])
    if options.command == "check":
        return execute("build", sorted(build_labels))
    if options.command == "verify":
        test_labels.update("//bridge/haskell:corpus_" + cohort.replace("-", "_") + "_test" for cohort in COHORTS)
        test_labels.update((
            "//tidepool/runtime:compile_fail_machine_lease_double_borrow",
            "//tidepool/runtime:compile_fail_prepared_install_consumes_pending",
            "//tidepool/runtime:compile_fail_prepared_install_crossed_image",
            "//tidepool/runtime:compile_fail_root_borrow_release",
            "//tidepool/runtime:compile_fail_root_borrow_collection",
        ))
        # Suite ownership is declared by generated native rules; query the graph
        # rather than maintaining or parsing a second component inventory.
        discovery = subprocess.run(
            ["bash", str(ROOT / "scripts/buck2-run.sh"), "uquery", "-c", "remote.enabled=false",
             "-c", f"tidepool.profile={options.profile}",
             'attrfilter(labels, haskell_component_suite, //bridge/haskell:)'],
            cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE)
        suites = discovery.stdout.splitlines()
        if not suites or any(not label.startswith("root//bridge/haskell:") for label in suites):
            raise ValueError("native Haskell suite discovery returned no valid owning targets")
        test_labels.update(suites)
        check_status = execute("build", ["//build/rust:native_checks", "//bridge/haskell:probe_opacity"])
        test_status = execute("test", sorted(test_labels))
        return check_status or test_status
    raise ValueError(f"unsupported command {options.command}")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, json.JSONDecodeError) as error:
        print(f"native workflow: {error}", file=sys.stderr)
        sys.exit(2)
