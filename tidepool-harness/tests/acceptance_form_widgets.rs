//! Acceptance coverage for the `Tidepool.Form` widening (display elements,
//! multi-select, field prefill) — through the real production path, same
//! discipline as `acceptance_run_llm_turn.rs`'s `dialog_form_multi_field_...`
//! test. Record-replay, CI-shaped, zero live calls.
//!
//! One form composes ALL THREE new pieces:
//! - `prose` — display context between fields, consuming NO field index (so
//!   the `multiChoiceField` right after it still gets key `f0`).
//! - `multiChoiceField` — a checkbox subset, decoding `values.f0` (an ARRAY
//!   of selected option-keys) back to the typed `[Text]` the caller chose.
//! - `textField'` — a prefilled draft (`values.f1`'s widget renders with
//!   `initial` set); the operator's edited submission is what decodes.
//!
//! Checks BOTH halves of the round-trip: the RENDERED `Ui` (the encode
//! direction — prose/multi_choice/initial actually reach the wire) and the
//! DECODED typed value from a scripted `{values}` submission (the decode
//! direction).

use std::sync::Arc;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{Harness, HoleRouting};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-form-widgets".into(),
        extract_fingerprint: "acceptance-form-widgets".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 50,
        output_tokens: 10,
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: usage(),
    }
}

fn outcome_tag(o: &tidepool_harness::TurnOutcome) -> &'static str {
    match o {
        tidepool_harness::TurnOutcome::Completed { .. } => "Completed",
        tidepool_harness::TurnOutcome::Suspended { .. } => "Suspended",
        tidepool_harness::TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prose_multichoice_and_prefill_round_trip() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("form_widgets.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    // prose (no field index) *> multiChoiceField (f0) <*> textField' (f1, prefilled).
    let replies = vec![reply(
        "```haskell\ndo\n  \
         r <- dialogForm ((,) <$> (prose \"Pick the ones that apply\" *> \
         multiChoiceField \"Pick\" [(\"a\", \"Alpha\" :: Text), (\"b\", \"Beta\"), (\"c\", \"Gamma\")]) \
         <*> textField' \"Title\" \"draft title\")\n  \
         pure (toJSON (show r))\n```",
    )];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("form widgets root", "Fill the widened form.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("drives to hole");
    let ui = match outcome {
        tidepool_harness::TurnOutcome::Suspended { classified, .. } => match classified.routing {
            HoleRouting::Dialog { ui } => ui,
            other => panic!("expected a Dialog hole, got {other:?}"),
        },
        other => panic!(
            "root should suspend on the dialog hole, got {}",
            outcome_tag(&other)
        ),
    };

    // ---- ENCODE direction: the rendered Ui actually carries prose, the
    // multi_choice widget (keyed f0), and the prefilled f1 text_in.
    let body = ui["body"].as_array().expect("card body is an array");
    let has_prose = body
        .iter()
        .any(|w| w["ui"] == "prose" && w["text"] == "Pick the ones that apply");
    assert!(has_prose, "rendered Ui must carry the prose widget: {ui}");

    let multi = body
        .iter()
        .find(|w| w["ui"] == "multi_choice")
        .expect("rendered Ui must carry a multi_choice widget");
    assert_eq!(multi["key"], "f0");
    // Mirrors `choiceField`'s existing convention: the option-key doubles as
    // both the wire key and the rendered label ([(k, k) | (k, _) <- opts]).
    assert_eq!(
        multi["options"],
        json!([["a", "a"], ["b", "b"], ["c", "c"]])
    );

    let text = body
        .iter()
        .find(|w| w["ui"] == "text_in")
        .expect("rendered Ui must carry a text_in widget");
    assert_eq!(text["key"], "f1");
    assert_eq!(
        text["initial"], "draft title",
        "the prefill draft reaches the wire: {ui}"
    );

    // ---- DECODE direction: submit a subset (a, c) plus an EDITED title,
    // and check the typed value that comes back.
    harness
        .answer_dialog(
            root,
            json!({ "values": { "f0": ["a", "c"], "f1": "edited title" }, "prose": "" }),
        )
        .await
        .expect("form submission resumes the hole");
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    let (_header, events) = LogReader::open(&log_path).expect("open log");
    let rendered = events
        .filter_map(|r| r.ok())
        .find_map(|r| match r.event {
            Event::NodeDone {
                node,
                result_rendered,
            } if node == root => Some(result_rendered),
            _ => None,
        })
        .expect("root's NodeDone event is in the log");
    assert!(
        rendered.contains("Alpha") && rendered.contains("Gamma"),
        "decoded multi-choice keeps the checked subset, in order: {rendered}"
    );
    assert!(
        !rendered.contains("Beta"),
        "decoded multi-choice must drop the unchecked option: {rendered}"
    );
    assert!(
        rendered.contains("edited title"),
        "decoded text field carries the operator's EDITED value, not the draft: {rendered}"
    );
}
