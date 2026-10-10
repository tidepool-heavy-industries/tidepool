//! The run's SQLite owner. Immutable facts live in authenticated IPLD blocks;
//! SQL owns only their roots, public ownership, and allocation ceilings.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use parking_lot::{Mutex, MutexGuard};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use tidepool_repr::{Generation, SessionId};

use super::RecoveryPublicOwner;

const FORMAT: u64 = 1;
const ENCODING: &str = "tidepool-run-v1/hamt-v3/5/0/3/sha256/cidv1/dag-cbor/blake2b256";

/// The root machine survives host reincarnation. Independent machines retain
/// the identity issued by the existing session registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RecoveryDomainId {
    Root,
    Session(SessionId),
}
impl RecoveryDomainId {
    fn bytes(self) -> [u8; 9] {
        let mut bytes = [0; 9];
        if let Self::Session(session) = self {
            bytes[0] = 1;
            bytes[1..].copy_from_slice(&session.0.to_be_bytes());
        }
        bytes
    }
}
impl Serialize for RecoveryDomainId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Root => (0u8, 0u64).serialize(serializer),
            Self::Session(session) => (1u8, session.0).serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for RecoveryDomainId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match <(u8, u64)>::deserialize(deserializer)? {
            (0, 0) => Ok(Self::Root),
            (1, value) => Ok(Self::Session(SessionId(value))),
            _ => Err(serde::de::Error::custom("invalid recovery domain identity")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoreRevision(pub u64);

macro_rules! snapshot_root {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Cid);
        impl $name {
            pub(crate) fn new(cid: Cid) -> Self {
                Self(cid)
            }
            pub fn cid(&self) -> &Cid {
                &self.0
            }
        }
    };
}
snapshot_root!(DomainSnapshotRoot);
snapshot_root!(PublicSurfaceRoot);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryDomainState {
    pub lineage: u64,
    pub revision: StoreRevision,
    pub content_revision: StoreRevision,
    #[serde(with = "generation_serde")]
    pub allocation_ceiling: Generation,
    pub snapshot_root: DomainSnapshotRoot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPublicOwnerState {
    pub owner: RecoveryPublicOwner,
    pub epoch: u64,
    pub surface_root: PublicSurfaceRoot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySnapshotIdentity {
    pub database_id: [u8; 16],
    pub domain_id: RecoveryDomainId,
    pub domain: RecoveryDomainState,
    pub public_owner: Option<RecoveryPublicOwnerState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreError {
    pub path: PathBuf,
    pub kind: StoreErrorKind,
    pub detail: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreErrorKind {
    MissingDatabase,
    UnsupportedFormat { found: u64 },
    EncodingContract,
    Storage,
    Integrity,
    RecoveryRequired,
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.detail)
    }
}
impl std::error::Error for StoreError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreRefusal {
    DomainMissing,
    DomainExists,
    StaleRevision,
    StaleEpoch,
    OwnerMissing,
    OwnerExists,
    CeilingDecrease,
    RevisionExhausted,
    Storage(StoreError),
}
impl std::fmt::Display for StoreRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for StoreRefusal {}

#[must_use]
#[derive(Debug)]
pub enum StoreDisposition<T> {
    Committed(T),
    Refused(StoreRefusal),
    RecoveryRequired(StoreError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CeilingCommit {
    pub revision: StoreRevision,
    pub content_revision:StoreRevision,
    pub allocation_ceiling: Generation,
}

/// An owner-produced transaction contains already encoded blocks and roots.
/// Compiler work and resident activation preparation precede this value.
pub(crate) struct PreparedRecoveryWrite {
    pub domain_id: RecoveryDomainId,
    pub expected_revision: StoreRevision,
    pub snapshot_root: DomainSnapshotRoot,
    pub allocation_ceiling: Generation,
    pub blocks: Vec<(Cid, Arc<[u8]>)>,
    pub public_changes: Vec<PreparedPublicOwnerChange>,
}
#[derive(Clone)]
pub(crate) enum PreparedPublicOwnerChange {
    Insert(RecoveryPublicOwnerState),
    Replace {
        expected_epoch: u64,
        next: RecoveryPublicOwnerState,
    },
    Remove {
        owner: RecoveryPublicOwner,
        expected_epoch: u64,
    },
}

struct StoreConnection {
    connection: Connection,
    fence: Option<StoreError>,
    #[cfg(test)]
    fault: Option<StoreFault>,
}
/// All machines in a run share this owner. An old owner never loses its fence;
/// authenticated reopen constructs a new owner after retiring its consumers.
#[derive(Clone)]
pub struct RunRecoveryStore {
    path: Arc<PathBuf>,
    database_id: [u8; 16],
    connection: Arc<Mutex<StoreConnection>>,
    transactions: Arc<AtomicU64>,
    block_writes: Arc<AtomicU64>,
    block_bytes: Arc<AtomicU64>,
}
pub struct RunStoreAdmissionGuard<'a> {
    _connection: MutexGuard<'a, StoreConnection>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryStoreWork {
    pub transactions: u64,
    pub block_writes: u64,
    pub block_bytes: u64,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreFault {
    BeforeCommit,
    CommitError,
    RollbackError,
}

impl RunRecoveryStore {
    /// New-file creation is explicit. An existing legacy or incompatible store
    /// is never changed into an empty recovery domain.
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
        drop(file);
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
        writer_configuration(path, &connection)?;
        let identity = *uuid::Uuid::new_v4().as_bytes();
        match transact(
            &mut connection,
            |transaction| {
                transaction.execute_batch(SCHEMA).map_err(sql_refusal)?;
                transaction.execute("INSERT INTO store_meta(singleton,format,encoding,identity) VALUES(1,?1,?2,?3)",
                params![FORMAT as i64, ENCODING, identity.as_slice()]).map_err(sql_refusal)?;
                Ok(())
            },
            #[cfg(test)]
            None,
        ) {
            TransactionResult::Committed(()) => {}
            TransactionResult::Refused(StoreRefusal::Storage(error)) => {
                return Err(StoreError {
                    path: path.to_path_buf(),
                    ..error
                })
            }
            TransactionResult::Refused(refusal) => {
                return Err(store_error(
                    path,
                    StoreErrorKind::Storage,
                    format!("initialization refused: {refusal:?}"),
                ))
            }
            TransactionResult::RecoveryRequired(error) => {
                return Err(store_error(path, StoreErrorKind::RecoveryRequired, error))
            }
        }
        Ok(Self::from_connection(path, identity, connection))
    }

    /// Validate the contract using a read-only connection before any writer
    /// PRAGMA. Only the run's authenticated attachment owner calls this.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let read = read_connection(path)?;
        let identity = read_contract(path, &read)?;
        drop(read);
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
        if read_contract(path, &connection)? != identity {
            return Err(store_error(
                path,
                StoreErrorKind::Integrity,
                "database changed while opening",
            ));
        }
        writer_configuration(path, &connection)?;
        Ok(Self::from_connection(path, identity, connection))
    }
    fn from_connection(path: &Path, database_id: [u8; 16], connection: Connection) -> Self {
        Self {
            path: Arc::new(path.to_path_buf()),
            database_id,
            connection: Arc::new(Mutex::new(StoreConnection {
                connection,
                fence: None,
                #[cfg(test)]
                fault: None,
            })),
            transactions: Arc::new(AtomicU64::new(0)),
            block_writes: Arc::new(AtomicU64::new(0)),
            block_bytes: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn database_id(&self) -> [u8; 16] {
        self.database_id
    }
    pub fn is_recovery_required(&self) -> bool {
        self.connection.lock().fence.is_some()
    }
    pub fn ensure_ready(&self) -> Result<(), StoreError> {
        self.lease_admission().map(drop)
    }
    pub fn lease_admission(&self) -> Result<RunStoreAdmissionGuard<'_>, StoreError> {
        let connection = self.connection.lock();
        if let Some(error) = &connection.fence {
            return Err(error.clone());
        }
        Ok(RunStoreAdmissionGuard {
            _connection: connection,
        })
    }
    pub fn work(&self) -> RecoveryStoreWork {
        RecoveryStoreWork {
            transactions: self.transactions.load(Ordering::Relaxed),
            block_writes: self.block_writes.load(Ordering::Relaxed),
            block_bytes: self.block_bytes.load(Ordering::Relaxed),
        }
    }
    #[cfg(test)]
    pub(crate) fn inject_fault(&self, fault: StoreFault) {
        self.connection.lock().fault = Some(fault);
    }

    pub fn read_domain(
        &self,
        domain: RecoveryDomainId,
    ) -> Result<Option<RecoveryDomainState>, StoreError> {
        let owner = self.connection.lock();
        if let Some(error) = &owner.fence {
            return Err(error.clone());
        }
        read_domain(&owner.connection, domain)
            .map_err(|error| store_error(self.path(), StoreErrorKind::Storage, error))
    }
    pub fn read_public_owner(
        &self,
        domain: RecoveryDomainId,
        owner: &RecoveryPublicOwner,
    ) -> Result<Option<RecoveryPublicOwnerState>, StoreError> {
        let connection = self.connection.lock();
        if let Some(error) = &connection.fence {
            return Err(error.clone());
        }
        read_public_owner(&connection.connection, domain, owner)
            .map_err(|error| store_error(self.path(), StoreErrorKind::Storage, error))
    }
    pub(crate) fn public_owner_states(&self, domain:RecoveryDomainId)->Result<Vec<RecoveryPublicOwnerState>,StoreError> {
        let connection=self.connection.lock();
        if let Some(error)=&connection.fence {return Err(error.clone())}
        let mut statement=connection.connection.prepare("SELECT owner,epoch,surface_root FROM public_owners WHERE domain=?1 ORDER BY owner")
            .map_err(|error|store_error(self.path(),StoreErrorKind::Storage,error))?;
        let rows=statement.query_map([domain.bytes().as_slice()],|row| {
            let bytes:Vec<u8>=row.get(0)?;
            let owner=fvm_ipld_encoding::from_slice(&bytes).map_err(|e|rusqlite::Error::FromSqlConversionFailure(0,rusqlite::types::Type::Blob,Box::new(e)))?;
            Ok(RecoveryPublicOwnerState{owner,epoch:read_u64(row,1)?,surface_root:PublicSurfaceRoot(read_cid(row,2)?)})
        }).map_err(|error|store_error(self.path(),StoreErrorKind::Storage,error))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|error|store_error(self.path(),StoreErrorKind::Storage,error))
    }
    pub fn has_public_owners(&self, domain: RecoveryDomainId) -> Result<bool, StoreError> {
        let connection = self.connection.lock();
        if let Some(error) = &connection.fence {
            return Err(error.clone());
        }
        connection
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM public_owners WHERE domain=?1)",
                [domain.bytes().as_slice()],
                |row| row.get(0),
            )
            .map_err(|error| store_error(self.path(), StoreErrorKind::Storage, error))
    }
    pub fn read_snapshot_identity(
        &self,
        domain: RecoveryDomainId,
        owner: Option<&RecoveryPublicOwner>,
    ) -> Result<Option<RecoverySnapshotIdentity>, StoreError> {
        let connection = self.connection.lock();
        if let Some(error) = &connection.fence {
            return Err(error.clone());
        }
        read_snapshot(&connection.connection, self.database_id, domain, owner)
            .map_err(|error| store_error(self.path(), StoreErrorKind::Storage, error))
    }
    pub fn read_snapshot_at(
        path: &Path,
        domain: RecoveryDomainId,
        owner: Option<&RecoveryPublicOwner>,
    ) -> Result<Option<RecoverySnapshotIdentity>, StoreError> {
        let mut connection = read_connection(path)?;
        let identity = read_contract(path, &connection)?;
        // Both metadata queries share one bounded read snapshot.
        let transaction = connection
            .transaction()
            .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
        let snapshot = read_snapshot(&transaction, identity, domain, owner)
            .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
        transaction
            .rollback()
            .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
        Ok(snapshot)
    }

    pub(crate) fn create_domain(
        &self,
        domain: RecoveryDomainId,
        lineage: u64,
        ceiling: Generation,
        root: DomainSnapshotRoot,
        blocks: Vec<(Cid, Arc<[u8]>)>,
    ) -> StoreDisposition<RecoveryDomainState> {
        self.transact(|transaction| {
            if read_domain(transaction, domain).map_err(sql_refusal)?.is_some() { return Err(StoreRefusal::DomainExists); }
            insert_blocks(transaction, &blocks).map_err(sql_refusal)?;
            transaction.execute("INSERT INTO recovery_domains(domain,lineage,revision,content_revision,ceiling,snapshot_root) VALUES(?1,?2,?3,?3,?4,?5)",
                params![domain.bytes().as_slice(), lineage.to_be_bytes().as_slice(), 0u64.to_be_bytes().as_slice(), ceiling.0.to_be_bytes().as_slice(), root.cid().to_bytes()]).map_err(sql_refusal)?;
            Ok(RecoveryDomainState { lineage, revision: StoreRevision(0), content_revision: StoreRevision(0), allocation_ceiling: ceiling, snapshot_root: root })
        }, &blocks)
    }

    pub fn reserve_ceiling(
        &self,
        domain: RecoveryDomainId,
        expected: StoreRevision,
        minimum: Generation,
    ) -> StoreDisposition<CeilingCommit> {
        self.transact(|transaction| {
            let current = read_domain(transaction, domain).map_err(sql_refusal)?.ok_or(StoreRefusal::DomainMissing)?;
            if current.revision != expected { return Err(StoreRefusal::StaleRevision); }
            if minimum < current.allocation_ceiling { return Err(StoreRefusal::CeilingDecrease); }
            if minimum == current.allocation_ceiling {
                return Ok(CeilingCommit { revision: expected, allocation_ceiling: minimum });
            }
            let revision = increment(expected)?;
            let content = increment(current.content_revision)?;
            let updated = transaction.execute("UPDATE recovery_domains SET revision=?1,content_revision=?2,ceiling=?3 WHERE domain=?4 AND revision=?5 AND ceiling<=?3", params![
                revision.0.to_be_bytes().as_slice(), content.0.to_be_bytes().as_slice(), minimum.0.to_be_bytes().as_slice(), domain.bytes().as_slice(), expected.0.to_be_bytes().as_slice()]).map_err(sql_refusal)?;
            if updated != 1 { return Err(StoreRefusal::StaleRevision); }
            Ok(CeilingCommit { revision, content_revision:content,allocation_ceiling: minimum })
        }, &[])
    }

    pub(crate) fn publish(
        &self,
        prepared: PreparedRecoveryWrite,
    ) -> StoreDisposition<RecoveryDomainState> {
        self.publish_and_install(prepared,|state|state)
    }
    pub(crate) fn publish_and_install<T>(&self,prepared:PreparedRecoveryWrite,install:impl FnOnce(RecoveryDomainState)->T)->StoreDisposition<T> {
        self.transact_and_install(|transaction| {
            let current = read_domain(transaction, prepared.domain_id).map_err(sql_refusal)?.ok_or(StoreRefusal::DomainMissing)?;
            if current.revision != prepared.expected_revision { return Err(StoreRefusal::StaleRevision); }
            let revision = increment(current.revision)?;
            let content_revision = increment(current.content_revision)?;
            if prepared.allocation_ceiling < current.allocation_ceiling { return Err(StoreRefusal::CeilingDecrease); }
            insert_blocks(transaction, &prepared.blocks).map_err(sql_refusal)?;
            for change in &prepared.public_changes { apply_owner_change(transaction, prepared.domain_id, change)?; }
            let updated = transaction.execute("UPDATE recovery_domains SET revision=?1,content_revision=?2,ceiling=?3,snapshot_root=?4 WHERE domain=?5 AND revision=?6 AND ceiling<=?3", params![
                revision.0.to_be_bytes().as_slice(), content_revision.0.to_be_bytes().as_slice(), prepared.allocation_ceiling.0.to_be_bytes().as_slice(), prepared.snapshot_root.cid().to_bytes(), prepared.domain_id.bytes().as_slice(), prepared.expected_revision.0.to_be_bytes().as_slice()]).map_err(sql_refusal)?;
            if updated != 1 { return Err(StoreRefusal::StaleRevision); }
            Ok(RecoveryDomainState { revision, content_revision, allocation_ceiling: prepared.allocation_ceiling, snapshot_root: prepared.snapshot_root.clone(), ..current })
        }, &prepared.blocks,install)
    }

    /// This structurally distinct operation retains both immutable roots and
    /// the semantic content revision. No caller-selected boolean can claim a
    /// content-changing publication is an owner transfer.
    pub(crate) fn transfer_owner(
        &self,
        domain: RecoveryDomainId,
        expected: StoreRevision,
        previous: &RecoveryPublicOwnerState,
        successor: RecoveryPublicOwner,
        successor_epoch: u64,
    ) -> StoreDisposition<RecoveryDomainState> {
        self.transfer_owner_and_install(domain,expected,previous,successor,successor_epoch,|state|state)
    }
    pub(crate) fn transfer_owner_and_install<T>(&self,domain:RecoveryDomainId,expected:StoreRevision,previous:&RecoveryPublicOwnerState,successor:RecoveryPublicOwner,successor_epoch:u64,install:impl FnOnce(RecoveryDomainState)->T)->StoreDisposition<T> {
        self.transact_and_install(
            |transaction| {
                let current = read_domain(transaction, domain)
                    .map_err(sql_refusal)?
                    .ok_or(StoreRefusal::DomainMissing)?;
                if current.revision != expected {
                    return Err(StoreRefusal::StaleRevision);
                }
                let retained = read_public_owner(transaction, domain, &previous.owner)
                    .map_err(sql_refusal)?
                    .ok_or(StoreRefusal::OwnerMissing)?;
                if &retained != previous {
                    return Err(StoreRefusal::StaleEpoch);
                }
                apply_owner_change(
                    transaction,
                    domain,
                    &PreparedPublicOwnerChange::Remove {
                        owner: previous.owner.clone(),
                        expected_epoch: previous.epoch,
                    },
                )?;
                apply_owner_change(
                    transaction,
                    domain,
                    &PreparedPublicOwnerChange::Insert(RecoveryPublicOwnerState {
                        owner: successor,
                        epoch: successor_epoch,
                        surface_root: retained.surface_root,
                    }),
                )?;
                let revision = increment(expected)?;
                let updated = transaction
                    .execute(
                        "UPDATE recovery_domains SET revision=?1 WHERE domain=?2 AND revision=?3",
                        params![
                            revision.0.to_be_bytes().as_slice(),
                            domain.bytes().as_slice(),
                            expected.0.to_be_bytes().as_slice()
                        ],
                    )
                    .map_err(sql_refusal)?;
                if updated != 1 {
                    return Err(StoreRefusal::StaleRevision);
                }
                Ok(RecoveryDomainState {
                    revision,
                    ..current
                })
            },
            &[],install,
        )
    }

    fn transact<T>(
        &self,
        action: impl FnOnce(&Transaction<'_>) -> Result<T, StoreRefusal>,
        blocks: &[(Cid, Arc<[u8]>)],
    ) -> StoreDisposition<T> {
        self.transact_and_install(action,blocks,|value|value)
    }
    fn transact_and_install<T,R>(&self,action:impl FnOnce(&Transaction<'_>)->Result<T,StoreRefusal>,blocks:&[(Cid,Arc<[u8]>)],install:impl FnOnce(T)->R)->StoreDisposition<R> {
        for (cid, bytes) in blocks {
            if let Err(error) = tidepool_toolchain::exact_context::verify_block(cid, bytes) {
                return StoreDisposition::Refused(StoreRefusal::Storage(store_error(
                    self.path(),
                    StoreErrorKind::Integrity,
                    error,
                )));
            }
        }
        let mut owner = self.connection.lock();
        if let Some(error) = &owner.fence {
            return StoreDisposition::RecoveryRequired(error.clone());
        }
        #[cfg(test)]
        let fault = owner.fault.take();
        self.transactions.fetch_add(1, Ordering::Relaxed);
        let result = transact(
            &mut owner.connection,
            action,
            #[cfg(test)]
            fault,
        );
        match result {
            TransactionResult::Committed(value) => {
                self.block_writes
                    .fetch_add(blocks.len() as u64, Ordering::Relaxed);
                self.block_bytes.fetch_add(
                    blocks.iter().map(|(_, bytes)| bytes.len() as u64).sum(),
                    Ordering::Relaxed,
                );
                StoreDisposition::Committed(install(value))
            }
            TransactionResult::Refused(refusal) => StoreDisposition::Refused(refusal),
            TransactionResult::RecoveryRequired(detail) => {
                let error = store_error(self.path(), StoreErrorKind::RecoveryRequired, detail);
                owner.fence = Some(error.clone());
                StoreDisposition::RecoveryRequired(error)
            }
        }
    }
}

impl Blockstore for RunRecoveryStore {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        let owner = self.connection.lock();
        if let Some(error) = &owner.fence {
            return Err(error.clone().into());
        }
        let bytes = owner
            .connection
            .query_row(
                "SELECT bytes FROM blocks WHERE cid=?1",
                [cid.to_bytes()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        if let Some(bytes) = &bytes {
            tidepool_toolchain::exact_context::verify_block(cid, bytes)?;
        }
        Ok(bytes)
    }
    fn put_keyed(&self, _cid: &Cid, _bytes: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!("run blocks require an owner-prepared root transaction")
    }
}

enum TransactionResult<T> {
    Committed(T),
    Refused(StoreRefusal),
    RecoveryRequired(String),
}
fn transact<T>(
    connection: &mut Connection,
    action: impl FnOnce(&Transaction<'_>) -> Result<T, StoreRefusal>,
    #[cfg(test)] fault: Option<StoreFault>,
) -> TransactionResult<T> {
    let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(transaction) => transaction,
        Err(error) => return TransactionResult::Refused(sql_refusal(error)),
    };
    let value = match action(&transaction) {
        Ok(value) => value,
        Err(refusal) => {
            return match transaction.rollback() {
                Ok(()) => TransactionResult::Refused(refusal),
                Err(error) => {
                    TransactionResult::RecoveryRequired(format!("rollback failed: {error}"))
                }
            }
        }
    };
    #[cfg(test)]
    if matches!(
        fault,
        Some(StoreFault::BeforeCommit | StoreFault::RollbackError)
    ) {
        return match transaction.rollback() {
            Ok(()) if fault == Some(StoreFault::BeforeCommit) => {
                TransactionResult::Refused(StoreRefusal::Storage(store_error(
                    Path::new(""),
                    StoreErrorKind::Storage,
                    "injected precommit refusal",
                )))
            }
            result => TransactionResult::RecoveryRequired(format!(
                "injected rollback uncertainty: {result:?}"
            )),
        };
    }
    // Commit consumes the RAII transaction. Every error fences the run even
    // if its destructor subsequently attempts rollback. A SELECT cannot prove
    // that an I/O-uncertain COMMIT was durable or that it did not occur.
    match transaction.commit() {
        Ok(()) => {
            #[cfg(test)]
            if fault == Some(StoreFault::CommitError) {
                return TransactionResult::RecoveryRequired(
                    "injected error after authentic FULL commit".into(),
                );
            }
            TransactionResult::Committed(value)
        }
        Err(error) => TransactionResult::RecoveryRequired(format!("commit failed: {error}")),
    }
}

fn increment(revision: StoreRevision) -> Result<StoreRevision, StoreRefusal> {
    revision
        .0
        .checked_add(1)
        .map(StoreRevision)
        .ok_or(StoreRefusal::RevisionExhausted)
}
fn sql_refusal(error: rusqlite::Error) -> StoreRefusal {
    StoreRefusal::Storage(store_error(Path::new(""), StoreErrorKind::Storage, error))
}
fn insert_blocks(
    transaction: &Transaction<'_>,
    blocks: &[(Cid, Arc<[u8]>)],
) -> rusqlite::Result<()> {
    let mut insert = transaction.prepare_cached(
        "INSERT INTO blocks(cid,bytes) VALUES(?1,?2) ON CONFLICT(cid) DO NOTHING",
    )?;
    let mut existing = transaction.prepare_cached("SELECT bytes FROM blocks WHERE cid=?1")?;
    for (cid, bytes) in blocks {
        let key = cid.to_bytes();
        if insert.execute(params![&key, bytes.as_ref()])? == 0 {
            let held: Vec<u8> = existing.query_row([&key], |row| row.get(0))?;
            if held.as_slice() != bytes.as_ref() {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
    }
    Ok(())
}
fn owner_bytes(owner: &RecoveryPublicOwner) -> Vec<u8> {
    fvm_ipld_encoding::to_vec(owner).expect("typed public owner is encodable")
}
fn apply_owner_change(
    transaction: &Transaction<'_>,
    domain: RecoveryDomainId,
    change: &PreparedPublicOwnerChange,
) -> Result<(), StoreRefusal> {
    let domain = domain.bytes();
    let changed = match change {
        PreparedPublicOwnerChange::Insert(next) => transaction.execute("INSERT INTO public_owners(domain,owner,epoch,surface_root) VALUES(?1,?2,?3,?4) ON CONFLICT(domain,owner) DO NOTHING", params![domain.as_slice(), owner_bytes(&next.owner), next.epoch.to_be_bytes().as_slice(), next.surface_root.cid().to_bytes()]).map_err(sql_refusal)?,
        PreparedPublicOwnerChange::Replace { expected_epoch, next } => transaction.execute("UPDATE public_owners SET epoch=?1,surface_root=?2 WHERE domain=?3 AND owner=?4 AND epoch=?5", params![next.epoch.to_be_bytes().as_slice(), next.surface_root.cid().to_bytes(), domain.as_slice(), owner_bytes(&next.owner), expected_epoch.to_be_bytes().as_slice()]).map_err(sql_refusal)?,
        PreparedPublicOwnerChange::Remove { owner, expected_epoch } => transaction.execute("DELETE FROM public_owners WHERE domain=?1 AND owner=?2 AND epoch=?3", params![domain.as_slice(), owner_bytes(owner), expected_epoch.to_be_bytes().as_slice()]).map_err(sql_refusal)?,
    };
    if changed != 1 {
        return Err(match change {
            PreparedPublicOwnerChange::Insert(_) => StoreRefusal::OwnerExists,
            _ => StoreRefusal::StaleEpoch,
        });
    }
    Ok(())
}
fn read_domain(
    connection: &Connection,
    domain: RecoveryDomainId,
) -> rusqlite::Result<Option<RecoveryDomainState>> {
    connection.query_row("SELECT lineage,revision,content_revision,ceiling,snapshot_root FROM recovery_domains WHERE domain=?1", [domain.bytes().as_slice()], |row| {
        Ok(RecoveryDomainState { lineage: read_u64(row, 0)?, revision: StoreRevision(read_u64(row, 1)?), content_revision: StoreRevision(read_u64(row, 2)?), allocation_ceiling: Generation(read_u64(row, 3)?), snapshot_root: DomainSnapshotRoot(read_cid(row, 4)?) })
    }).optional()
}
fn read_public_owner(
    connection: &Connection,
    domain: RecoveryDomainId,
    owner: &RecoveryPublicOwner,
) -> rusqlite::Result<Option<RecoveryPublicOwnerState>> {
    connection
        .query_row(
            "SELECT epoch,surface_root FROM public_owners WHERE domain=?1 AND owner=?2",
            params![domain.bytes().as_slice(), owner_bytes(owner)],
            |row| {
                Ok(RecoveryPublicOwnerState {
                    owner: owner.clone(),
                    epoch: read_u64(row, 0)?,
                    surface_root: PublicSurfaceRoot(read_cid(row, 1)?),
                })
            },
        )
        .optional()
}
fn read_snapshot(
    connection: &Connection,
    database_id: [u8; 16],
    domain_id: RecoveryDomainId,
    owner: Option<&RecoveryPublicOwner>,
) -> rusqlite::Result<Option<RecoverySnapshotIdentity>> {
    let Some(domain) = read_domain(connection, domain_id)? else {
        return Ok(None);
    };
    let public_owner = owner
        .map(|owner| read_public_owner(connection, domain_id, owner))
        .transpose()?
        .flatten();
    Ok(Some(RecoverySnapshotIdentity {
        database_id,
        domain_id,
        domain,
        public_owner,
    }))
}
fn read_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let bytes: Vec<u8> = row.get(index)?;
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(u64::from_be_bytes(bytes))
}
fn read_cid(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Cid> {
    Cid::try_from(row.get::<_, Vec<u8>>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Blob,
            Box::new(error),
        )
    })
}
fn read_connection(path: &Path) -> Result<Connection, StoreError> {
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(store_error(path, StoreErrorKind::MissingDatabase, error))
        }
        Err(error) => return Err(store_error(path, StoreErrorKind::Storage, error)),
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| store_error(path, StoreErrorKind::Storage, error))
}
fn read_contract(path: &Path, connection: &Connection) -> Result<[u8; 16], StoreError> {
    let (format, encoding, identity): (u64, String, Vec<u8>) = connection
        .query_row(
            "SELECT format,encoding,identity FROM store_meta WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
    if format != FORMAT {
        return Err(store_error(
            path,
            StoreErrorKind::UnsupportedFormat { found: format },
            "unsupported run recovery format",
        ));
    }
    if encoding != ENCODING {
        return Err(store_error(
            path,
            StoreErrorKind::EncodingContract,
            "unsupported snapshot encoding contract",
        ));
    }
    identity
        .try_into()
        .map_err(|_| store_error(path, StoreErrorKind::Integrity, "invalid database identity"))
}
fn writer_configuration(path: &Path, connection: &Connection) -> Result<(), StoreError> {
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(store_error(
            path,
            StoreErrorKind::Storage,
            "SQLite refused WAL mode",
        ));
    }
    connection
        .execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
    let sync: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
    if sync != 2 {
        return Err(store_error(
            path,
            StoreErrorKind::Storage,
            "SQLite refused FULL synchronous mode",
        ));
    }
    Ok(())
}
fn store_error(path: &Path, kind: StoreErrorKind, detail: impl std::fmt::Display) -> StoreError {
    StoreError {
        path: path.to_path_buf(),
        kind,
        detail: detail.to_string(),
    }
}

const SCHEMA: &str = r#"
CREATE TABLE store_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),format INTEGER NOT NULL,encoding TEXT NOT NULL,identity BLOB NOT NULL CHECK(typeof(identity)='blob' AND length(identity)=16)) STRICT;
CREATE TABLE blocks(cid BLOB PRIMARY KEY NOT NULL CHECK(typeof(cid)='blob'),bytes BLOB NOT NULL CHECK(typeof(bytes)='blob')) STRICT;
CREATE TABLE recovery_domains(domain BLOB PRIMARY KEY NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=9),lineage BLOB NOT NULL CHECK(typeof(lineage)='blob' AND length(lineage)=8),revision BLOB NOT NULL CHECK(typeof(revision)='blob' AND length(revision)=8),content_revision BLOB NOT NULL CHECK(typeof(content_revision)='blob' AND length(content_revision)=8),ceiling BLOB NOT NULL CHECK(typeof(ceiling)='blob' AND length(ceiling)=8),snapshot_root BLOB NOT NULL REFERENCES blocks(cid)) STRICT;
CREATE TABLE public_owners(domain BLOB NOT NULL REFERENCES recovery_domains(domain),owner BLOB NOT NULL CHECK(typeof(owner)='blob'),epoch BLOB NOT NULL CHECK(typeof(epoch)='blob' AND length(epoch)=8),surface_root BLOB NOT NULL REFERENCES blocks(cid),PRIMARY KEY(domain,owner)) STRICT;
"#;

mod generation_serde {
    use serde::{Deserialize, Serializer};
    use tidepool_repr::Generation;
    pub fn serialize<S: Serializer>(value: &Generation, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.0)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Generation, D::Error> {
        u64::deserialize(deserializer).map(Generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fvm_ipld_blockstore::MemoryBlockstore;
    use proptest::prelude::*;
    use tidepool_toolchain::exact_context::{read_value, Materializer};

    fn blocks<T: Serialize>(value: &T) -> (Cid, Vec<(Cid, Arc<[u8]>)>) {
        let base = MemoryBlockstore::default();
        let mut materializer = Materializer::new(&base);
        let cid = materializer.materialize_value(value).unwrap();
        (cid, materializer.finish().unwrap().into_blocks().collect())
    }
    fn store() -> (tempfile::TempDir, RunRecoveryStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = RunRecoveryStore::create(&directory.path().join("recovery.sqlite")).unwrap();
        (directory, store)
    }
    fn domain(
        store: &RunRecoveryStore,
        domain: RecoveryDomainId,
        ceiling: u64,
    ) -> RecoveryDomainState {
        let (root, blocks) = blocks(&("domain", domain, ceiling));
        match store.create_domain(
            domain,
            17,
            Generation(ceiling),
            DomainSnapshotRoot::new(root),
            blocks,
        ) {
            StoreDisposition::Committed(state) => state,
            other => panic!("domain creation failed: {other:?}"),
        }
    }
    fn owner(path: &str, incarnation: u64) -> RecoveryPublicOwner {
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse(path).unwrap(), incarnation).unwrap()
    }
    fn public_change(
        store: &RunRecoveryStore,
        domain: RecoveryDomainId,
        state: &RecoveryDomainState,
        owner: RecoveryPublicOwner,
        expected_epoch: Option<u64>,
        content: u64,
    ) -> StoreDisposition<RecoveryDomainState> {
        let (cid, blocks) = blocks(&("surface", content));
        let next = RecoveryPublicOwnerState {
            owner,
            epoch: expected_epoch.map_or(1, |epoch| epoch + 1),
            surface_root: PublicSurfaceRoot::new(cid),
        };
        store.publish(PreparedRecoveryWrite {
            domain_id: domain,
            expected_revision: state.revision,
            snapshot_root: state.snapshot_root.clone(),
            allocation_ceiling: state.allocation_ceiling,
            blocks,
            public_changes: vec![match expected_epoch {
                None => PreparedPublicOwnerChange::Insert(next),
                Some(expected_epoch) => PreparedPublicOwnerChange::Replace {
                    expected_epoch,
                    next,
                },
            }],
        })
    }

    #[test]
    fn real_sqlite_reopen_preserves_full_u64_and_authenticated_blocks() {
        let (_directory, store) = store();
        let root = domain(&store, RecoveryDomainId::Root, u64::MAX - 1);
        let original: (String, RecoveryDomainId, u64) =
            read_value(&store, root.snapshot_root.cid()).unwrap();
        assert_eq!(
            original,
            ("domain".into(), RecoveryDomainId::Root, u64::MAX - 1)
        );
        let committed =
            store.reserve_ceiling(RecoveryDomainId::Root, root.revision, Generation(u64::MAX));
        assert!(matches!(
            committed,
            StoreDisposition::Committed(CeilingCommit {
                allocation_ceiling: Generation(u64::MAX),
                ..
            })
        ));
        let reopened = RunRecoveryStore::open(store.path()).unwrap();
        let observed = reopened
            .read_domain(RecoveryDomainId::Root)
            .unwrap()
            .unwrap();
        assert_eq!(observed.allocation_ceiling, Generation(u64::MAX));
        assert_eq!(observed.revision, StoreRevision(1));
        assert_eq!(observed.content_revision, StoreRevision(1));
        assert_eq!(observed.snapshot_root, root.snapshot_root);
        assert_eq!(store.database_id(), reopened.database_id());
        let linked: (String, String) = reopened
            .connection
            .lock()
            .connection
            .query_row("SELECT sqlite_version(),sqlite_source_id()", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(linked.0, "3.53.2");
        eprintln!("linked SQLite {} {}", linked.0, linked.1);
    }

    #[test]
    fn independent_domains_and_stale_publication_do_not_erase_siblings() {
        let (_directory, store) = store();
        let root = domain(&store, RecoveryDomainId::Root, 64);
        let child_id = RecoveryDomainId::Session(SessionId(u64::MAX));
        let child = domain(&store, child_id, 512);
        let first = owner("root/a", 1);
        let second = owner("root/b", 1);
        let root = match public_change(
            &store,
            RecoveryDomainId::Root,
            &root,
            first.clone(),
            None,
            1,
        ) {
            StoreDisposition::Committed(state) => state,
            other => panic!("first publication: {other:?}"),
        };
        let stale = root.clone();
        let root = match public_change(
            &store,
            RecoveryDomainId::Root,
            &root,
            second.clone(),
            None,
            2,
        ) {
            StoreDisposition::Committed(state) => state,
            other => panic!("sibling publication: {other:?}"),
        };
        assert!(matches!(
            public_change(
                &store,
                RecoveryDomainId::Root,
                &stale,
                first.clone(),
                Some(1),
                3
            ),
            StoreDisposition::Refused(StoreRefusal::StaleRevision)
        ));
        assert_eq!(store.read_domain(child_id).unwrap().unwrap(), child);
        assert_eq!(
            store.read_domain(RecoveryDomainId::Root).unwrap().unwrap(),
            root
        );
        assert_eq!(
            store
                .read_public_owner(RecoveryDomainId::Root, &first)
                .unwrap()
                .unwrap()
                .epoch,
            1
        );
        assert_eq!(
            store
                .read_public_owner(RecoveryDomainId::Root, &second)
                .unwrap()
                .unwrap()
                .epoch,
            1
        );
        assert!(store.read_public_owner(child_id, &first).unwrap().is_none());
        assert!(matches!(
            store.create_domain(child_id, 17, Generation(512), child.snapshot_root, vec![]),
            StoreDisposition::Refused(StoreRefusal::DomainExists)
        ));
    }

    #[test]
    fn owner_transfer_retains_content_identity_and_ceiling() {
        let (_directory, store) = store();
        let domain = domain(&store, RecoveryDomainId::Root, 64);
        let previous = owner("root", 1);
        let successor = owner("root", 2);
        let domain = match public_change(
            &store,
            RecoveryDomainId::Root,
            &domain,
            previous.clone(),
            None,
            9,
        ) {
            StoreDisposition::Committed(state) => state,
            other => panic!("publication: {other:?}"),
        };
        let previous = store
            .read_public_owner(RecoveryDomainId::Root, &previous)
            .unwrap()
            .unwrap();
        let next = match store.transfer_owner(
            RecoveryDomainId::Root,
            domain.revision,
            &previous,
            successor.clone(),
            2,
        ) {
            StoreDisposition::Committed(state) => state,
            other => panic!("transfer: {other:?}"),
        };
        assert_eq!(next.content_revision, domain.content_revision);
        assert_eq!(next.snapshot_root, domain.snapshot_root);
        assert_eq!(next.allocation_ceiling, domain.allocation_ceiling);
        assert_eq!(next.revision, StoreRevision(domain.revision.0 + 1));
        let current = store
            .read_public_owner(RecoveryDomainId::Root, &successor)
            .unwrap()
            .unwrap();
        assert_eq!(current.surface_root, previous.surface_root);
        assert!(store
            .read_public_owner(RecoveryDomainId::Root, &previous.owner)
            .unwrap()
            .is_none());
    }

    #[test]
    fn authentic_faults_reopen_old_or_new_and_fence_every_domain() {
        for fault in [
            StoreFault::BeforeCommit,
            StoreFault::CommitError,
            StoreFault::RollbackError,
        ] {
            let (_directory, store) = store();
            let first = domain(&store, RecoveryDomainId::Root, 64);
            let sibling_id = RecoveryDomainId::Session(SessionId(8));
            let sibling = domain(&store, sibling_id, 9);
            store.inject_fault(fault);
            let outcome =
                store.reserve_ceiling(RecoveryDomainId::Root, first.revision, Generation(128));
            if fault == StoreFault::BeforeCommit {
                assert!(matches!(outcome, StoreDisposition::Refused(_)));
                assert!(!store.is_recovery_required());
                assert!(store.lease_admission().is_ok());
            } else {
                assert!(matches!(outcome, StoreDisposition::RecoveryRequired(_)));
                assert!(store.is_recovery_required());
                assert!(store.lease_admission().is_err());
                assert!(matches!(
                    store.reserve_ceiling(sibling_id, sibling.revision, Generation(10)),
                    StoreDisposition::RecoveryRequired(_)
                ));
                assert!(store.get(first.snapshot_root.cid()).is_err());
            }
            let reopened = RunRecoveryStore::open(store.path()).unwrap();
            assert!(!reopened.is_recovery_required());
            assert_eq!(reopened.read_domain(sibling_id).unwrap().unwrap(), sibling);
            let observed = reopened
                .read_domain(RecoveryDomainId::Root)
                .unwrap()
                .unwrap();
            if fault == StoreFault::CommitError {
                assert_eq!(observed.allocation_ceiling, Generation(128));
                assert_eq!(observed.revision, StoreRevision(1));
                assert_eq!(observed.content_revision, StoreRevision(1));
            } else {
                assert_eq!(observed, first);
            }
            assert_eq!(
                store.is_recovery_required(),
                fault != StoreFault::BeforeCommit
            );
        }
    }

    #[test]
    fn keyed_reads_refuse_corruption_and_contract_refusal_does_not_change_journal() {
        let (_directory, store) = store();
        let domain = domain(&store, RecoveryDomainId::Root, 0);
        store
            .connection
            .lock()
            .connection
            .execute(
                "UPDATE blocks SET bytes=?1 WHERE cid=?2",
                params![vec![1u8, 2, 3], domain.snapshot_root.cid().to_bytes()],
            )
            .unwrap();
        assert!(store.get(domain.snapshot_root.cid()).is_err());
        let path = store.path().to_path_buf();
        drop(store);
        let connection = Connection::open(&path).unwrap();
        connection
            .query_row::<String, _, _>("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        connection
            .execute("UPDATE store_meta SET format=100", [])
            .unwrap();
        drop(connection);
        assert!(matches!(
            RunRecoveryStore::open(&path),
            Err(StoreError {
                kind: StoreErrorKind::UnsupportedFormat { found: 100 },
                ..
            })
        ));
        let read = read_connection(&path).unwrap();
        let mode: String = read
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
    }

    #[test]
    fn strict_blob_constraints_refuse_text_and_missing_read_is_noncreating() {
        let (directory, store) = store();
        let domain = domain(&store, RecoveryDomainId::Root, 0);
        let owner = store.connection.lock();
        assert!(owner
            .connection
            .execute(
                "UPDATE recovery_domains SET ceiling='00000000' WHERE domain=?1",
                [RecoveryDomainId::Root.bytes().as_slice()]
            )
            .is_err());
        drop(owner);
        assert_eq!(
            store.read_domain(RecoveryDomainId::Root).unwrap().unwrap(),
            domain
        );
        let missing = directory.path().join("missing.sqlite");
        assert!(matches!(
            RunRecoveryStore::read_snapshot_at(&missing, RecoveryDomainId::Root, None),
            Err(StoreError {
                kind: StoreErrorKind::MissingDatabase,
                ..
            })
        ));
        assert!(!missing.exists());
    }

    proptest! {
        #[test]
        fn domain_histories_match_independent_scalar_model(operations in prop::collection::vec((0u8..4, 0u8..5, any::<u8>()), 1..80)) {
            let (_directory, initial) = store();
            let ids = [RecoveryDomainId::Root, RecoveryDomainId::Session(SessionId(1)), RecoveryDomainId::Session(SessionId(2)), RecoveryDomainId::Session(SessionId(u64::MAX))];
            let mut model = std::collections::BTreeMap::new();
            for (index, id) in ids.iter().copied().enumerate() {
                let initial_ceiling = if index == 3 { u64::MAX - 256 } else { index as u64 };
                domain(&initial, id, initial_ceiling);
                model.insert(index, (0u64, initial_ceiling));
            }
            let path = initial.path().to_path_buf();
            let mut active = initial;
            for (index, operation, amount) in operations {
                let index = index as usize;
                let (revision, ceiling) = model[&index];
                let next = ceiling.saturating_add(amount as u64);
                if operation == 0 {
                    active = RunRecoveryStore::open(&path).unwrap();
                } else {
                    let fault = match operation { 2 => Some(StoreFault::BeforeCommit), 3 => Some(StoreFault::CommitError), 4 => Some(StoreFault::RollbackError), _ => None };
                    if let Some(fault) = fault { active.inject_fault(fault); }
                    let outcome = active.reserve_ceiling(ids[index], StoreRevision(revision), Generation(next));
                    if matches!(operation, 1 | 3) {
                        model.insert(index, (revision + u64::from(next != ceiling), next));
                    }
                    match operation {
                        1 => prop_assert!(matches!(outcome, StoreDisposition::Committed(_))),
                        2 => prop_assert!(matches!(outcome, StoreDisposition::Refused(_))),
                        3 | 4 => {
                            prop_assert!(matches!(outcome, StoreDisposition::RecoveryRequired(_)));
                            for id in ids { prop_assert!(active.read_domain(id).is_err()); }
                            let old = active.clone();
                            active = RunRecoveryStore::open(&path).unwrap();
                            prop_assert!(old.is_recovery_required());
                        }
                        _ => unreachable!(),
                    }
                }
                for (index, id) in ids.iter().copied().enumerate() {
                    let (revision, ceiling) = model[&index];
                    let actual = active.read_domain(id).unwrap().unwrap();
                    prop_assert_eq!(actual.revision, StoreRevision(revision));
                    prop_assert_eq!(actual.content_revision, StoreRevision(revision));
                    prop_assert_eq!(actual.allocation_ceiling, Generation(ceiling));
                }
            }
        }
    }
}
