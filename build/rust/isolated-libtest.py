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
import shutil
import signal
import stat
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


def confirm_process_group_cleanup(pgid):
    _kill_group(pgid)
    deadline = time.monotonic() + 5
    while True:
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


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


# Only declared runtime selections, standard test campaign controls and
# user-service coordinates cross this boundary. Provider credentials remain in the user's own authentication owner.
DELEGATED_ENVIRONMENT = (
    'PATH', 'HOME', 'USER', 'LOGNAME', 'XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS',
    'LD_LIBRARY_PATH', 'TMPDIR', 'TMP', 'TEMP', 'TEMPDIR',
    'TIDEPOOL_EXTRACT', 'TIDEPOOL_EXTRACT_WORKER', 'TIDEPOOL_COMPILER_DEPLOYMENT',
    'TIDEPOOL_PRELUDE_DIR', 'TIDEPOOL_GHC_LIBDIR', 'GHC_LIBDIR',
    'TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES', 'TIDEPOOL_EXTRACT_DAEMON_SOCKET',
    'TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT',
    'TIDEPOOL_EXTRACT_DAEMON_LOG', 'TIDEPOOL_KEEP_TEST_LOGS',
    'TIDEPOOL_TEST_ARTIFACT_ROOT', 'TIDEPOOL_TEST_DIAGNOSTIC_SCOPE',
    'TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS', 'TIDEPOOL_TIMING', 'TIDEPOOL_MEMO_TRACE',
    'TIDEPOOL_ASYNC_LAYOUT_DIAGNOSTICS',
    'TIDEPOOL_TEST_BASH', 'TIDEPOOL_TEST_SLEEP', 'TIDEPOOL_BROWSER_NODE',
    'TIDEPOOL_TEST_SYSTEMD_RUN', 'TIDEPOOL_TEST_SYSTEMCTL',
    'TIDEPOOL_BROWSER_DRIVER', 'EXOMONAD_EMBEDDED_ASSET_ROOT',
    'EXOMONAD_WORKSPACE_GITLINK', 'EXOMONAD_WORKSPACE_GIT_BUNDLE',
    'EXOMONAD_NIX_BIN', 'EXOMONAD_NIX_OFFLINE',
    'PLAYWRIGHT_BROWSERS_PATH', 'TIDEPOOL_M3_FIXTURE_DIR',
    'TIDEPOOL_FREER_RESUME_FIXTURE_DIR', 'TIDEPOOL_FREER_RETENTION_FIXTURE_DIR',
    # proptest 1.11 Config::contextualize_config owns these standard controls.
    'PROPTEST_CASES', 'PROPTEST_RNG_SEED', 'PROPTEST_RNG_ALGORITHM',
    'PROPTEST_MAX_LOCAL_REJECTS', 'PROPTEST_MAX_GLOBAL_REJECTS',
    'PROPTEST_MAX_FLAT_MAP_REGENS', 'PROPTEST_MAX_SHRINK_TIME',
    'PROPTEST_MAX_SHRINK_ITERS', 'PROPTEST_MAX_DEFAULT_SIZE_RANGE',
    'PROPTEST_FORK', 'PROPTEST_TIMEOUT', 'PROPTEST_VERBOSE',
    'PROPTEST_DISABLE_FAILURE_PERSISTENCE',
)


def delegated_tools(environment):
    """Bind both manager tools once; declared tools never fall back to PATH."""
    selections = {'systemd-run': 'TIDEPOOL_TEST_SYSTEMD_RUN',
                  'systemctl': 'TIDEPOOL_TEST_SYSTEMCTL'}
    declared = [key in environment for key in selections.values()]
    if any(declared) and not all(declared):
        raise OSError('delegated runner requires both declared systemd tools')
    tools = {}
    for executable, key in selections.items():
        if not any(declared):
            # Frozen qualification supplies its verified runtime-tools PATH.
            tools[executable] = executable
            continue
        path = Path(environment[key])
        if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
            raise OSError(f'declared delegated tool is not an absolute executable: {key}={path}')
        tools[executable] = str(path.resolve(strict=True))
    return tools


def delegated_command(args, timeout, service_slice, record, environment=None, declared_resources=()):
    unit = 'tidepool-libtest-' + uuid.uuid4().hex + '.service'
    for key in ('manager_admission', 'manager_wait_success', 'launcher_wait_exit_code',
                'admission_observer_stopped', 'cleanup_error'):
        record.pop(key, None)
    child_environment = os.environ if environment is None else environment
    tools = delegated_tools(child_environment)
    environment_names = sorted(set(DELEGATED_ENVIRONMENT).union(declared_resources).intersection(child_environment))
    # Keep failed units loaded until the observer captures their invocation;
    # successful units are fenced by --wait before systemd unloads them.
    command = [
        tools['systemd-run'], '--user', '--pipe', '--wait',
        '--service-type=exec', '--property=Delegate=yes',
        '--property=KillMode=control-group', '--property=TimeoutStopSec=5s',
        f'--property=RuntimeMaxSec={timeout:g}s', '--slice=' + service_slice,
        '--unit=' + unit, '--working-directory=' + os.getcwd(),
    ]
    command.extend('--setenv=' + key + '=' + child_environment[key] for key in environment_names)
    command.extend(['--', *args])
    record.update(unit=unit, service_slice=service_slice,
                  environment_names=environment_names, systemd_tools=tools,
                  cleanup_confirmed=False)
    return command, unit


def observe_delegated_admission(unit, record, finished, final=False):
    """A registered transient invocation proves this launch reached the manager."""
    first = True
    while first or (not final and not finished.is_set()):
        first = False
        try:
            observed = subprocess.run([
                record.get('systemd_tools', {}).get('systemctl', 'systemctl'),
                '--user', 'show', '--property=Id',
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
        if final:
            return
        finished.wait(0.05)


def stop_delegated_service(unit, record):
    """Stop only this launch's service, including descendants outside client PGID."""
    base = [record.get('systemd_tools', {}).get('systemctl', 'systemctl'), '--user']
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
        reset_failed_exit_code = None
        if (observed.returncode == 0 and state.get('LoadState') == 'loaded'
                and state.get('ActiveState') == 'failed'):
            reset = subprocess.run([*base, 'reset-failed', unit], capture_output=True,
                                   text=True, timeout=5, check=False)
            reset_failed_exit_code = reset.returncode
            if reset.returncode == 0:
                observed = subprocess.run([
                    *base, 'show', '--property=LoadState', '--property=ActiveState', unit,
                ], capture_output=True, text=True, timeout=5, check=False)
                state = dict(line.split('=', 1) for line in observed.stdout.splitlines() if '=' in line)
        wait_fenced = record.get('manager_wait_success') is True
        service_stopped = (
            observed.returncode == 0 and state.get('LoadState') == 'not-found'
            and (wait_fenced or (stopped.returncode == 0
                                 and reset_failed_exit_code in (None, 0)))
        ) or (
            stopped.returncode == 0 and observed.returncode == 0
            and state.get('LoadState') == 'loaded' and state.get('ActiveState') == 'inactive'
        )
        record.update(stop_exit_code=stopped.returncode,
                      observation_exit_code=observed.returncode, state=state,
                      reset_failed_exit_code=reset_failed_exit_code,
                      cleanup_confirmed=((bool(record.get('manager_admission')) or wait_fenced)
                                         and record.get('admission_observer_stopped', True)
                                         and service_stopped))
        if not record['cleanup_confirmed']:
            record['cleanup_error'] = ('delegated launch admission is unknown; absent unit does not fence a queued start'
                if not (record.get('manager_admission') or record.get('manager_wait_success') is True)
                else 'exact delegated service is not confirmed stopped')
    except (OSError, subprocess.SubprocessError) as error:
        record.update(cleanup_confirmed=False, cleanup_error=str(error))


def execute(args, timeout, service_slice=None, service_record=None, environment=None, declared_resources=()):
    """Run a command in its own process group, reaping it even after timeout."""
    if INTERRUPT_SIGNAL is not None:
        error = RunnerInterrupted(INTERRUPT_SIGNAL)
        error.process_cleanup_status = 'not_started'
        raise error
    command, unit = args, None
    try:
        if service_slice is not None:
            command, unit = delegated_command(args, timeout, service_slice, service_record, environment, declared_resources)
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors='replace',
            start_new_session=True,
            env=environment,
        )
    except OSError as error:
        error.process_cleanup_status = 'not_started'
        raise
    process._tidepool_delegated = unit is not None
    observer, observer_finished = None, threading.Event()
    failure = None
    try:
        _register_process(process)
        if unit is not None:
            observer = threading.Thread(target=observe_delegated_admission,
                args=(unit, service_record, observer_finished), daemon=True)
            observer.start()
        stdout, stderr = process.communicate(timeout=timeout)
        if unit is not None:
            # A successful --wait fences a queued start even when a fast
            # successful unit unloaded before InvocationID polling saw it.
            # Failed units remain loaded without --collect until exact cleanup
            # resets their failure; nonzero service status is not a wait fence.
            service_record.update(launcher_wait_exit_code=process.returncode,
                                  manager_wait_success=process.returncode == 0)
    except subprocess.TimeoutExpired as error:
        stdout, stderr = _kill_and_reap(process)
        failure = subprocess.TimeoutExpired(
            args, timeout, output=stdout or error.output, stderr=stderr or error.stderr
        )
    except BaseException as error:
        _kill_and_reap(process)
        failure = error
    finally:
        _unregister_process(process)
        if unit is not None:
            observer_finished.set()
            if observer is not None:
                observer.join(timeout=2)
            service_record['admission_observer_stopped'] = observer is None or not observer.is_alive()
            if (not service_record.get('manager_admission')
                    and service_record.get('manager_wait_success') is not True):
                # The polling observer can finish just before systemd retains
                # a fast failed unit. Query once synchronously before stop or
                # reset-failed can erase the exact invocation receipt.
                observe_delegated_admission(unit, service_record,
                                            observer_finished, final=True)
            stop_delegated_service(unit, service_record)
    # Test leaders sometimes leave a detached same-group child after a passing
    # result. The process group remains the cleanup owner through this point.
    cleanup_confirmed = (confirm_process_group_cleanup(process.pid) if unit is None
                         else service_record.get('cleanup_confirmed') is True)
    if failure is not None:
        failure.cleanup_confirmed = cleanup_confirmed
        raise failure
    result = subprocess.CompletedProcess(args, process.returncode, stdout, stderr)
    result.cleanup_confirmed = cleanup_confirmed
    return result


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
    parser.add_argument('--compiler-mode', choices=('direct', 'owned-resident'), default='direct',
                        help='select an explicitly owned compiler for each isolated test')
    options = parser.parse_args(argv)
    if options.compiler_mode == 'owned-resident' and options.output_dir is None:
        parser.error('--compiler-mode owned-resident requires --output-dir')
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


def compiler_trace_context(row):
    """Resolve tracing's nearest request context without counting span copies."""
    if not isinstance(row, dict):
        raise ValueError('compiler trace row is not an object')
    contexts = row.get('spans', [])
    if not isinstance(contexts, list):
        raise ValueError('compiler trace spans are not a list')
    contexts = [*contexts, row.get('span', {}), row.get('fields', row)]
    merged = {}
    request = False
    for context in contexts:
        if not isinstance(context, dict):
            raise ValueError('compiler trace context is not an object')
        request |= context.get('name') == 'compile_request' or 'compile_request' in context
        for key in ('execution_layer', 'physical_execution', 'request_mode',
                    'compile_request', 'daemon_epoch', 'admission_id', 'request_ordinal'):
            if key in context:
                merged[key] = context[key]
    return merged, request


def diagnostic_summaries(artifact_root):
    """Keep bounded compiler outcome/timing evidence after success scratch removal."""
    def read_json(path):
        with path.open('rb') as stream:
            data = stream.read((1 << 20) + 1)
        if len(data) > 1 << 20:
            raise ValueError('diagnostic JSON exceeds one MiB summary bound')
        value = json.loads(data)
        if not isinstance(value, dict):
            raise ValueError('diagnostic JSON is not an object')
        return value

    summaries = {'transactions': [], 'issues': []}
    transaction_root = artifact_root / 'compiler-transactions'
    if transaction_root.is_symlink():
        summaries['issues'].append('compiler transaction directory must not be a symlink')
        transactions = []
    else:
        transactions = sorted(path / 'transaction.json' for path in transaction_root.glob('*')
                              if path.is_dir() or path.is_symlink())
    summaries['transaction_count'] = len(transactions)
    for path in transactions[:256]:
        try:
            if path.is_symlink() or path.parent.is_symlink():
                raise ValueError('compiler transaction marker must not be a symlink')
            report = read_json(path)
            summary = {key: report.get(key) for key in (
                'phase', 'compiler_process_success', 'original_directory',
                'artifact_bytes', 'artifact_byte_limit', 'artifact_entry_limit')}
            summary['path'] = str(path)
            if report.get('phase') != 'compiler_completed':
                summaries['issues'].append(f'{path}: compiler transaction capture is not completed')
            files = report['files']
            if not isinstance(files, list):
                raise ValueError('compiler diagnostic files are not an array')
            summary['file_count'] = len(files)
            issues = report['issues']
            if not isinstance(issues, list) or any(not isinstance(issue, str) for issue in issues):
                raise ValueError('compiler diagnostic issues are not an array')
            summary['issues'] = [str(issue)[:2048] for issue in issues[:8]]
            summary['issue_count'] = len(issues)
            summary['issues_truncated'] = len(issues) > len(summary['issues'])
            if issues:
                summaries['issues'].append(f'{path}: compiler artifact capture has {len(issues)} issue(s)')
            summaries['transactions'].append(summary)
        except (OSError, ValueError, TypeError, AttributeError, KeyError) as error:
            summaries['issues'].append(f'{path}: {error}')
    summaries['transactions_truncated'] = len(transactions) > 256
    def summarize_compiler(compiler):
        result = {'path': str(compiler), 'authority': False, 'issues': []}
        if any((compiler / filename).exists() or (compiler / filename).is_symlink()
               for filename in ('owned-compiler-outcome.json', 'lifecycle.json')):
            result['owned_cleanup_status'] = 'unknown'
        for filename, key in (('owned-compiler-outcome.json', 'owned_compiler_outcome'),
                              ('lifecycle.json', 'owned_compiler_lifecycle')):
            path = compiler / filename
            if path.exists() or path.is_symlink():
                try:
                    if path.is_symlink():
                        raise ValueError('compiler diagnostic marker must not be a symlink')
                    result[key] = read_json(path)
                    if key == 'owned_compiler_outcome':
                        cleanup = result[key].get('cleanup')
                        result['owned_cleanup_status'] = (cleanup.get('status', 'unknown')
                            if isinstance(cleanup, dict) else 'unknown')
                        if not isinstance(result['owned_cleanup_status'], str):
                            result['owned_cleanup_status'] = 'unknown'
                            raise ValueError('owned compiler cleanup status is not a string')
                except (OSError, ValueError, TypeError, AttributeError) as error:
                    result['issues'].append(f'{path}: {error}')
        if ('owned_compiler_outcome' in result) != ('owned_compiler_lifecycle' in result):
            result['issues'].append(f'incomplete owned compiler markers: {compiler}')
        trace = compiler / 'compiler.jsonl'
        if not trace.exists():
            result['physical_compiler_timing'] = {
                'records': [], 'physical_record_count': 0, 'physical_request_count': 0,
                'retained_record_count': 0, 'data_status': 'trace_absent', 'complete': False,
            }
            result['compiler_job_queue'] = {'records': [], 'data_status': 'trace_absent', 'complete': False}
            return result
        physical, retained_bytes, examined_bytes, malformed, total = [], 0, 0, 0, 0
        unknown, physical_requests, physical_jobs = 0, set(), set()
        queued, queue_bytes, queue_rows, queue_unknown = {}, 0, 0, 0
        try:
            if trace.is_symlink():
                raise ValueError('compiler diagnostic trace must not be a symlink')
            with trace.open('rb') as stream:
                while True:
                    line = stream.readline((1 << 20) + 1)
                    if not line:
                        break
                    examined_bytes += len(line)
                    if examined_bytes > 32 << 20 or len(line) > 1 << 20:
                        malformed += 1
                        break
                    try:
                        row = json.loads(line)
                        context, request = compiler_trace_context(row)
                        fields = row.get('fields', row)
                        if fields.get('phase') == 'compiler_queue':
                            queue_rows += 1
                            epoch, admission, elapsed = (fields.get(key) for key in
                                                         ('daemon_epoch', 'admission_id', 'queue_ms'))
                            if (not isinstance(epoch, str) or not epoch
                                    or type(admission) is not int or admission < 0
                                    or type(elapsed) not in (int, float)
                                    or not math.isfinite(elapsed) or elapsed < 0):
                                queue_unknown += 1
                            else:
                                job = (epoch, admission)
                                if job in queued:
                                    if queued[job]['queue_ms'] != elapsed:
                                        queue_unknown += 1
                                elif len(queued) < 256 and queue_bytes + len(line) <= 64 << 10:
                                    queued[job] = {'daemon_epoch': epoch, 'admission_id': admission,
                                                   'queue_ms': elapsed, 'row': row}
                                    queue_bytes += len(line)
                                else:
                                    queue_unknown += 1
                        layer = context.get('execution_layer')
                        if layer == 'physical' and context.get('physical_execution'):
                            total += 1
                            physical_requests.add(str(context['physical_execution']))
                            if context.get('daemon_epoch') is not None and context.get('admission_id') is not None:
                                physical_jobs.add((context['daemon_epoch'], context['admission_id']))
                            if retained_bytes + len(line) <= 256 << 10 and len(physical) < 512:
                                physical.append({**row, 'physical_context': context})
                                retained_bytes += len(line)
                        elif request and layer not in ('endpoint_submission', 'transaction_wrapper'):
                            unknown += 1
                    except (ValueError, TypeError, AttributeError):
                        malformed += 1
        except (OSError, ValueError) as error:
            malformed += 1
            result['issues'].append(f'{trace}: {error}')
        scanned = examined_bytes <= 32 << 20 and malformed == 0
        result['physical_compiler_timing'] = {
            'records': physical, 'physical_record_count': total,
            'physical_request_count': len(physical_requests),
            'unclassified_request_records': unknown,
            'retained_record_count': len(physical), 'malformed_lines': malformed,
            'data_status': 'observed' if total else 'no_identified_physical_requests',
            'complete': scanned and unknown == 0 and total > 0 and total == len(physical),
            'trace_bytes': trace.stat().st_size,
        }
        result['compiler_job_queue'] = {
            'records': list(queued.values()), 'observed_record_count': queue_rows,
            'retained_job_count': len(queued), 'unclassified_record_count': queue_unknown,
            'physical_job_count': len(physical_jobs),
            'physical_jobs_without_queue': len(physical_jobs.difference(queued)),
            'data_status': 'observed' if queue_rows else 'no_observed_job_queue',
            'complete': (scanned and queue_unknown == 0 and queue_rows > 0
                         and physical_jobs.issubset(queued)),
        }
        return result

    roots = []
    compiler = artifact_root / 'compiler'
    if transactions or compiler.exists():
        roots.append((compiler, 'case_compiler'))
    # The native lifecycle control creates one immediate test-data directory.
    # Only its immediate marker-bearing compiler children are considered; no
    # source trees or arbitrary recursive paths become compiler roots.
    for index, control in enumerate(artifact_root.glob('owned-compiler-control-*')):
        if index >= 64:
            summaries['issues'].append('owned compiler control directory bound exceeded')
            break
        if control.is_symlink() or not control.is_dir():
            summaries['issues'].append(f'unsafe owned compiler control directory: {control}')
            continue
        for child_index, candidate in enumerate(control.iterdir()):
            if child_index >= 32:
                summaries['issues'].append(f'owned compiler control child bound exceeded: {control}')
                break
            if candidate.is_symlink() or not candidate.is_dir():
                continue
            markers = [(candidate / filename).exists() or (candidate / filename).is_symlink() for filename in
                       ('owned-compiler-outcome.json', 'lifecycle.json')]
            if any(markers):
                if not all(markers):
                    summaries['issues'].append(f'incomplete owned compiler markers: {candidate}')
                if len(roots) < 8:
                    roots.append((candidate, 'native_test_control'))
                else:
                    summaries['issues'].append('owned compiler diagnostic root bound exceeded')
    summaries['owned_compiler_roots'] = []
    for compiler, role in roots:
        if compiler.is_symlink():
            summaries['issues'].append(f'unsafe compiler diagnostic directory: {compiler}')
            continue
        result = summarize_compiler(compiler)
        result['role'] = role
        summaries['owned_compiler_roots'].append(result)
        if role == 'case_compiler':
            summaries.update({key: value for key, value in result.items()
                              if key not in ('path', 'authority', 'issues', 'role')})
        summaries['issues'].extend(result['issues'])
    return summaries


def launch_file_identity(path):
    """Hash the selected file before launch, fencing replacement during capture."""
    selected = Path(path).absolute()
    with selected.open('rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode):
            raise RuntimeError(f'launch input is not a regular file: {selected}')
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        after = os.fstat(stream.fileno())
    current = selected.stat()
    def stamp(value):
        return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
    if stamp(before) != stamp(after) or stamp(after) != stamp(current):
        raise RuntimeError(f'launch input changed during hash capture: {selected}')
    return {'status': 'captured_before_execution', 'path': str(selected),
            'sha256': digest, 'bytes': before.st_size, 'device': before.st_dev,
            'inode': before.st_ino, 'mtime_ns': before.st_mtime_ns, 'ctime_ns': before.st_ctime_ns}


def capture_launch_inputs(binary, environment, declared_resources):
    started = time.monotonic_ns()
    resources = {}
    result = {'schema': 1, 'capture_started_ns': started, 'executable': None,
              'resources': resources, 'status': 'UNKNOWN',
              'resource_selection': 'declared resource environment plus explicitly bound frontend/worker'}
    try:
        result['executable'] = launch_file_identity(binary)
        names = set(declared_resources).union(
            name for name in ('TIDEPOOL_EXTRACT', 'TIDEPOOL_EXTRACT_WORKER') if environment.get(name))
        for name in sorted(names):
            value = environment.get(name)
            if not value:
                raise RuntimeError(f'declared launch input is missing: {name}')
            path = Path(value).absolute()
            if path.is_dir():
                resources[name] = {'path': str(path), 'status': 'UNKNOWN',
                                   'reason': 'directory contents require their owning resource manifest'}
            else:
                resources[name] = launch_file_identity(path)
        for reference in [result['executable'], *resources.values()]:
            if reference['status'] == 'UNKNOWN':
                continue
            current = Path(reference['path']).stat()
            observed = (current.st_dev, current.st_ino, current.st_size, current.st_mtime_ns, current.st_ctime_ns)
            captured = tuple(reference[key] for key in ('device', 'inode', 'bytes', 'mtime_ns', 'ctime_ns'))
            if observed != captured:
                raise RuntimeError(f"launch input changed before capture completed: {reference['path']}")
        result['status'] = 'captured_before_execution'
        result['resource_hashes_complete'] = all(value['status'] != 'UNKNOWN' for value in resources.values())
    except (OSError, RuntimeError) as error:
        result['error'] = str(error)
    result['capture_finished_ns'] = time.monotonic_ns()
    result['compiler_bindings'] = {name: resources.get(name, {}).get('status', 'UNKNOWN')
                                   for name in ('TIDEPOOL_EXTRACT', 'TIDEPOOL_EXTRACT_WORKER')}
    return result


def process_cleanup_status(result, service_record=None):
    status = getattr(result, 'process_cleanup_status', None)
    if status in ('not_started', 'confirmed', 'unconfirmed', 'unknown'):
        return status
    confirmed = getattr(result, 'cleanup_confirmed', None)
    if service_record is not None:
        service_confirmed = service_record.get('cleanup_confirmed')
        if confirmed is False or service_confirmed is False:
            return 'unconfirmed'
        if confirmed is not True or service_confirmed is not True:
            return 'unknown'
    return 'confirmed' if confirmed is True else 'unconfirmed' if confirmed is False else 'unknown'


def case_artifact_evidence(artifact_root, compiler_mode, record):
    """Capture diagnostic completeness and each resource owner on every exit path."""
    cleanup_complete = True
    stderr = ""
    hosted_runtime_not_started = False
    if artifact_root is not None:
        reports = sorted(path / 'hosted-outcome.json' for path in artifact_root.glob('hosted-campaign-*')
                         if path.is_dir() or path.is_symlink())
        if compiler_mode == 'owned-resident':
            reports.append(artifact_root / 'compiler/owned-compiler-outcome.json')
        cleanup_reports = []
        for report in reports:
            outcome = {}
            try:
                if report.is_symlink() or report.parent.is_symlink():
                    raise ValueError('cleanup marker must not be a symlink')
                outcome = json.loads(report.read_text())
                status = outcome['cleanup']['status']
                if not isinstance(status, str):
                    raise ValueError('cleanup status is not a string')
                # This is a host-runtime admission fact, not a teardown receipt.
                # The passing libtest and enclosing process/service cleanup are
                # still mandatory; retain the original refusal evidence below.
                not_started = (
                    report.name == 'hosted-outcome.json'
                    and type(outcome.get('schema')) is int
                    and outcome['schema'] in (1, 2)
                    and outcome.get('scenario', {}).get('status') == 'failed'
                    and outcome.get('scenario', {}).get('phase') == 'startup'
                    and outcome['cleanup'].get('executor_joined') is True
                    and outcome['cleanup'] == {
                        'status': 'not_started', 'domain': 'host_runtime',
                        'owner_admission': 'not_admitted', 'executor_joined': True,
                    }
                )
            except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
                status = 'unknown'
                not_started = False
                stderr += f'\ncleanup report unavailable: {report}: {error}'
            cleanup_reports.append({'path': str(report), 'status': status,
                                    'domain': ('host_runtime' if report.name == 'hosted-outcome.json'
                                               else 'compiler_runtime'),
                                    'host_runtime_not_started': not_started,
                                    'startup': outcome.get('startup') if isinstance(outcome, dict) else None})
            hosted_runtime_not_started |= not_started
            if status != 'confirmed' and not not_started:
                cleanup_complete = False
                stderr += f'\ncleanup remains {status}: {report}'
        record['cleanup_reports'] = cleanup_reports
        for domain, key in (('host_runtime', 'hosted_cleanup_status'),
                            ('compiler_runtime', 'compiler_cleanup_status')):
            reports = [report for report in cleanup_reports if report['domain'] == domain]
            statuses = set('not_started' if report['host_runtime_not_started'] else
                           'unconfirmed' if report['status'] == 'not_started' else report['status']
                           for report in reports)
            record[key] = ('not_observed' if not reports else
                           next(iter(statuses)) if len(statuses) == 1 else 'mixed')
        try:
            record['diagnostic_summaries'] = diagnostic_summaries(artifact_root)
        except (OSError, ValueError, TypeError) as error:
            record['diagnostic_summaries'] = {'issues': [str(error)]}
    summaries = record.get('diagnostic_summaries', {})
    compiler_statuses = {root['owned_cleanup_status'] for root in summaries.get('owned_compiler_roots', [])
                         if 'owned_cleanup_status' in root}
    if record['compiler_cleanup_status'] != 'not_observed':
        compiler_statuses.add(record['compiler_cleanup_status'])
    if compiler_statuses:
        record['compiler_cleanup_status'] = (next(iter(compiler_statuses))
                                             if len(compiler_statuses) == 1 else 'mixed')
    for root in summaries.get('owned_compiler_roots', []):
        status = root.get('owned_cleanup_status')
        if status is not None and status != 'confirmed':
            cleanup_complete = False
            stderr += f"\nowned compiler cleanup remains {status}: {root['path']}"
    evidence_complete = (not summaries.get('issues')
                         and not summaries.get('transactions_truncated')
                         and summaries.get('physical_compiler_timing', {}).get('complete', True)
                         and all(root.get('physical_compiler_timing', {}).get('complete', False)
                                 and root.get('owned_cleanup_status', 'not_required') in ('confirmed', 'not_required')
                                 and (not root.get('compiler_job_queue', {}).get('physical_job_count')
                                      or root['compiler_job_queue'].get('complete', False))
                                 for root in summaries.get('owned_compiler_roots', [])))
    record['diagnostic_evidence_complete'] = evidence_complete if artifact_root is not None else None
    return cleanup_complete, hosted_runtime_not_started, evidence_complete, stderr


def run_one(binary, name, ignored, timeout, record=None, service_slice=None,
            artifact_root=None, compiler_mode='direct', declared_resources=()):
    if record is None:
        record = {}
    record.update(schema=1, process_cleanup_status='not_started', hosted_cleanup_status='not_observed',
                  compiler_cleanup_status='not_observed', diagnostic_evidence_complete=None)
    record['process_cleanup_scope'] = 'delegated_service' if service_slice is not None else 'process_group'
    args = [binary, '--exact', name, '--nocapture']
    if ignored:
        args.append('--ignored')
    environment = None
    startup_diagnostic_seconds = os.environ.get('TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS')
    if startup_diagnostic_seconds is not None:
        record['startup_diagnostic_seconds'] = startup_diagnostic_seconds
    if artifact_root is not None:
        artifact_root = Path(artifact_root).absolute()
        artifact_root.mkdir(parents=True, exist_ok=False, mode=0o700)
        environment = dict(os.environ)
        environment.update(TIDEPOOL_TEST_ARTIFACT_ROOT=str(artifact_root),
                           TIDEPOOL_TEST_DIAGNOSTIC_SCOPE='1')
        (artifact_root / 'case.json').write_text(json.dumps({
            'schema': 1, 'test': name, 'scenario': 'running', 'process_cleanup_status': 'not_started',
            'hosted_cleanup_status': 'not_observed', 'compiler_cleanup_status': 'not_observed',
            'compiler_mode': compiler_mode,
        }, indent=2) + '\n')
    if compiler_mode == 'owned-resident':
        frontend = (environment or os.environ).get('TIDEPOOL_EXTRACT')
        if artifact_root is None or not frontend:
            raise RuntimeError('owned-resident requires a declared compiler and per-case artifact root')
        args = [frontend, '--owned-daemon-run', str(artifact_root / 'compiler'), '--', *args]
    started = time.monotonic_ns()
    record.update(command=args, started_ns=started, exit_code=None,
                  timeout_seconds=timeout,
                  executed_test_count=None, passed_test_count=None,
                  failed_test_count=None, process_execution_count=0)
    record.update(artifact_root=str(artifact_root) if artifact_root is not None else None,
                  compiler_mode=compiler_mode)
    service_record = {} if service_slice is not None else None
    if service_record is not None:
        record['delegated_service'] = service_record
    launch_inputs = capture_launch_inputs(binary, environment or os.environ, declared_resources)
    record['launch_inputs'] = launch_inputs
    if artifact_root is not None:
        (artifact_root / 'launch-inputs.json').write_text(json.dumps(launch_inputs, indent=2) + '\n')
    def finish(passed, stdout, stderr):
        cleanup_complete, hosted_runtime_not_started, evidence_complete, errors = case_artifact_evidence(
            artifact_root, compiler_mode, record)
        passed = passed and cleanup_complete
        stderr += errors
        if (passed and artifact_root is not None and evidence_complete
                and not hosted_runtime_not_started and startup_diagnostic_seconds is None):
            # Successful scenarios have completed their own acknowledged teardown.
            # The runner additionally confirms its enclosing process/service cleanup.
            shutil.rmtree(artifact_root)
            record['artifacts_removed_after_success'] = True
        return passed, stdout, stderr

    if launch_inputs['status'] == 'UNKNOWN':
        record.update(status='launch_identity_failed', elapsed_ns=time.monotonic_ns() - started)
        return finish(False, f"could not capture launch inputs: {launch_inputs['error']}", '')
    try:
        record['process_cleanup_status'] = 'unknown'
        record['execution_started_ns'] = time.monotonic_ns()
        execution_kwargs = {'environment': environment} if environment is not None else {}
        if service_slice is not None and declared_resources:
            execution_kwargs['declared_resources'] = declared_resources
        result = (execute(args, timeout, service_slice, service_record, **execution_kwargs)
                  if service_slice is not None else execute(args, timeout, **execution_kwargs))
    except subprocess.TimeoutExpired as error:
        record['process_cleanup_status'] = process_cleanup_status(error, service_record)
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
        return finish(False, detail, stderr)
    except RunnerInterrupted as error:
        record['process_cleanup_status'] = process_cleanup_status(error, service_record)
        record.update(status='interrupted', interrupt_signal=error.signum,
                      process_execution_count=None, elapsed_ns=time.monotonic_ns() - started)
        return finish(False, f'interrupted by signal {error.signum}', '')
    except OSError as error:
        record['process_cleanup_status'] = process_cleanup_status(error, service_record)
        record.update(status='spawn_failed', elapsed_ns=time.monotonic_ns() - started)
        return finish(False, f'could not start test process: {error}', '')
    except Exception as error:
        record['process_cleanup_status'] = process_cleanup_status(error, service_record)
        record.update(status='runner_failed', process_execution_count=None,
                      elapsed_ns=time.monotonic_ns() - started)
        return finish(False, f'test process observation failed: {type(error).__name__}: {error}', '')
    record.update(status='finished', process_execution_count=(None if service_record is not None else 1),
                  exit_code=result.returncode, elapsed_ns=time.monotonic_ns() - started,
                  process_cleanup_status=process_cleanup_status(result, service_record))
    record_actual_counts(record, result.stdout, service_record is not None)
    summaries = list(RESULT.finditer(result.stdout))
    summary = summaries[-1] if summaries else None
    passed = (
        result.returncode == 0
        and summary is not None
        and summary.groups() == ('1', '0', '0')
        and record['process_cleanup_status'] == 'confirmed'
        and (service_record is None or service_record.get('cleanup_confirmed') is True)
    )
    stderr = result.stderr
    if service_record is not None and not service_record.get('cleanup_confirmed'):
        stderr += '\n' + service_record.get('cleanup_error', 'delegated cleanup is unconfirmed')
    if record['process_cleanup_status'] != 'confirmed':
        stderr += '\nisolated process cleanup is unconfirmed'
    return finish(passed, result.stdout, stderr)



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
            artifact_root = (options.output_dir / hashlib.sha256(name.encode()).hexdigest() / 'artifacts'
                             if options.output_dir is not None else None)
            outcome = run_one(
                options.binary, name, name in ignored_names,
                options.case_timeouts.get(name, options.timeout), record,
                options.service_slice if options.delegated_service else None,
                artifact_root, options.compiler_mode, options.resource_env,
            )
            return name, outcome, record

        def retain_result(name, outcome):
            nonlocal failures
            _, (passed, output, stderr), record = outcome
            if options.output_dir is not None:
                retain_output(options.output_dir, name, passed, output, stderr, record)
                artifact_root = Path(record['artifact_root'])
                if artifact_root.exists():
                    (artifact_root / 'case.json').write_text(json.dumps({
                        'test': name, 'passed': passed, 'execution': record,
                        'scenario': 'failed' if record.get('failed_test_count') else record['status'],
                        'cleanup': record.get('delegated_service', {}).get('cleanup_confirmed'),
                    }, indent=2) + '\n')
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
