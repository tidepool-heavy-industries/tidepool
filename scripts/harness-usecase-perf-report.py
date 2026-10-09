#!/usr/bin/env python3
"""Join retained Harness use-case phases to existing host and daemon traces.

The report keeps overlapping clocks separate. It never turns missing trace
records into zero latency or a claim that no queueing/compilation occurred.
"""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import sys


EXPECTED_PHASES = (
    "first-arithmetic", "publish-retained", "lookup-retained", "reuse-retained",
    "repeat-retained", "async-yield-result", "reuse-async-action", "repeat-arithmetic",
)
THREE_ACTOR_ROSTER_PATH = (Path(__file__).resolve().parents[1] /
                           "bridge/facade/src/actor_host/fixtures/three_actor_workload_roster.json")
if not THREE_ACTOR_ROSTER_PATH.is_file():
    THREE_ACTOR_ROSTER_PATH = (Path(__file__).resolve().parent / "test-fixtures" /
                               "bridge/facade/src/actor_host/fixtures/three_actor_workload_roster.json")
THREE_ACTOR_PHASES = tuple(json.loads(THREE_ACTOR_ROSTER_PATH.read_text()))
KNOWN_ROSTERS = {"harness-eight-phase": EXPECTED_PHASES,
                 "three-actor-capture": THREE_ACTOR_PHASES}
STARTUP_OWNER_SPANS = {"compile_root", "workspace_toolsets_prepare", "actor_application_prepare"}
PROVIDER_RESPONSE_PHASES = {
    "root-setup", "root-fork-capture", "async-yield", "child-alpha-reply",
    "child-beta-reply", "parent-publication-read", "explicit-child-cleanup",
}


def startup_owner(span):
    name = span.get("name")
    if name in ("compile_root", "actor_application_prepare"):
        return name if span.get("actor_path") == "root" else None
    if name == "workspace_toolsets_prepare":
        return name if all(isinstance(span.get(key), str) and span[key]
                           for key in ("workspace", "deployment")) else None
    return None


def nonnegative_integer(value):
    parsed = integer(value)
    return parsed if parsed is not None and parsed >= 0 else None


def assign_submission_phases(submissions, dispatches, phases, roster):
    """Reconcile independent owner observations once, before any phase joins."""
    phases_by_execution = {}
    for phase in phases:
        for execution in dispatches.get(str(phase.get("call_id")), set()):
            phases_by_execution.setdefault(execution, set()).add(phase["phase"])
    for request in submissions:
        candidates = set(request["phase_owners"])
        for execution in request["executions"]:
            candidates.update(phases_by_execution.get(execution, set()))
        if request["startup_owner_spans"]:
            candidates.add("activation")
        request["owner_candidates"] = sorted(candidates)
        request["phase_attribution"] = (
            "ambiguous" if len(candidates) > 1 else
            next(iter(candidates)) if len(candidates) == 1 and
                candidates.issubset(set(roster) | {"activation"}) else "unknown")



def read_json(path):
    with path.open() as stream:
        return json.load(stream)


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_jsonl(path):
    return list(iter_jsonl(path))


def iter_jsonl(path):
    with path.open() as stream:
        for line_number, line in enumerate(stream, 1):
            try:
                row = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{line_number}: invalid JSON: {error}") from error
            if not isinstance(row, dict):
                raise ValueError(f"{path}:{line_number}: expected a JSON object")
            yield row


def phase_rows(path):
    result = []
    for line_number, row in enumerate(read_jsonl(path), 1):
        if row.get("schema") != 1 or not isinstance(row.get("phase"), str):
            raise ValueError(f"{path}:{line_number}: unsupported phase record")
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
    for event_index, row in enumerate(host_rows):
        if (row.get("target") != "tidepool_extract_cmd::endpoint"
                or fields(row).get("message") != "compiler request identified"):
            continue
        identity = request_identity(row)
        executions = {str(span.get("execution")) for span in span_fields(row)
                      if span.get("name") == "cell" and span.get("execution") is not None}
        owners = {
            str(owner)
            for span in span_fields(row)
            for owner in (span.get("workload_phase"),)
            if isinstance(owner, str) and owner
        }
        direct_owner = attr(row, "workload_phase")
        if isinstance(direct_owner, str) and direct_owner:
            owners.add(direct_owner)
        startup_owners = sorted({owner for span in span_fields(row)
                                 if (owner := startup_owner(span)) is not None})
        submissions.append({"event_index": event_index, "identity": identity, "executions": sorted(executions),
                            "phase_owners": sorted(owners), "startup_owner_spans": startup_owners})

    services = []
    queues = {}
    unidentified_queue_events = 0
    for row in daemon_rows:
        event_fields = fields(row)
        phase = event_fields.get("phase")
        if phase == "compiler_service":
            services.append({
                "identity": request_identity(row),
                "elapsed_ms": nonnegative_integer(event_fields.get("elapsed_ms")),
                "exit_code": integer(event_fields.get("exit_code")),
                "message": event_fields.get("message"),
            })
        elif phase == "compiler_queue":
            identity = admission_identity(row)
            if identity is None:
                unidentified_queue_events += 1
                continue
            entry = queues.setdefault(identity, {"queue_ms": [], "messages": [],
                                                 "record_count": 0, "invalid_timing_record_count": 0, "grants": []})
            entry["record_count"] += 1
            entry["grants"].append({key: event_fields.get(key) for key in
                                    ("compiler_workload", "compiler_jobs", "compiler_capabilities")})
            value = nonnegative_integer(event_fields.get("queue_ms"))
            if value is not None:
                entry["queue_ms"].append(value)
            else:
                entry["invalid_timing_record_count"] += 1
            if event_fields.get("message"):
                entry["messages"].append(event_fields["message"])
    return dispatches, calls, submissions, services, queues, unidentified_queue_events


def runtime_cost_events(host_rows, phases):
    phase_by_call = {}
    for phase in phases:
        call_id = phase.get("call_id")
        if isinstance(call_id, str):
            phase_by_call.setdefault(call_id, set()).add(phase["phase"])
    events = []
    for index, row in enumerate(host_rows):
        if not str(row.get("target", "")).startswith("harness::runtime_cost"):
            continue
        call_ids = {str(value) for value in [fields(row).get("call_id"),
                                             *(span.get("call_id") for span in span_fields(row))]
                    if isinstance(value, str) and value}
        explicit_phases = {str(value) for value in [fields(row).get("workload_phase"),
                                                    *(span.get("workload_phase")
                                                      for span in span_fields(row))]
                           if isinstance(value, str) and value}
        candidates = explicit_phases | set().union(*(phase_by_call.get(call_id, set())
                                                       for call_id in call_ids))
        events.append({
            "event_index": index,
            "target": row.get("target"),
            "level": row.get("level"),
            "fields": fields(row),
            "span": row.get("span"),
            "spans": row.get("spans"),
            "call_id": next(iter(call_ids)) if len(call_ids) == 1 else None,
            "call_ids": sorted(call_ids),
            "phase_attribution": next(iter(candidates)) if len(candidates) == 1 else
                                 "unknown" if not candidates else "ambiguous",
        })
    return events


def runtime_cost_coverage(events):
    names = set()
    messages = set()
    for event in events:
        fields_value = event.get("fields") or {}
        message = fields_value.get("message")
        if isinstance(message, str):
            messages.add(message)
        spans = [event.get("span"), *(event.get("spans") or [])]
        names.update(span["name"] for span in spans
                     if isinstance(span, dict) and isinstance(span.get("name"), str))
    store_spans = {"load_history_window", "read_model_history_window", "context_request_state",
                   "projected_history_transaction", "lineage_query_and_decode",
                   "portable_history_projection", "append_items_inner", "append_items_transaction",
                   "seal_replay_request_with_hashes"}
    return {
        "observed_span_names": sorted(names),
        "observed_event_messages": sorted(messages),
        "host_admission_status": "events_observed" if "claim_operation" in names else "not_observed_unknown",
        "store_projection_status": "events_observed" if names.intersection(store_spans)
                                   else "not_observed_unknown",
        "successor_preparation_status": "events_observed" if "successor request prepared" in messages
                                       else "not_observed_unknown",
        "output_acknowledgement_status": "events_observed" if "acknowledge_output" in names
                                         else "not_observed_unknown",
    }


def analyze(record_path):
    record_path = Path(record_path).resolve()
    record = read_json(record_path)
    if not isinstance(record, dict):
        raise ValueError("runner record is not a JSON object")
    execution = record.get("execution")
    if not isinstance(execution, dict):
        raise ValueError("runner record has no execution object")
    artifact_root_value = execution.get("artifact_root")
    if not isinstance(artifact_root_value, str):
        raise ValueError("runner record has no per-case artifact root")
    artifact_root = Path(artifact_root_value)
    if not artifact_root.is_dir():
        raise ValueError(f"runner artifact root does not exist: {artifact_root}")
    phase_path = artifact_root / "phases.jsonl"
    if not phase_path.is_file():
        raise ValueError(f"runner artifact root has no phase JSONL: {phase_path}")
    phases = phase_rows(phase_path)
    environment = next((row for row in phases if row["phase"] == "environment"), None)
    if environment is None:
        raise ValueError("retained output has no environment phase")
    phase_value = environment.get("phase_trace")
    if not isinstance(phase_value, str) or not phase_value:
        raise ValueError("environment phase has no phase_trace path")
    if Path(phase_value) != phase_path:
        raise ValueError("environment phase_trace does not match runner artifact root")
    host_value = environment.get("host_trace")
    daemon_value = environment.get("compiler_trace")
    if not isinstance(host_value, str) or not host_value:
        raise ValueError("environment phase has no host_trace path")
    if not isinstance(daemon_value, str) or not daemon_value:
        raise ValueError("environment phase has no compiler_trace path")
    host_path = Path(host_value)
    daemon_path = Path(daemon_value)
    if not host_path.is_file():
        raise ValueError(f"environment host_trace does not exist: {host_path}")
    if not daemon_path.is_file():
        raise ValueError(f"environment compiler_trace does not exist: {daemon_path}")
    host_rows = read_jsonl(host_path)
    daemon_rows = iter_jsonl(daemon_path)
    dispatches, calls, submissions, services, queues, unidentified_queue_events = compiler_events(
        host_rows, daemon_rows)
    cost_events = runtime_cost_events(host_rows, phases)

    cohort_name = environment.get("workload_cohort")
    roster = environment.get("workload_roster")
    if roster is None and record.get("test", "").endswith(
            "harness_usecase_performance::production_harness_notebook_usecase_phases"):
        cohort_name, roster = "harness-eight-phase", list(EXPECTED_PHASES)
    if not isinstance(roster, list) or not roster or any(not isinstance(name, str) for name in roster):
        raise ValueError("environment phase must declare a nonempty workload_roster")
    if len(roster) != len(set(roster)):
        raise ValueError("workload_roster contains duplicate phase names")
    expected_roster = KNOWN_ROSTERS.get(cohort_name)
    if expected_roster is not None and tuple(roster) != expected_roster:
        raise ValueError(f"workload roster does not match declared cohort {cohort_name}")

    assign_submission_phases(submissions, dispatches, phases, roster)
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
        phase_evidence = phase.get("evidence", {})
        if not isinstance(phase_evidence, dict):
            phase_evidence = {}
        call_id = phase.get("call_id")
        execution_ids = sorted(dispatches.get(str(call_id), set())) if call_id else []
        phase_submissions = [request for request in submissions
                             if request["phase_attribution"] == phase["phase"]]
        identities = sorted({request["identity"] for request in phase_submissions
                              if request["identity"] is not None})
        digest_set = {identity[3] for identity in identities}
        service_rows = []
        for identity in identities:
            matched = services_by_identity.get(identity, [])
            timed_record = (len(matched) == 1 and matched[0]["elapsed_ms"] is not None
                            and matched[0]["exit_code"] == 0)
            service_rows.append({
                "daemon_epoch": identity[0], "admission_id": identity[1],
                "request_ordinal": identity[2], "compile_request": identity[3],
                "service_records": matched,
                "service_attribution": "one_timed_success" if timed_record else
                                       "missing" if not matched else
                                       "incomplete_record" if len(matched) == 1 else "ambiguous",
            })
        logical = nonnegative_integer(phase.get("logical_compiler_requests"))
        call_timing = [item for execution_id in execution_ids for item in calls.get(execution_id, [])]
        source = phase.get("source")
        source_digest = hashlib.sha256(source.encode()).hexdigest() if isinstance(source, str) else None
        phase_costs = [event for event in cost_events if event["phase_attribution"] == phase["phase"]]
        cost_coverage = runtime_cost_coverage(phase_costs)
        phase_queue_ids = sorted({(identity[0], identity[1]) for identity in identities})
        matched_services = sum(row["service_attribution"] == "one_timed_success" for row in service_rows)
        missing_services = sum(row["service_attribution"] == "missing" for row in service_rows)
        incomplete_services = sum(row["service_attribution"] == "incomplete_record" for row in service_rows)
        ambiguous_services = sum(row["service_attribution"] == "ambiguous" for row in service_rows)
        missing_identities = sum(request["identity"] is None for request in phase_submissions)
        unattributed_events = sum(request["phase_attribution"] in ("unknown", "ambiguous")
                                  for request in submissions)
        identity_counts = Counter(request["identity"] for request in phase_submissions
                                  if request["identity"] is not None)
        duplicate_identities = sum(count > 1 for count in identity_counts.values())
        if not phase_submissions and logical == 0 and unattributed_events == 0:
            attribution = "zero_logical_requests_no_correlated_submission_observed"
        elif (logical is not None and logical == len(phase_submissions)
              and not missing_identities and (len(execution_ids) == 1 or
                                               all(request["phase_owners"] == [phase["phase"]]
                                                   for request in phase_submissions))
              and len(identities) == len(phase_submissions) and duplicate_identities == 0
              and matched_services == len(identities)
              and not missing_services and not incomplete_services and not ambiguous_services):
            attribution = "complete"
        elif phase_submissions or logical or unattributed_events:
            attribution = "partial"
        else:
            attribution = "unknown"
        output_phases.append({
            "phase_record": phase,
            "phase": phase["phase"], "sequence": phase.get("sequence"), "call_id": call_id,
            "completed": phase.get("completed") is True,
            "behavior_role": phase_evidence.get("cell_role"),
            "invocation_execution": phase.get("invocation_execution"),
            "yielded_before_terminal": phase.get("yielded_before_terminal"),
            "wall_ns": nonnegative_integer(phase.get("wall_ns")),
            "first_successor_ns": nonnegative_integer(phase.get("first_successor_ns")),
            "scripted_response_hold_ns": nonnegative_integer(phase.get("scripted_response_hold_ns")),
            "logical_compiler_requests": logical,
            "host_submission_event_count": len(phase_submissions),
            "host_submission_identity_count": len(identities),
            "host_submission_events_missing_identity": missing_identities,
            "host_submission_duplicate_identity_count": duplicate_identities,
            "daemon_service_matched_count": matched_services,
            "daemon_service_missing_count": missing_services,
            "daemon_service_incomplete_count": incomplete_services,
            "daemon_service_ambiguous_count": ambiguous_services,
            "globally_unattributed_host_submission_event_count": unattributed_events,
            "dispatch_executions": execution_ids,
            "dispatch_attribution": "one" if len(execution_ids) == 1 else
                                    "missing" if not execution_ids else "ambiguous",
            "call_timing_records": call_timing,
            "harness_runtime_cost_events": phase_costs,
            "harness_runtime_cost_coverage": cost_coverage,
            "host_admission_status": cost_coverage["host_admission_status"],
            "client_compiler_submissions": service_rows,
            "compiler_attribution": attribution,
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
            "scripted_response_hold_status": "observed_test_coordination" if
                nonnegative_integer(phase.get("scripted_response_hold_ns")) is not None else "unknown",
            "store_projection_ms": None,
            "store_projection_status": cost_coverage["store_projection_status"],
        })
        if source_digest:
            prior_sources.add(source_digest)
        prior_digests.update(digest_set)

    workload_phases = [phase for phase in output_phases if phase["phase"] in roster]
    workload_records = [phase for phase in phases if phase["phase"] in roster]
    phase_counts = {name: sum(phase["phase"] == name for phase in workload_records)
                    for name in roster}
    phase_coverage = {
        "cohort": cohort_name,
        "expected_count": len(roster),
        "expected_order": list(roster),
        "observed_workload_record_count": len(workload_records),
        "completed_unique_phase_count": sum(
            count == 1 and next((phase.get("completed") is True for phase in workload_records
                                 if phase["phase"] == name), False)
            for name, count in phase_counts.items()),
        "missing": [name for name, count in phase_counts.items() if count == 0],
        "duplicates": [name for name, count in phase_counts.items() if count > 1],
        "unexpected": sorted({phase["phase"] for phase in output_phases
                              if phase["phase"] not in roster}),
        "sequence_errors": [
            {"phase": phase["phase"], "observed": phase.get("sequence"),
             "expected": roster.index(phase["phase"])}
            for phase in workload_records
            if phase.get("sequence") != roster.index(phase["phase"])
        ],
    }
    control_phase_counts = {name: sum(phase["phase"] == name for phase in phases)
                            for name in ("environment", "activation")}
    activation = next((phase for phase in phases if phase["phase"] == "activation"), None)
    phase_coverage["control_phases"] = {
        "counts": control_phase_counts,
        "missing": [name for name, count in control_phase_counts.items() if count == 0],
        "duplicates": [name for name, count in control_phase_counts.items() if count > 1],
    }
    observed_order = [phase["phase"] for phase in phases
                      if phase["phase"] in roster]
    phase_coverage["observed_order"] = observed_order
    phase_coverage["order_matches_expected"] = observed_order == list(roster)
    phase_coverage["complete"] = (
        not phase_coverage["missing"] and not phase_coverage["duplicates"]
        and not phase_coverage["unexpected"] and not phase_coverage["sequence_errors"]
        and phase_coverage["order_matches_expected"]
        and phase_coverage["control_phases"]["counts"] == {"environment": 1, "activation": 1}
        and phase_coverage["completed_unique_phase_count"] == len(roster)
    )

    invalid_phase_measurements = []
    activation_evidence = activation.get("evidence", {}) if activation else {}
    if not isinstance(activation_evidence, dict):
        activation_evidence = {}
    workspace_preparation_ns = (activation.get("workspace_preparation_ns") if activation else None)
    if workspace_preparation_ns is None:
        workspace_preparation_ns = activation_evidence.get("workspace_preparation_ns")
    host_start_readiness_ns = (activation.get("host_start_readiness_ns") if activation else None)
    if host_start_readiness_ns is None:
        host_start_readiness_ns = activation_evidence.get("host_start_readiness_ns")
    for phase in phases:
        if phase["phase"] == "environment":
            continue
        invalid_fields = [name for name in ("wall_ns", "logical_compiler_requests")
                          if type(phase.get(name)) is not int or phase[name] < 0]
        if cohort_name == "three-actor-capture" and phase["phase"] == "activation":
            invalid_fields.extend(name for name, value in (
                ("workspace_preparation_ns", workspace_preparation_ns),
                ("host_start_readiness_ns", host_start_readiness_ns),
            ) if type(value) is not int or value < 0)
        if cohort_name == "three-actor-capture" and phase["phase"] in PROVIDER_RESPONSE_PHASES:
            if type(phase.get("scripted_response_hold_ns")) is not int or phase["scripted_response_hold_ns"] < 0:
                invalid_fields.append("scripted_response_hold_ns")
        if "first_successor_ns" in phase and (
                type(phase["first_successor_ns"]) is not int or phase["first_successor_ns"] < 0):
            invalid_fields.append("first_successor_ns")
        if invalid_fields:
            invalid_phase_measurements.append({"phase": phase["phase"],
                                               "sequence": phase.get("sequence"),
                                               "invalid_fields": invalid_fields})

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
            "queue_event_count": details["record_count"],
            "invalid_timing_record_count": details["invalid_timing_record_count"],
            "related_phases": sorted(phase_by_admission.get((epoch, admission), set())),
            "compiler_grants": details["grants"],
            "attribution_scope": "daemon worker admission; do not add to per-request service",
        })
    requested_allowances = execution.get("compiler_allowances") or {}
    compiler_grants = []
    for observation in queue_output:
        for grant in observation["compiler_grants"]:
            workload, jobs, capabilities = (grant[key] for key in
                                            ("compiler_workload", "compiler_jobs", "compiler_capabilities"))
            valid = (workload in ("foreground", "preparation") and type(jobs) is int and jobs > 0
                     and type(capabilities) is int and capabilities > 0)
            requested = requested_allowances.get("requested_" + str(workload) + "_jobs")
            compiler_grants.append({"daemon_epoch": observation["daemon_epoch"],
                                    "admission_id": observation["admission_id"],
                                    "workload": workload, "jobs": jobs, "capabilities": capabilities,
                                    "valid": valid, "requested_jobs": requested,
                                    "below_requested_maximum": jobs < requested if valid and requested is not None else None,
                                    "exceeds_requested_maximum": jobs > requested if valid and requested is not None else None})
    all_host_identities = {request["identity"] for request in submissions
                           if request["identity"] is not None}
    unattributed_host = []
    for request in submissions:
        if request["identity"] is None or request["phase_attribution"] in ("unknown", "ambiguous"):
            identity = request["identity"]
            matched = services_by_identity.get(identity, []) if identity is not None else []
            unattributed_host.append({
                "identity": identity, "executions": request["executions"],
                "phase_owners": request["phase_owners"],
                "startup_owner_spans": request["startup_owner_spans"],
                "owner_candidates": request["owner_candidates"],
                "phase_attribution": request["phase_attribution"],
                "daemon_service_records": matched,
            })
    orphan_services = [service for service in services
                       if service["identity"] is None or service["identity"] not in all_host_identities]

    host_identity_counts = Counter(request["identity"] for request in submissions
                                   if request["identity"] is not None)
    service_identity_counts = Counter(service["identity"] for service in services
                                      if service["identity"] is not None)
    host_identities = [request["identity"] for request in submissions]
    service_identities = [service["identity"] for service in services]
    queue_admissions_expected = {(identity[0], identity[1]) for identity in all_host_identities}
    queue_missing = sorted(queue_admissions_expected - set(queues))
    queue_unexpected = sorted(set(queues) - queue_admissions_expected)
    queue_duplicate_records = sorted(identity for identity, details in queues.items()
                                     if details["record_count"] != 1 or len(details["queue_ms"]) != 1)
    workload_exact = all(
        phase["compiler_attribution"] in ("complete", "zero_logical_requests_no_correlated_submission_observed")
        for phase in workload_phases
    )
    whole_stream_complete = (
        len(host_identities) == len(service_identities)
        and all(identity is not None for identity in host_identities + service_identities)
        and Counter(host_identities) == Counter(service_identities)
        and all(count == 1 for count in host_identity_counts.values())
        and all(count == 1 for count in service_identity_counts.values())
    )
    physical_services_successful = all(
        service["elapsed_ms"] is not None and service["exit_code"] == 0 for service in services
    )
    startup_unowned = [item for item in unattributed_host]
    ambiguous_owner_submissions = [request for request in submissions
                                   if request["phase_attribution"] == "ambiguous"]
    unknown_owner_submissions = [request for request in submissions
                                 if len(request["phase_owners"]) == 1
                                 and request["phase_owners"][0] not in roster
                                 and request["phase_owners"][0] != "activation"]
    activation_record = next((phase for phase in phases if phase["phase"] == "activation"), None)
    startup_owned_requests = [request for request in submissions
                              if request["phase_attribution"] == "activation" and not request["phase_owners"]
                              and request["startup_owner_spans"]]
    ambiguous_startup_owner_requests = [request for request in submissions
                                        if request["startup_owner_spans"] and
                                        request["phase_attribution"] == "ambiguous"]
    explicit_activation_requests = [request for request in submissions
                                    if request["phase_attribution"] == "activation" and
                                    request["phase_owners"] == ["activation"]]
    startup_owner_complete = (
        not startup_unowned and not ambiguous_owner_submissions and not unknown_owner_submissions
        and not ambiguous_startup_owner_requests
        and activation_record is not None
        and integer(activation_record.get("logical_compiler_requests")) ==
            len(explicit_activation_requests) + len(startup_owned_requests)
        and all(len(services_by_identity.get(request["identity"], [])) == 1
                and services_by_identity[request["identity"]][0]["exit_code"] == 0
                for request in explicit_activation_requests + startup_owned_requests)
    )
    startup_owner_status = "complete" if startup_owner_complete else "unknown_unowned_or_unmatched_requests"
    queue_complete = (bool(queues) and unidentified_queue_events == 0 and not queue_missing
                      and not queue_unexpected and not queue_duplicate_records)
    cleanup = {
        key: execution.get(key)
        for key in ("process_cleanup_status", "hosted_cleanup_status", "compiler_cleanup_status",
                    "compiler_cleanup_observation_complete")
    }
    cleanup["complete"] = all(cleanup[key] == "confirmed" for key in (
        "process_cleanup_status", "hosted_cleanup_status", "compiler_cleanup_status")) \
        and cleanup["compiler_cleanup_observation_complete"] is True
    prepared = environment.get("prepared_root_entry_supplied") is True
    diagnostic = execution.get("diagnostic_summaries") or {}
    compiler_scan = diagnostic.get("compiler_trace_scan") or {}
    physical_sample = diagnostic.get("physical_compiler_timing") or {}
    queue_sample = diagnostic.get("compiler_job_queue") or {}
    raw_capture_complete = (
        compiler_scan.get("complete") is True
        and physical_sample.get("request_count_complete") is True
        and queue_sample.get("physical_job_count_complete") is True
    )
    raw_file_fingerprints = {
        "host_trace": {"bytes": host_path.stat().st_size, "sha256": sha256_file(host_path)},
        "compiler_trace": {"bytes": daemon_path.stat().st_size, "sha256": sha256_file(daemon_path)},
    }

    return {
        "schema": 1,
        "runner": {
            "test": record.get("test"), "passed": record.get("passed"),
            "executed_test_count": execution.get("executed_test_count"),
            "compiler_mode": execution.get("compiler_mode"),
            "artifacts_retained_after_success": execution.get("artifacts_retained_after_success"),
            "diagnostic_evidence_complete": execution.get("diagnostic_evidence_complete"),
            "phase_trace": str(phase_path),
            "cleanup": cleanup,
        },
        "environment": {
            "prepared_root_entry_supplied": environment.get("prepared_root_entry_supplied"),
            "deployment": environment.get("deployment"),
            "execution_profile_observed": "prepared-root-entry-supplied" if prepared else
                                           "source-backed-or-unprepared",
            "frozen_catalog_qualification": "requires-descriptor-identity-check" if prepared else
                                             "not-established-by-this-counted-test",
        },
        "activation": activation,
        "activation_timing": {
            "workspace_preparation_ns": nonnegative_integer(workspace_preparation_ns),
            "host_start_readiness_ns": nonnegative_integer(host_start_readiness_ns),
            "scope": "workspace preparation and production readiness are separate nested startup intervals",
        },
        "phase_coverage": phase_coverage,
        "phase_measurements": {
            "status": "complete" if not invalid_phase_measurements else "partial_or_unknown",
            "invalid_records": invalid_phase_measurements,
            "required_values": "wall_ns and logical_compiler_requests are nonnegative JSON integers",
        },
        "timing_contract": {
            "phase_wall_and_first_successor_overlap_call_timing_and_compiler_spans": True,
            "compiler_service_is_per_request_and_not_added_to_phase_wall": True,
            "queue_wait_is_per_admission_and_not_added_to_request_service": True,
            "missing_queue_records_mean_unknown_not_zero": True,
            "nested_runtime_cost_events_are_retained_individually_not_summed": True,
            "scripted_response_hold_is_test_coordination_not_model_latency": True,
        },
        "harness_runtime_cost_trace": {
            "status": "events_observed" if cost_events else "no_records_observed_unknown",
            "event_count": len(cost_events),
            "phase_attributed_event_count": sum(event["phase_attribution"] not in ("unknown", "ambiguous")
                                                 for event in cost_events),
            "unattributed_event_count": sum(event["phase_attribution"] == "unknown" for event in cost_events),
            "ambiguous_event_count": sum(event["phase_attribution"] == "ambiguous" for event in cost_events),
            "events": cost_events,
            "coverage": runtime_cost_coverage(cost_events),
        },
        "queue_trace_status": ("records_observed" if queues else "no_records_observed_unknown"),
        "queue_evidence": {
            "status": "complete" if queue_complete else "partial_or_unknown",
            "expected_admission_count_from_exact_requests": len(queue_admissions_expected),
            "observed_admission_count": len(queues),
            "missing_admissions": [list(identity) for identity in queue_missing],
            "unexpected_admissions": [list(identity) for identity in queue_unexpected],
            "admissions_with_non_single_queue_timing": [list(identity) for identity in queue_duplicate_records],
            "unidentified_queue_event_count": unidentified_queue_events,
        },
        "workload_request_service_joins": {
            "status": "complete" if phase_coverage["complete"] and workload_exact else "partial_or_unknown",
            "logical_request_count": sum(phase.get("logical_compiler_requests") or 0 for phase in workload_phases),
            "exact_phase_request_join_count": sum(phase["daemon_service_matched_count"] for phase in workload_phases),
        },
        "whole_physical_stream_reconciliation": {
            "status": "complete" if whole_stream_complete else "partial_or_unknown",
            "raw_host_submission_count": len(submissions),
            "raw_physical_service_count": len(services),
            "host_identity_count": len(all_host_identities),
            "duplicate_host_identity_count": sum(count > 1 for count in host_identity_counts.values()),
            "duplicate_service_identity_count": sum(count > 1 for count in service_identity_counts.values()),
            "unmatched_physical_service_count": len(orphan_services),
            "host_submissions_missing_identity": sum(request["identity"] is None for request in submissions),
            "physical_service_outcome_status": "all_successful" if physical_services_successful
                                               else "failed_or_incomplete",
            "physical_service_failed_or_incomplete_count": sum(
                service["elapsed_ms"] is None or service["exit_code"] != 0 for service in services),
            "raw_jsonl_scan": "all_rows_parsed",
            "raw_event_capture_status": "complete" if raw_capture_complete else "unknown_or_partial",
            "raw_trace_files": raw_file_fingerprints,
            "runner_raw_scan": {
                "compiler_trace_scan_complete": compiler_scan.get("complete"),
                "physical_request_count_complete": physical_sample.get("request_count_complete"),
                "queue_physical_job_count_complete": queue_sample.get("physical_job_count_complete"),
            },
            "detailed_compiler_sample_status": "complete" if physical_sample.get("complete") is True
                                               else "truncated_or_unknown",
            "detailed_compiler_sample_records_truncated": physical_sample.get("records_truncated"),
            "runner_diagnostic_sample_status": "complete" if execution.get("diagnostic_evidence_complete") is True
                                               else "incomplete_or_unknown",
        },
        "startup_scope": {
            "owner_status": startup_owner_status,
            "unowned_host_submission_count": len(startup_unowned),
            "ambiguous_owner_submission_count": len(ambiguous_owner_submissions),
            "unknown_owner_submission_count": len(unknown_owner_submissions),
            "explicit_activation_owner_request_count": len(explicit_activation_requests),
            "explicit_startup_span_owner_request_count": len(startup_owned_requests),
            "recognized_startup_owner_spans": sorted(STARTUP_OWNER_SPANS),
            "explicit_startup_span_requests": [
                {"identity": request["identity"],
                 "owner_spans": request["startup_owner_spans"],
                 "daemon_service_records": services_by_identity.get(request["identity"], [])}
                for request in startup_owned_requests
            ],
            "requests": startup_unowned,
            "ambiguous_startup_span_requests": ambiguous_startup_owner_requests,
            "ambiguous_requests": ambiguous_owner_submissions,
            "unknown_owner_requests": unknown_owner_submissions,
            "assignment_policy": "exact_declared_phase_or_root_startup_owner; no count or timestamp assignment",
        },
        "compiler_allowances": requested_allowances,
        "compiler_job_grants": {
            "status": "observed" if (queue_complete and compiler_grants
                                     and all(row["valid"] and row["exceeds_requested_maximum"] is not True
                                             for row in compiler_grants)) else "partial_or_unknown",
            "admissions": compiler_grants,
            "observed_jobs": sorted({row["jobs"] for row in compiler_grants if row["valid"]}),
            "observed_capabilities": sorted({row["capabilities"] for row in compiler_grants if row["valid"]}),
            "interpretation": "Requested widths are maxima; admitted grants may be capacity capped. Worker processes and test concurrency are separate.",
        },
        "queue_observations": queue_output,
        "unattributed_compiler_submissions": unattributed_host,
        "unmatched_daemon_service_records": orphan_services,
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
