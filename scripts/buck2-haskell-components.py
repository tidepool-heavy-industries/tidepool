#!/usr/bin/env python3
"""Project pinned Cabal component metadata into native production/test targets."""
import argparse
from dataclasses import dataclass, replace
from enum import Enum
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "bridge/haskell"
WORKSPACE = ROOT / "exomonad/examples/workspace/.exomonad"
EFFECTS = {"Tidepool.Effects", "Tidepool.Effects.Core", "Tidepool.Effects.Authored"}
PROTOCOL_MODULES = {
    "Tidepool.Internal.ActorProfiles",
    "Tidepool.Internal.ModelControl",
}


class ComponentKind(Enum):
    LIBRARY = "library"
    EXECUTABLE = "executable"
    TEST_SUITE = "test-suite"


class Phase(Enum):
    PRODUCTION = "production"
    TESTS = "tests"
    BENCHMARKS = "benchmarks"


class RuntimeInputRole(Enum):
    FILE = "file"
    EXECUTABLE = "executable"
    DIRECTORY = "directory"
    SEARCH_LIST = "search-list"
    LIBRARY_SEARCH_LIST = "library-search-list"


@dataclass(frozen=True)
class RuntimeInput:
    expression: str
    role: RuntimeInputRole


@dataclass(frozen=True)
class Dependency:
    package: str
    libraries: tuple[str | None, ...]
    version_range: str


@dataclass(frozen=True)
class BuildTool:
    package: str
    component: str
    version_range: str


@dataclass(frozen=True)
class Component:
    kind: ComponentKind
    phase: Phase
    name: str
    main: str | None
    source_dirs: tuple[str, ...]
    modules: tuple[str, ...]
    language: str
    extensions: tuple[str, ...]
    ghc_options: tuple[str, ...]
    dependencies: tuple[Dependency, ...]
    tools: tuple[BuildTool, ...]


def normalized_components(metadata):
    if metadata["schema"] != 1:
        raise ValueError("unsupported Cabal metadata schema")
    if not metadata["compiler"].startswith("ghc-") or metadata["platform"] != "x86_64-linux":
        raise ValueError("Cabal metadata needs a supported pinned GHC/native platform mapping")
    source = (PACKAGE / "tidepool-extract.cabal").read_bytes()
    if metadata["source_sha256"] != hashlib.sha256(source).hexdigest():
        raise ValueError("Cabal metadata does not describe the current package source")
    package_name = metadata["package"]
    roster = {}
    configurations = metadata["configurations"]
    if [configuration["phase"] for configuration in configurations] != [phase.value for phase in Phase]:
        raise ValueError("Cabal metadata needs explicit production/tests/benchmarks configurations")
    for configuration in configurations:
        phase = Phase(configuration["phase"])
        flags = configuration["flags"]
        for flag, enabled in (("test-tools", phase != Phase.PRODUCTION),
                              ("benchmarks", phase == Phase.BENCHMARKS)):
            if flag in flags and flags[flag] is not enabled:
                raise ValueError(f"Cabal {phase.value} configuration has unexpected {flag} assignment")
        names = set()
        for value in configuration["components"]:
            component = Component(
                ComponentKind(value["kind"]), phase, value["name"], value["main"],
                tuple(value["source_dirs"]), tuple(value["modules"]), value["language"],
                tuple(value["extensions"]), tuple(value["ghc_options"]),
                tuple(Dependency(dependency["package"], tuple(dependency["libraries"]),
                                 dependency["version_range"]) for dependency in value["dependencies"]),
                tuple(BuildTool(tool["package"], tool["component"], tool["version_range"])
                      for tool in value["tools"]))
            if value["phase"] != phase.value or component.name in names:
                raise ValueError(f"duplicate/mismatched Cabal component in {phase.value}: {component.name}")
            names.add(component.name)
            if component.name in roster:
                previous = roster[component.name]
                if replace(component, phase=previous.phase) != previous:
                    raise ValueError(f"Cabal flags create unsupported native component variants: {component.name}")
            else:
                roster[component.name] = component
    labels = [component.name.replace("-", "_") for component in roster.values()]
    if len(labels) != len(set(labels)):
        raise ValueError("Cabal component names collide at the native label boundary")
    return package_name, roster


def read_metadata(path=None):
    if path is not None:
        return json.loads(Path(path).read_text())
    # The standalone native bootstrap does not depend on this generated output.
    command = ["bash", str(ROOT / "scripts/buck2-run.sh"), "run", "--local-only", "-c", "remote.enabled=false",
               "//build/haskell/cabal-metadata:metadata", "--",
               str(PACKAGE / "tidepool-extract.cabal")]
    completed = subprocess.run(command, check=True, text=True, stdout=subprocess.PIPE)
    return json.loads(completed.stdout)


def literal(value):
    return json.dumps(value, indent=4)


def normalized_path(path):
    return re.sub(r"[/.-]", "_", path)


def source_location(path):
    path = path.resolve()
    if path.is_relative_to(PACKAGE.resolve()):
        return path.relative_to(PACKAGE.resolve()).as_posix()
    if path.is_relative_to(WORKSPACE):
        relative = path.relative_to(WORKSPACE).as_posix()
        return "//exomonad/examples/workspace:authored_haskell_" + normalized_path(relative)
    raise ValueError(f"source {path} needs an owning native source export")


def is_jev_core(module):
    return module == "Jev.Core" or module.startswith("Jev.Core.")


def source(module, roots):
    path = module.replace(".", "/") + ".hs"
    if module in EFFECTS:
        return path, "//bridge/mcp:effects_generated[" + path.replace("/", "_").replace(".", "_") + "]"
    if module in PROTOCOL_MODULES:
        return path, "//bridge/protocol:generated[" + normalized_path("bridge/haskell/lib/" + path) + "]"
    if module == "Project.Checks" and "generated/pinned" in roots:
        return path, "//bridge/facade:workspace_pinned_check_source"
    if is_jev_core(module) and "generated/jev/core" in roots:
        return path, "toolchains//:jev_sources[" + normalized_path("core/" + path) + "]"
    candidates = [PACKAGE / root / path for root in roots if not root.startswith("generated/")]
    found = [source_location(candidate) for candidate in candidates if candidate.is_file()]
    if len(found) != 1:
        raise ValueError(f"module {module} needs one declared source under {roots}: {found}")
    return path, found[0]


def dependencies_for(component, roster, package_name):
    packages = set()
    libraries = set()
    for dependency in component.dependencies:
        if dependency.package == package_name:
            for library in dependency.libraries:
                name = library or package_name
                if name not in roster or roster[name].kind != ComponentKind.LIBRARY:
                    raise ValueError(f"missing native Cabal library {name} required by {component.name}")
                libraries.add(name)
        elif dependency.libraries != (None,):
            raise ValueError(f"external Cabal sublibrary needs a native package mapping: {dependency}")
        elif dependency.package != "base":
            packages.add(dependency.package)
    return sorted(packages), sorted(libraries)


def installed_link_packages(component, roster, package_name, ancestors=()):
    name = component.name
    if name in ancestors:
        raise ValueError("cyclic native library dependency: " + " -> ".join((*ancestors, name)))
    packages, libraries = dependencies_for(component, roster, package_name)
    closure = set(packages)
    for library in libraries:
        closure.update(installed_link_packages(roster[library], roster, package_name, (*ancestors, name)))
    return sorted(closure)


def fields_for(component, roster, package_name):
    roots = component.source_dirs
    srcs = {}
    if component.kind != ComponentKind.LIBRARY:
        if not component.main or Path(component.main).suffix != ".hs":
            raise ValueError(f"{component.name} needs an implemented native Haskell main source")
        candidates = [PACKAGE / root / component.main for root in roots if not root.startswith("generated/")]
        found = [source_location(candidate) for candidate in candidates if candidate.is_file()]
        if len(found) != 1:
            raise ValueError(f"component {component.name} needs one declared main-is: {found}")
        srcs["Main.hs"] = found[0]
    elif component.main is not None:
        raise ValueError(f"library {component.name} cannot have an executable main")
    for module in component.modules:
        path, target = source(module, roots)
        if path in srcs:
            raise ValueError(f"duplicate native source module in {component.name}: {module}")
        srcs[path] = target
    packages, libraries = dependencies_for(component, roster, package_name)
    dependencies = [":" + library.replace("-", "_") for library in libraries]
    flags = ["-X" + component.language]
    flags += ["-X" + extension for extension in component.extensions]
    flags += list(component.ghc_options)
    return roots, srcs, packages, dependencies, flags


def native_catalog_cohort(roster):
    """Project runtime source owners; compiler closure owns emitted inventory."""
    selected = {}
    components = set()
    for component in roster.values():
        for module in component.modules:
            path, target = source(module, component.source_dirs)
            if module in EFFECTS:
                relative = "effects/" + path
            elif is_jev_core(module):
                relative = "jev/core/" + path
            elif target.startswith(("lib/", "actors/")):
                relative = target
            else:
                continue
            previous = selected.setdefault(module, relative)
            if previous != relative:
                raise ValueError(f"conflicting runtime source owners for {module}")
            components.add(component.name)
    if not selected:
        raise ValueError("native catalog lacks declared runtime source owners")
    for module in EFFECTS:
        selected[module] = "effects/" + module.replace(".", "/") + ".hs"
    return {"components": sorted(components), "modules": dict(sorted(selected.items()))}


def render(metadata=None):
    package_name, roster = normalized_components(read_metadata() if metadata is None else metadata)
    jev_sources = sorted({
        "core/" + module.replace(".", "/") + ".hs"
        for component in roster.values()
        if "generated/jev/core" in component.source_dirs
        for module in component.modules
        if is_jev_core(module)
    })
    lines = ['# @generated by scripts/buck2-haskell-components.py from pinned Cabal component metadata.',
             'load("@prelude//:rules.bzl", "filegroup", "haskell_binary", "haskell_library", "sh_test")',
             'load("//build/haskell:defs.bzl", "haskell_component_flags", "haskell_component_link_flags")', '',
             "JEV_SOURCE_PATHS = " + literal(jev_sources), '']
    if "native-helper-contract" in roster:
        lines += ["NATIVE_CATALOG_COHORT = " + literal(native_catalog_cohort(roster)), '']
    elif package_name == "tidepool-extract":
        raise ValueError("native catalog requires its metadata-owned helper cohort")
    lines += ['def declare_haskell_components():']
    for component in roster.values():
        if component.tools and component.kind != ComponentKind.TEST_SUITE:
            raise ValueError(f"{component.name} needs a native build-tool implementation")
        roots, srcs, packages, dependencies, flags = fields_for(component, roster, package_name)
        link_packages = installed_link_packages(component, roster, package_name)
        name = component.name.replace("-", "_")
        suite = component.kind == ComponentKind.TEST_SUITE
        binary = name + "_bin" if suite else name
        production = component.phase == Phase.PRODUCTION or name == "execution_corpus_producer"
        library = component.kind == ComponentKind.LIBRARY
        toolchain = ("haskell_with_extractor_source_inputs" if name == "tidepool_extract_internal"
                     else "haskell" if production else "haskell_tests")
        rule = ["haskell_library(" if library else "haskell_binary(", f"    name = {literal(binary)},",
                f"    srcs = {literal(srcs)},", f"    deps = {literal(dependencies)},"]
        if not library:
            rule += ['    link_style = "static_pic",']
        rule += [f'    _haskell_toolchain = "toolchains//:{toolchain}",',
                 f"    compiler_flags = haskell_component_flags({literal(packages)}, {literal(flags)}),",
                 f"    linker_flags = haskell_component_link_flags({literal(link_packages)}, {literal(list(component.ghc_options))}, dynamic = {not library}),",
                 '    visibility = ["PUBLIC"],', ")", ""]
        lines.extend("    " + line if line else "" for line in "\n".join(rule).splitlines())
        if name in {"tidepool_extract_internal", "tidepool_extract_bin"}:
            profile_rule = list(rule)
            profile_rule[1] = f"    name = {literal(binary + '_profile')},"
            profile_flags = flags + ["-g3", "-fexpose-internal-symbols", "-finfo-table-map"]
            for index, line in enumerate(profile_rule):
                if line.startswith("    compiler_flags = "):
                    profile_rule[index] = f"    compiler_flags = haskell_component_flags({literal(packages)}, {literal(profile_flags)}),"
                elif line.startswith("    deps = "):
                    profile_rule[index] = f"    deps = {literal([dep + '_profile' if dep == ':tidepool_extract_internal' else dep for dep in dependencies])},"
            lines.extend("    " + line if line else "" for line in "\n".join(profile_rule).splitlines())
        if not suite:
            continue
        # Runtime sources preserve the owning package's paths; mutable fixture
        # writes take place only in the existing runner's private copy.
        resources = {}
        for root in sorted(set(roots) | {"lib", "actors"}):
            if root.startswith("generated/"):
                continue
            for path in sorted((PACKAGE / root).rglob("*")):
                if path.is_file() and path.suffix in {".hs", ".hs-boot", ".json", ".cbor", ".txt"}:
                    resolved = path.resolve()
                    relative = ("workspace/.exomonad/" + resolved.relative_to(WORKSPACE).as_posix()
                                if resolved.is_relative_to(WORKSPACE)
                                else resolved.relative_to(PACKAGE.resolve()).as_posix())
                    if "Prelude_cbor" not in path.parts:
                        resources[relative] = source_location(path)
        for module in sorted(PROTOCOL_MODULES):
            path, producer = source(module, ["generated/protocol"])
            resources["lib/" + path] = producer
        rule = ["filegroup(", f"    name = {literal(name + '_resources')},", f"    srcs = {literal(resources)},", ")", ""]
        lines.extend("    " + line if line else "" for line in "\n".join(rule).splitlines())
        # One declaration owns each path's Buck expression and runtime role.
        # The runner roots these inputs before entering its mutable fixture copy.
        env = {
            "PATH": RuntimeInput('read_root_config("nix", "ghc_bin") + ":" + read_root_config("nix", "action_path")', RuntimeInputRole.SEARCH_LIST),
            "TIDEPOOL_GHC_LIBDIR": RuntimeInput('read_root_config("nix", "ghc_libdir")', RuntimeInputRole.DIRECTORY),
            "TIDEPOOL_TEST_EFFECTS_DIR": RuntimeInput(literal("$(location //bridge/mcp:effects_generated)"), RuntimeInputRole.DIRECTORY),
            "TIDEPOOL_PRELUDE_DIR": RuntimeInput(literal("$(location :facade_embedded_sources)/lib"), RuntimeInputRole.DIRECTORY),
            "TIDEPOOL_TEST_PYTHON": RuntimeInput(literal("$(exe toolchains//:python)"), RuntimeInputRole.EXECUTABLE),
        }
        runtime = [":" + binary, ":" + name + "_resources", "//bridge/mcp:effects_generated", ":facade_embedded_sources", "toolchains//:haskell_test_closure", "toolchains//:test_tools_closure"]
        children = {"declaration_join_test": ("DECLARATION_JOIN", "declaration_join_consumer"),
                    "source_boot_product_reuse_test": ("SOURCE_BOOT", "source_boot_child"),
                    "worker_response_test": ("WORKER_RESPONSE", "worker_response_child")}
        declared_tools = []
        for tool in component.tools:
            if tool.package != package_name or tool.component not in roster or roster[tool.component].kind != ComponentKind.EXECUTABLE:
                raise ValueError(f"{component.name} needs an implemented native build-tool edge: {tool}")
            declared_tools.append(tool.component.replace("-", "_"))
        if name in children:
            variable, child = children[name]
            if declared_tools != [child]:
                raise ValueError(f"{component.name} child-process contract differs from Cabal build-tool-depends")
            env["TIDEPOOL_TEST_" + variable + "_CHILD"] = RuntimeInput(literal("$(exe :" + child + ")"), RuntimeInputRole.EXECUTABLE)
            runtime.append(":" + child)
        elif declared_tools:
            raise ValueError(f"{component.name} needs a native runtime binding for its Cabal build tools")
        if name in {"source_boot_product_reuse_test", "prepared_stg_pipeline_test"}:
            env.update({
                "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER": RuntimeInput(literal("$(exe //tidepool/toolchain:candidate_fixture_issuer)"), RuntimeInputRole.EXECUTABLE),
                "TIDEPOOL_COMPILER_DEPLOYMENT": RuntimeInput(literal("$(location //build/package:compiler_deployment)"), RuntimeInputRole.FILE),
                "TIDEPOOL_EXTRACT": RuntimeInput(literal("$(exe //tidepool/extract-cmd:tidepool-extract)"), RuntimeInputRole.EXECUTABLE),
                "TIDEPOOL_EXTRACT_WORKER": RuntimeInput(literal("$(exe :tidepool_extract_bin)"), RuntimeInputRole.EXECUTABLE),
                "LD_LIBRARY_PATH": RuntimeInput(literal("$(location //build/package:tidepool_extract_runtime_libraries)"), RuntimeInputRole.LIBRARY_SEARCH_LIST),
            })
            runtime.extend(["//tidepool/toolchain:candidate_fixture_issuer", "//build/package:compiler_deployment", "//build/package:tidepool_extract_runtime_libraries"])
        path_roles = " ".join(key + ":" + value.role.value for key, value in env.items())
        env_text = "{\n" + "\n".join("    " + literal(key) + ": " + value.expression + ","
                                    for key, value in env.items())
        env_text += "\n    " + literal("TIDEPOOL_TEST_INPUT_PATHS") + ": " + literal(path_roles) + ",\n}"
        rule = ["sh_test(", f"    name = {literal(name)},", '    test = "test-with-fixtures.sh",',
                f"    args = {literal(['$(exe :' + binary + ')', '$(location :' + name + '_resources)'])},",
                f"    resources = {literal(runtime)},", f"    env = {env_text},", "    labels = [\"haskell_component_suite\"],", "    test_rule_timeout_ms = 14400000,", '    visibility = ["PUBLIC"],', ")", ""]
        lines.extend("    " + line if line else "" for line in "\n".join(rule).splitlines())
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--metadata", type=Path, help="explicit producer output for source-only review")
    options = parser.parse_args()
    output = PACKAGE / "components.bzl"
    generated = render(read_metadata(options.metadata))
    if options.check:
        if not output.is_file() or output.read_text() != generated:
            raise SystemExit("stale native Haskell components; run scripts/buck2-haskell-components.py")
    else:
        output.write_text(generated)


if __name__ == "__main__":
    main()
