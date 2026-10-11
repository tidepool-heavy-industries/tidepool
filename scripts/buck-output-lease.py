#!/usr/bin/env python3
"""Keep one checkout's Buck outputs stable through its synchronous command."""

import fcntl
import os
from pathlib import Path
import signal
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
ENTRYPOINTS = {"buck": "buck2-run.sh", "configure": "buck2-configure.sh"}


class WaitingCancelled(Exception):
    pass


def main(argv):
    if not argv or argv[0] not in ENTRYPOINTS:
        raise ValueError("expected buck or configure entrypoint")
    directory = ROOT / ".buck2-toolchains"
    directory.mkdir(exist_ok=True)
    child = None
    waiting = False
    interrupted = None

    def cancel(signum, _frame):
        nonlocal interrupted
        if interrupted is not None:
            return
        interrupted = signum
        if waiting:
            raise WaitingCancelled()
        if child is not None:
            try:
                child.send_signal(signum)
            except ProcessLookupError:
                pass

    old_handlers = {signum: signal.signal(signum, cancel)
                    for signum in (signal.SIGINT, signal.SIGTERM)}
    try:
        # Keep this inode outside Buck's disposable outputs. Python descriptors
        # are non-inheritable; close_fds also excludes it from every child exec.
        # Only this short-lived supervisor owns the lease, never a Buck daemon.
        with (directory / "output.lock").open("a") as lease:
            if interrupted is not None:
                return 128 + interrupted
            try:
                waiting = True
                try:
                    fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    print(f"Waiting for checkout Buck output lease: {ROOT}",
                          file=sys.stderr, flush=True)
                    fcntl.flock(lease, fcntl.LOCK_EX)
            except WaitingCancelled:
                return 128 + interrupted
            finally:
                waiting = False
            if interrupted is not None:
                return 128 + interrupted
            script = ROOT / "scripts" / ENTRYPOINTS[argv[0]]
            # Sourcing enters the implementation without another acquisition.
            # No inherited environment marker can bypass a later invocation.
            child = subprocess.Popen(
                ["bash", "-c", 'entrypoint=$1; shift; source "$entrypoint" "$@"',
                 "buck-output-lease-child", str(script), *argv[1:]],
                cwd=ROOT, close_fds=True,
            )
            if interrupted is not None:
                child.send_signal(interrupted)
            status = child.wait()
            # INT/TERM does not release the lease until the command has settled.
            # Buck and the native runner retain their existing cleanup owners.
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
    if status < 0:
        signum = -status
        signal.signal(signum, signal.SIG_DFL)
        os.kill(os.getpid(), signum)
    return status


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (OSError, ValueError) as error:
        print(f"Buck output lease: {error}", file=sys.stderr)
        sys.exit(2)
