#!/usr/bin/env python3
"""Join retained Harness use-case phases to existing host and daemon traces.

The report keeps overlapping clocks separate. It never turns missing trace
records into zero latency or a claim that no queueing/compilation occurred.
"""

import argparse
from collections import Counter
from datetime import datetime, timezone
from decimal import Decimal
import hashlib
import json
import re
from pathlib import Path
import sys


EXPECTED_PHASES = (
    "first-arithmetic", "publish-retained", "lookup-retained", "reuse-retained",
    "repeat-retained", "async-yield-result", "reuse-async-action", "repeat-arithmetic",
)
PERSONA_PHASES = (
    "persona-setup", "fresh-source-display", "fresh-context-read",
    "accumulate-one", "accumulate-two", "accumulate-three", "accumulate-four",
    "warm-source-display-first", "warm-source-display-repeat",
    "warm-context-read-first", "warm-context-read-repeat",
    "context-edit-five", "context-edit-readback",
)
THREE_ACTOR_ROSTER_PATH = (Path(__file__).resolve().parents[1] /
                           "exomonad/host/src/actor_host/fixtures/three_actor_workload_roster.json")
if not THREE_ACTOR_ROSTER_PATH.is_file():
    THREE_ACTOR_ROSTER_PATH = Path(__file__).resolve().parent / "three-actor-workload-roster.json"
THREE_ACTOR_PHASES = tuple(json.loads(THREE_ACTOR_ROSTER_PATH.read_text()))
KNOWN_ROSTERS = {"harness-eight-phase": EXPECTED_PHASES,
                 "persona-workbench": PERSONA_PHASES,
                 "three-actor-capture": THREE_ACTOR_PHASES,
                 "two-children-concurrent": ("activation", "root-setup", "root-fork-capture", "async-yield", "children-concurrent-replies", "parent-publication-read", "explicit-child-cleanup", "host-cleanup"),
                 "notebook-a-a-b-a": ("first-a", "repeat-a", "changed-b", "return-a"),
                 "prepare-once-repeated-launch": ("fresh-preparation", "launch-1", "launch-2", "launch-3", "host-cleanup")}
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


def phase_call_ids(phase):
    call = phase.get("call_id")
    calls = phase.get("call_ids")
    if calls is not None and (not isinstance(calls, list) or not calls
            or any(not isinstance(value, str) or not value for value in calls)
            or len(calls) != len(set(calls))):
        raise ValueError("phase call_ids must contain unique nonempty exact operation call IDs")
    result = ([call] if isinstance(call, str) and call else []) + (calls or [])
    if len(result) != len(set(result)):
        raise ValueError("phase call_id and call_ids must not repeat an exact operation")
    return result


def assign_submission_phases(submissions, dispatches, phases, roster):
    """Reconcile independent owner observations once, before any phase joins."""
    phases_by_execution = {}
    for phase in phases:
        for execution in set().union(*(dispatches.get(call, set()) for call in phase_call_ids(phase))):
            phases_by_execution.setdefault(execution, set()).add(phase["phase"])
    for request in submissions:
        candidates = set(request["phase_owners"])
        for execution in request["executions"]:
            candidates.update(phases_by_execution.get(execution, set()))
        if request["startup_owner_spans"]:
            candidates.add("fresh-preparation" if "fresh-preparation" in roster and
                           "workspace_toolsets_prepare" in request["startup_owner_spans"] else "activation")
        if (request.get("activation_input_origin") is not None
                and request["activation_input_origin"]["status"] != "verified"):
            candidates.clear()
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


def physical_coordinates(row):
    epoch = attr(row, "daemon_epoch")
    admission = integer(attr(row, "admission_id"))
    ordinal = integer(attr(row, "request_ordinal"))
    physical = attr(row, "physical_execution")
    if physical is not None:
        parts = str(physical).split(":")
        if len(parts) != 3 or not parts[0] or integer(parts[1]) is None or integer(parts[2]) is None:
            raise ValueError("malformed physical_execution identity")
        alias = (parts[0], integer(parts[1]), integer(parts[2]))
        if any(value is not None and value != supplied
               for value, supplied in zip((epoch, admission, ordinal), alias)):
            raise ValueError("physical_execution contradicts explicit physical request identity")
        epoch, admission, ordinal = (value if value is not None else supplied
                                     for value, supplied in zip((epoch, admission, ordinal), alias))
    return epoch, admission, ordinal


def request_identity(row):
    epoch, admission, ordinal = physical_coordinates(row)
    digest = attr(row, "compile_request")
    if epoch is None or admission is None or ordinal is None or digest is None:
        return None
    return str(epoch), admission, ordinal, str(digest)


def admission_identity(row):
    epoch, admission, _ = physical_coordinates(row)
    if epoch is None or admission is None:
        return None
    return str(epoch), admission


def activation_origin_key(value, *, issued=False):
    request = value.get("request" if issued else "source_request")
    child = value.get("activation_actor" if issued else "actor")
    parent = value.get("parent_actor")
    execution = value.get("parent_execution")
    attempt = value.get("parent_attempt")
    if (type(request) is not int or request < 0
            or not all(isinstance(item, str) and item.strip()
                       for item in (child, parent, execution, attempt))):
        return None
    return request, child, parent, execution, attempt


def activation_origins(host_rows):
    """Index registry-issued receipts and their exact parent dispatch witnesses."""
    issued = {}
    dispatch_actors = {}
    for row in host_rows:
        value = fields(row)
        if value.get("message") == "workbench cell dispatched to its actor":
            if isinstance(value.get("execution"), str):
                dispatch_actors.setdefault(value["execution"], []).append(value.get("actor"))
        if (row.get("target") == "exomonad_actor::request"
                and value.get("message") == "request activation origin issued"):
            key = activation_origin_key(value, issued=True)
            parent_cells = {(span.get("actor"), span.get("execution"))
                            for span in span_fields(row) if span.get("name") == "cell"}
            if type(value.get("request")) is int:
                issued.setdefault(value["request"], []).append((key, parent_cells))
    return issued, dispatch_actors


def joined_activation_origin(row, issued, dispatch_actors):
    spans = [span for span in span_fields(row) if span.get("name") == "activation_input_prepare"]
    if not spans:
        return None
    keys = {activation_origin_key(span) for span in spans}
    if len(keys) != 1 or None in keys:
        return {"status": "missing_or_ambiguous_receipt"}
    key = next(iter(keys))
    request, child, parent, execution, attempt = key
    claims = issued.get(request, [])
    result = {"source_request": request, "actor": child, "parent_actor": parent,
              "parent_execution": execution, "parent_attempt": attempt}
    if (len(claims) != 1 or claims[0][0] != key
            or claims[0][1] != {(parent, execution)}
            or dispatch_actors.get(execution) != [parent]):
        return {**result, "status": "unmatched_or_ambiguous_issuance"}
    return {**result, "status": "verified"}


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

    issued_origins, dispatch_actors = activation_origins(host_rows)
    submissions = []
    for event_index, row in enumerate(host_rows):
        if (row.get("target") != "tidepool_extract_cmd::endpoint"
                or fields(row).get("message") != "compiler request identified"):
            continue
        identity = request_identity(row)
        executions = {str(span.get("execution")) for span in span_fields(row)
                      if span.get("name") == "cell" and span.get("execution") is not None}
        activation_origin = joined_activation_origin(row, issued_origins, dispatch_actors)
        if activation_origin is not None and activation_origin["status"] == "verified":
            executions.add(activation_origin["parent_execution"])
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
                            "phase_owners": sorted(owners), "startup_owner_spans": startup_owners,
                            "activation_input_origin": activation_origin})

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
    # The daemon issues an ordinal once. A digest describes that invocation's
    # input and cannot turn the same issued invocation into a second identity.
    payloads = {}
    for event in submissions + services:
        identity = event["identity"]
        if identity is None:
            continue
        physical, digest = identity[:3], identity[3]
        if physical in payloads and payloads[physical] != digest:
            raise ValueError("physical compiler request rebound to a different compile_request digest")
        payloads[physical] = digest
    return dispatches, calls, submissions, services, queues, unidentified_queue_events


def trace_capture_observation(environment, execution, host_rows, daemon_rows, host_path, daemon_path):
    """Applied evidence comes from successful writer installation, not a requested label."""
    requested = execution.get("trace_profile")
    phase_requested = environment.get("campaign_trace_profile")
    receipt = environment.get("campaign_trace_capture")
    problems = []
    if requested not in ("full", "minimal") or phase_requested != requested:
        problems.append("missing_or_mismatched_profile_selection")
    expected_spans = "none" if requested == "minimal" else "new_close"
    if not isinstance(receipt, dict):
        receipt = {}
    expected = {"profile": requested, "span_events": expected_spans,
                "filter_contract": 1, "writer": str(host_path)}
    pid = receipt.get("process_id")
    if (type(receipt.get("schema")) is not int or receipt.get("schema") != 1
            or type(receipt.get("filter_contract")) is not int
            or receipt.get("status") != "installed"
            or type(pid) is not int or pid <= 0
            or any(receipt.get(key) != value for key, value in expected.items())):
        problems.append("host_capture_unverified")
    host_receipts = [fields(row) for row in host_rows
                     if row.get("target") == "tidepool::actor_host::startup"
                     and fields(row).get("message") == "campaign trace profile applied"]
    if (len(host_receipts) != 1 or type(host_receipts[0].get("filter_contract")) is not int
            or host_receipts[0].get("process_id") != pid
            or any(host_receipts[0].get(key) != value for key, value in expected.items())):
        problems.append("host_applied_profile_missing_or_mismatched")
    compiler_receipts, boots = [], []
    for row in daemon_rows:
        value = fields(row)
        if (row.get("target") == "tidepool_extract_cmd::daemon"
                and value.get("message") == "compiler trace profile applied"):
            compiler_receipts.append(value)
        if value.get("message") == "compiler daemon ready":
            boots.append(value)
    compiler_expected = {"profile": requested, "raw_detail": "off" if requested == "minimal" else "debug",
                         "span_events": "close", "filter_contract": 1, "writer": str(daemon_path)}
    if (len(compiler_receipts) != 1 or len(boots) != 1
            or type(compiler_receipts[0].get("filter_contract")) is not int
            or type(compiler_receipts[0].get("process_id")) is not int
            or compiler_receipts[0].get("process_id", 0) <= 0
            or type(boots[0].get("daemon_pid")) is not int
            or compiler_receipts[0].get("process_id") != boots[0].get("daemon_pid")
            or any(compiler_receipts[0].get(key) != value for key, value in compiler_expected.items())):
        problems.append("compiler_applied_profile_missing_or_mismatched")
    return {"status": "complete" if not problems else "unverified_or_partial",
            "requested_profile": requested, "phase_requested_profile": phase_requested,
            "host_installation": receipt or None, "host_applied_events": host_receipts,
            "compiler_applied_events": compiler_receipts, "problems": problems}


def native_call_requirement(cohort, name):
    """The named workload owns applicability; observed rows cannot remove obligations."""
    if name in ("environment", "activation", "measurement-summary"):
        return 0, "lifecycle"
    if cohort in ("harness-eight-phase", "notebook-a-a-b-a", "persona-workbench") and name in KNOWN_ROSTERS[cohort]:
        return 1, None
    if cohort in ("three-actor-capture", "two-children-concurrent"):
        if name == "async-yield":
            return 0, "engine_intrinsic"
        if name == "host-cleanup":
            return 0, "lifecycle"
        if name == "children-concurrent-replies":
            return 2, None
        if name in KNOWN_ROSTERS[cohort]:
            return 1, None
    if cohort == "prepare-once-repeated-launch" and name in KNOWN_ROSTERS[cohort]:
        return 0, "workspace_preparation" if name == "fresh-preparation" else "lifecycle"
    return None, None


def trace_timestamp_ns(value):
    """Host UTC observations; never reinterpret them as actor monotonic time."""
    if not isinstance(value, str):
        raise ValueError("missing host timestamp")
    moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if moment.tzinfo is None:
        raise ValueError("host timestamp lacks timezone")
    delta = moment - datetime(1970, 1, 1, tzinfo=timezone.utc)
    return (delta.days * 86400 + delta.seconds) * 1_000_000_000 + delta.microseconds * 1000


def span_duration_ns(value):
    match = re.fullmatch(r"([0-9]+(?:\.[0-9]+)?)(ns|µs|us|ms|s)", str(value))
    if match is None:
        return None
    scale = {"ns": 1, "µs": 1000, "us": 1000, "ms": 1_000_000, "s": 1_000_000_000}
    return int(Decimal(match[1]) * scale[match[2]])


def workbench_phase_partition(host_rows, execution):
    """Partition sequential phase boundaries; retain nested owner spans separately.

    UTC timestamps support old trace replay. They can expose clock reversal or
    overlap, but do not share the call summary's process-monotonic origin.
    Missing/incomplete phases stay unknown instead of becoming zero work.
    """
    boundaries = {
        "private_begin_started": ("private_begin_completed", "private_begin"),
        "cell_prepare_started": ("cell_prepare_completed", "cell_prepare"),
        "checked_item_admit_started": ("checked_item_admitted", "item_admission"),
        "native_item_snapshot_started": ("native_item_snapshot_completed", "item_snapshot"),
        "native_item_snapshot_completed": ("native_item_jit_completed", "item_jit"),
        "native_item_run_started": ("native_item_run_completed", "item_run"),
        "private_publish_started": ("private_publish_completed", "private_publish"),
    }
    endings = {end: (start, name) for start, (end, name) in boundaries.items()}
    pending, intervals, owners, problems = {}, [], [], []
    previous_timestamp = None
    native_attempts = {}
    native_sequence = ("native_item_snapshot_started", "native_item_snapshot_completed",
                       "native_item_jit_completed", "native_item_run_started",
                       "native_item_run_completed")
    for index, row in enumerate(host_rows):
        if attr(row, "execution") != execution:
            continue
        value = fields(row)
        if (row.get("target") == "exomonad_harness::timing"
                and value.get("message") == "close"):
            span = row.get("span", {})
            busy = span_duration_ns(value.get("time.busy"))
            idle = span_duration_ns(value.get("time.idle"))
            owners.append({"event_index": index, "name": span.get("name"),
                           "inclusive": span.get("inclusive") is True,
                           "busy_ns": busy, "idle_ns": idle,
                           "wall_ns": busy + idle if busy is not None and idle is not None else None,
                           "ancestors": [part.get("name") for part in row.get("spans", [])]})
        if (row.get("target") != "exomonad_actor::workbench_phase"
                or value.get("message") != "workbench phase"):
            continue
        phase = value.get("phase")
        if phase not in boundaries and phase not in endings:
            continue
        try:
            timestamp = trace_timestamp_ns(row.get("timestamp"))
        except ValueError:
            problems.append({"event_index": index, "reason": "invalid_timestamp"})
            continue
        if previous_timestamp is not None and timestamp < previous_timestamp:
            problems.append({"event_index": index, "phase": phase,
                             "reason": "clock_reversed_in_event_order"})
        previous_timestamp = timestamp
        item, attempt = attr(row, "item"), attr(row, "install_attempt")
        if phase in native_sequence:
            if type(item) is not int or type(attempt) is not int or attempt < 1:
                problems.append({"event_index": index, "phase": phase,
                                 "reason": "missing_native_item_attempt_identity"})
                item, attempt = None, None
            native_attempts.setdefault((item, attempt), []).append(phase)
        identity = (item, attempt) if phase.startswith("native_item_") else (None, None)
        if phase in endings:
            start, name = endings[phase]
            opened = pending.pop((start, *identity), None)
            if opened is None or timestamp < opened[0]:
                problems.append({"event_index": index, "phase": phase,
                                 "reason": "missing_or_reversed_start"})
            else:
                intervals.append({"name": name, "start_ns": opened[0], "end_ns": timestamp,
                                  "wall_ns": timestamp - opened[0],
                                  **({"item": item, "install_attempt": attempt}
                                     if phase.startswith("native_item_") else {}),
                                  "start_event_index": opened[1], "end_event_index": index})
        if phase in boundaries:
            key = (phase, *identity)
            if key in pending:
                problems.append({"event_index": index, "phase": phase, "reason": "duplicate_start"})
            pending[key] = timestamp, index
    for phase, item, attempt in pending:
        problems.append({"phase": phase, "item": item, "install_attempt": attempt,
                         "reason": "missing_end"})
    # Declaration admissions need no native execution. Every observed native
    # attempt, including a stale-import retry, owns this exact sequence.
    for (item, attempt), observed in native_attempts.items():
        if tuple(observed) != native_sequence:
            problems.append({"item": item, "install_attempt": attempt,
                             "reason": "incomplete_or_reordered_native_attempt",
                             "observed_phases": observed})
    ordered = sorted(intervals, key=lambda interval: interval["start_ns"])
    for previous, current in zip(ordered, ordered[1:]):
        if current["start_ns"] < previous["end_ns"]:
            problems.append({"event_index": current["start_event_index"], "reason": "overlapping_phases"})
    counts = Counter(interval["name"] for interval in intervals)
    for required in ("private_begin", "cell_prepare", "private_publish"):
        if intervals and counts[required] != 1:
            problems.append({"phase": required, "reason": "missing_or_duplicate_owner_phase"})
    for required in ("item_admission", "item_snapshot", "item_jit", "item_run"):
        if intervals and not counts[required]:
            problems.append({"phase": required, "reason": "item_phase_not_observed"})
    compiler_siblings = {}
    for group, names in (
        ("pre_compiler", ("cell_program.setup", "cell_program.bind", "cell_program.offer")),
        ("post_compiler", ("cell_program.admit", "cell_program.declaration_projections", "cell_program.seal")),
    ):
        selected = [owner for owner in owners if owner["name"] in names]
        counts_by_name = Counter(owner["name"] for owner in selected)
        complete = all(counts_by_name[name] == 1 for name in names) and all(
            owner["wall_ns"] is not None for owner in selected)
        compiler_siblings[group] = {
            "status": "complete" if complete else "partial_or_unknown",
            "wall_ns": sum(owner["wall_ns"] for owner in selected) if complete else None,
            "owners": selected,
        }
    return {"status": "not_collected" if not intervals and not problems else
                      "partial_or_unknown" if problems else "complete",
            "clock_domain": "host_trace_utc", "problems": problems,
            "intervals": ordered,
            "totals_ns": {name: sum(interval["wall_ns"] for interval in intervals if interval["name"] == name)
                          for name in counts},
            "counts": dict(counts), "nested_owner_spans": owners,
            "compiler_pre_post_sibling_spans": compiler_siblings,
            "interpretation": "Sequential boundary intervals only; nested spans and aggregate compile/checkout counters overlap and must not be added. Pre/post compiler groups identify sibling preparation owners; declaration_projections can issue further physical compiler work."}


def execution_observations(host_rows, phases, cohort, host_process_id):
    """Join a call's eventual terminal across the whole stream, including parked calls."""
    dispatches = {}
    terminals = {}
    for row in host_rows:
        value = fields(row)
        if (row.get("target") == "exomonad_actor::resident_tools"
                and value.get("message") == "workbench cell dispatched to its actor"):
            dispatches.setdefault(value.get("context_call_id"), []).append(value)
        if row.get("target") == "exomonad_actor::call_timing" and value.get("message") == "call timing":
            terminals.setdefault(value.get("execution"), []).append(value)
    observations = []
    seen_executions = set()
    for phase in phases:
        count, reason = native_call_requirement(cohort, phase["phase"])
        if count == 0:
            observations.append({"phase": phase["phase"], "status": "not_applicable", "reason": reason})
            continue
        problems, calls = [], []
        call_ids = phase_call_ids(phase)
        expected_actors = {call: phase.get("actor_id") for call in call_ids}
        if count == 2:
            replies = (phase.get("evidence") or {}).get("replies", [])
            expected_actors = {reply.get("call_id"): reply.get("actor") for reply in replies
                               if isinstance(reply, dict)} if isinstance(replies, list) else {}
        if (count is None or len(call_ids) != count or set(expected_actors) != set(call_ids)
                or any(not isinstance(actor, str) or re.fullmatch(r"[0-9]+@[0-9]+", actor) is None
                       for actor in expected_actors.values())
                or count == 2 and len(set(expected_actors.values())) != 2):
            problems.append("missing_or_malformed_native_call_obligation")
        for call_id in call_ids:
            dispatched = dispatches.get(call_id, [])
            call_problems = []
            execution = dispatched[0].get("execution") if len(dispatched) == 1 else None
            expected_actor = expected_actors.get(call_id)
            if (len(dispatched) != 1 or not isinstance(execution, str) or not execution
                    or dispatched[0].get("actor") != expected_actor):
                call_problems.append("missing_ambiguous_or_mismatched_dispatch")
            if execution is not None:
                if execution in seen_executions:
                    call_problems.append("execution_reused_by_distinct_calls")
                seen_executions.add(execution)
            rows = terminals.get(execution, []) if execution else []
            terminal = rows[0] if len(rows) == 1 else {}
            actor = nonnegative_integer(terminal.get("actor"))
            incarnation = nonnegative_integer(terminal.get("incarnation"))
            if len(rows) != 1 or actor is None or incarnation is None or f"{actor}@{incarnation}" != expected_actor:
                call_problems.append("missing_ambiguous_or_mismatched_terminal")
            counters = ("publication_attempts", "semantic_rebases", "metadata_restages")
            timing_fields = ("start_ns", "end_ns", "wall_ns", "total_ms", "checkout_wait_ms",
                             "checkout_hold_ms", "compile_ms", "compile_count", "jev_ms", "jev_count", "exec_ms")
            if (any(nonnegative_integer(terminal.get(key)) is None for key in (*counters, *timing_fields))
                    or terminal.get("clock_domain") != "actor_process_monotonic"
                    or type(terminal.get("process_id")) is not int or terminal.get("process_id", 0) <= 0
                    or terminal.get("process_id") != host_process_id
                    or nonnegative_integer(terminal.get("end_ns")) is not None
                       and nonnegative_integer(terminal.get("start_ns")) is not None
                       and int(terminal["end_ns"]) < int(terminal["start_ns"])):
                call_problems.append("incomplete_or_invalid_terminal_fields")
            if terminal.get("outcome") not in ("Committed", "Replied"):
                call_problems.append("unsuccessful_or_unknown_terminal_outcome")
            calls.append({"call_id": call_id, "actor": expected_actor, "execution": execution,
                          "terminal": terminal or None, "problems": call_problems,
                          "status": "complete" if not call_problems else "partial_or_unknown"})
            problems.extend(call_problems)
        observations.append({"phase": phase["phase"], "expected_native_calls": count,
                             "status": "complete" if not problems else "partial_or_unknown",
                             "problems": problems, "calls": calls})
    return {"status": "complete" if all(row["status"] in ("complete", "not_applicable")
                                      for row in observations) else "partial_or_unknown",
            "scope": "exact dispatch and eventual terminal across the whole retained stream; not phase-wall attribution",
            "phases": observations}


def runtime_cost_events(host_rows, phases):
    phase_by_call = {}
    for phase in phases:
        for call_id in phase_call_ids(phase):
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



def interval_union_ns(rows):
    """Union only already joined intervals from one explicit clock domain."""
    intervals = sorted((row["start_ns"], row["end_ns"]) for row in rows)
    total, right = 0, None
    for left, end in intervals:
        total += end - left if right is None or left >= right else max(0, end - right)
        right = end if right is None else max(right, end)
    return total


def publication_work_observation(call_records):
    """A unique cell settlement owns these counts, not each retry event."""
    names = ("publication_attempts", "semantic_rebases", "metadata_restages")
    if not call_records:
        return {"status": "not_observed_unknown"}
    if len(call_records) != 1:
        return {"status": "ambiguous_settlement"}
    row = call_records[0]
    counts = {name: nonnegative_integer(row.get(name)) for name in names}
    if any(value is None for value in counts.values()):
        return {"status": "partial_or_unknown", "counts": counts}
    return {"status": "observed", "counts": counts,
            "latest_join_reservation": nonnegative_integer(row.get("latest_join_reservation"))}


def bound_native_demand_observations(host_rows):
    """Inventory totals can overlap between consumers; never sum or difference them."""
    local_names = ("issued_native_groups", "reused_native_groups")
    inventory_names = ("canonical_hits_total", "canonical_comparisons_total",
                       "canonical_bytes_offered_total", "issued_scope_nodes_total",
                       "graph_visits_total", "entry_handle_copies_total")
    observations = []
    for index, row in enumerate(host_rows):
        value = fields(row)
        if (row.get("target") != "exomonad_harness::timing"
                or value.get("phase") != "products.bound_native_demand"
                or value.get("message") != "bound native demand inventory observation"):
            continue
        local = {name: nonnegative_integer(value.get(name)) for name in local_names}
        inventory = {name: nonnegative_integer(value.get(name)) for name in inventory_names}
        observations.append({
            "event_index": index, "execution": attr(row, "execution"),
            "status": "observed" if all(item is not None for item in (*local.values(), *inventory.values()))
                      else "partial_or_unknown",
            "local_group_work": local, "inventory_totals": inventory,
            "span": row.get("span"), "spans": row.get("spans"),
        })
    return {
        "status": ("not_observed_unknown" if not observations else
                   "observed" if all(row["status"] == "observed" for row in observations)
                   else "partial_or_unknown"),
        "observations": observations,
        "interpretation": "Group work belongs to the demand operation. Inventory totals may include concurrent "
                          "consumers and are not operation deltas. Offered bytes are not measured I/O or memory traffic.",
    }


def validation_summary(value):
    """Admit the timing and optional counters issued by Tidepool.Timing."""
    required = ("invocation_id", "worker_pid", "start_ns", "end_ns", "wall_ns", "cpu_ns")
    if (not isinstance(value, dict) or value.get("schema") != 1
            or any(type(value.get(name)) is not int or value[name] < 0 for name in required)
            or value["end_ns"] < value["start_ns"]
            or value["wall_ns"] != value["end_ns"] - value["start_ns"]
            or value.get("outcome") not in ("success", "refused", "exception")
            or value.get("clock_domain") != "ghc_monotonic_ns"
            or not isinstance(value.get("counts"), dict)
            or any(type(count) is not int or count < 0 for count in value["counts"].values())):
        raise ValueError("invalid validation summary")
    rts_fields = ("allocated_bytes", "gc_cpu_ns", "gc_elapsed_ns")
    supplied = [name for name in rts_fields if name in value]
    scope = value.get("rts_scope")
    if (scope not in (None, "unavailable", "process_delta")
            or (supplied and scope != "process_delta")
            or any(type(value[name]) is not int or value[name] < 0 for name in supplied)):
        raise ValueError("invalid validation RTS counters or scope")
    return value


def owned_artifact_observations(host_rows, daemon_rows):
    """Retain owner facts; absent summaries never prove zero work or retention."""
    native_targets = {"tidepool_codegen::image_registry", "tidepool_codegen::prepared_compile",
                      "tidepool_codegen::image_install", "tidepool_codegen::image_lifetime",
                      "tidepool_runtime::activation_renderer_install"}
    native, validations, frontend, problems = [], [], [], []
    physical_terminals = {}
    for row in host_rows:
        if row.get("target") not in native_targets:
            continue
        value = dict(fields(row))
        for name in ("image_registry", "image_entry", "image_compile", "renderer_owner"):
            if name not in value and attr(row, name) is not None:
                value[name] = attr(row, name)
        native.append({"target": row["target"], "fields": value,
                       "execution": attr(row, "execution"), "span": row.get("span"),
                       "spans": row.get("spans")})
    for row in daemon_rows:
        physical = request_identity(row)
        if fields(row).get("message") in ("compiler request finished", "compiler request failed",
                                          "compiler request abandoned by client") and physical is not None:
            physical_terminals.setdefault(physical, []).append(fields(row))
        line = fields(row).get("line")
        if not isinstance(line, str):
            continue
        identity = request_identity(row)
        if line.startswith("tidepool-compile-summary "):
            frontend.append({"physical_request": list(identity) if identity else None,
                             "counts": {name: int(value) for name, value in re.findall(
                                 r"(?:^| )(modules|typecheck_modules|lowering_modules|interface_modules)=([0-9]+)(?= |$)", line)}})
        if not line.startswith("tidepool-validation "):
            continue
        try:
            value = validation_summary(json.loads(line[len("tidepool-validation "):]))
            if identity is None or (attr(row, "worker_pid") is not None
                                    and integer(attr(row, "worker_pid")) != value["worker_pid"]):
                raise ValueError("validation lacks matching physical request/worker identity")
            validations.append({"physical_request": list(identity), **value})
        except (ValueError, TypeError) as error:
            problems.append(str(error))
    seen = set()
    for event in validations:
        terminals = physical_terminals.get(tuple(event["physical_request"]), [])
        event["physical_terminal_status"] = "observed" if len(terminals) == 1 else "missing_or_ambiguous"
        if len(terminals) != 1:
            problems.append("validation lacks unique physical service terminal")
        key = (event["worker_pid"], event["invocation_id"])
        if key in seen:
            problems.append("duplicate validation invocation")
        seen.add(key)
    compile_events = [event for event in native if event["target"] == "tidepool_codegen::prepared_compile"
                      and event["fields"].get("message") == "prepared compile"]
    install_events = [event for event in native if event["target"] == "tidepool_codegen::image_install"
                      and event["fields"].get("outcome") == "machine_published"]
    producer_events = [event for event in native if event["target"] == "tidepool_codegen::image_registry"
                       and event["fields"].get("message") == "native image producer settled"]
    decisions = [event for event in native if event["fields"].get("message") == "native image registry decision"]
    images = set()
    for event in compile_events:
        key = (event["fields"].get("process_id"), event["fields"].get("image_instance"))
        if key in images:
            problems.append("duplicate native compile image identity")
        images.add(key)
    for event in compile_events + install_events:
        if any(nonnegative_integer(event["fields"].get(name)) is None
               for name in ("process_id", "image_instance")):
            problems.append("native compile/install lacks process-local image identity")
    intervals = {}
    for event in validations:
        key = (tuple(event["physical_request"]), event["worker_pid"], event["clock_domain"])
        intervals.setdefault(key, []).append(event)
    validation_groups = []
    for (physical, pid, clock), rows in intervals.items():
        nonoverlapping = sum(row["end_ns"] - row["start_ns"] for row in rows) == interval_union_ns(rows)
        validation_groups.append({"physical_request": list(physical), "worker_pid": pid,
            "clock_domain": clock, "invocation_count": len(rows),
            "wall_interval_union_ns": interval_union_ns(rows), "intervals_nonoverlapping": nonoverlapping,
            "process_allocated_bytes_sum": (sum(row["allocated_bytes"] for row in rows)
                if nonoverlapping and all(type(row.get("allocated_bytes")) is int for row in rows) else None)})
    key_groups = {}
    released = {(event["fields"].get("process_id"), event["fields"].get("image_instance"))
                for event in native if event["target"] == "tidepool_codegen::image_lifetime"
                and event["fields"].get("outcome") == "released"}
    for event in decisions + producer_events:
        value = event["fields"]
        key = tuple(value.get(name) for name in ("process_id", "image_registry", "image_entry"))
        if any(nonnegative_integer(item) is None for item in key):
            problems.append("registry event lacks exact retained entry identity")
            continue
        group = key_groups.setdefault(key, {"process_id": key[0], "image_registry": key[1],
                    "image_entry": key[2], "image_instances": set(), "producer_ids": set(),
                    "dispositions": Counter(), "producer_outcomes": Counter(), "live_hit_wall_ns": [],
                    "events": []})
        group["events"].append(value)
        if nonnegative_integer(value.get("image_instance")) is not None:
            group["image_instances"].add(value["image_instance"])
        if value.get("message") == "native image producer settled":
            if nonnegative_integer(value.get("image_compile")) is not None:
                group["producer_ids"].add(value["image_compile"])
            group["producer_outcomes"][value.get("outcome")] += 1
        else:
            group["dispositions"][value.get("disposition")] += 1
            if value.get("disposition") == "live_hit" and nonnegative_integer(value.get("wall_ns")) is not None:
                group["live_hit_wall_ns"].append(value["wall_ns"])
    key_output = []
    for group in key_groups.values():
        group["released_image_instances_observed"] = sorted(instance for instance in group["image_instances"]
            if (group["process_id"], instance) in released)
        group["image_instances"] = sorted(group["image_instances"])
        group["producer_ids"] = sorted(group["producer_ids"])
        group["dispositions"] = dict(group["dispositions"])
        group["producer_outcomes"] = dict(group["producer_outcomes"])
        key_output.append(group)
    def observed_count(rows):
        return len(rows) if rows else None
    return {
        "status": "partial_or_unknown" if problems or not compile_events or not install_events or not validations else "observed",
        "problems": problems,
        "counts_scope": "observed_owner_records; no missing-event zeros or exhaustive-stream claim",
        "ghc_frontend_summaries": frontend,
        "bound_native_demand": bound_native_demand_observations(host_rows),
        "ghc_physical_request_count_observed": len(physical_terminals) or None,
        "actual_native_image_compiles_observed": (observed_count(compile_events)
            if not any("native compile" in problem for problem in problems) else None),
        "machine_installations_observed": observed_count(install_events),
        "native_producer_outcomes_observed": dict(Counter(event["fields"].get("outcome") for event in producer_events)) or None,
        "registry_dispositions_observed": dict(Counter(event["fields"].get("disposition") for event in decisions)) or None,
        "registry_wait_ns_observed": (sum(nonnegative_integer(event["fields"].get("shared_wait_ns"))
                                         for event in decisions)
            if decisions and all(nonnegative_integer(event["fields"].get("shared_wait_ns")) is not None
                                 for event in decisions) else None),
        "successful_native_functions_defined_observed": (sum(nonnegative_integer(event["fields"].get("functions_defined")) or 0
            for event in compile_events) if compile_events and all(nonnegative_integer(event["fields"].get("functions_defined")) is not None
            for event in compile_events) else None),
        "successful_native_code_bytes_observed": (sum(nonnegative_integer(event["fields"].get("code_bytes")) or 0
            for event in compile_events) if compile_events and all(nonnegative_integer(event["fields"].get("code_bytes")) is not None
            for event in compile_events) else None),
        "registry_key_lifetimes": key_output,
        "native_events": native, "validation_events": validations, "validation_intervals": validation_groups,
        "interpretation": "GHC specialization, native production and machine publication are distinct. "
                          "Weak image expiry permits later production. Cache HIT duration includes lookup/lock cost. "
                          "Allocation is a process RTS delta, not retained heap; overlapping deltas are not summed.",
    }


def case_wall_ledger(phases, roster):
    summaries = [phase for phase in phases if phase["phase"] == "measurement-summary"]
    activation = [phase for phase in phases if phase["phase"] == "activation"]
    if not summaries:
        return {"status": "not_collected", "scope": "no whole-case monotonic summary"}
    if len(summaries) != 1 or len(activation) != 1:
        return {"status": "partial_or_unknown", "problems": ["non-single summary or activation"]}
    summary, active = summaries[0], activation[0]
    total = summary.get("case_wall_ns")
    intervals = [("activation", active.get("start_ns"), active.get("end_ns"), active.get("wall_ns")),
                 ("installation_verification", active.get("installation_verification_start_ns"),
                  active.get("installation_verification_end_ns"), active.get("installation_verification_ns"))]
    intervals += [(phase["phase"], phase.get("start_ns"), phase.get("end_ns"), phase.get("wall_ns"))
                  for phase in phases if phase["phase"] in roster]
    intervals.append(("shutdown", summary.get("start_ns"), summary.get("end_ns"), summary.get("wall_ns")))
    problems = []
    previous_end = 0
    for name, start, end, duration in intervals:
        if (any(type(value) is not int or value < 0 for value in (start, end, duration))
                or end < start or end - start != duration):
            problems.append(f"invalid monotonic interval: {name}")
        elif start < previous_end:
            problems.append(f"overlapping monotonic interval: {name}")
        else:
            previous_end = end
    if type(total) is not int or total < previous_end:
        problems.append("invalid whole-case wall")
    if problems:
        return {"status": "partial_or_unknown", "problems": problems}
    covered = sum(interval[3] for interval in intervals)
    return {"status": "complete", "case_wall_ns": total,
            "measured_disjoint_ns": covered, "unpartitioned_ns": total - covered,
            "intervals": [{"name": name, "start_ns": start, "end_ns": end, "wall_ns": duration}
                          for name, start, end, duration in intervals],
            "scope": "test body through acknowledged host shutdown; verification is separate from startup; "
                     "unpartitioned includes setup, assertions and recording, not an inferred cause; "
                     "runner setup/teardown and nested compiler spans are not added"}


def catalog_acceptance_observations(host_rows):
    names = ("deployment_accepted_modules", "deployment_original_product_bytes",
             "receipt_fresh_native_modules", "receipt_cached_native_modules",
             "receipt_retained_core_native_modules")
    events = []
    for index, row in enumerate(host_rows):
        if (row.get("target") != "tidepool_toolchain::module_candidates::acceptance"
                or fields(row).get("message") != "deployment module products accepted"):
            continue
        values = {name: nonnegative_integer(fields(row).get(name)) for name in names}
        events.append({"event_index": index, "request_identity": request_identity(row),
                       "execution": attr(row, "execution"), "workload_phase": attr(row, "workload_phase"),
                       "startup_owners": sorted({owner for span in span_fields(row)
                                                 if (owner := startup_owner(span)) is not None}), **values})
    complete = bool(events) and all(event[name] is not None for event in events for name in names)
    return {"status": "observed" if complete else "partial_or_unknown" if events else "not_observed_unknown",
            "events": events,
            "module_observations": {name: sum(event[name] for event in events)
                                    if complete else None for name in names},
            "scope": "successful certified native receipt dispositions with deployment candidates; "
                     "observations are not unique modules, physical reads, canonical-only rows or compiler time; "
                     "absent events do not imply zero work or a catalog miss; prepared entry loading may need no compile"}


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
    trace_profile = environment.get("campaign_trace_profile")
    if trace_profile is not None and trace_profile not in ("full", "minimal"):
        raise ValueError(f"unknown campaign trace profile: {trace_profile!r}")
    if execution.get("trace_profile") is not None and trace_profile is not None and execution["trace_profile"] != trace_profile:
        raise ValueError("runner and workload phase trace profiles do not match")
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
    owner_observations = owned_artifact_observations(host_rows, iter_jsonl(daemon_path))

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

    capture_observation = trace_capture_observation(
        environment, execution, host_rows, iter_jsonl(daemon_path), host_path, daemon_path)
    execution_observation = execution_observations(
        host_rows, phases, cohort_name, (capture_observation.get("host_installation") or {}).get("process_id"))

    assign_submission_phases(submissions, dispatches, phases, roster)
    services_by_identity = {}
    for service in services:
        if service["identity"] is not None:
            services_by_identity.setdefault(service["identity"][:3], []).append(service)

    prior_sources = set()
    prior_digests = set()
    output_phases = []
    for phase in phases:
        if phase["phase"] in ("environment", "activation", "measurement-summary"):
            continue
        phase_evidence = phase.get("evidence", {})
        if not isinstance(phase_evidence, dict):
            phase_evidence = {}
        call_id = phase.get("call_id")
        declared_calls = phase_call_ids(phase)
        execution_ids = sorted(set().union(*(dispatches.get(call, set()) for call in declared_calls)))
        exact_dispatch_group = (len(declared_calls) > 1 and len(execution_ids) == len(declared_calls)
                                and all(len(dispatches.get(call, set())) == 1 for call in declared_calls))
        phase_submissions = [request for request in submissions
                             if request["phase_attribution"] == phase["phase"]]
        identities = sorted({request["identity"] for request in phase_submissions
                              if request["identity"] is not None})
        digest_set = {identity[3] for identity in identities}
        service_rows = []
        for identity in identities:
            matched = services_by_identity.get(identity[:3], [])
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
        identity_counts = Counter(request["identity"][:3] for request in phase_submissions
                                  if request["identity"] is not None)
        duplicate_identities = sum(count > 1 for count in identity_counts.values())
        if not phase_submissions and logical == 0 and unattributed_events == 0:
            attribution = "zero_logical_requests_no_correlated_submission_observed"
        elif (logical is not None and logical == len(phase_submissions)
              and not missing_identities and (exact_dispatch_group or
                                               (len(declared_calls) == 1 and len(execution_ids) == 1) or
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
            "dispatch_attribution": "exact_group" if exact_dispatch_group else "one" if len(execution_ids) == 1 else
                                    "missing" if not execution_ids else "ambiguous",
            "call_timing_records": call_timing,
            "workbench_phase_partitions": [
                {"execution": execution, **workbench_phase_partition(host_rows, execution)}
                for execution in sorted(execution_ids)
            ],
            "publication_work": publication_work_observation(call_timing),
            "harness_runtime_cost_events": phase_costs,
            "harness_runtime_cost_coverage": cost_coverage,
            "host_admission_status": cost_coverage["host_admission_status"],
            "client_compiler_submissions": service_rows,
            "compiler_attribution": attribution,
            "source_sha256": source_digest,
            "successful_native_image_constructions": nonnegative_integer(phase.get("successful_native_image_constructions")),
            "evidence": phase_evidence,
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
        if phase["phase"] in ("environment", "measurement-summary"):
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
                type(phase["first_successor_ns"]) is not int or phase["first_successor_ns"] < 0
                or (type(phase.get("wall_ns")) is int
                    and phase["first_successor_ns"] > phase["wall_ns"])):
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
            matched = services_by_identity.get(identity[:3], []) if identity is not None else []
            unattributed_host.append({
                "identity": identity, "executions": request["executions"],
                "phase_owners": request["phase_owners"],
                "startup_owner_spans": request["startup_owner_spans"],
                "activation_input_origin": request["activation_input_origin"],
                "owner_candidates": request["owner_candidates"],
                "phase_attribution": request["phase_attribution"],
                "daemon_service_records": matched,
            })
    orphan_services = [service for service in services
                       if service["identity"] is None or service["identity"] not in all_host_identities]

    host_identity_counts = Counter(request["identity"][:3] for request in submissions
                                   if request["identity"] is not None)
    service_identity_counts = Counter(service["identity"][:3] for service in services
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
        and all(len(services_by_identity.get(request["identity"][:3], [])) == 1
                and services_by_identity[request["identity"][:3]][0]["exit_code"] == 0
                for request in explicit_activation_requests + startup_owned_requests)
    )
    startup_owner_status = "complete" if startup_owner_complete else "unknown_unowned_or_unmatched_requests"
    zero_compiler_work_verified = (
        cohort_name == "prepare-once-repeated-launch" and phase_coverage["complete"]
        and not invalid_phase_measurements and whole_stream_complete
        and not submissions and not services and not queues and unidentified_queue_events == 0
        and all(phase.get("logical_compiler_requests") == 0 for phase in phases
                if phase["phase"] != "environment")
    )
    if (zero_compiler_work_verified and not owner_observations["problems"]
            and owner_observations["machine_installations_observed"] is not None
            and owner_observations["actual_native_image_compiles_observed"] is not None):
        owner_observations["status"] = "native_only_no_compiler_requests"
        owner_observations["compiler_validation_applicability"] = "no_requests_verified_by_phase_counters_and_both_physical_streams"
    queue_complete = ((bool(queues) or zero_compiler_work_verified) and unidentified_queue_events == 0 and not queue_missing
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

    report = {
        "schema": 1,
        "whole_case_wall": case_wall_ledger(phases, roster),
        "catalog_acceptance": catalog_acceptance_observations(host_rows),
        "workspace_route": {"selected": environment.get("workspace_route"),
                            "installed_entry": (activation or {}).get("prepared_workspace_entry"),
                            "verified": environment.get("workspace_route") == "prepared-workspace"
                                        and isinstance((activation or {}).get("prepared_workspace_entry"), str)
                                        and (activation or {})["prepared_workspace_entry"].startswith("/")},
        "owned_artifact_observations": owner_observations,
        "trace_capture": capture_observation,
        "execution_observations": execution_observation,
        "trace_profile": {
            "name": trace_profile,
            "fine_joins_and_attribution": "capture unverified; see completeness fields"
                if capture_observation["status"] != "complete" else "collected; see completeness fields" if trace_profile == "full"
                else "bounded summaries and joins collected; per-file detail omitted; see completeness fields" if trace_profile == "minimal"
                else "unknown profile; see capture evidence",
        },
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
        "queue_trace_status": ("records_observed" if queues else "no_admissions_verified" if zero_compiler_work_verified else "no_records_observed_unknown"),
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
        "activation_input_origins": [
            {"identity": request["identity"], "phase_attribution": request["phase_attribution"],
             **request["activation_input_origin"]}
            for request in submissions if request["activation_input_origin"] is not None
        ],
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
                 "daemon_service_records": services_by_identity.get(request["identity"][:3], [])
                 if request["identity"] is not None else []}
                for request in startup_owned_requests
            ],
            "requests": startup_unowned,
            "ambiguous_startup_span_requests": ambiguous_startup_owner_requests,
            "ambiguous_requests": ambiguous_owner_submissions,
            "unknown_owner_requests": unknown_owner_submissions,
            "assignment_policy": "exact_declared_phase_or_issued_child_origin_or_root_startup_owner; no count or timestamp assignment",
        },
        "compiler_allowances": requested_allowances,
        "compiler_job_grants": {
            "status": "not_applicable_no_compiler_requests" if zero_compiler_work_verified else "observed" if (queue_complete and compiler_grants
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
    if trace_profile == "minimal":
        # Minimal capture omits fine Harness runtime-cost details. Exact compiler
        # and native execution joins retain their independent completeness.
        report["runner"]["diagnostic_evidence_complete"] = None
        report["runner"]["diagnostic_evidence_status"] = "not_collected_by_design"
        report["harness_runtime_cost_trace"] = {
            "status": "not_collected_by_design", "event_count": None,
            "phase_attributed_event_count": None, "unattributed_event_count": None,
            "ambiguous_event_count": None, "events": None, "coverage": None,
        }
        # Exact request/service joins and bounded owner summaries are required
        # in minimal too. Only high-volume runtime diagnostics remain omitted.
        for phase in report["phases"]:
            phase["harness_runtime_cost_events"] = None
            phase["harness_runtime_cost_coverage"] = None
            phase["store_projection_status"] = "unknown_by_trace_profile"
    return report


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
