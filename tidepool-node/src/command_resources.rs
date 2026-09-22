//! Command-tree admission and cgroup custody. Control processes stay outside the pool.
mod journal;
mod queue;
pub mod service;
pub use service::CommandResourceClient;

use journal::{EventKind as JournalEvent, Journal};
use parking_lot::Mutex;
use queue::{Key, Queue};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
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
    Admitted {
        cgroup: PathBuf,
    },
    Running,
    Completed,
    ResourceExhausted,
    CancelledBeforeStart,
    CleanupUnconfirmed {
        detail: String,
    },
    /// Durable identity fence retained after detailed terminal evidence was acknowledged.
    Retired,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResourceObservation {
    pub active: usize,
    pub historical: usize,
    pub retained_allocations: usize,
    pub cleanup_failures: usize,
    pub sealed_producers: usize,
    pub process_count: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub memory_pressure_avg10_micros: Option<u64>,
    pub cpu_usage_micros: Option<u64>,
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
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
    active: HashSet<Key>,
    cleanup_failures: HashSet<Key>,
    retained_allocations: HashSet<Key>,
    queue: Queue,
    sealed_producers: HashSet<String>,
    acknowledged: HashSet<Key>,
}

fn refresh_observation_indexes(state: &mut State, key: &Key) {
    let Some(entry) = state.entries.get(key) else {
        state.cleanup_failures.remove(key);
        state.retained_allocations.remove(key);
        return;
    };
    if matches!(
        entry.current(),
        CommandResourceStatus::CleanupUnconfirmed { .. }
    ) {
        state.cleanup_failures.insert(key.clone());
    } else {
        state.cleanup_failures.remove(key);
    }
    if entry.directory.is_some() {
        state.retained_allocations.insert(key.clone());
    } else {
        state.retained_allocations.remove(key);
    }
}

fn rebuild_observation_indexes(state: &mut State) {
    state.cleanup_failures.clear();
    state.retained_allocations.clear();
    let keys = state.entries.keys().cloned().collect::<Vec<_>>();
    for key in keys {
        refresh_observation_indexes(state, &key);
    }
}

pub struct CommandResources {
    root: PathBuf,
    policy: CommandResourcePolicy,
    state: Mutex<State>,
    journal: Mutex<Journal>,
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
fn read_scalar(path: &Path) -> std::io::Result<u64> {
    std::fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| io_error(format!("invalid counter in {}", path.display())))
}
fn read_pressure_avg10(path: &Path) -> std::io::Result<u64> {
    let text = std::fs::read_to_string(path)?;
    let value = text
        .lines()
        .find(|line| line.starts_with("some "))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("avg10="))
        })
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| io_error(format!("invalid pressure data in {}", path.display())))?;
    Ok((value * 1_000_000.0).round() as u64)
}
fn read_io_bytes(path: &Path) -> std::io::Result<(u64, u64)> {
    let mut read = 0_u64;
    let mut write = 0_u64;
    for line in std::fs::read_to_string(path)?.lines() {
        for field in line.split_whitespace().skip(1) {
            if let Some(value) = field.strip_prefix("rbytes=") {
                read = read.saturating_add(value.parse::<u64>().unwrap_or(0));
            } else if let Some(value) = field.strip_prefix("wbytes=") {
                write = write.saturating_add(value.parse::<u64>().unwrap_or(0));
            }
        }
    }
    Ok((read, write))
}
fn bounded_process_count(root: &Path, limit: usize) -> std::io::Result<u64> {
    let mut count = 0_u64;
    let mut visited = 0_usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        visited += 1;
        if visited > limit {
            return Err(io_error(
                "command cgroup observation exceeds directory limit",
            ));
        }
        count = count.saturating_add(
            std::fs::read_to_string(directory.join("cgroup.procs"))?
                .lines()
                .count() as u64,
        );
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(count)
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
        Self::delegated_inner(policy, None)
    }

    /// Open the durable owner used by the per-user resource service.
    pub fn delegated_with_journal(
        policy: CommandResourcePolicy,
        journal: PathBuf,
    ) -> std::io::Result<Arc<Self>> {
        Self::delegated_inner(policy, Some(journal))
    }

    fn delegated_inner(
        policy: CommandResourcePolicy,
        journal_path: Option<PathBuf>,
    ) -> std::io::Result<Arc<Self>> {
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
        let (journal, events) = match journal_path {
            Some(path) => Journal::open(path)?,
            None => (Journal::ephemeral(), Vec::new()),
        };
        let owner = Arc::new(Self {
            root,
            state: Mutex::new(State {
                entries: HashMap::new(),
                active: HashSet::new(),
                cleanup_failures: HashSet::new(),
                retained_allocations: HashSet::new(),
                queue: Queue::new(
                    policy.general_bytes,
                    policy.protected_bytes,
                    NATIVE_COMMAND_BYTES,
                ),
                sealed_producers: HashSet::new(),
                acknowledged: HashSet::new(),
            }),
            journal: Mutex::new(journal),
            policy,
            actor_starts: Mutex::new(0),
        });
        owner.reconcile(events)?;
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

    fn reconcile(&self, events: Vec<JournalEvent>) -> std::io::Result<()> {
        let mut state = self.state.lock();
        for event in events {
            match event {
                JournalEvent::Admission {
                    producer,
                    actor,
                    command,
                    requested_bytes,
                } => {
                    let key = validate_key(&actor, &command)?;
                    if producer != actor {
                        return Err(io_error("unsupported recovered producer identity"));
                    }
                    if state.sealed_producers.contains(&producer) {
                        return Err(io_error("admission follows durable producer seal"));
                    }
                    if let Some(entry) = state.entries.get(&key) {
                        if entry.bytes != Some(requested_bytes) {
                            return Err(io_error(
                                "command identity was reused with changed parameters",
                            ));
                        }
                    } else {
                        state.entries.insert(
                            key.clone(),
                            Entry::new(CommandResourceStatus::Queued, Some(requested_bytes)),
                        );
                        state.active.insert(key.clone());
                        state.queue.push(key, requested_bytes);
                    }
                }
                JournalEvent::CancellationFence {
                    producer,
                    actor,
                    command,
                } => {
                    if producer != actor {
                        return Err(io_error("unsupported recovered producer identity"));
                    }
                    let key = validate_key(&actor, &command)?;
                    if state.entries.contains_key(&key) {
                        return Err(io_error("cancellation fence follows command admission"));
                    }
                    state.entries.insert(
                        key,
                        Entry::new(CommandResourceStatus::CancelledBeforeStart, None),
                    );
                }
                JournalEvent::Allocation {
                    producer,
                    actor,
                    command,
                    requested_bytes,
                    allocation,
                } => {
                    let key = validate_key(&actor, &command)?;
                    if producer != actor {
                        return Err(io_error("unsupported recovered producer identity"));
                    }
                    let directory = journal::allocation_path(&self.root, &allocation)?;
                    let entry = state
                        .entries
                        .get_mut(&key)
                        .ok_or_else(|| io_error("allocation without admission intent"))?;
                    if entry.bytes != Some(requested_bytes) {
                        return Err(io_error("allocation budget differs from admission intent"));
                    }
                    entry.directory = Some(directory.clone());
                    entry
                        .status
                        .send_replace(CommandResourceStatus::Admitted { cgroup: directory });
                    state
                        .queue
                        .recover_active(key, requested_bytes)
                        .map_err(io_error)?;
                }
                JournalEvent::Started {
                    producer,
                    actor,
                    command,
                } => {
                    if producer != actor {
                        return Err(io_error("unsupported recovered producer identity"));
                    }
                    let entry = state
                        .entries
                        .get_mut(&validate_key(&actor, &command)?)
                        .ok_or_else(|| io_error("start without allocation"))?;
                    entry.started = true;
                    entry.status.send_replace(CommandResourceStatus::Running);
                }
                JournalEvent::Terminal {
                    producer,
                    actor,
                    command,
                    disposition,
                } => {
                    if producer != actor {
                        return Err(io_error("unsupported recovered producer identity"));
                    }
                    if matches!(
                        disposition,
                        CommandResourceStatus::Queued
                            | CommandResourceStatus::Admitted { .. }
                            | CommandResourceStatus::Running
                            | CommandResourceStatus::CleanupUnconfirmed { .. }
                            | CommandResourceStatus::Retired
                    ) {
                        return Err(io_error(
                            "journal terminal event has non-terminal disposition",
                        ));
                    }
                    let key = validate_key(&actor, &command)?;
                    let entry = state
                        .entries
                        .get_mut(&key)
                        .ok_or_else(|| io_error("terminal disposition without admission"))?;
                    entry.directory = None;
                    entry.status.send_replace(disposition);
                    state.queue.release(&key);
                    state.active.remove(&key);
                }
                JournalEvent::ProducerSealed { producer } => {
                    if state.active.iter().any(|key| key.0 == producer) {
                        return Err(io_error("producer was sealed with active commands"));
                    }
                    for (key, entry) in &state.entries {
                        if key.0 == producer {
                            entry.status.send_replace(CommandResourceStatus::Retired);
                        }
                    }
                    state.sealed_producers.insert(producer);
                }
                JournalEvent::Acknowledged {
                    producer,
                    actor,
                    command,
                } => {
                    if producer != actor {
                        return Err(io_error("unsupported recovered producer identity"));
                    }
                    let key = validate_key(&actor, &command)?;
                    if state.active.contains(&key) {
                        return Err(io_error("acknowledgment of active command"));
                    }
                    let entry = state
                        .entries
                        .get_mut(&key)
                        .ok_or_else(|| io_error("acknowledgment without admission"))?;
                    entry.status.send_replace(CommandResourceStatus::Retired);
                    state.acknowledged.insert(key);
                }
            }
        }

        let mut known_directories = HashSet::new();
        for (key, entry) in &state.entries {
            if let Some(directory) = &entry.directory {
                known_directories.insert(directory.clone());
                if !directory.is_dir() {
                    entry
                        .status
                        .send_replace(CommandResourceStatus::CleanupUnconfirmed {
                            detail: "recorded allocation is missing after resource-service restart"
                                .into(),
                        });
                    continue;
                }
                let populated = read_counter(&directory.join("cgroup.events"), "populated")?;
                if populated > 0 {
                    entry.status.send_replace(CommandResourceStatus::Running);
                } else if entry.started {
                    entry
                        .status
                        .send_replace(CommandResourceStatus::CleanupUnconfirmed {
                            detail:
                                "started allocation was empty at recovery; completion is unproven"
                                    .into(),
                        });
                } else {
                    tracing::warn!(actor = %key.0, command = %key.1,
                        "fencing unpublished command launch during recovery");
                }
            }
        }

        for actor in std::fs::read_dir(&self.root)? {
            let actor = actor?;
            if !actor.file_type()?.is_dir() {
                continue;
            }
            for command in std::fs::read_dir(actor.path())? {
                let command = command?;
                if !command.file_type()?.is_dir() || known_directories.contains(&command.path()) {
                    continue;
                }
                let actor_name = actor.file_name().to_string_lossy().into_owned();
                let command_name = command.file_name().to_string_lossy().into_owned();
                let key = validate_key(&actor_name, &command_name)?;
                let bytes = std::fs::read_to_string(command.path().join("memory.max"))?
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| io_error("orphaned allocation has invalid memory.max"))?;
                state
                    .queue
                    .recover_active(key.clone(), bytes)
                    .map_err(io_error)?;
                let mut entry = Entry::new(
                    CommandResourceStatus::CleanupUnconfirmed {
                        detail: "orphaned cgroup has no durable ownership record".into(),
                    },
                    Some(bytes),
                );
                entry.directory = Some(command.path());
                entry.started =
                    read_counter(&command.path().join("cgroup.events"), "populated")? > 0;
                state.entries.insert(key, entry);
            }
        }

        let unpublished = state
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                (!entry.started
                    && matches!(entry.current(), CommandResourceStatus::Admitted { .. }))
                .then(|| {
                    entry
                        .directory
                        .as_ref()
                        .map(|directory| (key.clone(), directory.clone()))
                })
                .flatten()
            })
            .collect::<Vec<_>>();
        for (key, directory) in unpublished {
            match std::fs::remove_dir(&directory) {
                Ok(()) => {
                    let disposition = CommandResourceStatus::CancelledBeforeStart;
                    self.journal.lock().append(JournalEvent::Terminal {
                        producer: key.0.clone(),
                        actor: key.0.clone(),
                        command: key.1.clone(),
                        disposition: disposition.clone(),
                    })?;
                    if let Some(entry) = state.entries.get_mut(&key) {
                        entry.directory = None;
                        entry.status.send_replace(disposition);
                    }
                    state.queue.release(&key);
                    state.active.remove(&key);
                }
                Err(error) => {
                    state.entries[&key].status.send_replace(
                        CommandResourceStatus::CleanupUnconfirmed {
                            detail: format!("cannot fence unpublished allocation: {error}"),
                        },
                    );
                }
            }
        }
        rebuild_observation_indexes(&mut state);
        self.admit_waiters(&mut state);
        Ok(())
    }

    pub fn policy(&self) -> &CommandResourcePolicy {
        &self.policy
    }

    pub fn observation(&self) -> CommandResourceObservation {
        self.observe();
        let state = self.state.lock();
        let mut observation = CommandResourceObservation {
            active: state.active.len(),
            historical: state.entries.len().saturating_sub(state.active.len()),
            retained_allocations: state.retained_allocations.len(),
            cleanup_failures: state.cleanup_failures.len(),
            sealed_producers: state.sealed_producers.len(),
            ..CommandResourceObservation::default()
        };
        observation.process_count = bounded_process_count(&self.root, 4096).ok();
        observation.memory_bytes = read_scalar(&self.root.join("memory.current")).ok();
        observation.memory_pressure_avg10_micros =
            read_pressure_avg10(&self.root.join("memory.pressure")).ok();
        observation.cpu_usage_micros = read_counter(&self.root.join("cpu.stat"), "usage_usec").ok();
        if let Ok((read, write)) = read_io_bytes(&self.root.join("io.stat")) {
            observation.io_read_bytes = Some(read);
            observation.io_write_bytes = Some(write);
        }
        observation
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
        if state.sealed_producers.contains(actor) {
            return Err(io_error("resource producer is sealed"));
        }
        if let Some(entry) = state.entries.get(&key) {
            if requested.is_some() && entry.bytes.is_some_and(|previous| previous != bytes) {
                return Err(io_error(
                    "command identity already accepted with different memory",
                ));
            }
            return Ok(entry.current());
        }
        self.journal.lock().append(JournalEvent::Admission {
            producer: actor.to_owned(),
            actor: actor.to_owned(),
            command: id.to_owned(),
            requested_bytes: bytes,
        })?;
        state.entries.insert(
            key.clone(),
            Entry::new(CommandResourceStatus::Queued, Some(bytes)),
        );
        state.active.insert(key.clone());
        state.queue.push(key.clone(), bytes);
        self.admit_waiters(&mut state);
        Ok(state.entries[&key].current())
    }

    /// Permanently fence new command identities from a retired producer.
    pub fn seal_producer(&self, producer: &str) -> std::io::Result<()> {
        if !valid_key(producer) {
            return Err(io_error("invalid resource producer identity"));
        }
        let mut state = self.state.lock();
        if state.sealed_producers.contains(producer) {
            return Ok(());
        }
        if state.active.iter().any(|key| key.0 == producer) {
            return Err(io_error("cannot seal a producer with active commands"));
        }
        self.journal.lock().append(JournalEvent::ProducerSealed {
            producer: producer.to_owned(),
        })?;
        state.sealed_producers.insert(producer.to_owned());
        let retired = state
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                (key.0 == producer && !state.active.contains(key) && entry.directory.is_none())
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        for key in retired {
            state.entries[&key]
                .status
                .send_replace(CommandResourceStatus::Retired);
            refresh_observation_indexes(&mut state, &key);
            state.acknowledged.insert(key);
        }
        Ok(())
    }

    /// Acknowledge that the producer no longer needs a terminal result's detail.
    /// The identity tombstone remains, so a delayed submission cannot revive it.
    pub fn acknowledge(&self, actor: &str, id: &str) -> std::io::Result<()> {
        let key = validate_key(actor, id)?;
        let mut state = self.state.lock();
        if !state.entries.contains_key(&key) {
            return Err(io_error("unknown command"));
        }
        if state.active.contains(&key) {
            return Err(io_error("cannot acknowledge an active command"));
        }
        if state.acknowledged.contains(&key) {
            return Ok(());
        }
        self.journal.lock().append(JournalEvent::Acknowledged {
            producer: actor.to_owned(),
            actor: actor.to_owned(),
            command: id.to_owned(),
        })?;
        state.entries[&key]
            .status
            .send_replace(CommandResourceStatus::Retired);
        refresh_observation_indexes(&mut state, &key);
        state.acknowledged.insert(key);
        Ok(())
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
        let mut changed = Vec::new();
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
                    let allocation = format!("{}/{}", key.0, key.1);
                    if let Err(error) = self.journal.lock().append(JournalEvent::Allocation {
                        producer: key.0.clone(),
                        actor: key.0.clone(),
                        command: key.1.clone(),
                        requested_bytes: bytes,
                        allocation,
                    }) {
                        let cleanup = std::fs::remove_dir(&directory);
                        if cleanup.is_ok() {
                            state.queue.release(&key);
                            state.active.remove(&key);
                        } else {
                            entry.directory = Some(directory);
                        }
                        entry
                            .status
                            .send_replace(CommandResourceStatus::CleanupUnconfirmed {
                                detail: match cleanup {
                                    Ok(()) => format!("allocation publication failed: {error}"),
                                    Err(cleanup) => format!(
                                        "allocation publication failed: {error}; cleanup failed: {cleanup}"
                                    ),
                                },
                            });
                        changed.push(key);
                        continue;
                    }
                    entry.directory = Some(directory.clone());
                    entry
                        .status
                        .send_replace(CommandResourceStatus::Admitted { cgroup: directory });
                }
                Err(error) => {
                    state.queue.release(&key);
                    state.active.remove(&key);
                    entry
                        .status
                        .send_replace(CommandResourceStatus::CleanupUnconfirmed {
                            detail: error.to_string(),
                        });
                }
            }
            changed.push(key);
        }
        for key in changed {
            refresh_observation_indexes(state, &key);
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
            self.journal.lock().append(JournalEvent::Started {
                producer: actor.to_owned(),
                actor: actor.to_owned(),
                command: id.to_owned(),
            })?;
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
        if let std::collections::hash_map::Entry::Vacant(vacant) = state.entries.entry(key.clone())
        {
            self.journal
                .lock()
                .append(JournalEvent::CancellationFence {
                    producer: actor.to_owned(),
                    actor: actor.to_owned(),
                    command: id.to_owned(),
                })?;
            vacant.insert(Entry::new(
                CommandResourceStatus::CancelledBeforeStart,
                None,
            ));
            return Ok(CommandResourceStatus::CancelledBeforeStart);
        }
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
            self.journal.lock().append(JournalEvent::Terminal {
                producer: actor.to_owned(),
                actor: actor.to_owned(),
                command: id.to_owned(),
                disposition: CommandResourceStatus::CancelledBeforeStart,
            })?;
            entry
                .status
                .send_replace(CommandResourceStatus::CancelledBeforeStart);
            state.queue.release(&key);
            state.active.remove(&key);
            refresh_observation_indexes(&mut state, &key);
            self.admit_waiters(&mut state);
        }
        Ok(state.entries[&key].current())
    }

    fn observe(&self) {
        let mut state = self.state.lock();
        let mut released = Vec::new();
        let active = state.active.iter().cloned().collect::<Vec<_>>();
        for key in active.iter().cloned() {
            let Some(entry) = state.entries.get_mut(&key) else {
                tracing::error!(actor = %key.0, command = %key.1,
                    "active command has no retained ownership entry");
                continue;
            };
            if matches!(
                entry.current(),
                CommandResourceStatus::CleanupUnconfirmed { .. }
            ) {
                continue;
            }
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
                let disposition = if oom > 0 {
                    CommandResourceStatus::ResourceExhausted
                } else {
                    CommandResourceStatus::Completed
                };
                self.journal.lock().append(JournalEvent::Terminal {
                    producer: key.0.clone(),
                    actor: key.0.clone(),
                    command: key.1.clone(),
                    disposition: disposition.clone(),
                })?;
                entry.status.send_replace(disposition);
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
            state.active.remove(&key);
        }
        for key in active {
            refresh_observation_indexes(&mut state, &key);
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

    fn owner(root: &Path) -> CommandResources {
        let policy = policy();
        CommandResources {
            root: root.to_path_buf(),
            state: Mutex::new(State {
                entries: HashMap::new(),
                active: HashSet::new(),
                cleanup_failures: HashSet::new(),
                retained_allocations: HashSet::new(),
                queue: Queue::new(
                    policy.general_bytes,
                    policy.protected_bytes,
                    NATIVE_COMMAND_BYTES,
                ),
                sealed_producers: HashSet::new(),
                acknowledged: HashSet::new(),
            }),
            journal: Mutex::new(Journal::ephemeral()),
            policy,
            actor_starts: Mutex::new(0),
        }
    }

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

    #[test]
    fn replay_rejects_command_identity_reuse_with_a_changed_budget() {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(root.path());
        let events = vec![
            JournalEvent::Admission {
                producer: "actor-1".into(),
                actor: "actor-1".into(),
                command: "command-1".into(),
                requested_bytes: MIB,
            },
            JournalEvent::Admission {
                producer: "actor-1".into(),
                actor: "actor-1".into(),
                command: "command-1".into(),
                requested_bytes: 2 * MIB,
            },
        ];
        assert!(owner.reconcile(events).is_err());
    }

    #[test]
    fn acknowledgment_compacts_detail_and_sealing_fences_late_submissions() {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(root.path());
        let key = ("actor-1".into(), "command-1".into());
        owner
            .state
            .lock()
            .entries
            .insert(key, Entry::new(CommandResourceStatus::Completed, Some(MIB)));

        owner.acknowledge("actor-1", "command-1").unwrap();
        assert_eq!(
            owner.status("actor-1", "command-1").unwrap(),
            CommandResourceStatus::Retired
        );
        owner.seal_producer("actor-1").unwrap();
        assert!(owner.submit("actor-1", "late-command", MIB).is_err());
    }

    #[test]
    fn cancellation_before_submission_is_a_durable_identity_fence() {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(root.path());
        owner
            .reconcile(vec![JournalEvent::CancellationFence {
                producer: "actor-1".into(),
                actor: "actor-1".into(),
                command: "command-1".into(),
            }])
            .unwrap();
        assert_eq!(
            owner.submit("actor-1", "command-1", MIB).unwrap(),
            CommandResourceStatus::CancelledBeforeStart
        );
    }

    #[test]
    #[ignore = "measurement harness; run explicitly at integration boundaries"]
    fn retained_history_does_not_scale_resource_polling() {
        fn rss_kib() -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
                .unwrap_or(0)
        }
        fn poll(owner: &CommandResources, count: usize) -> u128 {
            let started = std::time::Instant::now();
            for _ in 0..count {
                std::hint::black_box(owner.observation());
            }
            started.elapsed().as_nanos() / count as u128
        }

        let root = tempfile::tempdir().unwrap();
        let owner = owner(root.path());
        let empty_ns = poll(&owner, 2_000);
        let rss_before = rss_kib();
        {
            let mut state = owner.state.lock();
            for ordinal in 0..100_000 {
                state.entries.insert(
                    ("retired".into(), format!("command-{ordinal}")),
                    Entry::new(CommandResourceStatus::Completed, Some(MIB)),
                );
            }
            rebuild_observation_indexes(&mut state);
        }
        let retained_ns = poll(&owner, 2_000);
        let observation = owner.observation();
        eprintln!(
            "resource_poll empty_ns={empty_ns} retained_ns={retained_ns} historical={} rss_delta_kib={}",
            observation.historical,
            rss_kib().saturating_sub(rss_before),
        );
        assert_eq!(observation.historical, 100_000);
        assert_eq!(observation.active, 0);
    }
}
