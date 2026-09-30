#!/usr/bin/env python3
"""Measure Buck no-op reuse and controlled input invalidation in an isolated checkout."""

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import time


SUMMARY_FIELDS = {
    "Local actions": "local",
    "Remote actions": "remote",
    "Cached actions": "cached",
    "Other actions": "other",
}
MAX_LOG_BYTES = 8 * 1024 * 1024
INTERRUPTED_BY = None


class ProbeError(RuntimeError):
    pass


class ProbeInterrupted(Exception):
    def __init__(self, signum):
        self.signum = signum


def _interrupt_probe(signum, _frame):
    global INTERRUPTED_BY
    if INTERRUPTED_BY is None:
        INTERRUPTED_BY = signum


def check_interrupted():
    if INTERRUPTED_BY is not None:
        raise ProbeInterrupted(INTERRUPTED_BY)


@dataclass(frozen=True)
class ActionSummary:
    local: int
    remote: int
    cached: int
    other: int

    @property
    def classification(self):
        if self.cached and (self.local or self.remote):
            return "mixed-cache-hit-and-execution"
        if self.cached:
            return "action-cache-hit"
        if self.local or self.remote:
            return "actions-executed"
        return "no-action"


@dataclass(frozen=True)
class ActionRecord:
    identity: str
    reason: str
    executor: str | None
    build_id: str | None
    sources: str


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def parse_action_summary(text):
    found = {}
    for line in text.splitlines():
        match = re.fullmatch(r"- (Local actions|Remote actions|Cached actions|Other actions): (\d+)", line.strip())
        if match:
            found[SUMMARY_FIELDS[match.group(1)]] = int(match.group(2))
    missing = sorted(set(SUMMARY_FIELDS.values()) - set(found))
    if missing:
        raise ProbeError(f"Buck summary is missing action counts: {missing}")
    return ActionSummary(**found)


def parse_action_records(text, expected_build_id=None):
    try:
        document = json.loads(text)
    except json.JSONDecodeError:
        document = None
    if isinstance(document, dict):
        source_records = [document]
    elif isinstance(document, list):
        source_records = document
    else:
        source_records = []
        for line_number, line in enumerate(text.splitlines(), 1):
            if not line.strip():
                continue
            try:
                source_records.append(json.loads(line))
            except json.JSONDecodeError as error:
                raise ProbeError(f"invalid what-ran JSON on line {line_number}: {error}") from error

    records = []
    for line_number, raw in enumerate(source_records, 1):
        if not isinstance(raw, dict):
            raise ProbeError(f"what-ran record {line_number} is not a JSON object")
        identity = raw.get("identity")
        reason = raw.get("reason")
        reproducer = raw.get("reproducer") or {}
        details = reproducer.get("details") or {}
        env = details.get("env") or {}
        if not isinstance(identity, str) or not isinstance(reason, str):
            raise ProbeError(f"what-ran line {line_number} lacks an action identity or reason")
        build_id = env.get("BUCK_BUILD_ID")
        if expected_build_id and build_id and build_id != expected_build_id:
            raise ProbeError(
                f"action {identity!r} belongs to build {build_id}, expected {expected_build_id}"
            )
        records.append(ActionRecord(
            identity=identity.split(" (", 1)[0],
            reason=reason,
            executor=reproducer.get("executor"),
            build_id=build_id,
            sources=env.get("SRCS", ""),
        ))
    return records


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path, help="isolated Tidepool Git checkout")
    parser.add_argument("--evidence-dir", required=True, type=Path, help="new directory outside the checkout")
    parser.add_argument("--target", action="append", required=True, help="Buck target; repeat as needed")
    parser.add_argument("--probe-input", required=True, type=Path, help="clean tracked file to mutate temporarily")
    mutation = parser.add_mutually_exclusive_group(required=True)
    mutation.add_argument("--append-text", help="harmless text suffix to append during the mutation phase")
    mutation.add_argument("--append-hex", help="hex bytes to append during the mutation phase")
    parser.add_argument("--buck2", default=os.environ.get("BUCK2", "buck2"), help="pinned Buck executable")
    parser.add_argument("--build-timeout", type=float, default=1800)
    parser.add_argument("--expect-mutated-action", action="append", default=[], help="action label expected in mutation what-ran output")
    parser.add_argument("--expect-unaffected-action", action="append", default=[], help="action label forbidden in mutation what-ran output")
    options = parser.parse_args(argv)
    if not math.isfinite(options.build_timeout) or options.build_timeout <= 0:
        parser.error("--build-timeout must be finite and positive")
    if len(options.target) != len(set(options.target)):
        parser.error("--target values must be unique")
    if any(not target.strip() for target in options.target):
        parser.error("--target values must not be empty")
    if options.append_hex is not None:
        try:
            options.mutation_bytes = bytes.fromhex(options.append_hex)
        except ValueError as error:
            parser.error(f"--append-hex is invalid: {error}")
    else:
        options.mutation_bytes = options.append_text.encode("utf-8")
    if not options.mutation_bytes:
        parser.error("mutation suffix must not be empty")
    return options


def checked_paths(options):
    root = options.root.resolve(strict=True)
    evidence = options.evidence_dir.resolve(strict=False)
    probe = options.probe_input
    if not probe.is_absolute():
        probe = root / probe
    probe = probe.resolve(strict=True)
    if not probe.is_file():
        raise ProbeError(f"probe input is not a file: {probe}")
    try:
        relative_probe = probe.relative_to(root)
    except ValueError as error:
        raise ProbeError("probe input must be inside the isolated checkout") from error
    try:
        evidence.relative_to(root)
    except ValueError:
        pass
    else:
        raise ProbeError("evidence directory must be outside the checkout")
    if evidence.exists() and (not evidence.is_dir() or any(evidence.iterdir())):
        raise ProbeError(f"evidence directory is not empty: {evidence}")
    return root, evidence, probe, relative_probe.as_posix()


def validate_checkout(root, evidence, probe_relative, buck2):
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--show-toplevel"],
        text=True, capture_output=True, check=True,
    )
    top = Path(result.stdout.strip()).resolve(strict=True)
    if top != root:
        raise ProbeError(f"--root must name the Git checkout root, got {root} (top is {top})")
    buck_out = root / "buck-out"
    if buck_out.is_symlink() or not buck_out.is_dir() or not os.path.ismount(buck_out):
        raise ProbeError("buck-out must be the provisioned bind mount for this checkout")
    if not (root / ".buckconfig.local").is_file():
        raise ProbeError("checkout is not configured: .buckconfig.local is missing")
    tracked = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--error-unmatch", "--", probe_relative],
        text=True, capture_output=True,
    )
    if tracked.returncode:
        raise ProbeError(f"probe input is not tracked by Git: {probe_relative}")
    status = subprocess.run(
        ["git", "-C", str(root), "status", "--porcelain=v1", "--untracked-files=all", "--", probe_relative],
        text=True, capture_output=True, check=True,
    )
    if status.stdout.strip():
        raise ProbeError(f"refusing to mutate a dirty probe input: {probe_relative}\n{status.stdout}")
    executable = shutil.which(buck2)
    if executable is None:
        raise ProbeError(f"pinned Buck executable not found on PATH: {buck2}")
    return executable


def run_command(command, cwd, timeout):
    process = subprocess.Popen(
        command, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    selector = selectors.DefaultSelector()
    os.set_blocking(process.stdout.fileno(), False)
    selector.register(process.stdout, selectors.EVENT_READ)
    chunks = []
    output_size = 0
    output_truncated = False
    started = time.monotonic()
    timed_out = False

    def capture(chunk):
        nonlocal output_size, output_truncated
        keep = max(0, min(len(chunk), MAX_LOG_BYTES - output_size))
        if keep:
            chunks.append(chunk[:keep])
            output_size += keep
        if keep != len(chunk):
            output_truncated = True

    def drain_ready():
        drain_until = time.monotonic() + 0.5
        while time.monotonic() < drain_until:
            events = selector.select(0)
            if not events:
                return
            for key, _ in events:
                try:
                    chunk = os.read(key.fileobj.fileno(), 64 * 1024)
                except BlockingIOError:
                    continue
                if chunk:
                    capture(chunk)
                else:
                    selector.unregister(key.fileobj)

    def kill_group():
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def reap():
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired as error:
            raise ProbeError(f"command cleanup remained unconfirmed: {command!r}") from error

    try:
        while process.poll() is None:
            check_interrupted()
            remaining = timeout - (time.monotonic() - started)
            if remaining <= 0:
                timed_out = True
                break
            for key, _ in selector.select(min(0.2, remaining)):
                try:
                    chunk = os.read(key.fileobj.fileno(), 64 * 1024)
                except BlockingIOError:
                    continue
                if chunk:
                    capture(chunk)
                else:
                    selector.unregister(key.fileobj)
        if timed_out:
            kill_group()
            reap()
        else:
            # Do not leave descendants behind when a successful client exits.
            kill_group()
            reap()
        drain_ready()
    except BaseException:
        kill_group()
        reap()
        raise
    finally:
        selector.close()
        # This descriptor is nonblocking and has no competing buffered reader.
        process.stdout.close()
    output = b"".join(chunks).decode("utf-8", errors="replace")
    if output_truncated:
        output += "\n[output truncated by buck-cache-gate]\n"
    return process.returncode, output, timed_out


def run_build_phase(options, root, evidence, executable, phase):
    check_interrupted()
    event_log = evidence / f"{phase}.events.jsonl"
    build_id_file = evidence / f"{phase}.build-id"
    command = [
        executable, "build", "--local-only", "-c", "remote.enabled=false",
        "--event-log", str(event_log), "--write-build-id", str(build_id_file),
        "--show-full-simple-output", "-v", "1", *options.target,
    ]
    started = time.monotonic()
    status, output, timed_out = run_command(command, root, options.build_timeout)
    (evidence / f"{phase}.build.log").write_text(output)
    if timed_out:
        raise ProbeError(f"{phase} build exceeded {options.build_timeout:g}s; log: {phase}.build.log")
    if status:
        raise ProbeError(f"{phase} build failed ({status}); log: {phase}.build.log")
    if not build_id_file.is_file():
        raise ProbeError(f"{phase} did not produce a Buck build ID")
    build_id = build_id_file.read_text().strip()

    commands = {
        "actions": [executable, "log", "what-ran", "--no-remote", "--format", "json", str(event_log)],
        "cache-queries": [executable, "log", "what-ran", "--no-remote", "--emit-cache-queries", "--format", "json", str(event_log)],
        "summary": [executable, "log", "summary", "--no-remote", str(event_log)],
    }
    outputs = {}
    for name, command in commands.items():
        status, log, timed_out = run_command(command, root, min(options.build_timeout, 120))
        if timed_out or status:
            raise ProbeError(f"could not collect {phase} {name} evidence (status {status})")
        path = evidence / f"{phase}.{name}.jsonl" if name != "summary" else evidence / f"{phase}.summary.txt"
        path.write_text(log)
        outputs[name] = log

    summary = parse_action_summary(outputs["summary"])
    records = parse_action_records(outputs["actions"], build_id)
    cache_records = parse_action_records(outputs["cache-queries"], build_id)
    report = {
        "phase": phase,
        "build_id": build_id,
        "duration_seconds": round(time.monotonic() - started, 3),
        "classification": summary.classification,
        "action_summary": asdict(summary),
        "what_ran_records": [asdict(record) for record in records],
        "cache_query_records": [asdict(record) for record in cache_records],
    }
    (evidence / f"{phase}.report.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def input_status(root, relative_probe):
    result = subprocess.run(
        ["git", "-C", str(root), "status", "--porcelain=v1", "--untracked-files=all", "--", relative_probe],
        text=True, capture_output=True, check=True,
    )
    return result.stdout.strip()


def git_snapshot(root, relative_probe):
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        text=True, capture_output=True, check=True,
    ).stdout.strip()
    full_status = subprocess.run(
        ["git", "-C", str(root), "status", "--porcelain=v1", "--untracked-files=all"],
        text=True, capture_output=True, check=True,
    ).stdout
    probe_status = input_status(root, relative_probe)
    return {
        "head": head,
        "full_status_porcelain": full_status,
        "probe_status_porcelain": probe_status,
    }


def restore_input(path, baseline, mutated_hash, original_mode, original_mtime_ns):
    current = path.read_bytes()
    current_hash = sha256(current)
    baseline_hash = sha256(baseline)
    if current_hash == baseline_hash:
        # The data is already the known baseline. Restore metadata changed by
        # the atomic probe replacement without rewriting possibly identical
        # bytes supplied by another process.
        os.chmod(path, original_mode)
        os.utime(path, ns=(original_mtime_ns, original_mtime_ns))
        restored = path.stat()
        return (
            path.read_bytes() == baseline
            and restored.st_mode & 0o777 == original_mode
            and restored.st_mtime_ns == original_mtime_ns
        )
    if current_hash != mutated_hash:
        return False
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.restore-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(baseline)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, original_mode)
        os.utime(temporary, ns=(original_mtime_ns, original_mtime_ns))
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    restored = path.stat()
    return (
        path.read_bytes() == baseline
        and restored.st_mode & 0o777 == original_mode
        and restored.st_mtime_ns == original_mtime_ns
    )


def run_probe(options):
    root, evidence, probe, probe_relative = checked_paths(options)
    executable = validate_checkout(root, evidence, probe_relative, options.buck2)
    evidence.mkdir(parents=True, exist_ok=True)
    if any(evidence.iterdir()):
        raise ProbeError(f"evidence directory became nonempty during validation: {evidence}")
    baseline = probe.read_bytes()
    original_stat = probe.stat()
    baseline_snapshot = git_snapshot(root, probe_relative)
    backup = evidence / "probe-input.baseline"
    backup.write_bytes(baseline)
    baseline_hash = sha256(baseline)
    mutation_hash = sha256(baseline + options.mutation_bytes)
    metadata = {
        "root": str(root),
        "head": baseline_snapshot["head"],
        "baseline_full_status_porcelain": baseline_snapshot["full_status_porcelain"],
        "baseline_probe_status_porcelain": baseline_snapshot["probe_status_porcelain"],
        "branch": subprocess.run(
            ["git", "-C", str(root), "branch", "--show-current"],
            text=True, capture_output=True, check=True,
        ).stdout.strip(),
        "buck2": subprocess.run([executable, "--version"], text=True, capture_output=True, check=True).stdout.strip(),
        "probe_input": probe_relative,
        "probe_baseline_sha256": baseline_hash,
        "probe_mutated_sha256": mutation_hash,
        "targets": options.target,
        "evidence_directory": str(evidence),
    }
    (evidence / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    reports = []
    mutated = False
    restore_error = None
    restored_build_skipped = False
    try:
        reports.append(run_build_phase(options, root, evidence, executable, "baseline"))
        reports.append(run_build_phase(options, root, evidence, executable, "warm"))
        check_interrupted()
        before_mutation = git_snapshot(root, probe_relative)
        metadata["pre_mutation_full_status_porcelain"] = before_mutation["full_status_porcelain"]
        metadata["pre_mutation_probe_status_porcelain"] = before_mutation["probe_status_porcelain"]
        (evidence / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
        if before_mutation["head"] != baseline_snapshot["head"]:
            raise ProbeError("checkout HEAD changed after baseline capture; refusing to mutate input")
        if before_mutation["probe_status_porcelain"]:
            raise ProbeError("probe input became dirty before mutation; refusing to overwrite it")
        if probe.read_bytes() != baseline:
            raise ProbeError("probe input bytes changed after baseline capture; refusing to overwrite it")
        check_interrupted()
        mutated = True
        descriptor, temporary = tempfile.mkstemp(prefix=f".{probe.name}.probe-", dir=probe.parent)
        try:
            with os.fdopen(descriptor, "wb") as handle:
                handle.write(baseline + options.mutation_bytes)
                handle.flush()
                os.fsync(handle.fileno())
            os.chmod(temporary, original_stat.st_mode & 0o777)
            if probe.read_bytes() != baseline:
                raise ProbeError("probe input bytes changed before atomic mutation; refusing to overwrite it")
            current_head = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                text=True, capture_output=True, check=True,
            ).stdout.strip()
            if current_head != baseline_snapshot["head"] or input_status(root, probe_relative):
                raise ProbeError("checkout or probe input changed before atomic mutation; refusing to overwrite it")
            os.replace(temporary, probe)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        reports.append(run_build_phase(options, root, evidence, executable, "mutated"))
        mutated_actions = {record["identity"] for record in reports[-1]["what_ran_records"]}
        normalize = lambda label: label if label.startswith("root//") else "root" + label
        missing = [label for label in options.expect_mutated_action if normalize(label) not in mutated_actions]
        unexpected = [label for label in options.expect_unaffected_action if normalize(label) in mutated_actions]
        if missing or unexpected:
            raise ProbeError(
                f"mutation action mismatch: missing={missing!r}, unexpected={unexpected!r}"
            )
    finally:
        if mutated:
            try:
                restored = restore_input(
                    probe, baseline, mutation_hash,
                    original_stat.st_mode & 0o777, original_stat.st_mtime_ns,
                )
            except BaseException as error:
                restored = False
                restore_error = f"could not verify or restore probe input: {error}"
            if restored:
                mutated = False
                (evidence / "restoration.txt").write_text(
                    f"restored sha256={baseline_hash} from {backup}\n"
                )
                if INTERRUPTED_BY is not None or isinstance(sys.exc_info()[1], ProbeInterrupted):
                    restored_build_skipped = True
                else:
                    try:
                        reports.append(run_build_phase(options, root, evidence, executable, "restored"))
                    except ProbeInterrupted:
                        restored_build_skipped = True
                    except BaseException as error:
                        restore_error = f"input restored, but restored build failed: {error}"
                metadata["post_restoration_snapshot"] = git_snapshot(root, probe_relative)
                (evidence / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
            else:
                if restore_error is None:
                    restore_error = (
                        f"input changed concurrently; not overwriting {probe}. "
                        f"Original bytes are retained at {backup}"
                    )
                (evidence / "restoration.txt").write_text(restore_error + "\n")
        report_path = evidence / "gate-report.json"
        report_path.write_text(json.dumps({
            "phases": reports,
            "probe_restored": not mutated,
            "restoration_error": restore_error,
            "restored_build_skipped": restored_build_skipped,
        }, indent=2) + "\n")
    if restore_error:
        raise ProbeError(restore_error)
    check_interrupted()
    return evidence


def main(argv=None):
    global INTERRUPTED_BY
    INTERRUPTED_BY = None
    options = parse_args(sys.argv[1:] if argv is None else argv)
    old_handlers = {}
    for signum in (signal.SIGINT, signal.SIGTERM):
        old_handlers[signum] = signal.signal(signum, _interrupt_probe)
    try:
        evidence = run_probe(options)
        check_interrupted()
    except ProbeInterrupted as error:
        print(f"Buck cache gate interrupted by signal {error.signum}; input restoration ran", file=sys.stderr)
        return 128 + error.signum
    except (OSError, ProbeError, subprocess.CalledProcessError) as error:
        print(f"Buck cache gate failed: {error}", file=sys.stderr)
        return 1
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
    print(f"Buck cache gate evidence: {evidence}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
