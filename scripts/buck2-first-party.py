#!/usr/bin/env python3
"""Regenerate native Buck Rust targets from the locked Cargo workspace metadata."""

import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
metadata = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", "x86_64-unknown-linux-gnu"],
        cwd=ROOT,
    )
)
packages = {package["name"]: package for package in metadata["packages"]}
local = {
    name: package
    for name, package in packages.items()
    if package["id"] in metadata["workspace_members"]
}
arguments = argparse.ArgumentParser(description=__doc__)
arguments.add_argument("--package", action="append", required=True, help="Cargo package to generate (repeatable)")
arguments.add_argument("--check", action="store_true", help="check generated BUCK files without rewriting them")
options = arguments.parse_args()
selected = set(options.package)
unknown = selected - set(local)
if unknown:
    raise SystemExit(f"unknown Cargo workspace package(s): {', '.join(sorted(unknown))}")
SUPPORTED_PACKAGES = {"tidepool-atomic-write", "tidepool-repr", "tidepool-heap", "tidepool-bignum", "tidepool-bridge", "tidepool-effect", "tidepool-codegen"}
NORMAL_DEPENDENCY_ONLY_PACKAGES = {"tidepool-bignum", "tidepool-bridge", "tidepool-effect", "tidepool-codegen"}
UNIT_TEST_PACKAGES = {"tidepool-atomic-write", "tidepool-repr", "tidepool-heap", "tidepool-codegen"}
unsupported = selected - SUPPORTED_PACKAGES
if unsupported:
    raise SystemExit(
        "native Buck generation is currently supported only for "
        f"{', '.join(sorted(SUPPORTED_PACKAGES))}; unsupported: "
        f"{', '.join(sorted(unsupported))}"
    )


def package_dir(package):
    return pathlib.Path(package["manifest_path"]).parent.relative_to(ROOT).as_posix()


def target_name(package, kind):
    return next(
        target["name"]
        for target in package["targets"]
        if kind in target["kind"] or (kind == "lib" and "proc-macro" in target["kind"])
    )


def dependency_label(dependency):
    name = dependency["name"]
    if name in local:
        target = target_name(local[name], "lib")
        directory = package_dir(local[name])
        return (":" if directory == CURRENT_DIR else "//" + directory + ":") + target
    if name == "sha2":
        # Two incompatible direct versions occur in this workspace.
        major_minor = dependency["req"].lstrip("^=~").split(".")[:2]
        return "//third-party/rust:sha2-" + ".".join(major_minor)
    return "//third-party/rust:" + name


resolved_nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}


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
    return True


def dependency_sets(package, include_dev=False):
    deps = []
    named = {}
    for dependency in package["dependencies"]:
        if dependency["kind"] == "build":
            if package["name"] != "tidepool-codegen" or dependency["name"] != "cc" or dependency["rename"] is not None or dependency["target"] is not None:
                raise SystemExit(f"Unmodeled build dependency in {package['name']}: {dependency['name']}")
            continue
        if dependency["kind"] == "dev" and not include_dev:
            continue
        linux_dependency(package, dependency)
        label = dependency_label(dependency)
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
    }
    while pending:
        source = pending.pop()
        for relative in includes.findall(source.read_text()):
            included = (source.parent / relative).resolve()
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
            if package["name"] == "tidepool-codegen" and target["name"] == "tidepool_codegen" and repo_relative in CODEGEN_TEST_FIXTURES:
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


def render_rule(rule, name, target, package, deps, named, extra=""):
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
    features = package["features"]
    if features and features != {"default": []}:
        raise SystemExit(f"{package_name} declares unsupported Cargo features: {features}")
    if any("custom-build" in target["kind"] for target in package["targets"]) and package_name != "tidepool-codegen":
        raise SystemExit(f"{package_name} has a build.rs target; add a native Buck action before selecting it")
    rules = [header]
    if package_name == "tidepool-codegen":
        rules.append('load("//build/rust:codegen-md5.bzl", "tidepool_codegen_md5")\n')
        rules.append('load("//build/rust:defs.bzl", "tidepool_rust_isolated_test")\n')
    normal_deps, normal_named = dependency_sets(package)
    if package_name == "tidepool-codegen":
        normal_named["prepared_md5_native"] = ":prepared_md5_native"
    dev_deps, dev_named = dependency_sets(package, include_dev=True) if package_name not in NORMAL_DEPENDENCY_ONLY_PACKAGES else ([], {})
    unit_deps, unit_named = dev_deps, dev_named
    if package_name == "tidepool-codegen":
        # Native engine unit tests use exactly the library's dependency surface.
        # Admit a new dev dependency explicitly before expanding this closure.
        if any(dependency["kind"] == "dev" for dependency in package["dependencies"]):
            raise SystemExit("Model codegen dev dependencies before extending its native unit target")
        unit_deps, unit_named = normal_deps, normal_named
    targets = package["targets"]
    libraries = [target for target in targets if "lib" in target["kind"] or "proc-macro" in target["kind"]]
    binaries = [target for target in targets if "bin" in target["kind"]]
    tests = [target for target in targets if "test" in target["kind"]]
    for target in libraries:
        extra = "    proc_macro = True," if "proc-macro" in target["kind"] else ""
        rule = render_rule("tidepool_rust_library", target["name"], target, package, normal_deps, normal_named, extra)
        rules.append(rule)
    if package_name == "tidepool-codegen":
        rules.append("tidepool_codegen_md5()\n")
    for target in binaries:
        deps = normal_deps + ([":" + libraries[0]["name"]] if libraries else [])
        binary_name = target["name"] + "_bin" if libraries and target["name"] == libraries[0]["name"] else target["name"]
        rules.append(render_rule("tidepool_rust_binary", binary_name, target, package, deps, normal_named))
    if libraries and package_name in UNIT_TEST_PACKAGES:
        library = libraries[0]
        unit_target = dict(library)
        unit_target["name"] = library["name"] + "_unit_tests"
        unit_rule = "tidepool_rust_isolated_test" if package_name == "tidepool-codegen" else "tidepool_rust_test"
        rules.append(render_rule(unit_rule, unit_target["name"], unit_target, package, unit_deps, unit_named))
    selected_tests = [
        target for target in tests
        if package_name not in NORMAL_DEPENDENCY_ONLY_PACKAGES
        or (package_name == "tidepool-codegen" and target["name"] in CODEGEN_NATIVE_TESTS)
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
