"""Prove the isolated runner refuses an actually retained source control.

The native fixture uses no Nix GC registration. Controller success establishes
refusal detection; the retained native case remains unqualified evidence.
"""
import argparse
import hashlib
import json
import os
import signal
from pathlib import Path
import subprocess
import sys
import tempfile


CASE = "configured_source_release_refusal_is_retained"
PROFILE_REFUSAL = "unsupported immutable service profile version"
SOURCE_REFUSAL = "service package source release failed: exit status: 9"


def reference(path):
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def check_refusal(output, result):
    assert result.returncode == 1, (result.returncode, result.stdout, result.stderr)
    key = hashlib.sha256(CASE.encode()).hexdigest()
    case_path = output / (key + ".json")
    case_bytes = case_path.read_bytes()
    case = json.loads(case_bytes)
    execution = case["execution"]
    assert case["test"] == CASE and case["passed"] is False, case
    assert execution["libtest_passed"] is True, execution
    assert execution["exit_code"] == 0, execution
    assert execution["executed_test_count"] == execution["passed_test_count"] == 1, execution
    assert execution["failed_test_count"] == 0, execution
    assert execution["process_execution_count"] == 1, execution
    assert execution["process_cleanup_status"] == "confirmed", execution
    assert execution["compiler_cleanup_status"] == "unconfirmed", execution
    assert execution["compiler_cleanup_observation_complete"] is True, execution
    assert execution.get("timed_out") in (None, False), execution
    diagnostics = execution["diagnostic_summaries"]
    assert not diagnostics["issues"], diagnostics["issues"]
    owners = diagnostics["owned_compiler_roots"]
    assert len(owners) == 1, owners
    owner = owners[0]
    assert owner["role"] == "native_test_control" and owner["authority"] is False, owner
    assert owner["owned_cleanup_status"] == "unconfirmed", owner
    owner_path = Path(owner["path"])
    expected = Path(execution["artifact_root"]) / "owned-compiler-control-configured-source/release-refusal"
    assert owner_path == expected, (owner_path, expected)
    report_path = owner_path / "owned-service.json"
    report_bytes = report_path.read_bytes()
    report = json.loads(report_bytes)
    assert owner["owned_service"] == report, owner
    assert report["owner_directory"] == str(owner_path), report
    assert report["complete"] is False, report
    assert report["action"]["status"] == "failed", report
    assert report["acquisition"]["status"] == "acquired", report
    assert report["daemon_reap"]["status"] == "reaped", report
    assert report["service_terminal"]["workers_retired"] is True, report
    assert report["service_terminal"]["socket_retired"] is True, report
    assert PROFILE_REFUSAL in report["service_terminal"]["failures"], report
    assert report["source_settlement"]["status"] == "retained", report
    assert report["source_settlement"]["failure"] == SOURCE_REFUSAL, report
    assert SOURCE_REFUSAL in report["settlement_failures"], report
    streams = {}
    for label, stream in case["streams"].items():
        assert stream["truncated"] is False, stream
        path = Path(stream["path"])
        assert path.name == str(path) and path.name not in ("", ".", ".."), stream
        actual = reference(output / path)
        assert actual["sha256"] == stream["sha256"] and actual["bytes"] == stream["bytes"], stream
        streams[label] = actual
    # The failed native evidence is historical fact, not a cleanup capability.
    # Nothing in this controller changes it or disposes its retained source.
    assert report_path.read_bytes() == report_bytes
    assert case_path.read_bytes() == case_bytes
    return {"control_passed": True, "child_qualified": False,
            "native_executed": 1, "native_passed": 1, "outer_runner_exit": 1,
            "case_report": reference(case_path), "owned_service_report": reference(report_path),
            "streams": streams, "real_nix_gc_registration": False}


def run_refusal_case(command, output):
    process=subprocess.Popen(command,env=os.environ.copy(),text=True,
                             stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    primary=None
    stdout=stderr=""
    try:
        stdout,stderr=process.communicate(timeout=120)
    except BaseException as error:
        primary=error
        # The production runner owns the native case's process group. Ask that
        # owner to cancel and settle before a last bounded direct-child stop.
        process.send_signal(signal.SIGTERM)
        try:
            stdout,stderr=process.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            stdout,stderr=process.communicate(timeout=5)
    finally:
        (output / "runner.stdout").write_text(stdout)
        (output / "runner.stderr").write_text(stderr)
        (output / "invocation.json").write_text(json.dumps({"command":command,
            "exit_status":process.returncode,"failure":None if primary is None else type(primary).__name__+": "+str(primary)},indent=2)+"\n")
    if primary is not None:
        raise primary
    return subprocess.CompletedProcess(command,process.returncode,stdout,stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path)
    options = parser.parse_args()
    output = options.output_dir or Path(tempfile.mkdtemp(prefix="configured-service-refusal-"))
    if output.is_symlink():
        parser.error("output directory is a symlink")
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    output = output.resolve(strict=True)
    if next(output.iterdir(), None) is not None:
        parser.error("output directory must be fresh and empty")
    (output / ".configured-service-refusal-run").mkdir(mode=0o700)
    case_output = output / "expected-refusal"
    command = [sys.executable, str(options.runner.resolve(strict=True)),
               str(options.binary.resolve(strict=True)), "--exact", CASE,
               "--expected-count", "1", "--ignored", "--compiler-mode", "direct",
               "--timeout", "60", "--output-dir", str(case_output), "--retain-artifacts",
               "--resource-env", "TIDEPOOL_TEST_BASH", "--resource-env", "TIDEPOOL_TEST_PYTHON"]
    result = run_refusal_case(command, output)
    observation = check_refusal(case_output, result)
    (output / "observations.json").write_text(json.dumps(observation, indent=2) + "\n")
    print(json.dumps(observation, sort_keys=True))
    print(f"Retained expected-refusal evidence: {output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
