"""Run selected Buck-built libtest cases in fresh, bounded processes."""
import argparse
import concurrent.futures
import os
from pathlib import Path
import re
import signal
import subprocess
import sys


DEFAULT_TIMEOUT = 300
DISCOVERY_TIMEOUT = 30
RESULT = re.compile(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;")


def execute(args, timeout):
    """Run a command in its own process group, reaping it even after timeout."""
    process = subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        stdout, stderr = process.communicate()
        raise subprocess.TimeoutExpired(
            args, timeout, output=stdout or error.output, stderr=stderr or error.stderr
        ) from None
    except BaseException:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate()
        raise
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


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=lambda value: str(Path(value).resolve(strict=True)))
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
    options = parser.parse_args(argv)
    if options.timeout <= 0:
        parser.error('--timeout must be positive')
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
    return selected


def run_one(binary, name, ignored, timeout):
    args = [binary, '--exact', name, '--nocapture']
    if ignored:
        args.append('--ignored')
    try:
        result = execute(args, timeout)
    except subprocess.TimeoutExpired as error:
        detail = f'timed out after {timeout:g}s'
        if error.output:
            detail += f'\n{error.output}'
        return False, detail, error.stderr or ''
    except OSError as error:
        return False, f'could not start test process: {error}', ''
    summary = RESULT.search(result.stdout)
    passed = (
        result.returncode == 0
        and summary is not None
        and summary.groups() == ('1', '0', '0')
    )
    return passed, result.stdout, result.stderr


def main(argv=None):
    options = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        all_names = names(options.binary)
        ignored_names = set(names(options.binary, '--ignored'))
        selected = select_tests(all_names, ignored_names, options)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f'libtest selection failed: {error}', file=sys.stderr)
        return 1

    failures = 0

    def run(name):
        return name, run_one(
            options.binary, name, name in ignored_names, options.timeout
        )

    # Each selected case is a separate process; only light cases run in parallel.
    with concurrent.futures.ThreadPoolExecutor(
        max_workers=min(8, len(selected), os.cpu_count() or 1)
    ) as pool:
        for name, (passed, output, stderr) in pool.map(run, selected):
            print(f'{"PASS" if passed else "FAIL"} {name}', flush=True)
            if not passed:
                failures += 1
                if output:
                    print(output, end='' if output.endswith('\n') else '\n')
                if stderr:
                    print(stderr, end='' if stderr.endswith('\n') else '\n', file=sys.stderr)
    passed = len(selected) - failures
    print(
        f'Isolated libtest: {passed} passed; {failures} failed; '
        f'{len(ignored_names)} ignored in binary',
        flush=True,
    )
    return int(failures != 0)


if __name__ == '__main__':
    sys.exit(main())
