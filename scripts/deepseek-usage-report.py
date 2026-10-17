#!/usr/bin/env python3
"""Estimate token-price ranges from retained usage, never infer an API bill.

Reads existing Responses usage/transport observations. Does not contact a provider.
"""
import argparse
from datetime import datetime, timezone
from decimal import Decimal
import json
from pathlib import Path


def counter(value):
    return value if type(value) is int and value >= 0 else None


def observed_usage(wire):
    if wire is None:
        return {key: None for key in ('input_tokens', 'output_tokens', 'cached_tokens',
                                     'cache_miss_tokens', 'reasoning_tokens')}
    version = wire.get('version', 1)
    if type(version) is not int or version not in (1, 2, 3):
        raise ValueError('unsupported usage observation version')
    result = {key: counter(wire.get(key)) for key in
              ('input_tokens', 'output_tokens', 'cached_tokens', 'cache_miss_tokens', 'reasoning_tokens')}
    if version < 3:
        for key in ('input_tokens', 'output_tokens'):
            if wire.get('reported') is not True and result[key] == 0:
                result[key] = None
        if version == 1 and result['cached_tokens'] == 0:
            result['cached_tokens'] = None
        result['cache_miss_tokens'] = result['reasoning_tokens'] = None
    return result


def timestamp(value):
    if type(value) is int and value >= 0:
        try:
            return datetime.fromtimestamp(value / 1000, timezone.utc)
        except (OverflowError, OSError, ValueError):
            return None
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace('Z', '+00:00'))
        return parsed.astimezone(timezone.utc) if parsed.tzinfo is not None else None
    except ValueError:
        return None


def price_periods(started_at, completed_at, public_holiday=None):
    """UTC schedule is known; the Chinese public-holiday calendar is not guessed."""
    start, end = timestamp(started_at), timestamp(completed_at)
    if start is None or end is None or end < start:
        return ['off_peak', 'peak'], ['billing_interval_unavailable']
    # No provider billing instant is assumed. Check every schedule boundary
    # crossed by the observed request interval, including UTC day boundaries.
    if (end - start).total_seconds() > 86400:
        return ['off_peak', 'peak'], ['billing_interval_crosses_days']
    from datetime import timedelta
    points = [start, end]
    day = start.replace(hour=0, minute=0, second=0, microsecond=0)
    for offset in (0, 1):
        for hour in (0, 1, 4, 6, 10):
            point = day + timedelta(days=offset, hours=hour)
            if start <= point <= end:
                points.append(point)
    periods, reasons = set(), set()
    for point in points:
        peak = point.weekday() < 5 and (1 <= point.hour < 4 or 6 <= point.hour < 10)
        if not peak or public_holiday is True:
            periods.add('off_peak')
        elif public_holiday is False:
            periods.add('peak')
        else:
            periods.update(('off_peak', 'peak'))
            reasons.add('chinese_public_holiday_unknown')
    return sorted(periods), sorted(reasons)


def estimate(usage, observation, config):
    if config.get('schema') != 1 or config.get('currency') != 'USD':
        raise ValueError('unsupported pricing configuration')
    facts = observed_usage(usage)
    result = {'kind': 'unknown', 'currency': 'USD', 'pricing_version': config['version'],
              'usage': facts, 'reasons': [], 'not_an_invoice': True, 'estimate_basis': 'snapshot_repricing',
              'pricing_verified_at': config['verified_at']}
    if type(observation.get('version')) is not int or observation.get('version') != 3:
        result['reasons'] = ['request_observation_version_unavailable_or_unsupported']
        return result
    if observation.get('backend') != 'deep_seek':
        result['reasons'] = ['backend_unavailable_or_not_deepseek']
        return result
    models = config['models']
    requested, returned = observation.get('requested_model'), observation.get('returned_model')
    if requested not in models or returned not in models or models[requested] != models[returned]:
        result['reasons'] = ['requested_returned_pricing_identity_unavailable_or_different']
        return result
    output = facts['output_tokens']
    total, hits, misses = (facts[key] for key in ('input_tokens', 'cached_tokens', 'cache_miss_tokens'))
    if output is None or total is None and (hits is None or misses is None):
        result['reasons'] = ['required_billable_counts_unavailable']
        return result
    if total is not None and ((hits is not None and hits > total)
            or (misses is not None and misses > total)
            or (hits is not None and misses is not None and hits + misses != total)):
        result['reasons'] = ['inconsistent_input_partition']
        return result
    if facts['reasoning_tokens'] is not None and facts['reasoning_tokens'] > output:
        result['reasons'] = ['reasoning_exceeds_output']
        return result
    if hits is not None and misses is not None:
        partitions = [(hits, misses)]
    elif hits is not None:
        partitions = [(hits, total - hits)]
    elif misses is not None:
        partitions = [(total - misses, misses)]
    else:
        partitions = [(total, 0), (0, total)]
        result['reasons'].append('cache_partition_unavailable')
    periods, reasons = price_periods(observation.get('started_at_unix_ms'), observation.get('completed_at_unix_ms'),
                                     observation.get('chinese_public_holiday'))
    result['reasons'].extend(reasons)
    rates = models[requested]
    costs = []
    for period in periods:
        price = rates[period]
        for hit, miss in partitions:
            # Reasoning is already included in output_tokens.
            costs.append((Decimal(hit) * Decimal(price['cache_hit'])
                          + Decimal(miss) * Decimal(price['cache_miss'])
                          + Decimal(output) * Decimal(price['output'])) / Decimal(1_000_000))
    result.update(kind='estimated_range', minimum_usd=str(min(costs)), maximum_usd=str(max(costs)),
                  periods=periods, model=requested)
    return result


def normalize_events(events):
    """Join only exact retained request IDs, never sequence or model guesses."""
    usages, observations, direct = [], {}, []
    for event in events:
        if not isinstance(event, dict):
            raise ValueError('usage report requires event objects')
        kind = event.get('kind', event.get('event'))
        payload = event.get('payload', {})
        if isinstance(payload, str):
            payload = json.loads(payload)
        request = event.get('request_id', event.get('request'))
        if kind == 'responses_transport':
            if not isinstance(payload, dict) or not isinstance(request, str) or not request:
                raise ValueError('transport observation lacks its retained request identity')
            if type(payload.get('version')) is not int or payload.get('version') != 3:
                raise ValueError('unsupported transport observation version')
            observations.setdefault(request, []).append(payload)
        elif kind == 'responses_usage':
            if not isinstance(payload, dict) or not isinstance(request, str) or not request:
                raise ValueError('usage event lacks its retained request identity')
            usages.append((request, payload))
        elif kind in ('turn_complete', 'transport_completed'):
            usage = event.get('usage')
            if not isinstance(usage, dict):
                raise ValueError('completed transport event lacks usage object')
            direct.append({'usage': usage, 'observation': event.get('observation') or {}})
    for request, usage in usages:
        matching = observations.get(request, [])
        observation = matching[0] if len(matching) == 1 else {}
        direct.append({'usage': usage, 'observation': observation,
                       'raw_transport_observations': matching,
                       'request_id': request,
                       'association': 'unique_request_owner' if len(matching) == 1 else
                                      'missing_or_ambiguous_transport_observation'})
    completed_requests = {request for request, _ in usages}
    for request, matching in observations.items():
        if request in completed_requests:
            continue
        # Failed/incomplete/cancelled transports can retain terminal usage even
        # when Engine correctly refuses to publish a completed responses_usage.
        # Preserve every observation; ambiguous ownership never acquires a price.
        for observation in matching:
            usage = observation.get('usage')
            if usage is not None and not isinstance(usage, dict):
                raise ValueError('transport usage must be an object or null')
            direct.append({'usage': usage, 'observation': observation if len(matching) == 1 else {},
                           'raw_transport_observation': observation, 'request_id': request,
                           'association': 'unique_transport_owner' if len(matching) == 1 else
                                          'ambiguous_transport_observation'})
    if events and not direct:
        raise ValueError('no supported retained usage events; refusing an empty usage report')
    return direct


def read_events(path, sqlite=False):
    if not sqlite:
        with path.open() as stream:
            return [json.loads(line) for line in stream if line.strip()]
    import sqlite3
    connection = sqlite3.connect('file:' + str(path.resolve()) + '?mode=ro', uri=True)
    try:
        return [{'request_id': request, 'kind': kind, 'payload': payload}
                for request, kind, payload in connection.execute(
                    "SELECT request_id,kind,payload FROM events WHERE kind IN ('responses_usage','responses_transport') ORDER BY id")]
    finally:
        connection.close()


def report(events, config):
    rows = []
    for event in events:
        usage = event['usage']
        observation = event.get('observation') or {}
        rows.append({'usage_observation': usage, 'request_observation': observation,
                     'raw_transport_observation': event.get('raw_transport_observation'),
                     'raw_transport_observations': event.get('raw_transport_observations'),
                     'request_id': event.get('request_id'), 'association': event.get('association'),
                     'cost': estimate(usage, observation, config)})
    return {'schema': 1, 'kind': 'provider_usage_estimates', 'pricing_version': config['version'],
            'responses': rows, 'observed_response_count': len(rows), 'network_requests': 0}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--events', type=Path)
    source.add_argument('--store', type=Path)
    parser.add_argument('--pricing', type=Path, required=True)
    args = parser.parse_args()
    config = json.loads(args.pricing.read_text())
    events = read_events(args.store or args.events, sqlite=args.store is not None)
    print(json.dumps(report(normalize_events(events), config), indent=2))


if __name__ == '__main__':
    main()
