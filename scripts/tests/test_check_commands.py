#!/usr/bin/env python3
"""Exercise check orchestration with fake compiler and Cargo boundaries."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]


class CheckCommands(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "bin").mkdir()
        self.env = os.environ | {"PATH": f"{self.root / 'bin'}:{os.environ['PATH']}"}
        for name in ("check.sh", "test-suite.sh", "lib-steps.sh"):
            shutil.copyfile(SCRIPTS / name, self.root / "scripts" / name)
        self.script("scripts/lib-extract.sh", '''
resolve_tidepool_extract() { :; }
prepare_battery_artifacts() { BATTERY_NEXTEST_LOG="$PWD/check.log"; }
finalize_battery_artifacts() { echo "finalize:$1" >> events; }
start_battery_daemon() {
  echo start >> events
  export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$PWD/compiler.sock"
}
teardown_battery_daemon() { echo teardown >> events; }
_terminate_and_wait() { kill "$1"; wait "$1" || :; }
''')
        self.script("scripts/lint.sh", 'echo lint >> events\nexit "${LINT_STATUS:-0}"')
        self.script("scripts/test-suite-check.sh", "echo registration >> events")
        self.script("scripts/battery-shard.sh", 'echo "shard:$*" >> events')
        self.script("bin/jq", "printf 'alpha\\nbeta\\n'")
        self.script("bin/cargo", '''
import json, os, signal, sys, time
from pathlib import Path
with open("cargo.jsonl", "a") as f:
    f.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "metadata":
    print("{}")
    sys.exit(0)
assert os.environ.get("TIDEPOOL_EXTRACT_DAEMON_SOCKET"), "missing resident compiler"
if os.environ.get("WAIT_FOR_SIGNAL"):
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    Path("cargo.pid").write_text(str(os.getpid()))
    while True:
        time.sleep(0.01)
sys.exit(int(os.environ.get("TEST_STATUS", "0")))
''', python=True)

    def script(self, path, body, python=False):
        target = self.root / path
        target.write_text(("#!/usr/bin/env python3\n" if python else "#!/usr/bin/env bash\n") + body)
        target.chmod(0o755)

    def run_check(self, **env):
        return subprocess.run(["bash", "scripts/check.sh"], cwd=self.root,
                              env=self.env | env, capture_output=True, text=True, timeout=10)

    def test_suite_prebuild_selects_only_its_registered_integration_targets(self):
        result = subprocess.run(["bash", "scripts/test-suite.sh", "example"],
                                cwd=self.root, env=self.env, capture_output=True,
                                text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "cargo.jsonl").read_text().splitlines()]
        self.assertEqual(calls[-1], ["nextest", "run", "--no-run", "-p", "example",
                                    "--test", "alpha", "--test", "beta"])
        events = (self.root / "events").read_text().splitlines()
        self.assertEqual(events.count("start"), 1)
        self.assertEqual(events[-1], "teardown")
        self.assertIn("shard:example --test alpha", events)
        self.assertIn("shard:example --test beta", events)

    def test_lint_failure_still_runs_tests_and_retires_compiler(self):
        result = self.run_check(LINT_STATUS="7")
        self.assertNotEqual(result.returncode, 0)
        events = (self.root / "events").read_text().splitlines()
        self.assertEqual(events, ["lint", "start", "finalize:1", "teardown"])
        self.assertIn("nextest", (self.root / "cargo.jsonl").read_text())

    def test_test_failure_and_success_both_retire_compiler(self):
        for status in ("0", "9"):
            with self.subTest(status=status):
                result = self.run_check(TEST_STATUS=status)
                self.assertEqual(result.returncode == 0, status == "0")
                self.assertEqual((self.root / "events").read_text().splitlines()[-1], "teardown")

    def test_direct_signal_stops_test_process_before_cleanup(self):
        with open(self.root / "output", "w") as output:
            process = subprocess.Popen(["bash", "scripts/check.sh"], cwd=self.root,
                                       env=self.env | {"WAIT_FOR_SIGNAL": "1"},
                                       stdout=output, stderr=output)
            try:
                deadline = time.monotonic() + 5
                while not (self.root / "cargo.pid").exists():
                    self.assertIsNone(process.poll())
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(0.01)
                child = int((self.root / "cargo.pid").read_text())
                process.send_signal(signal.SIGTERM)
                self.assertEqual(process.wait(timeout=5), 130)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child, 0)
                self.assertEqual((self.root / "events").read_text().splitlines()[-1], "teardown")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    unittest.main()
