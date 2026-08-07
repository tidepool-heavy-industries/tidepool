//! W1/C1 acceptance: `render`'s output IS the answerer's SYSTEM message.
//!
//! The independent review found `render` was observational — its text landed
//! in `CycleOutcome` but never in the agent's system prompt (`assemble_request`
//! used only the hardcoded `SYSTEM_FRAMING`). This test drives one full cycle
//! through the production entry point (`run_one_cycle`) with a provider that
//! CAPTURES the exact request it is handed, and asserts the answerer turn's
//! System-role message is derived from `render` (not the default framing).
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::{Arc, Mutex};

use tidepool_harness::engine::{EngineConfig, SYSTEM_FRAMING};
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
        prelude_hash: "selfharness-framing".into(),
        extract_fingerprint: "selfharness-framing".into(),
        harness_version: "test".into(),
    }
}

/// A provider that records every System message it is asked to complete, then
/// serves a fixed `finalize @Decision` reply (so the cycle terminates).
struct CapturingProvider {
    systems: Arc<Mutex<Vec<String>>>,
    reply: String,
}

impl ModelProvider for CapturingProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        if let Some(system) = req.messages.iter().find(|m| matches!(m.role, Role::System)) {
            self.systems.lock().unwrap().push(system.content.clone());
        }
        Ok(TurnResponse {
            text: self.reply.clone(),
            usage: Usage {
                input_tokens: 50,
                output_tokens: 10,
            },
            reasoning: None,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_output_is_the_answerer_system_message() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, nix develop)");
        return;
    }

    let systems = Arc::new(Mutex::new(Vec::<String>::new()));
    let provider: Arc<dyn DynModelProvider> = Arc::new(CapturingProvider {
        systems: systems.clone(),
        reply: "```haskell\nimport Harness (Decision (..), Confidence (..))\n\n\
                (finalize @Decision (Decision { action = \"observe\", rationale = \"first loop\", \
                confidence = Medium }) :: M ())\n```"
            .to_string(),
    });

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let writer = tidepool_harness::log::LogWriter::create(
        &std::env::temp_dir().join(format!("selfharness-framing-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    driver
        .run_one_cycle(&source, None)
        .expect("one full render->loop->runLLMTurn->finalize->render cycle");

    let systems = systems.lock().unwrap();
    assert!(
        !systems.is_empty(),
        "the answerer must have been driven at least once (its System message captured)"
    );

    // Every answerer turn's System message is render-derived, NOT the default
    // full-surface framing.
    let answerer_system = &systems[0];
    assert!(
        answerer_system.contains("self-iterating agent"),
        "the answerer's System message must be render's output, got:\n{answerer_system}"
    );
    assert!(
        answerer_system.contains("observing"),
        "render's pre-loop text (initial Observing mode) must be in the System message, got:\n{answerer_system}"
    );
    assert_ne!(
        answerer_system, SYSTEM_FRAMING,
        "the answerer must NOT get the default SYSTEM_FRAMING — render's output overrides it"
    );
    // And the narrow answerer instruction (finalize) is appended.
    assert!(
        answerer_system.contains("finalize @T"),
        "the narrow answerer instruction must be appended after render's output, got:\n{answerer_system}"
    );
}
