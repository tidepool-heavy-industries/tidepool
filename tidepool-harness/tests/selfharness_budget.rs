//! Runaway-cap acceptance: a `runLLMTurn` answerer that never finalizes is
//! NUDGED at the soft cap and HARD-FAILS at the hard cap.
//!
//! 08-wave1-correctness.md LOCKED the per-hole budget: up to 16 tool-call
//! rounds accumulating context; at 16 the runtime nudges ("approaching max
//! tool calls, finalize now"); at 32 it hard-fails the `runLLMTurn` effect.
//! This drives a hole with a provider that NEVER finalizes (always emits a
//! compiling non-finalize block) and asserts (a) the cycle ultimately errors
//! at the hard cap and (b) a nudge was delivered at the soft cap. The caps are
//! lowered to 3/6 here (`set_answerer_round_caps`) so the SAME mechanism is
//! exercised with a few scripted turns rather than 16/32 real GHC compiles;
//! the 16/32 defaults live in the driver's constants.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::{Arc, Mutex};

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, Role, StreamSink, TurnRequest, TurnResponse,
    Usage,
};
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
        prelude_hash: "selfharness-budget".into(),
        extract_fingerprint: "selfharness-budget".into(),
        harness_version: "test".into(),
    }
}

/// A provider that NEVER finalizes: it emits a compiling but non-finalizing
/// block (`pure ()`) every turn, and records how many times it was called plus
/// whether it was ever handed a nudge ("approaching the maximum" appears in
/// the latest user turn).
struct NeverFinalizeProvider {
    calls: Arc<Mutex<u32>>,
    nudge_seen_at: Arc<Mutex<Option<u32>>>,
}

impl ModelProvider for NeverFinalizeProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let n = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            *c
        };
        let latest_user = req
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        if latest_user.contains("approaching the maximum") {
            let mut slot = self.nudge_seen_at.lock().unwrap();
            if slot.is_none() {
                *slot = Some(n);
            }
        }
        Ok(TurnResponse {
            text: "```haskell\n(pure () :: M ())\n```".to_string(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answerer_nudged_at_16_and_hard_fails_at_32() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, nix develop)");
        return;
    }
    let _cache_guard = support::isolate_cache();

    let calls = Arc::new(Mutex::new(0u32));
    let nudge_seen_at = Arc::new(Mutex::new(None));
    let provider: Arc<dyn DynModelProvider> = Arc::new(NeverFinalizeProvider {
        calls: calls.clone(),
        nudge_seen_at: nudge_seen_at.clone(),
    });

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("selfharness-budget-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    // Small caps (nudge=3, hard-fail=6) to trip the SAME nudge/hard-fail
    // behavior deterministically with a few scripted turns instead of 16/32
    // real GHC compiles (each round is a compile). The default 16/32 is
    // covered by the constants; this asserts the mechanism, not the numbers.
    driver.set_answerer_round_caps(3, 6);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let result = driver.run_one_cycle(&source, None).await;

    // The hole hard-fails the runLLMTurn effect at the configured hard cap (6).
    let err = result.expect_err("a never-finalizing answerer must hard-fail the cycle");
    let msg = err.to_string();
    assert!(
        msg.contains("6") && msg.contains("without finalizing"),
        "the failure must be the per-hole hard cap, got: {msg}"
    );

    // The answerer was driven exactly 6 rounds (the nudge is a push, not a
    // model round), so the provider saw 6 calls.
    let total_calls = *calls.lock().unwrap();
    assert_eq!(
        total_calls, 6,
        "the answerer must be driven exactly max-rounds (6) times before the hard cap fires"
    );

    // The nudge fired: the model saw the "approaching the maximum" message,
    // delivered right after round 3 (nudge cap), i.e. seen on the 4th model call.
    let nudge = nudge_seen_at
        .lock()
        .unwrap()
        .expect("the finalize nudge must have been delivered to the answerer");
    assert_eq!(
        nudge, 4,
        "the nudge must be delivered right after the nudge cap (round 3), on the 4th model call"
    );
}
