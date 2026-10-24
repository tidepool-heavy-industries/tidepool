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


def publish_json(path, value):
    temporary = path.with_name(path.name + ".publishing")
    temporary.write_text(json.dumps(value))
    temporary.replace(path)


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
    publish_json(root / "issued.json", fact)
    publication=os.environ.get("PACKAGE_LIFETIME_PUBLICATION","normal")
    if publication=="malformed":
        temporary=root / "child.json.publishing"
        temporary.write_text("{malformed")
        temporary.replace(root / "child.json")
    elif publication!="missing":
        publish_json(root / "child.json", fact)
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
                publish_json(root / "child.json", {
                    "pid": os.getpid(), "parent": os.getppid(), "stage": "before-arm"})
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
    owner_reaped = False
    child = None
    reaped = {}
    failure = None
    result = None
    publication=os.environ.get("PACKAGE_LIFETIME_PUBLICATION","normal")
    try:
        # Keep the exact group leader unreaped until failure cleanup has killed
        # its isolated group. Fact publication is not a cleanup prerequisite.
        owner = subprocess.Popen([sys.executable, __file__, "provider", str(source),
                                  str(root), policy], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                 start_new_session=True)
        fact = wait_file(root / "child.json")
        child = fact["pid"]
        assert fact["parent"] == owner.pid
        owner.kill()
        provider_exit=owner.wait(timeout=5)
        owner_reaped=True
        assert provider_exit == -signal.SIGKILL
        if policy == "raw":
            waited, status = os.waitpid(child, os.WNOHANG)
            if waited:
                reaped[waited]=status
            assert waited == 0, "negative control must remain alive after provider reap"
            (root / "allow-mutation").write_text("allow fake mutation")
            deadline = time.monotonic() + 5
            while not (root / "late-mutation").exists():
                assert time.monotonic() < deadline
                time.sleep(.005)
            os.kill(child, signal.SIGKILL)
        deadline = time.monotonic() + 5
        while child not in reaped:
            waited, status = os.waitpid(child, os.WNOHANG)
            if waited:
                reaped[waited]=status
                break
            assert time.monotonic() < deadline, "owned child did not retire"
            time.sleep(.005)
        result = {"policy": policy, "provider_pid": owner.pid, "provider_exit": provider_exit,
                  "qualification_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                  "child": fact, "child_reaped": child in reaped,
                  "child_exit": os.waitstatus_to_exitcode(reaped[child]),
                  "late_mutation": (root / "late-mutation").exists(),
                  "real_nix_used": False}
        if policy == "late-arm":
            assert result["child_exit"] == 125, result
        else:
            assert result["child_exit"] == -signal.SIGKILL, result
            assert fact["pdeathsig"] == (0 if policy == "raw" else signal.SIGKILL), result
            assert result["late_mutation"] == (policy == "raw"), result
    except (ValueError, AssertionError) as error:
        if publication not in ("malformed","missing"):
            raise
        failure=type(error).__name__ + ": " + str(error)
    finally:
        if owner is not None and not owner_reaped:
            # The retained unreaped leader reserves this PID/PGID even if it
            # has exited. No unrelated process can take this group identity.
            try:
                os.killpg(owner.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            owner.wait(timeout=5)
            owner_reaped=True
        if child is not None and child not in reaped:
            try:
                waited,status=os.waitpid(child,os.WNOHANG)
                if waited:
                    reaped[waited]=status
                else:
                    # A positive owned-child observation precedes this signal.
                    os.kill(child,signal.SIGKILL)
            except ChildProcessError:
                raise AssertionError("fixture lost its exact child reap")
        # The isolated subreaper owns every adopted child, including one whose
        # publication was absent/broken. Collect actual waits, never proc absence.
        deadline=time.monotonic()+5
        while True:
            try:
                waited,status=os.waitpid(-1,os.WNOHANG)
            except ChildProcessError:
                break
            if waited:
                reaped[waited]=status
                continue
            assert time.monotonic()<deadline,"fixture child reap remains unconfirmed"
            time.sleep(.005)
        if owner is not None:
            owner.stderr.close()
    if failure is not None:
        # This independent audit fact is read only AFTER cleanup; it cannot
        # supply any PID or other authority to the cleanup path above.
        issued=json.loads((root / "issued.json").read_text())
        assert issued["pid"] in reaped
        assert os.waitstatus_to_exitcode(reaped[issued["pid"]]) == -signal.SIGKILL
        result={"publication":publication,"fact_failure":failure,"provider_pid":owner.pid,
                "provider_exit":owner.returncode,"issued":issued,
                "qualification_sha256":hashlib.sha256(source.read_bytes()).hexdigest(),
                "actual_child_reaps":{str(pid):os.waitstatus_to_exitcode(status) for pid,status in reaped.items()},
                "real_nix_used":False}
    print(json.dumps(result))


if __name__ == "__main__":
    if Path(sys.argv[0]).name == "nix-store":
        raise SystemExit(fake_nix())
    role, source, root, policy = sys.argv[1:]
    if role == "supervise":
        supervise(Path(source), Path(root), policy)
    else:
        assert role == "provider"
        provider(Path(source), Path(root), policy)
