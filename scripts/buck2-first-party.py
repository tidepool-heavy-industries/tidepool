#!/usr/bin/env python3
"""Regenerate native Buck Rust targets from the locked Cargo workspace metadata."""

import argparse
import json
import pathlib
import re
import subprocess
import sys
import tomllib

from buck2_cargo_features import (
    CRATES_IO_SOURCE,
    FeatureSelectionError,
    metadata_feature_args,
    parse_locked_git_source,
    resolve as resolve_cargo_features,
)


def cargo_dependency_key(dependency):
    return dependency["rename"] or dependency["name"]

ROOT = pathlib.Path(__file__).resolve().parent.parent
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
SUPPORTED_PACKAGES = {
    "tidepool-atomic-write", "tidepool-repr", "tidepool-heap", "tidepool-bignum",
    "tidepool-bridge", "tidepool-effect", "tidepool-codegen", "tidepool-extract-cmd",
    "tidepool-extract-report", "tidepool-toolchain", "tidepool-bridge-derive",
    "tidepool-runtime", "tidepool-mcp", "tidepool-handlers", "tidepool",
    "exomonad-model", "exomonad-tool", "tidepool-bridge-effects",
    "exomonad-node", "exomonad-worktree", "exomonad-actor", "exomonad-agent",
}
NORMAL_DEPENDENCY_ONLY_PACKAGES = {
    "tidepool-bignum", "tidepool-bridge", "tidepool-effect", "tidepool-codegen",
    "tidepool-extract-report", "tidepool-bridge-derive", "tidepool-runtime",
    "exomonad-model", "exomonad-tool", "tidepool-bridge-effects",
    "exomonad-node", "exomonad-worktree", "exomonad-actor", "exomonad-agent",
    "tidepool-mcp", "tidepool-handlers", "tidepool",
}
UNIT_TEST_PACKAGES = {
    "tidepool-atomic-write", "tidepool-repr", "tidepool-heap", "tidepool-codegen",
    "tidepool-extract-cmd", "tidepool-toolchain",
}
ISOLATED_UNIT_TEST_PACKAGES = {
    "tidepool-codegen", "tidepool-extract-cmd", "tidepool-toolchain",
}
EXTRACTOR_FREE_ONLY_PACKAGES = {"tidepool-extract-cmd", "tidepool-toolchain"}
LIBRARY_TARGET_ONLY_PACKAGES = {
    "tidepool-extract-report", "tidepool-toolchain",
}
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
    unknown = (no_default | set(feature_overrides)) - workspace_packages
    if unknown:
        raise SystemExit("feature selection names an unknown workspace package: " + ", ".join(sorted(unknown)))
    return no_default, feature_overrides

metadata_command = [
    "cargo", "metadata", "--locked", "--format-version", "1",
    "--filter-platform", "x86_64-unknown-linux-gnu",
]
baseline = json.loads(subprocess.check_output(metadata_command, cwd=ROOT))
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


def feature_plan(package):
    try:
        features, dependencies, forwarded = resolve_cargo_features(
            package,
            FEATURE_OVERRIDES.get(package["name"], set()),
            package["name"] not in NO_DEFAULT_FEATURES,
            resolved_node=resolved_nodes[package["id"]],
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
            facade_native = (
                package["name"] == "tidepool"
                and dependency["name"] == "tidepool-toolchain"
                and dependency["rename"] is None
                and dependency["target"] is None
            )
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


# The repr Cargo suite is one explicit integration target with sibling module
# files. Other current integration targets are standalone crate roots.
INTEGRATION_SOURCES = {
    ("tidepool-repr", "repr"): (
        "tests/suites/repr.rs",
        "tests/execution_schema_codec.rs",
        "tests/execution_schema_contract.rs",
        "tests/extend_checked_equivalence.rs",
        "tests/metadata_strictness.rs",
        "tests/strict_jsonl_directory.rs",
    ),
}

# These codegen fixture includes are inside #[cfg(test)] modules. Keep the
# library action independent of the Haskell test corpus until test migration.
CODEGEN_TEST_FIXTURES = {
    "bridge/haskell/test-prepared-stg/fixtures/freer-resume.cbor",
    "bridge/haskell/test-prepared-stg/fixtures/freer-retention.cbor",
}

LIBRARY_TEST_FIXTURES = {
    ("tidepool-codegen", "tidepool_codegen"): CODEGEN_TEST_FIXTURES,
    ("tidepool-extract-cmd", "tidepool_extract_cmd"): {
        "bridge/haskell/src/Tidepool/ExtractRequest.hs",
        "bridge/haskell/src/Tidepool/Timing.hs",
        "bridge/haskell/src/Tidepool/GhcPipeline.hs",
    },
    ("tidepool-toolchain", "tidepool_toolchain"): {
        "bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.json",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.cbor",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.json",
    },
    # Runtime's only compile-time Haskell source is in its session tests. Those
    # tests remain on the Cargo path in this slice, so the native library's
    # source map must not claim the Haskell test source.
    ("tidepool-runtime", "tidepool_runtime"): {
        "bridge/haskell/src/Tidepool/Session.hs",
    },
}

CODEGEN_NATIVE_TESTS = {"native_md5_link"}

# Each module is declared behind #[cfg(test)] in prepared_program.rs.
CODEGEN_TEST_ONLY_SOURCES = {
    "apply_tests.rs", "bytes_tests.rs", "caller_result_tests.rs",
    "double_to_int_tests.rs", "entry_tests.rs", "foreign_apply_tests.rs",
    "freer_boundary_tests.rs", "lifetime_tests.rs", "no_success_tests.rs",
    "retention_tests.rs", "settlement_tests.rs", "tests.rs",
}


def source_inputs(package, target):
    directory = ROOT / CURRENT_DIR
    source_root = pathlib.Path(target["src_path"]).resolve()
    sources = {directory / "Cargo.toml"}
    external = {}
    if source_root.is_relative_to(directory / "src"):
        sources.update((directory / "src").rglob("*.rs"))
        if package["name"] == "tidepool":
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
        # Cargo gives the crate root. The repr suite has explicit sibling
        # modules; standalone integration targets need only their own root.
        declared = INTEGRATION_SOURCES.get((package["name"], target["name"]))
        if declared is None:
            sources.add(source_root)
        else:
            for relative in declared:
                source = directory / relative
                if not source.is_file():
                    raise SystemExit(f"missing integration source {source}")
                sources.add(source)
    includes = re.compile(r'include_(?:str|bytes)!\s*\(\s*"([^\"]+)"')
    pending = [path for path in sources if path.suffix == ".rs"]
    external_labels = {
        "bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor": "//bridge/haskell:m3_vertical_fixture",
        "bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor": "//bridge/haskell:schema6_intrinsic_fixture",
        "bridge/atomic-write/tests/fixtures/directory_fault.c": "//bridge/atomic-write:directory_fault_fixture",
        "bridge/haskell/test-prepared-stg/fixtures/freer-resume.cbor": "//bridge/haskell:freer_resume_fixture",
        "bridge/haskell/test-prepared-stg/fixtures/freer-retention.cbor": "//bridge/haskell:freer_retention_fixture",
        "bridge/haskell/src/Tidepool/ExtractRequest.hs": "//bridge/haskell:extract_request_source",
        "bridge/haskell/src/Tidepool/Timing.hs": "//bridge/haskell:timing_source",
        "bridge/haskell/src/Tidepool/GhcPipeline.hs": "//bridge/haskell:ghc_pipeline_source",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.cbor": "//bridge/haskell:declaration_join_v2_cbor_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.json": "//bridge/haskell:declaration_join_v2_json_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.cbor": "//bridge/haskell:declaration_inventory_v2_cbor_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.json": "//bridge/haskell:declaration_inventory_v2_json_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.cbor": "//bridge/haskell:declaration_join_v3_cbor_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.json": "//bridge/haskell:declaration_join_v3_json_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.cbor": "//bridge/haskell:declaration_join_typed_v3_cbor_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v3.json": "//bridge/haskell:declaration_join_typed_v3_json_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.cbor": "//bridge/haskell:declaration_inventory_v3_cbor_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.json": "//bridge/haskell:declaration_inventory_v3_json_fixture",
        "bridge/haskell/actors/Tidepool/Actors/Role.hs": "//bridge/haskell:actor_role_source",
        "exomonad/prompts/base.md": "//exomonad/prompts:base_prompt",
        "exomonad/prompts/api-guide.md": "//exomonad/prompts:api_guide_prompt",
        "exomonad/prompts/root.md": "//exomonad/prompts:root_prompt",
        "exomonad/prompts/recreated-root.md": "//exomonad/prompts:recreated_root_prompt",
        "exomonad/prompts/worktree-agent.md": "//exomonad/prompts:worktree_agent_prompt",
        "exomonad/prompts/readonly-agent.md": "//exomonad/prompts:readonly_agent_prompt",
        "exomonad/prompts/scaffolding-agent.md": "//exomonad/prompts:scaffolding_agent_prompt",
        "exomonad/prompts/integration-agent.md": "//exomonad/prompts:integration_agent_prompt",
        "exomonad/prompts/haskell-tool-description.md": "//exomonad/prompts:haskell_tool_description",
        "exomonad/prompts/haskell-tool-instructions.md": "//exomonad/prompts:haskell_tool_instructions",
        "exomonad/prompts/docs/actors.md": "//exomonad/prompts:doc_actors",
        "exomonad/prompts/docs/cleanup.md": "//exomonad/prompts:doc_cleanup",
        "exomonad/prompts/docs/deadline.md": "//exomonad/prompts:doc_deadline",
        "exomonad/prompts/docs/jev.md": "//exomonad/prompts:doc_jev",
        "exomonad/prompts/docs/lineage.md": "//exomonad/prompts:doc_lineage",
        "exomonad/prompts/docs/recovery.md": "//exomonad/prompts:doc_recovery",
        "exomonad/prompts/docs/refinement.md": "//exomonad/prompts:doc_refinement",
        "exomonad/prompts/docs/reflect.md": "//exomonad/prompts:doc_reflect",
        "exomonad/prompts/docs/request.md": "//exomonad/prompts:doc_request",
        "exomonad/prompts/docs/tree.md": "//exomonad/prompts:doc_tree",
        "exomonad/prompts/docs/unfold.md": "//exomonad/prompts:doc_unfold",
        "exomonad/prompts/docs/watch.md": "//exomonad/prompts:doc_watch",
        "exomonad/prompts/docs/workbench.md": "//exomonad/prompts:doc_workbench",
    }
    while pending:
        source = pending.pop()
        for relative in includes.findall(source.read_text()):
            included = (source.parent / relative).resolve()
            if (
                package["name"] == "tidepool"
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
            if repo_relative in LIBRARY_TEST_FIXTURES.get((package["name"], target["name"]), set()):
                continue
            if repo_relative not in external_labels:
                raise SystemExit(f"no Buck file input target for {repo_relative} (from {source})")
            external[external_labels[repo_relative]] = repo_relative
    mapped = {
        source.relative_to(directory).as_posix(): source.relative_to(ROOT).as_posix()
        for source in sources
    }
    mapped.update(external)
    return dict(sorted(mapped.items(), key=lambda item: item[1]))


def render_rule(rule, name, target, package, deps, named, extra="", features=()):
    crate_root = target["src_path"]
    src_root = pathlib.Path(crate_root).relative_to(ROOT).as_posix()
    target_edition = next(t["edition"] for t in package["targets"] if t["src_path"] == crate_root)
    source_group = name + "_sources"
    lines = [
        "rust_filegroup(",
        "    name = " + json.dumps(source_group) + ",",
        "    mapped_srcs = {",
    ]
    lines.extend(
        "        " + json.dumps(source) + ": " + json.dumps(mapped) + ","
        for source, mapped in source_inputs(package, target).items()
    )
    lines.extend([
        "    },",
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
    if extra:
        lines.append(extra)
    lines.extend(['    visibility = ["PUBLIC"],', ")", ""])
    return "\n".join(lines)


header = '''# @generated by scripts/buck2-first-party.py from Cargo metadata.
load("//build/rust:defs.bzl", "tidepool_rust_binary", "tidepool_rust_library", "tidepool_rust_test")
load("@prelude//:rules.bzl", "cxx_library", "export_file")
load("@prelude//rust:sources.bzl", "rust_filegroup")
'''

for package_name, package in local.items():
    if package_name not in selected:
        continue
    CURRENT_DIR = package_dir(package)
    package_features, enabled_dependencies, forwarded_features = feature_plan(package)
    has_custom_build = any("custom-build" in target["kind"] for target in package["targets"])
    if has_custom_build and package_name not in {"tidepool-codegen", "tidepool"}:
        raise SystemExit(f"{package_name} has a build.rs target; add a native Buck action before selecting it")
    rules = [header]
    if package_name == "tidepool-codegen":
        rules.append('load("//build/rust:codegen-md5.bzl", "tidepool_codegen_md5")\n')
    if package_name in ISOLATED_UNIT_TEST_PACKAGES:
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_isolated_test")\n')
    if package_name == "tidepool":
        rules.append('''load("//build/rust:buildscript.bzl", "tidepool_buildscript_run")
load("//build/rust:facade_build_inputs.bzl", "tidepool_facade_build_inputs")
''')
    normal_deps, normal_named = dependency_sets(package, enabled_dependencies, forwarded_features)
    if package_name == "tidepool-codegen":
        normal_named["prepared_md5_native"] = ":prepared_md5_native"
    dev_deps, dev_named = dependency_sets(package, enabled_dependencies, forwarded_features, include_dev=True) if package_name not in NORMAL_DEPENDENCY_ONLY_PACKAGES else ([], {})
    unit_deps, unit_named = dev_deps, dev_named
    if package_name == "tidepool-codegen":
        # Native engine unit tests use exactly the library's dependency surface.
        # Admit a new dev dependency explicitly before expanding this closure.
        if any(dependency["kind"] == "dev" for dependency in package["dependencies"]):
            raise SystemExit("Model codegen dev dependencies before extending its native unit target")
        unit_deps, unit_named = normal_deps, normal_named
    targets = package["targets"]
    if package_name == "tidepool":
        rules.append("""export_file(
    name = "facade_cargo_manifest",
    src = "Cargo.toml",
    visibility = ["PUBLIC"],
)
""")
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
        rules.append('''tidepool_facade_build_inputs(
    name = "tidepool_build_source_tree",
    cargo_manifest = "Cargo.toml",
    haskell_sources = "//bridge/haskell:facade_embedded_sources",
    workspace_sources = "//exomonad/examples/workspace:facade_scaffold_sources",
)

tidepool_buildscript_run(
    name = "tidepool_build_script_run",
    package_name = "tidepool",
    version = "''' + package["version"] + '''",
    buildscript_rule = ":tidepool_build_script",
    manifest_dir = ":tidepool_build_source_tree",
    env = {
        "TIDEPOOL_BUILD_SOURCE_ROOT": ".",
        "TIDEPOOL_EMBED_HASKELL": "1",
    },
)
''')
    libraries = [target for target in targets if "lib" in target["kind"] or "proc-macro" in target["kind"]]
    binaries = [target for target in targets if "bin" in target["kind"]]
    if package_name in LIBRARY_TARGET_ONLY_PACKAGES:
        binaries = []
    tests = [target for target in targets if "test" in target["kind"]]
    for target in libraries:
        extra = "    proc_macro = True," if "proc-macro" in target["kind"] else ""
        if package_name == "tidepool":
            extra = '    env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},'
        rule = render_rule("tidepool_rust_library", target["name"], target, package, normal_deps, normal_named, extra, features=package_features)
        rules.append(rule)
    if package_name == "tidepool-codegen":
        rules.append("tidepool_codegen_md5()\n")
    for target in binaries:
        deps = normal_deps + ([":" + libraries[0]["name"]] if libraries else [])
        binary_name = target["name"] + "_bin" if libraries and target["name"] == libraries[0]["name"] else target["name"]
        extra = ""
        if package_name == "tidepool":
            extra = '    env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},'
        rules.append(render_rule("tidepool_rust_binary", binary_name, target, package, deps, normal_named, extra, features=package_features))
    if libraries and package_name in UNIT_TEST_PACKAGES:
        library = libraries[0]
        unit_target = dict(library)
        unit_target["name"] = library["name"] + "_unit_tests"
        unit_rule = (
            "tidepool_rust_isolated_test"
            if package_name in ISOLATED_UNIT_TEST_PACKAGES
            else "tidepool_rust_test"
        )
        unit_extra = ""
        if package_name == "tidepool-toolchain":
            unit_extra = (
                '    env = {"TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)", '
                '"TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)"},\n'
                "    haskell_worker = True,"
            )
        rules.append(render_rule(unit_rule, unit_target["name"], unit_target, package, unit_deps, unit_named, unit_extra, features=package_features))
    selected_tests = [
        target for target in tests
        if (
            package_name not in EXTRACTOR_FREE_ONLY_PACKAGES
            and package_name not in NORMAL_DEPENDENCY_ONLY_PACKAGES
        ) or (package_name == "tidepool-codegen" and target["name"] in CODEGEN_NATIVE_TESTS)
    ]
    for target in selected_tests:
        deps = dev_deps + ([":" + libraries[0]["name"]] if libraries else [])
        extra = ""
        if package["name"] == "tidepool-atomic-write" and target["name"] == "strict_directory":
            extra = '    env = {"TIDEPOOL_DIRECTORY_FAULT_LIBRARY": "$(location :directory_fault_shared)"},'
        if package["name"] == "tidepool-repr" and target["name"] == "repr":
            extra = '    env = {"TIDEPOOL_DIRECTORY_FAULT_LIBRARY": "$(location //bridge/atomic-write:directory_fault_shared)"},'
        rules.append(
            render_rule(
                "tidepool_rust_test",
                target["name"],
                target,
                package,
                deps,
                dev_named,
                extra,
                package_features,
            )
        )
    if package["name"] == "tidepool-atomic-write":
        rules.append('''export_file(
    name = "directory_fault_fixture",
    src = "tests/fixtures/directory_fault.c",
    visibility = ["PUBLIC"],
)

cxx_library(
    name = "directory_fault_shared",
    srcs = ["tests/fixtures/directory_fault.c"],
    preferred_linkage = "shared",
    linker_flags = ["-ldl"],
    visibility = ["PUBLIC"],
)
''')
    output = "\n".join(rules)
    output_path = ROOT / CURRENT_DIR / "BUCK"
    if options.check:
        if not output_path.is_file() or output_path.read_text() != output:
            print(f"stale or missing Buck target graph: {output_path.relative_to(ROOT)}", file=sys.stderr)
            sys.exit(1)
    else:
        output_path.write_text(output)
