"""Actual child lifetime controls for the sealed package provider."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / "build/package/qualification.py"
HELPER = Path(__file__).with_name("package_provider_lifetime_helper.py")
SPEC = importlib.util.spec_from_file_location("provider_qualification", SCRIPT)
qualification = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qualification)


class PackageProviderLifetimeTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.program = self.root / "tools/bin/nix-store"
        self.program.parent.mkdir(parents=True)
        self.program.write_text("#!" + sys.executable + "\n" + HELPER.read_text())
        self.program.chmod(0o700)
        self.environment = {**os.environ, "PACKAGE_LIFETIME_FIXTURE": str(self.root)}

    def retain(self, name, value):
        if root := os.environ.get("TIDEPOOL_PROVIDER_LIFETIME_EVIDENCE"):
            directory=Path(root)
            directory.mkdir(parents=True,exist_ok=True,mode=0o700)
            qualification.write_private_json(directory / (name + ".json"),value)

    def test_parent_death_kills_child_and_raw_negative_control_survives(self):
        observations = []
        for policy in ("raw", "owned"):
            with self.subTest(policy=policy):
                for path in ("child.json", "late-mutation", "allow-mutation"):
                    (self.root / path).unlink(missing_ok=True)
                result = subprocess.run([sys.executable, str(HELPER), "supervise", str(SCRIPT),
                                         str(self.root), policy], env=self.environment,
                                        capture_output=True, text=True, timeout=15)
                self.assertEqual(result.returncode, 0, result.stderr)
                observation = json.loads(result.stdout)
                self.assertTrue(observation["child_reaped"])
                self.assertEqual(observation["late_mutation"], policy == "raw")
                observations.append(observation)
        self.assertEqual(observations[0]["child"]["pdeathsig"], 0)
        self.assertEqual(observations[1]["child"]["pdeathsig"], signal.SIGKILL)
        self.retain("actual-parent-death", observations)

    def test_broken_or_missing_fact_cannot_bypass_fixture_cleanup(self):
        observations=[]
        for publication in ("malformed","missing"):
            with self.subTest(publication=publication):
                for name in ("child.json","issued.json"):
                    (self.root / name).unlink(missing_ok=True)
                environment={**self.environment,"PACKAGE_LIFETIME_PUBLICATION":publication}
                result=subprocess.run([sys.executable,str(HELPER),"supervise",str(SCRIPT),
                                       str(self.root),"raw"],env=environment,
                                      capture_output=True,text=True,timeout=15)
                self.assertEqual(result.returncode,0,result.stderr)
                observation=json.loads(result.stdout)
                self.assertEqual(observation["issued"]["pdeathsig"],0)
                self.assertEqual(observation["provider_exit"],-signal.SIGKILL)
                self.assertEqual(observation["actual_child_reaps"][str(observation["issued"]["pid"])],-signal.SIGKILL)
                self.assertTrue(observation["fact_failure"])
                observations.append(observation)
        self.retain("broken-fact-cleanup",observations)

    def test_parent_exit_before_arm_refuses_executable(self):
        result = subprocess.run([sys.executable, str(HELPER), "supervise", str(SCRIPT),
                                 str(self.root), "late-arm"], env=self.environment,
                                capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        observation = json.loads(result.stdout)
        self.assertEqual(observation["child"]["stage"], "before-arm")
        self.assertEqual(observation["child_exit"], 125)
        self.assertFalse(observation["late_mutation"])
        self.retain("actual-late-arm",observation)

    def test_successful_provider_query_returns_only_after_owned_child_reap(self):
        with patch.dict(os.environ,{**self.environment,"PACKAGE_LIFETIME_MODE":"success"}):
            self.assertEqual(qualification._package_nar_bytes(self.root / "tools",["/fixture/input"]),13)
        child=json.loads((self.root / "child.json").read_text())
        self.assertEqual(child["pdeathsig"],signal.SIGKILL)
        with self.assertRaises(ChildProcessError):
            os.waitpid(child["pid"],os.WNOHANG)

    def test_normal_failure_retains_exact_argv_status_and_captured_streams(self):
        command = [str(self.program), "--query", "--size", "/fixture/input"]
        environment = {**self.environment, "PACKAGE_LIFETIME_MODE": "failure"}
        with self.assertRaises(subprocess.CalledProcessError) as observed:
            qualification._package_subprocess(command, stdout_only=True, text=True,
                                               stderr=subprocess.PIPE, env=environment, timeout=5)
        error = observed.exception
        self.assertEqual(error.cmd, command)
        self.assertEqual(error.returncode, 7)
        self.assertEqual(error.stdout, "declared stdout\n")
        self.assertEqual(error.stderr, "declared stderr\n")
        child = json.loads((self.root / "child.json").read_text())
        self.assertEqual(child["pdeathsig"], signal.SIGKILL)
        with self.assertRaises(ChildProcessError):
            os.waitpid(child["pid"], os.WNOHANG)

    def test_timeout_keeps_existing_kill_and_wait_owner(self):
        command = [str(self.program), "--query", "--size", "/fixture/input"]
        with self.assertRaises(subprocess.TimeoutExpired) as observed:
            qualification._package_subprocess(command, stdout_only=True, text=True,
                                               stderr=subprocess.PIPE, env=self.environment,
                                               timeout=.2)
        self.assertEqual(observed.exception.cmd, command)
        child = json.loads((self.root / "child.json").read_text())
        self.assertEqual(child["pdeathsig"], signal.SIGKILL)
        with self.assertRaises(ChildProcessError):
            os.waitpid(child["pid"], os.WNOHANG)
        self.assertFalse((self.root / "late-mutation").exists())

    def test_multithreaded_parent_refuses_before_spawn(self):
        ready, stop = threading.Event(), threading.Event()
        def worker():
            ready.set()
            stop.wait()
        thread = threading.Thread(target=worker)
        thread.start()
        try:
            self.assertTrue(ready.wait(5))
            with patch.object(qualification.subprocess, "run") as spawn:
                with self.assertRaisesRegex(RuntimeError, "one kernel thread"):
                    qualification._package_subprocess([str(self.program)])
                spawn.assert_not_called()
        finally:
            stop.set()
            thread.join(timeout=5)
            self.assertFalse(thread.is_alive())

    def test_failed_parent_death_setup_refuses_execution(self):
        class RefusedLibrary:
            @staticmethod
            def prctl(*_args):
                return -1
            getppid = staticmethod(os.getppid)
        with patch.object(qualification,"_package_child_libc",RefusedLibrary()):
            result=qualification._package_subprocess([str(self.program)],env=self.environment,
                                                     capture_output=True,timeout=5)
        self.assertEqual(result.returncode,125)
        self.assertFalse((self.root / "child.json").exists())

    def test_privileged_executable_refuses_execution(self):
        self.program.chmod(0o4700)
        result = qualification._package_subprocess([str(self.program)], env=self.environment,
                                                   capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 126)
        self.assertFalse((self.root / "child.json").exists())

    def test_query_helpers_share_the_provider_child_entry(self):
        calls = []
        def output(command, **options):
            self.assertTrue(callable(options["preexec_fn"]))
            calls.append(command)
            if "--requisites" in command:
                return "/fixture/object\n"
            if "--hash" in command:
                return "sha256:fixture\n"
            if "--size" in command:
                return "13\n"
            return "/fixture/root -> /fixture/object\n"
        tools = self.root / "tools"
        with patch.object(qualification.subprocess, "check_output", side_effect=output):
            self.assertEqual(qualification.nix_metadata(tools, ["/fixture/object"])["closure"],
                             ["/fixture/object"])
            self.assertEqual(qualification._package_nar_bytes(tools, ["/fixture/object"]), 13)
            observation = qualification._RegisteredGcRoots(tools, ["/fixture/object"])
            self.assertEqual(observation.pairs, frozenset({("/fixture/root", "/fixture/object")}))
        self.assertEqual(len(calls), 4)


if __name__ == "__main__":
    unittest.main()
