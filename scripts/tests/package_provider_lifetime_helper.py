"""Isolated Linux process fixture. It never starts Nix or registers GC roots."""
import ctypes
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def library():
    result = ctypes.CDLL(None, use_errno=True)
    result.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong,
                             ctypes.c_ulong, ctypes.c_ulong]
    result.prctl.restype = ctypes.c_int
    result.getppid.argtypes = []
    result.getppid.restype = ctypes.c_int
    return result


def wait_file(path):
    deadline = time.monotonic() + 5
    while not path.exists():
        if time.monotonic() >= deadline:
            raise AssertionError(f"fixture did not publish {path}")
        time.sleep(.005)
    return json.loads(path.read_text())


def fake_nix():
    libc = library()
    configured = ctypes.c_int()
    assert libc.prctl(2, ctypes.addressof(configured), 0, 0, 0) == 0
    root = Path(os.environ["PACKAGE_LIFETIME_FIXTURE"])
    fact = {"pid": os.getpid(), "parent": os.getppid(), "pdeathsig": configured.value,
            "argv": sys.argv, "stage": "executed"}
    (root / "child.json").write_text(json.dumps(fact))
    mode = os.environ.get("PACKAGE_LIFETIME_MODE", "block")
    if mode == "failure":
        print("declared stdout", flush=True)
        print("declared stderr", file=sys.stderr, flush=True)
        return 7
    if mode == "success":
        print("13", flush=True)
        return 0
    while True:
        if (root / "allow-mutation").exists():
            (root / "late-mutation").write_text("fake registration after owner death")
        time.sleep(.005)


def provider(source, root, policy):
    spec = importlib.util.spec_from_file_location("qualification", source)
    qualification = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(qualification)
    if policy == "late-arm":
        # Hold the real child before prctl until its parent has died. This tests
        # the actual kernel reparenting race without a production test hook.
        real = library()
        parent = os.getpid()
        class DelayedLibrary:
            def prctl(self, *args):
                (root / "child.json").write_text(json.dumps({
                    "pid": os.getpid(), "parent": os.getppid(), "stage": "before-arm"}))
                deadline = time.monotonic() + 5
                while os.getppid() == parent:
                    if time.monotonic() >= deadline:
                        os._exit(127)
                    time.sleep(.005)
                return real.prctl(*args)
            getppid = real.getppid
        qualification._package_child_libc = DelayedLibrary()
    if policy == "raw":
        subprocess.check_output([str(root / "tools/bin/nix-store"), "--query", "--size",
                                 "/fixture/store-path"], text=True, timeout=60)
    else:
        qualification._package_nar_bytes(root / "tools", ["/fixture/store-path"])


def supervise(source, root, policy):
    assert library().prctl(36, 1, 0, 0, 0) == 0, "fixture requires a child subreaper"
    owner = None
    child = None
    child_reaped = False
    try:
        owner = subprocess.Popen([sys.executable, __file__, "provider", str(source),
                                  str(root), policy], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        fact = wait_file(root / "child.json")
        child = fact["pid"]
        assert fact["parent"] == owner.pid
        owner.kill()
        assert owner.wait(timeout=5) == -signal.SIGKILL
        if policy == "raw":
            waited, _ = os.waitpid(child, os.WNOHANG)
            assert waited == 0, "negative control must remain alive after provider reap"
            (root / "allow-mutation").write_text("allow fake mutation")
            deadline = time.monotonic() + 5
            while not (root / "late-mutation").exists():
                assert time.monotonic() < deadline
                time.sleep(.005)
            os.kill(child, signal.SIGKILL)
            waited, status = os.waitpid(child, 0)
        else:
            deadline = time.monotonic() + 5
            while True:
                waited, status = os.waitpid(child, os.WNOHANG)
                if waited:
                    break
                assert time.monotonic() < deadline, "owned child did not retire"
                time.sleep(.005)
            assert not (root / "late-mutation").exists()
        child_reaped = True
        assert waited == child
        result = {"policy": policy, "provider_pid": owner.pid, "provider_exit": owner.returncode,
                  "qualification_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                  "child": fact, "child_reaped": True,
                  "child_exit": os.waitstatus_to_exitcode(status),
                  "late_mutation": (root / "late-mutation").exists(),
                  "real_nix_used": False}
        if policy == "late-arm":
            assert result["child_exit"] == 125, result
        else:
            assert result["child_exit"] == -signal.SIGKILL, result
            assert fact["pdeathsig"] == (0 if policy == "raw" else signal.SIGKILL), result
        print(json.dumps(result))
    finally:
        if owner is not None and owner.poll() is None:
            owner.kill()
            owner.wait(timeout=5)
        if child is not None and not child_reaped:
            try:
                os.kill(child, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                os.waitpid(child, 0)
            except ChildProcessError:
                pass


if __name__ == "__main__":
    if Path(sys.argv[0]).name == "nix-store":
        raise SystemExit(fake_nix())
    role, source, root, policy = sys.argv[1:]
    if role == "supervise":
        supervise(Path(source), Path(root), policy)
    else:
        assert role == "provider"
        provider(Path(source), Path(root), policy)
