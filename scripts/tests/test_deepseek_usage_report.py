import importlib.util
import json
from decimal import Decimal
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('deepseek_usage_report', ROOT / 'scripts/deepseek-usage-report.py')
report = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(report)


class DeepSeekUsageTests(unittest.TestCase):
    def setUp(self):
        self.config = json.loads((ROOT / 'build/pricing/deepseek-2026-10-17.json').read_text())
        self.observation = {'backend': 'deep_seek', 'requested_model': 'deepseek-flash',
                            'returned_model': 'deepseek-flash', 'started_at_unix_ms': 1792198800000,
                            'completed_at_unix_ms': 1792198805000}

    def test_optional_counters_legacy_migration_and_zero(self):
        for version in (1, 2):
            facts = report.observed_usage({'version': version, 'reported': False,
                                           'input_tokens': 12, 'output_tokens': 0, 'cached_tokens': 0})
            self.assertEqual(facts['input_tokens'], 12)
            self.assertIsNone(facts['output_tokens'])
            self.assertEqual(facts['cached_tokens'], 0 if version == 2 else None)
        for value in (None, 0, 12):
            facts = report.observed_usage({'version': 3, 'input_tokens': value, 'output_tokens': None})
            self.assertEqual(facts['input_tokens'], value)
            self.assertIsNone(facts['output_tokens'])
        with self.assertRaises(ValueError):
            report.observed_usage({'version': True})
        with self.assertRaises(ValueError):
            report.observed_usage({'version': 4})

    def test_reasoning_is_already_in_output_and_raw_unknowns_survive(self):
        usage = {'version': 3, 'input_tokens': 1_000_000, 'output_tokens': 1_000_000,
                 'cached_tokens': 1_000_000, 'reasoning_tokens': 800_000,
                 'raw': {'state': 'observed', 'value': {'future': None}}}
        result = report.report([{'usage': usage, 'observation': self.observation}], self.config)
        cost = result['responses'][0]['cost']
        self.assertEqual(Decimal(cost['minimum_usd']), Decimal('0.603'))
        self.assertEqual(Decimal(cost['maximum_usd']), Decimal('0.603'))
        self.assertEqual(result['responses'][0]['usage_observation'], usage)
        self.assertTrue(cost['not_an_invoice'])
        usage['reasoning_tokens'] = 1_000_001
        self.assertEqual(report.estimate(usage, self.observation, self.config)['kind'], 'unknown')

    def test_unknown_cache_and_holiday_produce_ranges(self):
        usage = {'version': 3, 'input_tokens': 1_000_000, 'output_tokens': 0}
        weekend = report.estimate(usage, self.observation, self.config)
        self.assertEqual(Decimal(weekend['minimum_usd']), Decimal('0.003'))
        self.assertEqual(Decimal(weekend['maximum_usd']), Decimal('0.15'))
        weekday = dict(self.observation, started_at_unix_ms=1792112400000,
                       completed_at_unix_ms=1792112405000)
        usage['cached_tokens'] = 0
        estimate = report.estimate(usage, weekday, self.config)
        self.assertEqual(Decimal(estimate['minimum_usd']), Decimal('0.15'))
        self.assertEqual(Decimal(estimate['maximum_usd']), Decimal('0.3'))
        self.assertIn('chinese_public_holiday_unknown', estimate['reasons'])
        known = report.estimate(usage, dict(weekday, chinese_public_holiday=False), self.config)
        self.assertEqual(Decimal(known['minimum_usd']), Decimal('0.3'))
        holiday = report.estimate(usage, dict(weekday, chinese_public_holiday=True), self.config)
        self.assertEqual(Decimal(holiday['maximum_usd']), Decimal('0.15'))

    def test_schedule_crossing_unknown_time_and_missing_counts(self):
        periods, _ = report.price_periods('2026-10-16T00:59:59Z', '2026-10-16T01:00:01Z', False)
        self.assertEqual(periods, ['off_peak', 'peak'])
        self.assertEqual(report.price_periods(None, None)[0], ['off_peak', 'peak'])
        for usage in ({'version': 3}, {'version': 3, 'input_tokens': 0},
                      {'version': 3, 'input_tokens': 7, 'output_tokens': None}):
            self.assertEqual(report.estimate(usage, self.observation, self.config)['kind'], 'unknown')
        zero = report.estimate({'version': 3, 'input_tokens': 0, 'output_tokens': 0}, self.observation, self.config)
        self.assertEqual(Decimal(zero['maximum_usd']), Decimal('0.0'))

    def test_invalid_partitions_backend_or_model_never_quote_a_bill(self):
        for usage in ({'version': 3, 'input_tokens': 1, 'cached_tokens': 2, 'output_tokens': 0},
                      {'version': 3, 'input_tokens': 3, 'cached_tokens': 1,
                       'cache_miss_tokens': 1, 'output_tokens': 0}):
            self.assertEqual(report.estimate(usage, self.observation, self.config)['kind'], 'unknown')
        usage = {'version': 3, 'input_tokens': 0, 'output_tokens': 0}
        for changed in ({'backend': 'other'}, {'returned_model': None}, {'returned_model': 'unknown'}):
            result = report.estimate(usage, dict(self.observation, **changed), self.config)
            self.assertEqual(result['kind'], 'unknown')
            self.assertNotIn('minimum_usd', result)

    def test_real_store_event_shape_joins_only_unique_request_owners(self):
        import sqlite3
        import tempfile
        usage = {'version': 3, 'input_tokens': 1_000_000, 'output_tokens': 0, 'cached_tokens': 0,
                 'raw': {'state': 'observed', 'value': {'future': None}}}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'store.sqlite'
            connection = sqlite3.connect(path)
            connection.execute('CREATE TABLE events(id INTEGER PRIMARY KEY,request_id TEXT,kind TEXT,payload TEXT)')
            for request, kind, payload in (
                    ('parent', 'responses_transport', self.observation),
                    ('parent', 'responses_usage', usage),
                    ('other', 'responses_usage', usage)):
                connection.execute('INSERT INTO events(request_id,kind,payload) VALUES(?,?,?)',
                                   (request, kind, json.dumps(payload)))
            connection.commit()
            connection.close()
            events = report.read_events(path, sqlite=True)
            normalized = report.normalize_events(events)
            results = report.report(normalized, self.config)['responses']
            self.assertEqual(results[0]['cost']['kind'], 'estimated_range')
            self.assertEqual(results[0]['usage_observation'], usage)
            self.assertEqual(results[1]['cost']['kind'], 'unknown')
            self.assertEqual(results[1]['association'], 'missing_or_ambiguous_transport_observation')
            events.append({'request_id': 'parent', 'kind': 'responses_transport',
                           'payload': json.dumps(self.observation)})
            ambiguous = report.report(report.normalize_events(events), self.config)['responses'][0]
            self.assertEqual(ambiguous['cost']['kind'], 'unknown')
            self.assertEqual(ambiguous['raw_transport_observations'],
                             [self.observation, self.observation])
        with self.assertRaisesRegex(ValueError, 'no supported'):
            report.normalize_events([{'kind': 'unrecognized'}])
        with self.assertRaisesRegex(ValueError, 'request identity'):
            report.normalize_events([{'kind': 'responses_usage', 'payload': usage}])

    def test_failed_or_incomplete_transport_usage_is_not_erased_by_missing_completion(self):
        for terminal in ('failed', 'incomplete'):
            observation = dict(self.observation, terminal=terminal,
                               usage={'version': 3, 'input_tokens': 0, 'output_tokens': 2,
                                      'raw': {'state': 'observed', 'value': None}},
                               queue_ms=0, first_text_ms=None, total_ms=5)
            rows = report.report(report.normalize_events([
                {'request_id': 'failed-request', 'kind': 'responses_transport',
                 'payload': json.dumps(observation)}]), self.config)['responses']
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]['usage_observation'], observation['usage'])
            self.assertEqual(rows[0]['request_observation'], observation)
            self.assertEqual(rows[0]['cost']['kind'], 'estimated_range')
            self.assertTrue(rows[0]['cost']['not_an_invoice'])
        observation = dict(self.observation, usage=None)
        row = report.report(report.normalize_events([
            {'request_id': 'before-terminal', 'kind': 'responses_transport',
             'payload': observation}]), self.config)['responses'][0]
        self.assertIsNone(row['usage_observation'])
        self.assertEqual(row['cost']['kind'], 'unknown')

    def test_ambiguous_transport_attempts_keep_all_raw_evidence_without_double_pricing(self):
        usage = {'version': 3, 'input_tokens': 0, 'output_tokens': 0}
        observations = [dict(self.observation, terminal=terminal, total_ms=elapsed,
                             usage=dict(usage, raw={'state': 'observed', 'value': raw}))
                        for terminal, elapsed, raw in [('failed', 1, None),
                                                       ('completed', 2, {'future': 0})]]
        events = [{'request_id': 'same', 'kind': 'responses_usage', 'payload': usage}]
        events.extend({'request_id': 'same', 'kind': 'responses_transport', 'payload': value}
                      for value in observations)
        rows = report.report(report.normalize_events(events), self.config)['responses']
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['usage_observation'], usage)
        self.assertEqual(rows[0]['raw_transport_observations'], observations)
        self.assertEqual(rows[0]['cost']['kind'], 'unknown')
        self.assertNotIn('minimum_usd', rows[0]['cost'])


if __name__ == '__main__':
    unittest.main()
