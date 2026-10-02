#!/usr/bin/env python3
"""Build the optimized extractor worker and retain its actual GHC actions."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid

from ghc_source_options import audit_source_optimization_options, verify_worker_home_actions
from source_snapshot import audit, capture, repository_root


RESPONSE_FILE_LIMIT = 16 * 1024 * 1024
ACTION_DIR_ENV = "TIDEPOOL_GHC_BUILD_ACTION_DIR"
COMPILER_ENV = "TIDEPOOL_GHC_BUILD_COMPILER"


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def reference(path):
    path = Path(path).resolve()
    return {"path": str(path), "sha256": digest(path)}


def ghc_response_words(contents):
    """Decode GHC's base GHC.ResponseFile quoting and escaping rules."""
    words = []
    word = []
    quote = None
    escaped = False
    for char in contents:
        if escaped:
            word.append(char)
            escaped = False
        elif char == "\\":
            escaped = True
        elif quote is not None:
            if char == quote:
                quote = None
            else:
                word.append(char)
        elif char in ("'", '"'):
            quote = char
        elif char.isspace():
            if word:
                words.append("".join(word))
                word.clear()
        else:
            word.append(char)
    if word:
        words.append("".join(word))
    return words


def expand_response_args(argv, cwd):
    """Expand the one response-file layer GHC expands on process startup."""
    expanded = []
    responses = []
    for arg in argv:
        if not arg.startswith("@"):
            expanded.append(arg)
            continue
        path = Path(arg[1:])
        if not path.is_absolute():
            path = Path(cwd) / path
        path = path.resolve(strict=True)
        size = path.stat().st_size
        if size > RESPONSE_FILE_LIMIT:
            raise ValueError(f"GHC response file exceeds {RESPONSE_FILE_LIMIT} bytes: {path}")
        contents = path.read_bytes()
        if len(contents) != size:
            raise ValueError(f"GHC response file changed while being read: {path}")
        text = contents.decode("utf-8")
        expanded.extend(ghc_response_words(text))
        responses.append({"path": str(path), "bytes": len(contents),
                          "sha256": hashlib.sha256(contents).hexdigest()})
    return expanded, responses


def ghc_wrapper_main(argv):
    """Transparent Cabal compiler wrapper; execute GHC with its original argv."""
    compiler = Path(os.environ[COMPILER_ENV]).resolve(strict=True)
    action_dir = Path(os.environ[ACTION_DIR_ENV]).resolve(strict=True)
    cwd = str(Path.cwd().resolve())
    expanded, response_files = expand_response_args(argv, cwd)
    try:
        completed = subprocess.run([str(compiler), *argv], check=False)
        exit_code = completed.returncode
        launch_error = None
    except OSError as error:
        exit_code = 127
        launch_error = f"{type(error).__name__}: {error}"
        print(f"GHC wrapper could not launch {compiler}: {error}", file=sys.stderr)
    row = {
        "cwd": cwd,
        "argv": [str(compiler), *expanded],
        "raw_argv": argv,
        "response_files": response_files,
        "exit_code": exit_code,
    }
    if launch_error is not None:
        row["launch_error"] = launch_error
    name = f"{time.time_ns()}-{os.getpid()}-{uuid.uuid4().hex}.json"
    with (action_dir / name).open("x") as stream:
        json.dump(row, stream, separators=(",", ":"))
        stream.write("\n")
    return exit_code


def nix_tool(name):
    selected = shutil.which(name)
    if not selected:
        raise RuntimeError(f"{name} is not available in the pinned Nix shell")
    path = Path(selected).resolve(strict=True)
    if not str(path).startswith("/nix/store/") or not path.is_file():
        raise RuntimeError(f"{name} is not the materialized Nix tool: {path}")
    return path


def build_environment(environment, action_dir, compiler):
    result = environment.copy()
    result[COMPILER_ENV] = str(compiler)
    result[ACTION_DIR_ENV] = str(action_dir.resolve())
    return result


def build_command(cabal, wrapper, ghc_pkg, builddir, operation="build"):
    execution = ["-j2", "--offline"] if operation == "build" else []
    return [str(cabal), operation, *execution, f"--builddir={builddir}",
            f"--with-compiler={wrapper}", f"--with-hc-pkg={ghc_pkg}",
            "--enable-optimization=2", "tidepool-extract-bin"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = repository_root(Path.cwd())
    output = args.output.resolve()
    if not output.is_relative_to(root) or subprocess.run(
            ["git", "-C", str(root), "check-ignore", "--quiet", str(output.relative_to(root))]).returncode:
        parser.error("output must be a Git-ignored repository directory")
    if output.exists():
        parser.error("output already exists; retained build evidence cannot be overwritten")
    if not os.environ.get("IN_NIX_SHELL"):
        parser.error("run through the declared Nix shell after materializing the Haskell toolchain")
    compiler = nix_tool("ghc")
    cabal = nix_tool("cabal")
    ghc_pkg = nix_tool("ghc-pkg")
    output.mkdir(parents=True)
    control = output / "control"
    action_dir = output / "ghc-invocations"
    control.mkdir()
    action_dir.mkdir()
    recorder_copy = control / Path(__file__).name
    snapshot_copy = control / "source_snapshot.py"
    source_options_copy = control / "ghc_source_options.py"
    shutil.copy2(Path(__file__).resolve(), recorder_copy)
    shutil.copy2(Path(__file__).with_name("source_snapshot.py").resolve(), snapshot_copy)
    shutil.copy2(Path(__file__).with_name("ghc_source_options.py").resolve(), source_options_copy)
    wrapper = control / "ghc-wrapper"
    wrapper.write_text(
        f"#!{sys.executable}\n"
        "import runpy, sys\n"
        f"namespace = runpy.run_path({str(recorder_copy)!r}, run_name='ghc_recorded_wrapper')\n"
        "raise SystemExit(namespace['ghc_wrapper_main'](sys.argv[1:]))\n")
    wrapper.chmod(0o555)
    builddir = output / "cabal-build"
    project = root / "bridge/haskell"
    environment = build_environment(os.environ, action_dir, compiler)
    environment_reference = {
        key: value for key, value in sorted(environment.items())
        if key in {"PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "CABAL_DIR", "CABAL_CONFIG",
                   "GHC_PACKAGE_PATH", "GHC_ENVIRONMENT", "GHC_LIBDIR", "NIX_GHC",
                   "NIX_GHC_LIBDIR", "NIX_GHC_PACKAGE_PATH"}
    }
    (output / "build-environment.json").write_text(json.dumps(environment_reference, indent=2) + "\n")
    command = build_command(cabal, wrapper, ghc_pkg, builddir)
    packet = {
        "schema": "ghc-build-v2",
        "compiler": reference(compiler),
        "cabal": reference(cabal),
        "ghc_pkg": reference(ghc_pkg),
        "recorder": reference(recorder_copy),
        "wrapper": reference(wrapper),
        "snapshot_tool": reference(snapshot_copy),
        "source_options_tool": reference(source_options_copy),
        "environment": reference(output / "build-environment.json"),
        "source_root": str(root),
        "working_directory": str(project.resolve()),
        "build_directory": str(builddir),
        "output_directory": str(output),
    }
    try:
        manifest = capture(root, output / "source-before", [])
        packet["source_oid"] = manifest["source"]["head_oid"]
        packet["source_before"] = reference(output / "source-before/manifest.json")
        packet["source_archive"] = reference(output / "source-before/sources.tar")
        helper_entry = next((entry for entry in manifest["source"]["entries"]
                             if entry.get("path") == "build/testing/ghc_source_options.py"), None)
        if helper_entry is None or digest(source_options_copy) != helper_entry.get("sha256"):
            raise RuntimeError("retained source auditor differs from the captured source bytes")
        packet["source_options_tool"] = reference(source_options_copy)
        source_option_audit = audit_source_optimization_options(
            output / "source-before/sources.tar", manifest)
        (output / "source-options.json").write_text(json.dumps(source_option_audit, indent=2) + "\n")
        packet["source_options"] = reference(output / "source-options.json")
        packet["command"] = command
        packet["started_at_ns"] = time.time_ns()
        with (output / "build.log").open("wb") as log:
            status = subprocess.run(command, cwd=project, env=environment,
                                    stdout=log, stderr=subprocess.STDOUT, check=False).returncode
        packet["exit_code"] = status
        packet["build_log"] = reference(output / "build.log")
        all_rows = [json.loads(path.read_text()) for path in sorted(action_dir.glob("*.json"))]
        with (output / "ghc-invocations.jsonl").open("w") as stream:
            for row in all_rows:
                stream.write(json.dumps(row, separators=(",", ":")) + "\n")
        packet["ghc_invocations"] = reference(output / "ghc-invocations.jsonl")
        source_unchanged = audit(root, output / "source-before", output / "source-after.json")
        packet["source_after"] = reference(output / "source-after.json")
        if status:
            raise RuntimeError(f"Cabal exited {status}; retained build.log and GHC actions contain diagnostics")
        home_actions = verify_worker_home_actions(
            all_rows, (output / "build.log").read_text(), compiler, root, builddir,
            source_option_audit)
        packet.update(home_actions)
        if not source_unchanged:
            raise RuntimeError("source changed during build; see retained source-after.json")
        list_bin = build_command(cabal, wrapper, ghc_pkg, builddir, "list-bin")
        located = subprocess.run(list_bin, cwd=project, env=environment,
                                 check=True, capture_output=True, text=True).stdout.strip()
        original = Path(located).resolve(strict=True)
        destination = output / "tidepool-extract-bin"
        source_hash = digest(original)
        shutil.copy2(original, destination)
        destination.chmod(0o555)
        if digest(destination) != source_hash:
            raise RuntimeError("worker output changed while being frozen")
        packet["list_bin_command"] = list_bin
        packet["output"] = dict(reference(destination), cabal_path=str(original))
        packet["finished_at_ns"] = time.time_ns()
        packet["status"] = "built"
    except Exception as error:
        packet["status"] = "refused"
        packet["error"] = str(error)
        raise
    finally:
        (output / "packet.json").write_text(json.dumps(packet, indent=2) + "\n")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--ghc-wrapper":
        raise SystemExit(ghc_wrapper_main(sys.argv[2:]))
    main()
