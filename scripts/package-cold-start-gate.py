#!/usr/bin/env python3
"""Measure five packaged Exomonad cold starts against a frozen embedded workspace.

The command runs real packaged `init` and `stop` operations. It retains one JSON
report, init output, and stop output per sample under the requested output path.
The timer starts immediately before each packaged init process is spawned and
stops after status.json and the authenticated loopback browser WebSocket both
prove that the root is idle.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import secrets
import socket
import sqlite3
import struct
import subprocess
import sys
import time
import tomllib
import uuid


SAMPLE_COUNT = 5
MAX_SAMPLE_SECONDS = 10
POLL_SECONDS = 0.05
WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


class GateError(RuntimeError):
    """A required measurement observation was unavailable or invalid."""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def seal_sample(sample: dict) -> dict:
    """Bind successful stop/store evidence to the retained compiler trace."""
    if sample.get("daemon_trace"):
        trace_path = Path(sample["daemon_trace"])
        try:
            sample["daemon_trace_sha256"] = sha256(trace_path)
        except OSError as error:
            sample["failure"] = sample.get("failure") or (
                f"compiler daemon trace unavailable after stop: {error}")
    sample["completed"] = sample.get("failure") is None
    return sample


def parse_status(path: Path, run_id: str, session: str) -> dict | None:
    """Read one run status; a partial atomic replacement is not readiness."""
    try:
        status = json.loads(path.read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        return None
    if not isinstance(status, dict) or status.get("run_id") != run_id:
        return None
    if status.get("session") != session:
        return None
    phase = status.get("phase")
    if not isinstance(phase, dict) or phase.get("state") not in ("ready", "embedded_ready"):
        return None
    return status


def compiler_boot(trace_path: Path, run_id: str) -> dict:
    """Return the sole compiler boot row for this run, failing closed otherwise."""
    try:
        lines = trace_path.read_text().splitlines()
    except OSError as error:
        raise GateError(f"compiler daemon trace unavailable: {trace_path}: {error}") from error
    boots = []
    for line_number, line in enumerate(lines, 1):
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise GateError(f"malformed compiler daemon trace at line {line_number}") from error
        fields: dict = {}
        if isinstance(event, dict):
            for value in [*(event.get("spans") or []), event.get("span"), event.get("fields")]:
                if isinstance(value, dict):
                    fields.update(value)
        if fields.get("message") == "compiler daemon ready" and fields.get("run_id") == run_id:
            boots.append(fields)
    if len(boots) != 1:
        raise GateError(f"expected one compiler daemon ready trace row for {run_id}, found {len(boots)}")
    boot = boots[0]
    for name in ("daemon_epoch", "daemon_pid", "producer", "executable", "worker"):
        if name not in boot or boot[name] in (None, "", 0):
            raise GateError(f"compiler daemon ready row lacks {name}")
    return boot


def read_session_run_id(workspace: Path, session: str) -> str | None:
    pointer = workspace / ".exomonad" / "sessions" / session / "run-id"
    try:
        run_id = pointer.read_text().strip()
    except (OSError, UnicodeDecodeError):
        return None
    if not run_id:
        return None
    if not all(char.isascii() and (char.isalnum() or char == "-") for char in run_id):
        raise GateError("session run-id pointer contains an invalid run identity")
    return run_id


class FrozenOperator:
    """Use the frozen package's existing execution owner for every host operation."""
    def __init__(self, descriptor: Path):
        self.path = descriptor.resolve(strict=True)
        owner_path = self.path.with_name("qualification.py")
        spec = importlib.util.spec_from_file_location("frozen_native_qualification", owner_path)
        self.owner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.owner)
        self.descriptor = self.owner.verify(self.path)
        if not callable(getattr(self.owner, "launch_execution", None)):
            raise GateError("frozen bundle lacks the current native operation provider; freeze the owning native build")
        self.environment = self.owner.execution_environment(self.descriptor)

    def launch(self, arguments, *, stdout=None, stderr=None, cache_root=None):
        return self.owner.launch_execution(self.path, arguments, stdout=stdout, stderr=stderr,
                                           cache_root=cache_root)

    def run(self, arguments, report: Path, timeout=30):
        execution = self.launch(arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            stdout, stderr = execution.process.communicate(timeout=timeout)
        except BaseException:
            if execution.process.poll() is None:
                execution.process.kill()
            execution.process.wait(timeout=5)
            raise
        finally:
            execution.record(report)
        return subprocess.CompletedProcess(execution.command, execution.process.returncode,
            stdout.decode(errors="replace"), stderr.decode(errors="replace"))

    def store_schema(self, output: Path):
        receipt = output / "store-schema.process.json"
        result = self.run(["harness-store-schema"], receipt)
        if result.returncode != 0:
            raise GateError("frozen host could not report its compiled Store schema")
        try:
            schema = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise GateError("frozen host returned malformed Store schema metadata") from error
        if (not isinstance(schema, dict) or set(schema) != {"version"}
                or type(schema["version"]) is not int or not 0 < schema["version"] <= 0xffffffff):
            raise GateError("frozen host returned unsupported Store schema metadata")
        metadata = output / "store-schema.json"
        metadata.write_text(json.dumps(schema) + "\n")
        return {**schema, "revision": self.descriptor["harness_revision"],
                "metadata": str(metadata), "metadata_sha256": sha256(metadata),
                "process_report": str(receipt), "process_report_sha256": sha256(receipt)}

    def provenance(self):
        return {"descriptor": str(self.path), "descriptor_sha256": sha256(self.path),
                **{key: self.descriptor[key] for key in (
                    "source_oid", "harness_revision", "profile", "stdlib_mode")}}


def provider_store_evidence(store_path: Path, expected_schema: int) -> dict:
    """Count provider attempts from the pinned harness Store, without reading payloads.

    The harness writes a durable `before-request` decision immediately before
    calling its provider transport. A model_turn event alone would miss a
    request that failed before returning, so both evidence streams are checked.
    """
    if not store_path.is_file():
        raise GateError(f"embedded provider Store is unavailable: {store_path}")
    connection = None
    try:
        connection = sqlite3.connect(
            store_path.resolve().as_uri() + "?mode=ro", uri=True, timeout=2,
        )
        connection.execute("PRAGMA query_only=ON")
        check = connection.execute("PRAGMA integrity_check").fetchone()
        if check != ("ok",):
            raise GateError(f"embedded provider Store integrity check failed: {check!r}")
        tables = {row[0] for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type='table'")}
        if not {"schema_version", "decisions", "events"}.issubset(tables):
            raise GateError("embedded provider Store lacks the pinned request evidence tables")
        versions = [row[0] for row in connection.execute("SELECT version FROM schema_version")]
        if len(versions) != 1 or versions[0] != expected_schema:
            raise GateError(
                "embedded provider Store schema version does not match the pinned harness "
                f"schema {expected_schema}: {versions!r}"
            )
        attempt_count = connection.execute(
            "SELECT COUNT(*) FROM decisions WHERE hook='before-request'").fetchone()[0]
        model_turn_count = connection.execute(
            "SELECT COUNT(*) FROM events WHERE kind='model_turn'").fetchone()[0]
        usage_count = connection.execute(
            "SELECT COUNT(*) FROM events WHERE kind='responses_usage'").fetchone()[0]
        if model_turn_count > attempt_count or usage_count > attempt_count:
            raise GateError("provider Store turn evidence exceeds its before-request attempts")
    except sqlite3.Error as error:
        raise GateError(f"could not inspect embedded provider Store read-only: {error}") from error
    finally:
        if connection is not None:
            connection.close()
    files = []
    for path in (store_path, Path(str(store_path) + "-wal"), Path(str(store_path) + "-shm")):
        if path.is_file():
            files.append({"path": str(path), "sha256": sha256(path), "bytes": path.stat().st_size})
    if not files or files[0]["path"] != str(store_path):
        raise GateError("embedded provider Store evidence disappeared during observation")
    return {
        "verified": True,
        "method": "harness-store-decisions-before-request",
        "store_path": str(store_path),
        "store_schema_version": versions[0],
        "provider_request_attempts": attempt_count,
        "completed_model_turns": model_turn_count,
        "responses_usage_events": usage_count,
        "files": files,
    }


def embedded_settings(workspace: Path) -> tuple[str, str, str]:
    config_path = workspace / ".exomonad" / "config.toml"
    try:
        config = tomllib.loads(config_path.read_text())
        embedded = config["launch"]["embedded"]
        secret_path = Path(embedded["session_secret_file"])
        listen = embedded["listen"]
        origin_scheme = embedded.get("public_origin_scheme", "https")
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise GateError(f"cannot read embedded launch settings from {config_path}") from error
    if not isinstance(listen, str):
        raise GateError("embedded launch listen address is malformed")
    if origin_scheme not in ("http", "https"):
        raise GateError("embedded public_origin_scheme must be http or https")
    if not secret_path.is_absolute():
        raise GateError("embedded session_secret_file must be absolute")
    try:
        secret = secret_path.read_text().rstrip("\r\n")
    except OSError as error:
        raise GateError(f"cannot read embedded session secret at {secret_path}") from error
    if not secret:
        raise GateError("embedded session secret is empty")
    return secret, listen, origin_scheme


def _websocket_frame(connection: socket.socket, maximum: int = 8 * 1024 * 1024) -> bytes:
    first, second = _recv_exact(connection, 2)
    opcode = first & 0x0F
    if first & 0x80 == 0 or opcode != 1:
        raise GateError("browser WebSocket did not begin with a complete text snapshot")
    length = second & 0x7F
    if length == 126:
        length = struct.unpack("!H", _recv_exact(connection, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", _recv_exact(connection, 8))[0]
    if second & 0x80 or length > maximum:
        raise GateError("browser WebSocket snapshot frame is masked or exceeds the bound")
    return _recv_exact(connection, length)


def _recv_exact(connection: socket.socket, length: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        part = connection.recv(length - len(chunks))
        if not part:
            raise GateError("browser WebSocket closed before its initial snapshot")
        chunks.extend(part)
    return bytes(chunks)


def validate_idle_snapshot(snapshot: dict, root_actor: dict) -> dict:
    if not isinstance(snapshot, dict) or snapshot.get("type", "snapshot") != "snapshot":
        raise GateError("authenticated WebSocket returned a malformed initial snapshot")
    body = snapshot.get("snapshot")
    if not isinstance(body, dict):
        raise GateError("authenticated WebSocket snapshot has no snapshot object")
    actors = body.get("actors")
    roots = [actor for actor in actors or [] if isinstance(actor, dict)
             and actor.get("parent") is None
             and actor.get("lifecycle") == "waiting"
             and isinstance(actor.get("identity"), dict)
             and actor["identity"].get("incarnation") == str(root_actor["incarnation"])]
    if len(roots) != 1:
        raise GateError("embedded root actor is not present and idle in the authenticated snapshot")
    actor_path = roots[0]["identity"].get("actor")
    if not isinstance(actor_path, str) or not actor_path:
        raise GateError("embedded root actor identity is malformed")
    conversations = body.get("conversations")
    if not isinstance(conversations, list):
        raise GateError("authenticated WebSocket snapshot has malformed conversations")
    conversation = next((item for item in conversations
                         if isinstance(item, dict)
                         and (item.get("id") == actor_path or item.get("path") == actor_path)), None)
    if conversation is None or conversation.get("state") != "idle":
        raise GateError("embedded root conversation is not idle in the authenticated snapshot")
    return snapshot


def authenticated_idle_snapshot(address: str, secret: str, root_actor: dict,
                                origin_scheme: str = "https") -> dict:
    """Authenticate over loopback HTTP, then require the root's idle WS snapshot."""
    try:
        host, port_text = address.rsplit(":", 1)
        host = host.strip("[]")
        port = int(port_text)
        if not socket.getaddrinfo(host, port, type=socket.SOCK_STREAM):
            raise GateError("embedded listener address did not resolve")
        if not ipaddress.ip_address(host).is_loopback:
            raise GateError(f"embedded listener is not loopback: {address}")
    except (ValueError, OSError) as error:
        raise GateError(f"invalid embedded loopback address {address!r}") from error

    origin = f"{origin_scheme}://{address}"
    payload = json.dumps({"secret": secret}, separators=(",", ":")).encode()
    client = http.client.HTTPConnection(host, port, timeout=1.0)
    try:
        client.request("POST", "/api/session", body=payload, headers={
            "Content-Type": "application/json",
            "Origin": origin,
            "Content-Length": str(len(payload)),
        })
        response = client.getresponse()
        response.read()
        if response.status != 200:
            raise GateError(f"embedded session authentication returned HTTP {response.status}")
        set_cookie = response.getheader("Set-Cookie")
        if not set_cookie:
            raise GateError("embedded session authentication returned no session cookie")
        cookie = set_cookie.split(";", 1)[0]
    finally:
        client.close()

    key = secrets.token_bytes(16)
    websocket_key = base64.b64encode(key).decode("ascii")
    connection = socket.create_connection((host, port), timeout=1.0)
    try:
        connection.settimeout(1.0)
        request = (
            f"GET /api/ws HTTP/1.1\r\nHost: {address}\r\n"
            f"Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {websocket_key}\r\nSec-WebSocket-Version: 13\r\n"
            f"Origin: {origin}\r\nCookie: {cookie}\r\n\r\n"
        ).encode("ascii")
        connection.sendall(request)
        response = bytearray()
        while b"\r\n\r\n" not in response:
            part = connection.recv(1)
            if not part or len(response) > 16384:
                raise GateError("embedded WebSocket handshake did not complete")
            response.extend(part)
        headers = bytes(response).split(b"\r\n\r\n", 1)[0].decode("latin1").split("\r\n")
        if not headers[0].startswith("HTTP/1.1 101 "):
            raise GateError(f"authenticated embedded WebSocket rejected with {headers[0]}")
        parsed = {key.strip().lower(): value.strip() for line in headers[1:] if ":" in line
                  for key, value in [line.split(":", 1)]}
        expected = base64.b64encode(
            hashlib.sha1((websocket_key + WEBSOCKET_GUID).encode("ascii")).digest()
        ).decode("ascii")
        if parsed.get("sec-websocket-accept") != expected:
            raise GateError("embedded WebSocket handshake accept key is invalid")
        snapshot = json.loads(_websocket_frame(connection))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise GateError(f"authenticated embedded HTTP/WebSocket probe failed: {error}") from error
    finally:
        connection.close()

    return validate_idle_snapshot(snapshot, root_actor)


def observe_ready(status_path: Path, run_id: str, session: str,
                  probe=authenticated_idle_snapshot) -> tuple[dict, dict]:
    status = parse_status(status_path, run_id, session)
    if status is None:
        raise GateError("run status is absent, malformed, or not ready")
    phase = status["phase"]
    if phase.get("state") != "embedded_ready":
        raise GateError(f"run reached {phase.get('state')}, expected embedded_ready")
    root_actor = phase.get("root_actor")
    address = phase.get("browser_address")
    if not isinstance(root_actor, dict) or not isinstance(root_actor.get("id"), int) \
            or not isinstance(root_actor.get("incarnation"), int) or not isinstance(address, str):
        raise GateError("embedded_ready status has malformed root identity or listener address")
    secret, configured_listener, origin_scheme = embedded_settings(Path(status["workspace"]))
    try:
        configured_host, configured_port = configured_listener.rsplit(":", 1)
        actual_host, actual_port = address.rsplit(":", 1)
        configured_host = configured_host.strip("[]")
        actual_host = actual_host.strip("[]")
        if not ipaddress.ip_address(configured_host).is_loopback \
                or ipaddress.ip_address(configured_host) != ipaddress.ip_address(actual_host) \
                or (int(configured_port) != 0 and int(configured_port) != int(actual_port)):
            raise GateError("embedded_ready listener does not match the frozen loopback configuration")
    except (ValueError, OSError) as error:
        raise GateError("embedded launch configuration has a malformed listener address") from error
    return status, probe(address, secret, root_actor, origin_scheme)


def state_root() -> Path:
    configured = os.environ.get("XDG_STATE_HOME")
    return Path(configured) if configured else Path.home() / ".local" / "state"


def host_pid(run_id: str) -> int:
    result = subprocess.run(
        ["systemctl", "--user", "show", f"exomonad-host-{run_id}.service",
         "--property=MainPID", "--value"], check=True, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5,
    )
    try:
        pid = int(result.stdout.strip())
    except ValueError as error:
        raise GateError("systemd did not report the Exomonad host PID") from error
    if pid <= 0:
        raise GateError("systemd reported no live Exomonad host PID")
    return pid


def retained_host_executable(run_root: Path, pid: int) -> Path:
    expected = (run_root / "bin" / "exomonad").resolve(strict=True)
    actual = Path(os.readlink(f"/proc/{pid}/exe")).resolve(strict=True)
    if actual != expected:
        raise GateError(f"systemd host PID executable mismatch: {actual} != {expected}")
    return expected


def live_executable_sha256(pid: int, expected_path: Path) -> str:
    """Hash the executable inode currently mapped by a live process."""
    proc_path = Path(f"/proc/{pid}/exe")
    actual = Path(os.readlink(proc_path)).resolve(strict=True)
    if actual != expected_path.resolve(strict=True):
        raise GateError(f"live PID executable mismatch: {actual} != {expected_path}")
    return sha256(proc_path)


def live_compiler_identity(boot: dict) -> dict:
    try:
        pid = int(boot["daemon_pid"])
        if pid <= 0:
            raise ValueError("nonpositive daemon pid")
        expected = Path(boot["executable"]).resolve(strict=True)
        worker = Path(boot["worker"]).resolve(strict=True)
    except (KeyError, OSError, TypeError, ValueError) as error:
        raise GateError(f"compiler daemon executable identity is malformed: {error}") from error
    actual = Path(os.readlink(f"/proc/{pid}/exe")).resolve(strict=True)
    if actual != expected:
        raise GateError(f"compiler daemon PID executable mismatch: {actual} != {expected}")
    if not worker.is_file():
        raise GateError(f"compiler worker executable is unavailable: {worker}")
    return {
        "daemon_pid": pid,
        "compiler_executable": str(expected),
        "compiler_executable_sha256": live_executable_sha256(pid, expected),
        "compiler_worker": str(worker),
        "compiler_worker_sha256": sha256(worker),
    }


def reap_init_observer(process: subprocess.Popen, failure: str | None) -> tuple[int | None, str | None]:
    """Reap only the CLI child, terminating it if observation did not finish."""
    exit_code = process.poll()
    if exit_code is not None:
        return exit_code, failure
    if failure is None:
        try:
            return process.wait(timeout=1), failure
        except subprocess.TimeoutExpired:
            failure = "packaged init observer remained live after readiness"
    try:
        process.terminate()
    except ProcessLookupError:
        pass
    try:
        return process.wait(timeout=3), failure
    except subprocess.TimeoutExpired:
        try:
            process.kill()
        except ProcessLookupError:
            pass
        try:
            return process.wait(timeout=3), failure
        except subprocess.TimeoutExpired as error:
            raise GateError("could not reap packaged init observer child") from error


def stop_run(operator: FrozenOperator, run_id: str, session: str, output: Path) -> tuple[list[str], int]:
    completed = operator.run(["stop", "--run-id", run_id, "--session", session],
                             output / "stop.process.json")
    argv = completed.args
    (output / "stop.stdout").write_text(completed.stdout)
    (output / "stop.stderr").write_text(completed.stderr)
    return argv, completed.returncode


def write_report(path: Path, report: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    temporary.chmod(0o600)
    temporary.replace(path)


def run_one(index: int, args, output: Path, report: dict) -> dict:
    session = f"cold-start-{index}-{uuid.uuid4().hex[:12]}"
    arguments = ["init", "--workspace", str(args.workspace), "--session", session, "--no-attach"]
    argv = None
    artifact = output / f"sample-{index}"
    artifact.mkdir()
    cache_path = artifact / "user-cache"
    cache_path.mkdir(mode=0o700)
    stdout_path, stderr_path = artifact / "init.stdout", artifact / "init.stderr"
    started_ns = time.monotonic_ns()
    deadline = started_ns + int(MAX_SAMPLE_SECONDS * 1_000_000_000)
    run_id = None
    process = None
    execution = None
    status = None
    settled_ns = None
    pid = None
    host_path = None
    host_sha256 = None
    daemon_boot = None
    live_compiler = None
    init_exit = None
    stop_argv = None
    stop_exit = None
    failure = None
    with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        try:
            execution = args.operator.launch(arguments, stdout=stdout, stderr=stderr, cache_root=cache_path)
            process, argv = execution.process, execution.command
            started_ns = execution.started_ns
            deadline = started_ns + int(MAX_SAMPLE_SECONDS * 1_000_000_000)
            report["runners"]["cold_start"]["process_execution_count"] += 1
            while time.monotonic_ns() < deadline:
                try:
                    # init writes this durable session pointer before starting the host;
                    # its human-readable status line is printed only after readiness.
                    run_id = read_session_run_id(args.workspace, session)
                except (OSError, UnicodeDecodeError):
                    pass
                if run_id:
                    status_path = state_root() / "tidepool" / "exomonad" / "runs" / run_id / "status.json"
                    status = parse_status(status_path, run_id, session)
                    if status is not None and Path(status.get("workspace", "")).resolve() != args.workspace:
                        raise GateError("run status names a workspace other than the frozen reference")
                    if status is not None and status.get("phase", {}).get("state") == "embedded_ready":
                        try:
                            _, _ = observe_ready(status_path, run_id, session)
                            settled_ns = time.monotonic_ns()
                            if settled_ns > deadline:
                                raise GateError(f"cold start exceeded {MAX_SAMPLE_SECONDS}s")
                            pid = host_pid(run_id)
                            run_root = status_path.parent
                            host_path = retained_host_executable(run_root, pid)
                            host_sha256 = live_executable_sha256(pid, host_path)
                            if host_sha256 != report["runners"]["cold_start"]["binary_sha256"]:
                                raise GateError("live host differs from the frozen native host")
                            trace_path = args.workspace / ".exomonad" / "logs" / f"{run_id}-compiler.jsonl"
                            daemon_boot = compiler_boot(trace_path, run_id)
                            live_compiler = live_compiler_identity(daemon_boot)
                            for selected_key, live_key in (
                                    ("TIDEPOOL_EXTRACT", "compiler_executable_sha256"),
                                    ("TIDEPOOL_EXTRACT_WORKER", "compiler_worker_sha256")):
                                if live_compiler[live_key] != sha256(Path(args.operator.environment[selected_key])):
                                    raise GateError("live compiler differs from the frozen native compiler")
                            init_exit = process.poll()
                            if init_exit not in (None, 0):
                                raise GateError(f"packaged init exited with status {init_exit}")
                            break
                        except (GateError, OSError, subprocess.SubprocessError) as error:
                            # The service can publish status just before its listener accepts.
                            if time.monotonic_ns() >= deadline:
                                raise
                init_exit = process.poll()
                if init_exit not in (None, 0):
                    raise GateError(f"packaged init exited with status {init_exit}")
                time.sleep(POLL_SECONDS)
            else:
                raise GateError(f"cold start exceeded {MAX_SAMPLE_SECONDS}s")
            if settled_ns is None:
                raise GateError("cold start had no readiness timestamp")
        except (Exception, KeyboardInterrupt) as error:  # retain failure and delegate cleanup
            if settled_ns is None:
                settled_ns = time.monotonic_ns()
            failure = f"{type(error).__name__}: {error}"
        finally:
            if process is not None:
                try:
                    init_exit, failure = reap_init_observer(process, failure)
                except (GateError, OSError, subprocess.SubprocessError) as error:
                    failure = failure or f"init observer cleanup failed: {error}"
                try:
                    execution.record(artifact / "init.process.json")
                except OSError as error:
                    failure = failure or f"init process receipt unavailable: {error}"

    # A failed init can write run-id and exit between polling iterations. Recover
    # that durable identity before deciding whether the package must retire a run.
    if run_id is None:
        try:
            run_id = read_session_run_id(args.workspace, session)
        except GateError as error:
            failure = failure or f"{type(error).__name__}: {error}"

    sample = {
        "schema": 1,
        "composition": "engine-store",
        "kind": "cold_start",
        "index": index,
        "runner_id": "cold_start",
        "run_id": run_id,
        "session": session,
        "command": argv,
        "init_exit_code": init_exit,
        "started_ns": started_ns,
        "settled_ns": settled_ns,
        "elapsed_ns": max(0, settled_ns - started_ns),
        "readiness": "workspace-ready" if failure is None else None,
        "provider_requests": None,
        "provider_request_evidence": None,
        "host_pid": pid,
        "actual_retained_host_executable": str(host_path) if host_path else None,
        "host_sha256": host_sha256,
        "daemon_epoch": None,
        "daemon_trace": None,
        "packaged": True,
        "user_cache": str(cache_path),
        "output": str(artifact),
        "failure": failure,
    }
    if sample["failure"] is None and init_exit != 0:
        sample["failure"] = f"packaged init did not exit successfully (status {init_exit})"
    if daemon_boot is not None and live_compiler is not None:
        sample["daemon_epoch"] = daemon_boot["daemon_epoch"]
        sample.update(live_compiler)
        sample["compiler_producer"] = daemon_boot["producer"]
    if run_id:
        run_root = state_root() / "tidepool" / "exomonad" / "runs" / run_id
        trace_path = args.workspace / ".exomonad" / "logs" / f"{run_id}-compiler.jsonl"
        sample["daemon_trace"] = str(trace_path)
        sample["run_root"] = str(run_root)
        sample["run_log"] = str(args.workspace / ".exomonad" / "logs" / f"{run_id}.log")
        sample["status_path"] = str(run_root / "status.json")
    if run_id:
        try:
            stop_argv, stop_exit = stop_run(args.operator, run_id, session, artifact)
            sample["stop_command"] = stop_argv
            sample["stop_exit_code"] = stop_exit
            if stop_exit != 0:
                sample["failure"] = sample["failure"] or f"packaged exomonad stop exited with status {stop_exit}"
        except (GateError, OSError, ValueError, subprocess.SubprocessError) as error:
            sample["stop_command"] = None
            sample["stop_exit_code"] = None
            sample["failure"] = sample["failure"] or f"packaged exomonad stop failed: {error}"
        store_path = run_root / "harness" / "store.sqlite"
        try:
            evidence = provider_store_evidence(store_path, args.harness_store_schema)
            sample["provider_request_evidence"] = evidence
            sample["provider_requests"] = evidence["provider_request_attempts"]
            if sample["provider_requests"] != 0:
                sample["failure"] = sample["failure"] or (
                    f"embedded harness recorded {sample['provider_requests']} provider request attempts"
                )
        except (GateError, OSError, ValueError) as error:
            sample["provider_request_evidence"] = {
                "verified": False,
                "store_path": str(store_path),
                "error": f"{type(error).__name__}: {error}",
            }
            sample["failure"] = sample["failure"] or f"provider request evidence unavailable: {error}"
    else:
        sample["stop_command"] = None
        sample["stop_exit_code"] = None
    sample["init_stdout"] = str(stdout_path)
    sample["init_stderr"] = str(stderr_path)
    sample = seal_sample(sample)
    if sample["failure"] is not None:
        sample["readiness"] = None
        report["samples"].append(sample)
        write_report(output / "report.json", report)
        raise GateError(f"sample {index} failed: {sample['failure']}; evidence: {artifact}")
    return sample


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--descriptor", required=True, type=Path)
    parser.add_argument("--workspace", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    args.workspace = args.workspace.resolve(strict=True)
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False, mode=0o700)
    runner = {"command": [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
              "exit_code": None, "expected_test_count": 0, "executed_test_count": 0,
              "process_execution_count": 0}
    report = {"schema": 3, "runners": {"cold_start": runner}, "samples": [], "failure": None}
    report_path = args.output / "report.json"
    write_report(report_path, report)
    try:
        args.operator = FrozenOperator(args.descriptor)
        runner["native_qualification"] = args.operator.provenance()
        selected = args.operator.descriptor["programs"]
        runner.update(binary_path=selected["host_elf"], binary_sha256=sha256(Path(selected["host_elf"])),
                      package_entrypoint=selected["host"], package_entrypoint_sha256=sha256(Path(selected["host"])))
        # Validate the required native loopback fixture before starting any host.
        embedded_settings(args.workspace)
        runner["harness_store_schema"] = args.operator.store_schema(args.output)
        args.harness_store_schema = runner["harness_store_schema"]["version"]
        write_report(report_path, report)
        for index in range(SAMPLE_COUNT):
            sample = run_one(index, args, args.output, report)
            report["samples"].append(sample)
            write_report(report_path, report)
        runner["exit_code"] = 0
        write_report(report_path, report)
        return 0
    except Exception as error:
        report["failure"] = f"{type(error).__name__}: {error}"
        runner["exit_code"] = 1
        write_report(report_path, report)
        print(f"cold-start gate failed; retained evidence: {report_path}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
