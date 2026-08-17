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
pub struct SiteId(u32);

impl SiteId {
    pub fn get(self) -> u32 {
        self.0
    }
}

impl std::convert::TryFrom<u64> for SiteId {
    type Error = std::num::TryFromIntError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        u32::try_from(value).map(SiteId)
    }
}

/// A validated fan-out cardinality from the classify seam's `fan` wire field
/// (`RunLLMTurnWith`'s `fork`/`fan` payload) — same minting discipline as
/// [`SiteId`]: only the fallible `TryFrom<u64>` boundary constructor, no
/// public field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FanCount(u32);

impl FanCount {
    pub fn get(self) -> u32 {
        self.0
    }
}

impl std::convert::TryFrom<u64> for FanCount {
    type Error = std::num::TryFromIntError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        u32::try_from(value).map(FanCount)
    }
}

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

/// Resident-session registry slot. Generic over the machine handle so this
/// crate stays free of the JIT dependency — a caller instantiates `M` with
/// its concrete resident-session type. The stowed-XOR-running discipline
/// (jit_machine.rs Send rationale) maps onto these variants: a machine is
/// in exactly one slot, and `Running{holes}` means it is out on a turn
/// (whatever kind — a fresh run, a resume, or a child run over parked
/// frames), carrying its parked holes with it.
#[derive(Debug)]
pub enum Slot<M> {
    Idle(M),
    /// The machine is out on a turn — a fresh run, a resume, or a child run
    /// over parked frames (one-session plan, Phase 2: with the continuation
    /// registry there is no special "child window"; a machine with N parked
    /// holes running one more fragment is the NORMAL state). `holes` are the
    /// parked holes the session had when it left, carried so reads and
    /// errors stay truthful while the machine is out, and so the
    /// panic-safety `Drop` can restore them instead of losing them.
    Running {
        holes: Vec<HoleId>,
    },
    /// The machine is present with one or more parked holes, each resumable
    /// by identity in any order (the machine's continuation registry imposes
    /// none). Newest last.
    Suspended {
        machine: M,
        holes: Vec<HoleId>,
    },
}
