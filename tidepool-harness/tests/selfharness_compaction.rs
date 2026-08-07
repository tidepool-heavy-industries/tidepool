//! WS-E acceptance coverage for the self-iterating harness driver's
//! runtime-owned emergency compaction (`plans/self-iterating-harness/
//! 02-runtime.md` Compaction section, `07-impl-orchestration.md` WS-E):
//! a low `compaction_threshold_percent` trips the driver's `~80%` check off
//! a single scripted `runLLMTurn` answerer's usage, forcing a compact-to-text
//! turn whose `Text` reaches the NEXT `render`'s `Maybe Text` argument
//! (`examples/harness/Harness.hs`'s `render` prints "Summary of the prior
//! window:" when `lastCompaction` is `Just`). Needs `TIDEPOOL_EXTRACT` and
//! the with-packages GHC on PATH — run inside `nix develop` (see
//! `haskell/CLAUDE.md`).

use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{load_harness_source, Harness, LogObserver, SelfHarnessDriver};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

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
        prelude_hash: "selfharness-compaction".into(),
        extract_fingerprint: "selfharness-compaction".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str, input_tokens: u64, output_tokens: u64) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: Usage {
            input_tokens,
            output_tokens,
        },
    }
}

/// A LOW `compaction_threshold_percent` trips the runtime-owned emergency
/// trigger off the single `runLLMTurn @Decision` answerer's usage
/// (02-runtime.md LOCKED: the *runtime* owns this check, never the loop),
/// forcing a second scripted turn that finalizes a `Text` summary. Asserts
/// the compaction `Text` is produced (`CycleOutcome::compaction`) AND
/// reaches the next render's `Maybe Text` (`prompt_after` shows the
/// reference harness's "Summary of the prior window:" block).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_trigger_fires_and_reaches_next_render() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let agent_cfg = EngineConfig::standard(prelude_dir(), Some(examples_harness_dir()))
        .expect("agent engine config");
    let replies = vec![
        // 1. The loop's own `runLLMTurn @Decision` hole.
        reply(
            "```haskell\nimport Harness (Decision (..), Confidence (..))\n\n\
             (finalize @Decision (Decision { action = \"observe\", rationale = \"first loop\", \
             confidence = Medium }) :: M ())\n```",
            50,
            10,
        ),
        // 2. The forced compaction turn (`compaction_trigger`'s own answerer node).
        reply(
            "```haskell\n(finalize @Text (\"Explored the state space and picked an \
             observation-first strategy.\" :: Text) :: M ())\n```",
            50,
            10,
        ),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        &std::env::temp_dir().join(format!(
            "selfharness-compaction-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    // 60 tokens (50+10) from the `runLLMTurn` answerer trivially crosses 1%
    // of the default 2048-token budget (~20 tokens) — trips compaction
    // deterministically without needing a long scripted reply sequence to
    // organically cross the real ~80% default.
    driver.set_compaction_threshold_percent(1);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .expect("one render->loop->runLLMTurn->finalize->compaction->render cycle");

    let compaction = outcome
        .compaction
        .as_deref()
        .expect("usage past the (lowered) threshold must force a compaction turn");
    assert!(
        compaction.contains("observation-first"),
        "the compaction Text must be the forced turn's finalized summary, got: {compaction:?}"
    );

    // The compaction Text reaches the NEXT render's `Maybe Text` — proven by
    // `prompt_after` (rendered with the just-updated `self.last_compaction`)
    // showing the reference harness's compaction block.
    assert!(
        outcome
            .prompt_after
            .contains("Summary of the prior window:"),
        "post-loop render must show the compaction block once lastCompaction is Just, got:\n{}",
        outcome.prompt_after
    );
    assert!(
        outcome.prompt_after.contains("observation-first"),
        "post-loop render must include the actual compaction summary text, got:\n{}",
        outcome.prompt_after
    );
}
