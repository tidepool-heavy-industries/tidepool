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
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
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
if any(name.startswith("codex-") for name in selected):
    raise SystemExit("Codex is outside the Buck migration")
unknown = selected - set(local)
if unknown:
    raise SystemExit(f"unknown Cargo workspace package(s): {', '.join(sorted(unknown))}")


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
        if name == "codex-shoal-protocol":
            return "//:codex_shoal_protocol"
        target = target_name(local[name], "lib")
        directory = package_dir(local[name])
        return (":" if directory == CURRENT_DIR else "//" + directory + ":") + target
    if name == "sha2":
        # Two incompatible direct versions occur in this workspace.
        major_minor = dependency["req"].lstrip("^=~").split(".")[:2]
        return "//third-party/rust:sha2-" + ".".join(major_minor)
    return "//third-party/rust:" + name


def linux_dependency(dependency):
    target = dependency["target"] or ""
    return not any(
        excluded in target
        for excluded in ("windows", "macos", "ios", "wasm32", "aarch64")
    )


def dependency_sets(package, include_dev=False):
    deps = []
    named = {}
    for dependency in package["dependencies"]:
        if dependency["kind"] == "build" or not linux_dependency(dependency):
            continue
        if dependency["kind"] == "dev" and not include_dev:
            continue
        label = dependency_label(dependency)
        if dependency["rename"]:
            named[dependency["rename"].replace("-", "_")] = label
        elif label not in deps:
            deps.append(label)
    return sorted(deps), dict(sorted(named.items()))


def render_strings(values, indent=4):
    return "\n".join(" " * indent + json.dumps(value) + "," for value in values)


def source_inputs(package, target):
    directory = ROOT / CURRENT_DIR
    source_root = pathlib.Path(target["src_path"]).resolve()
    sources = {directory / "Cargo.toml"}
    labels = set()
    if source_root.is_relative_to(directory / "src"):
        sources.update((directory / "src").rglob("*.rs"))
    else:
        # Integration tests are one native rust_test action. Track only that
        # package's Rust tests plus literal include_* resources they consume.
        sources.update((directory / "tests").rglob("*.rs"))
    includes = re.compile(r'include_(?:str|bytes)!\s*\(\s*"([^"]+)"')
    pending = [path for path in sources if path.suffix == ".rs"]
    external_labels = {
        "bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor": "//bridge/haskell:m3_vertical_fixture",
        "bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor": "//bridge/haskell:schema6_intrinsic_fixture",
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
            repo_relative = included.relative_to(ROOT).as_posix()
            if repo_relative == "bridge/atomic-write/tests/fixtures/directory_fault.c":
                external_labels[repo_relative] = "//bridge/atomic-write:directory_fault_fixture"
            if repo_relative not in external_labels:
                raise SystemExit(f"no Buck file input target for {repo_relative} (from {source})")
            labels.add(external_labels[repo_relative])
    local_sources = sorted(
        source.relative_to(directory).as_posix()
        for source in sources
        if source.is_relative_to(directory)
    )
    return local_sources + sorted(labels)


def render_rule(rule, name, target, package, deps, named, extra=""):
    crate_root = target["src_path"]
    src_root = pathlib.Path(crate_root).relative_to(ROOT / CURRENT_DIR).as_posix()
    target_edition = next(t["edition"] for t in package["targets"] if t["src_path"] == crate_root)
    lines = [
        rule + "(",
        "    name = " + json.dumps(name) + ",",
        "    package_name = " + json.dumps(package["name"]) + ",",
        "    package_dir = " + json.dumps(CURRENT_DIR) + ",",
        "    version = " + json.dumps(package["version"]) + ",",
        "    crate_root = " + json.dumps(src_root) + ",",
        '    edition = "' + target_edition + '",',
        "    srcs = [",
        render_strings(source_inputs(package, target), 8),
        "    ],",
    ]
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
load("@prelude//:rules.bzl", "cxx_library")
'''

for package_name, package in local.items():
    if package_name not in selected:
        continue
    CURRENT_DIR = package_dir(package)
    if CURRENT_DIR.startswith("vendor/"):
        continue  # The pinned Codex submodule is mapped from root//:codex_shoal_protocol.
    if any("custom-build" in target["kind"] for target in package["targets"]):
        raise SystemExit(f"{package_name} has a build.rs target; add a native Buck action before selecting it")
    rules = [header]
    normal_deps, normal_named = dependency_sets(package)
    dev_deps, dev_named = dependency_sets(package, include_dev=True)
    if package["name"] == "tidepool-codegen":
        normal_deps.append(":prepared_md5")
        dev_deps.append(":prepared_md5")
    targets = package["targets"]
    libraries = [target for target in targets if "lib" in target["kind"] or "proc-macro" in target["kind"]]
    binaries = [target for target in targets if "bin" in target["kind"]]
    tests = [target for target in targets if "test" in target["kind"]]
    for target in libraries:
        extra = "    proc_macro = True," if "proc-macro" in target["kind"] else ""
        rule = render_rule("tidepool_rust_library", target["name"], target, package, normal_deps, normal_named, extra)
        if package["name"] == "tidepool":
            rule = rule.replace(
                '    ],\n    deps = [',
                '        ":facade_generated",\n    ],\n    deps = [',
            ).replace(
                '    visibility = ["PUBLIC"],',
                '    env = {"OUT_DIR": "$(location :facade_generated)"},\n    visibility = ["PUBLIC"],',
            )
        rules.append(rule)
    for target in binaries:
        deps = normal_deps + ([":" + libraries[0]["name"]] if libraries else [])
        binary_name = target["name"] + "_bin" if libraries and target["name"] == libraries[0]["name"] else target["name"]
        rules.append(render_rule("tidepool_rust_binary", binary_name, target, package, deps, normal_named))
    if libraries:
        library = libraries[0]
        unit_target = dict(library)
        unit_target["name"] = library["name"] + "_unit_tests"
        rules.append(render_rule("tidepool_rust_test", unit_target["name"], unit_target, package, dev_deps, dev_named))
    for target in tests:
        deps = dev_deps + ([":" + libraries[0]["name"]] if libraries else [])
        extra = ""
        if package["name"] == "tidepool-atomic-write" and target["name"] == "strict_directory":
            deps.append(":directory_fault_shared")
            deps.sort()
            extra = '    env = {"TIDEPOOL_DIRECTORY_FAULT_LIBRARY": "$(location :directory_fault_shared)"},'
        if package["name"] == "tidepool" and target["name"] in (
            "exomonad_workspace_check",
            "exomonad_namespace_entry",
        ):
            extra = '    env = {"CARGO_BIN_EXE_exomonad": "$(exe :exomonad)"},'
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
    if package["name"] == "tidepool":
        rules.append('''load("@prelude//:rules.bzl", "genrule")

# Compile and run the existing Cargo build.rs as native Buck actions. This
# produces the embedded Haskell trees and scaffold source using its one owner.
tidepool_rust_binary(
    name = "facade_build_script",
    package_name = "tidepool",
    package_dir = "bridge/facade",
    version = "0.1.0",
    crate_root = "build.rs",
    edition = "2021",
    srcs = ["build.rs"],
    deps = ["//tidepool/toolchain:tidepool_toolchain"],
    visibility = ["PUBLIC"],
)

genrule(
    name = "facade_generated",
    srcs = ["//:facade_build_inputs"],
    out = "generated",
    cmd = "mkdir -p $OUT && CARGO_MANIFEST_DIR=$SRCDIR/bridge/facade OUT_DIR=$OUT TIDEPOOL_EMBED_HASKELL=1 $(exe :facade_build_script)",
    visibility = ["PUBLIC"],
)
''')
    if package["name"] == "tidepool-codegen":
        rules.append('''load("@prelude//:rules.bzl", "cxx_library")

cxx_library(
    name = "prepared_md5",
    srcs = ["csrc/prepared_md5/md5.c"],
    headers = ["csrc/prepared_md5/md5.h"],
    preferred_linkage = "static",
    visibility = ["PUBLIC"],
)
''')
    if package["name"] == "tidepool-atomic-write":
        rules.append('''filegroup(
    name = "directory_fault_fixture",
    srcs = ["tests/fixtures/directory_fault.c"],
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

