#!/usr/bin/env python3
"""Regenerate native Buck Rust targets from the locked Cargo workspace metadata."""

import argparse
import json
import os
import pathlib
import re
import stat
import subprocess
import sys
import tempfile
import tomllib

from buck2_cargo_features import (
    CRATES_IO_SOURCE,
    supports_native_facade_build_dependency,
    NATIVE_PACKAGES,
    reject_local_feature_requests,
    FeatureSelectionError,
    effective_feature_selection,
    metadata_feature_args,
    parse_locked_git_source,
    reject_forbidden_closure,
    resolve as resolve_cargo_features,
)
from test_source_ownership import TEST_ONLY_FIXTURES, TEST_ONLY_MODULES, TEST_ONLY_SOURCES, integration_target_sources, registration_errors


def cargo_dependency_key(dependency):
    return dependency["rename"] or dependency["name"]

ROOT = pathlib.Path(__file__).resolve().parent.parent
HASKELL_RUST_INPUTS = set()
HASKELL_TEST_FIXTURES = {}
arguments = argparse.ArgumentParser(description=__doc__)
arguments.add_argument("--package", action="append", required=True, help="Cargo package to generate (repeatable)")
arguments.add_argument(
    "--no-default-features", action="append", default=[], metavar="PACKAGE",
    help="generate this package with Cargo defaults disabled (repeatable)",
)
arguments.add_argument(
    "--features", action="append", default=[], metavar="PACKAGE=FEATURES",
    help="add comma-separated Cargo features to this package (repeatable)",
)
arguments.add_argument("--check", action="store_true", help="check generated BUCK files without rewriting them")
options = arguments.parse_args()
selected = set(options.package)
if selected & {"exomonad-host", "tidepool-runtime", "exomonad-actor"}:
    selected.add("tidepool-testing")
SUPPORTED_PACKAGES = NATIVE_PACKAGES
# Every executable suite uses the existing counted, process-isolated runner.
# Shared harness owners publish their named execution groups separately.
GENERATED_BIN_NAMES = {
    "tidepool-protocol-gen": "tidepool_protocol_gen",
    "tidepool-effects-gen": "tidepool_effects_gen",
}
TOOLCHAIN_COMPILER_FREE_TESTS = [
    "source_bundle::tests::materialization_repairs_a_stale_or_partial_completion_marker",
    "source_bundle::tests::completion_marker_does_not_hide_changed_or_missing_library_sources",
    "source_bundle::tests::selected_source_verification_rejects_changed_missing_and_extra_modules",
    "certified_products::tests::original_native_index_preserves_sparse_order_and_selected_identity",
    "certified_products::tests::original_native_index_issuer_rejects_duplicate_ordinals_and_wrong_owners",
    "certified_products::tests::original_native_index_matches_linear_scan_for_generated_sparse_groups",
    "certified_products::tests::original_native_witness_reuses_nonempty_cycle_and_checks_selected_closure",
    "certified_products::tests::original_native_witness_retains_package_custody_and_refuses_zero_group_downgrade",
    "certified_products::tests::repeated_retained_core_promotion_preserves_exact_original_membership",
    "certified_products::tests::nonempty_exact_source_recipe_preserves_original_versions_and_selected_subsets",
    "artifact_inventory::tests::native_requirement_index_refuses_interface_edges",
    "artifact_inventory::tests::native_requirement_index_preserves_empty_sparse_and_overlapping_edges",
    "artifact_inventory::tests::native_requirement_planning_visits_only_selected_group_edges",
    "artifact_inventory::native_history_properties::native_requirement_index_matches_flat_edge_oracle",
    "artifact_inventory::native_history_properties::staged_native_histories_match_raw_fact_model",
    "artifact_inventory::native_history_properties::recovery_selection_cannot_borrow_unrecorded_global_dependency",
    "artifact_inventory::tests::native_requirement_cannot_select_an_ambient_exact_native_key",
    "artifact_inventory::tests::recovery_restores_interface_edges_and_keeps_certified_native_facts",
    "certified_products::tests::artifact_view_group_index_properties::artifact_view_group_indexes_preserve_physical_identity_and_exact_refusals",
    "certified_products::tests::artifact_view_group_index_properties::artifact_view_group_indexes_match_physical_oracle_for_generated_selections",
    "artifact_inventory::view_read_properties::cached_read_projection_matches_exhaustive_graph_after_view_histories",
    "artifact_inventory::view_read_properties::warmed_read_getters_do_not_visit_graph_nodes",
    "artifact_inventory::view_read_properties::first_read_and_inventory_extension_settle_without_lock_recursion",
    "artifact_inventory::tests::retained_views_reclaim_after_last_reader_without_copying_payloads",
    "artifact_inventory::tests::payload_ownership_follows_last_reader_without_drop_traversal",
    "artifact_inventory::tests::empty_admission_and_parent_only_release_do_not_scan_history",
    "artifact_inventory::tests::reused_graph_targets_must_match_the_selected_sealed_dependencies",
    "artifact_inventory::tests::independent_views_retain_existing_nodes_without_sibling_visibility",
    "artifact_inventory::tests::selected_root_keeps_hidden_requirements_and_reclaims_unselected_history",
    "artifact_inventory::tests::reclamation_preserves_incoming_cycles_and_does_not_visit_unrelated_history",
    "artifact_inventory::view_read_properties::zero_mask_empty_selection_boundary_releases_valid_fixture_views",
    "artifact_inventory::tests::metadata_snapshot_preserves_canonical_typed_edges_in_one_closure",
    "artifact_inventory::view_read_properties::persistent_payload_history_releases_ancestors_and_shares_immutable_entries",
    "artifact_inventory::view_read_properties::persistent_captures_share_payload_and_scope_owners",
]
unsupported = selected - SUPPORTED_PACKAGES
if unsupported:
    raise SystemExit(
        "native Buck generation is currently supported only for "
        f"{', '.join(sorted(SUPPORTED_PACKAGES))}; unsupported: "
        f"{', '.join(sorted(unsupported))}"
    )


def parse_feature_options(workspace_packages):
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
    try:
        return effective_feature_selection(
            ROOT, workspace_packages, no_default, feature_overrides
        )
    except FeatureSelectionError as error:
        raise SystemExit(str(error)) from error

metadata_command = [
    "cargo", "metadata", "--locked", "--format-version", "1",
    "--filter-platform", "x86_64-unknown-linux-gnu",
]
baseline = json.loads(subprocess.check_output(metadata_command, cwd=ROOT))
registration_failures = registration_errors(baseline, ROOT)
if registration_failures:
    raise SystemExit("native source ownership refused:\n" + "\n".join(registration_failures))
baseline_local = {
    package["name"]: package for package in baseline["packages"]
    if package["id"] in baseline["workspace_members"]
}
workspace_packages = set(baseline_local)
unknown = selected - set(baseline_local)
if unknown:
    raise SystemExit(f"unknown Cargo workspace package(s): {', '.join(sorted(unknown))}")
NO_DEFAULT_FEATURES, FEATURE_OVERRIDES = parse_feature_options(workspace_packages)
metadata_command.extend(["--no-default-features"])
metadata_command.extend(metadata_feature_args(baseline, NO_DEFAULT_FEATURES, FEATURE_OVERRIDES))
metadata = json.loads(subprocess.check_output(metadata_command, cwd=ROOT))
packages = {package["name"]: package for package in metadata["packages"]}
local = {
    name: package
    for name, package in packages.items()
    if package["id"] in metadata["workspace_members"]
}
resolved_nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
try:
    reject_forbidden_closure(
        metadata, selected | NO_DEFAULT_FEATURES | set(FEATURE_OVERRIDES)
    )
    reject_local_feature_requests(metadata, selected | set(FEATURE_OVERRIDES), NO_DEFAULT_FEATURES, FEATURE_OVERRIDES)
except FeatureSelectionError as error:
    raise SystemExit(str(error)) from error


def feature_plan(package):
    try:
        features, dependencies, forwarded = resolve_cargo_features(
            package,
            FEATURE_OVERRIDES.get(package["name"], set()),
            package["name"] not in NO_DEFAULT_FEATURES,
            resolved_node=None,
        )
    except FeatureSelectionError as error:
        raise SystemExit(str(error)) from error
    return features, dependencies, forwarded


def package_dir(package):
    return pathlib.Path(package["manifest_path"]).parent.relative_to(ROOT).as_posix()


def target_name(package, kind):
    return next(
        target["name"]
        for target in package["targets"]
        if kind in target["kind"] or (kind == "lib" and "proc-macro" in target["kind"])
    )


def dependency_label(dependency, resolved_package):
    name = dependency["name"]
    if resolved_package["source"] is None:
        if name not in local or local[name]["id"] != resolved_package["id"]:
            raise SystemExit(f"resolved local dependency mismatch for {name}")
        target = target_name(local[name], "lib")
        directory = package_dir(local[name])
        return (":" if directory == CURRENT_DIR else "//" + directory + ":") + target
    source = resolved_package["source"]
    if source != CRATES_IO_SOURCE:
        try:
            parse_locked_git_source(source)
        except ValueError as error:
            raise SystemExit(str(error)) from error
    manifest_path = ROOT / "third-party/rust/Cargo.toml"
    if not manifest_path.is_file():
        raise SystemExit("missing generated third-party/rust/Cargo.toml; regenerate Reindeer inputs")
    manifest = tomllib.loads(manifest_path.read_text())
    identity = (resolved_package["name"], "=" + resolved_package["version"], source)
    aliases = []
    for alias, spec in manifest.get("dependencies", {}).items():
        package_name = spec.get("package", alias)
        dependency_source = CRATES_IO_SOURCE
        if "git" in spec:
            revision = spec.get("rev", "")
            try:
                repository, revision = parse_locked_git_source(
                    f"git+{spec['git']}?rev={revision}#{revision}"
                )
            except ValueError as error:
                raise SystemExit(str(error)) from error
            dependency_source = f"git+{repository}?rev={revision}#{revision}"
        if (package_name, spec.get("version"), dependency_source) == identity:
            aliases.append(alias)
    if len(aliases) == 1:
        return "//third-party/rust:" + aliases[0]
    if len(aliases) > 1:
        raise SystemExit(
            f"ambiguous Reindeer public dependency alias for "
            f"{resolved_package['name']} {resolved_package['version']}"
        )

    # Transitive dependencies have no direct manifest alias. Reindeer names
    # their package rule by the semver compatibility line; use that rule only
    # when the lock contains one exact source identity for this name/version.
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    locked = [
        package for package in lock["package"]
        if package["name"] == resolved_package["name"]
        and package["version"] == resolved_package["version"]
        and package.get("source") == source
    ]
    same_version = [
        package for package in lock["package"]
        if package["name"] == resolved_package["name"]
        and package["version"] == resolved_package["version"]
    ]
    if len(locked) != 1 or len(same_version) != 1:
        raise SystemExit(
            f"missing or source-ambiguous Reindeer target for "
            f"{resolved_package['name']} {resolved_package['version']}"
        )
    major, minor, *_ = resolved_package["version"].split(".")
    version_line = major if major != "0" else major + "." + minor
    target = resolved_package["name"] + "-" + version_line
    buck = (ROOT / "third-party/rust/BUCK").read_text()
    if not re.search(r'^\s*name = "' + re.escape(target) + r'",\s*$', buck, re.MULTILINE):
        raise SystemExit(
            f"missing Reindeer target {target} for {resolved_package['name']} "
            f"{resolved_package['version']}"
        )
    return "//third-party/rust:" + target


def linux_dependency(package, dependency):
    # Filtered Cargo resolution provides exact package identities, renames and
    # features. Cargo still retains some inactive dep_kinds on shared packages,
    # so admit only the platform selector present in this Linux native slice.
    target = dependency["target"]
    if target not in (None, "cfg(unix)"):
        raise SystemExit(f"unsupported Linux dependency selector in {package['name']}: {target}")
    cargo_name = (dependency["rename"] or dependency["name"]).replace("-", "_")
    kind = dependency["kind"]
    matches = [
        edge for edge in resolved_nodes[package["id"]]["deps"]
        if edge["name"] == cargo_name
        and any(item["kind"] == kind and item["target"] == target for item in edge["dep_kinds"])
    ]
    if len(matches) != 1:
        raise SystemExit(f"missing or ambiguous resolved dependency {cargo_name} in {package['name']}")
    resolved_package = next(p for p in metadata["packages"] if p["id"] == matches[0]["pkg"])
    if resolved_package["name"] != dependency["name"]:
        raise SystemExit(f"resolved dependency mismatch for {cargo_name} in {package['name']}")
    return resolved_package


def dependency_sets(package, enabled_dependencies, forwarded_features, include_dev=False, include_build=False):
    deps = []
    named = {}
    for dependency in package["dependencies"]:
        if dependency["kind"] == "build":
            codegen_native = (
                package["name"] == "tidepool-codegen"
                and dependency["name"] == "cc"
                and dependency["rename"] is None
                and dependency["target"] is None
            )
            facade_native = supports_native_facade_build_dependency(package["name"], dependency)
            if codegen_native:
                continue
            if not facade_native:
                raise SystemExit(f"Unmodeled build dependency in {package['name']}: {dependency['name']}")
            if not include_build:
                continue
        if dependency["kind"] == "dev" and not include_dev:
            continue
        dependency_key = cargo_dependency_key(dependency)
        if dependency.get("optional", False) and dependency_key not in enabled_dependencies:
            continue
        resolved_package = linux_dependency(package, dependency)
        label = dependency_label(dependency, resolved_package)
        if dependency["rename"]:
            named[dependency["rename"].replace("-", "_")] = label
        elif label not in deps:
            deps.append(label)
    return sorted(deps), dict(sorted(named.items()))


def render_strings(values, indent=4):
    return "\n".join(" " * indent + json.dumps(value) + "," for value in values)


LIBRARY_TEST_FIXTURES = {
    ("tidepool-extract-cmd", "tidepool-extract"): {
        "build/package/owned_service_report.py",
    },
    ("tidepool-extract-cmd", "tidepool_extract_cmd"): {
        "bridge/haskell/src/Tidepool/ExtractRequest.hs",
        "bridge/haskell/src/Tidepool/Logging.hs",
        "bridge/haskell/src/Tidepool/Timing.hs",
        "bridge/haskell/src/Tidepool/GhcPipeline.hs",
        "build/package/owned_service_report.py",
    },
    ("tidepool-toolchain", "tidepool_toolchain"): {
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v4.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v4.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v4.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v4.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.json",
    },
    # Keep test-only compiler sources out of the runtime library action.
    ("tidepool-runtime", "tidepool_runtime"): {
        "bridge/haskell/src/Tidepool/Session.hs",
    },
}

# Runtime directory reads that no include_str! or fixture_source reaches.
# The facade unit test walks the repository skill directory to compare the
# scaffolded workspace against this source projection, so every file it reads
# must be declared even though no compiled source names it.
RUNTIME_SOURCE_PROJECTIONS = {
    ("exomonad-host", "exomonad_host_unit_tests"): (
        ".exomonad/workspace/skills/exomonad-agent-spec/SKILL.md",
        ".exomonad/workspace/skills/exomonad-agent-work/SKILL.md",
        ".exomonad/workspace/skills/exomonad-cleanup/SKILL.md",
        ".exomonad/workspace/skills/exomonad-command/SKILL.md",
        ".exomonad/workspace/skills/exomonad-command/references/hosted-tools.md",
        ".exomonad/workspace/skills/exomonad-define-actors/SKILL.md",
        ".exomonad/workspace/skills/exomonad-jev/SKILL.md",
        ".exomonad/workspace/skills/exomonad-jev/references/recent-changes.md",
        ".exomonad/workspace/skills/exomonad-project-work/SKILL.md",
        ".exomonad/workspace/skills/exomonad-review/SKILL.md",
        ".exomonad/workspace/skills/exomonad-workbench/SKILL.md",
    ),
}

# A projection that names a directory root owns its whole file tree: the facade
# test reads every file under the root, so one the tuple omits is silently
# absent from its sandbox and never compared. Refuse that gap at generation.
RUNTIME_SOURCE_PROJECTION_ROOTS = {
    ("exomonad-host", "exomonad_host_unit_tests"): ".exomonad/workspace/skills",
}


def validate_runtime_source_projection(root_relative, projection):
    root = ROOT / root_relative
    if not root.is_dir():
        return
    declared = set(projection)
    actual = {
        path.relative_to(ROOT).as_posix()
        for path in root.rglob("*")
        if path.is_file() and ".git" not in path.parts
    }
    undeclared = sorted(actual - declared)
    missing = sorted(declared - actual)
    if undeclared or missing:
        raise SystemExit(
            f"skill source projection does not cover {root_relative}: "
            f"undeclared={undeclared} missing={missing}"
        )

# Each module is declared behind #[cfg(test)] in prepared_program.rs.
CODEGEN_TEST_ONLY_SOURCES = {
    "apply_tests.rs", "bytes_tests.rs", "caller_result_tests.rs",
    "double_to_int_tests.rs", "entry_tests.rs", "foreign_apply_tests.rs",
    "freer_boundary_tests.rs", "lifetime_tests.rs", "no_success_tests.rs",
    "retention_tests.rs", "settlement_tests.rs", "tests.rs",
}

def protocol_output_paths():
    manifest = ROOT / "build/protocol/outputs.txt"
    if not manifest.is_file():
        raise SystemExit("missing schema-owned build/protocol/outputs.txt")
    paths = manifest.read_text().splitlines()
    names = set()
    for path in paths:
        parsed = pathlib.PurePosixPath(path)
        if not path or parsed.is_absolute() or ".." in parsed.parts or parsed.as_posix() != path:
            raise SystemExit(f"invalid generated protocol output path {path!r}")
        name = path.replace("/", "_").replace(".", "_")
        if name in names:
            raise SystemExit(f"duplicate generated protocol subtarget {name}")
        names.add(name)
    if not paths:
        raise SystemExit("protocol output roster must not be empty")
    return paths


def generated_protocol_label(path):
    return "//bridge/protocol:generated[" + path.replace("/", "_").replace(".", "_") + "]"


def protocol_snapshot_owner(path):
    if path.startswith("bridge/haskell/lib/"):
        return "bridge/haskell"
    owner, separator, _ = path.partition("/src/")
    if not separator:
        raise SystemExit(f"protocol snapshot has no owning source package: {path}")
    return owner


def protocol_snapshot_rules(directory):
    paths = [path for path in protocol_output_paths()
             if protocol_snapshot_owner(path) == directory]
    if not paths:
        return ""
    patterns = sorted({pathlib.PurePosixPath(path).parent.relative_to(directory).as_posix() + "/*"
                       for path in paths})
    return "\n".join([
        "filegroup(", '    name = "protocol_snapshot_sources",',
        "    srcs = {" + json.dumps(directory + "/") + " + source: source for source in glob(" + json.dumps(patterns) + ")},",
        '    visibility = ["PUBLIC"],', ")", "",
    ])


def generated_producer_rules(package_name):
    if package_name == "tidepool-protocol":
        paths = protocol_output_paths()
        name, generator = "generated", ":tidepool_protocol_gen"
    elif package_name == "tidepool-mcp":
        paths = ["Tidepool/Effects/Core.hs", "Tidepool/Effects/Authored.hs", "Tidepool/Effects.hs"]
        name, generator = "effects_generated", ":tidepool_effects_gen"
    else:
        return ""
    rules = "\n".join([
        "protocol_generated(", f"    name = {json.dumps(name)},",
        f"    generator = {json.dumps(generator)},", "    outputs = [",
        render_strings(paths, 8), "    ],", '    visibility = ["PUBLIC"],', ")", "",
    ])
    if package_name == "tidepool-protocol":
        owners = sorted({protocol_snapshot_owner(path) for path in paths})
        rules += "\n".join([
            "native_source_snapshot(", '    name = "snapshot_sources",',
            "    trees = [",
            '        ("//build/protocol:snapshot_roster", ""),',
            *[f'        ("//{owner}:protocol_snapshot_sources", ""),' for owner in owners],
            "    ],", '    visibility = ["PUBLIC"],', ")", "",
            "protocol_snapshots_check(", '    name = "snapshots_check",',
            '    generated = ":generated",', '    snapshots = ":snapshot_sources",',
            '    visibility = ["PUBLIC"],', ")", "",
        ])
    return rules


def register_test_fixture(included, external_labels):
    """Keep authored test bytes in runtime resources with their owning labels."""
    relative = included.relative_to(ROOT).as_posix()
    if not included.is_file() or not included.resolve().is_relative_to(ROOT):
        raise SystemExit(f"missing or outside-repository Haskell test fixture: {relative}")
    native_owners = sorted(
        (package_dir(package) for package in local.values()
         if package["name"] in SUPPORTED_PACKAGES), key=len, reverse=True,
    )
    owner = next((directory for directory in native_owners
                  if relative.startswith(directory + "/")), None)
    if owner:
        source = relative.removeprefix(owner + "/")
        # Hex encoding is injective even when filenames differ only by punctuation.
        name = "haskell_test_fixture_" + source.encode().hex()
        label = "//" + owner + ":" + name
    elif relative.startswith("exomonad/examples/workspace/.exomonad/"):
        source = relative.removeprefix("exomonad/examples/workspace/.exomonad/")
        label = "//exomonad/examples/workspace:authored_haskell_" + source.replace("/", "_").replace(".", "_").replace("-", "_")
    elif relative.startswith("exomonad/examples/persona/.exomonad/"):
        source = relative.removeprefix("exomonad/examples/persona/.exomonad/")
        label = "//exomonad/examples/persona:authored_haskell_" + source.replace("/", "_").replace(".", "_").replace("-", "_")
    elif relative in external_labels:
        label = external_labels[relative]
    elif relative.startswith("bridge/haskell/"):
        source = relative.removeprefix("bridge/haskell/")
        HASKELL_RUST_INPUTS.add(source)
        label = "//bridge/haskell:rust_input_" + source.replace("/", "_").replace(".", "_").replace("-", "_")
    else:
        raise SystemExit(f"no owning Buck export for Haskell test fixture: {relative}")
    HASKELL_TEST_FIXTURES[relative] = label


def testing_fixture_test_cases(binary):
    tests = ["fixtures::tests::" + name for name in (
        "existing_binary_reads_changed_fixture_bytes",
        "missing_and_non_utf8_fixtures_fail_explicitly",
        "fixture_names_refuse_absolute_and_parent_components",
    )]
    return "\n".join([
        "tidepool_rust_test_cases(", '    name = "testing_fixture_resource_tests",',
        f"    binary = {json.dumps(':' + binary + '_binary')},",
        "    exact_tests = [", render_strings(tests, 8), "    ],",
        f"    expected_count = {len(tests)},", "    jobs = 1,", "    timeout = 30,",
        "    test_rule_timeout_ms = 150000,", '    visibility = ["PUBLIC"],', ")", "",
    ])


EXTERNAL_SOURCE_LABELS = {
    "Cargo.lock": "//:workspace_cargo_lock",
    "build/package/owned_service_report.py": "//build/package:owned_service_report_source",
    "bridge/handlers/src/lib.rs": "//bridge/handlers:handler_library_source",
    "bridge/atomic-write/tests/fixtures/directory_fault.c": "//bridge/atomic-write:directory_fault_fixture",
    "exomonad/actor/src/fixtures/quoted-agent-provider.hs": "//exomonad/actor:quoted_agent_provider_fixture",
    "tidepool/runtime/src/session/fixtures/activation-input-function.hs": "//tidepool/runtime:activation_input_function_fixture",
    "tidepool/runtime/src/session/fixtures/activation-input-receiver.hs": "//tidepool/runtime:activation_input_receiver_fixture",
    "tidepool/runtime/src/session/fixtures/activation-input-resident-original.hs": "//tidepool/runtime:activation_input_resident_original_fixture",
    "tidepool/runtime/src/session/fixtures/activation-input-resident-request.hs": "//tidepool/runtime:activation_input_resident_request_fixture",
    "tidepool/runtime/src/session/fixtures/activation-input-resident-shadow.hs": "//tidepool/runtime:activation_input_resident_shadow_fixture",
    "bridge/haskell/src/Tidepool/ExtractRequest.hs": "//bridge/haskell:extract_request_source",
    "bridge/haskell/src/Tidepool/Logging.hs": "//bridge/haskell:logging_source",
    "bridge/haskell/src/Tidepool/Timing.hs": "//bridge/haskell:timing_source",
    "bridge/haskell/src/Tidepool/WorkerServer.hs": "//bridge/haskell:worker_server_source",
    "bridge/haskell/src/Tidepool/GhcPipeline.hs": "//bridge/haskell:ghc_pipeline_source",
    "bridge/haskell/src/Tidepool/Session.hs": "//bridge/haskell:session_source",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.cbor": "//bridge/haskell:declaration_join_v2_cbor_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.json": "//bridge/haskell:declaration_join_v2_json_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.cbor": "//bridge/haskell:declaration_inventory_v2_cbor_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.json": "//bridge/haskell:declaration_inventory_v2_json_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.cbor": "//bridge/haskell:declaration_join_v3_cbor_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.json": "//bridge/haskell:declaration_join_v3_json_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.cbor": "//bridge/haskell:declaration_join_typed_v3_cbor_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.json": "//bridge/haskell:declaration_join_typed_v3_json_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v4.cbor": "//bridge/haskell:declaration_join_v4_cbor_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v4.json": "//bridge/haskell:declaration_join_v4_json_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v4.cbor": "//bridge/haskell:declaration_join_typed_v4_cbor_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v4.json": "//bridge/haskell:declaration_join_typed_v4_json_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.cbor": "//bridge/haskell:declaration_inventory_v3_cbor_fixture",
    "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.json": "//bridge/haskell:declaration_inventory_v3_json_fixture",
    "bridge/haskell/actors/Tidepool/Actors/Role.hs": "//bridge/haskell:actor_role_source",
    "bridge/haskell/examples/model-turns/ContextWorkflow.hs": "//bridge/haskell:context_workflow_example",
    "exomonad/examples/persona/.exomonad/config.argon.toml": "//exomonad/examples/persona:argon_config",
    "exomonad/examples/persona/.exomonad/prompts/base.md": "//exomonad/examples/persona:authored_prompt_base_md",
    "exomonad/examples/persona/.exomonad/prompts/agent.md": "//exomonad/examples/persona:authored_prompt_agent_md",
    "exomonad/examples/persona/.exomonad/prompts/api-guide.md": "//exomonad/examples/persona:authored_prompt_api_guide_md",
    "exomonad/examples/persona/.exomonad/prompts/references/companion-compositions.md": "//exomonad/examples/persona:authored_prompt_references_companion_compositions_md",
    "exomonad/examples/persona/.exomonad/prompts/references/typesafe-compositions.md": "//exomonad/examples/persona:authored_prompt_references_typesafe_compositions_md",
    "exomonad/examples/persona/.exomonad/prompts/typesafe-ai-LICENSE.md": "//exomonad/examples/persona:authored_prompt_typesafe_ai_LICENSE_md",
    "exomonad/prompts/base.md": "//exomonad/prompts:base_prompt",
    "exomonad/prompts/api-guide.md": "//exomonad/prompts:api_guide_prompt",
    "exomonad/prompts/root.md": "//exomonad/prompts:root_prompt",
    "exomonad/prompts/recreated-root.md": "//exomonad/prompts:recreated_root_prompt",
    "exomonad/prompts/worktree-agent.md": "//exomonad/prompts:worktree_agent_prompt",
    "exomonad/prompts/readonly-agent.md": "//exomonad/prompts:readonly_agent_prompt",
    "exomonad/prompts/scaffolding-agent.md": "//exomonad/prompts:scaffolding_agent_prompt",
    "exomonad/prompts/integration-agent.md": "//exomonad/prompts:integration_agent_prompt",
    "exomonad/prompts/haskell-tool-instructions.md": "//exomonad/prompts:haskell_tool_instructions",
    "exomonad/prompts/docs/actors.md": "//exomonad/prompts:doc_actors",
    "exomonad/prompts/docs/cleanup.md": "//exomonad/prompts:doc_cleanup",
    "exomonad/prompts/docs/deadline.md": "//exomonad/prompts:doc_deadline",
    "exomonad/prompts/docs/jev.md": "//exomonad/prompts:doc_jev",
    "exomonad/prompts/docs/lineage.md": "//exomonad/prompts:doc_lineage",
    "exomonad/prompts/docs/model-call.md": "//exomonad/prompts:doc_model_call",
    "exomonad/prompts/docs/recovery.md": "//exomonad/prompts:doc_recovery",
    "exomonad/prompts/docs/refinement.md": "//exomonad/prompts:doc_refinement",
    "exomonad/prompts/docs/reflect.md": "//exomonad/prompts:doc_reflect",
    "exomonad/prompts/docs/request.md": "//exomonad/prompts:doc_request",
    "exomonad/prompts/docs/tree.md": "//exomonad/prompts:doc_tree",
    "exomonad/prompts/docs/unfold.md": "//exomonad/prompts:doc_unfold",
    "exomonad/prompts/docs/watch.md": "//exomonad/prompts:doc_watch",
    "exomonad/prompts/docs/workbench.md": "//exomonad/prompts:doc_workbench",
    "exomonad/examples/workspace/.exomonad/AgentSpec.hs": "//exomonad/examples/workspace:facade_agent_spec",
    "exomonad/examples/workspace/.exomonad/prompts/review.md": "//exomonad/examples/workspace:facade_review_prompt",
    ".exomonad/workspace/Jev/Operators.hs": "//:facade_test_jev_operators",
    ".exomonad/workspace/checks/progress-route-producer.hs": "//:facade_doc__exomonad_workspace_checks_progress_route_producer_hs",
    ".exomonad/workspace/checks/progress-route-questions.hs": "//:facade_doc__exomonad_workspace_checks_progress_route_questions_hs",
    ".exomonad/workspace/checks/progress-route.hs": "//:facade_doc__exomonad_workspace_checks_progress_route_hs",
    ".exomonad/workspace/checks/project_decision_consumer.hs": "//:facade_doc__exomonad_workspace_checks_project_decision_consumer_hs",
    ".exomonad/workspace/checks/project_decision_return.hs": "//:facade_doc__exomonad_workspace_checks_project_decision_return_hs",
    ".exomonad/workspace/checks/project_delivery_setup.hs": "//:facade_doc__exomonad_workspace_checks_project_delivery_setup_hs",
    ".exomonad/workspace/checks/project_design_question.hs": "//:facade_doc__exomonad_workspace_checks_project_design_question_hs",
    ".exomonad/workspace/checks/project_plan_incorporation.hs": "//:facade_doc__exomonad_workspace_checks_project_plan_incorporation_hs",
    ".exomonad/workspace/checks/project_review_questions.hs": "//:facade_doc__exomonad_workspace_checks_project_review_questions_hs",
    ".exomonad/workspace/checks/project_review_repair.hs": "//:facade_doc__exomonad_workspace_checks_project_review_repair_hs",
    ".exomonad/workspace/checks/project_review_start.hs": "//:facade_doc__exomonad_workspace_checks_project_review_start_hs",
    ".exomonad/workspace/checks/route-reply-setup.hs": "//:facade_doc__exomonad_workspace_checks_route_reply_setup_hs",
    ".exomonad/workspace/checks/route-reply-worker.hs": "//:facade_doc__exomonad_workspace_checks_route_reply_worker_hs",
    ".exomonad/workspace/skills/exomonad-cleanup/SKILL.md": "//:actor_skill_exomonad_cleanup",
    ".exomonad/workspace/skills/exomonad-project-work/SKILL.md": "//:actor_skill_exomonad_project_work",
    ".exomonad/workspace/skills/exomonad-review/SKILL.md": "//:actor_skill_exomonad_review",
    ".exomonad/workspace/skills/exomonad-command/SKILL.md": "//:actor_skill_exomonad_command",
    ".exomonad/workspace/skills/exomonad-command/references/hosted-tools.md": "//:facade_doc__exomonad_workspace_skills_exomonad_command_references_hosted_tools_md",
    ".exomonad/workspace/skills/exomonad-define-actors/SKILL.md": "//:actor_skill_exomonad_define_actors",
    ".exomonad/workspace/skills/exomonad-agent-spec/SKILL.md": "//:actor_skill_exomonad_agent_spec",
    ".exomonad/workspace/skills/exomonad-jev/SKILL.md": "//:facade_doc__exomonad_workspace_skills_exomonad_jev_SKILL_md",
    ".exomonad/workspace/skills/exomonad-jev/references/recent-changes.md": "//:facade_doc__exomonad_workspace_skills_exomonad_jev_references_recent_changes_md",
    ".exomonad/workspace/skills/exomonad-workbench/SKILL.md": "//:facade_doc__exomonad_workspace_skills_exomonad_workbench_SKILL_md",
    "exomonad/examples/workspace/.exomonad/prompts/task.md": "//exomonad/examples/workspace:facade_task_prompt",
    "exomonad/examples/workspace/.exomonad/skills/exomonad-workbench/SKILL.md": "//exomonad/examples/workspace:facade_workbench_skill",
    '.exomonad/workspace/skills/exomonad-agent-work/SKILL.md': '//:facade_doc__exomonad_workspace_skills_exomonad_agent_work_SKILL_md',
    'exomonad/prompts/agent.md': '//exomonad/prompts:agent_prompt',
    'exomonad/prompts/docs/agents.md': '//exomonad/prompts:doc_agents',
}

def source_inputs(package, target, features=(), test_target=False):
    directory = ROOT / CURRENT_DIR
    generated_paths = [path for path in protocol_output_paths()
                       if path.startswith(CURRENT_DIR + "/src/generated/")
                       and path.endswith(".rs")]
    source_root = pathlib.Path(target["src_path"]).resolve()
    sources = {directory / "Cargo.toml"}
    external = {}
    test_only = (TEST_ONLY_SOURCES.get(package["name"], frozenset())
                 if not test_target
                 else frozenset())
    if source_root.is_relative_to(directory / "src"):
        sources.update((directory / "src").rglob("*.rs"))
        sources = {source for source in sources
                   if source.relative_to(ROOT).as_posix() not in test_only}
        facade_unit_test = package["name"] == "exomonad-host" and target["name"].endswith("_unit_tests")
        if package["name"] == "exomonad-host" and not facade_unit_test:
            sources = {
                source for source in sources
                if source.name != "tests.rs"
                and not source.stem.endswith("_tests")
                and source.stem != "test_campaign"
            }
        if "lib" in target["kind"] or "proc-macro" in target["kind"]:
            binary_roots = {
                pathlib.Path(candidate["src_path"]).resolve()
                for candidate in package["targets"] if "bin" in candidate["kind"]
            }
            sources = {
                source for source in sources
                if source.resolve() not in binary_roots
            }
        if package["name"] == "tidepool-codegen" and target["name"] == "tidepool_codegen":
            for filename in CODEGEN_TEST_ONLY_SOURCES:
                source = directory / "src/prepared_program" / filename
                if not source.is_file():
                    raise SystemExit(f"missing declared codegen test-only source {source}")
                sources.discard(source)
    else:
        # The shared module walker owns each integration root and its #[path]
        # children. Unknown declarations refuse generation rather than widening
        # a target to every test source or losing a sibling module.
        if "test" in target["kind"]:
            graph, unknown = integration_target_sources([target])
            if unknown:
                raise SystemExit(f"unresolved integration modules for {package['name']}:{target['name']}")
            sources.update(graph)
        else:
            sources.add(source_root)
    # Snapshot contents cannot contribute sources or include dependencies to a
    # native action. The producer roster owns this directory's complete closure.
    if generated_paths:
        sources = {source for source in sources
                   if not source.is_relative_to(directory / "src/generated")}
    includes = re.compile(r'include(?:_(?:str|bytes))?!\s*\(\s*"([^\"]+)"')
    pending = [path for path in sources if path.suffix == ".rs"]
    external_labels = EXTERNAL_SOURCE_LABELS
    projection_key = (package["name"], target["name"])
    projection = RUNTIME_SOURCE_PROJECTIONS.get(projection_key, ())
    projection_root = RUNTIME_SOURCE_PROJECTION_ROOTS.get(projection_key)
    if projection_root is not None:
        validate_runtime_source_projection(projection_root, projection)
    for repo_relative in projection:
        external[external_labels[repo_relative]] = repo_relative
    while pending:
        source = pending.pop()
        contents = source.read_text()
        if test_target:
            # fixture_source paths are repository relative, unlike include_str!.
            for relative in re.findall(r'\bfixture_source\s*\(\s*"([^\"]+)"', contents):
                parsed = pathlib.PurePosixPath(relative)
                if (parsed.is_absolute() or ".." in parsed.parts or parsed.as_posix() != relative
                        or "\\" in relative or parsed.suffix != ".hs"):
                    raise SystemExit(f"invalid Haskell test fixture path: {relative!r}")
                register_test_fixture(ROOT / relative, external_labels)
        relatives = list(includes.findall(contents))
        for relative in relatives:
            included = pathlib.Path(os.path.abspath(source.parent / relative))
            if not included.resolve().is_relative_to(ROOT):
                raise SystemExit(f"compile-time input outside repository {included} from {source}")
            if test_target and package["name"] in {"exomonad-host", "exomonad-actor"} and included.suffix == ".hs":
                # Compile-time includes and runtime reads share one fixture roster.
                register_test_fixture(included, external_labels)
            if included.is_relative_to(ROOT) and included.relative_to(ROOT).as_posix() in test_only:
                continue
            if (
                package["name"] == "exomonad-host"
                and not target["name"].endswith("_unit_tests")
                and included.is_relative_to(ROOT)
                and included.relative_to(ROOT).as_posix().startswith((
                    ".exomonad/workspace/",
                    "exomonad/examples/workspace/.exomonad/",
                ))
            ):
                # These include_str! references occur only in tests omitted by
                # this library/binary target slice; the user workspace is not a
                # source input to the shipped facade.
                continue
            if not included.is_file():
                raise SystemExit(f"missing compile-time input {included} from {source}")
            if included.is_relative_to(directory):
                if included not in sources:
                    sources.add(included)
                    if included.suffix == ".rs":
                        pending.append(included)
                continue
            if not included.is_relative_to(ROOT):
                raise SystemExit(f"compile-time input outside repository {included} from {source}")
            repo_relative = included.relative_to(ROOT).as_posix()
            if test_target and (
                repo_relative.startswith("exomonad/examples/workspace/.exomonad/")
                or repo_relative in {
                    "exomonad/examples/workspace/flake.nix",
                    "exomonad/examples/workspace/flake.lock",
                }
            ):
                external["//exomonad/examples/workspace:facade_test_sources"] = "exomonad/examples/workspace"
                continue
            if repo_relative in LIBRARY_TEST_FIXTURES.get((package["name"], target["name"]), set()):
                continue
            if repo_relative.startswith("bridge/haskell/") and repo_relative not in external_labels:
                relative = repo_relative.removeprefix("bridge/haskell/")
                if included.suffix == ".cbor" and relative.startswith(("test-prepared-stg/fixtures/", "test-execution-schema-encode/fixtures/")):
                    raise SystemExit(f"prepared program bytes must be declared runtime resources, not compile-time inputs: {repo_relative}")
                HASKELL_RUST_INPUTS.add(relative)
                external["//bridge/haskell:rust_input_" + relative.replace("/", "_").replace(".", "_").replace("-", "_")] = repo_relative
                continue
            if repo_relative not in external_labels:
                raise SystemExit(f"no Buck file input target for {repo_relative} (from {source})")
            external[external_labels[repo_relative]] = repo_relative
    mapped = {
        source.relative_to(directory).as_posix(): source.relative_to(ROOT).as_posix()
        for source in sources
    }
    mapped.update(external)
    # Schema output paths define the source closure, including outputs that do
    # not yet exist in the checkout. Checked-in generated copies are not inputs
    # to native Rust compilation.
    if generated_paths:
        # The producer roster is complete. A removed generated effect must not
        # survive as an ordinary source input through an obsolete snapshot.
        for path in generated_paths:
            mapped[generated_protocol_label(path)] = path
    return dict(sorted(mapped.items(), key=lambda item: item[1]))


PREPARED_FIXTURE_RESOURCES = {
    "M3": ("TIDEPOOL_M3_FIXTURE_DIR", "//bridge/haskell:m3_vertical_prepared"),
    "Resume": ("TIDEPOOL_FREER_RESUME_FIXTURE_DIR", "//bridge/haskell:freer_resume_prepared"),
    "Retention": ("TIDEPOOL_FREER_RETENTION_FIXTURE_DIR", "//bridge/haskell:freer_retention_prepared"),
    "TypedReceive": ("TIDEPOOL_TYPED_RECEIVE_FIXTURE_DIR", "//bridge/haskell:typed_receive_prepared"),
}


def test_runtime_inputs(package_name, target_name, unit=False):
    env, resources, worker = {}, [], False
    fixture_names = []
    if package_name == "tidepool-extract-cmd" and (unit or target_name == "build_products_frontend"):
        env["TIDEPOOL_TEST_BASH"] = "$(exe toolchains//:bash)"
        env["TIDEPOOL_TEST_PYTHON"] = "$(exe toolchains//:python)"
        env["TIDEPOOL_TEST_WORKER_FIXTURE"] = "$(exe :build_products_worker_fixture)"
        resources.extend(["toolchains//:bash", "toolchains//:python", ":build_products_worker_fixture"])
    if package_name == "tidepool-extract-cmd" and target_name == "no_open_coded_extract_spawns":
        env["TIDEPOOL_WORKSPACE_SOURCE_ROOT"] = "$(location //build/rust:native_qualification_sources)"
        resources.append("//build/rust:native_qualification_sources")
    if package_name == "tidepool-extract-cmd" and target_name == "retained_fingerprint":
        env["TIDEPOOL_FINGERPRINT_FIXTURE_ROOT"] = "$(location :retained_fingerprint_fixtures)"
        env["TIDEPOOL_TIMING"] = "1"
        resources.append(":retained_fingerprint_fixtures")
    if package_name == "tidepool-bignum" and target_name == "show_double_oracle":
        env["TIDEPOOL_GHC"] = "$(exe toolchains//:ghc)"
        env["TIDEPOOL_DOUBLE_ORACLE_SOURCE"] = "$(location :show_double_oracle_source)"
        resources.extend(["toolchains//:ghc", ":show_double_oracle_source"])
    if package_name in {"exomonad-host", "exomonad-actor"}:
        env["TIDEPOOL_TEST_FIXTURE_ROOT"] = "$(location //bridge/testing:haskell_test_fixtures)"
        resources.append("//bridge/testing:haskell_test_fixtures")
    if package_name == "tidepool-codegen" and unit:
        fixture_names = ["Resume", "Retention"]
    elif package_name == "tidepool-runtime":
        if unit or target_name in {"session", "prepared_execution"}:
            fixture_names = ["M3", "Resume", "Retention"]
        if not unit and target_name in {"session", "prepared_execution"}:
            fixture_names.append("TypedReceive")
        elif target_name == "prepared_resident_composite":
            fixture_names = ["Resume"]
    for name in fixture_names:
        variable, label = PREPARED_FIXTURE_RESOURCES[name]
        env[variable] = "$(location " + label + ")"
        resources.append(label)
    if (
        package_name in {"tidepool-toolchain", "tidepool-runtime", "tidepool-testing", "tidepool-handlers", "exomonad-actor"}
        or (package_name == "tidepool-extract-cmd" and not unit
            and target_name in {"daemon_integration", "transaction_integration", "build_products_ownership", "retained_fingerprint"})
        or (package_name == "exomonad-host" and target_name in {
            "facade_prepared_recipe_contract_test", "facade_recipe_source_capture_test",
            "exomonad_action_surface", "exomonad_workspace_check",
        })
        or (package_name == "tidepool-mcp" and not unit and target_name == "mcp")
    ):
        worker = True
        env.update({
            "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",
            "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",
            "TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)",
            "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",
            "TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES": "$(location //build/package:tidepool_extract_runtime_libraries)",
            "LD_LIBRARY_PATH": "$(location //build/package:tidepool_extract_runtime_libraries)",
            "TIDEPOOL_KEEP_TEST_LOGS": "1",
        })
        resources.extend(["//build/package:compiler_deployment", "//bridge/haskell:facade_embedded_sources", "//build/package:tidepool_extract_runtime_libraries"])
    if package_name == "tidepool-runtime" and not unit and target_name in {"session", "prepared_execution"}:
        env["TIDEPOOL_NATIVE_JSON_SOURCE_DIR"] = "$(location //bridge/haskell:native_json_test_sources)"
        resources.append("//bridge/haskell:native_json_test_sources")
    if package_name == "tidepool-runtime" and unit:
        env["TIDEPOOL_HASKELL_ACTORS_DIR"] = "$(location //bridge/haskell:facade_embedded_sources)/actors"
        env["TIDEPOOL_GHC"] = "$(exe toolchains//:ghc)"
        resources.append("toolchains//:ghc")
    if package_name == "tidepool-extract-cmd" and target_name == "daemon_integration":
        env["TIDEPOOL_RETAINED_IMPORT_FIXTURE_DIR"] = "$(location //bridge/haskell:retained_import_test_sources)"
        resources.append("//bridge/haskell:retained_import_test_sources")
    if package_name == "exomonad-host" and target_name in {
        "facade_prepared_recipe_contract_test", "facade_recipe_source_capture_test",
    }:
        env["TIDEPOOL_HASKELL_ACTORS_DIR"] = "$(location //bridge/haskell:facade_embedded_sources)/actors"
        env["TIDEPOOL_EFFECTS_SOURCE_ROOT"] = "$(location //bridge/mcp:effects_generated)"
        resources.append("//bridge/mcp:effects_generated")
        if target_name == "facade_recipe_source_capture_test":
            env["TIDEPOOL_RECIPE_WORKSPACE"] = "$(location //exomonad/examples/workspace:facade_test_sources)/.exomonad"
            resources.append("//exomonad/examples/workspace:facade_test_sources")
    if package_name == "tidepool-toolchain" and target_name == "declaration_join_worker":
        env["TIDEPOOL_GHC"] = "$(exe toolchains//:ghc)"
        env["TIDEPOOL_JOIN_PROOF_BIN"] = "$(exe //bridge/haskell:declaration_join_consumer)"
        resources.extend(["toolchains//:ghc", "//bridge/haskell:declaration_join_consumer"])
    if package_name == "tidepool-toolchain" and target_name == "prepared_fixture":
        env["TIDEPOOL_PREPARED_FIXTURE_COMPILER"] = "$(exe //tidepool/toolchain:prepared-fixture)"
        resources.append("//tidepool/toolchain:prepared-fixture")
    if package_name == "tidepool-toolchain" and unit:
        env["TIDEPOOL_CATALOG_TEST_PYTHON"] = "$(exe toolchains//:python)"
        env["TIDEPOOL_CATALOG_QUALIFICATION_SCRIPT"] = "$(location //build/package:qualification_script)"
        resources.extend(["toolchains//:python", "//build/package:qualification_script"])
    if package_name == "exomonad-host" and target_name == "exomonad_action_surface":
        env["TIDEPOOL_HASKELL_ACTORS_DIR"] = "$(location //bridge/haskell:facade_embedded_sources)/actors"
    if package_name == "tidepool-mcp" and not unit and target_name == "mcp":
        worker = True
        env.update({
            "TIDEPOOL_EFFECTS_SOURCE_ROOT": "$(location :effects_generated)",
            "TIDEPOOL_PROTOCOL_HASKELL_ROOT": "$(location //bridge/protocol:generated)/bridge/haskell/lib",
            "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",
            "TIDEPOOL_HASKELL_ACTORS_DIR": "$(location //bridge/haskell:facade_embedded_sources)/actors",
            "TIDEPOOL_GENERATED_SURFACE_FIXTURES": "$(location :generated_surface_fixtures)",
            "TIDEPOOL_GHC": "$(exe toolchains//:ghc)",
        })
        resources.extend([":effects_generated", "//bridge/protocol:generated", "//bridge/haskell:facade_embedded_sources", ":generated_surface_fixtures"])
    if package_name == "exomonad-node" and unit:
        env["EXOMONAD_INBOX_DIRECTORY_FAULT_LIBRARY"] = "$(location :inbox_directory_fault_shared)"
        resources.append(":inbox_directory_fault_shared")
    if package_name == "exomonad-actor":
        env["TIDEPOOL_HASKELL_ACTORS_DIR"] = "$(location //bridge/haskell:facade_embedded_sources)/actors"
    if package_name == "exomonad-actor" and unit:
        env["TIDEPOOL_JEV_SOURCE_DIR"] = "$(location toolchains//:jev_sources)"
        resources.append("toolchains//:jev_sources")
        env["TIDEPOOL_JEV_WORKSPACE_DIR"] = "$(location //exomonad/examples/workspace:facade_test_sources)/.exomonad"
        resources.append("//exomonad/examples/workspace:facade_test_sources")
        env["EXOMONAD_WORKSPACE_GITLINK"] = "$(location //build/rust:workspace_gitlink)"
        env["EXOMONAD_WORKSPACE_GIT_BUNDLE"] = "$(location //build/rust:workspace_git_bundle)"
        env["TIDEPOOL_WORKSPACE_TEST_GIT"] = "$(location toolchains//:exomonad_runtime_tools)/bin/git"
        resources.extend(["//build/rust:workspace_gitlink", "//build/rust:workspace_git_bundle", "toolchains//:exomonad_runtime_tools"])
    if package_name == "exomonad-worktree" and not unit and target_name == "worktree":
        for variable, name in (
            ("EXOMONAD_BINDING_DIRECTORY_FAULT_LIBRARY", "binding_directory_fault"),
            ("EXOMONAD_JOURNAL_FAULT_LIBRARY", "journal_fault"),
        ):
            env[variable] = "$(location :" + name + "_shared)"
            resources.append(":" + name + "_shared")
    if package_name == "tidepool-protocol":
        env["RUSTFMT"] = "$(exe toolchains//:rustfmt)"
        resources.append("toolchains//:rustfmt")
    if package_name == "exomonad-host":
        env["EXOMONAD_WORKSPACE_GITLINK"] = "$(location //build/rust:workspace_gitlink)"
        env["EXOMONAD_WORKSPACE_GIT_BUNDLE"] = "$(location //build/rust:workspace_git_bundle)"
        env["PATH"] = "$(location toolchains//:exomonad_runtime_tools)/bin"
        env["EXOMONAD_NIX_BIN"] = "$(location toolchains//:exomonad_runtime_tools)/bin/nix"
        env["EXOMONAD_NIX_OFFLINE"] = "1"
        resources.append("//build/rust:workspace_gitlink")
        resources.append("//build/rust:workspace_git_bundle")
        resources.append("toolchains//:exomonad_runtime_tools")
    return env, sorted(set(resources)), worker


def compiler_service_test_inputs(package_name, target_name, *, mixed=False, unit=True):
    env, resources, worker = test_runtime_inputs(package_name, target_name, unit=unit)
    # The Rooted service owns its Nix library closure. Generic finite test
    # loader paths must not enter that service's environment.
    env.pop("LD_LIBRARY_PATH", None)
    env.pop("TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES", None)
    deployment = "//build/package:compiler_service_deployment"
    manifest_key = "TIDEPOOL_COMPILER_SERVICE_DEPLOYMENT" if mixed else "TIDEPOOL_COMPILER_DEPLOYMENT"
    env[manifest_key] = "$(location " + deployment + ")/compiler-deployment.json"
    env["TIDEPOOL_COMPILER_SERVICE_SOURCE_CONFIG"] = "$(location " + deployment + ")/compiler-service-source.bin"
    resources = [resource for resource in resources
                 if resource != "//build/package:tidepool_extract_runtime_libraries"
                 and (mixed or resource != "//build/package:compiler_deployment")]
    return env, resources + [deployment], worker


# Single-path execution inputs must be resolved and forwarded by the native
# runner as declared resources. LD_LIBRARY_PATH here names one declared
# runtime-libraries directory; scalar controls stay in ordinary env.
NATIVE_RESOURCE_ENV_KEYS = (
    "TIDEPOOL_COMPILER_SERVICE_DEPLOYMENT",
    "EXOMONAD_OFFLINE_RUSTC",
    "EXOMONAD_OFFLINE_CC",
    "TIDEPOOL_WORKSPACE_SOURCE_ROOT",
    "PATH",
    "EXOMONAD_BINDING_DIRECTORY_FAULT_LIBRARY",
    "EXOMONAD_INBOX_DIRECTORY_FAULT_LIBRARY",
    "EXOMONAD_JOURNAL_FAULT_LIBRARY",
    "EXOMONAD_NIX_BIN",
    "EXOMONAD_WORKSPACE_GITLINK",
    "EXOMONAD_WORKSPACE_GIT_BUNDLE",
    "LD_LIBRARY_PATH",
    "RUSTFMT",
    "TIDEPOOL_CATALOG_TEST_PYTHON",
    "TIDEPOOL_CATALOG_QUALIFICATION_SCRIPT",
    "TIDEPOOL_COMPILER_DEPLOYMENT",
    "TIDEPOOL_COMPILER_SERVICE_SOURCE_CONFIG",
    "TIDEPOOL_COMPILER_MODULES",
    "TIDEPOOL_DOUBLE_ORACLE_SOURCE",
    "TIDEPOOL_EFFECTS_SOURCE_ROOT",
    "TIDEPOOL_EXTRACT",
    "TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES",
    "TIDEPOOL_EXTRACT_WORKER",
    "TIDEPOOL_FINGERPRINT_FIXTURE_ROOT",
    "TIDEPOOL_FREER_RESUME_FIXTURE_DIR",
    "TIDEPOOL_FREER_RETENTION_FIXTURE_DIR",
    "TIDEPOOL_GENERATED_SURFACE_FIXTURES",
    "TIDEPOOL_GHC",
    "TIDEPOOL_HASKELL_ACTORS_DIR",
    "TIDEPOOL_INTROSPECTION_FRONT_DOOR_FIXTURE_DIR",
    "TIDEPOOL_JEV_SOURCE_DIR",
    "TIDEPOOL_JEV_WORKSPACE_DIR",
    "TIDEPOOL_JOIN_PROOF_BIN",
    "TIDEPOOL_M3_FIXTURE_DIR",
    "TIDEPOOL_NATIVE_JSON_SOURCE_DIR",
    "TIDEPOOL_PREPARED_FIXTURE_COMPILER",
    "TIDEPOOL_PRELUDE_DIR",
    "TIDEPOOL_PROTOCOL_HASKELL_ROOT",
    "TIDEPOOL_RECIPE_WORKSPACE",
    "TIDEPOOL_RETAINED_IMPORT_FIXTURE_DIR",
    "TIDEPOOL_WORKSPACE_TEST_GIT",
    "TIDEPOOL_TYPED_RECEIVE_FIXTURE_DIR",
    "TIDEPOOL_TEST_FIXTURE_ROOT",
    "TIDEPOOL_TEST_BASH",
    "TIDEPOOL_TEST_PYTHON",
    "TIDEPOOL_TEST_WORKER_FIXTURE",
)


def runtime_arguments(env, resources, worker, resource_env=None):
    env = dict(env)
    resource_env = dict(resource_env or {})
    for key in NATIVE_RESOURCE_ENV_KEYS:
        if key in env:
            resource_env[key] = env.pop(key)
    lines = []
    if env:
        lines.append("    env = {")
        lines.extend(f"        {json.dumps(key)}: {json.dumps(value)}," for key, value in sorted(env.items()))
        lines.append("    },")
    if resources:
        lines.extend(["    resources = [", render_strings(resources, 8), "    ],"])
    if resource_env:
        lines.append("    resource_env = {")
        lines.extend(f"        {json.dumps(key)}: {json.dumps(value)}," for key, value in sorted(resource_env.items()))
        lines.append("    },")
    if worker:
        lines.append("    haskell_worker = True,")
    return "\n".join(lines)


def integration_binary_compile_env(package, source_map):
    """Project literal Cargo executable inputs from their emitted binary owners."""
    requested = set()
    for source in source_map:
        if source.endswith(".rs") and not source.startswith(("//", ":", "root//", "toolchains//")):
            contents = (ROOT / package_dir(package) / source).read_text()
            requested.update(re.findall(
                r'\b(?:option_)?env!\s*\(\s*"CARGO_BIN_EXE_([^\"]+)"', contents,
            ))
    environment = {}
    binaries = NATIVE_TARGETS[package["name"]]["binaries"]
    for binary in sorted(requested):
        if binary not in binaries:
            raise SystemExit(f"Cargo executable input {binary!r} has no binary owner in {package['name']}")
        environment["CARGO_BIN_EXE_" + binary] = "$(location " + binaries[binary]["build"] + ")"
    return environment


def render_rule(rule, name, target, package, deps, named, extra="", features=(), test_target=False):
    crate_root = target["src_path"]
    src_root = pathlib.Path(crate_root).relative_to(ROOT).as_posix()
    target_edition = next(t["edition"] for t in package["targets"] if t["src_path"] == crate_root)
    source_group = name + "_sources"
    lines = [
        "rust_filegroup(",
        "    name = " + json.dumps(source_group) + ",",
        "    mapped_srcs = {",
    ]
    source_map = source_inputs(
        package, target, features,
        test_target=test_target or rule in ("tidepool_rust_test", "tidepool_rust_isolated_test"),
    )
    if rule == "tidepool_rust_isolated_test" and "test" in target["kind"]:
        compile_env = integration_binary_compile_env(package, source_map)
        if compile_env:
            extra += "\n    compile_env = " + json.dumps(compile_env, sort_keys=True) + ","
    CURRENT_QUALIFICATION_SOURCES.update(source for source in source_map if not source.startswith(("//", ":", "root//", "toolchains//")))
    lines.extend(
        "        " + json.dumps(source) + ": " + json.dumps(mapped) + ","
        for source, mapped in source_map.items()
    )
    lines.extend([
        "    },",
        '    visibility = ["PUBLIC"],',
        ")",
        "",
        rule + "(",
        "    name = " + json.dumps(name) + ",",
        "    package_name = " + json.dumps(package["name"]) + ",",
        "    package_dir = " + json.dumps(CURRENT_DIR) + ",",
        "    version = " + json.dumps(package["version"]) + ",",
        "    crate_root = " + json.dumps(src_root) + ",",
        '    edition = "' + target_edition + '",',
        "    srcs_filegroup = " + json.dumps(":" + source_group) + ",",
    ])
    if deps:
        lines.extend(["    deps = [", render_strings(deps, 8), "    ],"])
    if named:
        lines.append("    named_deps = {")
        lines.extend("        " + json.dumps(key) + ": " + json.dumps(label) + "," for key, label in named.items())
        lines.append("    },")
    if features:
        lines.extend(["    features = [", render_strings(features, 8), "    ],"])
    if test_target or rule in ("tidepool_rust_test", "tidepool_rust_isolated_test"):
        modules = {src_root}
        fixtures = set()
        if "lib" in target["kind"] or "proc-macro" in target["kind"]:
            modules.update(TEST_ONLY_MODULES.get(package["name"], ()))
            fixtures.update(TEST_ONLY_FIXTURES.get(package["name"], ()))
            if package["name"] == "tidepool-codegen":
                modules.update(CURRENT_DIR + "/src/prepared_program/" + filename
                               for filename in CODEGEN_TEST_ONLY_SOURCES)
        elif "test" in target["kind"]:
            graph, unknown = integration_target_sources([target])
            if unknown:
                raise SystemExit(f"unresolved integration modules for {package['name']}:{target['name']}")
            modules.update(source.relative_to(ROOT).as_posix() for source in graph)
        lines.extend(["    required_test_modules = [", render_strings(sorted(modules), 8), "    ],"])
        if fixtures:
            lines.extend(["    test_fixtures = [", render_strings(sorted(fixtures), 8), "    ],"])
    if extra:
        lines.append(extra)
    lines.extend(['    visibility = ["PUBLIC"],', ")", ""])
    return "\n".join(lines)


def compile_fail_cases(package, library):
    directory = pathlib.Path(package["manifest_path"]).parent
    extra_dependencies = {
        'recovery_record_is_not_certified_product': (),
        'certified_product_has_no_digest_admission': (),
        'canonical_digest_is_not_source_admission': (),
        'source_admission_issuer_is_private': (),
        'original_draft_is_private': (),
        "retained_context_is_not_completed_execution": (),
        "original_execution_census_is_private": (),
        "native_group_issuer_is_not_artifact": (),
        "native_group_issuer_fields_are_private": (),
        "display_requires_completed_original": (),
        "type_interfaces_cannot_issue_completion": (),
        "retained_context_is_not_selected_type_interfaces": (),
        "generic_request_has_no_world_primary_advance": (),
        "frozen_execution_rejects_raw_write_selection": (),
        "frozen_execution_rejects_recovered_compiler_context": ("tidepool-toolchain",),
        "compiler_cell_requires_settled": (),
        "compiler_cell_program_requires_runtime_settlement": (),
        "compiler_renderer_requires_settled": (),
        "compiler_settled_consumed_once": (),
        "compiler_settled_no_reassociation": (),
        "machine_lease_double_borrow": (),
        "root_borrow_release": ("tidepool-codegen",),
        "request_receiver_from_root": ("frunk", "tidepool-codegen", "tidepool-mcp"),
        "root_borrow_collection": ("tidepool-codegen",),
        "managed_node_cross_builder": ("tidepool-codegen", "tidepool-repr"),
        "managed_node_consumed": ("tidepool-codegen",),
        "prepared_install_consumes_pending": (),
        "prepared_install_crossed_image": ("frunk", "tidepool-mcp"),
    }
    rules = []
    for source in sorted((directory / "tests/compile_fail").glob("*.rs")):
        if source.stem not in extra_dependencies:
            raise SystemExit(f"unregistered native compile-fail contract {source}")
        expected = source.with_suffix(".stderr")
        control = directory / "tests/compile_pass" / source.name
        if not expected.is_file() or not control.is_file():
            raise SystemExit(f"native compile-fail contract needs pinned diagnostic and valid control: {source}")
        dependencies = {library["name"].replace("-", "_"): ":" + library["name"]}
        for name in extra_dependencies[source.stem]:
            declaration = next(dep for dep in package["dependencies"] if dep["name"] == name)
            dependencies[name.replace("-", "_")] = dependency_label(
                declaration, linux_dependency(package, declaration))
        lines = ["rust_compile_fail(", f'    name = "compile_fail_{source.stem}",']
        for key, path in (("source", source), ("control", control), ("expected", expected)):
            lines.append(f"    {key} = {json.dumps(path.relative_to(directory).as_posix())},")
        lines.append("    dependencies = {")
        lines.extend(f"        {json.dumps(alias)}: {json.dumps(label)},"
                     for alias, label in sorted(dependencies.items()))
        lines.extend(["    },", '    visibility = ["PUBLIC"],', ")", ""])
        rules.append("\n".join(lines))
    return "\n".join(rules)


def codegen_publication_test_cases(binary):
    tests = (
        "exact_visibility_query_tracks_frozen_and_promoted_identities",
        "prepared_projection_preserves_ancestor_after_local_removal",
        "promotion_shadowing_keeps_hidden_dependency_custody_without_install_allocations",
        "prepared_promotion_witness_matches_committed_history",
        "prepared_promotion_histories_match_visible_model",
        "prepared_promotion_witness_refuses_exhaustion_before_mutation",
    )
    return "\n".join([
        "tidepool_rust_test_cases(",
        '    name = "codegen_publication_preparation_tests",',
        f"    binary = {json.dumps(binary)},",
        "    exact_tests = [",
        *(f"        {json.dumps('binding_table::publication_preparation_tests::' + test)}," for test in tests),
        "    ],",
        f"    expected_count = {len(tests)},",
        "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
        '    visibility = ["PUBLIC"],', ")", "",
    ])


def toolchain_compiler_free_test_cases(binary):
    # These cases use synthetic artifact state, not the package-wide compiler worker.
    lines = [
        "tidepool_rust_test_cases(",
        '    name = "toolchain_compiler_free_index_view_tests",',
        f"    binary = {json.dumps(binary)},",
        "    exact_tests = [",
        *(f"        {json.dumps(test)}," for test in TOOLCHAIN_COMPILER_FREE_TESTS),
        "    ],",
        f"    expected_count = {len(TOOLCHAIN_COMPILER_FREE_TESTS)},",
        "    jobs = 1,",
        "    timeout = 600,",
        "    test_rule_timeout_ms = 660000,",
        '    visibility = ["PUBLIC"],',
        ")",
        "",
    ]
    return "\n".join(lines)


def toolchain_package_occurrence_test_cases(binary):
    tests = ["artifact_inventory::package_occurrence_linkage_tests::" + name for name in (
        "reversed_targets_preserve_exact_package_graphs",
        "shared_package_symbol_graph_is_order_invariant",
        "compiler_deduplicates_physical_sort_global",
        "duplicate_envelope_smoke_compiles_both_targets",
        "multi_ordinal_home_keeps_per_ordinal_package_roots",
        "repeated_fresh_home_targets_reuse_exact_ordinal_bindings",
        "same_direct_package_ignores_unrelated_dictionary_cohort",
        "historical_same_seal_bodies_keep_distinct_code_identities",
        "transitive_home_package_demand_reaches_the_final_target",
    )]
    tests += ["artifacts::world_value_selection_tests::" + name for name in (
        "real_world_package_item_keeps_exact_roots_for_a_later_cell",
        "real_cached_home_world_item_admits_its_package_roots",
    )]
    env, resources, worker = compiler_service_test_inputs(
        "tidepool-toolchain", "toolchain_package_occurrence_linkage_tests"
    )
    return "\n".join([
        "tidepool_rust_test_cases(",
        '    name = "toolchain_package_occurrence_linkage_tests",',
        f"    binary = {json.dumps(binary)},",
        "    exact_tests = [", render_strings(tests, 8), "    ],",
        f"    expected_count = {len(tests)},", "    ignored = True,", "    jobs = 1,", "    timeout = 600,",
        f"    test_rule_timeout_ms = {(len(tests) * 600 + 60) * 1000},",
        '    case_compiler_modes = {' + ", ".join(
            json.dumps(test) + ': "owned-resident"' for test in tests
        ) + "},",
        runtime_arguments(env, resources, worker),
        '    visibility = ["PUBLIC"],', ")", "",
    ])


def actor_observation_test_cases(binary):
    groups = [
        ("actor_inference_journal_tests", "recovery::tests::", (
            "version_ten_reasoning_is_explicitly_openai_without_rewriting_source",
            "version_ten_history_reopens_without_rewriting",
            "version_ten_refuses_nonhistorical_reasoning_encodings",
        )),
        ("actor_forest_shutdown_tests", "", (
            'termination::tests::shutdown_batch_holds_every_fence_before_admission_wake',
            'termination::tests::overlapping_shutdown_batches_preserve_one_intent_in_opposite_input_orders',
            'termination::tests::shutdown_batch_preserves_completed_exit_prior_intent_and_unsettled_work',
            'local_actor::tests::startup_ownership_precedes_live_link_and_survives_reparenting',
            'local_actor::tests::admitted_pre_start_is_included_in_sealed_retirement_snapshot',
            'local_actor::tests::owned_child_teardown_fences_siblings_before_resource_cleanup',
            'local_actor::tests::cancelled_prepared_replacement_accepts_custody_without_running_backlog',
            'resident_actor::forest_shutdown_tests::forest_cancels_linked_members_before_staging_and_drains_only_roots',
            'resident_actor::forest_shutdown_tests::sealed_forest_refuses_provisioned_workbench_with_actual_startup_cleanup',
            'resident_actor::child_launch::tests::closed_parent_refusal_retires_prepared_shared_scope',
            'resident_actor::child_launch::tests::startup_reservation_refusal_awaits_prepared_shared_scope_cleanup',
            'resident_actor::child_launch::tests::scheduler_refusal_awaits_scope_cleanup_without_upgrading_uncertainty',
            'resident_actor::child_launch::tests::unavailable_refusal_cleanup_remains_unconfirmed',
            'resident_actor::request_wait::tests::issued_sibling_retirement_beats_ready_watch_in_both_cleanup_orders',
            'resident_actor::forest_shutdown_tests::provider_abort_refuses_nested_only_before_releasing_source_or_settlement',
            'local_actor::tests::prepared_replacement_defers_shutdown_until_activation',
            'local_actor::tests::replacement_activation_preserves_backlog_order_and_drain_intent',
            'local_actor::tests::prepared_replacement_abort_cleans_only_an_unactivated_actor',
            'local_actor::tests::replacement_fence_publishes_through_finish_actor',
            'local_actor::tests::forest_shutdown_preserves_confirmed_and_unconfirmed_component_evidence',
            'local_actor::tests::forest_shutdown_timeout_remains_unconfirmed_after_forced_actor_exit',
            'local_actor::tests::unconfirmed_cleanup_preserves_requested_exit_kind',
        )),
        ("actor_reply_settlement_history_tests", "local_actor::reply_settlement_history::", (
            "fixed_reply_delivery_histories_preserve_pending_until_continuation_settles",
            "generated_reply_delivery_histories_match_acknowledged_settlement",
            "singleton_reply_watches_distinguish_required_success_from_allowed_failure",
        )),
        ("actor_runtime_observation_lease_tests", "runtime_observation::provider_health_tests::", (
            "owned_provider_round_authorizes_idle_only_after_success",
            "abandoned_provider_round_requires_attention_until_a_new_success",
            "obsolete_provider_round_cannot_overwrite_newer_observation",
        )),
        ("actor_idle_retirement_fence_tests", "kernel::tests::", (
            "idle_retirement_fences_input_and_rounds_until_claim_is_released",
            "idle_retirement_refuses_admitted_input_and_durable_work_before_wake",
            "idle_retirement_does_not_treat_abandoned_or_failed_rounds_as_idle",
            "idle_retirement_forced_close_dominates_claim_rollback",
            "idle_retirement_probe_failure_reopens_admission",
        )),
    ]
    rules = []
    for name, prefix, leaves in groups:
        tests = [prefix + leaf for leaf in leaves]
        rules.append("\n".join([
            "tidepool_rust_test_cases(", f"    name = {json.dumps(name)},",
            f"    binary = {json.dumps(':' + binary)},",
            "    exact_tests = [", render_strings(tests, 8), "    ],",
            f"    expected_count = {len(tests)},", "    jobs = 1,", "    timeout = 30,",
            f"    test_rule_timeout_ms = {(len(tests) * 30 + 60) * 1000},",
            '    visibility = ["PUBLIC"],', ")", "",
        ]))
    return "\n".join(rules)


def actor_private_publication_test_cases(binary):
    """Private histories retain both finite fixture and owned World resources."""
    tests = ["resident_workbench::request_tests::" + name for name in (
        "abandoned_visible_host_mount_repairs_private_world_and_allows_publication",
        "hidden_private_input_retires_then_allows_publication",
        "host_only_private_execution_publishes_its_exact_retained_text",
        "successful_private_cell_publishes_declarations_and_accepted_host_writes_only",
        "tool_result_mounts_retain_original_checked_interfaces",
        "two_abandoned_visible_host_mounts_retire_before_publication",
        "two_abandoned_visible_host_mounts_retire_in_reverse_order",
        "declaration_only_history_publishes_real_progress_for_a_fresh_cell",
        "empty_import_only_history_has_an_explicit_terminal_transition",
        "failed_later_item_keeps_its_private_progress_unpublished",
        "distinct_private_programs_compose_before_publication_and_real_readmission",
        "private_freeze_between_items_refuses_without_sealing_the_running_program",
        "unchanged_private_world_preserves_later_public_source_selection",
        "disjoint_private_world_edit_preserves_later_public_source_selection",
    )]
    env, resources, worker = compiler_service_test_inputs(
        "exomonad-actor", "actor_n2_publication_tests", mixed=True
    )
    return "\n".join([
        "tidepool_rust_test_cases(",
        '    name = "actor_n2_publication_tests",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(tests, 8), "    ],",
        f"    expected_count = {len(tests)},", "    jobs = 1,", "    timeout = 600,",
        f"    test_rule_timeout_ms = {(len(tests) * 600 + 60) * 1000},",
        '    case_compiler_modes = {' + ", ".join(
            json.dumps(test) + ': "owned-resident"' for test in tests
        ) + "},",
        runtime_arguments(env, resources, worker),
        '    visibility = ["PUBLIC"],', ")", "",
    ])


def actor_notebook_contract_test_cases(binary):
    """Exercise installed notebook policy, settlement, reload refusal and cleanup."""
    acknowledgement = "resident_local_actor::completion_progress::parked_cell_does_not_block_another_calls_acknowledgement_or_control"
    tests = [acknowledgement] + ["resident_local_actor::" + name for name in (
        "reload_progress::invalid_authored_reload_arguments_release_the_invocation",
        "reload_progress::unavailable_reload_leaves_parked_cells_and_control_independent",
        "reload_uncertainty::unavailable_generation_does_not_recapture_source_or_discard_bindings",
        "resident_captured_source_retains_the_parked_job_for_a_later_cell",
    )]
    env, resources, worker = compiler_service_test_inputs("exomonad-actor", "actor_notebook_contract_tests")
    env["TIDEPOOL_ACTOR_TEST_TRACE"] = "1"
    rules = []
    for target, selected in [
        ("actor_notebook_contract_tests", tests),
        ("actor_notebook_acknowledgement_test", [acknowledgement]),
    ]:
        rules.append("\n".join([
            "tidepool_rust_test_cases(",
            f"    name = {json.dumps(target)},",
            f"    binary = {json.dumps(':' + binary)},",
            "    exact_tests = [", render_strings(selected, 8), "    ],",
            f"    expected_count = {len(selected)},", "    ignored = True,", "    jobs = 1,", "    timeout = 600,",
            f"    test_rule_timeout_ms = {(len(selected) * 600 + 60) * 1000},",
            '    case_compiler_modes = {' + ", ".join(
                json.dumps(test) + ': "owned-resident"' for test in selected
            ) + "},",
            runtime_arguments(env, resources, worker),
            '    visibility = ["PUBLIC"],', ")", "",
        ]))
    return "\n".join(rules)


# These actual World/capture cases need a resident compiler both in their
# maintained cohorts and when selected directly through `just test-lib`.
RUNTIME_CHECKED_CAPTURE_CASES = ["session::resident::authored_publication_tests::" + name for name in (
    "checked_interface_publication_conflict_preserves_private_prefix",
    "checked_interface_publication_write_failure_preserves_private_prefix",
    "checked_capture_completes_issued_binding_through_resident",
    "checked_capture_stage_requires_its_actual_issued_ticket_and_session",
    "checked_capture_rejects_invalid_second_binder_then_commits_the_actual_pair",
    "produced_native_capture_requires_its_actual_acquired_slot_and_row",
    "native_retained_resolution_visits_distinct_acquisitions_and_anchored_descendants",
    "checked_snapshot_retains_expired_observation_until_its_last_owner_drops",
)]
RUNTIME_PARKED_CAPTURE_CASES = ["session::turn::scaling_tests::typed_segment_tests::" + name for name in (
    "checked_resume_uses_the_parked_owner_and_refuses_revocation_before_effects",
    "historical_native_captures_bind_the_original_value_after_shadowing",
    "same_cell_native_capture_demands_only_the_referenced_prior_output",
    "compiler_issued_binding_survives_capture_and_releases_interface_and_root",
)]


RUNTIME_TYPED_SEGMENT_SEMANTIC_CASES = ["session::turn::scaling_tests::typed_segment_tests::" + name for name in (
    "native_class_default_keeps_its_captured_seed_after_shadowing",
    "native_class_default_calls_its_explicit_sibling_after_shadowing",
    "native_inline_specialized_function_keeps_its_strict_historical_capture",
    "scaffold_diagnostics::shipped_world_scaffold_scopes_equality_with_a_custom_prelude",
    "scaffold_diagnostics::world_preparation_reports_authored_record_warning_without_imported_warnings",
    "signed_template_helpers_execute_recursion_and_polymorphism_once",
    "genuine_let_generalization_and_unused_inner_bottom_match_ghc",
    "scalar_publication_forces_after_effect_without_publishing_then_recovers",
    "retained_action_closure_demand_does_not_repeat_completed_effect",
    "bare_observation_retains_lazy_thunk_before_later_demand",
    "effectful_observation_runs_once_and_retains_its_lazy_result",
    "late_type_error_prevents_all_effects_and_publication_before_new_retry",
    "cancellation_after_real_effect_preserves_receipt_and_allows_new_intent",
    "generated_capture_histories_match_ghc_cold_warm_and_recovery",
    "deterministic_capture_history_matches_ghc_cold_warm_and_recovery",
    "authentic_native_entries_refuse_root_and_order_substitution_before_effects",
    "warm_target_selects_previously_unselected_original_native_groups",
    "one_four_eight_actions_use_one_completed_inference_segment",
    "produced_capture_types_cross_a_declaration_barrier_without_replaying_effects",
    "zero_capture_let_executes_without_publishing_a_dummy_binding",
    "zero_capture_bang_let_preserves_forcing_and_prior_effects",
    "zero_capture_action_runs_once_before_the_next_item",
    "strict_let_group_forces_before_retaining_its_closure_capture",
)] + RUNTIME_PARKED_CAPTURE_CASES


def runtime_world_publication_test_cases(binary):
    tests = ["session::resident::paired_publication_tests::" + name for name in (
        "paired_independent_authored_rebase_preserves_originals_and_current_winners",
        "paired_first_authored_publication_preserves_atomic_visibility",
        "paired_rich_fixed_suffix_rebases_nonempty_base_and_final_value_winners",
        "paired_reused_projection_keeps_nominal_owner_and_atomic_arbitration",
        "paired_reused_projection_restores_detached_summary_under_original_interface",
        "paired_authored_parent_inventory_survives_value_only_tip_and_instance_only_suffix",
        "paired_ephemeral_completion_order_rebases_without_replaying_native_writes",
        "paired_ephemeral_ticket_keeps_attached_manifest_and_admission_mode",
        "paired_ephemeral_foreign_runtime_ticket_never_claims",
        "paired_foreign_staged_ticket_never_claims_publication",
        "paired_materialized_value_retracts_only_admitted_value_namespace",
        "paired_binding_only_intent_removes_later_value_head_and_preserves_type_namespace",
        "paired_rich_projection_reuses_exact_members_and_instance_graph",
        "paired_projection_history_matches_forced_ghc_join",
    )]
    tests.append("session::resident::world_cell_test_support::source_hidden_join::"
                 "paired_join_compiles_source_hidden_checked_bind_in_fresh_worker")
    tests.extend("session::resident::world_cell_test_support::publication_linearization::" + name for name in (
        "paired_guarded_cancellation_before_publication_preserves_public_winner",
        "paired_publication_before_guarded_cancellation_preserves_committed_winner",
        "fixed_sibling_intents_restage_after_observation_and_domain_changes_without_reexecution",
    ))
    tests.extend("session::resident::world_cell_test_support::publication_semantic_currency::" + name for name in (
        "reservations_refill_and_unrelated_private_facts_preserve_certificate_through_reopen",
        "semantic_change_between_metadata_retry_and_refresh_retains_execution_intent",
        "raw_visibility_mutation_does_not_issue_a_current_compiler_world",
        "declaration_rejection_baseline_refuses_retired_public_owner",
        "library_replacement_under_same_runtime_owner_refuses_held_publication_and_intent",
        "bounded_interference_then_fair_drain_preserves_public_owners",
    ))
    tests.extend(RUNTIME_CHECKED_CAPTURE_CASES)
    env, resources, worker = compiler_service_test_inputs(
        "tidepool-runtime", "runtime_world_publication_histories"
    )
    return "\n".join([
        "tidepool_rust_test_cases(",
        '    name = "runtime_world_publication_histories",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(tests, 8), "    ],",
        f"    expected_count = {len(tests)},", "    jobs = 1,", "    timeout = 600,",
        f"    test_rule_timeout_ms = {(len(tests) * 600 + 60) * 1000},",
        '    case_compiler_modes = {' + ", ".join(
            json.dumps(test) + ': "owned-resident"' for test in tests
        ) + "},",
        runtime_arguments(env, resources, worker),
        '    visibility = ["PUBLIC"],', ")", "",
    ])


def runtime_test_cases(binary):
    """Admission checks use native machines but do not start a compiler worker."""
    tests = ["session::admission::tests::" + name for name in (
        "inspection_injection_keeps_exact_scope_without_sibling_inventory",
        "world_private_admission_requires_the_actual_transaction_original",
    )]
    tests += ["session::resident::authored_publication_tests::" + name for name in (
        "inspection_key_invalidates_hidden_inherited_binding_projection",
        "never_visible_input_retirement_keeps_hidden_classification_until_last_lease",
    )]
    tests.append("session::persistent::package_runtime_tests::refused_install_retains_primary_and_uncertain_abort_separately")
    compiler_env, compiler_resources, worker = test_runtime_inputs("tidepool-runtime", "")
    compiler_arguments = runtime_arguments(
        compiler_env, compiler_resources, worker,
    )
    rules = ["\n".join([
        "tidepool_rust_test_cases(",
        '    name = "runtime_admission_tests",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(tests, 8), "    ],",
        f"    expected_count = {len(tests)},", "    jobs = 1,", "    timeout = 30,",
        f"    test_rule_timeout_ms = {(len(tests) * 30 + 60) * 1000},", '    visibility = ["PUBLIC"],', ")", "",
    ])]
    package_race = "session::persistent::package_runtime_tests::sibling_native_slot_install_refuses_recorded_vacancy_then_reuses_live_group"
    package_env, package_resources, package_worker = compiler_service_test_inputs(
        "tidepool-runtime", "runtime_package_slot_race_test"
    )
    rules.append("\n".join([
        "tidepool_rust_test_cases(", '    name = "runtime_package_slot_race_test",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings([package_race], 8), "    ],",
        "    expected_count = 1,", "    jobs = 1,", "    timeout = 600,",
        "    test_rule_timeout_ms = 660000,",
        '    case_compiler_modes = {' + json.dumps(package_race) + ': "owned-resident"},',
        runtime_arguments(package_env, package_resources, package_worker),
        '    visibility = ["PUBLIC"],', ")", "",
    ]))
    lifetime_env, lifetime_resources, lifetime_worker = compiler_service_test_inputs("tidepool-runtime", "runtime_world_lifetime_tests")
    lifetime_test = "session::persistent::checkpoint_scope_tests::completed_world_holds_exact_source_custody_until_install_and_abandonment"
    rules.append("\n".join([
        "tidepool_rust_test_cases(", '    name = "runtime_world_lifetime_tests",',
        f"    binary = {json.dumps(':' + binary)},",
        f"    exact_tests = [{json.dumps(lifetime_test)}],",
        "    expected_count = 1,", "    ignored = True,", "    jobs = 1,", "    timeout = 180,",
        "    test_rule_timeout_ms = 240000,", runtime_arguments(lifetime_env, lifetime_resources, lifetime_worker),
        "    case_compiler_modes = {" + json.dumps(lifetime_test) + ': "owned-resident"},',
        '    visibility = ["PUBLIC"],', ")", "",
    ]))
    semantic_tests = RUNTIME_TYPED_SEGMENT_SEMANTIC_CASES
    semantic_env, semantic_resources, semantic_worker = compiler_service_test_inputs(
        "tidepool-runtime", "runtime_typed_segment_semantic_tests"
    )
    rules.append("\n".join([
        "tidepool_rust_test_cases(",
        '    name = "runtime_typed_segment_semantic_tests",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(semantic_tests, 8), "    ],",
        f"    expected_count = {len(semantic_tests)},", "    jobs = 1,", "    timeout = 1200,",
        f"    test_rule_timeout_ms = {(len(semantic_tests) * 1200 + 60) * 1000},",
        '    case_compiler_modes = {' + ", ".join(
            json.dumps(test) + ': "owned-resident"' for test in semantic_tests
        ) + "},",
        runtime_arguments(semantic_env, semantic_resources, semantic_worker),
        '    visibility = ["PUBLIC"],', ")", "",
    ]))
    staged_history = "session::turn::scaling_tests::typed_segment_tests::staged_world_acceptance::declarations_precede_first_native_demands_and_display_without_replaying_effects"
    staged_env, staged_resources, staged_worker = compiler_service_test_inputs(
        "tidepool-runtime", "runtime_staged_world_first_demands_test"
    )
    rules.append("\n".join([
        "tidepool_rust_test_cases(",
        '    name = "runtime_staged_world_first_demands_test",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings([staged_history], 8), "    ],",
        "    expected_count = 1,", "    jobs = 1,", "    timeout = 600,",
        "    test_rule_timeout_ms = 660000,",
        '    case_compiler_modes = {' + json.dumps(staged_history) + ': "owned-resident"},',
        runtime_arguments(staged_env, staged_resources, staged_worker),
        '    visibility = ["PUBLIC"],', ")", "",
    ]))
    world_tests = ["session::turn::scaling_tests::typed_segment_tests::world_component_controls::" + name for name in (
        "world_checked_items_refuse_cross_item_mutations_before_effects",
        "world_inference_ledgers_record_single_pass_segments",
        "world_warm_emission_excludes_previously_issued_owners",
    )]
    world_env, world_resources, world_worker = test_runtime_inputs("tidepool-runtime", "", unit=True)
    rules.append("\n".join([
        "tidepool_rust_test_cases(",
        '    name = "runtime_world_segment_component_tests",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(world_tests, 8), "    ],",
        f"    expected_count = {len(world_tests)},", "    jobs = 1,", "    timeout = 1200,",
        f"    test_rule_timeout_ms = {(len(world_tests) * 1200 + 60) * 1000},",
        runtime_arguments(world_env, world_resources, world_worker),
        '    visibility = ["PUBLIC"],', ")", "",
    ]))
    for name, test in (
        ("runtime_checked_cache_test", "empty_checked_context_reuses_immutable_support_and_invalidates_changed_source"),
        ("runtime_checked_original_test", "admitted_cell_certifies_original_local_declaration_before_its_bind_and_expression"),
    ):
        rules.append("\n".join([
            "tidepool_rust_test_cases(", f"    name = {json.dumps(name)},",
            f"    binary = {json.dumps(':' + binary)},",
            f'    exact_tests = ["session::turn::tests::{test}"],',
            "    expected_count = 1,", "    jobs = 1,", "    timeout = 600,",
            "    test_rule_timeout_ms = 660000,",
            compiler_arguments,
            '    visibility = ["PUBLIC"],', ")", "",
        ]))
    return "\n".join(rules)


def facade_test_cases(binary):
    """Share one linked test harness while declaring inputs per execution group."""
    process_paths = {
        "TIDEPOOL_TEST_BASH": "$(exe toolchains//:bash)",
        "TIDEPOOL_TEST_SLEEP": "$(exe toolchains//:sleep)",
        "EXOMONAD_WORKSPACE_GITLINK": "$(location //build/rust:workspace_gitlink)",
        "EXOMONAD_WORKSPACE_GIT_BUNDLE": "$(location //build/rust:workspace_git_bundle)",
    }
    host_paths = {
        **process_paths,
        "TIDEPOOL_GHC": "$(exe toolchains//:ghc)",
        "TIDEPOOL_TEST_FIXTURE_ROOT": "$(location //bridge/testing:haskell_test_fixtures)",
        "EXOMONAD_EMBEDDED_ASSET_ROOT": "$(location //web:dist)/web",
        "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",
        "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",
        "TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)",
        "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",
        "PATH": "$(location toolchains//:exomonad_runtime_tools)/bin",
        "EXOMONAD_NIX_BIN": "$(location toolchains//:exomonad_runtime_tools)/bin/nix",
    }
    host_env = {"TIDEPOOL_KEEP_TEST_LOGS": "1", "EXOMONAD_NIX_OFFLINE": "1"}
    browser_paths = {
        **host_paths,
        "TIDEPOOL_BROWSER_DRIVER": "$(location //build/testing/browser:driver_bundle)/driver.mjs",
        "TIDEPOOL_BROWSER_NODE": "$(exe toolchains//:browser_node)",
        "PLAYWRIGHT_BROWSERS_PATH": "$(location toolchains//:playwright_browsers)",
    }
    process_resources = ["toolchains//:test_tools_closure", "//build/rust:workspace_gitlink", "//build/rust:workspace_git_bundle"]
    host_resources = process_resources + [
        "toolchains//:ghc",
        "//bridge/testing:haskell_test_fixtures",
        "toolchains//:exomonad_runtime_tools",
        "//web:dist",
        "//bridge/haskell:facade_embedded_sources",
        "//build/package:compiler_deployment",
    ]
    service_deployment = "//build/package:compiler_service_deployment"
    service_host_paths = {
        **host_paths,
        "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location " + service_deployment + ")/compiler-deployment.json",
        "TIDEPOOL_COMPILER_SERVICE_SOURCE_CONFIG": "$(location " + service_deployment + ")/compiler-service-source.bin",
    }
    service_host_resources = [resource for resource in host_resources
                              if resource != "//build/package:compiler_deployment"] + [service_deployment]
    browser_resources = host_resources + [
        "//build/testing/browser:driver_bundle",
        "toolchains//:browser_test_closure",
        "toolchains//:playwright_browsers",
    ]
    prepared_recipe_env, prepared_recipe_resources, _ = test_runtime_inputs("exomonad-host", "facade_prepared_recipe_contract_test")
    capture_recipe_env, capture_recipe_resources, _ = test_runtime_inputs("exomonad-host", "facade_recipe_source_capture_test")
    all_env = {**host_env, **prepared_recipe_env, **capture_recipe_env}
    all_paths = {**browser_paths,
                 "EXOMONAD_OFFLINE_RUSTC": "$(exe toolchains//:rustc)",
                 "EXOMONAD_OFFLINE_CC": "$(exe toolchains//:cc)",
                 "TIDEPOOL_DIRECTORY_FAULT_LIBRARY": "$(location //bridge/atomic-write:directory_fault_shared)"}
    all_resources = sorted(set(browser_resources + prepared_recipe_resources + capture_recipe_resources
                               + ["//bridge/atomic-write:directory_fault_shared", "toolchains//:rustc", "toolchains//:cc"]))
    prefix = "actor_host::m1_host_tests::"
    groups = [
        ("facade_reload_default_publication_test", [
            "actor_host::coherent_reload_acceptance_tests::standard_reload_retains_complete_large_source_receipts_after_publication",
        ], host_env, service_host_paths, service_host_resources, True, False, 900),
        ("facade_reload_frozen_source_test", [
            "actor_host::coherent_reload_acceptance_tests::staged_reload_publishes_frozen_b_and_executes_it_after_disk_moves_to_c",
        ], host_env, service_host_paths, service_host_resources, True, False, 900),
        ("facade_reload_cancellation_test", [
            "actor_host::coherent_reload_acceptance_tests::cancelled_staged_reload_preserves_a_for_a_later_default_reload",
        ], host_env, service_host_paths, service_host_resources, True, False, 900),
        ("facade_reload_caller_loss_test", [
            "actor_host::coherent_reload_acceptance_tests::staged_reload_retains_confirmed_b_when_the_caller_reply_is_lost",
        ], host_env, service_host_paths, service_host_resources, True, False, 900),
        ("facade_reload_force_refusal_test", [
            "actor_host::coherent_reload_acceptance_tests::unsupported_force_preserves_the_original_generation_for_a_later_default_reload",
        ], host_env, service_host_paths, service_host_resources, True, False, 900),
        ("facade_introspection_service_test", [
            "actor_host::documentation_tests::published_introspection_example_returns_structured_declaration",
        ], host_env, service_host_paths, service_host_resources, True, False, 600),
        ("host_inference_config_tests", [
            "exomonad::inference_selection::tests::legacy_config_remains_openai_and_new_config_is_explicit",
            "exomonad::inference_selection::tests::qualified_config_refuses_cross_backend_and_legacy_effort_ambiguity",
            "exomonad::tests::retained_launch_selection_preserves_legacy_openai_without_rewriting",
            "exomonad::tests::launch_version_and_selection_shape_must_agree",
            "actor_host::display_output::transport_admission_tests::preadmitted_transport_requires_unopened_store_and_preserves_existing_state",
            "actor_host::display_output::transport_admission_tests::preadmitted_transport_does_not_treat_uninspectable_store_as_fresh",
            "actor_host::cell_model::tests::typed_cell_model_selection_checks_actor_grants_before_provider_work",
            "actor_host::run_auth::tests::injected_contract_refuses_backend_and_limits_before_startup_work",
        ], {}, {}, ["toolchains//:exomonad_runtime_tools"], False, False, 30),
        ("facade_source_inspection_tests", [
            "exomonad::source::tests::inspection_service_retains_exact_owner_and_refuses_foreign_source",
            "exomonad::source::tests::uncovered_toolset_requires_the_same_source_owners_fresh_admission",
            "exomonad::source::tests::source_graph_keeps_workspace_resource_once_through_live_updates",
            "exomonad::source::tests::coherent_source_generation_retains_reads_prompts_and_recovers_without_links",
            "exomonad::source::tests::source_reference_refuses_incomplete_graph_aliases_and_raw_file_drift",
            "exomonad::source::tests::source_recovery_validates_recorded_ownership_and_disk_before_entry_selection",
            "exomonad::source::tests::source_generation_history_support_reaches_delayed_reads_and_refusals",
            "exomonad::source::tests::source_generation_histories_preserve_issued_reads_and_exact_recovery",
        ], {}, {}, [], False, False, 60),
        ("facade_source_revision_tests", [
            "exomonad::source::tests::missing_active_record_is_reconciled_from_the_published_link",
            "exomonad::source::tests::corrupt_published_root_indexes_remain_visibly_unavailable",
            "exomonad::source::tests::stale_active_record_is_reconciled_without_republishing_source",
            "exomonad::source::tests::publishing_a_revision_leaves_the_frozen_capture_verifiable",
            "exomonad::source::tests::source_publication_histories_preserve_revisions_through_record_recovery",
        ], {}, {}, [], False, False, 30),
        ("facade_source_fixture_tests", [
            "actor_host::tests::driver_sources_keep_policy_private_and_preserve_explicit_public_modules",
        ], {}, {
            "TIDEPOOL_TEST_FIXTURE_ROOT": "$(location //bridge/testing:haskell_test_fixtures)",
            "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",
        }, ["//bridge/testing:haskell_test_fixtures", "//bridge/haskell:facade_embedded_sources"],
         False, False, 30),
        ("facade_browser_command_tests", [
            "actor_host::test_campaign::scripted_calls_preserve_explicit_execution_modes_across_serde",
            "actor_host::embedded_command_tests::production_dispatch_admits_input_only_to_the_exact_actor",
            "actor_host::embedded_command_tests::production_dispatch_refuses_wrong_run_and_incarnation_before_admission",
            "actor_host::embedded_command_tests::production_dispatch_routes_interrupt_and_retire_to_their_exact_actors",
            "actor_host::embedded_command_tests::refused_retirement_does_not_publish_retiring_lifecycle",
            "actor_host::embedded_command_tests::admitted_retry_after_retirement_does_not_resolve_admit_or_wake",
            "actor_host::embedded_command_tests::claimed_control_and_retained_uncertainty_never_redispatch",
            "actor_host::embedded_command_tests::failed_control_acknowledgment_is_retained_as_unconfirmed",
            "actor_host::embedded_command_tests::pending_retirement_keeps_command_admission_live_and_never_reclaims_its_operation",
            "actor_host::embedded_command_tests::pending_browser_retirement_allows_exact_release_during_resident_shutdown",
            "actor_host::embedded_command_tests::browser_command_join_failures_are_reported_after_observer_drain",
            "actor_host::embedded_command_tests::observer_timeout_preserves_already_observed_failure_and_joins_remaining_tasks",
            "actor_host::embedded_command_tests::retirement_owner_loss_retains_uncertainty_without_control_replay",
            "actor_host::embedded_command_tests::timed_out_command_observation_is_joined_and_does_not_fabricate_a_receipt",
            "actor_host::embedded_command_tests::durable_command_drain_runs_without_a_channel_hint_and_skips_claimed_work",
            "actor_host::embedded_shutdown_tests::embedded_shutdown_settles_exact_waiters_and_preserves_timeout_uncertainty",
            "actor_host::embedded_shutdown_tests::shutdown_closes_release_admission_and_preserves_queued_exact_actor_waits",
            "actor_host::embedded_shutdown_tests::draining_join_retains_failed_cancellation_receipt_for_later_release_waits",
            "actor_host::embedded_shutdown_tests::final_driver_drain_records_completion_for_later_release_observations",
        ], {}, {}, [], False, False, 30),
        ("facade_process_tests", [prefix + "browser_process::tests::" + name for name in (
            "oversized_unterminated_frame_is_rejected_before_eof",
            "complete_frames_and_eof_are_observed_without_replay",
            "eof_with_partial_frame_is_a_protocol_failure",
            "cancelled_read_retains_partial_frame_for_next_select",
            "deadline_stops_descendant_and_drains_bounded_redacted_stderr",
            "successful_leader_exit_also_stops_pipe_holding_descendant",
        )], {}, process_paths, process_resources, False, False, 30),
        ("facade_host_tests", [prefix + name for name in (
            "production_host_retains_http_haskell_commands_and_reconnects_without_replay",
            "host_cancellation_stops_a_real_running_haskell_cell",
            "production_host_marks_embedded_root_ready_and_retires_invalid_auth_failure",
        )], host_env, host_paths, host_resources, True, False, 600),
        ("facade_idle_retirement_tests", ["actor_host::embedded_idle_retirement_tests::" + name for name in (
            "committed_input_before_wake_refuses_idle_retirement_and_runs_after_release",
            "idle_claim_before_input_refuses_store_mutation_and_confirms_cleanup",
        )], host_env, host_paths, host_resources, True, False, 600),
        ("facade_prepared_recipe_contract_test", [
            "actor_host::recipe_checks::prepared_contract_tests::prepared_recipe_receipts_and_assertions_execute_all_eight_cases",
        ], prepared_recipe_env, {}, prepared_recipe_resources, True, False, 600),
        ("facade_recipe_source_capture_test", [
            "exomonad::workspace::source_capture_tests::background_command_recipe_capture_records_seven_cells_without_validating_assertions",
        ], capture_recipe_env, {}, capture_recipe_resources, True, False, 600),
        ("facade_inherited_counter_test", [
            "actor_host::tests::stateful_replacement_preserves_owned_children",
        ], host_env, host_paths, host_resources, True, False, 600),
        ("facade_selected_nominal_membrane_test", [
            "actor_host::documentation_tests::selected_nominal_capture_excludes_ambient_sibling_from_native_notebook",
        ], host_env, host_paths, host_resources, True, False, 600),
        ("facade_narrow_parent_forwarding_test", [
            "actor_host::documentation_tests::narrow_parent_forwards_one_request_with_exact_child_effects",
        ], host_env, host_paths, host_resources, True, False, 600),
        ("facade_selected_notifications_test", [
            "actor_host::tests::haskell_actor_sends_normal_steering_without_a_native_session",
        ], host_env, host_paths, host_resources, True, False, 600),
        ("facade_selected_source_test", [
            "actor_host::tests::actor_sources_capture_current_then_deliver_every_publication_and_settlement",
        ], host_env, host_paths, host_resources, True, False, 600),
        ("facade_terminal_transfer_test", [
            "actor_host::terminal_transfer_tests::request_local_reply_helper_settles_exact_request_and_keeps_actor_live",
        ], host_env, host_paths, host_resources, True, False, 600),
        ("facade_selected_parent_spawn_test", [
            "actor_host::terminal_transfer_tests::selected_parent_launches_exact_subset_and_refuses_wider_or_duplicate_rows",
        ], host_env, host_paths, host_resources, True, False, 600),
        ("facade_selected_routing_tests", ["actor_host::documentation_tests::" + name for name in (
            "recipe_workspace_definitions_compile_before_resident_execution",
            "work_actor_consumes_later_progress_without_rearming",
            "attention_actor_recipe_retains_independent_sources_through_closure",
            "work_actor_projects_mixed_sources_without_changing_terminal_evidence",
            "work_actor_refuses_invalid_configuration_before_creating_a_collector",
            "work_router_queries_receipts_as_the_issuing_actor",
            "assumption_actor_stages_a_concrete_progress_source_and_retains_decisions",
        )], host_env, host_paths, host_resources, True, False, 600),
        ("facade_exact_actor_effects_tests", ["actor_host::tests::" + name for name in (
            "haskell_mailbox_preserves_state_and_opaque_replies_across_calls",
            "stateful_actor_drains_accepted_messages_into_its_retained_exit",
            "stateful_handler_failure_after_effect_pauses_without_replay_or_closing_mailbox",
            "lifecycle_sources_follow_replacement_and_capture_retained_exit",
            "stateful_replacement_preserves_owned_children",
            "stateful_replacement_rejects_changed_state_and_protocol_types",
            "actor_sources_capture_current_then_deliver_every_publication_and_settlement",
            "record_actor_explains_invalid_state_shapes",
        )], host_env, host_paths, host_resources, True, False, 600),
        ("facade_host_raw_test", [prefix +
            "production_host_retains_http_haskell_commands_and_reconnects_without_replay"],
         host_env, host_paths, host_resources, True, False, 600),
        ("facade_host_cancel_test", [prefix + "host_cancellation_stops_a_real_running_haskell_cell"],
         host_env, host_paths, host_resources, True, False, 600),
        ("facade_rejected_ancestor_output_test", [prefix +
            "production_host_recovers_ancestor_tool_output_after_request_rejection"],
         host_env, host_paths, host_resources, True, False, 600),
        ("facade_request_reload_test", [prefix +
            "request_reload_tests::real_host_pins_typed_handler_across_reload_and_pending_compaction"],
         host_env, host_paths, host_resources, True, False, 600),
        ("facade_late_output_test", [prefix +
            "real_host_late_output_tests::real_host_retains_one_late_haskell_output_across_compaction"],
         host_env, host_paths, host_resources, True, False, 600),
        ("facade_browser_test", [prefix +
            "production_browser_executes_resident_haskell_retries_and_controls_root"],
         host_env, browser_paths, browser_resources, True, True, 900),
        ("facade_outer_restart_test", [
            "exomonad::production_restart_acceptance_tests::outer_host_stop_and_crash_restore_accepted_source_and_selected_account_without_replay",
            "exomonad::production_restart_acceptance_tests::outer_host_rolls_split_binding_admission_forward_and_refuses_missing_issued_evidence",
            "exomonad::production_restart_acceptance_tests::outer_host_before_launch_admission_is_nonrunnable_without_allocating_an_epoch",
            "exomonad::production_restart_acceptance_tests::outer_host_uncertain_initializer_refuses_before_successor_without_replaying_effects",
            "exomonad::production_restart_acceptance_tests::outer_host_pending_effect_after_crash_remains_blocked_without_replay",
        ], all_env, all_paths, all_resources, True, True, 1200),
        ("facade_outer_restart_positive_test", [
            "exomonad::production_restart_acceptance_tests::outer_host_stop_and_crash_restore_accepted_source_and_selected_account_without_replay",
        ], all_env, all_paths, all_resources, True, True, 1200),
        ("exomonad_host_unit_tests_all", [], all_env, all_paths, all_resources, True, False, 600),
    ]
    rules = []
    for name, tests, env, resource_env, resources, worker, ignored, timeout in groups:
        lines = ["tidepool_rust_test_cases(", f"    name = {json.dumps(name)},",
                 f"    binary = {json.dumps(':' + binary)},"]
        if tests:
            lines.extend(["    exact_tests = [", render_strings(tests, 8), "    ],",
                          f"    expected_count = {len(tests)},"])
        if name in (
            "facade_introspection_service_test",
            "facade_reload_default_publication_test",
            "facade_reload_frozen_source_test",
            "facade_reload_cancellation_test",
            "facade_reload_caller_loss_test",
            "facade_reload_force_refusal_test",
        ):
            lines.append('    case_compiler_modes = {' + ", ".join(
                json.dumps(test) + ': "owned-resident"' for test in tests
            ) + "},")
        if name in ("facade_outer_restart_test", "facade_outer_restart_positive_test"):
            lines.extend(["    delegated_service = True,", '    service_slice = "tidepool-completion-build.slice",',
                          '    companion_executables = {"exomonad-view-helper": "//exomonad/node:view_helper_fixture"},'])
        lines.extend([f"    ignored = {ignored},", "    jobs = 1,",
                      f"    timeout = {timeout},",
                      f"    test_rule_timeout_ms = {(len(tests) * timeout + 60) * 1000 if tests else 7_200_000},",
                      runtime_arguments(env, resources, worker, resource_env),
                      '    visibility = ["PUBLIC"],', ")", ""])
        rules.append("\n".join(lines))
    return "\n".join(rules)


header = '''# @generated by scripts/buck2-first-party.py from Cargo metadata.
load("//build/rust:defs.bzl", "tidepool_rust_binary", "tidepool_rust_library", "tidepool_rust_test")
load("@prelude//:rules.bzl", "cxx_library", "export_file", "filegroup")
load("@prelude//rust:sources.bzl", "rust_filegroup")
'''

outputs = {}
NATIVE_TARGETS = {}
for package_name, package in local.items():
    if package_name not in selected:
        continue
    CURRENT_DIR = package_dir(package)
    CURRENT_QUALIFICATION_SOURCES = {"Cargo.toml", "BUCK"}
    package_features, enabled_dependencies, forwarded_features = feature_plan(package)
    has_custom_build = any("custom-build" in target["kind"] for target in package["targets"])
    if has_custom_build and package_name not in {"tidepool-codegen", "tidepool", "exomonad-host"}:
        raise SystemExit(f"{package_name} has a build.rs target; add a native Buck action before selecting it")
    rules = [header]
    if package_name in {"tidepool-protocol", "tidepool-mcp"}:
        rules.append('load("//build/protocol:defs.bzl", "protocol_generated"' +
                     (', "protocol_snapshots_check"' if package_name == "tidepool-protocol" else "") + ')\n')
    if package_name == "tidepool-protocol":
        rules.append('load("//build/rust:source_snapshot.bzl", "native_source_snapshot")\n')
    if package_name == "tidepool-codegen":
        rules.append('load("//build/rust:codegen-md5.bzl", "tidepool_codegen_md5")\n')
    rules.append('load("//build/rust:defs.bzl", "tidepool_rust_isolated_test")\n')
    if package_name == "exomonad-node":
        rules.append("""tidepool_rust_binary(
    name = "view_helper_fixture",
    package_name = "exomonad-host",
    package_dir = "exomonad/host",
    version = """ + json.dumps(package["version"]) + """,
    crate_root = "exomonad/host/src/bin/exomonad-view-helper.rs",
    edition = "2021",
    srcs_filegroup = "//exomonad/host:exomonad-view-helper_sources",
    deps = [":exomonad_node"],
    visibility = ["PUBLIC"],
)
""")
    if package_name == "exomonad-actor":
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_test_cases")\n')
    if package_name == "tidepool-toolchain":
        rules.append('load("//build/rust:executable_resource.bzl", "runtime_executable")\n')
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_test_cases")\n')
    if package_name in ("tidepool-runtime", "tidepool-codegen", "tidepool-extract-cmd"):
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_test_cases")\n')
    if package_name in ("tidepool-runtime", "tidepool-toolchain"):
        rules.append('load("//build/rust:compile_fail.bzl", "rust_compile_fail")\n')
    if package_name == "tidepool-testing":
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_test_cases")\n')
    if package_name in {"tidepool", "exomonad-host"}:
        rules.append('''load("//build/rust:buildscript.bzl", "tidepool_buildscript_run")
load("//build/rust:facade_build_inputs.bzl", "{source_inputs_rule}")
load("//build/rust:defs.bzl", "tidepool_rust_test_cases")
'''.replace("{source_inputs_rule}", "exomonad_host_build_inputs" if package_name == "exomonad-host" else "tidepool_facade_build_inputs"))
    normal_deps, normal_named = dependency_sets(package, enabled_dependencies, forwarded_features)
    dev_deps, dev_named = dependency_sets(package, enabled_dependencies, forwarded_features, include_dev=True)
    unit_deps, unit_named = dev_deps, dev_named
    if package_name == "exomonad-host":
        unit_deps, unit_named = dependency_sets(
            package, enabled_dependencies, forwarded_features, include_dev=True
        )
    if package_name == "exomonad-actor":
        unit_deps, unit_named = dependency_sets(
            package, enabled_dependencies, forwarded_features, include_dev=True
        )
    if package_name == "tidepool-runtime":
        # Compile-fail actions use declared rustc inputs instead of trybuild Cargo.
        unit_package = dict(package)
        unit_package["dependencies"] = [
            dependency for dependency in package["dependencies"]
            if not (dependency["kind"] == "dev" and dependency["name"] == "trybuild")
        ]
        unit_deps, unit_named = dependency_sets(
            unit_package, enabled_dependencies, forwarded_features, include_dev=True
        )
    if package_name == "tidepool-codegen":
        for named_dependencies in (normal_named, dev_named, unit_named):
            named_dependencies["prepared_md5_native"] = ":prepared_md5_native"
    targets = package["targets"]
    if package_name in {"tidepool", "exomonad-host"}:
        rules.append("""export_file(
    name = "{manifest_name}",
    src = "Cargo.toml",
    visibility = ["PUBLIC"],
)
""".replace("{manifest_name}", "host_cargo_manifest" if package_name == "exomonad-host" else "facade_cargo_manifest"))
        build_targets = [target for target in targets if "custom-build" in target["kind"]]
        if len(build_targets) != 1:
            raise SystemExit("facade requires exactly one Cargo build.rs target")
        build_package = dict(package)
        build_package["dependencies"] = [
            dependency for dependency in package["dependencies"] if dependency["kind"] == "build"
        ]
        build_deps, build_named = dependency_sets(
            build_package, enabled_dependencies, forwarded_features, include_build=True
        )
        rules.append(render_rule(
            "tidepool_rust_binary", "tidepool_build_script", build_targets[0],
            package, build_deps, build_named,
        ))
        source_rule = "exomonad_host_build_inputs" if package_name == "exomonad-host" else "tidepool_facade_build_inputs"
        source_args = ('    workspace_gitlink = "//build/rust:workspace_gitlink",\n'
                       '    workspace_sources = "//exomonad/examples/workspace:facade_scaffold_sources",\n'
                       if package_name == "exomonad-host" else "")
        haskell_sources = "facade_embedded_sources" if package_name == "exomonad-host" else "stdlib_embedded_sources"
        rules.append(source_rule + '''(
    name = "tidepool_build_source_tree",
    cargo_manifest = "Cargo.toml",
    haskell_sources = "//bridge/haskell:''' + haskell_sources + '''",
''' + source_args + ''')

tidepool_buildscript_run(
    name = "tidepool_build_script_run",
    first_party = True,
    package_name = "''' + package_name + '''",
    version = "''' + package["version"] + '''",
    buildscript_rule = ":tidepool_build_script",
    manifest_dir = ":tidepool_build_source_tree",
    env = {
        "TIDEPOOL_BUILD_SOURCE_ROOT": ".",
        "TIDEPOOL_EMBED_HASKELL": "1",
    },
)
''')
    NATIVE_TARGETS[package_name] = {"directory": CURRENT_DIR, "libraries": {}, "binaries": {}, "integration": {}}
    libraries = [target for target in targets if "lib" in target["kind"] or "proc-macro" in target["kind"]]
    binaries = [target for target in targets if "bin" in target["kind"]]
    tests = [target for target in targets if "test" in target["kind"]]
    for target in libraries:
        extra = "    proc_macro = True," if "proc-macro" in target["kind"] else ""
        if package_name in {"tidepool", "exomonad-host"}:
            extra = '    env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},'
        rule = render_rule("tidepool_rust_library", target["name"], target, package, normal_deps, normal_named, extra, features=package_features)
        rules.append(rule)
        NATIVE_TARGETS[package_name]["libraries"][target["name"]] = {"build": "//" + CURRENT_DIR + ":" + target["name"]}
    if package_name == "tidepool-codegen":
        rules.append("tidepool_codegen_md5()\n")
    for target in binaries:
        deps = normal_deps + ([":" + libraries[0]["name"]] if libraries else [])
        binary_name = target["name"] + "_bin" if libraries and target["name"] == libraries[0]["name"] else GENERATED_BIN_NAMES.get(target["name"], target["name"])
        extra = ""
        if package_name in {"tidepool", "exomonad-host"}:
            extra = '    env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},'
        rules.append(render_rule("tidepool_rust_binary", binary_name, target, package, deps, normal_named, extra, features=package_features))
        NATIVE_TARGETS[package_name]["binaries"][target["name"]] = {"build": "//" + CURRENT_DIR + ":" + binary_name}
    for target in binaries:
        if not target.get("test", True):
            continue
        binary_name = target["name"] + "_bin" if libraries and target["name"] == libraries[0]["name"] else GENERATED_BIN_NAMES.get(target["name"], target["name"])
        deps = dev_deps + ([":" + libraries[0]["name"]] if libraries else [])
        env, resources, worker = test_runtime_inputs(package_name, target["name"])
        extra = runtime_arguments(env, resources, worker)
        if package_name in {"tidepool", "exomonad-host"}:
            extra += '\n    compile_env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},'
        rules.append(render_rule("tidepool_rust_isolated_test", binary_name + "_unit_tests", target, package, deps, dev_named, extra, features=package_features, test_target=True))
        NATIVE_TARGETS[package_name]["binaries"][target["name"]].update({"test_build": "//" + CURRENT_DIR + ":" + binary_name + "_unit_tests_binary", "test": "//" + CURRENT_DIR + ":" + binary_name + "_unit_tests"})
    rules.append(generated_producer_rules(package_name))
    if libraries and libraries[0].get("test", True):
        library = libraries[0]
        unit_target = dict(library)
        unit_target["name"] = library["name"] + "_unit_tests"
        unit_rule = "tidepool_rust_isolated_test"
        unit_extra = ""
        env, resources, worker = test_runtime_inputs(package_name, unit_target["name"], unit=True)
        if package_name == "tidepool-toolchain":
            env, resources, worker = compiler_service_test_inputs(package_name, unit_target["name"], mixed=True)
        unit_extra = runtime_arguments(env, resources, worker)
        if package_name == "tidepool-toolchain":
            toolchain_output_context_cases = [
                'declaration_context::tests::context_role_observations_are_compact_and_bind_publication_policy',
                'declaration_context::tests::selected_raw_materialization_skips_retained_bytes_and_refuses_missing_custody',
                'declaration_context::tests::role_snapshot_equality_preserves_proof_availability_and_refuses_false_baselines',
                'declaration_context::tests::compiler_role_conflict_diagnostics_bind_actual_publication',
                'declaration_context::tests::merkle_context_history_matches_canonical_and_cold_facts',
                'declaration_context::tests::held_context_receipt_reuses_authenticated_blocks_without_reads',
                'declaration_context::tests::captured_template_data_cannot_enlarge_issued_source_publication',
                'declaration_context::tests::published_original_selection_refuses_unissued_root_and_custody_loss',
                'declaration_context::tests::published_original_manifest_binds_native_selection_and_restored_policy',
                'declaration_context::tests::publication_target_selection_distinguishes_authored_native_and_source_origins',
                'declaration_context::tests::publication_target_retains_source_policy_without_published_native_group_demand',
                'declaration_context::tests::publication_fact_snapshot_does_not_retain_restored_native_row',
                'declaration_context::tests::published_original_refuses_substituted_native_identity',
                'declaration_context::tests::published_original_composition_recovery_history',
                'declaration_context::tests::program_support_scopes_published_roles_to_exact_carriers_without_retiring_parent',
                'declaration_context::tests::program_support_completion_keeps_unrelated_publication_out_of_transient_offer',
            ]
            toolchain_output_evidence_cases = [
                'declaration_context::published_source::tests::published_source_composition_preserves_sealed_selection_across_repeated_imports',
                'declaration_context::published_source::tests::nested_publication_digest_cannot_change_recovered_source_or_native_selection',
                'declaration_context::published_source::tests::authored_certifier_preserves_published_policy_with_complete_demand',
                'declaration_context::published_source::tests::published_native_custody_does_not_require_dependency_namespace_originals',
                'declaration_context::published_source::history_properties::publication_reopening_keeps_birth_when_current_roles_change',
                'declaration_context::published_source::history_properties::publication_reopening_refuses_missing_type_namespace_with_retained_custody',
                'declaration_context::published_source::history_properties::published_role_refuses_missing_issuer_selection',
                'declaration_context::published_source::history_properties::published_source_narrowing_preserves_native_dependency_custody',
            ]
            toolchain_retained_proof_cases = [
                'artifact_inventory::tests::ordinary_publication_and_display_preserve_the_actual_sealed_issuer',
                'certified_products::tests::publication_request_history::generated_publication_support_and_recovery_preserve_issuer_selection',
            ]
            toolchain_source_admission_cases = [
                'declaration_join::authored_tests::authored_source_consumer_refuses_recovered_identity_without_admission',
                'certified_products::tests::native_request_history::actual_source_handoff_preserves_sealed_native_dependency',
                'certified_products::tests::promotion_import_history::retained_core_promotion_rekeys_nonempty_source_import_history',
                'declaration_context::tests::exact_product_binding_preserves_lexical_evidence_and_request_identity',
                'declaration_context::tests::generated_source_owner_accepts_separate_projection_receipts_and_checks_every_snapshot',
                'declaration_context::tests::terminal_output_source_scope_keeps_required_owners_and_canonical_identity',
                'artifact_inventory::compiler_projection::acquisition_tests::authored_promotion_preserves_actual_offered_acquisition',
                'artifact_inventory::compiler_projection::acquisition_tests::completed_output_keeps_actual_world_producer_beside_equal_canonical_private_offer',
                'artifact_inventory::tests::bound_projection_rebinding_preserves_issuing_none_override',
                'artifact_inventory::tests::independent_native_admission_does_not_inherit_global_source_witness',
                'artifact_inventory::tests::source_witness_histories_follow_selected_issuer',
                'artifact_inventory::tests::native_inventory_reuse_preserves_each_owners_source_admission_witness',
                'artifact_inventory::tests::artifact_subset_materialization_preserves_selected_custody_and_bounded_warm_reads',
            ]
            toolchain_existing_resident_cases = [
                'module_candidates::publication_refusal_tests::refused_completed_output_keeps_candidate_store_empty_before_valid_followup',
                'artifacts::module_product_tests::retained_entry_known_preparation_refusal_allows_same_original_foreground',
                'artifacts::source_proof_pairing_tests::source_selected_receipt_pairs_prior_program_support_with_actual_original_proof',
                'declaration_context::native_availability_tests::complete_snapshot_recovery_keeps_proof_only_native_offer_scopes',
                'declaration_context::native_availability_tests::candidate_reuse_keeps_full_availability_separate_from_withdrawn_demand',
                'declaration_context::native_availability_tests::authored_fallback_does_not_requalify_unrelated_selected_published_owner',
                'declaration_context::native_availability_tests::retained_template_capture_uses_selected_canonical_with_historical_custody',
                'declaration_context::native_availability_tests::retained_template_capture_preserves_normalized_input_modes_with_broader_proof_custody',
            ]
            toolchain_resident_cases = (
                toolchain_existing_resident_cases + toolchain_output_context_cases
                + toolchain_output_evidence_cases + toolchain_retained_proof_cases
                + toolchain_source_admission_cases
            )
            unit_extra += '\n    case_compiler_modes = {' + ", ".join(
                json.dumps(case) + ': "owned-resident"' for case in toolchain_resident_cases
            ) + "},"
        if package_name == "tidepool":
            unit_extra += '\n    compile_env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},'
        if package_name == "exomonad-host":
            unit_rule = "tidepool_rust_binary"
            unit_extra = '''    env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},
    rustc_flags = ["--test"],'''
        if package_name in {"tidepool-runtime", "exomonad-actor"}:
            unit_rule = "tidepool_rust_binary"
            unit_extra = '    rustc_flags = ["--test"],'
        rules.append(render_rule(unit_rule, unit_target["name"], unit_target, package, unit_deps, unit_named, unit_extra, features=package_features, test_target=True))
        if package_name == "tidepool-testing":
            rules.append(testing_fixture_test_cases(unit_target["name"]))
        shared = package_name in {"exomonad-host", "tidepool-runtime", "exomonad-actor"}
        if package_name in {"tidepool-runtime", "exomonad-actor"}:
            env, resources, worker = compiler_service_test_inputs(package_name, unit_target["name"], mixed=True)
            rules.append("\n".join(["tidepool_rust_test_cases(", f"    name = {json.dumps(unit_target['name'] + '_all')},", f"    binary = {json.dumps(':' + unit_target['name'])},", "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 14400000,", runtime_arguments(env, resources, worker),
                *(['    case_compiler_modes = {' + ", ".join(
                    json.dumps(name) + ': "owned-resident"'
                    for name in [
                        "session::turn::tests::cell_program_primary_failure_keeps_its_outer_close_observation",
                        *RUNTIME_CHECKED_CAPTURE_CASES,
                        *RUNTIME_TYPED_SEGMENT_SEMANTIC_CASES,
                    ]) + "},"] if package_name == "tidepool-runtime" else []),
                *(['    case_compiler_modes = {' + ", ".join(
                    json.dumps(name) + ": \"owned-resident\""
                    for name in [
                        "resident_actor::child_launch::tests::child_factory_requires_clean_uncancelled_preparation_before_runtime_adoption",
                        "resident_workbench::request_tests::durable_sleep_binding_observes_real_retraction_request_and_clean_end",
                    ]) + "},"] if package_name == "exomonad-actor" else []),
                '    visibility = ["PUBLIC"],', ")", ""]))
            if package_name == "tidepool-runtime":
                world_tests = [
                    "session::resident::activation_input_tests::frozen_activation_replays_same_input_and_refuses_second_private_input",
                    "session::resident::activation_input_tests::frozen_world_retry_keeps_completed_effect_and_original_binding",
                    "session::resident::activation_input_tests::frozen_world_unchanged_and_declaration_controls_preserve_source_refusals",
                    "session::resident::activation_input_tests::frozen_private_execution_refuses_a_later_checked_start",
                    "session::resident::activation_input_tests::resident_parcel_preserves_original_authenticated_request_across_sessions",
                    "session::resident::activation_input_tests::active_world_item_freeze_refusal_preserves_completion_and_publication",
                    "session::resident::activation_input_tests::abandoned_private_mount_after_epoch_change_settles_only_after_scope_retirement",
                    "session::resident::activation_input_tests::checked_declaration_cancellation_after_reservation_preserves_prefix",
                    "session::resident::activation_input_tests::crossed_world_declarations_refuse_foreign_checked_prefixes_then_both_publish",
                ]
                world_env, world_resources, world_worker = compiler_service_test_inputs(
                    package_name, "runtime_world_owner_controls"
                )
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "runtime_world_owner_controls",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings(world_tests, 8), "    ],",
                    f"    expected_count = {len(world_tests)},",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 1260000,",
                    '    case_compiler_modes = {' + ", ".join(
                        json.dumps(test) + ': "owned-resident"' for test in world_tests
                    ) + "},",
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                host_binding_tests = ["session::resident::activation_input_tests::" + name for name in (
                    "activation_input_reservation_refuses_visible_names_without_recompiling",
                    "activation_input_committed_binding_owns_interface_and_scope_releases_root_once",
                    "activation_input_mount_fences_owner_epoch_visibility_and_consumed_reservations",
                )]
                host_env, host_resources, host_worker = compiler_service_test_inputs(
                    package_name, "runtime_host_binding_tests"
                )
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "runtime_host_binding_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings(host_binding_tests, 8), "    ],",
                    f"    expected_count = {len(host_binding_tests)},",
                    "    jobs = 1,", "    timeout = 600,",
                    f"    test_rule_timeout_ms = {(len(host_binding_tests) * 600 + 60) * 1000},",
                    '    case_compiler_modes = {' + ", ".join(
                        json.dumps(test) + ': "owned-resident"' for test in host_binding_tests
                    ) + "},",
                    runtime_arguments(host_env, host_resources, host_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                end_history = "session::resident::activation_input_tests::compiler_end_history::physical_cell_response_without_end_ack_refuses_publication"
                end_env, end_resources, end_worker = compiler_service_test_inputs(
                    package_name, "runtime_compiler_end_history_test"
                )
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "runtime_compiler_end_history_test",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([end_history], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(end_history) + ': "owned-resident"},',
                    runtime_arguments(end_env, end_resources, end_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                disposal_history = "session::resident::activation_input_tests::world_disposal_history::released_compiler_world_restores_while_retained_native_custody_executes"
                disposal_env, disposal_resources, disposal_worker = compiler_service_test_inputs(
                    package_name, "runtime_world_disposal_history_test"
                )
                disposal_env["TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES"] = "0"
                disposal_env["TIDEPOOL_TEST_TRACE_PROFILE"] = "full"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "runtime_world_disposal_history_test",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([disposal_history], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(disposal_history) + ': "owned-resident"},',
                    runtime_arguments(disposal_env, disposal_resources, disposal_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                package_history = "session::resident::activation_input_tests::finite_source_world_history::finite_source_original_feeds_public_world_cells_after_prior_cutoff"
                package_env, package_resources, package_worker = compiler_service_test_inputs(
                    package_name, "runtime_package_source_history_test"
                )
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "runtime_package_source_history_test",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([package_history], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 900,", "    test_rule_timeout_ms = 960000,",
                    '    case_compiler_modes = {' + json.dumps(package_history) + ': "owned-resident"},',
                    runtime_arguments(package_env, package_resources, package_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                cold_history = "session::turn::scaling_tests::historical_original_executes_old_child_beside_current_same_interface_source"
                cold_env, cold_resources, cold_worker = compiler_service_test_inputs(
                    package_name, "runtime_historical_original_recovery_test"
                )
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "runtime_historical_original_recovery_test",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([cold_history], 8), "    ],",
                    "    expected_count = 1,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(cold_history) + ': "owned-resident"},',
                    runtime_arguments(cold_env, cold_resources, cold_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
            if package_name == "exomonad-actor":
                for world_target, world_tests in [
                    ("actor_compiler_world_queries_test", [
                        "resident_workbench::request_tests::public_module_world_hot_queries_skip_retained_history_preparation",
                        "resident_workbench::request_tests::public_module_world_retains_source_domain_and_invalidates_runtime_base",
                    ]),
                    ("actor_world_status_test", [
                        "resident_workbench::request_tests::binding_status_rejects_actual_world_answer_after_publication",
                    ]),
                    ("actor_private_host_custody_test", [
                        "resident_workbench::request_tests::private_input_retirement_refusal_retains_original_cleanup_owner",
                        "resident_workbench::request_tests::dropped_private_input_retains_refused_cleanup_until_scope_retirement",
                        "resident_workbench::request_tests::original_source_activation_publishes_into_recipient_world_and_replaces_input",
                    ]),
                    ("actor_world_cell_input_test", [
                        "resident_workbench::request_tests::private_world_cell_json_operand_survives_plan_end_and_preparation",
                        "resident_workbench::request_tests::private_world_cell_operand_refuses_crossed_and_retired_owners",
                    ]),
                    ("actor_world_declaration_publication_test", [
                        "resident_workbench::request_tests::world_declaration_publication_retains_operand_after_failed_later_cell",
                    ]),
                    ("actor_world_durable_generation_history_test", [
                        "resident_workbench::request_tests::quoted_source_original_survives_coherent_generation_replacement",
                    ]),
                    ("actor_world_lifetime_test", [
                        "resident_workbench::request_tests::abandoned_private_world_birth_reaps_scope_after_lost_delivery",
                    ]),
                ]:
                    world_env, world_resources, world_worker = compiler_service_test_inputs(
                        package_name, world_target
                    )
                    rules.append("\n".join([
                        "tidepool_rust_test_cases(",
                        f'    name = {json.dumps(world_target)},',
                        f"    binary = {json.dumps(':' + unit_target['name'])},",
                        "    exact_tests = [", render_strings(world_tests, 8), "    ],",
                        f"    expected_count = {len(world_tests)},", "    ignored = True,",
                        "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 1260000,",
                        '    case_compiler_modes = {' + ", ".join(
                            json.dumps(test) + ': "owned-resident"' for test in world_tests
                        ) + "},",
                        runtime_arguments(world_env, world_resources, world_worker),
                        '    visibility = ["PUBLIC"],', ")", "",
                    ]))
                renderer_env, renderer_resources, renderer_worker = compiler_service_test_inputs(
                    package_name, "activation_renderer_terminal_tests"
                )
                retirement_tests = [
                    "resident_workbench::request_tests::actual_retirement_reservation_refuses_foreign_target_before_cutoff",
                    "resident_workbench::request_tests::captured_a_publication_refusal_survives_lost_body_delivery",
                    "resident_workbench::request_tests::actual_retirement_owner_loss_after_reservation_retains_frozen_candidate",
                    "resident_workbench::request_tests::actual_retirement_owner_loss_retains_candidate_before_publisher_disposal_panic",
                    "resident_workbench::request_tests::actual_retirement_owner_loss_before_send_keeps_original_open",
                    "resident_workbench::request_tests::actual_retirement_queued_owner_loss_keeps_original_open",
                    "resident_workbench::request_tests::actual_retirement_handler_panic_retains_frozen_candidate",
                    "resident_workbench::request_tests::actual_retirement_waiter_loss_retains_frozen_candidate",
                    "resident_workbench::request_tests::actual_retirement_owner_loss_after_capture_retains_guarded_membership",
                    "resident_workbench::request_tests::actual_retirement_publication_body_refusal_retains_same_guard",
                    "resident_workbench::request_tests::actual_retirement_linked_publication_worker_settles_cancelled_body",
                    "resident_workbench::request_tests::actual_retirement_native_recovery_consumes_same_linked_publication_result",
                    "resident_workbench::request_tests::actual_unstarted_retirement_settlement_rejects_crossed_receipts_and_partial_reopen",
                ]
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_retirement_owner_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings(retirement_tests, 8), "    ],",
                    f"    expected_count = {len(retirement_tests)},", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", f"    test_rule_timeout_ms = {(len(retirement_tests) * 600 + 60) * 1000},",
                    '    case_compiler_modes = {' + ", ".join(
                        json.dumps(test) + ': "owned-resident"' for test in retirement_tests
                    ) + "},",
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                # The full retirement roster is fixed in the runner's arguments;
                # this component alias selects one genuine publisher history.
                publication_refusal = "resident_workbench::request_tests::actual_retirement_publication_body_refusal_retains_same_guard"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_publication_refusal_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([publication_refusal], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(publication_refusal) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                unstarted_control = "resident_workbench::request_tests::actual_unstarted_retirement_settlement_rejects_crossed_receipts_and_partial_reopen"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_unstarted_publication_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([unstarted_control], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(unstarted_control) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                capture_control = "resident_workbench::request_tests::actual_retirement_owner_loss_after_capture_retains_guarded_membership"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_retirement_capture_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([capture_control], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(capture_control) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                reservation_control = "resident_workbench::request_tests::actual_retirement_reservation_refuses_foreign_target_before_cutoff"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_retirement_reservation_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([reservation_control], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(reservation_control) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                delivery_loss_control = "resident_workbench::request_tests::captured_a_publication_refusal_survives_lost_body_delivery"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_retirement_delivery_loss_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([delivery_loss_control], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(delivery_loss_control) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                publication_protocol_control = "resident_workbench::request_tests::actual_publication_transport_return_controls_source_restoration"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_publication_protocol_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([publication_protocol_control], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(publication_protocol_control) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                initial_publication_control = "resident_workbench::request_tests::actual_initial_cancelled_body_preserves_its_owned_obligations"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_initial_publication_protocol_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([initial_publication_control], 8), "    ],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(initial_publication_control) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                source_registration_control = "resident_actor::replacement::tests::source_registration_slots_preserve_independent_callbacks_and_deferred_input"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_source_registration_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings([source_registration_control], 8), "    ],",
                    "    expected_count = 1,", "    ignored = False,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                    '    case_compiler_modes = {' + json.dumps(source_registration_control) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                publication_workers = [
                    "resident_workbench::request_tests::actual_retirement_linked_publication_worker_settles_cancelled_body",
                    "resident_workbench::request_tests::actual_retirement_native_recovery_consumes_same_linked_publication_result",
                ]
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "actor_generation_retirement_worker_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings(publication_workers, 8), "    ],",
                    "    expected_count = 2,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 1260000,",
                    '    case_compiler_modes = {' + ", ".join(
                        json.dumps(test) + ': "owned-resident"' for test in publication_workers
                    ) + "},",
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
                renderer_controls = ["resident_workbench::request_tests::" + name for name in (
                    "renderer_producer_loss_preserves_shared_terminal_cause_for_current_and_later_observers",
                    "renderer_producer_and_waiter_release_machine_checkout",
                    "cancelled_renderer_producer_retains_sealed_code_for_fresh_child",
                    "cancelled_renderer_producer_retries_only_after_native_close",
                )]
                renderer_diagnostics = ["resident_workbench::failure_diagnostic_tests::" + name for name in (
                    "renderer_production_keeps_action_and_unconfirmed_close_together",
                    "renderer_production_keeps_action_and_publication_refusal_together",
                    "renderer_clean_admission_failure_preserves_the_resident_recovery_requirement",
                )]
                renderer_tests = renderer_diagnostics + renderer_controls
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    '    name = "activation_renderer_terminal_tests",',
                    f"    binary = {json.dumps(':' + unit_target['name'])},",
                    "    exact_tests = [", render_strings(renderer_tests, 8), "    ],",
                    f"    expected_count = {len(renderer_tests)},",
                    "    jobs = 1,", "    timeout = 600,",
                    f"    test_rule_timeout_ms = {(len(renderer_tests) * 600 + 60) * 1000},",
                    '    case_compiler_modes = {' + ", ".join(
                        json.dumps(test) + ': "owned-resident"' for test in renderer_controls
                    ) + "},",
                    runtime_arguments(renderer_env, renderer_resources, renderer_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
        NATIVE_TARGETS[package_name]["libraries"][library["name"]].update({"test_build": "//" + CURRENT_DIR + ":" + unit_target["name"] + ("" if shared else "_binary"), "test": "//" + CURRENT_DIR + ":" + unit_target["name"] + ("_all" if shared else "")})
        if package_name == "tidepool-toolchain":
            finite_tests = ["artifacts::invocation_output::tests::" + name for name in (
                "finite_package_targets_reuse_exact_bindings_without_crossing_target_slots",
                "finite_canonical_package_interfaces_survive_output_release_and_recovery",
                "finite_original_package_interfaces_survive_independent_outputs_and_cold_recovery",
                "runtime_targets_complete_through_physical_output_with_native_products",
                "admitted_turn_uses_inline_sites_and_keeps_declarations_without_programs",
            )]
            finite_tests.append("artifacts::module_product_tests::real_worker_ordinary_package_interface_relocation_reuses_retained_token")
            finite_env, finite_resources, finite_worker = compiler_service_test_inputs(
                package_name, "toolchain_finite_output_tests"
            )
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_finite_output_tests",',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [", render_strings(finite_tests, 8), "    ],",
                f"    expected_count = {len(finite_tests)},", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,",
                f"    test_rule_timeout_ms = {(len(finite_tests) * 600 + 60) * 1000},",
                "    case_compiler_modes = {" + ", ".join(
                    json.dumps(test) + ': "owned-resident"' for test in finite_tests
                ) + "},",
                runtime_arguments(finite_env, finite_resources, finite_worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            checked_value_tests = [
                "checked_cell::tests::checked_baseline_borrows_issued_rows_and_releases_actual_backing",
                "checked_cell::tests::checked_baseline_keeps_distinct_equal_byte_issuers_and_refuses_mismatch",
            ]
            checked_value_env, checked_value_resources, checked_value_worker = compiler_service_test_inputs(
                package_name, "toolchain_checked_value_authority_tests"
            )
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_checked_value_authority_tests",',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [", render_strings(checked_value_tests, 8), "    ],",
                f"    expected_count = {len(checked_value_tests)},", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 1260000,",
                "    case_compiler_modes = {" + ", ".join(
                    json.dumps(test) + ': "owned-resident"' for test in checked_value_tests
                ) + "},",
                runtime_arguments(checked_value_env, checked_value_resources, checked_value_worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            for world_target, world_case in [
                ("toolchain_world_native_seal_test", "artifacts::module_product_tests::real_world_output_seals_native_items_after_its_matching_compiler_end"),
                ("toolchain_world_original_seal_test", "artifacts::module_product_tests::real_world_original_output_seals_its_actual_native_declaration"),
                ("toolchain_world_mixed_seal_test", "artifacts::module_product_tests::real_world_mixed_output_retains_prior_native_stages"),
                ("toolchain_world_lifetime_test", "artifacts::module_product_tests::real_world_shadow_successors_release_transport_ancestors_before_cold_restore"),
                ("toolchain_world_value_selection_test", "artifacts::world_value_selection_tests::real_world_value_selection_preserves_physical_owner_and_item_prefix"),
                ("toolchain_world_produced_value_prefix_test", "artifacts::world_value_selection_tests::real_world_produced_value_prefix_preserves_closed_indexes_without_an_operand"),
                ("toolchain_world_reference_history_test", "artifacts::world_value_selection_tests::real_world_reference_history_restores_same_owner_and_rejects_crossed_ready"),
                ("toolchain_staged_world_first_demands_test", "artifacts::world_value_selection_tests::prior_source_home_links_bind_first_packagefree_and_packageful_targets"),
            ]:
                world_env, world_resources, world_worker = compiler_service_test_inputs(
                    package_name, world_target
                )
                if world_target == "toolchain_world_reference_history_test":
                    world_env["TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES"] = "0"
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    f'    name = {json.dumps(world_target)},',
                    f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                    "    exact_tests = [" + json.dumps(world_case) + "],",
                    "    expected_count = 1,", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 1260000,",
                    "    case_compiler_modes = {" + json.dumps(world_case) + ': "owned-resident"},',
                    runtime_arguments(world_env, world_resources, world_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
            output_publication_cohorts = [
                ("toolchain_output_context_publication_test", toolchain_output_context_cases),
                ("toolchain_output_publication_evidence_test", toolchain_output_evidence_cases),
                ("toolchain_retained_output_proof_test", toolchain_retained_proof_cases),
            ]
            for cohort_target, cohort_cases in output_publication_cohorts:
                cohort_env, cohort_resources, cohort_worker = compiler_service_test_inputs(package_name, cohort_target)
                rules.append("\n".join([
                    "tidepool_rust_test_cases(",
                    f'    name = {json.dumps(cohort_target)},',
                    f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                    "    exact_tests = [", render_strings(cohort_cases, 8), "    ],",
                    f"    expected_count = {len(cohort_cases)},", "    ignored = True,",
                    "    jobs = 1,", "    timeout = 600,",
                    f"    test_rule_timeout_ms = {(len(cohort_cases) * 600 + 60) * 1000},",
                    '    case_compiler_modes = {' + ", ".join(
                        json.dumps(case) + ': "owned-resident"' for case in cohort_cases
                    ) + "},",
                    runtime_arguments(cohort_env, cohort_resources, cohort_worker),
                    '    visibility = ["PUBLIC"],', ")", "",
                ]))
            env, resources, worker = compiler_service_test_inputs(package_name, "toolchain_source_proof_pairing_test")
            source_proof_cases = [
                "artifacts::source_proof_pairing_tests::source_selected_receipt_pairs_prior_program_support_with_actual_original_proof",
                "declaration_context::native_availability_tests::complete_snapshot_recovery_keeps_proof_only_native_offer_scopes",
                "declaration_context::native_availability_tests::candidate_reuse_keeps_full_availability_separate_from_withdrawn_demand",
                "declaration_context::native_availability_tests::authored_fallback_does_not_requalify_unrelated_selected_published_owner",
                "declaration_context::native_availability_tests::retained_template_capture_uses_selected_canonical_with_historical_custody",
                "declaration_context::native_availability_tests::retained_template_capture_preserves_normalized_input_modes_with_broader_proof_custody",
            ]
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_source_proof_pairing_test",',
                '    case_compiler_modes = {' + ", ".join(
                    json.dumps(case) + ': "owned-resident"' for case in source_proof_cases
                ) + "},",
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [",
                render_strings(source_proof_cases, 8),
                "    ],", f"    expected_count = {len(source_proof_cases)},", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 3660000,",
                runtime_arguments(env, resources, worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            retained_case = "artifacts::module_product_tests::retained_entry_known_preparation_refusal_allows_same_original_foreground"
            env, resources, worker = compiler_service_test_inputs(package_name, "toolchain_retained_entry_refusal_test")
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_retained_entry_refusal_test",',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [" + json.dumps(retained_case) + "],",
                "    expected_count = 1,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                '    case_compiler_modes = {' + json.dumps(retained_case) + ': "owned-resident"},',
                runtime_arguments(env, resources, worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            env, resources, worker = compiler_service_test_inputs(package_name, "toolchain_accepted_join_custody_test")
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_accepted_join_custody_test",',
                '    case_compiler_modes = {"declaration_join::authored_tests::accepted_join_preserves_selected_dependencies_with_historical_interface_custody": "owned-resident"},',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [",
                '        "declaration_join::authored_tests::accepted_join_preserves_selected_dependencies_with_historical_interface_custody",',
                "    ],", "    expected_count = 1,", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                runtime_arguments(env, resources, worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            env, resources, worker = compiler_service_test_inputs(package_name, "toolchain_catalog_captured_inputs_test")
            env["TIDEPOOL_COMPILER_MODULES"] = "$(location //build/package:native_catalog)/catalog.json"
            resources.append("//build/package:native_catalog")
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_catalog_captured_inputs_test",',
                '    case_compiler_modes = {"artifacts::module_product_tests::real_deployment_package_reuses_cohort_and_invalidates_source_shadow": "owned-resident"},',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [",
                '        "artifacts::module_product_tests::real_deployment_package_reuses_cohort_and_invalidates_source_shadow",',
                "    ],", "    expected_count = 1,", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                runtime_arguments(env, resources, worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            env, resources, worker = test_runtime_inputs(package_name, "toolchain_borrowed_native_recapture_test", unit=True)
            env["TIDEPOOL_COMPILER_MODULES"] = "$(location //build/package:native_catalog)/catalog.json"
            resources.append("//build/package:native_catalog")
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_borrowed_native_recapture_test",',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [",
                '        "artifact_inventory::tests::borrowed_native_original_late_recapture_preserves_issued_descendants",',
                "    ],", "    expected_count = 1,", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                runtime_arguments(env, resources, worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            env, resources, worker = compiler_service_test_inputs(package_name, "toolchain_linked_entry_reopen_test")
            env["TIDEPOOL_COMPILER_MODULES"] = "$(location //build/package:native_catalog)/catalog.json"
            resources.append("//build/package:native_catalog")
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_linked_entry_reopen_test",',
                '    case_compiler_modes = {"artifacts::production_entry::source_selection_tests::linked_entry_reopens_exact_selection_in_new_process_without_source_replay": "owned-resident"},',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [",
                '        "artifacts::production_entry::source_selection_tests::linked_entry_reopens_exact_selection_in_new_process_without_source_replay",',
                "    ],", "    expected_count = 1,", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                runtime_arguments(env, resources, worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            env, resources, worker = test_runtime_inputs(package_name, "toolchain_native_original_recapture_test", unit=True)
            env["TIDEPOOL_COMPILER_MODULES"] = "$(location //build/package:native_catalog)/catalog.json"
            resources.append("//build/package:native_catalog")
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "toolchain_native_original_recapture_test",',
                f"    binary = {json.dumps(':' + unit_target['name'] + '_binary')},",
                "    exact_tests = [",
                '        "artifact_inventory::tests::borrowed_native_original_late_recapture_preserves_issued_descendants",',
                "    ],", "    expected_count = 1,", "    ignored = True,",
                "    jobs = 1,", "    timeout = 600,", "    test_rule_timeout_ms = 660000,",
                runtime_arguments(env, resources, worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
            rules.append(toolchain_compiler_free_test_cases(":" + unit_target["name"] + "_binary"))
            rules.append(toolchain_package_occurrence_test_cases(":" + unit_target["name"] + "_binary"))
            rules.append('''runtime_executable(
    name = "candidate_fixture_issuer",
    executable = ":tidepool_toolchain_unit_tests_binary",
    visibility = ["PUBLIC"],
)
''')
        if package_name == "tidepool-codegen":
            rules.append(codegen_publication_test_cases(":" + unit_target["name"] + "_binary"))
        if package_name == "exomonad-host":
            rules.append(facade_test_cases(unit_target["name"]))
        if package_name == "tidepool-runtime":
            rules.append(runtime_test_cases(unit_target["name"]))
            rules.append(runtime_world_publication_test_cases(unit_target["name"]))
        if package_name == "exomonad-actor":
            rules.append(actor_observation_test_cases(unit_target["name"]))
            rules.append(actor_private_publication_test_cases(unit_target["name"]))
    selected_tests = [target for target in tests if not (
        package_name == "tidepool-runtime" and target["name"] == "compile_fail"
    )]
    if package_name in ("tidepool-runtime", "tidepool-toolchain") and libraries:
        rules.append(compile_fail_cases(package, libraries[0]))
    for target in selected_tests:
        deps = dev_deps + ([":" + libraries[0]["name"]] if libraries else [])
        env, resources, worker = test_runtime_inputs(package_name, target["name"])
        extra = runtime_arguments(env, resources, worker)
        if package_name == "exomonad-node" and target["name"] == "command_resources":
            extra += '\n    ignored = True,\n    delegated_service = True,\n    expected_count = 4,\n    jobs = 1,'
        if package_name == "exomonad-worktree" and target["name"] == "worktree":
            extra += '\n    companion_executables = {"exomonad-view-helper": "//exomonad/node:view_helper_fixture"},'
        if package["name"] == "tidepool-atomic-write" and target["name"] == "strict_directory":
            extra = '    env = {"TIDEPOOL_DIRECTORY_FAULT_LIBRARY": "$(location :directory_fault_shared)"},'
        if package_name == "exomonad-node" and target["name"] == "overlay_rotation":
            extra += '\n    companion_executables = {"exomonad-view-helper": ":view_helper_fixture"},'
        if package["name"] == "tidepool-repr" and target["name"] == "repr":
            extra = '    env = {"TIDEPOOL_DIRECTORY_FAULT_LIBRARY": "$(location //bridge/atomic-write:directory_fault_shared)"},'
        rules.append(
            render_rule(
                "tidepool_rust_isolated_test",
                target["name"],
                target,
                package,
                deps,
                dev_named,
                extra,
                package_features,
            )
        )
        NATIVE_TARGETS[package_name]["integration"][target["name"]] = {"test_build": "//" + CURRENT_DIR + ":" + target["name"] + "_binary", "test": "//" + CURRENT_DIR + ":" + target["name"]}
        if package_name == "tidepool-extract-cmd" and target["name"] == "daemon_integration":
            service_env, service_resources, service_worker = compiler_service_test_inputs(
                package_name, target["name"], unit=False
            )
            rules.append("\n".join([
                "tidepool_rust_test_cases(",
                '    name = "owned_compiler_lifecycle_test",',
                '    binary = ":daemon_integration_binary",',
                '    exact_tests = ["owned_compiler_case_preserves_isolation_and_confirms_cleanup"],',
                "    expected_count = 1,", "    ignored = True,",
                "    jobs = 1,", "    timeout = 900,", "    test_rule_timeout_ms = 960000,",
                runtime_arguments(service_env, service_resources, service_worker),
                '    visibility = ["PUBLIC"],', ")", "",
            ]))
        if package_name == "tidepool-extract-cmd" and target["name"] == "retained_fingerprint":
            NATIVE_TARGETS[package_name]["integration"][target["name"]]["execution_disposition"] = "manual_measurement"
        if package_name == "exomonad-actor" and target["name"] == "actor":
            rules.append(actor_notebook_contract_test_cases(target["name"] + "_binary"))
    if package_name == "tidepool-extract-cmd":
        CURRENT_QUALIFICATION_SOURCES.add("src/fixtures/build_products_worker.rs")
        rules.append('''rust_filegroup(
    name = "build_products_worker_fixture_sources",
    mapped_srcs = {"src/fixtures/build_products_worker.rs": "tidepool/extract-cmd/src/fixtures/build_products_worker.rs"},
    visibility = ["PUBLIC"],
)

tidepool_rust_binary(
    name = "build_products_worker_fixture",
    package_name = "tidepool-extract-cmd",
    package_dir = "tidepool/extract-cmd",
    version = "0.1.0",
    crate_root = "tidepool/extract-cmd/src/fixtures/build_products_worker.rs",
    edition = "2021",
    srcs_filegroup = ":build_products_worker_fixture_sources",
    deps = [":tidepool_extract_cmd"],
    visibility = ["PUBLIC"],
)
''')
    if package_name == "tidepool-extract-cmd":
        rules.append('filegroup(\n    name = "retained_fingerprint_fixtures",\n    srcs = {path[len("tests/fixtures/retained_fingerprint/"):]: path for path in glob(["tests/fixtures/retained_fingerprint/*.hs"])},\n    visibility = ["PUBLIC"],\n)\n')
    if package_name == "tidepool-bignum":
        rules.append('''export_file(
    name = "show_double_oracle_source",
    src = "tests/show_double_oracle.hs",
    out = "show_double_oracle.hs",
    visibility = ["PUBLIC"],
)
''')
    if package_name == "tidepool-handlers":
        rules.append('''export_file(
    name = "handler_library_source",
    src = "src/lib.rs",
    visibility = ["PUBLIC"],
)
''')
    if package_name == "exomonad-actor":
        rules.append('''export_file(
    name = "quoted_agent_provider_fixture",
    src = "src/fixtures/quoted-agent-provider.hs",
    out = "quoted-agent-provider.hs",
    visibility = ["PUBLIC"],
)
''')
    if package_name == "tidepool-mcp":
        rules.append('''filegroup(
    name = "generated_surface_fixtures",
    srcs = {path[len("tests/fixtures/generated-surface/"): ]: path for path in glob(["tests/fixtures/generated-surface/*.hs"])},
    visibility = ["PUBLIC"],
)
''')
    if package_name == "exomonad-host":
        rules.append('''export_file(
    name = "workspace_pinned_check_source",
    src = "src/exomonad/workspace_pinned_check.hs",
    out = "workspace_pinned_check.hs",
    visibility = ["PUBLIC"],
)

filegroup(
    name = "corpus_project_sources",
    srcs = {
        "Project/Work.hs": "src/actor_host/fixtures/project/Work.hs",
        "Project/Types.hs": "src/actor_host/fixtures/project/Types.hs",
    },
    visibility = ["PUBLIC"],
)
''')
    if package["name"] == "tidepool-runtime":
        for fixture in (
            "function", "receiver", "resident-original", "resident-request", "resident-shadow",
        ):
            rules.append(f'''export_file(
    name = "activation_input_{fixture.replace("-", "_")}_fixture",
    src = "src/session/fixtures/activation-input-{fixture}.hs",
    out = "activation-input-{fixture}.hs",
    visibility = ["PUBLIC"],
)
''')
    if package["name"] == "exomonad-node":
        CURRENT_QUALIFICATION_SOURCES.add("src/inbox/fixtures/directory_fault.c")
        rules.append('''cxx_library(
    name = "inbox_directory_fault_shared",
    _cxx_toolchain = "toolchains//:c",
    srcs = ["src/inbox/fixtures/directory_fault.c"],
    preferred_linkage = "shared",
    linker_flags = ["-ldl"],
    visibility = ["PUBLIC"],
)
''')
    if package["name"] == "exomonad-worktree":
        for name in ("binding_directory_fault", "journal_fault"):
            source = "tests/fixtures/" + name + ".c"
            CURRENT_QUALIFICATION_SOURCES.add(source)
            rules.append(f'''cxx_library(
    name = "{name}_shared",
    _cxx_toolchain = "toolchains//:c",
    srcs = ["{source}"],
    preferred_linkage = "shared",
    linker_flags = ["-ldl"],
    visibility = ["PUBLIC"],
)
''')
    if package["name"] == "tidepool-atomic-write":
        rules.append('''export_file(
    name = "directory_fault_fixture",
    src = "tests/fixtures/directory_fault.c",
    visibility = ["PUBLIC"],
)

cxx_library(
    name = "directory_fault_shared",
    _cxx_toolchain = "toolchains//:c",
    srcs = ["tests/fixtures/directory_fault.c"],
    preferred_linkage = "shared",
    linker_flags = ["-ldl"],
    visibility = ["PUBLIC"],
)
''')
    snapshot_rules = protocol_snapshot_rules(CURRENT_DIR)
    if snapshot_rules:
        rules.append(snapshot_rules)
    rules.append("\n".join([
        "filegroup(", '    name = "native_qualification_inputs",', "    srcs = {",
        *("        " + json.dumps(CURRENT_DIR + "/" + source) + ": " + json.dumps(source) + "," for source in sorted(CURRENT_QUALIFICATION_SOURCES)),
        "    },", '    visibility = ["PUBLIC"],', ")", "",
    ]))
    output = "\n".join(rules).rstrip() + "\n"
    output_path = ROOT / CURRENT_DIR / "BUCK"
    outputs[output_path] = output

# Runtime fixture exports are emitted after all source walks, so a consumer may
# discover bytes owned by a package generated earlier in this invocation.
def fixture_resource_outputs(complete=True):
    for relative, label in sorted(HASKELL_TEST_FIXTURES.items()):
        owner, name = label.removeprefix("//").split(":", 1)
        output_path = ROOT / owner / "BUCK"
        if name.startswith("haskell_test_fixture_"):
            if output_path not in outputs:
                if complete:
                    raise SystemExit(f"fixture owner {owner} needs generation alongside its consumer")
                continue
            source = relative.removeprefix(owner + "/")
            outputs[output_path] += "\n" + "\n".join([
                "export_file(", f"    name = {json.dumps(name)},",
                f"    src = {json.dumps(source)},",
                f"    out = {json.dumps(pathlib.PurePosixPath(source).name)},",
                '    visibility = ["PUBLIC"],', ")", "",
            ])
    if complete:
        outputs[ROOT / "build/test-fixtures.json"] = json.dumps({
            "schema": 1, "kind": "haskell-test-fixtures", "files": sorted(HASKELL_TEST_FIXTURES),
        }, indent=2) + "\n"
    if ROOT / "bridge/testing/BUCK" not in outputs:
        return
    outputs[ROOT / "bridge/testing/BUCK"] += "\n" + "\n".join([
        "filegroup(", '    name = "haskell_test_fixtures",', "    srcs = {",
        *(f"        {json.dumps(relative)}: {json.dumps(label)},"
          for relative, label in sorted(HASKELL_TEST_FIXTURES.items())),
        "    },", '    visibility = ["PUBLIC"],', ")", "",
    ])

if selected == set(SUPPORTED_PACKAGES):
    fixture_resource_outputs()
else:
    manifest_path = ROOT / "build/test-fixtures.json"
    manifest = json.loads(manifest_path.read_text()) if manifest_path.is_file() else {}
    if not set(HASKELL_TEST_FIXTURES).issubset(set(manifest.get("files", []))):
        raise SystemExit("new Haskell fixture inputs need complete native graph regeneration")
    # Preserve the global roster when regenerating one resource owner. Only the
    # complete source walk may replace the manifest and retire fixture exports.
    HASKELL_TEST_FIXTURES.clear()
    for relative in manifest.get("files", []):
        register_test_fixture(ROOT / relative, EXTERNAL_SOURCE_LABELS)
    fixture_resource_outputs(complete=False)

if "exomonad-host" in selected:
    gitlink_rows = subprocess.check_output(["git", "ls-files", "--stage", "--", ".exomonad/workspace"], cwd=ROOT, text=True).splitlines()
    if len(gitlink_rows) != 1:
        raise SystemExit("native facade needs one recorded .exomonad/workspace gitlink")
    mode, revision, stage_path = gitlink_rows[0].split(" ", 2)
    if mode != "160000" or not re.fullmatch(r"[0-9a-f]{40}", revision) or stage_path != "0\t.exomonad/workspace":
        raise SystemExit("native facade needs an unambiguous stage-0 workspace gitlink")
    outputs[ROOT / "build/native-workspace-gitlink.json"] = json.dumps({"schema": 1, "path": ".exomonad/workspace", "mode": mode, "revision": revision}, indent=2) + "\n"

if selected == set(SUPPORTED_PACKAGES):
    qualification_trees = [("//" + package["directory"] + ":native_qualification_inputs", "") for package in NATIVE_TARGETS.values()]
    qualification_trees += [(label, "") for label in (
        "//:qualification_inputs", "//bridge/testing:haskell_test_fixtures", "toolchains//:qualification_inputs", "//platforms:qualification_inputs",
        "//build/rust:qualification_inputs", "//build/haskell:qualification_inputs", "//build/protocol:qualification_inputs",
        "//scripts:qualification_inputs", "//bridge/haskell:qualification_compiler_inputs",
        "//exomonad/examples/workspace:qualification_inputs", "//exomonad/prompts:qualification_inputs", "//build/package:qualification_inputs",
        "//build/testing/browser:qualification_inputs", "//nix/browser-test:qualification_inputs",
    )]
    outputs[ROOT / "build/rust/native_qualification_sources.bzl"] = "# @generated by scripts/buck2-first-party.py from the complete native source graph.\nNATIVE_QUALIFICATION_TREES = [\n" + "\n".join("    (" + json.dumps(label) + ", " + json.dumps(prefix) + ")," for label, prefix in sorted(qualification_trees)) + "\n]\n"

if selected == set(SUPPORTED_PACKAGES):
    outputs[ROOT / "build/native-targets.json"] = json.dumps({"schema": 1, "packages": dict(sorted(NATIVE_TARGETS.items()))}, indent=2, sort_keys=True) + "\n"
    checked_targets = set()
    source_groups = set()
    for package in NATIVE_TARGETS.values():
        for kind in ("libraries", "binaries", "integration"):
            for target in package[kind].values():
                for key in ("build", "test_build"):
                    if key in target:
                        label = target[key]
                        checked_targets.add(label)
                        source_groups.add((label[:-len("_binary")] if key == "test_build" and label.endswith("_binary") else label) + "_sources")
    outputs[ROOT / "build/rust/native_check_targets.bzl"] = "# @generated by scripts/buck2-first-party.py from all native targets.\nNATIVE_CHECK_TARGETS = " + json.dumps(sorted(checked_targets), indent=4) + "\nNATIVE_CHECK_SOURCES = " + json.dumps(sorted(source_groups), indent=4) + "\n"
    lint_levels = {"allow": [], "warn": [], "deny": [], "forbid": []}
    workspace_lints = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"].get("lints", {})
    for namespace, lints in workspace_lints.items():
        for lint, setting in lints.items():
            level = setting if isinstance(setting, str) else setting["level"]
            if level not in lint_levels:
                raise SystemExit(f"unsupported workspace lint level {level}")
            lint_levels[level].append((namespace + "::" if namespace != "rust" else "") + lint)
    for level, lints in lint_levels.items():
        outputs[ROOT / "build/rust/native_check_targets.bzl"] += "NATIVE_LINT_" + level.upper() + " = " + json.dumps(sorted(lints)) + "\n"

if selected == set(SUPPORTED_PACKAGES):
    lines = ['# @generated by scripts/buck2-first-party.py from exact Rust include inputs.',
             'load("@prelude//:rules.bzl", "export_file")', '', 'def declare_rust_test_inputs():']
    for relative in sorted(HASKELL_RUST_INPUTS):
        name = "rust_input_" + relative.replace("/", "_").replace(".", "_").replace("-", "_")
        lines.append(f"    export_file(name = {json.dumps(name)}, src = {json.dumps(relative)}, out = {json.dumps(pathlib.PurePosixPath(relative).name)}, visibility = [\"PUBLIC\"])")
    if not HASKELL_RUST_INPUTS:
        lines.append("    pass")
    outputs[ROOT / "bridge/haskell/rust_inputs.bzl"] = "\n".join(lines) + "\n"
elif HASKELL_RUST_INPUTS:
    retained = ROOT / "bridge/haskell/rust_inputs.bzl"
    content = retained.read_text() if retained.is_file() else ""
    for relative in HASKELL_RUST_INPUTS:
        name = "rust_input_" + relative.replace("/", "_").replace(".", "_").replace("-", "_")
        if f"name = {json.dumps(name)}, src = {json.dumps(relative)}," not in content:
            raise SystemExit(f"new cross-package input {relative} needs complete native graph regeneration")

if options.check:
    stale = False
    for output_path, output in outputs.items():
        if not output_path.is_file() or output_path.read_text() != output:
            print(f"stale or missing Buck target graph: {output_path.relative_to(ROOT)}", file=sys.stderr)
            stale = True
    if stale:
        sys.exit(1)
else:
    # All package validation finishes before publication. Each replacement is
    # atomic; interruption between files leaves complete, reviewable graphs.
    for output_path, output in outputs.items():
        replacement = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", dir=output_path.parent,
                prefix=".BUCK.", suffix=".tmp", delete=False,
            ) as staged:
                replacement = pathlib.Path(staged.name)
                staged.write(output)
                mode = (
                    stat.S_IMODE(output_path.stat().st_mode)
                    if output_path.exists() else 0o644
                )
                os.fchmod(staged.fileno(), mode)
            os.replace(replacement, output_path)
        finally:
            if replacement is not None:
                replacement.unlink(missing_ok=True)
