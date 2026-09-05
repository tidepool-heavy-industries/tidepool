//! Durable provider usage projection. Token-count notifications can repeat on
//! rate-limit updates; only response-identified records support aggregation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};

use serde_json::Value;
use tidepool_model::{
    ProviderUsageCompleteness, ProviderUsageObservation, ProviderUsageScope, ProviderUsageSnapshot,
    ProviderUsageSummary, TokenUsage,
};

#[derive(Default)]
struct Turn {
    records: Vec<ProviderUsageObservation>,
    expected_responses: BTreeSet<String>,
    last_usage: usize,
    completed: Option<usize>,
    incomplete: bool,
}

pub(super) fn read(
    reader: impl BufRead,
    thread: &str,
) -> io::Result<Option<ProviderUsageSnapshot>> {
    let mut own_thread = false;
    let mut legacy_first = None;
    let mut legacy_latest = None;
    let mut records = BTreeMap::<String, (String, ProviderUsageObservation)>::new();
    let mut ordered = Vec::new();
    let mut turns = BTreeMap::<String, Turn>::new();
    let mut current_turn = None::<String>;
    let mut incomplete = false;
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            // A torn tail may become readable on the next poll. It must not
            // make the currently visible prefix look complete.
            incomplete |= own_thread;
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str);
        let payload = &value["payload"];
        if kind == Some("session_meta") {
            own_thread = payload.get("id").and_then(Value::as_str) == Some(thread);
            continue;
        }
        if kind == Some("token_usage_record") {
            if payload.get("thread_id").and_then(Value::as_str).is_none() {
                incomplete |= own_thread;
            }
            if payload.get("thread_id").and_then(Value::as_str) != Some(thread) {
                continue;
            }
            let Some((response, turn, usage)) = record(payload) else {
                incomplete = true;
                continue;
            };
            let observation = ProviderUsageObservation {
                id: format!("{thread}:{response}"),
                timestamp: value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                usage,
            };
            if let Some((prior_turn, prior)) = records.get(response) {
                // Replayed records are idempotent; conflicting durable claims
                // invalidate completeness instead of changing already counted usage.
                incomplete |= prior_turn != turn || prior.usage != usage;
                continue;
            }
            records.insert(response.into(), (turn.into(), observation.clone()));
            ordered.push(observation.clone());
            let state = turns.entry(turn.into()).or_default();
            state.records.push(observation);
            state.last_usage = index;
            current_turn.get_or_insert_with(|| turn.into());
            continue;
        }
        if !own_thread || kind != Some("event_msg") {
            continue;
        }
        match payload.get("type").and_then(Value::as_str) {
            Some("task_started" | "turn_started") => {
                if let Some(turn) = payload.get("turn_id").and_then(Value::as_str) {
                    current_turn = Some(turn.into());
                    turns.entry(turn.into()).or_default().completed = None;
                } else {
                    incomplete = true;
                }
            }
            Some("task_complete" | "turn_complete") => {
                if let Some(turn) = payload.get("turn_id").and_then(Value::as_str) {
                    turns.entry(turn.into()).or_default().completed = Some(index);
                } else {
                    incomplete = true;
                }
            }
            Some("raw_response_completed") => {
                let Some(turn) = current_turn.as_ref() else {
                    incomplete = true;
                    continue;
                };
                let state = turns.entry(turn.clone()).or_default();
                state.last_usage = index;
                if let Some(response) = payload.get("response_id").and_then(Value::as_str) {
                    state.expected_responses.insert(response.into());
                } else {
                    state.incomplete = true;
                }
                state.incomplete |= payload.get("token_usage").and_then(parse_usage).is_none();
            }
            Some("token_count") => {
                if let Some(usage) = payload
                    .pointer("/info/last_token_usage")
                    .and_then(parse_usage)
                {
                    let observation = ProviderUsageObservation {
                        id: format!("{thread}:{index}"),
                        timestamp: value
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        usage,
                    };
                    legacy_first.get_or_insert_with(|| observation.clone());
                    legacy_latest = Some(observation);
                }
            }
            _ => {}
        }
    }
    if ordered.is_empty() {
        return Ok(legacy_first
            .zip(legacy_latest)
            .map(|(first, latest)| ProviderUsageSnapshot {
                first,
                latest,
                thread_summary: None,
                latest_turn_summary: None,
            }));
    }
    let turn_complete = |turn_id: &str, turn: &Turn| {
        !incomplete
            && !turn.incomplete
            && !turn.records.is_empty()
            && turn.completed.is_some_and(|end| end > turn.last_usage)
            && turn
                .expected_responses
                .iter()
                .all(|id| records.get(id).is_some_and(|(owner, _)| owner == turn_id))
    };
    let latest_turn_summary = current_turn.as_ref().and_then(|turn| {
        let state = turns.get(turn)?;
        summarize(
            ProviderUsageScope::Turn {
                thread: thread.into(),
                turn: turn.clone(),
            },
            &state.records,
            turn_complete(turn, state),
        )
    });
    let thread_summary = summarize(
        ProviderUsageScope::Thread(thread.into()),
        &ordered,
        turns.iter().all(|(id, turn)| turn_complete(id, turn)),
    );
    Ok(Some(ProviderUsageSnapshot {
        first: ordered[0].clone(),
        latest: ordered[ordered.len() - 1].clone(),
        thread_summary,
        latest_turn_summary,
    }))
}

fn summarize(
    scope: ProviderUsageScope,
    records: &[ProviderUsageObservation],
    complete: bool,
) -> Option<ProviderUsageSummary> {
    if records.is_empty() {
        return None;
    }
    let usage = records
        .iter()
        .try_fold(TokenUsage::default(), |sum, record| {
            let usage = record.usage;
            Some(TokenUsage {
                input_tokens: sum.input_tokens.checked_add(usage.input_tokens)?,
                cached_input_tokens: sum
                    .cached_input_tokens
                    .checked_add(usage.cached_input_tokens)?,
                output_tokens: sum.output_tokens.checked_add(usage.output_tokens)?,
                reasoning_output_tokens: sum
                    .reasoning_output_tokens
                    .checked_add(usage.reasoning_output_tokens)?,
                total_tokens: sum.total_tokens.checked_add(usage.total_tokens)?,
            })
        })?;
    Some(ProviderUsageSummary {
        scope,
        completeness: if complete {
            ProviderUsageCompleteness::Complete
        } else {
            ProviderUsageCompleteness::Partial
        },
        observations: i64::try_from(records.len()).ok()?,
        usage,
    })
}

fn record(value: &Value) -> Option<(&str, &str, TokenUsage)> {
    let response = value
        .get("response_id")?
        .as_str()
        .filter(|id| !id.is_empty())?;
    let turn = value.get("turn_id")?.as_str().filter(|id| !id.is_empty())?;
    Some((response, turn, parse_usage(value.get("usage")?)?))
}

pub(super) fn parse_usage(value: &Value) -> Option<TokenUsage> {
    let usage = TokenUsage {
        input_tokens: value.get("input_tokens")?.as_i64()?,
        cached_input_tokens: value.get("cached_input_tokens")?.as_i64()?,
        output_tokens: value.get("output_tokens")?.as_i64()?,
        reasoning_output_tokens: value.get("reasoning_output_tokens")?.as_i64()?,
        total_tokens: value.get("total_tokens")?.as_i64()?,
    };
    (usage.input_tokens >= 0
        && usage.cached_input_tokens >= 0
        && usage.cached_input_tokens <= usage.input_tokens
        && usage.output_tokens >= 0
        && usage.reasoning_output_tokens >= 0
        && usage.total_tokens >= 0)
        .then_some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage() -> Value {
        json!({"input_tokens":100,"cached_input_tokens":80,"output_tokens":7,
            "reasoning_output_tokens":3,"total_tokens":107})
    }

    fn record(thread: &str, turn: &str, response: &str) -> Value {
        json!({"type":"token_usage_record", "payload":{
            "thread_id":thread,"turn_id":turn,"response_id":response,"usage":usage()}})
    }

    fn event(kind: &str, turn: &str) -> Value {
        json!({"type":"event_msg", "payload":{"type":kind,"turn_id":turn}})
    }

    fn read_values(values: &[Value]) -> ProviderUsageSnapshot {
        let source = values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        read(source.as_bytes(), "child").unwrap().unwrap()
    }

    fn own() -> Value {
        json!({"type":"session_meta","payload":{"id":"child"}})
    }

    #[test]
    fn durable_records_deduplicate_responses_without_losing_equal_usage_or_parent_exclusion() {
        let a = record("child", "turn-1", "a");
        let lines = vec![
            own(),
            record("parent", "turn-0", "parent-a"),
            event("task_started", "turn-1"),
            a.clone(),
            a,
            record("child", "turn-1", "b"),
            event("task_complete", "turn-1"),
        ];
        let snapshot = read_values(&lines);
        let summary = snapshot.thread_summary.as_ref().unwrap();
        assert_eq!(summary.observations, 2);
        assert_eq!(summary.usage.input_tokens, 200);
        assert_eq!(summary.usage.cached_input_tokens, 160);
        assert_eq!(summary.completeness, ProviderUsageCompleteness::Complete);
        assert_eq!(snapshot.first.id, "child:a");
        assert_eq!(snapshot.latest.id, "child:b");
        assert_eq!(read_values(&lines), snapshot);
        let turn = snapshot.latest_turn_summary.unwrap();
        assert_eq!(
            turn.scope,
            ProviderUsageScope::Turn {
                thread: "child".into(),
                turn: "turn-1".into()
            }
        );
        assert_eq!(turn.observations, 2);
    }

    #[test]
    fn turn_boundaries_and_late_records_control_completeness_not_sampling_or_polling() {
        let mut lines = vec![
            own(),
            event("task_started", "one"),
            record("child", "one", "a"),
        ];
        assert_eq!(
            read_values(&lines).thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Partial
        );
        lines.push(event("task_complete", "one"));
        assert_eq!(
            read_values(&lines).thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Complete
        );
        lines.push(record("child", "one", "late"));
        assert_eq!(
            read_values(&lines).thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Partial
        );
        lines.push(event("task_complete", "one"));
        lines.push(event("task_started", "two"));
        let waiting = read_values(&lines);
        assert_eq!(
            waiting.thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Partial
        );
        assert_eq!(waiting.latest_turn_summary, None);
        lines.push(record("child", "two", "b"));
        let sampling = read_values(&lines);
        assert_eq!(sampling.thread_summary.unwrap().observations, 3);
        assert_eq!(sampling.latest_turn_summary.unwrap().observations, 1);
    }

    #[test]
    fn missing_response_usage_conflicts_and_torn_records_never_look_complete() {
        let base = vec![
            own(),
            event("task_started", "one"),
            record("child", "one", "a"),
        ];
        for bad in [
            json!({"type":"event_msg","payload":{"type":"raw_response_completed","response_id":"missing","token_usage":null}}),
            json!({"type":"token_usage_record","payload":{"thread_id":"child","response_id":"invalid"}}),
        ] {
            let mut lines = base.clone();
            lines.extend([bad, event("task_complete", "one")]);
            assert_eq!(
                read_values(&lines).thread_summary.unwrap().completeness,
                ProviderUsageCompleteness::Partial
            );
        }
        let mut conflict = record("child", "one", "a");
        conflict["payload"]["usage"]["input_tokens"] = json!(200);
        let mut lines = base;
        lines.extend([conflict, event("task_complete", "one")]);
        let snapshot = read_values(&lines);
        assert_eq!(snapshot.thread_summary.as_ref().unwrap().observations, 1);
        assert_eq!(
            snapshot.thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Partial
        );
        let source = format!(
            "{}\n{{\"type\":",
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(
            read(source.as_bytes(), "child")
                .unwrap()
                .unwrap()
                .thread_summary
                .unwrap()
                .completeness,
            ProviderUsageCompleteness::Partial
        );
    }

    #[test]
    fn aggregates_include_every_durable_record_after_delayed_poll() {
        let mut lines = vec![own(), event("task_started", "one")];
        lines.extend((0..80).map(|i| record("child", "one", &i.to_string())));
        lines.push(event("task_complete", "one"));
        let snapshot = read_values(&lines);
        assert_eq!(snapshot.thread_summary.as_ref().unwrap().observations, 80);
        assert_eq!(
            snapshot.thread_summary.unwrap().usage.cached_input_tokens,
            6400
        );
        assert_eq!(snapshot.first.id, "child:0");
        assert_eq!(snapshot.latest.id, "child:79");
    }

    #[test]
    fn absent_identified_records_leave_totals_unavailable_and_overflow_is_not_a_total() {
        assert_eq!(read("".as_bytes(), "child").unwrap(), None);
        let snapshot = read_values(&[
            own(),
            json!({"type":"event_msg","payload":{"type":"token_count",
            "info":{"last_token_usage":usage()}}}),
        ]);
        assert_eq!(snapshot.thread_summary, None);
        assert_eq!(snapshot.latest_turn_summary, None);
        let mut huge = record("child", "one", "a");
        huge["payload"]["usage"]["input_tokens"] = json!(i64::MAX);
        let mut huge2 = huge.clone();
        huge2["payload"]["response_id"] = json!("b");
        assert_eq!(read_values(&[own(), huge, huge2]).thread_summary, None);
    }

    #[test]
    fn completed_response_requires_a_matching_record_and_keeps_active_turn_scope() {
        let raw = json!({"type":"event_msg","payload":{"type":"raw_response_completed",
            "response_id":"a","token_usage":usage()}});
        let mut lines = vec![
            own(),
            event("task_started", "one"),
            record("child", "one", "earlier"),
            raw,
        ];
        lines.push(event("task_complete", "one"));
        assert_eq!(
            read_values(&lines).thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Partial
        );
        lines.push(record("child", "one", "a"));
        assert_eq!(
            read_values(&lines).thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Partial
        );
        lines.push(event("task_complete", "one"));
        assert_eq!(
            read_values(&lines).thread_summary.unwrap().completeness,
            ProviderUsageCompleteness::Complete
        );
        lines.extend([
            event("task_started", "two"),
            record("child", "two", "b"),
            record("child", "one", "late-one"),
        ]);
        assert_eq!(
            read_values(&lines).latest_turn_summary.unwrap().scope,
            ProviderUsageScope::Turn {
                thread: "child".into(),
                turn: "two".into()
            }
        );
    }
}
