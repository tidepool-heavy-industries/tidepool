//! Acceptance coverage: the answerer `askUser`
//! operator-form round-trip (`plans/self-iterating-harness/09-askuser-form-gui.md`).
//! ONE `render` -> `loop` -> `runLLMTurn @Decision` -> (answerer suspends on
//! `askUser`, a scripted [`OperatorGate`] submits, the typed value resumes
//! and flows into `finalize`) -> `render` cycle, driven through the
//! production entry point (`SelfHarnessDriver::run_one_cycle`), against the
//! reference harness module (`examples/harness/Harness.hs`). Also asserts
//! the durable per-node log's `turn_start` record carries the EXTRACTED
//! executed Haskell (the `askUser`+`finalize` block), not a coarse
//! "model" provenance tag. Needs `TIDEPOOL_EXTRACT` and the with-packages
//! GHC on PATH — run inside `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::Arc;

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, FormSpec, Harness, LogObserver, OperatorGate,
    SelfHarnessDriver, Submission,
};

fn repo_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn examples_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-askuser".into(),
        extract_fingerprint: "acceptance-askuser".into(),
        harness_version: "test".into(),
    }
}

/// A scripted operator: always submits `{"f0": "High", "f1": 7}` (the
/// positional keys `Tidepool.Form` assigns an `enumField`/`intField` pair
/// built applicatively, `Form.hs`'s `keyName`) and never blocks on the
/// between-loops continue gate.
struct ScriptedGate;

impl OperatorGate for ScriptedGate {
    fn present_form(&self, _spec: &FormSpec) -> Submission {
        let mut sub = Submission::new();
        sub.insert("f0".to_string(), serde_json::Value::String("High".into()));
        sub.insert("f1".to_string(), serde_json::Value::Number(7.into()));
        sub
    }

    fn await_continue(&self) {}
}

/// The ONE recorded answerer reply: a fenced Haskell `do`-block that reads
/// BOTH an enum and an int from a single `askUser` form, then `finalize`s a
/// `Decision` built from them. `askUser` suspends AND resumes WITHIN this
/// one block execution (the form resume is not a new model turn), so one
/// reply covers the whole exchange. `Tidepool.Form` is auto-imported here
/// (answerer_decls includes `AskUser`), so the import list only needs the
/// reference harness's own types.
fn askuser_reply() -> RecordedReply {
    let content = "```haskell\n\
         import HarnessTypes (Decision (..), Confidence (..))\n\n\
         (do\n\
         \x20  (conf, n) <- askUser ((,) <$> enumField \"Confidence\" [(\"Low\", Low), (\"High\", High)] <*> intField \"Count\")\n\
         \x20  finalize @Decision (Decision { action = \"act-\" <> show n, rationale = \"from form\", confidence = conf })) :: M ()\n\
         ```";
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
        },
    }
}

/// ONE render -> loop -> runLLMTurn @Decision -> (askUser form round-trip)
/// -> finalize -> render cycle. Asserts a typed enum+int flows from a
/// scripted operator submission through `Tidepool.Form`'s decode into a
/// `finalize @Decision` reply, and that the durable per-node log records the
/// EXTRACTED executed Haskell for that turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn askuser_operator_form_round_trip_and_ws4_log() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![askuser_reply()]));

    let log_path =
        std::env::temp_dir().join(format!("acceptance-askuser-{}.jsonl", std::process::id()));
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_gate(Arc::new(ScriptedGate));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one full render->loop->runLLMTurn->askUser->finalize->render cycle");

    // The typed round-trip: the scripted submission's enum tag ("High")
    // decoded to `Confidence High`, its int (7) decoded to `Int`, and both
    // flowed into the `finalize @Decision` reply, which `loop` folded into
    // the returned `State`.
    let state = &outcome.state_json;
    assert_eq!(
        state.get("loopCount").and_then(|v| v.as_i64()),
        Some(1),
        "loopCount must advance across the loop boundary, got {state:?}"
    );
    let decision = state
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| panic!("lastDecision must be a Just Decision, got {state:?}"));
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("act-7"),
        "askUser's int field must flow through finalize into Decision.action, got {decision:?}"
    );
    assert_eq!(
        decision.get("confidence").and_then(|v| v.as_str()),
        Some("High"),
        "askUser's enum field must flow through finalize into Decision.confidence, got {decision:?}"
    );

    // The post-loop render reflects the NEW state — the loop reached
    // the next render with the form-derived decision folded in.
    assert!(
        !outcome.prompt_after.trim().is_empty(),
        "post-loop render must be non-empty"
    );
    assert!(
        outcome.prompt_after.contains("act-7"),
        "post-loop render should show the new decision's action, got:\n{}",
        outcome.prompt_after
    );
    assert!(
        outcome.prompt_after.contains("High"),
        "post-loop render should show the new decision's confidence, got:\n{}",
        outcome.prompt_after
    );

    // The durable per-node log's `turn_start` record carries the
    // EXTRACTED executed Haskell — `tail -f log.jsonl | jq -r
    // 'select(.ev=="turn_start").source'` shows the askUser+
    // finalize block this turn ran, not a coarse "model" provenance tag.
    // (No `Event::Effect` assertion: the scoped `[AskUser, Finalize]`
    // answerer stack has no handled effects by construction — see
    // tidepool-harness/CLAUDE.md's Replay section.)
    let log_contents = std::fs::read_to_string(&log_path).expect("log file readable");
    let found_turn_start_with_source = log_contents.lines().any(|line| {
        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let Some(event) = record.get("event") else {
            return false;
        };
        if event.get("ev").and_then(|v| v.as_str()) != Some("turn_start") {
            return false;
        }
        let Some(source) = event.get("source").and_then(|v| v.as_str()) else {
            return false;
        };
        source.contains("askUser") && source.contains("finalize")
    });
    assert!(
        found_turn_start_with_source,
        "WS4: expected a turn_start log record whose source contains both \
         askUser and finalize (the executed Haskell), log:\n{log_contents}"
    );
}
