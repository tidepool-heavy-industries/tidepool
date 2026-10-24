//! Attribute one hosted cell's submissions and terminal to its issued operation.
//!
//! Host timestamps bound host events only. The independent daemon census proves
//! request completion; its timestamps do not share the host clock or prove END.

use serde_json::Value;
use std::collections::BTreeMap;

const DISPATCH_MESSAGE: &str = "workbench cell dispatched to its actor";
const TERMINAL_MESSAGE: &str = "call timing";
const SUBMISSION_MESSAGE: &str = "compiler request identified";

/// A physical attempt remains distinct even when it compiles an identical request.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PhysicalRequestKey {
    daemon_epoch: [u8; 32],
    admission: u64,
    ordinal: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LogicalCompileRequest([u8; 8]);

pub(super) type CompilerSubmissions = BTreeMap<PhysicalRequestKey, LogicalCompileRequest>;

fn hex_identity<const N: usize>(value: &Value, label: &str) -> [u8; N] {
    let value = value.as_str().expect(label);
    assert!(
        value.len() == N * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "attribution UNKNOWN: malformed {label}"
    );
    std::array::from_fn(|index| {
        u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).expect("validated hex pair")
    })
}

fn request_identity(fields: &Value) -> (PhysicalRequestKey, LogicalCompileRequest) {
    let admission = fields["admission_id"]
        .as_u64()
        .expect("attribution UNKNOWN: admission identity");
    let ordinal = fields["request_ordinal"]
        .as_u64()
        .expect("attribution UNKNOWN: physical request ordinal");
    assert!(
        admission > 0 && ordinal > 0,
        "attribution UNKNOWN: invalid physical identity"
    );
    (
        PhysicalRequestKey {
            daemon_epoch: hex_identity(&fields["daemon_epoch"], "daemon epoch"),
            admission,
            ordinal,
        },
        LogicalCompileRequest(hex_identity(
            &fields["compile_request"],
            "logical request correlation",
        )),
    )
}

pub(super) fn physical_key(fields: &Value) -> PhysicalRequestKey {
    request_identity(fields).0
}

pub(super) fn same_request(fields: &Value, request: &Value) -> bool {
    [
        "daemon_epoch",
        "admission_id",
        "request_ordinal",
        "compile_request",
    ]
    .iter()
    .all(|key| !fields[key].is_null())
        && request_identity(fields) == request_identity(request)
}

pub(super) fn compiler_submissions<'a>(
    rows: impl IntoIterator<Item = &'a Value>,
) -> CompilerSubmissions {
    let mut submissions = BTreeMap::new();
    for fields in rows {
        let (physical, logical) = request_identity(fields);
        assert!(
            submissions.insert(physical, logical).is_none(),
            "attribution UNKNOWN: duplicate physical submission"
        );
    }
    submissions
}

pub(super) struct CommittedCellOperation {
    pub(super) execution: String,
    submissions: CompilerSubmissions,
}

/// Require this operation's own committed terminal and nonempty submissions.
/// A refusal before submission has a different contract and does not enter here.
pub(super) fn assert_committed_cell_operation(
    lines: &[Value],
    request_id: &str,
    call_id: &str,
) -> CommittedCellOperation {
    let (dispatch, execution) = dispatched_operation(lines, request_id, call_id);
    let execution = execution.to_owned();
    let terminal = call_terminal(lines, &execution, "cell");
    assert_committed_terminal(terminal);
    let submitted = lines
        .iter()
        .filter(|line| {
            log_message(line) == Some(SUBMISSION_MESSAGE)
                && line_carries_execution(line, &execution)
        })
        .collect::<Vec<_>>();
    assert!(
        !submitted.is_empty(),
        "attribution UNKNOWN: committed cell has no submissions"
    );
    for line in &submitted {
        assert_eq!(line["target"], "tidepool_extract_cmd::endpoint");
        assert_eq!(line["fields"]["execution_layer"], "endpoint_submission");
        assert_eq!(line["fields"]["transport"], "daemon");
        assert!(
            log_timestamp(dispatch) <= log_timestamp(line)
                && log_timestamp(line) <= log_timestamp(terminal),
            "host submission lies outside its dispatch/terminal window: {line}"
        );
    }
    CommittedCellOperation {
        execution,
        submissions: compiler_submissions(submitted.into_iter().map(|line| &line["fields"])),
    }
}

/// Compare exact attempt-to-request associations. Companion work remains in the
/// independently checked daemon census, but cannot satisfy this cell's evidence.
pub(super) fn assert_operation_submissions_match(
    operation: &CommittedCellOperation,
    host: &[Value],
    daemon_work: &[Value],
) {
    assert_eq!(
        operation.submissions,
        compiler_submissions(host),
        "host submissions differ from the committed operation"
    );
    assert_eq!(
        operation.submissions,
        compiler_submissions(
            daemon_work
                .iter()
                .filter(|row| row["attribution"] == "operation")
                .map(|row| &row["physical_request"])
        ),
        "daemon operation submissions differ from the committed operation"
    );
}

fn log_message(line: &Value) -> Option<&str> {
    line["fields"]["message"].as_str()
}

/// Tracing's default JSON timer is fixed-format RFC 3339 UTC, so lexical order
/// is chronological within one subscriber.
fn log_timestamp(line: &Value) -> &str {
    line["timestamp"]
        .as_str()
        .expect("tracing JSON line carries its occurrence timestamp")
}

pub(super) fn line_carries_execution(line: &Value, execution: &str) -> bool {
    line["fields"]["execution"].as_str() == Some(execution)
        || line["span"]["execution"].as_str() == Some(execution)
        || line["spans"].as_array().is_some_and(|spans| {
            spans
                .iter()
                .any(|span| span["execution"].as_str() == Some(execution))
        })
}

/// Select one exact originating request/call pair. A companion request may use
/// the same call spelling; its dispatch is independent evidence.
fn dispatched_operation<'a>(
    lines: &'a [Value],
    request_id: &str,
    call_id: &str,
) -> (&'a Value, &'a str) {
    let dispatches: Vec<_> = lines
        .iter()
        .filter(|line| {
            log_message(line) == Some(DISPATCH_MESSAGE)
                && line["fields"]["turn_id"].as_str() == Some(request_id)
                && line["fields"]["call_id"].as_str() == Some(call_id)
        })
        .collect();
    assert_eq!(
        dispatches.len(),
        1,
        "expected one dispatch row for request {request_id} call {call_id}: {dispatches:#?}"
    );
    let dispatch = dispatches[0];
    assert_eq!(
        dispatch["fields"]["context_call_id"].as_str(),
        Some(call_id),
        "dispatch row does not retain the originating call: {dispatch}"
    );
    let execution = dispatch["fields"]["execution"]
        .as_str()
        .expect("dispatch row names its execution");
    (dispatch, execution)
}

pub(super) fn dispatch_execution(lines: &[Value], request_id: &str, call_id: &str) -> String {
    dispatched_operation(lines, request_id, call_id)
        .1
        .to_owned()
}

/// Select the latest `call timing` terminal for this execution and tool.
///
/// The terminal is chosen by execution, not by global recency: another
/// operation's terminal may be written later. Duplicate rows for the same
/// execution are tolerated only by taking the latest; this helper does not
/// assert a process-global event count.
fn call_terminal<'a>(lines: &'a [Value], execution: &str, tool: &str) -> &'a Value {
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
fn assert_committed_terminal(terminal: &Value) -> u64 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn timestamp(tick: u32) -> String {
        format!("2026-10-24T00:00:00.{tick:09}Z")
    }

    fn request(ordinal: u64) -> Value {
        json!({"daemon_epoch":"a".repeat(64), "admission_id":1,
            "request_ordinal":ordinal, "compile_request":"b".repeat(16)})
    }

    fn correlated_log() -> Vec<Value> {
        let mut submitted = request(1);
        submitted.as_object_mut().unwrap().extend(
            json!({
                "message":SUBMISSION_MESSAGE, "execution_layer":"endpoint_submission",
                "transport":"daemon", "execution":"exec-1"
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        vec![
            json!({"timestamp":timestamp(1), "fields":{
                "message":DISPATCH_MESSAGE, "call_id":"call-1", "turn_id":"request-1",
                "context_call_id":"call-1", "execution":"exec-1"}}),
            json!({"timestamp":timestamp(2), "target":"tidepool_extract_cmd::endpoint",
                "fields":submitted}),
            json!({"timestamp":timestamp(3), "fields":{
                "message":TERMINAL_MESSAGE, "execution":"exec-1", "tool":"cell",
                "outcome":"Committed", "compile_count":1}}),
        ]
    }

    fn operation(lines: &[Value]) -> CommittedCellOperation {
        assert_committed_cell_operation(lines, "request-1", "call-1")
    }

    fn refuses(expected: &str, action: impl FnOnce()) {
        let panic = catch_unwind(AssertUnwindSafe(action)).expect_err(expected);
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains(expected),
            "expected {expected:?}, got {message:?}"
        );
    }

    fn daemon_row(physical: Value, attribution: &str) -> Value {
        json!({"physical_request":physical, "attribution":attribution})
    }

    #[test]
    fn committed_cell_joins_exact_submissions_and_preserves_companion_work() {
        let mut lines = correlated_log();
        let mut companion = lines[1].clone();
        companion["fields"]["execution"] = json!("exec-other");
        companion["fields"]["request_ordinal"] = json!(2);
        lines.push(companion);
        let mut companion_dispatch = lines[0].clone();
        companion_dispatch["fields"]["turn_id"] = json!("request-other");
        companion_dispatch["fields"]["execution"] = json!("exec-other");
        lines.push(companion_dispatch);
        let cell = operation(&lines);
        assert_eq!(cell.execution, "exec-1");
        let work = [
            daemon_row(request(1), "operation"),
            daemon_row(request(2), "explicit_unrelated"),
        ];
        assert_operation_submissions_match(&cell, &[request(1)], &work);
    }

    #[test]
    fn committed_cell_refuses_missing_or_foreign_operation_evidence() {
        let original = correlated_log();
        for (removed, expected) in [
            (0, "expected one dispatch row"),
            (1, "committed cell has no submissions"),
            (2, "no call timing terminal"),
        ] {
            let mut lines = original.clone();
            lines.remove(removed);
            refuses(expected, || {
                operation(&lines);
            });
        }
        let mut duplicate = original.clone();
        duplicate.push(original[0].clone());
        refuses("expected one dispatch row", || {
            operation(&duplicate);
        });
        for (index, field, value, expected) in [
            (0, "turn_id", json!("foreign"), "expected one dispatch row"),
            (
                0,
                "context_call_id",
                json!("foreign"),
                "does not retain the originating call",
            ),
            (
                2,
                "execution",
                json!("exec-other"),
                "no call timing terminal",
            ),
            (
                2,
                "compile_count",
                json!(0),
                "terminal reports no compile work",
            ),
            (
                2,
                "outcome",
                json!("Cancelled"),
                "terminal does not report the committed outcome",
            ),
        ] {
            let mut lines = original.clone();
            lines[index]["fields"][field] = value;
            refuses(expected, || {
                operation(&lines);
            });
        }
    }

    #[test]
    fn execution_correlation_accepts_each_production_field_location() {
        for location in ["fields", "span", "spans"] {
            let mut lines = correlated_log();
            lines[1]["fields"]
                .as_object_mut()
                .unwrap()
                .remove("execution");
            match location {
                "spans" => lines[1]["spans"] = json!([{"name":"cell", "execution":"exec-1"}]),
                location => lines[1][location]["execution"] = json!("exec-1"),
            }
            let cell = operation(&lines);
            assert_operation_submissions_match(
                &cell,
                &[request(1)],
                &[daemon_row(request(1), "operation")],
            );
        }
    }

    #[test]
    fn physical_submission_parser_refuses_malformed_and_duplicate_attempts() {
        for (field, value, expected) in [
            (
                "daemon_epoch",
                json!("a".repeat(63)),
                "malformed daemon epoch",
            ),
            (
                "daemon_epoch",
                json!("g".repeat(64)),
                "malformed daemon epoch",
            ),
            ("admission_id", json!(0), "invalid physical identity"),
            ("request_ordinal", json!(0), "invalid physical identity"),
            ("request_ordinal", json!(-1), "physical request ordinal"),
            (
                "compile_request",
                json!("b".repeat(15)),
                "malformed logical request correlation",
            ),
            (
                "compile_request",
                json!("z".repeat(16)),
                "malformed logical request correlation",
            ),
        ] {
            let mut malformed = request(1);
            malformed[field] = value;
            refuses(expected, || {
                compiler_submissions([&malformed]);
            });
        }
        let first = request(1);
        let mut conflicting = first.clone();
        conflicting["compile_request"] = json!("c".repeat(16));
        for duplicate in [&first, &conflicting] {
            refuses("duplicate physical submission", || {
                compiler_submissions([&first, duplicate]);
            });
        }
    }

    #[test]
    fn distinct_physical_attempts_can_share_a_logical_request() {
        let mut lines = correlated_log();
        let mut retry = lines[1].clone();
        retry["fields"]["request_ordinal"] = json!(2);
        lines.push(retry);
        let cell = operation(&lines);
        assert_eq!(cell.submissions.len(), 2);
        assert_eq!(
            cell.submissions.values().next(),
            cell.submissions.values().next_back()
        );
        assert_operation_submissions_match(
            &cell,
            &[request(1), request(2)],
            &[
                daemon_row(request(1), "operation"),
                daemon_row(request(2), "operation"),
            ],
        );
    }

    #[test]
    fn exact_association_checks_refuse_equal_sized_substitutions() {
        let cell = operation(&correlated_log());
        let valid = daemon_row(request(1), "operation");
        refuses("host submissions differ", || {
            assert_operation_submissions_match(&cell, &[request(2)], std::slice::from_ref(&valid));
        });
        let mut changed = request(1);
        changed["compile_request"] = json!("c".repeat(16));
        refuses("host submissions differ", || {
            assert_operation_submissions_match(
                &cell,
                std::slice::from_ref(&changed),
                std::slice::from_ref(&valid),
            );
        });
        for work in [
            daemon_row(request(2), "operation"),
            daemon_row(changed, "operation"),
            daemon_row(request(1), "explicit_unrelated"),
        ] {
            refuses("daemon operation submissions differ", || {
                assert_operation_submissions_match(&cell, &[request(1)], &[work]);
            });
        }
    }

    #[test]
    fn host_submissions_must_stay_in_their_dispatch_terminal_window() {
        for tick in [0, 4] {
            let mut lines = correlated_log();
            lines[1]["timestamp"] = json!(timestamp(tick));
            refuses("outside its dispatch/terminal window", || {
                operation(&lines);
            });
        }
        for tick in [1, 3] {
            let mut lines = correlated_log();
            lines[1]["timestamp"] = json!(timestamp(tick));
            operation(&lines);
        }
    }
}
