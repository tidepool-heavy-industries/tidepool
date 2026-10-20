//! Operation-identity anchoring for resident service-route tests.
//!
//! Production telemetry is process-wide and interleaved across concurrent
//! operations. A service-route test must not accept a row because it carries a
//! convenient global property such as `compiled_off_checkout=true`: that row
//! may belong to another cell, another session, or a companion operation.
//!
//! These helpers bind every observation to one caller-chosen operation:
//!
//! 1. `(request_id, call_id)` selects exactly one dispatch row, which must
//!    retain the request as its turn and the call as its context call id;
//! 2. the dispatch row's `execution` selects the cell's own
//!    `begin_ready_block_split` snapshot/JIT phases;
//! 3. checkout admitted/released rows are correlated to those phase windows by
//!    session and timestamp;
//! 4. the same execution selects its own `call timing` terminal.
//!
//! [`assert_off_checkout_split`] never inspects `compiled_off_checkout`.
//! Off-checkout compilation is established by the absence of a checkout
//! admission between snapshot completion and JIT completion, plus a prepared
//! compile row for the same execution in that window -- not by a row that
//! claims it.
//!
//! This is a service-route oracle. It does not preserve the retired split
//! install route or its per-unit economics; callers own the contract they
//! assert around it.
//!
//! Consumed by Q10 service-route tests landing separately; compiled here so the
//! oracle can be reviewed before its callers.
#![allow(dead_code)]

use exomonad_tool::{ConversationOrigin, OriginalOperation, ToolArguments, ToolInvocation};
use serde_json::Value;
use std::collections::BTreeMap;

use super::ResidentToolEndpointTestExt;

const DISPATCH_MESSAGE: &str = "workbench cell dispatched to its actor";
const TERMINAL_MESSAGE: &str = "call timing";
const SPLIT_SPAN: &str = "begin_ready_block_split";
const CHECKOUT_TARGET: &str = "exomonad_actor::resident_workbench";
const ADMITTED_MESSAGE: &str = "resident machine checkout admitted";
const RELEASED_MESSAGE: &str = "resident machine checkout released";
const PREPARED_COMPILE_TARGET: &str = "tidepool_codegen::prepared_compile";

const SNAPSHOT_STARTED: &str = "native_item_snapshot_started";
const SNAPSHOT_COMPLETED: &str = "native_item_snapshot_completed";
const JIT_COMPLETED: &str = "native_item_jit_completed";
const SPLIT_PHASES: [&str; 3] = [SNAPSHOT_STARTED, SNAPSHOT_COMPLETED, JIT_COMPLETED];

/// Dispatch a script under a caller-chosen request/call identity so the captured
/// production log can be correlated to this exact operation.
pub(super) async fn dispatch_haskell_script_with_call_id(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    request_id: &str,
    call_id: &str,
    script: &str,
) -> Value {
    let (invocation, completion) = super::test_campaign::original_tool_call(OriginalOperation {
        origin: ConversationOrigin::External {
            thread_id: "operation-identity-oracle".into(),
        },
        request_id: request_id.into(),
        call_id: call_id.into(),
    });
    let result = endpoint
        .dispatch_json_boxed(ToolInvocation {
            context: Some(invocation),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(script.into()),
        })
        .await
        .unwrap_or_else(|error| panic!("Haskell script failed:\n{script}\n\n{error}"));
    endpoint
        .complete_boxed(completion)
        .await
        .expect("recorded tool completion");
    result
}

/// Dispatch a tool known to refuse before compiler submission, and prove the
/// refusal did not open a compiler transaction. The caller asserts the refusal
/// class; this helper owns the zero-submission property.
pub(super) async fn dispatch_refused_tool_without_submission(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    request_id: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
) -> exomonad_actor::ResidentToolError {
    let (invocation, completion) = super::test_campaign::original_tool_call(OriginalOperation {
        origin: ConversationOrigin::External {
            thread_id: "operation-identity-oracle".into(),
        },
        request_id: request_id.into(),
        call_id: call_id.into(),
    });
    let before = tidepool_extract_cmd::extract_spawn_count();
    let error = endpoint
        .dispatch_boxed(ToolInvocation {
            context: Some(invocation),
            name: name.into(),
            arguments: ToolArguments::Raw(arguments.into()),
        })
        .await
        .expect_err("known refusal must reject before admission");
    let after = tidepool_extract_cmd::extract_spawn_count();
    assert_eq!(
        after, before,
        "a refused dispatch must not submit a compiler transaction ({before} -> {after})"
    );
    endpoint
        .complete_boxed(completion)
        .await
        .expect("rejected operation settles without native work");
    error
}

pub(super) fn captured_json_lines(log_text: &str) -> Vec<Value> {
    log_text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

pub(super) fn log_message(line: &Value) -> Option<&str> {
    line["fields"]["message"].as_str()
}

/// Tracing's default JSON timer is fixed-format RFC 3339 UTC, so lexical order
/// is chronological within one subscriber.
pub(super) fn log_timestamp(line: &Value) -> &str {
    line["timestamp"]
        .as_str()
        .expect("tracing JSON line carries its occurrence timestamp")
}

pub(super) fn line_carries_execution(line: &Value, execution: &str) -> bool {
    line["fields"]["execution"].as_str() == Some(execution)
        || line["spans"].as_array().is_some_and(|spans| {
            spans
                .iter()
                .any(|span| span["execution"].as_str() == Some(execution))
        })
}

/// Select the single dispatch row for `call_id` and return its execution.
///
/// The row must also retain the originating request as its turn and the model
/// call as its context call id, so a coincidentally reused call id with a
/// different origin cannot satisfy the identity check.
pub(super) fn dispatch_execution(lines: &[Value], request_id: &str, call_id: &str) -> String {
    let dispatches: Vec<_> = lines
        .iter()
        .filter(|line| {
            log_message(line) == Some(DISPATCH_MESSAGE)
                && line["fields"]["call_id"].as_str() == Some(call_id)
        })
        .collect();
    assert_eq!(
        dispatches.len(),
        1,
        "expected one dispatch row for call {call_id}: {dispatches:#?}"
    );
    let dispatch = dispatches[0];
    assert_eq!(
        dispatch["fields"]["turn_id"].as_str(),
        Some(request_id),
        "dispatch row does not retain the originating turn: {dispatch}"
    );
    assert_eq!(
        dispatch["fields"]["context_call_id"].as_str(),
        Some(call_id),
        "dispatch row does not retain the originating call: {dispatch}"
    );
    dispatch["fields"]["execution"]
        .as_str()
        .expect("dispatch row names its execution")
        .to_owned()
}

/// Select the latest `call timing` terminal for this execution and tool.
///
/// The terminal is chosen by execution, not by global recency: another
/// operation's terminal may be written later. Duplicate rows for the same
/// execution are tolerated only by taking the latest; this helper does not
/// assert a process-global event count.
pub(super) fn call_terminal<'a>(lines: &'a [Value], execution: &str, tool: &str) -> &'a Value {
    let terminal = lines
        .iter()
        .filter(|line| {
            log_message(line) == Some(TERMINAL_MESSAGE)
                && line["fields"]["execution"].as_str() == Some(execution)
        })
        .max_by_key(|line| log_timestamp(line))
        .unwrap_or_else(|| panic!("no call timing terminal for execution {execution}"));
    assert_eq!(
        terminal["fields"]["tool"].as_str(),
        Some(tool),
        "terminal does not name the {tool} tool: {terminal}"
    );
    terminal
}

/// Assert a committed terminal and return its compile count.
pub(super) fn assert_committed_terminal(terminal: &Value) -> u64 {
    assert_eq!(
        terminal["fields"]["outcome"].as_str(),
        Some("Committed"),
        "terminal does not report the committed outcome: {terminal}"
    );
    let compile_count = terminal["fields"]["compile_count"].as_u64().unwrap_or(0);
    assert!(
        compile_count > 0,
        "terminal reports no compile work for the committed cell: {terminal}"
    );
    compile_count
}

/// Group this execution's `begin_ready_block_split` phases by block ordinal.
pub(super) fn split_items<'a>(
    lines: &'a [Value],
    execution: &str,
) -> BTreeMap<i64, Vec<&'a Value>> {
    let mut items: BTreeMap<i64, Vec<&Value>> = BTreeMap::new();
    for line in lines {
        if !SPLIT_PHASES
            .iter()
            .any(|phase| line["fields"]["phase"].as_str() == Some(phase))
        {
            continue;
        }
        if line["span"]["name"].as_str() != Some(SPLIT_SPAN) {
            continue;
        }
        if !line_carries_execution(line, execution) {
            continue;
        }
        let item = line["span"]["item"]
            .as_i64()
            .expect("split phases name their block ordinal");
        items.entry(item).or_default().push(line);
    }
    assert!(
        !items.is_empty(),
        "no begin_ready_block_split phases correlated to {execution}"
    );
    items
}

fn checkout_events(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| {
            line["target"].as_str() == Some(CHECKOUT_TARGET)
                && matches!(log_message(line), Some(ADMITTED_MESSAGE | RELEASED_MESSAGE))
        })
        .collect()
}

/// Assert every correlated block snapshotted under the session checkout, JIT
/// compiled after that checkout was released, and finished before this
/// execution's own terminal.
pub(super) fn assert_off_checkout_split(lines: &[Value], execution: &str) {
    let items = split_items(lines, execution);
    let checkout_events = checkout_events(lines);
    let prepared_compiles: Vec<_> = lines
        .iter()
        .filter(|line| {
            line["target"].as_str() == Some(PREPARED_COMPILE_TARGET)
                && line_carries_execution(line, execution)
        })
        .collect();
    let terminal = call_terminal(lines, execution, "cell");

    let mut last_jit = "";
    for (item, phases) in &items {
        let phase = |name: &str| -> &Value {
            let matching: Vec<_> = phases
                .iter()
                .copied()
                .filter(|line| line["fields"]["phase"].as_str() == Some(name))
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "item {item} of {execution} must report exactly one {name}: {phases:#?}"
            );
            matching[0]
        };
        let started = phase(SNAPSHOT_STARTED);
        let snapshotted = phase(SNAPSHOT_COMPLETED);
        let jitted = phase(JIT_COMPLETED);
        let started_at = log_timestamp(started);
        let snapshotted_at = log_timestamp(snapshotted);
        let jitted_at = log_timestamp(jitted);
        assert!(
            started_at < snapshotted_at && snapshotted_at < jitted_at,
            "item {item} phases are out of order: {started_at} < {snapshotted_at} < {jitted_at}"
        );
        last_jit = jitted_at;

        let admitted_during_snapshot: Vec<_> = checkout_events
            .iter()
            .copied()
            .filter(|line| {
                log_message(line) == Some(ADMITTED_MESSAGE)
                    && log_timestamp(line) > started_at
                    && log_timestamp(line) < snapshotted_at
            })
            .collect();
        assert_eq!(
            admitted_during_snapshot.len(),
            1,
            "item {item} did not take exactly one checkout for its snapshot: \
             {admitted_during_snapshot:#?}"
        );
        let session = admitted_during_snapshot[0]["fields"]["session"]
            .as_str()
            .expect("checkout admission names its session")
            .to_owned();
        let released_during_snapshot: Vec<_> = checkout_events
            .iter()
            .copied()
            .filter(|line| {
                log_message(line) == Some(RELEASED_MESSAGE)
                    && line["fields"]["session"].as_str() == Some(session.as_str())
                    && log_timestamp(line) > log_timestamp(admitted_during_snapshot[0])
                    && log_timestamp(line) < snapshotted_at
            })
            .collect();
        assert_eq!(
            released_during_snapshot.len(),
            1,
            "item {item}'s snapshot checkout was not released before completion: \
             {released_during_snapshot:#?}"
        );

        let admitted_during_jit: Vec<_> = checkout_events
            .iter()
            .copied()
            .filter(|line| {
                log_message(line) == Some(ADMITTED_MESSAGE)
                    && line["fields"]["session"].as_str() == Some(session.as_str())
                    && log_timestamp(line) > snapshotted_at
                    && log_timestamp(line) <= jitted_at
            })
            .collect();
        assert!(
            admitted_during_jit.is_empty(),
            "item {item} admitted its checkout during off-checkout compilation: \
             {admitted_during_jit:#?}"
        );
        let compiled_during_jit: Vec<_> = prepared_compiles
            .iter()
            .copied()
            .filter(|line| log_timestamp(line) > snapshotted_at && log_timestamp(line) < jitted_at)
            .collect();
        assert!(
            !compiled_during_jit.is_empty(),
            "item {item} produced no prepared compile between its snapshot completion \
             and JIT completion: {phases:#?}"
        );
    }
    assert!(
        log_timestamp(terminal) > last_jit,
        "terminal for {execution} precedes its last JIT completion"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn row(timestamp: &str, target: &str, fields: Value, span: Value, spans: Value) -> Value {
        serde_json::json!({
            "timestamp": timestamp,
            "target": target,
            "fields": fields,
            "span": span,
            "spans": spans,
        })
    }

    fn dispatch(at: &str, request: &str, call: &str, execution: &str) -> Value {
        row(
            at,
            "exomonad_actor::resident_workbench",
            serde_json::json!({
                "message": DISPATCH_MESSAGE,
                "call_id": call,
                "turn_id": request,
                "context_call_id": call,
                "execution": execution,
            }),
            Value::Null,
            serde_json::json!([]),
        )
    }

    fn phase(at: &str, execution: &str, item: i64, name: &str) -> Value {
        row(
            at,
            "exomonad_actor::resident_workbench",
            serde_json::json!({ "phase": name }),
            serde_json::json!({ "name": SPLIT_SPAN, "item": item }),
            serde_json::json!([{ "execution": execution }]),
        )
    }

    fn checkout(at: &str, message: &str, session: &str) -> Value {
        row(
            at,
            CHECKOUT_TARGET,
            serde_json::json!({ "message": message, "session": session }),
            Value::Null,
            serde_json::json!([]),
        )
    }

    fn compiled(at: &str, execution: &str) -> Value {
        row(
            at,
            PREPARED_COMPILE_TARGET,
            serde_json::json!({}),
            Value::Null,
            serde_json::json!([{ "execution": execution }]),
        )
    }

    fn terminal(at: &str, execution: &str, outcome: &str, compile_count: u64) -> Value {
        row(
            at,
            "exomonad_actor::call_timing",
            serde_json::json!({
                "message": TERMINAL_MESSAGE,
                "execution": execution,
                "tool": "cell",
                "outcome": outcome,
                "compile_count": compile_count,
            }),
            Value::Null,
            serde_json::json!([]),
        )
    }

    fn correlated_log() -> Vec<Value> {
        let execution = "exec-1";
        vec![
            dispatch(
                "2026-10-19T00:00:00.000001Z",
                "request-1",
                "call-1",
                execution,
            ),
            phase(
                "2026-10-19T00:00:00.000010Z",
                execution,
                0,
                SNAPSHOT_STARTED,
            ),
            checkout("2026-10-19T00:00:00.000020Z", ADMITTED_MESSAGE, "session-1"),
            checkout("2026-10-19T00:00:00.000030Z", RELEASED_MESSAGE, "session-1"),
            phase(
                "2026-10-19T00:00:00.000040Z",
                execution,
                0,
                SNAPSHOT_COMPLETED,
            ),
            compiled("2026-10-19T00:00:00.000050Z", execution),
            phase("2026-10-19T00:00:00.000060Z", execution, 0, JIT_COMPLETED),
            terminal("2026-10-19T00:00:00.000070Z", execution, "Committed", 2),
        ]
    }

    #[test]
    fn accepts_a_fully_correlated_off_checkout_item() {
        let lines = correlated_log();
        let execution = dispatch_execution(&lines, "request-1", "call-1");
        assert_eq!(execution, "exec-1");
        assert_off_checkout_split(&lines, &execution);
        assert_eq!(
            assert_committed_terminal(call_terminal(&lines, &execution, "cell")),
            2
        );
    }

    #[test]
    fn rejects_an_item_that_admits_its_checkout_during_compilation() {
        let mut lines = correlated_log();
        lines.insert(
            5,
            checkout("2026-10-19T00:00:00.000055Z", ADMITTED_MESSAGE, "session-1"),
        );
        let panic = catch_unwind(AssertUnwindSafe(|| {
            assert_off_checkout_split(&lines, "exec-1")
        }))
        .expect_err("checkout admitted during JIT must fail the oracle");
        let text = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            text.contains("admitted its checkout during off-checkout compilation"),
            "{text}"
        );
    }

    #[test]
    fn rejects_telemetry_from_an_unrelated_execution() {
        let execution = "exec-1";
        let lines = vec![
            dispatch(
                "2026-10-19T00:00:00.000001Z",
                "request-1",
                "call-1",
                execution,
            ),
            row(
                "2026-10-19T00:00:00.000050Z",
                "tidepool_runtime::prepared_install",
                serde_json::json!({ "compiled_off_checkout": true }),
                Value::Null,
                serde_json::json!([{ "execution": "exec-other" }]),
            ),
            terminal("2026-10-19T00:00:00.000070Z", "exec-other", "Committed", 1),
        ];
        let panic = catch_unwind(AssertUnwindSafe(|| {
            assert_off_checkout_split(&lines, execution)
        }))
        .expect_err("another execution's compiled_off_checkout row must not satisfy the oracle");
        let text = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            text.contains("no begin_ready_block_split phases correlated to exec-1"),
            "{text}"
        );
    }

    #[test]
    fn rejects_a_dispatch_row_that_does_not_retain_its_origin() {
        let mut lines = correlated_log();
        lines[0]["fields"]["turn_id"] = serde_json::json!("some-other-request");
        let panic = catch_unwind(AssertUnwindSafe(|| {
            dispatch_execution(&lines, "request-1", "call-1")
        }))
        .expect_err("a dispatch row with a different origin must not anchor the operation");
        let text = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            text.contains("does not retain the originating turn"),
            "{text}"
        );
    }
}
