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
    sys.stdout.buffer.write(b"TPCID001" + bytes(32))
    sys.exit(2)  # EOF is deliberately not a complete compiler request.
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
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.bind(sys.argv[sys.argv.index("--socket") + 1])
sock.listen()
while True:
    conn, _ = sock.accept()
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
        self.temp = tempfile.TemporaryDirectory(prefix="extract helpers ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "haskell").mkdir()
        (self.root / "bin").mkdir()
        for directory in ("haskell/src", "haskell/app", "tidepool-extract-cmd/src"):
            (self.root / directory).mkdir(parents=True)
        for manifest in ("haskell/tidepool-extract.cabal", "tidepool-extract-cmd/Cargo.toml"):
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
                        TIDEPOOL_ALLOW_STALE_EXTRACT="1", TMPDIR=str(self.root))

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


if __name__ == "__main__":
    unittest.main()
