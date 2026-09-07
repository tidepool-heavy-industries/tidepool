use super::*;
use serde_json::json;
use std::io::Cursor;

fn selection() -> UsageSelection {
    UsageSelection {
        threads: BTreeSet::from(["child".into()]),
        from_unix_ms: Some(1000),
        until_unix_ms: Some(2000),
    }
}
fn row(thread: &str, response: &str, timestamp: &str, input: i64) -> Value {
    json!({"type":"token_usage_record","timestamp":timestamp,"payload":{
        "thread_id":thread,"turn_id":"turn","response_id":response,
        "usage":{"input_tokens":input,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":input}
    }})
}
fn input(rows: &[Value]) -> Cursor<Vec<u8>> {
    Cursor::new(
        rows.iter()
            .map(|value| format!("{value}\n"))
            .collect::<String>()
            .into_bytes(),
    )
}
fn read(rows: &[Value]) -> BoundedUsageReport {
    read_bounded_usage(
        [("fixture".into(), Ok(input(rows)))],
        selection(),
        UsageReadLimits::default(),
    )
    .unwrap()
}

#[test]
fn bounded_usage_window_is_timezone_aware_and_excludes_cumulative_parent_records() {
    let a = row("child", "a", "1970-01-01T00:00:01Z", 5);
    let result = read(&[
        row("parent", "parent", "1970-01-01T00:00:01Z", 9000),
        row("child", "before", "1970-01-01T00:00:00.999Z", 9000),
        a.clone(),
        row("child", "a", "1970-01-01T01:00:01+01:00", 5),
        row("child", "until", "1970-01-01T00:00:02Z", 9000),
        json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":999999}}}}),
        json!({"type":"response_item","payload":{"message":"PRIVATE PROMPT DO NOT EMIT"}}),
    ]);
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.aggregate.unwrap().input_tokens, 5);
    assert_eq!(result.sources[0].ignored_cumulative_records, 1);
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.records[0].provenance.line, 3);
    assert!(!serde_json::to_string(&result).unwrap().contains("PRIVATE"));
}

#[test]
fn bounded_usage_conflicts_across_sources_never_restore_a_winner() {
    let a = row("child", "a", "1970-01-01T00:00:01Z", 5);
    let changed = row("child", "a", "1970-01-01T00:00:01Z", 6);
    let result = read_bounded_usage(
        [
            ("first".into(), Ok(input(&[a.clone()]))),
            (
                "second".into(),
                Ok(input(&[
                    changed,
                    a,
                    row("child", "b", "1970-01-01T00:00:01Z", 7),
                ])),
            ),
        ],
        selection(),
        UsageReadLimits::default(),
    )
    .unwrap();
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.aggregate.unwrap().input_tokens, 7);
    assert!(result.diagnostics.iter().any(|d| matches!(&d.issue, UsageIssue::ConflictingResponse { response, first } if response == "a" && first.source == "first") && d.provenance.source == "second"));
    // Contradictory timestamps across a cutoff are still contradictory evidence.
    let result = read(&[
        row("child", "same", "1970-01-01T00:00:01Z", 5),
        row("child", "same", "1970-01-01T00:00:02Z", 5),
    ]);
    assert!(result.records.is_empty());
    assert!(result.aggregate.is_none());
}

struct Broken;
impl io::Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("fixture read failure"))
    }
}
impl BufRead for Broken {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Err(io::Error::other("fixture read failure"))
    }
    fn consume(&mut self, _: usize) {}
}

#[test]
fn bounded_usage_retains_partial_evidence_across_missing_malformed_and_failed_inputs() {
    let mut bytes = b"not json\n".to_vec();
    bytes.extend(
        input(&[
            row("child", "bad-time", "not a timestamp", 5),
            json!({"type":"token_usage_record","payload":{"usage":{}}}),
            row("child", "good", "1970-01-01T00:00:01Z", 9),
        ])
        .into_inner(),
    );
    bytes.extend(b"{\"type\":");
    type Source = (String, io::Result<Box<dyn BufRead>>);
    let sources: Vec<Source> = vec![
        (
            "missing".into(),
            Err(io::Error::new(io::ErrorKind::NotFound, "fixture")),
        ),
        ("broken".into(), Ok(Box::new(Broken))),
        ("partial".into(), Ok(Box::new(Cursor::new(bytes)))),
    ];
    let result = read_bounded_usage(sources, selection(), UsageReadLimits::default()).unwrap();
    assert_eq!(result.aggregate.unwrap().input_tokens, 9);
    for expected in [
        UsageIssue::SourceUnavailable,
        UsageIssue::ReadFailure,
        UsageIssue::InvalidJson,
        UsageIssue::InvalidTimestamp,
        UsageIssue::MissingThread,
        UsageIssue::PartialLine,
    ] {
        assert!(result
            .diagnostics
            .iter()
            .any(|d| std::mem::discriminant(&d.issue) == std::mem::discriminant(&expected)));
    }
    assert!(matches!(
        result.sources[0].state,
        UsageSourceState::Unavailable
    ));
    assert!(matches!(
        result.sources[2].state,
        UsageSourceState::LimitedOrInvalid
    ));
}

#[test]
fn bounded_usage_limits_report_omissions_instead_of_successful_zero() {
    let row = row("child", "a", "1970-01-01T00:00:01Z", 5);
    for limits in [
        UsageReadLimits {
            sources: 0,
            ..UsageReadLimits::default()
        },
        UsageReadLimits {
            lines: 0,
            ..UsageReadLimits::default()
        },
        UsageReadLimits {
            bytes_per_line: 4,
            ..UsageReadLimits::default()
        },
        UsageReadLimits {
            responses: 0,
            ..UsageReadLimits::default()
        },
    ] {
        let result = read_bounded_usage(
            [("fixture".into(), Ok(input(&[row.clone()])))],
            selection(),
            limits,
        )
        .unwrap();
        assert!(result.records.is_empty());
        assert!(result.aggregate.is_none());
        assert!(result
            .diagnostics
            .iter()
            .any(|d| matches!(d.issue, UsageIssue::Limit(_))));
    }
}

#[test]
fn bounded_usage_overflow_missing_and_observed_zero_remain_distinct() {
    let result = read(&[
        row("child", "a", "1970-01-01T00:00:01Z", i64::MAX),
        row("child", "b", "1970-01-01T00:00:01Z", 1),
    ]);
    assert!(result.aggregate.is_none());
    assert_eq!(result.records.len(), 2);
    assert!(result
        .diagnostics
        .iter()
        .any(|d| matches!(d.issue, UsageIssue::AggregateOverflow)));
    assert!(read(&[]).aggregate.is_none());
    assert_eq!(
        read(&[row("child", "zero", "1970-01-01T00:00:01Z", 0)])
            .aggregate
            .unwrap(),
        TokenUsage::default()
    );
    let cumulative = read(&[json!({"type":"event_msg","payload":{"type":"token_count"}})]);
    assert!(cumulative.aggregate.is_none());
}

#[test]
fn bounded_usage_validates_selection_and_shared_usage_invariants() {
    let mut invalid = selection();
    invalid.threads.clear();
    assert!(read_bounded_usage(
        [("fixture".into(), Ok(input(&[])))],
        invalid,
        UsageReadLimits::default()
    )
    .is_err());
    let mut invalid = selection();
    invalid.from_unix_ms = Some(3000);
    assert!(read_bounded_usage(
        [("fixture".into(), Ok(input(&[])))],
        invalid,
        UsageReadLimits::default()
    )
    .is_err());
    if usize::BITS == 64 {
        assert!(read_bounded_usage(
            [("fixture".into(), Ok(input(&[])))],
            selection(),
            UsageReadLimits {
                bytes_per_line: usize::MAX,
                ..UsageReadLimits::default()
            }
        )
        .is_err());
    }
    let mut invalid = row("child", "invalid", "1970-01-01T00:00:01Z", 5);
    invalid["payload"]["usage"]["reasoning_output_tokens"] = json!(1);
    assert!(parse_usage(&invalid["payload"]["usage"]).is_none());
    invalid["payload"]["usage"]["reasoning_output_tokens"] = json!(0);
    invalid["payload"]["usage"]["total_tokens"] = json!(6);
    assert!(parse_usage(&invalid["payload"]["usage"]).is_none());
}

#[test]
fn bounded_usage_response_ids_cannot_be_double_counted_across_bound_threads() {
    let mut selected = selection();
    selected.threads.insert("sibling".into());
    let result = read_bounded_usage(
        [(
            "fixture".into(),
            Ok(input(&[
                row("child", "same", "1970-01-01T00:00:01Z", 5),
                row("sibling", "same", "1970-01-01T00:00:01Z", 5),
            ])),
        )],
        selected,
        UsageReadLimits::default(),
    )
    .unwrap();
    assert!(result.aggregate.is_none());
    assert!(result.records.is_empty());
    assert!(result
        .diagnostics
        .iter()
        .any(|d| matches!(d.issue, UsageIssue::ConflictingResponse { .. })));
}
