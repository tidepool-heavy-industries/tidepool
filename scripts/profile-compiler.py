#!/usr/bin/env python3
"""Bounded, same-user CPU/RSS capture of an explicitly selected compiler worker."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import subprocess
import time

LIMIT = 4 * 1024 * 1024
DERIVED_LIMIT = 16 * 1024 * 1024


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


def bounded_copy(source, destination, offset=0):
    with open(source, "rb") as stream:
        stream.seek(offset)
        data = stream.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise ValueError(f"capture exceeds four MiB: {source}")
    destination.write_bytes(data)
    return {"path": str(source), "offset": offset, "bytes": len(data), "sha256": digest(destination)}


def terminate_child(process):
    if process is not None and process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()



def diagnostic_rows(path, worker_pid=None):
    for line in path.read_text(errors="replace").splitlines():
        identity = {}
        try:
            parsed = json.loads(line)
            fields, span = parsed.get("fields", {}), parsed.get("span", {})
            line = fields.get("line", line)
            identity = {key: fields.get(key, span.get(key)) for key in
                        ("compile_request", "worker_pid", "run_id", "daemon_epoch", "admission_id")
                        if fields.get(key, span.get(key)) is not None}
        except (ValueError, AttributeError):
            pass
        if worker_pid is not None and "worker_pid" in identity and int(identity["worker_pid"]) != worker_pid:
            continue
        identity["worker_identity"] = "trace_pid" if "worker_pid" in identity else "assumed_selected_worker; log has no PID"
        yield line, identity


def timing_rows(path, worker_pid=None):
    rows = []
    for line, identity in diagnostic_rows(path, worker_pid):
        if "tidepool-timing-detail " in line:
            fields = dict(re.findall(r"(\w+)=([^\s]+)", line))
            if "start_ns" in fields and "end_ns" in fields:
                rows.append({**fields, "trace": identity})
    return rows


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", required=True, type=int, help="already running, privately owned worker")
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
                metadata["timing_log"] = bounded_copy(args.timing_log, out / "timing.log", log_offset)
            except (OSError, ValueError) as error:
                metadata["timing_log_error"] = str(error)
        (out / "capture.json").write_text(json.dumps(metadata, indent=2) + "\n")
    data = out / "cpu.perf.data"
    if data.exists() and data.stat().st_size:
        script_exit = run_bounded_output([perf, "script", "--show-lost-events", "--ns", "-i", str(data),
                                          "-F", "time,pid,tid,ip,dso,sym,symoff"], out / "samples.txt", out / "script.stderr")
        report_exit = run_bounded_output([perf, "report", "--stdio", "--show-nr-samples", "--no-children", "-i", str(data)],
                                        out / "report.txt", out / "report.stderr")
        rows = (out / "samples.txt").read_text(errors="replace").splitlines()
        sample_rows, lost = parse_samples(rows)
        phases = timing_rows(out / "timing.log", args.pid) if (out / "timing.log").exists() else []
        phase_samples = []
        for phase in phases:
            start, end = int(phase["start_ns"]), int(phase["end_ns"])
            count = 0
            for row in sample_rows:
                count += start <= row["monotonic_ns"] <= end
            phase_samples.append({**phase, "cpu_samples": count, "overlap": "nested phases are not additive"})
        rss_rows = [json.loads(row) for row in (out / "rss.jsonl").read_text().splitlines()]
        summary = {"sample_count": len(sample_rows), "lost_samples": lost if script_exit == 0 else None,
                   "perf_script_exit": script_exit,
                   "unknown_symbol_samples": sum("[unknown]" in row["line"] for row in sample_rows),
                   "rss_sample_count": len(rss_rows),
                   "sampled_peak_rss_bytes": max((row.get("VmRSS", 0) for row in rss_rows), default=None),
                   "phase_samples": phase_samples,
                   "observed_compile_requests": sorted({phase["trace"]["compile_request"] for phase in phases if "compile_request" in phase["trace"]}),
                   "hash_byte_counts": hash_byte_counts(out / "timing.log", args.pid) if (out / "timing.log").exists() else {},
                   "attribution_limit": "leaf instruction symbols; tail-call stacks and nearest global symbols do not identify callers",
                   "phase_coverage": metadata["phase_coverage"],
                   "workload_success": metadata["command_exit"] == 0 if command else None,
                   "timing_log_complete": args.timing_log is not None and "timing_log_error" not in metadata,
                   "perf_report_exit": report_exit,
                   "capture_complete": capture_error is None and metadata["perf_recording_ok"] and script_exit == 0 and report_exit == 0 and len(sample_rows) > 0 and lost is not None}
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps({"output": str(out), "sample_count": len(sample_rows), "lost_samples": summary["lost_samples"], "capture_complete": summary["capture_complete"],
                          "workload_success": summary["workload_success"], "timing_log_complete": summary["timing_log_complete"]}))
    else:
        print(json.dumps({"output": str(out), "capture_error": capture_error, "sample_count": 0}))
    return 1 if capture_error or not metadata["perf_recording_ok"] or metadata["command_exit"] not in (None, 0) or "timing_log_error" in metadata or not data.exists() or not data.stat().st_size or not summary["capture_complete"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
