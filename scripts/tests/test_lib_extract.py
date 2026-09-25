#!/usr/bin/env python3
"""Focused process-level contracts for the shared shell toolchain helpers."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


LIBRARY = Path(__file__).resolve().parents[1] / "lib-extract.sh"
FRONTEND = r'''#!/usr/bin/env python3
import os, signal, socket, sys, time
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
    sys.stdout.buffer.write(b"TPCID001" + bytes([producer_byte]) * 32)
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
mode = os.environ.get("DAEMON_MODE", "ready")
if mode == "exit":
    print("daemon startup failure", file=sys.stderr)
    sys.exit(2)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
if mode == "hang":
    while True:
        time.sleep(0.01)
daemon_sock_path = sys.argv[sys.argv.index("--socket") + 1]
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
CARGO = r'''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
assert sys.argv[1:] == ["build", "-p", "tidepool-extract-cmd", "--bin",
                       "tidepool-extract", "--message-format=json-render-diagnostics"]
if os.environ.get("FAIL_CARGO"):
    sys.exit(7)
target = Path(os.environ.get("CARGO_TARGET_DIR", "target")).resolve()
binary = target / "debug" / "tidepool-extract"
binary.parent.mkdir(parents=True, exist_ok=True)
binary.write_bytes(Path(os.environ["FRONTEND_FIXTURE"]).read_bytes())
binary.chmod(0o755)
print(json.dumps({"reason": "compiler-artifact", "target": {"name": "tidepool-extract"},
                  "executable": str(binary), "fresh": True}))
'''


class ExtractHelpers(unittest.TestCase):
    def test_owned_daemon_keeps_endpoint_through_worker_rotation(self):
        self.run_shell('trap teardown_battery_daemon EXIT\nstart_battery_daemon',
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
        for directory in ("bridge/haskell/src", "bridge/haskell/app", "bridge/haskell/lib/Tidepool/Aeson",
                          "bridge/haskell/lib/Tidepool/Command", "bridge/haskell/lib/Tidepool/Data",
                          "tidepool/extract-cmd/src"):
            (self.root / directory).mkdir(parents=True)
        for source in ("bridge/haskell/lib/Tidepool/Aeson/Scientific.hs",
                       "bridge/haskell/lib/Tidepool/Aeson/Value.hs",
                       "bridge/haskell/lib/Tidepool/Command/Types.hs",
                       "bridge/haskell/lib/Tidepool/Data/Time.hs",
                       "bridge/haskell/lib/Tidepool/Double.hs"):
            (self.root / source).touch()
        for manifest in ("bridge/haskell/tidepool-extract.cabal", "tidepool/extract-cmd/Cargo.toml"):
            (self.root / manifest).touch()
        self.frontend = self.executable("frontend", FRONTEND)
        self.worker = self.executable("worker", "#!/bin/sh\nexit 0\n")
        self.executable("cargo", CARGO)
        self.executable("ghc-pkg", "#!/bin/sh\necho lens-5.0\n")
        self.executable("cabal", '#!/bin/sh\n[ "$1" != list-bin ] || printf "%s\\n" "$TEST_WORKER"\n')
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("TIDEPOOL_", "CARGO_TARGET_DIR"))}
        self.env.update(PATH=f"{self.root / 'bin'}:{os.environ['PATH']}",
                        FRONTEND_FIXTURE=str(self.frontend), TEST_WORKER=str(self.worker),
                        DAEMON_PID_FILE=str(self.root / "daemon.pid"),
                        TIDEPOOL_ALLOW_STALE_EXTRACT="1", TMPDIR=str(self.root),
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

    def test_cargo_artifact_locations_and_matched_worker(self):
        for target in (None, "relative target", str(self.root / "absolute target")):
            with self.subTest(target=target):
                env = {} if target is None else {"CARGO_TARGET_DIR": target}
                result = self.run_shell('resolve_tidepool_extract\nprintf "worker=%s\\n" "$TIDEPOOL_EXTRACT_WORKER"', **env)
                expected = (self.root / (target or "target") / "debug/tidepool-extract").resolve()
                self.assertIn(f"TIDEPOOL_EXTRACT={expected}", result.stdout)
                self.assertIn(f"worker={self.worker}", result.stdout)

    def test_cargo_failure_is_not_hidden(self):
        self.run_shell("resolve_tidepool_extract", success=False, FAIL_CARGO="1")

    def test_override_is_preserved_and_invalid_binaries_fail(self):
        result = self.run_shell("resolve_tidepool_extract", TIDEPOOL_EXTRACT=str(self.frontend), FAIL_CARGO="1")
        self.assertIn(f"TIDEPOOL_EXTRACT={self.frontend}", result.stdout)
        self.run_shell("resolve_tidepool_extract", success=False, TIDEPOOL_EXTRACT="/missing")
        self.run_shell("resolve_tidepool_extract", success=False,
                       TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_WORKER="/missing")
        self.run_shell("resolve_tidepool_extract", success=False, TIDEPOOL_EXTRACT=str(self.worker))

    def test_preset_worker_rejects_newer_embedded_authority_source(self):
        authority = self.root / "bridge/haskell/lib/Tidepool/Aeson/Value.hs"
        os.utime(self.worker, (1_700_000_000, 1_700_000_000))
        os.utime(authority, (1_700_000_100, 1_700_000_100))
        result = self.run_shell(
            "unset TIDEPOOL_ALLOW_STALE_EXTRACT\nresolve_tidepool_extract",
            success=False, TIDEPOOL_EXTRACT=str(self.frontend),
            TIDEPOOL_EXTRACT_WORKER=str(self.worker))
        self.assertIn("older than Haskell worker sources", result.stderr)

    def test_successful_start_and_teardown(self):
        self.run_shell('trap teardown_battery_daemon EXIT\nstart_battery_daemon\n'
                       '[[ "$BATTERY_DAEMON_OWNED" = 1 ]]\n'
                       'saved_dir="$BATTERY_DAEMON_SOCKET_DIR"\n'
                       'teardown_battery_daemon\n[[ ! -d "$saved_dir" ]]\n'
                       '[[ -z "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ]]',
                       TIDEPOOL_EXTRACT=str(self.frontend))
        self.assert_daemon_reaped()

    def test_failed_start_validates_fallback_and_retains_log(self):
        result = self.run_shell('trap teardown_battery_daemon EXIT\nstart_battery_daemon\n'
                                '[[ "$BATTERY_DAEMON_START_FAILED" = 1 ]]\n'
                                '[[ -z "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ]]',
                                TIDEPOOL_EXTRACT=str(self.frontend), DAEMON_MODE="exit",
                                TIDEPOOL_EXTRACT_DAEMON_SOCKET="/stale")
        self.assertIn("direct compiler endpoint validated", result.stderr)
        logs = list(self.root.glob("tidepool-extract-daemon.*/daemon.log"))
        self.assertEqual(len(logs), 1)
        self.assertIn("daemon startup failure", logs[0].read_text())
        self.assert_daemon_reaped()

    def test_failed_start_and_unusable_fallback_fail(self):
        result = self.run_shell('trap teardown_battery_daemon EXIT\nstart_battery_daemon',
                                success=False, TIDEPOOL_EXTRACT=str(self.frontend),
                                DAEMON_MODE="exit", BAD_ENDPOINT="1")
        self.assertIn("could not bind a direct compiler endpoint", result.stderr)
        self.assertNotIn("direct compiler endpoint validated", result.stderr)
        self.assert_daemon_reaped()

    def test_elapsed_timeout_reaps_starting_process(self):
        # Advance Bash's elapsed clock after the fixture has started, without
        # spending 30 seconds testing the fixed deadline.
        result = self.run_shell('sleep() { command sleep 0.1; SECONDS=$((SECONDS + 31)); }\n'
                                'trap teardown_battery_daemon EXIT\nstart_battery_daemon',
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
            self.run_shell('start_battery_daemon\n[[ "$BATTERY_DAEMON_OWNED" = 0 ]]\n'
                           'teardown_battery_daemon\n[[ -S "$TIDEPOOL_EXTRACT_DAEMON_SOCKET" ]]',
                           TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_DAEMON_SOCKET=str(socket))
            self.assertIsNone(proc.poll())
        finally:
            proc.terminate()
            proc.wait(timeout=5)

    def test_disabled_daemon_clears_inherited_socket_and_checks_direct(self):
        result = self.run_shell('start_battery_daemon\n[[ -z "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ]]',
                                TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_NO_DAEMON="1",
                                TIDEPOOL_EXTRACT_DAEMON_SOCKET="/inherited")
        self.assertIn("daemon disabled", result.stderr)
        self.assertFalse((self.root / "daemon.pid").exists())
        self.run_shell("start_battery_daemon", success=False,
                       TIDEPOOL_EXTRACT=str(self.frontend), TIDEPOOL_EXTRACT_NO_DAEMON="1",
                       BAD_ENDPOINT="1")

    def test_timed_out_endpoint_is_rejected_even_after_identity(self):
        self.executable("timeout", '#!/usr/bin/env python3\nimport sys\n'
                        'sys.stdout.buffer.write(b"TPCID001" + bytes(32))\nsys.exit(124)\n')
        self.run_shell("validate_tidepool_extract_endpoint", success=False,
                       TIDEPOOL_EXTRACT=str(self.frontend))

    def test_opt_in_success_logs_are_bounded_without_pruning_failures(self):
        artifact_root = self.root / "artifacts"
        artifact_root.mkdir()
        for index in range(7):
            old = artifact_root / str(index)
            old.mkdir()
            (old / ".successful-run").touch()
        failure = artifact_root / "failure"
        failure.mkdir()
        (failure / "nextest.log").write_text("failure evidence")
        self.run_shell('prepare_battery_artifacts fixture true\n'
                       'finalize_battery_artifacts 0\n',
                       TIDEPOOL_KEEP_TEST_LOGS="1",
                       TIDEPOOL_TEST_ARTIFACT_ROOT=str(artifact_root))
        self.assertEqual(len(list(artifact_root.glob("*/.successful-run"))), 5)
        self.assertEqual((failure / "nextest.log").read_text(), "failure evidence")

    def test_successful_tests_preserve_daemon_failure_artifacts(self):
        (self.root / "scripts").mkdir()
        doctor = self.root / "scripts/toolchain-doctor.sh"
        doctor.write_text("#!/bin/sh\necho toolchain diagnostics\n")
        doctor.chmod(0o755)
        self.run_shell('prepare_battery_artifacts fixture true\n'
                       'trap teardown_battery_daemon EXIT\nstart_battery_daemon\n'
                       'finalize_battery_artifacts 0\n'
                       '[[ -f "$BATTERY_ARTIFACT_DIR/daemon.log" ]]\n'
                       'saved_dir="$BATTERY_DAEMON_SOCKET_DIR"\n'
                       'teardown_battery_daemon\n[[ ! -d "$saved_dir" ]]\n'
                       '[[ -f "$BATTERY_ARTIFACT_DIR/daemon.log" ]]',
                       TIDEPOOL_EXTRACT=str(self.frontend), DAEMON_MODE="exit")

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

    def test_start_battery_daemon_reuses_current_persistent_daemon(self):
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        pid_before = (daemon_dir / "daemon.pid").read_text()
        sock = str(daemon_dir / "extract.sock")
        result = self.run_shell(
            'trap teardown_battery_daemon EXIT\nstart_battery_daemon\n'
            'printf "OWNED=%s\\n" "$BATTERY_DAEMON_OWNED"\n'
            'printf "SOCK=%s\\n" "$TIDEPOOL_EXTRACT_DAEMON_SOCKET"',
            **env)
        self.assertIn(f"reusing persistent compile daemon at {sock}", result.stderr)
        self.assertIn("OWNED=0", result.stdout)
        self.assertIn(f"SOCK={sock}", result.stdout)
        pid_after = (daemon_dir / "daemon.pid").read_text()
        self.assertEqual(pid_before, pid_after)
        os.kill(int(pid_after), 0)

    def test_start_battery_daemon_skips_stale_persistent_daemon(self):
        env = self.persistent_env()
        self.run_shell('daemon_start_persistent', **env)
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        persistent_sock = str(daemon_dir / "extract.sock")
        result = self.run_shell(
            'trap teardown_battery_daemon EXIT\nstart_battery_daemon\n'
            'printf "OWNED=%s\\n" "$BATTERY_DAEMON_OWNED"\n'
            'printf "SOCK=%s\\n" "$TIDEPOOL_EXTRACT_DAEMON_SOCKET"',
            **env, PRODUCER_BYTE="1")
        self.assertIn(f"persistent compile daemon at {persistent_sock} is stale (producer mismatch)",
                     result.stderr)
        self.assertIn("just daemon-stop && just daemon-start", result.stderr)
        self.assertIn("OWNED=1", result.stdout)
        self.assertNotIn(f"SOCK={persistent_sock}", result.stdout)
        # The persistent daemon itself is left running untouched.
        os.kill(int((daemon_dir / "daemon.pid").read_text()), 0)

    def git_checkout(self):
        # The producer-source fingerprint reads the checkout through Git.
        git = dict(GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@invalid",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@invalid")
        self.env.update(git)
        (self.root / ".gitignore").write_text("bin/\ncache/\ntarget/\ndaemon.pid*\ntidepool-*\n")
        for args in (["init", "-q"], ["add", "-A"], ["commit", "-qm", "fixture"]):
            subprocess.run(["git", *args], cwd=self.root, env=self.env, check=True)

    def start_recorded_persistent_daemon(self, env):
        # Built from this checkout, so daemon_start_persistent records its
        # producer-source fingerprint and worker.
        self.run_shell('resolve_tidepool_extract\ndaemon_start_persistent',
                       **{k: v for k, v in env.items() if k != "TIDEPOOL_EXTRACT"})
        self.addCleanup(lambda: self.run_shell('daemon_stop_persistent', **env))
        daemon_dir = self.persistent_dir()
        self.assertEqual(len((daemon_dir / "sources").read_text().strip()), 64)
        self.assertEqual((daemon_dir / "daemon.worker").read_text().strip(), str(self.worker))
        return daemon_dir

    def test_matching_producer_sources_reuse_the_persistent_daemon(self):
        self.git_checkout()
        env = self.persistent_env()
        daemon_dir = self.start_recorded_persistent_daemon(env)
        pid = (daemon_dir / "daemon.pid").read_text()
        # Uncommitted edits outside the producer sources keep the match; a
        # failing cargo proves the checkout does not build its own extractor.
        (self.root / "notes.md").write_text("unrelated edit")
        # A linked worktree's files are newer than the adopted worker; content,
        # not file times, decides the match.
        os.utime(self.worker, (1_700_000_000, 1_700_000_000))
        result = self.run_shell(
            'resolve_tidepool_extract --prefer-persistent-daemon\n'
            'trap teardown_battery_daemon EXIT\nstart_battery_daemon\n'
            'printf "OWNED=%s\\nSOCK=%s\\nEXE=%s\\nWORKER=%s\\n" "$BATTERY_DAEMON_OWNED" '
            '"$TIDEPOOL_EXTRACT_DAEMON_SOCKET" "$TIDEPOOL_EXTRACT" "$TIDEPOOL_EXTRACT_WORKER"',
            **{k: v for k, v in env.items() if k != "TIDEPOOL_EXTRACT"}, FAIL_CARGO="1",
            TIDEPOOL_ALLOW_STALE_EXTRACT="0")
        sock = daemon_dir / "extract.sock"
        self.assertIn(f"reusing persistent compile daemon at {sock} (producer sources match)", result.stderr)
        self.assertIn("OWNED=0", result.stdout)
        self.assertIn(f"SOCK={sock}", result.stdout)
        self.assertIn(f"EXE={(daemon_dir / 'daemon.exe').read_text().strip()}", result.stdout)
        self.assertIn(f"WORKER={self.worker}", result.stdout)
        self.assertEqual((daemon_dir / "daemon.pid").read_text(), pid)

    def test_differing_producer_sources_start_a_one_worker_daemon(self):
        self.git_checkout()
        env = self.persistent_env()
        daemon_dir = self.start_recorded_persistent_daemon(env)
        (self.root / "tidepool/extract-cmd/src/main.rs").write_text("// changed frontend\n")
        result = self.run_shell(
            'resolve_tidepool_extract --prefer-persistent-daemon\n'
            'trap teardown_battery_daemon EXIT\nstart_battery_daemon\n'
            'printf "OWNED=%s\\n" "$BATTERY_DAEMON_OWNED"',
            **{k: v for k, v in env.items() if k != "TIDEPOOL_EXTRACT"}, PRODUCER_BYTE="1")
        self.assertIn("stale (producer sources differ from this checkout)", result.stderr)
        self.assertIn("with one GHC worker", result.stderr)
        self.assertIn("OWNED=1", result.stdout)
        argv = (self.root / "daemon.pid.argv").read_text().splitlines()
        self.assertEqual(argv[argv.index("--workers") + 1], "1")
        os.kill(int((daemon_dir / "daemon.pid").read_text()), 0)


if __name__ == "__main__":
    unittest.main()
