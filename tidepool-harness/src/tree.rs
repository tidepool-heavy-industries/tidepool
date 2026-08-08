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
/// rewrite, key into the `asks.json` sidecar
/// (plans/harness-r0/10-extract-pass/SPEC.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SiteId(pub u32);

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

/// Pre-force price class. DRAFT granularity — segment 30 (forcing)
/// may refine; the contract is that a class exists and renders as a badge.
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
/// crate stays free of the JIT dependency — segment 20 instantiates `M`
/// with its resident-session type. The stowed-XOR-running discipline
/// (jit_machine.rs Send rationale) maps onto these variants: a machine is
/// in exactly one slot, and `Running`/`RunningChild` means it is out on a turn.
#[derive(Debug)]
pub enum Slot<M> {
    Idle(M),
    Running,
    Suspended {
        machine: M,
        hole: HoleId,
    },
    /// Segment 40: the machine is out on a NESTED CHILD run against a suspended
    /// parent — the parent is still suspended on `hole`, and the child restores
    /// the slot back to `Suspended { hole }` on completion. A parent
    /// resume/abort or a new top-level run is rejected while in this state
    /// (sequential-isolated: exactly one computation on the heap at a time).
    RunningChild {
        hole: HoleId,
    },
}
