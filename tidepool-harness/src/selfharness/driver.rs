//! WS-A seam: the runtime driver spine — the outer `render`/`loop`
//! alternation (01/02-runtime.md) over the OUTER Harness-monad resident
//! session, servicing each `runLLMTurn` hole by driving a NESTED
//! [`crate::harness::Harness`] (an ordinary Agent node, reusing
//! `run_to_hole_or_done` — recon: `harness.rs:634-676,975-1016`) to a
//! `finalize` (WS-B) and `run_child`-ing the result back in-heap
//! (`resident.rs:407-465`) to resume `loop`.
//!
//! Frozen contract only: every method here is `unimplemented!()`. WS-A
//! builds the behavior; WS-C (`state_cross`), WS-D (`harness_source` +
//! `render_framing`'s wiring into `assemble_request`), and WS-E
//! (`compaction_trigger`'s threshold policy) build the seams this module
//! calls out to.
//!
//! ANTI-PATTERNS this seam is shaped to avoid (07-impl-orchestration.md
//! WS-A): don't reuse `tidepool-repl`'s parked-thread mechanism (dead end
//! once `fork` lands — [`SelfHarnessDriver::outer`] is `Threadless`, same
//! mechanism `Harness`'s own nodes use); don't reimplement the turn loop —
//! [`SelfHarnessDriver::service_runllm_hole`] reshapes
//! `Harness::run_to_hole_or_done`, it does not duplicate it; don't copy
//! heaps — the nested Agent's `finalize` value crosses via `run_child`,
//! zero-copy into the suspended outer session.

use std::sync::Arc;

use tidepool_eval::value::Value;
use tidepool_runtime::session::{PersistentSession, Threadless};

use crate::harness::{Harness, HarnessError};
use crate::provider::Usage;
use crate::selfharness::harness_source::HarnessSource;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::Observer;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("self-harness driver: {0}")]
    Session(String),
    #[error(transparent)]
    Agent(#[from] HarnessError),
}

/// The outer driver: owns the Harness-monad's own resident session
/// (`Eff '[RunLLMTurn]`, distinct from any Agent node's session) plus a
/// nested [`Harness`] used ONLY to answer `runLLMTurn` holes by driving an
/// Agent turn loop to `finalize`. One driver per running self-harness
/// process (`tidepool-selfharness`).
pub struct SelfHarnessDriver {
    /// The Harness-monad resident session. `None` before bootstrap; the
    /// scaffold does not yet name a concrete handler stack for the
    /// `RunLLMTurn`-only effect row (WS-B adds it) — WS-A instantiates this
    /// alongside that effect's landing.
    outer: Option<PersistentSession<Threadless>>,
    /// The nested multi-node orchestrator that answers a `runLLMTurn` hole
    /// by driving an Agent turn loop (`run_to_hole_or_done`) to a
    /// `finalize`. Shared, not owned exclusively, so a future GUI/inspector
    /// can observe the same node tree.
    agent: Arc<Harness>,
    lifecycle: SelfHarnessState,
    observer: Arc<dyn Observer>,
}

impl SelfHarnessDriver {
    /// Construct a driver over an already-booted [`Harness`] (the nested
    /// orchestrator for `runLLMTurn`-answering Agent sessions) and an event
    /// [`Observer`]. The outer Harness-monad session itself is not
    /// bootstrapped until [`Self::run_loop`] (it needs the loaded
    /// [`HarnessSource`] first).
    pub fn new(agent: Arc<Harness>, observer: Arc<dyn Observer>) -> Self {
        SelfHarnessDriver {
            outer: None,
            agent,
            lifecycle: SelfHarnessState::Idle,
            observer,
        }
    }

    /// The driver's current lifecycle state.
    pub fn lifecycle(&self) -> &SelfHarnessState {
        &self.lifecycle
    }

    /// Bootstrap the outer `PersistentSession<Threadless>` over `source`
    /// (`render`/`loop`/`State`, loaded once — see [`HarnessSource`]),
    /// restore the last persisted `State` if any (via
    /// [`crate::selfharness::state_cross::state_in`]), then run FOREVER:
    /// evaluate `render(state, lastCompaction)` ([`Self::render_framing`])
    /// as the next loop's system prompt, run `loop state` as a suspendable
    /// fragment (servicing each `runLLMTurn` hole via
    /// [`Self::service_runllm_hole`] and each compaction trigger via
    /// [`Self::compaction_trigger`]), serialize the returned `State`
    /// (`state_cross::state_out`), and restart-reload to pick up a new
    /// harness version between loops (02-runtime.md: no live hot-reload —
    /// the restart IS the reload mechanism). WS-A.
    pub fn run_loop(&mut self, source: &HarnessSource) -> Result<(), DriverError> {
        let _ = source;
        unimplemented!(
            "WS-A: bootstrap PersistentSession<Threadless> over `source`, alternate \
             render -> loop forever, restart-reload between loops"
        )
    }

    /// Service one `runLLMTurn @A` suspension (`site`/`ty` from
    /// [`crate::engine::HoleRouting::RunLLMTurn`]): register + force a fresh
    /// node on `self.agent`, drive it (`run_to_hole_or_done`) until it
    /// resolves via `finalize` (WS-B's effect — terminates the Agent turn
    /// loop rather than resuming it, per 03-agent-surface.md), then
    /// `run_child` the finalized value back into the OUTER session,
    /// zero-copy, to resume `loop`'s parked continuation. WS-A + WS-B.
    pub fn service_runllm_hole(
        &mut self,
        site: u32,
        ty: Option<&str>,
    ) -> Result<Value, DriverError> {
        let _ = (site, ty);
        unimplemented!(
            "WS-A: drive a nested Agent session on self.agent to a `finalize` (WS-B), \
             then run_child the value back into the outer session"
        )
    }

    /// Evaluate `render(state, lastCompaction)` against the outer session
    /// and return its `Text` result — the next loop's system prompt.
    /// Runtime-invoked at loop boundaries ONLY (02-runtime.md LOCKED); WS-D
    /// wires this into `assemble_request` as the system message. Seam lives
    /// here (not in `engine.rs`) so [`Self::run_loop`] can call it without
    /// depending on WS-D's `assemble_request` parameterization landing
    /// first.
    pub fn render_framing(
        &mut self,
        state_json: &serde_json::Value,
        last_compaction: Option<&str>,
    ) -> Result<String, DriverError> {
        let _ = (state_json, last_compaction);
        unimplemented!(
            "WS-D: evaluate render(state, lastCompaction) -> Text against the outer session"
        )
    }

    /// Runtime-owned emergency compaction check (02-runtime.md LOCKED: the
    /// *runtime* owns this trigger, never the loop). At `usage` past the
    /// configured threshold (~80% of `max_tokens`), force a "compact to
    /// text, target X tokens" turn and return its `Text`; `None` under
    /// threshold. The returned `Text` feeds the NEXT [`Self::render_framing`]
    /// call as `lastCompaction`. WS-E.
    pub fn compaction_trigger(
        &mut self,
        usage: &Usage,
        max_tokens: u32,
    ) -> Result<Option<String>, DriverError> {
        let _ = (usage, max_tokens);
        unimplemented!(
            "WS-E: ~80% token-usage check against max_tokens -> forced compact-to-text turn"
        )
    }

    /// Emit `event` to the configured [`Observer`] — the ONE place the
    /// driver touches the observer, so no call site hardwires logging or a
    /// future GUI push directly (WS-H anti-pattern guard).
    fn emit(&self, event: crate::selfharness::observer::Event) {
        self.observer.on_event(&event);
    }
}
