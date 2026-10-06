import importlib.util
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
               'disabled': 'cache_disabled', 'complete': 'stage_complete'}
    return {'schema': 1, 'cycle': 7, 'purpose': 'cell_program', 'observed_ns': 100,
            'stage': stage, 'decision': decision, 'reason': reason or reasons[decision],
            'unit': None if decision == 'complete' else 'main',
            'module': None if decision == 'complete' else 'Support',
            'version_kind': None if decision == 'complete' else 'source_fingerprint',
            'version': None if decision == 'complete' else 'abcdef', 'items': items, 'bytes': None}


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

    def test_event_after_terminal_is_refused(self):
        rows = trace(completed([packet()]))
        rows.append(rows.pop(1))
        self.assertTrue(any('outside request boundaries' in problem for problem in REPORT.analyze(rows)['problems']))

    def test_old_lowering_phase_without_interval_is_retained(self):
        rows = trace(completed([packet()]))
        rows.insert(1, {'fields': {'line': 'tidepool-timing phase=lowering ms=11470'}, 'span': rows[0]['span']})
        result = REPORT.analyze(rows)
        self.assertEqual(result['requests'][0]['phases_ms']['lowering'], [11470])
        self.assertIn('nonexclusive', result['interpretation'])

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
