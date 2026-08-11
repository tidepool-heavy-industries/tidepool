//! Record-replay provider + crash-replay.
//!
//! # Record-replay provider
//!
//! [`ReplayProvider`] is a [`ModelProvider`] that answers turns from a
//! PRE-RECORDED log instead of calling a live model. In LIVE mode the harness
//! logs each assistant turn (`TurnDelta` with `role = Assistant`); in REPLAY
//! mode this provider hands those SAME assistant replies back in order, so a CI
//! run re-drives the exact golden path with zero API calls — the turns ARE log
//! events.
//!
//! The provider is intentionally order-only (a queue of assistant replies) —
//! a deterministic single-thread golden path emits its turns in a fixed
//! sequence.
//!
//! # Offline log inspection (crash-replay tree reconstruction)
//!
//! [`fold_tree_state`] folds a log's events into the terminal per-node
//! [`NodeState`] + tree structure, so a finished or crashed run's browsable
//! history tree can be reconstructed from the durable log alone. It is
//! explicitly NOT the startup recovery path: a `SelfHarnessDriver` restores
//! from `persistence::Checkpoint` at boot, not by folding the log — a second
//! recovery source could disagree with the checkpoint about a run's state.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use crate::log::{Event, LogReader, ReadError};
use crate::provider::{
    Message, ModelProvider, ProviderError, Role, TurnRequest, TurnResponse, Usage,
};
use crate::tree::{NodeId, NodeState};

/// One recorded assistant reply (the text a live model produced, plus its
/// usage) — replayed verbatim.
#[derive(Debug, Clone)]
pub struct RecordedReply {
    pub node: NodeId,
    pub turn: u64,
    pub content: String,
    pub usage: Usage,
}

/// A [`ModelProvider`] that serves assistant replies from a recorded log, in
/// order. Panics-free: an exhausted queue returns a typed `ProviderError` so a
/// replay that runs longer than the recording fails loud, not silently.
pub struct ReplayProvider {
    queue: Mutex<std::collections::VecDeque<RecordedReply>>,
}

impl ReplayProvider {
    /// Build a replay provider from an ordered list of recorded replies.
    pub fn new(replies: Vec<RecordedReply>) -> Self {
        ReplayProvider {
            queue: Mutex::new(replies.into_iter().collect()),
        }
    }

    /// Load the recorded assistant replies from a log file, in `seq` order.
    /// Only `TurnDelta` events with `role = Assistant` are replayable turns
    /// (user/operator framing turns cost no model call).
    pub fn from_log(path: impl AsRef<Path>) -> Result<Self, ReadError> {
        let (_header, events) = LogReader::open(path)?;
        let mut replies = Vec::new();
        for record in events {
            let record = record?;
            if let Event::TurnDelta {
                node,
                turn,
                role: Role::Assistant,
                content,
                usage,
                ..
            } = record.event
            {
                replies.push(RecordedReply {
                    node,
                    turn,
                    content,
                    usage: usage.unwrap_or(Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                    }),
                });
            }
        }
        Ok(ReplayProvider::new(replies))
    }

    /// How many replies remain unserved.
    pub fn remaining(&self) -> usize {
        self.queue.lock().unwrap().len()
    }
}

impl ModelProvider for ReplayProvider {
    async fn complete(
        &self,
        _req: TurnRequest,
        sink: Option<crate::provider::StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let reply = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::Api("replay queue exhausted".to_string()))?;
        // Replay has no real stream; emit the recorded reply as one delta so a
        // watching observatory still sees the turn appear.
        if let Some(s) = &sink {
            let _ = s.send(crate::provider::StreamDelta::Text(reply.content.clone()));
        }
        Ok(TurnResponse {
            text: reply.content,
            usage: reply.usage,
            reasoning: None,
            reasoning_items: Vec::new(), // replay never has live reasoning data
        })
    }
}

/// A provider that RECORDS a live provider's replies into a side channel while
/// passing them through — the "live mode logs" half. In the harness, logging
/// happens via the event log (`TurnDelta`), so this wrapper is only needed when
/// a caller wants an in-memory capture too (e.g. a test that records then
/// replays in one process without a file round-trip).
pub struct RecordingProvider<P> {
    inner: P,
    captured: Mutex<Vec<String>>,
}

impl<P> RecordingProvider<P> {
    pub fn new(inner: P) -> Self {
        RecordingProvider {
            inner,
            captured: Mutex::new(Vec::new()),
        }
    }

    /// The replies captured so far, in order.
    pub fn captured(&self) -> Vec<String> {
        self.captured.lock().unwrap().clone()
    }
}

impl<P: ModelProvider> ModelProvider for RecordingProvider<P> {
    async fn complete(
        &self,
        req: TurnRequest,
        sink: Option<crate::provider::StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let resp = self.inner.complete(req, sink).await?;
        self.captured.lock().unwrap().push(resp.text.clone());
        Ok(resp)
    }
}

/// Reconstructed per-node state after folding a log — an OFFLINE read, not
/// the startup recovery path (see the module doc).
#[derive(Debug, Clone, Default)]
pub struct FoldedTree {
    /// Terminal state per node.
    pub states: HashMap<NodeId, NodeState>,
    /// Parent per node (root nodes map to `None`).
    pub parents: HashMap<NodeId, Option<NodeId>>,
    /// Children per node, in creation order.
    pub children: HashMap<NodeId, Vec<NodeId>>,
    /// The transcript per node, reconstructed by folding `TurnDelta`s.
    pub transcripts: HashMap<NodeId, Vec<Message>>,
    /// Fork references: child → (parent, parent_turn).
    pub forks: HashMap<NodeId, (NodeId, u64)>,
}

/// Fold a log file's events into the terminal tree state — an OFFLINE read,
/// NOT the path a driver restores from on boot (see the module doc).
/// Divergence-tolerant: unknown-ordering is impossible (the log is
/// total-ordered by `seq`), and a torn tail is already dropped by the reader.
pub fn fold_tree_state(path: impl AsRef<Path>) -> Result<FoldedTree, ReadError> {
    let (_header, events) = LogReader::open(path)?;
    let mut folded = FoldedTree::default();
    for record in events {
        let record = record?;
        apply_event(&mut folded, record.event);
    }
    Ok(folded)
}

/// Apply one event to the folded state. Public so a live harness can share the
/// exact fold logic with the crash-replay path (one code path, no divergence).
pub fn apply_event(folded: &mut FoldedTree, event: Event) {
    match event {
        Event::NodeCreated { node, parent, .. } => {
            folded.states.insert(node, NodeState::Thunk);
            folded.parents.insert(node, parent);
            folded.children.entry(node).or_default();
            if let Some(p) = parent {
                folded.children.entry(p).or_default().push(node);
            }
        }
        Event::Forced { node, .. } => {
            folded.states.insert(node, NodeState::Running);
        }
        Event::HolePublished { node, hole, .. } => {
            folded.states.insert(node, NodeState::Suspended { hole });
        }
        Event::HoleConsumed { node, .. } => {
            folded.states.insert(node, NodeState::Running);
        }
        Event::NodeDone { node, .. } => {
            folded.states.insert(node, NodeState::Done);
        }
        Event::NodeCancelled { node, reason } => {
            folded.states.insert(node, NodeState::Cancelled { reason });
        }
        Event::TurnDelta {
            node,
            role,
            content,
            ..
        } => {
            folded.transcripts.entry(node).or_default().push(Message {
                role,
                content,
                reasoning_items: Vec::new(), // the durable log never carries them
            });
        }
        Event::TurnForked {
            node,
            parent,
            parent_turn,
        } => {
            folded.forks.insert(node, (parent, parent_turn));
        }
        Event::TurnSpliced {
            node,
            role,
            content,
            ..
        } => {
            // Folded exactly like a `TurnDelta`: a splice is transcript
            // content the child sees on its next prompt assembly, whatever
            // the audit trail calls it.
            folded.transcripts.entry(node).or_default().push(Message {
                role,
                content,
                reasoning_items: Vec::new(), // the durable log never carries them
            });
        }
        // TurnStart / Effect / HoleAnswerAttempt / TurnExtracted do not change
        // tree STATE (they are within-turn detail the replayer substitutes
        // against, not folded into node lifecycle here).
        Event::TurnStart { .. }
        | Event::Effect { .. }
        | Event::HoleAnswerAttempt { .. }
        | Event::TurnExtracted { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Actor, LogHeader, LogWriter};
    use crate::tree::HoleId;

    fn header() -> LogHeader {
        LogHeader {
            prelude_hash: "p".into(),
            extract_fingerprint: "e".into(),
            harness_version: "h".into(),
        }
    }

    #[test]
    fn fold_reconstructs_terminal_states() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.jsonl");
        let mut w = LogWriter::create(&path, &header()).unwrap();
        w.append(Event::NodeCreated {
            node: NodeId(0),
            parent: None,
            teaser: "root".into(),
            effect_row: vec![],
            fan: crate::tree::FanBadge::Exact { n: 0 },
            price: crate::tree::PriceClass::Frontier,
        })
        .unwrap();
        w.append(Event::Forced {
            node: NodeId(0),
            actor: Actor::Operator,
        })
        .unwrap();
        w.append(Event::HolePublished {
            node: NodeId(0),
            hole: HoleId("scont_1".into()),
            site: None,
            ty: Some("Verdict".into()),
            prompt: "?".into(),
            fork: true,
        })
        .unwrap();
        w.append(Event::HoleConsumed {
            node: NodeId(0),
            hole: HoleId("scont_1".into()),
        })
        .unwrap();
        w.append(Event::NodeDone {
            node: NodeId(0),
            result_rendered: "42".into(),
        })
        .unwrap();

        let folded = fold_tree_state(&path).unwrap();
        assert_eq!(folded.states.get(&NodeId(0)), Some(&NodeState::Done));
        assert_eq!(folded.parents.get(&NodeId(0)), Some(&None));
    }

    #[test]
    fn fold_reconstructs_suspended_state_and_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.jsonl");
        let mut w = LogWriter::create(&path, &header()).unwrap();
        w.append(Event::NodeCreated {
            node: NodeId(0),
            parent: None,
            teaser: "root".into(),
            effect_row: vec![],
            fan: crate::tree::FanBadge::Exact { n: 0 },
            price: crate::tree::PriceClass::Frontier,
        })
        .unwrap();
        w.append(Event::Forced {
            node: NodeId(0),
            actor: Actor::Operator,
        })
        .unwrap();
        w.append(Event::TurnDelta {
            node: NodeId(0),
            turn: 0,
            role: Role::User,
            content: "go".into(),
            usage: None,
            reasoning: None,
        })
        .unwrap();
        w.append(Event::TurnDelta {
            node: NodeId(0),
            turn: 1,
            role: Role::Assistant,
            content: "```haskell\nrunLLMTurnFork @Int \"n\"\n```".into(),
            reasoning: None,
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 20,
            }),
        })
        .unwrap();
        w.append(Event::HolePublished {
            node: NodeId(0),
            hole: HoleId("scont_1".into()),
            site: Some(crate::tree::SiteId(0)),
            ty: Some("Int".into()),
            prompt: "n".into(),
            fork: true,
        })
        .unwrap();

        let folded = fold_tree_state(&path).unwrap();
        assert_eq!(
            folded.states.get(&NodeId(0)),
            Some(&NodeState::Suspended {
                hole: HoleId("scont_1".into())
            })
        );
        assert_eq!(folded.transcripts.get(&NodeId(0)).map(Vec::len), Some(2));

        // The replay provider serves the one recorded assistant turn.
        let replay = ReplayProvider::from_log(&path).unwrap();
        assert_eq!(replay.remaining(), 1);
    }
}
