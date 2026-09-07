//! Durable provider usage projection. Token-count notifications can repeat on
//! rate-limit updates; only response-identified records support aggregation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};

use serde_json::Value;
use tidepool_model::{
    ProviderFailure, ProviderObservation, ProviderTurnObservation, ProviderTurnState,
    ProviderUsageCompleteness, ProviderUsageObservation, ProviderUsageScope, ProviderUsageSnapshot,
    ProviderUsageSummary, TokenUsage,
};

/// Explicit offline selection; never inferred from provider-home or fork history.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSelection {
    pub threads: BTreeSet<String>,
    pub from_unix_ms: Option<i64>,
    pub until_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct UsageReadLimits {
    pub sources: usize,
    pub lines: usize,
    pub bytes_per_line: usize,
    pub responses: usize,
}
impl Default for UsageReadLimits {
    fn default() -> Self {
        Self {
            sources: 256,
            lines: 100_000,
            bytes_per_line: 1_048_576,
            responses: 10_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageProvenance {
    pub source: String,
    pub line: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BoundedUsageRecord {
    pub thread: String,
    pub turn: String,
    pub response: String,
    pub timestamp_unix_ms: i64,
    pub usage: TokenUsage,
    pub provenance: UsageProvenance,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageLimit {
    Sources,
    Lines,
    LineBytes,
    Responses,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum UsageIssue {
    SourceUnavailable,
    ReadFailure,
    InvalidJson,
    PartialLine,
    InvalidRecord,
    InvalidTimestamp,
    MissingThread,
    ConflictingResponse {
        response: String,
        first: UsageProvenance,
    },
    Limit(UsageLimit),
    MissingThreadCoverage {
        thread: String,
    },
    AggregateOverflow,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageDiagnostic {
    pub provenance: UsageProvenance,
    pub issue: UsageIssue,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSourceState {
    ReadThroughEof,
    LimitedOrInvalid,
    Unavailable,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSourceCoverage {
    pub source: String,
    pub state: UsageSourceState,
    pub ignored_cumulative_records: usize,
}

/// Counts only the recorded nonconflicting subset, never entire billing/history.
/// None aggregate means no usable response evidence or arithmetic overflow, not zero.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BoundedUsageReport {
    pub selection: UsageSelection,
    pub records: Vec<BoundedUsageRecord>,
    pub aggregate: Option<TokenUsage>,
    pub diagnostics: Vec<UsageDiagnostic>,
    pub sources: Vec<UsageSourceCoverage>,
}

#[derive(Default)]
struct Turn {
    records: Vec<ProviderUsageObservation>,
    expected_responses: BTreeSet<String>,
    last_usage: usize,
    completed: Option<usize>,
    incomplete: bool,
}

#[cfg(test)]
pub(super) fn read(
    reader: impl BufRead,
    thread: &str,
) -> io::Result<Option<ProviderUsageSnapshot>> {
    observe(reader, thread).map(|snapshot| snapshot.usage)
}

pub(super) fn observe(reader: impl BufRead, thread: &str) -> io::Result<ProviderObservation> {
    let mut observation = ProviderObservation::default();
    let mut started_turns = BTreeSet::new();
    let mut failed_turns = BTreeSet::new();
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
        if own_thread && kind == Some("turn_context") {
            observation.confirmed_model = payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            observation.confirmed_effort = payload
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if !own_thread || kind != Some("event_msg") {
            continue;
        }
        if let Some(turn) = payload.get("turn_id").and_then(Value::as_str) {
            let event = payload.get("type").and_then(Value::as_str);
            let state = match event {
                Some("task_started" | "turn_started") => Some(ProviderTurnState::Active),
                Some("task_complete" | "turn_complete") => Some(
                    if let Some(error) = payload.get("error").filter(|error| !error.is_null()) {
                        // Only structured protocol codes classify failures.
                        let code = error.get("codex_error_info").and_then(Value::as_str);
                        ProviderTurnState::Failed(match code {
                            Some("bad_request") => ProviderFailure::RequestRejected,
                            Some("response_stream_connection_failed") => {
                                ProviderFailure::TransportFailed
                            }
                            _ => ProviderFailure::Other(error.to_string()),
                        })
                    } else {
                        ProviderTurnState::Succeeded
                    },
                ),
                Some("turn_aborted") => Some(ProviderTurnState::Interrupted),
                _ => None,
            };
            if let Some(state) = state {
                if matches!(state, ProviderTurnState::Failed(_))
                    && failed_turns.insert(turn.to_owned())
                {
                    observation.failures.push(ProviderTurnObservation {
                        thread: thread.into(),
                        turn: turn.into(),
                        revision: index,
                        state: state.clone(),
                    });
                }
                let starts = matches!(state, ProviderTurnState::Active);
                let new_start = starts && started_turns.insert(turn.to_owned());
                if new_start
                    || (!starts
                        && observation.turn.as_ref().is_none_or(|current| {
                            current.turn == turn
                                && matches!(current.state, ProviderTurnState::Active)
                        }))
                {
                    observation.turn = Some(ProviderTurnObservation {
                        thread: thread.into(),
                        turn: turn.into(),
                        revision: index,
                        state,
                    });
                }
            }
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
                    let state = turns.entry(turn.into()).or_default();
                    state.completed = Some(index);
                    // A failed provider call can lack a usage record. Earlier
                    // successful responses cannot prove that missing usage was zero.
                    state.incomplete |= payload.get("error").is_some_and(|error| !error.is_null());
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
        observation.usage =
            legacy_first
                .zip(legacy_latest)
                .map(|(first, latest)| ProviderUsageSnapshot {
                    first,
                    latest,
                    thread_summary: None,
                    latest_turn_summary: None,
                });
        return Ok(observation);
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
    observation.usage = Some(ProviderUsageSnapshot {
        first: ordered[0].clone(),
        latest: ordered[ordered.len() - 1].clone(),
        thread_summary,
        latest_turn_summary,
    });
    Ok(observation)
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

    #[test]
    fn health_survives_missing_usage_and_ignores_older_completion() {
        let failure = json!({"type":"event_msg","payload":{
            "type":"task_complete","turn_id":"first",
            "error":{"codex_error_info":"other","message":"bad_request"}}});
        let mut values = vec![
            json!({"type":"session_meta","payload":{"id":"child"}}),
            event("task_started", "first"),
            failure.clone(),
        ];
        let project = |values: &[Value]| {
            observe(
                values
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .as_bytes(),
                "child",
            )
            .unwrap()
        };
        let failed = project(&values);
        assert!(failed.usage.is_none());
        let first_revision = failed.turn.as_ref().unwrap().revision;
        values.push(failure.clone());
        values.push(event("task_started", "first"));
        assert_eq!(project(&values).turn.unwrap().revision, first_revision);
        assert!(matches!(
            failed.turn.unwrap().state,
            ProviderTurnState::Failed(ProviderFailure::Other(_))
        ));
        values.push(event("task_started", "second"));
        values.push(failure);
        let active = project(&values).turn.unwrap();
        assert_eq!(active.turn, "second");
        assert_eq!(active.state, ProviderTurnState::Active);
        values.push(event("task_complete", "second"));
        let recovered = project(&values);
        assert_eq!(recovered.failures.len(), 1);
        assert_eq!(recovered.failures[0].turn, "first");
        assert_eq!(recovered.failures[0].revision, first_revision);
        assert_eq!(
            project(&values).turn.unwrap().state,
            ProviderTurnState::Succeeded
        );
    }

    #[test]
    fn confirmed_settings_require_own_durable_context() {
        let values = [
            json!({"type":"session_meta","payload":{"id":"parent"}}),
            json!({"type":"turn_context","payload":{"model":"parent-model","effort":"high"}}),
            json!({"type":"session_meta","payload":{"id":"child"}}),
        ];
        let source = values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let snapshot = observe(source.as_bytes(), "child").unwrap();
        assert_eq!(snapshot.confirmed_model, None);
        assert_eq!(snapshot.confirmed_effort, None);
        let source = format!(
            "{source}\n{}",
            json!({"type":"turn_context","payload":{
            "model":"gpt-5.6-sol","effort":"low"}})
        );
        let snapshot = observe(source.as_bytes(), "child").unwrap();
        assert_eq!(snapshot.confirmed_model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(snapshot.confirmed_effort.as_deref(), Some("low"));
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
    fn provider_error_keeps_prior_usage_partial_after_turn_completion() {
        let mut failed = event("task_complete", "one");
        failed["payload"]["error"] = json!({"message":"unsupported configuration_update"});
        let snapshot = read_values(&[
            own(),
            event("task_started", "one"),
            record("child", "one", "earlier"),
            failed,
        ]);
        for summary in [snapshot.thread_summary, snapshot.latest_turn_summary] {
            let summary = summary.unwrap();
            assert_eq!(summary.observations, 1);
            assert_eq!(summary.usage.input_tokens, 100);
            assert_eq!(summary.completeness, ProviderUsageCompleteness::Partial);
        }
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
