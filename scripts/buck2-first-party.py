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
    NATIVE_PACKAGES,
    reject_local_feature_requests,
    FeatureSelectionError,
    effective_feature_selection,
    metadata_feature_args,
    parse_locked_git_source,
    reject_forbidden_closure,
    resolve as resolve_cargo_features,
)
from test_source_ownership import TEST_ONLY_SOURCES, integration_target_sources


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
if selected & {"tidepool", "tidepool-runtime"}:
    selected.add("tidepool-testing")
SUPPORTED_PACKAGES = NATIVE_PACKAGES
# Every executable suite uses the existing counted, process-isolated runner.
# Shared harness owners publish their named execution groups separately.
SHARED_UNIT_TEST_PACKAGES = {"tidepool", "tidepool-runtime", "exomonad-actor"}
GENERATED_BIN_NAMES = {
    "tidepool-protocol-gen": "tidepool_protocol_gen",
    "tidepool-effects-gen": "tidepool_effects_gen",
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
    # Keep test-only compiler sources out of the runtime library action.
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


def generated_producer_rules(package_name):
    if package_name == "tidepool-protocol":
        paths = protocol_output_paths()
        name, generator = "generated", ":tidepool_protocol_gen"
    elif package_name == "tidepool-mcp":
        paths = ["Tidepool/Effects/Core.hs", "Tidepool/Effects/Authored.hs", "Tidepool/Effects.hs"]
        name, generator = "effects_generated", ":tidepool_effects_gen"
    else:
        return ""
    return "\n".join([
        "protocol_generated(", f"    name = {json.dumps(name)},",
        f"    generator = {json.dumps(generator)},", "    outputs = [",
        render_strings(paths, 8), "    ],", '    visibility = ["PUBLIC"],', ")", "",
    ])


def source_inputs(package, target, features=(), test_target=False):
    directory = ROOT / CURRENT_DIR
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
        facade_unit_test = package["name"] == "tidepool" and target["name"].endswith("_unit_tests")
        if package["name"] == "tidepool" and not facade_unit_test:
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
    includes = re.compile(r'include_(?:str|bytes)!\s*\(\s*"([^\"]+)"')
    pending = [path for path in sources if path.suffix == ".rs"]
    external_labels = {
        "bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor": "//bridge/haskell:m3_vertical_fixture",
        "bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor": "//bridge/haskell:schema6_intrinsic_fixture",
        "bridge/atomic-write/tests/fixtures/directory_fault.c": "//bridge/atomic-write:directory_fault_fixture",
        "tidepool/runtime/src/session/fixtures/activation-input-function.hs": "//tidepool/runtime:activation_input_function_fixture",
        "tidepool/runtime/src/session/fixtures/activation-input-receiver.hs": "//tidepool/runtime:activation_input_receiver_fixture",
        "tidepool/runtime/src/session/fixtures/activation-input-resident-original.hs": "//tidepool/runtime:activation_input_resident_original_fixture",
        "tidepool/runtime/src/session/fixtures/activation-input-resident-request.hs": "//tidepool/runtime:activation_input_resident_request_fixture",
        "tidepool/runtime/src/session/fixtures/activation-input-resident-shadow.hs": "//tidepool/runtime:activation_input_resident_shadow_fixture",
        "bridge/haskell/test-prepared-stg/fixtures/freer-resume.cbor": "//bridge/haskell:freer_resume_fixture",
        "bridge/haskell/test-prepared-stg/fixtures/freer-retention.cbor": "//bridge/haskell:freer_retention_fixture",
        "bridge/haskell/src/Tidepool/ExtractRequest.hs": "//bridge/haskell:extract_request_source",
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
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.cbor": "//bridge/haskell:declaration_inventory_v3_cbor_fixture",
        "bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.json": "//bridge/haskell:declaration_inventory_v3_json_fixture",
        "bridge/haskell/actors/Tidepool/Actors/Role.hs": "//bridge/haskell:actor_role_source",
        "bridge/haskell/examples/model-turns/ContextWorkflow.hs": "//bridge/haskell:context_workflow_example",
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
        ".exomonad/workspace/skills/exomonad-jev/SKILL.md": "//:facade_doc__exomonad_workspace_skills_exomonad_jev_SKILL_md",
        ".exomonad/workspace/skills/exomonad-jev/references/recent-changes.md": "//:facade_doc__exomonad_workspace_skills_exomonad_jev_references_recent_changes_md",
        ".exomonad/workspace/skills/exomonad-workbench/SKILL.md": "//:facade_doc__exomonad_workspace_skills_exomonad_workbench_SKILL_md",
        "exomonad/examples/workspace/.exomonad/prompts/task.md": "//exomonad/examples/workspace:facade_task_prompt",
        "exomonad/examples/workspace/.exomonad/skills/exomonad-workbench/SKILL.md": "//exomonad/examples/workspace:facade_workbench_skill",
    }
    while pending:
        source = pending.pop()
        contents = source.read_text()
        relatives = list(includes.findall(contents))
        for relative in relatives:
            included = (source.parent / relative).resolve()
            if included.is_relative_to(ROOT) and included.relative_to(ROOT).as_posix() in test_only:
                continue
            if (
                package["name"] == "tidepool"
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
    # Schema output paths define the source closure, including outputs that do
    # not yet exist in the checkout. Checked-in generated copies are not inputs
    # to native Rust compilation.
    if package["name"] in {
        "tidepool-mcp", "tidepool-handlers", "tidepool-bridge-effects",
        "tidepool-runtime", "exomonad-actor", "tidepool",
    }:
        for path in protocol_output_paths():
            if path.startswith(CURRENT_DIR + "/src/generated/") and path.endswith(".rs"):
                mapped.pop(path.removeprefix(CURRENT_DIR + "/"), None)
                mapped[generated_protocol_label(path)] = path
    return dict(sorted(mapped.items(), key=lambda item: item[1]))


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
    lines.extend(
        "        " + json.dumps(source) + ": " + json.dumps(mapped) + ","
        for source, mapped in source_inputs(
            package, target, features,
            test_target=test_target or rule in ("tidepool_rust_test", "tidepool_rust_isolated_test"),
        ).items()
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


def compile_fail_cases(package, library):
    directory = pathlib.Path(package["manifest_path"]).parent
    extra_dependencies = {
        "machine_lease_double_borrow": (),
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


def actor_observation_test_cases(binary):
    tests = ["runtime_observation::provider_health_tests::" + name for name in (
        "owned_provider_round_authorizes_idle_only_after_success",
        "abandoned_provider_round_requires_attention_until_a_new_success",
        "obsolete_provider_round_cannot_overwrite_newer_observation",
    )]
    return "\n".join([
        "tidepool_rust_test_cases(",
        '    name = "actor_runtime_observation_lease_tests",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(tests, 8), "    ],",
        "    expected_count = 3,", "    jobs = 1,", "    timeout = 30,",
        "    test_rule_timeout_ms = 150000,", '    visibility = ["PUBLIC"],', ")", "",
    ])


def runtime_test_cases(binary):
    """Admission checks use native machines but do not start a compiler worker."""
    tests = ["session::admission::tests::" + name for name in (
        "initial_interface_inventory_refuses_missing_extra_duplicate_and_changed_bytes",
        "native_admission_uses_exact_scoped_roots_and_keeps_its_original_ledger",
        "native_admission_commits_the_live_machine_export_owner",
    )]
    rules = ["\n".join([
        "tidepool_rust_test_cases(",
        '    name = "runtime_admission_tests",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(tests, 8), "    ],",
        "    expected_count = 3,", "    jobs = 1,", "    timeout = 30,",
        "    test_rule_timeout_ms = 150000,", '    visibility = ["PUBLIC"],', ")", "",
    ])]
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
            "    env = {",
            '        "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",',
            '        "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",',
            '        "TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)",',
            '        "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",',
            '        "TIDEPOOL_KEEP_TEST_LOGS": "1",',
            "    },", "    haskell_worker = True,",
            '    resources = ["//bridge/haskell:facade_embedded_sources", "//build/package:compiler_deployment"],',
            '    visibility = ["PUBLIC"],', ")", "",
        ]))
    fixture_tests = ["session::turn::tests::" + name for name in (
        "compiled_cell_fixture_rejects_a_missing_worker",
        "whole_cell_check_harvests_downstream_fixed_local_type",
        "whole_cell_check_harvests_same_cell_nominal_type",
        "checked_handler_pin_carries_qualified_type_imports",
        "whole_cell_check_reports_missing_record_fields_without_rejecting_declaration",
        "checked_expression_plans_cover_pure_and_effectful_values",
        "a_final_pure_cell_is_accepted_as_effectful",
        "a_genuinely_pure_final_expression_still_takes_the_pure_path",
    )]
    rules.append("\n".join([
        "tidepool_rust_test_cases(",
        '    name = "runtime_compiled_cell_fixture_test",',
        f"    binary = {json.dumps(':' + binary)},",
        "    exact_tests = [", render_strings(fixture_tests, 8), "    ],",
        f"    expected_count = {len(fixture_tests)},", "    jobs = 1,", "    timeout = 600,",
        "    test_rule_timeout_ms = 660000,", "    env = {",
        '        "TIDEPOOL_CELL_TEST_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",',
        '        "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",',
        '        "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",',
        '        "TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)",',
        '        "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",',
        '        "TIDEPOOL_KEEP_TEST_LOGS": "1",',
        "    },", "    haskell_worker = True,", "    resources = [",
        '        "//bridge/haskell:facade_embedded_sources",',
        '        "//build/package:compiler_deployment",',
        "    ],", '    visibility = ["PUBLIC"],', ")", "",
    ]))
    recovery_prefix = "session::recovery::newrecovery_v2::tests::"
    recovery_graph_tests = [recovery_prefix + name for name in (
        "structural_graph_records_do_not_grant_recovery_authority",
        "persistent_snapshots_share_history_payloads_across_publication_candidates",
        "wire_admission_preserves_original_revision_and_refuses_duplicate_rows",
        "noncanonical_value_requirements_preserve_graph_admission_and_candidates",
        "producer_mismatch_tombstones_the_original_without_resurrection",
        "interface_recovery_retains_native_requirements_and_selected_evidence",
        "v6_manifest_refuses_persisted_native_relation_rows",
        "binding_only_publication_is_durable_and_restarts_as_a_winner_tombstone",
        "same_session_actor_surfaces_keep_independent_winners_and_epochs",
        "sibling_actors_project_their_own_declaration_roots",
        "duplicate_or_invalid_actor_surfaces_are_refused",
        "old_recovery_formats_are_refused_without_rewriting_bytes",
        "native_recovery_requires_the_exact_canonical_companion_and_selection",
        "all_recovery_witness_paths_are_confined_before_hydration",
        "canonical_witness_losses_keep_the_exact_component_and_relative_path",
        "canonical_interface_identity_ignores_all_materialization_paths",
        "artifact_ids_ignore_materialization_paths",
        "value_interface_artifact_id_and_exact_requirements_are_validated",
        "future_recovery_format_is_distinctly_refused_without_rewriting_bytes",
        "family_only_inventory_retains_hidden_consistency_evidence",
        "selected_family_and_associated_axioms_require_exact_inventory_membership",
        "lexical_graph_requires_closed_reachable_artifact_owned_identities",
        "v6_checksum_covers_lexical_roots_and_edges",
        "checksum_covers_the_graph_and_projection_preserves_lost_winner_tombstones",
        "checksum_matches_the_canonical_unsigned_graph_encoding",
        "artifact_paths_must_stay_relative_to_the_recovery_root",
        "reservations_advance_exactly_one_generation_without_publishing_a_node",
        "staged_high_water_is_invisible_until_publish_and_survives_readback",
        "pre_rename_failure_keeps_target_unpublished",
        "missing_replacement_tombstone_retracts_old_original_identity",
        "workbench_imports_fold_in_lexical_order_and_keep_exact_specs",
    )]
    recovery_compiler_tests = [recovery_prefix + name for name in (
        "private_only_exact_nodes_survive_restart_without_lexical_visibility",
        "private_recovery_refuses_live_marker_not_in_verified_native_inventory",
        "private_recovery_validates_authentic_native_markers_and_later_tamper",
        "published_exact_root_requires_its_run_owner_on_restart",
        "private_recovery_attach_refuses_corrupt_owned_artifact",
        "canonical_interface_closure_recovers_without_a_native_product",
        "artifact_validation_shares_real_package_bytes_and_rechecks_each_read",
        "staged_publication_work_counts_actual_checksum_hash_and_write_bytes",
        "artifact_bytes_are_checked_against_the_manifest_digests",
        "a_missing_winning_artifact_becomes_a_tombstone_without_resurrection",
        "restart_read_keeps_missing_winning_artifact_as_tombstone",
    )]
    for name, tests, compiler in (
        ("runtime_recovery_graph_tests", recovery_graph_tests, False),
        ("runtime_recovery_compiler_tests", recovery_compiler_tests, True),
    ):
        lines = [
            "tidepool_rust_test_cases(", f"    name = {json.dumps(name)},",
            f"    binary = {json.dumps(':' + binary)},",
            "    exact_tests = [", render_strings(tests, 8), "    ],",
            f"    expected_count = {len(tests)},", "    jobs = 1,",
            f"    timeout = {600 if compiler else 30},",
            f"    test_rule_timeout_ms = {660000 if compiler else 150000},",
        ]
        if compiler:
            lines.extend([
                "    env = {",
                '        "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",',
                '        "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",',
                '        "TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)",',
                '        "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",',
                '        "TIDEPOOL_KEEP_TEST_LOGS": "1",',
                "    },", "    haskell_worker = True,",
                '    resources = ["//bridge/haskell:facade_embedded_sources", "//build/package:compiler_deployment"],',
            ])
        lines.extend(['    visibility = ["PUBLIC"],', ")", ""])
        rules.append("\n".join(lines))
    return "\n".join(rules)


def facade_test_cases(binary):
    """Share one linked test harness while declaring inputs per execution group."""
    process_env = {
        "TIDEPOOL_TEST_BASH": "$(exe toolchains//:bash)",
        "TIDEPOOL_TEST_SLEEP": "$(exe toolchains//:sleep)",
    }
    host_env = {
        **process_env,
        "EXOMONAD_EMBEDDED_ASSET_ROOT": "$(location //web:dist)/web",
        "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",
        "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",
        "TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)",
        "TIDEPOOL_PRELUDE_DIR": "$(location //bridge/haskell:facade_embedded_sources)/lib",
        "TIDEPOOL_KEEP_TEST_LOGS": "1",
    }
    browser_env = {
        **host_env,
        "TIDEPOOL_BROWSER_DRIVER": "$(location //build/testing/browser:driver_bundle)/driver.mjs",
        "TIDEPOOL_BROWSER_NODE": "$(exe toolchains//:browser_node)",
        "PLAYWRIGHT_BROWSERS_PATH": "$(location toolchains//:playwright_browsers)",
    }
    process_resources = ["toolchains//:test_tools_closure"]
    host_resources = process_resources + [
        "//web:dist",
        "//bridge/haskell:facade_embedded_sources",
        "//build/package:compiler_deployment",
    ]
    browser_resources = host_resources + [
        "//build/testing/browser:driver_bundle",
        "toolchains//:browser_test_closure",
        "toolchains//:playwright_browsers",
    ]
    prefix = "actor_host::m1_host_tests::"
    groups = [
        ("facade_process_tests", [prefix + "browser_process::tests::" + name for name in (
            "oversized_unterminated_frame_is_rejected_before_eof",
            "complete_frames_and_eof_are_observed_without_replay",
            "eof_with_partial_frame_is_a_protocol_failure",
            "cancelled_read_retains_partial_frame_for_next_select",
            "deadline_stops_descendant_and_drains_bounded_redacted_stderr",
            "successful_leader_exit_also_stops_pipe_holding_descendant",
        )], process_env, process_resources, False, False, 30),
        ("facade_host_tests", [prefix + name for name in (
            "production_host_retains_http_haskell_commands_and_reconnects_without_replay",
            "host_cancellation_stops_a_real_running_haskell_cell",
            "production_host_marks_embedded_root_ready_and_retires_invalid_auth_failure",
        )], host_env, host_resources, True, False, 600),
        ("facade_host_raw_test", [prefix +
            "production_host_retains_http_haskell_commands_and_reconnects_without_replay"],
         host_env, host_resources, True, False, 600),
        ("facade_late_output_test", [prefix +
            "real_host_late_output_tests::real_host_retains_one_late_haskell_output_across_compaction"],
         host_env, host_resources, True, False, 600),
        ("facade_browser_test", [prefix +
            "production_browser_executes_resident_haskell_retries_and_controls_root"],
         browser_env, browser_resources, True, True, 900),
        ("tidepool_unit_tests_all", [], browser_env, browser_resources, True, False, 600),
    ]
    rules = []
    for name, tests, env, resources, worker, ignored, timeout in groups:
        lines = ["tidepool_rust_test_cases(", f"    name = {json.dumps(name)},",
                 f"    binary = {json.dumps(':' + binary)},"]
        if tests:
            lines.extend(["    exact_tests = [", render_strings(tests, 8), "    ],",
                          f"    expected_count = {len(tests)},"])
        lines.extend([f"    ignored = {ignored},", "    jobs = 1,",
                      f"    timeout = {timeout},",
                      f"    test_rule_timeout_ms = {(len(tests) * timeout + 60) * 1000 if tests else 7_200_000},",
                      "    env = {"])
        lines.extend(f"        {json.dumps(key)}: {json.dumps(value)}," for key, value in env.items())
        lines.extend(["    },", "    resources = [", render_strings(resources, 8), "    ],",
                      f"    haskell_worker = {worker},", '    visibility = ["PUBLIC"],', ")", ""])
        rules.append("\n".join(lines))
    return "\n".join(rules)


header = '''# @generated by scripts/buck2-first-party.py from Cargo metadata.
load("//build/rust:defs.bzl", "tidepool_rust_binary", "tidepool_rust_library", "tidepool_rust_test")
load("@prelude//:rules.bzl", "cxx_library", "export_file", "filegroup")
load("@prelude//rust:sources.bzl", "rust_filegroup")
'''

outputs = {}
for package_name, package in local.items():
    if package_name not in selected:
        continue
    CURRENT_DIR = package_dir(package)
    package_features, enabled_dependencies, forwarded_features = feature_plan(package)
    has_custom_build = any("custom-build" in target["kind"] for target in package["targets"])
    if has_custom_build and package_name not in {"tidepool-codegen", "tidepool"}:
        raise SystemExit(f"{package_name} has a build.rs target; add a native Buck action before selecting it")
    rules = [header]
    if package_name in {"tidepool-protocol", "tidepool-mcp"}:
        rules.append('load("//build/protocol:defs.bzl", "protocol_generated")\n')
    if package_name == "tidepool-codegen":
        rules.append('load("//build/rust:codegen-md5.bzl", "tidepool_codegen_md5")\n')
    rules.append('load("//build/rust:defs.bzl", "tidepool_rust_isolated_test")\n')
    if package_name == "exomonad-actor":
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_test_cases")\n')
    if package_name == "tidepool-runtime":
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_test_cases")\n')
        rules.append('load("//build/rust:compile_fail.bzl", "rust_compile_fail")\n')
    if package_name == "tidepool":
        rules.append('''load("//build/rust:buildscript.bzl", "tidepool_buildscript_run")
load("//build/rust:facade_build_inputs.bzl", "tidepool_facade_build_inputs")
load("//build/rust:defs.bzl", "tidepool_rust_test_cases")
''')
    normal_deps, normal_named = dependency_sets(package, enabled_dependencies, forwarded_features)
    if package_name == "tidepool-codegen":
        normal_named["prepared_md5_native"] = ":prepared_md5_native"
    dev_deps, dev_named = dependency_sets(package, enabled_dependencies, forwarded_features, include_dev=True)
    unit_deps, unit_named = dev_deps, dev_named
    if package_name == "tidepool":
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
        binary_name = target["name"] + "_bin" if libraries and target["name"] == libraries[0]["name"] else GENERATED_BIN_NAMES.get(target["name"], target["name"])
        extra = ""
        if package_name == "tidepool":
            extra = '    env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},'
        rules.append(render_rule("tidepool_rust_binary", binary_name, target, package, deps, normal_named, extra, features=package_features))
    rules.append(generated_producer_rules(package_name))
    if libraries and libraries[0].get("test", True):
        library = libraries[0]
        unit_target = dict(library)
        unit_target["name"] = library["name"] + "_unit_tests"
        unit_rule = "tidepool_rust_isolated_test"
        unit_extra = ""
        if package_name == "tidepool-toolchain":
            unit_extra = (
                '    env = {"TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)", '
                '"TIDEPOOL_EXTRACT_WORKER": "$(exe //bridge/haskell:tidepool_extract_bin)"},\n'
                "    haskell_worker = True,"
            )
        if package_name == "tidepool":
            unit_rule = "tidepool_rust_binary"
            unit_extra = '''    env = {"OUT_DIR": "$(location :tidepool_build_script_run[out_dir])"},
    rustc_flags = ["--test"],'''
        if package_name in {"tidepool-runtime", "exomonad-actor"}:
            unit_rule = "tidepool_rust_binary"
            unit_extra = '    rustc_flags = ["--test"],'
        rules.append(render_rule(unit_rule, unit_target["name"], unit_target, package, unit_deps, unit_named, unit_extra, features=package_features, test_target=True))
        if package_name == "tidepool":
            rules.append(facade_test_cases(unit_target["name"]))
        if package_name == "tidepool-runtime":
            rules.append(runtime_test_cases(unit_target["name"]))
        if package_name == "exomonad-actor":
            rules.append(actor_observation_test_cases(unit_target["name"]))
    selected_tests = [target for target in tests if not (
        package_name == "tidepool-runtime" and target["name"] == "compile_fail"
    )]
    if package_name == "tidepool-runtime" and libraries:
        rules.append(compile_fail_cases(package, libraries[0]))
    for target in selected_tests:
        deps = dev_deps + ([":" + libraries[0]["name"]] if libraries else [])
        extra = ""
        if package["name"] == "tidepool-atomic-write" and target["name"] == "strict_directory":
            extra = '    env = {"TIDEPOOL_DIRECTORY_FAULT_LIBRARY": "$(location :directory_fault_shared)"},'
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
    if package_name == "tidepool":
        rules.append('''filegroup(
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
    srcs = ["tests/fixtures/directory_fault.c"],
    preferred_linkage = "shared",
    linker_flags = ["-ldl"],
    visibility = ["PUBLIC"],
)
''')
    output = "\n".join(rules)
    output_path = ROOT / CURRENT_DIR / "BUCK"
    outputs[output_path] = output

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
