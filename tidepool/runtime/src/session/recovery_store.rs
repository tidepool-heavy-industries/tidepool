//! The run's SQLite owner. Immutable facts live in authenticated IPLD blocks;
//! SQL owns only their roots, public ownership, and allocation ceilings.
use std::cell::{Cell, RefCell};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cid::Cid;
use fvm_ipld_blockstore::Blockstore;

use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use tidepool_repr::{Generation, SessionId};

use super::RecoveryPublicOwner;

const FORMAT: u64 = 2;
const ENCODING: &str = "tidepool-run-v2/hamt-v3/5/0/3/sha256/cidv1/dag-cbor/blake2b256";

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
    pub content_revision: StoreRevision,
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
    pub generation_change: Option<PreparedGenerationChange>,
}
pub(crate) struct PreparedGenerationChange {
    pub expected: Option<super::ActiveNotebookGeneration>,
    pub next: super::ActiveNotebookGeneration,
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
    #[cfg(test)]
    fault: Option<StoreFault>,
}
struct NativeStorage {
    path: Arc<PathBuf>,
    database_id: [u8; 16],
    physical_identity: (u64, u64),
    connection: RefCell<StoreConnection>,
    transactions: Cell<u64>,
    block_writes: Cell<u64>,
    block_bytes: Cell<u64>,
}

mod owner;
pub(crate) use owner::{NativeInstallationReceipt, OwnedTransition};
pub use owner::{NativeStoreStatus, ProviderTransition, RunRecoveryStore};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryStoreWork {
    pub transactions: u64,
    /// Actual SQL INSERT rows, including work later rolled back or fenced.
    pub block_writes: u64,
    pub block_bytes: u64,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreFault {
    BeforeCommit,
    CommitError,
    RollbackError,
    WorkerPanic,
}

impl NativeStorage {
    /// New-file creation is explicit. An existing legacy or incompatible store
    /// is never changed into an empty recovery domain.
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| store_error(path, StoreErrorKind::Storage, error))?;
        let physical_identity = file
            .metadata()
            .map(|metadata| (metadata.dev(), metadata.ino()))
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
                });
            }
            TransactionResult::Refused(refusal) => {
                return Err(store_error(
                    path,
                    StoreErrorKind::Storage,
                    format!("initialization refused: {refusal:?}"),
                ));
            }
            TransactionResult::RecoveryRequired(error) => {
                return Err(store_error(path, StoreErrorKind::RecoveryRequired, error));
            }
        }
        Self::from_connection(path, identity, physical_identity, connection)
    }

    /// Validate the contract using a read-only connection before any writer
    /// PRAGMA. Only the run's authenticated attachment owner calls this.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let physical_identity = physical_identity(path)?;
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
        Self::from_connection(path, identity, physical_identity, connection)
    }

    /// Inspect an existing store without creating it or applying writer PRAGMAs.
    pub(crate) fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        let physical_identity = physical_identity(path)?;
        let connection = read_connection(path)?;
        let identity = read_contract(path, &connection)?;
        Self::from_connection(path, identity, physical_identity, connection)
    }
    fn from_connection(
        path: &Path,
        database_id: [u8; 16],
        physical_identity: (u64, u64),
        connection: Connection,
    ) -> Result<Self, StoreError> {
        if self::physical_identity(path)? != physical_identity {
            return Err(store_error(
                path,
                StoreErrorKind::Integrity,
                "database pathname changed during open",
            ));
        }
        Ok(Self {
            path: Arc::new(path.to_path_buf()),
            database_id,
            physical_identity,
            connection: RefCell::new(StoreConnection {
                connection,
                #[cfg(test)]
                fault: None,
            }),
            transactions: Cell::new(0),
            block_writes: Cell::new(0),
            block_bytes: Cell::new(0),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn database_id(&self) -> [u8; 16] {
        self.database_id
    }
    fn ensure_ready(&self) -> Result<(), StoreError> {
        self.validate_physical_identity()
    }
    fn validate_physical_identity(&self) -> Result<(), StoreError> {
        if physical_identity(self.path())? != self.physical_identity {
            return Err(store_error(
                self.path(),
                StoreErrorKind::Integrity,
                "database pathname no longer names this run store",
            ));
        }
        Ok(())
    }
    pub fn work(&self) -> RecoveryStoreWork {
        RecoveryStoreWork {
            transactions: self.transactions.get(),
            block_writes: self.block_writes.get(),
            block_bytes: self.block_bytes.get(),
        }
    }
    #[cfg(test)]
    pub(crate) fn inject_fault(&self, fault: StoreFault) {
        self.connection.borrow_mut().fault = Some(fault);
    }

    pub fn read_domain(
        &self,
        domain: RecoveryDomainId,
    ) -> Result<Option<RecoveryDomainState>, StoreError> {
        let owner = self.connection.borrow();
        self.validate_physical_identity()?;
        read_domain(&owner.connection, domain).map_err(|error| sql_store_error(self.path(), error))
    }
    pub fn read_public_owner(
        &self,
        domain: RecoveryDomainId,
        owner: &RecoveryPublicOwner,
    ) -> Result<Option<RecoveryPublicOwnerState>, StoreError> {
        let connection = self.connection.borrow();
        self.validate_physical_identity()?;
        read_public_owner(&connection.connection, domain, owner)
            .map_err(|error| sql_store_error(self.path(), error))
    }
    pub(crate) fn public_owner_states(
        &self,
        domain: RecoveryDomainId,
    ) -> Result<Vec<RecoveryPublicOwnerState>, StoreError> {
        let connection = self.connection.borrow();
        self.validate_physical_identity()?;
        let mut statement = connection
            .connection
            .prepare(
                "SELECT owner,epoch,surface_root FROM public_owners WHERE domain=?1 ORDER BY owner",
            )
            .map_err(|error| store_error(self.path(), StoreErrorKind::Storage, error))?;
        let rows = statement
            .query_map([domain.bytes().as_slice()], |row| {
                let bytes: Vec<u8> = row.get(0)?;
                let owner = fvm_ipld_encoding::from_slice(&bytes).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Blob,
                        Box::new(e),
                    )
                })?;
                Ok(RecoveryPublicOwnerState {
                    owner,
                    epoch: read_u64(row, 1)?,
                    surface_root: PublicSurfaceRoot(read_cid(row, 2)?),
                })
            })
            .map_err(|error| store_error(self.path(), StoreErrorKind::Storage, error))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| sql_store_error(self.path(), error))
    }
    pub fn has_public_owners(&self, domain: RecoveryDomainId) -> Result<bool, StoreError> {
        let connection = self.connection.borrow();
        self.validate_physical_identity()?;
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
        let connection = self.connection.borrow();
        self.validate_physical_identity()?;
        read_snapshot(&connection.connection, self.database_id, domain, owner)
            .map_err(|error| sql_store_error(self.path(), error))
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
            .map_err(|error| sql_store_error(path, error))?;
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
            insert_blocks(transaction, &blocks, &self.block_writes, &self.block_bytes).map_err(sql_refusal)?;
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
                return Ok(CeilingCommit { revision: expected,content_revision:current.content_revision, allocation_ceiling: minimum });
            }
            let revision = increment(expected)?;
            let content = increment(current.content_revision)?;
            let updated = transaction.execute("UPDATE recovery_domains SET revision=?1,content_revision=?2,ceiling=?3 WHERE domain=?4 AND revision=?5 AND ceiling<=?3", params![
                revision.0.to_be_bytes().as_slice(), content.0.to_be_bytes().as_slice(), minimum.0.to_be_bytes().as_slice(), domain.bytes().as_slice(), expected.0.to_be_bytes().as_slice()]).map_err(sql_refusal)?;
            if updated != 1 { return Err(StoreRefusal::StaleRevision); }
            Ok(CeilingCommit { revision, content_revision:content,allocation_ceiling: minimum })
        }, &[])
    }

    fn active_notebook_generation(
        &self,
        domain: RecoveryDomainId,
        actor: &RecoveryPublicOwner,
    ) -> Result<Option<super::ActiveNotebookGeneration>, StoreError> {
        self.validate_physical_identity()?;
        let connection = self.connection.borrow();
        read_active_generation(&connection.connection, domain, actor)
            .map_err(|e| store_error(self.path(), StoreErrorKind::Integrity, e))
    }
    fn publish(&self, prepared: PreparedRecoveryWrite) -> StoreDisposition<RecoveryDomainState> {
        let owner_keys = prepared
            .public_changes
            .iter()
            .map(|change| {
                owner_bytes(match change {
                    PreparedPublicOwnerChange::Insert(next)
                    | PreparedPublicOwnerChange::Replace { next, .. } => &next.owner,
                    PreparedPublicOwnerChange::Remove { owner, .. } => owner,
                })
            })
            .collect::<Vec<_>>();
        self.transact(|transaction| {
            let current = read_domain(transaction, prepared.domain_id).map_err(sql_refusal)?.ok_or(StoreRefusal::DomainMissing)?;
            if current.revision != prepared.expected_revision { return Err(StoreRefusal::StaleRevision); }
            let revision = increment(current.revision)?;
            let content_revision = increment(current.content_revision)?;
            if prepared.allocation_ceiling < current.allocation_ceiling { return Err(StoreRefusal::CeilingDecrease); }
            insert_blocks(transaction, &prepared.blocks, &self.block_writes, &self.block_bytes).map_err(sql_refusal)?;
            for (change, key) in prepared.public_changes.iter().zip(&owner_keys) { apply_owner_change(transaction, prepared.domain_id, change, key)?; }
            if let Some(change) = &prepared.generation_change {
                apply_generation_change(transaction, prepared.domain_id, change)?;
            }
            let updated = transaction.execute("UPDATE recovery_domains SET revision=?1,content_revision=?2,ceiling=?3,snapshot_root=?4 WHERE domain=?5 AND revision=?6 AND ceiling<=?3", params![
                revision.0.to_be_bytes().as_slice(), content_revision.0.to_be_bytes().as_slice(), prepared.allocation_ceiling.0.to_be_bytes().as_slice(), prepared.snapshot_root.cid().to_bytes(), prepared.domain_id.bytes().as_slice(), prepared.expected_revision.0.to_be_bytes().as_slice()]).map_err(sql_refusal)?;
            if updated != 1 { return Err(StoreRefusal::StaleRevision); }
            Ok(RecoveryDomainState { revision, content_revision, allocation_ceiling: prepared.allocation_ceiling, snapshot_root: prepared.snapshot_root.clone(), ..current })
        }, &prepared.blocks)
    }

    /// This structurally distinct operation retains both immutable roots and
    /// the semantic content revision. No caller-selected boolean can claim a
    /// content-changing publication is an owner transfer.
    fn transfer_owner(
        &self,
        domain: RecoveryDomainId,
        expected: StoreRevision,
        previous: &RecoveryPublicOwnerState,
        successor: RecoveryPublicOwner,
        successor_epoch: u64,
    ) -> StoreDisposition<RecoveryDomainState> {
        let previous_key = owner_bytes(&previous.owner);
        let successor_key = owner_bytes(&successor);
        self.transact(
            |transaction| {
                let current = read_domain(transaction, domain)
                    .map_err(sql_refusal)?
                    .ok_or(StoreRefusal::DomainMissing)?;
                if current.revision != expected {
                    return Err(StoreRefusal::StaleRevision);
                }
                let retained =
                    read_public_owner_key(transaction, domain, &previous.owner, &previous_key)
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
                    &previous_key,
                )?;
                apply_owner_change(
                    transaction,
                    domain,
                    &PreparedPublicOwnerChange::Insert(RecoveryPublicOwnerState {
                        owner: successor.clone(),
                        epoch: successor_epoch,
                        surface_root: retained.surface_root,
                    }),
                    &successor_key,
                )?;
                if let Some(active) = read_active_generation(transaction, domain, &previous.owner)
                    .map_err(sql_refusal)?
                {
                    if active.owner != previous.owner {
                        return Err(StoreRefusal::StaleEpoch);
                    }
                    let next = super::ActiveNotebookGeneration {
                        owner: successor.clone(),
                        references: active.references,
                    };
                    apply_generation_change(
                        transaction,
                        domain,
                        &PreparedGenerationChange {
                            expected: None,
                            next,
                        },
                    )?;
                    transaction
                        .execute(
                            "DELETE FROM active_notebook_generations WHERE domain=?1 AND actor=?2",
                            params![
                                domain.bytes().as_slice(),
                                owner_bytes(&previous.owner.actor_owner())
                            ],
                        )
                        .map_err(sql_refusal)?;
                }
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
            &[],
        )
    }

    fn transact<T>(
        &self,
        action: impl FnOnce(&Transaction<'_>) -> Result<T, StoreRefusal>,
        blocks: &[(Cid, Arc<[u8]>)],
    ) -> StoreDisposition<T> {
        for (cid, bytes) in blocks {
            if let Err(error) = tidepool_toolchain::exact_context::verify_block(cid, bytes) {
                return StoreDisposition::Refused(StoreRefusal::Storage(store_error(
                    self.path(),
                    StoreErrorKind::Integrity,
                    error,
                )));
            }
        }
        let mut owner = self.connection.borrow_mut();
        if let Err(error) = self.validate_physical_identity() {
            return StoreDisposition::Refused(StoreRefusal::Storage(error));
        }
        #[cfg(test)]
        let fault = owner.fault.take();
        #[cfg(test)]
        if fault == Some(StoreFault::WorkerPanic) {
            panic!("injected native storage worker panic");
        }
        self.transactions
            .set(self.transactions.get().wrapping_add(1));
        let result = transact(
            &mut owner.connection,
            action,
            #[cfg(test)]
            fault,
        );
        match result {
            TransactionResult::Committed(value) => StoreDisposition::Committed(value),
            TransactionResult::Refused(mut refusal) => {
                if let StoreRefusal::Storage(error) = &mut refusal {
                    if error.path.as_os_str().is_empty() {
                        error.path = self.path().to_path_buf();
                    }
                }
                StoreDisposition::Refused(refusal)
            }
            TransactionResult::RecoveryRequired(detail) => {
                let error = store_error(self.path(), StoreErrorKind::RecoveryRequired, detail);
                StoreDisposition::RecoveryRequired(error)
            }
        }
    }
}

impl Blockstore for NativeStorage {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        let owner = self.connection.borrow();
        self.validate_physical_identity()?;
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
            };
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
    StoreRefusal::Storage(sql_store_error(Path::new(""), error))
}
fn sql_store_error(path: &Path, error: rusqlite::Error) -> StoreError {
    let kind = match &error {
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::InvalidQuery
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::QueryReturnedNoRows => StoreErrorKind::Integrity,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            StoreErrorKind::Integrity
        }
        _ => StoreErrorKind::Storage,
    };
    store_error(path, kind, error)
}
fn insert_blocks(
    transaction: &Transaction<'_>,
    blocks: &[(Cid, Arc<[u8]>)],
    writes: &Cell<u64>,
    written_bytes: &Cell<u64>,
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
        } else {
            writes.set(writes.get().wrapping_add(1));
            written_bytes.set(written_bytes.get().wrapping_add(bytes.len() as u64));
        }
    }
    Ok(())
}
fn read_active_generation(
    connection: &Connection,
    domain: RecoveryDomainId,
    actor: &RecoveryPublicOwner,
) -> Result<Option<super::ActiveNotebookGeneration>, rusqlite::Error> {
    let record: Option<(Vec<u8>, Vec<u8>)> = connection.query_row(
        "SELECT g.record,g.owner FROM active_notebook_generations a JOIN notebook_generations g ON a.domain=g.domain AND a.owner=g.owner WHERE a.domain=?1 AND a.actor=?2",
        params![domain.bytes().as_slice(), owner_bytes(&actor.actor_owner())], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
    record
        .map(|(bytes, selected_owner)| {
            let value: super::ActiveNotebookGeneration =
                fvm_ipld_encoding::from_slice(&bytes).map_err(|_| rusqlite::Error::InvalidQuery)?;
            value
                .references
                .validate()
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            if value.owner.actor_owner() != actor.actor_owner()
                || owner_bytes(&value.owner) != selected_owner
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Ok(value)
        })
        .transpose()
}
fn apply_generation_change(
    transaction: &Transaction<'_>,
    domain: RecoveryDomainId,
    change: &PreparedGenerationChange,
) -> Result<(), StoreRefusal> {
    change
        .next
        .references
        .validate()
        .map_err(|_| StoreRefusal::StaleEpoch)?;
    let actor = change.next.owner.actor_owner();
    let current = read_active_generation(transaction, domain, &actor).map_err(sql_refusal)?;
    if current != change.expected {
        return Err(StoreRefusal::StaleEpoch);
    }
    let issued = match current {
        Some(current) => current
            .generation()
            .next()
            .map_err(|_| StoreRefusal::RevisionExhausted)?,
        None => super::NotebookGenerationId::INITIAL,
    };
    if change.next.generation() != issued
        || read_public_owner(transaction, domain, &change.next.owner)
            .map_err(sql_refusal)?
            .is_none()
    {
        return Err(StoreRefusal::OwnerMissing);
    }
    let record = fvm_ipld_encoding::to_vec(&change.next).map_err(|_| StoreRefusal::StaleEpoch)?;
    transaction
        .execute(
            "INSERT INTO notebook_generations(domain,owner,record) VALUES(?1,?2,?3)",
            params![
                domain.bytes().as_slice(),
                owner_bytes(&change.next.owner),
                record
            ],
        )
        .map_err(sql_refusal)?;
    transaction.execute("INSERT INTO active_notebook_generations(domain,actor,owner) VALUES(?1,?2,?3) ON CONFLICT(domain,actor) DO UPDATE SET owner=excluded.owner", params![domain.bytes().as_slice(),owner_bytes(&actor),owner_bytes(&change.next.owner)]).map_err(sql_refusal)?;
    Ok(())
}
fn owner_bytes(owner: &RecoveryPublicOwner) -> Vec<u8> {
    fvm_ipld_encoding::to_vec(owner).expect("typed public owner is encodable")
}
fn apply_owner_change(
    transaction: &Transaction<'_>,
    domain: RecoveryDomainId,
    change: &PreparedPublicOwnerChange,
    key: &[u8],
) -> Result<(), StoreRefusal> {
    let domain = domain.bytes();
    let changed = match change {
        PreparedPublicOwnerChange::Insert(next) => transaction.execute("INSERT INTO public_owners(domain,owner,epoch,surface_root) VALUES(?1,?2,?3,?4) ON CONFLICT(domain,owner) DO NOTHING", params![domain.as_slice(), key, next.epoch.to_be_bytes().as_slice(), next.surface_root.cid().to_bytes()]).map_err(sql_refusal)?,
        PreparedPublicOwnerChange::Replace { expected_epoch, next } => transaction.execute("UPDATE public_owners SET epoch=?1,surface_root=?2 WHERE domain=?3 AND owner=?4 AND epoch=?5", params![next.epoch.to_be_bytes().as_slice(), next.surface_root.cid().to_bytes(), domain.as_slice(), key, expected_epoch.to_be_bytes().as_slice()]).map_err(sql_refusal)?,
        PreparedPublicOwnerChange::Remove { expected_epoch, .. } => transaction.execute("DELETE FROM public_owners WHERE domain=?1 AND owner=?2 AND epoch=?3", params![domain.as_slice(), key, expected_epoch.to_be_bytes().as_slice()]).map_err(sql_refusal)?,
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
    read_public_owner_key(connection, domain, owner, &owner_bytes(owner))
}
fn read_public_owner_key(
    connection: &Connection,
    domain: RecoveryDomainId,
    owner: &RecoveryPublicOwner,
    key: &[u8],
) -> rusqlite::Result<Option<RecoveryPublicOwnerState>> {
    connection
        .query_row(
            "SELECT epoch,surface_root FROM public_owners WHERE domain=?1 AND owner=?2",
            params![domain.bytes().as_slice(), key],
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
            return Err(store_error(path, StoreErrorKind::MissingDatabase, error));
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
fn physical_identity(path: &Path) -> Result<(u64, u64), StoreError> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        store_error(
            path,
            if error.kind() == std::io::ErrorKind::NotFound {
                StoreErrorKind::MissingDatabase
            } else {
                StoreErrorKind::Storage
            },
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(store_error(
            path,
            StoreErrorKind::Integrity,
            "run store pathname is not a regular file",
        ));
    }
    Ok((metadata.dev(), metadata.ino()))
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
CREATE TABLE notebook_generations(domain BLOB NOT NULL REFERENCES recovery_domains(domain),owner BLOB NOT NULL,record BLOB NOT NULL,PRIMARY KEY(domain,owner)) STRICT;
CREATE TABLE active_notebook_generations(domain BLOB NOT NULL,actor BLOB NOT NULL,owner BLOB NOT NULL,PRIMARY KEY(domain,actor),FOREIGN KEY(domain,owner) REFERENCES notebook_generations(domain,owner)) STRICT;
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
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse(path).unwrap(), incarnation)
            .unwrap()
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
            generation_change: None,
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
        let linked: (String, String) = Connection::open(reopened.path())
            .unwrap()
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
                assert!(store
                    .begin_native_transition()
                    .unwrap()
                    .complete_blocking()
                    .is_ok());
            } else {
                assert!(matches!(outcome, StoreDisposition::RecoveryRequired(_)));
                assert!(store.is_recovery_required());
                assert!(store.begin_native_transition().is_err());
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
        Connection::open(store.path())
            .unwrap()
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
    fn malformed_sql_metadata_is_integrity_failure_and_never_absence() {
        let (_directory, store) = store();
        domain(&store, RecoveryDomainId::Root, 64);
        let external = Connection::open(store.path()).unwrap();
        // Relational integrity remains satisfied; the immutable CID itself is
        // malformed and must be refused by the owning typed decoder.
        external
            .execute("INSERT INTO blocks(cid,bytes) VALUES(x'01',x'00')", [])
            .unwrap();
        external
            .execute(
                "UPDATE recovery_domains SET snapshot_root=x'01' WHERE domain=?1",
                [RecoveryDomainId::Root.bytes().as_slice()],
            )
            .unwrap();
        let error = store.read_domain(RecoveryDomainId::Root).unwrap_err();
        assert_eq!(error.kind, StoreErrorKind::Integrity);
        assert_eq!(error.path, store.path());
        assert!(matches!(
            store.read_snapshot_identity(RecoveryDomainId::Root, None),
            Err(StoreError {
                kind: StoreErrorKind::Integrity,
                ..
            })
        ));
        assert!(matches!(
            RunRecoveryStore::read_snapshot_at(store.path(), RecoveryDomainId::Root, None),
            Err(StoreError {
                kind: StoreErrorKind::Integrity,
                ..
            })
        ));
        assert!(matches!(
            store.reserve_ceiling(RecoveryDomainId::Root, StoreRevision(0), Generation(128)),
            StoreDisposition::Refused(StoreRefusal::Storage(StoreError {
                kind: StoreErrorKind::Integrity,
                ..
            }))
        ));
        assert!(!store.is_recovery_required());
    }

    #[test]
    fn strict_blob_constraints_refuse_text_and_missing_read_is_noncreating() {
        let (directory, store) = store();
        let domain = domain(&store, RecoveryDomainId::Root, 0);
        let connection = Connection::open(store.path()).unwrap();
        assert!(connection
            .execute(
                "UPDATE recovery_domains SET ceiling='00000000' WHERE domain=?1",
                [RecoveryDomainId::Root.bytes().as_slice()]
            )
            .is_err());
        drop(connection);
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

    #[tokio::test]
    async fn run_fence_wakes_existing_and_late_observers_without_storage_reads() {
        let (_directory, store) = store();
        let state = domain(&store, RecoveryDomainId::Root, 64);
        let waiting = store.wait_recovery_required();
        tokio::pin!(waiting);
        tokio::select! {
            biased;
            _ = waiting.as_mut() => panic!("unfenced run cannot notify"),
            _ = async {} => {}
        }
        store.inject_fault(StoreFault::CommitError);
        let StoreDisposition::RecoveryRequired(first) =
            store.reserve_ceiling(RecoveryDomainId::Root, state.revision, Generation(128))
        else {
            panic!("commit uncertainty must fence")
        };
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap();
        assert_eq!(observed, first);
        std::fs::rename(store.path(), store.path().with_extension("retained")).unwrap();
        let late = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.wait_recovery_required(),
        )
        .await
        .unwrap();
        assert_eq!(late, first);
        assert_eq!(store.ensure_ready().unwrap_err(), first);
    }

    #[test]
    fn replaced_database_path_revokes_every_old_handle_before_admission() {
        let (_directory, store) = store();
        let state = domain(&store, RecoveryDomainId::Root, 64);
        std::fs::rename(store.path(), store.path().with_extension("retained")).unwrap();
        let replacement = RunRecoveryStore::create(store.path()).unwrap();
        assert_ne!(store.database_id(), replacement.database_id());
        assert_eq!(
            store.ensure_ready().unwrap_err().kind,
            StoreErrorKind::Integrity
        );
        assert_eq!(
            store.read_domain(RecoveryDomainId::Root).unwrap_err().kind,
            StoreErrorKind::Integrity
        );
        assert!(store.get(state.snapshot_root.cid()).is_err());
        let before = store.work().unwrap();
        assert!(matches!(
            store.reserve_ceiling(RecoveryDomainId::Root, state.revision, Generation(128)),
            StoreDisposition::Refused(StoreRefusal::Storage(StoreError {
                kind: StoreErrorKind::Integrity,
                ..
            }))
        ));
        assert_eq!(before, store.work().unwrap());
    }

    #[test]
    fn durable_native_installation_requires_ack_and_abandonment_fences() {
        let (_directory, store) = store();
        let state = domain(&store, RecoveryDomainId::Root, 64);
        let (root, blocks) = blocks(&("installed candidate", 3u64));
        let StoreDisposition::Committed(receipt) =
            store.commit_publication(PreparedRecoveryWrite {
                domain_id: RecoveryDomainId::Root,
                expected_revision: state.revision,
                snapshot_root: DomainSnapshotRoot::new(root),
                allocation_ceiling: state.allocation_ceiling,
                blocks,
                generation_change: None,
                public_changes: Vec::new(),
            })
        else {
            panic!("real durable publication must commit");
        };
        assert_eq!(store.status(), NativeStoreStatus::Installing);
        assert_eq!(receipt.state.revision, StoreRevision(1));
        drop(receipt);
        assert!(matches!(store.status(), NativeStoreStatus::Fenced(_)));
        assert!(store.begin_native_transition().is_err());
        assert!(store.read_domain(RecoveryDomainId::Root).is_err());
        let reopened = RunRecoveryStore::open(store.path()).unwrap();
        assert_eq!(
            reopened
                .read_domain(RecoveryDomainId::Root)
                .unwrap()
                .unwrap()
                .snapshot_root
                .cid(),
            &root
        );
        assert_eq!(reopened.status(), NativeStoreStatus::Ready);
    }

    #[test]
    fn durable_native_transition_has_explicit_completion_and_no_drop_success() {
        let (_directory, store) = store();
        domain(&store, RecoveryDomainId::Root, 64);
        let ticket = store.begin_native_transition().unwrap();
        assert_eq!(store.status(), NativeStoreStatus::Pending);
        ticket.complete_blocking().unwrap();
        assert_eq!(store.status(), NativeStoreStatus::Ready);
        let abandoned = store.begin_native_transition().unwrap();
        drop(abandoned);
        assert!(matches!(store.status(), NativeStoreStatus::Fenced(_)));
        assert!(store.begin_native_transition().is_err());
    }

    #[test]
    fn confirmed_commit_installs_once_under_run_guard_and_faults_never_install() {
        for fault in [
            None,
            Some(StoreFault::BeforeCommit),
            Some(StoreFault::CommitError),
        ] {
            let (_directory, store) = store();
            let state = domain(&store, RecoveryDomainId::Root, 64);
            let (root, blocks) = blocks(&("new root", 1u64));
            if let Some(fault) = fault {
                store.inject_fault(fault);
            }
            let mut installed = 0;
            let outcome = store.publish_and_install(
                PreparedRecoveryWrite {
                    domain_id: RecoveryDomainId::Root,
                    expected_revision: state.revision,
                    snapshot_root: DomainSnapshotRoot::new(root),
                    allocation_ceiling: state.allocation_ceiling,
                    blocks,
                    generation_change: None,
            public_changes: Vec::new(),
                },
                |state| {
                    assert!(
                        store.status() == NativeStoreStatus::Installing,
                        "native installation stays pending until resident activation acknowledgement"
                    );
                    installed += 1;
                    state
                },
            );
            assert_eq!(installed, usize::from(fault.is_none()));
            assert_eq!(
                matches!(outcome, StoreDisposition::Committed(_)),
                fault.is_none()
            );
            let reopened = RunRecoveryStore::open(store.path()).unwrap();
            let observed = reopened
                .read_domain(RecoveryDomainId::Root)
                .unwrap()
                .unwrap();
            assert_eq!(
                observed.snapshot_root.cid() == &root,
                fault != Some(StoreFault::BeforeCommit)
            );
        }
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
    fn generation_write(
        store: &RunRecoveryStore,
        expected: Option<super::super::ActiveNotebookGeneration>,
        actor: &RecoveryPublicOwner,
        marker: u8,
    ) -> (
        PreparedRecoveryWrite,
        super::super::ActiveNotebookGeneration,
    ) {
        use super::super::{
            ActiveNotebookGeneration, ComposedPromptSnapshot, DurableGenerationReferences,
            GenerationArtifactReference, NotebookGenerationId,
        };
        let state = store.read_domain(RecoveryDomainId::Root).unwrap().unwrap();
        let id = expected
            .as_ref()
            .map_or(NotebookGenerationId::INITIAL, |v| {
                v.generation().next().unwrap()
            });
        let next = ActiveNotebookGeneration {
            owner: actor.for_notebook_generation(id),
            references: DurableGenerationReferences {
                source: GenerationArtifactReference::capture(vec![marker, 1]),
                installation: GenerationArtifactReference::capture(vec![marker, 2]),
                prompts: ComposedPromptSnapshot::capture(format!("prompt {marker}")),
            },
        };
        let (cid, blocks) = blocks(&("surface", marker));
        let write = PreparedRecoveryWrite {
            domain_id: RecoveryDomainId::Root,
            expected_revision: state.revision,
            snapshot_root: state.snapshot_root,
            allocation_ceiling: state.allocation_ceiling,
            blocks,
            public_changes: vec![PreparedPublicOwnerChange::Insert(
                RecoveryPublicOwnerState {
                    owner: next.owner.clone(),
                    epoch: 1,
                    surface_root: PublicSurfaceRoot::new(cid),
                },
            )],
            generation_change: Some(PreparedGenerationChange {
                expected,
                next: next.clone(),
            }),
        };
        (write, next)
    }

    #[test]
    fn notebook_generation_commit_and_crash_recovery_select_one_complete_generation() {
        for fault in [
            None,
            Some(StoreFault::BeforeCommit),
            Some(StoreFault::CommitError),
        ] {
            let (directory, store) = store();
            domain(&store, RecoveryDomainId::Root, 64);
            let actor = owner("root/generation", 1);
            let (write, next) = generation_write(&store, None, &actor, 7);
            if let Some(fault) = fault {
                store.inject_fault(fault);
            }
            match store.commit_publication(write) {
                StoreDisposition::Committed(receipt) => receipt.installed_blocking().unwrap(),
                StoreDisposition::Refused(_) => assert_eq!(fault, Some(StoreFault::BeforeCommit)),
                StoreDisposition::RecoveryRequired(_) => {
                    assert_eq!(fault, Some(StoreFault::CommitError));
                    assert!(store.ensure_ready().is_err());
                }
            }
            drop(store);
            let recovered =
                RunRecoveryStore::open(&directory.path().join("recovery.sqlite")).unwrap();
            let actual = recovered
                .active_notebook_generation(RecoveryDomainId::Root, &actor)
                .unwrap();
            assert_eq!(
                actual,
                if fault == Some(StoreFault::BeforeCommit) {
                    None
                } else {
                    Some(next)
                }
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn notebook_generation_histories_preserve_late_owners_and_refuse_stale_heads(ops in prop::collection::vec((0u8..7, any::<u8>()), 1..35)) {
            let (_directory, mut store) = store();
            domain(&store, RecoveryDomainId::Root, 64);
            let actor = owner("root/model", 1);
            let mut head = None;
            let other = owner("root/independent", 1);
            let mut other_head = None;
            let mut retained = Vec::<RecoveryPublicOwner>::new();
            for (operation, marker) in ops {
                match operation {
                    0 | 1 => {
                        let (write, next) = generation_write(&store, head.clone(), &actor, marker);
                        if operation == 1 { store.inject_fault(StoreFault::BeforeCommit); }
                        match store.commit_publication(write) {
                            StoreDisposition::Committed(receipt) => { receipt.installed_blocking().unwrap(); retained.push(next.owner.clone()); head = Some(next); }
                            StoreDisposition::Refused(_) => prop_assert_eq!(operation, 1),
                            _ => panic!("unexpected generation transition"),
                        }
                    }
                    2 => {
                        // A declaration update changes only its qualified owner,
                        // never whichever notebook generation is currently active.
                        if let Some(old) = retained.first() {
                            let state = store.read_domain(RecoveryDomainId::Root).unwrap().unwrap();
                            let old_state = store.read_public_owner(RecoveryDomainId::Root, old).unwrap().unwrap();
                            let StoreDisposition::Committed(_) = public_change(&store, RecoveryDomainId::Root, &state, old.clone(), Some(old_state.epoch), u64::from(marker)) else { panic!("late owner publication refused") };
                        }
                    }
                    3 => {
                        if head.is_some() {
                            let (write, _) = generation_write(&store, None, &actor, marker);
                            prop_assert!(matches!(store.commit_publication(write), StoreDisposition::Refused(_)));
                        }
                    }
                    5 => {
                        let (write, next) = generation_write(&store, other_head.clone(), &other, marker);
                        let StoreDisposition::Committed(receipt) = store.commit_publication(write) else { panic!("other actor generation refused") };
                        receipt.installed_blocking().unwrap();
                        other_head = Some(next);
                    }
                    6 => {
                        let (write, next) = generation_write(&store, head.clone(), &actor, marker);
                        store.inject_fault(StoreFault::CommitError);
                        prop_assert!(matches!(store.commit_publication(write), StoreDisposition::RecoveryRequired(_)));
                        prop_assert!(store.ensure_ready().is_err());
                        let path = store.path().to_owned();
                        store = RunRecoveryStore::open(&path).unwrap();
                        retained.push(next.owner.clone());
                        head = Some(next);
                    }
                    _ => {
                        let recovered = RunRecoveryStore::open(store.path()).unwrap();
                        prop_assert_eq!(recovered.active_notebook_generation(RecoveryDomainId::Root, &actor).unwrap(), head.clone());
                    }
                }
                prop_assert_eq!(store.active_notebook_generation(RecoveryDomainId::Root, &actor).unwrap(), head.clone());
                prop_assert_eq!(store.active_notebook_generation(RecoveryDomainId::Root, &other).unwrap(), other_head.clone());
                for old in &retained { prop_assert!(store.read_public_owner(RecoveryDomainId::Root, old).unwrap().is_some()); }
            }
        }
    }
}
