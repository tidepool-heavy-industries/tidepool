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

/// Move-only custody of one active binding generation. Settlement consumes
/// the receipt; transfer updates it to the successor's generation. A stale
/// receipt cannot settle a later occupant of the same worktree.
///
/// Dropping this receipt leaves its row active. Only explicit settlement can
/// attest release; a panic or lost owner cannot manufacture cleanup evidence.
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
    /// Latched after a possibly visible persistence failure; cleared only by
    /// dropping this owner and exclusively reopening authoritative storage.
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
    /// Move a live lease to a replacement actor without opening an unbound
    /// interval. Both history rows are published in the same atomic file write.
    /// An uncertain write retains the receipt but fences all table authority.
    pub fn transfer(
        &mut self,
        lease: &mut ActiveBinding,
        successor: &AgentRef,
        now_ms: i64,
    ) -> Result<(), WorktreeError> {
        let previous = self.active_index(lease)?;
        let generation = self.next_generation;
        self.next_generation += 1;
        self.bindings[previous].state = BindingState::Released;
        self.bindings.push(Binding::new(
            lease.worktree.clone(),
            successor.clone(),
            BindingState::Active,
            now_ms,
        ));
        self.generations.push(Some(generation));
        lease.generation = generation;
        if let Err(error) = self.persist(&lease.worktree) {
            self.write_uncertain = true;
            return Err(error);
        }
        Ok(())
    }

    /// The exact worktree currently owned by `agent`, if any. Actor admission
    /// uses this to resolve the typed `boundHead` placement without exposing
    /// filesystem paths or asking Haskell to rediscover custody. An uncertain
    /// table returns no authority, even when it retains Active diagnostic rows.
    pub fn active_for_agent(&self, agent: &AgentRef) -> Option<&WorktreeId> {
        if self.write_uncertain {
            return None;
        }
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
            // Reopening is the reconciliation boundary: the loaded bytes and
            // pathname must be durable before these rows can authorize custody.
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(|e| storage_failure(&path, e))?;
            tidepool_atomic_write::sync_parent_directory(&path)
                .map_err(|e| storage_failure(&e.path, e.source))?;
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
    /// file in the same directory, fsync, rename, strict directory fsync).
    /// Failure may follow visible publication and does not imply rollback.
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
        self.ensure_writable()?;
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
        // No lease escapes a failed bind. Retain the tentative Active row for
        // diagnosis, but fence every authority lookup and mutation: the rename
        // may already be visible and reverting memory cannot undo publication.
        if let Err(e) = self.persist(worktree) {
            self.write_uncertain = true;
            if let Some(generation) = self.generations.last_mut() {
                *generation = None;
            }
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
    /// the type's docs), reported as a storage invariant failure.
    fn settle(&mut self, lease: ActiveBinding, to: BindingTerminal) -> Result<(), WorktreeError> {
        let i = self.active_index(&lease)?;
        let previous = self.bindings[i].state();
        self.bindings[i].state = to.into();
        // Retain the previous Active diagnostic snapshot conservatively. This
        // does NOT roll back disk: publication may have succeeded. The consumed
        // lease cannot be retried and this handle cannot authorize or mutate.
        if let Err(e) = self.persist(&lease.worktree) {
            self.bindings[i].state = previous;
            self.write_uncertain = true;
            return Err(e);
        }
        Ok(())
    }

    fn active_index(&self, lease: &ActiveBinding) -> Result<usize, WorktreeError> {
        self.ensure_writable()?;
        self.bindings.iter().enumerate().rposition(|(i, binding)| {
            binding.worktree() == &lease.worktree
                && binding.state() == BindingState::Active
                && self.generations[i] == Some(lease.generation)
        }).ok_or_else(|| WorktreeError::StorageFailure {
            path: self.path_for(&lease.worktree),
            detail: format!("no Active binding for worktree {} matches bind-generation {}; receipt is stale", lease.worktree, lease.generation),
        })
    }

    fn ensure_writable(&self) -> Result<(), WorktreeError> {
        if self.write_uncertain {
            return Err(storage_failure(
                self.dir.dir(),
                "binding persistence is uncertain; drop and exclusively reopen before using custody",
            ));
        }
        Ok(())
    }

    /// Confirmed active custody only. An uncertain table grants no authority;
    /// retained rows are diagnostic until exclusive reopen reconciles disk.
    pub fn current(&self, worktree: &WorktreeId) -> Option<&Binding> {
        if self.write_uncertain {
            return None;
        }
        self.bindings
            .iter()
            .find(|b| b.worktree() == worktree && b.state() == BindingState::Active)
    }
}
