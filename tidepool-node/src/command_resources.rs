//! Command-tree admission and cgroup custody. Control processes stay outside the pool.
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandResourcePolicy {
    pub concurrency: usize,
    pub memory_high_bytes: Option<u64>,
    pub memory_max_bytes: u64,
    pub swap_max_bytes: u64,
    pub queue_timeout_seconds: u64,
    pub machine_headroom_bytes: u64,
    pub actor_start_bytes: u64,
}
impl Default for CommandResourcePolicy {
    fn default() -> Self {
        const GIB: u64 = 1024 * 1024 * 1024;
        Self {
            concurrency: 2,
            memory_high_bytes: None,
            memory_max_bytes: 8 * GIB,
            swap_max_bytes: GIB,
            queue_timeout_seconds: 300,
            machine_headroom_bytes: 6 * GIB,
            actor_start_bytes: GIB,
        }
    }
}
impl CommandResourcePolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.concurrency == 0
            || self.concurrency > Semaphore::MAX_PERMITS
            || self.actor_start_bytes == 0
            || self.memory_max_bytes == 0
            || self
                .memory_high_bytes
                .is_some_and(|high| high == 0 || high > self.memory_max_bytes)
            || self.queue_timeout_seconds == 0
            || self.queue_timeout_seconds > 300
        {
            return Err("invalid command resource limits".into());
        }
        self.memory_max_bytes
            .checked_mul(self.concurrency as u64)
            .and_then(|n| n.checked_add(self.machine_headroom_bytes))
            .ok_or("command resource limits overflow")?;
        self.machine_headroom_bytes
            .checked_add(self.actor_start_bytes)
            .ok_or("actor resource limits overflow")?;
        self.swap_max_bytes
            .checked_mul(self.concurrency as u64)
            .ok_or("command swap limits overflow")?;
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CommandResourceStatus {
    Queued,
    Admitted { cgroup: PathBuf },
    Running,
    Completed,
    ResourceExhausted,
    AdmissionTimedOut,
    CancelledBeforeStart,
    CleanupUnconfirmed { detail: String },
}
struct Entry {
    status: CommandResourceStatus,
    directory: Option<PathBuf>,
    permit: Option<OwnedSemaphorePermit>,
    admitted: Instant,
    started: bool,
    cancel: Arc<Notify>,
}
pub struct CommandResources {
    root: PathBuf,
    policy: CommandResourcePolicy,
    slots: Arc<Semaphore>,
    entries: Mutex<HashMap<(String, String), Entry>>,
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
        && key.len() <= 100
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
impl CommandResources {
    /// Called once by the host inside its delegated systemd scope.
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
        std::fs::create_dir(&root)?;
        std::fs::write(
            root.join("memory.max"),
            (policy.memory_max_bytes * policy.concurrency as u64).to_string(),
        )?;
        std::fs::write(
            root.join("memory.swap.max"),
            (policy.swap_max_bytes * policy.concurrency as u64).to_string(),
        )?;
        std::fs::write(root.join("cgroup.subtree_control"), "+memory")?;
        let owner = Arc::new(Self {
            root,
            slots: Arc::new(Semaphore::new(policy.concurrency)),
            policy,
            entries: Mutex::new(HashMap::new()),
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
    pub async fn acquire(
        self: &Arc<Self>,
        actor: &str,
        id: &str,
    ) -> std::io::Result<CommandResourceStatus> {
        if !valid_key(actor) || !valid_key(id) {
            return Err(io_error("invalid command identity"));
        }
        let key = (actor.to_owned(), id.to_owned());
        let cancel = Arc::new(Notify::new());
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.get(&key) {
                return Ok(entry.status.clone());
            }
            entries.insert(
                key.clone(),
                Entry {
                    status: CommandResourceStatus::Queued,
                    directory: None,
                    permit: None,
                    admitted: Instant::now(),
                    started: false,
                    cancel: cancel.clone(),
                },
            );
        }
        let _admission = PendingAdmission {
            owner: self.clone(),
            actor: actor.to_owned(),
            id: id.to_owned(),
        };
        let permit = tokio::select! {
            _=cancel.notified()=>None,
            result=tokio::time::timeout(Duration::from_secs(self.policy.queue_timeout_seconds),self.slots.clone().acquire_owned())=>result.ok().and_then(Result::ok),
        };
        let mut entries = self.entries.lock();
        let entry = entries
            .get_mut(&key)
            .ok_or_else(|| io_error("command admission identity lost"))?;
        if !matches!(entry.status, CommandResourceStatus::Queued) {
            return Ok(entry.status.clone());
        }
        let Some(permit) = permit else {
            entry.status = CommandResourceStatus::AdmissionTimedOut;
            return Ok(entry.status.clone());
        };
        let dir = match self.actor_directory(actor).and_then(|parent| {
            let dir = parent.join(id);
            std::fs::create_dir(&dir)?;
            Ok(dir)
        }) {
            Ok(dir) => dir,
            Err(error) => {
                entry.status = CommandResourceStatus::CancelledBeforeStart;
                return Err(error);
            }
        };
        let configured = (|| {
            std::fs::write(
                dir.join("memory.high"),
                self.policy
                    .memory_high_bytes
                    .map_or_else(|| "max".to_owned(), |high| high.to_string()),
            )?;
            std::fs::write(
                dir.join("memory.max"),
                self.policy.memory_max_bytes.to_string(),
            )?;
            std::fs::write(
                dir.join("memory.swap.max"),
                self.policy.swap_max_bytes.to_string(),
            )?;
            std::fs::write(dir.join("memory.oom.group"), "1")
        })();
        if let Err(error) = configured {
            let _ = std::fs::remove_dir(&dir);
            entry.status = CommandResourceStatus::CancelledBeforeStart;
            return Err(error);
        }
        entry.status = CommandResourceStatus::Admitted {
            cgroup: dir.clone(),
        };
        entry.directory = Some(dir);
        entry.permit = Some(permit);
        entry.admitted = Instant::now();
        Ok(entry.status.clone())
    }
    pub fn started(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        let mut entries = self.entries.lock();
        let entry = entries
            .get_mut(&(actor.into(), id.into()))
            .ok_or_else(|| io_error("unknown command"))?;
        if matches!(entry.status, CommandResourceStatus::Admitted { .. }) {
            entry.started = true;
            entry.status = CommandResourceStatus::Running;
        }
        Ok(entry.status.clone())
    }
    pub fn status(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        self.observe();
        self.entries
            .lock()
            .get(&(actor.into(), id.into()))
            .map(|e| e.status.clone())
            .ok_or_else(|| io_error("unknown command"))
    }
    pub fn cancel(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        if !valid_key(actor) || !valid_key(id) {
            return Err(io_error("invalid command identity"));
        }
        let mut entries = self.entries.lock();
        // Cancellation can arrive before the admission request on another connection.
        // Retain the identity so a delayed request cannot launch it afterward.
        let entry = entries
            .entry((actor.into(), id.into()))
            .or_insert_with(|| Entry {
                status: CommandResourceStatus::CancelledBeforeStart,
                directory: None,
                permit: None,
                admitted: Instant::now(),
                started: false,
                cancel: Arc::new(Notify::new()),
            });
        if matches!(entry.status, CommandResourceStatus::Queued) {
            entry.status = CommandResourceStatus::CancelledBeforeStart;
            entry.cancel.notify_one();
        }
        // Admitted commands may already have executed: cancellation is not cleanup.
        Ok(entry.status.clone())
    }
    fn observe(&self) {
        let mut entries = self.entries.lock();
        for entry in entries.values_mut() {
            let Some(dir) = entry.directory.as_ref() else {
                continue;
            };
            let result = (|| -> std::io::Result<bool> {
                let populated = read_counter(&dir.join("cgroup.events"), "populated")?;
                let oom = read_counter(&dir.join("memory.events"), "oom_kill")?;
                if oom > 0 {
                    entry.status = CommandResourceStatus::ResourceExhausted;
                }
                if populated > 0 {
                    entry.started = true;
                    return Ok(false);
                }
                if !entry.started && entry.admitted.elapsed() < Duration::from_secs(30) {
                    return Ok(false);
                }
                // Removing an empty group invalidates retained join descriptors too.
                std::fs::remove_dir(dir)?;
                if oom == 0 {
                    entry.status = if entry.started {
                        CommandResourceStatus::Completed
                    } else {
                        CommandResourceStatus::CancelledBeforeStart
                    };
                }
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    entry.directory = None;
                    entry.permit = None;
                }
                Ok(false) => {}
                Err(error) => {
                    entry.status = CommandResourceStatus::CleanupUnconfirmed {
                        detail: error.to_string(),
                    }
                }
            }
        }
    }
    pub async fn admit_actor(self: &Arc<Self>) -> std::io::Result<ActorStartReservation> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.policy.queue_timeout_seconds);
        loop {
            let available = read_counter(Path::new("/proc/meminfo"), "MemAvailable:")? * 1024;
            let used = std::fs::read_to_string(self.root.join("memory.current"))?
                .trim()
                .parse::<u64>()
                .map_err(|e| io_error(e.to_string()))?;
            let unspent = (self.policy.memory_max_bytes * self.policy.concurrency as u64)
                .saturating_sub(used);
            {
                let mut pending = self.actor_starts.lock();
                if available.saturating_sub(unspent).saturating_sub(*pending)
                    >= self.policy.machine_headroom_bytes + self.policy.actor_start_bytes
                {
                    *pending += self.policy.actor_start_bytes;
                    return Ok(ActorStartReservation {
                        owner: self.clone(),
                    });
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(io_error(
                    "actor resource admission timed out; actor not started",
                ));
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

// Dropping an HTTP admission future must not leave a queued command behind.
struct PendingAdmission {
    owner: Arc<CommandResources>,
    actor: String,
    id: String,
}
impl Drop for PendingAdmission {
    fn drop(&mut self) {
        let _ = self.owner.cancel(&self.actor, &self.id);
    }
}
