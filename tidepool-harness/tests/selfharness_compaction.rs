//! Acceptance coverage for the self-iterating harness driver's
//! runtime-owned MID-LOOP, IN-PLACE compaction (`plans/self-iterating-harness/
//! 02-runtime.md` Compaction):
//!
//! Compaction is ONE ordinary turn on the EXISTING answerer session (which
//! already holds the full context): the driver pushes a "summarize everything
//! above" User turn, captures the model's PLAIN-TEXT reply as the summary, then
//! resets that session's context to `[system + summary]`. No separate node, no
//! `finalize @Text`, no transcript serialized into a prompt, and no second
//! drive-to-finalize loop, by construction.
//!
//! These tests exercise:
//!   - the threshold measures the answerer's context as the LAST turn's
//!     `input_tokens` (a high-water mark), NOT a running SUM across rounds — a
//!     multi-round hole whose SUMMED input crosses the budget but whose LATEST
//!     input does not must NOT trip compaction.
//!   - the summarize turn's model call counts against the per-loop 1024
//!     inference-call cap.
//!   - the in-place relief property: the second hole runs under the summary,
//!     and the summary reaches the next render.
//!
//! (restart durability and jsonl payload have their own test files /
//! unit tests; see `selfharness_compaction_fixes.rs` and `persistence.rs`.)
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::{Arc, Mutex};

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, Message, ModelProvider, ProviderError, Role, StreamSink, TurnRequest,
    TurnResponse, Usage,
};
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
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

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "selfharness-compaction-{}-{}",
        std::process::id(),
        name
    ));
    // Clear any stale contents so a re-run's `LogWriter::create` (which refuses
    // an existing file) starts fresh rather than tripping AlreadyExists.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A summarize turn is recognizable by its prompt (see `driver.rs`
/// `maybe_compact_answerer`): it asks the model to "Summarize EVERYTHING above"
/// and to reply with the summary text directly. The scripted providers detect
/// that and reply with a plain-text summary.
fn is_summarize_prompt(user: &str) -> bool {
    user.contains("Summarize EVERYTHING above")
}

/// The plain-text summary the scripted providers reply with on a summarize
/// turn. A unique sentinel so a test can prove it reached the next render and
/// replaced the answerer's context in place.
const SUMMARY_SENTINEL: &str = "COMPACTED-SUMMARY-chose-a-fruit-first";

// ---------------------------------------------------------------------------
// Mechanism test: in-place relief + summary reaches the next render.
// ---------------------------------------------------------------------------

/// Provider for the in-place-relief test. First hole answers `apple` with a
/// large single-turn input (high-water crosses threshold); the summarize turn
/// replies with a plain-text summary; the second hole answers `blue` and
/// records whether the transcript it was handed carries the SUMMARY (in-place
/// relief) rather than the raw first-hole exchange (`apple`).
struct InPlaceProbeProvider {
    /// `Some((saw_summary, saw_raw_first))` once the second hole is serviced.
    second_hole: Arc<Mutex<Option<(bool, bool)>>>,
}

impl ModelProvider for InPlaceProbeProvider {
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

        // The summarize turn: reply with the plain-text summary (no code block).
        if is_summarize_prompt(&latest_user) {
            return Ok(TurnResponse {
                text: SUMMARY_SENTINEL.to_string(),
                usage: Usage {
                    input_tokens: 20,
                    output_tokens: 5,
                },
                reasoning: None,
                reasoning_items: Vec::new(),
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
            // The raw first-hole answer ("apple") must be GONE once the context
            // has been compacted in place.
            let saw_raw_first = joined.contains("apple");
            *self.second_hole.lock().unwrap() = Some((saw_summary, saw_raw_first));
        }

        let answer = if is_second { "blue" } else { "apple" };
        // Large single-turn input so the FIRST hole's LAST-turn input_tokens
        // (the high-water measure) alone crosses the (low) threshold.
        Ok(TurnResponse {
            text: format!("```haskell\n(finalize @Text (\"{answer}\" :: Text) :: M ())\n```"),
            usage: Usage {
                input_tokens: 600,
                output_tokens: 50,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

/// Mid-loop, in-place compaction on the SIMPLE mechanism: the answerer's
/// last-turn input crosses ~80% after the FIRST hole; the runtime pushes ONE
/// summarize turn onto the same answerer, resets its context to the summary,
/// and the SECOND hole runs under it. The summary reaches the next render.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_fires_mid_loop_in_place_and_reaches_next_render() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let second_hole = Arc::new(Mutex::new(None));
    let provider: Arc<dyn DynModelProvider> = Arc::new(InPlaceProbeProvider {
        second_hole: second_hole.clone(),
    });

    let mut agent_cfg =
        EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(fixtures_dir()))
            .expect("answerer engine config");
    agent_cfg.context_window_tokens = Some(1000);

    let writer =
        tidepool_harness::log::LogWriter::create(scratch("inplace").join("log.jsonl"), &header())
            .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(scratch("inplace").join("checkpoint.json"));
    // 50% of 1000 = 500: the first hole's 600-token LAST-turn input trips
    // compaction between the holes, deterministically.
    driver.set_compaction_threshold_percent(50);
    let source = load_harness_source(&fixtures_dir().join("CompactionHarness.hs"))
        .expect("compaction harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one two-hole cycle with a mid-loop compaction");

    // (1) The mid-loop compaction produced its Text.
    let compaction = outcome
        .compaction
        .as_deref()
        .expect("last-turn input past the (lowered) threshold must force a mid-loop compaction");
    assert!(
        compaction.contains(SUMMARY_SENTINEL),
        "the compaction Text must be the summarize turn's plain-text reply, got: {compaction:?}"
    );

    // Both holes ran to completion — the loop CONTINUED, no abort.
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
    // SUMMARY and NOT the raw first-hole exchange.
    let (saw_summary, saw_raw_first) = second_hole
        .lock()
        .unwrap()
        .expect("the second hole must have been serviced");
    assert!(
        saw_summary,
        "the second hole's answerer context must carry the compaction summary"
    );
    assert!(
        !saw_raw_first,
        "the second hole's answerer context must NOT still carry the raw first-hole \
         exchange (`apple`) — it was compacted away in place"
    );
}

// ---------------------------------------------------------------------------
// High-water measure, not a running sum, across a MULTI-ROUND hole.
// ---------------------------------------------------------------------------

/// Provider that drives the FIRST hole across THREE rounds before finalizing,
/// each round with a MODEST last-turn input (`per_round_input`), and answers a
/// summarize turn (should never be asked here) with a sentinel. Records whether
/// a summarize turn was ever requested.
struct MultiRoundProvider {
    per_round_input: u64,
    saw_summarize: Arc<Mutex<bool>>,
    first_hole_rounds: Arc<Mutex<u32>>,
}

impl ModelProvider for MultiRoundProvider {
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

        if is_summarize_prompt(&latest_user) {
            *self.saw_summarize.lock().unwrap() = true;
            return Ok(TurnResponse {
                text: "UNEXPECTED-SUMMARY".to_string(),
                usage: Usage {
                    input_tokens: self.per_round_input,
                    output_tokens: 5,
                },
                reasoning: None,
                reasoning_items: Vec::new(),
            });
        }

        let is_second = latest_user.contains("SECOND-HOLE");
        let usage = Usage {
            input_tokens: self.per_round_input,
            output_tokens: 20,
        };
        if is_second {
            // The second hole finalizes immediately.
            return Ok(TurnResponse {
                text: "```haskell\n(finalize @Text (\"blue\" :: Text) :: M ())\n```".to_string(),
                usage,
                reasoning: None,
                reasoning_items: Vec::new(),
            });
        }

        // FIRST hole: reply with a plain non-finalize turn for the first two
        // rounds (a wasted round → corrective re-prompt), then finalize on the
        // third. Every round's input is `per_round_input` — the SUMMED input
        // across the three rounds far exceeds it, but the LATEST (high-water)
        // is only `per_round_input`.
        let mut rounds = self.first_hole_rounds.lock().unwrap();
        *rounds += 1;
        let this_round = *rounds;
        drop(rounds);
        if this_round < 3 {
            // A plain-text (NoBlock) reply is a wasted round, re-prompted toward
            // finalize by the driver — it does not resolve the hole.
            Ok(TurnResponse {
                text: "thinking about fruit...".to_string(),
                usage,
                reasoning: None,
                reasoning_items: Vec::new(),
            })
        } else {
            Ok(TurnResponse {
                text: "```haskell\n(finalize @Text (\"apple\" :: Text) :: M ())\n```".to_string(),
                usage,
                reasoning: None,
                reasoning_items: Vec::new(),
            })
        }
    }
}

/// Across a THREE-round first hole, each round's input is 300 tokens. A
/// summed-usage measure would see ~900 input and trip a 500-token
/// threshold; the high-water measure sees only the latest 300 and must
/// NOT compact — asserts the threshold reads the last-turn input, not the sum.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c1_multiround_highwater_does_not_overcount() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let saw_summarize = Arc::new(Mutex::new(false));
    let provider: Arc<dyn DynModelProvider> = Arc::new(MultiRoundProvider {
        per_round_input: 300,
        saw_summarize: saw_summarize.clone(),
        first_hole_rounds: Arc::new(Mutex::new(0)),
    });

    let mut agent_cfg =
        EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(fixtures_dir()))
            .expect("answerer engine config");
    agent_cfg.context_window_tokens = Some(1000);

    let writer =
        tidepool_harness::log::LogWriter::create(scratch("c1").join("log.jsonl"), &header())
            .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(scratch("c1").join("checkpoint.json"));
    // 50% of 1000 = 500. Summed 3×300 = 900 > 500 (would over-trip); the
    // high-water 300 < 500 (must NOT trip).
    driver.set_compaction_threshold_percent(50);
    // Allow up to 6 rounds/hole so the 3-round first hole is not itself capped.
    driver.set_answerer_round_caps(5, 6);
    let source = load_harness_source(&fixtures_dir().join("CompactionHarness.hs"))
        .expect("compaction harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("two-hole cycle, multi-round first hole, NO compaction");

    assert!(
        !*saw_summarize.lock().unwrap(),
        "no summarize turn may fire: the high-water input (300) is below the 500 threshold — \
         the OLD summed measure (~900) would have wrongly tripped it"
    );
    assert!(
        outcome.compaction.is_none(),
        "a multi-round hole whose SUMMED input crosses the budget but whose LATEST input \
         does not must NOT compact, got: {:?}",
        outcome.compaction
    );
    // The loop still completed correctly.
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
        vec!["apple", "blue"]
    );
}
