//! Event-log schema: append-only jsonl, one file per run, fsync per
//! event. The header pins toolchain identity; replay is EFFECT-RESPONSE
//! SUBSTITUTION (re-run logged sources with logged responses injected), so
//! `Effect` events MUST record the response — a missing response breaks
//! restoration. This module is the wire contract; the sibling `writer`/
//! `reader` submodules and `crate::replay` implement the writer and the
//! replayer against it.
//!
//! Layout: [`LogHeader`] is the file's first line, unwrapped. Every
//! subsequent line is an [`EventRecord`] — the writer-assigned monotonic
//! `seq` envelope wrapping an [`Event`] (`{"seq": N, "event": {"ev": "...",
//! ...}}`). `seq` is nested rather than flattened deliberately:
//! `Event::Effect` already has its own `seq` field (the per-node effect
//! ordering the divergence check compares), and flattening the envelope
//! would collide the two same-named fields into one JSON object — a sharp
//! serde edge (`#[serde(flatten)]` + adjacent same-named fields silently
//! duplicates the key) rather than a real conflict, so the envelope stays
//! nested and unambiguous instead.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{Role, Usage};
use crate::snapshot::SnapshotDigest;
use crate::tree::{FanBadge, HoleId, NodeId, PriceClass, SiteId};

mod reader;
mod writer;

#[cfg(test)]
mod tests;

pub use reader::{EventIter, LogReader, ReadError};
pub use writer::{LogWriter, WriteError};

/// First line of every log file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogHeader {
    /// Content hash of the materialized stdlib/prelude.
    pub prelude_hash: String,
    /// Fingerprint of the extract binary (same one the compile cache keys on).
    pub extract_fingerprint: String,
    pub harness_version: String,
}

/// One jsonl line after the header: the writer-assigned monotonic
/// per-file `seq` wrapped around an [`Event`]. This is the additive
/// envelope the writer owns — `Event` itself is not reshaped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: u64,
    pub event: Event,
}

/// One jsonl line. `seq` ordering is per-file and total; per-node effect
/// ordering (`Effect::seq`) is the sequence the replayer substitutes
/// against and the divergence check compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    NodeCreated {
        node: NodeId,
        parent: Option<NodeId>,
        /// Harness-generated only — never program-authored.
        teaser: String,
        effect_row: Vec<String>,
        fan: FanBadge,
        price: PriceClass,
    },
    /// The ONLY work-begins event. Consent integrity = no Turn/Effect
    /// events for a node without a prior Forced (audited, literal zero).
    Forced {
        node: NodeId,
        actor: Actor,
    },
    TurnStart {
        node: NodeId,
        source: String,
        input: Option<Value>,
    },
    /// What extract said the just-compiled turn's holes and binds ARE:
    /// the `asks.json` sidecar's site → rendered-type pairs (each a
    /// `runLLMTurn`/`runLLMTurnFork`/`finalize` yield site in the compiled
    /// block), and — for a value-plane bind (`x <- e`) — the bound name and
    /// its rendered type. Emitted right after the compile that produced
    /// `TurnStart` for the same turn succeeds; `asks` is empty and `bound` is
    /// `None` when the turn has neither (most turns).
    TurnExtracted {
        node: NodeId,
        asks: Vec<(u32, String)>,
        bound: Option<(String, String)>,
    },
    Effect {
        node: NodeId,
        seq: u64,
        /// Effect tag name (from the stack declaration).
        tag: String,
        req: Value,
        resp: Value,
    },
    HolePublished {
        node: NodeId,
        hole: HoleId,
        site: Option<SiteId>,
        /// Pretty-printed answer type from the sidecar; None for
        /// schema-`ask` fast-path suspensions.
        ty: Option<String>,
        prompt: String,
        fork: bool,
    },
    HoleAnswerAttempt {
        node: NodeId,
        hole: HoleId,
        source: String,
        outcome: AnswerOutcome,
    },
    HoleConsumed {
        node: NodeId,
        hole: HoleId,
    },
    NodeDone {
        node: NodeId,
        result_rendered: String,
    },
    NodeCancelled {
        node: NodeId,
        reason: String,
    },
    /// One conversation-turn delta — the transcript is reconstructed by
    /// FOLDING these in `seq` order. A turn's `content` is the whole message.
    /// `turn` is the per-node monotonic turn index the transcript store
    /// assigns; a fork references a parent `(node, turn)` via
    /// [`Event::TurnForked`].
    TurnDelta {
        node: NodeId,
        turn: u64,
        role: Role,
        content: String,
        /// Present on assistant turns; `None` for the operator/system framing
        /// turns that cost no tokens.
        usage: Option<Usage>,
        /// The assistant turn's reasoning-summary ("thinking"), when the
        /// provider surfaced one. Optional + `serde(default)` so older logs
        /// (written before thinking capture) still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    /// A fork's transcript reference: the child `node` inherits the parent
    /// conversation up to and including the parent's turn at index
    /// `parent_turn` — a fork is a reference to the parent's position, not a
    /// copy. Reconstruction
    /// folds the parent's `TurnDelta`s with `turn <= parent_turn`, then the
    /// child's own.
    TurnForked {
        node: NodeId,
        parent: NodeId,
        parent_turn: u64,
    },
    /// An OPERATOR verb that interjects a message into `node`'s OWN
    /// transcript (never a different node's — unlike a fork's parent-position
    /// reference). `turn` is the SAME per-node monotonic turn-index space
    /// `TurnDelta` uses, landing at `node`'s CURRENT turn position, so
    /// transcript reconstruction folds it in exactly there. `role` is always
    /// `Role::User` (the model sees it like a user-turn nudge); a splice
    /// never carries `usage` (costs no tokens by construction).
    TurnSpliced {
        node: NodeId,
        turn: u64,
        role: Role,
        content: String,
    },
    /// `node`'s context prefix was FROZEN as a named cache root
    /// ([`crate::harness::Harness::freeze_snapshot`]). `digest` is the blake3
    /// identity of the exact prefix `engine::assemble_request` re-emits;
    /// `messages` is the frozen TRANSCRIPT length (the assembled prefix is one
    /// longer — the system framing message); `prefix_bytes` is the assembled
    /// prefix's total UTF-8 content bytes, framing included.
    ///
    /// Emitted once per DISTINCT snapshot: freezing an unchanged transcript
    /// again is idempotent and writes no second line. A node that is compacted
    /// after a freeze gets a SECOND `SnapshotFrozen` with a different digest —
    /// a new cache root, the old one still interned and still resolving for
    /// its existing children.
    SnapshotFrozen {
        node: NodeId,
        digest: SnapshotDigest,
        messages: u64,
        prefix_bytes: u64,
    },
    /// A branch minted by [`crate::harness::Harness::fork_from_snapshot`] ran
    /// its FIRST turn: what it shares with the frozen root and what it added.
    ///
    /// The byte counts are exact and locally recomputable. They are NOT a
    /// token split — there is no local tokenizer here, so `input_tokens` (the
    /// provider's own count for this turn's whole request) is carried
    /// alongside them rather than a derived estimate.
    ///
    /// `cached_input_tokens` is `Some` only when the provider's response
    /// actually reported a cache-read count. `None` means NOT REPORTED and is
    /// serialized as absent, never as `0` — see [`Usage::cached_input_tokens`]
    /// and `tidepool-harness/CLAUDE.md`'s "The provider cache-metric gap".
    BranchInvocation {
        node: NodeId,
        snapshot: SnapshotDigest,
        shared_prefix_bytes: u64,
        branch_suffix_bytes: u64,
        input_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cached_input_tokens: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Operator,
    /// Auto-forcing exists only behind the (R2) policy ladder; logged
    /// distinctly so consent audits can separate the regimes.
    Policy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AnswerOutcome {
    Consumed,
    /// Continuation NOT consumed; the error is the retry prompt.
    Rejected {
        error: String,
    },
}
