#!/usr/bin/env python3
"""Actual Shoal bootstrap/placement acceptance. Private offline Codex homes; no inference.

Use --shoal for a packaged runner in a clean environment. Optional --codex,
--extractor and --worker overrides support unwrapped development binaries. Requires the configured user swarm.slice and no existing
command-resource service. Evidence is retained under --output, including failures.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid


def run(*args, env=None, check=True, timeout=600):
    return subprocess.run(args, env=env, text=True, capture_output=True,
                          check=check, timeout=timeout)


def service():
    return run("systemctl", "--user", "show", "shoal-command-resources.service",
               "-p", "MainPID", "-p", "ControlGroup").stdout


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


def verify_small_budget_oom(output):
    unit = "shoal-budget-test-" + uuid.uuid4().hex + ".service"
    try:
        result = run("systemd-run", "--user", "--wait", "--pipe", "--quiet",
                     "--slice=swarm.slice", "--unit=" + unit,
                     "--property=MemoryMax=64M", "--property=MemoryHigh=64M",
                     "--property=MemorySwapMax=32M", "--expand-environment=no",
                     sys.executable, "-c",
                     "a = bytearray(256 * 1024 * 1024)", check=False, timeout=60)
        properties = run("systemctl", "--user", "show", unit, "-p", "Result",
                         "-p", "MemoryMax", "-p", "MemorySwapMax").stdout
        (output / "small-budget-oom.log").write_text(result.stdout + result.stderr + properties)
        assert result.returncode != 0 and "Result=oom-kill\n" in properties, properties
        assert "MemoryMax=67108864\n" in properties, properties
        assert "MemorySwapMax=33554432\n" in properties, properties
    finally:
        run("systemctl", "--user", "stop", unit, check=False, timeout=30)
        run("systemctl", "--user", "reset-failed", unit, check=False, timeout=30)


def verify_wrong_service_slice(args, output, env):
    """A second configured budget must not silently adopt the live service."""
    slice_name = "shoaltest" + uuid.uuid4().hex + ".slice"
    unit = Path(os.environ["XDG_RUNTIME_DIR"]) / "systemd/user" / slice_name
    unit.parent.mkdir(parents=True, exist_ok=True)
    session = "shoal-wrong-slice-test-" + uuid.uuid4().hex
    before = service()
    try:
        unit.write_text("[Slice]\nMemoryHigh=1G\nMemoryMax=2G\nMemorySwapMax=1G\n")
        run("systemctl", "--user", "daemon-reload")
        work = output / "wrong-slice"
        selected = dict(env, CODEX_HOME=str(output / "codex-0"))
        run(str(args.shoal), "new", str(work), env=selected)
        config = work / ".shoal/config.toml"
        config.write_text(config.read_text() + '\n[launch]\nsystemd_slice = "' + slice_name + '"\n')
        rejected = run(str(args.shoal), "init", "--workspace", str(work),
                       "--session", session, "--no-attach", env=selected, check=False)
        (output / "wrong-slice.log").write_text(rejected.stdout + rejected.stderr)
        assert rejected.returncode != 0, rejected.stdout
        assert "shared command service is outside the selected slice" in rejected.stderr, rejected.stderr
        assert service() == before, "rejected run disturbed the existing service"
    finally:
        run("tmux", "kill-session", "-t", session, check=False, timeout=20)
        run("systemctl", "--user", "stop", slice_name, check=False, timeout=30)
        unit.unlink(missing_ok=True)
        run("systemctl", "--user", "daemon-reload")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("shoal", "output"):
        parser.add_argument("--" + key, required=True, type=Path)
    for key in ("codex", "extractor", "worker"):
        parser.add_argument("--" + key, type=Path)
    args = parser.parse_args()
    assert "MainPID=0\n" in service(), "existing shared service: use an idle test boundary"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    sessions = []
    units = set()
    roots = []
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("TIDEPOOL_", "CODEX_", "OPENAI_"))}
    for key, value in (("TIDEPOOL_EXTRACT", args.extractor),
                       ("TIDEPOOL_EXTRACT_WORKER", args.worker),
                       ("TIDEPOOL_INTERACTIVE_CODEX_BIN", args.codex)):
        if value is not None:
            env[key] = str(value.resolve())
    shared = None
    try:
        missing = output / "missing-slice"
        run(str(args.shoal), "new", str(missing), env=env)
        config = missing / ".shoal/config.toml"
        config.write_text(config.read_text() + '\n[launch]\nsystemd_slice = "shoal-missing-' + uuid.uuid4().hex + '.slice"\n')
        rejected = run(str(args.shoal), "init", "--workspace", str(missing),
                       "--no-attach", env=env, check=False)
        (output / "missing-slice.log").write_text(rejected.stdout + rejected.stderr)
        assert rejected.returncode != 0 and ("configure the swarm slice" in rejected.stderr
                                             or "slice requires a finite MemoryHigh" in rejected.stderr)
        assert "MainPID=0\n" in service(), "failed preflight started resources"
        for index in range(2):
            work = output / f"work-{index}"
            home = output / f"codex-{index}"
            home.mkdir(mode=0o700)
            # Deliberately no credentials and no reachable inference provider.
            (home / "config.toml").write_text('''model_provider = "offline"
approval_policy = "never"
sandbox_mode = "danger-full-access"
[model_providers.offline]
name = "Offline acceptance"
base_url = "http://127.0.0.1:1/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
''')
            selected = dict(env, CODEX_HOME=str(home))
            run(str(args.shoal), "new", str(work), env=selected)
            session = "shoal-placement-test-" + uuid.uuid4().hex
            sessions.append(session)
            started = run(str(args.shoal), "init", "--workspace", str(work),
                          "--session", session, "--no-attach", env=selected, check=False)
            (output / f"launch-{index}.log").write_text(started.stdout + started.stderr)
            assert started.returncode == 0, started.stdout + started.stderr
            status_path = next(line.removeprefix("status: ") for line in started.stdout.splitlines() if line.startswith("status: "))
            root = Path(status_path).parent
            roots.append(root)
            units.add("shoal-host-" + root.name + ".scope")
            deadline = time.monotonic() + 60
            while True:
                status = json.loads((root / "status.json").read_text())
                if status["phase"]["state"] == "ready":
                    break
                assert status["phase"]["state"] in ("starting", "awaiting_binding"), status
                assert time.monotonic() < deadline, status
                time.sleep(0.1)
            (output / f"ready-{index}.json").write_text(json.dumps(status, indent=2))
            budget = json.loads((root / "resource-budget.json").read_text())
            assert budget["memory_max"] == 18 * 1024**3
            assert budget["swap_max"] == 24 * 1024**3
            group = Path("/sys/fs/cgroup") / budget["cgroup"].lstrip("/")
            assert int((group / "memory.max").read_text()) == budget["memory_max"]
            assert int((group / "memory.swap.max").read_text()) == budget["swap_max"]
            current = service()
            assert "/swarm.slice/" in current and "MainPID=0\n" not in current, current
            if shared is not None:
                assert current == shared, "runs started different resource owners"
            shared = current
            # Find real payloads, not the outside tmux server's launcher processes.
            observed = processes()
            owned = {pid: row for pid, row in observed.items()
                     if str(root) in row["argv"] and "/swarm.slice/" in row["group"]}
            for command in ("--daemon", " host ", "process-supervisor"):
                assert any(command in row["argv"] for row in owned.values()), (command, owned)
            for row in owned.values():
                units.update(part for part in Path(row["group"]).parts if part.endswith(".scope"))
            (output / f"processes-{index}.json").write_text(json.dumps(owned, indent=2))
        verify_wrong_service_slice(args, output, env)
        verify_small_budget_oom(output)
        (output / "result.json").write_text(json.dumps({"passed": True, "service": shared}))
    finally:
        # Only this fixture's exact sessions/scopes; keep all source and logs.
        for row in processes().values():
            if (str(output) in row["argv"] or any(str(root) in row["argv"] for root in roots)) and "/swarm.slice/" in row["group"]:
                units.update(part for part in Path(row["group"]).parts if part.endswith(".scope"))
        for session in sessions:
            run("tmux", "kill-session", "-t", session, check=False, timeout=20)
        for unit in units:
            run("systemctl", "--user", "stop", unit, check=False, timeout=30)
        run("systemctl", "--user", "stop", "shoal-command-resources.service", check=False, timeout=30)
        # The fixture was the exclusive service owner. Remove only its stopped socket.
        if "MainPID=0\n" in service():
            socket = Path(os.environ["XDG_RUNTIME_DIR"]) / "shoal-commands/resources.sock"
            socket.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
