#!/usr/bin/env python3
"""Production embedded host placement and refusal against a frozen native bundle.

Requires an idle shared command service and a configured loopback embedded
workspace. Provider credentials stay with that workspace's external auth owner;
readiness sends no model input. Each operation retains its frozen-owner receipt.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib
import uuid

GATE_PATH = Path(__file__).resolve().parents[3] / "scripts/package-cold-start-gate.py"
SPEC = importlib.util.spec_from_file_location("native_operator_observations", GATE_PATH)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


def run(*args, env=None, check=True, timeout=30):
    return subprocess.run(args, env=env, text=True, capture_output=True,
                          check=check, timeout=timeout)


def service(env):
    return run("systemctl", "--user", "show", "exomonad-command-resources.service",
               "-p", "MainPID", "-p", "ControlGroup", env=env).stdout


def processes():
    result = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            result[int(path.name)] = {
                "argv": (path / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace"),
                "group": (path / "cgroup").read_text().strip().removeprefix("0::"),
            }
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            pass
    return result


def slice_configuration(text, name):
    """Change only the selected slice in an explicit fixture's launch table."""
    parsed = tomllib.loads(text)
    if not isinstance(parsed.get("launch", {}).get("embedded"), dict):
        raise gate.GateError("placement fixture requires [launch.embedded]")
    if not re.fullmatch(r"[A-Za-z0-9_.@:-]+\.slice", name):
        raise gate.GateError("placement fixture requires one systemd slice")
    lines, in_launch, found = [], False, False
    for line in text.splitlines():
        if line.lstrip().startswith("["):
            in_launch = bool(re.fullmatch(r"\s*\[launch\]\s*(?:#.*)?", line))
            if in_launch:
                found = True
                lines.extend([line, "systemd_slice = " + json.dumps(name)])
                continue
        if in_launch and re.match(r"\s*systemd_slice\s*=", line):
            continue
        lines.append(line)
    if not found:
        raise gate.GateError("placement fixture requires an explicit [launch] table")
    result = "\n".join(lines) + "\n"
    if tomllib.loads(result)["launch"]["systemd_slice"] != name:
        raise gate.GateError("fixture slice override did not select the requested slice")
    return result


def refusal_workspace(operator, fixture_config, path, slice_name, output):
    created = operator.run(["new", str(path)], output / (path.name + "-new.process.json"), timeout=120)
    if created.returncode:
        raise gate.GateError("native owner could not create the refusal workspace")
    config = path / ".exomonad/config.toml"
    config.write_text(slice_configuration(fixture_config, slice_name))
    config.chmod(0o600)


def verify_small_budget_oom(output, env, selected_slice):
    unit = "exomonad-budget-test-" + uuid.uuid4().hex + ".service"
    try:
        result = run("systemd-run", "--user", "--wait", "--pipe", "--quiet",
                     "--slice=" + selected_slice, "--unit=" + unit,
                     "--property=MemoryMax=64M", "--property=MemoryHigh=64M",
                     "--property=MemorySwapMax=32M", "--expand-environment=no",
                     sys.executable, "-c", "a = bytearray(256 * 1024 * 1024)",
                     env=env, check=False, timeout=60)
        properties = run("systemctl", "--user", "show", unit, "-p", "Result",
                         "-p", "MemoryMax", "-p", "MemorySwapMax", env=env).stdout
        (output / "small-budget-oom.log").write_text(result.stdout + result.stderr + properties)
        assert result.returncode != 0 and "Result=oom-kill\n" in properties, properties
        assert "MemoryMax=67108864\n" in properties, properties
        assert "MemorySwapMax=33554432\n" in properties, properties
    finally:
        run("systemctl", "--user", "stop", unit, env=env)
        run("systemctl", "--user", "reset-failed", unit, env=env)


def verify_wrong_service_slice(operator, fixture_config, output, env):
    """A second configured budget must refuse the existing service untouched."""
    slice_name = "exomonadtest" + uuid.uuid4().hex + ".slice"
    unit = Path(env["XDG_RUNTIME_DIR"]) / "systemd/user" / slice_name
    unit.parent.mkdir(parents=True, exist_ok=True)
    session = "exomonad-wrong-slice-test-" + uuid.uuid4().hex
    before = service(env)
    work = output / "wrong-slice"
    try:
        unit.write_text("[Slice]\nMemoryHigh=1G\nMemoryMax=2G\nMemorySwapMax=1G\n")
        run("systemctl", "--user", "daemon-reload", env=env)
        refusal_workspace(operator, fixture_config, work, slice_name, output)
        rejected = operator.run(["init", "--workspace", str(work), "--session", session, "--no-attach"],
                                output / "wrong-slice.process.json", timeout=120)
        (output / "wrong-slice.log").write_text(rejected.stdout + rejected.stderr)
        assert rejected.returncode != 0, "wrong-slice launch was admitted"
        assert "shared command service is outside the selected slice" in rejected.stderr, rejected.stderr
        assert service(env) == before, "rejected run disturbed the existing service"
    finally:
        # An unexpected admission must still retire through the production owner.
        try:
            run_id = gate.read_session_run_id(work, session)
            if run_id:
                stopped = operator.run(["stop", "--run-id", run_id, "--session", session],
                                      output / "wrong-slice-stop.process.json")
                assert stopped.returncode == 0, "unexpected wrong-slice run did not stop"
        finally:
            try:
                run("systemctl", "--user", "stop", slice_name, env=env)
            finally:
                unit.unlink(missing_ok=True)
                run("systemctl", "--user", "daemon-reload", env=env)


def cleanup_runs(operator, runs, output, env, sessions=()):
    failures = []
    # A failed init can publish its durable identity before returning an error.
    # Invalid pointers fail cleanup without preventing known runs from retiring.
    for workspace, session in sessions:
        try:
            run_id = gate.read_session_run_id(workspace, session)
            if run_id and (run_id, session) not in runs:
                runs.append((run_id, session))
        except (OSError, gate.GateError) as error:
            failures.append("native run identity is unconfirmed: " + str(error))
    for run_id, session in runs:
        try:
            stopped = operator.run(["stop", "--run-id", run_id, "--session", session],
                                  output / (run_id + "-stop.process.json"))
            if stopped.returncode != 0:
                failures.append("native run stop failed: " + run_id)
        except (OSError, ValueError, subprocess.SubprocessError, gate.GateError) as error:
            failures.append("native run stop unconfirmed: " + str(error))
    try:
        run("systemctl", "--user", "stop", "exomonad-command-resources.service", env=env)
        if "MainPID=0\n" not in service(env):
            failures.append("shared command service stop is unconfirmed")
        else:
            (Path(env["XDG_RUNTIME_DIR"]) / "exomonad-commands/resources.sock").unlink(missing_ok=True)
    except (OSError, subprocess.SubprocessError) as error:
        failures.append("shared command service stop failed: " + str(error))
    (output / "cleanup.json").write_text(json.dumps({"confirmed": not failures, "failures": failures}, indent=2))
    if failures:
        raise gate.GateError("; ".join(failures))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("descriptor", "workspace", "output"):
        parser.add_argument("--" + key, required=True, type=Path)
    args = parser.parse_args()
    operator = gate.FrozenOperator(args.descriptor)
    env = operator.environment
    workspace = args.workspace.resolve(strict=True)
    _, listener, _ = gate.embedded_settings(workspace)
    if listener.rsplit(":", 1)[-1] != "0":
        raise gate.GateError("two concurrent placement hosts require an ephemeral loopback listener")
    fixture_config = (workspace / ".exomonad/config.toml").read_text()
    selected_slice = tomllib.loads(fixture_config)["launch"].get("systemd_slice", "swarm.slice")
    assert "MainPID=0\n" in service(env), "existing shared service: use an idle test boundary"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    (output / "qualification.json").write_text(json.dumps(operator.provenance(), indent=2))
    runs, sessions, shared = [], [], None
    passed = False
    try:
        missing = output / "missing-slice"
        refusal_workspace(operator, fixture_config, missing, "exomonad-missing-" + uuid.uuid4().hex + ".slice", output)
        missing_session = "exomonad-missing-slice-test-" + uuid.uuid4().hex
        sessions.append((missing, missing_session))
        rejected = operator.run(["init", "--workspace", str(missing), "--session", missing_session, "--no-attach"],
                                output / "missing-slice.process.json", timeout=120)
        (output / "missing-slice.log").write_text(rejected.stdout + rejected.stderr)
        assert rejected.returncode != 0 and ("configure the swarm slice" in rejected.stderr
                                             or "slice requires a finite MemoryHigh" in rejected.stderr)
        assert "MainPID=0\n" in service(env), "failed preflight started resources"
        limits = dict(line.split("=", 1) for line in run(
            "systemctl", "--user", "show", selected_slice, "-p", "MemoryMax", "-p", "MemorySwapMax", env=env).stdout.splitlines())
        for index in range(2):
            session = "exomonad-placement-test-" + uuid.uuid4().hex
            sessions.append((workspace, session))
            started = operator.run(["init", "--workspace", str(workspace), "--session", session, "--no-attach"],
                                   output / f"launch-{index}.process.json", timeout=120)
            (output / f"launch-{index}.log").write_text(started.stdout + started.stderr)
            run_id = gate.read_session_run_id(workspace, session)
            if run_id:
                runs.append((run_id, session))
            assert started.returncode == 0 and run_id, "native embedded launch failed"
            root = gate.state_root() / "tidepool/exomonad/runs" / run_id
            deadline = time.monotonic() + 60
            while True:
                try:
                    status, snapshot = gate.observe_ready(root / "status.json", run_id, session)
                    break
                except gate.GateError:
                    assert time.monotonic() < deadline, "embedded host never became ready"
                    time.sleep(0.1)
            (output / f"ready-{index}.json").write_text(json.dumps(status, indent=2))
            (output / f"snapshot-{index}.json").write_text(json.dumps(snapshot, indent=2))
            budget = json.loads((root / "resource-budget.json").read_text())
            assert budget["memory_max"] == int(limits["MemoryMax"])
            assert budget["swap_max"] == int(limits["MemorySwapMax"])
            group = Path("/sys/fs/cgroup") / budget["cgroup"].lstrip("/")
            assert int((group / "memory.max").read_text()) == budget["memory_max"]
            assert int((group / "memory.swap.max").read_text()) == budget["swap_max"]
            current = service(env)
            assert selected_slice in Path(current.split("ControlGroup=", 1)[1].splitlines()[0]).parts and "MainPID=0\n" not in current
            if shared is not None:
                assert current == shared, "runs started different resource owners"
            shared = current
            owned = {pid: row for pid, row in processes().items()
                     if str(root) in row["argv"] and selected_slice in Path(row["group"]).parts}
            for command in ("--daemon", " host ", "process-supervisor"):
                assert any(command in row["argv"] for row in owned.values()), (command, owned)
            (output / f"processes-{index}.json").write_text(json.dumps(owned, indent=2))
        verify_wrong_service_slice(operator, fixture_config, output, env)
        verify_small_budget_oom(output, env, selected_slice)
        passed = True
    finally:
        try:
            cleanup_runs(operator, runs, output, env, sessions)
        except BaseException:
            passed = False
            raise
        finally:
            (output / "result.json").write_text(json.dumps({"passed": passed, "service": shared}, indent=2))


if __name__ == "__main__":
    main()
