//! Event-log schema (E4): append-only jsonl, one file per run, fsync per
//! event. The header pins toolchain identity; replay is EFFECT-RESPONSE
//! SUBSTITUTION (re-run logged sources with logged responses injected), so
//! `Effect` events MUST record the response — a missing response breaks
//! restoration. Segment 30 C1 implements the writer, C2 the replayer;
//! this module is the wire contract.
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

use crate::tree::{FanBadge, HoleId, NodeId, PriceClass, SiteId};

mod reader;
mod writer;

#[cfg(test)]
mod tests;

pub use reader::{EventIter, Follower, LogReader, ReadError};
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
/// envelope C1 owns — `Event` itself is not reshaped.
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
        /// Harness-generated only (C7) — never program-authored.
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
