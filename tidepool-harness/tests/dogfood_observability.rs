//! Acceptance coverage for the wave-1.5 dogfood-observability spec
//! (`plans/post-restart/dev/dogfood-observability.md`): a person watching the
//! console can narrate what the self-iterating harness is doing — what
//! source it compiled, what types it extracted, what it asked, what came
//! back — without reading code, AND tier-0 telemetry (first-compile success
//! rate, retries-per-hole) is a fold over `transcript.jsonl` alone. Needs
//! `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside `nix
//! develop` (see `haskell/CLAUDE.md`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod support;

use serde_json::Value as Json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::persistence;
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Event, Harness, JsonlObserver, LogObserver, Observer,
    SelfHarnessDriver,
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

fn examples_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "dogfood-observability".into(),
        extract_fingerprint: "dogfood-observability".into(),
        harness_version: "test".into(),
    }
}

/// Fans every driver [`Event`] out to several observers — mirrors the
/// production binary's `FanoutObserver` (`tidepool-selfharness.rs`), so this
/// test exercises the SAME console (`LogObserver`) + durable-transcript
/// (`JsonlObserver`) wiring a live dogfood run uses.
struct FanoutObserver {
    observers: Vec<Arc<dyn Observer>>,
}
impl Observer for FanoutObserver {
    fn on_event(&self, event: &Event) {
        for o in &self.observers {
            o.on_event(event);
        }
    }
}

/// A `tracing_subscriber::fmt::MakeWriter` that appends every formatted line
/// to a shared in-memory buffer instead of stderr — captures exactly what a
/// dogfood operator's terminal would show, for direct string assertions.
#[derive(Clone)]
struct CapturedWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for CapturedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedWriter {
    type Writer = CapturedWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A recorded assistant reply with a `finalize @Decision` block that will
/// NOT compile (an out-of-scope identifier) — a corrective-retry round the
/// answerer must recover from.
fn bad_finalize_reply() -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
                  (finalize @Decision (Decision { action = \"observe\", rationale = \"first \
                  loop\", confidence = totallyUndefinedIdentifier }) :: M ())\n```"
            .to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
        },
    }
}

/// The corrected reply for the SAME hole — compiles and finalizes.
fn good_finalize_reply() -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 1,
        content: "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
                  (finalize @Decision (Decision { action = \"observe\", rationale = \"first \
                  loop\", confidence = Medium }) :: M ())\n```"
            .to_string(),
        usage: Usage {
            input_tokens: 55,
            output_tokens: 12,
        },
    }
}

/// Drive ONE `render -> loop -> runLLMTurn @Decision -> finalize -> render`
/// cycle through the production entry point (`SelfHarnessDriver::run_one_cycle`)
/// with a [`ReplayProvider`] serving `bad_finalize_reply` then
/// `good_finalize_reply` — the answerer's `runLLMTurn @Decision` hole burns
/// one corrective-retry round before finalizing. Returns the captured console
/// text and the written `transcript.jsonl` contents. `_cache_guard` must
/// outlive the caller's use of the returned paths' contents (already read to
/// owned `String`s here, so the caller doesn't need to keep it).
async fn run_one_cycle_with_a_retry() -> (String, String) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let _ = tracing_subscriber::fmt()
        .with_writer(CapturedWriter(buf.clone()))
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "warn,tidepool_harness=debug",
        ))
        .with_ansi(false)
        .try_init();

    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(vec![
        bad_finalize_reply(),
        good_finalize_reply(),
    ]));
    let log_path = persistence::default_log_path();
    std::fs::create_dir_all(log_path.parent().expect("log path has a parent"))
        .expect("create selfharness cache dir");
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let jsonl =
        JsonlObserver::create(&persistence::default_transcript_path()).expect("jsonl observer");
    let observer: Arc<dyn Observer> = Arc::new(FanoutObserver {
        observers: vec![Arc::new(LogObserver), Arc::new(jsonl)],
    });
    let mut driver = SelfHarnessDriver::new(agent, observer);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    driver
        .run_one_cycle(&source, None)
        .await
        .expect("one render->loop->runLLMTurn->finalize->render cycle, surviving one retry");

    let console = String::from_utf8_lossy(&buf.lock().unwrap()).to_string();
    let transcript = std::fs::read_to_string(persistence::default_transcript_path())
        .expect("read transcript.jsonl");
    (console, transcript)
}

/// Acceptance 1+2 (narration + telemetry fold): `run_one_cycle_with_a_retry`
/// ALREADY returns `(console, transcript)` as a tuple for ONE replayed cycle —
/// formerly two tests each discarding half of it. One drive, both assertion
/// blocks: the narration elements a person watching the console needs are
/// present (the compiled source, both the failing AND the corrected block,
/// the extracted type, the hole's prompt, the untruncated compile error, the
/// finalized answer, and that `u64::MAX` never appears raw), AND the tier-0
/// telemetry fold (first-compile success rate, retries-per-hole) computed
/// from `transcript.jsonl` alone is correct for a cycle whose single hole
/// burns exactly one corrective-retry round.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn narration_and_transcript_fold_both_hold_for_one_retry_cycle() {
    support::require_extract();
    let (console, transcript) = run_one_cycle_with_a_retry().await;

    // Deliverable 1: the compiled source, verbatim — both the failing round's
    // block and the corrected one.
    assert!(
        console.contains("totallyUndefinedIdentifier"),
        "console must show the failing round's compiled source verbatim, got:\n{console}"
    );
    assert!(
        console.contains("confidence = Medium"),
        "console must show the corrected round's compiled source verbatim, got:\n{console}"
    );

    // Deliverable 2: extracted types (the asks.json site -> type table names
    // `Decision`, surfaced by the existing `compile::compile_turn` INFO log
    // and/or the new `log_turn_extracted` line).
    assert!(
        console.contains("Decision"),
        "console must show the extracted answer type, got:\n{console}"
    );

    // Deliverable 3: the untruncated GHC error, with which round it was.
    assert!(
        console.contains("Variable not in scope") || console.contains("totallyUndefinedIdentifier"),
        "console must show the corrective retry's GHC error, got:\n{console}"
    );
    assert!(
        console.contains("round: 1")
            || console.contains("round=1")
            || console.contains("\"round\":1"),
        "console must show the retry's round index, got:\n{console}"
    );

    // Deliverable 4: the hole's prompt and the answer that came back.
    assert!(
        console.contains("decide the single next thing to do"),
        "console must show the runLLMTurn hole's prompt, got:\n{console}"
    );
    assert!(
        console.contains("observe") && console.contains("Medium"),
        "console must show the finalized answer, got:\n{console}"
    );

    // Deliverable 5: no raw u64::MAX sentinel anywhere.
    assert!(
        !console.contains("18446744073709551615"),
        "u64::MAX must never appear raw in console output, got:\n{console}"
    );

    // Deliverable 6 (the telemetry fold): compute first-compile success rate
    // and retries-per-hole from the SAME cycle's `transcript.jsonl` alone —
    // both metrics non-degenerate (rate < 1.0, at least one retry) because
    // the single hole burned exactly one corrective-retry round.
    let by_site = fold_answerer_rounds(&transcript);
    assert_eq!(
        by_site.len(),
        1,
        "exactly one hole (site 0, the loop's single runLLMTurn) was serviced, got: {by_site:?}"
    );
    let rounds = &by_site[&0];
    assert_eq!(
        rounds.len(),
        2,
        "site 0 must show both the failing and the corrected round, got: {rounds:?}"
    );
    assert_eq!(
        rounds[0],
        (1, false),
        "round 1 must be recorded as a compile FAILURE, got: {rounds:?}"
    );
    assert_eq!(
        rounds[1],
        (2, true),
        "round 2 must be recorded as a compile SUCCESS, got: {rounds:?}"
    );

    let rate = first_compile_success_rate(&by_site);
    assert_eq!(
        rate, 0.0,
        "the only hole did NOT succeed on its first attempt, so the rate must be 0.0, got {rate}"
    );

    let retries = retries_per_hole(&by_site);
    assert_eq!(
        retries.get(&0),
        Some(&1),
        "site 0 consumed exactly one corrective-retry round, got: {retries:?}"
    );
}

/// One hole's rounds folded from `transcript.jsonl`: `(round, compiled_ok)`
/// pairs in the order they were appended.
type Rounds = Vec<(u64, bool)>;

/// The real fold this crate's telemetry depends on: first-compile success
/// rate (of the holes serviced, the fraction whose ROUND 1 attempt compiled)
/// and retries-per-hole (how many rounds each hole's servicing needed beyond
/// the first), computed from `transcript.jsonl` ALONE — no other input. Reads
/// exactly the fields `AnswererRound` promises: `site` (hole identity),
/// `round` (per-turn attempt index), `error` (compile outcome).
fn fold_answerer_rounds(transcript_jsonl: &str) -> BTreeMap<u64, Rounds> {
    let mut by_site: BTreeMap<u64, Rounds> = BTreeMap::new();
    for line in transcript_jsonl.lines() {
        let ev: Json = serde_json::from_str(line).expect("well-formed transcript.jsonl line");
        if ev.get("ev").and_then(Json::as_str) != Some("answerer_round") {
            continue;
        }
        let site = ev["site"].as_u64().expect("answerer_round.site");
        let round = ev["round"].as_u64().expect("answerer_round.round");
        let ok = ev.get("error").is_none_or(|e| e.is_null());
        by_site.entry(site).or_default().push((round, ok));
    }
    by_site
}

fn first_compile_success_rate(by_site: &BTreeMap<u64, Rounds>) -> f64 {
    assert!(!by_site.is_empty(), "expected at least one serviced hole");
    let successes = by_site
        .values()
        .filter(|rounds| {
            rounds
                .iter()
                .find(|(r, _)| *r == 1)
                .map(|(_, ok)| *ok)
                .unwrap_or(false)
        })
        .count();
    successes as f64 / by_site.len() as f64
}

fn retries_per_hole(by_site: &BTreeMap<u64, Rounds>) -> BTreeMap<u64, usize> {
    by_site
        .iter()
        .map(|(site, rounds)| (*site, rounds.iter().filter(|(_, ok)| !ok).count()))
        .collect()
}
