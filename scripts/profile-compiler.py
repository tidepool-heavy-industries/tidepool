#!/usr/bin/env python3
"""Bounded, same-user CPU/RSS capture of an explicitly selected compiler worker."""

import argparse
import bisect
import collections
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import shlex
import signal
import subprocess
import time

LIMIT = 4 * 1024 * 1024
DERIVED_LIMIT = 16 * 1024 * 1024
TIMING_LIMIT = 16 * 1024 * 1024
PHASE_LIMIT = 8192
PHASE_GROUP_LIMIT = 256
HOT_LEAF_LIMIT = 8
RECOVERY_SCAN_LIMIT = 64 * 1024 * 1024
IDENTITY_FIELDS = ("compile_request", "worker_pid", "run_id", "daemon_epoch", "admission_id",
                   "request_ordinal", "served", "transaction", "transport", "daemon_pid", "worker")
NUMERIC_IDENTITY_FIELDS = {"worker_pid", "admission_id", "request_ordinal", "served", "daemon_pid", "worker"}
NUMERIC_DETAIL_FIELDS = {"ms", "start_ns", "end_ns", "wall_ns", "cpu_ns", "allocated_bytes",
                         "gc_cpu_ns", "gc_elapsed_ns", "gcs"}


def anchor():
    return {
        "monotonic_ns": time.monotonic_ns(),
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    }


def proc_identity(pid):
    # comm can contain spaces or parentheses; fields after its closing ')' start
    # with field 3. Field 22 is the process start tick, which fences PID reuse.
    stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return int(stat[19])


def digest(path):
    sha = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def bounded_copy(source, destination, offset=0, limit=LIMIT, retain_prefix=False):
    with open(source, "rb") as stream:
        stream.seek(offset)
        data = stream.read(limit + 1)
    truncated = len(data) > limit
    if truncated and not retain_prefix:
        raise ValueError(f"capture exceeds {limit} bytes: {source}")
    destination.write_bytes(data[:limit])
    return {"path": str(source), "offset": offset, "bytes": min(len(data), limit),
            "sha256": digest(destination), "truncated": truncated,
            "omitted_bytes": max(0, source.stat().st_size - offset - limit) if truncated else 0}


def terminate_child(process):
    if process is not None and process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()



def diagnostic_rows(path, worker_pid=None):
    return diagnostic_rows_from_lines(path.read_text(errors="replace").splitlines(), worker_pid)


def diagnostic_rows_from_lines(lines, worker_pid=None):
    for line in lines:
        identity = {}
        try:
            if line.lstrip().startswith("{"):
                parsed = json.loads(line)
                fields, span = parsed.get("fields", {}), parsed.get("span", {})
                identity = {key: fields.get(key, span.get(key)) for key in IDENTITY_FIELDS
                            if fields.get(key, span.get(key)) is not None}
                line = fields.get("line", "")
            elif match := re.search(r"\bcompile_request\{([^}]*)\}", line):
                # tracing's text formatter encloses the actual diagnostic in a
                # quoted `line` field. Never parse its surrounding span as detail.
                envelope = dict(token.split("=", 1) for token in shlex.split(match[1]) if "=" in token)
                identity = {key: envelope[key] for key in IDENTITY_FIELDS if key in envelope}
                payload = re.search(r'\bline=("(?:[^"\\]|\\.)*")\s*$', line)
                if not payload:
                    if "compiler timing" in line:
                        raise ValueError("invalid quoted diagnostic")
                    line = ""
                else:
                    line = json.loads(payload[1])
            if not line:
                continue
            if not isinstance(line, str):
                raise ValueError("diagnostic payload is not text")
            for key in NUMERIC_IDENTITY_FIELDS & identity.keys():
                if not re.fullmatch(r"\d+", str(identity[key])):
                    raise ValueError("invalid numeric trace identity")
                identity[key] = int(identity[key])
            if "transaction" in identity and isinstance(identity["transaction"], str):
                if identity["transaction"] not in ("true", "false"):
                    raise ValueError("invalid transaction identity")
                identity["transaction"] = identity["transaction"] == "true"
            if "transaction" in identity and not isinstance(identity["transaction"], bool):
                raise ValueError("invalid transaction identity")
            if any(not isinstance(identity[key], str) for key in identity.keys() - NUMERIC_IDENTITY_FIELDS - {"transaction"}):
                raise ValueError("invalid text trace identity")
            if worker_pid is not None and "worker_pid" in identity and identity["worker_pid"] != worker_pid:
                continue
        except (ValueError, AttributeError, TypeError):
            yield "", {"diagnostic_error": "invalid trace envelope or quoted diagnostic"}
            continue
        identity["worker_identity"] = "trace_pid" if "worker_pid" in identity else "assumed_selected_worker; log has no PID"
        yield line, identity


def detail_rows(path, worker_pid=None):
    rows, invalid = [], 0
    for line, identity in diagnostic_rows(path, worker_pid):
        if "diagnostic_error" in identity:
            invalid += 1
        if "tidepool-timing-detail " in line:
            fields = dict(re.findall(r"(\w+)=([^\s]+)", line))
            if any(not re.fullmatch(r"\d+", fields[key]) for key in NUMERIC_DETAIL_FIELDS & fields.keys()):
                invalid += 1
                continue
            if "start_ns" in fields and "end_ns" in fields:
                if int(fields["end_ns"]) < int(fields["start_ns"]) or not fields.get("parent") or not fields.get("phase"):
                    invalid += 1
                    continue
                rows.append({**fields, "trace": identity})
    return rows, invalid


def timing_rows(path, worker_pid=None):
    return detail_rows(path, worker_pid)[0]


def hash_byte_counts(path, worker_pid=None):
    grouped = {}
    for line, _ in diagnostic_rows(path, worker_pid):
        match = re.search(r"tidepool-count name=hash_bytes\.([^\s]+) count=(\d+)", line)
        if not match:
            continue
        identity, size = match.groups()
        purpose, sha = identity.rsplit(".", 1)
        item = grouped.setdefault(purpose, {"calls": 0, "total_bytes": 0, "by_digest": {}, "size_conflict": False})
        item["calls"] += 1
        item["total_bytes"] += int(size)
        prior = item["by_digest"].get(sha)
        item["size_conflict"] |= prior is not None and prior != int(size)
        item["by_digest"][sha] = int(size)
    return {purpose: {"calls": item["calls"], "total_bytes": item["total_bytes"],
                      "unique_bytes": sum(item["by_digest"].values()), "unique_contents": len(item["by_digest"]),
            "size_conflict": item["size_conflict"]} for purpose, item in grouped.items()}


def phase_hash_counts(path, worker_pid, phases):
    events, descriptions = [], {}
    for phase in phases[:PHASE_LIMIT]:
        identity = tuple(phase["trace"].get(field) for field in IDENTITY_FIELDS)
        key = identity + (phase["parent"], phase["phase"])
        descriptions[key] = {"trace": phase["trace"], "parent": phase["parent"], "phase": phase["phase"]}
        events.extend([(int(phase["start_ns"]), 1, key), (int(phase["end_ns"]) + 1, -1, key)])
    events.sort(key=lambda event: event[0])
    counters, untimestamped, invalid = [], 0, 0
    for line, trace in diagnostic_rows(path, worker_pid):
        match = re.search(r"tidepool-count name=hash_bytes\.([^\s]+) count=(\d+)", line)
        if not match:
            continue
        fields = dict(re.findall(r"(\w+)=([^\s]+)", line))
        if "count_ns" not in fields:
            untimestamped += 1
            continue
        if not re.fullmatch(r"\d+", fields["count_ns"]):
            invalid += 1
            continue
        counters.append((int(fields["count_ns"]), match[1], int(match[2]), tuple(trace.get(field) for field in IDENTITY_FIELDS)))
    active, groups, cursor, unassigned = collections.Counter(), {}, 0, 0
    omitted_keys = set()
    for stamp, digest_identity, size, identity in sorted(counters, key=lambda row: row[0]):
        while cursor < len(events) and events[cursor][0] <= stamp:
            _, change, key = events[cursor]
            active[key] += change
            if active[key] == 0:
                del active[key]
            cursor += 1
        assigned = False
        for key in active:
            if key[:-2] != identity:
                continue
            if key not in groups and len(groups) >= PHASE_GROUP_LIMIT:
                omitted_keys.add(key)
                continue
            purpose, sha = digest_identity.rsplit(".", 1)
            values = groups.setdefault(key, {}).setdefault(purpose, {"calls": 0, "total_bytes": 0, "digests": {}, "size_conflict": False})
            values["calls"] += 1
            values["total_bytes"] += size
            values["size_conflict"] |= sha in values["digests"] and values["digests"][sha] != size
            values["digests"][sha] = size
            assigned = True
        unassigned += not assigned
    rendered = [{**descriptions[key], "hash_byte_counts": {
        purpose: {"calls": values["calls"], "total_bytes": values["total_bytes"],
                  "unique_bytes": sum(values["digests"].values()), "unique_contents": len(values["digests"]),
                  "size_conflict": values["size_conflict"]} for purpose, values in group.items()},
        "scope": "count emission within interval union; event position, not hash duration; nested groups overlap"}
        for key, group in groups.items()]
    return rendered, {"untimestamped_hash_count_rows": untimestamped, "invalid_hash_timestamp_rows": invalid,
                      "unassigned_timestamped_hash_count_rows": unassigned, "omitted_hash_phase_groups": len(omitted_keys)}


def parse_samples(lines):
    samples = []
    lost = 0
    loss_known = True
    awaiting_leaf = False
    for line in lines:
        if "LOST" in line:
            match = re.search(r"\blost\s+(\d+)|\bLOST\s+(\d+)", line)
            if match:
                lost += int(next(value for value in match.groups() if value is not None))
            else:
                loss_known = False
            continue
        if awaiting_leaf and re.match(r"\s+[0-9a-fA-F]+\s", line):
            samples[-1]["line"] = line
            awaiting_leaf = False
            continue
        # perf prints PID/TID before the timestamp even when -F lists time first.
        # Continuation stack lines have no event timestamp and are excluded.
        match = re.match(r"\s*(?:\d+(?:/\d+)?\s+)?(\d+)\.(\d+):\s", line)
        if match:
            seconds, fractional = match.groups()
            samples.append({"monotonic_ns": int(seconds) * 10**9 + int(fractional.ljust(9, "0")), "line": line})
            awaiting_leaf = not line[match.end():].strip()
    return samples, lost if loss_known else None


def leaf_fields(line):
    line = re.sub(r"^\s*(?:\d+(?:/\d+)?\s+)?\d+\.\d+:\s*", "", line)
    match = re.match(r"\s*[0-9a-fA-F]+\s+(.+?)\s+\((.+)\)\s*$", line)
    if not match:
        return None
    return {"symbol": re.sub(r"\+0x[0-9a-fA-F]+$", "", match[1]), "dso": match[2]}


def phase_attribution(phases, sample_rows):
    samples = sorted(sample_rows, key=lambda row: row["monotonic_ns"])
    stamps = [row["monotonic_ns"] for row in samples]
    leaves = [leaf_fields(row["line"]) for row in samples]
    retained, grouped = [], {}
    for phase in phases[:PHASE_LIMIT]:
        start, end = int(phase["start_ns"]), int(phase["end_ns"])
        first, last = bisect.bisect_left(stamps, start), bisect.bisect_right(stamps, end)
        retained.append({**phase, "cpu_samples": last - first, "overlap": "nested phases are not additive"})
        # The content request digest alone is not an execution identity.
        trace = phase["trace"]
        key = tuple(trace.get(field) for field in IDENTITY_FIELDS) + (phase["parent"], phase["phase"])
        if key not in grouped:
            if len(grouped) >= PHASE_GROUP_LIMIT:
                continue
            grouped[key] = {"trace": trace, "parent": phase["parent"], "phase": phase["phase"], "intervals": []}
        grouped[key]["intervals"].append((first, last))
    hot_groups = []
    for group in grouped.values():
        intervals = sorted(group.pop("intervals"))
        merged = []
        for first, last in intervals:
            if merged and first <= merged[-1][1]:
                merged[-1][1] = max(merged[-1][1], last)
            else:
                merged.append([first, last])
        symbols, dsos = collections.Counter(), collections.Counter()
        unparsed, unknown, total = 0, 0, 0
        for first, last in merged:
            for leaf in leaves[first:last]:
                total += 1
                if leaf is None:
                    unparsed += 1
                    continue
                symbols[(leaf["dso"], leaf["symbol"])] += 1
                dsos[leaf["dso"]] += 1
                unknown += leaf["symbol"] == "[unknown]"
        hot_groups.append({**group, "completed_spans": len(intervals), "cpu_samples": total,
                           "unknown_symbol_samples": unknown, "unparsed_leaf_samples": unparsed,
                           "top_leaf_symbols": [{"dso": dso, "symbol": symbol, "samples": count}
                                                for (dso, symbol), count in symbols.most_common(HOT_LEAF_LIMIT)],
                           "top_dsos": [{"dso": dso, "samples": count} for dso, count in dsos.most_common(HOT_LEAF_LIMIT)],
                           "other_symbol_samples": sum(count for _, count in symbols.most_common()[HOT_LEAF_LIMIT:]),
                           "other_dso_samples": sum(count for _, count in dsos.most_common()[HOT_LEAF_LIMIT:]),
                           "overlap": "interval union within this group; nested groups are not additive"})
    all_keys = {tuple(phase["trace"].get(field) for field in IDENTITY_FIELDS) + (phase["parent"], phase["phase"])
                for phase in phases}
    return retained, hot_groups, {"omitted_spans": max(0, len(phases) - PHASE_LIMIT),
                                 "omitted_phase_groups": len(all_keys - grouped.keys()),
                                 "unparsed_leaf_samples": sum(leaf is None for leaf in leaves)}


def write_summary(path, summary, limit=DERIVED_LIMIT):
    encoded = (json.dumps(summary, indent=2) + "\n").encode()
    if len(encoded) > limit:
        summary["summary_limit_error"] = "detailed summary exceeds byte limit; retained counts and raw evidence"
        summary["omitted_summary_spans"] = len(summary.pop("phase_samples", []))
        summary["omitted_summary_phase_groups"] = len(summary.pop("phase_leaf_groups", []))
        summary["omitted_summary_hash_groups"] = len(summary.pop("phase_hash_groups", []))
        summary["phase_analysis_complete"] = False
        encoded = (json.dumps(summary, indent=2) + "\n").encode()
    if len(encoded) > limit:
        raise ValueError("summary metadata exceeds byte limit")
    path.write_bytes(encoded)


def cap_output(path):
    size = path.stat().st_size
    if size > LIMIT:
        with path.open("r+b") as stream:
            stream.truncate(LIMIT)
        return {"path": path.name, "original_bytes": size, "retained_bytes": LIMIT}
    return None


def run_bounded_output(argv, output, error_log, limit=DERIVED_LIMIT):
    written = 0
    with output.open("wb") as destination, error_log.open("wb") as errors:
        with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=errors) as process:
            while block := process.stdout.read(65536):
                remaining = limit - written
                destination.write(block[:remaining])
                written += min(len(block), remaining)
                if len(block) > remaining:
                    process.terminate()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    return process.returncode or 1
            return process.wait()


def analyze_capture(out, metadata, perf, timing_requested):
    data = out / "cpu.perf.data"
    if data.exists() and data.stat().st_size:
        script_exit = run_bounded_output([perf, "script", "--show-lost-events", "--ns", "-i", str(data),
                                          "-F", "time,pid,tid,ip,dso,sym,symoff"], out / "samples.txt", out / "script.stderr")
        report_exit = run_bounded_output([perf, "report", "--stdio", "--show-nr-samples", "--no-children", "-i", str(data)],
                                        out / "report.txt", out / "report.stderr")
        rows = (out / "samples.txt").read_text(errors="replace").splitlines()
        sample_rows, lost = parse_samples(rows)
        phases, invalid_rows = detail_rows(out / "timing.log", metadata["pid"]) if (out / "timing.log").exists() else ([], 0)
        if "sampling_enabled" in metadata and "sampling_end" in metadata:
            window_start, window_end = metadata["sampling_enabled"]["monotonic_ns"], metadata["sampling_end"]["monotonic_ns"]
            phases = [{**phase, "cpu_sampling_overlap": "full_span" if window_start <= int(phase["start_ns"]) <= int(phase["end_ns"]) <= window_end
                       else "partial_or_outside_span; resource deltas cover the complete span"} for phase in phases]
        phase_samples, phase_groups, omissions = phase_attribution(phases, sample_rows)
        hash_groups, hash_positions = phase_hash_counts(out / "timing.log", metadata["pid"], phases) if (out / "timing.log").exists() else ([], {})
        phase_complete = timing_requested and "timing_log_error" not in metadata and invalid_rows == 0 and not any(omissions.values()) and not hash_positions.get("invalid_hash_timestamp_rows") and not hash_positions.get("omitted_hash_phase_groups")
        rss_rows = [json.loads(row) for row in (out / "rss.jsonl").read_text().splitlines()]
        summary = {"sample_count": len(sample_rows), "lost_samples": lost if script_exit == 0 else None,
                   "perf_script_exit": script_exit,
                   "unknown_symbol_samples": sum("[unknown]" in row["line"] for row in sample_rows),
                   "rss_sample_count": len(rss_rows),
                   "sampled_peak_rss_bytes": max((row.get("VmRSS", 0) for row in rss_rows), default=None),
                   "phase_samples": phase_samples,
                   "phase_leaf_groups": phase_groups, "phase_omissions": omissions,
                   "phase_hash_groups": hash_groups, "hash_position_coverage": hash_positions,
                   "invalid_diagnostic_rows": invalid_rows, "phase_analysis_complete": phase_complete,
                   "observed_compile_requests": sorted({phase["trace"]["compile_request"] for phase in phases if "compile_request" in phase["trace"]}),
                   "hash_byte_counts": hash_byte_counts(out / "timing.log", metadata["pid"]) if (out / "timing.log").exists() else {},
                   "attribution_limit": "leaf instruction symbols; tail-call stacks and nearest global symbols do not identify callers",
                   "phase_coverage": metadata["phase_coverage"],
                   "workload_success": metadata["command_exit"] == 0 if metadata["command"] else None,
                   "timing_log_complete": timing_requested and "timing_log_error" not in metadata,
                   "perf_report_exit": report_exit,
                   "analysis_mode": "offline reanalysis" if "reanalysis" in metadata else "live capture",
                   "recovery_complete": metadata["reanalysis"]["recovery_complete"] if "reanalysis" in metadata else None,
                   "recovery_errors": metadata["reanalysis"]["recovery_errors"] if "reanalysis" in metadata else [],
                   "capture_complete": metadata.get("capture_error") is None and metadata["perf_recording_ok"] and script_exit == 0 and report_exit == 0 and len(sample_rows) > 0 and lost is not None}
        write_summary(out / "summary.json", summary)
        print(json.dumps({"output": str(out), "sample_count": len(sample_rows), "lost_samples": summary["lost_samples"], "capture_complete": summary["capture_complete"],
                          "workload_success": summary["workload_success"], "timing_log_complete": summary["timing_log_complete"]}))
    else:
        print(json.dumps({"output": str(out), "capture_error": metadata.get("capture_error"), "sample_count": 0}))
    return summary if data.exists() and data.stat().st_size else None


def recover_timing_window(source, destination, metadata):
    """Select explicitly supplied diagnostics by recorded monotonic interval.

    Daemon diagnostics can be flushed after their measured spans end. First
    identify invocations from embedded monotonic spans, then retain their other
    diagnostics; log timestamps alone do not define compiler CPU overlap.
    """
    before = source.stat()
    with source.open("rb") as stream:
        data = stream.read(RECOVERY_SCAN_LIMIT + 1)
    scan_truncated = len(data) > RECOVERY_SCAN_LIMIT
    data = data[:RECOVERY_SCAN_LIMIT]
    start = metadata["sampling_enabled"]["monotonic_ns"]
    end = metadata["sampling_end"]["monotonic_ns"]
    lines = data.decode(errors="replace").splitlines(keepends=True)
    selected, decoded, invalid = set(), [], 0
    for raw in lines:
        line, trace = next(diagnostic_rows_from_lines([raw.rstrip("\n")], metadata["pid"]), ("", {}))
        if "diagnostic_error" in trace:
            invalid += 1
        key = tuple(trace.get(field) for field in IDENTITY_FIELDS)
        decoded.append((raw, line, trace, key))
        if "tidepool-timing-detail " in line:
            fields = dict(re.findall(r"(\w+)=([^\s]+)", line))
            if all(re.fullmatch(r"\d+", fields.get(field, "")) for field in ("start_ns", "end_ns")):
                if int(fields["start_ns"]) <= end and int(fields["end_ns"]) >= start:
                    selected.add(key)
    retained, omitted, written, count = [], 0, 0, 0
    for raw, line, trace, key in decoded:
        if key not in selected or not line.startswith("tidepool-"):
            continue
        # Unqualified stderr has no invocation envelope; only recover the
        # individually bounded spans, rather than assigning unrelated counts.
        if "compile_request" not in trace:
            fields = dict(re.findall(r"(\w+)=([^\s]+)", line))
            if not all(re.fullmatch(r"\d+", fields.get(field, "")) for field in ("start_ns", "end_ns")):
                continue
            if int(fields["start_ns"]) > end or int(fields["end_ns"]) < start:
                continue
        encoded = raw.encode()
        if written + len(encoded) > TIMING_LIMIT:
            omitted += len(encoded)
            continue
        retained.append(encoded)
        written += len(encoded)
        count += 1
    destination.write_bytes(b"".join(retained))
    after = source.stat()
    changed = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    return {"path": str(source.resolve()), "scanned_bytes": len(data), "source_bytes": before.st_size,
            "scanned_sha256": hashlib.sha256(data).hexdigest(), "scan_limit_bytes": RECOVERY_SCAN_LIMIT,
            "scan_truncated": scan_truncated, "source_changed_during_scan": changed,
            "bytes": written, "sha256": digest(destination), "retained_lines": count,
            "omitted_matching_bytes": omitted, "invalid_source_rows": invalid,
            "selection": "invocation identity from worker spans intersecting original monotonic sampling window"}


def reanalyze(args, parser):
    if not args.timing_log:
        parser.error("--reanalyze requires an explicitly supplied --timing-log")
    os.umask(0o077)
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    out = args.output.resolve()
    original = args.reanalyze.resolve()
    bounded_copy(original / "capture.json", out / "original-capture.json")
    metadata = json.loads((out / "original-capture.json").read_text())
    if not all(key in metadata for key in ("sampling_enabled", "sampling_end")):
        parser.error("original capture has no qualified monotonic sampling window")
    metadata["reanalysis"] = {"original_capture": str(original), "begin": anchor(),
                               "original_capture_sha256": digest(out / "original-capture.json"),
                               "original_timing_log_error": metadata.pop("timing_log_error", None),
                               "scope": "offline analysis only; no workload was rerun"}
    metadata["timing_log"] = recover_timing_window(args.timing_log, out / "timing.log", metadata)
    recovery = metadata["timing_log"]
    recovery_errors = [key for key in ("scan_truncated", "source_changed_during_scan", "omitted_matching_bytes", "invalid_source_rows") if recovery[key]]
    metadata["reanalysis"]["recovery_errors"] = recovery_errors
    metadata["reanalysis"]["recovery_complete"] = not recovery_errors
    if recovery_errors:
        metadata["timing_log_error"] = "recovered timing evidence incomplete; inspect recovery bounds/errors"
    bounded_copy(original / "cpu.perf.data", out / "cpu.perf.data", limit=64 * 1024 * 1024)
    bounded_copy(original / "rss.jsonl", out / "rss.jsonl")
    perf = shutil.which(args.perf) or metadata.get("perf")
    if not perf:
        parser.error("perf executable unavailable for offline analysis")
    metadata["reanalysis"]["perf"] = str(Path(perf).resolve())
    metadata["reanalysis"]["perf_version"] = subprocess.check_output([perf, "version"], text=True).strip()
    (out / "capture.json").write_text(json.dumps(metadata, indent=2) + "\n")
    summary = analyze_capture(out, metadata, perf, True)
    return 0 if summary and summary["capture_complete"] and summary["phase_analysis_complete"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, help="already running, privately owned worker")
    parser.add_argument("--reanalyze", type=Path, help="existing capture directory; offline recovery into a new --output")
    parser.add_argument("--output", required=True, type=Path, help="new private evidence directory")
    parser.add_argument("--perf", default="perf")
    parser.add_argument("--duration", type=float, default=30, help="maximum seconds; command timeout when present")
    parser.add_argument("--frequency", type=int, default=199)
    parser.add_argument("--rss-interval", type=float, default=0.05)
    parser.add_argument("--timing-log", type=Path, help="existing worker/daemon log; captures only new bytes")
    parser.add_argument("--worker-build-identity", type=Path, help="frozen build identity JSON with worker_sha256 and source provenance")
    parser.add_argument("--request-id", action="append", default=[], help="explicit linkage to the measured requests")
    parser.add_argument("--retain-file", type=Path, action="append", default=[], help="opt-in immutable request/scope/graph file, <=4 MiB each, <=8 MiB total")
    parser.add_argument("command", nargs=argparse.REMAINDER, help="optional request command following --")
    args = parser.parse_args()
    if args.reanalyze:
        return reanalyze(args, parser)
    if args.pid is None:
        parser.error("capture requires --pid (or use --reanalyze for offline recovery)")
    if not (0 < args.duration <= 300 and 0.01 <= args.rss_interval <= 10 and 0 < args.frequency <= 1000):
        parser.error("duration must be <=300s, frequency <=1000Hz, and RSS interval between 10ms and 10s; duration and frequency positive")
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    proc = Path(f"/proc/{args.pid}")
    if proc.stat().st_uid != os.getuid():
        parser.error("worker must belong to the current Unix user")
    identity = proc_identity(args.pid)
    exe = (proc / "exe").resolve(strict=True)
    perf = shutil.which(args.perf)
    if perf is None:
        parser.error("perf executable is unavailable; pass --perf from the pinned tool closure")
    # All retained artifacts can contain caller source and request metadata.
    os.umask(0o077)
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    out = args.output.resolve()
    metadata = {
        "pid": args.pid, "start_ticks": identity, "worker": str(exe),
        "worker_sha256": digest(proc / "exe"), "request_ids": args.request_id,
        "command": command, "duration_limit_seconds": args.duration,
        "frequency_hz": args.frequency, "event": "cpu-clock:u",
        "clock": "CLOCK_MONOTONIC", "call_graph": "none; leaf samples only",
        "rss_interval_seconds": args.rss_interval, "derived_file_limit_bytes": DERIVED_LIMIT,
        "timing_log_limit_bytes": TIMING_LIMIT, "phase_limit": PHASE_LIMIT,
        "phase_group_limit": PHASE_GROUP_LIMIT, "hot_leaf_limit": HOT_LEAF_LIMIT,
        "capture_checkout_oid": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "perf": str(Path(perf).resolve()),
        "perf_version": subprocess.check_output([perf, "version"], text=True).strip(),
        "begin": anchor(), "retained_inputs": [],
        "measurement_scope": "selected process only; no inherited tasks; userspace CPU samples",
        "phase_coverage": "only completed instrumented spans; missing phases are unmeasured, not zero",
        "rts_allocation_scope": "process counter deltas; allocation counters can lag until an RTS GC accounting boundary",
    }
    if args.worker_build_identity:
        bounded_copy(args.worker_build_identity, out / "worker-build-identity.json")
        build_identity = json.loads((out / "worker-build-identity.json").read_text())
        if build_identity.get("worker_sha256") != metadata["worker_sha256"]:
            parser.error("build identity does not match the selected worker bytes")
        metadata["worker_build_identity"] = build_identity
    bounded_copy(proc / "maps", out / "worker.maps")
    retained_size = sum(path.stat().st_size for path in args.retain_file)
    if retained_size > 8 * 1024 * 1024:
        parser.error("retained input set exceeds eight MiB")
    for index, path in enumerate(args.retain_file):
        metadata["retained_inputs"].append(bounded_copy(path, out / f"input-{index}-{path.name}"))
    log_stat = args.timing_log.stat() if args.timing_log else None
    log_offset = log_stat.st_size if log_stat else None
    control_read, control_write = os.pipe()
    ack_read, ack_write = os.pipe()
    perf_args = [perf, "record", "--clockid", "mono", "-e", "cpu-clock:u", "-F", str(args.frequency),
                 "--no-inherit", "--no-buildid-cache", "--buildid-all", "--max-size", "64M",
                 "--delay=-1", "--control", f"fd:{control_read},{ack_write}",
                 "-p", str(args.pid), "-o", str(out / "cpu.perf.data")]
    metadata["perf_record_command"] = perf_args
    child = None
    recorder = None
    capture_error = None
    try:
        with open(out / "perf.stderr", "wb") as perf_log, open(out / "rss.jsonl", "w") as rss:
            recorder = subprocess.Popen(perf_args, stdout=perf_log, stderr=perf_log, pass_fds=(control_read, ack_write))
            os.close(control_read)
            os.close(ack_write)
            os.write(control_write, b"enable\n")
            ready, _, _ = select.select([ack_read], [], [], 10)
            ack = os.read(ack_read, 128) if ready else b""
            metadata["perf_enable_ack_hex"] = ack.hex()
            if ack.rstrip(b"\x00\n") != b"ack":
                raise RuntimeError("perf did not acknowledge enable; no request command was launched")
            metadata["sampling_enabled"] = anchor()
            with open(out / "command.stdout", "wb") as stdout, open(out / "command.stderr", "wb") as stderr:
                if command:
                    child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
                    metadata["command_begin"] = anchor()
                deadline = time.monotonic() + args.duration
                while time.monotonic() < deadline:
                    if proc_identity(args.pid) != identity:
                        raise RuntimeError("selected worker PID was reused")
                    status = (proc / "status").read_text()
                    memory = {key: int(value) * 1024 for key, value in re.findall(r"^(VmRSS|VmHWM|RssAnon|RssFile):\s+(\d+) kB", status, re.M)}
                    rss.write(json.dumps({**anchor(), **memory}) + "\n")
                    if recorder.poll() is not None:
                        raise RuntimeError("perf ended before the capture boundary; inspect perf.stderr and size limit")
                    if any((out / name).stat().st_size > LIMIT for name in ("command.stdout", "command.stderr")):
                        raise RuntimeError("command output exceeds four MiB")
                    if child is not None and child.poll() is not None:
                        break
                    time.sleep(args.rss_interval)
                else:
                    if child is not None:
                        raise RuntimeError("request command exceeded the capture timeout")
                metadata["command_exit"] = child.poll() if child else None
                metadata["sampling_end"] = anchor()
    except (OSError, RuntimeError) as error:
        capture_error = str(error)
    finally:
        terminate_child(child)
        if recorder is not None and recorder.poll() is None:
            metadata["perf_stop_requested"] = True
            recorder.send_signal(signal.SIGINT)
            try:
                recorder.wait(timeout=10)
            except subprocess.TimeoutExpired:
                recorder.kill()
                recorder.wait()
        for fd in (control_write, ack_read):
            os.close(fd)
        metadata["perf_exit"] = recorder.returncode if recorder else None
        metadata["perf_recording_ok"] = metadata["perf_exit"] == 0 or (metadata.get("perf_stop_requested", False) and metadata["perf_exit"] == -signal.SIGINT)
        metadata["end"] = anchor()
        metadata["command_output_truncated"] = [item for name in ("command.stdout", "command.stderr")
                                                if (out / name).exists()
                                                if (item := cap_output(out / name))]
        if metadata["command_output_truncated"]:
            capture_error = capture_error or "command output exceeded four MiB; retained bounded prefix"
        metadata["capture_error"] = capture_error
        metadata["command_exit"] = child.returncode if child else None
        if args.timing_log:
            try:
                final_log_stat = args.timing_log.stat()
                if (final_log_stat.st_dev, final_log_stat.st_ino) != (log_stat.st_dev, log_stat.st_ino) or final_log_stat.st_size < log_offset:
                    raise ValueError("timing log rotated during capture")
                metadata["timing_log"] = bounded_copy(args.timing_log, out / "timing.log", log_offset,
                                                      limit=TIMING_LIMIT, retain_prefix=True)
                if metadata["timing_log"]["truncated"]:
                    metadata["timing_log_error"] = "timing log exceeds sixteen MiB; bounded prefix retained"
            except (OSError, ValueError) as error:
                metadata["timing_log_error"] = str(error)
        (out / "capture.json").write_text(json.dumps(metadata, indent=2) + "\n")
    summary = analyze_capture(out, metadata, perf, args.timing_log is not None)
    data = out / "cpu.perf.data"
    return 1 if capture_error or not metadata["perf_recording_ok"] or metadata["command_exit"] not in (None, 0) or "timing_log_error" in metadata or not data.exists() or not data.stat().st_size or not summary["capture_complete"] or (args.timing_log and not summary["phase_analysis_complete"]) else 0


if __name__ == "__main__":
    raise SystemExit(main())
