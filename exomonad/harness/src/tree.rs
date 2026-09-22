//! Session-tree vocabulary: nodes, states, holes, badges.
//!
//! A NODE is a branch of the cognition tree — a thunk until forced (forcing
//! is the only way work begins), a resident session once running.
//! A HOLE is a published typed suspension (`runLLMTurn @T`): the model's
//! next task, the operator's next form, and the approval gate, all at once.

use serde::{Deserialize, Serialize};

/// Tree-node identity, minted by the harness. Distinct from
/// [`tidepool_repr::SessionId`], which a node acquires only when forced
/// (thunk nodes have no session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

/// A published typed suspension's continuation id (the engine's
/// `scont_N`-style string — opaque here).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HoleId(pub String);

/// Extract-time yield-site id: the literal threaded by the head-swap
/// rewrite, key into the `asks.json` sidecar.
///
/// Minted ONLY via the fallible `TryFrom<u64>` boundary constructor, at the
/// classify seam (`engine::classify_hole`, over a wire `u64`/JSON number
/// straight from a suspended request) — never via a public field a caller
/// could hand-construct from an unvalidated integer. An out-of-range wire
/// value fails there as a typed `ClassifyError`, so a `SiteId` in hand is
/// always one `asks.json` genuinely indexes; a raw `u32`/`u64` past this
/// point is structurally unrepresentable as a site id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SiteId(u64);

impl SiteId {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::convert::TryFrom<u64> for SiteId {
    type Error = SiteIdOutOfRange;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value <= i64::MAX as u64 {
            Ok(SiteId(value))
        } else {
            Err(SiteIdOutOfRange)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteIdOutOfRange;

/// Node lifecycle. Who a suspended hole is routed to (model child vs
/// operator) is a property of the hole, not the node — the observatory's
/// waiting-on-operator glyph derives from hole routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NodeState {
    /// Unforced: no session, no tokens, no effects.
    Thunk,
    Running,
    Suspended {
        hole: HoleId,
    },
    Done,
    Cancelled {
        reason: String,
    },
}

/// Pre-force fan-out badge, three-valued: `Dynamic`
/// converts to `Exact` at materialization and re-checks forcing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fan", rename_all = "snake_case")]
pub enum FanBadge {
    Exact { n: u32 },
    Bounded { max: u32 },
    Dynamic,
}

/// Pre-force price class. Coarse-grained by design: the contract is only
/// that a class always exists and renders as a badge, not that this
/// taxonomy is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceClass {
    /// No model calls possible in this branch's effect row.
    Zero,
    /// In-program `Llm` effect only.
    Llm,
    /// Spawns calling-model (frontier) turns.
    Frontier,
}

/// Resident-session registry slot, instantiated at the harness's hole
/// identity type. The mechanism itself — `Idle(M) | Running{holes} |
/// Suspended{machine,holes} | Wedged{since}`, the stowed-XOR-running
/// discipline, the epoch guard — lives in
/// `tidepool_runtime::session::registry` (the one promoted home, see the
/// root `CLAUDE.md` Mechanism Index); this crate never constructs
/// `Slot::Wedged` (a wedged turn here retires the whole node via
/// `Harness::terminate_node` instead), it simply never sees that variant.
pub type Slot<M> = tidepool_runtime::session::registry::Slot<M, HoleId>;
