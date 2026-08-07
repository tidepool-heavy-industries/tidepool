//! Acceptance coverage for `finalize` (self-iterating-harness WS-B): an
//! Agent turn's `finalize @T x` hands a typed value UP and TERMINATES its
//! own turn loop — it does NOT resume, unlike a `runLLMTurn`/`Fork` answer
//! (see `acceptance_run_llm_turn.rs`). Needs `TIDEPOOL_EXTRACT` and the
//! with-packages GHC on PATH (`--ignore-default-filter` to run; see
//! `tests/golden_path.rs` for the env recipe).
//!
//! The thread: force root -> the (replayed) model's block evaluates
//! `finalize @T x` and suspends -> the suspension classifies as
//! `HoleRouting::Finalize` -> `Harness::take_finalized_value` reads the raw
//! value straight out of the suspended request (never through JSON) and
//! terminates the node -> the node's continuation is never resumed.
//!
//! KNOWN LIMITATION (found while writing this suite, not fixed here — out
//! of WS-B's safe scope): the "relaxed function-arrow rule" is fully wired
//! at the EXTRACT level (`checkFinalizeType` in Translate.hs skips
//! `typeHasFunctionArrow`, so `finalize @(Int -> Int) f` compiles where
//! `runLLMTurn @(Int -> Int)` is rejected — see
//! `finalize_accepts_function_typed_site_where_runllmturn_rejects_it` below)
//! but a function VALUE cannot yet complete a full runtime round-trip
//! through `take_finalized_value`: `tidepool-codegen/src/heap_bridge.rs`'s
//! shared request bridge (`heap_to_value_forcing`, the ONE suspend-path
//! bridge `Ask`/`RunLLMTurn`/`Finalize` all share) hits a heap object tagged
//! `TAG_CLOSURE` and DELIBERATELY errors — `core-shapes.md §8` documents this
//! as intentional ("closures are opaque and should not appear as top-level
//! bridge results"), and `heap_bridge.rs`'s own `test_tag_closure_error`
//! pins it. That bridge produces `tidepool_eval::value::Value::Closure {
//! env, binder, body: CoreExpr }` — the TREE-WALKING ORACLE's closure shape
//! — but a JIT-compiled closure has no `CoreExpr` left to hand back (it's
//! already lowered to machine code), so this isn't a one-line fix: finalize
//! carrying a genuine closure end-to-end needs the value to stay off this
//! bridge entirely (e.g. a raw-pointer handoff straight into `run_child`'s
//! `ExternalEnv`, bypassing `Value` altogether) — a deeper change to
//! `DriveOutcome`/`SuspendableOutcome`/`ResidentOutcome`'s suspend shape than
//! WS-B's "share Ask's suspend path" scope covers, and one that touches
//! GC-rooting-sensitive code. Flagging for a follow-up rather than papering
//! over it. What IS proven end-to-end here: an ordinary DATA value finalizes
//! and terminates the loop correctly (`finalize_hands_up_a_plain_data_value`).

use std::sync::Arc;

use tidepool_eval::value::Value;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{Harness, HoleRouting, TurnOutcome};

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
        prelude_hash: "acceptance-finalize".into(),
        extract_fingerprint: "acceptance-finalize".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str) -> RecordedReply {
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

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// `finalize @Int n` — ordinary DATA crossing in-heap. Proves the full
/// production path: the suspension classifies as `HoleRouting::Finalize`
/// with the rendered answer type from asks.json; `take_finalized_value`
/// reads the value's OWN native representation straight out of the
/// suspended request (not JSON-decoded — this is the SAME shared
/// suspend/classify path `Ask`/`RunLLMTurn` use, just a different wire
/// shape for the value field); and the node TERMINATES (never resumed) —
/// `finalize` ends the turn loop rather than feeding a value back into it,
/// unlike answering a `runLLMTurn`/`Fork` hole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_hands_up_a_plain_data_value() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("finalize-data.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    // The `:: M ()` annotation is NOT part of finalize's own contract — it's
    // needed because `finalize`'s result type is fully free (it never
    // actually returns; the send diverges via suspension), and the
    // template's own `toJSON _r` wrapper around the WHOLE turn otherwise
    // leaves `_r`'s type ambiguous (GHC has no `ToJSON` instance to pick
    // without a hint) — this never actually matters at runtime since the
    // block suspends before reaching that wrapper.
    let replies = vec![reply("```haskell\n(finalize @Int (41 + 1) :: M ())\n```")];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("finalize data root", "Finalize with plain Int data.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    match &outcome {
        TurnOutcome::Suspended { classified, .. } => match &classified.routing {
            HoleRouting::Finalize { ty, .. } => {
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
        Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
        Value::Con(_, fields) => match fields.as_slice() {
            [Value::Lit(tidepool_repr::Literal::LitInt(n))] => *n,
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

/// EXTRACT-LEVEL proof of the relaxed function-arrow rule (mirrors
/// `run_llm_turn_sidecar.rs`'s `runllmturn_rejects_function_typed_site`,
/// which asserts the OPPOSITE for `runLLMTurn`): `finalize @(Int -> Int) f`
/// compiles cleanly — `checkFinalizeType` (Translate.hs) deliberately skips
/// `typeHasFunctionArrow`, unlike `checkRunLLMTurnType`. This is the
/// achievable half of "finalize may carry a closure" today; see this file's
/// module doc for why the RUNTIME round-trip of an actual closure value is a
/// separate, deeper gap this suite does not attempt to close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_accepts_function_typed_site_where_runllmturn_rejects_it() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let src = tidepool_harness::engine::template_turn(
        &cfg,
        "(finalize @(Int -> Int) (\\x -> x + 1) :: M ())\n",
        "",
        "",
    );
    let result =
        tidepool_harness::compile::compile_turn(&cfg.extract_bin, &src, "result", &cfg.include);
    assert!(
        result.is_ok(),
        "a function-typed finalize site must compile cleanly (the relaxed \
         function-arrow rule), got error: {:?}",
        result.err().map(|e| e.to_string())
    );
}
