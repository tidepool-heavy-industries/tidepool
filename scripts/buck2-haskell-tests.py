#!/usr/bin/env python3
"""Generate native Haskell test components from their Cabal source declarations."""
import argparse
import json
from pathlib import Path
import re
import shlex

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "bridge/haskell"
WORKSPACE = ROOT / "exomonad/examples/workspace/.exomonad"
INTERNAL = {"tidepool-extract-internal": ":tidepool_extract_internal", "assignment-internal": ":assignment_internal"}
EFFECTS = {"Tidepool.Effects", "Tidepool.Effects.Core", "Tidepool.Effects.Authored"}


def components(text):
    result = {}
    current = None
    field = None
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("--"):
            continue
        heading = re.fullmatch(r"(common|library|test-suite|executable) ([a-zA-Z0-9_-]+)", line)
        if heading:
            current = {"kind": heading[1], "name": heading[2], "fields": {}}
            result[heading[2]] = current
            field = None
        elif line and not line[0].isspace():
            current = None
        elif current is not None:
            match = re.match(r"^  ([a-z-]+):\s*(.*)", line)
            if match:
                field = match[1]
                current["fields"].setdefault(field, []).append(match[2])
            elif line.startswith("  if "):
                field = None
            elif field and line.startswith("    "):
                current["fields"][field].append(line.strip())
    for component in result.values():
        fields = component["fields"]
        for common in words(fields.get("import", [])):
            if common not in result or result[common]["kind"] != "common":
                raise ValueError(f"unknown Cabal common component {common}")
            for key, values in result[common]["fields"].items():
                fields[key] = list(values) + fields.get(key, [])
    return result


def words(values):
    return " ".join(values).replace(",", " ").split()


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
    if module == "Tidepool.Internal.ModelControl":
        return path, "//bridge/protocol:generated[bridge_haskell_lib_Tidepool_Internal_ModelControl_hs]"
    if module == "Project.Checks" and "generated/pinned" in roots:
        return path, "//bridge/facade:workspace_pinned_check_source"
    if is_jev_core(module) and "generated/jev/core" in roots:
        return path, "toolchains//:jev_sources[" + normalized_path("core/" + path) + "]"
    candidates = [PACKAGE / root / path for root in roots if not root.startswith("generated/")]
    found = [source_location(candidate) for candidate in candidates if candidate.is_file()]
    if len(found) != 1:
        raise ValueError(f"module {module} needs one declared source under {roots}: {found}")
    return path, found[0]


def dependencies_for(component, roster):
    packages = set()
    libraries = set()
    for declaration in " ".join(component["fields"].get("build-depends", [])).split(","):
        if not declaration.strip():
            continue
        package = declaration.strip().split()[0]
        declared_library = roster.get(package, {}).get("kind") == "library"
        if package in INTERNAL or declared_library:
            if not declared_library:
                raise ValueError(f"missing Cabal internal library {package} required by {component['name']}")
            if package not in INTERNAL:
                raise ValueError(f"Cabal internal library {package} needs a native Buck target")
            libraries.add(package)
        elif package != "base":
            packages.add(package)
    return sorted(packages), sorted(libraries)


def installed_link_packages(component, roster, ancestors=()):
    """Installed packages needed by a binary's complete native library closure.

    Prelude carries project archives through HaskellLinkInfo, but library
    linker_flags are local to the library's own link action. Installed package
    roots must therefore be selected again by the final GHC linker invocation.
    """
    name = component["name"]
    if name in ancestors:
        raise ValueError("cyclic Cabal internal library dependency: " + " -> ".join((*ancestors, name)))
    packages, libraries = dependencies_for(component, roster)
    closure = set(packages)
    for library in libraries:
        closure.update(installed_link_packages(roster[library], roster, (*ancestors, name)))
    return sorted(closure)


def fields_for(component, roster):
    fields = component["fields"]
    roots = words(fields.get("hs-source-dirs", []))
    main = " ".join(fields.get("main-is", []))
    candidates = [PACKAGE / root / main for root in roots if not root.startswith("generated/")]
    found = [source_location(candidate) for candidate in candidates if candidate.is_file()]
    if len(found) != 1:
        raise ValueError(f"component {component['name']} needs one declared main-is: {found}")
    srcs = {"Main.hs": found[0]}
    srcs.update(source(module, roots) for module in words(fields.get("other-modules", [])))
    packages, libraries = dependencies_for(component, roster)
    dependencies = [INTERNAL[library] for library in libraries]
    flags = ["-Wall", "-X" + " ".join(fields.get("default-language", ["GHC2024"]))]
    flags += ["-X" + extension for extension in words(fields.get("default-extensions", []))]
    flags += shlex.split(" ".join(fields.get("ghc-options", [])))
    return roots, srcs, sorted(set(packages)), sorted(set(dependencies)), list(dict.fromkeys(flags))


def render():
    roster = components((PACKAGE / "tidepool-extract.cabal").read_text())
    jev_sources = sorted({
        "core/" + module.replace(".", "/") + ".hs"
        for component in roster.values()
        if "generated/jev/core" in words(component["fields"].get("hs-source-dirs", []))
        for module in words(component["fields"].get("other-modules", []))
        if is_jev_core(module)
    })
    lines = ['# @generated by scripts/buck2-haskell-tests.py from Cabal component declarations.',
             'load("@prelude//:rules.bzl", "filegroup", "haskell_binary", "haskell_library", "sh_test")',
             'load("//build/haskell:defs.bzl", "haskell_component_flags", "haskell_component_link_flags")', '',
             "JEV_SOURCE_PATHS = " + literal(jev_sources), '',
             'def declare_haskell_test_components():']
    for component in roster.values():
        if component["kind"] in {"common", "library"} or component["name"] == "tidepool-extract-bin":
            continue
        roots, srcs, packages, dependencies, flags = fields_for(component, roster)
        link_packages = installed_link_packages(component, roster)
        name = component["name"].replace("-", "_")
        suite = component["kind"] == "test-suite"
        binary = name + "_bin" if suite else name
        production = name == "execution_corpus_producer"
        rule = ["haskell_binary(", f"    name = {literal(binary)},", f"    srcs = {literal(srcs)},",
                f"    deps = {literal(dependencies)},", '    link_style = "static_pic",',
                f'    _haskell_toolchain = "toolchains//:{"haskell" if production else "haskell_tests"}",',
                f"    compiler_flags = haskell_component_flags({literal(packages)}, {literal(flags)}),",
                f"    linker_flags = haskell_component_link_flags({literal(link_packages)}, {literal([flag for flag in flags if flag.startswith(('-rtsopts', '-with-rtsopts'))])}),",
                '    visibility = ["PUBLIC"],', ")", ""]
        lines.extend("    " + line if line else "" for line in "\n".join(rule).splitlines())
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
        resources["lib/Tidepool/Internal/ModelControl.hs"] = "//bridge/protocol:generated[bridge_haskell_lib_Tidepool_Internal_ModelControl_hs]"
        rule = ["filegroup(", f"    name = {literal(name + '_resources')},", f"    srcs = {literal(resources)},", ")", ""]
        lines.extend("    " + line if line else "" for line in "\n".join(rule).splitlines())
        env = {
            "PATH": None,
            "TIDEPOOL_GHC_LIBDIR": None,
            "TIDEPOOL_TEST_EFFECTS_DIR": "$(location //bridge/mcp:effects_generated)",
            "TIDEPOOL_PRELUDE_DIR": "$(location :facade_embedded_sources)/lib",
            "TIDEPOOL_TEST_PYTHON": "$(exe toolchains//:python)",
        }
        runtime = [":" + binary, ":" + name + "_resources", "//bridge/mcp:effects_generated", ":facade_embedded_sources", "toolchains//:haskell_test_closure", "toolchains//:test_tools_closure"]
        children = {"declaration_join_test": ("DECLARATION_JOIN", "declaration_join_consumer"),
                    "source_boot_product_reuse_test": ("SOURCE_BOOT", "source_boot_child"),
                    "worker_response_test": ("WORKER_RESPONSE", "worker_response_child")}
        if name in children:
            variable, child = children[name]
            env["TIDEPOOL_TEST_" + variable + "_CHILD"] = "$(exe :" + child + ")"
            runtime.append(":" + child)
        if name == "source_boot_product_reuse_test":
            env.update({
                "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER": "$(exe //tidepool/toolchain:candidate_fixture_issuer)",
                "TIDEPOOL_COMPILER_DEPLOYMENT": "$(location //build/package:compiler_deployment)",
                "TIDEPOOL_EXTRACT": "$(exe //tidepool/extract-cmd:tidepool-extract)",
                "TIDEPOOL_EXTRACT_WORKER": "$(exe :tidepool_extract_bin)",
                "LD_LIBRARY_PATH": "$(location //build/package:tidepool_extract_runtime_libraries)",
            })
            runtime.extend(["//tidepool/toolchain:candidate_fixture_issuer", "//build/package:compiler_deployment", "//build/package:tidepool_extract_runtime_libraries"])
        env_text = "{\n" + "\n".join("    " + literal(key) + ": " + (('read_root_config("nix", "ghc_bin") + ":" + read_root_config("nix", "action_path")') if key == "PATH" else 'read_root_config("nix", "ghc_libdir")') + "," if value is None else "    " + literal(key) + ": " + literal(value) + "," for key, value in env.items()) + "\n}"
        rule = ["sh_test(", f"    name = {literal(name)},", '    test = "test-with-fixtures.sh",',
                f"    args = {literal(['$(exe :' + binary + ')', '$(location :' + name + '_resources)'])},",
                f"    resources = {literal(runtime)},", f"    env = {env_text},", "    test_rule_timeout_ms = 14400000,", '    visibility = ["PUBLIC"],', ")", ""]
        lines.extend("    " + line if line else "" for line in "\n".join(rule).splitlines())
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    options = parser.parse_args()
    output = PACKAGE / "tests.bzl"
    generated = render()
    if options.check:
        if not output.is_file() or output.read_text() != generated:
            raise SystemExit("stale native Haskell test components; run scripts/buck2-haskell-tests.py")
    else:
        output.write_text(generated)


if __name__ == "__main__":
    main()
