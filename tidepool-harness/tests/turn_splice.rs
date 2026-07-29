//! Wave C leftovers: F2's `turn_spliced` operator verb, exercised end to end
//! through the real Harness (GHC-tier, record-replay — zero live model
//! calls). Splices an operator message into a FORK-ANSWERER CHILD's
//! transcript between its two scripted turns and asserts the child's very
//! NEXT prompt assembly carries it — "splice lands at the child's current
//! turn position, visible in its next prompt assembly."
//!
//! The script mirrors `golden_path.rs`'s proven fork-answerer shape (root
//! forks for an Int, the answerer's first attempt is deliberately ill-typed,
//! the second is valid) so the GHC-facing behavior is known-good; the only
//! addition is a `Harness::splice` call injected as a side effect of the
//! scripted provider's FIRST reply to the child, timed so it lands after
//! that reply's own (already-sent) prompt but before the child's next one.

use std::sync::{Arc, Mutex, OnceLock};

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{
    DynModelProvider, Message, ModelProvider, ProviderError, Role, TurnRequest, TurnResponse, Usage,
};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{ClassifiedHole, Harness, HoleRouting, TurnOutcome};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "splice".into(),
        extract_fingerprint: "splice".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 40,
        output_tokens: 8,
    }
}

const SPLICE_TEXT: &str = "operator: focus the retry on getting a real Int, ignore the string idea";

/// A scripted provider that ALSO performs one side effect at a fixed call
/// index: on the fork answerer child's FIRST turn (global call #1), it
/// splices an operator message into the child's OWN transcript (via a
/// `Harness` back-reference set right after construction — the provider must
/// exist before the harness does, so the reference is filled in after) before
/// returning its canned (deliberately ill-typed) reply. The child's SECOND
/// turn's outbound prompt (global call #2) is captured so the test can assert
/// the splice landed in it.
struct SpliceProbeProvider {
    harness: OnceLock<Arc<Harness>>,
    child: NodeId,
    call_index: Mutex<u32>,
    captured_second_prompt: Mutex<Option<Vec<Message>>>,
}

impl SpliceProbeProvider {
    fn new(child: NodeId) -> Self {
        SpliceProbeProvider {
            harness: OnceLock::new(),
            child,
            call_index: Mutex::new(0),
            captured_second_prompt: Mutex::new(None),
        }
    }

    fn set_harness(&self, harness: Arc<Harness>) {
        self.harness.set(harness).ok().expect("set_harness called once");
    }
}

impl ModelProvider for SpliceProbeProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<tidepool_harness::provider::StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let idx = {
            let mut n = self.call_index.lock().unwrap();
            let i = *n;
            *n += 1;
            i
        };
        match idx {
            // 0. Root's opening turn: fork for a number.
            0 => Ok(TurnResponse {
                text: "I'll get a number from a sub-agent, then finish.\n\n\
                       ```haskell\n\
                       do\n\
                       \x20 n <- returnControlFork @Int \"pick a number between 1 and 100\"\n\
                       \x20 pure (toJSON n)\n\
                       ```"
                    .to_string(),
                usage: usage(),
                reasoning: None,
            }),
            // 1. Child's FIRST turn: splice an operator note into the CHILD's
            //    own transcript, then answer ill-typed (forcing a GHC-verbatim
            //    retry — the retry is the "next prompt assembly" that must
            //    carry the splice).
            1 => {
                let harness = self.harness.get().expect("harness set before first call");
                harness
                    .splice(self.child, SPLICE_TEXT)
                    .expect("splice into the live child");
                Ok(TurnResponse {
                    text: "```haskell\nresume \"forty-two\"\n```".to_string(),
                    usage: usage(),
                    reasoning: None,
                })
            }
            // 2. Child's SECOND turn: capture the outbound prompt (the
            //    child's next prompt assembly after the splice), then answer
            //    correctly.
            2 => {
                *self.captured_second_prompt.lock().unwrap() = Some(req.messages.clone());
                Ok(TurnResponse {
                    text: "Right, an Int.\n\n```haskell\nresume (42 :: Int)\n```".to_string(),
                    usage: usage(),
                    reasoning: None,
                })
            }
            other => Err(ProviderError::Api(format!(
                "unexpected extra provider call #{other}"
            ))),
        }
    }
}

fn events_for(log_path: &std::path::Path, node: NodeId) -> Vec<Event> {
    let (_h, events) = LogReader::open(log_path).expect("open log");
    events
        .map(|r| r.expect("well-formed record").event)
        .filter(|e| event_node(e) == Some(node))
        .collect()
}

fn event_node(e: &Event) -> Option<NodeId> {
    match e {
        Event::NodeCreated { node, .. }
        | Event::Forced { node, .. }
        | Event::TurnStart { node, .. }
        | Event::Effect { node, .. }
        | Event::HolePublished { node, .. }
        | Event::HoleAnswerAttempt { node, .. }
        | Event::HoleConsumed { node, .. }
        | Event::NodeDone { node, .. }
        | Event::NodeCancelled { node, .. }
        | Event::TurnDelta { node, .. }
        | Event::TurnForked { node, .. }
        | Event::TurnSpliced { node, .. } => Some(*node),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn splice_lands_in_childs_next_prompt_assembly() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("splice.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    // The fork-answerer child is minted right after the root (NodeId(0)) —
    // `register_fork_child` (private, invoked inside `answer_fork`) mints the
    // very next id, deterministic since this test creates no other node.
    let child = NodeId(1);
    let probe = Arc::new(SpliceProbeProvider::new(child));
    let provider: Arc<dyn DynModelProvider> = probe.clone();
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    probe.set_harness(harness.clone());

    let root = harness
        .create_root("splice root", "Get a number from a sub-agent, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    assert!(
        matches!(
            outcome,
            TurnOutcome::Suspended {
                classified: ClassifiedHole {
                    routing: HoleRouting::Fork { .. },
                    ..
                },
                ..
            }
        ),
        "root should suspend on a FORK hole"
    );

    let answered_child = harness
        .answer_fork(root, Actor::Operator)
        .await
        .expect("fork answered end to end incl. the spliced-in retry");
    assert_eq!(
        answered_child, child,
        "the fork answerer must be the predicted NodeId(1)"
    );
    assert_eq!(harness.tree().state(child), Some(NodeState::Done));
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the parent's continuation (a single `pure (toJSON n)`) completes once resumed"
    );

    // The child's SECOND prompt (captured by the probe) carries the spliced
    // message — "visible in its next prompt assembly".
    let second_prompt = probe
        .captured_second_prompt
        .lock()
        .unwrap()
        .clone()
        .expect("the child's second turn must have been driven");
    assert!(
        second_prompt
            .iter()
            .any(|m| m.role == Role::User && m.content == SPLICE_TEXT),
        "the child's next prompt after the splice must include the spliced operator \
         message, got: {second_prompt:?}"
    );

    // The durable log carries a distinct TurnSpliced event for the child (not
    // a TurnDelta) — audit-distinguishable from a modeled turn.
    let events = events_for(&log_path, child);
    let spliced: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::TurnSpliced { .. }))
        .collect();
    assert_eq!(spliced.len(), 1, "exactly one splice landed on the child, got {spliced:?}");
    let Event::TurnSpliced { content, role, .. } = spliced[0] else {
        unreachable!()
    };
    assert_eq!(content, SPLICE_TEXT);
    assert_eq!(*role, Role::User);
}
