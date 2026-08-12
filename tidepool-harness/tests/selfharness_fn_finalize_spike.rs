//! FEASIBILITY SPIKE (not a regression gate — see the boundary note in the
//! module doc below once the verdict lands): can a self-iterating-harness
//! answerer `finalize @(State -> State)` a FUNCTION value that crosses
//! in-heap into the OUTER loop's suspended continuation and is APPLIED
//! there — `loop st = do { f <- runLLMTurn @(State -> State) ...; pure (f
//! st) }`, as opposed to finalizing a whole replacement `State`?
//!
//! Fixture: `examples/harness/fn-finalize-spike/{Harness,HarnessTypes}.hs`
//! — `State { counter :: Int, history :: [Text] }`, `loop` asks for a
//! `State -> State` edit and applies it to the incoming state.
//! (Field named `counter`, not `count` — `Tidepool.Prelude` re-exports
//! `Tidepool.Records.UpdateAllOutcome`'s own `count` field, which makes
//! every turn compile that references `count` ambiguous; not a spike
//! finding, just a fixture naming collision worked around before the real
//! test ran at all.)
//!
//! Drives TWO full `render -> loop -> runLLMTurn -> finalize -> render`
//! cycles through the PRODUCTION entry point
//! (`SelfHarnessDriver::run_one_cycle`, the same building block
//! `SelfHarnessDriver::run_loop` uses forever — see
//! `acceptance_selfharness.rs`), threading `state_json` between them exactly
//! like the production loop does, with a scripted [`ReplayProvider`] (no
//! live model). Each cycle's reply finalizes a closure that BUMPS `counter`
//! and APPENDS to `history` — so passing requires the function to have been
//! genuinely APPLIED to the incoming state (composing with cycle 1's edit),
//! not a wholesale replace.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`). Standalone (not bundled into an
//! existing family) because this is a crash-class probe: crossing an
//! unproven closure into a foreign session's continuation may abort the
//! process rather than raise a clean `Result` — see `tests/battery.sh`'s
//! tier rules on when a standalone compile is warranted.
//!
//! # Verdict (as originally written): NOT FEASIBLE AS-IS
//!
//! Blocked much earlier than the finalize/resume crossing this spike set out
//! to probe — at `tidepool-extract` COMPILE time, on the very first fused
//! `render`+`loop` compile of the OUTER session
//! (`SelfHarnessDriver::compile_cycle_entry`), before any answerer turn ever
//! runs. `loop`'s own `runLLMTurn @(State -> State)` call site failed to
//! compile at all, with:
//!
//! ```text
//! Error: function-typed answers not supported in R0 (site in loop): State -> State
//! ```
//!
//! **This specific gate is GONE (one-session plan Phase 3e).**
//! `checkRunLLMTurnType` (`haskell/src/Tidepool/Translate.hs`) no longer
//! rejects every function arrow — it admits a PURE function-typed answer
//! like `State -> State` and rejects only an answer type that itself
//! mentions the effect monad (`typeMentionsEffectMonad`: the `Eff` tycon, or
//! any tycon defined in the generated `Tidepool.Effects` module), with a
//! different error text ("effectful function answers not supported (the row
//! is fragment-nominal): ..."). `loop`'s `runLLMTurn @(State -> State)` site
//! is exactly the class of type this now admits, so the specific verbatim
//! error and reasoning quoted above no longer describe current behavior.
//!
//! What this file does NOT establish: whether this spike's fixture now runs
//! end to end. The extract gate was only the FIRST seam this spike hit —
//! the "further, UNREACHED concern" below (the driver's finalize-value
//! extraction path having no closure-aware branch, and no existing
//! mechanism applying a closure live in one session's heap against a value
//! from a DIFFERENT session's heap) is exactly the cross-session delivery
//! problem the one-session plan's later phases (registry collapse, parked
//! fragments on one shared machine) are the intended fix for. Left
//! `#[ignore]`d pending that work landing and this spike being re-run
//! against it — un-ignoring it is a separate, deliberate step for whoever
//! verifies the new behavior end to end, not implied by the extract gate
//! alone being lifted.
//!
//! **A further, UNREACHED concern, noted for whoever picks this up:** even
//! setting the (now-lifted) extract gate aside, the driver's OWN finalize-value
//! extraction path (`Harness::take_finalized_value_keep_open` ->
//! `take_finalized_value_core`, `harness.rs`) unconditionally reads
//! `fields[1]` of the `FinalizeWith(site, value)` request as "the finalized
//! value" — correct for plain data, but for a closure that field is
//! `CLOSURE_SENTINEL` (a synthetic `Con(u64::MAX, [])` the TOLERANT suspend
//! bridge substitutes for the real, live closure — `heap_bridge.rs`'s
//! `heap_to_value_forcing_tolerant`; see `acceptance_finalize.rs`'s module
//! doc). The driver has no closure-aware branch there: it would hand that
//! SENTINEL straight to `ResidentSession::resume` on the OUTER session
//! (`driver.rs::run_loop_fragment_inner`), which is a DIFFERENT
//! `ResidentSession` (its own heap/JITModule) than the one that produced the
//! closure — there is no existing mechanism, anywhere in this codebase, that
//! applies a closure live in one session's heap against a value from a
//! DIFFERENT session's heap (the only closure-application mechanism that
//! exists, `ResidentSession::apply_finalized`, runs same-session only, and
//! even that has its own known gap — `acceptance_finalize.rs`'s ignored
//! `finalize_closure_full_round_trip`). At the time this spike was written
//! the test never reached that path (the since-lifted extract gate stopped
//! it first), so this paragraph was, and remains, informed prediction from
//! reading the code rather than an observed result — this file has not been
//! re-run against the lifted gate to confirm whether it now reaches this
//! path, or what happens if it does.
//!
//! Seams from the spec's checklist (as observed AT THE TIME, against the
//! now-superseded extract gate — not re-verified against current behavior):
//! 1. ROW SPLICE SYNTAX — NOT hit. `EngineConfig::turn_target`'s
//!    `tidepool_mcp::RowArgs::at("Finalize", [ty])` already parenthesizes a
//!    compound answer type correctly (`acceptance_finalize.rs`'s
//!    `finalize_accepts_function_typed_site_where_runllmturn_rejects_it` /
//!    `finalize_closure_crosses_by_reference` already exercise `Finalize
//!    (Int -> Int)` through the identical `set_answer_contract` path). Not
//!    reached at the time — the failure was on the OUTER `runLLMTurn` site,
//!    which uses no `Finalize`-row splice at all.
//! 2. TRANSCRIPT RENDER — not reached at the time; no answerer turn ever ran.
//! 3. DEEP-FORCE OF CLOSURES — not reached at the time.
//! 4. JITMODULE LIFETIME — not reached at the time.
//! 5. asks.json TYPE RENDER — the closest match at the time, but stronger
//!    than anticipated: not a type-STRING mismatch, but `tidepool-extract`
//!    refusing to translate the `runLLMTurn` site at all — the gate that
//!    fired is the one described above, now lifted for pure arrows.

mod support;

use std::sync::{Arc, Mutex};

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Event, Harness, Observer, SelfHarnessDriver,
};

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn spike_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness/fn-finalize-spike")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "fn-finalize-spike".into(),
        extract_fingerprint: "fn-finalize-spike".into(),
        harness_version: "test".into(),
    }
}

/// One recorded `finalize @(State -> State) (...)` reply — a
/// fenced Haskell block an answerer turn would emit, finalizing an EDIT
/// closure that bumps `counter` and appends `note` to `history`. Mirrors
/// `acceptance_selfharness.rs`'s `decision_reply` and
/// `acceptance_finalize.rs`'s function-typed finalize reply shape.
fn edit_reply(note: &str) -> RecordedReply {
    let content = format!(
        "```haskell\nimport HarnessTypes (State (..))\n\n\
         (finalize @(State -> State) \
         (\\st -> st {{ counter = counter st + 1, history = history st <> [\"{note}\"] }}) \
         :: M ())\n```"
    );
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content,
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
        },
    }
}

/// Collects every [`Event`] the driver emits, so the test can assert a
/// `Finalize` event reached the transcript even where a later step (the
/// outer `resume`) fails — the event fires from inside
/// `service_runllm_hole`, strictly before the resume this spike's verdict is
/// about.
#[derive(Default)]
struct CapturingObserver {
    events: Mutex<Vec<Event>>,
}

impl Observer for CapturingObserver {
    fn on_event(&self, event: &Event) {
        self.events.lock().unwrap().push(event.clone());
    }
}

/// Drive two full cycles of the fn-finalize-spike fixture through the
/// production entry point. See this file's module doc for the verdict this
/// test originally pinned (NOT FEASIBLE AS-IS, at the extract's compile-time
/// gate) — that specific gate is now lifted (one-session plan Phase 3e:
/// `checkRunLLMTurnType` admits a pure function-typed answer like
/// `State -> State`), but this test's disposition against the current code
/// is UNVERIFIED (a deeper, still-open seam — cross-session closure
/// delivery — is documented in the module doc's "further, UNREACHED
/// concern"). Left ignored pending that work landing and this spike being
/// deliberately re-run and re-verified, not un-ignored as a side effect of
/// the extract gate alone changing.
#[ignore = "spike verdict pinned NOT FEASIBLE AS-IS against the extract's \
            prior compile-time gate — see module doc. That gate \
            (`checkRunLLMTurnType`, haskell/src/Tidepool/Translate.hs) no \
            longer hard-rejects a pure function-typed runLLMTurn answer \
            (one-session plan Phase 3e); it now rejects only an answer type \
            that itself mentions the effect monad. This test's outcome \
            against the lifted gate is UNVERIFIED — a deeper, still-open \
            seam (cross-session closure delivery on the finalize/resume \
            path) is documented in the module doc and is the real reason \
            this stays ignored rather than the now-superseded compile \
            rejection. Left ignored rather than deleted so the history and \
            the still-open seam stay pinned for whoever picks this up and \
            re-verifies end to end."]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fn_finalize_crosses_two_cycles_and_composes() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg =
        EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(spike_harness_dir()))
            .expect("answerer engine config");
    let replies = vec![edit_reply("cycle 1"), edit_reply("cycle 2")];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("fn-finalize-spike-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let observer = Arc::new(CapturingObserver::default());
    let mut driver = SelfHarnessDriver::new(agent, observer.clone());
    let source = load_harness_source(&spike_harness_dir().join("Harness.hs"))
        .expect("spike harness source loads");

    // --- Cycle 1 ---
    let outcome1 = driver
        .run_one_cycle(&source, None)
        .await
        .expect("cycle 1: finalize@(State -> State) crosses and applies");

    let state1 = &outcome1.state_json;
    assert_eq!(
        state1.get("counter").and_then(|v| v.as_i64()),
        Some(1),
        "cycle 1: counter must be bumped by the applied edit, got {state1:?}"
    );
    assert_eq!(
        state1.get("history").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .map(|x| x.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
        }),
        Some(vec!["cycle 1"]),
        "cycle 1: history must carry the applied edit's note, got {state1:?}"
    );

    // The transcript recorded a Finalize event for the answerer node, even
    // though the value it carries is the closure sentinel, not real data.
    let saw_finalize = observer
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, Event::Finalize { .. }));
    assert!(
        saw_finalize,
        "cycle 1: transcript must record a Finalize event for the answerer's \
         finalize@(State -> State) suspension"
    );

    // --- Cycle 2 --- proves COMPOSITION: the function must be applied to
    // the incoming (already-edited) state, not a wholesale replace — count
    // reaches 2 and BOTH cycles' notes are present.
    let outcome2 = driver
        .run_one_cycle(&source, Some(&outcome1.state_json))
        .await
        .expect("cycle 2: finalize@(State -> State) crosses and composes");

    let state2 = &outcome2.state_json;
    assert_eq!(
        state2.get("counter").and_then(|v| v.as_i64()),
        Some(2),
        "cycle 2: counter must accumulate across loop boundaries, got {state2:?}"
    );
    assert_eq!(
        state2.get("history").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .map(|x| x.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
        }),
        Some(vec!["cycle 1", "cycle 2"]),
        "cycle 2: history must carry BOTH cycles' notes — proves the \
         function was APPLIED to the incoming state, not a wholesale \
         replace, got {state2:?}"
    );

    // The driver survives both answerer retirements without being poisoned.
    assert!(
        driver
            .run_one_cycle(&source, Some(&outcome2.state_json))
            .await
            .is_ok(),
        "driver must survive both prior cycles' answerer retirements and \
         still be able to run a further cycle"
    );
}
