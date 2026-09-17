//! Command-tree admission and cgroup custody. Control processes stay outside the pool.
mod queue;
pub mod service;
pub use service::CommandResourceClient;

use parking_lot::Mutex;
use queue::{Key, Queue};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;
pub const NATIVE_COMMAND_BYTES: u64 = 256 * MIB;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandResourcePolicy {
    pub general_bytes: u64,
    pub protected_bytes: u64,
    pub swap_max_bytes: u64,
    pub nix_memory_bytes: u64,
    pub machine_headroom_bytes: u64,
    pub actor_start_bytes: u64,
    pub actor_start_timeout_seconds: u64,
}
impl Default for CommandResourcePolicy {
    fn default() -> Self {
        Self {
            general_bytes: 8 * GIB,
            protected_bytes: 512 * MIB,
            swap_max_bytes: GIB,
            nix_memory_bytes: 8 * GIB,
            machine_headroom_bytes: 6 * GIB,
            actor_start_bytes: GIB,
            actor_start_timeout_seconds: 300,
        }
    }
}
impl CommandResourcePolicy {
    pub fn capacity(&self) -> u64 {
        self.general_bytes.saturating_add(self.protected_bytes)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.general_bytes == 0
            || self.actor_start_bytes == 0
            || self.actor_start_timeout_seconds == 0
        {
            return Err("invalid command resource limits".into());
        }
        self.general_bytes
            .checked_add(self.protected_bytes)
            .and_then(|n| n.checked_add(self.machine_headroom_bytes))
            .and_then(|n| n.checked_add(self.actor_start_bytes))
            .and_then(|n| n.checked_add(self.nix_memory_bytes))
            .ok_or("command resource limits overflow")?;
        Ok(())
    }
}
/// Memory short by `needed - (available - pending)`. Formats in GiB for logs and errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionShortfall {
    available: u64,
    pending: u64,
    headroom: u64,
    start: u64,
}
impl AdmissionShortfall {
    fn needed(&self) -> u64 {
        self.headroom + self.start
    }
}
impl std::fmt::Display for AdmissionShortfall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn gib(bytes: u64) -> f64 {
            bytes as f64 / GIB as f64
        }
        write!(
            f,
            "{:.1} GiB available after {:.1} GiB pending actor starts; \
             {:.1} GiB needed ({:.1} GiB headroom + {:.1} GiB start)",
            gib(self.available.saturating_sub(self.pending)),
            gib(self.pending),
            gib(self.needed()),
            gib(self.headroom),
            gib(self.start),
        )
    }
}

/// Pure admission rule: only memory actually available counts, minus what other
/// pending actor starts have already claimed. Idle command/nix pool budget is not
/// reserved against — the pools stay capped by their own cgroup limits.
fn actor_start_decision(
    policy: &CommandResourcePolicy,
    available: u64,
    pending: u64,
) -> Result<(), AdmissionShortfall> {
    let needed = policy.machine_headroom_bytes + policy.actor_start_bytes;
    if available.saturating_sub(pending) >= needed {
        Ok(())
    } else {
        Err(AdmissionShortfall {
            available,
            pending,
            headroom: policy.machine_headroom_bytes,
            start: policy.actor_start_bytes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CommandResourceStatus {
    Queued,
    Admitted { cgroup: PathBuf },
    Running,
    Completed,
    ResourceExhausted,
    CancelledBeforeStart,
    CleanupUnconfirmed { detail: String },
}
impl CommandResourceStatus {
    pub fn is_queued(&self) -> bool {
        matches!(self, Self::Queued)
    }
}
struct Entry {
    status: watch::Sender<CommandResourceStatus>,
    bytes: Option<u64>,
    directory: Option<PathBuf>,
    started: bool,
}
impl Entry {
    fn new(status: CommandResourceStatus, bytes: Option<u64>) -> Self {
        Self {
            status: watch::channel(status).0,
            bytes,
            directory: None,
            started: false,
        }
    }

    fn current(&self) -> CommandResourceStatus {
        self.status.borrow().clone()
    }
}
struct State {
    entries: HashMap<Key, Entry>,
    queue: Queue,
}

pub struct CommandResources {
    root: PathBuf,
    policy: CommandResourcePolicy,
    state: Mutex<State>,
    actor_starts: Mutex<u64>,
}
fn io_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::other(message.into())
}
fn read_counter(path: &Path, key: &str) -> std::io::Result<u64> {
    std::fs::read_to_string(path)?
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(' ')?;
            (k == key)
                .then(|| v.split_whitespace().next()?.parse().ok())
                .flatten()
        })
        .ok_or_else(|| io_error(format!("missing {key} in {}", path.display())))
}
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 160
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn validate_key(actor: &str, id: &str) -> std::io::Result<Key> {
    if !valid_key(actor) || !valid_key(id) {
        return Err(io_error("invalid command identity"));
    }
    Ok((actor.into(), id.into()))
}
impl CommandResources {
    /// Create the sole resource owner inside a fresh delegated systemd scope.
    pub fn delegated(policy: CommandResourcePolicy) -> std::io::Result<Arc<Self>> {
        policy.validate().map_err(io_error)?;
        let membership = std::fs::read_to_string("/proc/self/cgroup")?;
        let relative = membership
            .lines()
            .find_map(|l| l.strip_prefix("0::/"))
            .ok_or_else(|| io_error("cgroup v2 delegation required"))?;
        if relative.split('/').any(|p| p == "..") {
            return Err(io_error("invalid cgroup membership"));
        }
        let parent = Path::new("/sys/fs/cgroup").join(relative);
        let control = parent.join("control");
        std::fs::create_dir(&control)?;
        std::fs::write(control.join("cgroup.procs"), std::process::id().to_string())?;
        std::fs::write(parent.join("cgroup.subtree_control"), "+memory")?;
        let root = parent.join("commands");
        // Never overwrite an existing resource tree on service restart.
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("memory.max"), policy.capacity().to_string())?;
        std::fs::write(
            root.join("memory.swap.max"),
            policy.swap_max_bytes.to_string(),
        )?;
        std::fs::write(root.join("cgroup.subtree_control"), "+memory")?;
        let owner = Arc::new(Self {
            root,
            state: Mutex::new(State {
                entries: HashMap::new(),
                queue: Queue::new(
                    policy.general_bytes,
                    policy.protected_bytes,
                    NATIVE_COMMAND_BYTES,
                ),
            }),
            policy,
            actor_starts: Mutex::new(0),
        });
        let weak = Arc::downgrade(&owner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(owner) = weak.upgrade() else { break };
                owner.observe();
            }
        });
        Ok(owner)
    }

    pub fn policy(&self) -> &CommandResourcePolicy {
        &self.policy
    }

    pub fn actor_directory(&self, actor: &str) -> std::io::Result<PathBuf> {
        if !valid_key(actor) {
            return Err(io_error("invalid actor resource identity"));
        }
        let dir = self.root.join(actor);
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        std::fs::write(dir.join("cgroup.subtree_control"), "+memory")?;
        Ok(dir)
    }

    /// Acceptance is retained independently of every observing transport future.
    pub fn submit(
        &self,
        actor: &str,
        id: &str,
        bytes: u64,
    ) -> std::io::Result<CommandResourceStatus> {
        self.submit_inner(actor, id, Some(bytes))
    }

    /// Native launches may claim a hosted job's existing grant. New identities
    /// always receive the small-command limit; native input cannot choose it.
    pub fn submit_native(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        self.submit_inner(actor, id, None)
    }

    fn submit_inner(
        &self,
        actor: &str,
        id: &str,
        requested: Option<u64>,
    ) -> std::io::Result<CommandResourceStatus> {
        let key = validate_key(actor, id)?;
        let bytes = requested.unwrap_or(NATIVE_COMMAND_BYTES);
        if bytes == 0 || bytes > self.policy.general_bytes {
            return Err(io_error("command memory must fit the general allowance"));
        }
        let mut state = self.state.lock();
        if let Some(entry) = state.entries.get(&key) {
            if requested.is_some() && entry.bytes.is_some_and(|previous| previous != bytes) {
                return Err(io_error(
                    "command identity already accepted with different memory",
                ));
            }
            return Ok(entry.current());
        }
        state.entries.insert(
            key.clone(),
            Entry::new(CommandResourceStatus::Queued, Some(bytes)),
        );
        state.queue.push(key.clone(), bytes);
        self.admit_waiters(&mut state);
        Ok(state.entries[&key].current())
    }

    pub async fn acquire(
        self: &Arc<Self>,
        actor: &str,
        id: &str,
    ) -> std::io::Result<CommandResourceStatus> {
        self.submit_native(actor, id)?;
        self.wait(actor, id).await
    }

    pub async fn wait(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        let mut changes = self
            .state
            .lock()
            .entries
            .get(&validate_key(actor, id)?)
            .ok_or_else(|| io_error("unknown command"))?
            .status
            .subscribe();
        loop {
            let status = changes.borrow_and_update().clone();
            if !status.is_queued() {
                return Ok(status);
            }
            changes
                .changed()
                .await
                .map_err(|_| io_error("resource owner stopped"))?;
        }
    }

    fn admit_waiters(&self, state: &mut State) {
        while let Some((key, bytes)) = state.queue.next() {
            let configured = self.configure_command(&key, bytes);
            #[expect(
                clippy::expect_used,
                reason = "queue admission and retained entries share this lock; entries are never removed"
            )]
            let entry = state
                .entries
                .get_mut(&key)
                .expect("queued command has retained entry");
            match configured {
                Ok(directory) => {
                    entry.directory = Some(directory.clone());
                    entry
                        .status
                        .send_replace(CommandResourceStatus::Admitted { cgroup: directory });
                }
                Err(error) => {
                    state.queue.release(&key);
                    entry
                        .status
                        .send_replace(CommandResourceStatus::CleanupUnconfirmed {
                            detail: error.to_string(),
                        });
                }
            }
        }
    }

    fn configure_command(&self, key: &Key, bytes: u64) -> std::io::Result<PathBuf> {
        let dir = self.actor_directory(&key.0)?.join(&key.1);
        std::fs::create_dir(&dir)?;
        let result = (|| {
            std::fs::write(dir.join("memory.max"), bytes.to_string())?;
            std::fs::write(
                dir.join("memory.swap.max"),
                self.policy.swap_max_bytes.to_string(),
            )?;
            std::fs::write(dir.join("memory.oom.group"), "1")
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_dir(&dir);
            return Err(error);
        }
        Ok(dir)
    }

    pub fn started(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        let mut state = self.state.lock();
        let entry = state
            .entries
            .get_mut(&validate_key(actor, id)?)
            .ok_or_else(|| io_error("unknown command"))?;
        if matches!(entry.current(), CommandResourceStatus::Admitted { .. }) {
            entry.started = true;
            entry.status.send_replace(CommandResourceStatus::Running);
        }
        Ok(entry.current())
    }

    pub fn status(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        self.observe();
        self.state
            .lock()
            .entries
            .get(&validate_key(actor, id)?)
            .map(Entry::current)
            .ok_or_else(|| io_error("unknown command"))
    }

    pub fn cancel(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        let key = validate_key(actor, id)?;
        let mut state = self.state.lock();
        // A tombstone prevents a delayed submission after cancellation from starting.
        let entry = state
            .entries
            .entry(key.clone())
            .or_insert_with(|| Entry::new(CommandResourceStatus::CancelledBeforeStart, None));
        let mut released = entry.current().is_queued();
        if let Some(directory) = &entry.directory {
            if !entry.started && read_counter(&directory.join("cgroup.events"), "populated")? == 0 {
                // An empty cgroup can be removed with open join descriptors. Either
                // removal fences the late join, or a racing join wins and is killed.
                match std::fs::remove_dir(directory) {
                    Ok(()) => {
                        entry.directory = None;
                        released = true;
                    }
                    Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                        std::fs::write(directory.join("cgroup.kill"), "1")?;
                    }
                    Err(error) => return Err(error),
                }
            } else {
                std::fs::write(directory.join("cgroup.kill"), "1")?;
            }
        }
        if released {
            entry
                .status
                .send_replace(CommandResourceStatus::CancelledBeforeStart);
            state.queue.release(&key);
            self.admit_waiters(&mut state);
        }
        Ok(state.entries[&key].current())
    }

    fn observe(&self) {
        let mut state = self.state.lock();
        let mut released = Vec::new();
        for (key, entry) in &mut state.entries {
            let Some(dir) = entry.directory.as_ref() else {
                continue;
            };
            let result = (|| -> std::io::Result<bool> {
                let populated = read_counter(&dir.join("cgroup.events"), "populated")?;
                let oom = read_counter(&dir.join("memory.events"), "oom_kill")?;
                if populated > 0 {
                    entry.started = true;
                    return Ok(false);
                }
                if !entry.started {
                    return Ok(false);
                }
                // Removal invalidates retained join descriptors, fencing a late spawn.
                std::fs::remove_dir(dir)?;
                entry.status.send_replace(if oom > 0 {
                    CommandResourceStatus::ResourceExhausted
                } else if entry.started {
                    CommandResourceStatus::Completed
                } else {
                    CommandResourceStatus::CancelledBeforeStart
                });
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    entry.directory = None;
                    released.push(key.clone());
                }
                Ok(false) => {}
                Err(error) => {
                    entry
                        .status
                        .send_replace(CommandResourceStatus::CleanupUnconfirmed {
                            detail: error.to_string(),
                        });
                }
            }
        }
        for key in released {
            state.queue.release(&key);
        }
        self.admit_waiters(&mut state);
    }

    pub async fn admit_actor(self: &Arc<Self>) -> std::io::Result<ActorStartReservation> {
        let start = tokio::time::Instant::now();
        let deadline = start + Duration::from_secs(self.policy.actor_start_timeout_seconds);
        let mut logged_waiting = false;
        let mut last_warn = start;
        loop {
            let available = read_counter(Path::new("/proc/meminfo"), "MemAvailable:")? * 1024;
            let decision = {
                let mut pending = self.actor_starts.lock();
                let decision = actor_start_decision(&self.policy, available, *pending);
                if decision.is_ok() {
                    *pending += self.policy.actor_start_bytes;
                }
                decision
            };
            match decision {
                Ok(()) => {
                    return Ok(ActorStartReservation {
                        owner: self.clone(),
                    });
                }
                Err(shortfall) => {
                    let now = tokio::time::Instant::now();
                    if !logged_waiting {
                        tracing::info!(%shortfall, "actor admission waiting");
                        logged_waiting = true;
                        last_warn = now;
                    } else if now.saturating_duration_since(last_warn) >= Duration::from_secs(30) {
                        tracing::warn!(
                            %shortfall,
                            elapsed_secs = now.saturating_duration_since(start).as_secs(),
                            "actor admission still waiting"
                        );
                        last_warn = now;
                    }
                    if now >= deadline {
                        return Err(io_error(format!(
                            "actor resource admission timed out after {}s; actor not started: {shortfall}",
                            start.elapsed().as_secs(),
                        )));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}
pub struct ActorStartReservation {
    owner: Arc<CommandResources>,
}
impl Drop for ActorStartReservation {
    fn drop(&mut self) {
        *self.owner.actor_starts.lock() -= self.owner.policy.actor_start_bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> CommandResourcePolicy {
        CommandResourcePolicy {
            machine_headroom_bytes: 6 * GIB,
            actor_start_bytes: GIB,
            ..CommandResourcePolicy::default()
        }
    }

    #[test]
    fn admits_when_available_minus_pending_covers_headroom_and_start() {
        let policy = policy();
        // Exactly headroom + start, no pending.
        assert_eq!(actor_start_decision(&policy, 7 * GIB, 0), Ok(()));
        // Comfortably over, with some pending already deducted.
        assert_eq!(actor_start_decision(&policy, 10 * GIB, 2 * GIB), Ok(()));
    }

    #[test]
    fn refuses_with_shortfall_numbers() {
        let policy = policy();
        let err = actor_start_decision(&policy, 3 * GIB + 200 * MIB, GIB).unwrap_err();
        assert_eq!(
            err,
            AdmissionShortfall {
                available: 3 * GIB + 200 * MIB,
                pending: GIB,
                headroom: 6 * GIB,
                start: GIB,
            }
        );
        assert_eq!(
            err.to_string(),
            "2.2 GiB available after 1.0 GiB pending actor starts; \
             7.0 GiB needed (6.0 GiB headroom + 1.0 GiB start)"
        );
    }

    #[test]
    fn pending_counts_against_availability() {
        let policy = policy();
        // Plenty raw available, but pending starts already claim it all.
        assert!(actor_start_decision(&policy, 8 * GIB, 8 * GIB).is_err());
        // One byte more pending than headroom+start allows tips it over.
        assert_eq!(actor_start_decision(&policy, 14 * GIB, 7 * GIB), Ok(()));
        assert!(actor_start_decision(&policy, 14 * GIB, 7 * GIB + 1).is_err());
    }
}
