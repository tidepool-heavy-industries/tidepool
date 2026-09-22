//! Acceptance coverage for `finalize`: an
//! Agent turn's `finalize @T x` hands a typed value UP and TERMINATES its
//! own turn loop — it does NOT resume, unlike a `runLLMTurn`/`Fork` answer.
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH
//! (`--ignore-default-filter` to run; see `bridge/haskell/CLAUDE.md`'s "Local
//! iteration" section for the env recipe).
//!
//! The thread: force root -> the (replayed) model's block evaluates
//! `finalize @T x` and suspends -> the suspension classifies as
//! `SuspensionRouting::Finalize` -> `Harness::take_finalized_value` reads the raw
//! value straight out of the suspended request (never through JSON) and
//! terminates the node -> the node's continuation is never resumed.
//!
//! CLOSURES THROUGH FINALIZE (reference-passing):
//! the "relaxed function-arrow rule" is wired at the EXTRACT level
//! (`checkFinalizeType` in Translate.hs skips `typeHasFunctionArrow`, so
//! `finalize @(Int -> Int) f` compiles where `runLLMTurn @(Int -> Int)` is
//! rejected — see `finalize_accepts_function_typed_site_where_runllmturn_rejects_it`)
//! AND prepared observation accepts a function-typed finalize value at
//! suspend by substituting a `CLOSURE_SENTINEL` placeholder for the closure field so the
//! surrounding `FinalizeWith(site, _)` still bridges for the classifier, while
//! the REAL closure stays LIVE in the suspended session's heap. The harness
//! detects this via `finalize_is_closure` and takes the payload as a handle
//! (`take_live_payload_handle_keep_open`) rather than bridging it to a data
//! `HaskellValue`. An ordinary DATA value still finalizes + terminates correctly
//! (`finalize_hands_up_a_plain_data_value`).

use crate::support;

use std::sync::Arc;

use tidepool_bridge::HaskellValue;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::harness::AnswerContract;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeState;
use tidepool_harness::{Harness, SuspensionRouting, TurnOutcome};

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("bridge/haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("bridge/haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-finalize".into(),
        extract_fingerprint: "acceptance-finalize".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        content: content.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// `finalize @Int n` — ordinary DATA crossing in-heap. Proves the full
/// production path: the suspension classifies as `SuspensionRouting::Finalize`
/// with the rendered answer type from asks.json; `take_finalized_value`
/// reads the value's OWN native representation straight out of the
/// suspended request (not JSON-decoded — this is the SAME shared
/// suspend/classify path `Ask`/`RunLLMTurn` use, just a different wire
/// shape for the value field); and the node TERMINATES (never resumed) —
/// `finalize` ends the turn loop rather than feeding a value back into it,
/// unlike answering a `runLLMTurn`/`Fork` hole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_hands_up_a_plain_data_value() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("finalize-data.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    // Bare, no annotation of any kind — the shape the answerer prompt
    // prescribes. `finalize`'s result type is fully free (it never actually
    // returns; the send diverges via suspension); `template_turn_for`
    // (`exomonad/harness/src/engine.rs`) anchors the shared template's
    // `toJSON _r` wrapper for a turn compiled against a real `Finalize T`
    // row, so this block does not need to write an annotation itself.
    let replies = vec![reply("```haskell\nfinalize @Int (41 + 1)\n```")];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("finalize data root", "Finalize with plain Int data.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    // `Finalize` is type-indexed (`Member (Finalize T) effs`): the block below
    // calls `finalize @Int`, so the row this turn compiles against must name
    // `Finalize Int` — the config's own default row (`Finalize Void`) is
    // uninhabited by design.
    harness.set_answer_contract(
        root,
        Some(AnswerContract {
            ty: "Int".to_string(),
            imports: vec![],
        }),
    );

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    match &outcome {
        TurnOutcome::Suspended { classified, .. } => match &classified.routing {
            SuspensionRouting::Finalize { ty, .. } => {
                assert_eq!(
                    ty.as_deref(),
                    Some("Int"),
                    "the published hole must carry the RENDERED finalize answer \
                     type from asks.json, got {ty:?}"
                );
            }
            other => panic!("expected a Finalize hole, got {other:?}"),
        },
        other => panic!(
            "root should suspend at finalize, got {}",
            outcome_tag(other)
        ),
    }
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    let value = harness
        .take_finalized_value(root)
        .expect("finalize value extracted");
    // A boxed `Int` (`data Int = I# Int#`) crosses as `Con(I#, [Lit(LitInt
    // n)])` — a bare bottom-level `Lit` only when something along the way
    // unboxes it, which `finalize`'s no-`unsafeCoerce` path doesn't do.
    // Accept either shape; what matters is finalize carries the value's OWN
    // native representation (not a JSON round-trip).
    let n = match &value {
        HaskellValue::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
        HaskellValue::Con(_, fields) => match fields.as_slice() {
            [HaskellValue::Lit(tidepool_repr::Literal::LitInt(n))] => *n,
            _ => panic!("expected a boxed Int (one LitInt field), got {value:?}"),
        },
        other => panic!("expected an Int value, got {other:?}"),
    };
    assert_eq!(
        n, 42,
        "finalize must carry the value's OWN native representation"
    );

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Cancelled {
            reason: "finalized".to_string()
        }),
        "finalize terminates the turn loop — the node reaches the tree's \
         terminal-from-Suspended state, never resumed (no HoleConsumed, no \
         re-entry into the continuation)"
    );

    // Once finalized, the hole is gone (take_finalized_value clears pending)
    // — a second call must fail rather than re-extract stale state.
    assert!(
        harness.take_finalized_value(root).is_err(),
        "a node with no pending hole must reject a second take_finalized_value"
    );
}

/// The answer contract's imports are part of the whole authored compile view,
/// including declarations persisted into the resident declaration plane. The
/// model does not need to repeat a system-supplied type import merely because
/// its first turn defines a helper around that type.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answer_contract_imports_persist_with_declarations() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("finalize-contract-decl-import.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    cfg.include.push(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repository root")
            .join("examples/harness"),
    );

    let replies = vec![
        reply("```haskell\ndata WrappedDecision = WrappedDecision Decision\n```"),
        reply(
            "```haskell\n\
             (case WrappedDecision (Decision { action = \"observe\", rationale = \"because\", confidence = High }) of\n\
             \x20  WrappedDecision decision -> finalize @Decision decision) :: M ()\n\
             ```",
        ),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "contract import declaration root",
            "Define a helper around the answer type, then use it.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    harness.set_answer_contract(
        root,
        Some(AnswerContract {
            ty: "Decision".to_string(),
            imports: vec!["HarnessTypes (Confidence (..), Decision (..))".to_string()],
        }),
    );

    let declared = harness
        .run_to_hole_or_done(root)
        .await
        .expect("contract-imported declaration must persist");
    assert!(
        matches!(declared, TurnOutcome::Completed { .. }),
        "the declaration turn must complete, got {}",
        outcome_tag(&declared)
    );

    let finalized = harness
        .follow_up(root, "Now construct and finalize the wrapped decision.")
        .await
        .expect("the persisted declaration must remain usable");
    assert!(
        matches!(finalized, TurnOutcome::Suspended { .. }),
        "the second turn must suspend at finalize, got {}",
        outcome_tag(&finalized)
    );
}

/// EXTRACT-LEVEL proof of the relaxed function-arrow rule:
/// `finalize @(Int -> Int) f`
/// compiles cleanly — `checkFinalizeType` (Translate.hs) deliberately skips
/// `typeHasFunctionArrow`, unlike `checkRunLLMTurnType`. This is the
/// achievable half of "finalize may carry a closure" today; see this file's
/// module doc for why the RUNTIME round-trip of an actual closure value is a
/// separate, deeper gap this suite does not attempt to close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_accepts_function_typed_site_where_runllmturn_rejects_it() {
    support::require_extract();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let target = cfg
        .turn_target(Some(("Int -> Int", &[])))
        .expect("turn target");
    let src = tidepool_harness::engine::template_turn(
        &cfg,
        &target.stack,
        "(finalize @(Int -> Int) (\\x -> x + 1) :: M ())\n",
        "",
        "",
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
        "a function-typed finalize site must compile cleanly (the relaxed \
         function-arrow rule), got error: {:?}",
        result.err().map(|e| e.to_string())
    );
}
