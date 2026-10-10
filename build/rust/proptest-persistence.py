"""Prove native property shrinking, seed persistence and zero-fresh-case replay."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path("build/rust/tests/proptest-persistence/src/lib.rs")
PACKAGE = SOURCE.parent.parent


class PersistenceTests(unittest.TestCase):
    def phase(self, name, phase, cases, expected_failure):
        output = self.output / name / phase
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith("PROPTEST_")}
        environment.update(PROPTEST_CASES=str(cases),
                           PROPTEST_RNG_SEED="2026101001" if cases else "2026101002",
                           PROPTEST_MAX_SHRINK_ITERS="4096")
        command = [sys.executable, str(self.runner), str(self.binary),
                   "--exact", name, "--expected-count", "1", "--ignored",
                   "--timeout", "10", "--output-dir", str(output), "--retain-artifacts"]
        completed = subprocess.run(command, env=environment, text=True,
                                   capture_output=True, timeout=60)
        (output / "invocation.json").write_text(json.dumps({
            "command": command, "configured_fresh_cases": cases,
            "rng_seed": environment["PROPTEST_RNG_SEED"],
            "exit_status": completed.returncode,
        }, indent=2) + "\n")
        self.assertEqual(completed.returncode, int(expected_failure), completed.stderr)
        key = hashlib.sha256(name.encode()).hexdigest()
        report = json.loads((output / (key + ".json")).read_text())
        execution = report["execution"]
        self.assertEqual(execution["executed_test_count"], 1)
        self.assertEqual(execution["failed_test_count"], int(expected_failure))
        self.assertEqual(execution["process_execution_count"], 1)
        self.assertEqual(execution["process_cleanup_status"], "confirmed")
        self.assertEqual(report["passed"], not expected_failure)
        self.assertTrue(all(not stream["truncated"] for stream in report["streams"].values()))
        text = "".join((output / (key + "." + stream + ".log")).read_text()
                       for stream in ("stdout", "stderr"))
        histories = re.findall(r"^controlled_history=(\[[^\n]*\])$", text, re.MULTILINE)
        self.assertIn(f"configured_fresh_cases={cases}", text)
        if histories:
            identities = re.findall(r"^source_identity=(.+)$", text, re.MULTILINE)
            self.assertTrue(identities)
            self.assertEqual(set(identities), {SOURCE.as_posix()})
            self.assertTrue(Path(identities[0]).is_file())
            self.assertRegex(text, r"panicked at " + re.escape(SOURCE.as_posix()) + r":\d+:\d+")
        (output / "observations.json").write_text(json.dumps({
            "configured_fresh_cases": cases, "actual_callbacks": len(histories),
            "histories": histories, "executed_libtest_cases": execution["executed_test_count"],
        }, indent=2) + "\n")
        return text, histories

    def assert_persistence(self, name, direct=False, source=True):
        seed = PACKAGE / "proptest-regressions" / (
            "proptest_persistence_probe.txt" if direct else "lib.txt")
        self.assertFalse(seed.exists(), f"refuse to replace existing regression evidence: {seed}")
        try:
            fresh, histories = self.phase(name, "fresh", 1, True)
            self.assertGreater(len(histories), 1, "fresh failure must execute shrinking")
            self.assertEqual(histories[-1], "[7]")
            self.assertIn("controlled history defect", fresh)
            if source:
                self.assertNotIn("Failed to find absolute path", fresh)
                data = seed.read_text()
                rows = [row for row in data.splitlines() if row and not row.startswith("#")]
                self.assertEqual(len(rows), 1)
                self.assertIn("# shrinks to", rows[0])
                self.assertIn("[7]", rows[0])
                (self.output / name / "saved-regression.txt").write_text(data)
                replay, replay_histories = self.phase(name, "replay", 0, True)
                self.assertGreater(len(replay_histories), 1)
                self.assertEqual(replay_histories, histories,
                                 "zero-case replay must regenerate and shrink the saved history")
                self.assertIn("controlled history defect", replay)
                self.assertEqual(seed.read_text(), data, "replay must not append duplicate seeds")
                self.assertNotIn("Failed to find absolute path", replay)
            else:
                self.assertIn("manual_source_file=None", fresh)
                self.assertIn("SourceParallel set, but no source file known", fresh)
                self.assertFalse(seed.exists())
                replay, replay_histories = self.phase(name, "empty-replay", 0, False)
                self.assertEqual(replay_histories, [])
                self.assertFalse(seed.exists())
        finally:
            seed.unlink(missing_ok=True)

    def test_macro_source_parallel_persists_shrunk_failure_and_replays(self):
        self.assert_persistence("macro_source_parallel")

    def test_macro_direct_persists_shrunk_failure_and_replays(self):
        self.assert_persistence("macro_direct", direct=True)

    def test_manual_source_parallel_persists_shrunk_failure_and_replays(self):
        self.assert_persistence("manual_source_parallel")

    def test_manual_direct_persists_shrunk_failure_and_replays(self):
        self.assert_persistence("manual_direct", direct=True)

    def test_missing_manual_source_cannot_persist_or_replay(self):
        self.assert_persistence("manual_missing_source", source=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path)
    options = parser.parse_args()
    PersistenceTests.binary = options.binary.absolute()
    PersistenceTests.runner = options.runner.absolute()
    temporary = options.output_dir is None
    PersistenceTests.output = (options.output_dir or Path(tempfile.mkdtemp(
        prefix="native-proptest-control-"))).absolute()
    # SourceParallel deliberately selects one source-owned regression file.
    # Concurrent invocations must not erase another control's pending replay.
    with SOURCE.open("rb") as source:
        fcntl.flock(source, fcntl.LOCK_EX)
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(PersistenceTests)
        passed = unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful()
    if passed and temporary:
        shutil.rmtree(PersistenceTests.output)
    elif not passed:
        print(f"Persistence control evidence: {PersistenceTests.output}", file=sys.stderr)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
