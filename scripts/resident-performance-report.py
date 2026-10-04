#!/usr/bin/env python3
"""Report retained production resident samples against actual daemon evidence.

Run after the owning production runner has exited; this tool starts no services
and changes no caches. See bridge/facade/tests/embedded-gates.md for the owning
production fixtures and retained-evidence reporting procedure.
"""

import argparse
import importlib.util
import json
import math
import hashlib
import re
from pathlib import Path
import sys

GHC_SOURCE_HELPER = Path(__file__).resolve().parents[1] / 'build/testing/ghc_source_options.py'
sys.path.insert(0, str(GHC_SOURCE_HELPER.parent))
from ghc_source_options import audit_source_optimization_options, verify_worker_home_actions

COLD_START_GATE_PATH = Path(__file__).resolve().with_name("package-cold-start-gate.py")
COLD_START_GATE_SPEC = importlib.util.spec_from_file_location(
    "resident_package_cold_start_gate", COLD_START_GATE_PATH)
COLD_START_GATE = importlib.util.module_from_spec(COLD_START_GATE_SPEC)
COLD_START_GATE_SPEC.loader.exec_module(COLD_START_GATE)

PREFIX = "resident-performance "
LIMITS_MS = {"warm_cell": 1000, "cold_start": 10000, "cancel_ack": 250}
MIN_COUNTS = {"warm_cell": 50, "cold_start": 5, "cancel_ack": 50}
MAX_BUILD_JSON = 16 * 1024 * 1024
CURRENT_RECOVERY_SCHEMA = "paired-public-v6"
RECOVERY_SCHEMA_VERSIONS = {"paired-public-v4": 4, "paired-public-v5": 5, CURRENT_RECOVERY_SCHEMA: 6}
DURABLE_MATRIX = {(baseline, prefix) for baseline in (0, 100) for prefix in (1, 10, 100)}


def file_sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def retained_digest(path, expected):
    try:
        selected = Path(path)
        return selected.is_absolute() and file_sha256(selected) == expected
    except (TypeError, OSError):
        return False


def nix_compiler_path(path):
    return Path(path).resolve(strict=True).is_relative_to('/nix/store')


def build_reference(value):
    if not isinstance(value, dict) or not retained_digest(value.get('path'), value.get('sha256')):
        raise ValueError('build evidence reference does not match retained bytes')
    return Path(value['path'])


def build_json(path):
    if path.stat().st_size > MAX_BUILD_JSON:
        raise ValueError('build JSON exceeds bound')
    return json.loads(path.read_text())


def build_rows(path):
    with path.open() as stream:
        while line := stream.readline(MAX_BUILD_JSON + 1):
            if len(line) > MAX_BUILD_JSON:
                raise ValueError('build evidence row exceeds bound')
            if line.strip():
                yield json.loads(line)


def build_packet(reference, schema, source_oid, output_sha256):
    if not isinstance(reference, dict):
        raise ValueError('missing build packet')
    path = build_reference({'path': reference.get('packet_path'), 'sha256': reference.get('packet_sha256')})
    packet = build_json(path)
    command = packet.get('command')
    if packet.get('schema') != schema or packet.get('exit_code') != 0 or packet.get('source_oid') != source_oid:
        raise ValueError('build schema, source or successful exit does not match')
    if not isinstance(command, list) or not command or any(not isinstance(arg, str) for arg in command):
        raise ValueError('build has no exact command argv')
    compiler = build_reference(packet.get('compiler'))
    if not nix_compiler_path(compiler):
        raise ValueError('build compiler is not in the admitted Nix store')
    output = build_reference(packet.get('output'))
    if packet['output']['sha256'] != output_sha256:
        raise ValueError('build output differs from selected binary')
    before = build_json(build_reference(packet.get('source_before')))
    after = build_json(build_reference(packet.get('source_after')))
    archive = packet.get('source_archive')
    build_reference(archive)
    if before.get('version') != 1 or before.get('capture_changes') != [] or after.get('changes') != []:
        raise ValueError('build source capture or audit is incomplete')
    if before.get('source') != after.get('source') or before.get('source', {}).get('head_oid') != source_oid:
        raise ValueError('source moved during build or differs from selected revision')
    if before.get('archive_sha256') != archive['sha256']:
        raise ValueError('source archive differs from captured source')
    build_reference(packet.get('build_log'))
    return packet, compiler, output


def rust_build_evidence(manifest, runner, kind, problems):
    try:
        packet, _, _ = build_packet(runner.get('rust_build'), 'cargo-build-v1',
                                    manifest.get('source_oid'), runner.get('binary_sha256'))
        matches = []
        for row in build_rows(build_reference(packet.get('cargo_messages'))):
            if row.get('reason') == 'compiler-artifact' and row.get('executable') == packet['output'].get('cargo_path'):
                matches.append(row)
        if len(matches) != 1 or not packet['output'].get('cargo_path'):
            raise ValueError('build has no unique selected Cargo compiler artifact')
        profile = matches[0].get('profile', {})
        if profile.get('opt_level') not in ('2', '3', 's', 'z') or profile.get('test') is not (kind != 'cold_start'):
            raise ValueError('selected Cargo compiler artifact has another optimization or test profile')
    except (OSError, ValueError, TypeError, AttributeError, KeyError) as error:
        problems.append(f'runner {kind} optimized build evidence: {error}')


def worker_build_evidence(manifest, problems):
    try:
        packet, compiler, _ = build_packet(manifest.get('worker_build'), 'ghc-build-v2',
                                           manifest.get('source_oid'), manifest.get('worker_sha256'))
        source_before = build_json(build_reference(packet.get('source_before')))
        helper_entry = next((entry for entry in source_before.get('source', {}).get('entries', [])
                             if entry.get('path') == 'build/testing/ghc_source_options.py'), None)
        helper_reference = build_reference(packet.get('source_options_tool'))
        if helper_entry is None or helper_entry.get('sha256') != file_sha256(helper_reference):
            raise ValueError('retained GHC source auditor differs from captured source bytes')
        if file_sha256(GHC_SOURCE_HELPER) != helper_entry.get('sha256'):
            raise ValueError('reporter GHC source auditor differs from captured source bytes')
        source_options = audit_source_optimization_options(
            build_reference(packet.get('source_archive')), source_before)
        build_log = build_reference(packet.get('build_log'))
        if build_log.stat().st_size > MAX_BUILD_JSON:
            raise ValueError('GHC build log exceeds retained verification bound')
        verify_worker_home_actions(
            build_rows(build_reference(packet.get('ghc_invocations'))),
            build_log.read_text(), compiler, packet['source_root'],
            packet['build_directory'], source_options)
    except (OSError, ValueError, TypeError, AttributeError, KeyError) as error:
        problems.append(f'worker optimized build evidence: {error}')


def frozen_runner_evidence(manifest, runner):
    reference = runner.get("native_qualification")
    if not isinstance(reference, dict):
        raise ValueError("native runner has no frozen qualification reference")
    operator = COLD_START_GATE.FrozenOperator(Path(reference["descriptor"]))
    if operator.provenance() != reference:
        raise ValueError("native runner differs from the frozen qualification owner")
    if reference["source_oid"] != manifest.get("source_oid"):
        raise ValueError("native runner source differs from the measured manifest")
    if reference["profile"] != "production":
        raise ValueError("performance acceptance requires the declared native production profile")
    selected = operator.descriptor["programs"]
    if (runner.get("binary_path") != selected["host_elf"]
            or runner.get("binary_sha256") != file_sha256(Path(selected["host_elf"]))
            or runner.get("package_entrypoint") != selected["host"]):
        raise ValueError("cold runner does not match the frozen host component")
    return operator


def frozen_store_schema_evidence(operator, authority):
    if not isinstance(authority, dict) or authority.get("revision") != operator.descriptor["harness_revision"]:
        raise ValueError("Store schema differs from the frozen Harness component")
    for path_key, hash_key in (("metadata", "metadata_sha256"), ("process_report", "process_report_sha256")):
        if not retained_digest(authority.get(path_key), authority.get(hash_key)):
            raise ValueError("compiled Store schema evidence is absent or changed")
    metadata = json.loads(Path(authority["metadata"]).read_text())
    receipt = json.loads(Path(authority["process_report"]).read_text())
    if metadata != {"version": authority.get("version")} or type(metadata.get("version")) is not int or not 0 < metadata["version"] <= 0xffffffff:
        raise ValueError("compiled Store schema metadata is malformed")
    if (receipt.get("command") != [operator.descriptor["programs"]["host"], "harness-store-schema"]
            or receipt.get("descriptor_sha256") != operator.provenance()["descriptor_sha256"]
            or receipt.get("source_oid") != operator.descriptor["source_oid"]
            or receipt.get("harness_revision") != operator.descriptor["harness_revision"]
            or receipt.get("profile") != operator.descriptor["profile"]
            or receipt.get("stdlib_mode") != operator.descriptor["stdlib_mode"]
            or receipt.get("exit_code") != 0 or receipt.get("process_execution_count") != 1):
        raise ValueError("Store schema was not reported by the actual frozen host component")


def runner_evidence(manifest, kind, problems):
    runners = manifest.get("runners", {})
    runner = runners.get(kind, {}) if isinstance(runners, dict) else {}
    if not isinstance(runner, dict):
        runner = {}
    if not retained_digest(runner.get("binary_path"), runner.get("binary_sha256")):
        problems.append(f"runner {kind} has no matching retained binary")
    command = runner.get("command")
    if not isinstance(command, list) or not command or any(not isinstance(arg, str) for arg in command):
        problems.append(f"runner {kind} has no exact argv array")
    if runner.get("exit_code") != 0:
        problems.append(f"runner {kind} is not a successful execution")
    if kind == "cold_start" and "native_qualification" in runner:
        try:
            frozen_runner_evidence(manifest, runner)
        except (COLD_START_GATE.GateError, OSError, ValueError, TypeError, AttributeError, KeyError) as error:
            problems.append(f"cold native qualification evidence: {error}")
    else:
        rust_build_evidence(manifest, runner, kind, problems)
    expected, executed = runner.get("expected_test_count"), runner.get("executed_test_count")
    minimum = 0 if kind == "cold_start" else 1
    if type(expected) is not int or expected < minimum or type(executed) is not int or expected != executed:
        problems.append(f"runner {kind} has no matching executed test count")
    if kind == "cold_start":
        if not retained_digest(runner.get("package_entrypoint"), runner.get("package_entrypoint_sha256")):
            problems.append("cold runner has no matching packaged entrypoint")
    return runner


def operation_identity(value, problems, label):
    origin = value.get("origin") if isinstance(value, dict) else None
    if not isinstance(origin, dict) or origin.get("kind") != "embedded" or any(
        not isinstance(origin.get(key), str) or not origin[key] for key in ("run", "actor", "incarnation")
    ) or any(not isinstance(value.get(key), str) or not value[key] for key in ("request", "call")):
        problems.append(f"{label} has no exact original embedded operation")
        return None
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def percentile(values, quantile):
    """Nearest rank: sorted values[ceil(q * n) - 1], including n=1."""
    if not values:
        return None
    return sorted(values)[max(0, math.ceil(quantile * len(values)) - 1)]


def fields(event):
    merged = {}
    for span in event.get("spans", []):
        merged.update(span)
    merged.update(event.get("span", {}))
    merged.update(event.get("fields", {}))
    return merged


def invocation_identity(epoch, record, problems, label):
    """Return the unique transport invocation; the digest remains payload identity."""
    if not isinstance(record, dict):
        problems.append(f"{label} lacks an exact admission/request identity")
        return None
    admission = record.get("admission_id")
    ordinal = record.get("request_ordinal")
    digest = record.get("compile_request")
    record_epoch = record.get("daemon_epoch")
    if (not isinstance(epoch, str) or not epoch or record_epoch != epoch or
            not isinstance(record_epoch, str) or type(admission) is not int or admission < 0 or
            type(ordinal) is not int or ordinal < 1 or not isinstance(digest, str) or not digest):
        problems.append(f"{label} has malformed admission/request identity")
        return None
    return (epoch, admission, ordinal), digest


def analyze(samples, events, manifest):
    problems = []
    required = (
        "source_oid", "command", "exit_code",
        "frontend_sha256", "worker_sha256", "compiler_producer", "runners", "worker_build",
    )
    for key in required:
        if key not in manifest:
            problems.append(f"manifest missing {key}")
    if manifest.get("schema") != 2:
        problems.append("measurement manifest requires schema 2")
    for key, length in (("source_oid", 40), ("frontend_sha256", 64), ("worker_sha256", 64), ("compiler_producer", 64)):
        value = manifest.get(key)
        if not isinstance(value, str) or len(value) != length or any(char not in "0123456789abcdef" for char in value):
            problems.append(f"manifest {key} is not a complete hexadecimal digest")
    runners = {kind: runner_evidence(manifest, kind, problems) for kind in MIN_COUNTS}
    worker_build_evidence(manifest, problems)
    command = manifest.get("command")
    if not isinstance(command, list) or not command or any(not isinstance(arg, str) for arg in command):
        problems.append("manifest command must be the exact nonempty argv array")
    if manifest.get("exit_code") != 0:
        problems.append("production runner did not exit successfully")
    ready = {}
    request_starts = {}
    request_terminals = {}
    admissions = {}
    queue = []
    phases = {}
    counts = {}
    for event in events:
        row = fields(event)
        if row.get("message") == "compiler daemon ready":
            ready[row.get("daemon_epoch")] = row
        if row.get("message") == "compiler job dequeued" and "queue_ms" in row:
            queue.append(row["queue_ms"])
            admission_key = (row.get("daemon_epoch"), row.get("admission_id"))
            admissions.setdefault(admission_key, []).append(row)
        if row.get("message") == "compiler timing":
            phase = re.fullmatch(r"tidepool-timing phase=([a-zA-Z0-9_]+) ms=([0-9]+)", row.get("line", ""))
            count = re.fullmatch(r"tidepool-count name=([a-zA-Z0-9_.]+) count=([0-9]+)", row.get("line", ""))
            if phase:
                phases.setdefault(phase[1], []).append(int(phase[2]))
            if count:
                counts[count[1]] = counts.get(count[1], 0) + int(count[2])
        if row.get("message") == "compiler request started":
            identity_record = invocation_identity(
                row.get("daemon_epoch"), row, problems, "compiler start event")
            if identity_record is not None:
                identity, _ = identity_record
                request_starts.setdefault(identity, []).append(row)
        if row.get("message") in ("compiler request finished", "compiler request failed",
                                  "compiler request abandoned"):
            identity_record = invocation_identity(
                row.get("daemon_epoch"), row, problems, "compiler terminal event")
            if identity_record is not None:
                identity, _ = identity_record
                request_terminals.setdefault(identity, []).append(row)
    for admission_key, rows_for_admission in admissions.items():
        if (not isinstance(admission_key[0], str) or type(admission_key[1]) is not int or
                admission_key[1] < 0 or len(rows_for_admission) != 1):
            problems.append(f"compiler admission {admission_key} has malformed or repeated queue evidence")
    if not ready:
        problems.append("no actual daemon startup evidence")
    product = [row for row in samples if row.get("composition") == "engine-store"]
    if len(product) != len(samples):
        problems.append("samples include a composition other than engine-store")
    seen = set()
    operations = set()
    warm_workers = {}
    correlation_owners = {}
    warm_operations = set()
    acknowledgments = {}
    cleanups = {}
    for row in product:
        if row.get("schema") != 1:
            problems.append("unsupported sample schema")
        kind = row.get("kind")
        runner_id = "cancel_ack" if kind == "cancel_cleanup" else kind
        if row.get("runner_id") != runner_id:
            problems.append(f"sample {kind}/{row.get('index')} has no exact runner identity")
        if kind not in (*MIN_COUNTS, "cancel_cleanup"):
            problems.append(f"unsupported sample kind {kind}")
        startup = ready.get(row.get("daemon_epoch"))
        if not startup or startup.get("producer") != manifest.get("compiler_producer") or not startup.get("daemon_pid"):
            problems.append(f"sample {kind}/{row.get('index')} has no actual configured daemon startup")
        key = (kind, row.get("index"))
        if key in seen:
            problems.append(f"duplicate sample {key}")
        seen.add(key)
        if row.get("completed") is not True or not type(row.get("elapsed_ns")) is int or row["elapsed_ns"] < 0:
            problems.append(f"incomplete or invalid sample {key}")
        if kind in ("cold_start", "cancel_ack", "cancel_cleanup"):
            start, end = row.get("started_ns"), row.get("settled_ns")
            if type(start) is not int or type(end) is not int or start < 0 or end < start or end - start != row.get("elapsed_ns"):
                problems.append(f"sample {key} has no exact start/settlement boundaries")
        if kind == "cold_start":
            if row.get("packaged") is not True or type(row.get("host_pid")) is not int or row["host_pid"] <= 0 or row.get("readiness") != "workspace-ready":
                problems.append(f"cold sample {key} lacks packaged host/readiness evidence")
            host_hash = runners["cold_start"].get("binary_sha256")
            if row.get("host_sha256") != host_hash or not retained_digest(row.get("actual_retained_host_executable"), host_hash):
                problems.append(f"cold sample {key} used another host binary")
            if row.get("provider_requests") != 0:
                problems.append(f"cold sample {key} did not remain idle without provider requests")
        if kind == "cancel_ack":
            operation = row.get("operation_id")
            if not isinstance(operation, str) or not operation or operation in operations:
                problems.append(f"cancel sample {key} has no unique original operation")
            operations.add(operation)
            original = row.get("original_operation")
            identity = operation_identity(original, problems, f"cancel sample {key}")
            try:
                if identity != operation_identity(json.loads(operation), problems, f"cancel sample {key} serialized identity"):
                    problems.append(f"cancel sample {key} operation carrier differs from its original operation")
            except (ValueError, TypeError):
                problems.append(f"cancel sample {key} has malformed serialized operation identity")
            issued = row.get("compiler_requests")
            if not isinstance(issued, list) or not issued:
                problems.append(f"cancel sample {key} lacks exact preparation compiler requests")
            else:
                for reference in issued:
                    identity_record = invocation_identity(
                        row.get("daemon_epoch"), reference, problems, f"cancel sample {key}")
                    if identity_record is None:
                        continue
                    request_key, digest = identity_record
                    if request_key in correlation_owners:
                        problems.append(f"compiler invocation {request_key} is charged to multiple cells")
                    correlation_owners[request_key] = identity
                    started = request_starts.get(request_key, [])
                    terminals = request_terminals.get(request_key, [])
                    admission_key = (request_key[0], request_key[1])
                    if len(started) != 1 or started[0].get("compile_request") != digest:
                        problems.append(f"cancel sample {key} has no exact daemon start for {request_key}")
                    if (len(terminals) != 1 or terminals[0].get("message") != "compiler request finished" or
                            terminals[0].get("compile_request") != digest or terminals[0].get("transport") != "daemon" or
                            terminals[0].get("exit_code") != 0 or not startup or
                            terminals[0].get("daemon_pid") != startup.get("daemon_pid") or
                            not terminals[0].get("worker_pid")):
                        problems.append(f"cancel sample {key} preparation has no exact successful daemon completion")
                    if admission_key not in admissions:
                        problems.append(f"cancel sample {key} has no one-time admission queue record for {admission_key}")
            acknowledgments[row.get("index")] = row
            active = row.get("effect_active_ns")
            if type(active) is not int or type(row.get("started_ns")) is not int or active < 0 or active > row["started_ns"] or row.get("acknowledged") is not True:
                problems.append(f"cancel sample {key} has no proved active effect and acknowledgment")
        if kind == "cancel_cleanup":
            cleanups[row.get("index")] = row
            if row.get("native_abort_confirmed") is not True:
                problems.append(f"cancel cleanup {key} has no confirmed native cleanup")
        if kind == "warm_cell":
            operation = operation_identity(row.get("operation_id"), problems, f"warm sample {key}")
            if operation in warm_operations:
                problems.append(f"warm sample {key} repeats an original operation")
            warm_operations.add(operation)
            source_digest = row.get("source_digest")
            if not isinstance(source_digest, str) or len(source_digest) != 64 or any(char not in "0123456789abcdef" for char in source_digest):
                problems.append(f"warm sample {key} has no actual cell source digest")
            if "source" in row:
                source = row["source"]
                if not isinstance(source, str) or hashlib.sha256(source.encode()).hexdigest() != source_digest:
                    problems.append(f"warm sample {key} source differs from its SHA256 digest")
            if row.get("displayed") is not True:
                problems.append(f"warm sample {key} has no display evidence")
            epoch = row.get("daemon_epoch")
            startup = ready.get(epoch)
            if not startup or startup.get("producer") != manifest.get("compiler_producer"):
                problems.append(f"warm sample {key} has no matched configured producer/epoch")
            correlations = row.get("compiler_requests", [])
            if not correlations:
                problems.append(f"warm sample {key} has no exact compiler invocation identities")
            for reference in correlations:
                identity_record = invocation_identity(
                    epoch, reference, problems, f"warm sample {key}")
                if identity_record is None:
                    continue
                owner_key, digest = identity_record
                if owner_key in correlation_owners:
                    problems.append(f"compiler invocation {owner_key} is charged to multiple cells")
                correlation_owners[owner_key] = operation
                started = request_starts.get(owner_key, [])
                terminals = request_terminals.get(owner_key, [])
                admission_key = (owner_key[0], owner_key[1])
                if len(started) != 1 or started[0].get("compile_request") != digest:
                    problems.append(f"warm sample {key} has no exact daemon start for {owner_key}")
                if len(terminals) != 1:
                    problems.append(f"warm sample {key} invocation {owner_key} has {len(terminals)} terminal events")
                    continue
                request = terminals[0]
                if (request.get("message") != "compiler request finished" or
                        request.get("compile_request") != digest or request.get("transport") != "daemon" or
                        request.get("exit_code") != 0):
                    problems.append(f"warm sample {key} did not complete through its exact daemon invocation")
                if not request.get("worker_pid") or not startup or request.get("daemon_pid") != startup.get("daemon_pid"):
                    problems.append(f"warm sample {key} has no matched worker/daemon PID")
                if request.get("served", 0) < 1 or request.get("followed_rotation") is not False:
                    problems.append(f"warm sample {key} used a cold or rotated worker")
                if admission_key not in admissions:
                    problems.append(f"warm sample {key} has no one-time admission queue record for {admission_key}")
                warm_workers.setdefault((epoch, request.get("worker_pid")), set()).add(row.get("index"))
    for worker, cells in warm_workers.items():
        if len(cells) < 2:
            problems.append(f"warm worker {worker} has no measured reuse")
    if acknowledgments.keys() != cleanups.keys():
        problems.append("every cancellation acknowledgment requires exactly one cleanup sample")
    for index in acknowledgments.keys() & cleanups.keys():
        ack, cleanup = acknowledgments[index], cleanups[index]
        if any(ack.get(key) != cleanup.get(key) for key in ("operation_id", "daemon_epoch")) or ack.get("settled_ns") != cleanup.get("started_ns"):
            problems.append(f"cancel cleanup {index} belongs to different acknowledgment evidence")
    for startup in ready.values():
        for path_key, digest_key in (("executable", "frontend_sha256"), ("worker", "worker_sha256")):
            try:
                path = Path(startup[path_key])
                digest = file_sha256(path)
                if not path.is_absolute() or digest != manifest.get(digest_key):
                    problems.append(f"daemon {path_key} differs from frozen manifest")
            except (KeyError, OSError):
                problems.append(f"daemon has no retained {path_key} binary")
    result = {}
    for kind in (*MIN_COUNTS, "cancel_cleanup"):
        selected = [row for row in product if row.get("kind") == kind]
        values = [row["elapsed_ns"] / 1_000_000 for row in selected if type(row.get("elapsed_ns")) is int and row["elapsed_ns"] >= 0]
        p95 = percentile(values, 0.95)
        sufficient = len(selected) >= MIN_COUNTS.get(kind, 50)
        if kind == "warm_cell" and len({row.get("workload") for row in selected if row.get("workload")}) < 10:
            sufficient = False
            problems.append("warm samples must include at least ten varied workload labels")
        if kind == "warm_cell" and len({row.get("source_digest") for row in selected if isinstance(row.get("source_digest"), str) and len(row["source_digest"]) == 64}) < 10:
            sufficient = False
            problems.append("warm samples must include at least ten actual cell source digests")
        if kind == "cold_start" and len({row.get("daemon_epoch") for row in selected if row.get("daemon_epoch")}) < 5:
            sufficient = False
            problems.append("cold starts require five distinct daemon epochs")
        if kind == "cold_start":
            if len({row.get("host_pid") for row in selected}) < 5:
                problems.append("cold starts require five distinct host processes")
            if len({row.get("run_id") for row in selected if row.get("run_id")}) < 5:
                problems.append("cold starts require five distinct run identities")
            if runners[kind].get("process_execution_count") != len(selected):
                problems.append("cold runner process count differs from actual samples")
        limit = LIMITS_MS.get(kind)
        measured = max(values) if kind == "cold_start" and values else p95
        result[kind] = {
            "count": len(selected), "p50_ms": percentile(values, 0.5),
            "p95_ms": p95, "max_ms": max(values) if values else None,
            "sufficient_count": sufficient, "limit_ms": limit,
            "latency_met": sufficient and measured is not None and (limit is None or measured <= limit),
        }
    service = [row for rows in request_terminals.values() for row in rows
               if row.get("message") == "compiler request finished"]
    return {
        "schema": 1, "manifest": manifest, "percentile": "nearest-rank",
        "samples": result, "evidence_problems": sorted(set(problems)),
        "daemon_epochs": sorted(str(epoch) for epoch in ready),
        "worker_pids": sorted({row.get("worker_pid") for row in service if row.get("worker_pid")}),
        "worker_peak_observed_rss_mb": max((row["worker_rss_mb"] for row in service if "worker_rss_mb" in row), default=None),
        "compiler_phases_ms": {name: {"count": len(values), "total_ms": sum(values), "p95_ms": percentile(values, 0.95)} for name, values in sorted(phases.items())},
        "compiler_counts": dict(sorted(counts.items())),
        "queue_p95_ms": percentile(queue, 0.95),
        "compiler_service_p95_ms": percentile([row["elapsed_ms"] for row in service if "elapsed_ms" in row], 0.95),
        "accepted": not problems and all(result[kind]["latency_met"] for kind in MIN_COUNTS),
    }


def analyze_actors(rows, events, manifest):
    """The eight-actor workload is independent of the warm/cold/cancel limits."""
    if not rows:
        return {"status": "unmeasured", "count": 0, "evidence_problems": []}
    problems = []
    runner_evidence(manifest, "actor_workload", problems)
    ready, request_starts, request_terminals, admissions = {}, {}, {}, {}
    for event in events:
        row = fields(event)
        if row.get("message") == "compiler daemon ready":
            ready[row.get("daemon_epoch")] = row
        if row.get("message") == "compiler job dequeued":
            admissions.setdefault((row.get("daemon_epoch"), row.get("admission_id")), []).append(row)
        if row.get("message") in ("compiler request started", "compiler request finished",
                                  "compiler request failed", "compiler request abandoned"):
            identity_record = invocation_identity(row.get("daemon_epoch"), row, problems, "actor compiler event")
            if identity_record is not None:
                identity, _ = identity_record
                target = request_starts if row.get("message") == "compiler request started" else request_terminals
                target.setdefault(identity, []).append(row)
    cells, operations, correlations, actors, workers = set(), set(), set(), {}, {}
    gauge_fields = ("native_functions", "native_code_bytes", "live_old_bytes", "major_collections", "programs", "block_words", "persistent_roots", "handles", "code_exports", "parked", "static_regions", "descriptor_rows", "callable_rows", "enter_rows")
    for row in rows:
        if row.get("schema") != 1 or row.get("composition") != "engine-store-eight-actor" or row.get("runner_id") != "actor_workload":
            problems.append("unsupported eight-actor sample or runner")
        actor, sequence = row.get("actor_index"), row.get("sequence")
        if type(actor) is not int or type(sequence) is not int or actor not in range(8) or sequence not in range(10):
            problems.append("eight-actor sample is outside the finite workload")
        key = (actor, sequence)
        if key in cells:
            problems.append(f"duplicate actor cell {key}")
        cells.add(key)
        operation = operation_identity(row.get("operation_id"), problems, f"actor cell {key}")
        if operation in operations:
            problems.append(f"actor cell {key} repeats an original operation")
        operations.add(operation)
        if operation is not None:
            origin = json.dumps(row["operation_id"]["origin"], sort_keys=True)
            actors.setdefault(actor, set()).add(origin)
        if any(row.get(flag) is not True for flag in ("completed", "displayed", "typed_reply_confirmed", "owner_cleanup_confirmed")):
            problems.append(f"actor cell {key} lacks actual display/reply/owner cleanup")
        if type(row.get("elapsed_ns")) is not int or row["elapsed_ns"] < 0:
            problems.append(f"actor cell {key} has invalid latency")
        source = row.get("source")
        if not isinstance(source, str) or hashlib.sha256(source.encode()).hexdigest() != row.get("source_digest"):
            problems.append(f"actor cell {key} has no matching retained source")
        machine, owner = row.get("machine"), row.get("machine_owner")
        if row.get("counter_scope") != "exact-session-gauges-at-display-wave" or not isinstance(owner, dict) or not owner.get("session") or not owner.get("actor"):
            problems.append(f"actor cell {key} lacks exact machine counter ownership")
        if not isinstance(machine, dict) or any(type(machine.get(field)) is not int or machine[field] < 0 for field in gauge_fields):
            problems.append(f"actor cell {key} has unavailable native gauges")
        epoch = row.get("daemon_epoch")
        startup = ready.get(epoch)
        if not startup or startup.get("producer") != manifest.get("compiler_producer"):
            problems.append(f"actor cell {key} lacks an actual matched daemon epoch")
        issued = row.get("compiler_requests")
        if not isinstance(issued, list) or not issued:
            problems.append(f"actor cell {key} lacks exact compiler request attribution")
            continue
        for reference in issued:
            identity_record = invocation_identity(
                epoch, reference, problems, f"actor cell {key}")
            if identity_record is None:
                continue
            request_key, digest = identity_record
            if request_key in correlations:
                problems.append(f"actor compiler invocation {request_key} charged twice")
            correlations.add(request_key)
            starts = request_starts.get(request_key, [])
            terminals = request_terminals.get(request_key, [])
            if len(starts) != 1 or starts[0].get("compile_request") != digest:
                problems.append(f"actor compiler invocation {request_key} has no exact daemon start")
            if len(terminals) != 1:
                problems.append(f"actor compiler invocation {request_key} has {len(terminals)} terminal events")
                continue
            request = terminals[0]
            if (request.get("message") != "compiler request finished" or
                    request.get("compile_request") != digest or request.get("transport") != "daemon" or
                    request.get("exit_code") != 0 or not request.get("worker_pid") or not startup or
                    request.get("daemon_pid") != startup.get("daemon_pid")):
                problems.append(f"actor compiler invocation {request_key} lacks matched successful transport")
            if (request_key[0], request_key[1]) not in admissions:
                problems.append(f"actor compiler invocation {request_key} lacks its admission queue event")
            workers.setdefault((epoch, request.get("worker_pid")), set()).add(key)
    if cells != {(actor, sequence) for actor in range(8) for sequence in range(10)} or len(rows) != 80:
        problems.append("eight-actor workload requires ten sequential cells for each of eight actors")
    if len(actors) != 8 or any(len(origins) != 1 for origins in actors.values()) or len({origin for origins in actors.values() for origin in origins}) != 8:
        problems.append("eight-actor workload requires eight distinct exact actor incarnations")
    values = [row["elapsed_ns"] / 1_000_000 for row in rows if type(row.get("elapsed_ns")) is int and row["elapsed_ns"] >= 0]
    return {"status": "invalid" if problems else "measured", "count": len(rows), "actor_count": len(actors), "p50_ms": percentile(values, .5), "p95_ms": percentile(values, .95), "max_ms": max(values) if values else None, "limit_ms": None, "evidence_problems": sorted(set(problems)), "counter_note": "Native counters are exact session gauges at held display waves; shared inventory deltas are not attributed to actor operations."}


def analyze_durable(rows):
    """Attribute real durable publication independently of product acceptance."""
    if not rows:
        return {
            "status": "unmeasured", "count": 0,
            "matrix": {"status": "unmeasured", "expected_count": len(DURABLE_MATRIX),
                       "observed_count": 0, "observed": [], "missing": [], "duplicates": [],
                       "replicate_policy": "one publication per coordinate; schema has no replicate identity"},
            "evidence_problems": [], "current_public_schema": CURRENT_RECOVERY_SCHEMA,
            "observed_public_schemas": [],
        }
    problems = []
    phases = {}
    observed_schemas = set()
    matrix_rows = {}
    matrix_seen = False
    matrix_duplicates = set()
    byte_counters = ("checksum_encode_bytes", "recovery_validation_hash_bytes", "recovery_materialization_hash_bytes", "manifest_write_bytes")
    for row in rows:
        if row.get("schema") != 1 or row.get("composition") != "durable-publication" or row.get("completed") is not True:
            problems.append("unsupported or incomplete durable publication sample")
            continue
        for phase in ("elapsed_ns", "certification_ns", "metadata_stage_file_sync_ns", "publication_rename_directory_sync_ns"):
            value = row.get(phase)
            if value is None and phase == "certification_ns":
                continue
            if type(value) is not int or value < 0:
                problems.append(f"durable {row.get('cell')} has invalid {phase}")
            else:
                phases.setdefault(phase, []).append(value / 1_000_000)
        try:
            path = Path(row["manifest_path"])
            content = path.read_bytes()
            document = json.loads(content)
            if not isinstance(document, dict):
                raise ValueError("retained manifest is not an object")
            if not path.is_absolute() or len(content) != row.get("manifest_bytes"):
                problems.append("durable manifest size/path differs from retained snapshot")
            if document.get("checksum") != row.get("manifest_checksum") or not row.get("manifest_checksum"):
                problems.append("durable manifest checksum differs from retained snapshot")
            public_schema = document.get("public_schema")
            version = document.get("version")
            expected_version = RECOVERY_SCHEMA_VERSIONS.get(public_schema) if isinstance(public_schema, str) else None
            matching_format = (expected_version is not None and type(version) is int and
                               version == expected_version and row.get("public_schema") == public_schema)
            if matching_format:
                observed_schemas.add(public_schema)
            if public_schema in ("paired-public-v5", CURRENT_RECOVERY_SCHEMA):
                for counter in byte_counters:
                    if type(row.get(counter)) is not int or row[counter] < 0:
                        problems.append(f"durable {row.get('cell')} has unavailable {counter}")
                inventory = row.get("artifact_inventory")
                if row.get("inventory_counter_scope") != "shared-artifact-inventory-owner" or not isinstance(inventory, dict) or any(type(inventory.get(counter)) is not int or inventory[counter] < 0 for counter in ("structural_whole_graph_copies", "reclamation_runs", "reclamation_candidate_nodes", "reclaimed_nodes", "reclamation_elapsed_ns")):
                    problems.append(f"durable {row.get('cell')} has unavailable owning inventory counters")
            if not matching_format:
                problems.append("durable sample is not a matching retained artifact graph format")
            # The measured one-cell workload is the second cell, named
            # `prefix`: B is the preexisting baseline installed by setup and N
            # is the number of bindings introduced in this single prefix cell.
            # Foundation and baseline setup rows are intentionally excluded.
            # Historical reports retain their timings, but only the current
            # schema supplies the matrix used for durable acceptance.
            if matching_format and row.get("cell") == "prefix" and public_schema == CURRENT_RECOVERY_SCHEMA:
                matrix_seen = True
                baseline, prefix = row.get("baseline"), row.get("prefix")
                coordinate = (baseline, prefix)
                if coordinate not in DURABLE_MATRIX:
                    problems.append(f"durable prefix publication has unsupported B/N coordinate {coordinate}")
                elif coordinate in matrix_rows:
                    problems.append(f"duplicate durable prefix publication coordinate B{baseline}/N{prefix}")
                    matrix_duplicates.add(f"B{baseline}/N{prefix}")
                else:
                    matrix_rows[coordinate] = row
        except (KeyError, TypeError, OSError, ValueError):
            problems.append("durable sample has no readable retained manifest")
    if matrix_seen:
        missing = sorted(DURABLE_MATRIX - matrix_rows.keys())
        if missing:
            problems.extend(f"missing durable prefix publication coordinate B{baseline}/N{prefix}"
                            for baseline, prefix in missing)
        matrix_status = "measured" if not missing and not any(
            problem.startswith("duplicate durable prefix publication") or
            problem.startswith("durable prefix publication has unsupported")
            for problem in problems) else "invalid"
    else:
        missing = sorted(DURABLE_MATRIX)
        matrix_status = "unmeasured"
    if matrix_status == "invalid":
        problems.append("durable v6 prefix matrix is incomplete or duplicated")
    return {
        "status": "invalid" if problems else "measured", "count": len(rows),
        "matrix": {"status": matrix_status, "expected_count": len(DURABLE_MATRIX),
                   "observed_count": len(matrix_rows),
                   "observed": [f"B{baseline}/N{prefix}" for baseline, prefix in sorted(matrix_rows)],
                   "missing": [f"B{baseline}/N{prefix}" for baseline, prefix in missing],
                   "duplicates": sorted(matrix_duplicates),
                   "replicate_policy": "one publication per coordinate; schema has no replicate identity"},
        "phases_ms": {name: {"count": len(values), "p50_ms": percentile(values, 0.5), "p95_ms": percentile(values, 0.95)} for name, values in phases.items()},
        "samples": rows, "evidence_problems": sorted(set(problems)),
        "current_public_schema": CURRENT_RECOVERY_SCHEMA,
        "observed_public_schemas": sorted(observed_schemas),
        "format_note": "v4/v5 rows describe historical timing only; current recovery and durable acceptance require v6",
        "byte_counter_note": "v5/v6 encode/validation-hash/materialization-hash/write counters are per publication ticket; manifest_bytes is retained file size. Historical v4 counters remain unknown. Inventory counters are shared-owner gauges; structural_whole_graph_copies is an estimate, not a measured copy count.",
    }


def package_cold_samples(path, manifest):
    """Adapt the package owner's retained report without inventing provenance."""
    cold_report = json.loads(path.read_text())
    if cold_report.get("schema") not in (2, 3) or cold_report.get("failure") is not None:
        raise ValueError("package cold-start report is incomplete or unsupported")
    runners = cold_report.get("runners")
    cold_runner = runners.get("cold_start") if isinstance(runners, dict) else None
    if not isinstance(cold_runner, dict) or cold_runner.get("exit_code") != 0:
        raise ValueError("package cold-start report has no successful runner")
    source_samples = cold_report.get("samples")
    if not isinstance(source_samples, list) or len(source_samples) != MIN_COUNTS["cold_start"]:
        raise ValueError("package cold-start report must retain exactly five samples")
    if cold_runner.get("process_execution_count") != len(source_samples):
        raise ValueError("package cold-start process count differs from retained samples")
    if cold_runner.get("expected_test_count") != 0 or cold_runner.get("executed_test_count") != 0:
        raise ValueError("package cold-start runner must retain its zero-test count")
    command = cold_runner.get("command")
    if not isinstance(command, list) or not command or any(not isinstance(arg, str) for arg in command):
        raise ValueError("package cold-start runner has no exact command argv")
    for path_key, digest_key in (("binary_path", "binary_sha256"),
                                 ("package_entrypoint", "package_entrypoint_sha256")):
        if not retained_digest(cold_runner.get(path_key), cold_runner.get(digest_key)):
            raise ValueError(f"package cold-start {path_key} is not retained with its recorded digest")
    current = manifest.get("runners", {}).get("cold_start", {})
    if cold_report["schema"] == 3:
        operator = frozen_runner_evidence(manifest, cold_runner)
        frozen_store_schema_evidence(operator, cold_runner.get("harness_store_schema"))
        if not isinstance(current, dict):
            raise ValueError("manifest cold runner is malformed")
        if "native_qualification" in current and current["native_qualification"] != cold_runner["native_qualification"]:
            raise ValueError("manifest cold runner differs from the frozen qualification")
    elif not isinstance(current, dict) or not current.get("rust_build"):
        raise ValueError("manifest must supply the actual cold runner Rust build packet")
    for key in ("binary_path", "binary_sha256", "package_entrypoint", "package_entrypoint_sha256"):
        if key in current and current.get(key) != cold_runner.get(key):
            raise ValueError(f"cold report {key} differs from the manifest's built runner")
    store_schema = cold_runner.get("harness_store_schema")
    if not isinstance(store_schema, dict) or type(store_schema.get("version")) is not int:
        raise ValueError("package cold-start runner has no matched Store schema authority")

    converted = []
    traces = []
    seen_indexes = set()
    for source in source_samples:
        if not isinstance(source, dict) or source.get("kind") != "cold_start" or source.get("composition") != "engine-store":
            raise ValueError("package report contains a non-product cold sample")
        index = source.get("index")
        if type(index) is not int or index not in range(MIN_COUNTS["cold_start"]) or index in seen_indexes:
            raise ValueError("package cold-start indexes are missing, duplicated or outside the five samples")
        seen_indexes.add(index)
        if (source.get("completed") is not True or source.get("failure") is not None or
                source.get("init_exit_code") != 0 or source.get("stop_exit_code") != 0):
            raise ValueError(f"package cold-start sample {index} did not complete init and stop")
        if source.get("readiness") != "workspace-ready" or source.get("packaged") is not True:
            raise ValueError(f"package cold-start sample {index} lacks packaged workspace readiness")
        provider = source.get("provider_request_evidence")
        if (source.get("provider_requests") != 0 or not isinstance(provider, dict) or
                provider.get("verified") is not True or provider.get("provider_request_attempts") != 0):
            raise ValueError(f"package cold-start sample {index} lacks verified zero-provider evidence")
        store_files = provider.get("files")
        if not isinstance(store_files, list) or not store_files:
            raise ValueError(f"package cold-start sample {index} has no retained provider Store files")
        for store_file in store_files:
            store_path = store_file.get("path") if isinstance(store_file, dict) else None
            store_digest = store_file.get("sha256") if isinstance(store_file, dict) else None
            store_bytes = store_file.get("bytes") if isinstance(store_file, dict) else None
            if (not retained_digest(store_path, store_digest) or type(store_bytes) is not int or
                    Path(store_path).stat().st_size != store_bytes):
                raise ValueError(f"package cold-start sample {index} has an invalid retained Store file")
        if provider.get("store_path") != store_files[0].get("path"):
            raise ValueError(f"package cold-start sample {index} Store path differs from retained files")
        actual_provider = COLD_START_GATE.provider_store_evidence(
            Path(provider["store_path"]), store_schema["version"])
        for field in ("method", "store_schema_version", "provider_request_attempts",
                      "completed_model_turns", "responses_usage_events", "files"):
            if provider.get(field) != actual_provider.get(field):
                raise ValueError(f"package cold-start sample {index} Store {field} differs from SQLite")
        if actual_provider["provider_request_attempts"] != 0:
            raise ValueError(f"package cold-start sample {index} actually attempted a provider request")
        trace = source.get("daemon_trace")
        trace_digest = source.get("daemon_trace_sha256")
        if not retained_digest(trace, trace_digest):
            raise ValueError(f"package cold-start sample {index} has no retained daemon trace digest")
        trace_path = Path(trace).resolve(strict=True)
        trace_events = [fields(event) for event in read_jsonl(trace_path)]
        boots = [event for event in trace_events
                 if event.get("message") == "compiler daemon ready" and
                 event.get("run_id") == source.get("run_id")]
        if len(boots) != 1:
            raise ValueError(f"package cold-start sample {index} has no unique ready row for its run")
        boot = boots[0]
        if (boot.get("daemon_epoch") != source.get("daemon_epoch") or
                boot.get("daemon_pid") != source.get("daemon_pid") or
                boot.get("producer") != source.get("compiler_producer")):
            raise ValueError(f"package cold-start sample {index} differs from its retained ready row")
        traces.append(trace_path)
        row = dict(source)
        if row.get("compiler_producer") != manifest.get("compiler_producer"):
            raise ValueError(f"package cold-start sample {index} used another compiler producer")
        converted.append(row)
    if seen_indexes != set(range(MIN_COUNTS["cold_start"])):
        raise ValueError("package cold-start report is missing a sample index")
    return converted, cold_runner, traces


def merge_package_cold_runner(manifest, cold_runner):
    """Use actual package runner metadata while preserving build packet refs."""
    merged = dict(manifest)
    runners = dict(manifest.get("runners", {}))
    existing = dict(runners["cold_start"])
    rust_build = existing.get("rust_build")
    existing.update(cold_runner)
    if rust_build is not None:
        existing["rust_build"] = rust_build
    runners["cold_start"] = existing
    merged["runners"] = runners
    return merged


def read_jsonl(path, prefix=None):
    """Parse retained rows without also retaining the complete raw log text."""
    with path.open() as stream:
        for line in stream:
            if prefix is not None:
                _, marker, line = line.partition(prefix)
                if not marker:
                    continue
                yield json.loads(line)
            elif line.strip():
                yield json.loads(line)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=Path, required=True)
    parser.add_argument("--compiler-trace", type=Path, action="append", required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--actor-samples", type=Path, help="independent actor-performance rows from the eight-actor fixture")
    parser.add_argument("--durable-samples", type=Path, help="independent durable-performance rows; never count toward product acceptance")
    parser.add_argument("--package-cold-report", type=Path, help="retained report.json from package-cold-start-gate.py")
    args = parser.parse_args()
    samples = list(read_jsonl(args.samples, PREFIX))
    manifest = json.loads(args.manifest.read_text())
    cold_trace_paths = []
    cold_report_reference = None
    if not args.package_cold_report and any(row.get("kind") == "cold_start" for row in samples):
        parser.error("cold-start rows require --package-cold-report for Store and trace verification")
    if args.package_cold_report:
        cold_path = args.package_cold_report.resolve(strict=True)
        cold_digest_before = file_sha256(cold_path)
        cold_samples, cold_runner, cold_trace_paths = package_cold_samples(
            cold_path, manifest)
        if any(row.get("kind") == "cold_start" for row in samples):
            raise ValueError("--samples already contains cold-start rows; do not merge them twice")
        cold_digest_after = file_sha256(cold_path)
        if cold_digest_before != cold_digest_after:
            raise ValueError("package cold-start report changed during adaptation")
        samples.extend(cold_samples)
        manifest = merge_package_cold_runner(manifest, cold_runner)
        cold_report_reference = {
            "path": str(cold_path), "sha256": cold_digest_after,
        }
    trace_paths = []
    for path in [*args.compiler_trace, *cold_trace_paths]:
        resolved = path.resolve(strict=True)
        if resolved not in trace_paths:
            trace_paths.append(resolved)
    events = [event for path in trace_paths for event in read_jsonl(path)]
    if args.package_cold_report:
        expected_trace_hashes = {
            str(Path(row["daemon_trace"]).resolve(strict=True)): row["daemon_trace_sha256"]
            for row in samples if row.get("kind") == "cold_start"
        }
        for path in cold_trace_paths:
            if file_sha256(path) != expected_trace_hashes.get(str(path)):
                raise ValueError(f"cold compiler trace changed during reporting: {path}")
    report = analyze(samples, events, manifest)
    if cold_report_reference is not None:
        report["package_cold_report"] = cold_report_reference
    durable = []
    if args.durable_samples:
        durable = list(read_jsonl(args.durable_samples, "durable-performance "))
    report["durable_publication"] = analyze_durable(durable)
    actors = []
    if args.actor_samples:
        actors = list(read_jsonl(args.actor_samples, "actor-performance "))
    report["actor_workload"] = analyze_actors(actors, events, report["manifest"])
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"accepted": report["accepted"], "samples": report["samples"], "evidence_problems": report["evidence_problems"]}, indent=2))
    durable_ok = (not args.durable_samples or (
        report["durable_publication"]["status"] == "measured" and
        report["durable_publication"]["matrix"].get("status") == "measured"))
    return 0 if report["accepted"] and durable_ok and (not args.actor_samples or report["actor_workload"]["status"] == "measured") else 1


if __name__ == "__main__":
    raise SystemExit(main())
