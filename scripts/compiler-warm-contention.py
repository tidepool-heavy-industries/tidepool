#!/usr/bin/env python3
"""Run the existing guarded notebook workload beside typed preparation.

The frontend remains the only compiler lifecycle owner. Supply a privately
fenced owning runtime closure and the existing campaign observation helpers.
Without --launch this prints the finite command; it starts no processes.
Execute only after central semantic eligibility and resource admission.
"""

import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

SELECTOR = "session::turn::scaling_tests::resident_warm_capture_cells_50"
RESERVE = 20 * 1024**3
MESSAGES = {"compiler request started", "compiler request finished",
            "compiler job dequeued", "compiler request waiting for capacity"}


def save(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def available():
    return int(next(row.split()[1] for row in Path("/proc/meminfo").read_text().splitlines()
                    if row.startswith("MemAvailable:"))) * 1024


def settle(process, *, grace=10, stop=False):
    """Reap one owned helper; compiler shutdown is still the frontend's job."""
    if process is None:
        return None
    forced = False
    if stop and process.poll() is None:
        process.terminate()
    try:
        code = process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        forced = True
        process.kill()
        code = process.wait(timeout=5)
    return {"exit_code": code, "forced_kill": forced}


def check_running(out, deadline):
    if (out / "cancel").exists():
        raise ValueError("campaign cancelled; settle through existing process owners")
    if time.monotonic() >= deadline:
        raise TimeoutError("bounded campaign deadline expired")


def settle_compiler_owner(owner, output, environment, grace=90):
    if owner is None:
        return None
    try:
        return {"exit_code": owner.wait(timeout=grace), "forced_kill": False}
    except subprocess.TimeoutExpired:
        pass
    # Ask the existing private daemon owner to STOP before killing its frontend.
    # Its lifecycle receipt was written by that exact selected frontend; never
    # discover or address another session's socket.
    control = {"status": "unavailable"}
    try:
        receipt = json.loads((output / "compiler/lifecycle.json").read_text())
        if not isinstance(receipt, dict):
            raise ValueError("private lifecycle receipt is not an object")
        socket = receipt["socket_path"]
        if not isinstance(socket, str) or not Path(socket).is_absolute():
            raise ValueError("private lifecycle socket is not absolute")
        argv = [environment["TIDEPOOL_EXTRACT"], "--stop-daemon", "--socket", socket]
        result = subprocess.run(argv, env=environment, timeout=30, capture_output=True)
        control = {"argv": argv, "exit_code": result.returncode,
                   "stdout": result.stdout.decode(errors="replace"),
                   "stderr": result.stderr.decode(errors="replace")}
        code = owner.wait(timeout=30)
        return {"exit_code": code, "forced_kill": False, "stop_control": control}
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        control["error"] = str(error)
    row = settle(owner, grace=5, stop=True)
    # Both frontend children already use the crate's parent-death constructor.
    # This fallback settles the process chain but cannot claim acknowledged
    # compiler cleanup; the actual lifecycle receipt remains authoritative.
    row.update(owner_forced_termination=True, stop_control=control)
    return row


class Trace:
    """Incrementally observe complete lines, retaining only request envelopes."""

    def __init__(self, path):
        self.path, self.offset, self.rows = path, 0, []

    def poll(self, final=False):
        if not self.path.exists():
            if final:
                raise ValueError("missing actual owner trace")
            return
        with self.path.open("rb") as stream:
            stream.seek(self.offset)
            while line := stream.readline(32 * 1024**2 + 1):
                if len(line) > 32 * 1024**2:
                    raise ValueError("trace row exceeds existing evidence bound")
                if not line.endswith(b"\n"):
                    if final:
                        raise ValueError("unfinished final trace row")
                    break
                event = json.loads(line)
                self.offset = stream.tell()
                fields = {}
                for span in event.get("spans", []):
                    fields.update(span)
                fields.update(event.get("span", {}))
                fields.update(event.get("fields", {}))
                if fields.get("message") in MESSAGES:
                    fields["observed_timestamp"] = event["timestamp"]
                    self.rows.append(fields)


def physical(rows):
    requests = {}
    for row in rows:
        if row["message"] not in {"compiler request started", "compiler request finished"}:
            continue
        identity = row.get("physical_execution")
        digest = row.get("compile_request")
        if not isinstance(identity, str) or not identity or not isinstance(digest, str) or not digest:
            raise ValueError("missing typed physical request identity")
        for field in ("admission_id", "request_ordinal", "compiler_jobs", "compiler_capabilities"):
            if type(row.get(field)) is not int or row[field] <= 0:
                raise ValueError("missing actual positive identity/grant: " + field)
        if type(row.get("worker")) is not int or row["worker"] < 0:
            raise ValueError("missing actual worker slot")
        epoch = row.get("daemon_epoch")
        if not isinstance(epoch, str) or not epoch:
            raise ValueError("missing actual daemon epoch")
        if row.get("compiler_workload") not in {"foreground", "preparation"}:
            raise ValueError("missing closed workload class")
        if identity != f'{epoch}:{row["admission_id"]}:{row["request_ordinal"]}':
            raise ValueError("physical identity disagrees with actual ownership tuple")
        entry = requests.setdefault(identity, {})
        kind = "start" if row["message"].endswith("started") else "finish"
        if kind in entry:
            raise ValueError("duplicate physical terminal/start")
        entry[kind] = row
    for entry in requests.values():
        if set(entry) != {"start", "finish"}:
            raise ValueError("unfinished physical request")
        for field in ("compile_request", "compiler_workload", "worker", "admission_id",
                      "daemon_epoch", "request_ordinal", "compiler_jobs", "compiler_capabilities"):
            if field not in entry["start"] or entry["start"][field] != entry["finish"].get(field):
                raise ValueError("request ownership/grant changed: " + field)
        if type(entry["finish"].get("exit_code")) is not int or entry["finish"]["exit_code"] != 0:
            raise ValueError("actual compiler failure")
        if stamp(entry["finish"]) < stamp(entry["start"]):
            raise ValueError("negative physical service interval")
    return list(requests.values())


def stamp(row):
    return datetime.fromisoformat(row["observed_timestamp"].replace("Z", "+00:00"))


def analyze(rows):
    requests = physical(rows)
    foreground = sorted((r for r in requests if r["start"]["compiler_workload"] == "foreground"),
                        key=lambda r: stamp(r["start"]))
    preparation = [r for r in requests if r["start"]["compiler_workload"] == "preparation"]
    if len(foreground) != 102 or len(requests) != len(foreground) + len(preparation):
        raise ValueError("foreground workload count or foreign workload differs")
    if any(r["start"]["worker"] != 0 for r in foreground):
        raise ValueError("foreground reserved slot not observed")
    if any(r["start"]["worker"] == 0 for r in preparation):
        raise ValueError("preparation occupied reserved foreground slot")
    overlaps = []
    for fg in foreground[2:]:  # two real warmup requests settle before preparation starts
        bg_ids = [bg["start"]["physical_execution"] for bg in preparation
                  if max(stamp(fg["start"]), stamp(bg["start"])) <
                  min(stamp(fg["finish"]), stamp(bg["finish"]))]
        if bg_ids:
            overlaps.append({"foreground": fg["start"]["physical_execution"], "preparation": bg_ids})
    return {"status": "observed_overlap" if overlaps else "inconclusive_no_overlap",
            "actual_foreground_requests": len(foreground),
            "actual_preparation_requests": len(preparation), "overlap": overlaps,
            "physical_requests": requests,
            "queue_and_capacity_observations": [r for r in rows if r["message"] not in
                                                {"compiler request started", "compiler request finished"}],
            "scope": "request wall overlap; notebook native guard is owned by counted test; no exclusive CPU claim"}


def graph(root, iteration):
    root.mkdir(exist_ok=True)
    imports, terms, sources = [], [], {}
    for n in range(64):
        name = f"ContentionLeaf{n}"
        path = root / (name + ".hs")
        path.write_text(f"module {name} where\nvalue{n} :: Int\nvalue{n} = {iteration * 64 + n}\n")
        imports.append(f"import {name}")
        terms.append(f"value{n}")
        sources[path.name] = sha(path)
    path = root / "ContentionGraph.hs"
    path.write_text("module ContentionGraph where\n" + "\n".join(imports) +
                    "\nresult :: Int\nresult = " + " + ".join(terms) + "\n")
    sources[path.name] = sha(path)
    return path, sources


def inside(plan):
    out = Path(plan["output"])
    socket = os.environ["TIDEPOOL_EXTRACT_DAEMON_SOCKET"]
    if not os.environ.get("TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT"):
        raise ValueError("required owned endpoint missing; direct fallback forbidden")
    save(out / "inherited-endpoint.json", {key: os.environ[key] for key in
         ("TIDEPOOL_EXTRACT_DAEMON_SOCKET", "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT")})
    trace = Trace(out / "compiler/compiler.jsonl")
    rows = []
    with (out / "foreground.stdout").open("xb") as stdout, (out / "foreground.stderr").open("xb") as stderr:
        foreground = subprocess.Popen(plan["foreground_argv"], stdout=stdout, stderr=stderr)
        deadline = time.monotonic() + 1800
        warm = []
        try:
            while time.monotonic() < deadline and foreground.poll() is None:
                check_running(out, deadline)
                trace.poll()
                warm = [r for r in trace.rows if r["message"] == "compiler request finished"
                        and r.get("compiler_workload") == "foreground"]
                if any(r.get("exit_code") != 0 for r in warm):
                    raise ValueError("foreground compiler failure before background admission")
                if len(warm) >= 2:
                    break
                time.sleep(.02)
            if len(warm) < 2:
                raise ValueError("native warmup did not settle")
            for iteration in range(plan["maximum_preparation_requests"]):
                check_running(out, deadline)
                if foreground.poll() is not None:
                    break
                if available() < RESERVE:
                    raise ValueError("host reserve below admitted 20GiB")
                source, sources = graph(out / "preparation-graph", iteration)
                command = [os.environ["TIDEPOOL_EXTRACT"], "--connect", socket, str(source),
                           "--target", "result", "--include", str(source.parent),
                           "--output-dir", str(out / f"preparation-{iteration:02}"),
                           "--workload", "preparation"]
                row = {"iteration": iteration, "argv": command, "source_sha256": sources,
                       "configured_source_files": 65, "actual_module_work": "see actual worker telemetry",
                       "semantics": "compile-only background", "started_ns": time.monotonic_ns()}
                rows.append(row)
                save(out / "preparation-execution.json", rows)
                with (out / f"preparation-{iteration:02}.stdout").open("xb") as bg_out, \
                     (out / f"preparation-{iteration:02}.stderr").open("xb") as bg_err:
                    background = subprocess.Popen(command, stdout=bg_out, stderr=bg_err)
                    try:
                        bg_deadline = min(deadline, time.monotonic() + 300)
                        while background.poll() is None:
                            check_running(out, bg_deadline)
                            if foreground.poll() not in (None, 0):
                                raise ValueError("foreground failed while preparation was active")
                            time.sleep(.05)
                    finally:
                        row["client_settlement"] = settle(background, grace=30, stop=True)
                        save(out / "preparation-execution.json", rows)
                row.update(exit_code=background.returncode, finished_ns=time.monotonic_ns())
                save(out / "preparation-execution.json", rows)
                if background.returncode or (foreground.poll() not in (None, 0)):
                    raise ValueError("first actual compiler/notebook failure; stop variants")
            while foreground.poll() is None:
                check_running(out, deadline)
                time.sleep(.05)
            code = foreground.returncode
            if code:
                raise ValueError(f"counted native workload failed: {code}")
        finally:
            # The counted runner's existing SIGTERM handler stops its delegated
            # service/process groups. Do not terminate the compiler frontend:
            # it must observe this child exit and acknowledge STOP/reap itself.
            receipt = settle(foreground, grace=30, stop=True)
            save(out / "foreground-settlement.json", receipt)
            if receipt["forced_kill"]:
                raise ValueError("counted runner forced settlement; native acceptance refused")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inside", type=Path)
    parser.add_argument("--frozen", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--support-dir", type=Path)
    parser.add_argument("--eligibility-evidence", type=Path)
    parser.add_argument("--compiler-source")
    parser.add_argument("--runtime-source")
    parser.add_argument("--workload-context", default="shared host; see actual resource observations")
    parser.add_argument("--launch", action="store_true")
    args = parser.parse_args()
    if args.inside:
        return inside(json.loads(args.inside.read_text()))
    if not all((args.frozen, args.output, args.support_dir, args.eligibility_evidence,
                args.compiler_source, args.runtime_source)):
        parser.error("frozen closure, fresh output, observation helpers, source identities and central eligibility evidence required")
    frozen = args.frozen.resolve(strict=True)
    output = args.output.resolve()
    support = args.support_dir.resolve(strict=True)
    environment = json.loads((frozen / "launch-environment.json").read_text())
    manifest = json.loads((frozen / "resource-files.json").read_text())
    compiler = json.loads((frozen / "compiler-roles/freeze-record.json").read_text())
    for item in manifest["files"] + compiler["inputs"]:
        if sha(Path(item["target"])) != item["sha256"]:
            raise ValueError("fenced role/resource changed: " + item["target"])
    for path, expected in [(frozen / "isolated-libtest.py", manifest["runner_sha256"]),
                           (frozen / "owning-test_env.json", manifest["wrapper_sha256"]),
                           (Path(environment["TIDEPOOL_COMPILER_DEPLOYMENT"]), compiler["deployment_sha256"])]:
        if sha(path) != expected:
            raise ValueError("fenced runner/deployment changed: " + str(path))
    environment["TIDEPOOL_TIMING"] = "1"
    resources = [k for k in json.loads((frozen / "owning-test_env.json").read_text())
                 if k not in {"PATH", "TIDEPOOL_KEEP_TEST_LOGS"}]
    resources.append("TIDEPOOL_PERFORMANCE_WORKSPACE_ROOT")
    runner = [sys.executable, str(frozen / "isolated-libtest.py"), str(frozen / "libtest"),
              "--exact", SELECTOR, "--expected-count", "1", "--jobs", "1", "--timeout", "1800",
              "--output-dir", str(output / "counted-results"), "--ignored", "--compiler-mode", "direct",
              "--delegated-service", "--service-slice", "tidepool-completion-build.slice"]
    for key in resources:
        runner += ["--resource-env", key]
    plan = {"schema": 1, "fixture_source": args.runtime_source, "snapshot_source": manifest["source"],
            "output": str(output),
            "maximum_preparation_requests": 48, "foreground_argv": runner,
            "environment": environment, "role_manifest_sha256": sha(frozen / "resource-files.json"),
            "compiler_source": args.compiler_source,
            "compiler_role_manifest_sha256": sha(frozen / "compiler-roles/freeze-record.json"),
            "eligibility_evidence": {"path": str(args.eligibility_evidence.resolve()),
                                     "sha256": sha(args.eligibility_evidence)},
            "runner_sha256": sha(Path(__file__)), "host_reserve_bytes": RESERVE,
            "parent_cgroup": Path("/proc/self/cgroup").read_text(),
            "external_workload_context": args.workload_context,
            "observation_inputs": {name: sha(support / name) for name in
                                   ("observe-resources.py", "observe-authored-cell-sources.py",
                                    "guarded50-source-recipe/manifest.json")}}
    command = [environment["TIDEPOOL_EXTRACT"], "--owned-daemon-run", str(output / "compiler"),
               "--workers", "2", "--", sys.executable, str(Path(__file__).resolve()),
               "--inside", str(output / "launch.json")]
    plan["owner_argv"] = command
    if not args.launch:
        print(json.dumps(plan, indent=2))
        return 0
    if available() < RESERVE:
        raise ValueError("host reserve below admitted 20GiB")
    output.mkdir(mode=0o700)
    save(output / "launch.json", plan)
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(("TIDEPOOL_DISABLE_", "TIDEPOOL_PERFORMANCE_", "TIDEPOOL_COMPILER_")) or key in {
            "TIDEPOOL_EXTRACT_DAEMON_SOCKET", "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT",
            "TIDEPOOL_EXTRACT_NO_DAEMON", "GHCRTS", "TIDEPOOL_MEMO_TRACE", "TIDEPOOL_TIMING"}:
            env.pop(key)
    env.update(environment)
    source_command = [sys.executable, str(support / "observe-authored-cell-sources.py"),
                      "--cases", str(output / "counted-results"), "--output", str(output / "authored-sources"),
                      "--finished", str(output / "finished"),
                      "--recipe", str(support / "guarded50-source-recipe/manifest.json")]
    source_observer = owner = observer = None
    settlement, failure, code = {}, None, 1
    old_term = signal.getsignal(signal.SIGTERM)
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt("campaign cancellation requested")
    signal.signal(signal.SIGTERM, interrupted)
    try:
        source_observer = subprocess.Popen(source_command, stdout=subprocess.DEVNULL)
        with (output / "owner.stdout").open("xb") as stdout, (output / "owner.stderr").open("xb") as stderr:
            owner = subprocess.Popen(command, env=env, stdout=stdout, stderr=stderr)
            observer = subprocess.Popen([sys.executable, str(support / "observe-resources.py"),
                                         "--pid", str(owner.pid), "--seconds", "1900",
                                         "--output", str(output / "resources.jsonl")], stdout=subprocess.DEVNULL)
            code = owner.wait(timeout=1900)
    except BaseException as error:
        failure = f"{type(error).__name__}: {error}"
    finally:
        # The inner workflow polls this marker even during an active request,
        # settles its clients/runner, and returns to the frontend shutdown owner.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        old_int = signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            try:
                (output / "cancel").touch()
            except OSError as error:
                settlement["cancellation_marker"] = {"error": str(error)}
            try:
                (output / "finished").write_text(str(code))
            except OSError as error:
                settlement["finished_marker"] = {"error": str(error)}
            for name, process, grace in (("owner", owner, 90),
                                         ("source_observer", source_observer, 10),
                                         ("resource_observer", observer, 5)):
                try:
                    settlement[name] = (settle_compiler_owner(process, output, env, grace)
                                        if name == "owner" else settle(process, grace=grace))
                except BaseException as error:
                    settlement[name] = {"error": f"{type(error).__name__}: {error}"}
        finally:
            signal.signal(signal.SIGINT, old_int)
            signal.signal(signal.SIGTERM, old_term)
            save(output / "process-settlement.json", settlement)
    if failure or any(row and (row.get("forced_kill") or row.get("owner_forced_termination")
                              or row.get("error") or row.get("exit_code", 0) != 0)
                      for row in settlement.values()):
        receipt = None
        try:
            receipt = json.loads((output / "compiler/lifecycle.json").read_text())
        except (OSError, ValueError):
            pass
        save(output / "summary.json", {"status": "failed", "exception": failure,
                                       "process_settlement": settlement,
                                       "actual_frontend_lifecycle": receipt,
                                       "cleanup_confirmed": receipt.get("cleanup_confirmed")
                                       if isinstance(receipt, dict) else None})
        return 1
    lifecycle = json.loads((output / "compiler/lifecycle.json").read_text())
    trace = Trace(output / "compiler/compiler.jsonl")
    trace.poll(final=True)
    report = analyze(trace.rows) if code == 0 else {"status": "failed", "request_envelopes": trace.rows}
    report.update(exit_code=code, cleanup_confirmed=lifecycle["cleanup_confirmed"],
                  trace_sha256=sha(trace.path), preparation_execution=json.loads(
                      (output / "preparation-execution.json").read_text())
                  if (output / "preparation-execution.json").exists() else [])
    capture = json.loads((output / "authored-sources/capture.json").read_text())
    report["actual_distinct_measured_sources"] = capture["observed_distinct_measured_source_sha256"]
    save(output / "summary.json", report)
    if code or not report["cleanup_confirmed"] or report["actual_distinct_measured_sources"] != 50:
        return 1
    return 0 if report["status"] == "observed_overlap" else 2


if __name__ == "__main__":
    sys.exit(main())
