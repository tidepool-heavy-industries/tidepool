use std::fs::OpenOptions;

use serde_json::json;

use super::*;
use crate::tree::{FanBadge, HoleId, NodeId, PriceClass, SiteId};

fn sample_header() -> LogHeader {
    LogHeader {
        prelude_hash: "prelude-abc123".to_string(),
        extract_fingerprint: "extract-def456".to_string(),
        harness_version: "0.1.0".to_string(),
    }
}

/// One instance of every `Event` variant, in a fixed order.
fn sample_events() -> Vec<Event> {
    vec![
        Event::NodeCreated {
            node: NodeId(1),
            parent: None,
            teaser: "root node".to_string(),
            effect_row: vec!["Llm".to_string(), "Fs".to_string()],
            fan: FanBadge::Bounded { max: 4 },
            price: PriceClass::Frontier,
        },
        Event::Forced {
            node: NodeId(1),
            actor: Actor::Operator,
        },
        Event::TurnStart {
            node: NodeId(1),
            source: "operator".to_string(),
            input: Some(json!({"prompt": "go"})),
        },
        Event::Effect {
            node: NodeId(1),
            seq: 0,
            tag: "Llm".to_string(),
            req: json!({"messages": ["hi"]}),
            resp: json!({"text": "hello"}),
        },
        Event::HolePublished {
            node: NodeId(1),
            hole: HoleId("scont_1".to_string()),
            site: Some(SiteId::try_from(7u64).unwrap()),
            ty: Some("Int".to_string()),
            prompt: "pick a number".to_string(),
            fork: false,
        },
        Event::HoleAnswerAttempt {
            node: NodeId(1),
            hole: HoleId("scont_1".to_string()),
            source: "operator".to_string(),
            outcome: AnswerOutcome::Rejected {
                error: "type mismatch".to_string(),
            },
        },
        Event::HoleAnswerAttempt {
            node: NodeId(1),
            hole: HoleId("scont_1".to_string()),
            source: "operator".to_string(),
            outcome: AnswerOutcome::Consumed,
        },
        Event::HoleConsumed {
            node: NodeId(1),
            hole: HoleId("scont_1".to_string()),
        },
        Event::NodeDone {
            node: NodeId(1),
            result_rendered: "42".to_string(),
        },
        Event::NodeCancelled {
            node: NodeId(2),
            reason: "operator cancel".to_string(),
        },
        Event::TurnDelta {
            node: NodeId(1),
            turn: 0,
            role: crate::provider::Role::User,
            content: "reconcile the verdicts".to_string(),
            usage: None,
            reasoning: None,
        },
        Event::TurnDelta {
            node: NodeId(1),
            turn: 1,
            role: crate::provider::Role::Developer,
            content: "child reviewer exited unexpectedly".to_string(),
            usage: None,
            reasoning: None,
        },
        Event::TurnDelta {
            node: NodeId(1),
            turn: 2,
            role: crate::provider::Role::Assistant,
            content: "```haskell\nresume Approve\n```".to_string(),
            usage: Some(crate::provider::Usage {
                input_tokens: 120,
                output_tokens: 8,
                cached_input_tokens: None,
                cache_write_tokens: None,
            }),
            reasoning: Some("checking both verdicts agree".to_string()),
        },
        Event::TurnForked {
            node: NodeId(2),
            parent: NodeId(1),
            parent_turn: 2,
        },
        Event::TurnSpliced {
            node: NodeId(1),
            turn: 3,
            role: crate::provider::Role::User,
            content: "operator: also check the edge case".to_string(),
        },
    ]
}

fn write_sample_log(path: &std::path::Path) -> LogHeader {
    let header = sample_header();
    let mut writer = LogWriter::create(path, &header).expect("create log");
    for (i, event) in sample_events().into_iter().enumerate() {
        let seq = writer.append(event).expect("append event");
        assert_eq!(
            seq, i as u64,
            "writer must assign monotonic seq starting at 0"
        );
    }
    header
}

/// Reasoning-continuity's wire-format guard: `Harness::drive_turn` builds a
/// turn's `Event::TurnDelta` from `driven.reply`/`usage`/`reasoning` only —
/// `driven.reasoning_items` is never read at that call site. Two turns whose
/// `DrivenTurn`s differ ONLY in `reasoning_items` must produce a
/// byte-identical durable-log line.
#[test]
fn turn_delta_log_line_is_byte_identical_regardless_of_reasoning_items() {
    use crate::engine::DrivenTurn;
    use crate::provider::{ReasoningItem, Usage};

    let to_event = |driven: &DrivenTurn| Event::TurnDelta {
        node: NodeId(1),
        turn: 1,
        role: crate::provider::Role::Assistant,
        content: driven.reply.clone(),
        usage: Some(driven.usage),
        reasoning: driven.reasoning.clone(),
    };

    let without = DrivenTurn {
        reply: "```haskell\nresume Approve\n```".to_string(),
        usage: Usage {
            input_tokens: 120,
            output_tokens: 8,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
        reasoning: Some("checking both verdicts agree".to_string()),
        reasoning_items: Vec::new(),
        blocks: Vec::new(),
    };
    let with_reasoning = DrivenTurn {
        reply: "```haskell\nresume Approve\n```".to_string(),
        usage: Usage {
            input_tokens: 120,
            output_tokens: 8,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
        reasoning: Some("checking both verdicts agree".to_string()),
        reasoning_items: vec![ReasoningItem(json!({
            "type": "reasoning",
            "id": "rs_1",
            "encrypted_content": "opaque-blob",
        }))],
        blocks: Vec::new(),
    };

    let dir = tempfile::tempdir().unwrap();
    let header = sample_header();

    let path_without = dir.path().join("without.jsonl");
    LogWriter::create(&path_without, &header)
        .unwrap()
        .append(to_event(&without))
        .unwrap();

    let path_with = dir.path().join("with.jsonl");
    LogWriter::create(&path_with, &header)
        .unwrap()
        .append(to_event(&with_reasoning))
        .unwrap();

    let bytes_without = std::fs::read(&path_without).unwrap();
    let bytes_with = std::fs::read(&path_with).unwrap();
    assert_eq!(
        bytes_without, bytes_with,
        "a turn's durable log line must not depend on its reasoning_items"
    );
    let text = String::from_utf8(bytes_without).unwrap();
    assert!(!text.contains("encrypted_content"));
    assert!(!text.contains("reasoning_items"));
}

#[test]
fn roundtrips_every_event_variant() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");
    let header = write_sample_log(&path);

    let (read_header, iter) = LogReader::open(&path).expect("open log");
    assert_eq!(read_header, header);

    let records: Vec<EventRecord> = iter.collect::<Result<_, _>>().expect("read all events");
    let expected = sample_events();
    assert_eq!(records.len(), expected.len());
    for (i, (record, event)) in records.into_iter().zip(expected).enumerate() {
        assert_eq!(record.seq, i as u64);
        assert_eq!(record.event, event);
    }
}

#[test]
fn seq_is_monotonic_and_total_per_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");
    write_sample_log(&path);

    let (_, iter) = LogReader::open(&path).expect("open log");
    let seqs: Vec<u64> = iter.map(|r| r.expect("event").seq).collect();
    let expected: Vec<u64> = (0..sample_events().len() as u64).collect();
    assert_eq!(seqs, expected);
}

#[test]
fn torn_final_line_reads_cleanly_to_last_whole_event() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");
    write_sample_log(&path);

    let full_len = std::fs::metadata(&path).unwrap().len();
    // Truncate partway through the final line's JSON body (not at a
    // newline boundary), simulating a crash mid-append.
    let torn_len = full_len - 5;
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(torn_len).unwrap();
    drop(file);

    let (_, iter) = LogReader::open(&path).expect("open log");
    let records: Vec<EventRecord> = iter.collect::<Result<_, _>>().expect("tolerate torn tail");
    let expected = sample_events();
    // Every whole event before the torn one is readable; the torn final
    // event is dropped, not surfaced as an error.
    assert_eq!(records.len(), expected.len() - 1);
    for (record, event) in records.iter().zip(expected.iter()) {
        assert_eq!(&record.event, event);
    }
}

/// V2 deliberately drops the retired snapshot/branch receipt schema. Older
/// logs are rejected through the typed version boundary, not a serde error.
#[test]
fn unstamped_legacy_log_is_rejected_below_floor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");

    let header = sample_header();
    let header_line = serde_json::to_string(&header).unwrap();
    let event = Event::NodeDone {
        node: NodeId(1),
        result_rendered: "42".to_string(),
    };
    let record_line = serde_json::to_string(&EventRecord { seq: 0, event }).unwrap();
    std::fs::write(&path, format!("{header_line}\n{record_line}\n")).unwrap();

    assert!(matches!(
        LogReader::open(&path),
        Err(ReadError::BelowFloor { found: 0, floor: 2 })
    ));
}

#[test]
fn version_one_log_is_rejected_below_floor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");
    let mut header = serde_json::to_value(sample_header()).unwrap();
    header["version"] = json!(1);
    std::fs::write(&path, format!("{header}\n")).unwrap();
    assert!(matches!(
        LogReader::open(&path),
        Err(ReadError::BelowFloor { found: 1, floor: 2 })
    ));
}

/// A version newer than this build supports is a loud, typed refusal — never
/// a silent best-effort read.
#[test]
fn future_version_header_is_a_typed_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");

    let header = sample_header();
    let mut header_value = serde_json::to_value(&header).unwrap();
    header_value
        .as_object_mut()
        .unwrap()
        .insert("version".to_string(), json!(9999));
    std::fs::write(&path, format!("{header_value}\n")).unwrap();

    let err = LogReader::open(&path).expect_err("a future version must be refused");
    assert!(
        matches!(err, ReadError::FutureVersion { found: 9999, .. }),
        "expected FutureVersion, got {err:?}"
    );
}

#[test]
fn corrupt_middle_line_is_a_read_error_not_silent_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");
    write_sample_log(&path);

    // Corrupt one JSON body in the middle of the file (not the last
    // line) while preserving line structure: same byte length, still
    // newline-terminated, but no longer valid JSON.
    let contents = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = contents.lines().map(|l| l.to_string()).collect();
    assert!(
        lines.len() > 3,
        "need at least a header + a few events to corrupt a middle one"
    );
    let target = 2; // header is lines[0]; corrupt an early event line.
    let corrupted = lines[target].replace('{', "#");
    assert_ne!(
        corrupted, lines[target],
        "corruption must actually change the line"
    );
    lines[target] = corrupted;
    let mut rewritten = lines.join("\n");
    rewritten.push('\n');
    std::fs::write(&path, rewritten).unwrap();

    // Folding onto `jsonl::read_tail` (which reads the whole file up front,
    // since a version stamp is a whole-file property) means a mid-file
    // corruption is now caught EAGERLY, at `open()` — not lazily, partway
    // through iteration, the way the old hand-rolled `BufReader` reader
    // surfaced it. Fail-fast-at-open is strictly the better contract; this
    // pins the new one.
    match LogReader::open(&path) {
        Err(ReadError::TornMidFile { .. }) => {}
        Err(other) => panic!("expected ReadError::TornMidFile, got {other:?}"),
        Ok(_) => panic!("corrupt middle record must not silently truncate the fold"),
    }
}
