//! One worktree, one agent — LANE L1.
//!
//! The coupling revision (Inanna, 2026-08-08) made agent creation and worktree
//! allocation a single act: every agent gets its own managed worktree, all
//! agents are isolated, and a managed worktree is the only workspace an agent
//! can receive.
//!
//! That decision dissolves the writer-lease problem STRUCTURALLY rather than
//! mechanically. There is no lease to acquire, no read-only mode to police, and
//! no shared-directory coexistence to reason about, because at most one agent
//! is ever bound to a worktree at a time. A reviewer of a child's work is
//! isolated like everyone else: it gets its own worktree created from the
//! child's branch. This module is the small amount of bookkeeping that remains.
//!
//! ## Scope right now
//!
//! The binding STATE MACHINE and its enforcement are in scope and testable
//! today against the scripted writer, with [`AgentRef`] standing in for a real
//! agent identity. Wiring it to actual spawns waits on the coupled-spawn seam,
//! which is designed jointly with the agent lane — so this module must not
//! reach for anything agent-shaped beyond an opaque identity.

use serde::{Deserialize, Serialize};

use crate::error::WorktreeError;
use crate::id::WorktreeId;

/// An opaque agent identity. Deliberately a string newtype and not a typed
/// agent handle: the coupled-spawn seam is on hold, and coupling this module to
/// a handle type that has not been designed yet would have to be undone.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentRef(String);

impl AgentRef {
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a binding is in its life.
///
/// `Terminal` and `Released` are distinct because they arise differently — an
/// agent that finished versus one the resident let go — and a post-mortem that
/// cannot tell them apart cannot tell "the worker completed" from "we stopped
/// waiting for it".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingState {
    /// The agent owns this worktree. No other agent may bind.
    Active,
    /// The agent reached a terminal state. Rebinding is permitted.
    Terminal,
    /// The resident released the agent. Rebinding is permitted.
    Released,
}

impl BindingState {
    /// Whether a replacement agent may take this worktree.
    pub fn permits_rebinding(self) -> bool {
        matches!(self, BindingState::Terminal | BindingState::Released)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub worktree: WorktreeId,
    pub agent: AgentRef,
    pub state: BindingState,
    pub bound_at_ms: i64,
}

/// Tracks which agent owns which worktree.
///
/// Durable alongside the registry: a restart that forgot its bindings would
/// happily hand a retained worktree to a second writer while the first is still
/// running.
#[derive(Debug, Default)]
pub struct BindingTable {
    bindings: Vec<Binding>,
}

impl BindingTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind an agent to a worktree.
    ///
    /// [`WorktreeError::WorktreeBusy`] when an `Active` binding already exists,
    /// naming the current holder — the failure has to be explicit enough that
    /// the resident can act on it, which means saying who is in the way.
    pub fn bind(
        &mut self,
        worktree: &WorktreeId,
        agent: &AgentRef,
        now_ms: i64,
    ) -> Result<(), WorktreeError> {
        let _ = (worktree, agent, now_ms);
        todo!("L1")
    }

    /// Mark the current binding terminal or released, permitting a rebind.
    pub fn settle(
        &mut self,
        worktree: &WorktreeId,
        state: BindingState,
    ) -> Result<(), WorktreeError> {
        let _ = (worktree, state);
        todo!("L1")
    }

    pub fn current(&self, worktree: &WorktreeId) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| &b.worktree == worktree && b.state == BindingState::Active)
    }
}
