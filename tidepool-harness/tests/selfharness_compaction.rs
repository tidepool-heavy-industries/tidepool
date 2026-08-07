//! W2 acceptance coverage for the self-iterating harness driver's
//! runtime-owned MID-LOOP, IN-PLACE compaction (`plans/self-iterating-harness/
//! 02-runtime.md` Compaction section; `08-wave1-correctness.md` W2):
//!
//! A low context-window budget trips the driver's `~80%` check off the
//! answerer session's REAL accumulated context BETWEEN the loop's two holes.
//! The runtime forces a compact-to-text turn (summarizing the answerer's real
//! transcript, not a fresh context-free node — the review C3 fix), then
//! REPLACES the answerer's context with that summary IN PLACE so the loop's
//! SECOND hole continues under the smaller window (no loop-abort). The test
//! asserts:
//!   1. the compaction `Text` is produced (`CycleOutcome::compaction`),
//!   2. it reaches the next `render`'s `Maybe Text` (`prompt_after` shows the
//!      fixture's "Summary of the prior window:" block),
//!   3. IN-PLACE relief: the SECOND hole's answerer transcript carries the
//!      SUMMARY, not the raw first-hole exchange — the loop CONTINUED under the
//!      replaced context.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::{Arc, Mutex};

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, Message, ModelProvider, ProviderError, Role, StreamSink, TurnRequest,
    TurnResponse, Usage,
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
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
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
        prelude_hash: "selfharness-compaction".into(),
        extract_fingerprint: "selfharness-compaction".into(),
        harness_version: "test".into(),
    }
}

/// The compaction summary the scripted compaction turn finalizes. A unique
/// sentinel so the test can prove it (a) reached the next render and (b)
/// replaced the second hole's context in place.
const SUMMARY_SENTINEL: &str = "COMPACTED-SUMMARY-chose-a-fruit-first";

/// A provider that:
/// - answers the compaction request (its prompt asks to `finalize @Text
///   (yourSummary`) with a fixed [`SUMMARY_SENTINEL`] summary,
/// - answers the two `runLLMTurn @Text` holes with `apple` / `blue`,
/// - and, when it services the SECOND hole, records whether the transcript it
///   was handed carries the SUMMARY (in-place relief) rather than the raw
///   first-hole exchange (`apple`) — the W2 property.
struct CompactionProbeProvider {
    /// `Some((saw_summary, saw_raw_first))` once the second hole is serviced.
    second_hole: Arc<Mutex<Option<(bool, bool)>>>,
}

impl ModelProvider for CompactionProbeProvider {
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

        // The forced compaction turn: its prompt asks for `finalize @Text
        // (yourSummary`. Emit a big summary so it is unambiguous.
        if latest_user.contains("yourSummary") {
            return Ok(TurnResponse {
                text: format!(
                    "```haskell\n(finalize @Text (\"{SUMMARY_SENTINEL}\" :: Text) :: M ())\n```"
                ),
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 5,
                },
                reasoning: None,
            });
        }

        let is_second = latest_user.contains("SECOND-HOLE");
        if is_second {
            let joined: String = req
                .messages
                .iter()
                .map(|m: &Message| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let saw_summary = joined.contains(SUMMARY_SENTINEL);
            // The raw first-hole answer ("apple") must be GONE from the
            // transcript once the context has been compacted in place.
            let saw_raw_first = joined.contains("apple");
            *self.second_hole.lock().unwrap() = Some((saw_summary, saw_raw_first));
        }

        let answer = if is_second { "blue" } else { "apple" };
        // Large per-turn usage so the FIRST hole alone crosses the (low) test
        // context-window budget, tripping compaction between the two holes.
        Ok(TurnResponse {
            text: format!("```haskell\n(finalize @Text (\"{answer}\" :: Text) :: M ())\n```"),
            usage: Usage {
                input_tokens: 400,
                output_tokens: 100,
            },
            reasoning: None,
        })
    }
}

/// Mid-loop, in-place compaction: with a low context-window budget, the
/// answerer's real context crosses ~80% after the FIRST hole; the runtime
/// compacts to text, replaces the answerer's context in place, and the SECOND
/// hole runs under the summary. The summary reaches the next render.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_fires_mid_loop_in_place_and_reaches_next_render() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let second_hole = Arc::new(Mutex::new(None));
    let provider: Arc<dyn DynModelProvider> = Arc::new(CompactionProbeProvider {
        second_hole: second_hole.clone(),
    });

    let mut agent_cfg =
        EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(fixtures_dir()))
            .expect("answerer engine config");
    // A LOW context-window budget (1000 tokens) so the first hole's ~500-token
    // usage crosses the 80% threshold (800) only AFTER the second hole would
    // push it over — set threshold to 50% (500) so the first hole alone
    // (400+100 = 500) trips it, tripping compaction BETWEEN the two holes.
    agent_cfg.context_window_tokens = Some(1000);

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
    // 50% of the 1000-token budget = 500 tokens: the first hole's 500-token
    // usage trips compaction between the holes, deterministically.
    driver.set_compaction_threshold_percent(50);
    let source = load_harness_source(&fixtures_dir().join("CompactionHarness.hs"))
        .expect("compaction harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .expect("one two-hole cycle with a mid-loop compaction");

    // (1) The mid-loop compaction produced its Text.
    let compaction = outcome
        .compaction
        .as_deref()
        .expect("usage past the (lowered) threshold must force a mid-loop compaction turn");
    assert!(
        compaction.contains(SUMMARY_SENTINEL),
        "the compaction Text must be the forced turn's finalized summary, got: {compaction:?}"
    );

    // Both holes still ran to completion — the loop CONTINUED, no abort.
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
        "both holes' answers must fold into State — the loop continued past compaction"
    );

    // (2) The compaction Text reaches the NEXT render's `Maybe Text`.
    assert!(
        outcome
            .prompt_after
            .contains("Summary of the prior window:"),
        "post-loop render must show the compaction block once lastCompaction is Just, got:\n{}",
        outcome.prompt_after
    );
    assert!(
        outcome.prompt_after.contains(SUMMARY_SENTINEL),
        "post-loop render must include the actual compaction summary text, got:\n{}",
        outcome.prompt_after
    );

    // (3) IN-PLACE relief: the second hole's answerer transcript carried the
    // SUMMARY and NOT the raw first-hole exchange — the context was replaced in
    // place, and the loop's remaining hole drove under the smaller window.
    let (saw_summary, saw_raw_first) = second_hole
        .lock()
        .unwrap()
        .expect("the second hole must have been serviced");
    assert!(
        saw_summary,
        "the second hole's answerer context must carry the compaction summary \
         (the replaced-in-place window)"
    );
    assert!(
        !saw_raw_first,
        "the second hole's answerer context must NOT still carry the raw first-hole \
         exchange (`apple`) — it was compacted away in place"
    );
}
