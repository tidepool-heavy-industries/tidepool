#!/usr/bin/env python3
"""Read-only, bounded resource observations for one Exomonad run (Linux cgroup v2)."""

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import socket
import sys
import time
from urllib.parse import quote

LIMIT = 20 * 1024 * 1024
PROC = Path("/proc")
CGROUP = Path("/sys/fs/cgroup")
GROUPS = ("host", "client", "compiler", "build", "other")


def read(path, maximum=65536):
    try:
        with path.open("rb") as file:
            data = file.read(maximum + 1)
        if len(data) > maximum:
            return None
        return data.decode("utf-8", "replace")
    except (OSError, ValueError):
        return None


def scalar(path):
    value = read(path, 128)
    if value is None:
        return None
    try:
        return int(value.strip())
    except ValueError:
        return None


def counters(path):
    data = read(path)
    if data is None:
        return None
    result = {}
    for line in data.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].isdigit():
            result[parts[0]] = int(parts[1])
    return result


def pressure(path):
    data = read(path)
    if data is None:
        return None
    result = {}
    for line in data.splitlines():
        parts = line.split()
        if parts and parts[0] in ("some", "full"):
            fields = {}
            for part in parts[1:]:
                if "=" in part:
                    key, value = part.split("=", 1)
                    try:
                        fields[key] = float(value) if key.startswith("avg") else int(value)
                    except ValueError:
                        pass
            result[parts[0]] = fields
    return result


def cgpath(pid):
    data = read(PROC / str(pid) / "cgroup", 4096)
    if data is None:
        return None
    for line in data.splitlines():
        if line.startswith("0::/"):
            relative = line[4:]
            if all(part not in ("", ".", "..") for part in relative.split("/")):
                return "/" + relative
    return None


def processes(run_id):
    """Use only exact argv tokens for ownership; never retain argv or environment."""
    found = {}
    for entry in PROC.iterdir():
        if not entry.name.isdecimal():
            continue
        pid = int(entry.name)
        cg = cgpath(pid)
        if cg is None:
            continue
        stat = read(entry / "stat", 4096)
        if stat is None or ") " not in stat:
            continue
        try:
            ppid = int(stat.rsplit(") ", 1)[1].split()[1])
        except (IndexError, ValueError):
            continue
        comm = (read(entry / "comm", 128) or "").strip()
        raw = None
        try:
            with (entry / "cmdline").open("rb") as file:
                raw = file.read(16384)
        except OSError:
            pass
        tokens = raw.split(b"\0") if raw is not None else []
        marker = run_id.encode()
        explicit = any(token == marker for token in tokens)
        # Runtime launcher arguments can contain the root path instead.
        explicit = explicit or any(token.endswith(b"/" + marker) for token in tokens)
        try:
            executable = Path(os.readlink(entry / "exe")).name.removesuffix(" (deleted)")
        except OSError:
            executable = None
        found[pid] = {"ppid": ppid, "comm": comm, "executable": executable,
                      "cgroup": cg, "explicit": explicit}
    return found


def group_for(value, host_unit):
    name = value["executable"] or value["comm"]
    if name.startswith(("codex", "claude", "gemini", "opencode")):
        return "client"
    if name.startswith(("tidepool-extract", "ghc")):
        return "compiler"
    if name.startswith(("cargo", "rustc", "cabal", "nix", "make", "cc1", "ld")):
        return "build"
    if host_unit:
        return "host"
    return "other"


def ownership(table, run_id):
    unit = "exomonad-host-" + run_id + ".service"
    host_paths = {v["cgroup"] for v in table.values() if unit in v["cgroup"].split("/")}
    seed_paths = set(host_paths)
    # A run-specific launcher labels its entire sibling scope. Ignore marker
    # processes outside the configured shared slice unless they are descendants.
    for value in table.values():
        if value["explicit"] and "/swarm.slice/" in value["cgroup"]:
            seed_paths.add(value["cgroup"])
    # Parent cgroup counters include descendants. Keep only disjoint roots.
    collapsed = set()
    for path in sorted(seed_paths, key=len):
        if not any(path == parent or path.startswith(parent + "/") for parent in collapsed):
            collapsed.add(path)
    seed_paths = collapsed
    owned = set()
    for pid, value in table.items():
        if any(value["cgroup"] == path or value["cgroup"].startswith(path + "/") for path in seed_paths):
            owned.add(pid)
    # Follow children even when a sandbox moves them into another cgroup.
    changed = True
    while changed:
        before = len(owned)
        owned.update(pid for pid, value in table.items() if value["ppid"] in owned)
        changed = len(owned) != before
    return owned, host_paths, seed_paths


def cgroup_snapshot(relative, table):
    if relative is None:
        return None
    path = CGROUP / relative.lstrip("/")
    if not path.is_dir():
        return {"path": relative, "available": False}
    high = read(path / "memory.high", 128)
    maximum = read(path / "memory.max", 128)
    swap_max = read(path / "memory.swap.max", 128)
    return {
        "path": relative, "available": True,
        "memory_current": scalar(path / "memory.current"),
        "memory_high": high.strip() if high else None,
        "memory_max": maximum.strip() if maximum else None,
        "swap_current": scalar(path / "memory.swap.current"),
        "swap_max": swap_max.strip() if swap_max else None,
        "events": counters(path / "memory.events"),
        "pressure": pressure(path / "memory.pressure"),
        "processes_observed": sum(1 for v in table.values() if v["cgroup"] == relative or v["cgroup"].startswith(relative + "/")),
    }


def aggregate_scopes(paths, table):
    snapshots = [cgroup_snapshot(path, table) for path in sorted(paths)]
    present = [item for item in snapshots if item and item.get("available")]
    def total(field):
        values = [item.get(field) for item in present]
        return sum(values) if values and all(isinstance(value, int) for value in values) else None
    return {
        "count": len(paths), "available": len(present),
        "cgroups": snapshots,
        "memory_current": total("memory_current"),
        "swap_current": total("swap_current"),
        "events": {key: sum((item.get("events") or {}).get(key, 0) for item in present)
                   for key in ("high", "max", "oom", "oom_kill")},
        "highest_pressure_some_avg10": max(
            [((item.get("pressure") or {}).get("some") or {}).get("avg10", 0) for item in present],
            default=None),
        "limits": [{"path": item["path"], "high": item["memory_high"], "max": item["memory_max"]}
                   for item in present if item["memory_high"] != "max" or item["memory_max"] != "max"],
    }


def memory_rollup(pid):
    data = read(PROC / str(pid) / "smaps_rollup", 16384)
    if data is None:
        return None
    result = {}
    for line in data.splitlines():
        if line.startswith(("Pss:", "SwapPss:")):
            parts = line.split()
            if len(parts) >= 2 and parts[1].isdigit():
                result[parts[0][:-1].lower() + "_bytes"] = int(parts[1]) * 1024
    return result if result else None


def log_freshness(workspace, run_id, now):
    root = workspace / ".exomonad" / "logs"
    names = [run_id + suffix for suffix in (".log", ".jsonl", "-journal.jsonl", "-compiler.log", "-compiler.jsonl")]
    result = {}
    for name in names:
        try:
            stat = (root / name).stat()
            age = round(max(0, now - stat.st_mtime), 1)
            result[name.removeprefix(run_id)] = {"bytes": stat.st_size, "age_seconds": age, "stale_120s": age >= 120}
        except OSError:
            result[name.removeprefix(run_id)] = None
    return result


def actor_probe(run_root):
    sock = run_root / "operator" / "operator.sock"
    proxy = run_root / "operator" / "proxy.json"
    try:
        session = json.loads(proxy.read_text())["session"]
        if not isinstance(session, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,100}", session):
            return {"state": "invalid_session"}
    except (OSError, ValueError, KeyError, TypeError):
        return {"state": "session_absent"}
    if not sock.exists():
        return {"state": "socket_absent"}
    started = time.monotonic()
    deadline = started + 2.0
    try:
        previous = os.open(".", os.O_RDONLY)
        try:
            # Unix socket addresses have a short path limit; use its directory.
            os.chdir(sock.parent)
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(max(0.001, deadline - time.monotonic()))
                connection.connect(sock.name)
                request = f"GET /v1/sessions/{quote(session)}/actors HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                connection.settimeout(max(0.001, deadline - time.monotonic()))
                connection.sendall(request.encode("ascii"))
                response = bytearray()
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        return {"state": "timeout", "latency_ms": round((time.monotonic() - started) * 1000)}
                    connection.settimeout(remaining)
                    chunk = connection.recv(min(65536, 262144 + 8192 - len(response) + 1))
                    if not chunk:
                        break
                    response.extend(chunk)
                    if len(response) > 262144 + 8192:
                        return {"state": "response_too_large", "latency_ms": round((time.monotonic() - started) * 1000)}
        finally:
            os.fchdir(previous)
            os.close(previous)
        headers, separator, body = bytes(response).partition(b"\r\n\r\n")
        if not separator or len(headers) > 8192 or len(body) > 262144:
            return {"state": "invalid_response"}
        lines = headers.split(b"\r\n")
        code = int(lines[0].split(b" ", 2)[1])
        result = {"state": "ok" if code == 200 else "http_error", "http_status": code,
                  "latency_ms": round((time.monotonic() - started) * 1000)}
        if code != 200:
            return result
        transfer_chunked = any(line.lower().startswith(b"transfer-encoding: chunked") for line in lines[1:])
        if transfer_chunked:
            decoded = bytearray()
            while body:
                size_text, separator, rest = body.partition(b"\r\n")
                if not separator:
                    return {"state": "invalid_response"}
                size = int(size_text.split(b";", 1)[0], 16)
                if size == 0:
                    break
                if size > len(rest) - 2 or len(decoded) + size > 262144:
                    return {"state": "invalid_response"}
                decoded.extend(rest[:size])
                body = rest[size + 2:]
            body = bytes(decoded)
        graph = json.loads(body)
        actors = graph.get("actors") if isinstance(graph, dict) else None
        if not isinstance(actors, list) or graph.get("session") != session:
            return {"state": "invalid_roster"}
        kinds = {}
        terminal = 0
        for actor in actors:
            if not isinstance(actor, dict):
                return {"state": "invalid_roster"}
            if actor.get("terminal") is not None:
                terminal += 1
            workbench = actor.get("workbench")
            kind = workbench.get("kind") if isinstance(workbench, dict) else None
            if isinstance(kind, str) and re.fullmatch(r"[a-z_]{1,40}", kind):
                kinds[kind] = kinds.get(kind, 0) + 1
        result["roster"] = {"actors": len(actors), "terminal": terminal, "workbench_kinds": kinds}
        return result
    except socket.timeout:
        return {"state": "timeout", "latency_ms": round((time.monotonic() - started) * 1000)}
    except (OSError, ValueError, IndexError, TypeError, UnicodeError):
        return {"state": "unresponsive", "latency_ms": round((time.monotonic() - started) * 1000)}


def sample(args, deep, prior):
    now = time.time()
    status_path = args.run_root / "status.json"
    status = None
    try:
        status = json.loads(status_path.read_text())
    except (OSError, ValueError):
        pass
    record_valid = isinstance(status, dict) and status.get("run_id") == args.run_id and status.get("workspace") == str(args.workspace)
    table = processes(args.run_id)
    owned, host_paths, seed_paths = ownership(table, args.run_id) if record_valid else (set(), set(), set())
    run_path = sorted(host_paths)[0] if host_paths else None
    slice_path = None
    if run_path and "/swarm.slice/" in run_path:
        slice_path = run_path.split("/swarm.slice/", 1)[0] + "/swarm.slice"
    else:
        candidates = [v["cgroup"].split("/swarm.slice/", 1)[0] + "/swarm.slice" for v in table.values() if "/swarm.slice/" in v["cgroup"]]
        if candidates:
            slice_path = max(set(candidates), key=candidates.count)
    ancestors = []
    if slice_path:
        parts = slice_path.strip("/").split("/")
        ancestors = ["/" + "/".join(parts[:i]) for i in range(len(parts), 0, -1)]
    run_counts = {key: 0 for key in GROUPS}
    slice_pids = {pid for pid, v in table.items() if slice_path and (v["cgroup"] == slice_path or v["cgroup"].startswith(slice_path + "/"))}
    for pid in owned:
        value = table[pid]
        run_counts[group_for(value, any(value["cgroup"] == path or value["cgroup"].startswith(path + "/") for path in host_paths))] += 1
    result = {
        "time": dt.datetime.fromtimestamp(now, dt.timezone.utc).isoformat(),
        "record": {"available": status is not None, "matches_input": record_valid,
                   "phase": status.get("phase", {}).get("state") if record_valid and isinstance(status.get("phase"), dict) else None,
                   "age_seconds": round(max(0, now - status_path.stat().st_mtime), 1) if status_path.exists() else None},
        "ownership": {"run_processes": len(owned), "run_groups": run_counts,
                      "run_cgroups": len(seed_paths), "shared_slice_processes": len(slice_pids),
                      "other_slice_processes": len(slice_pids - owned), "basis": "matching_run_record_and_exact_launcher_token_or_host_unit"},
        "run_cgroup": cgroup_snapshot(run_path, table),
        "run_scopes": aggregate_scopes(seed_paths, table),
        "shared_slice": cgroup_snapshot(slice_path, table),
        "ancestors": [cgroup_snapshot(path, table) for path in ancestors[1:]],
        "logs": log_freshness(args.workspace, args.run_id, now),
    }
    if deep:
        process_memory = {}
        for label, pids in (("run", owned), ("other_slice", slice_pids - owned)):
            groups = {key: {"processes": 0, "pss_bytes": 0, "swap_pss_bytes": 0, "unreadable": 0} for key in GROUPS}
            for pid in pids:
                value = table[pid]
                group = group_for(value, any(value["cgroup"] == path or value["cgroup"].startswith(path + "/") for path in host_paths))
                target = groups[group]
                target["processes"] += 1
                rollup = memory_rollup(pid)
                if rollup is None:
                    target["unreadable"] += 1
                else:
                    target["pss_bytes"] += rollup.get("pss_bytes", 0)
                    target["swap_pss_bytes"] += rollup.get("swappss_bytes", 0)
            process_memory[label] = groups
        result["process_memory"] = process_memory
        result["actor_probe"] = actor_probe(args.run_root) if record_valid else {"state": "record_unmatched"}
    warnings = []
    if not record_valid:
        warnings.append("run_record_absent_or_unmatched")
    if not run_path:
        warnings.append("run_host_cgroup_absent")
    current = result["shared_slice"]
    events = current.get("events") if current else None
    pressure_now = ((current or {}).get("pressure") or {}).get("some", {}).get("avg10", 0)
    if events and prior.get("events"):
        if events.get("high", 0) > prior["events"].get("high", 0) or events.get("oom", 0) > prior["events"].get("oom", 0):
            prior["pressure_streak"] = prior.get("pressure_streak", 0) + 1
        elif pressure_now >= 10.0:
            prior["pressure_streak"] = prior.get("pressure_streak", 0) + 1
        else:
            prior["pressure_streak"] = 0
    prior["events"] = events
    if prior.get("pressure_streak", 0) >= 2:
        warnings.append("sustained_shared_slice_memory_pressure")
    if deep:
        probe = result["actor_probe"]["state"]
        prior["probe_streak"] = prior.get("probe_streak", 0) + 1 if probe in ("unresponsive", "timeout") else 0
        if prior["probe_streak"] >= 2:
            warnings.append("actor_inspection_unresponsive")
    result["warnings"] = warnings
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="JSONL path outside the workload slice")
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}", args.run_id):
        parser.error("--run-id must be a UUID")
    for name in ("workspace", "run_root", "output"):
        if not getattr(args, name).is_absolute():
            parser.error(f"--{name.replace('_', '-')} must be absolute")
    args.workspace = args.workspace.resolve()
    args.run_root = args.run_root.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    own_cg = cgpath(os.getpid()) or ""
    if "/swarm.slice" in own_cg:
        parser.error("observer must run outside the workload slice")
    summary_path = args.output.with_suffix(args.output.suffix + ".summary.json")
    prior = {}
    count = 0
    start = time.monotonic()
    while True:
        deep = args.once or count % 4 == 0
        observation = sample(args, deep, prior)
        line = (json.dumps(observation, separators=(",", ":")) + "\n").encode()
        size = args.output.stat().st_size if args.output.exists() else 0
        stopped = size + len(line) > LIMIT
        if not stopped:
            with args.output.open("ab") as file:
                file.write(line)
            count += 1
        summary = {"run_id": args.run_id, "samples_this_invocation": count,
                   "jsonl_bytes": size if stopped else size + len(line),
                   "latest": {key: observation.get(key) for key in ("time", "record", "ownership", "warnings", "actor_probe")},
                   "stopped_at_limit": stopped}
        summary_path.write_text(json.dumps(summary, indent=2) + "\n")
        if args.once or stopped:
            print(json.dumps(summary, separators=(",", ":")))
            return 0 if not stopped else 2
        next_tick = start + count * 15
        time.sleep(max(0, next_tick - time.monotonic()))


if __name__ == "__main__":
    sys.exit(main())
