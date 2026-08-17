//! Durability across a restart, in the two things a killed run leaves behind:
//! the ONE generation-tagged CHECKPOINT, and — since PRD 20 S1-L5 — the run
//! LEASE plus its append-only JOURNAL.
//!
//! Checkpoint half: a completed cycle commits its own state and its own
//! compaction summary together, at the end of
//! `SelfHarnessDriver::run_one_cycle`'s success path, so a restart always
//! reads a state and a summary from the SAME generation.
//!
//! Lease/journal half (wave 2b/3, `plans/self-iterating-harness/
//! 20-s1-l5-resume.md`): a run that ends ABNORMALLY does not retire its lease,
//! so the next boot resumes it and does only the delta; a run that ends
//! normally does retire, so the next boot mints a fresh run. A resumed
//! process always owns its OWN freshly-allocated journal SEGMENT — never one
//! a prior process wrote to (wave 3) — which is what makes a kill mid-append
//! (a torn final line) survivable across arbitrarily many further boots
//! rather than one.
//!
//! Every cycle runs through the production entry point
//! (`SelfHarnessDriver::run_one_cycle`), and a "restarted process" is always a
//! FRESH `SelfHarnessDriver` over a FRESH `Harness`, carrying nothing but what
//! is on disk. The GHC-heavy tests need `TIDEPOOL_EXTRACT` and the
//! with-packages GHC on PATH — run inside `nix develop` (see
//! `haskell/CLAUDE.md`). The lease/journal tests that fold hand-written
//! segments compile no Haskell at all and are plain `#[test]`s.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod support;

use tidepool_handlers::{load_journal, ConsoleHandler, JournalEntry, JournalLoadError};
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
    acquire_lease, answerer_decls, load_harness_source, retire_lease, DriverError, Event, Harness,
    HarnessSource, LogObserver, Observer, ResumeFold, RunLease, SelfHarnessDriver,
};

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
            cached_input_tokens: None,
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
    fresh_driver_with_observer(replies, log_tag, Arc::new(LogObserver))
}

/// Same construction as [`fresh_driver`], but with a caller-supplied
/// [`Observer`] — the addendum's fingerprint-mismatch test needs to capture
/// [`Event::HarnessSourceChanged`], which the fixed [`LogObserver`] only logs.
fn fresh_driver_with_observer(
    replies: Vec<RecordedReply>,
    log_tag: &str,
    observer: Arc<dyn Observer>,
) -> SelfHarnessDriver {
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
    SelfHarnessDriver::new(agent, observer)
}

fn source() -> HarnessSource {
    load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads")
}

/// State + summary + iteration count persist to one checkpoint after a
/// cycle, and a FRESH driver (simulating a restart) restores ALL THREE from
/// the same generation — advancing mode from where the killed process left
/// off, rather than starting over from `initialState`, AND resuming the
/// loop-iteration count at the right number rather than resetting to 0 (the
/// iteration count lives in the checkpoint ENVELOPE, never in `State`
/// itself — `plans/self-iterating-harness/15-generic-surface-wave.md`,
/// "Runtime context is the runtime's job"). A third cycle (no restart in
/// between) asserts generation keeps increasing within one process too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_cycles_restore_state_and_summary_from_the_same_generation() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

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
        .await
        .expect("cycle 1 (initialState)");
    assert_eq!(
        driver1.iteration(),
        1,
        "the driver's iteration count must advance after cycle 1"
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
    assert_eq!(
        cp1.iteration, 1,
        "the checkpoint envelope must carry cycle 1's iteration count"
    );
    drop(driver1);

    // --- "restart": brand-new driver, brand-new agent, nothing in-process
    // carried over except the file on disk. ---
    let mut driver2 = fresh_driver(vec![decision_reply("act", "High")], "process2");
    driver2.set_checkpoint_path(checkpoint_path.clone());

    let restored = driver2
        .restore(&harness_source)
        .await
        .expect("restore after restart")
        .expect("cycle 1's checkpoint was committed to disk");
    assert_eq!(
        restored, outcome1.state_json,
        "restored state must equal exactly what cycle 1 committed"
    );
    // THE round-trip: `restore` alone (no cycle run yet) must resume the
    // iteration count at exactly what cycle 1 committed, not reset it to 0 —
    // the count lives in the checkpoint envelope, restored independently of
    // `State`.
    assert_eq!(
        driver2.iteration(),
        1,
        "a restored driver must resume counting from the persisted iteration, not 0"
    );

    // What `SelfHarnessDriver::run_loop` does: run the next cycle against the
    // restored state instead of `None`.
    let outcome2 = driver2
        .run_one_cycle(&harness_source, Some(&restored))
        .await
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
        driver2.iteration(),
        2,
        "the iteration count must continue from the restored 1, not reset to 0"
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
    assert_eq!(
        cp2.iteration, 2,
        "the checkpoint envelope must carry the continued iteration count across a restart"
    );

    // A third cycle, same process, no restart — generation keeps climbing.
    let _ = driver2
        .run_one_cycle(&harness_source, Some(&outcome2.state_json))
        .await
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
                    cached_input_tokens: None,
                },
                reasoning: None,
                reasoning_items: Vec::new(),
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
                cached_input_tokens: None,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
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
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let checkpoint_path = scratch("crash").join("checkpoint.json");
    let (mut driver, source) = compaction_driver(checkpoint_path.clone());

    // Cycle 1: both holes cross the compaction threshold (pre_input=600 >=
    // 50% of the 1000-token budget) and complete normally — commits
    // generation 1 with ITS OWN final compaction summary.
    let outcome1 = driver
        .run_one_cycle(&source, None)
        .await
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
        .await
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
        .await
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
        iteration: 1,
        state: serde_json::json!({"mode": "Deciding"}),
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
    let _cache_guard = support::isolate_cache();
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

/// Captures every [`Event::HarnessSourceChanged`] the driver emits — the
/// stale-checkpoint tests below need to assert the event still fires even
/// though the mismatched checkpoint's state is now discarded rather than
/// restored.
#[derive(Default)]
struct FingerprintChangeObserver {
    changes: Mutex<Vec<(String, String)>>,
}

impl Observer for FingerprintChangeObserver {
    fn on_event(&self, event: &Event) {
        if let Event::HarnessSourceChanged {
            restored_fingerprint,
            current_fingerprint,
        } = event
        {
            self.changes
                .lock()
                .unwrap()
                .push((restored_fingerprint.clone(), current_fingerprint.clone()));
        }
    }
}

/// Addendum (stale-checkpoint boot crash), REVISED to carry-forward
/// (2026-08-15, with the operator): a checkpoint committed by a DIFFERENT
/// harness source is now CARRIED into the first cycle rather than
/// discarded — the original crash this branch once guarded against (a
/// shape-incompatible decode killing the process on boot) is absorbed by
/// `run_loop`'s `DriverError::StateDecode` retry instead, so a
/// shape-COMPATIBLE harness edit (a prompt tweak) keeps its accumulated
/// state. Pinned here with the original incident's real fixture: `restore`
/// returns the stale state, the cycle against it fails as `StateDecode`
/// (not a crash), the fallback cycle from `initialState` succeeds, the
/// event still fires as the durable record, the compaction summary still
/// drops, and the generation still continues monotonically.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_fingerprint_state_carries_forward_and_falls_back_on_decode_failure() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let checkpoint_path = scratch("stale-fp").join("checkpoint.json");
    std::fs::copy(
        fixtures_dir().join("checkpoint-stale-fingerprint.json"),
        &checkpoint_path,
    )
    .expect("plant the real crashed-run fixture (from the live dogfood incident)");

    let observer = Arc::new(FingerprintChangeObserver::default());
    let mut driver = fresh_driver_with_observer(
        vec![decision_reply("observe", "Medium")],
        "stale-fp",
        observer.clone(),
    );
    driver.set_checkpoint_path(checkpoint_path.clone());

    // `restore` only ever compares `HarnessSource::fingerprint` against the
    // checkpoint's `harness_source`, nothing else about the source — so
    // overriding the fingerprint on a clone pins the mismatch by
    // construction, rather than trusting the reference harness's real
    // content-hash to differ from the fixture's stale one.
    let mut current_source = source();
    current_source.fingerprint = "00000000deadbeef".to_string();
    assert_ne!(current_source.fingerprint, "fcbd20d2594c4426");

    let restored = driver
        .restore(&current_source)
        .await
        .expect("restore must not error on a mismatched fingerprint");
    assert!(
        restored.is_some(),
        "a fingerprint-mismatched checkpoint's state must CARRY FORWARD for the decode \
         attempt, not be discarded"
    );
    assert_eq!(
        driver.last_compaction(),
        None,
        "a source-changed checkpoint's compaction summary must not carry over — it \
         describes a different harness's loop"
    );
    assert_eq!(
        observer.changes.lock().unwrap().as_slice(),
        &[(
            "fcbd20d2594c4426".to_string(),
            current_source.fingerprint.clone()
        )],
        "HarnessSourceChanged must still fire, carrying both fingerprints, as the \
         durable record of the carry-forward"
    );

    // The fixture's state is SHAPE-COMPATIBLE with the reference harness
    // (the original incident predated shape drift), so the carried state
    // must simply WORK: the cycle runs on it — this is the whole point of
    // carry-forward, a compatible harness edit keeps accumulated state.
    // (The incompatible-shape path is pinned by the decode-retry test
    // below, which plants a state that cannot decode.)
    let _outcome = driver
        .run_one_cycle(&current_source, restored.as_ref())
        .await
        .expect("a shape-compatible carried state must run, not be discarded");
    assert_eq!(
        driver.iteration(),
        2,
        "the carried state's loop history continues (fixture iteration 1 + this \
         cycle), rather than restarting at 1 as a discard would"
    );
    let committed = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after the fresh cycle")
        .expect("the fresh cycle committed its own checkpoint");
    assert_eq!(
        committed.generation, 2,
        "generation must continue from the carried checkpoint's generation (1), not \
         reset to 1"
    );
}

/// Addendum (stale-checkpoint boot crash), defect 2: defense in depth for
/// whatever the fingerprint check in the test above misses (a hash
/// collision, a hand-edited checkpoint, a same-source edit that changes the
/// `State` type without changing the file's fingerprint). A restored `State`
/// that fails the author's `FromJSON State` decode must not take the whole
/// process down via `run_loop` — it retries the cycle exactly once from
/// fresh `initialState` instead. Proven by planting a checkpoint whose
/// `harness_source` MATCHES the current source (so defect 1's discard does
/// NOT fire) but whose `state` cannot possibly decode against the reference
/// harness's real `State` type, then observing that `run_loop` still
/// produces a real committed cycle from fresh state before it eventually
/// runs out of scripted replies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_decode_failure_retries_once_from_fresh_state_instead_of_killing_run_loop() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let checkpoint_path = scratch("decode-retry").join("checkpoint.json");
    let current_source = source();
    persistence::save_checkpoint(
        &checkpoint_path,
        &persistence::Checkpoint {
            generation: 1,
            iteration: 1,
            // Cannot decode against the reference harness's real `State`
            // (which requires `lastDecision`/`mode`/`notes`) —
            // same shape of failure as a hand-edited or cross-version
            // checkpoint that slips past the fingerprint check.
            state: serde_json::json!({"totally": "not a State"}),
            compaction: None,
            harness_source: current_source.fingerprint.clone(),
        },
    )
    .expect("plant a same-fingerprint, undecodable checkpoint");

    // Exactly one scripted reply: enough for the RETRIED cycle (fresh
    // initialState) to finalize; the loop's second cycle then finds the
    // replay queue empty and run_loop returns a non-StateDecode error,
    // ending the test deterministically without an artificial cap.
    let mut driver = fresh_driver(vec![decision_reply("observe", "Medium")], "decode-retry");
    driver.set_checkpoint_path(checkpoint_path.clone());

    let err = driver
        .run_loop(&current_source, true)
        .await
        .expect_err("the replay queue runs out on the second cycle");
    assert!(
        !matches!(err, DriverError::StateDecode(_)),
        "the decode failure on cycle 1's restored state must have been retried away, \
         not have propagated out of run_loop as StateDecode, got: {err:?}"
    );

    // The retried cycle (fresh initialState, loopCount 1) must have actually
    // run and committed — proving `run_loop` recovered rather than dying
    // silently on the first cycle.
    let committed = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after the retried cycle")
        .expect("the retried cycle committed its own checkpoint");
    assert_eq!(
        committed.generation, 2,
        "the retried cycle must commit generation 2, continuing from the planted \
         checkpoint's generation 1"
    );
    // Unlike the fingerprint-DISCARD case (where the stale harness's loop
    // history is thrown away with its state), a same-fingerprint decode
    // retry is the SAME harness: the envelope's cycles-completed count
    // legitimately continues (planted 1 -> this cycle is 2). What proves the
    // retry started from fresh initialState is the committed STATE: a real
    // `State` (its `mode` key present), not an echo of the planted garbage.
    assert_eq!(
        committed.iteration, 1,
        "the fallback cycle runs as iteration 1 — the discarded state's loop \
         history goes with it (run_loop's retry arm zeroes the count)"
    );
    assert!(
        committed.state.get("mode").is_some(),
        "the retried cycle must commit a real State, got {:?}",
        committed.state
    );
    assert_eq!(committed.state.get("totally"), None);
}

// ============================================================================
// PRD 20 S1-L5 wave 2b — the run lease and its journal under ABNORMAL
// termination
//
// Wave 1's acceptance (`tests/outer_effects.rs::
// resume_boot_fold_fresh_then_resumed_appends_only_the_delta`) proves ENTRY
// SELECTION over a clean journal: a fresh boot compiles `loop`, a boot with a
// non-empty fold compiles `resumeLoop` and appends only the delta, and a
// non-empty fold against a harness with no `resumeLoop` is refused. It gets its
// "prior process" by running a `loop` that deliberately covers less work than
// `resumeLoop` does, and its refusal leg from a hand-staged journal — no run in
// it ever terminates abnormally.
//
// What follows is the crash path itself: that an abnormally-terminated cycle
// leaves its lease unretired (and a normally-finished one does not), that its
// half-written journal folds correctly, and that the resumed run's work is
// exactly the remainder.
// ============================================================================

/// A fresh [`SelfHarnessDriver`] over `tests/fixtures` — one per simulated
/// PROCESS below. No scripted replies: `CrashResumeHarness.hs`'s loop is
/// authored orchestration and opens no model holes, so the provider exists only
/// because the constructor takes one.
fn fixture_driver(log_tag: &str) -> SelfHarnessDriver {
    let agent_cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(fixtures_dir()))
        .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
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

/// A `state_json` array field as `Vec<&str>`, for a plain assertion against
/// `vec!["alpha", "beta"]`-shaped expectations.
fn text_array<'a>(state: &'a serde_json::Value, field: &str) -> Vec<&'a str> {
    state
        .get(field)
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("{field} must be an array, got {state:?}"))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("{field} entry must be a string"))
        })
        .collect()
}

fn keys(entries: &[JournalEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.key.as_str()).collect()
}

/// Every entry a run id owns, across every segment it has ever written, in
/// segment order — the run's TRUE PHYSICAL WRITE ORDER (see
/// `selfharness::resume`'s module doc). What a test reaches for whenever it
/// wants "the whole run's journal", now that no single file holds it.
fn load_run_entries(log_dir: &std::path::Path, run_id: &str) -> Vec<JournalEntry> {
    let mut entries = Vec::new();
    for segment in tidepool_harness::list_segments(log_dir, run_id).expect("list segments") {
        entries.extend(load_journal(&segment).expect("segment loads"));
    }
    entries
}

fn seqs(entries: &[JournalEntry]) -> Vec<u64> {
    entries.iter().map(|e| e.seq).collect()
}

/// The keys a fold kept, read back off the WIRE shape the splice carries — so
/// the assertion is about what the authored side would actually see.
fn folded_keys(fold: &ResumeFold) -> Vec<String> {
    fold.to_json()["entries"]
        .as_array()
        .expect("entries is a list")
        .iter()
        .map(|e| e["key"].as_str().expect("key").to_string())
        .collect()
}

/// One journal line exactly as [`tidepool_handlers::JournalHandler`] writes it,
/// newline included. Hand-written rather than recorded through the handler
/// because `JournalHandler::append` is private to its crate — and because the
/// tests below need to plant torn bytes, which no handler can produce on
/// purpose.
fn line(seq: u64, key: &str, payload: i64) -> String {
    let mut s =
        serde_json::json!({"seq": seq, "kind": "step", "key": key, "payload": payload}).to_string();
    s.push('\n');
    s
}

/// A kill landing mid-`write_all`: the kernel took a PREFIX of the line's bytes
/// and, therefore, no newline. That is the only shape a torn line can have —
/// the handler hands `write_all` ONE buffer that ends in `\n`, so a partial
/// write is always a prefix of it.
fn torn_prefix_of(full_line: &str) -> &str {
    &full_line[..full_line.len() / 2]
}

/// THE crash-resume acceptance test: a run killed mid-flight, resumed, doing
/// only the delta — and the retire/resume asymmetry that decides which of those
/// two things the next boot does.
///
/// **The crash mechanism, stated plainly.** No process is killed. The first
/// cycle dies at `CrashResumeHarness.hs`'s crash seam: after recording
/// `alpha` and `beta`, the loop calls `say`, and this driver has no Console
/// handler wired, so `run_one_cycle` returns `Err` mid-cycle with `gamma` and
/// `delta` still to do. **Why that is equivalent to a kill at the durability
/// boundary:** durability is decided entirely by what is on disk when the
/// process stops, and the three things on disk are identical either way —
/// two durably fsynced journal lines (`record` fsyncs before returning), NO
/// committed checkpoint (only a successful cycle commits), and an ACTIVE run lease
/// (retirement happens on a normal `run_loop` return, which never happens
/// here). Nothing about the boot path reads a pid, an exit status, or a "was
/// this clean" flag — `acquire_lease` resumes on the mere presence of the lease
/// file — so a real `kill -9` between two appends reaches the same next boot
/// through the same state. What this does NOT simulate is a kill DURING an
/// append; that leaves a torn final line, covered by the three tests below.
///
/// Asserted, in order:
/// 1. The crashed cycle's durable residue: two journal entries at seq 0/1, no
///    checkpoint.
/// 2. The next boot RESUMES (the lease was never retired), with the same run id
///    and the same journal file, folding both entries.
/// 3. `resumeLoop` skips exactly `alpha`/`beta` and records exactly
///    `gamma`/`delta`, whose seqs continue at 2/3 rather than restarting at 0.
/// 4. That cycle finishes normally, commits its checkpoint, and its lease
///    retires — RETAINED on disk (renamed, readable, naming the crashed run's
///    id), never deleted.
/// 5. The boot after that MINTS: a new run id, a fresh segment that does not
///    exist yet and folds to nothing, while the finished run's segments still
///    hold all four entries. Retire-on-normal-exit and resume-on-crash are
///    the two directions of one mechanism, so both are pinned here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashed_cycle_keeps_its_lease_and_the_resumed_run_does_only_the_delta() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let log_dir = scratch("crash-delta");
    let checkpoint_path = log_dir.join("checkpoint.json");
    let source = load_harness_source(&fixtures_dir().join("CrashResumeHarness.hs"))
        .expect("crash-resume fixture harness loads");

    // --- process 1: dies at the crash seam, two records already flushed -----
    let mut crashed = fixture_driver("crash-delta-1");
    crashed.set_checkpoint_path(checkpoint_path.clone());
    let first = acquire_lease(&log_dir).expect("mint the lease");
    assert!(!first.resumed, "no lease on disk yet — this boot mints one");
    assert_eq!(
        crashed
            .open_run_journal(&log_dir, &first)
            .expect("open the (nonexistent) journal"),
        0,
        "a fresh run folds nothing"
    );
    // Console deliberately left unwired: that IS the crash seam.
    let err = crashed
        .run_one_cycle(&source, None)
        .await
        .expect_err("the cycle must die at the crash seam, mid-flight");
    assert!(
        err.to_string().contains("set_console_handler"),
        "the crash must be the intended one (the Console seam), not some other \
         failure that happens to abort the cycle: {err}"
    );
    drop(crashed);

    let crashed_entries = load_journal(&first.segment).expect("the crashed run's segment loads");
    assert_eq!(
        keys(&crashed_entries),
        vec!["alpha", "beta"],
        "the steps recorded before the crash must be durable, and no others, \
         got {crashed_entries:?}"
    );
    assert_eq!(seqs(&crashed_entries), vec![0, 1]);
    assert_eq!(
        persistence::load_checkpoint(&checkpoint_path).expect("load_checkpoint after the crash"),
        None,
        "a cycle that dies mid-flight commits no checkpoint — the journal is the \
         ONLY record that those two steps happened, which is why resume folds it"
    );

    // --- process 2: the next boot resumes the run the crash left open -------
    let second = acquire_lease(&log_dir).expect("the boot after the crash");
    assert!(
        second.resumed,
        "an abnormally-terminated run never retires its lease, so the next boot \
         must resume it rather than start a new run"
    );
    assert_eq!(second.lease.run_id, first.lease.run_id);
    assert_ne!(
        second.segment, first.segment,
        "the resumed process must own a FRESH segment — never append into the \
         segment the crashed process left, which is exactly what closes the \
         torn-tail hazard"
    );

    let mut resumed = fixture_driver("crash-delta-2");
    resumed.set_checkpoint_path(checkpoint_path.clone());
    // The seam the crashed process lacked — this process walks past the point
    // its predecessor died at.
    resumed.set_console_handler(ConsoleHandler);
    assert_eq!(
        resumed
            .open_run_journal(&log_dir, &second)
            .expect("fold the crashed run's segments"),
        2
    );

    let outcome = resumed
        .run_one_cycle(&source, None)
        .await
        .expect("the resumed cycle completes: resumeLoop walks every step against the fold");
    let state = &outcome.state_json;
    assert_eq!(
        state.get("sawResume").and_then(|v| v.as_bool()),
        Some(true),
        "the resumed boot must have entered through resumeLoop, got {state:?}"
    );
    assert_eq!(
        text_array(state, "skipped"),
        vec!["alpha", "beta"],
        "exactly the steps the crashed process durably recorded must be skipped, \
         got {state:?}"
    );
    assert_eq!(
        text_array(state, "recorded"),
        vec!["gamma", "delta"],
        "the resumed run's work must be exactly the remainder — no redo of \
         finished steps, nothing dropped, got {state:?}"
    );

    let resumed_entries = load_run_entries(&log_dir, &second.lease.run_id);
    assert_eq!(
        keys(&resumed_entries),
        vec!["alpha", "beta", "gamma", "delta"],
        "every segment is append-only and every segment is folded: the crashed \
         process's entries are still there, in its own segment, with the delta \
         in the resumed process's — got {resumed_entries:?}"
    );
    assert_eq!(
        seqs(&resumed_entries),
        vec![0, 1, 2, 3],
        "the resumed run's seqs must continue PAST the crashed run's rather than \
         restarting at 0 (which would make the two processes' entries \
         indistinguishable under a max-seq fold)"
    );
    let committed = persistence::load_checkpoint(&checkpoint_path)
        .expect("load_checkpoint after the resumed cycle")
        .expect("the resumed cycle committed");
    assert_eq!(committed.generation, 1);

    // --- normal completion retires; the boot after that MINTS ---------------
    // What `tidepool-selfharness`'s `run_loop` return path does, and the other
    // direction of the same mechanism: a crash skips this call, which is
    // precisely how the boot above knew to resume.
    let retired = retire_lease(&log_dir)
        .expect("retire the lease")
        .expect("there is an active lease to retire");
    assert!(
        retired.exists(),
        "a retired lease is RETAINED — renamed beside its segments, never deleted"
    );
    let retained: RunLease = serde_json::from_slice(&std::fs::read(&retired).expect("read"))
        .expect("a retired lease stays readable, not just present");
    assert_eq!(
        retained.run_id, first.lease.run_id,
        "the retained lease must name the run it belonged to"
    );

    let third = acquire_lease(&log_dir).expect("the boot after a normal completion");
    assert!(
        !third.resumed,
        "a retired lease must not be resumed — a finished run's work would be \
         re-entered on every subsequent boot"
    );
    assert_ne!(third.lease.run_id, first.lease.run_id);
    assert_ne!(
        third.segment, first.segment,
        "a fresh run must get its OWN segment, never adopt the finished run's"
    );
    assert!(
        third.segment.exists(),
        "allocation itself exclusively claims the segment file, empty, via \
         create_new — it exists before the first append, not after"
    );
    let mut after = fixture_driver("crash-delta-3");
    assert_eq!(
        after
            .open_run_journal(&log_dir, &third)
            .expect("open the fresh run's journal"),
        0,
        "the fresh run folds nothing — the finished run's entries are not its own"
    );
    assert_eq!(
        keys(&load_run_entries(&log_dir, &first.lease.run_id)),
        vec!["alpha", "beta", "gamma", "delta"],
        "and the finished run's segments are untouched by any of this — nothing \
         rewrites, compacts, or deletes them"
    );
}

/// A kill landing DURING an append, rather than between two: the journal's
/// final line is a byte prefix with no newline.
///
/// `load_journal` skips it with a warning and returns the complete entries, so
/// the boot fold survives — and the torn record is ABSENT from the fold, which
/// is what makes the resumed run redo exactly that one step (a step whose
/// record never landed durably is a step still to do; the harness's skip
/// decision is a pure function of the fold, as
/// `crashed_cycle_keeps_its_lease_and_the_resumed_run_does_only_the_delta`
/// exercises end to end). The resumed run's first append even REUSES the torn
/// record's seq, since nothing durable ever claimed it.
///
/// Folds a hand-written journal — no Haskell compiled.
#[test]
fn torn_final_line_folds_to_its_complete_entries_and_leaves_its_step_undone() {
    let _cache_guard = support::isolate_cache();
    let dir = scratch("torn-tail");
    let acquired = acquire_lease(&dir).expect("mint the lease");

    let gamma = line(2, "gamma", 30);
    let mut file = line(0, "alpha", 10) + &line(1, "beta", 20);
    file.push_str(torn_prefix_of(&gamma));
    std::fs::write(&acquired.segment, &file).expect("plant a torn-tail segment");

    let entries = load_journal(&acquired.segment).expect(
        "a torn FINAL line is skipped, never fatal — that \
             tolerance is the whole reason a crash mid-append is recoverable",
    );
    assert_eq!(keys(&entries), vec!["alpha", "beta"]);

    let fold = ResumeFold::fold(&acquired.lease.run_id, &entries);
    assert_eq!(
        folded_keys(&fold),
        vec!["alpha".to_string(), "beta".to_string()],
        "the torn record must not reach the authored side — it would make a step \
         that never durably completed look finished"
    );
    assert_eq!(
        fold.next_seq(),
        2,
        "the resumed run's first append reuses the torn record's own seq: no \
         durable entry ever claimed it"
    );

    // The driver's boot seam agrees with the fold read directly — one segment,
    // one loaded-and-folded result, whichever way it is reached.
    let mut driver = fixture_driver("torn-tail");
    assert_eq!(
        driver
            .open_run_journal(&dir, &acquired)
            .expect("the boot fold tolerates a torn tail"),
        2
    );
}

/// A line that fails to parse ANYWHERE but the end cannot come from a crash: an
/// append-only file's every write except the last completed, so this is real
/// corruption. It fails the boot loudly, naming the file and the line — never
/// absorbed the way a torn tail is, because absorbing it would silently drop
/// facts a run genuinely recorded and then redo that work blind.
///
/// Folds a hand-written journal — no Haskell compiled.
#[test]
fn torn_line_before_the_last_fails_the_boot_loudly() {
    let _cache_guard = support::isolate_cache();
    let dir = scratch("torn-mid");
    let acquired = acquire_lease(&dir).expect("mint the lease");

    let beta = line(1, "beta", 20);
    let mut file = line(0, "alpha", 10);
    file.push_str(torn_prefix_of(&beta));
    file.push('\n'); // a COMPLETE line that does not parse — not a torn tail
    file.push_str(&line(2, "gamma", 30));
    std::fs::write(&acquired.segment, &file).expect("plant a mid-file corrupted segment");

    assert!(
        matches!(
            load_journal(&acquired.segment),
            Err(JournalLoadError::TornMidFile { line_no: 1, .. })
        ),
        "expected TornMidFile at line 1, got {:?}",
        load_journal(&acquired.segment)
    );

    let mut driver = fixture_driver("torn-mid");
    let err = driver
        .open_run_journal(&dir, &acquired)
        .expect_err("the boot must refuse a corrupted segment, not fold around it");
    let msg = err.to_string();
    assert!(
        msg.contains(&acquired.segment.display().to_string()) && msg.contains("line 1"),
        "the refusal must name the segment and the offending line, got: {msg}"
    );
}

/// THE regression this whole lane exists to close. Under the old single-file
/// design, a torn tail was survivable exactly once — the boot that folds it
/// was fine, but the boot after that either failed loudly (two or more
/// appends after the tear) or silently lost a durable record (exactly one
/// append), because the resumed run's first append landed on the SAME
/// physical line as the torn bytes and the two merged into one unparseable
/// line.
///
/// With segments, a resumed process never appends into a segment a prior
/// process left — it always gets a fresh, freshly-allocated one — so neither
/// failure mode is reachable, no matter how many further boots happen. This
/// walks several, through the REAL driver seam (`acquire_lease` +
/// `SelfHarnessDriver::open_run_journal`), and every one folds cleanly.
///
/// Folds hand-written segments — no Haskell compiled.
#[test]
fn a_torn_tail_never_poisons_a_later_boot_through_the_driver_seam() {
    let _cache_guard = support::isolate_cache();
    let dir = scratch("torn-tail-many-boots");

    // Boot 1: mints the run, writes two complete records, then crashes
    // mid-append on a third — its segment's torn tail, sealed forever.
    let first = acquire_lease(&dir).expect("mint the lease");
    let gamma = line(2, "gamma", 30);
    let mut file = line(0, "alpha", 10) + &line(1, "beta", 20);
    file.push_str(torn_prefix_of(&gamma));
    std::fs::write(&first.segment, &file).expect("plant a torn-tail segment");

    let mut driver = fixture_driver("torn-tail-many-boots-1");
    assert_eq!(
        driver
            .open_run_journal(&dir, &first)
            .expect("boot 1 folds around the torn tail"),
        2,
        "alpha and beta; the torn gamma never landed durably"
    );

    // Boots 2 through 6: each is a FRESH process over the same run — never
    // touching a byte any prior process wrote. Every single one must stay
    // parseable, not just the first — "one boot deep" was exactly the old
    // design's limit, and this is the case that closes it.
    for boot in 2..=6u64 {
        let acquired = acquire_lease(&dir).expect("resume");
        assert_eq!(acquired.lease.run_id, first.lease.run_id);
        assert!(
            acquired.segment.exists(),
            "boot {boot}'s segment is exclusively claimed (empty) at allocation \
             time, via create_new — it exists before this boot ever appends"
        );

        let mut driver = fixture_driver(&format!("torn-tail-many-boots-{boot}"));
        let folded = driver
            .open_run_journal(&dir, &acquired)
            .unwrap_or_else(|e| {
                panic!("boot {boot} must stay parseable this many boots past the torn tail: {e}")
            });
        assert_eq!(folded, boot as usize, "boot {boot}'s fold");

        // This boot records its own step, into its OWN segment — the next
        // boot must see it only as a prior, sealed segment, never merge
        // anything into its bytes.
        std::fs::write(&acquired.segment, line(boot, &format!("step-{boot}"), 0))
            .expect("this boot's own segment write");
    }

    // Segment 0's torn tail is untouched, byte for byte, after five further
    // boots — nothing downstream of it was ever rewritten into.
    let seg0 = std::fs::read_to_string(&first.segment).expect("segment 0 read");
    assert!(seg0.ends_with(torn_prefix_of(&gamma)));
}

/// Idempotence as a property of the driver's boot seam, not just of the pure
/// fold over a vector (`selfharness::resume`'s own unit tests already pin
/// that). Folding the same segment TWICE through `open_run_journal` — the
/// operation a resumed boot performs on segments a prior process wrote — is
/// the same fold both times.
///
/// Also pins the flip side of what used to be "order-insensitive": a segment
/// whose LINE order disagrees with its `seq` order now folds DIFFERENTLY
/// from the canonical one — physical order, not `seq`, decides the winner
/// (see `tidepool_handlers::last_by_kind_key`'s doc). Any reordering of the
/// same entries folding identically was exactly the property a positional
/// fold gives up: a foreign or mis-seeded segment can no longer invert the
/// result by carrying a `seq` that contradicts the bytes it was actually
/// written in.
///
/// Folds hand-written segments — no Haskell compiled.
#[test]
fn boot_fold_is_idempotent_and_sensitive_to_the_segments_physical_order() {
    let _cache_guard = support::isolate_cache();
    let dir = scratch("fold-idempotent");
    let acquired = acquire_lease(&dir).expect("mint the lease");

    // `alpha` recorded twice — the LAST one in the file must win, so the
    // fold is doing real work rather than trivially keeping everything.
    let lines = [
        line(0, "alpha", 10),
        line(1, "beta", 20),
        line(2, "alpha", 11),
        line(3, "gamma", 30),
    ];
    std::fs::write(&acquired.segment, lines.concat()).expect("plant a segment");

    let mut driver = fixture_driver("fold-idempotent");
    let once = driver.open_run_journal(&dir, &acquired).expect("fold once");
    let twice = driver
        .open_run_journal(&dir, &acquired)
        .expect("fold the same segment again");
    assert_eq!(
        (once, twice),
        (3, 3),
        "folding the same segment again must yield the same fold — the operation \
         a resumed boot performs on segments a prior process wrote"
    );

    let canonical = ResumeFold::fold(
        &acquired.lease.run_id,
        &load_journal(&acquired.segment).expect("canonical segment loads"),
    );
    assert_eq!(canonical.next_seq(), 4);
    assert_eq!(
        canonical.to_json()["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .find(|e| e["key"] == "alpha")
            .expect("alpha survived the fold")["payload"],
        serde_json::json!(11),
        "the LAST record per (kind, key) in the file wins"
    );

    // The same entries, written in an order the file's own lines contradict —
    // a DIFFERENT physical order, which must fold DIFFERENTLY.
    let shuffled_dir = scratch("fold-shuffled");
    let shuffled = acquire_lease(&shuffled_dir).expect("mint the lease");
    let mut reversed = lines.clone();
    reversed.reverse();
    std::fs::write(&shuffled.segment, reversed.concat()).expect("plant a reversed segment");
    let shuffled_fold = ResumeFold::fold(
        // The same run id: a fold carries the run it came from, and only the
        // ENTRY order is under test here.
        &acquired.lease.run_id,
        &load_journal(&shuffled.segment).expect("reversed segment loads"),
    );
    assert_ne!(
        shuffled_fold, canonical,
        "reversing the physical order must change the fold — position, not \
         seq, decides the winner now"
    );
    assert_eq!(
        shuffled_fold.to_json()["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .find(|e| e["key"] == "alpha")
            .expect("alpha survived the fold")["payload"],
        serde_json::json!(10),
        "in the REVERSED file, entry(0, alpha, 10) is now LAST, so it wins — \
         even though its seq (0) is lower than the other alpha's (2)"
    );
}
