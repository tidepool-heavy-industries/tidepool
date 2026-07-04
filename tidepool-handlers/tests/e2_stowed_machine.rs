//! E2 — end-to-end coverage for threadless ask suspension driven through the
//! real [`SessionEngine`] production path (start_turn → SuspendedAsk → resume /
//! abort), on genuine GHC + JIT compiled Haskell that calls `ask`.
//!
//! These exercise the whole stow → resume choreography the E1 → E2 restructure
//! introduced:
//!   1. a turn that `ask`s suspends as a STOWED machine (no parked thread) and
//!      resumes on a fresh thread with the answer delivered intact;
//!   2. THE GC HAZARD (root's mandatory test): a heap value live at the ask
//!      boundary survives the suspension AND a GC triggered during resume — if
//!      the stowed continuation's rooting were wrong, the post-resume result
//!      would be corrupt or crash;
//!   3. abort-while-stowed produces the same terminal `Error` a pre-E2
//!      answer-channel abort did.
//!
//! Needs `TIDEPOOL_EXTRACT` (a built `tidepool-extract-bin`) + GHC on PATH;
//! skips (passes) otherwise, like the other extract-dependent suites.

use std::path::{Path, PathBuf};

use tidepool_mcp::CapturedOutput;
use tidepool_runtime::session::{
    EngineConfig, RenderPolicy, ResumeOutcome, Retention, SessionEngine, StartError, StartTurn,
    TurnOutcome,
};

fn ghc_available() -> bool {
    if std::env::var("TIDEPOOL_EXTRACT").is_err() {
        let bin = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("haskell")
            .join("tidepool-extract");
        if bin.exists() {
            std::env::set_var("TIDEPOOL_EXTRACT", &bin);
        }
    }
    std::process::Command::new("ghc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn prelude_include() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell/lib")
}

fn test_engine() -> SessionEngine<CapturedOutput> {
    SessionEngine::new(EngineConfig {
        max_concurrent: 4,
        max_orphaned: 16,
        cont_prefix: "cont".into(),
        default_timeout_secs: 60,
        render: RenderPolicy::Json,
        retention: Retention::DropAfterDone,
    })
}

/// Build a `StartTurn` for `code` (a do-block body) using the minimal effect
/// stack (Console + Ask). `nursery_size` is exposed so the GC test can force a
/// small heap.
fn start_turn_for(
    code: &str,
    nursery_size: usize,
) -> StartTurn<
    impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput> + Send + 'static,
    CapturedOutput,
> {
    let stack = tidepool_handlers::build_minimal_stack();
    let (decls, ask_tag) = tidepool_handlers::base_decls_with_ask(&stack);
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack_type = tidepool_mcp::build_effect_stack_type(&decls);
    let wrapped_code = tidepool_mcp::wrap_do(code);
    let source: std::sync::Arc<str> =
        tidepool_mcp::template_haskell(&preamble, &stack_type, &wrapped_code, "", "", None, None)
            .into();
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("effects module");
    let include = vec![prelude_include(), effects_dir];
    let effect_names: Vec<String> = decls.iter().map(|d| d.type_name.to_string()).collect();

    StartTurn {
        source,
        include,
        handlers: stack,
        ask_tag,
        effect_names,
        captured: CapturedOutput::new(),
        nursery_size,
        timeout_secs: 60,
    }
}

/// Bridge a JSON value into the canonical answer the engine's `resume` validator
/// would return (here we skip schema validation and pass it through verbatim).
fn answer(json: serde_json::Value) -> serde_json::Value {
    json
}

const DEFAULT_NURSERY: usize = 1 << 20; // 1 MiB

/// A turn that `ask`s and returns the answer verbatim: the suspension stows the
/// machine, and resuming delivers the answer through the reconstructed machine
/// on a fresh thread.
#[tokio::test]
async fn e2_suspend_resume_roundtrip() {
    if !ghc_available() {
        eprintln!("skipping: GHC/TIDEPOOL_EXTRACT unavailable");
        return;
    }
    let engine = test_engine();
    let turn = start_turn_for("x <- ask SNum \"pick\"\npure x", DEFAULT_NURSERY);
    let cont_id = match engine.start_turn(turn).await {
        Ok(TurnOutcome::SuspendedAsk {
            cont_id, prompt, ..
        }) => {
            assert_eq!(prompt, "pick");
            cont_id
        }
        Ok(other) => panic!(
            "expected SuspendedAsk, got a different outcome: {}",
            describe(&other)
        ),
        Err(StartError::Overloaded) => panic!("unexpected Overloaded"),
        Err(StartError::Busy) => panic!("unexpected Busy"),
    };

    let outcome = engine
        .resume::<_, String>(&cont_id, |_schema| Ok(answer(serde_json::json!(42))))
        .await;
    match outcome {
        ResumeOutcome::Driven(TurnOutcome::Completed { result, .. }) => {
            assert_eq!(
                result, "42",
                "the answer must round-trip through the resumed machine"
            );
        }
        other => panic!(
            "expected Driven(Completed 42), got {}",
            describe_resume(&other)
        ),
    }
}

/// THE GC HAZARD TEST (mandatory). A heap list is built + its spine forced
/// BEFORE the `ask`, so the stowed continuation captures it. After resume a
/// large allocation (`sum [1..5000]`) triggers at least one GC on the retained
/// session heap while that continuation is live; the final result re-consumes
/// the pre-suspension list. A broken continuation/heap rooting across the stow
/// would corrupt or collect it and the arithmetic would be wrong (or crash).
#[tokio::test]
async fn e2_stow_gc_resume_heap_intact() {
    if !ghc_available() {
        eprintln!("skipping: GHC/TIDEPOOL_EXTRACT unavailable");
        return;
    }
    let engine = test_engine();
    // Small nursery so the post-resume `sum [1..5000]` forces GC.
    let code = "\
let xs = enumFromTo 1 200 :: [Int]\n\
n <- pure $! length xs\n\
_ <- ask SNum \"checkpoint\"\n\
let filler = sum (enumFromTo 1 5000 :: [Int])\n\
pure (n + filler + sum xs)";
    let turn = start_turn_for(code, 64 * 1024);
    let cont_id = match engine.start_turn(turn).await {
        Ok(TurnOutcome::SuspendedAsk { cont_id, .. }) => cont_id,
        Ok(other) => panic!("expected SuspendedAsk, got {}", describe(&other)),
        Err(_) => panic!("admission failure"),
    };

    let outcome = engine
        .resume::<_, String>(&cont_id, |_schema| Ok(answer(serde_json::json!(0))))
        .await;
    match outcome {
        ResumeOutcome::Driven(TurnOutcome::Completed { result, .. }) => {
            // 200 (length) + 12502500 (sum 1..5000) + 20100 (sum 1..200).
            assert_eq!(
                result, "12522800",
                "pre-suspension heap must survive the stow + a post-resume GC intact"
            );
        }
        other => panic!(
            "expected Driven(Completed), heap likely corrupted across suspension; got {}",
            describe_resume(&other)
        ),
    }
}

/// Aborting a stowed ask continuation drives the machine to the same terminal
/// error a pre-E2 answer-channel abort produced: an `Error` outcome whose detail
/// carries the "ask aborted by caller" message.
#[tokio::test]
async fn e2_abort_while_stowed() {
    if !ghc_available() {
        eprintln!("skipping: GHC/TIDEPOOL_EXTRACT unavailable");
        return;
    }
    let engine = test_engine();
    let turn = start_turn_for("_ <- ask SNum \"cp\"\npure (0 :: Int)", DEFAULT_NURSERY);
    let cont_id = match engine.start_turn(turn).await {
        Ok(TurnOutcome::SuspendedAsk { cont_id, .. }) => cont_id,
        Ok(other) => panic!("expected SuspendedAsk, got {}", describe(&other)),
        Err(_) => panic!("admission failure"),
    };

    match engine.abort(&cont_id, "user cancelled".into()).await {
        tidepool_runtime::session::AbortOutcome::Driven(TurnOutcome::Error { detail, .. }) => {
            assert!(
                detail.contains("aborted by caller"),
                "abort detail should mirror pre-E2's answer-channel abort; got: {detail}"
            );
        }
        other => panic!(
            "expected Driven(Error) with abort detail, got {}",
            match other {
                tidepool_runtime::session::AbortOutcome::Driven(o) => describe(&o),
                tidepool_runtime::session::AbortOutcome::NotFound => "NotFound".into(),
                tidepool_runtime::session::AbortOutcome::ThreadGone => "ThreadGone".into(),
            }
        ),
    }
    // The continuation is consumed either way.
    let second = engine.abort(&cont_id, "again".into()).await;
    assert!(matches!(
        second,
        tidepool_runtime::session::AbortOutcome::NotFound
    ));
}

fn describe(o: &TurnOutcome) -> String {
    match o {
        TurnOutcome::Completed { result, .. } => format!("Completed({result})"),
        TurnOutcome::SuspendedAsk { prompt, .. } => format!("SuspendedAsk({prompt})"),
        TurnOutcome::Paused { .. } => "Paused".into(),
        TurnOutcome::Error { detail, .. } => format!("Error({detail})"),
        TurnOutcome::TimedOut { .. } => "TimedOut".into(),
        TurnOutcome::Crashed { thread_panic, .. } => format!("Crashed({thread_panic:?})"),
    }
}

fn describe_resume(o: &ResumeOutcome<String>) -> String {
    match o {
        ResumeOutcome::NotFound => "NotFound".into(),
        ResumeOutcome::Invalid(v) => format!("Invalid({v})"),
        ResumeOutcome::ThreadGone => "ThreadGone".into(),
        ResumeOutcome::Driven(o) => format!("Driven({})", describe(o)),
    }
}
