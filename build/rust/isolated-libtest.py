"""Run a Buck-built libtest binary with one fresh process per nonignored test."""
import concurrent.futures
import os
from pathlib import Path
import re
import subprocess
import sys


def names(binary, *args):
    result = subprocess.run([binary, '--list', '--format', 'terse', *args],
                            capture_output=True, text=True, check=True, timeout=30)
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


def main():
    binary = str(Path(sys.argv[1]).resolve(strict=True))
    all_names = names(binary)
    ignored = set(names(binary, '--ignored'))
    if not ignored.issubset(all_names):
        raise RuntimeError('ignored tests missing from discovery')
    selected = [name for name in all_names if name not in ignored]
    if not selected:
        raise RuntimeError('no runnable tests discovered')

    def run(name):
        result = subprocess.run([binary, '--exact', name, '--nocapture'],
                                capture_output=True, text=True, timeout=300)
        passed = result.returncode == 0 and re.search(
            r'test result: ok\. 1 passed; 0 failed; 0 ignored;', result.stdout)
        return name, bool(passed), result

    failures = 0
    # Build scheduling stays with Buck; this bounds only lightweight test cases.
    with concurrent.futures.ThreadPoolExecutor(max_workers=min(8, os.cpu_count() or 1)) as pool:
        for name, passed, result in pool.map(run, selected):
            print(f'{"PASS" if passed else "FAIL"} {name}', flush=True)
            if not passed:
                failures += 1
                print(result.stdout, end='')
                print(result.stderr, end='', file=sys.stderr)
    print(f'Isolated libtest: {len(selected) - failures} passed; {failures} failed; {len(ignored)} ignored', flush=True)
    return bool(failures)


if __name__ == '__main__':
    sys.exit(main())
