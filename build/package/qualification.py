#!/usr/bin/env python3
"""Freeze and qualify one native Exomonad deployment at its final path."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import time
import tomllib

M2_TESTS = [
    "actor_host::embedded_captured_unfold_tests::admitted_cell_late_type_error_has_no_effect_or_publication_on_retry",
    "actor_host::embedded_captured_unfold_tests::embedded_captured_unfold_awaits_two_child_replies_before_parent_call_returns",
    "actor_host::embedded_captured_unfold_tests::embedded_captured_children_and_capture_survive_failure_of_the_unfinished_parent_cell",
    "actor_host::embedded_captured_unfold_tests::embedded_same_root_parked_nominal_a_joins_later_b_publication",
    "actor_host::embedded_captured_unfold_tests::embedded_parked_captured_pipeline_cancellation_settles_invocation_owned_children",
    "actor_host::embedded_checkpoint_release_tests::released_checkpoint_keeps_an_admitted_childs_hosted_context",
]
M1_TESTS = ["actor_host::m1_host_tests::production_browser_executes_resident_haskell_retries_and_controls_root"]
DESCRIPTOR = "share/exomonad/qualification.json"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def inventory(root: Path, excluded=()) -> dict:
    """Bind copied bytes and declared immutable closure references, including modes."""
    entries = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if relative in excluded:
            continue
        if path.is_symlink():
            resolved = path.resolve(strict=True)
            if not (resolved.is_relative_to(root) or str(resolved).startswith("/nix/store/")):
                raise ValueError(f"runtime input escapes the bundle and declared Nix closure: {path} -> {resolved}")
            item = {"kind": "symlink", "target": os.readlink(path), "resolved": str(resolved)}
            if resolved.is_file():
                item.update(sha256=sha256(resolved), bytes=resolved.stat().st_size)
        elif path.is_file():
            item = {"kind": "file", "sha256": sha256(path), "bytes": path.stat().st_size,
                    "executable": bool(path.stat().st_mode & 0o111)}
        elif path.is_dir():
            item = {"kind": "directory"}
        else:
            raise ValueError(f"unsupported runtime input: {path}")
        entries[relative] = item
    return entries


def digest_inventory(entries: dict) -> str:
    return hashlib.sha256(json.dumps(entries, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def nix_path(path: Path) -> Path:
    resolved = path.resolve(strict=True)
    if not str(resolved).startswith("/nix/store/"):
        raise ValueError(f"toolchain inputs must be declared immutable Nix paths: {path}")
    return resolved


def store_root(path: Path) -> Path:
    return Path(*nix_path(path).parts[:4])


def nix_metadata(tools: Path, roots: list[str]) -> dict:
    executable = str(tools / "bin/nix-store")
    closure = sorted(set(subprocess.check_output(
        [executable, "--query", "--requisites", *roots], text=True, timeout=60).splitlines()))
    hashes = subprocess.check_output(
        [executable, "--query", "--hash", *closure], text=True, timeout=60).splitlines()
    if len(hashes) != len(closure) or not closure or any(not value.startswith("sha256:") for value in hashes):
        raise ValueError("declared Nix closure lacks complete SHA-256 NAR evidence")
    return {"roots": roots, "closure": closure, "nar_hashes": dict(zip(closure, hashes))}


def pin_nix_closure(root: Path, tools: Path, selected: list[Path]) -> tuple[dict, list[dict]]:
    roots = sorted({str(store_root(path)) for path in selected})
    pinned = []
    for selected_root in roots:
        target = root / "share/exomonad/gc-roots" / hashlib.sha256(selected_root.encode()).hexdigest()
        target.parent.mkdir(parents=True, exist_ok=True)
        command = [str(tools / "bin/nix-store"), "--add-root", str(target), "--indirect", "--realise", selected_root]
        subprocess.run(command, check=True, capture_output=True, timeout=60)
        if target.resolve(strict=True) != Path(selected_root):
            raise ValueError(f"Nix did not retain the requested deployment root: {target}")
        pinned.append({"path": str(target), "store_path": selected_root, "command": command, "exit_code": 0})
    return nix_metadata(tools, roots), pinned


def native_environment(root: Path) -> dict:
    ghc = (root / "share/exomonad/ghc-libdir.txt").read_text().strip()
    tools = (root / "share/exomonad/runtime-tools").resolve(strict=True)
    return {
        "TIDEPOOL_EXTRACT": str(root / "bin/tidepool-extract"),
        "TIDEPOOL_EXTRACT_WORKER": str(root / "bin/tidepool-extract-bin"),
        "TIDEPOOL_COMPILER_DEPLOYMENT": str(root / "share/exomonad/compiler-deployment.json"),
        "TIDEPOOL_PRELUDE_DIR": str(root / "share/exomonad/stdlib"),
        "TIDEPOOL_GHC_LIBDIR": ghc,
        "EXOMONAD_EMBEDDED_ASSET_ROOT": str(root / "share/exomonad/web"),
        "LD_LIBRARY_PATH": str(root / "lib/tidepool"),
        "PATH": str(tools / "bin") + ":" + str(root / "bin"),
        "TIDEPOOL_KEEP_TEST_LOGS": "1",
    }


def execution_environment(descriptor: dict) -> dict:
    environment = dict(os.environ)
    # No resident endpoint, project catalog or compiler selection may override
    # the pair and source mode that the exact bundle was qualified with.
    for key in ("TIDEPOOL_EXTRACT_DAEMON_SOCKET", "TIDEPOOL_COMPILER_MODULES",
                "TIDEPOOL_COMPILER_DEPLOYMENT", "TIDEPOOL_EXTRACT_WORKER",
                "TIDEPOOL_EXTRACT", "TIDEPOOL_PRELUDE_DIR", "TIDEPOOL_GHC_LIBDIR",
                "EXOMONAD_EMBEDDED_ASSET_ROOT", "LD_LIBRARY_PATH",
                "TIDEPOOL_EXTRACT_NO_DAEMON"):
        environment.pop(key, None)
    environment.update(descriptor["environment"])
    return environment


def elf_interpreter(path: Path) -> str | None:
    with path.open("rb") as source:
        header = source.read(64)
        if header[:4] != b"\x7fELF" or header[4:6] != b"\x02\x01":
            raise ValueError(f"expected native ELF64 little-endian executable: {path}")
        offset, entry_size, count = struct.unpack_from("<Q", header, 32)[0], *struct.unpack_from("<HH", header, 54)
        for index in range(count):
            source.seek(offset + index * entry_size)
            entry = source.read(entry_size)
            if struct.unpack_from("<I", entry)[0] == 3:  # PT_INTERP
                position, length = struct.unpack_from("<Q", entry, 8)[0], struct.unpack_from("<Q", entry, 32)[0]
                if length > 4096:
                    raise ValueError("ELF interpreter exceeds path bound")
                source.seek(position)
                return source.read(length).rstrip(b"\0").decode()
    return None


def loader_evidence(root: Path, environment: dict) -> dict:
    """Inspect the actual ELF loader resolution, not assumed worker link style."""
    evidence = {}
    for name in ("exomonad-unwrapped", "exomonad-view-helper", "tidepool-extract", "tidepool-extract-bin", "tidepool-tests"):
        path = root / "bin" / name
        interpreter = elf_interpreter(path)
        if interpreter is None:
            evidence[name] = {"linkage": "static"}
            continue
        loader = nix_path(Path(interpreter))
        result = subprocess.run([str(loader), "--list", str(path)], env=environment,
                                text=True, capture_output=True, check=True, timeout=30)
        libraries = {}
        for match in re.finditer(r"(?:=>\s+)?(/[\S]+)\s+\(0x", result.stdout):
            selected = Path(match[1]).resolve(strict=True)
            if not (selected.is_relative_to(root) or str(selected).startswith("/nix/store/")):
                raise ValueError(f"{name} still loads a non-deployment library: {selected}")
            libraries[str(selected)] = (
                {"sha256": sha256(selected)} if selected.is_relative_to(root)
                else {"store_path": str(store_root(selected))})
        if "not found" in result.stdout:
            raise ValueError(f"unresolved native runtime dependency: {name}")
        evidence[name] = {"linkage": "dynamic", "loader": str(loader), "loader_store_path": str(store_root(loader)),
                          "libraries": libraries,
                          "stdout": re.sub(r"0x[0-9a-f]+", "<address>", result.stdout)}
    return evidence


def assemble(args) -> None:
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    (root / "bin").mkdir(exist_ok=True)
    (root / "lib/tidepool").mkdir(parents=True, exist_ok=True)
    shared = root / "share/exomonad"
    shared.mkdir(parents=True, exist_ok=True)
    for name, source in (("exomonad-unwrapped", args.host), ("exomonad-view-helper", args.view_helper),
                         ("tidepool-extract", args.frontend), ("tidepool-extract-bin", args.worker)):
        shutil.copy2(source.resolve(strict=True), root / "bin" / name)
    for name, source in (("stdlib", args.sources / "lib"), ("actors", args.sources / "actors"), ("web", args.assets / "web")):
        shutil.copytree(source, shared / name, symlinks=False)
    for source in args.libraries.iterdir():
        shutil.copy2(source.resolve(strict=True), root / "lib/tidepool" / source.name)
    shutil.copy2(args.harness_revision, shared / "harness-source-revision.txt")
    tools, ghc = nix_path(args.runtime_tools), nix_path(args.ghc_libdir)
    (shared / "runtime-tools").symlink_to(tools)
    (shared / "ghc-libdir.txt").write_text(str(ghc) + "\n")
    wrapper = root / "bin/exomonad"
    wrapper.write_text(f"#!{tools}/bin/bash\n" + args.entrypoint_template.read_text().split("\n", 1)[1])
    wrapper.chmod(0o755)
    write_json(shared / "native-build-contract.json", {
        "profile": args.profile, "feature_profile": "embedded-native", "stdlib_mode": "source-backed",
        "targets": ["//bridge/facade:exomonad", "//bridge/facade:exomonad-view-helper",
                    "//tidepool/extract-cmd:tidepool-extract", "//bridge/haskell:tidepool_extract_bin"],
    })


def freeze(args) -> Path:
    source = args.source_root.resolve(strict=True)
    oid = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if subprocess.check_output(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=no", "--ignore-submodules=untracked"], text=True).strip():
        raise ValueError("commit tracked source changes before freezing qualification")
    submodules = []
    for row in subprocess.check_output(["git", "-C", str(source), "submodule", "status", "--recursive"], text=True).splitlines():
        if row[:1] != " ":
            raise ValueError(f"recorded source submodule must be initialized at its pinned commit: {row}")
        revision, relative = row[1:].split()[:2]
        selected = source / relative
        if subprocess.check_output(["git", "-C", str(selected), "status", "--porcelain", "--untracked-files=no", "--ignore-submodules=untracked"], text=True).strip():
            raise ValueError(f"recorded source submodule has tracked changes: {relative}")
        submodules.append({"path": relative, "source_oid": revision})
    contract = json.loads((args.bundle / "share/exomonad/native-build-contract.json").read_text())
    if contract["feature_profile"] != "embedded-native" or contract["stdlib_mode"] != "source-backed":
        raise ValueError("qualification requires the native source-backed bundle")
    lock = tomllib.loads((source / "Cargo.lock").read_text())
    harness = [row for row in lock["package"] if row["name"] == "harness"]
    if len(harness) != 1:
        raise ValueError("expected one exact Harness dependency")
    matched = re.fullmatch(r"git\+https://github\.com/tidepool-heavy-industries/exomonad-harness\.git\?rev=([0-9a-f]{40})#\1", harness[0]["source"])
    if matched is None:
        raise ValueError("Harness Cargo pin must match its exact resolved revision")
    revision = matched[1]
    if (args.bundle / "share/exomonad/harness-source-revision.txt").read_text().strip() != revision:
        raise ValueError("bundle web/host Harness provenance differs from the source pin")
    commands = json.loads(args.build_commands.read_text())
    if not isinstance(commands, list) or not commands or any(not isinstance(command, list) or not command or not all(isinstance(word, str) for word in command) for command in commands):
        raise ValueError("--build-commands must be a JSON array of actual argv arrays")
    root = args.output.absolute()
    root.mkdir(parents=True, exist_ok=False)
    # Dereference Buck source trees; retain only the declared Nix tool closure.
    shutil.copytree(args.bundle, root, symlinks=False, dirs_exist_ok=True,
                    ignore=lambda directory, names: ["runtime-tools"] if Path(directory).name == "exomonad" else [])
    (root / "share/exomonad/runtime-tools").symlink_to((args.bundle / "share/exomonad/runtime-tools").resolve(strict=True))
    shutil.copy2(args.libtest.resolve(strict=True), root / "bin/tidepool-tests")
    shutil.copytree(args.browser_driver, root / "share/exomonad/browser-driver", symlinks=False)
    shutil.copy2(source / "build/rust/isolated-libtest.py", root / "share/exomonad/isolated-libtest.py")
    shutil.copy2(source / "build/package/qualification.py", root / "share/exomonad/qualification.py")
    environment = native_environment(root)
    environment.update(TIDEPOOL_BROWSER_NODE=str(nix_path(args.browser_node)),
                       TIDEPOOL_BROWSER_DRIVER=str(root / "share/exomonad/browser-driver/driver.mjs"),
                       PLAYWRIGHT_BROWSERS_PATH=str(nix_path(args.playwright_browsers)))
    compiler = root / "share/exomonad/compiler-deployment.json"
    subprocess.run([environment["TIDEPOOL_EXTRACT"], "--compiler-deployment-manifest", str(compiler)],
                   env=execution_environment({"environment": environment}), check=True, timeout=60)
    authority = json.loads(compiler.read_text())
    for key, selected in (("frontend_path", environment["TIDEPOOL_EXTRACT"]), ("worker_path", environment["TIDEPOOL_EXTRACT_WORKER"]), ("ghc_libdir", environment["TIDEPOOL_GHC_LIBDIR"])):
        if authority[key] != selected:
            raise ValueError(f"compiler manifest does not bind its final deployment path: {key}")
    external = {}
    for key in ("TIDEPOOL_GHC_LIBDIR", "TIDEPOOL_BROWSER_NODE", "PLAYWRIGHT_BROWSERS_PATH"):
        path = nix_path(Path(environment[key]))
        external[key] = {"path": str(path), "store_path": str(store_root(path))}
    tools = (root / "share/exomonad/runtime-tools").resolve(strict=True)
    external["runtime_tools"] = {"path": str(tools), "store_path": str(store_root(tools))}
    evidence = loader_evidence(root, execution_environment({"environment": environment}))
    selected = [Path(item["path"]) for item in external.values()]
    for item in evidence.values():
        if item["linkage"] == "dynamic":
            selected.append(Path(item["loader"]))
            selected.extend(Path(path) for path in item["libraries"] if path.startswith("/nix/store/"))
    closure, gc_roots = pin_nix_closure(root, tools, selected)
    copied_log = root / "share/exomonad/build.log"
    shutil.copy2(args.build_log, copied_log)
    descriptor = {
        "schema": 1, "kind": "native-runtime-qualification", "bundle_root": str(root),
        "source_oid": oid, "harness_revision": revision, **contract,
        "source_submodules": submodules,
        "source_inputs": {name: sha256(source / name) for name in ("Cargo.toml", "Cargo.lock", "flake.nix", "flake.lock", "scripts/native-profile.toml")},
        "build": {"commands": commands, "log": str(copied_log), "log_sha256": sha256(copied_log)},
        "environment": environment, "external_inputs": external, "elf_runtime": evidence,
        "nix_closure": closure, "gc_roots": gc_roots,
        "programs": {"libtest": str(root / "bin/tidepool-tests"), "runner": str(root / "share/exomonad/isolated-libtest.py"),
                     "host": str(root / "bin/exomonad"), "host_elf": str(root / "bin/exomonad-unwrapped")},
        "cohorts": {"m2": {"tests": M2_TESTS, "expected_count": 6, "ignored": False, "timeout": 600},
                    "m1": {"tests": M1_TESTS, "expected_count": 1, "ignored": True, "timeout": 900}},
    }
    descriptor["inventory"] = inventory(root, [DESCRIPTOR])
    descriptor["inventory_sha256"] = digest_inventory(descriptor["inventory"])
    destination = root / DESCRIPTOR
    write_json(destination, descriptor)
    verify(destination)
    for path in root.rglob("*"):
        if not path.is_symlink():
            path.chmod(path.stat().st_mode & ~0o222)
    root.chmod(root.stat().st_mode & ~0o222)
    return destination


def verify(path: Path) -> dict:
    descriptor = json.loads(path.read_text())
    root = Path(descriptor["bundle_root"])
    if path.absolute() != root / DESCRIPTOR or descriptor["schema"] != 1 or descriptor["kind"] != "native-runtime-qualification":
        raise ValueError("qualification descriptor was relocated or has an unsupported schema")
    if descriptor["stdlib_mode"] != "source-backed" or descriptor["feature_profile"] != "embedded-native":
        raise ValueError("qualification is not the selected native source mode")
    if inventory(root, [DESCRIPTOR]) != descriptor["inventory"]:
        raise ValueError("frozen runtime inventory changed")
    if digest_inventory(descriptor["inventory"]) != descriptor["inventory_sha256"]:
        raise ValueError("runtime inventory digest mismatch")
    for item in descriptor["external_inputs"].values():
        path = nix_path(Path(item["path"]))
        if str(store_root(path)) != item["store_path"]:
            raise ValueError(f"declared Nix selection changed: {path}")
    if descriptor["external_inputs"]:
        tools = Path(descriptor["external_inputs"]["runtime_tools"]["path"])
        if nix_metadata(tools, descriptor["nix_closure"]["roots"]) != descriptor["nix_closure"]:
            raise ValueError("declared immutable Nix closure identity changed")
        for pinned in descriptor["gc_roots"]:
            if Path(pinned["path"]).resolve(strict=True) != Path(pinned["store_path"]):
                raise ValueError("deployment Nix GC root was removed or changed")
    if loader_evidence(root, execution_environment(descriptor)) != descriptor["elf_runtime"]:
        raise ValueError("native runtime loader dependency resolution changed")
    return descriptor


def run_cohort(args) -> int:
    descriptor = verify(args.descriptor.absolute())
    cohort = descriptor["cohorts"][args.cohort]
    output = args.output.absolute()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    command = [sys.executable, descriptor["programs"]["runner"], descriptor["programs"]["libtest"],
               "--expected-count", str(cohort["expected_count"]), "--jobs", "1", "--timeout", str(cohort["timeout"]),
               "--output-dir", str(output / "tests")]
    for name in cohort["tests"]:
        command.extend(["--exact", name])
    if cohort["ignored"]:
        command.append("--ignored")
    started = time.monotonic_ns()
    with (output / "runner.stdout").open("wb") as stdout, (output / "runner.stderr").open("wb") as stderr:
        result = subprocess.run(command, env=execution_environment(descriptor), stdout=stdout, stderr=stderr, check=False)
    records = [json.loads(path.read_text()) for path in sorted((output / "tests").glob("*.json"))]
    exact = {record["test"] for record in records} == set(cohort["tests"])
    confirmed = exact and len(records) == cohort["expected_count"] and all(record["passed"] and record["execution"]["executed_test_count"] == 1 and record["execution"]["exit_code"] == 0 for record in records)
    code = result.returncode if result.returncode else int(not confirmed)
    report = {"schema": 1, "descriptor": str(args.descriptor.absolute()), "descriptor_sha256": sha256(args.descriptor),
              "source_oid": descriptor["source_oid"], "harness_revision": descriptor["harness_revision"],
              "profile": descriptor["profile"], "stdlib_mode": descriptor["stdlib_mode"], "cohort": args.cohort,
              "command": command, "exit_code": code, "runner_exit_code": result.returncode,
              "elapsed_ns": time.monotonic_ns() - started, "expected_count": cohort["expected_count"],
              "executed_test_count": sum((record.get("execution") or {}).get("executed_test_count") or 0 for record in records),
              "unknown_execution_count": sum((record.get("execution") or {}).get("executed_test_count") is None for record in records),
              "completed": code == 0, "tests": records}
    write_json(output / "report.json", report)
    return code


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser("assemble")
    for key in ("output", "host", "view-helper", "frontend", "worker", "sources", "assets", "libraries", "harness-revision", "runtime-tools", "ghc-libdir", "entrypoint-template"):
        stage.add_argument("--" + key, required=True, type=Path)
    stage.add_argument("--profile", required=True, choices=("fast-dev", "debug", "production"))
    frozen = commands.add_parser("freeze")
    for key in ("bundle", "output", "source-root", "libtest", "browser-driver", "browser-node", "playwright-browsers", "build-log", "build-commands"):
        frozen.add_argument("--" + key, required=True, type=Path)
    checked = commands.add_parser("verify")
    checked.add_argument("descriptor", type=Path)
    run = commands.add_parser("run")
    run.add_argument("descriptor", type=Path)
    run.add_argument("--cohort", required=True, choices=("m2", "m1"))
    run.add_argument("--output", required=True, type=Path)
    live = commands.add_parser("exec")
    live.add_argument("descriptor", type=Path)
    live.add_argument("--report", required=True, type=Path)
    live.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    try:
        if args.command == "assemble":
            assemble(args)
        elif args.command == "freeze":
            print(freeze(args))
        elif args.command == "verify":
            verify(args.descriptor.absolute())
        elif args.command == "run":
            return run_cohort(args)
        elif args.command == "exec":
            descriptor = verify(args.descriptor.absolute())
            arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
            if not arguments or any(flag in arguments for flag in ("--help", "-h", "--version", "-V")):
                raise ValueError("live execution requires an actual package operation")
            command = [descriptor["programs"]["host"], *arguments]
            started = time.monotonic_ns()
            result = subprocess.run(command, env=execution_environment(descriptor), check=False)
            write_json(args.report, {"schema": 1, "descriptor_sha256": sha256(args.descriptor), "command": command,
                                     "source_oid": descriptor["source_oid"], "harness_revision": descriptor["harness_revision"],
                                     "profile": descriptor["profile"], "stdlib_mode": descriptor["stdlib_mode"],
                                     "process_execution_count": 1, "executed_test_count": None,
                                     "exit_code": result.returncode, "elapsed_ns": time.monotonic_ns() - started})
            return result.returncode
        return 0
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"native qualification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
