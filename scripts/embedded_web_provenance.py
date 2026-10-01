#!/usr/bin/env python3
"""Check that packaged browser assets follow Tidepool's selected harness crate."""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
import unittest
import tempfile
import subprocess
from pathlib import Path
from urllib.parse import parse_qs, urlsplit


HARNESS_REPOSITORY = "https://github.com/tidepool-heavy-industries/exomonad-harness.git"
HARNESS_WEB_INPUT = "harnessWeb"


class ProvenanceError(ValueError):
    pass


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ProvenanceError(message)


def _git_revision(source: str, *, origin: str) -> tuple[str, str]:
    _require(source.startswith("git+"), f"{origin} is not a Git source: {source!r}")
    parsed = urlsplit(source.removeprefix("git+"))
    query = parse_qs(parsed.query)
    requested = query.get("rev", [None])
    resolved = parsed.fragment
    _require(len(requested) == 1 and requested[0], f"{origin} has no unique rev pin")
    _require(bool(resolved), f"{origin} has no resolved Git commit")
    return requested[0], resolved


def validate_generated_source(cargo_lock: dict, flake_lock: dict, revision: str, nar_hash: str) -> str:
    """Bind a generated source owner's immutable selection to the current locks."""
    _require(re.fullmatch(r"[0-9a-f]{40}", revision) is not None, "generated harness revision is malformed")
    _require(
        isinstance(nar_hash, str) and re.fullmatch(r"sha256-[A-Za-z0-9+/]{43}=", nar_hash) is not None,
        "generated harness source NAR hash is malformed",
    )
    packages = [package for package in cargo_lock.get("package", []) if package.get("name") == "harness"]
    _require(len(packages) == 1, "Cargo.lock must select exactly one harness package")
    source = packages[0].get("source", "")
    _require(
        source == f"git+{HARNESS_REPOSITORY}?rev={revision}#{revision}",
        "generated harness source revision differs from Cargo.lock",
    )
    nodes = flake_lock.get("nodes", {})
    _require(
        nodes.get("root", {}).get("inputs", {}).get(HARNESS_WEB_INPUT) == HARNESS_WEB_INPUT,
        "flake.lock harnessWeb is not connected to the root input graph",
    )
    web_input = nodes.get(HARNESS_WEB_INPUT, {})
    _require(web_input.get("flake") is False, "flake.lock harnessWeb must be a non-flake source")
    for kind in ("original", "locked"):
        pin = web_input.get(kind, {})
        _require(
            (pin.get("type"), pin.get("owner"), pin.get("repo"), pin.get("rev"))
            == ("github", "tidepool-heavy-industries", "exomonad-harness", revision),
            f"generated harness source revision differs from flake.lock {kind} pin",
        )
    locked_hash = web_input.get("locked", {}).get("narHash")
    _require(
        isinstance(locked_hash, str) and re.fullmatch(r"sha256-[A-Za-z0-9+/]{43}=", locked_hash) is not None,
        "flake.lock harnessWeb NAR hash is missing or malformed",
    )
    _require(locked_hash == nar_hash, "generated harness source NAR hash differs from flake.lock")
    return revision


def validate(
    cargo_manifest: dict,
    cargo_lock: dict,
    facade_manifest: dict,
    flake_lock: dict,
    flake_source: str,
) -> str:
    """Return the selected harness commit, or reject any disconnected pin."""
    workspace_dependency = cargo_manifest.get("workspace", {}).get("dependencies", {}).get("harness")
    _require(isinstance(workspace_dependency, dict), "workspace harness dependency is missing")
    _require(
        workspace_dependency.get("git") == HARNESS_REPOSITORY,
        "workspace harness dependency does not use the expected repository",
    )
    requested = workspace_dependency.get("rev")
    _require(isinstance(requested, str) and requested, "workspace harness dependency has no rev")
    _require(
        workspace_dependency.get("package", "harness") == "harness",
        "workspace harness dependency selects a different package",
    )

    facade_package = facade_manifest.get("package", {})
    _require(facade_package.get("name") == "tidepool", "facade manifest is not the tidepool package")
    facade_version = facade_package.get("version")
    if isinstance(facade_version, dict) and facade_version.get("workspace") is True:
        facade_version = cargo_manifest.get("workspace", {}).get("package", {}).get("version")
    _require(isinstance(facade_version, str), "facade package version is not declared")
    facade_harness = facade_manifest.get("dependencies", {}).get("harness")
    _require(
        isinstance(facade_harness, dict) and facade_harness.get("workspace") is True,
        "production facade does not consume the workspace harness dependency",
    )

    harness_lock_records = [
        package
        for package in cargo_lock.get("package", [])
        if package.get("name") == "harness"
        and package.get("source", "").startswith("git+")
    ]
    _require(len(harness_lock_records) == 1, "Cargo.lock must select exactly one Git harness package")
    selected = harness_lock_records[0]
    lock_requested, lock_resolved = _git_revision(selected["source"], origin="Cargo.lock harness source")
    _require(lock_requested == requested, "Cargo.lock harness rev differs from Cargo.toml")
    _require(lock_resolved == requested, "Cargo.lock did not resolve the exact requested harness commit")

    facade_records = [
        package
        for package in cargo_lock.get("package", [])
        if package.get("name") == "tidepool"
        and package.get("version") == facade_version
    ]
    _require(len(facade_records) == 1, "Cargo.lock must contain exactly one production tidepool package")
    selected_harness_dependencies = [
        dependency
        for dependency in facade_records[0].get("dependencies", [])
        if dependency.split(" ", 1)[0] == "harness"
    ]
    _require(
        len(selected_harness_dependencies) == 1,
        "Cargo.lock production tidepool package must depend on the selected harness package",
    )

    nodes = flake_lock.get("nodes", {})
    root_inputs = nodes.get("root", {}).get("inputs", {})
    _require(
        root_inputs.get(HARNESS_WEB_INPUT) == HARNESS_WEB_INPUT,
        "flake.lock harnessWeb is not connected to the root input graph",
    )
    web_input = nodes.get(HARNESS_WEB_INPUT, {})
    locked = web_input.get("locked", {})
    original = web_input.get("original", {})
    _require(locked.get("rev") == lock_resolved, "flake.lock browser input differs from Cargo's selected commit")
    _require(original.get("rev") == requested, "flake.lock browser input request differs from Cargo.toml")

    declared_input = re.search(
        r'harnessWeb\s*=\s*\{\s*url\s*=\s*"github:tidepool-heavy-industries/exomonad-harness/([0-9a-f]{40})"\s*;\s*flake\s*=\s*false\s*;\s*\}',
        flake_source,
        re.DOTALL,
    )
    _require(declared_input is not None, "flake.nix must declare the pinned harnessWeb source input")
    _require(declared_input.group(1) == requested, "flake.nix browser input differs from Cargo.toml")
    assets = re.search(r"embeddedWebAssets\s*=\s*harnessPkgs\.buildNpmPackage\s*\{(.*?)\n\s*\};", flake_source, re.DOTALL)
    _require(assets is not None, "flake.nix has no embedded browser asset derivation")
    _require(
        re.search(r'src\s*=\s*"\$\{harnessWeb\}/web"\s*;', assets.group(1)) is not None,
        "embedded browser assets are not built from the locked harnessWeb input",
    )
    return lock_resolved


def load_and_validate(root: Path) -> str:
    return validate(
        tomllib.loads((root / "Cargo.toml").read_text()),
        tomllib.loads((root / "Cargo.lock").read_text()),
        tomllib.loads((root / "bridge/facade/Cargo.toml").read_text()),
        json.loads((root / "flake.lock").read_text()),
        (root / "flake.nix").read_text(),
    )


def fixture(revision: str = "a" * 40) -> tuple[dict, dict, dict, dict, str]:
    source = f"git+{HARNESS_REPOSITORY}?rev={revision}#{revision}"
    return (
        {"workspace": {"dependencies": {"harness": {"git": HARNESS_REPOSITORY, "rev": revision, "package": "harness"}}}},
        {"package": [
            {"name": "harness", "version": "0.1.0", "source": source, "dependencies": []},
            {"name": "tidepool", "version": "0.1.0", "dependencies": ["harness"]},
        ]},
        {"package": {"name": "tidepool", "version": "0.1.0"}, "dependencies": {"harness": {"workspace": True}}},
        {"nodes": {
            "root": {"inputs": {"harnessWeb": "harnessWeb"}},
            "harnessWeb": {"original": {"rev": revision}, "locked": {"rev": revision}},
        }},
        f'''inputs = {{ harnessWeb = {{ url = "github:tidepool-heavy-industries/exomonad-harness/{revision}"; flake = false; }}; }};
           embeddedWebAssets = harnessPkgs.buildNpmPackage {{
             src = "${{harnessWeb}}/web";
           }};''',
    )


class ProvenanceTests(unittest.TestCase):
    def test_connected_revision_passes(self) -> None:
        self.assertEqual(validate(*fixture()), "a" * 40)

    def test_unrelated_matching_revision_cannot_hide_cargo_lock_drift(self) -> None:
        args = list(fixture())
        args[1]["package"][0]["source"] = f"git+{HARNESS_REPOSITORY}?rev={'b' * 40}#{'b' * 40}"
        with self.assertRaisesRegex(ProvenanceError, "differs from Cargo.toml"):
            validate(*args)

    def test_orphan_flake_lock_node_is_rejected(self) -> None:
        args = list(fixture())
        args[3]["nodes"]["root"]["inputs"].pop("harnessWeb")
        with self.assertRaisesRegex(ProvenanceError, "not connected"):
            validate(*args)

    def test_dead_harness_input_does_not_prove_asset_source(self) -> None:
        args = list(fixture())
        args[4] = args[4].replace('src = "${harnessWeb}/web"', 'src = "${otherSource}/web"')
        with self.assertRaisesRegex(ProvenanceError, "not built from"):
            validate(*args)


class BuckSourceProvenanceTests(unittest.TestCase):
    nar_hash = "sha256-" + "A" * 43 + "="

    def source_locks(self, revision="a" * 40):
        cargo_lock = fixture(revision)[1]
        pin = {"type": "github", "owner": "tidepool-heavy-industries", "repo": "exomonad-harness", "rev": revision}
        flake_lock = {"nodes": {
            "root": {"inputs": {"harnessWeb": "harnessWeb"}},
            "harnessWeb": {"flake": False, "original": pin.copy(), "locked": pin | {"narHash": self.nar_hash}},
        }}
        return cargo_lock, flake_lock

    def test_current_generated_revision_passes(self):
        self.assertEqual(validate_generated_source(*self.source_locks(), "a" * 40, self.nar_hash), "a" * 40)

    def test_stale_generated_revision_is_rejected(self):
        with self.assertRaisesRegex(ProvenanceError, "differs from Cargo.lock"):
            validate_generated_source(*self.source_locks(), "b" * 40, self.nar_hash)

    def test_foreign_source_owner_is_rejected(self):
        cargo, flake = self.source_locks()
        flake["nodes"]["harnessWeb"]["locked"]["owner"] = "foreign-owner"
        with self.assertRaisesRegex(ProvenanceError, "differs from flake.lock locked"):
            validate_generated_source(cargo, flake, "a" * 40, self.nar_hash)

    def test_orphan_source_pin_is_rejected(self):
        cargo, flake = self.source_locks()
        flake["nodes"]["root"]["inputs"].clear()
        with self.assertRaisesRegex(ProvenanceError, "not connected"):
            validate_generated_source(cargo, flake, "a" * 40, self.nar_hash)

    def test_malformed_generated_revision_is_rejected(self):
        with self.assertRaisesRegex(ProvenanceError, "malformed"):
            validate_generated_source(*self.source_locks(), "newest", self.nar_hash)

    def test_same_revision_with_changed_nar_hash_is_rejected(self):
        cargo, flake = self.source_locks()
        flake["nodes"]["harnessWeb"]["locked"]["narHash"] = "sha256-" + "B" * 43 + "="
        with self.assertRaisesRegex(ProvenanceError, "NAR hash differs from flake.lock"):
            validate_generated_source(cargo, flake, "a" * 40, self.nar_hash)

    def test_missing_null_or_invalid_locked_nar_hash_is_rejected(self):
        for value in (None, "", "sha256-short", "sha512-" + "A" * 43 + "=", 123):
            with self.subTest(nar_hash=value):
                cargo, flake = self.source_locks()
                flake["nodes"]["harnessWeb"]["locked"]["narHash"] = value
                with self.assertRaisesRegex(ProvenanceError, "missing or malformed"):
                    validate_generated_source(cargo, flake, "a" * 40, self.nar_hash)
        cargo, flake = self.source_locks()
        del flake["nodes"]["harnessWeb"]["locked"]["narHash"]
        with self.assertRaisesRegex(ProvenanceError, "missing or malformed"):
            validate_generated_source(cargo, flake, "a" * 40, self.nar_hash)

    def test_package_checks_actual_source_owner_revision(self):
        with tempfile.TemporaryDirectory(prefix="buck-package-source-test-") as temporary:
            root = Path(temporary)
            revision = "a" * 40
            (root / "bridge/facade").mkdir(parents=True)
            (root / "Cargo.toml").write_text(f'[workspace.dependencies.harness]\ngit = "{HARNESS_REPOSITORY}"\nrev = "{revision}"\n')
            (root / "Cargo.lock").write_text(f'[[package]]\nname = "harness"\nversion = "0.1.0"\nsource = "git+{HARNESS_REPOSITORY}?rev={revision}#{revision}"\n[[package]]\nname = "tidepool"\nversion = "0.1.0"\ndependencies = ["harness"]\n')
            (root / "bridge/facade/Cargo.toml").write_text('[package]\nname = "tidepool"\nversion = "0.1.0"\n[dependencies.harness]\nworkspace = true\n')
            (root / "flake.lock").write_text(json.dumps(fixture(revision)[3]))
            (root / "flake.nix").write_text(fixture(revision)[4])
            actual = root / "actual-revision.txt"
            output = root / "package-revision.txt"
            command = [sys.executable, __file__, "--check", "--root", str(root),
                "--actual-source-revision", str(actual), "--output", str(output)]
            actual.write_text("b" * 40 + "\n")
            stale = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("actual Buck harness source revision differs", stale.stderr)
            self.assertFalse(output.exists())
            actual.write_text(revision + "\n")
            current = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(current.returncode, 0, current.stderr)
            self.assertEqual(output.read_text(), revision + "\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--output", type=Path)
    source_mode = parser.add_mutually_exclusive_group()
    source_mode.add_argument("--generated-source-revision")
    source_mode.add_argument("--actual-source-revision", type=Path)
    parser.add_argument("--generated-source-nar-hash")
    parser.add_argument("--cargo-lock", type=Path)
    parser.add_argument("--flake-lock", type=Path)
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.TestSuite([unittest.defaultTestLoader.loadTestsFromTestCase(tests) for tests in (ProvenanceTests, BuckSourceProvenanceTests)])
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        if not result.wasSuccessful():
            return 1
        if not args.check:
            return 0
    try:
        if args.generated_source_revision is not None:
            if args.cargo_lock is None or args.flake_lock is None or args.output is None or args.generated_source_nar_hash is None:
                parser.error("--generated-source-revision requires --generated-source-nar-hash, --cargo-lock, --flake-lock and --output")
            revision = validate_generated_source(
                tomllib.loads(args.cargo_lock.read_text()),
                json.loads(args.flake_lock.read_text()),
                args.generated_source_revision,
                args.generated_source_nar_hash,
            )
        else:
            revision = load_and_validate(args.root)
            if args.actual_source_revision is not None:
                _require(
                    args.actual_source_revision.read_text().strip() == revision,
                    "actual Buck harness source revision differs from the current pins; regenerate Buck dependencies",
                )
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(f"embedded web provenance rejected: {error}", file=sys.stderr)
        return 1
    if args.output is not None:
        args.output.write_text(revision + "\n")
    print(f"embedded web and Cargo harness source agree at {revision}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
