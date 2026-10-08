#!/usr/bin/env python3
"""Freeze and qualify one native Exomonad deployment at its final path."""
from __future__ import annotations

import argparse
from enum import Enum
import hashlib
import json
import os
from pathlib import Path
from pathlib import PurePosixPath
import re
import shutil
import shlex
import struct
import subprocess
import sys
import tempfile
import time
import tomllib

M2_SURVIVAL_TEST = "actor_host::embedded_captured_unfold_tests::embedded_captured_children_and_capture_survive_failure_of_the_unfinished_parent_cell"
M2_NOMINAL_JOIN_TEST = "actor_host::embedded_captured_unfold_tests::embedded_same_root_parked_nominal_a_joins_later_b_publication"
M2_CHECKPOINT_RELEASE_TEST = "actor_host::embedded_checkpoint_release_tests::released_checkpoint_keeps_an_admitted_childs_hosted_context"
M2_SELECTED_CODING_TEST = "actor_host::fresh_child_tests::scaffolded_selected_coding_child_preserves_workspace_input_and_effect_row"
M2_TESTS = [
    "actor_host::embedded_captured_unfold_tests::admitted_cell_late_type_error_has_no_effect_or_publication_on_retry",
    "actor_host::embedded_captured_unfold_tests::embedded_captured_unfold_awaits_two_child_replies_before_parent_call_returns",
    M2_SURVIVAL_TEST,
    M2_NOMINAL_JOIN_TEST,
    "actor_host::embedded_captured_unfold_tests::embedded_parked_captured_pipeline_cancellation_settles_invocation_owned_children",
    M2_CHECKPOINT_RELEASE_TEST,
    M2_SELECTED_CODING_TEST,
]
M1_TESTS = ["actor_host::m1_host_tests::production_browser_executes_resident_haskell_retries_and_controls_root"]
PREPARED_CHILD_TESTS = [
    "actor_host::prepared_runtime_acceptance::production_prepared_toolset_twenty_children_execute_original_native_probe",
]
DESCRIPTOR = "share/exomonad/qualification.json"
UNSET_ENVIRONMENT = (
    "TIDEPOOL_CACHE_DIR", "TIDEPOOL_COMPILE_CACHE_DIR", "TIDEPOOL_BUILD_PRODUCTS_DIR",
    "TIDEPOOL_EXTRACT_DAEMON_SOCKET", "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT", "TIDEPOOL_COMPILER_MODULES",
    "TIDEPOOL_COMPILER_DEPLOYMENT", "TIDEPOOL_EXTRACT_WORKER", "TIDEPOOL_PREPARED_ROOT_ENTRY",
    "TIDEPOOL_EXTRACT", "TIDEPOOL_PRELUDE_DIR", "TIDEPOOL_GHC_LIBDIR",
    "TIDEPOOL_TEST_SYSTEMD_RUN", "TIDEPOOL_TEST_SYSTEMCTL",
    "TIDEPOOL_TEST_FIXTURE_ROOT",
    "EXOMONAD_EMBEDDED_ASSET_ROOT", "EXOMONAD_WORKSPACE_GITLINK", "EXOMONAD_WORKSPACE_GIT_BUNDLE", "EXOMONAD_NIX_BIN", "EXOMONAD_NIX_OFFLINE", "LD_LIBRARY_PATH", "TIDEPOOL_EXTRACT_NO_DAEMON",
)
ARTIFACT_TARGETS = {
    "bin/exomonad-unwrapped": "//bridge/facade:exomonad",
    "bin/exomonad-view-helper": "//bridge/facade:exomonad-view-helper",
    "bin/tidepool-extract": "//tidepool/extract-cmd:tidepool-extract",
    "bin/tidepool-extract-bin": "//bridge/haskell:tidepool_extract_bin",
    "bin/tidepool-tests": "//bridge/facade:tidepool_unit_tests",
}
EXTERNAL_INPUTS = {"TIDEPOOL_GHC_LIBDIR", "TIDEPOOL_BROWSER_NODE", "PLAYWRIGHT_BROWSERS_PATH", "runtime_tools"}
BUILD_CONTRACT_FIELDS = ("profile", "feature_profile", "stdlib_mode", "startup_mode", "source_inputs", "source_inputs_sha256", "artifacts")
CATALOG_SOURCE_METADATA = "catalog-sources.json"
CATALOG_SOURCE_ROOTS = {"effects": "effects", "stdlib": "lib", "actors": "actors", "jev": "jev/core"}
RETAINED_CATALOG_SOURCES = "share/exomonad/retained-catalog-sources.json"
NATIVE_CATALOG_BUILD = "native-catalog-build.json"
NATIVE_ROOT_ENTRY_BUILD = "native-root-entry-build.json"
NATIVE_SOURCE_ROLES = ["stable_effects", "stdlib", "actors", "jev"]
ROOT_ENTRY_SOURCE = "TidepoolPreparedDriver.hs"
ROOT_ENTRY_SOURCE_OWNER = "//tidepool/runtime:tidepool-entry-source"
ROOT_ENTRY_GENERATOR_SOURCE = "tidepool/runtime/src/bin/tidepool-entry-source.rs"
CATALOG_TEST = "actor_host::packaged_catalog_tests::packaged_cohort_executes_and_displays_without_build_inputs"
REQUIRED_RUNTIME_EXECUTABLES = (
    "bash", "python3", "dirname", "git", "bwrap", "tmux", "systemd-run", "systemctl", "nix", "nix-store",
    "rg", "find",
)

def programs(root: Path) -> dict:
    return {"libtest": str(root / "bin/tidepool-tests"),
            "runner": str(root / "share/exomonad/isolated-libtest.py"),
            "host": str(root / "bin/exomonad"), "host_elf": str(root / "bin/exomonad-unwrapped")}

def cohorts() -> dict:
    return {"m2": {"tests": M2_TESTS, "expected_count": len(M2_TESTS), "ignored": False, "timeout": 600,
                   "case_timeouts": {M2_SURVIVAL_TEST: 900, M2_NOMINAL_JOIN_TEST: 900,
                                     M2_CHECKPOINT_RELEASE_TEST: 900,
                                     M2_SELECTED_CODING_TEST: 900}},
            "m1": {"tests": M1_TESTS, "expected_count": 1, "ignored": True, "timeout": 900},
            "prepared-child": {"tests": PREPARED_CHILD_TESTS, "expected_count": 1,
                               "ignored": True, "timeout": 1800,
                               "compiler_mode": "owned-resident", "max_jobs": 1}}

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


def native_runtime_tools(path: Path) -> Path:
    """Validate the executable closure used by the supported native runtime."""
    tools = nix_path(path)
    for name in REQUIRED_RUNTIME_EXECUTABLES:
        executable = tools / "bin" / name
        if not executable.is_file() or not os.access(executable, os.X_OK):
            raise ValueError(f"declared native runtime tools lack executable {name}: {executable}")
        nix_path(executable)
    return tools

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
    verify_registered_gc_roots(tools, pinned)
    return nix_metadata(tools, roots), pinned

def catalog_source_inventory(root: Path) -> dict:
    """Retained source names are original evidence, so aliases are forbidden."""
    if root != root.resolve(strict=True):
        raise ValueError("catalog sources require their canonical original path")
    entries = inventory(root)
    if any(item["kind"] == "symlink" for item in entries.values()):
        raise ValueError("catalog sources must be regular files without aliases")
    return entries

def catalog_source_metadata(root: Path) -> dict:
    metadata = json.loads((root / CATALOG_SOURCE_METADATA).read_text())
    if (not isinstance(metadata, dict)
            or set(metadata) != {"schema", "kind", "components", "modules", "roots", "probe", "targets", "root_entry"}
            or metadata["schema"] != 1 or metadata["kind"] != "native-catalog-source-snapshot"
            or metadata["roots"] != CATALOG_SOURCE_ROOTS
            or metadata["probe"] != "TidepoolCatalog.hs" or metadata["targets"] != ["catalogSentinel"]
            or not isinstance(metadata["components"], list) or not metadata["components"]
            or any(not isinstance(component, str) or not component for component in metadata["components"])
            or metadata["components"] != sorted(set(metadata["components"]))
            or not isinstance(metadata["modules"], dict) or not metadata["modules"]):
        raise ValueError("invalid native catalog source metadata")
    for module, relative in metadata["modules"].items():
        if not isinstance(module, str) or re.fullmatch(r"[A-Z][A-Za-z0-9_]*(?:\.[A-Z][A-Za-z0-9_]*)*", module) is None:
            raise ValueError("invalid catalog module name")
        if not isinstance(relative, str):
            raise ValueError("invalid catalog module path")
        path = Path(relative)
        owners = [Path(relative_root) for relative_root in CATALOG_SOURCE_ROOTS.values()
                  if path.is_relative_to(relative_root)]
        module_path = Path(*module.split(".")).with_suffix(".hs")
        if (path.is_absolute() or len(owners) != 1
                or path.relative_to(owners[0]) != module_path
                or not (root / path).is_file()):
            raise ValueError(f"catalog module lacks its declared original source: {module}")
    for relative in CATALOG_SOURCE_ROOTS.values():
        if not (root / relative).is_dir():
            raise ValueError(f"missing catalog source role: {relative}")
    if (root / metadata["probe"]).read_text() != catalog_probe(metadata["modules"]):
        raise ValueError("catalog probe differs from its declared module cohort")
    if metadata["root_entry"] != root_entry_source_record(root):
        raise ValueError("root entry differs from its declared generated source bytes")
    return metadata


def root_entry_source_record(root: Path) -> dict:
    return {"path": ROOT_ENTRY_SOURCE, "producer_target": ROOT_ENTRY_SOURCE_OWNER,
            "generator_source": ROOT_ENTRY_GENERATOR_SOURCE,
            "module": "TidepoolPreparedDriver",
            "entry": "Tidepool.Actors.Internal.ExomonadDriver.rootDriver",
            "effects": "Tidepool.Actors.Internal.ExomonadDriver.RootEffects",
            "sha256": sha256(root / ROOT_ENTRY_SOURCE)}

def catalog_probe(modules: dict) -> str:
    return ("module TidepoolCatalog where\n"
            + "".join(f"import {module} ()\n" for module in sorted(modules))
            + "catalogSentinel :: Int\ncatalogSentinel = 0\n")

def snapshot_catalog_sources(args) -> None:
    """Buck owns the source projection and generators; qualification owns bytes."""
    cohort = json.loads(args.cohort.read_text())
    if (not isinstance(cohort, dict) or set(cohort) != {"components", "modules"}
            or not isinstance(cohort["modules"], dict)):
        raise ValueError("invalid metadata-derived catalog cohort")
    root = args.output.absolute()
    root.mkdir(parents=True, exist_ok=False)
    for relative in ("lib", "actors"):
        shutil.copytree(args.sources / relative, root / relative, symlinks=False)
    shutil.copytree(args.jev_sources / "core", root / "jev/core", symlinks=False)
    (root / "effects").mkdir()
    for relative in cohort["modules"].values():
        if not isinstance(relative, str):
            raise ValueError("invalid catalog module source path")
        path = Path(relative)
        if path.is_absolute() or not path.parts or any(part in (".", "..") for part in path.parts):
            raise ValueError("invalid catalog module source path")
        if path.parts[0] == "effects":
            destination = root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(args.effects.joinpath(*path.parts[1:]), destination)
    (root / "TidepoolCatalog.hs").write_text(catalog_probe(cohort["modules"]))
    shutil.copyfile(args.root_entry_source, root / ROOT_ENTRY_SOURCE)
    write_json(root / CATALOG_SOURCE_METADATA, {
        "schema": 1, "kind": "native-catalog-source-snapshot", **cohort,
        "roots": CATALOG_SOURCE_ROOTS, "probe": "TidepoolCatalog.hs", "targets": ["catalogSentinel"],
        "root_entry": root_entry_source_record(root),
    })
    # NAR identity ignores timestamps. Fixed names and modes make the original
    # retained path stable across actions and runs with the same complete bytes.
    for path in root.rglob("*"):
        path.chmod(0o755 if path.is_dir() else 0o644)
    catalog_source_metadata(root)
    catalog_source_inventory(root)

def retain_catalog_sources(args) -> Path:
    source = args.snapshot.resolve(strict=True)
    catalog_source_metadata(source)
    expected = catalog_source_inventory(source)
    destination = args.output.absolute()
    if destination != destination.resolve():
        raise ValueError("source retention requires a canonical output path")
    destination.mkdir(parents=True, exist_ok=False)
    tools = nix_path(args.runtime_tools)
    # The basename is fixed because it participates in the original store path.
    with tempfile.TemporaryDirectory(prefix="native-catalog-", dir=destination) as temporary:
        staged = Path(temporary) / "tidepool-catalog-sources"
        shutil.copytree(source, staged, symlinks=False)
        if catalog_source_inventory(staged) != expected:
            raise ValueError("catalog snapshot changed during retention")
        retained = Path(subprocess.check_output(
            [str(tools / "bin/nix-store"), "--add", str(staged)], text=True, timeout=60).strip())
    if retained != store_root(retained):
        raise ValueError("source retention did not return one original store root")
    subprocess.run([str(tools / "bin/nix-store"), "--verify-path", str(retained)],
                   check=True, capture_output=True, timeout=60)
    if catalog_source_inventory(retained) != expected:
        raise ValueError("retained source bytes differ from the declared snapshot")
    closure, gc_roots = pin_nix_closure(destination, tools, [retained])
    record = destination / RETAINED_CATALOG_SOURCES
    write_json(record, {
        "schema": 1, "kind": "native-catalog-source-retention", "original_root": str(retained),
        "inventory": expected, "inventory_sha256": digest_inventory(expected),
        "nix_closure": closure, "gc_roots": gc_roots,
    })
    verify_retained_catalog_sources(record, source, tools)
    return retained

def verify_retained_catalog_sources(record: Path, snapshot: Path, tools: Path, *, record_origin: Path | None = None) -> Path:
    """Return the original compiler root only after current retention checks."""
    value = json.loads(record.read_text())
    if (not isinstance(value, dict)
            or set(value) != {"schema", "kind", "original_root", "inventory", "inventory_sha256", "nix_closure", "gc_roots"}
            or value["schema"] != 1 or value["kind"] != "native-catalog-source-retention"):
        raise ValueError("unsupported retained catalog sources")
    original = Path(value["original_root"])
    if original != store_root(original):
        raise ValueError("retained catalog root moved or aliases another path")
    catalog_source_metadata(original)
    expected = catalog_source_inventory(snapshot.resolve(strict=True))
    if (expected != value["inventory"] or digest_inventory(expected) != value["inventory_sha256"]
            or catalog_source_inventory(original) != expected):
        raise ValueError("retained catalog sources differ from the declared action snapshot")
    executable = str(tools / "bin/nix-store")
    subprocess.run([executable, "--verify-path", str(original)], check=True, capture_output=True, timeout=60)
    if value["nix_closure"] != nix_metadata(tools, [str(original)]):
        raise ValueError("retained catalog NAR registration changed")
    pins = value["gc_roots"]
    expected_pin = (record_origin or record).parent / "gc-roots" / hashlib.sha256(str(original).encode()).hexdigest()
    if (len(pins) != 1 or pins[0]["path"] != str(expected_pin)
            or pins[0]["store_path"] != str(original) or expected_pin.resolve(strict=True) != original):
        raise ValueError("retained catalog source GC root is missing or changed")
    verify_registered_gc_roots(tools, pins)
    return original


def verify_registered_gc_roots(tools: Path, pins: list[dict]) -> None:
    """A symlink alone does not establish registration with the Nix collector."""
    expected = set()
    for pinned in pins:
        target = Path(pinned["path"])
        if not target.is_absolute() or target.resolve(strict=True) != Path(pinned["store_path"]):
            raise ValueError("deployment Nix GC root was removed or changed")
        expected.add((str(target), pinned["store_path"]))
    if not expected:
        return
    registered = subprocess.check_output(
        [str(tools / "bin/nix-store"), "--query", "--roots",
         *sorted({store_path for _, store_path in expected})],
        text=True, timeout=60).splitlines()
    pairs = {tuple(line.split(" -> ", 1)) for line in registered}
    if not expected <= pairs:
        raise ValueError("deployment Nix GC root is not registered")

def native_catalog_selection(catalog: Path, original: Path) -> dict:
    value = json.loads(catalog.read_text())
    selection = value.get("source_selection")
    if value.get("schema") != 4:
        raise ValueError("unsupported native catalog schema")
    return native_source_selection(selection, original)


def native_source_selection(selection: dict, original: Path) -> dict:
    if (not isinstance(selection, dict)
            or set(selection) != {"snapshot_root", "roles", "source_files"}
            or selection["snapshot_root"] != str(original)
            or selection["roles"] != NATIVE_SOURCE_ROLES):
        raise ValueError("native catalog does not select the retained ordered source roles")
    files = selection["source_files"]
    expected = [{"path": path, "sha256": item["sha256"]}
                for path, item in sorted(catalog_source_inventory(original).items())
                if item["kind"] == "file" and path.endswith((".hs", ".hs-boot", ".lhs", ".lhs-boot"))]
    if (not isinstance(files, list)
            or any(not isinstance(item, dict) or set(item) != {"path", "sha256"}
                   or not isinstance(item["path"], str) or not isinstance(item["sha256"], str)
                   or re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is None for item in files)
            or files != expected):
        raise ValueError("native catalog source selection lacks the complete source manifest")
    return selection


def native_catalog_products(root: Path, build_record: str = NATIVE_CATALOG_BUILD) -> dict:
    entries = inventory(root, [build_record, "source-retention.json"])
    if any(item["kind"] == "symlink" for item in entries.values()):
        raise ValueError("native catalog products must be regular artifact bytes")
    return entries


def verify_native_catalog(root: Path, tools: Path) -> dict:
    shared = root / "share/exomonad"
    contract = json.loads((shared / "native-build-contract.json").read_text())
    catalog = shared / "catalog/catalog.json"
    receipt = json.loads((shared / "catalog" / NATIVE_CATALOG_BUILD).read_text())
    if (not isinstance(receipt, dict)
            or set(receipt) != {"schema", "kind", "producer_target", "catalog_sha256", "source_selection", "source_inventory_sha256", "retention_record_origin", "product_inventory", "product_inventory_sha256"}
            or receipt["schema"] != 1 or receipt["kind"] != "native-catalog-build"
            or receipt["producer_target"] != "//tidepool/toolchain:tidepool-module-package"):
        raise ValueError("unsupported native catalog build evidence")
    record = shared / RETAINED_CATALOG_SOURCES.split("/")[-1]
    # Before freezing the action's original retention record is selected; after
    # freezing the bundle owns an independently registered collector root.
    if record.exists():
        origin = record
    else:
        record = shared / "catalog/source-retention.json"
        origin = Path(receipt["retention_record_origin"])
    retained = json.loads(record.read_text())
    original = Path(retained["original_root"])
    verify_retained_catalog_sources(record, original, tools, record_origin=origin)
    selection = native_catalog_selection(catalog, original)
    products = native_catalog_products(catalog.parent)
    products_digest = digest_inventory(products)
    if receipt["product_inventory"] != products or receipt["product_inventory_sha256"] != products_digest:
        raise ValueError("native catalog product inventory changed")
    expected = {"catalog_sha256": sha256(catalog), "source_selection": selection,
                "source_inventory_sha256": retained["inventory_sha256"],
                "product_inventory_sha256": products_digest}
    if contract.get("native_catalog") != expected or any(receipt.get(key) != value for key, value in expected.items()):
        raise ValueError("native catalog differs from the owning build source selection")
    return expected


def transfer_catalog_retention(root: Path, contract: dict, gc_roots: list[dict], tools: Path) -> None:
    original = Path(contract["native_catalog"]["source_selection"]["snapshot_root"])
    retention = json.loads((root / "share/exomonad/catalog/source-retention.json").read_text())
    retention["gc_roots"] = [item for item in gc_roots if item["store_path"] == str(original)]
    write_json(root / RETAINED_CATALOG_SOURCES, retention)
    verify_native_catalog(root, tools)


class CatalogProducerOperation(Enum):
    BUILD = "build"
    INSPECT = "inspect"
    ENTRY = "entry"


def invoke_retained_catalog_producer(args, operation: CatalogProducerOperation) -> Path:
    """One source/retention/compiler boundary for production and inspection."""
    if operation is CatalogProducerOperation.INSPECT and not 600 <= args.timeout <= 1800:
        raise ValueError("inspection timeout must be between 600 and 1800 seconds")
    tools = nix_path(args.runtime_tools)
    original = verify_retained_catalog_sources(args.retention_record, args.snapshot, tools,
                                                record_origin=args.retention_record_origin)
    if args.source_root != original or catalog_source_inventory(args.declared_source_root.resolve(strict=True)) != catalog_source_inventory(original):
        raise ValueError("configured catalog root differs from the complete declared source snapshot")
    output = args.output.absolute()
    if output != output.resolve() or output.exists():
        raise ValueError("native catalog requires an absent canonical action output")
    declared_environment = {
        "TIDEPOOL_EXTRACT": str(args.frontend.resolve(strict=True)),
        "TIDEPOOL_EXTRACT_WORKER": str(args.worker.resolve(strict=True)),
        "TIDEPOOL_COMPILER_DEPLOYMENT": str(args.deployment.resolve(strict=True)),
        "TIDEPOOL_GHC_LIBDIR": str(nix_path(args.ghc_libdir)),
        "LD_LIBRARY_PATH": str(args.libraries.resolve(strict=True)),
        "PATH": str(tools / "bin"),
    }
    environment = execution_environment({"environment": declared_environment})
    probe, target = ((ROOT_ENTRY_SOURCE, "__prepared")
                     if operation is CatalogProducerOperation.ENTRY else ("TidepoolCatalog.hs", "catalogSentinel"))
    command = [str(args.producer.resolve(strict=True)), operation.value, "--source", str(original / probe),
               "--target", target, "--source-root", str(original), "--output-root", str(output)]
    invocation_root = output.with_name(output.name + ".invocation") if operation is CatalogProducerOperation.INSPECT else None
    primary = None
    secondary = None
    return_code = None
    started = time.monotonic_ns()
    invocation = {
        "schema": 1, "kind": "native-catalog-producer-inspection",
        "command": command, "declared_environment": declared_environment,
        "unset_environment": UNSET_ENVIRONMENT, "output_root": str(output),
        "retention_record": str(args.retention_record),
        "retention_record_origin": str(args.retention_record_origin),
        "source_inventory_sha256": json.loads(args.retention_record.read_text())["inventory_sha256"],
        "catalog_qualified": False,
    }
    if invocation_root is not None:
        invocation_root.mkdir(parents=True, exist_ok=False)
        invocation.update(timeout_seconds=args.timeout, cwd=str(invocation_root), stdout=str(invocation_root / "stdout.log"),
                          stderr=str(invocation_root / "stderr.log"))
        write_json(invocation_root / "invocation.json", invocation)
    try:
        if invocation_root is None:
            result = subprocess.run(command, env=environment, check=False)
        else:
            # The extractor owns child lifetime hooks. subprocess.run kills
            # and waits for its producer on timeout; do not duplicate that
            # process-tree policy in this invocation owner.
            with (invocation_root / "stdout.log").open("wb") as stdout, (invocation_root / "stderr.log").open("wb") as stderr:
                result = subprocess.run(command, env=environment, check=False,
                                        stdout=stdout, stderr=stderr, timeout=args.timeout, cwd=invocation_root)
        return_code = result.returncode
        if return_code != 0:
            primary = subprocess.CalledProcessError(return_code, command)
    except BaseException as error:
        primary = error
    finally:
        # A refusal or timeout does not waive custody of the consumed originals.
        try:
            verify_retained_catalog_sources(args.retention_record, args.snapshot, tools,
                                           record_origin=args.retention_record_origin)
        except Exception as error:
            secondary = error
        if invocation_root is not None:
            write_json(invocation_root / "outcome.json", {
                **invocation, "producer_exit_code": return_code,
                "timed_out": isinstance(primary, subprocess.TimeoutExpired),
                "producer_error": str(primary) if primary is not None else None,
                "retention_recheck": {"passed": secondary is None,
                                      "error": str(secondary) if secondary is not None else None},
                "completed": primary is None and secondary is None,
                "elapsed_ns": time.monotonic_ns() - started,
            })
    if primary is not None:
        if secondary is not None:
            raise ValueError(f"producer failed: {primary}; retention recheck failed: {secondary}") from primary
        raise primary
    if secondary is not None:
        raise secondary
    return original


def inspect_native_catalog(args) -> Path:
    invoke_retained_catalog_producer(args, CatalogProducerOperation.INSPECT)
    return args.output.absolute().with_name(args.output.name + ".invocation")


def build_native_catalog(args) -> None:
    original = invoke_retained_catalog_producer(args, CatalogProducerOperation.BUILD)
    output = args.output.absolute()
    selection = native_catalog_selection(output / "catalog.json", original)
    retained = json.loads(args.retention_record.read_text())
    shutil.copy2(args.retention_record, output / "source-retention.json")
    products = native_catalog_products(output)
    write_json(output / NATIVE_CATALOG_BUILD, {
        "schema": 1, "kind": "native-catalog-build",
        "producer_target": "//tidepool/toolchain:tidepool-module-package",
        "catalog_sha256": sha256(output / "catalog.json"), "source_selection": selection,
        "source_inventory_sha256": retained["inventory_sha256"],
        "retention_record_origin": str(args.retention_record_origin),
        "product_inventory": products, "product_inventory_sha256": digest_inventory(products),
    })


def root_entry_selection(entry: Path, original: Path) -> dict:
    manifest = json.loads(entry.read_text())
    if (manifest.get("schema") != 2 or manifest.get("purpose") != "original_source"
            or manifest.get("target") != "__prepared"
            or manifest.get("source") != str(original / ROOT_ENTRY_SOURCE)):
        raise ValueError("root entry does not retain the declared original settled driver")
    sources = manifest.get("sources")
    if not isinstance(sources, dict) or set(sources) != {"kind", "selection"} or sources["kind"] != "native_catalog":
        raise ValueError("root entry requires the retained native source selection")
    return native_source_selection(sources["selection"], original)


def build_native_root_entry(args) -> None:
    original = invoke_retained_catalog_producer(args, CatalogProducerOperation.ENTRY)
    output = args.output.absolute()
    selection = root_entry_selection(output / "entry.json", original)
    retained = json.loads(args.retention_record.read_text())
    shutil.copy2(args.retention_record, output / "source-retention.json")
    products = native_catalog_products(output, NATIVE_ROOT_ENTRY_BUILD)
    write_json(output / NATIVE_ROOT_ENTRY_BUILD, {
        "schema": 1, "kind": "native-root-entry-build",
        "producer_target": "//tidepool/toolchain:tidepool-module-package",
        "manifest_sha256": sha256(output / "entry.json"), "source_selection": selection,
        "source_inventory_sha256": retained["inventory_sha256"],
        "product_inventory": products, "product_inventory_sha256": digest_inventory(products),
    })


def verify_native_root_entry(root: Path, catalog: dict) -> dict:
    directory = root / "share/exomonad/root-entry"
    receipt = json.loads((directory / NATIVE_ROOT_ENTRY_BUILD).read_text())
    selection = root_entry_selection(directory / "entry.json", Path(catalog["source_selection"]["snapshot_root"]))
    products = native_catalog_products(directory, NATIVE_ROOT_ENTRY_BUILD)
    expected = {"manifest_sha256": sha256(directory / "entry.json"), "source_selection": selection,
                "source_inventory_sha256": catalog["source_inventory_sha256"],
                "product_inventory_sha256": digest_inventory(products)}
    if (not isinstance(receipt, dict)
            or set(receipt) != {"schema", "kind", "producer_target", "manifest_sha256", "source_selection", "source_inventory_sha256", "product_inventory", "product_inventory_sha256"}
            or receipt["schema"] != 1 or receipt["kind"] != "native-root-entry-build"
            or receipt["producer_target"] != "//tidepool/toolchain:tidepool-module-package"
            or receipt["product_inventory"] != products
            or any(receipt.get(key) != value for key, value in expected.items())
            or selection != catalog["source_selection"]):
        raise ValueError("prepared root entry differs from the complete retained source and artifact selection")
    contract = json.loads((root / "share/exomonad/native-build-contract.json").read_text())
    if contract.get("startup_mode") != "prepared" or contract.get("native_root_entry") != expected:
        raise ValueError("prepared root entry differs from the owning build contract")
    return expected


def declared_haskell_sources(source: Path, bundle: Path, original_sources: Path | None = None,
                             generated_source: dict | None = None) -> dict:
    tracked = subprocess.check_output([
        "git", "-C", str(source), "ls-files", "-z", "--",
        "bridge/haskell/lib", "bridge/haskell/actors",
    ]).decode().split("\0")
    evidence = {}
    if original_sources is not None:
        declared = catalog_source_metadata(original_sources)["root_entry"]
        if generated_source != declared:
            raise ValueError("generated root source is absent from the owning native build contract")
    for relative, packaged in (("bridge/haskell/lib", "stdlib"), ("bridge/haskell/actors", "actors")):
        expected = {
            Path(path).relative_to(relative).as_posix(): sha256(source / path)
            for path in tracked if path.startswith(relative + "/") and path.endswith(".hs")
            and "/Prelude_cbor/" not in path
        }
        actual_root = (original_sources / Path(relative).name if original_sources is not None
                       else bundle / "share/exomonad" / packaged)
        actual = {path.relative_to(actual_root).as_posix(): sha256(path)
                  for path in actual_root.rglob("*") if path.is_file()}
        if not expected or actual != expected:
            raise ValueError(f"bundle {packaged} does not contain exactly the tracked declared Haskell source bytes")
        evidence[packaged] = expected
    return evidence

def verify_build_contract(source: Path, bundle: Path, contract: dict, expected_profile: str) -> None:
    if contract["profile"] != expected_profile:
        raise ValueError("native bundle profile differs from the requested qualification profile")
    inputs = contract["source_inputs"]
    if not inputs or digest_inventory(inputs) != contract["source_inputs_sha256"]:
        raise ValueError("native build source manifest is missing or has an invalid digest")
    tracked = set(subprocess.check_output([
        "git", "-C", str(source), "ls-files", "-z", "--recurse-submodules",
    ]).decode().split("\0"))
    for relative, expected in inputs.items():
        path = Path(relative)
        if path.is_absolute() or ".." in path.parts or relative not in tracked:
            raise ValueError(f"native build input is not tracked in the recorded source: {relative}")
        if sha256(source / relative) != expected:
            raise ValueError(f"native bundle was built from different source bytes: {relative}")
    generated = contract.get("generated_root_source")
    if contract["startup_mode"] == "prepared":
        if (not isinstance(generated, dict) or generated.get("producer_target") != ROOT_ENTRY_SOURCE_OWNER
                or generated.get("generator_source") != ROOT_ENTRY_GENERATOR_SOURCE
                or ROOT_ENTRY_GENERATOR_SOURCE not in inputs):
            raise ValueError("prepared root source lacks its declared generator build input")
    elif generated is not None:
        raise ValueError("unprepared bundle cannot declare a generated prepared root source")
    verify_artifact_contract(bundle, contract)

def verify_artifact_contract(bundle: Path, contract: dict) -> None:
    if set(contract["artifacts"]) != set(ARTIFACT_TARGETS):
        raise ValueError("native build contract does not own every qualified executable")
    for relative, target in ARTIFACT_TARGETS.items():
        artifact = contract["artifacts"][relative]
        if artifact["target"] != target or artifact["sha256"] != sha256(bundle / relative):
            raise ValueError(f"mixed native build artifact: {relative}")

TEST_FIXTURE_MANIFEST = "build/test-fixtures.json"
TEST_FIXTURE_ROOT = "share/exomonad/test-fixtures"

def read_fixture_manifest(path: Path) -> dict:
    manifest = json.loads(path.read_text())
    if (not isinstance(manifest, dict) or set(manifest) != {"schema", "kind", "files"}
            or manifest["schema"] != 1 or manifest["kind"] != "haskell-test-fixtures"
            or not isinstance(manifest["files"], list) or not manifest["files"]
            or any(not isinstance(name, str) for name in manifest["files"])
            or manifest["files"] != sorted(set(manifest["files"]))):
        raise ValueError("invalid Haskell test fixture manifest")
    for relative in manifest["files"]:
        path = PurePosixPath(relative)
        if (not relative or "\\" in relative or path.is_absolute() or not path.parts
                or path.as_posix() != relative or any(part in (".", "..") for part in path.parts)
                or not relative.endswith(".hs")):
            raise ValueError(f"fixture is not a repository-relative Haskell source: {relative}")
    return manifest

def fixture_manifest(source: Path) -> tuple[dict, dict[str, str]]:
    """Validate the tracked source roster and hash its current file bytes."""
    source = source.resolve(strict=True)
    if not source.is_dir():
        raise ValueError("fixture source root must be a directory")
    manifest_path = source / TEST_FIXTURE_MANIFEST
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("fixture manifest must be a regular source file")
    manifest = read_fixture_manifest(manifest_path)
    tracked = set(subprocess.check_output([
        "git", "-C", str(source), "ls-files", "-z", "--recurse-submodules",
    ]).decode().split("\0"))
    submodules = []
    for row in subprocess.check_output(["git", "-C", str(source), "submodule", "status", "--recursive"], text=True).splitlines():
        if row[:1] != " ":
            continue
        revision, relative = row[1:].split()[:2]
        submodules.append((Path(relative), revision))
    hashes = {}
    for relative in manifest["files"]:
        path = PurePosixPath(relative)
        if (not relative or "\\" in relative or path.is_absolute() or not path.parts
                or path.as_posix() != relative or any(part in (".", "..") for part in path.parts)
                or not relative.endswith(".hs")):
            raise ValueError(f"fixture is not a repository-relative Haskell source: {relative}")
        selected = source / path
        is_tracked = relative in tracked
        if not is_tracked:
            owners = [(submodule, revision) for submodule, revision in submodules
                      if path.is_relative_to(submodule)]
            if owners:
                submodule, _ = max(owners, key=lambda item: len(item[0].parts))
                inner = path.relative_to(submodule).as_posix()
                owner = source / submodule
                submodule_tracked = set(subprocess.check_output([
                    "git", "-C", str(owner), "ls-files", "-z",
                ]).decode().split("\0"))
                is_tracked = inner in submodule_tracked
        if not is_tracked:
            raise ValueError(f"fixture is not tracked by the recorded source or an initialized submodule: {relative}")
        component = source
        symlinked = False
        for part in path.parts:
            component = component / part
            symlinked = symlinked or component.is_symlink()
        if (not selected.is_file() or not selected.resolve(strict=True).is_relative_to(source)
                or symlinked):
            raise ValueError(f"fixture source is missing or aliased: {relative}")
        hashes[relative] = sha256(selected)
    if TEST_FIXTURE_MANIFEST not in tracked or (source / TEST_FIXTURE_MANIFEST).is_symlink():
        raise ValueError("fixture manifest must be tracked source")
    return manifest, hashes

def copy_test_fixtures(source_inputs: Path, manifest_path: Path, fixture_root: Path, shared: Path) -> dict:
    """Admit declared resource bytes only when they match the native source snapshot."""
    manifest = read_fixture_manifest(manifest_path)
    expected_manifest_hash = source_inputs / TEST_FIXTURE_MANIFEST
    if not expected_manifest_hash.is_file() or sha256(expected_manifest_hash) != sha256(manifest_path):
        raise ValueError("fixture manifest differs from the declared native source snapshot")
    fixture_root = fixture_root.resolve(strict=True)
    if not fixture_root.is_dir():
        raise ValueError("declared fixture resource is not a directory")
    files = {}
    for relative in manifest["files"]:
        path = PurePosixPath(relative)
        selected = fixture_root.joinpath(*path.parts)
        source = source_inputs.joinpath(*path.parts)
        if not selected.is_file() or not source.is_file():
            raise ValueError(f"declared fixture is absent from runtime resources or source snapshot: {relative}")
        files[relative] = sha256(selected)
        if files[relative] != sha256(source):
            raise ValueError(f"runtime fixture differs from native source snapshot: {relative}")
    actual = set()
    for current, directories, names in os.walk(fixture_root, followlinks=False):
        current_path = Path(current)
        if any((current_path / name).is_symlink() for name in directories):
            raise ValueError("runtime fixture tree contains a directory alias")
        actual.update((current_path / name).relative_to(fixture_root).as_posix()
                      for name in names if (current_path / name).is_file())
    if actual != set(files):
        raise ValueError("runtime fixture tree differs from its manifest")
    destination = shared / "test-fixtures"
    shutil.copytree(fixture_root, destination, symlinks=False)
    shutil.copyfile(manifest_path, shared / "test-fixtures.json")
    bundled_files = {path.relative_to(destination).as_posix(): sha256(path)
                     for path in destination.rglob("*") if path.is_file()}
    if bundled_files != files:
        raise ValueError("assembled fixture bytes changed while copying into the native bundle")
    return {"manifest": manifest, "manifest_sha256": sha256(shared / "test-fixtures.json"),
            "source_manifest_sha256": sha256(expected_manifest_hash), "files": bundled_files}

def freeze_test_fixtures(source: Path, root: Path) -> dict:
    """Bind the already assembled resource tree to the clean recorded source."""
    manifest, source_files = fixture_manifest(source)
    manifest_path = root / "share/exomonad/test-fixtures.json"
    if not manifest_path.is_file() or manifest_path.is_symlink():
        raise ValueError("native bundle lacks its declared fixture manifest")
    if manifest != read_fixture_manifest(manifest_path) or sha256(manifest_path) != sha256(source / TEST_FIXTURE_MANIFEST):
        raise ValueError("native bundle fixture manifest differs from recorded source")
    fixture_root = root / TEST_FIXTURE_ROOT
    if fixture_root.is_symlink() or not fixture_root.is_dir():
        raise ValueError("native bundle fixture root is missing or aliased")
    bundled_files = {path.relative_to(fixture_root).as_posix(): sha256(path)
                     for path in fixture_root.rglob("*") if path.is_file()}
    if bundled_files != source_files:
        raise ValueError("native bundle fixture bytes differ from the clean recorded source")
    record = {"manifest": manifest, "manifest_sha256": sha256(manifest_path),
              "source_manifest_sha256": sha256(manifest_path), "files": bundled_files}
    verify_frozen_test_fixtures(root, record)
    return record

def verify_frozen_test_fixtures(root: Path, record: dict) -> None:
    shared = root / "share/exomonad"
    manifest_path = root / "share/exomonad/test-fixtures.json"
    fixture_root = root / TEST_FIXTURE_ROOT
    if (not (root / "share").is_dir() or (root / "share").is_symlink()
            or not shared.is_dir() or shared.is_symlink()
            or manifest_path.is_symlink() or not manifest_path.is_file()):
        raise ValueError("frozen fixture manifest path is missing or aliased")
    if fixture_root.is_symlink() or not fixture_root.is_dir():
        raise ValueError("frozen test fixture root is missing or aliased")
    manifest = json.loads(manifest_path.read_text())
    if manifest != record.get("manifest") or sha256(manifest_path) != record.get("manifest_sha256"):
        raise ValueError("frozen fixture manifest changed")
    if manifest != {"schema": 1, "kind": "haskell-test-fixtures", "files": sorted(record.get("files", {}))}:
        raise ValueError("frozen fixture inventory differs from its manifest")
    actual = {path.relative_to(fixture_root).as_posix(): sha256(path)
              for path in fixture_root.rglob("*") if path.is_file()}
    if actual != record.get("files"):
        raise ValueError("frozen test fixture bytes or file inventory changed")
    if any(path.is_symlink() for path in fixture_root.rglob("*")):
        raise ValueError("frozen test fixtures cannot contain aliases")

def verify_fixture_source_contract(record: dict, contract: dict) -> None:
    files = record.get("files")
    manifest = record.get("manifest")
    source_inputs = contract.get("source_inputs")
    if (contract.get("test_fixtures") != record
            or not isinstance(files, dict) or not isinstance(manifest, dict)
            or not isinstance(source_inputs, dict)
            or source_inputs.get(TEST_FIXTURE_MANIFEST) != record.get("source_manifest_sha256")
            or any(source_inputs.get(relative) != digest for relative, digest in files.items())):
        raise ValueError("frozen fixture evidence differs from the native build source contract")
    if manifest.get("files") != sorted(files):
        raise ValueError("frozen fixture roster differs from its source contract")

def workspace_gitlink(path: Path) -> dict:
    record = json.loads(path.read_text())
    if not isinstance(record, dict):
        raise ValueError("workspace Gitlink input must be an object")
    expected = {"schema": 1, "path": ".exomonad/workspace", "mode": "160000", "revision": record.get("revision")}
    if record != expected or not isinstance(record["revision"], str) or re.fullmatch(r"[0-9a-f]{40}", record["revision"]) is None:
        raise ValueError("workspace Gitlink input must describe one exact recorded submodule revision")
    return record

def verify_workspace_gitlink(source: Path, path: Path) -> dict:
    record = workspace_gitlink(path)
    actual = subprocess.check_output([
        "git", "-C", str(source), "ls-tree", "HEAD", "--", record["path"],
    ], text=True)
    expected = f'{record["mode"]} commit {record["revision"]}\t{record["path"]}\n'
    if actual != expected:
        raise ValueError("bundled workspace Gitlink differs from the source HEAD recorded submodule")
    return record

def verify_workspace_bundle(bundle: Path, record: dict, git: Path) -> None:
    heads = subprocess.check_output([str(git), "bundle", "list-heads", str(bundle)], text=True)
    if heads != f'{record["revision"]} HEAD\n{record["revision"]} refs/heads/workspace\n':
        raise ValueError("workspace Git bundle differs from its exact recorded Gitlink")
    # Import into an empty repository to reject prerequisite-only or damaged
    # bundles. Git verifies the original commit and all reachable object hashes.
    with tempfile.TemporaryDirectory(prefix="qualify-workspace-git-") as temporary:
        repository = Path(temporary) / "workspace.git"
        subprocess.run([str(git), "-c", "protocol.file.allow=always", "clone", "--quiet",
                        "--bare", "--", str(bundle), str(repository)], check=True)
        actual = subprocess.check_output([str(git), "-C", str(repository), "rev-parse",
                                          "refs/heads/workspace^{commit}"], text=True).strip()
        if actual != record["revision"]:
            raise ValueError("workspace bundle contains a different commit object")
        subprocess.run([str(git), "-C", str(repository), "fsck", "--no-dangling"], check=True,
                       stdout=subprocess.DEVNULL)

def native_environment(root: Path) -> dict:
    tools = native_runtime_tools(root / "share/exomonad/runtime-tools")
    ghc = (root / "share/exomonad/ghc-libdir.txt").read_text().strip()
    environment = {
        "TIDEPOOL_EXTRACT": str(root / "bin/tidepool-extract"),
        "TIDEPOOL_EXTRACT_WORKER": str(root / "bin/tidepool-extract-bin"),
        "TIDEPOOL_COMPILER_DEPLOYMENT": str(root / "share/exomonad/compiler-deployment.json"),
        "TIDEPOOL_PRELUDE_DIR": str(root / "share/exomonad/stdlib"),
        "TIDEPOOL_GHC_LIBDIR": ghc,
        "EXOMONAD_EMBEDDED_ASSET_ROOT": str(root / "share/exomonad/web"),
        "EXOMONAD_WORKSPACE_GITLINK": str(root / "share/exomonad/workspace-gitlink.json"),
        "EXOMONAD_WORKSPACE_GIT_BUNDLE": str(root / "share/exomonad/workspace.bundle"),
        "TIDEPOOL_TEST_FIXTURE_ROOT": str(root / TEST_FIXTURE_ROOT),
        "EXOMONAD_NIX_BIN": str(root / "share/exomonad/runtime-tools/bin/nix"),
        "LD_LIBRARY_PATH": str(root / "lib/tidepool"),
        "PATH": str(tools / "bin") + ":" + str(root / "bin"),
        "TIDEPOOL_KEEP_TEST_LOGS": "1",
    }

    contract = json.loads((root / "share/exomonad/native-build-contract.json").read_text())
    if contract["stdlib_mode"] == "catalog-backed":
        selection = verify_native_catalog(root, tools)["source_selection"]
        verify_native_root_entry(root, contract["native_catalog"])
        environment["TIDEPOOL_COMPILER_MODULES"] = str(root / "share/exomonad/catalog/catalog.json")
        environment["TIDEPOOL_PREPARED_ROOT_ENTRY"] = str(root / "share/exomonad/root-entry")
        environment["TIDEPOOL_PRELUDE_DIR"] = str(Path(selection["snapshot_root"]) / "lib")
    elif contract["stdlib_mode"] != "source-backed":
        raise ValueError("unsupported native stdlib mode")
    return environment

def execution_environment(descriptor: dict) -> dict:
    environment = dict(os.environ)
    # No resident endpoint, project catalog or compiler selection may override
    # the pair and source mode that the exact bundle was qualified with.
    for key in UNSET_ENVIRONMENT:
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
    tools, ghc = native_runtime_tools(args.runtime_tools), nix_path(args.ghc_libdir)
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    (root / "bin").mkdir(exist_ok=True)
    (root / "lib/tidepool").mkdir(parents=True, exist_ok=True)
    shared = root / "share/exomonad"
    shared.mkdir(parents=True, exist_ok=True)
    for name, source in (("exomonad-unwrapped", args.host), ("exomonad-view-helper", args.view_helper),
                         ("tidepool-extract", args.frontend), ("tidepool-extract-bin", args.worker),
                         ("tidepool-tests", args.libtest)):
        shutil.copy2(source.resolve(strict=True), root / "bin" / name)
    sources = [("web", args.assets / "web")]
    if getattr(args, "catalog", None) is None:
        sources.extend([("stdlib", args.sources / "lib"), ("actors", args.sources / "actors")])
    for name, source in sources:
        shutil.copytree(source, shared / name, symlinks=False)
    for source in args.libraries.iterdir():
        shutil.copy2(source.resolve(strict=True), root / "lib/tidepool" / source.name)
    fixture_record = copy_test_fixtures(args.build_sources, args.fixture_manifest,
                                        args.test_fixtures, shared)
    shutil.copy2(args.harness_revision, shared / "harness-source-revision.txt")
    shutil.copy2(args.workspace_gitlink, shared / "workspace-gitlink.json")
    shutil.copy2(args.workspace_git_bundle, shared / "workspace.bundle")
    verify_workspace_bundle(shared / "workspace.bundle", workspace_gitlink(shared / "workspace-gitlink.json"), tools / "bin/git")
    (shared / "runtime-tools").symlink_to(tools)
    (shared / "ghc-libdir.txt").write_text(str(ghc) + "\n")
    wrapper = root / "bin/exomonad"
    wrapper.write_text(f"#!{tools}/bin/bash\n" + args.entrypoint_template.read_text().split("\n", 1)[1])
    wrapper.chmod(0o755)
    source_inputs = {path.relative_to(args.build_sources).as_posix(): sha256(path)
                     for path in sorted(args.build_sources.rglob("*")) if path.is_file()}
    contract = {
        "profile": args.profile, "feature_profile": "embedded-native", "stdlib_mode": "source-backed", "startup_mode": "unprepared",
        "source_inputs": source_inputs, "source_inputs_sha256": digest_inventory(source_inputs),
        "test_fixtures": fixture_record,
        "artifacts": {relative: {"target": target, "sha256": sha256(root / relative)}
                      for relative, target in ARTIFACT_TARGETS.items()},
    }
    if getattr(args, "catalog", None) is not None:
        if getattr(args, "root_entry", None) is None:
            raise ValueError("qualified catalog-backed assembly requires a complete prepared root entry")
        shutil.copytree(args.catalog, shared / "catalog", symlinks=False)
        shutil.copytree(args.root_entry, shared / "root-entry", symlinks=False)
        receipt = json.loads((shared / "catalog" / NATIVE_CATALOG_BUILD).read_text())
        contract["stdlib_mode"] = "catalog-backed"
        contract["startup_mode"] = "prepared"
        contract["native_catalog"] = {key: receipt[key] for key in
            ("catalog_sha256", "source_selection", "source_inventory_sha256", "product_inventory_sha256")}
        entry_receipt = json.loads((shared / "root-entry" / NATIVE_ROOT_ENTRY_BUILD).read_text())
        contract["native_root_entry"] = {key: entry_receipt[key] for key in
            ("manifest_sha256", "source_selection", "source_inventory_sha256", "product_inventory_sha256")}
        contract["generated_root_source"] = catalog_source_metadata(
            Path(receipt["source_selection"]["snapshot_root"]))["root_entry"]
    write_json(shared / "native-build-contract.json", contract)
    if contract["stdlib_mode"] == "catalog-backed":
        verify_native_catalog(root, tools)
        verify_native_root_entry(root, contract["native_catalog"])

def freeze(args) -> Path:
    tools = native_runtime_tools(args.bundle / "share/exomonad/runtime-tools")
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
    if contract["feature_profile"] != "embedded-native" or contract["stdlib_mode"] not in ("source-backed", "catalog-backed"):
        raise ValueError("qualification requires a supported native bundle")
    verify_build_contract(source, args.bundle, contract, args.expect_profile)
    fixture_source_manifest, fixture_source_hashes = fixture_manifest(source)
    required_fixture_inputs = {TEST_FIXTURE_MANIFEST: sha256(source / TEST_FIXTURE_MANIFEST), **fixture_source_hashes}
    if any(contract["source_inputs"].get(name) != digest
           for name, digest in required_fixture_inputs.items()):
        raise ValueError("native build contract does not retain the exact fixture manifest and source bytes")
    recorded_workspace = verify_workspace_gitlink(source, args.bundle / "share/exomonad/workspace-gitlink.json")
    verify_workspace_bundle(args.bundle / "share/exomonad/workspace.bundle", recorded_workspace,
                            tools / "bin/git")
    original_sources = None
    if contract["stdlib_mode"] == "catalog-backed":
        selection = verify_native_catalog(args.bundle, tools)["source_selection"]
        verify_native_root_entry(args.bundle, contract["native_catalog"])
        original_sources = Path(selection["snapshot_root"])
    haskell_sources = declared_haskell_sources(source, args.bundle, original_sources,
                                             contract.get("generated_root_source"))
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
    if root != root.resolve():
        raise ValueError("freeze requires a canonical final bundle path")
    root.mkdir(parents=True, exist_ok=False)
    # Dereference Buck source trees; retain only the declared Nix tool closure.
    shutil.copytree(args.bundle, root, symlinks=False, dirs_exist_ok=True,
                    ignore=lambda directory, names: ["runtime-tools"] if Path(directory).name == "exomonad" else [])
    (root / "share/exomonad/runtime-tools").symlink_to((args.bundle / "share/exomonad/runtime-tools").resolve(strict=True))
    shutil.copytree(args.browser_driver, root / "share/exomonad/browser-driver", symlinks=False)
    shutil.copy2(source / "build/rust/isolated-libtest.py", root / "share/exomonad/isolated-libtest.py")
    shutil.copy2(source / "build/package/qualification.py", root / "share/exomonad/qualification.py")
    shutil.copy2(source / "build/package/packaged-catalog-consumer.sh", root / "share/exomonad/packaged-catalog-consumer.sh")
    fixture_record = freeze_test_fixtures(source, args.bundle)
    if fixture_record["manifest"] != fixture_source_manifest or fixture_record["files"] != fixture_source_hashes:
        raise ValueError("assembled fixture resources differ from recorded source")
    verify_fixture_source_contract(fixture_record, contract)
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
    if contract["stdlib_mode"] == "catalog-backed":
        selected.append(Path(contract["native_catalog"]["source_selection"]["snapshot_root"]))
    closure, gc_roots = pin_nix_closure(root, tools, selected)
    if contract["stdlib_mode"] == "catalog-backed":
        transfer_catalog_retention(root, contract, gc_roots, tools)
    copied_log = root / "share/exomonad/build.log"
    shutil.copy2(args.build_log, copied_log)
    descriptor = {
        "schema": 1, "kind": "native-runtime-qualification", "bundle_root": str(root),
        "source_oid": oid, "harness_revision": revision, **contract,
        "source_submodules": submodules,
        "workspace_gitlink": recorded_workspace,
        "test_fixtures": fixture_record,
        "declared_haskell_sources": haskell_sources,
        "source_metadata_inputs": {name: sha256(source / name) for name in ("Cargo.toml", "Cargo.lock", "flake.nix", "flake.lock", "scripts/native-profile.toml")},
        "build": {"commands": commands, "log": str(copied_log), "log_sha256": sha256(copied_log)},
        "environment": environment, "external_inputs": external, "elf_runtime": evidence,
        "nix_closure": closure, "gc_roots": gc_roots,
        "programs": programs(root), "cohorts": cohorts(),
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

def verify_frozen_inventory(root: Path, descriptor: dict) -> None:
    if inventory(root, [DESCRIPTOR]) != descriptor["inventory"]:
        raise ValueError("frozen runtime inventory changed")
    if digest_inventory(descriptor["inventory"]) != descriptor["inventory_sha256"]:
        raise ValueError("runtime inventory digest mismatch")

def verify(path: Path) -> dict:
    descriptor = json.loads(path.read_text())
    root = Path(descriptor["bundle_root"])
    if not root.is_absolute() or root != root.resolve(strict=True) or path.absolute() != root / DESCRIPTOR or descriptor["schema"] != 1 or descriptor["kind"] != "native-runtime-qualification":
        raise ValueError("qualification descriptor was relocated or has an unsupported schema")
    if descriptor["stdlib_mode"] not in ("source-backed", "catalog-backed") or descriptor["feature_profile"] != "embedded-native":
        raise ValueError("qualification is not the selected native source mode")
    if descriptor["programs"] != programs(root) or descriptor["cohorts"] != cohorts():
        raise ValueError("qualification cannot replace its owned programs or mandatory test cohorts")
    verify_frozen_inventory(root, descriptor)
    verify_frozen_test_fixtures(root, descriptor["test_fixtures"])
    contract = json.loads((root / "share/exomonad/native-build-contract.json").read_text())
    verify_fixture_source_contract(descriptor["test_fixtures"], contract)
    if any(descriptor.get(field) != contract.get(field) for field in (*BUILD_CONTRACT_FIELDS, "native_catalog", "native_root_entry", "generated_root_source")):
        raise ValueError("qualification differs from the owning native build contract")
    if not contract["source_inputs"] or digest_inventory(contract["source_inputs"]) != contract["source_inputs_sha256"]:
        raise ValueError("native build source manifest is missing or has an invalid digest")
    verify_artifact_contract(root, contract)
    if workspace_gitlink(root / "share/exomonad/workspace-gitlink.json") != descriptor["workspace_gitlink"]:
        raise ValueError("qualification differs from its recorded workspace Gitlink")
    if descriptor["environment"].get("TIDEPOOL_TEST_FIXTURE_ROOT") != str(root / TEST_FIXTURE_ROOT):
        raise ValueError("qualification does not bind the frozen test fixture tree")
    if set(descriptor["external_inputs"]) != EXTERNAL_INPUTS:
        raise ValueError("qualification requires every declared Nix runtime input")
    for item in descriptor["external_inputs"].values():
        path = nix_path(Path(item["path"]))
        if str(store_root(path)) != item["store_path"]:
            raise ValueError(f"declared Nix selection changed: {path}")
    tools = native_runtime_tools(Path(descriptor["external_inputs"]["runtime_tools"]["path"]))
    expected_roots = {item["store_path"] for item in descriptor["external_inputs"].values()}
    if descriptor["stdlib_mode"] == "catalog-backed":
        selection = verify_native_catalog(root, tools)["source_selection"]
        verify_native_root_entry(root, contract["native_catalog"])
        expected_roots.add(str(Path(selection["snapshot_root"])))
    for item in descriptor["elf_runtime"].values():
        if item["linkage"] == "dynamic":
            expected_roots.add(item["loader_store_path"])
            expected_roots.update(library["store_path"] for library in item["libraries"].values() if "store_path" in library)
    if descriptor["nix_closure"]["roots"] != sorted(expected_roots):
        raise ValueError("Nix closure does not retain every selected runtime dependency")
    if nix_metadata(tools, descriptor["nix_closure"]["roots"]) != descriptor["nix_closure"]:
        raise ValueError("declared immutable Nix closure identity changed")
    expected_gc_roots = {str(root / "share/exomonad/gc-roots" / hashlib.sha256(selected.encode()).hexdigest()): selected
                         for selected in expected_roots}
    if len(descriptor["gc_roots"]) != len(expected_gc_roots) or {item["path"]: item["store_path"] for item in descriptor["gc_roots"]} != expected_gc_roots:
        raise ValueError("deployment must pin exactly its declared Nix runtime roots")
    for pinned in descriptor["gc_roots"]:
        if Path(pinned["path"]).resolve(strict=True) != Path(pinned["store_path"]):
            raise ValueError("deployment Nix GC root was removed or changed")
    verify_registered_gc_roots(tools, descriptor["gc_roots"])
    expected_environment = native_environment(root)
    expected_environment.update(
        TIDEPOOL_BROWSER_NODE=descriptor["external_inputs"]["TIDEPOOL_BROWSER_NODE"]["path"],
        TIDEPOOL_BROWSER_DRIVER=str(root / "share/exomonad/browser-driver/driver.mjs"),
        PLAYWRIGHT_BROWSERS_PATH=descriptor["external_inputs"]["PLAYWRIGHT_BROWSERS_PATH"]["path"],
    )
    if descriptor["environment"] != expected_environment:
        raise ValueError("qualification cannot replace its declared runtime environment")
    if loader_evidence(root, execution_environment(descriptor)) != descriptor["elf_runtime"]:
        raise ValueError("native runtime loader dependency resolution changed")
    return descriptor

def run_cohort(args) -> int:
    if 'TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS' in os.environ:
        raise ValueError('diagnostic startup overrides cannot qualify a frozen cohort')
    if args.jobs <= 0:
        raise ValueError("--jobs must be positive")
    if args.service_slice is not None and not args.delegated_service:
        raise ValueError("--service-slice requires --delegated-service")
    service_slice = (args.service_slice or "app.slice") if args.delegated_service else None
    if service_slice is not None and not re.fullmatch(r"[A-Za-z0-9_.@:-]+\.slice", service_slice):
        raise ValueError("--service-slice must name one systemd slice")
    descriptor = verify(args.descriptor.absolute())
    tools = Path(descriptor["external_inputs"]["runtime_tools"]["path"])
    cohort = descriptor["cohorts"][args.cohort]
    required_compiler_mode = cohort.get("compiler_mode")
    compiler_mode = getattr(args, "compiler_mode", None) or required_compiler_mode or "direct"
    if required_compiler_mode is not None and compiler_mode != required_compiler_mode:
        raise ValueError(f"{args.cohort} requires compiler mode {required_compiler_mode}")
    if args.jobs > cohort.get("max_jobs", args.jobs):
        raise ValueError(f"{args.cohort} permits at most {cohort['max_jobs']} test process")
    output = args.output.absolute()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    command = [str(tools / "bin/python3"), descriptor["programs"]["runner"], descriptor["programs"]["libtest"],
               "--expected-count", str(cohort["expected_count"]), "--jobs", str(args.jobs), "--timeout", str(cohort["timeout"]),
               "--output-dir", str(output / "tests"), "--compiler-mode", compiler_mode]
    for name, timeout in sorted(cohort.get("case_timeouts", {}).items()):
        command.extend(["--case-timeout", f"{name}={timeout}"])
    if args.delegated_service:
        command.extend(["--delegated-service", "--service-slice", service_slice])
    # These bundle-owned resources are verified by the frozen owner. The runner
    # must retain their declared-resource identity across delegation.
    for name in ("TIDEPOOL_COMPILER_MODULES", "TIDEPOOL_PREPARED_ROOT_ENTRY", "TIDEPOOL_TEST_FIXTURE_ROOT"):
        if name in descriptor["environment"]:
            command.extend(["--resource-env", name])
    for name in cohort["tests"]:
        command.extend(["--exact", name])
    if cohort["ignored"]:
        command.append("--ignored")
    started = time.monotonic_ns()
    with (output / "runner.stdout").open("wb") as stdout, (output / "runner.stderr").open("wb") as stderr:
        result = subprocess.run(command, env=execution_environment(descriptor), stdout=stdout, stderr=stderr, check=False)
    records = [json.loads(path.read_text()) for path in sorted((output / "tests").glob("*.json"))]
    exact = {record["test"] for record in records} == set(cohort["tests"])
    confirmed = exact and len(records) == cohort["expected_count"] and all(record["passed"] and record["execution"]["executed_test_count"] == 1
        and record["execution"]["exit_code"] == 0
        and record["execution"].get("startup_diagnostic_seconds") is None for record in records)
    code = result.returncode if result.returncode else int(not confirmed)
    report = {"schema": 1, "descriptor": str(args.descriptor.absolute()), "descriptor_sha256": sha256(args.descriptor),
              "source_oid": descriptor["source_oid"], "harness_revision": descriptor["harness_revision"],
              "profile": descriptor["profile"], "stdlib_mode": descriptor["stdlib_mode"], "cohort": args.cohort,
              "command": command, "exit_code": code, "runner_exit_code": result.returncode,
              "scheduling": {"jobs": args.jobs, "effective_jobs": min(args.jobs, cohort["expected_count"]),
                             "delegated_service": args.delegated_service, "service_slice": service_slice,
                             "compiler_mode": compiler_mode,
                             "timeout_seconds": cohort["timeout"], "case_timeout_seconds": cohort.get("case_timeouts", {})},
              "elapsed_ns": time.monotonic_ns() - started, "expected_count": cohort["expected_count"],
              "executed_test_count": sum((record.get("execution") or {}).get("executed_test_count") or 0 for record in records),
              "unknown_execution_count": sum((record.get("execution") or {}).get("executed_test_count") is None for record in records),
              "completed": code == 0, "tests": records}
    write_json(output / "report.json", report)
    return code


class NativeExecution:
    """One actual host child selected and recorded by the frozen owner."""
    def __init__(self, path, descriptor, command, process, started_ns, descriptor_sha256):
        self.path, self.descriptor, self.command = path, descriptor, command
        self.process, self.started_ns = process, started_ns
        self.descriptor_sha256 = descriptor_sha256

    def record(self, report: Path) -> None:
        write_json(report, {
            "schema": 1, "descriptor_sha256": self.descriptor_sha256, "command": self.command,
            "source_oid": self.descriptor["source_oid"], "harness_revision": self.descriptor["harness_revision"],
            "profile": self.descriptor["profile"], "stdlib_mode": self.descriptor["stdlib_mode"],
            "process_execution_count": 1, "executed_test_count": None,
            "exit_code": self.process.returncode, "elapsed_ns": time.monotonic_ns() - self.started_ns,
        })

def launch_execution(path: Path, arguments: list[str], *, stdout=None, stderr=None,
                     cache_root: Path | None = None) -> NativeExecution:
    """Expose the same verified operation to CLI and asynchronous observers."""
    path = path.absolute()
    descriptor = verify(path)
    if not arguments or any(flag in arguments for flag in ("--help", "-h", "--version", "-V")):
        raise ValueError("live execution requires an actual package operation")
    command = [descriptor["programs"]["host"], *arguments]
    environment = execution_environment(descriptor)
    if cache_root is not None:
        environment["XDG_CACHE_HOME"] = str(cache_root.resolve(strict=True))
    admitted_digest = sha256(path)
    started = time.monotonic_ns()
    process = subprocess.Popen(command, env=environment, stdout=stdout, stderr=stderr)
    return NativeExecution(path, descriptor, command, process, started, admitted_digest)


def run_catalog_gate(args) -> int:
    path = args.descriptor.absolute()
    descriptor = verify(path)
    if descriptor["stdlib_mode"] != "catalog-backed":
        raise ValueError("catalog gate requires the qualified native catalog mode")
    output = args.output.absolute()
    if output != output.resolve():
        raise ValueError("catalog gate requires a canonical evidence directory")
    output.mkdir(parents=True, exist_ok=False)
    root = Path(descriptor["bundle_root"])
    tools = Path(descriptor["external_inputs"]["runtime_tools"]["path"])
    command = [str(tools / "bin/bash"), str(root / "share/exomonad/packaged-catalog-consumer.sh"),
               str(root), str(path), str(tools / "bin/bwrap"), str(output), str(tools / "bin/python3")]
    admitted_digest = sha256(path)
    started = time.monotonic_ns()
    result = subprocess.run(command, env=execution_environment(descriptor), check=False)
    records = [json.loads(record.read_text()) for record in sorted((output / "tests").glob("*.json"))]
    completed = (result.returncode == 0 and len(records) == 1 and records[0].get("test") == CATALOG_TEST
                 and records[0].get("passed") is True
                 and records[0].get("execution", {}).get("executed_test_count") == 1
                 and records[0].get("execution", {}).get("exit_code") == 0)
    counts = [record.get("execution", {}).get("executed_test_count") for record in records]
    executed_count = sum(counts) if all(type(count) is int and count >= 0 for count in counts) else None
    write_json(output / "report.json", {
        "schema": 1, "kind": "native-catalog-gate", "descriptor": str(path),
        "descriptor_sha256": admitted_digest, "bundle_root": str(root),
        "source_oid": descriptor["source_oid"], "harness_revision": descriptor["harness_revision"],
        "profile": descriptor["profile"], "stdlib_mode": descriptor["stdlib_mode"],
        "native_catalog": descriptor["native_catalog"], "command": command,
        "selected_test_count": 1,
        "executed_test_count": executed_count,
        "exit_code": result.returncode, "completed": completed,
        "elapsed_ns": time.monotonic_ns() - started, "tests": records,
    })
    return 0 if completed else (result.returncode or 1)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    snapshot = commands.add_parser("snapshot-sources")
    for key in ("sources", "effects", "jev-sources", "cohort", "root-entry-source", "output"):
        snapshot.add_argument("--" + key, required=True, type=Path)
    retained = commands.add_parser("retain-sources")
    for key in ("snapshot", "output", "runtime-tools"):
        retained.add_argument("--" + key, required=True, type=Path)
    selected = commands.add_parser("select-sources")
    for key in ("record", "snapshot", "runtime-tools"):
        selected.add_argument("--" + key, required=True, type=Path)
    producer_inputs = ("snapshot", "source-root", "declared-source-root", "retention-record", "retention-record-origin", "runtime-tools", "producer", "frontend", "worker", "deployment", "ghc-libdir", "libraries", "output")
    for name in ("build-catalog", "inspect-catalog", "build-root-entry"):
        catalog = commands.add_parser(name)
        for key in producer_inputs:
            catalog.add_argument("--" + key, required=True, type=Path)
        if name == "inspect-catalog":
            catalog.add_argument("--timeout", required=True, type=int,
                                 help="producer wall limit in seconds (600–1800)")
    gate = commands.add_parser("catalog-gate")
    gate.add_argument("descriptor", type=Path)
    gate.add_argument("--output", type=Path, required=True)
    stage = commands.add_parser("assemble")
    stage.add_argument("--catalog", type=Path)
    stage.add_argument("--root-entry", type=Path)
    for key in ("output", "host", "view-helper", "frontend", "worker", "libtest", "build-sources", "workspace-gitlink", "workspace-git-bundle", "sources", "assets", "libraries", "harness-revision", "runtime-tools", "ghc-libdir", "entrypoint-template", "test-fixtures", "fixture-manifest"):
        stage.add_argument("--" + key, required=True, type=Path)
    stage.add_argument("--profile", required=True, choices=("fast-dev", "debug", "production"))
    frozen = commands.add_parser("freeze")
    for key in ("bundle", "output", "source-root", "browser-driver", "browser-node", "playwright-browsers", "build-log", "build-commands"):
        frozen.add_argument("--" + key, required=True, type=Path)
    frozen.add_argument("--expect-profile", required=True, choices=("fast-dev", "debug", "production"))
    checked = commands.add_parser("verify")
    checked.add_argument("descriptor", type=Path)
    environment = commands.add_parser("environment")
    environment.add_argument("descriptor", type=Path)
    environment.add_argument("--shell", action="store_true")
    run = commands.add_parser("run")
    run.add_argument("descriptor", type=Path)
    run.add_argument("--cohort", required=True, choices=tuple(cohorts()))
    run.add_argument("--output", required=True, type=Path)
    run.add_argument("--jobs", type=int, default=1,
                     help="maximum concurrent test processes (default: 1)")
    run.add_argument("--compiler-mode", choices=("direct", "owned-resident"),
                     help="isolated per-case compiler lifecycle (default: cohort selection)")
    run.add_argument("--delegated-service", action="store_true",
                     help="run each test in the isolated runner's delegated user service")
    run.add_argument("--service-slice",
                     help="delegated user service slice (default: app.slice)")
    live = commands.add_parser("exec")
    live.add_argument("descriptor", type=Path)
    live.add_argument("--report", required=True, type=Path)
    live.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    try:
        if args.command == "snapshot-sources":
            snapshot_catalog_sources(args)
        elif args.command == "retain-sources":
            print(retain_catalog_sources(args))
        elif args.command == "select-sources":
            print(verify_retained_catalog_sources(args.record, args.snapshot, nix_path(args.runtime_tools)))
        elif args.command == "build-catalog":
            build_native_catalog(args)
        elif args.command == "build-root-entry":
            build_native_root_entry(args)
        elif args.command == "inspect-catalog":
            print(inspect_native_catalog(args))
        elif args.command == "catalog-gate":
            return run_catalog_gate(args)
        elif args.command == "assemble":
            assemble(args)
        elif args.command == "freeze":
            print(freeze(args))
        elif args.command == "verify":
            verify(args.descriptor.absolute())
        elif args.command == "environment":
            descriptor = verify(args.descriptor.absolute())
            selected = dict(descriptor["environment"])
            selected.update(TIDEPOOL_NATIVE_LIBTEST=descriptor["programs"]["libtest"],
                            TIDEPOOL_NATIVE_DESCRIPTOR=str(args.descriptor.absolute()))
            if args.shell:
                print("unset " + " ".join(UNSET_ENVIRONMENT))
                for key, value in sorted(selected.items()):
                    print("export " + key + "=" + shlex.quote(value))
            else:
                print(json.dumps({"set": selected, "unset": UNSET_ENVIRONMENT}, sort_keys=True))
        elif args.command == "run":
            return run_cohort(args)
        elif args.command == "exec":
            arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
            execution = launch_execution(args.descriptor, arguments)
            try:
                return execution.process.wait()
            except BaseException:
                if execution.process.poll() is None:
                    execution.process.kill()
                execution.process.wait(timeout=5)
                raise
            finally:
                execution.record(args.report)
        return 0
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"native qualification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
