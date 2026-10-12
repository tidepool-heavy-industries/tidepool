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
    fn send(self, value: T) -> bool {
        match self {
            Self::Blocking(reply) => reply.send(value).is_ok(),
            Self::Async(reply) => reply.send(value).is_ok(),
        }
    }
}

enum Operation {
    ReadDomain { domain: RecoveryDomainId },
    ReadOwner { domain: RecoveryDomainId, owner: RecoveryPublicOwner },
    Owners { domain: RecoveryDomainId },
    HasOwners { domain: RecoveryDomainId },
    Snapshot { domain: RecoveryDomainId, owner: Option<RecoveryPublicOwner> },
    Get { cid: Cid },
    Create { domain: RecoveryDomainId, lineage: u64, ceiling: Generation, root: DomainSnapshotRoot, blocks: Vec<(Cid, Arc<[u8]>)> },
    Ceiling { domain: RecoveryDomainId, expected: StoreRevision, minimum: Generation },
    Publish { prepared: PreparedRecoveryWrite },
    Transfer { domain: RecoveryDomainId, expected: StoreRevision, previous: RecoveryPublicOwnerState, successor: RecoveryPublicOwner, successor_epoch: u64 },
    Check {  },
    #[cfg(test)]
    Fault(StoreFault),
}
enum Value {
    ReadDomain(Result<Option<RecoveryDomainState>, StoreError>),
    ReadOwner(Result<Option<RecoveryPublicOwnerState>, StoreError>),
    Owners(Result<Vec<RecoveryPublicOwnerState>, StoreError>),
    HasOwners(Result<bool, StoreError>),
    Snapshot(Result<Option<RecoverySnapshotIdentity>, StoreError>),
    Get(Result<Option<Vec<u8>>, StoreError>),
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
            Self::Create(StoreDisposition::RecoveryRequired(error))
            | Self::Publish(StoreDisposition::RecoveryRequired(error))
            | Self::Transfer(StoreDisposition::RecoveryRequired(error))
            | Self::Ceiling(StoreDisposition::RecoveryRequired(error)) => Some(error.clone()),
            _ => None,
        }
    }
    fn committed_installation(&self) -> bool {
        matches!(self, Self::Publish(StoreDisposition::Committed(_)) | Self::Transfer(StoreDisposition::Committed(_)))
    }
}
impl Operation {
    fn execute(self, storage: &NativeStorage) -> Value {
        match self {
            Self::ReadDomain { domain } => Value::ReadDomain(storage.read_domain(domain)),
            Self::ReadOwner { domain, owner } => Value::ReadOwner(storage.read_public_owner(domain, &owner)),
            Self::Owners { domain } => Value::Owners(storage.public_owner_states(domain)),
            Self::HasOwners { domain } => Value::HasOwners(storage.has_public_owners(domain)),
            Self::Snapshot { domain, owner } => Value::Snapshot(storage.read_snapshot_identity(domain, owner.as_ref())),
            Self::Get { cid } => Value::Get(storage.get(&cid).map_err(|error| store_error(storage.path(), StoreErrorKind::Storage, error))),
            Self::Create { domain, lineage, ceiling, root, blocks } => Value::Create(storage.create_domain(domain, lineage, ceiling, root, blocks)),
            Self::Ceiling { domain, expected, minimum } => Value::Ceiling(storage.reserve_ceiling(domain, expected, minimum)),
            Self::Publish { prepared } => Value::Publish(storage.publish(prepared)),
            Self::Transfer { domain, expected, previous, successor, successor_epoch } => Value::Transfer(storage.transfer_owner(domain, expected, &previous, successor, successor_epoch)),
            Self::Check {  } => Value::Check(storage.ensure_ready()),
            #[cfg(test)]
            Self::Fault(fault) => { storage.inject_fault(fault); Value::Fault }
        }
    }
}
struct Response {
    value: Value,
    installation: Option<u64>,
}
enum Command {
    Request { operation: Operation, reply: Reply<Result<Response, StoreError>> },
    Begin { reply: Reply<Result<u64, StoreError>> },
    Settle { id: u64, reply: Reply<Result<(), StoreError>> },
    Abandon { id: u64 },
    Status { reply: Reply<NativeStoreStatus> },
    Work { reply: Reply<RecoveryStoreWork> },
    WaitFence { reply: Reply<StoreError> },
    Finished { value: Value, work: RecoveryStoreWork },
    Shutdown,
}
struct WorkerRequest(Operation);

enum Phase {
    Ready,
    Working { id: u64, reply: Reply<Result<Response, StoreError>> },
    Checking { id: u64, reply: Reply<Result<u64, StoreError>> },
    Installing(u64),
    Transition(u64),
    Fenced(StoreError),
}

#[derive(Clone)]
pub struct RunRecoveryStore {
    path: Arc<PathBuf>,
    database_id: [u8; 16],
    handle: Arc<OwnerHandle>,
}
struct OwnerHandle { sender: mpsc::Sender<Command> }
impl Drop for OwnerHandle { fn drop(&mut self) { let _ = self.sender.send(Command::Shutdown); } }
impl RunRecoveryStore {
    pub fn create(path: &Path) -> Result<Self, StoreError> { Self::start(NativeStorage::create(path)?) }
    pub fn open(path: &Path) -> Result<Self, StoreError> { Self::start(NativeStorage::open(path)?) }
    fn start(storage: NativeStorage) -> Result<Self, StoreError> {
        let path = storage.path.clone();
        let database_id = storage.database_id;
        let (sender, receiver) = mpsc::channel();
        let (worker, work_queue) = mpsc::channel::<WorkerRequest>();
        let completed = sender.clone();
        std::thread::Builder::new().name("run-native-storage".into()).spawn(move || {
            for WorkerRequest(operation) in work_queue {
                let value = operation.execute(&storage);
                if completed.send(Command::Finished { value, work: storage.work() }).is_err() { break; }
            }
        }).map_err(|error| store_error(&path, StoreErrorKind::Storage, error))?;
        let owner_path = path.clone();
        std::thread::Builder::new().name("run-native-owner".into()).spawn(move || {
            let mut phase = Phase::Ready;
            let mut pending = VecDeque::new();
            let mut next = 0u64;
            let mut work = RecoveryStoreWork::default();
            let mut watchers: Vec<Reply<StoreError>> = Vec::new();
            loop {
                let command = if matches!(phase, Phase::Ready | Phase::Fenced(_)) && !pending.is_empty() {
                    pending.pop_front().expect("nonempty deferred queue")
                } else {
                    match receiver.recv() { Ok(command) => command, Err(_) => break }
                };
                match command {
                    Command::Shutdown => break,
                    Command::Status { reply } => {
                        let status = match &phase {
                            Phase::Ready => NativeStoreStatus::Ready,
                            Phase::Working { .. } | Phase::Checking { .. } | Phase::Transition(_) => NativeStoreStatus::Pending,
                            Phase::Installing(_) => NativeStoreStatus::Installing,
                            Phase::Fenced(error) => NativeStoreStatus::Fenced(error.clone()),
                        };
                        reply.send(status);
                    }
                    Command::Work { reply } => { reply.send(work); }
                    Command::WaitFence { reply } => {
                        if let Phase::Fenced(error) = &phase { reply.send(error.clone()); }
                        else { watchers.push(reply); }
                    }
                    Command::Begin { reply } => match &phase {
                        Phase::Ready => {
                            next = match next.checked_add(1) {
                                Some(next) => next,
                                None => { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native operation identity exhausted"))); continue; }
                            };
                            phase = Phase::Checking { id: next, reply };
                            if worker.send(WorkerRequest(Operation::Check {})).is_err() {
                                let old = std::mem::replace(&mut phase, Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped")));
                                if let Phase::Checking { reply, .. } = old { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped"))); }
                            }
                        }
                        Phase::Fenced(error) => { reply.send(Err(error.clone())); }
                        _ => pending.push_back(Command::Begin { reply }),
                    },
                    Command::Settle { id, reply } => {
                        match &phase {
                            Phase::Transition(current) | Phase::Installing(current) if *current == id => {
                                phase = Phase::Ready;
                                reply.send(Ok(()));
                            }
                            Phase::Fenced(error) => { reply.send(Err(error.clone())); }
                            _ => { reply.send(Err(store_error(&owner_path, StoreErrorKind::Integrity, "native settlement does not own the outstanding operation"))); }
                        }
                    }
                    Command::Abandon { id } => {
                        if matches!(&phase, Phase::Transition(current) | Phase::Installing(current) if *current == id) {
                            phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native transition abandoned before explicit settlement"));
                        }
                    }
                    Command::Request { operation, reply } => match &phase {
                        Phase::Ready => {
                            next = match next.checked_add(1) {
                                Some(next) => next,
                                None => { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native operation identity exhausted"))); continue; }
                            };
                            phase = Phase::Working { id: next, reply };
                            if worker.send(WorkerRequest(operation)).is_err() {
                                let old = std::mem::replace(&mut phase, Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped")));
                                if let Phase::Working { reply, .. } = old { reply.send(Err(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native storage worker stopped"))); }
                            }
                        }
                        Phase::Fenced(error) => { reply.send(Err(error.clone())); }
                        _ => pending.push_back(Command::Request { operation, reply }),
                    },
                    Command::Finished { value, work: observed } => {
                        work = observed;
                        let old = std::mem::replace(&mut phase, Phase::Ready);
                        if let Phase::Working { id, reply } = old {
                            if let Some(error) = value.fence() {
                                phase = Phase::Fenced(error);
                            } else if value.committed_installation() {
                                phase = Phase::Installing(id);
                            }
                            let installation = matches!(phase, Phase::Installing(_)).then_some(id);
                            if !reply.send(Ok(Response { value, installation })) && installation.is_some() {
                                phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "committed native installation acknowledgement lost"));
                            }
                        } else if let Phase::Checking { id, reply } = old {
                            match value {
                                Value::Check(Ok(())) => {
                                    phase = Phase::Transition(id);
                                    if !reply.send(Ok(id)) { phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native grant acknowledgement lost")); }
                                }
                                Value::Check(Err(error)) => { reply.send(Err(error)); }
                                _ => { phase = Phase::Fenced(store_error(&owner_path, StoreErrorKind::RecoveryRequired, "native grant check has wrong completion")); }
                            }
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
        Ok(Self { path, database_id, handle: Arc::new(OwnerHandle { sender }) })
    }
    pub fn path(&self) -> &Path { &self.path }
    pub fn database_id(&self) -> [u8; 16] { self.database_id }
    fn stopped(&self) -> StoreError { store_error(self.path(), StoreErrorKind::RecoveryRequired, "native owner stopped") }
    fn request(&self, operation: Operation) -> Result<Response, StoreError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.handle.sender.send(Command::Request { operation, reply: Reply::Blocking(reply) }).map_err(|_| self.stopped())?;
        result.recv().map_err(|_| self.stopped())?
    }
    pub async fn ensure_ready_async(&self) -> Result<(), StoreError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.handle.sender.send(Command::Request { operation: Operation::Check {}, reply: Reply::Async(reply) }).map_err(|_| self.stopped())?;
        match result.await.map_err(|_| self.stopped())?? {
            Response { value: Value::Check(result), .. } => result,
            _ => unreachable!("typed native readiness response differs from request"),
        }
    }
    pub fn status(&self) -> NativeStoreStatus {
        let (reply, result) = mpsc::sync_channel(1);
        if self.handle.sender.send(Command::Status { reply: Reply::Blocking(reply) }).is_err() { return NativeStoreStatus::Fenced(self.stopped()); }
        result.recv().unwrap_or_else(|_| NativeStoreStatus::Fenced(self.stopped()))
    }
    pub async fn status_async(&self) -> NativeStoreStatus {
        let (reply, result) = tokio::sync::oneshot::channel();
        if self.handle.sender.send(Command::Status { reply: Reply::Async(reply) }).is_err() { return NativeStoreStatus::Fenced(self.stopped()); }
        result.await.unwrap_or_else(|_| NativeStoreStatus::Fenced(self.stopped()))
    }
    pub fn is_recovery_required(&self) -> bool { matches!(self.status(), NativeStoreStatus::Fenced(_)) }
    pub async fn wait_recovery_required(&self) -> StoreError {
        let (reply, result) = tokio::sync::oneshot::channel();
        if self.handle.sender.send(Command::WaitFence { reply: Reply::Async(reply) }).is_err() { return self.stopped(); }
        result.await.unwrap_or_else(|_| self.stopped())
    }
    pub fn work(&self) -> RecoveryStoreWork {
        let (reply, result) = mpsc::sync_channel(1);
        if self.handle.sender.send(Command::Work { reply: Reply::Blocking(reply) }).is_err() { return RecoveryStoreWork::default(); }
        result.recv().unwrap_or_default()
    }
    pub fn read_snapshot_at(path: &Path, domain: RecoveryDomainId, owner: Option<&RecoveryPublicOwner>) -> Result<Option<RecoverySnapshotIdentity>, StoreError> {
        NativeStorage::read_snapshot_at(path, domain, owner)
    }
    pub async fn begin_provider_transition(&self) -> Result<ProviderTransition, StoreError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.handle.sender.send(Command::Begin { reply: Reply::Async(reply) }).map_err(|_| self.stopped())?;
        let id = result.await.map_err(|_| self.stopped())??;
        Ok(ProviderTransition { ticket: Some(OwnedTransition { store: self.clone(), id, settled: false }), attachment: None })
    }
    pub(crate) fn begin_native_transition(&self) -> Result<OwnedTransition, StoreError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.handle.sender.send(Command::Begin { reply: Reply::Blocking(reply) }).map_err(|_| self.stopped())?;
        let id = result.recv().map_err(|_| self.stopped())??;
        Ok(OwnedTransition { store: self.clone(), id, settled: false })
    }
    pub fn read_domain(&self, domain: RecoveryDomainId) -> Result<Option<RecoveryDomainState>, StoreError> {
        match self.request(Operation::ReadDomain { domain }) {
            Err(error) => Err(error),
            Ok(Response { value: Value::ReadDomain(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn read_public_owner(&self, domain: RecoveryDomainId, owner: &RecoveryPublicOwner) -> Result<Option<RecoveryPublicOwnerState>, StoreError> {
        match self.request(Operation::ReadOwner { domain, owner: owner.clone() }) {
            Err(error) => Err(error),
            Ok(Response { value: Value::ReadOwner(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn public_owner_states(&self, domain: RecoveryDomainId) -> Result<Vec<RecoveryPublicOwnerState>, StoreError> {
        match self.request(Operation::Owners { domain }) {
            Err(error) => Err(error),
            Ok(Response { value: Value::Owners(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn has_public_owners(&self, domain: RecoveryDomainId) -> Result<bool, StoreError> {
        match self.request(Operation::HasOwners { domain }) {
            Err(error) => Err(error),
            Ok(Response { value: Value::HasOwners(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn read_snapshot_identity(&self, domain: RecoveryDomainId, owner: Option<&RecoveryPublicOwner>) -> Result<Option<RecoverySnapshotIdentity>, StoreError> {
        match self.request(Operation::Snapshot { domain, owner: owner.cloned() }) {
            Err(error) => Err(error),
            Ok(Response { value: Value::Snapshot(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn get_block(&self, cid: Cid) -> Result<Option<Vec<u8>>, StoreError> {
        match self.request(Operation::Get { cid }) {
            Err(error) => Err(error),
            Ok(Response { value: Value::Get(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn create_domain(&self, domain: RecoveryDomainId, lineage: u64, ceiling: Generation, root: DomainSnapshotRoot, blocks: Vec<(Cid, Arc<[u8]>)>) -> StoreDisposition<RecoveryDomainState> {
        match self.request(Operation::Create { domain, lineage, ceiling, root, blocks }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response { value: Value::Create(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn reserve_ceiling(&self, domain: RecoveryDomainId, expected: StoreRevision, minimum: Generation) -> StoreDisposition<CeilingCommit> {
        match self.request(Operation::Ceiling { domain, expected, minimum }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response { value: Value::Ceiling(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn publish(&self, prepared: PreparedRecoveryWrite) -> StoreDisposition<RecoveryDomainState> {
        match self.request(Operation::Publish { prepared }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response { value: Value::Publish(value), installation }) => {
                if let Some(id) = installation { if let Err(error) = (OwnedTransition { store: self.clone(), id, settled: false }).complete_blocking() { return StoreDisposition::RecoveryRequired(error); } }
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn transfer_owner(&self, domain: RecoveryDomainId, expected: StoreRevision, previous: &RecoveryPublicOwnerState, successor: RecoveryPublicOwner, successor_epoch: u64) -> StoreDisposition<RecoveryDomainState> {
        match self.request(Operation::Transfer { domain, expected, previous: previous.clone(), successor, successor_epoch }) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response { value: Value::Transfer(value), installation }) => {
                if let Some(id) = installation { if let Err(error) = (OwnedTransition { store: self.clone(), id, settled: false }).complete_blocking() { return StoreDisposition::RecoveryRequired(error); } }
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub fn ensure_ready(&self) -> Result<(), StoreError> {
        match self.request(Operation::Check {  }) {
            Err(error) => Err(error),
            Ok(Response { value: Value::Check(value), installation }) => {
                debug_assert!(installation.is_none());
                value
            }
            Ok(_) => unreachable!("typed native response differs from request"),
        }
    }
    pub(crate) fn commit_publication(&self, prepared: PreparedRecoveryWrite) -> StoreDisposition<NativeInstallationReceipt> {
        self.commit_installation(Operation::Publish { prepared })
    }
    fn commit_installation(&self, operation: Operation) -> StoreDisposition<NativeInstallationReceipt> {
        match self.request(operation) {
            Err(error) => StoreDisposition::RecoveryRequired(error),
            Ok(Response { value: Value::Publish(value) | Value::Transfer(value), installation }) => match value {
                StoreDisposition::Committed(state) => StoreDisposition::Committed(NativeInstallationReceipt { state, ticket: Some(OwnedTransition { store: self.clone(), id: installation.expect("committed installation owns receipt"), settled: false }) }),
                StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
                StoreDisposition::RecoveryRequired(error) => StoreDisposition::RecoveryRequired(error),
            },
            Ok(_) => unreachable!("typed native installation response differs from request"),
        }
    }
    pub(crate) fn publish_and_install<T>(&self, prepared: PreparedRecoveryWrite, install: impl FnOnce(RecoveryDomainState) -> T) -> StoreDisposition<T> {
        match self.commit_publication(prepared) {
            StoreDisposition::Committed(receipt) => {
                let value = install(receipt.state.clone());
                match receipt.installed_blocking() { Ok(()) => StoreDisposition::Committed(value), Err(error) => StoreDisposition::RecoveryRequired(error) }
            }
            StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
            StoreDisposition::RecoveryRequired(error) => StoreDisposition::RecoveryRequired(error),
        }
    }
    pub(crate) fn transfer_owner_and_install<T>(&self, domain: RecoveryDomainId, expected: StoreRevision, previous: &RecoveryPublicOwnerState, successor: RecoveryPublicOwner, successor_epoch: u64, install: impl FnOnce(RecoveryDomainState) -> T) -> StoreDisposition<T> {
        match self.commit_installation(Operation::Transfer { domain, expected, previous: previous.clone(), successor, successor_epoch }) {
            StoreDisposition::Committed(receipt) => {
                let value = install(receipt.state.clone());
                match receipt.installed_blocking() { Ok(()) => StoreDisposition::Committed(value), Err(error) => StoreDisposition::RecoveryRequired(error) }
            }
            StoreDisposition::Refused(error) => StoreDisposition::Refused(error),
            StoreDisposition::RecoveryRequired(error) => StoreDisposition::RecoveryRequired(error),
        }
    }
    #[cfg(test)]
    pub(crate) fn inject_fault(&self, fault: StoreFault) { let _ = self.request(Operation::Fault(fault)); }
}

pub(crate) struct OwnedTransition { store: RunRecoveryStore, id: u64, settled: bool }
impl OwnedTransition {
    pub(crate) fn complete_blocking(mut self) -> Result<(), StoreError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.store.handle.sender.send(Command::Settle { id: self.id, reply: Reply::Blocking(reply) }).map_err(|_| self.store.stopped())?;
        let outcome = result.recv().map_err(|_| self.store.stopped())?;
        if outcome.is_ok() { self.settled = true; }
        outcome
    }
    async fn complete(mut self) -> Result<(), StoreError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.store.handle.sender.send(Command::Settle { id: self.id, reply: Reply::Async(reply) }).map_err(|_| self.store.stopped())?;
        let outcome = result.await.map_err(|_| self.store.stopped())?;
        if outcome.is_ok() { self.settled = true; }
        outcome
    }
}
impl Drop for OwnedTransition {
    fn drop(&mut self) { if !self.settled { let _ = self.store.handle.sender.send(Command::Abandon { id: self.id }); } }
}

/// Native exclusion for one actual provider transition. Completion is explicit;
/// dropping an unknown transition permanently fences this run owner.
pub struct ProviderTransition { ticket: Option<OwnedTransition>, attachment: Option<Arc<()>> }
impl ProviderTransition {
    pub(crate) fn retain_attachment(mut self, attachment: Arc<()>) -> Self { self.attachment = Some(attachment); self }
    pub async fn complete(mut self) -> Result<(), StoreError> { self.ticket.take().expect("owned transition").complete().await }
    pub async fn refuse(mut self) -> Result<(), StoreError> { self.ticket.take().expect("owned transition").complete().await }
}
pub(crate) struct NativeInstallationReceipt {
    pub(crate) state: RecoveryDomainState,
    ticket: Option<OwnedTransition>,
}
impl NativeInstallationReceipt {
    pub(crate) fn installed_blocking(mut self) -> Result<(), StoreError> { self.ticket.take().expect("owned installation").complete_blocking() }
}

impl Blockstore for RunRecoveryStore {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> { Ok(self.get_block(*cid)?) }
    fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> { anyhow::bail!("run blocks require an owner-prepared root transaction") }
}
