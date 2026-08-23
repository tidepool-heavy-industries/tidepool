//! Minimal standalone repro for the JIT row-changing `reinterpret` gap named
//! in PRD 21 C5's second amendment
//! (`plans/self-iterating-harness/21-c5-delegate-effect-survey.md`): a
//! `reinterpret`'d handler body that performs a `send` which must reach the
//! driver as a real suspension does not classify correctly on this JIT.
//!
//! Reduced to the smallest possible shape — see `tests/fixtures/ReinterpretRepro.hs`
//! for the private `Ping` effect and its `reinterpret`-based interpreter onto
//! the real `AskUser`/`NoteWith` machinery. The ISOLATING CONTROL
//! (`direct_note_send_classifies_correctly`) performs the IDENTICAL
//! `send (NoteWith "ping")` directly in the turn's own text, with no
//! `reinterpret` involved at all, on the SAME row.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH
//! (`--ignore-default-filter -p tidepool-harness` to run).

mod support;

use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::harness::AnswerContract;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::{typed_request_agent_decls, Harness, SuspensionRouting, TurnOutcome};

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
        prelude_hash: "reinterpret-rowchange-repro".into(),
        extract_fingerprint: "reinterpret-rowchange-repro".into(),
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

fn haskell(block: &str) -> String {
    format!("```haskell\n{block}\n```")
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

fn engine_cfg() -> EngineConfig {
    let mut cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer config");
    cfg.include.push(fixtures_dir());
    cfg
}

/// THE GAP: `runPing (ping 5)` lowers via `ReinterpretRepro.runPing`'s
/// `reinterpret` onto a `send (NoteWith "ping")` performed FROM WITHIN the
/// reinterpretation handler. If the JIT executes freer-simple's row-changing
/// `reinterpret`/`replaceRelay`/`decomp`/`weaken` correctly, this must
/// classify identically to the direct send in the control test below:
/// `SuspensionRouting::Note { text: "ping" }`. The PRD 21 C5 lane's observation was
/// that it instead reaches the driver as an UNCLASSIFIED suspension
/// (`SuspensionRouting::Ask { payload: Null }`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reinterpret_handler_send_classifies_correctly() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let cfg = engine_cfg();
    let replies = vec![reply(&haskell(
        "import ReinterpretRepro (Ping (..), ping, runPing)\n\n\
         (do\n\
         \x20  n <- runPing (ping 5)\n\
         \x20  finalize @Int n) :: M Int",
    ))];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let dir = std::env::temp_dir().join(format!(
        "reinterpret-rowchange-repro-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let writer = LogWriter::create(dir.join("reinterpret.jsonl"), &header()).expect("log writer");
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("reinterpret repro root", "Run the reinterpret repro.")
        .expect("create root");
    harness.force(root, Actor::Operator).expect("force root");
    harness.set_answer_contract(
        root,
        Some(AnswerContract {
            ty: "Int".to_string(),
            imports: vec![],
        }),
    );

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole or completes");

    match &outcome {
        TurnOutcome::Suspended { classified, .. } => {
            assert_eq!(
                classified.routing,
                SuspensionRouting::Note {
                    text: "ping".to_string()
                },
                "a `send` performed from inside a `reinterpret` handler body \
                 must classify identically to the same `send` written \
                 directly in the turn's own text — got {:?}. This is the \
                 JIT row-changing `reinterpret` gap named in PRD 21 C5's \
                 second amendment.",
                classified.routing
            );
        }
        other => panic!(
            "expected a Suspended outcome (the note), got {}",
            outcome_tag(other)
        ),
    }
}

/// THE ISOLATING CONTROL: the IDENTICAL `send (NoteWith "ping")`, written
/// directly in the turn's own text — no `reinterpret` involved at all — on
/// the SAME row (`typed_request_agent_decls()`, `AskUser` present). This must classify
/// as `SuspensionRouting::Note { text: "ping" }`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_note_send_classifies_correctly() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let cfg = engine_cfg();
    let replies = vec![reply(&haskell(
        "(do\n\
         \x20  send (NoteWith \"ping\")\n\
         \x20  finalize @Int (5 :: Int)) :: M Int",
    ))];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let dir = std::env::temp_dir().join(format!("direct-note-repro-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let writer = LogWriter::create(dir.join("direct.jsonl"), &header()).expect("log writer");
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("direct note repro root", "Run the direct-send control.")
        .expect("create root");
    harness.force(root, Actor::Operator).expect("force root");
    harness.set_answer_contract(
        root,
        Some(AnswerContract {
            ty: "Int".to_string(),
            imports: vec![],
        }),
    );

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole or completes");

    match &outcome {
        TurnOutcome::Suspended { classified, .. } => {
            assert_eq!(
                classified.routing,
                SuspensionRouting::Note {
                    text: "ping".to_string()
                },
                "a direct `send (NoteWith ...)` must classify as Note — got {:?}",
                classified.routing
            );
        }
        other => panic!(
            "expected a Suspended outcome (the note), got {}",
            outcome_tag(other)
        ),
    }
}
