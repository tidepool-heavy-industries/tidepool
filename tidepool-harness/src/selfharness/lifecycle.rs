//! The self-harness [`driver`](crate::selfharness::driver)'s outer lifecycle
//! — one owned enum, transitioned atomically at the driver's dispatch
//! boundary: a single source of truth rather than state smeared across
//! booleans/`Option`s, so a composite condition (e.g. "servicing a hole
//! while a compaction is also due") has to be resolved into one variant
//! instead of going unrepresented.
//!
//! This is the OUTER hylo's lifecycle (one `render`→`loop` driver), distinct
//! from [`crate::tree::NodeState`] (one Agent node's lifecycle) — a single
//! `RunningLoop` tick can drive many Agent nodes through their own
//! `NodeState` machines as it services `runLLMTurn` holes.

/// The self-iterating harness driver's lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfHarnessState {
    /// Between loops: no `loop` fragment executing. The driver's next step
    /// is composing the system prompt (`render(state)` plus runtime-owned
    /// compaction/loop-metadata framing) followed by a fresh `loop state`.
    Idle,
    /// `loop state` is running as a suspendable fragment on the outer
    /// (Harness-monad) resident session.
    RunningLoop,
    /// `loop` suspended on a `runLLMTurn @A` hole; the driver is servicing it
    /// by driving a nested Agent session to a `finalize`
    /// ([`crate::selfharness::driver::SelfHarnessDriver::service_runllm_hole`]).
    SuspendedOnHole,
    /// The runtime-owned emergency compaction turn (~80% threshold) is
    /// running. Distinct from `RunningLoop` — the loop itself never enters
    /// this state; only the driver's forced trigger does.
    Compacting,
    /// A cycle's fallible body (`loop`, hole servicing, state serialization,
    /// or the post-loop render) raised an error. The driver has discarded
    /// every mutable resident component the failed cycle could have left
    /// behind — the per-loop answerer, its framing, the current cycle's
    /// compaction, the inference-call counter, and the outer resident
    /// session itself, which may have been parked mid-fragment on a hole —
    /// so none of it carries into the next cycle. Recoverable: the next
    /// `run_one_cycle` rebuilds the outer session from the harness source
    /// and proceeds normally.
    Failed { reason: String },
    /// Recovery from a `Failed` cycle could not rebuild a usable outer
    /// session. The driver holds no resident state it can trust, and its
    /// public entry points (`run_one_cycle`/`run_loop`/`restore`) refuse to
    /// run until a new driver is constructed.
    Poisoned { reason: String },
}
