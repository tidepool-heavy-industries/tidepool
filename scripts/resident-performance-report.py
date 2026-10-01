#!/usr/bin/env python3
"""Report retained production resident samples against actual daemon evidence.

Run after the owning production runner has exited; this tool starts no services
and changes no caches. See plans/engine-resident-performance.md for the input
contract and reproducible measurement procedure.
"""

import argparse
import json
import math
import hashlib
import re
from pathlib import Path

PREFIX = "resident-performance "
LIMITS_MS = {"warm_cell": 1000, "cold_start": 10000, "cancel_ack": 250}
MIN_COUNTS = {"warm_cell": 50, "cold_start": 5, "cancel_ack": 50}


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


def analyze(samples, events, manifest):
    problems = []
    required = (
        "source_oid", "command", "exit_code", "binary_sha256",
        "frontend_sha256", "worker_sha256", "compiler_producer", "binary_path",
    )
    for key in required:
        if key not in manifest:
            problems.append(f"manifest missing {key}")
    for key, length in (("source_oid", 40), ("binary_sha256", 64), ("frontend_sha256", 64), ("worker_sha256", 64), ("compiler_producer", 64)):
        value = manifest.get(key)
        if not isinstance(value, str) or len(value) != length or any(char not in "0123456789abcdef" for char in value):
            problems.append(f"manifest {key} is not a complete hexadecimal digest")
    try:
        host_binary = Path(manifest["binary_path"])
        if not host_binary.is_absolute() or hashlib.sha256(host_binary.read_bytes()).hexdigest() != manifest.get("binary_sha256"):
            problems.append("retained host binary differs from frozen manifest")
    except (KeyError, TypeError, OSError):
        problems.append("manifest has no retained host binary")
    command = manifest.get("command")
    if not isinstance(command, list) or not command or any(not isinstance(arg, str) for arg in command):
        problems.append("manifest command must be the exact nonempty argv array")
    if manifest.get("exit_code") != 0:
        problems.append("production runner did not exit successfully")
    ready = {}
    requests = {}
    queue = []
    phases = {}
    counts = {}
    for event in events:
        row = fields(event)
        if row.get("message") == "compiler daemon ready":
            ready[row.get("daemon_epoch")] = row
        if row.get("message") == "compiler job dequeued" and "queue_ms" in row:
            queue.append(row["queue_ms"])
        if row.get("message") == "compiler timing":
            phase = re.fullmatch(r"tidepool-timing phase=([a-zA-Z0-9_]+) ms=([0-9]+)", row.get("line", ""))
            count = re.fullmatch(r"tidepool-count name=([a-zA-Z0-9_.]+) count=([0-9]+)", row.get("line", ""))
            if phase:
                phases.setdefault(phase[1], []).append(int(phase[2]))
            if count:
                counts[count[1]] = counts.get(count[1], 0) + int(count[2])
        if row.get("message") == "compiler request finished":
            key = (row.get("daemon_epoch"), row.get("compile_request"))
            requests.setdefault(key, []).append(row)
    if not ready:
        problems.append("no actual daemon startup evidence")
    product = [row for row in samples if row.get("composition") == "engine-store"]
    if len(product) != len(samples):
        problems.append("samples include a composition other than engine-store")
    seen = set()
    operations = set()
    warm_workers = {}
    for row in product:
        if row.get("schema") != 1:
            problems.append("unsupported sample schema")
        kind = row.get("kind")
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
        if kind in ("cold_start", "cancel_ack"):
            start, end = row.get("started_ns"), row.get("settled_ns")
            if type(start) is not int or type(end) is not int or start < 0 or end < start or end - start != row.get("elapsed_ns"):
                problems.append(f"sample {key} has no exact start/settlement boundaries")
        if kind == "cold_start":
            if row.get("packaged") is not True or not row.get("host_pid") or row.get("readiness") != "workspace-ready":
                problems.append(f"cold sample {key} lacks packaged host/readiness evidence")
            if row.get("host_sha256") != manifest.get("binary_sha256") or row.get("host_executable") != manifest.get("binary_path"):
                problems.append(f"cold sample {key} used another host binary")
        if kind == "cancel_ack":
            operation = row.get("operation_id")
            if not isinstance(operation, str) or not operation or operation in operations:
                problems.append(f"cancel sample {key} has no unique original operation")
            operations.add(operation)
            active = row.get("effect_active_ns")
            if type(active) is not int or type(row.get("started_ns")) is not int or active < 0 or active > row["started_ns"] or row.get("acknowledged") is not True:
                problems.append(f"cancel sample {key} has no proved active effect and acknowledgment")
        if kind == "warm_cell":
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
                problems.append(f"warm sample {key} has no compiler request correlations")
            for correlation in correlations:
                matches = requests.get((epoch, correlation), [])
                if len(matches) != 1:
                    problems.append(f"warm sample {key} request {correlation} has {len(matches)} completions")
                    continue
                request = matches[0]
                if request.get("transport") != "daemon" or request.get("exit_code") != 0:
                    problems.append(f"warm sample {key} did not complete through daemon transport")
                if not request.get("worker_pid") or request.get("daemon_pid") != startup.get("daemon_pid"):
                    problems.append(f"warm sample {key} has no matched worker/daemon PID")
                if request.get("served", 0) < 1 or request.get("followed_rotation") is not False:
                    problems.append(f"warm sample {key} used a cold or rotated worker")
                warm_workers.setdefault((epoch, request.get("worker_pid")), set()).add(row.get("index"))
    for worker, cells in warm_workers.items():
        if len(cells) < 2:
            problems.append(f"warm worker {worker} has no measured reuse")
    for startup in ready.values():
        for path_key, digest_key in (("executable", "frontend_sha256"), ("worker", "worker_sha256")):
            try:
                path = Path(startup[path_key])
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                if not path.is_absolute() or digest != manifest.get(digest_key):
                    problems.append(f"daemon {path_key} differs from frozen manifest")
            except (KeyError, OSError):
                problems.append(f"daemon has no retained {path_key} binary")
    result = {}
    for kind in (*MIN_COUNTS, "cancel_cleanup"):
        selected = [row for row in product if row.get("kind") == kind]
        values = [row["elapsed_ns"] / 1_000_000 for row in selected if type(row.get("elapsed_ns")) is int and row["elapsed_ns"] >= 0]
        p95 = percentile(values, 0.95)
        sufficient = len(selected) >= MIN_COUNTS.get(kind, 1)
        if kind == "warm_cell" and len({row.get("workload") for row in selected if row.get("workload")}) < 10:
            sufficient = False
            problems.append("warm samples must include at least ten varied workload labels")
        if kind == "warm_cell" and len({row.get("source_digest") for row in selected if isinstance(row.get("source_digest"), str) and len(row["source_digest"]) == 64}) < 10:
            sufficient = False
            problems.append("warm samples must include at least ten actual cell source digests")
        if kind == "cold_start" and len({row.get("daemon_epoch") for row in selected if row.get("daemon_epoch")}) < 5:
            sufficient = False
            problems.append("cold starts require five distinct daemon epochs")
        limit = LIMITS_MS.get(kind)
        measured = max(values) if kind == "cold_start" and values else p95
        result[kind] = {
            "count": len(selected), "p50_ms": percentile(values, 0.5),
            "p95_ms": p95, "max_ms": max(values) if values else None,
            "sufficient_count": sufficient, "limit_ms": limit,
            "latency_met": sufficient and measured is not None and (limit is None or measured <= limit),
        }
    service = [row for rows in requests.values() for row in rows]
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


def analyze_durable(rows):
    """Attribute real durable publication independently of product acceptance."""
    if not rows:
        return {"status": "unmeasured", "count": 0, "evidence_problems": []}
    problems = []
    phases = {}
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
            if not path.is_absolute() or len(content) != row.get("manifest_bytes"):
                problems.append("durable manifest size/path differs from retained snapshot")
            if document.get("checksum") != row.get("manifest_checksum") or not row.get("manifest_checksum"):
                problems.append("durable manifest checksum differs from retained snapshot")
            public_schema = document.get("public_schema")
            if public_schema not in ("paired-public-v4", "paired-public-v5") or row.get("public_schema") != public_schema:
                problems.append("durable sample is not a matching retained artifact graph format")
        except (KeyError, TypeError, OSError, ValueError):
            problems.append("durable sample has no readable retained manifest")
    return {
        "status": "invalid" if problems else "measured", "count": len(rows),
        "phases_ms": {name: {"count": len(values), "p50_ms": percentile(values, 0.5), "p95_ms": percentile(values, 0.95)} for name, values in phases.items()},
        "samples": rows, "evidence_problems": sorted(set(problems)),
        "format_note": "v4 rows describe historical timing only; current recovery requires v5",
        "byte_counter_note": "manifest_bytes is retained file size; uninstrumented encode/hash/write byte counts remain unknown",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=Path, required=True)
    parser.add_argument("--compiler-trace", type=Path, action="append", required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--durable-samples", type=Path, help="independent durable-performance rows; never count toward product acceptance")
    args = parser.parse_args()
    samples = [json.loads(line.split(PREFIX, 1)[1]) for line in args.samples.read_text().splitlines() if PREFIX in line]
    events = [json.loads(line) for path in args.compiler_trace for line in path.read_text().splitlines() if line.strip()]
    report = analyze(samples, events, json.loads(args.manifest.read_text()))
    durable = []
    if args.durable_samples:
        durable = [json.loads(line.split("durable-performance ", 1)[1]) for line in args.durable_samples.read_text().splitlines() if "durable-performance " in line]
    report["durable_publication"] = analyze_durable(durable)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"accepted": report["accepted"], "samples": report["samples"], "evidence_problems": report["evidence_problems"]}, indent=2))
    return 0 if report["accepted"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
