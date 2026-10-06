#!/usr/bin/env python3
"""Summarize actual stage decisions from retained compiler JSON trace envelopes."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re

STAGES = ('source_frontend', 'interface', 'finalized_core', 'prepared_body',
          'site_witness', 'original_recovery', 'raw_projection',
          'artifact_reference', 'artifact_transfer', 'native_image')
DECISIONS = {'hit', 'miss', 'work', 'disabled', 'evicted', 'epoch_rotated', 'complete'}
REASONS = {'matched', 'absent', 'changed_source', 'changed_dependency',
           'changed_authority', 'th_fresh', 'epoch', 'recovery',
           'cache_disabled', 'evicted', 'stage_complete'}
KINDS = {'source_fingerprint', 'canonical_seal', 'prepared_identity', 'interface_fingerprint', 'image_identity'}
KEY_FIELDS = ('daemon_epoch', 'worker_pid', 'admission_id', 'request_ordinal')
PREFIX = 'tidepool-reuse '


def fields(event):
    result = {}
    if 'fields' not in event:
        result.update(event)
    for span in event.get('spans', []):
        result.update(span)
    result.update(event.get('span', {}))
    result.update(event.get('fields', {}))
    return result


def natural(value):
    return type(value) is int and value >= 0


def text(value, maximum=256):
    return isinstance(value, str) and 0 < len(value) <= maximum


def identity(row):
    if (not text(row.get('daemon_epoch')) or not natural(row.get('worker_pid'))
            or row['worker_pid'] == 0 or not natural(row.get('admission_id'))
            or not natural(row.get('request_ordinal')) or row['request_ordinal'] == 0):
        return None
    return tuple(row[name] for name in KEY_FIELDS)


def validate_event(event):
    required = {'schema', 'stage', 'decision', 'reason', 'unit', 'module', 'version',
                'version_kind', 'items', 'bytes', 'cycle', 'purpose', 'observed_ns'}
    if (not required <= event.keys() or type(event.get('schema')) is not int or event['schema'] != 1 or event.get('stage') not in STAGES
            or event.get('decision') not in DECISIONS or event.get('reason') not in REASONS
            or not natural(event.get('items')) or not natural(event.get('cycle'))
            or not text(event.get('purpose')) or not natural(event.get('observed_ns'))
            or (event.get('bytes') is not None and not natural(event['bytes']))):
        raise ValueError('malformed reuse event')
    owner = [event.get(key) for key in ('unit', 'module', 'version_kind', 'version')]
    if any(value is not None for value in owner):
        image = owner[:2] == [None, None] and owner[2] == 'image_identity' and text(owner[3])
        if not image and (not all(text(value) for value in owner) or owner[2] not in KINDS):
            raise ValueError('incomplete exact owner identity')
    if event['decision'] == 'complete':
        if event['reason'] != 'stage_complete' or event['items'] != 0 or any(owner) or event['bytes'] is not None:
            raise ValueError('invalid stage completion')
    elif event['reason'] == 'stage_complete':
        raise ValueError('completion reason on decision event')
    expected = {'hit': 'matched', 'disabled': 'cache_disabled', 'evicted': 'evicted', 'epoch_rotated': 'epoch'}
    if event['decision'] in expected and event['reason'] != expected[event['decision']]:
        raise ValueError('decision contradicts reason')


def analyze(events):
    problems, unlinked, requests, admissions = [], [], {}, {}
    normalized = [fields(event) for event in events]
    for index, row in enumerate(normalized):
        key = identity(row)
        if row.get('message') == 'compiler job dequeued':
            if text(row.get('daemon_epoch')) and natural(row.get('admission_id')):
                admissions.setdefault((row['daemon_epoch'], row['admission_id']), []).append(row.get('queue_ms'))
        if row.get('message') != 'compiler request started':
            continue
        if key is None or not text(row.get('compile_request')):
            problems.append(f'row {index}: request start lacks physical identity')
            continue
        if key in requests:
            problems.append(f'row {index}: duplicate physical request start')
            continue
        requests[key] = {'identity': dict(zip(KEY_FIELDS, key)), 'compile_request': row['compile_request'],
                         'transaction': row.get('transaction'), 'start_row': index,
                         'terminal_rows': [], 'service_ms': None, 'phases_ms': {},
                         'legacy_counts': {}, 'legacy_compile_summaries': [],
                         'events': [], 'stages': {}}
    for index, row in enumerate(normalized):
        key = identity(row)
        request = requests.get(key)
        line = row.get('line', '')
        if not isinstance(line, str):
            continue
        is_reuse = line.startswith(PREFIX)
        if not request:
            if is_reuse:
                unlinked.append({'row': index, 'line': line, 'envelope': row})
                problems.append(f'row {index}: reuse event lacks exact request linkage')
            continue
        if row.get('compile_request') != request['compile_request']:
            if is_reuse or row.get('message', '').startswith('compiler request '):
                problems.append(f'row {index}: request digest conflicts with physical invocation')
            continue
        if row.get('message') in ('compiler request finished', 'compiler request failed',
                                  'compiler request abandoned by client'):
            request['terminal_rows'].append(index)
            request['service_ms'] = row.get('elapsed_ms')
            request['exit_code'] = row.get('exit_code')
        phase = re.fullmatch(r'tidepool-timing phase=([a-zA-Z0-9_]+) ms=([0-9]+)', line)
        if phase:
            request['phases_ms'].setdefault(phase[1], []).append(int(phase[2]))
        count = re.fullmatch(r'tidepool-count name=([a-zA-Z0-9_.]+) count=([0-9]+)(?: count_ns=[0-9]+)?', line)
        if count:
            request['legacy_counts'][count[1]] = request['legacy_counts'].get(count[1], 0) + int(count[2])
        if line.startswith('tidepool-compile-summary '):
            request['legacy_compile_summaries'].append({
                'row': index, 'line': line,
                'counts': {name: int(value) for name, value in re.findall(
                    r'(?:^| )(modules|typecheck_modules|lowering_modules|interface_modules)=([0-9]+)(?= |$)', line)}})
        if is_reuse:
            try:
                event = json.loads(line[len(PREFIX):])
                if not isinstance(event, dict):
                    raise ValueError('reuse payload is not an object')
                validate_event(event)
                request['events'].append({'row': index, **event})
            except (ValueError, TypeError) as error:
                problems.append(f'row {index}: {error}')
    for key, request in requests.items():
        terminals = request['terminal_rows']
        if len(terminals) != 1:
            problems.append(f'{key}: expected one request terminal')
        for event in request['events']:
            if event['row'] <= request['start_row'] or (terminals and event['row'] >= terminals[0]):
                problems.append(f"row {event['row']}: reuse event outside request boundaries")
        request['admission_queue_ms'] = admissions.get((key[0], key[2]), [])
        cycles = sorted({(event['cycle'], event['purpose']) for event in request['events']})
        request['cycles'] = [{'cycle': cycle, 'purpose': purpose} for cycle, purpose in cycles]
        for stage in STAGES:
            selected = [event for event in request['events'] if event['stage'] == stage]
            complete = {(event['cycle'], event['purpose']) for event in selected if event['decision'] == 'complete'}
            touched = {(event['cycle'], event['purpose']) for event in selected}
            status = 'observed' if complete and complete == touched and complete == set(cycles) else 'UNKNOWN'
            counts, byte_counts, reasons, accounted = Counter(), Counter(), Counter(), 0
            for event in selected:
                if event['decision'] == 'complete':
                    continue
                counts[event['decision']] += event['items']
                reasons[event['reason']] += event['items']
                if event['bytes'] is not None:
                    accounted += 1
                    byte_counts[event['decision']] += event['bytes']
            request['stages'][stage] = {'status': status, 'counts': dict(counts) if status == 'observed' else None,
                                        'bytes': dict(byte_counts) if status == 'observed' and accounted else None,
                                        'byte_accounted_events': accounted,
                                        'reasons': dict(reasons), 'completion_cycles': len(complete)}
        request['status'] = 'observed' if request['events'] and len(terminals) == 1 else 'UNKNOWN'
    incomplete = problems or not requests or any(request['status'] == 'UNKNOWN' or any(
        value['status'] == 'UNKNOWN' and value['reasons'] for value in request['stages'].values()) for request in requests.values())
    return {'schema': 1, 'status': 'incomplete' if incomplete else 'observed',
            'problems': problems, 'requests': list(requests.values()), 'unlinked_events': unlinked,
            'interpretation': 'Hits name one stage only. Missing stage completion is UNKNOWN. Admission queue times are shared by the transaction, not additive per request. Phase totals without interval boundaries remain nonexclusive; do not sum overlapping timers.'}


def compare_control(normal, disabled, stage):
    """Require actual extra work for matching module versions, not faster latency."""
    problems = []
    if stage not in ('source_frontend', 'prepared_body'):
        return {'status': 'incomplete', 'problems': ['unsupported disable control stage']}
    for label, request in (('normal', normal), ('disabled', disabled)):
        if request.get('exit_code') != 0 or request['stages'][stage]['status'] != 'observed':
            problems.append(f'{label}: successful completed stage evidence required')
    def roster(request):
        return {(event['unit'], event['module'], event['version_kind'], event['version'])
                for event in request['events'] if event['stage'] == stage
                and event['decision'] in ('hit', 'work') and event['unit'] is not None}
    if not roster(normal) or roster(normal) != roster(disabled):
        problems.append('controls do not describe the same exact module/version roster')
    marker = any(event['stage'] == stage and event['decision'] == 'disabled'
                 and event['reason'] == 'cache_disabled' and event['items'] > 0 for event in disabled['events'])
    if not marker:
        problems.append('disabled control has no actual disable decision')
    normal_work = sum(event['items'] for event in normal['events'] if event['stage'] == stage and event['decision'] == 'work')
    disabled_work = sum(event['items'] for event in disabled['events'] if event['stage'] == stage and event['decision'] == 'work')
    normal_hits = sum(event['items'] for event in normal['events'] if event['stage'] == stage and event['decision'] == 'hit')
    if normal_hits <= 0 or disabled_work <= normal_work or disabled_work <= 0:
        problems.append('control lacks a normal hit and nonzero increased actual work')
    return {'status': 'observed' if not problems else 'incomplete', 'stage': stage,
            'normal_work': normal_work, 'disabled_work': disabled_work,
            'work_delta': disabled_work - normal_work, 'problems': problems}


def load_events(path):
    if path.stat().st_size > 64 * 1024 * 1024:
        raise ValueError('trace exceeds 64 MiB; retain a bounded exact-request trace')
    rows = []
    with path.open() as stream:
        for line in stream:
            if len(line) > 1024 * 1024:
                raise ValueError('trace row exceeds one MiB')
            if line.strip():
                row = json.loads(line)
                if not isinstance(row, dict):
                    raise ValueError('trace row is not an object')
                rows.append(row)
    return rows


def analyze_workload(manifest, report):
    """Link retained authored inputs and controls to physical requests."""
    problems, cases, groups = [], {}, {}
    requests = {tuple(row['identity'][key] for key in KEY_FIELDS): row for row in report['requests']}
    if manifest.get('schema') != 1 or not isinstance(manifest.get('cases'), list):
        raise ValueError('workload requires schema 1 and cases')
    for case in manifest['cases']:
        name, scenario = case.get('name'), case.get('scenario')
        if not text(name) or name in cases or scenario not in ('distinct', 'repeat', 'binding_growth', 'aba', 'control'):
            raise ValueError('workload has invalid/duplicate case name or scenario')
        request = requests.get(identity(case.get('identity', {})))
        source = case.get('source', {})
        path = Path(source.get('path', ''))
        digest = source.get('sha256')
        if (not path.is_absolute() or not path.is_file() or path.stat().st_size > 32 * 1024 * 1024
                or hashlib.sha256(path.read_bytes()).hexdigest() != digest):
            raise ValueError(f'{name}: authored source reference missing or changed')
        if request is None or request.get('exit_code') != 0:
            problems.append(f'{name}: no successful exact physical request')
            continue
        cases[name] = request
        groups.setdefault(scenario, []).append((case, request))
    for scenario in ('distinct', 'repeat', 'binding_growth', 'aba'):
        selected = groups.get(scenario, [])
        minimum = 3 if scenario == 'aba' else 2
        if len(selected) < minimum:
            problems.append(f'{scenario}: missing required workload cases')
            continue
        ordered = sorted(selected, key=lambda pair: pair[0].get('step', -1))
        steps = [case.get('step') for case, _ in ordered]
        if any(not natural(step) for step in steps) or len(set(steps)) != len(steps):
            problems.append(f'{scenario}: steps missing or duplicated')
        hashes = [case['source']['sha256'] for case, _ in ordered]
        if scenario in ('distinct', 'binding_growth') and len(set(hashes)) < 2:
            problems.append(f'{scenario}: authored inputs do not change')
        if scenario == 'repeat' and len(set(hashes)) != 1:
            problems.append('repeat: authored input changed')
        if scenario == 'aba' and (len(hashes) != 3 or hashes[0] != hashes[2] or hashes[0] == hashes[1]):
            problems.append('aba: expected exactly A, B, A authored inputs')
        workers = {(request['identity']['daemon_epoch'], request['identity']['worker_pid']) for _, request in selected}
        if len(workers) != 1:
            problems.append(f'{scenario}: worker or daemon epoch changed')
        if len({tuple(request['identity'][key] for key in KEY_FIELDS) for _, request in selected}) != len(selected):
            problems.append(f'{scenario}: one request reused as multiple samples')
    controls = []
    for control in manifest.get('controls', []):
        normal, disabled = cases.get(control.get('normal')), cases.get(control.get('disabled'))
        if normal is None or disabled is None:
            controls.append({'status': 'incomplete', 'stage': control.get('stage'), 'problems': ['control case absent']})
        else:
            normal_case = next(case for case in manifest['cases'] if case['name'] == control['normal'])
            disabled_case = next(case for case in manifest['cases'] if case['name'] == control['disabled'])
            result = compare_control(normal, disabled, control.get('stage'))
            if normal_case['source']['sha256'] != disabled_case['source']['sha256']:
                result['problems'].append('control authored inputs differ')
                result['status'] = 'incomplete'
            controls.append(result)
    for stage in ('source_frontend', 'prepared_body'):
        if not any(control.get('stage') == stage and control['status'] == 'observed' for control in controls):
            problems.append(f'{stage}: no observed disable control')
    return {'status': 'observed' if not problems and all(control['status'] == 'observed' for control in controls) else 'incomplete',
            'problems': problems, 'controls': controls,
            'limitations': 'Source hashes prove retained inputs, not binding semantics. Owning live runner must assert semantic results and capture these sources as the actual requests.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--trace', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--require-stage', choices=STAGES, action='append', default=[])
    parser.add_argument('--workload', type=Path, help='retained physical-request/source mapping and disable controls')
    args = parser.parse_args()
    if args.trace.stat().st_size > 64 * 1024 * 1024:
        raise ValueError('trace exceeds 64 MiB; retain a bounded exact-request trace')
    before = hashlib.sha256(args.trace.read_bytes()).hexdigest()
    report = analyze(load_events(args.trace))
    after = hashlib.sha256(args.trace.read_bytes()).hexdigest()
    if before != after:
        raise ValueError('trace changed during reporting; retain an immutable request trace')
    report['input'] = {'path': str(args.trace.resolve()), 'sha256': after}
    if args.workload:
        if args.workload.stat().st_size > 4 * 1024 * 1024:
            raise ValueError('workload manifest exceeds four MiB')
        workload_bytes = args.workload.read_bytes()
        report['workload'] = analyze_workload(json.loads(workload_bytes), report)
        report['workload_input'] = {'path': str(args.workload.resolve()), 'sha256': hashlib.sha256(workload_bytes).hexdigest()}
        if report['workload']['status'] != 'observed':
            report['problems'].append('workload/control evidence incomplete')
    for request in report['requests']:
        for stage in args.require_stage:
            if request['stages'][stage]['status'] != 'observed':
                report['problems'].append(f"{request['identity']}: required stage {stage} is UNKNOWN")
    if report['problems']:
        report['status'] = 'incomplete'
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'status': report['status'], 'requests': len(report['requests']), 'problems': report['problems']}))
    return 0 if report['status'] == 'observed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
