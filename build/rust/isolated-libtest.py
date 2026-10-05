"""Run selected Buck-built libtest cases in fresh, bounded processes."""
import argparse
import concurrent.futures
from concurrent.futures import FIRST_COMPLETED, wait
import os
import hashlib
import json
import math
from pathlib import Path
import re
import signal
import subprocess
import sys
import threading
import time
import uuid


DEFAULT_TIMEOUT = 300
DISCOVERY_TIMEOUT = 30
OUTPUT_LIMIT = 4 << 20
RESULT = re.compile(
    r"^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;.*$",
    re.MULTILINE,
)
EXECUTION_RESULT = re.compile(
    r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;.*$",
    re.MULTILINE,
)
ACTIVE_PROCESSES = {}
ACTIVE_PROCESSES_LOCK = threading.Lock()
ACTIVE_PROCESS_SNAPSHOT = ()
INTERRUPT_SIGNAL = None


class RunnerInterrupted(Exception):
    def __init__(self, signum):
        self.signum = signum


def _signal_active_processes(signum, _frame):
    global INTERRUPT_SIGNAL
    if INTERRUPT_SIGNAL is None:
        INTERRUPT_SIGNAL = signum
    # Signal handlers do not take locks: a repeated signal must not deadlock
    # against a worker registering or retiring a child process.
    for process in ACTIVE_PROCESS_SNAPSHOT:
        if getattr(process, 'returncode', None) is None:
            _kill_group(process.pid)


def _kill_group(pgid):
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def _register_process(process):
    global ACTIVE_PROCESS_SNAPSHOT
    with ACTIVE_PROCESSES_LOCK:
        ACTIVE_PROCESSES[process.pid] = process
        ACTIVE_PROCESS_SNAPSHOT = tuple(ACTIVE_PROCESSES.values())
    if INTERRUPT_SIGNAL is not None:
        _kill_group(process.pid)
        _kill_and_reap(process)
        _unregister_process(process)
        raise RunnerInterrupted(INTERRUPT_SIGNAL)


def _unregister_process(process):
    global ACTIVE_PROCESS_SNAPSHOT
    with ACTIVE_PROCESSES_LOCK:
        ACTIVE_PROCESSES.pop(process.pid, None)
        ACTIVE_PROCESS_SNAPSHOT = tuple(ACTIVE_PROCESSES.values())


def _kill_and_reap(process):
    if not getattr(process, '_tidepool_delegated', False) or getattr(process, 'returncode', None) is None:
        _kill_group(process.pid)
    try:
        return process.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        # Do not wait indefinitely on inherited pipes from an escaped child.
        if process.stdout is not None:
            process.stdout.close()
        if process.stderr is not None:
            process.stderr.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            return '', ''
        return '', ''


# Only declared runtime selections and user-service coordinates cross this
# boundary. Provider credentials remain in the user's own authentication owner.
DELEGATED_ENVIRONMENT = (
    'PATH', 'HOME', 'USER', 'LOGNAME', 'XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS',
    'LD_LIBRARY_PATH', 'TMPDIR', 'TMP', 'TEMP', 'TEMPDIR',
    'TIDEPOOL_EXTRACT', 'TIDEPOOL_EXTRACT_WORKER', 'TIDEPOOL_COMPILER_DEPLOYMENT',
    'TIDEPOOL_PRELUDE_DIR', 'TIDEPOOL_GHC_LIBDIR', 'GHC_LIBDIR',
    'TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES', 'TIDEPOOL_EXTRACT_DAEMON_SOCKET',
    'TIDEPOOL_EXTRACT_DAEMON_LOG', 'TIDEPOOL_KEEP_TEST_LOGS',
    'TIDEPOOL_TEST_ARTIFACT_ROOT',
    'TIDEPOOL_TEST_BASH', 'TIDEPOOL_TEST_SLEEP', 'TIDEPOOL_BROWSER_NODE',
    'TIDEPOOL_BROWSER_DRIVER', 'EXOMONAD_EMBEDDED_ASSET_ROOT',
    'EXOMONAD_WORKSPACE_GITLINK', 'EXOMONAD_WORKSPACE_GIT_BUNDLE',
    'EXOMONAD_NIX_BIN', 'EXOMONAD_NIX_OFFLINE',
    'PLAYWRIGHT_BROWSERS_PATH', 'TIDEPOOL_M3_FIXTURE_DIR',
    'TIDEPOOL_FREER_RESUME_FIXTURE_DIR', 'TIDEPOOL_FREER_RETENTION_FIXTURE_DIR',
)


def delegated_command(args, timeout, service_slice, record):
    unit = 'tidepool-libtest-' + uuid.uuid4().hex + '.service'
    environment = [key for key in DELEGATED_ENVIRONMENT if key in os.environ]
    command = [
        'systemd-run', '--user', '--pipe', '--wait', '--collect',
        '--service-type=exec', '--property=Delegate=yes',
        '--property=KillMode=control-group', '--property=TimeoutStopSec=5s',
        f'--property=RuntimeMaxSec={timeout:g}s', '--slice=' + service_slice,
        '--unit=' + unit, '--working-directory=' + os.getcwd(),
    ]
    command.extend('--setenv=' + key + '=' + os.environ[key] for key in environment)
    command.extend(['--', *args])
    record.update(unit=unit, service_slice=service_slice,
                  environment_names=environment, cleanup_confirmed=False)
    return command, unit


def observe_delegated_admission(unit, record, finished):
    """A registered transient invocation proves this launch reached the manager."""
    while not finished.is_set():
        try:
            observed = subprocess.run([
                'systemctl', '--user', 'show', '--property=Id',
                '--property=LoadState', '--property=Transient',
                '--property=InvocationID', unit,
            ], capture_output=True, text=True, timeout=1, check=False)
            state = dict(line.split('=', 1) for line in observed.stdout.splitlines() if '=' in line)
            if (observed.returncode == 0 and state.get('Id') == unit
                    and state.get('LoadState') == 'loaded' and state.get('Transient') == 'yes'
                    and re.fullmatch(r'[a-fA-F0-9]{32}', state.get('InvocationID', ''))):
                record['manager_admission'] = state
                return
        except (OSError, subprocess.SubprocessError) as error:
            record['manager_observation_error'] = str(error)
        finished.wait(0.05)


def stop_delegated_service(unit, record):
    """Stop only this launch's service, including descendants outside client PGID."""
    base = ['systemctl', '--user']
    try:
        try:
            stopped = subprocess.run([*base, 'stop', unit], capture_output=True,
                                     text=True, timeout=10, check=False)
        except subprocess.TimeoutExpired:
            subprocess.run([*base, 'kill', '--kill-whom=all', '--signal=KILL', unit],
                           capture_output=True, text=True, timeout=5, check=False)
            stopped = subprocess.run([*base, 'stop', unit], capture_output=True,
                                     text=True, timeout=10, check=False)
        observed = subprocess.run([
            *base, 'show', '--property=LoadState', '--property=ActiveState', unit,
        ], capture_output=True, text=True, timeout=5, check=False)
        state = dict(line.split('=', 1) for line in observed.stdout.splitlines() if '=' in line)
        record.update(stop_exit_code=stopped.returncode,
                      observation_exit_code=observed.returncode, state=state,
                      cleanup_confirmed=(bool(record.get('manager_admission'))
                          and record.get('admission_observer_stopped', True)
                          and (state.get('LoadState') == 'not-found'
                              or (stopped.returncode == 0 and observed.returncode == 0
                                  and state.get('ActiveState') == 'inactive'))))
        if not record['cleanup_confirmed']:
            record['cleanup_error'] = ('delegated launch admission is unknown; absent unit does not fence a queued start'
                if not record.get('manager_admission') else 'exact delegated service is not confirmed stopped')
    except (OSError, subprocess.SubprocessError) as error:
        record.update(cleanup_confirmed=False, cleanup_error=str(error))


def execute(args, timeout, service_slice=None, service_record=None):
    """Run a command in its own process group, reaping it even after timeout."""
    if INTERRUPT_SIGNAL is not None:
        raise RunnerInterrupted(INTERRUPT_SIGNAL)
    command, unit = args, None
    if service_slice is not None:
        command, unit = delegated_command(args, timeout, service_slice, service_record)
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    process._tidepool_delegated = unit is not None
    observer, observer_finished = None, threading.Event()
    try:
        _register_process(process)
        if unit is not None:
            observer = threading.Thread(target=observe_delegated_admission,
                args=(unit, service_record, observer_finished), daemon=True)
            observer.start()
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        stdout, stderr = _kill_and_reap(process)
        raise subprocess.TimeoutExpired(
            args, timeout, output=stdout or error.output, stderr=stderr or error.stderr
        ) from None
    except BaseException:
        _kill_and_reap(process)
        raise
    finally:
        _unregister_process(process)
        if unit is not None:
            observer_finished.set()
            if observer is not None:
                observer.join(timeout=2)
            service_record['admission_observer_stopped'] = observer is None or not observer.is_alive()
            stop_delegated_service(unit, service_record)
    # Test leaders sometimes leave a detached same-group child after a passing
    # result. The process group remains the cleanup owner through this point.
    if unit is None:
        _kill_group(process.pid)
    return subprocess.CompletedProcess(args, process.returncode, stdout, stderr)


def names(binary, *args):
    result = execute([binary, '--list', '--format', 'terse', *args], DISCOVERY_TIMEOUT)
    if result.returncode:
        raise RuntimeError(f"test discovery failed ({result.returncode}): {result.stderr}")
    found = []
    for line in result.stdout.splitlines():
        if not line:
            continue
        if not line.endswith(': test'):
            raise RuntimeError(f'unexpected libtest discovery output: {line!r}')
        found.append(line[:-6])
    if len(found) != len(set(found)):
        raise RuntimeError('duplicate libtest names')
    return found


def resolve_resource_environment(names):
    """Bind declared single-path resources to this Buck launch directory."""
    selected = {}
    for name in names:
        value = os.environ.get(name)
        if not value:
            raise RuntimeError(f'declared resource environment is missing: {name}')
        # Keep Buck's logical artifact/symlink path. Resource consumers retain
        # their own authority checks; the runner only supplies an absolute path.
        path = Path(value).absolute()
        if not path.exists():
            raise RuntimeError(f'declared resource does not exist: {name}={path}')
        selected[name] = str(path)
    os.environ.update(selected)


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=lambda value: str(Path(value).resolve(strict=True)))
    parser.add_argument('--resource-env', action='append', default=[], metavar='NAME',
                        help='resolve this declared single-path environment input before any test process')
    parser.add_argument(
        '--exact', action='append', dest='exact_names', metavar='TEST_NAME',
        help='run this exact fully qualified test name; may be repeated',
    )
    parser.add_argument(
        '--expected-count', type=int,
        help='require exactly this many selected tests before execution',
    )
    parser.add_argument(
        '--ignored', action='store_true',
        help='select ignored tests (exact names must all be ignored)',
    )
    parser.add_argument(
        '--timeout', type=float, default=DEFAULT_TIMEOUT,
        help=f'maximum seconds per test process (default: {DEFAULT_TIMEOUT})',
    )
    parser.add_argument(
        '--case-timeout', action='append', default=[], metavar='NAME=SECONDS',
        help='override the timeout of a selected exact test name; may be repeated',
    )
    parser.add_argument(
        '--jobs', type=int,
        help='maximum concurrent test processes (focused selection defaults to 1)',
    )
    parser.add_argument('--delegated-service', action='store_true',
                        help='run each test alone in a fresh delegated user service')
    parser.add_argument('--service-slice',
                        help='delegated user service slice (default: app.slice)')
    parser.add_argument('--output-dir', type=Path,
                        help='retain bounded stdout/stderr and outcome records for every test')
    options = parser.parse_args(argv)
    if (len(options.resource_env) != len(set(options.resource_env))
            or any(not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', name)
                   for name in options.resource_env)):
        parser.error('--resource-env names must be valid and unique')
    if options.service_slice is not None and not options.delegated_service:
        parser.error('--service-slice requires --delegated-service')
    if options.delegated_service:
        options.service_slice = options.service_slice or 'app.slice'
        if not re.fullmatch(r'[A-Za-z0-9_.@:-]+\.slice', options.service_slice):
            parser.error('--service-slice must name one systemd slice')
    if not math.isfinite(options.timeout) or options.timeout <= 0:
        parser.error('--timeout must be positive and finite')
    case_timeouts = {}
    for override in options.case_timeout:
        name, separator, value = override.partition('=')
        try:
            seconds = float(value)
        except ValueError:
            parser.error('--case-timeout must be NAME=SECONDS with positive finite seconds')
        if not name or not separator or not math.isfinite(seconds) or seconds <= 0:
            parser.error('--case-timeout must be NAME=SECONDS with positive finite seconds')
        if name in case_timeouts:
            parser.error('--case-timeout names must be unique')
        case_timeouts[name] = seconds
    options.case_timeouts = case_timeouts
    if options.jobs is not None and options.jobs <= 0:
        parser.error('--jobs must be positive')
    if options.expected_count is not None and options.expected_count <= 0:
        parser.error('--expected-count must be positive')
    if options.exact_names and options.expected_count is None:
        parser.error('--expected-count is required with --exact')
    if options.exact_names and len(options.exact_names) != len(set(options.exact_names)):
        parser.error('--exact names must be unique')
    return options


def select_tests(all_names, ignored_names, options):
    if not ignored_names.issubset(all_names):
        raise RuntimeError('ignored tests missing from discovery')
    if options.exact_names is not None:
        missing = [name for name in options.exact_names if name not in all_names]
        if missing:
            raise RuntimeError(f'exact test names not discovered: {missing!r}')
        selected = list(options.exact_names)
    elif options.ignored:
        selected = [name for name in all_names if name in ignored_names]
    else:
        selected = [name for name in all_names if name not in ignored_names]

    wrong_ignored_mode = [
        name for name in selected
        if (name in ignored_names) != options.ignored
    ]
    if wrong_ignored_mode:
        mode = 'ignored' if options.ignored else 'nonignored'
        raise RuntimeError(f'exact test names are not all {mode}: {wrong_ignored_mode!r}')
    if not selected:
        raise RuntimeError('no tests selected')
    if options.expected_count is not None and len(selected) != options.expected_count:
        raise RuntimeError(
            f'expected {options.expected_count} selected tests, discovered {len(selected)}'
        )
    unknown_timeouts = sorted(set(options.case_timeouts) - set(selected))
    if unknown_timeouts:
        raise RuntimeError(f'case timeout names are not selected: {unknown_timeouts!r}')
    return selected


def record_actual_counts(record, stdout, delegated=False):
    if record is None:
        return
    if isinstance(stdout, bytes):
        stdout = stdout.decode(errors='replace')
    actual = list(EXECUTION_RESULT.finditer(stdout or ''))
    if actual:
        passed, failed, ignored = map(int, actual[-1].groups())
        record.update(executed_test_count=passed + failed,
                      passed_test_count=passed, failed_test_count=failed,
                      ignored_test_count=ignored)
        if delegated and passed + failed > 0:
            record['process_execution_count'] = 1


def run_one(binary, name, ignored, timeout, record=None, service_slice=None):
    args = [binary, '--exact', name, '--nocapture']
    if ignored:
        args.append('--ignored')
    started = time.monotonic_ns()
    if record is not None:
        record.update(command=args, started_ns=started, exit_code=None,
                      timeout_seconds=timeout,
                      executed_test_count=None, passed_test_count=None,
                      failed_test_count=None, process_execution_count=0)
    service_record = {} if service_slice is not None else None
    if record is not None and service_record is not None:
        record['delegated_service'] = service_record
    try:
        result = (execute(args, timeout, service_slice, service_record)
                  if service_slice is not None else execute(args, timeout))
    except subprocess.TimeoutExpired as error:
        if record is not None:
            record.update(status='timeout', process_execution_count=(None if service_record is not None else 1),
                          elapsed_ns=time.monotonic_ns() - started)
        stdout = error.output or ''
        stderr = error.stderr or ''
        if isinstance(stdout, bytes):
            stdout = stdout.decode(errors='replace')
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors='replace')
        record_actual_counts(record, stdout, service_record is not None)
        detail = f'timed out after {timeout:g}s'
        if stdout:
            detail += f'\n{stdout}'
        return False, detail, stderr
    except RunnerInterrupted as error:
        if record is not None:
            record.update(status='interrupted', interrupt_signal=error.signum,
                          process_execution_count=None, elapsed_ns=time.monotonic_ns() - started)
        return False, f'interrupted by signal {error.signum}', ''
    except OSError as error:
        if record is not None:
            record.update(status='spawn_failed', elapsed_ns=time.monotonic_ns() - started)
        return False, f'could not start test process: {error}', ''
    if record is not None:
        record.update(status='finished', process_execution_count=(None if service_record is not None else 1),
                      exit_code=result.returncode, elapsed_ns=time.monotonic_ns() - started)
    record_actual_counts(record, result.stdout, service_record is not None)
    summaries = list(RESULT.finditer(result.stdout))
    summary = summaries[-1] if summaries else None
    passed = (
        result.returncode == 0
        and summary is not None
        and summary.groups() == ('1', '0', '0')
        and (service_record is None or service_record.get('cleanup_confirmed') is True)
    )
    stderr = result.stderr
    if service_record is not None and not service_record.get('cleanup_confirmed'):
        stderr += '\n' + service_record.get('cleanup_error', 'delegated cleanup is unconfirmed')
    return passed, result.stdout, stderr



def retain_output(directory, name, passed, stdout, stderr, record=None):
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    key = hashlib.sha256(name.encode()).hexdigest()
    streams = {}
    for label, text in (("stdout", stdout), ("stderr", stderr)):
        data = text.encode()
        path = directory / f"{key}.{label}.log"
        path.write_bytes(data[:OUTPUT_LIMIT])
        streams[label] = {
            "path": path.name,
            "bytes": len(data),
            "retained_bytes": min(len(data), OUTPUT_LIMIT),
            "sha256": hashlib.sha256(data).hexdigest(),
            "truncated": len(data) > OUTPUT_LIMIT,
        }
    (directory / f"{key}.json").write_text(json.dumps({
        "version": 2, "test": name, "passed": passed, "streams": streams,
        "execution": record,
    }, indent=2) + "\n")


def main(argv=None):
    global INTERRUPT_SIGNAL
    options = parse_args(sys.argv[1:] if argv is None else argv)
    mutation_modes = sorted(name for name in (
        'TIDEPOOL_REGEN_BRIDGED', 'TIDEPOOL_REGEN_PROTOCOL_GOLDENS',
    ) if name in os.environ)
    if mutation_modes:
        print('libtest acceptance refuses fixture regeneration: ' + ', '.join(mutation_modes), file=sys.stderr)
        return 2
    with ACTIVE_PROCESSES_LOCK:
        if ACTIVE_PROCESSES:
            raise RuntimeError('libtest runner already has active child processes')
        INTERRUPT_SIGNAL = None
    old_handlers = {}
    for signum in (signal.SIGINT, signal.SIGTERM):
        old_handlers[signum] = signal.signal(signum, _signal_active_processes)
    try:
        try:
            resolve_resource_environment(options.resource_env)
            all_names = names(options.binary)
            ignored_names = set(names(options.binary, '--ignored'))
            selected = select_tests(all_names, ignored_names, options)
        except RunnerInterrupted as error:
            print(f'libtest runner interrupted by signal {error.signum}', file=sys.stderr)
            return 128 + error.signum
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            if INTERRUPT_SIGNAL is not None:
                print(f'libtest runner interrupted by signal {INTERRUPT_SIGNAL}', file=sys.stderr)
                return 128 + INTERRUPT_SIGNAL
            print(f'libtest selection failed: {error}', file=sys.stderr)
            return 1
        if INTERRUPT_SIGNAL is not None:
            print(f'libtest runner interrupted by signal {INTERRUPT_SIGNAL}', file=sys.stderr)
            return 128 + INTERRUPT_SIGNAL

        failures = 0

        def run(name):
            if INTERRUPT_SIGNAL is not None:
                raise RunnerInterrupted(INTERRUPT_SIGNAL)
            record = {}
            outcome = run_one(
                options.binary, name, name in ignored_names,
                options.case_timeouts.get(name, options.timeout), record,
                options.service_slice if options.delegated_service else None,
            )
            return name, outcome, record

        def retain_result(name, outcome):
            nonlocal failures
            _, (passed, output, stderr), record = outcome
            if options.output_dir is not None:
                retain_output(options.output_dir, name, passed, output, stderr, record)
            print(f'{"PASS" if passed else "FAIL"} {name}', flush=True)
            if not passed:
                failures += 1
                if output:
                    print(output, end='' if output.endswith('\n') else '\n')
                if stderr:
                    print(stderr, end='' if stderr.endswith('\n') else '\n', file=sys.stderr)

        jobs = options.jobs
        if jobs is None:
            jobs = 1 if options.exact_names else min(8, os.cpu_count() or 1)
        jobs = min(jobs, len(selected))
        pool = concurrent.futures.ThreadPoolExecutor(max_workers=jobs)
        pending = {}
        next_test = 0
        interrupted = None
        try:
            # Keep at most `jobs` futures outstanding so cancellation cannot
            # start a large queued tail after the runner receives a signal.
            while next_test < len(selected) and len(pending) < jobs:
                name = selected[next_test]
                pending[pool.submit(run, name)] = name
                next_test += 1
            while pending:
                if INTERRUPT_SIGNAL is not None:
                    interrupted = INTERRUPT_SIGNAL
                    for future in pending:
                        future.cancel()
                    break
                completed, _ = wait(pending, return_when=FIRST_COMPLETED)
                for future in completed:
                    name = pending.pop(future)
                    try:
                        retain_result(name, future.result())
                    except RunnerInterrupted:
                        interrupted = INTERRUPT_SIGNAL or signal.SIGTERM
                        continue
                if interrupted is not None or INTERRUPT_SIGNAL is not None:
                    interrupted = interrupted or INTERRUPT_SIGNAL
                    for future in pending:
                        future.cancel()
                    break
                while next_test < len(selected) and len(pending) < jobs:
                    name = selected[next_test]
                    pending[pool.submit(run, name)] = name
                    next_test += 1
        finally:
            for future in pending:
                future.cancel()
            pool.shutdown(wait=True, cancel_futures=True)
            for future, name in pending.items():
                if future.cancelled():
                    continue
                try:
                    retain_result(name, future.result())
                except RunnerInterrupted:
                    interrupted = INTERRUPT_SIGNAL or signal.SIGTERM
        if interrupted is not None:
            print(f'libtest runner interrupted by signal {interrupted}', file=sys.stderr)
            return 128 + interrupted

        passed = len(selected) - failures
        print(
            f'Isolated libtest: {passed} passed; {failures} failed; '
            f'{len(ignored_names)} ignored in binary',
            flush=True,
        )
        return int(failures != 0)
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)


if __name__ == '__main__':
    sys.exit(main())
