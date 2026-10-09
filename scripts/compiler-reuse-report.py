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
DECISIONS = {'hit', 'miss', 'work', 'disabled', 'evicted', 'epoch_rotated', 'complete', 'not_applicable'}
REASONS = {'matched', 'absent', 'changed_source', 'changed_dependency',
           'changed_authority', 'th_fresh', 'epoch', 'recovery',
           'cache_disabled', 'evicted', 'stage_complete', 'check_only'}
CHECK_ONLY_STAGES = {'prepared_body', 'site_witness', 'original_recovery', 'raw_projection'}
KINDS = {'source_fingerprint', 'canonical_seal', 'prepared_identity', 'interface_fingerprint', 'image_identity'}
KEY_FIELDS = ('daemon_epoch', 'worker_pid', 'admission_id', 'request_ordinal')
PREFIX = 'tidepool-reuse '
TASK_PARENTS = {'prepared_stg_task_service': 'prepared_graph',
                'raw_projection_task_service': 'prepared_graph',
                'source_frontend_task_service': 'ghc_load',
                'source_finalization_task_service': 'ghc_load'}
TASK_PHASES = set(TASK_PARENTS)


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
    if event['decision'] in ('complete', 'not_applicable'):
        expected_reason = 'stage_complete' if event['decision'] == 'complete' else 'check_only'
        if (event['reason'] != expected_reason or event['items'] != 0 or any(owner)
                or event['bytes'] is not None
                or (event['decision'] == 'not_applicable' and event['stage'] not in CHECK_ONLY_STAGES)):
            raise ValueError('invalid stage terminal observation')
    elif event['reason'] in ('stage_complete', 'check_only'):
        raise ValueError('terminal reason on decision event')
    expected = {'hit': 'matched', 'disabled': 'cache_disabled', 'evicted': 'evicted', 'epoch_rotated': 'epoch'}
    if event['decision'] in expected and event['reason'] != expected[event['decision']]:
        raise ValueError('decision contradicts reason')


def parse_timing_detail(line):
    """Parse one existing timing-detail line without inventing missing fields."""
    if not line.startswith('tidepool-timing-detail '):
        return None
    parsed, values, duplicates = {}, {}, []
    for token in line[len('tidepool-timing-detail '):].split():
        if '=' not in token:
            continue
        name, value = token.split('=', 1)
        if name in values:
            duplicates.append(name)
        values.setdefault(name, []).append(value)
        parsed[name] = value
    phases = values.get('phase', [])
    selected_phase = next((phase for phase in phases if phase in TASK_PHASES), None)
    if selected_phase is None:
        return None
    parsed['phase'] = selected_phase
    problems = [f'duplicate timing field: {name}' for name in sorted(set(duplicates))]
    expected_parent = TASK_PARENTS[selected_phase]
    if parsed.get('parent') != expected_parent:
        problems.append(f"task phase parent must be '{expected_parent}'")
    for key in ('ms', 'start_ns', 'end_ns', 'wall_ns', 'cpu_ns'):
        value = parsed.get(key)
        if value is None:
            continue
        if re.fullmatch(r'[0-9]+', value):
            parsed[key] = int(value)
        else:
            problems.append(f'timing field {key} is not a nonnegative integer')
    if 'wall_ns' not in parsed:
        problems.append('task phase lacks wall_ns')
    if type(parsed.get('start_ns')) is int and type(parsed.get('end_ns')) is int:
        if parsed['end_ns'] < parsed['start_ns']:
            problems.append('task phase has invalid interval endpoints')
        elif type(parsed.get('wall_ns')) is int and parsed['wall_ns'] != parsed['end_ns'] - parsed['start_ns']:
            problems.append('task phase wall_ns does not match endpoints')
    parsed['_problems'] = problems
    return parsed


def task_overlap(details):
    """Report overlap among qualified task spans; never sum resource deltas."""
    selected = [detail for detail in details if detail['parsed'].get('phase') in TASK_PHASES]
    intervals, problems = [], []
    for detail in selected:
        parsed = detail['parsed']
        problems.extend(f"row {detail['row']}: {problem}" for problem in parsed.get('_problems', []))
        if detail.get('_boundary_problem'):
            problems.append(f"row {detail['row']}: {detail['_boundary_problem']}")
        owner_unit, owner_module = parsed.get('owner_unit'), parsed.get('owner_module')
        start, end = parsed.get('start_ns'), parsed.get('end_ns')
        if not text(owner_unit) or not text(owner_module):
            problems.append(f"row {detail['row']}: task span lacks owner unit/module")
            continue
        if type(start) is not int or type(end) is not int or start < 0 or end < start:
            problems.append(f"row {detail['row']}: task span has invalid interval endpoints")
            continue
        intervals.append({'row': detail['row'], 'phase': parsed['phase'],
                          'owner_unit': owner_unit, 'owner_module': owner_module,
                          'start_ns': start, 'end_ns': end})
    interval_keys = {}
    for span in intervals:
        key = (span['phase'], span['owner_unit'], span['owner_module'], span['start_ns'], span['end_ns'])
        interval_keys.setdefault(key, []).append(span['row'])
    for rows in interval_keys.values():
        if len(rows) > 1:
            problems.append(f'duplicate task interval rows: {", ".join(map(str, rows))}')
    owner_phase_intervals = {}
    for span in intervals:
        if span['end_ns'] > span['start_ns']:
            owner_phase_intervals.setdefault(
                (span['phase'], span['owner_unit'], span['owner_module']), []).append(span)
    for (phase, unit, module), spans in owner_phase_intervals.items():
        furthest_end = None
        for span in sorted(spans, key=lambda item: (item['start_ns'], item['end_ns'])):
            if furthest_end is not None and span['start_ns'] < furthest_end:
                problems.append(f'{phase}: overlapping intervals for owner {unit}/{module}')
                break
            furthest_end = max(furthest_end or span['end_ns'], span['end_ns'])
    if not selected:
        problems.append('no qualified task phase spans')
    if problems:
        unknown = {'status': 'UNKNOWN', 'maximum_simultaneous_tasks': None,
                   'maximum_simultaneous_distinct_owners': None,
                   'overlapping_wall_ns': None, 'overlapping_wall_ns_distinct_owners': None}
        return {**unknown, 'qualified_spans': intervals, 'by_phase': {
            phase: {**unknown, 'qualified_spans': [span for span in intervals if span['phase'] == phase]}
            for phase in sorted(TASK_PHASES)}, 'combined_scope': 'mixed_phase_descriptive_only',
                'mixed_phase_overlap_is_independent_cpu_proof': False,
                'same_owner_policy': 'same owner across phases counts once; same-phase overlap is UNKNOWN',
                'problems': problems}

    def measure(spans):
        if not spans:
            return {'status': 'UNKNOWN', 'maximum_simultaneous_tasks': None,
                    'maximum_simultaneous_distinct_owners': None,
                    'overlapping_wall_ns': None, 'overlapping_wall_ns_distinct_owners': None,
                    'qualified_spans': []}
        points = []
        for span in spans:
            if span['end_ns'] > span['start_ns']:
                owner = (span['owner_unit'], span['owner_module'])
                points.append((span['start_ns'], 1, owner))
                points.append((span['end_ns'], -1, owner))
        # Close before opening at one timestamp so touching intervals do not overlap.
        points.sort(key=lambda point: (point[0], point[1]))
        active, maximum, overlap_ns = {}, 0, 0
        previous = None
        index = 0
        while index < len(points):
            timestamp = points[index][0]
            if previous is not None and len(active) >= 2:
                overlap_ns += timestamp - previous
            while index < len(points) and points[index][0] == timestamp and points[index][1] < 0:
                owner = points[index][2]
                active[owner] -= 1
                if active[owner] == 0:
                    del active[owner]
                index += 1
            while index < len(points) and points[index][0] == timestamp:
                owner = points[index][2]
                active[owner] = active.get(owner, 0) + 1
                maximum = max(maximum, len(active))
                index += 1
            previous = timestamp
        return {'status': 'observed', 'maximum_simultaneous_tasks': maximum,
                'maximum_simultaneous_distinct_owners': maximum,
                'overlapping_wall_ns': overlap_ns, 'overlapping_wall_ns_distinct_owners': overlap_ns,
                'qualified_spans': spans}

    measured = measure(intervals)
    measured['by_phase'] = {phase: measure([span for span in intervals if span['phase'] == phase])
                            for phase in sorted(TASK_PHASES)}
    measured['combined_scope'] = 'mixed_phase_descriptive_only'
    measured['mixed_phase_overlap_is_independent_cpu_proof'] = False
    measured['same_owner_policy'] = 'same owner across phases counts once; same-phase overlap is UNKNOWN'
    measured['problems'] = []
    return measured


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
            requests[key]['parse_problems'].append('duplicate physical request start')
            continue
        requests[key] = {'identity': dict(zip(KEY_FIELDS, key)), 'compile_request': row['compile_request'],
                         'transaction': row.get('transaction'), 'start_row': index,
                         'terminal_rows': [], 'service_ms': None, 'phases_ms': {},
                         'timing_details': [],
                         'legacy_counts': {}, 'legacy_compile_summaries': [], 'legacy_observations': [],
                         'events': [], 'stages': {}, 'parse_problems': []}
    for index, row in enumerate(normalized):
        key = identity(row)
        request = requests.get(key)
        line = row.get('line', '')
        if not isinstance(line, str):
            continue
        is_reuse = line.startswith(PREFIX)
        detail = parse_timing_detail(line)
        is_task_detail = detail is not None
        phase = re.fullmatch(r'tidepool-timing phase=([a-zA-Z0-9_]+) ms=([0-9]+)', line)
        count = re.fullmatch(r'tidepool-count name=([a-zA-Z0-9_.]+) count=([0-9]+)(?: count_ns=[0-9]+)?', line)
        is_legacy = phase is not None or count is not None or line.startswith('tidepool-compile-summary ')
        if not request:
            if is_reuse or is_task_detail or is_legacy:
                unlinked.append({'row': index, 'line': line, 'envelope': row})
                problems.append(f'row {index}: diagnostic event lacks exact request linkage')
            continue
        if row.get('compile_request') != request['compile_request']:
            if is_reuse or is_task_detail or is_legacy or row.get('message', '').startswith('compiler request '):
                problems.append(f'row {index}: request digest conflicts with physical invocation')
                request['parse_problems'].append(f'row {index}: request digest conflicts with physical invocation')
            continue
        if row.get('message') in ('compiler request finished', 'compiler request failed',
                                  'compiler request abandoned by client'):
            request['terminal_rows'].append(index)
            request['service_ms'] = row.get('elapsed_ms')
            request['exit_code'] = row.get('exit_code')
            request['terminal_message'] = row['message']
        if phase:
            request['legacy_observations'].append({'row': index, 'line': line,
                                                  'kind': 'phase', 'name': phase[1], 'value': int(phase[2])})
        if detail is not None:
            request['timing_details'].append({'row': index, 'line': line, 'parsed': detail})
        if count:
            request['legacy_observations'].append({'row': index, 'line': line,
                                                  'kind': 'count', 'name': count[1], 'value': int(count[2])})
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
                request['events'].append({**event, 'row': index})
            except (ValueError, TypeError) as error:
                problems.append(f'row {index}: {error}')
                request['parse_problems'].append(f'row {index}: {error}')
    cycle_purposes = {}
    for key, request in requests.items():
        for event in request['events']:
            cycle_purposes.setdefault((key[0], key[1], event['cycle']), set()).add(event['purpose'])
    for key, request in requests.items():
        request_problems = list(request.pop('parse_problems'))
        terminals = request['terminal_rows']
        if len(terminals) != 1:
            request_problems.append('expected one request terminal')
        elif (request.get('terminal_message') != 'compiler request finished'
                or type(request.get('exit_code')) is not int or request['exit_code'] != 0):
            request_problems.append('request did not finish successfully')
        legacy_problems = []
        for observation in request['legacy_observations'] + request['legacy_compile_summaries']:
            bounded = (len(terminals) == 1 and request['start_row'] < observation['row'] < terminals[0])
            observation['boundary_status'] = 'observed' if bounded else 'UNKNOWN'
            if not bounded:
                legacy_problems.append(f"row {observation['row']}: legacy diagnostic outside request boundaries")
        request['legacy_status'] = 'UNKNOWN'
        if request['legacy_observations'] and not legacy_problems and not request_problems:
            request['legacy_status'] = 'observed'
            for observation in request['legacy_observations']:
                name, value = observation['name'], observation['value']
                if observation['kind'] == 'phase':
                    request['phases_ms'].setdefault(name, []).append(value)
                else:
                    request['legacy_counts'][name] = request['legacy_counts'].get(name, 0) + value
        request_problems.extend(legacy_problems)
        for detail in request['timing_details']:
            if detail['row'] <= request['start_row']:
                detail['_boundary_problem'] = 'task timing detail outside request boundaries'
            elif len(terminals) != 1 or detail['row'] >= terminals[0]:
                detail['_boundary_problem'] = 'task timing detail outside request boundaries'
            if detail.get('_boundary_problem'):
                request_problems.append(f"row {detail['row']}: {detail['_boundary_problem']}")
        seen_reuse_events = set()
        for event in request['events']:
            if event['row'] <= request['start_row']:
                request_problems.append(f"row {event['row']}: reuse event precedes its physical request start")
            terminal_order = 'unknown'
            terminal_row = None
            if len(terminals) == 1:
                terminal_row = terminals[0]
                terminal_order = 'after' if event['row'] >= terminal_row else 'before'
            # Trace subscribers can drain a worker's buffered diagnostics after
            # its terminal event. Exact envelope identity and request digest
            # establish ownership; row order is retained as evidence, not used
            # as a substitute for that correlation.
            event['correlation'] = {
                'basis': 'exact_physical_request_identity',
                'terminal_order': terminal_order,
                'request_start_row': request['start_row'],
                'request_terminal_row': terminal_row,
            }
            fingerprint = json.dumps(
                {name: value for name, value in event.items() if name not in ('row', 'correlation')},
                sort_keys=True, separators=(',', ':'))
            if fingerprint in seen_reuse_events:
                request_problems.append(f"row {event['row']}: duplicate reuse event for physical request")
            seen_reuse_events.add(fingerprint)
            if len(cycle_purposes[(key[0], key[1], event['cycle'])]) != 1:
                request_problems.append(f"cycle {event['cycle']}: conflicting purposes within worker")
        request['admission_queue_ms'] = admissions.get((key[0], key[2]), [])
        cycles = sorted({(event['cycle'], event['purpose']) for event in request['events']})
        request['cycles'] = [{'cycle': cycle, 'purpose': purpose} for cycle, purpose in cycles]
        request['cycle_stages'] = []
        for cycle in cycles:
            cycle_events = [event for event in request['events']
                            if (event['cycle'], event['purpose']) == cycle]
            request['cycle_stages'].append({
                'cycle': cycle[0], 'purpose': cycle[1],
                'stages': {stage: stage_observation(
                    [event for event in cycle_events if event['stage'] == stage], [cycle])[0]
                    for stage in STAGES}})
        for stage in STAGES:
            selected = [event for event in request['events'] if event['stage'] == stage]
            observation, stage_problems = stage_observation(selected, cycles)
            request['stages'][stage] = observation
            request_problems.extend(f'{stage}{problem}' for problem in stage_problems)
        request['problems'] = sorted(set(request_problems))
        request['task_overlap'] = task_overlap(request['timing_details'])
        problems.extend(f'{key}: {problem}' for problem in request['problems'])
        request['status'] = 'observed' if request['events'] and len(terminals) == 1 and not request_problems else 'UNKNOWN'
    incomplete = problems or not requests or any(request['status'] == 'UNKNOWN' or any(
        value['status'] == 'UNKNOWN' and value['reasons'] for value in request['stages'].values()) for request in requests.values())
    return {'schema': 1, 'status': 'incomplete' if incomplete else 'observed',
            'problems': problems, 'requests': list(requests.values()), 'unlinked_events': unlinked,
            'phase_meanings': {'lowering': 'GHC hscDesugar and hscSimplify; excludes prepared STG',
                               'prepared_stg': 'GHC CorePrep, coreToStg and stg2stg'},
            'interpretation': 'Hits name one stage only. Each request cycle requires stage completion or explicit applicability evidence; missing evidence is UNKNOWN. A not_applicable cycle supplies no work count. Admission queue times are shared by the transaction, not additive per request. Phase totals without interval boundaries remain nonexclusive; do not sum overlapping timers. task_overlap uses only qualified postload task-service intervals; UNKNOWN is not zero.'}


def stage_observation(selected, cycles):
    """One validator for both request aggregates and separately retained cycles."""
    complete = {(event['cycle'], event['purpose']) for event in selected if event['decision'] == 'complete'}
    not_applicable = {(event['cycle'], event['purpose']) for event in selected
                      if event['decision'] == 'not_applicable'}
    touched = {(event['cycle'], event['purpose']) for event in selected}
    problems = []
    for cycle in touched:
        ordered = [event for event in selected if (event['cycle'], event['purpose']) == cycle]
        closures = [event for event in ordered if event['decision'] in ('complete', 'not_applicable')]
        if (len(closures) != 1 or ordered[-1]['decision'] not in ('complete', 'not_applicable')
                or (closures[0]['decision'] == 'not_applicable' and len(ordered) != 1)):
            problems.append(f' cycle {cycle[0]}: expected exactly one final stage completion '
                            'or applicability observation without decisions')
    covered = complete | not_applicable
    if selected and covered != set(cycles):
        problems.append(': missing stage completion or applicability for request cycles')
    status = 'UNKNOWN'
    if not problems and covered and covered == touched and covered == set(cycles):
        status = 'observed' if complete else 'not_applicable'
    counts, byte_counts, reasons, accounted = Counter(), Counter(), Counter(), 0
    for event in selected:
        if event['decision'] in ('complete', 'not_applicable'):
            continue
        counts[event['decision']] += event['items']
        reasons[event['reason']] += event['items']
        if event['bytes'] is not None:
            accounted += 1
            byte_counts[event['decision']] += event['bytes']
    return ({'status': status, 'counts': dict(counts) if status == 'observed' else None,
             'bytes': dict(byte_counts) if status == 'observed' and accounted else None,
             'byte_accounted_events': accounted, 'reasons': dict(reasons),
             'completion_cycles': len(complete), 'not_applicable_cycles': len(not_applicable)}, problems)


def compare_control(normal, disabled, stage):
    """Require actual extra work for matching module versions, not faster latency."""
    problems = []
    if stage not in ('source_frontend', 'prepared_body'):
        return {'status': 'incomplete', 'problems': ['unsupported disable control stage']}
    for label, request in (('normal', normal), ('disabled', disabled)):
        if request.get('status') != 'observed' or request.get('exit_code') != 0 or request['stages'][stage]['status'] != 'observed':
            problems.append(f'{label}: successful completed stage evidence required')
    def owners(request, decision):
        result = Counter()
        for event in request['events']:
            if event['stage'] == stage and event['decision'] == decision and event['items'] > 0 and event['unit'] is not None:
                result[(event['unit'], event['module'], event['version_kind'], event['version'])] += event['items']
        return result
    normal_hits, normal_work_by_owner = owners(normal, 'hit'), owners(normal, 'work')
    disabled_hits, disabled_work_by_owner = owners(disabled, 'hit'), owners(disabled, 'work')
    markers = owners(disabled, 'disabled')
    normal_roster = normal_hits.keys() | normal_work_by_owner.keys()
    disabled_roster = disabled_hits.keys() | disabled_work_by_owner.keys()
    if not normal_roster or normal_roster != disabled_roster:
        problems.append('controls do not describe the same exact module/version roster')
    if not normal_hits or not normal_hits.keys() <= markers.keys() <= normal_roster:
        problems.append('disable markers must cover actual normal-hit owners within the matched roster')
    for owner, hits in normal_hits.items():
        if disabled_hits[owner] > 0:
            problems.append(f'{owner}: disabled owner still reports a cache hit')
        if markers[owner] < hits or disabled_work_by_owner[owner] - normal_work_by_owner[owner] < hits:
            problems.append(f'{owner}: disabled hit was not replaced by actual owner work')
    normal_work = sum(event['items'] for event in normal['events'] if event['stage'] == stage and event['decision'] == 'work')
    disabled_work = sum(event['items'] for event in disabled['events'] if event['stage'] == stage and event['decision'] == 'work')
    if sum(normal_hits.values()) <= 0 or disabled_work <= normal_work or disabled_work <= 0:
        problems.append('control lacks a normal hit and nonzero increased actual work')
    return {'status': 'observed' if not problems else 'incomplete', 'stage': stage,
            'normal_work': normal_work, 'disabled_work': disabled_work,
            'work_delta': disabled_work - normal_work, 'problems': problems,
            'owner_work_deltas': [{'unit': owner[0], 'module': owner[1], 'version_kind': owner[2], 'version': owner[3],
                                  'normal_hits': hits, 'normal_work': normal_work_by_owner[owner],
                                  'disabled_work': disabled_work_by_owner[owner], 'disabled_hits': disabled_hits[owner]}
                                 for owner, hits in sorted(normal_hits.items())]}


def load_retained_events(path):
    """Parse one bounded immutable byte snapshot and retain its exact hash."""
    if path.stat().st_size > 64 * 1024 * 1024:
        raise ValueError('trace exceeds 64 MiB; retain a bounded exact-request trace')
    with path.open('rb') as stream:
        snapshot = stream.read(64 * 1024 * 1024 + 1)
    if len(snapshot) > 64 * 1024 * 1024:
        raise ValueError('trace exceeds 64 MiB; retain a bounded exact-request trace')
    digest = hashlib.sha256(snapshot).hexdigest()
    rows = []
    for line in snapshot.splitlines():
        if len(line) > 1024 * 1024:
            raise ValueError('trace row exceeds one MiB')
        if line.strip():
            row = json.loads(line)
            if not isinstance(row, dict):
                raise ValueError('trace row is not an object')
            rows.append(row)
    with path.open('rb') as stream:
        after = stream.read(64 * 1024 * 1024 + 1)
    if hashlib.sha256(after).hexdigest() != digest:
        raise ValueError('trace changed during reporting; retain an immutable request trace')
    return rows, {'path': str(path.resolve()), 'sha256': digest}


CELL_PREFIX = 'resident-cell-correlation '


def load_cell_correlations(path):
    """Read a bounded immutable resident-cell correlation log snapshot."""
    limit = 64 * 1024 * 1024
    if path.stat().st_size > limit:
        raise ValueError('cell correlation log exceeds 64 MiB')
    with path.open('rb') as stream:
        snapshot = stream.read(limit + 1)
    if len(snapshot) > limit:
        raise ValueError('cell correlation log exceeds 64 MiB')
    digest = hashlib.sha256(snapshot).hexdigest()
    rows = []
    for line_number, line in enumerate(snapshot.splitlines(), 1):
        if len(line) > 1024 * 1024:
            raise ValueError('cell correlation row exceeds one MiB')
        if not line.startswith(CELL_PREFIX.encode()):
            continue
        try:
            row = json.loads(line[len(CELL_PREFIX):])
        except (ValueError, UnicodeDecodeError) as error:
            raise ValueError(f'cell correlation row {line_number}: invalid JSON') from error
        if not isinstance(row, dict):
            raise ValueError(f'cell correlation row {line_number}: expected object')
        rows.append({'line': line_number, 'raw': line.decode('utf-8'), 'row': row})
    with path.open('rb') as stream:
        after = stream.read(limit + 1)
    if hashlib.sha256(after).hexdigest() != digest:
        raise ValueError('cell correlation log changed during reporting')
    if not rows:
        raise ValueError('cell correlation log contains no resident-cell-correlation rows')
    return rows, {'path': str(path.resolve()), 'sha256': digest}


def cell_correlation_workload(rows, requests, scenario):
    """Join completed resident cells to unique physical daemon requests."""
    problems, cases, joined, included, warmups = [], [], set(), [], 0
    cell_keys, source_hashes = set(), set()
    starts = []
    for request in requests:
        if 'duplicate physical request start' in request.get('problems', []):
            continue
        physical = request['identity']
        starts.append((physical['daemon_epoch'], physical['admission_id'],
                       physical['request_ordinal'], request['compile_request'], request))
    refs_count = joined_refs = 0
    cells_with_join = set()
    for retained in rows:
        row, line_number = retained['row'], retained['line']
        if row.get('schema') != 1 or type(row.get('schema')) is not int:
            problems.append(f'line {line_number}: unsupported cell correlation schema')
            continue
        phase = row.get('phase')
        if phase == 'warmup':
            warmups += 1
            continue
        if phase not in ('measured', 'binding_growth'):
            problems.append(f'line {line_number}: unsupported or missing cell phase')
            continue
        index, label = row.get('index'), row.get('label')
        path_value, digest = row.get('source_path'), row.get('source_sha256')
        source_path = Path(path_value) if isinstance(path_value, str) else Path('')
        if (not natural(index) or not text(label) or not source_path.is_absolute()
                or not isinstance(digest, str) or not re.fullmatch(r'[0-9a-f]{64}', digest)
                or type(row.get('completed')) is not bool or not row['completed']):
            problems.append(f'line {line_number}: malformed or incomplete authored cell record')
            continue
        try:
            source_valid = (source_path.is_file() and source_path.stat().st_size <= 32 * 1024 * 1024
                            and hashlib.sha256(source_path.read_bytes()).hexdigest() == digest)
        except OSError:
            source_valid = False
        if not source_valid:
            problems.append(f'line {line_number}: authored source path/hash missing or changed')
            continue
        cell_key = (index, label, str(source_path), digest)
        if cell_key in cell_keys:
            problems.append(f'line {line_number}: duplicate authored cell correlation record')
        cell_keys.add(cell_key)
        source_hashes.add(digest)
        compiler_requests = row.get('compiler_requests')
        if not isinstance(compiler_requests, list):
            problems.append(f'line {line_number}: compiler_requests must be an array')
            continue
        if not compiler_requests:
            problems.append(f'line {line_number}: authored cell has no linked compiler requests')
            continue
        for request_index, compiler_request in enumerate(compiler_requests):
            refs_count += 1
            if not isinstance(compiler_request, dict):
                problems.append(f'line {line_number}: compiler request reference is not an object')
                continue
            epoch = compiler_request.get('daemon_epoch')
            admission = compiler_request.get('admission_id')
            ordinal = compiler_request.get('request_ordinal')
            compile_digest = compiler_request.get('compile_request')
            if (not text(epoch) or not natural(admission) or admission == 0
                    or not natural(ordinal) or ordinal == 0
                    or not text(compile_digest)):
                problems.append(f'line {line_number}: compiler request reference lacks exact identity')
                continue
            matches = [request for start_epoch, start_admission, start_ordinal, start_digest, request in starts
                       if (start_epoch, start_admission, start_ordinal, start_digest) ==
                       (epoch, admission, ordinal, compile_digest)]
            if len(matches) != 1:
                problems.append(f'line {line_number}: compiler request reference joined {len(matches)} daemon starts')
                continue
            physical_request = matches[0]
            physical_key = tuple(physical_request['identity'][key] for key in KEY_FIELDS)
            if physical_key in joined:
                problems.append(f'line {line_number}: physical request is referenced more than once')
            joined.add(physical_key)
            joined_refs += 1
            cells_with_join.add(cell_key)
            case_name = f"cell-{index}-{line_number}-{request_index}"
            cases.append({'name': case_name, 'scenario': scenario, 'step': len(cases),
                          'identity': physical_request['identity'],
                          'source': {'path': str(source_path), 'sha256': digest}})
            included.append({'line': line_number, 'cell_index': index, 'label': label,
                             'phase': phase, 'request_identity': physical_request['identity'],
                             'source': {'path': str(source_path), 'sha256': digest}})
    return {'schema': 1, 'cases': cases}, {
        'status': 'observed' if not problems else 'incomplete',
        'observed_counts': {'correlation_rows': len(rows) - warmups,
                            'warmup_rows_excluded': warmups,
                            'authored_cells': len(cell_keys),
                            'authored_cells_with_joined_requests': len(cells_with_join),
                            'compiler_request_references': refs_count,
                            'joined_request_references': joined_refs,
                            'unique_joined_physical_requests': len(joined),
                            'distinct_authored_source_hashes': len(source_hashes),
                            'authored_cell_join_coverage': (len(cells_with_join) / len(cell_keys)) if cell_keys else None,
                            'request_reference_join_coverage': (joined_refs / refs_count) if refs_count else None},
        'configured_demand_counts': None, 'joined_cells': included,
        'problems': problems}


def analyze_workload(manifest, report):
    """Link retained authored inputs and controls to physical requests."""
    problems, cases, groups, used_invocations = [], {}, {}, set()
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
        physical = tuple(request['identity'][key] for key in KEY_FIELDS)
        if physical in used_invocations:
            problems.append(f'{name}: physical request reused across workload cases')
        used_invocations.add(physical)
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
        starts = [request['start_row'] for _, request in ordered]
        if starts != sorted(starts) or len(set(starts)) != len(starts):
            problems.append(f'{scenario}: authored step order differs from actual request order')
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
    parser.add_argument('--cell-log', type=Path, help='retained resident-cell-correlation log')
    parser.add_argument('--scenario', choices=('distinct', 'repeat', 'binding_growth', 'aba', 'control'))
    parser.add_argument('--write-workload', type=Path, help='write the combined generated workload manifest')
    args = parser.parse_args()
    if bool(args.cell_log) != bool(args.scenario):
        parser.error('--cell-log and --scenario must be supplied together')
    if args.write_workload and not (args.cell_log or args.workload):
        parser.error('--write-workload requires --cell-log or --workload')
    events, reference = load_retained_events(args.trace)
    report = analyze(events)
    report['input'] = reference
    workload_manifest = None
    if args.workload:
        if args.workload.stat().st_size > 4 * 1024 * 1024:
            raise ValueError('workload manifest exceeds four MiB')
        workload_bytes = args.workload.read_bytes()
        workload_manifest = json.loads(workload_bytes)
        if not isinstance(workload_manifest, dict):
            raise ValueError('workload manifest must be an object')
        report['workload_input'] = {'path': str(args.workload.resolve()),
                                    'sha256': hashlib.sha256(workload_bytes).hexdigest()}
    if args.cell_log:
        retained_cells, cell_reference = load_cell_correlations(args.cell_log)
        generated, correlation = cell_correlation_workload(retained_cells, report['requests'], args.scenario)
        report['cell_correlation'] = {**correlation, 'input': cell_reference,
                                      'scenario': args.scenario,
                                      'retained_rows': [{'line': row['line'], 'raw': row['raw'],
                                                         'parsed': row['row']} for row in retained_cells]}
        if correlation['status'] != 'observed':
            report['problems'].append('cell correlation evidence incomplete')
        if workload_manifest is None:
            workload_manifest = generated
        else:
            if workload_manifest.get('schema') != 1 or not isinstance(workload_manifest.get('cases'), list):
                raise ValueError('existing workload requires schema 1 and cases')
            workload_manifest['cases'].extend(generated['cases'])
    if workload_manifest is not None:
        report['workload'] = analyze_workload(workload_manifest, report)
        if report['workload']['status'] != 'observed':
            report['problems'].append('workload/control evidence incomplete')
        if args.write_workload:
            args.write_workload.write_text(json.dumps(workload_manifest, indent=2) + '\n')
    for request in report['requests']:
        for stage in args.require_stage:
            if request['stages'][stage]['status'] != 'observed':
                report['problems'].append(f"{request['identity']}: required stage {stage} is UNKNOWN")
    if report['problems']:
        report['status'] = 'incomplete'
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'status': report['status'], 'requests': len(report['requests']),
                      'cell_correlation': report.get('cell_correlation', {}).get('observed_counts'),
                      'problems': report['problems']}))
    return 0 if report['status'] == 'observed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
