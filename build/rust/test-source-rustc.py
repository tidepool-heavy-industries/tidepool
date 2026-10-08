#!/usr/bin/env python3
"""Retain source participation from the same first-party rustc --test action."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys


def expanded_arguments(arguments, active=()):
    """Inspect rustc's one-argument-per-line response format without rewriting it."""
    result = []
    for argument in arguments:
        if argument.startswith("@"):
            path = Path(argument[1:]).absolute()
            if path in active:
                raise ValueError(f"recursive rustc response file: {path}")
            result.extend(expanded_arguments(path.read_text().splitlines(), (*active, path)))
        else:
            result.append(argument)
    return result


def option_values(arguments, option):
    values = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == option:
            index += 1
            if index == len(arguments):
                raise ValueError(f"missing value for {option}")
            values.append(arguments[index])
        elif argument.startswith(option + "="):
            values.append(argument[len(option) + 1:])
        elif option in ("-C", "-o") and argument.startswith(option):
            values.append(argument[len(option):])
        index += 1
    return values


def emit_modes(arguments):
    result = {}
    for value in option_values(arguments, "--emit"):
        for output in value.split(","):
            mode, separator, path = output.partition("=")
            if not mode or (separator and not path):
                raise ValueError(f"invalid rustc emit mode: {output!r}")
            result[mode] = path if separator else None
    return result or {"link": None}


def depfile_dependencies(text):
    """Decode each Make rule, including continuations, phony rules and escapes.

    Prelude's makefile_to_dep_file decodes escaped spaces after joining line
    continuations. Rust dep-info additionally emits several rules and phony
    source targets; only their dependency side contributes source evidence.
    """
    text = text.replace("\\\r\n", "").replace("\\\n", "")
    dependencies = set()
    for line in text.splitlines():
        tokens = []
        token = []
        in_dependencies = False
        index = 0
        while index < len(line):
            character = line[index]
            if character == "\\" and index + 1 < len(line):
                following = line[index + 1]
                if following in " \t#:\\":
                    token.append(following)
                    index += 2
                    continue
            if character == "$" and line[index:index + 2] == "$$":
                token.append("$")
                index += 2
                continue
            if character == "#":
                break
            if character == ":" and not in_dependencies:
                in_dependencies = True
                tokens.clear()
                token.clear()
            elif character.isspace():
                if token:
                    tokens.append("".join(token))
                    token.clear()
            else:
                token.append(character)
            index += 1
        if token:
            tokens.append("".join(token))
        if in_dependencies:
            dependencies.update(tokens)
        elif line.strip() and not line.lstrip().startswith("#"):
            raise ValueError("malformed rustc dep-info rule")
    if not dependencies:
        raise ValueError("rustc dep-info contains no source dependencies")
    return dependencies


def source_path(path):
    parsed = PurePosixPath(path)
    if (not path or parsed.is_absolute() or any(part in (".", "..") for part in parsed.parts)
            or parsed.as_posix() != path or "\\" in path):
        raise ValueError(f"invalid source obligation: {path!r}")
    return path


def load_requirements(path):
    raw = Path(path).read_bytes()
    requirements = json.loads(raw)
    if requirements.get("version") != 1:
        raise ValueError("unsupported test source requirements version")
    source_path(requirements["package_dir"])
    for role in ("modules", "fixtures"):
        paths = requirements[role]
        if not isinstance(paths, list) or len(paths) != len(set(paths)):
            raise ValueError(f"duplicate or malformed {role} obligations")
        for item in paths:
            source_path(item)
    if set(requirements["modules"]) & set(requirements["fixtures"]):
        raise ValueError("one test source cannot be both Module and Fixture")
    return requirements, hashlib.sha256(raw).hexdigest()


def projection_root(requirements, environment):
    manifest = environment.get("CARGO_MANIFEST_DIR")
    if not manifest or not Path(manifest).is_absolute():
        raise ValueError("test source participation requires the declared absolute manifest directory")
    root = Path(manifest)
    package_parts = PurePosixPath(requirements["package_dir"]).parts
    if root.parts[-len(package_parts):] != package_parts:
        raise ValueError("manifest directory does not match the declared package source projection")
    for _ in package_parts:
        root = root.parent
    return root


def compile_test(rustc, arguments, environment):
    requirement_path = environment.get("TIDEPOOL_TEST_SOURCE_REQUIREMENTS")
    if not requirement_path:
        return subprocess.call([rustc, *arguments])
    expanded = expanded_arguments(arguments)
    if "--test" not in expanded:
        return subprocess.call([rustc, *arguments])
    requirements, requirements_hash = load_requirements(requirement_path)
    root = projection_root(requirements, environment)
    directories = option_values(expanded, "--out-dir")
    if not directories:
        raise ValueError("test source participation requires Prelude's declared extras directory")
    output = Path(directories[-1])
    modes = emit_modes(expanded)
    retained_depfile = output / "test-source-participation.d"
    depfile = modes.get("dep-info")
    compiler_arguments = list(arguments)
    if "dep-info" not in modes:
        modes["dep-info"] = str(retained_depfile)
        compiler_arguments.append("--emit=" + ",".join(
            mode + ("=" + path if path is not None else "") for mode, path in modes.items()
        ))
        depfile = retained_depfile
    elif depfile is None:
        # Retain the compiler's existing output selection without rewriting
        # response arguments or changing where the action expects dep-info.
        explicit_output = option_values(expanded, "-o")
        if explicit_output:
            depfile = Path(explicit_output[-1])
            if len(modes) > 1:
                depfile = depfile.with_suffix(".d")
        else:
            crates = option_values(expanded, "--crate-name")
            if not crates:
                raise ValueError("bare dep-info emission requires a declared crate name")
            extra = ""
            for value in option_values(expanded, "-C"):
                if value.startswith("extra-filename="):
                    extra = value.partition("=")[2]
            depfile = output / (crates[-1] + extra + ".d")
    output.mkdir(parents=True, exist_ok=True)
    status = subprocess.call([rustc, *compiler_arguments])
    if status:
        return status
    body = Path(depfile).read_bytes()
    dependencies = depfile_dependencies(body.decode())
    if Path(depfile).absolute() != retained_depfile.absolute():
        retained_depfile.write_bytes(body)
    consumed = set()
    for dependency in dependencies:
        try:
            consumed.add(Path(os.path.abspath(dependency)).relative_to(root).as_posix())
        except ValueError:
            continue
    missing = sorted(set(requirements["modules"]) - consumed)
    proof = {
        "version": 1,
        "kind": "rustc-test-source-participation",
        "target": requirements["target"],
        "package": requirements["package"],
        "status": "refused" if missing else "complete",
        "compiler": rustc,
        "arguments": arguments,
        "compiler_arguments": compiler_arguments,
        "expanded_arguments": expanded,
        "cfg": option_values(expanded, "--cfg"),
        "requirements_sha256": requirements_hash,
        "dep_info_sha256": hashlib.sha256(body).hexdigest(),
        "dependencies": sorted(consumed),
        "modules": [{"path": path, "role": "Module", "compiler_dependency": path in consumed}
                    for path in requirements["modules"]],
        "fixtures": [{"path": path, "role": "Fixture", "compiler_dependency": path in consumed}
                     for path in requirements["fixtures"]],
        "missing_modules": missing,
    }
    (output / "test-source-participation.json").write_text(json.dumps(proof, indent=2) + "\n")
    if missing:
        raise ValueError("required test Module sources absent from rustc dep-info: " + ", ".join(missing))
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rustc", required=True)
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    options = parser.parse_args(argv)
    arguments = options.arguments
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    try:
        return compile_test(options.rustc, arguments, os.environ)
    except (OSError, ValueError, KeyError, TypeError) as error:
        message = f"test source participation: {error}"
        try:
            inspected = expanded_arguments(arguments)
        except (OSError, ValueError):
            inspected = arguments
        if "json" in option_values(inspected, "--error-format"):
            print(json.dumps({"$message_type": "diagnostic", "message": message,
                              "level": "error", "code": None, "spans": [],
                              "children": [], "rendered": message + "\n"}), file=sys.stderr)
        else:
            print(message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
