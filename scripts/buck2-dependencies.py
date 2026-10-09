#!/usr/bin/env python3
"""Derive Reindeer's dependency-only manifest from the migrated Cargo packages."""
import argparse
import json
from pathlib import Path
import subprocess
import tomllib

from buck2_cargo_features import (
    CRATES_IO_SOURCE,
    supports_native_facade_build_dependency,
    NATIVE_PACKAGES,
    reject_local_feature_requests,
    FeatureSelectionError,
    dependency_aliases,
    effective_feature_selection,
    metadata_feature_args,
    parse_locked_git_source,
    reject_forbidden_closure,
    resolve as resolve_cargo_features,
)

ROOT = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser()
parser.add_argument("--output-dir", type=Path, required=True)
parser.add_argument("--no-default-features", action="append", default=[], metavar="PACKAGE")
parser.add_argument("--features", action="append", default=[], metavar="PACKAGE=FEATURES")
options = parser.parse_args()
DEST = options.output_dir.resolve()
DEST.mkdir(parents=True, exist_ok=True)
# Every retained package owns its direct development closure. Transitive local
# libraries are also explicit roots, so test-data/corpus and actor/runtime test
# dependencies contribute their external feature requests without local unions.
ROOTS = NATIVE_PACKAGES
LINUX_TARGETS = {None, "cfg(unix)"}
metadata_command = [
    "cargo", "metadata", "--locked", "--filter-platform", "x86_64-unknown-linux-gnu",
    "--format-version", "1",
]
no_default = set(options.no_default_features)
feature_overrides = {}
for value in options.features:
    package_name, separator, raw_features = value.partition("=")
    if not separator or not package_name or not raw_features:
        raise SystemExit(f"invalid --features value {value!r}; expected PACKAGE=FEATURE[,FEATURE...]")
    features = [feature for feature in raw_features.split(",") if feature]
    if len(features) != len(set(features)):
        raise SystemExit(f"duplicate Cargo feature in --features {value!r}")
    feature_overrides.setdefault(package_name, set()).update(features)
baseline = json.loads(subprocess.check_output(metadata_command, cwd=ROOT))
workspace_packages = {
    package["name"]
    for package in baseline["packages"]
    if package["id"] in baseline["workspace_members"]
}
try:
    no_default, feature_overrides = effective_feature_selection(
        ROOT, workspace_packages, no_default, feature_overrides
    )
except FeatureSelectionError as error:
    raise SystemExit(str(error)) from error
metadata_command.append("--no-default-features")
metadata_command.extend(metadata_feature_args(baseline, no_default, feature_overrides))
metadata = json.loads(subprocess.check_output(metadata_command, cwd=ROOT))
packages = {p["id"]: p for p in metadata["packages"]}
nodes = {p["id"]: p for p in metadata["resolve"]["nodes"]}
selected = [p for p in packages.values() if p["name"] in ROOTS and p["source"] is None]
if len(selected) != len(ROOTS) or {p["name"] for p in selected} != ROOTS:
    raise SystemExit("Missing migrated Cargo package")
try:
    reject_forbidden_closure(metadata, ROOTS)
    reject_local_feature_requests(metadata, ROOTS, no_default, feature_overrides)
except FeatureSelectionError as error:
    raise SystemExit(str(error)) from error


dependencies = {}
for package in selected:
    resolved_deps = nodes[package["id"]]["deps"]
    try:
        _, active_dependencies, dependency_features = resolve_cargo_features(
            package,
            feature_overrides.get(package["name"], set()),
            package["name"] not in no_default,
            resolved_node=None,
        )
    except FeatureSelectionError as error:
        raise SystemExit(str(error)) from error
    for dependency in package["dependencies"]:
        kind = dependency["kind"]
        # codegen's build.rs is replaced by the checked-in native Buck C action.
        if package["name"] == "tidepool-codegen" and kind == "build":
            if dependency["name"] != "cc" or dependency["rename"] is not None or dependency["target"] is not None:
                raise SystemExit(f"Unmodeled codegen build dependency: {dependency['name']}")
            continue
        if kind == "build":
            if not supports_native_facade_build_dependency(package["name"], dependency):
                raise SystemExit(f"Model build dependency for {package['name']}: {dependency['name']}")
        dependency_key = dependency["rename"] or dependency["name"]
        if dependency["optional"] and dependency_key not in active_dependencies:
            continue
        target_platform = dependency["target"]
        if target_platform not in LINUX_TARGETS:
            raise SystemExit(
                f"Unsupported Linux dependency selector in {package['name']}: {target_platform}"
            )
        dep_name = (dependency["rename"] or dependency["name"]).replace("-", "_")
        matching_edges = [
            edge for edge in resolved_deps
            if edge["name"] == dep_name
            and any(
                dep_kind["kind"] == kind and dep_kind["target"] == target_platform
                for dep_kind in edge["dep_kinds"]
            )
        ]
        if len(matching_edges) != 1:
            raise SystemExit(
                f"Missing or ambiguous Linux-resolved dependency {dep_name} in {package['name']}"
            )
        edge = matching_edges[0]
        target = packages[edge["pkg"]]
        if package["name"] == "tidepool-runtime" and kind == "dev" and target["name"] == "trybuild":
            # Native compile-fail actions invoke declared rustc inputs directly.
            continue
        if target["source"] is None:
            if target["name"] not in ROOTS:
                raise SystemExit(f"Unmigrated local dependency: {package['name']} --{kind or 'normal'}:{dependency_key}--> {target['name']}")
            continue
        source = target["source"]
        git = None
        if source != CRATES_IO_SOURCE:
            try:
                git = parse_locked_git_source(source)
            except ValueError as error:
                raise SystemExit(str(error)) from error
        key = (target["name"], target["version"], source)
        spec = dependencies.setdefault(key, {"features": set(), "default": False, "git": git})
        if spec["git"] != git:
            raise SystemExit(f"inconsistent Cargo source for {target['name']} {target['version']}")
        spec["features"].update(dependency["features"])
        spec["features"].update(dependency_features.get(dependency_key, set()))
        spec["default"] |= dependency["uses_default_features"]
lines = [
    '# @generated by scripts/buck2-dependencies.py; Cargo manifests and root lock own dependencies.',
    '[package]', 'name = "tidepool-buck-dependencies"', 'version = "0.0.0"',
    'edition = "2021"', 'publish = false', '[workspace]', '[lib]', 'path = "empty.rs"',
    '[dependencies]',
]
aliases = dependency_aliases(dependencies)
for identity, spec in sorted(dependencies.items()):
    name, version, source = identity
    alias = aliases[identity]
    fields = [f'package = {json.dumps(name)}', f'version = {json.dumps("=" + version)}',
              'default-features = ' + str(spec["default"]).lower(),
              'features = ' + json.dumps(sorted(spec["features"]))]
    if spec["git"] is not None:
        repository, revision = spec["git"]
        fields.extend([f'git = {json.dumps(repository)}', f'rev = {json.dumps(revision)}'])
    lines.append(json.dumps(alias) + ' = { ' + ', '.join(fields) + ' }')
(DEST / 'Cargo.toml').write_text('\n'.join(lines) + '\n')
(DEST / 'Cargo.lock').write_bytes((ROOT / 'Cargo.lock').read_bytes())
subprocess.run(['cargo', 'metadata', '--offline', '--manifest-path', str(DEST / 'Cargo.toml'),
                '--format-version', '1', '--filter-platform', 'x86_64-unknown-linux-gnu'],
               cwd=ROOT, stdout=subprocess.DEVNULL, check=True)
root_lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
generated_lock = tomllib.loads((DEST / 'Cargo.lock').read_text())
def identity(p):
    return p['name'], p['version'], p.get('source'), p.get('checksum')
accepted = {identity(p) for p in root_lock['package']}
for package in generated_lock['package']:
    if package['name'] == 'tidepool-buck-dependencies' and package.get('source') is None:
        continue
    if identity(package) not in accepted:
        raise SystemExit(f"Generated dependency escaped root Cargo.lock: {package['name']} {package['version']}")
