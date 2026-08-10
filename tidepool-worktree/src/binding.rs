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
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::id::WorktreeId;

/// Build a [`WorktreeError::StorageFailure`] naming the path that actually
/// failed, from any underlying error with a `Display` impl (`std::io::Error`
/// for I/O, `serde_json::Error` for a corrupt record).
fn storage_failure(path: &Path, detail: impl std::fmt::Display) -> WorktreeError {
    WorktreeError::StorageFailure {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    }
}

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
/// running. There is deliberately no in-memory-only constructor — every
/// binding decision this table makes has to survive a crash, so `open` (not
/// `new`) is the only way to get one.
///
/// One JSON file per worktree id under `root`, holding that worktree's full
/// lease history (every agent that ever bound to it, each entry's `state`
/// its outcome) — an append for `bind`, an in-place state edit for `settle`.
/// The full history loads into memory at `open` so [`Self::current`] can stay
/// a cheap borrow; every mutation re-persists just the affected worktree's
/// file with the same temp-file/fsync/rename discipline as the registry.
#[derive(Debug)]
pub struct BindingTable {
    root: PathBuf,
    bindings: Vec<Binding>,
    /// Held (exclusively flocked) for this table's whole lifetime. The
    /// enforcement decisions (`bind`'s already-bound refusal) run against the
    /// IN-MEMORY rows, which is only sound while exactly one process owns the
    /// root — two tables over one root could both see "unbound" and both
    /// persist a binding. The lock turns that silent double-writer into a
    /// loud open-time refusal. Released by drop.
    _owner_lock: fs::File,
}

impl BindingTable {
    /// Open (creating if absent) a binding table rooted at `root`, loading
    /// every persisted binding into memory.
    ///
    /// SINGLE-OWNER: refuses (typed, loud) when another live `BindingTable` —
    /// in this process or any other — already owns `root`. Isolation is
    /// enforced from in-memory state, so one owning table per root is a
    /// correctness precondition, not a deployment nicety.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| storage_failure(&root, e))?;

        let lock_path = root.join(".owner.lock");
        let owner_lock = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| storage_failure(&lock_path, e))?;
        if owner_lock.try_lock().is_err() {
            return Err(storage_failure(
                &lock_path,
                "another process (or another BindingTable in this one) already \
                 owns this binding root — one worktree, one agent is enforced \
                 from in-memory state, so exactly one owner may hold it",
            ));
        }

        let mut bindings = Vec::new();
        for entry in fs::read_dir(&root).map_err(|e| storage_failure(&root, e))? {
            let entry = entry.map_err(|e| storage_failure(&root, e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|e| storage_failure(&path, e))?;
            let mut rows: Vec<Binding> =
                serde_json::from_slice(&bytes).map_err(|e| storage_failure(&path, e))?;
            bindings.append(&mut rows);
        }

        Ok(Self {
            root,
            bindings,
            _owner_lock: owner_lock,
        })
    }

    fn path_for(&self, worktree: &WorktreeId) -> PathBuf {
        // Same backstop as `WorktreeRegistry::record_path` — ids are validated
        // at the wire boundary; here that assumption fails loud, not as an
        // escape.
        debug_assert!(
            WorktreeId::is_path_safe(worktree.as_str()),
            "worktree id {:?} is not path-safe — a wire boundary failed to validate",
            worktree.as_str()
        );
        self.root.join(format!("{}.json", worktree.as_str()))
    }

    /// Rewrite the on-disk file for `worktree` from the current in-memory
    /// rows, crash-safely (temp file in the same directory, fsync, rename).
    fn persist(&self, worktree: &WorktreeId) -> Result<(), WorktreeError> {
        let rows: Vec<&Binding> = self
            .bindings
            .iter()
            .filter(|b| &b.worktree == worktree)
            .collect();
        let bytes = serde_json::to_vec_pretty(&rows).expect("serialize bindings");
        let path = self.path_for(worktree);
        // `path_for` always joins onto `self.root`, so this always has a parent.
        let dir = path.parent().expect("binding path has a parent directory");
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| storage_failure(dir, e))?;
        tmp.write_all(&bytes)
            .map_err(|e| storage_failure(&path, e))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| storage_failure(&path, e))?;
        tmp.persist(&path).map_err(|e| storage_failure(&path, e))?;
        Ok(())
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
        if let Some(current) = self.current(worktree) {
            return Err(WorktreeError::WorktreeBusy {
                worktree: worktree.clone(),
                holder: current.agent.to_string(),
            });
        }
        self.bindings.push(Binding {
            worktree: worktree.clone(),
            agent: agent.clone(),
            state: BindingState::Active,
            bound_at_ms: now_ms,
        });
        // ROLL BACK on a failed write. Without this, a persist failure leaves
        // memory holding a binding that disk does not — and since the isolation
        // invariant is enforced from THIS table, a restart would read the
        // unbound disk state and let a SECOND agent bind the same worktree.
        // Two writers in one tree is the exact failure the coupling exists to
        // make unconstructible, so memory and disk must not be allowed to
        // disagree even transiently.
        if let Err(e) = self.persist(worktree) {
            self.bindings.pop();
            return Err(e);
        }
        Ok(())
    }

    /// Mark the current binding terminal or released, permitting a rebind.
    /// A no-op (not an error) when there is no active binding to settle:
    /// `error.rs` is frozen and has no variant for that case, and settling
    /// twice is a harmless idempotent request rather than a domain failure.
    pub fn settle(
        &mut self,
        worktree: &WorktreeId,
        state: BindingState,
    ) -> Result<(), WorktreeError> {
        let idx = self
            .bindings
            .iter()
            .rposition(|b| &b.worktree == worktree && b.state == BindingState::Active);
        if let Some(i) = idx {
            let previous = self.bindings[i].state;
            self.bindings[i].state = state;
            // Same rollback discipline as `bind`, mirrored: a failed write here
            // would leave memory believing the worktree is rebindable while
            // disk still says Active — the inverse disagreement, reached the
            // same way.
            if let Err(e) = self.persist(worktree) {
                self.bindings[i].state = previous;
                return Err(e);
            }
        }
        Ok(())
    }

    pub fn current(&self, worktree: &WorktreeId) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| &b.worktree == worktree && b.state == BindingState::Active)
    }
}
