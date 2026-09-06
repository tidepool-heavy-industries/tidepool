//! One worktree, one agent.
//!
//! Every agent gets its own managed worktree, all agents are isolated, and a
//! managed worktree is the only workspace an agent can receive: at most one
//! agent is ever bound to a worktree at a time, so there is no lease to
//! acquire, no read-only mode to police, and no shared-directory coexistence
//! to reason about. A reviewer of a child's work is isolated like everyone
//! else: it gets its own worktree created from the child's branch. This
//! module is the small amount of bookkeeping that remains.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::id::WorktreeId;
use crate::storage::{storage_failure, DurableJsonDir};

/// An opaque agent identity. Deliberately a string newtype and not a typed
/// agent handle: this module must not reach for anything agent-shaped beyond
/// an opaque identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentRef(String);

impl AgentRef {
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Exact actor-incarnation principal spelling used by the actor host.
    /// Legacy callers may still provide their own opaque identity through
    /// `from_raw`; new actor bindings must use this constructor so a later
    /// incarnation cannot inherit the old one's resource authority.
    pub fn exact_actor(runtime: &str, identity: u64, incarnation: u64) -> Self {
        Self(format!("actor:{runtime}:{identity}:{incarnation}"))
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

/// The legal terminal states a caller may SETTLE an active binding to —
/// deliberately smaller than [`BindingState`], which also has to name
/// `Active` for reading back stored history. Widening this to `BindingState`
/// is exactly the bug it exists to rule out: `settle(lease, Active)` would let
/// a caller report a clean teardown while the durable row stays active and
/// permanently blocks the next writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingTerminal {
    /// The agent reached a terminal state on its own.
    Completed,
    /// The resident released the agent.
    Released,
}

impl From<BindingTerminal> for BindingState {
    fn from(t: BindingTerminal) -> Self {
        match t {
            BindingTerminal::Completed => BindingState::Terminal,
            BindingTerminal::Released => BindingState::Released,
        }
    }
}

/// One row of a worktree's lease history — a READ MODEL. Fields are private
/// and construction is `pub(crate)`-only: this type is what
/// [`BindingTable::open`] deserializes off disk and what [`BindingTable::bind`]
/// appends, and nothing outside this crate may manufacture a row (an "active"
/// binding with no acquisition provenance, for instance) that never went
/// through the table's own rules. Reached from outside only by the accessors
/// below, or (for durable-format golden tests, which must be able to pin the
/// wire shape of every state a row COULD hold) via
/// [`crate::testing::binding_row`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    worktree: WorktreeId,
    agent: AgentRef,
    state: BindingState,
    bound_at_ms: i64,
}

impl Binding {
    pub fn worktree(&self) -> &WorktreeId {
        &self.worktree
    }

    pub fn agent(&self) -> &AgentRef {
        &self.agent
    }

    pub fn state(&self) -> BindingState {
        self.state
    }

    pub(crate) fn new(
        worktree: WorktreeId,
        agent: AgentRef,
        state: BindingState,
        bound_at_ms: i64,
    ) -> Self {
        Self {
            worktree,
            agent,
            state,
            bound_at_ms,
        }
    }
}

/// A non-`Clone` lease for exactly one `Active` row, returned by
/// [`BindingTable::bind`] and consumed by [`Self::complete`]/[`Self::release`].
///
/// This is the whole fix for "`settle` accepts any `BindingState` as the
/// requested terminal": there is no longer a way to settle a binding without
/// first holding the receipt `bind` handed out for THAT row, and the receipt
/// is consumed by value, so it can be spent at most once. It carries the
/// worktree id and this bind's own in-memory GENERATION (never persisted —
/// see [`BindingTable`]'s `generations` field) so a stale receipt from a
/// settled binding can never be replayed against whatever occupies that
/// worktree after a rebind: [`BindingTable::settle`] checks the generation
/// still matches the CURRENT active row before mutating anything.
///
/// Deliberately has no `Drop` impl: a dropped-without-settling receipt leaves
/// the row `Active` forever (until some later process notices and can never
/// settle it either, for want of a receipt) rather than quietly recording a
/// released binding. A panic must not falsely report a clean release — that
/// is the lease principle this type exists to enforce.
#[derive(Debug)]
pub struct ActiveBinding {
    worktree: WorktreeId,
    generation: u64,
}

impl ActiveBinding {
    pub fn worktree(&self) -> &WorktreeId {
        &self.worktree
    }

    /// Settle this lease `Completed` — the agent reached a terminal state on
    /// its own, so cycle completion is agent completion.
    pub fn complete(self, table: &mut BindingTable) -> Result<(), WorktreeError> {
        table.settle(self, BindingTerminal::Completed)
    }

    /// Settle this lease `Released` — the resident stopped waiting for it
    /// (a rollback, or an explicit cancellation).
    pub fn release(self, table: &mut BindingTable) -> Result<(), WorktreeError> {
        table.settle(self, BindingTerminal::Released)
    }
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
    dir: DurableJsonDir,
    bindings: Vec<Binding>,
    write_uncertain: bool,
    /// Parallel to `bindings` (same length, same index) — the in-memory
    /// bind-generation of each row, assigned when [`Self::bind`] creates it.
    /// NEVER persisted: an [`ActiveBinding`] receipt's identity is a fact
    /// about this process's lifetime, not a durable one. A row loaded from
    /// disk at [`Self::open`] carries `None` here, so a leftover `Active` row
    /// from a crashed process can never be settled by a forged receipt — only
    /// a fresh [`Self::bind`] produces one, and `bind` refuses
    /// (`WorktreeBusy`) while that row still stands.
    generations: Vec<Option<u64>>,
    next_generation: u64,
    /// Held (exclusively flocked) for this table's whole lifetime. The
    /// enforcement decisions (`bind`'s already-bound refusal) run against the
    /// IN-MEMORY rows, which is only sound while exactly one process owns the
    /// root — two tables over one root could both see "unbound" and both
    /// persist a binding. The lock turns that silent double-writer into a
    /// loud open-time refusal. Released by drop.
    _owner_lock: fs::File,
}

impl BindingTable {
    /// The exact worktree currently owned by `agent`, if any. Actor admission
    /// uses this to resolve the typed `boundHead` placement without exposing
    /// filesystem paths or asking Haskell to rediscover custody.
    pub fn active_for_agent(&self, agent: &AgentRef) -> Option<&WorktreeId> {
        self.bindings
            .iter()
            .rev()
            .find(|binding| binding.agent() == agent && binding.state() == BindingState::Active)
            .map(Binding::worktree)
    }

    /// Open (creating if absent) a binding table rooted at `root`, loading
    /// every persisted binding into memory.
    ///
    /// SINGLE-OWNER: refuses (typed, loud) when another live `BindingTable` —
    /// in this process or any other — already owns `root`. Isolation is
    /// enforced from in-memory state, so one owning table per root is a
    /// correctness precondition, not a deployment nicety.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        Self::open_with_timeout(root, std::time::Duration::ZERO)
    }

    /// Wait for the existing owner to release its lock, without replacing it.
    pub fn open_with_timeout(
        root: impl AsRef<Path>,
        timeout: std::time::Duration,
    ) -> Result<Self, WorktreeError> {
        let root = root.as_ref().to_path_buf();
        let dir = DurableJsonDir::open(&root)?;

        let lock_path = root.join(".owner.lock");
        let owner_lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| storage_failure(&lock_path, e))?;
        let started = std::time::Instant::now();
        loop {
            match owner_lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < timeout => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    std::thread::sleep(std::time::Duration::from_millis(50).min(remaining));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(storage_failure(&lock_path,
                        "another process already owns this binding root; stop its Shoal session and wait for shutdown before launching again (ownership was not changed)"));
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(storage_failure(&lock_path, error));
                }
            }
        }

        let mut bindings = Vec::new();
        let mut generations = Vec::new();
        for (path, bytes) in dir.read_all()? {
            let mut rows: Vec<Binding> =
                serde_json::from_slice(&bytes).map_err(|e| storage_failure(&path, e))?;
            // Loaded rows carry no generation — see the field docs on
            // `generations`.
            generations.resize(generations.len() + rows.len(), None);
            bindings.append(&mut rows);
        }

        Ok(Self {
            dir,
            bindings,
            write_uncertain: false,
            generations,
            next_generation: 0,
            _owner_lock: owner_lock,
        })
    }

    fn path_for(&self, worktree: &WorktreeId) -> PathBuf {
        self.dir.path_for(worktree.as_str())
    }

    /// Rewrite the on-disk file for `worktree` from the current in-memory
    /// rows, crash-safely via the shared durable atomic-write helper (temp
    /// file in the same directory, fsync, rename, best-effort dir fsync).
    fn persist(&self, worktree: &WorktreeId) -> Result<(), WorktreeError> {
        let rows: Vec<&Binding> = self
            .bindings
            .iter()
            .filter(|b| b.worktree() == worktree)
            .collect();
        #[allow(clippy::expect_used, reason = "serialize bindings")]
        let bytes = serde_json::to_vec_pretty(&rows).expect("serialize bindings");
        self.dir.write(worktree.as_str(), &bytes)
    }

    /// Bind an agent to a worktree, returning the [`ActiveBinding`] lease
    /// for the row just created.
    ///
    /// [`WorktreeError::WorktreeBusy`] when an `Active` binding already exists,
    /// naming the current holder — the failure has to be explicit enough that
    /// the resident can act on it, which means saying who is in the way.
    pub fn bind(
        &mut self,
        worktree: &WorktreeId,
        agent: &AgentRef,
        now_ms: i64,
    ) -> Result<ActiveBinding, WorktreeError> {
        if let Some(current) = self.current(worktree) {
            return Err(WorktreeError::WorktreeBusy {
                worktree: worktree.clone(),
                holder: current.agent().to_string(),
            });
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.bindings.push(Binding::new(
            worktree.clone(),
            agent.clone(),
            BindingState::Active,
            now_ms,
        ));
        self.generations.push(Some(generation));
        // ROLL BACK on a failed write. Without this, a persist failure leaves
        // memory holding a binding that disk does not — and since the isolation
        // invariant is enforced from THIS table, a restart would read the
        // unbound disk state and let a SECOND agent bind the same worktree.
        // Two writers in one tree is the exact failure the coupling exists to
        // make unconstructible, so memory and disk must not be allowed to
        // disagree even transiently.
        if let Err(e) = self.persist(worktree) {
            self.bindings.pop();
            self.generations.pop();
            return Err(e);
        }
        Ok(ActiveBinding {
            worktree: worktree.clone(),
            generation,
        })
    }

    /// Settle `lease`'s row to `to`'s terminal state, consuming the receipt.
    ///
    /// Looks up the row by BOTH `lease.worktree` and `lease.generation` — not
    /// just the worktree — so a stale receipt from a binding that was already
    /// settled (and, since then, rebound) fails loud instead of silently
    /// settling the NEW occupant. That case is a genuine invariant violation
    /// rather than a normal outcome (a live `ActiveBinding` is, by
    /// construction, the only receipt for its worktree until consumed — see
    /// the type's docs) — reported via the existing `StorageFailure` variant
    /// rather than a new one (`error.rs` is frozen).
    fn settle(&mut self, lease: ActiveBinding, to: BindingTerminal) -> Result<(), WorktreeError> {
        let idx = {
            let generations = &self.generations;
            self.bindings.iter().enumerate().rposition(|(i, b)| {
                b.worktree() == &lease.worktree
                    && b.state() == BindingState::Active
                    && generations[i] == Some(lease.generation)
            })
        };
        let Some(i) = idx else {
            return Err(WorktreeError::StorageFailure {
                path: self.path_for(&lease.worktree),
                detail: format!(
                    "settle: no Active binding for worktree {} matches bind-generation {} — \
                     this receipt is stale (already settled, or superseded by a rebind)",
                    lease.worktree, lease.generation
                ),
            });
        };
        let previous = self.bindings[i].state();
        self.bindings[i].state = to.into();
        // Same rollback discipline as `bind`, mirrored: a failed write here
        // would leave memory believing the worktree is rebindable while
        // disk still says Active — the inverse disagreement, reached the
        // same way.
        if let Err(e) = self.persist(&lease.worktree) {
            self.bindings[i].state = previous;
            return Err(e);
        }
        Ok(())
    }

    pub fn current(&self, worktree: &WorktreeId) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.worktree() == worktree && b.state() == BindingState::Active)
    }
}
