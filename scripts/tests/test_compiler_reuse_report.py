import importlib.util
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import unittest
import tempfile
import hashlib
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('compiler_reuse', Path(__file__).parents[1] / 'compiler-reuse-report.py')
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


def packet(decision='hit', stage='source_frontend', items=1, reason=None):
    reasons = {'hit': 'matched', 'miss': 'absent', 'work': 'absent',
               'disabled': 'cache_disabled', 'complete': 'stage_complete', 'not_applicable': 'check_only'}
    terminal = decision in ('complete', 'not_applicable')
    return {'schema': 1, 'cycle': 7, 'purpose': 'cell_program', 'observed_ns': 100,
            'stage': stage, 'decision': decision, 'reason': reason or reasons[decision],
            'unit': None if terminal else 'main',
            'module': None if terminal else 'Support',
            'version_kind': None if terminal else 'source_fingerprint',
            'version': None if terminal else 'abcdef', 'items': items, 'bytes': None}


def trace(packets, ordinal=1, digest='request'):
    span = {'daemon_epoch': 'epoch', 'worker_pid': 123, 'admission_id': 4,
            'request_ordinal': ordinal, 'compile_request': digest, 'transaction': True}
    rows = [{'fields': {'message': 'compiler request started'}, 'span': span}]
    for event in packets:
        rows.append({'fields': {'message': 'compiler timing', 'line': REPORT.PREFIX + json.dumps(event)}, 'span': span})
    rows.append({'fields': {'message': 'compiler request finished', 'elapsed_ms': 100, 'exit_code': 0}, 'span': span})
    return rows


def completed(packets, stage='source_frontend'):
    return packets + [packet('complete', stage, 0)]


def timing_detail(phase='prepared_stg_task_service', start=10, end=20, **changes):
    fields = {'parent': 'prepared_graph', 'phase': phase, 'owner_unit': 'main',
              'owner_module': changes.pop('owner_module', 'Support'), 'ms': 1, 'start_ns': start,
              'end_ns': end, 'wall_ns': end - start, 'cpu_ns': end - start,
              'allocated_bytes': 7}
    fields.update(changes)
    return 'tidepool-timing-detail ' + ' '.join(f'{key}={value}' for key, value in fields.items())


def with_detail_lines(lines):
    rows = trace([])
    span = rows[0]['span']
    rows[1:1] = [{'fields': {'message': 'compiler timing', 'line': line}, 'span': span}
                 for line in lines]
    return rows


class ReuseEvidenceControls(unittest.TestCase):
    def test_stage_hit_keeps_other_stages_unknown(self):
        result = REPORT.analyze(trace(completed([packet()])))
        request = result['requests'][0]
        self.assertEqual(request['stages']['source_frontend']['counts'], {'hit': 1})
        self.assertIsNone(request['stages']['source_frontend']['bytes'])
        self.assertEqual(request['stages']['prepared_body']['status'], 'UNKNOWN')
        self.assertEqual(request['service_ms'], 100)
        self.assertEqual(len(request['events']), 2)

    def test_missing_completion_is_not_zero_or_hit(self):
        result = REPORT.analyze(trace([packet()]))
        stage = result['requests'][0]['stages']['source_frontend']
        self.assertEqual(result['status'], 'incomplete')
        self.assertEqual(stage['status'], 'UNKNOWN')
        self.assertIsNone(stage['counts'])

    def test_completion_must_be_unique_and_final(self):
        for events in ([packet('complete', items=0), packet('work')],
                       [packet(), packet('complete', items=0), packet('complete', items=0)]):
            with self.subTest(events=events):
                report = REPORT.analyze(trace(events))
                self.assertEqual(report['status'], 'incomplete')
                self.assertEqual(report['requests'][0]['stages']['source_frontend']['status'], 'UNKNOWN')

    def test_reuse_events_after_terminal_join_by_exact_physical_request(self):
        rows = trace(completed([packet()]))
        start, events, terminal = rows[0], rows[1:-1], rows[-1]
        report = REPORT.analyze([start, terminal, *events])
        request = report['requests'][0]
        self.assertEqual(report['status'], 'observed')
        self.assertEqual(request['status'], 'observed')
        self.assertEqual(request['stages']['source_frontend']['counts'], {'hit': 1})
        self.assertTrue(all(event['correlation'] == {
            'basis': 'exact_physical_request_identity',
            'terminal_order': 'after', 'request_start_row': 0, 'request_terminal_row': 1,
        } for event in request['events']))

    def test_duplicate_late_reuse_event_keeps_request_incomplete(self):
        rows = trace(completed([packet()]))
        duplicate = dict(rows[1])
        duplicate['span'] = dict(rows[1]['span'])
        report = REPORT.analyze([rows[0], rows[-1], *rows[1:-1], duplicate])
        request = report['requests'][0]
        self.assertEqual(report['status'], 'incomplete')
        self.assertEqual(request['status'], 'UNKNOWN')
        self.assertTrue(any('duplicate reuse event for physical request' in problem
                            for problem in request['problems']))

    def test_late_reuse_event_without_exact_identity_is_unlinked(self):
        rows = trace(completed([packet()]))
        late = dict(rows[1])
        late['span'] = {'compile_request': 'request'}
        report = REPORT.analyze([rows[0], rows[-1], late, *rows[2:-1]])
        request = report['requests'][0]
        self.assertEqual(report['status'], 'incomplete')
        self.assertEqual(len(report['unlinked_events']), 1)
        self.assertTrue(any('diagnostic event lacks exact request linkage' in problem
                            for problem in report['problems']))

    def test_cycle_purpose_conflicts_are_refused_within_worker(self):
        end = packet('complete', items=0)
        end['purpose'] = 'lookup_type'
        report = REPORT.analyze(trace(completed([packet()]) + [end]))
        self.assertEqual(report['status'], 'incomplete')
        rows = trace(completed([packet()])) + trace([end], ordinal=2)
        report = REPORT.analyze(rows)
        self.assertTrue(all(request['status'] == 'UNKNOWN' for request in report['requests']))

    def test_completed_zero_stage_is_explicit(self):
        request = REPORT.analyze(trace([packet('complete', items=0)]))['requests'][0]
        self.assertEqual(request['stages']['source_frontend']['counts'], {})
        self.assertEqual(request['stages']['source_frontend']['status'], 'observed')

    def test_cycle_views_do_not_hide_an_incomplete_later_cycle(self):
        first = completed([packet('hit')])
        later = packet('work')
        later['cycle'] = 8
        request = REPORT.analyze(trace(first + [later]))['requests'][0]
        self.assertEqual(request['stages']['source_frontend']['status'], 'UNKNOWN')
        self.assertEqual(request['cycle_stages'][0]['stages']['source_frontend']['counts'], {'hit': 1})
        self.assertEqual(request['cycle_stages'][1]['stages']['source_frontend']['status'], 'UNKNOWN')
        self.assertIsNone(request['cycle_stages'][1]['stages']['source_frontend']['counts'])

    def test_buffered_legacy_diagnostics_join_by_physical_identity(self):
        for line in ('tidepool-count name=activation_preview_frontends count=9',
                     'tidepool-count name=exact_execution_original_load_owners count=9',
                     'tidepool-timing phase=retained_finalized_bytecode ms=2'):
            for position in ('before', 'after'):
                with self.subTest(line=line, position=position):
                    rows = trace(completed([packet()]))
                    diagnostic = {'fields': {'line': line}, 'span': rows[0]['span']}
                    rows.insert(0 if position == 'before' else len(rows), diagnostic)
                    report = REPORT.analyze(rows)
                    request = report['requests'][0]
                    self.assertEqual(report['status'], 'observed')
                    self.assertEqual(request['legacy_status'], 'observed')
                    self.assertEqual(request['legacy_observations'][0]['boundary_status'], 'observed')
                    if 'count=' in line:
                        self.assertIn(9, request['legacy_counts'].values())
                    else:
                        self.assertEqual(request['phases_ms'], {'retained_finalized_bytecode': [2]})


    def test_legacy_totals_need_one_terminal_and_do_not_hide_partial_subtotals(self):
        for terminal in ('missing', 'duplicate', 'after'):
            with self.subTest(terminal=terminal):
                rows = trace(completed([packet()]))
                diagnostic = {'fields': {'line': 'tidepool-count name=activation_preview_frontends count=1'}, 'span': rows[0]['span']}
                rows.insert(1, diagnostic)
                if terminal == 'missing':
                    rows.pop()
                elif terminal == 'duplicate':
                    rows.append(rows[-1])
                else:
                    rows.append(diagnostic)
                request = REPORT.analyze(rows)['requests'][0]
                self.assertEqual(request['legacy_status'], 'UNKNOWN')
                self.assertEqual(request['legacy_counts'], {})
                self.assertTrue(request['legacy_observations'])

    def test_checked_then_native_cycle_preserves_actual_work(self):
        stages = ('prepared_body', 'site_witness', 'original_recovery', 'raw_projection')
        checked = [packet('complete', items=0)]
        native = [packet('complete', items=0)]
        for stage in stages:
            checked.append(packet('not_applicable', stage, 0))
            native += completed([packet('hit', stage, 32), packet('work', stage)], stage)
        for event in native:
            event['cycle'] = 8
        report = REPORT.analyze(trace(checked + native))
        self.assertEqual(report['status'], 'observed')
        self.assertEqual(report['problems'], [])
        for stage in stages:
            observed = report['requests'][0]['stages'][stage]
            self.assertEqual(observed['status'], 'observed')
            self.assertEqual(observed['counts'], {'hit': 32, 'work': 1})
            self.assertEqual(observed['completion_cycles'], 1)
            self.assertEqual(observed['not_applicable_cycles'], 1)
            self.assertEqual(observed['reasons'], {'matched': 32, 'absent': 1})

    def test_check_only_stage_is_inapplicable_not_observed_zero(self):
        report = REPORT.analyze(trace([packet('not_applicable', 'prepared_body', 0)]))
        self.assertEqual(report['status'], 'observed')
        stage = report['requests'][0]['stages']['prepared_body']
        self.assertEqual(stage['status'], 'not_applicable')
        self.assertIsNone(stage['counts'])
        self.assertEqual(stage['completion_cycles'], 0)
        self.assertEqual(stage['not_applicable_cycles'], 1)

    def test_stage_applicability_cannot_be_inferred_from_other_cycle(self):
        native = completed([packet('work', 'prepared_body')], 'prepared_body')
        for event in native:
            event['cycle'] = 8
        report = REPORT.analyze(trace([packet('complete', items=0)] + native))
        self.assertEqual(report['status'], 'incomplete')
        self.assertEqual(report['requests'][0]['stages']['prepared_body']['status'], 'UNKNOWN')
        self.assertTrue(any('missing stage completion or applicability' in problem
                            for problem in report['problems']))

    def test_applicability_refuses_decisions_and_contradictory_terminals(self):
        not_applicable = packet('not_applicable', 'prepared_body', 0)
        for events in ([packet('work', 'prepared_body'), not_applicable],
                       [not_applicable, packet('hit', 'prepared_body')],
                       [not_applicable, not_applicable],
                       [not_applicable, packet('complete', 'prepared_body', 0)],
                       [packet('complete', 'prepared_body', 0), not_applicable]):
            with self.subTest(events=events):
                report = REPORT.analyze(trace(events))
                self.assertEqual(report['status'], 'incomplete')
                self.assertEqual(report['requests'][0]['stages']['prepared_body']['status'], 'UNKNOWN')

    def test_applicability_wire_requires_check_only_native_stage_and_empty_owner(self):
        for changes in ({'stage': 'source_frontend'}, {'stage': 'future_stage'},
                        {'reason': 'stage_complete'}, {'items': 1}, {'bytes': 0},
                        {'unit': 'main'}, {'version': 'actual-owner'}, {'schema': 2}):
            with self.subTest(changes=changes):
                event = packet('not_applicable', 'prepared_body', 0)
                event.update(changes)
                self.assertEqual(REPORT.analyze(trace([event]))['status'], 'incomplete')
        event = packet('work', 'prepared_body', reason='check_only')
        self.assertEqual(REPORT.analyze(trace(completed([event], 'prepared_body')))['status'], 'incomplete')

    def test_failed_request_does_not_gain_applicability_from_successful_check_cycle(self):
        checked = [packet('not_applicable', 'prepared_body', 0)]
        native = packet('work', 'prepared_body')
        native['cycle'] = 8
        for native_events in ([], [native]):
            with self.subTest(native_events=native_events):
                rows = trace(checked + native_events)
                rows[-1]['fields'].update(message='compiler request failed', exit_code=1)
                report = REPORT.analyze(rows)
                self.assertEqual(report['status'], 'incomplete')
                self.assertEqual(report['requests'][0]['status'], 'UNKNOWN')
                self.assertIsNone(report['requests'][0]['stages']['prepared_body']['counts'])
        for terminal in ({'message': 'compiler request abandoned by client', 'exit_code': 0},
                         {'exit_code': None}, {'exit_code': True}, {'exit_code': 1}):
            with self.subTest(terminal=terminal):
                rows = trace(checked)
                rows[-1]['fields'].update(terminal)
                self.assertEqual(REPORT.analyze(rows)['status'], 'incomplete')

    def test_native_image_identity_does_not_need_fabricated_module(self):
        event = packet(stage='native_image')
        event.update(unit=None, module=None, version_kind='image_identity', version='actual-image-key')
        request = REPORT.analyze(trace(completed([event], 'native_image')))['requests'][0]
        self.assertEqual(request['stages']['native_image']['counts'], {'hit': 1})

    def test_hit_with_disable_reason_cannot_pass(self):
        event = packet(reason='cache_disabled')
        self.assertEqual(REPORT.analyze(trace(completed([event])))['status'], 'incomplete')

    def test_identical_digest_cannot_join_distinct_invocations(self):
        rows = trace(completed([packet()]))
        rows += trace(completed([packet('work')]), ordinal=2)
        result = REPORT.analyze(rows)
        self.assertEqual(len(result['requests']), 2)
        self.assertEqual(result['requests'][0]['stages']['source_frontend']['counts'], {'hit': 1})
        self.assertEqual(result['requests'][1]['stages']['source_frontend']['counts'], {'work': 1})

    def test_digest_only_old_trace_is_unlinked(self):
        rows = trace(completed([packet()]))
        rows[1].pop('span')
        rows[1]['fields']['compile_request'] = 'request'
        result = REPORT.analyze(rows)
        self.assertEqual(result['status'], 'incomplete')
        self.assertEqual(len(result['unlinked_events']), 1)

    def test_event_after_terminal_retains_physical_identity_correlation(self):
        rows = trace(completed([packet()]))
        report = REPORT.analyze([rows[0], rows[-1], *rows[1:-1]])
        self.assertEqual(report['status'], 'observed')
        self.assertTrue(all(event['correlation']['basis'] == 'exact_physical_request_identity'
                            and event['correlation']['terminal_order'] == 'after'
                            for event in report['requests'][0]['events']))

    def test_old_lowering_phase_without_interval_is_retained(self):
        rows = trace(completed([packet()]))
        rows.insert(1, {'fields': {'line': 'tidepool-timing phase=lowering ms=11470'}, 'span': rows[0]['span']})
        result = REPORT.analyze(rows)
        self.assertEqual(result['requests'][0]['phases_ms']['lowering'], [11470])
        self.assertIn('nonexclusive', result['interpretation'])

    def test_task_overlap_sweep_distinguishes_disjoint_from_overlapping_intervals(self):
        disjoint = REPORT.analyze(with_detail_lines([
            timing_detail(start=10, end=20), timing_detail(start=20, end=30)]))['requests'][0]['task_overlap']
        self.assertEqual(disjoint['status'], 'observed')
        self.assertEqual(disjoint['maximum_simultaneous_tasks'], 1)
        self.assertEqual(disjoint['overlapping_wall_ns'], 0)
        overlap = REPORT.analyze(with_detail_lines([
            timing_detail(start=10, end=30, owner_module='PreparedA'),
            timing_detail(phase='raw_projection_task_service', start=20, end=40, owner_module='RawA'),
            timing_detail(phase='raw_projection_task_service', start=25, end=28, owner_module='RawB')]))['requests'][0]['task_overlap']
        self.assertEqual(overlap['maximum_simultaneous_tasks'], 3)
        self.assertEqual(overlap['overlapping_wall_ns'], 10)
        self.assertEqual(overlap['by_phase']['prepared_stg_task_service']['maximum_simultaneous_tasks'], 1)
        self.assertEqual(overlap['by_phase']['raw_projection_task_service']['maximum_simultaneous_tasks'], 2)
        self.assertEqual(overlap['by_phase']['raw_projection_task_service']['overlapping_wall_ns'], 3)
        self.assertFalse(overlap['mixed_phase_overlap_is_independent_cpu_proof'])
        same_owner = REPORT.analyze(with_detail_lines([
            timing_detail(start=10, end=30, owner_module='Shared'),
            timing_detail(phase='raw_projection_task_service', start=20, end=40, owner_module='Shared')]))['requests'][0]['task_overlap']
        self.assertEqual(same_owner['maximum_simultaneous_tasks'], 1)
        self.assertEqual(same_owner['overlapping_wall_ns'], 0)
        self.assertNotIn('cpu_ns', overlap)
        self.assertNotIn('allocated_bytes', overlap)

    def test_task_overlap_ignores_nested_non_task_parent_span(self):
        report = REPORT.analyze(with_detail_lines([
            timing_detail(start=10, end=30),
            timing_detail(phase='prepared_graph', start=0, end=100, owner_unit='other', owner_module='Parent')]))
        overlap = report['requests'][0]['task_overlap']
        self.assertEqual(overlap['status'], 'observed')
        self.assertEqual(len(overlap['qualified_spans']), 1)
        self.assertEqual(overlap['maximum_simultaneous_tasks'], 1)

    def test_actual_source_phase_overlap_requires_its_ghc_load_parent(self):
        for phase in ('source_frontend_task_service', 'source_finalization_task_service'):
            with self.subTest(phase=phase):
                lines = [timing_detail(phase=phase, parent='ghc_load', start=10, end=30,
                                       owner_module='First'),
                         timing_detail(phase=phase, parent='ghc_load', start=20, end=40,
                                       owner_module='Second')]
                overlap = REPORT.analyze(with_detail_lines(lines))['requests'][0]['task_overlap']
                self.assertEqual(overlap['by_phase'][phase]['maximum_simultaneous_distinct_owners'], 2)
                self.assertEqual(overlap['by_phase'][phase]['overlapping_wall_ns'], 10)
                invalid = REPORT.analyze(with_detail_lines([
                    timing_detail(phase=phase, parent='prepared_graph')]))['requests'][0]['task_overlap']
                self.assertEqual(invalid['status'], 'UNKNOWN')

    def test_task_overlap_missing_or_invalid_owner_and_endpoints_is_unknown(self):
        for changes in ({'owner_unit': ''}, {'owner_module': ''}, {'start_ns': 'bad'}, {'end_ns': 9}):
            with self.subTest(changes=changes):
                report = REPORT.analyze(with_detail_lines([timing_detail(start=10, end=20, **changes)]))
                overlap = report['requests'][0]['task_overlap']
                self.assertEqual(overlap['status'], 'UNKNOWN')
                self.assertIsNone(overlap['maximum_simultaneous_tasks'])
                self.assertIsNone(overlap['overlapping_wall_ns'])

    def test_zero_length_task_span_has_no_positive_overlap(self):
        overlap = REPORT.analyze(with_detail_lines([timing_detail(start=10, end=10)]))['requests'][0]['task_overlap']
        self.assertEqual(overlap['status'], 'observed')
        self.assertEqual(overlap['maximum_simultaneous_tasks'], 0)
        self.assertEqual(overlap['overlapping_wall_ns'], 0)

    def test_task_overlap_refuses_mismatched_physical_envelope(self):
        rows = with_detail_lines([timing_detail()])
        rows[1]['fields']['compile_request'] = 'different-request'
        report = REPORT.analyze(rows)
        self.assertEqual(report['requests'][0]['task_overlap']['status'], 'UNKNOWN')
        self.assertTrue(any('digest conflicts with physical invocation' in problem
                            for problem in report['problems']))
        rows = with_detail_lines([timing_detail()])
        rows[1]['fields']['worker_pid'] = 999
        report = REPORT.analyze(rows)
        self.assertEqual(report['requests'][0]['task_overlap']['status'], 'UNKNOWN')
        self.assertTrue(report['unlinked_events'])

    def test_task_detail_rejects_duplicate_fields_wrong_parent_and_bad_wall(self):
        controls = [
            timing_detail().replace(' owner_module=Support', ''),
            timing_detail().replace(' end_ns=20', ''),
            timing_detail().replace(' wall_ns=10', ''),
            timing_detail() + ' owner_unit=other',
            timing_detail() + ' start_ns=10',
            timing_detail(parent='wrong_owner'),
            timing_detail(wall_ns=11),
            timing_detail(cpu_ns='bad'),
        ]
        for line in controls:
            with self.subTest(line=line):
                overlap = REPORT.analyze(with_detail_lines([line]))['requests'][0]['task_overlap']
                self.assertEqual(overlap['status'], 'UNKNOWN')
                self.assertIsNone(overlap['maximum_simultaneous_tasks'])

    def test_identical_task_interval_rows_do_not_create_concurrency(self):
        overlap = REPORT.analyze(with_detail_lines([timing_detail(), timing_detail()]))['requests'][0]['task_overlap']
        self.assertEqual(overlap['status'], 'UNKNOWN')
        self.assertTrue(any('duplicate task interval rows' in problem for problem in overlap['problems']))

    def test_same_owner_overlapping_intervals_within_one_phase_are_unknown(self):
        overlap = REPORT.analyze(with_detail_lines([
            timing_detail(start=10, end=30, owner_module='Repeated'),
            timing_detail(start=20, end=40, owner_module='Repeated')]))['requests'][0]['task_overlap']
        self.assertEqual(overlap['status'], 'UNKNOWN')
        self.assertTrue(any('overlapping intervals for owner' in problem for problem in overlap['problems']))

    def test_buffered_task_timing_detail_joins_by_physical_identity(self):
        rows = trace([])
        span = rows[0]['span']
        detail_row = {'fields': {'message': 'compiler timing', 'line': timing_detail()}, 'span': span}
        for location in ('before', 'after'):
            with self.subTest(location=location):
                rows = trace([])
                if location == 'before':
                    rows.insert(0, dict(detail_row))
                else:
                    rows.append(dict(detail_row))
                report = REPORT.analyze(rows)
                overlap = report['requests'][0]['task_overlap']
                self.assertEqual(overlap['status'], 'observed')
                self.assertEqual(len(overlap['qualified_spans']), 1)


    def test_cell_correlation_joins_all_requests_and_excludes_warmup(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'cell.hs'
            source.write_text('main = 42\n')
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            rows = trace([], ordinal=1, digest='request') + trace([], ordinal=2, digest='request2')
            report = REPORT.analyze(rows)
            def link(ordinal, compile_request):
                return {'daemon_epoch': 'epoch', 'admission_id': 4,
                        'request_ordinal': ordinal, 'compile_request': compile_request}
            correlation_rows = [
                {'line': 1, 'raw': 'warmup', 'row': {'schema': 1, 'phase': 'warmup'}},
                {'line': 2, 'raw': 'measured', 'row': {'schema': 1, 'phase': 'measured',
                 'index': 0, 'label': 'cell', 'source_path': str(source), 'source_sha256': digest,
                 'completed': True, 'compiler_requests': [link(1, 'request'), link(2, 'request2')]}}]
            workload, evidence = REPORT.cell_correlation_workload(correlation_rows, report['requests'], 'repeat')
            self.assertEqual(len(workload['cases']), 2)
            self.assertEqual(workload['cases'][0]['source'], workload['cases'][1]['source'])
            self.assertEqual(evidence['observed_counts']['authored_cells'], 1)
            self.assertEqual(evidence['observed_counts']['compiler_request_references'], 2)
            self.assertEqual(evidence['observed_counts']['unique_joined_physical_requests'], 2)
            self.assertEqual(evidence['observed_counts']['warmup_rows_excluded'], 1)
            self.assertIsNone(evidence['configured_demand_counts'])
            _, duplicate = REPORT.cell_correlation_workload(
                correlation_rows + [correlation_rows[-1]], report['requests'], 'repeat')
            self.assertEqual(duplicate['status'], 'incomplete')
            self.assertTrue(any('duplicate authored cell correlation record' in problem
                                for problem in duplicate['problems']))

    def test_cell_correlation_requires_unique_physical_start_and_source_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'cell.hs'
            source.write_text('main = 42\n')
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            duplicate = trace([], ordinal=1, digest='request')
            second = trace([], ordinal=1, digest='request')
            second[0]['span']['worker_pid'] = 124
            second[-1]['span']['worker_pid'] = 124
            report = REPORT.analyze(duplicate + second)
            row = {'schema': 1, 'phase': 'measured', 'index': 0, 'label': 'cell',
                   'source_path': str(source), 'source_sha256': digest, 'completed': True,
                   'compiler_requests': [{'daemon_epoch': 'epoch', 'admission_id': 4,
                                          'request_ordinal': 1, 'compile_request': 'request'}]}
            workload, evidence = REPORT.cell_correlation_workload(
                [{'line': 1, 'raw': 'row', 'row': row}], report['requests'], 'distinct')
            self.assertEqual(workload['cases'], [])
            self.assertEqual(evidence['status'], 'incomplete')
            self.assertTrue(any('joined 2 daemon starts' in problem for problem in evidence['problems']))
            row['source_sha256'] = '0' * 64
            _, evidence = REPORT.cell_correlation_workload(
                [{'line': 1, 'raw': 'row', 'row': row}], report['requests'], 'distinct')
            self.assertTrue(any('source path/hash missing or changed' in problem
                                for problem in evidence['problems']))

    def test_cell_correlation_log_retains_bounded_prefixed_rows_and_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'cells.log'
            content = ('noise\n' + REPORT.CELL_PREFIX + json.dumps({'schema': 1, 'phase': 'warmup'}) + '\n').encode()
            path.write_bytes(content)
            rows, reference = REPORT.load_cell_correlations(path)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]['row']['phase'], 'warmup')
            self.assertEqual(reference['sha256'], hashlib.sha256(content).hexdigest())

    def test_cli_generates_workload_from_cell_correlation_rows(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources, cases, requests = [], [], []
            for index, (name, digest_text) in enumerate((('A.hs', 'main = 1\n'), ('B.hs', 'main = 2\n'))):
                source = root / name
                source.write_text(digest_text)
                digest = hashlib.sha256(source.read_bytes()).hexdigest()
                sources.append(source)
                rows = trace([packet('complete', 'source_frontend', 0)],
                             ordinal=index + 1, digest=f'request{index}')
                requests.extend(rows)
                cases.append({'schema': 1, 'index': index, 'label': name, 'phase': 'measured',
                              'completed': True, 'source_path': str(source), 'source_sha256': digest,
                              'compiler_requests': [{'daemon_epoch': 'epoch', 'admission_id': 4,
                                  'request_ordinal': index + 1, 'compile_request': f'request{index}'}]})
            trace_path, cell_path = root / 'trace.jsonl', root / 'cells.log'
            output_path, workload_path = root / 'report.json', root / 'workload.json'
            trace_path.write_text(''.join(json.dumps(row) + '\n' for row in requests))
            cell_path.write_text(''.join(REPORT.CELL_PREFIX + json.dumps(row) + '\n' for row in cases))
            argv = ['compiler-reuse-report.py', '--trace', str(trace_path), '--output', str(output_path),
                    '--cell-log', str(cell_path), '--scenario', 'distinct', '--write-workload', str(workload_path)]
            with patch('sys.argv', argv), redirect_stdout(io.StringIO()):
                exit_code = REPORT.main()
            self.assertEqual(exit_code, 1)  # One cohort is intentionally incomplete without the other scenarios/controls.
            report = json.loads(output_path.read_text())
            workload = json.loads(workload_path.read_text())
            self.assertEqual(report['cell_correlation']['observed_counts']['authored_cells'], 2)
            self.assertEqual(report['cell_correlation']['observed_counts']['unique_joined_physical_requests'], 2)
            self.assertEqual([case['identity']['worker_pid'] for case in workload['cases']], [123, 123])
            self.assertEqual(report['workload']['status'], 'incomplete')

    def test_malformed_counts_identity_and_completion_fail_closed(self):
        for key, value in [('items', True), ('items', -1), ('version_kind', 'invented'), ('version', ''), ('schema', True)]:
            event = packet()
            event[key] = value
            with self.subTest(key=key, value=value):
                self.assertEqual(REPORT.analyze(trace(completed([event])))['status'], 'incomplete')
        event = packet('complete', items=1)
        self.assertEqual(REPORT.analyze(trace([event]))['status'], 'incomplete')

    def control(self, stage, disabled_work=1, version='abcdef', complete=True):
        normal = REPORT.analyze(trace(completed([packet(stage=stage)], stage)))['requests'][0]
        work = packet('work', stage, disabled_work)
        work['version'] = version
        rows = [packet('disabled', stage), work]
        if complete:
            rows = completed(rows, stage)
        disabled = REPORT.analyze(trace(rows, ordinal=2))['requests'][0]
        return REPORT.compare_control(normal, disabled, stage)

    def test_disable_controls_require_actual_nonzero_work_delta(self):
        for stage in ('source_frontend', 'prepared_body'):
            with self.subTest(stage=stage):
                self.assertEqual(self.control(stage)['status'], 'observed')
                self.assertEqual(self.control(stage, disabled_work=0)['status'], 'incomplete')
                self.assertEqual(self.control(stage, version='changed')['status'], 'incomplete')
                self.assertEqual(self.control(stage, complete=False)['status'], 'incomplete')

    def test_unrelated_extra_work_cannot_calibrate_surviving_hit(self):
        def owner(decision, name, items=1):
            event = packet(decision, items=items)
            event['module'] = name
            return event
        normal = REPORT.analyze(trace(completed([owner('hit', 'Support'), owner('work', 'UnrelatedWork')])))['requests'][0]
        for marker in ('NeverHit', 'Support'):
            disabled = REPORT.analyze(trace(completed([owner('hit', 'Support'), owner('disabled', marker),
                                                       owner('work', 'UnrelatedWork', 2)]), ordinal=2))['requests'][0]
            self.assertEqual(REPORT.compare_control(normal, disabled, 'source_frontend')['status'], 'incomplete')

    def test_missing_one_disabled_hit_owner_cannot_pass(self):
        other = packet()
        other['module'] = 'Other'
        normal = REPORT.analyze(trace(completed([packet(), other])))['requests'][0]
        disabled = REPORT.analyze(trace(completed([packet('disabled'), packet('work', items=2), other]), ordinal=2))['requests'][0]
        self.assertEqual(REPORT.compare_control(normal, disabled, 'source_frontend')['status'], 'incomplete')

    def test_retained_trace_reader_refuses_mutation_and_hashes_parsed_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'trace.jsonl'
            original = b'{"message":"original"}\n'
            path.write_bytes(original)
            rows, reference = REPORT.load_retained_events(path)
            self.assertEqual(rows, [{'message': 'original'}])
            self.assertEqual(reference['sha256'], hashlib.sha256(original).hexdigest())
            decoder = json.loads
            def change(line):
                path.write_text('{"message":"changed"}\n')
                return decoder(line)
            with patch.object(REPORT.json, 'loads', side_effect=change):
                with self.assertRaisesRegex(ValueError, 'trace changed during reporting'):
                    REPORT.load_retained_events(path)

    def test_multiple_cycles_require_completion_for_each(self):
        other = packet(stage='prepared_body')
        other['cycle'] = 8
        end = packet('complete', stage='prepared_body', items=0)
        end['cycle'] = 8
        report = REPORT.analyze(trace(completed([packet()]) + [other, end]))
        self.assertEqual(report['requests'][0]['stages']['source_frontend']['status'], 'UNKNOWN')

    def test_legacy_actual_work_counts_survive_without_reuse_claim(self):
        rows = trace([])
        rows.insert(1, {'span': rows[0]['span'], 'fields': {'line': 'tidepool-count name=transaction_reused_source_products count=0 count_ns=8'}})
        rows.insert(2, {'span': rows[0]['span'], 'fields': {'line': 'tidepool-compile-summary modules=27 typecheck_modules=26 lowering_modules=26 interface_modules=27'}})
        report = REPORT.analyze(rows)
        request = report['requests'][0]
        self.assertEqual(request['legacy_counts']['transaction_reused_source_products'], 0)
        self.assertEqual(request['legacy_compile_summaries'][0]['counts']['typecheck_modules'], 26)
        self.assertEqual(request['stages']['source_frontend']['status'], 'UNKNOWN')
        self.assertEqual(report['status'], 'incomplete')

    def test_workload_requires_real_inputs_sequences_and_two_work_controls(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources = []
            for name in ('A', 'B'):
                path = root / f'{name}.hs'
                path.write_text(name)
                sources.append({'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
            cases, rows = [], []
            scenarios = [('distinct', [0, 1]), ('repeat', [0, 0]), ('binding_growth', [0, 1]), ('aba', [0, 1, 0]), ('control', [0, 0, 0, 0])]
            ordinal = 0
            for scenario, inputs in scenarios:
                for step, source in enumerate(inputs):
                    ordinal += 1
                    stage = 'prepared_body' if scenario == 'control' and step >= 2 else 'source_frontend'
                    disabled = scenario == 'control' and step % 2 == 1
                    events = [packet('disabled', stage), packet('work', stage)] if disabled else [packet(stage=stage)]
                    selected = trace(completed(events, stage), ordinal=ordinal)
                    rows.extend(selected)
                    cases.append({'name': f'{scenario}{step}', 'scenario': scenario, 'step': step,
                                  'identity': selected[0]['span'], 'source': sources[source]})
            manifest = {'schema': 1, 'cases': cases, 'controls': [
                {'stage': 'source_frontend', 'normal': 'control0', 'disabled': 'control1'},
                {'stage': 'prepared_body', 'normal': 'control2', 'disabled': 'control3'}]}
            report = REPORT.analyze(rows)
            self.assertEqual(REPORT.analyze_workload(manifest, report)['status'], 'observed')
            growth_identities = [dict(cases[index]['identity']) for index in (4, 5)]
            cases[4]['identity'] = dict(cases[0]['identity'])
            cases[5]['identity'] = dict(cases[1]['identity'])
            result = REPORT.analyze_workload(manifest, report)
            self.assertTrue(any('reused across workload cases' in problem for problem in result['problems']))
            cases[4]['identity'], cases[5]['identity'] = growth_identities
            first, second = cases[7]['identity'], cases[8]['identity']
            cases[7]['identity'], cases[8]['identity'] = second, first
            result = REPORT.analyze_workload(manifest, report)
            self.assertTrue(any('authored step order differs' in problem for problem in result['problems']))
            cases[7]['identity'], cases[8]['identity'] = first, second
            manifest['cases'][8]['source'] = sources[1]
            self.assertEqual(REPORT.analyze_workload(manifest, report)['status'], 'incomplete')
            sources[0]['sha256'] = 'wrong'
            with self.assertRaisesRegex(ValueError, 'source reference missing or changed'):
                REPORT.analyze_workload(manifest, report)


if __name__ == '__main__':
    unittest.main()
