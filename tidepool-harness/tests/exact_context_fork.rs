//! Exact-context fork: a forked child's provider request prefix (system
//! framing + messages, in order) is byte-identical to the parent's through the
//! fork checkpoint. This is what lets a forked child behave like the parent
//! (same render-derived system message, same conversation prefix) and what
//! gives the provider a stable prefix to cache on.
//!
//! Driven through the REAL production path (`SelfHarnessDriver::run_one_cycle`,
//! same scenario as `acceptance_fork.rs`) with a provider that RECORDS every
//! assembled `TurnRequest`. The recorded order is: the parent answerer's turn
//! (which `forkAll`s two briefs), then child 0's turn, then child 1's. The
//! assertion: each child's request opens with the SAME system message the
//! parent's did (the render-derived answerer framing, NOT the default
//! `SYSTEM_FRAMING`), and the SAME first user message (the hole card) — the
//! regression this guards is `Harness::force` resetting a forked child's
//! framing to `None`, which made the child send `SYSTEM_FRAMING` instead.

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
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "exact-context-fork".into(),
        extract_fingerprint: "exact-context-fork".into(),
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

/// A provider that records every assembled [`TurnRequest`] (in call order)
/// before delegating to an inner [`ReplayProvider`].
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forked_children_inherit_the_parent_framing_and_transcript_prefix() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(repo_root().join("examples/harness")),
    )
    .expect("answerer engine config");

    let replies = vec![
        // Parent answerer: fork two briefs, then finalize with the first
        // child's answer.
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
        // Child 0.
        reply(
            "```haskell\n\
             import HarnessTypes (Decision (..), Confidence (..))\n\
             \n\
             resume (Decision { action = \"observe\", rationale = \"child A\", \
             confidence = Low })\n\
             ```",
        ),
        // Child 1.
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
        std::env::temp_dir().join(format!("exact-context-fork-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    let source = load_harness_source(&repo_root().join("examples/harness/Harness.hs"))
        .expect("reference harness source loads");

    driver
        .run_one_cycle(&source, None)
        .await
        .expect("one render->loop->forkAll(2 children)->finalize->render cycle");

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
