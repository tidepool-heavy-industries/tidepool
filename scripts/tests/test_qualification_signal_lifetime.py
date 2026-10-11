#!/usr/bin/env python3
"""Deterministically exercise qualification cancel status-check/send lifetime."""
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
QUAL = ROOT / 'build/package/qualification.py'
ISOLATED = ROOT / 'build/rust/isolated-libtest.py'


def fifo_wait(path, timeout=20):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    try:
        ready, _, _ = select.select([fd], [], [], timeout)
        if not ready:
            raise TimeoutError(f'timed out waiting for FIFO event: {path}')
        data = os.read(fd, 4096)
        if not data:
            raise RuntimeError(f'FIFO closed without event: {path}')
        return data.decode()
    finally:
        os.close(fd)


def fifo_send(path, value='go'):
    with open(path, 'w') as stream:
        stream.write(value)
        stream.flush()


def write(path, value):
    path.write_text(value)


def setup_case(root, delayed_second_cancel, fail_report=False):
    tools = root / 'tools/bin'
    tools.mkdir(parents=True)
    (tools / 'python3').symlink_to(sys.executable)
    output = root / 'output'
    descriptor_path = root / 'descriptor.json'
    receipt_path = output / 'run-owner.json'
    report_ready = root / 'report-ready.fifo'
    report_release = root / 'report-release.fifo'
    child_ready = root / 'child-ready.fifo'
    child_release = root / 'child-release.fifo'
    fake_libtest = root / 'fake-libtest'
    starts = root / 'starts.txt'
    child_pid = root / 'child.pid'
    delayed_ready = root / 'cancel-ready.fifo'
    delayed_release = root / 'cancel-release.fifo'
    for path in (report_ready, report_release, child_ready, child_release):
        os.mkfifo(path, 0o600)
    if delayed_second_cancel:
        for path in (delayed_ready, delayed_release):
            os.mkfifo(path, 0o600)
    fake_libtest.write_text(
        '#!' + sys.executable + '\n'
        'import os, subprocess, sys\n'
        "names = ['suite::complete', 'suite::active', 'suite::queued']\n"
        "if '--list' in sys.argv:\n"
        "    print(''.join(name + ': test\\n' for name in ([] if '--ignored' in sys.argv else names)))\n"
        '    raise SystemExit(0)\n'
        "name = sys.argv[sys.argv.index('--exact') + 1]\n"
        f"open({str(starts)!r}, 'a').write(name + '\\n')\n"
        "if name == 'suite::complete':\n"
        "    print('test suite::complete ... ok')\n"
        "    print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s')\n"
        '    raise SystemExit(0)\n'
        f"child = subprocess.Popen([sys.executable, '-c', {('import os; open(' + repr(str(child_ready)) + ', \'w\').write(\'ready\'); open(' + repr(str(child_release)) + ').read()')!r}], start_new_session=True)\n"
        f"open({str(child_pid)!r}, 'w').write(str(child.pid))\n"
        "open(" + repr(str(root / 'active-hold.fifo')) + ").read()\n"
    )
    os.mkfifo(root / 'active-hold.fifo', 0o600)
    fake_libtest.chmod(0o700)
    descriptor = {
        'external_inputs': {'runtime_tools': {'path': str(root / 'tools')}},
        'programs': {'runner': str(ISOLATED), 'libtest': str(fake_libtest)},
        'cohorts': {'interrupt': {'tests': ['suite::complete', 'suite::active', 'suite::queued'],
                                  'expected_count': 3, 'timeout': 60, 'ignored': False,
                                  'compiler_mode': 'direct'}},
        'source_oid': 'a' * 40, 'harness_revision': 'b' * 40,
        'profile': 'fixture', 'stdlib_mode': 'source-backed', 'startup_mode': 'prepared',
        'environment': {},
    }
    descriptor_path.write_text(json.dumps(descriptor))
    owner_helper = root / 'qualification-owner.py'
    owner_helper.write_text(
        'import importlib.util, os, signal, sys\n'
        'from pathlib import Path\n'
        'from types import SimpleNamespace\n'
        f'script = Path({str(QUAL)!r})\n'
        'spec = importlib.util.spec_from_file_location("qualification_under_test", script)\n'
        'module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)\n'
        'descriptor = __import__("json").loads(Path(sys.argv[1]).read_text())\n'
        'module.verify = lambda _: descriptor\n'
        'module.execution_environment = lambda _: dict(os.environ)\n'
        'original_write_json = module.write_json\n'
        'def gated_write_json(path, value):\n'
        f'    if Path(path).name == "report.json":\n'
        f'        with open({str(report_ready)!r}, "w") as event: event.write("ready"); event.flush()\n'
        f'        with open({str(report_release)!r}, "r") as gate: gate.read(1)\n'
        f'        if {fail_report!r}: raise OSError("fixture report publication failure")\n'
        '    return original_write_json(path, value)\n'
        'module.write_json = gated_write_json\n'
        'args = SimpleNamespace(jobs=1, service_slice=None, delegated_service=False, '
        'descriptor=Path(sys.argv[1]), cohort="interrupt", output=Path(sys.argv[2]), '
        'foreground_jobs=None, preparation_jobs=None, compiler_mode=None, trace_profile=None)\n'
        'before = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}\n'
        'code = module.run_cohort(args)\n'
        'after = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}\n'
        f'Path({str(root / "handlers-restored.json")!r}).write_text(__import__("json").dumps('
        '{str(number): before[number] == after[number] for number in before}))\n'
        'raise SystemExit(code)\n'
    )
    cancel_helper = root / 'qualification-cancel-barrier.py'
    if delayed_second_cancel:
        cancel_helper.write_text(
            'import importlib.util, os, signal, sys\n'
            'from pathlib import Path\n'
            f'script = Path({str(QUAL)!r})\n'
            'spec = importlib.util.spec_from_file_location("qualification_cancel_under_test", script)\n'
            'module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)\n'
            'actual_send = signal.pidfd_send_signal\n'
            'def delayed_send(pidfd, signum, info=None, flags=0):\n'
            f'    with open({str(delayed_ready)!r}, "w") as event: event.write("ready"); event.flush()\n'
            f'    with open({str(delayed_release)!r}, "r") as gate: gate.read(1)\n'
            '    return actual_send(pidfd, signum, info, flags)\n'
            'signal.pidfd_send_signal = delayed_send\n'
            'raise SystemExit(module.main(["cancel", sys.argv[1]]))\n'
        )
    return {
        'output': output, 'descriptor': descriptor_path, 'receipt': receipt_path,
        'child_ready': child_ready, 'child_release': child_release, 'child_pid': child_pid,
        'starts': starts, 'report_ready': report_ready, 'report_release': report_release,
        'delayed_ready': delayed_ready, 'delayed_release': delayed_release,
        'owner_helper': owner_helper, 'cancel_helper': cancel_helper,
    }


def run_case(root, delayed_second_cancel, fail_report=False):
    files = setup_case(root, delayed_second_cancel, fail_report)
    owner = subprocess.Popen([sys.executable, str(files['owner_helper']),
                              str(files['descriptor']), str(files['output'])],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                             start_new_session=True)
    delayed = None
    child = None
    try:
        fifo_wait(files['child_ready'])
        child = int(files['child_pid'].read_text())
        first = subprocess.run([sys.executable, str(QUAL), 'cancel', str(files['receipt'])],
                               capture_output=True, text=True, check=False, timeout=10)
        if first.returncode != 0 or first.stdout.strip() != 'signal_sent':
            raise AssertionError(f'first cancel failed: {first.returncode} {first.stdout!r} {first.stderr!r}')
        if delayed_second_cancel:
            delayed = subprocess.Popen([sys.executable, str(files['cancel_helper']), str(files['receipt'])],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                       start_new_session=True)
            fifo_wait(files['delayed_ready'])
            receipt_while_delayed = json.loads(files['receipt'].read_text())
            if receipt_while_delayed['status'] not in ('running', 'cancellation_requested'):
                raise AssertionError(f'stale cancel did not pass active status: {receipt_while_delayed}')
        fifo_send(files['child_release'])
        fifo_wait(files['report_ready'])
        report_exists_before_release = (files['output'] / 'report.json').exists()
        owner_receipt_at_gate = json.loads(files['receipt'].read_text())
        with open(f'/proc/{owner.pid}/status') as status_file:
            sigcgt = int(next(line.split()[1] for line in status_file if line.startswith('SigCgt:')), 16)
        term_handler_caught = bool(sigcgt & (1 << (signal.SIGTERM - 1)))
        if owner_receipt_at_gate.get('status') != 'interrupted' or owner_receipt_at_gate.get('runner_reaped') is not True:
            raise AssertionError(f'owner did not reach post-runner report tail: {owner_receipt_at_gate}')
        if report_exists_before_release:
            raise AssertionError('report unexpectedly exists before report-tail gate release')
        if delayed_second_cancel:
            fifo_send(files['delayed_release'])
            delayed_out, delayed_err = delayed.communicate(timeout=10)
            fifo_send(files['report_release'])
            owner_code = owner.wait(timeout=10)
            owner_out, owner_err = owner.communicate()
            final_receipt = json.loads(files['receipt'].read_text())
            result = {
                'scenario': 'stale_status_then_real_second_pidfd_SIGTERM',
                'delayed_cancel_returncode': delayed.returncode,
                'delayed_cancel_stdout': delayed_out,
                'delayed_cancel_stderr': delayed_err,
                'owner_returncode': owner_code,
                'owner_stdout': owner_out,
                'owner_stderr': owner_err,
                'report_exists': (files['output'] / 'report.json').exists(),
                'owner_receipt_status': final_receipt['status'],
                'owner_state': final_receipt.get('owner_state'),
                'owner_receipt_has_report_path': 'report_path' in final_receipt,
                'cancellation_requests': final_receipt.get('cancellation_requests'),
                'report_publication': final_receipt.get('report_publication'),
                'starts': files['starts'].read_text().splitlines(),
                'detached_child_pid': child,
                'term_handler_caught_at_report_gate': term_handler_caught,
                'handlers_restored': json.loads((root / 'handlers-restored.json').read_text()),
            }
        else:
            fresh = subprocess.run([sys.executable, str(QUAL), 'cancel', str(files['receipt'])],
                                   capture_output=True, text=True, check=False, timeout=10)
            if fresh.returncode != 0 or fresh.stdout.strip() != 'qualification_not_running':
                raise AssertionError(f'fresh terminal-state cancel not refused: {fresh.returncode} {fresh.stdout!r} {fresh.stderr!r}')
            fifo_send(files['report_release'])
            owner_code = owner.wait(timeout=10)
            owner_out, owner_err = owner.communicate()
            final_receipt = json.loads(files['receipt'].read_text())
            result = {
                'scenario': 'fresh_terminal_status_refuses_second_cancel',
                'fresh_cancel_returncode': fresh.returncode,
                'fresh_cancel_stdout': fresh.stdout.strip(),
                'owner_returncode': owner_code,
                'owner_stdout': owner_out,
                'owner_stderr': owner_err,
                'report_exists': (files['output'] / 'report.json').exists(),
                'owner_receipt_status': final_receipt['status'],
                'owner_receipt_has_report_path': 'report_path' in final_receipt,
                'owner_state': final_receipt.get('owner_state'),
                'report_publication': final_receipt.get('report_publication'),
                'starts': files['starts'].read_text().splitlines(),
                'detached_child_pid': child,
                'term_handler_caught_at_report_gate': term_handler_caught,
                'handlers_restored': json.loads((root / 'handlers-restored.json').read_text()),
            }
        return result
    finally:
        if delayed is not None and delayed.poll() is None:
            delayed.kill()
            delayed.communicate(timeout=5)
        if owner.poll() is None:
            owner.kill()
            owner.communicate(timeout=5)
        if child is not None:
            try:
                os.kill(child, signal.SIGKILL)
            except ProcessLookupError:
                pass



class QualificationSignalLifetimeTests(unittest.TestCase):
    def test_accepted_delayed_cancel_preserves_report_and_terminal_cancel_is_refused(self):
        with tempfile.TemporaryDirectory(prefix='qualification-signal-lifetime-') as directory:
            base = Path(directory)
            race_root = base / 'race'
            control_root = base / 'control'
            race_root.mkdir()
            control_root.mkdir()
            race = run_case(race_root, True)
            control = run_case(control_root, False)

        self.assertEqual(race['delayed_cancel_returncode'], 0, race)
        self.assertEqual(race['delayed_cancel_stdout'].strip(), 'signal_sent', race)
        self.assertEqual(race['owner_returncode'], 128 + signal.SIGTERM, race)
        self.assertTrue(race['report_exists'], race)
        self.assertEqual(race['owner_receipt_status'], 'interrupted', race)
        self.assertEqual(race['owner_state'], 'published', race)
        self.assertTrue(race['owner_receipt_has_report_path'], race)
        self.assertEqual(race['cancellation_requests'][-1]['forwarding_status'],
                         'runner_already_exited', race)
        self.assertTrue(race['term_handler_caught_at_report_gate'], race)
        self.assertTrue(all(race['handlers_restored'].values()), race)
        self.assertEqual(race['starts'], ['suite::complete', 'suite::active'])

        self.assertEqual(control['fresh_cancel_returncode'], 0, control)
        self.assertEqual(control['fresh_cancel_stdout'], 'qualification_not_running', control)
        self.assertEqual(control['owner_returncode'], 128 + signal.SIGTERM, control)
        self.assertTrue(control['report_exists'], control)
        self.assertEqual(control['owner_receipt_status'], 'interrupted', control)
        self.assertEqual(control['owner_state'], 'published', control)
        self.assertTrue(control['owner_receipt_has_report_path'], control)
        self.assertTrue(all(control['handlers_restored'].values()), control)
        self.assertEqual(control['starts'], ['suite::complete', 'suite::active'])

    def test_report_write_failure_retains_runner_result_without_report_path(self):
        with tempfile.TemporaryDirectory(prefix='qualification-report-failure-') as directory:
            result = run_case(Path(directory), False, fail_report=True)
        self.assertEqual(result['fresh_cancel_stdout'], 'qualification_not_running', result)
        self.assertEqual(result['owner_returncode'], 128 + signal.SIGTERM, result)
        self.assertFalse(result['report_exists'], result)
        self.assertEqual(result['owner_receipt_status'], 'interrupted', result)
        self.assertEqual(result['owner_state'], 'failed', result)
        self.assertFalse(result['owner_receipt_has_report_path'], result)
        self.assertEqual(result['report_publication']['status'], 'failed', result)
        self.assertEqual(result['starts'], ['suite::complete', 'suite::active'])
        self.assertTrue(all(result['handlers_restored'].values()), result)

    def test_cli_catches_real_pidfd_term_during_final_receipt_write(self):
        with tempfile.TemporaryDirectory(prefix='qualification-cli-signal-exit-') as directory:
            root = Path(directory)
            ready = root / 'receipt-write-ready.fifo'
            release = root / 'receipt-write-release.fifo'
            receipt = root / 'run-owner.json'
            os.mkfifo(ready, 0o600)
            os.mkfifo(release, 0o600)
            helper = root / 'qualification-cli-exit.py'
            helper.write_text(
                'import importlib.util, os, signal, sys\n'
                'from pathlib import Path\n'
                f'script = Path({str(QUAL)!r})\n'
                'spec = importlib.util.spec_from_file_location("qualification_under_test", script)\n'
                'module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)\n'
                f'receipt_path = Path({str(receipt)!r})\n'
                f'ready_path = {str(ready)!r}\n'
                f'release_path = {str(release)!r}\n'
                'original_write = module.write_private_json\n'
                'write_gated = False\n'
                'def gated_write(path, value):\n'
                '    global write_gated\n'
                '    if Path(path) == receipt_path and not write_gated:\n'
                '        write_gated = True\n'
                '        with open(ready_path, "w") as event: event.write("ready"); event.flush()\n'
                '        with open(release_path, "r") as gate: gate.read(1)\n'
                '    return original_write(path, value)\n'
                'module.write_private_json = gated_write\n'
                'def completed_main(_argv):\n'
                '    owner = module._active_qualification_signal_owner\n'
                '    owner.bind(receipt_path, {"schema": 1, "status": "interrupted", '
                '        "runner_reaped": True, "runner_exit_code": 143, "cancellation_requests": []})\n'
                '    owner.phase = "published"\n'
                '    owner.receipt["owner_state"] = "published"\n'
                '    owner.dirty = True\n'
                '    return 143\n'
                'module.main = completed_main\n'
                'module.cli_entrypoint(["run"])\n'
            )
            process = subprocess.Popen([sys.executable, str(helper)],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       text=True, start_new_session=True)
            pidfd = None
            try:
                fifo_wait(ready)
                with open(f'/proc/{process.pid}/status') as status_file:
                    sigcgt = int(next(line.split()[1] for line in status_file
                                      if line.startswith('SigCgt:')), 16)
                self.assertTrue(sigcgt & (1 << (signal.SIGTERM - 1)))
                pidfd = os.pidfd_open(process.pid, 0)
                signal.pidfd_send_signal(pidfd, signal.SIGTERM, None, 0)
                fifo_send(release)
                stdout, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 143, (stdout, stderr))
                final_receipt = json.loads(receipt.read_text())
                self.assertEqual(final_receipt['runner_exit_code'], 143)
                self.assertEqual(final_receipt['owner_state'], 'published')
                self.assertEqual(final_receipt['cancellation_requests'][-1]['forwarding_status'],
                                 'runner_already_exited')
            finally:
                if pidfd is not None:
                    os.close(pidfd)
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=5)


if __name__ == '__main__':
    unittest.main()
