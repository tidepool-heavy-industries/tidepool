"""Shared Nextest execution gate behavior without compilation."""
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]


class NextestGate(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "bin").mkdir()
        shutil.copy(SCRIPTS / "lib-nextest.sh", self.root / "scripts")
        self.runner = self.root / "runner.sh"
        self.runner.write_text(
            "#!/usr/bin/env bash\nset -euo pipefail\n"
            "source scripts/lib-nextest.sh\n"
            "trap 'kill -TERM \"$nextest_pid\" 2>/dev/null || true; wait \"$nextest_pid\" 2>/dev/null || true; exit 130' INT TERM\n"
            "status=0; nextest_run_checked || status=$?\n"
            "printf 'process=%s gate=%s\\n' \"$NEXTTEST_PROCESS_STATUS\" \"$NEXTTEST_GATE_STATUS\"\n"
            "exit \"$status\"\n")
        self.runner.chmod(0o755)
        cargo = self.root / "bin" / "cargo"
        cargo.write_text("""#!/usr/bin/env python3
import os, signal, sys, time
from pathlib import Path
if os.environ.get('WAIT'):
    Path('child.pid').write_text(str(os.getpid()))
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    while True: time.sleep(.01)
if os.environ.get('ZERO'):
    print('Summary 0 tests run:', file=sys.stderr)
elif not os.environ.get('MISSING'):
    print('Summary: 3 tests run: 3 passed', file=sys.stderr)
sys.exit(int(os.environ.get('STATUS', '0')))
""")
        cargo.chmod(0o755)
        self.env = os.environ | {"PATH": f"{self.root / 'bin'}:{os.environ['PATH']}"}

    def run_gate(self, **values):
        return subprocess.run([str(self.runner)], cwd=self.root,
                              env=self.env | values, capture_output=True,
                              text=True, timeout=5)

    def test_valid_nonzero_and_invalid_summaries(self):
        for values, status, stages in [
            ({}, 0, "process=0 gate=0"),
            ({"ZERO": "1"}, 1, "process=0 gate=1"),
            ({"MISSING": "1"}, 1, "process=0 gate=1"),
            ({"STATUS": "8"}, 8, "process=8 gate=8"),
        ]:
            with self.subTest(values=values):
                result = self.run_gate(**values)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertIn(stages, result.stdout)

    def test_signal_stops_actual_nextest_process(self):
        process = subprocess.Popen([str(self.runner)], cwd=self.root,
                                   env=self.env | {"WAIT": "1"},
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 3
            while not (self.root / "child.pid").exists():
                self.assertIsNone(process.poll())
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.01)
            child = int((self.root / "child.pid").read_text())
            process.send_signal(signal.SIGTERM)
            self.assertEqual(process.wait(timeout=3), 130)
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()


if __name__ == "__main__":
    unittest.main()
