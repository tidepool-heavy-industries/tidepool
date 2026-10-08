#!/usr/bin/env python3
"""Join retained Harness use-case phases to existing host and daemon traces.

The report keeps overlapping clocks separate. It never turns missing trace
records into zero latency or a claim that no queueing/compilation occurred.
"""

import argparse
import hashlib
import json
from pathlib import Path
import sys


PREFIX = "harness-usecase "


def read_json(path):
    with path.open() as stream:
        return json.load(stream)


def read_jsonl(path):
    rows = []
    with path.open() as stream:
        for line_number, line in enumerate(stream, 1):
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{line_number}: invalid JSON: {error}") from error
    return rows


def phase_rows(stdout_path):
    result = []
    with stdout_path.open() as stream:
        for line_number, line in enumerate(stream, 1):
            if not line.startswith(PREFIX):
                continue
            try:
                row = json.loads(line[len(PREFIX):])
            except json.JSONDecodeError as error:
                raise ValueError(f"{stdout_path}:{line_number}: invalid phase JSON: {error}") from error
            if row.get("schema") != 1 or not isinstance(row.get("phase"), str):
                raise ValueError(f"{stdout_path}:{line_number}: unsupported phase record")
            result.append(row)
    return result


def fields(row):
    value = row.get("fields")
    return value if isinstance(value, dict) else {}


def span_fields(row):
    """Return tracing JSON's current and ancestor span attribute maps."""
    result = []
    for key in ("span", "spans"):
        value = row.get(key)
        values = value if isinstance(value, list) else [value]
        for span in values:
            if isinstance(span, dict):
                result.append(span)
    return result


def attr(row, name):
    direct = fields(row).get(name)
    if direct is not None:
        return direct
    for span in span_fields(row):
        if span.get(name) is not None:
            return span[name]
    return None


def integer(value):
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str) and value.isdecimal():
        return int(value)
    return None


def request_identity(row):
    epoch = attr(row, "daemon_epoch")
    admission = integer(attr(row, "admission_id"))
    ordinal = integer(attr(row, "request_ordinal"))
    digest = attr(row, "compile_request")
    physical = attr(row, "physical_execution")
    if physical is not None and (epoch is None or admission is None or ordinal is None):
        parts = str(physical).split(":")
        if len(parts) == 3:
            epoch = epoch or parts[0]
            admission = admission if admission is not None else integer(parts[1])
            ordinal = ordinal if ordinal is not None else integer(parts[2])
    if epoch is None or admission is None or ordinal is None or digest is None:
        return None
    return str(epoch), admission, ordinal, str(digest)


def admission_identity(row):
    epoch = attr(row, "daemon_epoch")
    admission = integer(attr(row, "admission_id"))
    physical = attr(row, "physical_execution")
    if physical is not None and (epoch is None or admission is None):
        parts = str(physical).split(":")
        if len(parts) == 3:
            epoch = epoch or parts[0]
            admission = admission if admission is not None else integer(parts[1])
    if epoch is None or admission is None:
        return None
    return str(epoch), admission


def compiler_events(host_rows, daemon_rows):
    dispatches = {}
    for row in host_rows:
        if fields(row).get("message") != "workbench cell dispatched to its actor":
            continue
        call_id = fields(row).get("context_call_id")
        execution = fields(row).get("execution")
        if call_id and execution:
            dispatches.setdefault(str(call_id), set()).add(str(execution))

    calls = {}
    for row in host_rows:
        if (row.get("target") == "exomonad_actor::call_timing"
                and fields(row).get("message") == "call timing"):
            execution = fields(row).get("execution")
            if execution:
                calls.setdefault(str(execution), []).append(fields(row))

    submissions = []
    for row in host_rows:
        if (row.get("target") != "tidepool_extract_cmd::endpoint"
                or fields(row).get("message") != "compiler request identified"):
            continue
        identity = request_identity(row)
        executions = {str(span.get("execution")) for span in span_fields(row)
                      if span.get("execution") is not None}
        # Some formatter versions include the cell execution on the current span.
        if attr(row, "execution") is not None:
            executions.add(str(attr(row, "execution")))
        submissions.append({"identity": identity, "executions": sorted(executions)})

    services = []
    queues = {}
    for row in daemon_rows:
        event_fields = fields(row)
        phase = event_fields.get("phase")
        if phase == "compiler_service":
            services.append({
                "identity": request_identity(row),
                "elapsed_ms": integer(event_fields.get("elapsed_ms")),
                "exit_code": integer(event_fields.get("exit_code")),
                "message": event_fields.get("message"),
            })
        elif phase == "compiler_queue":
            identity = admission_identity(row)
            if identity is None:
                continue
            entry = queues.setdefault(identity, {"queue_ms": [], "messages": []})
            value = integer(event_fields.get("queue_ms"))
            if value is not None:
                entry["queue_ms"].append(value)
            if event_fields.get("message"):
                entry["messages"].append(event_fields["message"])
    return dispatches, calls, submissions, services, queues


def analyze(record_path):
    record_path = Path(record_path).resolve()
    record = read_json(record_path)
    execution = record.get("execution")
    if not isinstance(execution, dict):
        raise ValueError("runner record has no execution object")
    artifact_root_value = execution.get("artifact_root")
    if not isinstance(artifact_root_value, str):
        raise ValueError("runner record has no per-case artifact root")
    artifact_root = Path(artifact_root_value)
    stdout = record.get("streams", {}).get("stdout", {})
    stdout_name = stdout.get("path")
    if not isinstance(stdout_name, str) or Path(stdout_name).name != stdout_name:
        raise ValueError("runner record has no safe retained stdout path")
    if stdout.get("truncated") is True:
        raise ValueError("retained stdout is truncated; phase coverage is incomplete")
    phases = phase_rows(record_path.parent / stdout_name)
    environment = next((row for row in phases if row["phase"] == "environment"), None)
    if environment is None:
        raise ValueError("retained output has no environment phase")
    host_path = Path(environment["host_trace"])
    daemon_path = Path(environment["compiler_trace"])
    host_rows = read_jsonl(host_path)
    daemon_rows = read_jsonl(daemon_path)
    dispatches, calls, submissions, services, queues = compiler_events(host_rows, daemon_rows)

    by_execution = {}
    for request in submissions:
        for execution_id in request["executions"]:
            by_execution.setdefault(execution_id, []).append(request["identity"])
    services_by_identity = {}
    for service in services:
        if service["identity"] is not None:
            services_by_identity.setdefault(service["identity"], []).append(service)

    prior_sources = set()
    prior_digests = set()
    output_phases = []
    for phase in phases:
        if phase["phase"] in ("environment", "activation"):
            continue
        call_id = phase.get("call_id")
        execution_ids = sorted(dispatches.get(str(call_id), set())) if call_id else []
        identity_set = set()
        for execution_id in execution_ids:
            identity_set.update(identity for identity in by_execution.get(execution_id, [])
                                if identity is not None)
        identities = sorted(identity_set)
        digest_set = {identity[3] for identity in identities}
        service_rows = []
        for identity in identities:
            matched = services_by_identity.get(identity, [])
            service_rows.append({
                "daemon_epoch": identity[0], "admission_id": identity[1],
                "request_ordinal": identity[2], "compile_request": identity[3],
                "service_records": matched,
                "service_attribution": "one" if len(matched) == 1 else
                                       "missing" if not matched else "ambiguous",
            })
        logical = integer(phase.get("logical_compiler_requests"))
        call_timing = [item for execution_id in execution_ids for item in calls.get(execution_id, [])]
        source = phase.get("source")
        source_digest = hashlib.sha256(source.encode()).hexdigest() if isinstance(source, str) else None
        phase_queue_ids = sorted({(identity[0], identity[1]) for identity in identities})
        output_phases.append({
            "phase": phase["phase"], "sequence": phase.get("sequence"), "call_id": call_id,
            "invocation_execution": phase.get("invocation_execution"),
            "yielded_before_terminal": phase.get("yielded_before_terminal"),
            "wall_ns": integer(phase.get("wall_ns")),
            "first_successor_ns": integer(phase.get("first_successor_ns")),
            "logical_compiler_requests": logical,
            "dispatch_executions": execution_ids,
            "call_timing_records": call_timing,
            "host_admission_status": "not_separately_instrumented",
            "client_compiler_submissions": service_rows,
            "compiler_attribution": "joined" if identities else
                                    "no_correlated_submission_observed" if logical == 0 else
                                    "incomplete_submission_trace",
            "source_sha256": source_digest,
            "same_source_seen_in_prior_phase": source_digest in prior_sources if source_digest else None,
            "compiler_request_digest_seen_before": {
                digest: digest in prior_digests for digest in sorted(digest_set)
            },
            "compiler_cache_hit_or_miss": "not_established_by_request_repetition_alone",
            "queue_admission_ids": [
                {"daemon_epoch": epoch, "admission_id": admission,
                 "scope": "admission, not an individual request"}
                for epoch, admission in phase_queue_ids
            ],
            "provider_model_latency_ms": None,
            "provider_model_latency_status": "not_measured_scripted_provider",
            "store_projection_ms": None,
            "store_projection_status": "not_instrumented",
        })
        if source_digest:
            prior_sources.add(source_digest)
        prior_digests.update(digest_set)

    queue_output = []
    phase_by_admission = {}
    for phase in output_phases:
        for admission in phase["queue_admission_ids"]:
            key = (admission["daemon_epoch"], admission["admission_id"])
            phase_by_admission.setdefault(key, set()).add(phase["phase"])
    for (epoch, admission), details in sorted(queues.items()):
        queue_output.append({
            "daemon_epoch": epoch, "admission_id": admission,
            "queue_ms_records": details["queue_ms"],
            "related_phases": sorted(phase_by_admission.get((epoch, admission), set())),
            "attribution_scope": "daemon worker admission; do not add to per-request service",
        })

    return {
        "schema": 1,
        "runner": {
            "test": record.get("test"), "passed": record.get("passed"),
            "executed_test_count": execution.get("executed_test_count"),
            "compiler_mode": execution.get("compiler_mode"),
            "artifacts_retained_after_success": execution.get("artifacts_retained_after_success"),
            "diagnostic_evidence_complete": execution.get("diagnostic_evidence_complete"),
        },
        "environment": {
            "prepared_root_entry_supplied": environment.get("prepared_root_entry_supplied"),
            "deployment": environment.get("deployment"),
            "frozen_catalog_qualification": "not_established_by_this_counted_test",
        },
        "timing_contract": {
            "phase_wall_and_first_successor_overlap_call_timing_and_compiler_spans": True,
            "compiler_service_is_per_request_and_not_added_to_phase_wall": True,
            "queue_wait_is_per_admission_and_not_added_to_request_service": True,
            "missing_queue_records_mean_unknown_not_zero": True,
        },
        "queue_trace_status": ("records_observed" if queues else "no_records_observed_unknown"),
        "queue_observations": queue_output,
        "unattributed_compiler_submissions": [
            {"identity": item["identity"], "executions": item["executions"]}
            for item in submissions
            if item["identity"] is None or not item["executions"]
        ],
        "phases": output_phases,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runner_record", type=Path,
                        help="retained <sha256(test-name)>.json from isolated-libtest --output-dir")
    options = parser.parse_args(argv)
    try:
        report = analyze(options.runner_record)
    except (OSError, ValueError, TypeError) as error:
        print(f"harness use-case report failed: {error}", file=sys.stderr)
        return 2
    json.dump(report, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
