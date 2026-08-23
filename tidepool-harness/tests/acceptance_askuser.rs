//! Acceptance coverage: the answerer `askUser @T` operator-form round-trip
//! (`plans/self-iterating-harness/14-generic-derived-askuser-prd.md`, delivery
//! step 5), driven through the production entry point
//! (`SelfHarnessDriver::run_one_loop_iteration`) against the reference harness module
//! (`examples/harness/Harness.hs`).
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::observer::FormSource;
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, VariantShape};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Event, Harness, Observer, OperatorGate,
    SelfHarnessDriver,
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
                doc: None,
            },
            FieldShape {
                key: "rationale".to_string(),
                shape: FormShape::String,
                doc: None,
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
                    doc: None,
                },
                doc: None,
            },
        ],
        doc: None,
    }
}

fn empty_product(type_key: &str, constructor: &str) -> FormShape {
    FormShape::Product {
        type_key: type_key.to_string(),
        constructor: constructor.to_string(),
        fields: vec![],
        doc: None,
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
    /// Every `note` text posted, in order — a `note` never blocks, so this
    /// is populated with no matching "answer" the way `seen` has one.
    notes: Mutex<Vec<String>>,
}

impl ScriptedGate {
    fn new() -> Self {
        ScriptedGate {
            seen: Mutex::new(Vec::new()),
            decision_presentations: AtomicUsize::new(0),
            notes: Mutex::new(Vec::new()),
        }
    }
}

impl OperatorGate for ScriptedGate {
    fn post_note(&self, text: &str) {
        self.notes.lock().push(text.to_string());
    }

    fn present_form(&self, shape: &FormShape) -> serde_json::Value {
        let shape = shape.clone();
        self.seen.lock().push(shape.clone());

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
}

/// A driver-emitted form event, reduced to what this test needs to compare:
/// which side raised the form ([`FormSource::Answerer`] vs
/// [`FormSource::OuterLoop`], collapsed to a bool since this cycle only
/// exercises the nested answerer) and the shape/submission payload.
#[derive(Debug, Clone, PartialEq)]
enum CapturedForm {
    Presented {
        answerer: bool,
        shape: FormShape,
        ask_id: u64,
    },
    Submitted {
        answerer: bool,
        submission: serde_json::Value,
        ask_id: u64,
    },
    /// `note text` — riding the same `AskUser` GADT, but display-only: no
    /// matching `Submitted` ever follows it (the driver resumes with `()`
    /// immediately, never blocking on the operator).
    Noted { answerer: bool, text: String },
}

#[derive(Default)]
struct CaptureObserver {
    forms: Mutex<Vec<CapturedForm>>,
}

impl Observer for CaptureObserver {
    fn on_event(&self, event: &Event) {
        let captured = match event {
            Event::FormPresented {
                source,
                shape,
                ask_id,
            } => CapturedForm::Presented {
                answerer: matches!(source, FormSource::Answerer { .. }),
                shape: shape.clone(),
                ask_id: ask_id.0,
            },
            Event::FormSubmitted {
                source,
                submission,
                ask_id,
            } => CapturedForm::Submitted {
                answerer: matches!(source, FormSource::Answerer { .. }),
                submission: submission.clone(),
                ask_id: ask_id.0,
            },
            Event::NotePosted { source, text } => CapturedForm::Noted {
                answerer: matches!(source, FormSource::Answerer { .. }),
                text: text.clone(),
            },
            _ => return,
        };
        self.forms.lock().push(captured);
    }
}

/// The narration `note` posts before the `Decision` form — asserted against
/// verbatim below, both at the gate ([`ScriptedGate::notes`]) and the driver's
/// own event stream ([`CapturedForm::Noted`]).
const NOTE_TEXT: &str = "About to gather a decision from the operator.";

/// The ONE recorded answerer reply: a fenced Haskell `do`-block that first
/// posts non-blocking narration with `note`, then asks the operator for a
/// whole `Decision` with `askUser @Decision`, then picks among two RUNTIME
/// values with `chooseMany`, then `finalize`s. All three suspensions happen
/// WITHIN this one block execution (a note/form resume is not a new model
/// turn), so one reply covers the whole exchange.
///
/// Nothing is imported but the answer types themselves: no form builder, no
/// codec, no `Tidepool.Form` import (it is auto-imported whenever `AskUser` is
/// in the compiling row).
fn askuser_reply() -> RecordedReply {
    let content = format!(
        "```haskell\n\
         import HarnessTypes (Decision (..), Confidence (..))\n\n\
         (do\n\
         \x20  note \"{NOTE_TEXT}\"\n\
         \x20  d <- askUser @Decision\n\
         \x20  picks <- chooseMany [(\"keep\", \"kept\"), (\"drop\", \"dropped\")]\n\
         \x20  finalize @Decision (d {{ rationale = T.intercalate \"+\" picks }})) :: M ()\n\
         ```"
    );
    RecordedReply {
        content,
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
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
        typed_request_agent_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![askuser_reply()]));

    let log_path = support::unique_temp_log_path("acceptance-askuser");
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let observer = Arc::new(CaptureObserver::default());
    let mut driver = SelfHarnessDriver::new(agent, observer.clone());
    let gate = Arc::new(ScriptedGate::new());
    driver.set_gate(gate.clone());
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one full render->loop->runLLMTurn->note->askUser->finalize->render cycle");

    // `note` posted to the gate WITHOUT blocking — the block's very next
    // statement (`askUser @Decision`) still ran in the SAME turn, and the
    // gate saw the note text verbatim.
    assert_eq!(
        gate.notes.lock().as_slice(),
        &[NOTE_TEXT.to_string()],
        "note must post to the gate exactly once, verbatim"
    );

    // The form the operator saw IS the type's own structure, derived with no
    // `Decision` value in existence: exact selector keys, exact constructor
    // keys, declaration order, and the nested `Confidence` choice. This is the
    // Haskell encoder and the Rust wire agreeing through the real extract/JIT.
    let seen = gate.seen.lock().clone();
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
                    doc: None,
                },
                FieldShape {
                    key: "drop".to_string(),
                    shape: FormShape::Bool,
                    doc: None,
                },
            ],
            doc: None,
        }),
        "chooseMany must offer one control per runtime label, got: {seen:?}"
    );

    // The driver's own emitted event stream — not just what the gate saw —
    // is the same NOTED/PRESENTED/SUBMITTED pairing in order, all from the
    // nested answerer (`FormSource::Answerer`), never `OuterLoop`. `note`
    // fires FIRST, with no matching `Submitted` (it never blocks). Each
    // Presented/Submitted pair shares one `ask_id`, and the re-prompt (a
    // FRESH presentation of the same `Decision` form after the malformed
    // submission) mints its OWN id rather than reusing the first — three
    // presentations here, so ids 1..=3, one per `present_askuser_form` call,
    // not per logical ask.
    let forms = observer.forms.lock().clone();
    assert_eq!(
        forms,
        vec![
            CapturedForm::Noted {
                answerer: true,
                text: NOTE_TEXT.to_string(),
            },
            CapturedForm::Presented {
                answerer: true,
                shape: expected_decision_shape(),
                ask_id: 1,
            },
            CapturedForm::Submitted {
                answerer: true,
                submission: serde_json::json!({}),
                ask_id: 1,
            },
            CapturedForm::Presented {
                answerer: true,
                shape: expected_decision_shape(),
                ask_id: 2,
            },
            CapturedForm::Submitted {
                answerer: true,
                submission: decision_answer(),
                ask_id: 2,
            },
            CapturedForm::Presented {
                answerer: true,
                shape: FormShape::Product {
                    type_key: "Choices".to_string(),
                    constructor: "Choices".to_string(),
                    fields: vec![
                        FieldShape {
                            key: "keep".to_string(),
                            shape: FormShape::Bool,
                            doc: None,
                        },
                        FieldShape {
                            key: "drop".to_string(),
                            shape: FormShape::Bool,
                            doc: None,
                        },
                    ],
                    doc: None,
                },
                ask_id: 3,
            },
            CapturedForm::Submitted {
                answerer: true,
                submission: serde_json::json!({"keep": true, "drop": false}),
                ask_id: 3,
            },
        ],
        "the driver must emit one FormPresented/FormSubmitted pair per \
         present_form call, in order, each pair sharing one ask_id and a \
         re-prompt minting a fresh one — the servicing loop's observability \
         contract is unchanged by the answerer/outer dedupe"
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

/// A scripted operator for the root-`Maybe` regression below: asserts every
/// presented shape is `OptionalShape String` (never a tagged `Nothing`/`Just`
/// sum), then answers the FIRST ask with JSON `null` (`Nothing`) and every
/// later ask with a JSON string (`Just`) — covering both directions the
/// authoritative `FromJSON (Maybe a)` decode distinguishes.
struct MaybeGate {
    calls: AtomicUsize,
}

impl OperatorGate for MaybeGate {
    fn present_form(&self, shape: &FormShape) -> serde_json::Value {
        assert_eq!(
            shape,
            &FormShape::Optional(Box::new(FormShape::String)),
            "root `Maybe Text` must derive OptionalShape String — the tagged \
             Nothing/Just sum the generic fallback used to produce is \
             something `FromJSON (Maybe a)` (null-or-inner) can never decode"
        );
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            serde_json::Value::Null
        } else {
            serde_json::json!("some text")
        }
    }
}

/// High-2 regression: `askUser @(Maybe Text)` at the ROOT (not nested in a
/// field) must present `OptionalShape String` and the operator's answer must
/// actually decode back into a typed `Maybe Text` — before the `GForm.hs`
/// `FormRoot (Maybe a)` fix, the generic fallback derived a `Nothing`/`Just`
/// tagged sum that `FromJSON (Maybe a)` (null-or-inner) could never read, so
/// every selection re-presented until the driver's reprompt cap. Exercises
/// BOTH directions — a `null` (`Nothing`) answer and a string (`Just`)
/// answer — in one cycle, through the real production entry point
/// (`SelfHarnessDriver::run_one_loop_iteration`), proving the shape AND the decode,
/// not just that the type compiles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_maybe_form_shape_and_decode_round_trip() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let content = "```haskell\n\
         import HarnessTypes (Decision (..), Confidence (..))\n\n\
         (do\n\
         \x20  a <- askUser @(Maybe Text)\n\
         \x20  b <- askUser @(Maybe Text)\n\
         \x20  finalize @Decision (Decision\n\
         \x20    { action = fromMaybe \"none\" a <> \"|\" <> fromMaybe \"none\" b\n\
         \x20    , rationale = \"root maybe round trip\"\n\
         \x20    , confidence = Low\n\
         \x20    })) :: M ()\n\
         ```"
    .to_string();
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![RecordedReply {
        content,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }]));

    let log_path = std::env::temp_dir().join(format!(
        "acceptance-askuser-root-maybe-{}.jsonl",
        std::process::id()
    ));
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let observer = Arc::new(CaptureObserver::default());
    let mut driver = SelfHarnessDriver::new(agent, observer);
    driver.set_gate(Arc::new(MaybeGate {
        calls: AtomicUsize::new(0),
    }));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("root Maybe askUser -> finalize round trip");

    let decision = outcome
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| {
            panic!(
                "lastDecision must be a Just Decision, got {:?}",
                outcome.state_json
            )
        });
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("none|some text"),
        "the Nothing answer and the Just answer must both have decoded \
         correctly and flowed through finalize, got {decision:?}"
    );
}

/// A gate that panics if ever asked to present a form — proof that a code
/// path never suspends at all, rather than merely proof that whatever it
/// was asked happened to decode.
struct PanicIfAskedGate;

impl OperatorGate for PanicIfAskedGate {
    fn present_form(&self, shape: &FormShape) -> serde_json::Value {
        panic!("present_form must never be called here, got shape: {shape:?}")
    }
}

/// Medium-5 regression: `choose []` must fail LOUD at the point it is
/// called, never suspend an empty choice the web gate can only reject
/// (permanently pending — no Haskell-side re-prompt ever fires because a
/// rejected submission never reaches `resolve_form`'s decode at all, unlike
/// a duplicate-label `error` which fires before any suspension happens
/// too).
///
/// Drives the answerer's row DIRECTLY as a plain root turn — `Harness::
/// run_to_hole_or_done` (unlike the nested runLLMTurn-servicing path)
/// retries only a compile failure or a blockless reply, never a RUNTIME
/// fault, so a genuine `error` call propagates immediately with no retry
/// and no risk of exhausting a replay queue. [`PanicIfAskedGate`] proves the
/// gate is never consulted at all — `choose` rejects before ever calling
/// `askUserRaw`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choose_with_no_options_fails_loud_before_suspending() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    // A bare expression turn (no `finalize` — this root carries no answer
    // contract, so it compiles at `Finalize Void` and has no finalize
    // capability at all; irrelevant here since `choose` never returns).
    let content = "```haskell\nchoose ([] :: [(Text, Int)]) :: M Int\n```".to_string();
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![RecordedReply {
        content,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }]));

    let log_path = std::env::temp_dir().join(format!(
        "acceptance-askuser-choose-empty-{}.jsonl",
        std::process::id()
    ));
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let harness = Arc::new(Harness::new(writer, agent_cfg, provider).expect("harness boots"));
    harness.set_escalation_gate(Arc::new(PanicIfAskedGate));

    let root = harness
        .create_root("choose-empty root", "call choose with no options")
        .unwrap();
    harness
        .force(root, tidepool_harness::log::Actor::Operator)
        .unwrap();
    let outcome = harness.run_to_hole_or_done(root).await;
    let err = outcome
        .err()
        .expect("choose [] must fail the turn rather than suspend or hang");
    let message = err.to_string();
    assert!(
        message.contains("choose"),
        "the failure should be traceable to choose, got: {message}"
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
    let mut cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
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
        false,
    );
    let result = tidepool_harness::engine::compile_turn(
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

/// Medium-6 regression: a `Maybe ()` field must be rejected at COMPILE time,
/// naming the field — `Nothing` and `Just ()` both collect to JSON `null`,
/// and `FromJSON (Maybe a)` maps every `null` back to `Nothing`, so `Just
/// ()` could never survive the round trip even though the rendered form
/// offers two distinct states.
#[test]
fn maybe_unit_field_is_a_compile_error_naming_the_field() {
    support::require_extract();
    let mut cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    cfg.include.push(fixtures_dir());

    let code = "askUser @MaybeUnitField";
    let target = cfg.turn_target(None).expect("turn target");
    let src = tidepool_harness::engine::template_turn_for(
        &cfg.decls,
        &target.stack,
        code,
        "PrdTypes",
        "",
        false,
    );
    let result = tidepool_harness::engine::compile_turn(
        &cfg.extract_bin,
        &src,
        "result",
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    );
    let err = result
        .err()
        .expect("askUser @MaybeUnitField must be a compile error, not a runtime one");
    let diags = match &err {
        tidepool_runtime::CompileError::Diagnostics(diags) => diags.clone(),
        other => panic!("expected a real GHC diagnostics report, got: {other}"),
    };
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("flag") && d.message.contains("Maybe ()")),
        "the diagnostic must name the offending field and its type, got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}
