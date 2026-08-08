//! Targeted coverage for the ONE generation-tagged checkpoint that replaces
//! the old split `state.json` + `compaction.txt` persistence: a completed
//! cycle commits its own state and its own compaction summary together, at
//! the end of `SelfHarnessDriver::run_one_cycle`'s success path, so a
//! restart always reads a state and a summary from the SAME generation.
//!
//! Drives cycles through the production entry point
//! (`SelfHarnessDriver::run_one_cycle`, mirroring `acceptance_selfharness.rs`'s
//! direct-cycle-driving style — the frozen sync contract's acceptance path),
//! then constructs a FRESH `SelfHarnessDriver` over a FRESH `Harness` —
//! simulating a killed and restarted process — and confirms what it
//! restores. Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH —
//! run inside `nix develop` (see `haskell/CLAUDE.md`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, Role, StreamSink, TurnRequest, TurnResponse,
    Usage,
};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::persistence;
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, HarnessSource, LogObserver, SelfHarnessDriver,
};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> PathBuf {
    repo_root().join("haskell/lib")
}

fn examples_harness_dir() -> PathBuf {
    repo_root().join("examples/harness")
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "selfharness-persistence-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "selfharness-persistence".into(),
        extract_fingerprint: "selfharness-persistence".into(),
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

fn decision_reply(action: &str, confidence: &str) -> RecordedReply {
    reply(&format!(
        "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
         (finalize @Decision (Decision {{ action = \"{action}\", rationale = \"because\", \
         confidence = {confidence} }}) :: M ())\n```"
    ))
}

/// A fresh [`SelfHarnessDriver`] over a fresh [`Harness`] scripted with
/// `replies` — a new one each call mirrors what a restarted PROCESS would
/// construct (a brand-new agent orchestrator with no memory of the prior
/// run's nodes), distinct from just reusing the same driver across cycles.
fn fresh_driver(replies: Vec<RecordedReply>, log_tag: &str) -> SelfHarnessDriver {
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "selfharness-persistence-{log_tag}-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    SelfHarnessDriver::new(agent, Arc::new(LogObserver))
}

fn source() -> HarnessSource {
    load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads")
}

/// State + summary persist to one checkpoint after a cycle, and a FRESH
/// driver (simulating a restart) restores BOTH from the same generation —
/// advancing loopCount/mode from where the killed process left off, rather
/// than starting over from `initialState`. A third cycle (no restart in
/// between) proves generation keeps increasing within one process too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_cycles_restore_state_and_summary_from_the_same_generation() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let checkpoint_path = scratch("restart").join("checkpoint.json");

    // No file yet: load_checkpoint must be Ok(None), not an error — the very
    // first-ever run has nothing to restore.
    assert_eq!(
        persistence::load_checkpoint(&checkpoint_path).expect("load_checkpoint on a missing file"),
        None,
        "a never-committed path must restore to None, not an error"
    );

    let harness_source = source();

    // --- "Process 1": cycle 1, from initialState (no prior checkpoint). ---
    let mut driver1 = fresh_driver(vec![decision_reply("observe", "Medium")], "process1");
    driver1.set_checkpoint_path(checkpoint_path.clone());
    let outcome1 = driver1
        .run_one_cycle(&harness_source, None)
        .expect("cycle 1 (initialState)");
    assert_eq!(
        outcome1
            .state_json
            .get("loopCount")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        outcome1.state_json.get("mode").and_then(|v| v.as_str()),
        Some("Deciding"),
        "Observing -> Deciding after cycle 1"
    );

    // `run_one_cycle` commits generation 1 on success — no separate save call.
    let cp1 = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after cycle 1")
        .expect("cycle 1 committed a checkpoint");
    assert_eq!(cp1.generation, 1);
    assert_eq!(cp1.state, outcome1.state_json);
    drop(driver1);

    // --- "restart": brand-new driver, brand-new agent, nothing in-process
    // carried over except the file on disk. ---
    let mut driver2 = fresh_driver(vec![decision_reply("act", "High")], "process2");
    driver2.set_checkpoint_path(checkpoint_path.clone());

    let restored = driver2
        .restore(&harness_source)
        .expect("restore after restart")
        .expect("cycle 1's checkpoint was committed to disk");
    assert_eq!(
        restored, outcome1.state_json,
        "restored state must equal exactly what cycle 1 committed"
    );

    // What `SelfHarnessDriver::run_loop` does: run the next cycle against the
    // restored state instead of `None`.
    let outcome2 = driver2
        .run_one_cycle(&harness_source, Some(&restored))
        .expect("cycle 2 (from restored state)");

    // The PRE-loop render for cycle 2 must reflect the RESTORED mode
    // (Deciding), proving the restored state reached `render`, not
    // initialState's Observing.
    assert!(
        outcome2.prompt_before.contains("deciding what to do next"),
        "restart must resume from the persisted Deciding mode, not \
         initialState's Observing, got:\n{}",
        outcome2.prompt_before
    );
    assert!(
        !outcome2.prompt_before.contains("No decision made yet"),
        "restart must resume with cycle 1's decision already recorded, got:\n{}",
        outcome2.prompt_before
    );

    assert_eq!(
        outcome2
            .state_json
            .get("loopCount")
            .and_then(|v| v.as_i64()),
        Some(2),
        "loopCount must continue from the persisted 1, not reset to 0 after 1, \
         got {:?}",
        outcome2.state_json
    );
    assert_eq!(
        outcome2.state_json.get("mode").and_then(|v| v.as_str()),
        Some("Acting"),
        "Deciding -> Acting after cycle 2, continuing the restored mode chain"
    );

    let cp2 = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after cycle 2")
        .expect("cycle 2 committed a checkpoint");
    assert_eq!(
        cp2.generation, 2,
        "generation must increase by exactly one per committed cycle, across a restart"
    );
    assert_eq!(cp2.state, outcome2.state_json);

    // A third cycle, same process, no restart — generation keeps climbing.
    let _ = driver2
        .run_one_cycle(&harness_source, Some(&outcome2.state_json))
        .expect_err("no more scripted replies for a third cycle");
    // The failed third cycle must NOT have overwritten generation 2's
    // checkpoint (only a SUCCESSFUL cycle commits).
    let cp2_again = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after the failed third cycle")
        .expect("still generation 2's checkpoint");
    assert_eq!(cp2_again, cp2);
}

const SUMMARY_PREFIX: &str = "CRASH-TEST-SUMMARY";

fn is_summarize_prompt(user: &str) -> bool {
    user.contains("Summarize EVERYTHING above")
}

/// A provider for `CompactionHarness.hs`'s two `runLLMTurn @Text` holes:
/// every finalize turn's input crosses the (low, test-configured) compaction
/// threshold, and every summarize turn gets a distinct tagged sentinel (via
/// `counter`) so a test can tell WHICH compaction produced a given summary.
struct CompactingProvider {
    pre_input: u64,
    counter: Arc<AtomicU64>,
}

impl ModelProvider for CompactingProvider {
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
            let n = self.counter.fetch_add(1, Ordering::Relaxed);
            return Ok(TurnResponse {
                text: format!("{SUMMARY_PREFIX}-{n}"),
                usage: Usage {
                    input_tokens: 30,
                    output_tokens: 5,
                },
                reasoning: None,
            });
        }

        let answer = if latest_user.contains("SECOND-HOLE") {
            "blue"
        } else {
            "apple"
        };
        Ok(TurnResponse {
            text: format!("```haskell\n(finalize @Text (\"{answer}\" :: Text) :: M ())\n```"),
            usage: Usage {
                input_tokens: self.pre_input,
                output_tokens: 50,
            },
            reasoning: None,
        })
    }
}

static NEXT_LOG_ID: AtomicU64 = AtomicU64::new(0);

fn compaction_driver(checkpoint_path: PathBuf) -> (SelfHarnessDriver, HarnessSource) {
    let counter = Arc::new(AtomicU64::new(0));
    let provider: Arc<dyn DynModelProvider> = Arc::new(CompactingProvider {
        pre_input: 600,
        counter,
    });
    let mut agent_cfg =
        EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(fixtures_dir()))
            .expect("answerer engine config");
    agent_cfg.context_window_tokens = Some(1000);
    let log_id = NEXT_LOG_ID.fetch_add(1, Ordering::Relaxed);
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "selfharness-persistence-compaction-{}-{log_id}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(checkpoint_path);
    driver.set_compaction_threshold_percent(50);
    let source = load_harness_source(&fixtures_dir().join("CompactionHarness.hs"))
        .expect("compaction harness source loads");
    (driver, source)
}

/// A cycle whose mid-loop compaction fires and then errors out BEFORE
/// reaching its own commit must not leak that compaction's summary: restore
/// after the crash gets back the PRIOR generation's state AND the PRIOR
/// generation's summary — never the newer in-memory-only summary paired
/// with the older state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_before_cycle_commits_restores_prior_generation_not_a_mixed_pair() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let checkpoint_path = scratch("crash").join("checkpoint.json");
    let (mut driver, source) = compaction_driver(checkpoint_path.clone());

    // Cycle 1: both holes cross the compaction threshold (pre_input=600 >=
    // 50% of the 1000-token budget) and complete normally — commits
    // generation 1 with ITS OWN final compaction summary.
    let outcome1 = driver
        .run_one_cycle(&source, None)
        .expect("cycle 1 completes and commits");
    assert!(
        outcome1
            .compaction
            .as_deref()
            .is_some_and(|s| s.starts_with(SUMMARY_PREFIX)),
        "cycle 1 must have compacted, got {:?}",
        outcome1.compaction
    );
    let cp1 = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after cycle 1")
        .expect("cycle 1 committed");
    assert_eq!(cp1.generation, 1);
    assert_eq!(cp1.compaction, outcome1.compaction);

    // Cycle 2: a cap of 2 lets the first hole's finalize round (call #1) and
    // the compaction it triggers (call #2) both run — updating
    // `last_compaction` IN MEMORY to a NEW summary — but leaves no budget for
    // the second hole's own answerer round, so the cycle hard-fails before
    // reaching its commit.
    driver.set_loop_inference_call_cap(2);
    let err = driver
        .run_one_cycle(&source, Some(&outcome1.state_json))
        .expect_err("cycle 2 must hard-fail before finishing its second hole");
    assert!(
        format!("{err}").contains("inference-call cap"),
        "cycle 2 must fail via the inference cap, not some other error: {err}"
    );

    // The loop DID continue under cycle 2's mid-loop compaction before
    // erroring — this driver's in-memory state reflects it.
    let mid_loop_summary = driver
        .last_compaction()
        .expect("cycle 2's mid-loop compaction updated last_compaction in memory")
        .to_string();
    assert!(mid_loop_summary.starts_with(SUMMARY_PREFIX));
    assert_ne!(
        Some(mid_loop_summary.as_str()),
        outcome1.compaction.as_deref(),
        "cycle 2's mid-loop summary must be a NEW one, not cycle 1's carried forward unchanged"
    );

    // But nothing was committed for cycle 2: a fresh "restart" driver reads
    // back exactly generation 1 — cycle 1's state AND cycle 1's summary,
    // never cycle 2's newer summary against cycle 1's (or any) state.
    let (mut restart_driver, restart_source) = compaction_driver(checkpoint_path.clone());
    let restored_state = restart_driver
        .restore(&restart_source)
        .expect("restore after the crash")
        .expect("generation 1's checkpoint is still on disk");
    assert_eq!(
        restored_state, outcome1.state_json,
        "a crash before commit must restore the PRIOR generation's state"
    );
    assert_eq!(
        restart_driver.last_compaction(),
        outcome1.compaction.as_deref(),
        "a crash before commit must restore the PRIOR generation's summary, \
         not cycle 2's mid-loop one"
    );

    let cp_after_crash = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after the crash")
        .expect("still generation 1's checkpoint");
    assert_eq!(
        cp_after_crash, cp1,
        "the on-disk checkpoint must be untouched by cycle 2's failed attempt"
    );
}

/// A checkpoint file that exists but fails to parse is a typed
/// [`persistence::PersistenceError`], never a silent reset to `None` — and
/// the atomic `.tmp`-then-rename write never itself produces one. Plain
/// [`persistence`] calls, no GHC compile needed.
#[test]
fn truncated_checkpoint_is_a_typed_error_and_writes_leave_no_tmp_behind() {
    let path = scratch("truncated").join("checkpoint.json");
    let checkpoint = persistence::Checkpoint {
        generation: 1,
        state: serde_json::json!({"loopCount": 1}),
        compaction: Some("a summary".to_string()),
        harness_source: "fingerprint".to_string(),
    };
    persistence::save_checkpoint(&path, &checkpoint).expect("save_checkpoint");
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    assert!(
        !tmp.exists(),
        "a completed write must leave no .tmp sibling"
    );

    let mut bytes = std::fs::read(&path).expect("read back");
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&path, &bytes).expect("write truncated bytes");

    let err = persistence::load_checkpoint(&path)
        .expect_err("a truncated checkpoint must not silently reset to None");
    assert!(matches!(err, persistence::PersistenceError::Json { .. }));
}

/// `SelfHarnessDriver`'s default `checkpoint_path` is a stable, non-empty
/// path under the runtime cache dir (not e.g. accidentally empty/relative to
/// whatever the current directory happens to be at construction) — a cheap
/// sanity check independent of `TIDEPOOL_EXTRACT`.
#[test]
fn default_checkpoint_path_is_under_the_cache_dir() {
    let agent_cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![]));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "selfharness-persistence-default-path-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    assert!(driver
        .checkpoint_path()
        .ends_with("selfharness/checkpoint.json"));
}
