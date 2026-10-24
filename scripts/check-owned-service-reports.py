#!/usr/bin/env python3
"""Check Rust-emitted service reports through the isolated libtest consumer."""
import argparse
import importlib.util
import json
from pathlib import Path


def check_reports(fixtures):
    fixtures = fixtures.resolve(strict=True)
    if not fixtures.name.startswith('owned-compiler-control-'):
        raise ValueError('fixture directory must use the runner owned-compiler-control- prefix')
    runner_path = Path(__file__).resolve().parents[1] / 'build/rust/isolated-libtest.py'
    spec = importlib.util.spec_from_file_location('service_report_runner_check', runner_path)
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    expected = {
        'complete': ('exited', 0, True),
        'failed-action': ('exited', 17, True),
        'startup-failed': ('failed', None, True),
        'source-retained': ('exited', 0, False),
        'unknown': ('exited', 0, False),
    }
    summaries = runner.diagnostic_summaries(fixtures.parent)
    if summaries['issues']:
        raise AssertionError(summaries['issues'])
    observed = {Path(root['path']): root for root in summaries['owned_compiler_roots']}
    expected_paths = {fixtures / name for name in expected}
    if set(observed) != expected_paths:
        raise AssertionError(f'expected five exact Rust fixture owners: {set(observed)}')
    for name, (action_status, code, complete) in expected.items():
        owner = fixtures / name
        summary = observed[owner]
        report = summary['owned_service']
        if report != json.loads((owner / 'owned-service.json').read_text()):
            raise AssertionError(f'runner changed the emitted observations: {name}')
        if (report['action']['status'], report['action']['exit_code'], report['complete']) != (
                action_status, code, complete):
            raise AssertionError(f'wrong independent action/settlement observations: {name}')
        if summary['owned_cleanup_status'] != ('confirmed' if complete else 'unconfirmed'):
            raise AssertionError(f'runner collapsed action and settlement: {name}')
        if summary['authority'] is not False or summary['role'] != 'native_test_control':
            raise AssertionError(f'report acquired authority in diagnostics: {name}')
    return {'checked_reports': len(expected), 'fixtures': str(fixtures)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=Path)
    args = parser.parse_args()
    print(json.dumps(check_reports(args.fixtures), sort_keys=True))
