//! Acceptance for the COLLAPSED recursive companion (fork-subsumes-split
//! step 4): one turn is ONE top-level typed request, and the tree emerges
//! from the root session's own `async (fork @T brief)` calls, serviced by
//! the driver — no authored layer walk, no gate, no companion caps.
//!
//! Drives the SHIPPED `harness-dogfooding/recursive-companion/` harness
//! through the production entry point (`SelfHarnessDriver::run_one_loop_iteration`),
//! scripted record-replay, zero live calls — the same discipline as
//! `answerer_async_fork.rs`, which pins the fork/green servicing mechanics
//! themselves against the reference harness. What THIS file pins is the
//! companion contract on top of them:
//!
//! - the seed gate: a fresh boot (no checkpoint) asks the operator for the
//!   question via `askUser @SeedQuestion` BEFORE any model session runs, and
//!   the seeded question checkpoints with the completed turn;
//! - the collapsed turn: root request → session forks → typed child answers
//!   land on the right handles → the root's OWN `finalize @Text` value is
//!   stored AS-IS in `lastAnswer` (never re-shaped) and journaled
//!   under kind `"turn"`.
//!
//! The ReplayProvider queue is itself a behavior pin: the fork scenario's
//! queue only works if both children are driven (in spawn order) after the
//! one spawning block — a swapped delivery yields "BETA | ALPHA".
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value as Json};

use tidepool_handlers::{ConsoleHandler, JournalHandler, SegmentPath};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::operator::FormShape;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, OperatorGate,
    SelfHarnessDriver,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn companion_dir() -> PathBuf {
    repo_root().join("harness-dogfooding/recursive-companion")
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "companion-collapsed-slice-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "companion-collapsed-slice".into(),
        extract_fingerprint: "companion-collapsed-slice".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        content: content.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn finalize_text_reply(text: &str) -> RecordedReply {
    reply(&format!(
        "```haskell\n(finalize @Text (\"{text}\" :: Text) :: M ())\n```"
    ))
}

/// The root session's block: two forks spawned as green threads, BOTH
/// outstanding before the first `wait`, typed results combined into the
/// finalize. `a <> " | " <> b` pins that each child's value landed on the
/// right handle.
// NOTE: single-line literal with explicit \n — a `\` line-continuation
// strips the next line's leading spaces, which silently destroys do-block
// indentation (answerer_async_fork.rs learned this live).
const FORKING_ROOT_BLOCK: &str = "```haskell\nimport Tidepool.Fork (fork)\n\ndo\n  ha <- async (fork @Text \"gather the alpha fact\")\n  hb <- async (fork @Text \"gather the beta fact\")\n  a <- wait ha\n  b <- wait hb\n  (finalize @Text (a <> \" | \" <> b) :: M ())\n```";

/// A gate that answers the seed-question form once, then refuses: the
/// collapsed loop presents exactly ONE form on a fresh boot (the seed gate)
/// and none at all on a seeded turn.
struct SeedOnlyGate {
    question: &'static str,
    presented: std::sync::atomic::AtomicU32,
}

impl SeedOnlyGate {
    fn new(question: &'static str) -> Self {
        Self {
            question,
            presented: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

impl OperatorGate for SeedOnlyGate {
    fn present_form(&self, _shape: &FormShape) -> Json {
        let n = self
            .presented
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            n, 0,
            "the collapsed loop presents exactly one form (the seed gate)"
        );
        json!({"seedQuestion": self.question})
    }
}

/// A gate for the seeded scenario: any presentation is a failure.
struct NoGate;

impl OperatorGate for NoGate {
    fn present_form(&self, _shape: &FormShape) -> Json {
        panic!("a seeded turn presents no operator form")
    }
}

fn build_driver(
    replies: Vec<RecordedReply>,
    gate: Arc<dyn OperatorGate>,
    dir: &std::path::Path,
) -> (SelfHarnessDriver, PathBuf, PathBuf) {
    let checkpoint_path = dir.join("checkpoint.json");
    let journal_path = dir.join("journal.jsonl");
    let log_path = dir.join("log.jsonl");

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        repo_root().join("haskell/lib"),
        Some(companion_dir()),
    )
    .expect("answerer engine config over the recursive-companion harness dir");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(checkpoint_path.clone());
    driver.set_console_handler(ConsoleHandler);
    driver.set_gate(gate);
    driver.set_journal_handler(
        JournalHandler::new(
            SegmentPath::create_exclusive(journal_path.clone())
                .expect("this scenario's journal segment is fresh in its own tempdir"),
        )
        .expect("fresh segment header stamp succeeds"),
    );
    (driver, checkpoint_path, journal_path)
}

/// A seeded turn: the root session forks two typed sub-answerers, waits
/// both, and finalizes the combination — which lands AS-IS in
/// `lastAnswer` and in the `"turn"` journal entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seeded_turn_forks_and_stores_the_roots_typed_answer_as_is() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let dir = scratch("forking");
    let (mut driver, checkpoint_path, journal_path) = build_driver(
        vec![
            reply(FORKING_ROOT_BLOCK),
            finalize_text_reply("ALPHA"),
            finalize_text_reply("BETA"),
        ],
        Arc::new(NoGate),
        &dir,
    );

    let source = load_harness_source(&companion_dir().join("Harness.hs"))
        .expect("the shipped recursive-companion harness loads");

    persistence::save_checkpoint(
        &checkpoint_path,
        &persistence::Checkpoint::committed(
            None,
            json!({
                "question": "SCENARIO: combine the alpha and beta facts.",
                "turnCount": 0,
                "lastAnswer": Json::Null,
            }),
            None,
            source.fingerprint.clone(),
            persistence::LoopIteration::new(0),
        ),
    )
    .expect("seed the scenario's durable checkpoint");

    let restored = driver
        .restore(&source)
        .await
        .expect("restore the seeded checkpoint")
        .expect("the seeded checkpoint is on disk");

    let outcome = driver
        .run_one_loop_iteration(&source, Some(&restored))
        .await
        .expect("one render -> loop -> render cycle, forks included");

    let state = outcome.state_json;
    assert_eq!(
        state.get("turnCount").and_then(Json::as_i64),
        Some(1),
        "one turn completed: {state}"
    );
    let last_answer = state
        .get("lastAnswer")
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("the cycle recorded no lastAnswer: {state}"));
    assert_eq!(
        last_answer, "ALPHA | BETA",
        "the root's own typed value, stored as-is — a swapped fork delivery \
         or any re-shaping fails here: {state}"
    );

    let journal = tidepool_handlers::load_journal(&journal_path).expect("journal loads");
    let answer_entry = journal
        .iter()
        .filter(|e| e.kind == "turn")
        .find_map(|e| e.payload.get("answer").and_then(Json::as_str))
        .unwrap_or_else(|| panic!("no \"turn\" journal entry with an \"answer\" field"));
    assert_eq!(
        answer_entry, "ALPHA | BETA",
        "the turn journal carries the same as-is answer"
    );
}

/// A fresh boot (NO checkpoint): the loop's first act is the seed gate —
/// `askUser @SeedQuestion` through the operator gate, before any model
/// session runs — and the seeded question checkpoints WITH the completed
/// first turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_boot_seeds_the_question_through_the_operator_gate() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let dir = scratch("seed-gate");
    let (mut driver, _checkpoint_path, _journal_path) = build_driver(
        vec![finalize_text_reply("SKY IS BLUE")],
        Arc::new(SeedOnlyGate::new("why is the sky blue?")),
        &dir,
    );

    let source = load_harness_source(&companion_dir().join("Harness.hs"))
        .expect("the shipped recursive-companion harness loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("a fresh boot: seed gate, then the first turn");

    let state = outcome.state_json;
    assert_eq!(
        state.get("question").and_then(Json::as_str),
        Some("why is the sky blue?"),
        "the operator's seed answer is the durable question: {state}"
    );
    assert_eq!(
        state.get("turnCount").and_then(Json::as_i64),
        Some(1),
        "seeding recurses straight into turn 1 (never parks the operator on \
         a confirm-what-you-just-did gate): {state}"
    );
    let last_answer = state
        .get("lastAnswer")
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("the first turn recorded no lastAnswer: {state}"));
    assert_eq!(last_answer, "SKY IS BLUE", "{state}");
}
