#!/usr/bin/env python3
"""Read local integration history and declared native/browser Harness identities."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib
from urllib.parse import parse_qs, urlsplit


OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")


class ObservationError(ValueError):
    pass


def git(repo, *args, allow_failure=False):
    location_overrides = {"GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"}
    environment = {key: value for key, value in os.environ.items() if key not in location_overrides}
    result = subprocess.run(
        ["git", "--no-optional-locks", "-C", str(repo), *args],
        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment,
    )
    if result.returncode and not allow_failure:
        # Git diagnostics can contain configured remote URLs; never relay them.
        raise ObservationError("Git observation failed: " + args[0])
    return result


def resolve(repo, ref):
    result = git(repo, "rev-parse", "--verify", "--end-of-options", ref + "^{commit}", allow_failure=True)
    return result.stdout.strip() if result.returncode == 0 else None


def comparison(repo, left, right):
    ahead, behind = map(int, git(repo, "rev-list", "--left-right", "--count", left + "..." + right).stdout.split())
    relation = "diverged" if ahead and behind else "ahead" if ahead else "behind" if behind else "equal"
    return {"left_unique_commits": ahead, "right_unique_commits": behind, "relation": relation}


def contains(repo, descendant, ancestor):
    result = git(repo, "merge-base", "--is-ancestor", ancestor, descendant, allow_failure=True)
    if result.returncode not in (0, 1):
        raise ObservationError("Git ancestry observation failed")
    return result.returncode == 0


def repository_identity(url):
    parsed = urlsplit(url.removeprefix("git+"))
    if parsed.hostname != "github.com":
        raise ObservationError("Harness source must identify a GitHub repository")
    path = parsed.path.strip("/").removesuffix(".git")
    if len(path.split("/")) != 2:
        raise ObservationError("Harness source repository is malformed")
    return "github.com/" + path.lower()


def pins(repo):
    with (repo / "Cargo.toml").open("rb") as handle:
        cargo = tomllib.load(handle)["workspace"]["dependencies"]["harness"]
    with (repo / "Cargo.lock").open("rb") as handle:
        packages = [p for p in tomllib.load(handle)["package"] if p["name"] == cargo.get("package", "harness")]
    if len(packages) != 1:
        raise ObservationError("Cargo.lock must contain one selected Harness package")
    locked = urlsplit(packages[0]["source"].removeprefix("git+"))
    revisions = parse_qs(locked.query).get("rev", [])
    if len(revisions) != 1:
        raise ObservationError("Cargo.lock Harness source requires one exact rev")
    evaluation = subprocess.run(
        [os.environ.get("EXOMONAD_NIX_BIN", "nix"), "eval", "--offline",
         "--option", "allow-import-from-derivation", "false", "--json",
         "--file", str(repo / "flake.nix"), "inputs.harnessWeb.url"],
        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if evaluation.returncode:
        raise ObservationError("Nix manifest input evaluation is unavailable")
    declared = urlsplit(json.loads(evaluation.stdout))
    parts = declared.path.split("/")
    if declared.scheme != "github" or len(parts) != 3 or declared.query or declared.fragment:
        raise ObservationError("Evaluated harnessWeb URL requires an exact GitHub revision")
    owner, name, revision = parts
    lock = json.loads((repo / "flake.lock").read_text())
    node = lock["nodes"][lock["nodes"][lock["root"]]["inputs"]["harnessWeb"]]["locked"]
    if node["type"] != "github":
        raise ObservationError("flake.lock harnessWeb must identify GitHub")
    identities = {
        "cargo_manifest": {"revision": cargo["rev"], "repository": repository_identity(cargo["git"])},
        "cargo_lock": {"revision": locked.fragment, "requested_revision": revisions[0], "repository": repository_identity(packages[0]["source"])},
        "flake_manifest": {"revision": revision, "repository": f"github.com/{owner}/{name}".lower()},
        "flake_lock": {"revision": node["rev"], "repository": f'github.com/{node["owner"]}/{node["repo"]}'.lower()},
    }
    for identity in identities.values():
        for key in ("revision", "requested_revision"):
            if key in identity and not OID.fullmatch(identity[key]):
                raise ObservationError("Harness pins must use exact full object IDs")
    agreed = len({i["revision"] for i in identities.values()}) == 1 and len({i["repository"] for i in identities.values()}) == 1
    agreed = agreed and revisions[0] == locked.fragment
    return {"agreement": agreed, "observation": "working_tree_files", "sources": identities, "selected_native_revision": cargo["rev"]}


def observe(repo, candidates=(), harness_repo=None, required_ancestor=None):
    repo = Path(git(Path(repo).resolve(), "rev-parse", "--show-toplevel").stdout.strip()).resolve()
    errors = []
    head = resolve(repo, "HEAD")
    if head is None:
        raise ObservationError("Checkout HEAD is unavailable")
    branch = git(repo, "symbolic-ref", "--quiet", "--short", "HEAD", allow_failure=True)
    changes = git(repo, "status", "--porcelain=v1", "--untracked-files=no").stdout.splitlines()
    main = resolve(repo, "refs/heads/main")
    upstream = git(repo, "for-each-ref", "--format=%(upstream)", "refs/heads/main").stdout.strip()
    remote_ref = upstream or "refs/remotes/origin/main"
    remote = resolve(repo, remote_ref)
    report = {
        "schema": 1, "checkout": {"path": str(repo), "head": head, "branch": branch.stdout.strip() or None, "tracked_change_count": len(changes)},
        "main": {"ref": "refs/heads/main", "head": main},
        "remote_main": {"ref": remote_ref, "head": remote, "observation": "cached_local_tracking_ref", "selection": "main_upstream" if upstream else "default_origin_main", "fetched": False},
        "candidates": [], "errors": errors,
    }
    if main is None:
        errors.append("local_main_missing")
    else:
        report["checkout_vs_main"] = comparison(repo, head, main)
    if remote is None:
        errors.append("cached_remote_main_unknown")
    if main and remote:
        report["main_vs_cached_remote"] = comparison(repo, main, remote)
        if report["main_vs_cached_remote"]["relation"] == "diverged":
            errors.append("main_diverged_from_cached_remote")
    for ref in candidates:
        selected = resolve(repo, ref)
        candidate = {"ref": ref, "head": selected, "contained_in_main": None}
        if selected is None:
            errors.append("candidate_ref_invalid: " + ref)
        elif main:
            candidate["contained_in_main"] = contains(repo, main, selected)
            candidate["vs_main"] = comparison(repo, selected, main)
            unique, main_unique = map(int, git(repo, "rev-list", "--left-right", "--cherry-pick", "--count", selected + "..." + main).stdout.split())
            candidate["patch_unique_commits"] = unique
            candidate["main_patch_unique_commits"] = main_unique
        report["candidates"].append(candidate)
    try:
        report["harness_pins"] = pins(repo)
        if not report["harness_pins"]["agreement"]:
            errors.append("harness_pin_disagreement")
    except (OSError, ValueError, KeyError, TypeError) as error:
        # Do not render file contents or dependency URLs on malformed inputs.
        report["harness_pins"] = {"agreement": False, "observation": "unavailable", "error_type": type(error).__name__}
        errors.append("harness_pins_unavailable")
    if harness_repo is not None:
        harness_repo = Path(harness_repo).resolve()
        selected = report["harness_pins"].get("selected_native_revision")
        available = bool(selected and resolve(harness_repo, selected))
        check = {"path": str(harness_repo), "selected_revision": selected, "selected_object_available": available, "required_ancestor": required_ancestor}
        if not available:
            errors.append("selected_harness_object_missing")
        if required_ancestor:
            ancestor_available = resolve(harness_repo, required_ancestor) is not None
            check["required_object_available"] = ancestor_available
            check["selected_contains_required"] = contains(harness_repo, selected, required_ancestor) if available and ancestor_available else None
            if not ancestor_available:
                errors.append("required_harness_object_missing")
            elif available and not check["selected_contains_required"]:
                errors.append("selected_harness_missing_required_ancestor")
        report["harness_repository"] = check
    report["strict_ok"] = not errors
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--json", action="store_true", help="emit a structured report")
    parser.add_argument("--strict", action="store_true", help="fail missing observations, pin mismatch or divergent main history")
    parser.add_argument("--candidate", action="append", default=[], metavar="REF", help="compare a candidate with local main; repeatable")
    parser.add_argument("--harness-repo", type=Path)
    parser.add_argument("--required-harness-ancestor", metavar="EXACT_OID")
    args = parser.parse_args(argv)
    if args.required_harness_ancestor and (not args.harness_repo or not OID.fullmatch(args.required_harness_ancestor)):
        parser.error("--required-harness-ancestor requires --harness-repo and a full exact object ID")
    try:
        report = observe(args.repo, args.candidate, args.harness_repo, args.required_harness_ancestor)
    except (OSError, ObservationError) as error:
        report = {"schema": 1, "strict_ok": False, "errors": ["checkout_observation_unavailable"], "error_type": type(error).__name__}
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        checkout = report.get("checkout", {})
        print(f"Checkout: {checkout.get('branch') or '(detached)'} {checkout.get('head', 'unknown')}")
        print(f"Tracked working-tree changes: {checkout.get('tracked_change_count', 'unknown')} (not an integration failure)")
        print(f"Local main: {report.get('main', {}).get('head') or 'unknown'}")
        remote = report.get("remote_main", {})
        print(f"Cached tracking ref: {remote.get('ref', 'unknown')} {remote.get('head') or 'unknown'} (no fetch)")
        for name in ("checkout_vs_main", "main_vs_cached_remote"):
            if name in report:
                row = report[name]
                print(f"{name}: {row['relation']}; left-only {row['left_unique_commits']}, right-only {row['right_unique_commits']}")
        pin = report.get("harness_pins", {})
        print(f"Harness pins: {'agree' if pin.get('agreement') else 'unqualified'}; selected {pin.get('selected_native_revision', 'unknown')}")
        for row in report.get("candidates", []):
            print(f"Candidate {row['ref']}: {row.get('head') or 'invalid'}; contained in main {row['contained_in_main']}; patch-unique {row.get('patch_unique_commits', 'unknown')}")
        if "harness_repository" in report:
            row = report["harness_repository"]
            print(f"Harness selected object available: {row['selected_object_available']}; contains required ancestor: {row.get('selected_contains_required', 'not requested')}")
        print("Checks: " + ("pass" if report["strict_ok"] else "; ".join(report["errors"])))
    invalid_ref = any(e.startswith("candidate_ref_invalid:") for e in report["errors"])
    unavailable = "checkout_observation_unavailable" in report["errors"]
    return 1 if invalid_ref or unavailable or (args.strict and not report["strict_ok"]) else 0


if __name__ == "__main__":
    raise SystemExit(main())
