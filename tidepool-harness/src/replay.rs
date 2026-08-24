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
//! The provider is order-only (a queue of assistant replies) for a
//! deterministic single-thread golden path, which emits its turns in a fixed
//! sequence — but that guarantee does not survive genuine overlap: once more
//! than one in-flight session can call `complete` concurrently (the
//! answerer-plane green scheduler driving several fork children at once),
//! requests reach this provider in a NONDETERMINISTIC order, and plain FIFO
//! can no longer promise "reply N answers request N". [`ReplayProvider::new_keyed`]
//! covers that case: a reply tagged with a CONTENT needle is matched to
//! whichever request's rendered text contains it, wherever that reply sits
//! in the queue — see its doc.
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

use parking_lot::Mutex;

use crate::log::{Event, LogReader, ReadError};
use crate::provider::{
    Message, ModelProvider, ProviderError, Role, TurnRequest, TurnResponse, Usage,
};
use crate::tree::{NodeId, NodeState};

/// One recorded assistant reply (the text a live model produced, plus its
/// usage) — replayed verbatim. Replay is strictly queue-order-only, so no
/// node/turn identity rides along — see the module doc.
#[derive(Debug, Clone)]
pub struct RecordedReply {
    pub content: String,
    pub usage: Usage,
}

/// One queued reply plus its optional CONTENT match key — see
/// [`ReplayProvider::new_keyed`].
struct QueueEntry {
    reply: RecordedReply,
    needle: Option<String>,
}

/// A [`ModelProvider`] that serves assistant replies from a recorded log.
/// Panics-free: an exhausted queue returns a typed `ProviderError` so a
/// replay that runs longer than the recording fails loud, not silently.
///
/// Also a CONCURRENCY RECEIPT: every call tracks how many `complete` calls
/// are simultaneously in flight (`Self::max_concurrent`), so a caller driving
/// several sessions against ONE `ReplayProvider` can assert genuine overlap
/// happened rather than one-request-at-a-time — see `Self::with_hold`.
pub struct ReplayProvider {
    queue: Mutex<std::collections::VecDeque<QueueEntry>>,
    /// An artificial hold each `complete` call awaits before resolving —
    /// zero (every construction here) is a no-op costing nothing; only a
    /// caller that opts in via `Self::with_hold` pays it. Needed because a
    /// replayed reply otherwise resolves with no `.await` point at all, so
    /// two concurrent callers have no window to actually overlap ON: a race
    /// that never blocks can complete each call within a single poll,
    /// leaving `max_concurrent` unable to observe overlap that is real at
    /// the scheduler level but invisible to this mock.
    hold: std::time::Duration,
    in_flight: std::sync::atomic::AtomicUsize,
    max_in_flight: std::sync::atomic::AtomicUsize,
}

impl ReplayProvider {
    /// Build a replay provider from an ordered list of recorded replies,
    /// served strictly in order — correct only for a deterministic
    /// single-thread golden path (see the module doc). Every entry is
    /// FIFO-only (no needle); behaviorally identical to every construction
    /// this crate made before [`Self::new_keyed`] existed.
    pub fn new(replies: Vec<RecordedReply>) -> Self {
        ReplayProvider {
            queue: Mutex::new(
                replies
                    .into_iter()
                    .map(|reply| QueueEntry {
                        reply,
                        needle: None,
                    })
                    .collect(),
            ),
            hold: std::time::Duration::ZERO,
            in_flight: std::sync::atomic::AtomicUsize::new(0),
            max_in_flight: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Build a replay provider from entries that may each carry a CONTENT
    /// match key: `(Some(needle), reply)` is served to whichever request's
    /// rendered text (any message's content) CONTAINS `needle`, regardless
    /// of the entry's position in the queue — required once more than one
    /// session can call `complete` concurrently, where request arrival order
    /// at this mock is no longer meaningful (see the module doc).
    /// `(None, reply)` keeps the plain FIFO behavior [`Self::new`] gives
    /// every entry, and is served only once no queued entry's needle matches
    /// the current request — so a queue mixing keyed and unkeyed entries is
    /// exactly "the keyed ones are found by content; everything else is
    /// still FIFO."
    ///
    /// Pick a needle that cannot appear ANYWHERE else in a rendered prompt —
    /// shared framing text (an effect row's own doc strings, boilerplate
    /// every turn carries) is real prose and can incidentally contain a
    /// short/generic phrase (`"pick a"` collided with `Tidepool.Form`'s own
    /// "pick a subset" doc line, live, before this note existed). Prefer a
    /// needle scoped to the exact rendered shape only the intended request
    /// carries — e.g. a fork child's own brief, blank-line-delimited exactly
    /// as its hole card renders it — over a bare word.
    pub fn new_keyed(entries: Vec<(Option<String>, RecordedReply)>) -> Self {
        ReplayProvider {
            queue: Mutex::new(
                entries
                    .into_iter()
                    .map(|(needle, reply)| QueueEntry { reply, needle })
                    .collect(),
            ),
            hold: std::time::Duration::ZERO,
            in_flight: std::sync::atomic::AtomicUsize::new(0),
            max_in_flight: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Append more keyed/unkeyed entries to an ALREADY-CONSTRUCTED provider
    /// — for a caller driving several cycles/rounds against ONE provider
    /// (machine-rotation and other multi-cycle acceptance tests) where a
    /// LATER cycle reuses the same needle text an EARLIER cycle's child
    /// used (e.g. the same brief, "pick a", spawned again next cycle): if
    /// every cycle's entries were queued upfront, an earlier cycle's request
    /// could match a LATER cycle's identically-worded reply before that
    /// cycle's own request ever exists — observed live in a fixture where an
    /// intentionally-unkeyed (starving) child's request matched a next
    /// cycle's keyed reply for the same brief text, stealing its answer.
    /// Queue only the CURRENT cycle's entries at construction and append the
    /// next cycle's here right before driving it, so a needle can only ever
    /// match a reply that is actually, chronologically, its own.
    pub fn extend_keyed(&self, entries: Vec<(Option<String>, RecordedReply)>) {
        self.queue.lock().extend(
            entries
                .into_iter()
                .map(|(needle, reply)| QueueEntry { reply, needle }),
        );
    }

    /// Hold every `complete` call open for `dur` before it resolves — an
    /// opt-in CONCURRENCY PROBE (see the struct doc for why one is needed at
    /// all), never a golden-path timing behavior. `Duration::ZERO` (every
    /// constructor's default) is a no-op.
    pub fn with_hold(mut self, dur: std::time::Duration) -> Self {
        self.hold = dur;
        self
    }

    /// The highest number of `complete` calls ever simultaneously in
    /// flight — the concurrency receipt: `> 1` proves two requests genuinely
    /// overlapped rather than running strictly one at a time.
    pub fn max_concurrent(&self) -> usize {
        self.max_in_flight.load(std::sync::atomic::Ordering::SeqCst)
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
                role: Role::Assistant,
                content,
                usage,
                ..
            } = record.event
            {
                replies.push(RecordedReply {
                    content,
                    // DEFERRED, not fixed: a recorded `None` here means the
                    // source log genuinely has no usage for this turn, and
                    // per the cache-metric-gap discipline
                    // (tidepool-harness/CLAUDE.md) that should replay as
                    // `None`, not a fabricated zero. It can't today —
                    // `RecordedReply.usage`/`TurnResponse.usage` are bare
                    // `Usage`, so purifying this one call site needs
                    // `TurnResponse.usage: Option<Usage>` threaded through
                    // ~19 construction sites (every mock `ModelProvider` in
                    // tests, plus oauth.rs/http.rs) and every unconditional
                    // `driven.usage` read in harness.rs's accounting. Left
                    // as `unwrap_or_default()` because the branch is
                    // UNREACHABLE from any log this codebase actually
                    // writes: every live write site logs
                    // `Some(driven.usage)` unconditionally, so a real
                    // recorded assistant `TurnDelta.usage` is never `None`
                    // — only a hand-built test fixture can hit this.
                    usage: usage.unwrap_or_default(),
                });
            }
        }
        Ok(ReplayProvider::new(replies))
    }

    /// How many replies remain unserved.
    pub fn remaining(&self) -> usize {
        self.queue.lock().len()
    }
}

/// Decrements [`ReplayProvider::in_flight`] on every exit from `complete`
/// (success, an exhausted-queue error, or a future dropped mid-hold) — a
/// plain counter without this would over-count on the very first early
/// return.
struct InFlightGuard<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl ModelProvider for ReplayProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        sink: Option<crate::provider::StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let now = self
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.max_in_flight
            .fetch_max(now, std::sync::atomic::Ordering::SeqCst);
        let _guard = InFlightGuard(&self.in_flight);
        if !self.hold.is_zero() {
            tokio::time::sleep(self.hold).await;
        }
        let reply = {
            let mut queue = self.queue.lock();
            // Content-keyed match first (see the module doc and
            // `Self::new_keyed`): the FIRST queue entry (in queue order)
            // whose needle appears in any message of THIS request wins,
            // regardless of position. A keyed entry is reserved for its own
            // matcher — an unrelated request that matches NO needle falls
            // back to the first UNKEYED entry, never to a keyed one sitting
            // ahead of it in the queue (a plain `pop_front()` fallback would
            // let an unrelated concurrent request steal a reply some OTHER
            // request's needle was reserving). When every entry is
            // FIFO-only (no needles at all — every construction through
            // `Self::new`), this is bit-identical to a plain `pop_front()`.
            let idx = queue
                .iter()
                .position(|e| {
                    e.needle
                        .as_deref()
                        .is_some_and(|n| req.messages.iter().any(|m| m.content.contains(n)))
                })
                .or_else(|| queue.iter().position(|e| e.needle.is_none()));
            idx.and_then(|i| queue.remove(i))
        }
        .ok_or_else(|| ProviderError::Api("replay queue exhausted".to_string()))?
        .reply;
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

/// Reconstructed per-node state after folding a log — an OFFLINE read, not
/// the startup recovery path (see the module doc).
#[derive(Debug, Clone, Default)]
pub struct FoldedTree {
    /// Terminal state per node.
    pub states: HashMap<NodeId, NodeState>,
    /// Parent per node (root nodes map to `None`).
    pub parents: HashMap<NodeId, Option<NodeId>>,
    /// The transcript per node, reconstructed by folding `TurnDelta`s.
    pub transcripts: HashMap<NodeId, Vec<Message>>,
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
        // against, not folded into node lifecycle here). TurnForked is fork
        // PROVENANCE, not lifecycle state. SnapshotFrozen / BranchInvocation
        // are RECEIPTS about a context prefix, not a node lifecycle state.
        Event::TurnStart { .. }
        | Event::Effect { .. }
        | Event::HoleAnswerAttempt { .. }
        | Event::TurnExtracted { .. }
        | Event::TurnForked { .. }
        | Event::SnapshotFrozen { .. }
        | Event::BranchInvocation { .. } => {}
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
                cached_input_tokens: None,
                cache_write_tokens: None,
            }),
        })
        .unwrap();
        w.append(Event::HolePublished {
            node: NodeId(0),
            hole: HoleId("scont_1".into()),
            site: Some(crate::tree::SiteId::try_from(0u64).unwrap()),
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
