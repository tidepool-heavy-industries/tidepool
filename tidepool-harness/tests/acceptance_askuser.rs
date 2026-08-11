//! Acceptance coverage: the answerer `askUser @T` operator-form round-trip
//! (`plans/self-iterating-harness/14-generic-derived-askuser-prd.md`, delivery
//! step 5).
//!
//! ONE `render` -> `loop` -> `runLLMTurn @Decision` -> (answerer suspends on
//! `askUser @Decision`, a scripted [`OperatorGate`] submits, the typed value
//! resumes and flows into `finalize`) -> `render` cycle, driven through the
//! production entry point (`SelfHarnessDriver::run_one_cycle`), against the
//! reference harness module (`examples/harness/Harness.hs`).
//!
//! Three things ride that one cycle:
//!
//! - the form the operator is presented is the shape DERIVED from `Decision`'s
//!   own `Generic` representation — asserted here structurally, which is what
//!   proves the Haskell encoder (`Tidepool.Form.Wire`) and the Rust wire
//!   (`selfharness::operator`) agree through the real extract/JIT;
//! - a MALFORMED submission re-presents the SAME form rather than surfacing an
//!   error — `askUser` re-prompts by recursion, no `Either` reaches the caller,
//!   and the driver's bounded servicing loop is untouched;
//! - `chooseMany` selects among runtime VALUES by their labels and returns the
//!   typed values.
//!
//! Also asserts the durable per-node log's `turn_start` record carries the
//! EXTRACTED executed Haskell, not a coarse "model" provenance tag. Needs
//! `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, VariantShape};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, OperatorGate, SelfHarnessDriver,
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

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-askuser".into(),
        extract_fingerprint: "acceptance-askuser".into(),
        harness_version: "test".into(),
    }
}

/// The form `askUser @Decision` must present: `Decision`'s structure, keyed by
/// its own selector and constructor names, in declaration order — a nested
/// product-of-sum, since `confidence :: Confidence` is an enum.
///
/// Written out rather than derived, because "the operator sees the type's own
/// structure" is exactly the claim under test.
fn expected_decision_shape() -> FormShape {
    FormShape::Product {
        type_key: "Decision".to_string(),
        constructor: "Decision".to_string(),
        fields: vec![
            FieldShape {
                key: "action".to_string(),
                shape: FormShape::String,
            },
            FieldShape {
                key: "rationale".to_string(),
                shape: FormShape::String,
            },
            FieldShape {
                key: "confidence".to_string(),
                shape: FormShape::Sum {
                    type_key: "Confidence".to_string(),
                    variants: vec![
                        VariantShape {
                            constructor: "Low".to_string(),
                            shape: empty_product("Confidence", "Low"),
                        },
                        VariantShape {
                            constructor: "Medium".to_string(),
                            shape: empty_product("Confidence", "Medium"),
                        },
                        VariantShape {
                            constructor: "High".to_string(),
                            shape: empty_product("Confidence", "High"),
                        },
                    ],
                },
            },
        ],
    }
}

fn empty_product(type_key: &str, constructor: &str) -> FormShape {
    FormShape::Product {
        type_key: type_key.to_string(),
        constructor: constructor.to_string(),
        fields: vec![],
    }
}

/// A `Decision` the operator built, as the PLAIN JSON the generic `FromJSON`
/// decode reads: a record is an object of its fields, and a nullary-sum
/// field (`confidence :: Confidence`) is the chosen constructor as a bare
/// string.
fn decision_answer() -> serde_json::Value {
    serde_json::json!({
        "action": "act-7",
        "rationale": "from form",
        "confidence": "High"
    })
}

/// A scripted operator that answers from the SHAPE it is handed rather than
/// from a hardcoded key list — which is the point: `askUser @T` sends the
/// derived structure, and a gate that can read it can fill it.
///
/// The FIRST presentation of the `Decision` form is answered with an empty
/// submission — a malformed answer, the thing a real operator produces by
/// submitting an incomplete form (and the thing the headless `StdinGate`
/// produces at EOF). `askUser` must re-present the same form rather than
/// failing the turn.
struct ScriptedGate {
    /// Every shape presented, in order — the receipt for "the SAME form came
    /// back" after a malformed submission.
    seen: Mutex<Vec<FormShape>>,
    decision_presentations: AtomicUsize,
}

impl ScriptedGate {
    fn new() -> Self {
        ScriptedGate {
            seen: Mutex::new(Vec::new()),
            decision_presentations: AtomicUsize::new(0),
        }
    }
}

impl OperatorGate for ScriptedGate {
    fn present_form(&self, shape: &FormShape) -> serde_json::Value {
        let shape = shape.clone();
        self.seen.lock().unwrap().push(shape.clone());

        match &shape {
            // `askUser @Decision`.
            FormShape::Product { type_key, .. } if type_key == "Decision" => {
                let nth = self.decision_presentations.fetch_add(1, Ordering::SeqCst);
                if nth == 0 {
                    // Malformed: not an answer at all. Re-prompt, don't fail.
                    serde_json::json!({})
                } else {
                    decision_answer()
                }
            }
            // `chooseMany` — one checkbox per offered label, submitted as a
            // plain JSON object of booleans. Keep the first.
            FormShape::Product {
                type_key, fields, ..
            } if type_key == "Choices" => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|f| (f.key.clone(), serde_json::Value::Bool(f.key == "keep")))
                    .collect(),
            ),
            other => panic!("unexpected form shape presented: {other:?}"),
        }
    }

    fn await_continue(&self) {}
}

/// The ONE recorded answerer reply: a fenced Haskell `do`-block that asks the
/// operator for a whole `Decision` with `askUser @Decision`, then picks among
/// two RUNTIME values with `chooseMany`, then `finalize`s. Both suspensions
/// happen WITHIN this one block execution (a form resume is not a new model
/// turn), so one reply covers the whole exchange.
///
/// Nothing is imported but the answer types themselves: no form builder, no
/// codec, no `Tidepool.Form` import (it is auto-imported whenever `AskUser` is
/// in the compiling row).
fn askuser_reply() -> RecordedReply {
    let content = "```haskell\n\
         import HarnessTypes (Decision (..), Confidence (..))\n\n\
         (do\n\
         \x20  d <- askUser @Decision\n\
         \x20  picks <- chooseMany [(\"keep\", \"kept\"), (\"drop\", \"dropped\")]\n\
         \x20  finalize @Decision (d { rationale = T.intercalate \"+\" picks })) :: M ()\n\
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

/// ONE render -> loop -> runLLMTurn @Decision -> (askUser @Decision form
/// round-trip, one malformed submission, one `chooseMany`) -> finalize ->
/// render cycle.
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
    let gate = Arc::new(ScriptedGate::new());
    driver.set_gate(gate.clone());
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one full render->loop->runLLMTurn->askUser->finalize->render cycle");

    // The form the operator saw IS the type's own structure, derived with no
    // `Decision` value in existence: exact selector keys, exact constructor
    // keys, declaration order, and the nested `Confidence` choice. This is the
    // Haskell encoder and the Rust wire agreeing through the real extract/JIT.
    let seen = gate.seen.lock().unwrap().clone();
    assert_eq!(
        seen.first(),
        Some(&expected_decision_shape()),
        "askUser @Decision must present Decision's own derived structure"
    );

    // A malformed submission re-presented the SAME form — `askUser` re-prompts
    // by recursion, so the driver saw a fresh `AskUser` suspension rather than
    // an error, and no `Either` ever reached the answerer's Haskell.
    assert_eq!(
        seen.get(1),
        Some(&expected_decision_shape()),
        "a malformed submission must re-present the SAME form, got: {seen:?}"
    );
    assert_eq!(
        gate.decision_presentations.load(Ordering::SeqCst),
        2,
        "exactly one re-prompt was scripted"
    );

    // `chooseMany`'s form is the third: one checkbox per RUNTIME label.
    assert_eq!(
        seen.get(2),
        Some(&FormShape::Product {
            type_key: "Choices".to_string(),
            constructor: "Choices".to_string(),
            fields: vec![
                FieldShape {
                    key: "keep".to_string(),
                    shape: FormShape::Bool,
                },
                FieldShape {
                    key: "drop".to_string(),
                    shape: FormShape::Bool,
                },
            ],
        }),
        "chooseMany must offer one control per runtime label, got: {seen:?}"
    );

    // The typed round-trip: the operator's structural submission decoded into a
    // real `Decision` (a nested ADT — `confidence` is a `Confidence`, not a
    // string), which flowed into `finalize @Decision` and then into `State`.
    let state = &outcome.state_json;
    assert_eq!(
        driver.iteration(),
        1,
        "the driver's iteration count must advance across the loop boundary"
    );
    let decision = state
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| panic!("lastDecision must be a Just Decision, got {state:?}"));
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("act-7"),
        "askUser's text field must flow through finalize into Decision.action, got {decision:?}"
    );
    assert_eq!(
        decision.get("confidence").and_then(|v| v.as_str()),
        Some("High"),
        "askUser's nested enum must flow through finalize into Decision.confidence, got {decision:?}"
    );
    // `chooseMany` returned the VALUE behind the chosen LABEL ("keep" -> "kept"),
    // and only the chosen one.
    assert_eq!(
        decision.get("rationale").and_then(|v| v.as_str()),
        Some("kept"),
        "chooseMany must return the typed values of the selected labels, got {decision:?}"
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

/// The author-contract claim, at COMPILE level: declare the example ADTs
/// with `deriving (Generic, FromJSON)` — the FromJSON being the vendored
/// generic DEFAULT, no method written — then ask for one. No codec, no form
/// builder, no instance of anything Tidepool-specific
/// (`tests/fixtures/PrdTypes.hs` is the fixture, and what it does NOT
/// contain is the assertion). One decode path: the same generic `FromJSON`
/// that reads every other external value reads the form answer.
///
/// `req.service :: Text` in the same block is what pins the SECOND half — the
/// binding has exactly the requested Haskell type, not a `Value` or a tuple.
/// A wrong type there is a GHC error, so this compiling IS the proof.
#[test]
fn prd_example_adts_compile_with_the_bare_derive_contract() {
    support::require_extract();
    let mut cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    cfg.include.push(fixtures_dir());

    let code = "(do { req <- askUser @DeployRequest; pure req.service }) :: M Text";
    let target = cfg.turn_target(None).expect("turn target");
    let src = tidepool_harness::engine::template_turn_for(
        &cfg.decls,
        &target.stack,
        code,
        "PrdTypes",
        "",
    );
    let result = tidepool_harness::compile::compile_turn(
        &cfg.extract_bin,
        &src,
        "result",
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    );
    assert!(
        result.is_ok(),
        "`askUser @DeployRequest` against types with the bare derive contract \
         must compile, and its binding must be a real DeployRequest — got: {:?}",
        result.err().map(|e| e.to_string())
    );
}
