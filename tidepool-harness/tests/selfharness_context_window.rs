//! Acceptance: a SECOND `runLLMTurn` hole in ONE loop sees the FIRST
//! hole's exchange (the accumulating context window).
//!
//! One render-seeded answerer session is
//! created per loop and every hole pushes onto it. This test drives a two-hole
//! harness through `run_one_loop_iteration` and, on the SECOND hole, asserts the
//! answerer's transcript already carries the FIRST hole's prompt AND answer.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::Arc;

use parking_lot::Mutex;

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, Message, ModelProvider, ProviderError, Role, StreamSink, TurnRequest,
    TurnResponse, Usage,
};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
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

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "selfharness-context-window".into(),
        extract_fingerprint: "selfharness-context-window".into(),
        harness_version: "test".into(),
    }
}

/// A provider that answers each hole with `finalize @Text ...`, but when it
/// sees the SECOND hole (its latest user message contains "SECOND-HOLE") it
/// records whether the FIRST hole's prompt AND answer are already present in
/// the transcript it was handed.
struct ContextCapturingProvider {
    /// `Some(true/false)` once the second hole is serviced: did the transcript
    /// already carry the first hole's exchange?
    second_saw_first: Arc<Mutex<Option<bool>>>,
}

impl ModelProvider for ContextCapturingProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let latest_user = req
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let is_second = latest_user.contains("SECOND-HOLE");
        if is_second {
            // The whole transcript (every user + assistant turn so far).
            let joined: String = req
                .messages
                .iter()
                .map(|m: &Message| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let saw_first_prompt = joined.contains("FIRST-HOLE");
            let saw_first_answer = joined.contains("apple");
            *self.second_saw_first.lock() = Some(saw_first_prompt && saw_first_answer);
        }

        let answer = if is_second { "blue" } else { "apple" };
        Ok(TurnResponse {
            text: format!("```haskell\n(finalize @Text (\"{answer}\" :: Text) :: M ())\n```"),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 5,
                cached_input_tokens: None,
                cache_write_tokens: None,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_hole_sees_first_holes_exchange() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let second_saw_first = Arc::new(Mutex::new(None));
    let provider: Arc<dyn DynModelProvider> = Arc::new(ContextCapturingProvider {
        second_saw_first: second_saw_first.clone(),
    });

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "selfharness-context-window-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    let source = load_harness_source(&fixtures_dir().join("TwoHoleHarness.hs"))
        .expect("two-hole harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one full two-hole cycle");

    // Both answers landed in State — the loop ran both holes.
    let answers = outcome
        .state_json
        .get("answers")
        .and_then(|v| v.as_array())
        .expect("answers array");
    assert_eq!(
        answers
            .iter()
            .filter_map(|a| a.as_str())
            .collect::<Vec<_>>(),
        vec!["apple", "blue"],
        "both holes' answers must fold into State"
    );

    // When the SECOND hole was serviced, the answerer's
    // transcript ALREADY carried the FIRST hole's prompt + answer — one
    // accumulating session, not two isolated nodes.
    let saw = second_saw_first
        .lock()
        .expect("the second hole must have been serviced");
    assert!(
        saw,
        "the second hole's answerer transcript must already contain the first hole's \
         exchange (prompt FIRST-HOLE + answer apple) — the accumulating context window"
    );
}
