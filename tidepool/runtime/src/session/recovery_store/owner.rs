//! The native owner serializes run transitions; its blocking worker alone owns SQLite.
use super::*;
use std::collections::VecDeque;
use std::sync::mpsc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeStoreStatus {
    Ready,
    Pending,
    Installing,
    Fenced(StoreError),
}

enum Reply<T> {
    Blocking(mpsc::SyncSender<T>),
    Async(tokio::sync::oneshot::Sender<T>),
}
impl<T> Reply<T> {
    fn is_closed(&self) -> bool {
        match self {
            Self::Async(reply) => reply.is_closed(),
            Self::Blocking(_) => false,
        }
    }
    fn send(self, value: T) -> bool {
        match self {
            Self::Blocking(reply) => reply.send(value).is_ok(),
            Self::Async(reply) => reply.send(value).is_ok(),
        }
    }
}

enum Operation {
    ReadDomain {
        domain: RecoveryDomainId,
    },
    ReadGeneration {
        domain: RecoveryDomainId,
        actor: RecoveryPublicOwner,
    },
    ReadOwner {
        domain: RecoveryDomainId,
        owner: RecoveryPublicOwner,
    },
    Owners {
        domain: RecoveryDomainId,
    },
    HasOwners {
        domain: RecoveryDomainId,
    },
    Snapshot {
        domain: RecoveryDomainId,
        owner: Option<RecoveryPublicOwner>,
    },
    Get {
        cid: Cid,
    },
    Create {
        domain: RecoveryDomainId,
        lineage: u64,
        ceiling: Generation,
        root: DomainSnapshotRoot,
        blocks: Vec<(Cid, Arc<[u8]>)>,
    },
    Ceiling {
        domain: RecoveryDomainId,
        expected: StoreRevision,
        minimum: Generation,
    },
    Publish {
        prepared: PreparedRecoveryWrite,
    },
    Transfer {
        domain: RecoveryDomainId,
        expected: StoreRevision,
        previous: RecoveryPublicOwnerState,
        successor: RecoveryPublicOwner,
        successor_epoch: u64,
    },
    Check {},
    #[cfg(test)]
    Fault(StoreFault),
}
enum Value {
    ReadDomain(Result<Option<RecoveryDomainState>, StoreError>),
    ReadOwner(Result<Option<RecoveryPublicOwnerState>, StoreError>),
    ReadGeneration(Result<Option<super::super::ActiveNotebookGeneration>, StoreError>),
    Owners(Result<Vec<RecoveryPublicOwnerState>, StoreError>),
    HasOwners(Result<bool, StoreError>),
    Snapshot(Result<Option<RecoverySnapshotIdentity>, StoreError>),
    Get(anyhow::Result<Option<Vec<u8>>>),
    Create(StoreDisposition<RecoveryDomainState>),
    Ceiling(StoreDisposition<CeilingCommit>),
    Publish(StoreDisposition<RecoveryDomainState>),
    Transfer(StoreDisposition<RecoveryDomainState>),
    Check(Result<(), StoreError>),
    #[cfg(test)]
    Fault,
}
impl Value {
    fn fence(&self) -> Option<StoreError> {
        match self {
            Self::Create(value) | Self::Publish(value) | Self::Transfer(value) => match value {
                StoreDisposition::RecoveryRequired(error) => Some(error.clone()),
                StoreDisposition::Committed(_) | StoreDisposition::Refused(_) => None,
            },
            Self::Ceiling(value) => match value {
                StoreDisposition::RecoveryRequired(error) => Some(error.clone()),
                StoreDisposition::Committed(_) | StoreDisposition::Refused(_) => None,
            },
            Self::ReadDomain(_)
            | Self::ReadOwner(_)
            | Self::ReadGeneration(_)
            | Self::Owners(_)
            | Self::HasOwners(_)
            | Self::Snapshot(_)
            | Self::Get(_)
            | Self::Check(_) => None,
            #[cfg(test)]
            Self::Fault => None,
        }
    }
    fn committed_installation(&self) -> Option<RecoveryDomainState> {
        match self {
            Self::Publish(StoreDisposition::Committed(state))
            | Self::Transfer(StoreDisposition::Committed(state)) => Some(state.clone()),
            _ => None,
        }
    }
}
impl Operation {
    fn execute(self, storage: &NativeStorage) -> Value {
        match self {
            Self::ReadGeneration { domain, actor } => {
                Value::ReadGeneration(storage.active_notebook_generation(domain, &actor))
            }
            Self::ReadDomain { domain } => Value::ReadDomain(storage.read_domain(domain)),
            Self::ReadOwner { domain, owner } => {
                Value::ReadOwner(storage.read_public_owner(domain, &owner))
            }
            Self::Owners { domain } => Value::Owners(storage.public_owner_states(domain)),
            Self::HasOwners { domain } => Value::HasOwners(storage.has_public_owners(domain)),
            Self::Snapshot { domain, owner } => {
                Value::Snapshot(storage.read_snapshot_identity(domain, owner.as_ref()))
            }
            Self::Get { cid } => Value::Get(storage.get(&cid)),
            Self::Create {
                domain,
                lineage,
                ceiling,
                root,
                blocks,
            } => Value::Create(storage.create_domain(domain, lineage, ceiling, root, blocks)),
            Self::Ceiling {
                domain,
                expected,
                minimum,
            } => Value::Ceiling(storage.reserve_ceiling(domain, expected, minimum)),
            Self::Publish { prepared } => Value::Publish(storage.publish(prepared)),
            Self::Transfer {
                domain,
                expected,
                previous,
                successor,
                successor_epoch,
            } => Value::Transfer(storage.transfer_owner(
                domain,
                expected,
                &previous,
                successor,
                successor_epoch,
            )),
            Self::Check {} => Value::Check(storage.ensure_ready()),
            #[cfg(test)]
            Self::Fault(fault) => {
                storage.inject_fault(fault);
                Value::Fault
            }
        }
    }
}
struct Response {
    value: Value,
    installation: Option<OwnedTransition>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum SettlementOutcome {
    Completed,
    Refused,
    Installed(RecoveryDomainState),
}
enum Command {
    Request {
        store: RunRecoveryStore,
        operation: Operation,
        reply: Reply<Result<Response, StoreError>>,
    },
    Begin {
        store: RunRecoveryStore,
        reply: Reply<Result<OwnedTransition, StoreError>>,
    },
    Settle {
        id: u64,
        outcome: SettlementOutcome,
        reply: Reply<Result<(), StoreError>>,
    },
    Abandon {
        id: u64,
    },
    Status {
        reply: Reply<NativeStoreStatus>,
    },
    Work {
        reply: Reply<Result<RecoveryStoreWork, StoreError>>,
    },
    WaitFence {
        reply: Reply<StoreError>,
    },
    Finished {
        value: Value,
        work: RecoveryStoreWork,
    },
    WorkerFailed(StoreError),
    Shutdown,
}
struct WorkerRequest(Operation);

enum Phase {
    Ready,
    Working {
        id: u64,
        store: RunRecoveryStore,
        reply: Reply<Result<Response, StoreError>>,
    },
    Checking {
        id: u64,
        store: RunRecoveryStore,
        reply: Reply<Result<OwnedTransition, StoreError>>,
    },
    Installing {
        id: u64,
        state: RecoveryDomainState,
    },
    Transition(u64),
    Fenced(StoreError),
}

#[derive(Clone)]
pub struct RunRecoveryStore {
    path: Arc<PathBuf>,
    database_id: [u8; 16],
    handle: Arc<OwnerHandle>,
}
struct OwnerHandle {
    sender: mpsc::Sender<Command>,
}
impl Drop for OwnerHandle {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
    }
}
enum OpenMode {
    Create,
    Open,
    ReadOnly,
}
impl RunRecoveryStore {
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        Self::start(path, OpenMode::Create)
    }
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::start(path, OpenMode::Open)
    }
    pub(crate) fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        Self::start(path, OpenMode::ReadOnly)
    }
    pub async fn create_async(path: PathBuf) -> Result<Self, StoreError> {
        let error_path = path.clone();
        tokio::task::spawn_blocking(move || Self::create(&path))
            .await
            .map_err(|error| store_error(&error_path, StoreErrorKind::RecoveryRequired, error))?
    }
    pub async fn open_async(path: PathBuf) -> Result<Self, StoreError> {
        let error_path = path.clone();
        tokio::task::spawn_blocking(move || Self::open(&path))
            .await
            .map_err(|error| store_error(&error_path, StoreErrorKind::RecoveryRequired, error))?
    }
    fn start(path: &Path, mode: OpenMode) -> Result<Self, StoreError> {
        let path = Arc::new(path.to_owned());
        let (sender, receiver) = mpsc::channel();
        let (worker, work_queue) = mpsc::channel::<WorkerRequest>();
        let completed = sender.clone();
        let (initialized, initialization) = mpsc::sync_channel(1);
        let storage_path = path.clone();
        std::thread::Builder::new()
            .name("run-native-storage".into())
            .spawn(move || {
                let opened =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match mode {
                        OpenMode::Create => NativeStorage::create(&storage_path),
                        OpenMode::Open => NativeStorage::open(&storage_path),
                        OpenMode::ReadOnly => NativeStorage::open_read_only(&storage_path),
                    }))
                    .unwrap_or_else(|_| {
                        Err(store_error(
                            &storage_path,
                            StoreErrorKind::RecoveryRequired,
                            "native storage worker panicked while opening",
                        ))
                    });
                let storage = match opened {
                    Ok(storage) => storage,
                    Err(error) => {
                        let _ = initialized.send(Err(error));
                        return;
                    }
                };
                if initialized.send(Ok(storage.database_id)).is_err() {
                    return;
                }
                for WorkerRequest(operation) in work_queue {
                    let value = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        operation.execute(&storage)
                    })) {
                        Ok(value) => value,
                        Err(_) => {
                            let _ = completed.send(Command::WorkerFailed(store_error(
                                storage.path(),
                                StoreErrorKind::RecoveryRequired,
                                "native storage worker panicked during an operation",
                            )));
                            break;
                        }
                    };
                    if completed
                        .send(Command::Finished {
                            value,
                            work: storage.work(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| store_error(&path, StoreErrorKind::Storage, error))?;
        let database_id = initialization.recv().map_err(|_| {
            store_error(
                &path,
                StoreErrorKind::RecoveryRequired,
                "native storage initialization acknowledgement lost",
            )
        })??;
        let owner_path = path.clone();
        std::thread::Builder::new().name("run-native-owner".into()).spawn(move || {
            let mut phase = Phase::Ready;
            let mut pending = VecDeque::new();
            let mut next = 0u64;
            let mut work = RecoveryStoreWork::default();
            let mut watchers: Vec<Reply<StoreError>> = Vec::new();
            let mut settled = std::collections::BTreeMap::<u64, SettlementOutcome>::new();
            loop {
                watchers.retain(|observer| !observer.is_closed());
                let command = if matches!(phase, Phase::Ready | Phase::Fenced(_)) && !pending.is_empty() {
                    pending.pop_front().expect("nonempty deferred queue")
                } else {
                    match receiver.recv() { Ok(command) => command, Err(_) => break }
                };
                match command {
                    Command::Shutdown => break,
                    Command::WorkerFailed(error) => {
                        let old = std::mem::replace(&mut phase, Phase::Fenced(error.clone()));
                        match old {
                            Phase::Working { reply, .. } => { reply.send(Err(error)); }
                            Phase::Checking { reply, .. } => { reply.send(Err(error)); }
                            Phase::Fenced(original) => { phase = Phase::Fenced(original); }
                            _ => {}
                        }
                    }
                    Command::Status { reply } => {
                        let status = match &phase {
                            Phase::Ready => NativeStoreStatus::Ready,
                            Phase::Working { .. } | Phase::Checking { .. } | Phase::Transition(_) => NativeStoreStatus::Pending,
                            Phase::Installing { .. } => NativeStoreStatus::Installing,
                            Phase::Fenced(error) => NativeStoreStatus::Fenced(error.clone()),
                        };
                        reply.send(status);
                    }
                    Command::Work { reply } => { reply.send(Ok(work)); }
                    Command::WaitFence { reply } => {
                        if let Phase::Fenced(error) = &phase { reply.send(error.clone()); }
                        else { watchers.push(reply); }
                    }
                    Command::Begin { store, reply } => match &phase {
                        Phase::Ready => {
                            next = match next.checked_add(1) {
                                Some(next) => next,
                                None => { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native operation identity exhausted"))); continue; }
                            };
                            phase = Phase::Checking { id: next, store, reply };
                            if worker.send(WorkerRequest(Operation::Check {})).is_err() {
                                let old = std::mem::replace(&mut phase, Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped")));
                                if let Phase::Checking { reply, .. } = old { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped"))); }
                            }
                        }
                        Phase::Fenced(error) => { reply.send(Err(error.clone())); }
                        _ => pending.push_back(Command::Begin { store, reply }),
                    },
                    Command::Settle { id, outcome, reply } => {
                        if let Some(completed) = settled.get(&id) {
                            let result = if completed == &outcome { Ok(()) } else { Err(store_error(&owner_path, StoreErrorKind::Integrity, "native settlement conflicts with retained completion")) };
                            reply.send(result);
                            continue;
                        }
                        let admitted = match (&phase, &outcome) {
                            (Phase::Transition(current), SettlementOutcome::Completed | SettlementOutcome::Refused) => *current == id,
                            (Phase::Installing { id: current, state }, SettlementOutcome::Installed(installed)) => *current == id && state == installed,
                            _ => false,
                        };
                        if admitted {
                            phase = Phase::Ready;
                            settled.insert(id, outcome);
                            // Local completion is authoritative; losing its response observer does not undo it.
                            reply.send(Ok(()));
                        } else if let Phase::Fenced(error) = &phase { reply.send(Err(error.clone())); }
                        else { reply.send(Err(store_error(&owner_path, StoreErrorKind::Integrity, "native settlement does not own the outstanding operation"))); }
                    }
                    Command::Abandon { id } => {
                        if matches!(&phase, Phase::Transition(current) if *current == id) || matches!(&phase, Phase::Installing { id: current, .. } if *current == id) {
                            phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native transition abandoned before explicit settlement"));
                        }
                    }
                    Command::Request { store, operation, reply } => match &phase {
                        Phase::Ready => {
                            next = match next.checked_add(1) {
                                Some(next) => next,
                                None => { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native operation identity exhausted"))); continue; }
                            };
                            phase = Phase::Working { id: next, store, reply };
                            if worker.send(WorkerRequest(operation)).is_err() {
                                let old = std::mem::replace(&mut phase, Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped")));
                                if let Phase::Working { reply, .. } = old { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped"))); }
                            }
                        }
                        Phase::Fenced(error) => { reply.send(Err(error.clone())); }
                        _ => pending.push_back(Command::Request { store, operation, reply }),
                    },
                    Command::Finished { value, work: observed } => {
                        work = observed;
                        let old = std::mem::replace(&mut phase, Phase::Ready);
                        if let Phase::Working { id, store, reply } = old {
                            if let Some(error) = value.fence() {
                                phase = Phase::Fenced(error);
                            } else if let Some(state) = value.committed_installation() {
                                phase = Phase::Installing { id, state };
                            }
                            let installation = matches!(phase, Phase::Installing { .. }).then(|| OwnedTransition { store, id, settled: false });
                            let committed = installation.is_some();
                            if !reply.send(Ok(Response { value, installation })) && committed {
                                phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "committed native installation acknowledgement lost"));
                            }
                        } else if let Phase::Checking { id, store, reply } = old {
                            match value {
                                Value::Check(Ok(())) => {
                                    phase = Phase::Transition(id);
                                    if !reply.send(Ok(OwnedTransition { store, id, settled: false })) { phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native grant acknowledgement lost")); }
                                }
                                Value::Check(Err(error)) => { reply.send(Err(error)); }
                                _ => { phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native grant check has wrong completion")); }
                            }
                        } else if let Phase::Fenced(error) = old {
                            phase = Phase::Fenced(error);
                        } else {
                            phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage completion has no owning request"));
                        }
                    }
                }
                if let Phase::Fenced(error) = &phase {
                    for watcher in watchers.drain(..) { watcher.send(error.clone()); }
                }
            }
        }).map_err(|error| store_error(&path, StoreErrorKind::Storage, error))?;
        Ok(Self {
            path,
            database_id,
            handle: Arc::new(OwnerHandle { sender }),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn database_id(&self) -> [u8; 16] {
        self.database_id
    }
    pub(crate) fn shares_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.handle, &other.handle)
    }
    fn stopped(&self) -> StoreError {
        store_error(
            self.path(),
            StoreErrorKind::RecoveryRequired,
            "native owner stopped",
        )
    }
    fn request(&self, operation: Operation) -> Result<Response, StoreError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.handle
            .sender
            .send(Command::Request {
                store: self.clone(),
                operation,
                reply: Reply::Blocking(reply),
            })
            .map_err(|_| self.stopped())?;
        result.recv().map_err(|_| self.stopped())?
    }
    pub async fn ensure_ready_async(&self) -> Result<(), StoreError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.handle
            .sender
            .send(Command::Request {
                store: self.clone(),
                operation: Operation::Check {},
                reply: Reply::Async(reply),
            })
            .map_err(|_| self.stopped())?;
        match result.await.map_err(|_| self.stopped())?? {
            Response {
                value: Value::Check(result),
                ..
            } => result,
            _ => unreachable!("typed native readiness response differs from request"),
        }
    }
    pub fn status(&self) -> NativeStoreStatus {
        let (reply, result) = mpsc::sync_channel(1);
        if self
            .handle
            .sender
            .send(Command::Status {
                reply: Reply::Blocking(reply),
            })
            .is_err()
        {
            return NativeStoreStatus::Fenced(self.stopped());
        }
        result
            .recv()
            .unwrap_or_else(|_| NativeStoreStatus::Fenced(self.stopped()))
    }
    pub async fn status_async(&self) -> NativeStoreStatus {
        let (reply, result) = tokio::sync::oneshot::channel();
        if self
            .handle
            .sender
            .send(Command::Status {
                reply: Reply::Async(reply),
            })
            .is_err()
        {
            return NativeStoreStatus::Fenced(self.stopped());
        }
        result
            .await
            .unwrap_or_else(|_| NativeStoreStatus::Fenced(self.stopped()))
    }
    pub fn is_recovery_required(&self) -> bool {
        matches!(self.status(), NativeStoreStatus::Fenced(_))
    }
    pub async fn wait_recovery_required(&self) -> StoreError {
        let (reply, result) = tokio::sync::oneshot::channel();
        if self
            .handle
            .sender
            .send(Command::WaitFence {
                reply: Reply::Async(reply),
            })
            .is_err()
        {
            return self.stopped();
        }
        result.await.unwrap_or_else(|_| self.stopped())
    }
    /// Work reported by the last completed storage operation. A failed owner
    /// observation is an error, never fabricated zero work.
    pub fn work(&self) -> Result<RecoveryStoreWork, StoreError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.handle
            .sender
            .send(Command::Work {
                reply: Reply::Blocking(reply),
            })
            .map_err(|_| self.stopped())?;
        result.recv().map_err(|_| self.stopped())?
    }
    pub fn read_snapshot_at(
        path: &Path,
        domain: RecoveryDomainId,
        owner: Option<&RecoveryPublicOwner>,
    ) -> Result<Option<RecoverySnapshotIdentity>, StoreError> {
        NativeStorage::read_snapshot_at(path, domain, owner)
    }
    pub fn read_selected_snapshot_at(
        path: &Path,
        domain: RecoveryDomainId,
        actor: &RecoveryPublicOwner,
    ) -> Result<Option<RecoverySnapshotIdentity>, StoreError> {
        NativeStorage::read_selected_snapshot_at(path, domain, actor)
    }
    /// Read-only observation of persisted generation references. Issuing
    /// owners must independently restore and validate the referenced inputs.
    pub fn read_notebook_generation_at(
        path: &Path,
        domain: RecoveryDomainId,
        actor: &RecoveryPublicOwner,
    ) -> Result<Option<super::super::ActiveNotebookGeneration>, StoreError> {
        NativeStorage::read_notebook_generation_at(path, domain, actor)
    }
    pub async fn begin_provider_transition(&self) -> Result<ProviderTransition, StoreError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.handle
            .sender
            .send(Command::Begin {
                store: self.clone(),
                reply: Reply::Async(reply),
            })
            .map_err(|_| self.stopped())?;
        let ticket = result.await.map_err(|_| self.stopped())??;
        Ok(ProviderTransition {
            ticket: Some(ticket),
            attachment: None,
            store_lifetime: None,
        })
    }
    pub(crate) fn begin_native_transition(&self) -> Result<OwnedTransition, StoreError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.handle
            .sender
            .send(Command::Begin {
                store: self.clone(),
                reply: Reply::Blocking(reply),
            })
            .map_err(|_| self.stopped())?;
        result.recv().map_err(|_| self.stopped())?
    }
    pub fn read_domain(
        &self,
        domain: RecoveryDomainId,
    ) -> Result<Option<RecoveryDomainState>, StoreError> {
        match self.request(Operation::ReadDomain { domain }) {
            Err(error) => Err(error),
            Ok(Response {
                value: Value::ReadDomain(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn active_notebook_generation(
        &self,
        domain: RecoveryDomainId,
        actor: &RecoveryPublicOwner,
    ) -> Result<Option<super::super::ActiveNotebookGeneration>, StoreError> {
        match self.request(Operation::ReadGeneration {
            domain,
            actor: actor.clone(),
        })? {
            Response {
                value: Value::ReadGeneration(value),
                ..
            } => value,
            _ => unreachable!("typed native generation read differs from request"),
        }
    }
    pub fn read_public_owner(
        &self,
        domain: RecoveryDomainId,
        owner: &RecoveryPublicOwner,
    ) -> Result<Option<RecoveryPublicOwnerState>, StoreError> {
        match self.request(Operation::ReadOwner {
            domain,
            owner: owner.clone(),
        }) {
            Err(error) => Err(error),
            Ok(Response {
                value: Value::ReadOwner(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn public_owner_states(
        &self,
        domain: RecoveryDomainId,
    ) -> Result<Vec<RecoveryPublicOwnerState>, StoreError> {
        match self.request(Operation::Owners { domain }) {
            Err(error) => Err(error),
            Ok(Response {
                value: Value::Owners(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn has_public_owners(&self, domain: RecoveryDomainId) -> Result<bool, StoreError> {
        match self.request(Operation::HasOwners { domain }) {
            Err(error) => Err(error),
            Ok(Response {
                value: Value::HasOwners(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn read_snapshot_identity(
        &self,
        domain: RecoveryDomainId,
        owner: Option<&RecoveryPublicOwner>,
    ) -> Result<Option<RecoverySnapshotIdentity>, StoreError> {
        match self.request(Operation::Snapshot {
            domain,
            owner: owner.cloned(),
        }) {
            Err(error) => Err(error),
            Ok(Response {
                value: Value::Snapshot(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn get_block(&self, cid: Cid) -> anyhow::Result<Option<Vec<u8>>> {
        match self.request(Operation::Get { cid }) {
            Err(error) => Err(anyhow::Error::new(error)),
            Ok(Response {
                value: Value::Get(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn create_domain(
        &self,
        domain: RecoveryDomainId,
        lineage: u64,
        ceiling: Generation,
        root: DomainSnapshotRoot,
        blocks: Vec<(Cid, Arc<[u8]>)>,
    ) -> StoreDisposition<RecoveryDomainState> {
        match self.request(Operation::Create {
            domain,
            lineage,
            ceiling,
            root,
            blocks,
        }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response {
                value: Value::Create(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn reserve_ceiling(
        &self,
        domain: RecoveryDomainId,
        expected: StoreRevision,
        minimum: Generation,
    ) -> StoreDisposition<CeilingCommit> {
        match self.request(Operation::Ceiling {
            domain,
            expected,
            minimum,
        }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response {
                value: Value::Ceiling(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn publish(
        &self,
        prepared: PreparedRecoveryWrite,
    ) -> StoreDisposition<RecoveryDomainState> {
        match self.request(Operation::Publish { prepared }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response {
                value: Value::Publish(value),
                installation,
            }) => {
                if let Some(ticket) = installation {
                    let StoreDisposition::Committed(state) = &value else {
                        unreachable!("installation ticket without committed state");
                    };
                    if let Err(error) =
                        ticket.finish_blocking(SettlementOutcome::Installed(state.clone()))
                    {
                        return StoreDisposition::RecoveryRequired(error);
                    }
                }
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn transfer_owner(
        &self,
        domain: RecoveryDomainId,
        expected: StoreRevision,
        previous: &RecoveryPublicOwnerState,
        successor: RecoveryPublicOwner,
        successor_epoch: u64,
    ) -> StoreDisposition<RecoveryDomainState> {
        match self.request(Operation::Transfer {
            domain,
            expected,
            previous: previous.clone(),
            successor,
            successor_epoch,
        }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response {
                value: Value::Transfer(value),
                installation,
            }) => {
                if let Some(ticket) = installation {
                    let StoreDisposition::Committed(state) = &value else {
                        unreachable!("installation ticket without committed state");
                    };
                    if let Err(error) =
                        ticket.finish_blocking(SettlementOutcome::Installed(state.clone()))
                    {
                        return StoreDisposition::RecoveryRequired(error);
                    }
                }
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn ensure_ready(&self) -> Result<(), StoreError> {
        match self.request(Operation::Check {}) {
            Err(error) => Err(error),
            Ok(Response {
                value: Value::Check(value),
                installation,
            }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn commit_publication(
        &self,
        prepared: PreparedRecoveryWrite,
    ) -> StoreDisposition<NativeInstallationReceipt> {
        self.commit_installation(Operation::Publish { prepared })
    }
    fn commit_installation(
        &self,
        operation: Operation,
    ) -> StoreDisposition<NativeInstallationReceipt> {
        match self.request(operation) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response {
                value: Value::Publish(value) | Value::Transfer(value),
                installation,
            }) => match value {
                StoreDisposition::Committed(state) => {
                    StoreDisposition::Committed(NativeInstallationReceipt {
                        state,
                        ticket: Some(installation.expect("committed installation owns receipt")),
                    })
                }
                StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
                StoreDisposition::RecoveryRequired(error) => {
                    StoreDisposition::RecoveryRequired(error)
                }
            },
            Ok(_) => unreachable!("typed native installation response differs from request"),
        }
    }
    #[cfg(test)]
    pub(super) fn publish_and_install<T>(
        &self,
        prepared: PreparedRecoveryWrite,
        install: impl FnOnce(RecoveryDomainState) -> T,
    ) -> StoreDisposition<T> {
        match self.commit_publication(prepared) {
            StoreDisposition::Committed(receipt) => {
                let value = install(receipt.state.clone());
                match receipt.installed_blocking() {
                    Ok(()) => StoreDisposition::Committed(value),
                    Err(error) => StoreDisposition::RecoveryRequired(error),
                }
            }
            StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
            StoreDisposition::RecoveryRequired(error) => StoreDisposition::RecoveryRequired(error),
        }
    }
    pub(crate) fn commit_owner_transfer(
        &self,
        domain: RecoveryDomainId,
        expected: StoreRevision,
        previous: &RecoveryPublicOwnerState,
        successor: RecoveryPublicOwner,
        successor_epoch: u64,
    ) -> StoreDisposition<NativeInstallationReceipt> {
        self.commit_installation(Operation::Transfer {
            domain,
            expected,
            previous: previous.clone(),
            successor,
            successor_epoch,
        })
    }
    #[cfg(test)]
    pub(crate) fn inject_fault(&self, fault: StoreFault) {
        let _ = self.request(Operation::Fault(fault));
    }
}

pub(crate) struct OwnedTransition {
    store: RunRecoveryStore,
    id: u64,
    settled: bool,
}
impl OwnedTransition {
    pub(crate) fn complete_blocking(self) -> Result<(), StoreError> {
        self.finish_blocking(SettlementOutcome::Completed)
    }
    fn finish_blocking(mut self, outcome: SettlementOutcome) -> Result<(), StoreError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.store
            .handle
            .sender
            .send(Command::Settle {
                id: self.id,
                outcome,
                reply: Reply::Blocking(reply),
            })
            .map_err(|_| self.store.stopped())?;
        let outcome = result.recv().map_err(|_| self.store.stopped())?;
        if outcome.is_ok() {
            self.settled = true;
        }
        outcome
    }
    async fn complete(self) -> Result<(), StoreError> {
        self.finish(SettlementOutcome::Completed).await
    }
    async fn refuse(self) -> Result<(), StoreError> {
        self.finish(SettlementOutcome::Refused).await
    }
    async fn finish(mut self, outcome: SettlementOutcome) -> Result<(), StoreError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.store
            .handle
            .sender
            .send(Command::Settle {
                id: self.id,
                outcome,
                reply: Reply::Async(reply),
            })
            .map_err(|_| self.store.stopped())?;
        let outcome = result.await.map_err(|_| self.store.stopped())?;
        if outcome.is_ok() {
            self.settled = true;
        }
        outcome
    }
}
impl Drop for OwnedTransition {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self
                .store
                .handle
                .sender
                .send(Command::Abandon { id: self.id });
        }
    }
}

/// Native exclusion for one actual provider transition. Completion is explicit;
/// dropping an unknown transition permanently fences this run owner.
pub struct ProviderTransition {
    ticket: Option<OwnedTransition>,
    attachment: Option<Arc<()>>,
    store_lifetime: Option<Arc<RunRecoveryStore>>,
}
impl ProviderTransition {
    pub(crate) fn retain_attachment(
        mut self,
        attachment: Arc<()>,
        store: Arc<RunRecoveryStore>,
    ) -> Self {
        self.attachment = Some(attachment);
        self.store_lifetime = Some(store);
        self
    }
    pub async fn complete(mut self) -> Result<(), StoreError> {
        self.ticket
            .take()
            .expect("owned transition")
            .complete()
            .await
    }
    pub async fn refuse(mut self) -> Result<(), StoreError> {
        self.ticket.take().expect("owned transition").refuse().await
    }
}
pub(crate) struct NativeInstallationReceipt {
    pub(crate) state: RecoveryDomainState,
    ticket: Option<OwnedTransition>,
}
impl NativeInstallationReceipt {
    pub(crate) fn installed_blocking(mut self) -> Result<(), StoreError> {
        self.ticket
            .take()
            .expect("owned installation")
            .finish_blocking(SettlementOutcome::Installed(self.state.clone()))
    }
}

impl Blockstore for RunRecoveryStore {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        self.get_block(*cid)
    }
    fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!("run blocks require an owner-prepared root transaction")
    }
}

#[cfg(test)]
mod control_cluster_tests;
