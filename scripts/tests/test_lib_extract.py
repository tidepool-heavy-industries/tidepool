#!/usr/bin/env python3
"""Focused process-level contracts for the shared shell toolchain helpers."""

import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import time
import unittest


LIBRARY = Path(__file__).resolve().parents[1] / "lib-extract.sh"
FRONTEND = r'''#!/usr/bin/env python3
import json, os, signal, socket, sys, time
from pathlib import Path
if len(sys.argv) == 1:
    print("Usage: tidepool-extract", file=sys.stderr)
    sys.exit(2)
if sys.argv[1] == "--compiler-endpoint-v1":
    if os.environ.get("BAD_ENDPOINT"):
        print("worker unavailable", file=sys.stderr)
        sys.exit(2)
    # PRODUCER_BYTE parameterizes the fake producer identity so tests can
    # simulate a rebuild (a changed producer) between two probes.
    producer_byte = int(os.environ.get("PRODUCER_BYTE", "0"))
    sys.stdout.buffer.write(b"TPCID002" + bytes([producer_byte]) * 64)
    sys.exit(2)  # EOF is deliberately not a complete compiler request.
if sys.argv[1] == "--stop-daemon":
    # Mirrors tidepool-extract's real --stop-daemon mode: send the wire's
    # STOP tag and wait (briefly) for the daemon's one-byte ack. No daemon
    # listening is success too — the desired end state already holds.
    stop_sock = sys.argv[sys.argv.index("--socket") + 1]
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(5)
    try:
        client.connect(stop_sock)
    except OSError:
        sys.exit(0)
    client.sendall(b"TPDST001")
    try:
        client.recv(1)
    except OSError:
        pass
    sys.exit(0)
assert sys.argv[1] == "--daemon"
Path(os.environ["DAEMON_PID_FILE"]).write_text(str(os.getpid()))
Path(os.environ["DAEMON_PID_FILE"] + ".argv").write_text("\n".join(sys.argv[1:]))
if "--log-path" in sys.argv:
    log_path = Path(sys.argv[sys.argv.index("--log-path") + 1])
    log_path.write_text("compile timing fixture\n")
mode = os.environ.get("DAEMON_MODE", "ready")
if mode == "exit":
    print("daemon startup failure", file=sys.stderr)
    sys.exit(2)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
if mode == "hang":
    while True:
        time.sleep(0.01)
daemon_sock_path = sys.argv[sys.argv.index("--socket") + 1]
if "--log-path" in sys.argv:
    producer_byte = int(os.environ.get("PRODUCER_BYTE", "0"))
    ready_pid = os.getpid() + (1 if os.environ.get("BAD_READY_PID") else 0)
    ready_producer = ("01" if os.environ.get("BAD_READY_PRODUCER") else f"{producer_byte:02x}") * 32
    ready = {"fields": {"message": "compiler daemon ready", "producer": ready_producer,
                        "daemon_pid": ready_pid, "daemon_epoch": "ab" * 32}}
    ready_trace = Path(sys.argv[sys.argv.index("--log-path") + 1]).with_suffix(".jsonl")
    ready_lines = [json.dumps(ready)]
    if os.environ.get("DUP_READY_IDENTITY"):
        ready_lines.append(json.dumps(ready))
    ready_trace.write_text("\n".join(ready_lines) + "\n")
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.bind(daemon_sock_path)
sock.listen()
while True:
    conn, _ = sock.accept()
    header = conn.recv(8)
    if header == b"TPDST001":
        # The real daemon acks before retiring its socket and exiting its
        # accept loop; this fake mirrors that so daemon_stop_persistent's
        # wait-for-exit can observe an orderly shutdown.
        conn.sendall(b"\x01")
        conn.close()
        sock.close()
        os.unlink(daemon_sock_path)
        sys.exit(0)
    conn.close()
'''


class ExtractHelpers(unittest.TestCase):
    def test_owned_daemon_keeps_endpoint_through_worker_rotation(self):
        self.run_shell('trap teardown_compile_daemon EXIT\nstart_compile_daemon',
                       TIDEPOOL_EXTRACT=str(self.frontend))
        self.assertIn("--persistent", (self.root / "daemon.pid.argv").read_text().splitlines())

    def setUp(self):
        # A short base keeps fixture socket paths under the AF_UNIX limit even
        # when the dev shell's TMPDIR is deeply nested.
        self.temp = tempfile.TemporaryDirectory(prefix="extract helpers ",
                                                dir="/tmp" if os.path.isdir("/tmp") else None)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "bin").mkdir()
        self.frontend = self.executable("frontend", FRONTEND)
        self.worker = self.executable("worker", "#!/bin/sh\nexit 0\n")
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("TIDEPOOL_", "CARGO_TARGET_DIR"))}
        self.env.update(PATH=f"{self.root / 'bin'}:{os.environ['PATH']}",
                        DAEMON_PID_FILE=str(self.root / "daemon.pid"),
                        TMPDIR=str(self.root),
                        TIDEPOOL_GHC_LIBDIR="/ghc/lib",
                        # Never observe the host's persistent compile daemon.
                        XDG_CACHE_HOME=str(self.root / "cache"))

    def executable(self, name, source):
        path = self.root / "bin" / name
        path.write_text(source)
        path.chmod(0o755)
        return path

    def run_shell(self, body, *, success=True, **env):
        result = subprocess.run(
            ["bash", "-euo", "pipefail", "-c", f'source "{LIBRARY}"\n{body}'],
            env=self.env | env, cwd=self.root, text=True, capture_output=True, timeout=20)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def assert_daemon_reaped(self):
        pid_file = self.root / "daemon.pid"
        self.assertTrue(pid_file.exists())
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pid_file.read_text()), 0)


    def test_native_bundle_selection_preserves_quoted_owner_values(self):
        bundle = self.root / "frozen bundle"
        shared = bundle / "share/exomonad"
        shared.mkdir(parents=True)
        owner = shared / "qualification.py"
        value = "test 'quoted'\n$(touch should-not-exist)"
        owner.write_text("import sys\n"
                         "assert sys.argv[1:] == ['environment', " + repr(str(shared / 'qualification.json')) + ", '--shell']\n"
                         + "print(" + repr("export TIDEPOOL_NATIVE_LIBTEST=" + shlex.quote(value)) + ")\n")
        descriptor = shared / 'qualification.json'
        descriptor.write_text('{}\n')
        result = self.run_shell("select_native_bundle " + shlex.quote(str(descriptor))
                                + "\nprintf '%s' \"$TIDEPOOL_NATIVE_LIBTEST\"")
        self.assertEqual(result.stdout, value)
        self.assertFalse((self.root / "should-not-exist").exists())

    def test_unverified_bundle_does_not_apply_environment_or_launch(self):
        bundle = self.root / "rejected bundle"
        shared = bundle / "share/exomonad"
        shared.mkdir(parents=True)
        owner = shared / "qualification.py"
        owner.write_text("import sys\nprint('touch should-not-exist')\nsys.exit(17)\n")
        descriptor = shared / 'qualification.json'
        descriptor.write_text('{}\n')
        self.run_shell("select_native_bundle " + shlex.quote(str(descriptor))
                       + "\ntouch should-not-launch", success=False)
        self.assertFalse((self.root / "should-not-exist").exists())
        self.assertFalse((self.root / "should-not-launch").exists())

    def test_descriptor_selects_its_sibling_owner_and_rejects_legacy_pair(self):
        first = self.root / 'first'
        second = self.root / 'second'
        for bundle, value in ((first, 'first'), (second, 'second')):
            shared = bundle / 'share/exomonad'
            shared.mkdir(parents=True)
            (shared / 'qualification.json').write_text('{}\n')
            (shared / 'qualification.py').write_text(
                "import sys\nassert sys.argv[1] == 'environment'\n"
                "print('export SELECTED_OWNER=" + value + "')\n")
        descriptor = second / 'share/exomonad/qualification.json'
        result = self.run_shell("select_native_bundle " + shlex.quote(str(descriptor))
                                + "\nprintf '%s' \"$SELECTED_OWNER\"")
        self.assertEqual(result.stdout, 'second')
        result = self.run_shell("select_native_bundle " + shlex.quote(str(first)) + " "
                                + shlex.quote(str(descriptor)), success=False)
        self.assertIn('accepts only a qualification descriptor', result.stderr)

    def delegated_fixture(self):
        bundle = self.root / 'frozen native bundle'
        shared = bundle / 'share/exomonad'
        shared.mkdir(parents=True)
        selected = {
            'TIDEPOOL_EXTRACT': str(self.frontend),
            'TIDEPOOL_EXTRACT_WORKER': str(self.worker),
            'TIDEPOOL_NATIVE_LIBTEST': str(bundle / 'bin/tidepool-tests'),
        }
        exports = '\n'.join('export ' + key + '=' + shlex.quote(value)
                            for key, value in selected.items())
        (shared / 'qualification.py').write_text('print(' + repr(exports) + ')\n')
        (shared / 'isolated-libtest.py').write_text(
            "import sys\nassert '--delegated-service' in sys.argv\n"
            "assert '--expected-count' in sys.argv and '1' in sys.argv\nraise SystemExit(7)\n")
        script = LIBRARY.parents[1] / 'exomonad/scripts/test-embedded-command-delegated.sh'
        return script, shared / 'qualification.json', self.root / 'native evidence'

    def test_native_delegated_wrapper_failure_retains_logs_and_reaps_daemon(self):
        script, descriptor, output = self.delegated_fixture()
        result = subprocess.run(['bash', str(script), str(descriptor), str(output)],
                                env=self.env, text=True, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
        self.assertTrue((output / 'compiler/daemon.log').exists())
        self.assertEqual(list(self.root.glob('tidepool-extract-daemon.*')), [])
        self.assert_daemon_reaped()

    def test_native_delegated_wrapper_signal_during_startup_reaps_daemon(self):
        script, descriptor, output = self.delegated_fixture()
        process = subprocess.Popen(['bash', str(script), str(descriptor), str(output)],
                                   env=self.env | {'DAEMON_MODE': 'hang'},
                                   text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 5
            while not (self.root / 'daemon.pid').exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue((self.root / 'daemon.pid').exists())
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 143, stdout + stderr)
            self.assertTrue((output / 'compiler/daemon.log').exists())
            self.assert_daemon_reaped()
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=5)

    def test_successful_start_and_teardown(self):
        self.run_shell('trap teardown_compile_daemon EXIT\nstart_compile_daemon\n'
                       '[[ "$COMPILE_DAEMON_OWNED" = 1 ]]\n'
                       'saved_dir="$COMPILE_DAEMON_SOCKET_DIR"\n'
                       'teardown_compile_daemon\n[[ ! -d "$saved_dir" ]]\n'
                       '[[ -z "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ]]',
                       TIDEPOOL_EXTRACT=str(self.frontend))
        self.assert_daemon_reaped()

    def test_failed_start_validates_fallback_and_retains_log(self):
        result = self.run_shell('trap teardown_compile_daemon EXIT\nstart_compile_daemon\n'
                                '[[ "$COMPILE_DAEMON_START_FAILED" = 1 ]]\n'
                                '[[ -z "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ]]',
                                TIDEPOOL_EXTRACT=str(self.frontend), DAEMON_MODE="exit",
                                TIDEPOOL_EXTRACT_DAEMON_SOCKET="/stale")
        self.assertIn("direct compiler endpoint validated", result.stderr)
        logs = list(self.root.glob("tidepool-extract-daemon.*/daemon.log"))
        self.assertEqual(len(logs), 1)
        self.assertIn("daemon startup failure", logs[0].read_text())
        self.assert_daemon_reaped()

    def test_failed_start_and_unusable_fallback_fail(self):
        result = self.run_shell('trap teardown_compile_daemon EXIT\nstart_compile_daemon',
                                success=False, TIDEPOOL_EXTRACT=str(self.frontend),
                                DAEMON_MODE="exit", BAD_ENDPOINT="1")
        self.assertIn("could not bind a direct compiler endpoint", result.stderr)
        self.assertNotIn("direct compiler endpoint validated", result.stderr)
        self.assert_daemon_reaped()

    def test_elapsed_timeout_reaps_starting_process(self):
        # Advance Bash's elapsed clock after the fixture has started, without
        # spending 30 seconds testing the fixed deadline.
        result = self.run_shell('sleep() { command sleep 0.1; SECONDS=$((SECONDS + 31)); }\n'
                                'trap teardown_compile_daemon EXIT\nstart_compile_daemon',
                                TIDEPOOL_EXTRACT=str(self.frontend), DAEMON_MODE="hang")
        self.assertIn("not ready within 30s", result.stderr)
        self.assertIn("direct compiler endpoint validated", result.stderr)
        self.assert_daemon_reaped()

    def test_inherited_daemon_is_not_owned_or_removed(self):
        socket = self.root / "outer.sock"
        proc = subprocess.Popen([str(self.frontend), "--daemon", "--socket", str(socket)], env=self.env)
        self.addCleanup(lambda: proc.poll() is None and proc.kill())
        self.addCleanup(lambda: proc.poll() is None and proc.terminate())
        try:
            import time
            for _ in range(100):
                if socket.exists():
                    break
                time.sleep(0.01)
            self.run_shell('start_compile_daemon\n[[ "$COMPILE_DAEMON_OWNED" = 0 ]]\n'
                           'teardown_compile_daemon\n[[ -S "$TIDEPOOL_EXTRACT_DAEMON_SOCKET" ]]',
                           TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_DAEMON_SOCKET=str(socket))
            self.assertIsNone(proc.poll())
        finally:
            proc.terminate()
            proc.wait(timeout=5)

    def test_disabled_daemon_clears_inherited_socket_and_checks_direct(self):
        result = self.run_shell('start_compile_daemon\n[[ -z "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ]]',
                                TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_NO_DAEMON="1",
                                TIDEPOOL_EXTRACT_DAEMON_SOCKET="/inherited")
        self.assertIn("daemon disabled", result.stderr)
        self.assertFalse((self.root / "daemon.pid").exists())
        self.run_shell("start_compile_daemon", success=False,
                       TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_NO_DAEMON="1",
                       BAD_ENDPOINT="1")

    def test_measurement_mode_owns_daemon_and_retains_trace_after_stop(self):
        result = self.run_shell(
            'trap teardown_compile_daemon EXIT\n'
            'start_compile_daemon\n'
            '[[ "$COMPILE_DAEMON_OWNED" = 1 ]]\n'
            '[[ -f "$TIDEPOOL_PERFORMANCE_COMPILER_TRACE" ]]\n'
            'teardown_compile_daemon --preserve-logs\n'
            'printf "%s\n" "$TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID" '
            '"$TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER" '
            '"$TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH"\n'
            'cat "$TIDEPOOL_PERFORMANCE_COMPILER_TRACE"',
            TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_MEASUREMENT="1")
        self.assertIn("measurement daemon identity", result.stderr)
        self.assertIn("00" * 32, result.stdout)
        self.assertIn("ab" * 32, result.stdout)
        self.assertIn('"compiler daemon ready"', result.stdout)
        self.assertIn((self.root / "daemon.pid").read_text(), result.stdout)
        self.assertEqual(list(self.root.glob("tidepool-extract-daemon.*")), [])
        self.assert_daemon_reaped()

    def test_measurement_mode_rejects_inherited_socket_and_direct_mode(self):
        for env in ({"TIDEPOOL_EXTRACT_DAEMON_SOCKET": "/inherited"},
                    {"TIDEPOOL_EXTRACT_NO_DAEMON": "1"}):
            result = self.run_shell("start_compile_daemon", success=False,
                                    TIDEPOOL_EXTRACT=str(self.frontend),
                                    TIDEPOOL_EXTRACT_MEASUREMENT="1", **env)
            self.assertIn("measurement mode", result.stderr)
            self.assertFalse((self.root / "daemon.pid").exists())

    def test_measurement_mode_does_not_adopt_live_persistent_daemon(self):
        env = self.persistent_env(TIDEPOOL_EXTRACT_MEASUREMENT="1")
        self.run_shell("daemon_start_persistent", **self.persistent_env())
        self.addCleanup(lambda: self.run_shell("daemon_stop_persistent", **self.persistent_env()))
        persistent_pid = int((self.persistent_dir() / "daemon.pid").read_text())
        result = self.run_shell(
            'trap teardown_compile_daemon EXIT\nstart_compile_daemon\n'
            'printf "OWNED=%s\\n" "$COMPILE_DAEMON_OWNED"',
            **env)
        self.assertIn("OWNED=1", result.stdout)
        self.assertIn("measurement daemon identity", result.stderr)
        os.kill(persistent_pid, 0)

    def test_measurement_mode_never_falls_back_or_accepts_mismatched_identity(self):
        result = self.run_shell("start_compile_daemon", success=False,
                                TIDEPOOL_EXTRACT=str(self.frontend),
                                TIDEPOOL_EXTRACT_MEASUREMENT="1", DAEMON_MODE="exit")
        self.assertIn("direct fallback is forbidden", result.stderr)
        self.assertNotIn("direct compiler endpoint validated", result.stderr)
        result = self.run_shell("start_compile_daemon", success=False,
                                TIDEPOOL_EXTRACT=str(self.frontend),
                                TIDEPOOL_EXTRACT_MEASUREMENT="1", BAD_READY_PID="1")
        self.assertIn("did not publish matching producer/pid/epoch evidence", result.stderr)
        self.assertNotIn("direct compiler endpoint validated", result.stderr)
        result = self.run_shell("start_compile_daemon", success=False,
                                TIDEPOOL_EXTRACT=str(self.frontend),
                                TIDEPOOL_EXTRACT_MEASUREMENT="1", DUP_READY_IDENTITY="1")
        self.assertIn("did not publish matching producer/pid/epoch evidence", result.stderr)
        self.assertNotIn("direct compiler endpoint validated", result.stderr)


    def test_timed_out_endpoint_is_rejected_even_after_identity(self):
        self.executable("timeout", '#!/usr/bin/env python3\nimport sys\n'
                        'sys.stdout.buffer.write(b"TPCID002" + bytes(64))\nsys.exit(124)\n')
        self.run_shell("validate_tidepool_extract_endpoint", success=False,
                       TIDEPOOL_EXTRACT=str(self.frontend))


    # --- Persistent compile daemon (daemon_start_persistent / daemon_stop_persistent) ---

    def persistent_env(self, **extra):
        cache = self.root / "cache"
        return dict(TIDEPOOL_EXTRACT=str(self.frontend), XDG_CACHE_HOME=str(cache), **extra)

    def persistent_dir(self):
        return self.root / "cache/tidepool/battery-daemon"

    def test_daemon_start_persistent_creates_state_and_prints_export(self):
        env = self.persistent_env()
        result = self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        sock = daemon_dir / "extract.sock"
        self.assertIn(f"export TIDEPOOL_EXTRACT_DAEMON_SOCKET={sock}", result.stdout)
        self.assertTrue((daemon_dir / "daemon.pid").exists())
        self.assertTrue(sock.exists())
        self.assertEqual((daemon_dir / "producer").read_text().strip(), "00" * 32)
        os.kill(int((daemon_dir / "daemon.pid").read_text()), 0)

    def test_daemon_start_persistent_is_idempotent(self):
        env = self.persistent_env()
        result1 = self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        sock = daemon_dir / "extract.sock"
        self.assertIn(f"export TIDEPOOL_EXTRACT_DAEMON_SOCKET={sock}", result1.stdout)
        pid1 = (daemon_dir / "daemon.pid").read_text()
        result2 = self.run_shell('daemon_start_persistent', **env)
        self.assertIn("already running", result2.stderr)
        self.assertIn(f"export TIDEPOOL_EXTRACT_DAEMON_SOCKET={sock}", result2.stdout)
        self.assertEqual((daemon_dir / "daemon.pid").read_text(), pid1)

    def test_daemon_start_persistent_cleans_dead_state_and_restarts(self):
        env = self.persistent_env()
        daemon_dir = self.persistent_dir()
        daemon_dir.mkdir(parents=True)
        (daemon_dir / "daemon.pid").write_text("999999")
        (daemon_dir / "extract.sock").write_text("stale, not a real socket")
        (daemon_dir / "producer").write_text("deadbeef")
        result = self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        self.assertIn("clearing stale persistent compile daemon state", result.stderr)
        pid = int((daemon_dir / "daemon.pid").read_text())
        self.assertNotEqual(pid, 999999)
        os.kill(pid, 0)

    def test_daemon_start_persistent_restarts_on_producer_change(self):
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        pid1 = int((daemon_dir / "daemon.pid").read_text())
        result = self.run_shell('daemon_start_persistent', **env, PRODUCER_BYTE="1")
        self.assertIn("is stale (producer changed) — restarting", result.stderr)
        pid2 = int((daemon_dir / "daemon.pid").read_text())
        self.assertNotEqual(pid1, pid2)
        self.assertEqual((daemon_dir / "producer").read_text().strip(), "01" * 32)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid1, 0)
        os.kill(pid2, 0)

    def test_daemon_stop_persistent_reaps_and_cleans(self):
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        daemon_dir = self.persistent_dir()
        pid = int((daemon_dir / "daemon.pid").read_text())
        self.run_shell('daemon_stop_persistent', **env)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        self.assertFalse((daemon_dir / "extract.sock").exists())
        self.assertFalse((daemon_dir / "daemon.pid").exists())
        self.assertFalse((daemon_dir / "producer").exists())
        # Quiet no-op when nothing is running.
        result = self.run_shell('daemon_stop_persistent', **env)
        self.assertEqual(result.returncode, 0)

    def test_daemon_stop_persistent_uses_the_graceful_stop_daemon_path(self):
        # The fake daemon's "ready" loop only exits on the wire's STOP tag
        # (see FRONTEND above) — it ignores SIGTERM's default disposition by
        # installing its own handler that also just exits cleanly, so this
        # only passes if daemon_stop_persistent actually drove the
        # `--stop-daemon` CLI mode rather than falling straight through to
        # signaling.
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        daemon_dir = self.persistent_dir()
        self.assertEqual((daemon_dir / "daemon.exe").read_text().strip(), str(self.frontend))
        pid = int((daemon_dir / "daemon.pid").read_text())
        result = self.run_shell('daemon_stop_persistent', **env)
        self.assertIn("requesting graceful stop", result.stderr)
        self.assertNotIn("falling back to termination", result.stderr)
        self.assertIn("stopped gracefully", result.stderr)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        self.assertFalse((daemon_dir / "extract.sock").exists())
        self.assertFalse((daemon_dir / "daemon.pid").exists())
        self.assertFalse((daemon_dir / "daemon.exe").exists())

    def test_daemon_stop_persistent_falls_back_without_a_recorded_binary(self):
        # State from before daemon.exe existed (or a caller that never went
        # through daemon_start_persistent): no recorded binary to ask
        # gracefully, so this must still reap the process via the ordinary
        # signal escalation path, quietly.
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        daemon_dir = self.persistent_dir()
        pid = int((daemon_dir / "daemon.pid").read_text())
        (daemon_dir / "daemon.exe").unlink()
        result = self.run_shell('daemon_stop_persistent', **env)
        self.assertNotIn("requesting graceful stop", result.stderr)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        self.assertFalse((daemon_dir / "extract.sock").exists())
        self.assertFalse((daemon_dir / "daemon.pid").exists())

    def test_start_compile_daemon_reuses_current_persistent_daemon(self):
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        pid_before = (daemon_dir / "daemon.pid").read_text()
        sock = str(daemon_dir / "extract.sock")
        result = self.run_shell(
            'trap teardown_compile_daemon EXIT\nstart_compile_daemon\n'
            'printf "OWNED=%s\\n" "$COMPILE_DAEMON_OWNED"\n'
            'printf "SOCK=%s\\n" "$TIDEPOOL_EXTRACT_DAEMON_SOCKET"',
            **env)
        self.assertIn(f"reusing persistent compile daemon at {sock}", result.stderr)
        self.assertIn("OWNED=0", result.stdout)
        self.assertIn(f"SOCK={sock}", result.stdout)
        pid_after = (daemon_dir / "daemon.pid").read_text()
        self.assertEqual(pid_before, pid_after)
        os.kill(int(pid_after), 0)

    def test_start_compile_daemon_skips_stale_persistent_daemon(self):
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        persistent_sock = str(daemon_dir / "extract.sock")
        result = self.run_shell(
            'trap teardown_compile_daemon EXIT\nstart_compile_daemon\n'
            'printf "OWNED=%s\\n" "$COMPILE_DAEMON_OWNED"\n'
            'printf "SOCK=%s\\n" "$TIDEPOOL_EXTRACT_DAEMON_SOCKET"',
            **env, PRODUCER_BYTE="1")
        self.assertIn(f"persistent compile daemon at {persistent_sock} is stale (producer mismatch)",
                     result.stderr)
        self.assertIn("just daemon-stop && just daemon-start", result.stderr)
        self.assertIn("OWNED=1", result.stdout)
        self.assertNotIn(f"SOCK={persistent_sock}", result.stdout)
        # The persistent daemon itself is left running untouched.
        os.kill(int((daemon_dir / "daemon.pid").read_text()), 0)


if __name__ == "__main__":
    unittest.main()
