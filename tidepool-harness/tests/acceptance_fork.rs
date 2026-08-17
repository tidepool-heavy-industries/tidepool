//! Self-iterating-harness fork widen acceptance coverage: an ANSWERER agent
//! can call `forkAll [briefs]` mid-turn (spawning N parallel child
//! sub-answerers, gathering their typed answers as `[a]`, resuming the
//! parent) and then continue to `finalize`, driven through the REAL
//! production entry point (`SelfHarnessDriver::run_one_cycle`) — same
//! discipline as `selfharness_spine.rs`, extended to prove the driver
//! services a `HoleRouting::Fork` suspension on the per-loop answerer via
//! the EXISTING `Harness::answer_fanout`/`answer_fork` machinery
//! (`SelfHarnessDriver::drain_answerer_fork`), not a reimplementation.
//!
//! Reply order (global, `ReplayProvider`): parent answerer turn (forks two
//! briefs, finalizes with the first child's answer), then child 0's turn,
//! then child 1's turn — `Harness::answer_fanout` drives fanout children in
//! declaration order.
//!
//! The provider is wrapped in a [`CapturingProvider`] so one drive of the
//! scenario asserts both fork-servicing/finalize AND that forked children
//! inherit the parent's byte-identical transcript prefix + render-derived
//! framing — guarding against `Harness::force` resetting a forked child's
//! framing to `None`, which would make the child send the default
//! `SYSTEM_FRAMING` instead of the inherited one.

use std::sync::{Arc, Mutex};

mod support;

use tidepool_harness::engine::{EngineConfig, SYSTEM_FRAMING};
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, Role, StreamSink, TurnRequest, TurnResponse,
    Usage,
};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
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

fn examples_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-fork".into(),
        extract_fingerprint: "acceptance-fork".into(),
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

/// A provider that records every assembled [`TurnRequest`] (in call order)
/// before delegating to an inner [`ReplayProvider`] — lets one drive of the
/// scenario assert BOTH the fork-servicing/finalize outcome and the exact
/// request prefix each forked child sent.
struct CapturingProvider {
    inner: ReplayProvider,
    requests: Arc<Mutex<Vec<TurnRequest>>>,
}

impl ModelProvider for CapturingProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        self.requests.lock().unwrap().push(req.clone());
        self.inner.complete(req, sink).await
    }
}

/// ONE render -> loop -> `runLLMTurn @Decision` -> the answerer `forkAll`s
/// two briefs -> two child answerers each resume a typed `Decision` ->
/// `[Decision]` resumes the parent -> the parent `finalize`s with the FIRST
/// child's `Decision` -> the value flows back up through `loop` into `State`
/// -> render. Proves the outer loop, the driver's fork-hole servicing, and
/// the outer/nested-Agent State boundary all compose end to end — AND that
/// each forked child's provider request prefix (system framing + messages,
/// in order) is byte-identical to the parent's through the fork checkpoint,
/// which is what lets a forked child behave like the parent and gives the
/// provider a stable prefix to cache on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selfharness_answerer_forks_to_two_children_then_finalizes() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");

    let replies = vec![
        // Parent answerer turn: fork out two briefs, then finalize with the
        // first child's typed answer.
        reply(
            "```haskell\n\
             import HarnessTypes (Decision (..), Confidence (..))\n\
             import Tidepool.Fork (forkAll)\n\
             \n\
             do\n\
             \x20 ds <- forkAll @Decision [\"sub-brief A\", \"sub-brief B\"]\n\
             \x20 finalize @Decision (L.head ds) :: M ()\n\
             ```",
        ),
        // Child 0 ("sub-brief A"): resumes with a typed Decision.
        reply(
            "```haskell\n\
             import HarnessTypes (Decision (..), Confidence (..))\n\
             \n\
             resume (Decision { action = \"observe\", rationale = \"child A\", \
             confidence = Low })\n\
             ```",
        ),
        // Child 1 ("sub-brief B"): resumes with a different typed Decision.
        reply(
            "```haskell\n\
             import HarnessTypes (Decision (..), Confidence (..))\n\
             \n\
             resume (Decision { action = \"wait\", rationale = \"child B\", \
             confidence = Low })\n\
             ```",
        ),
    ];
    let requests = Arc::new(Mutex::new(Vec::<TurnRequest>::new()));
    let provider: Arc<dyn DynModelProvider> = Arc::new(CapturingProvider {
        inner: ReplayProvider::new(replies),
        requests: Arc::clone(&requests),
    });
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("acceptance-fork-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one full render->loop->runLLMTurn->forkAll(2 children)->finalize->render cycle");

    // --- block 1: fork servicing + first-child Decision reaching State ---

    // The finalized Decision is the FIRST child's ("sub-brief A" / child 0),
    // per `L.head ds` — asserts declaration order is preserved through the
    // fanout and that the driver actually resumed the parent with a real
    // `[Decision]`, not a stub.
    let state = &outcome.state_json;
    let decision = state
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision, not null");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("observe"),
        "the finalized decision must be the FIRST fork child's, got {state:?}"
    );
    assert_eq!(
        decision.get("rationale").and_then(|v| v.as_str()),
        Some("child A")
    );

    // The loop boundary + outer State-threading invariants still hold with a
    // fork in the middle (same shape `selfharness_spine.rs` pins for the
    // non-fork path).
    assert_eq!(
        state.get("mode").and_then(|v| v.as_str()),
        Some("Deciding"),
        "loop must advance Observing -> Deciding, got {state:?}"
    );
    assert_eq!(
        driver.iteration(),
        1,
        "the driver's iteration count must increment across the loop boundary"
    );

    // The POST-loop render reflects the new state — the value that
    // crossed the fork boundary reaches the very next render.
    assert!(
        outcome.prompt_after.contains("observe"),
        "post-loop render should show the finalized decision's action, got:\n{}",
        outcome.prompt_after
    );

    // --- block 2: children inherit the parent's byte-identical transcript
    // --- prefix + render-derived framing ---

    let reqs = requests.lock().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "expected 3 model turns (parent + 2 children), got {}",
        reqs.len()
    );

    // The parent answerer's request: a System message first (its render-derived
    // framing), then the hole card as the first user message.
    let parent = &reqs[0];
    assert_eq!(
        parent.messages.first().map(|m| &m.role),
        Some(&Role::System),
        "the parent request must open with a System framing message"
    );
    let parent_system = parent.messages[0].clone();
    // The framing is the render output + answerer suffix — NOT the default
    // full-surface SYSTEM_FRAMING. (If this ever equals SYSTEM_FRAMING the
    // answerer never got its render framing at all.)
    assert_ne!(
        parent_system.content, SYSTEM_FRAMING,
        "the answerer framing must be render-derived, not the default SYSTEM_FRAMING"
    );

    // Exact-context: each child's request BEGINS with the parent's entire
    // request prefix (system framing + transcript through the fork checkpoint),
    // byte-identical, then appends the child's own turns. The system message
    // being inherited is the fix — Harness::force keeps the parent framing
    // instead of resetting to None (which would send the default SYSTEM_FRAMING).
    for (i, child) in reqs[1..].iter().enumerate() {
        assert_eq!(
            child.messages.first(),
            Some(&parent_system),
            "child {i} must inherit the parent's system framing verbatim (exact-context \
             fork); a mismatch means force() reset the child framing to None"
        );
        assert_ne!(
            child.messages[0].content, SYSTEM_FRAMING,
            "child {i}'s system message must be the inherited answerer framing, not \
             the default SYSTEM_FRAMING"
        );
        assert!(
            child.messages.starts_with(&parent.messages),
            "child {i}'s request must begin with the parent's full request prefix \
             (system framing + transcript through the fork checkpoint), byte-identical.\n\
             parent: {:?}\nchild:  {:?}",
            parent.messages,
            child.messages
        );
        // The child appends its own turns after the inherited prefix, the last
        // being its own hole card (a User message).
        assert!(
            child.messages.len() > parent.messages.len(),
            "child {i} must append its own hole card after the inherited prefix"
        );
        assert_eq!(
            child.messages.last().map(|m| &m.role),
            Some(&Role::User),
            "child {i}'s last message is its own hole card"
        );
    }
}
