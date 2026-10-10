//! Immutable exact snapshots. Resident maps retain structural sharing; the
//! operation-local materializer applies their library diff to FVM HAMTs.

use std::cell::RefCell;
use std::sync::Arc;

use cid::Cid;
use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
use fvm_ipld_encoding::{CborStore, DAG_CBOR};
use fvm_ipld_hamt::{BytesKey, Config, Hamt};
use imbl::{ordmap::DiffItem, OrdMap};
use multihash_codetable::{Code, MultihashDigest};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::artifact_inventory::{
    ArtifactBinding, ArtifactBindingNode, ArtifactId, ArtifactSelectionId, ScopeProofV3,
};

const HAMT_CONFIG: Config = Config {
    bit_width: 5,
    min_data_depth: 0,
    max_array_width: 3,
};

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("missing immutable block {0}")]
    MissingBlock(Cid),
    #[error("immutable block {0} failed its CID integrity check")]
    Integrity(Cid),
    #[error("unsupported immutable block CID {0}")]
    UnsupportedCid(Cid),
    #[error("immutable block storage: {0}")]
    Storage(#[source] anyhow::Error),
    #[error("malformed immutable snapshot: {0}")]
    Malformed(String),
}

macro_rules! root_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Cid);

        impl $name {
            pub fn cid(&self) -> &Cid {
                &self.0
            }
        }
    };
}

root_type!(SnapshotMapRoot);
root_type!(SelectionRoot);
root_type!(ContextRoot);

/// A key's encoding is a typed tuple, never a rendered name or a CBOR map
/// with a compound key. Its concrete Rust type belongs to the map's owner.
pub trait SnapshotKey: Ord + Clone + Serialize + DeserializeOwned {
    fn encode_key(&self) -> Result<Vec<u8>, SnapshotError> {
        fvm_ipld_encoding::to_vec(&(self,)).map_err(malformed)
    }

    fn decode_key(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let (key,): (Self,) = fvm_ipld_encoding::from_slice(bytes).map_err(malformed)?;
        Ok(key)
    }
}

impl SnapshotKey for u64 {}
impl SnapshotKey for String {}
impl SnapshotKey for ArtifactId {}
impl SnapshotKey for ArtifactSelectionId {}
impl SnapshotKey for ArtifactBinding {}
impl SnapshotKey for ArtifactBindingNode {}
impl SnapshotKey for crate::declaration_join::ExactModuleIdentity {}

fn malformed(error: impl std::fmt::Display) -> SnapshotError {
    SnapshotError::Malformed(error.to_string())
}

fn hamt_error(error: fvm_ipld_hamt::Error) -> SnapshotError {
    match error {
        fvm_ipld_hamt::Error::Dynamic(source) => match source.downcast::<SnapshotError>() {
            Ok(error) => error,
            Err(source) => malformed(source),
        },
        other => malformed(other),
    }
}

struct CheckedStore<'a, BS>(&'a BS);
impl<BS: Blockstore> Blockstore for CheckedStore<'_, BS> {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        let bytes = self
            .0
            .get(cid)
            .map_err(SnapshotError::Storage)?
            .ok_or(SnapshotError::MissingBlock(*cid))?;
        verify_block(cid, &bytes)?;
        Ok(Some(bytes))
    }
    fn put_keyed(&self, cid: &Cid, bytes: &[u8]) -> anyhow::Result<()> {
        verify_block(cid, bytes)?;
        self.0
            .put_keyed(cid, bytes)
            .map_err(SnapshotError::Storage)?;
        Ok(())
    }
}

pub fn verify_block(cid: &Cid, bytes: &[u8]) -> Result<(), SnapshotError> {
    if cid.version() != cid::Version::V1
        || cid.codec() != DAG_CBOR
        || cid.hash().code() != u64::from(Code::Blake2b256)
        || cid.hash().size() != 32
    {
        return Err(SnapshotError::UnsupportedCid(*cid));
    }
    if &Code::Blake2b256.digest(bytes) != cid.hash() {
        return Err(SnapshotError::Integrity(*cid));
    }
    Ok(())
}

pub fn read_value<T: DeserializeOwned, BS: Blockstore>(
    store: &BS,
    cid: &Cid,
) -> Result<T, SnapshotError> {
    let bytes = store
        .get(cid)
        .map_err(SnapshotError::Storage)?
        .ok_or(SnapshotError::MissingBlock(*cid))?;
    verify_block(cid, &bytes)?;
    fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaterializationMetrics {
    pub blocks: u64,
    pub encoded_bytes: u64,
    pub changed_entries: u64,
}

/// Bytes stay owned until the run transaction either commits or refuses. This
/// packet has no durability or compiler authority of its own.
#[derive(Clone, Debug, Default)]
pub struct StagedBlocks {
    blocks: Vec<(Cid, Arc<[u8]>)>,
    metrics: MaterializationMetrics,
}

impl StagedBlocks {
    pub fn blocks(&self) -> &[(Cid, Arc<[u8]>)] {
        &self.blocks
    }

    pub fn metrics(&self) -> MaterializationMetrics {
        self.metrics
    }

    pub fn into_blocks(self) -> impl Iterator<Item = (Cid, Arc<[u8]>)> {
        self.blocks.into_iter()
    }
}

/// FVM HAMTs and their unsynchronized child caches never leave this owner.
pub struct Materializer<'a, BS: Blockstore> {
    base: &'a BS,
    pending: MemoryBlockstore,
    changed_entries: u64,
}

impl<'a, BS: Blockstore> Materializer<'a, BS> {
    pub fn new(base: &'a BS) -> Self {
        Self {
            base,
            pending: MemoryBlockstore::new(),
            changed_entries: 0,
        }
    }

    pub fn materialize_value<T: Serialize>(&mut self, value: &T) -> Result<Cid, SnapshotError> {
        self.put_cbor(value, Code::Blake2b256)
            .map_err(SnapshotError::Storage)
    }

    /// Only borrowed structural differences are consumed. A bootstrap has no
    /// resident predecessor; subsequent calls retain the actual base snapshot.
    pub fn materialize_map<K, V>(
        &mut self,
        base: Option<(&SnapshotMapRoot, &OrdMap<K, V>)>,
        next: &OrdMap<K, V>,
    ) -> Result<SnapshotMapRoot, SnapshotError>
    where
        K: SnapshotKey,
        V: Clone + Eq + Serialize + DeserializeOwned,
    {
        self.materialize_map_with(base, next, |_, value| Ok(value.clone()))
    }

    /// Large owner facts become CID leaves through this same diff traversal.
    /// The mapper runs only for added or changed facts.
    pub fn materialize_map_with<K, V, W>(
        &mut self,
        base: Option<(&SnapshotMapRoot, &OrdMap<K, V>)>,
        next: &OrdMap<K, V>,
        mut encode: impl FnMut(&mut Self, &V) -> Result<W, SnapshotError>,
    ) -> Result<SnapshotMapRoot, SnapshotError>
    where
        K: SnapshotKey,
        V: Eq,
        W: Eq + Serialize + DeserializeOwned,
    {
        let empty = OrdMap::new();
        let previous = base.map_or(&empty, |(_, facts)| facts);
        let mut changes = previous.diff(next).peekable();
        if changes.peek().is_none() {
            if let Some((root, _)) = base {
                return Ok(root.clone());
            }
        }
        // Encode values before borrowing the materializer as a HAMT store.
        // This is the changed-entry set, never a cumulative map collection.
        let mut edits = Vec::new();
        for change in changes {
            self.changed_entries += 1;
            match change {
                DiffItem::Add(key, value)
                | DiffItem::Update {
                    new: (key, value), ..
                } => {
                    edits.push((BytesKey(key.encode_key()?), Some(encode(self, value)?)));
                }
                DiffItem::Remove(key, _) => edits.push((BytesKey(key.encode_key()?), None)),
            }
        }
        let mut hamt: Hamt<_, W> = match base {
            Some((root, _)) => {
                Hamt::load_with_config(root.cid(), &*self, HAMT_CONFIG).map_err(hamt_error)?
            }
            None => Hamt::new_with_config(&*self, HAMT_CONFIG),
        };
        for (key, value) in edits {
            match value {
                Some(value) => {
                    hamt.set(key, value).map_err(hamt_error)?;
                }
                None => {
                    hamt.delete(&key).map_err(hamt_error)?;
                }
            }
        }
        hamt.flush().map(SnapshotMapRoot).map_err(hamt_error)
    }

    pub fn finish(self) -> Result<StagedBlocks, SnapshotError> {
        struct Collector(RefCell<Vec<(Cid, Arc<[u8]>)>>);
        impl Blockstore for Collector {
            fn get(&self, _: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
                Ok(None)
            }
            fn put_keyed(&self, cid: &Cid, bytes: &[u8]) -> anyhow::Result<()> {
                self.0.borrow_mut().push((*cid, Arc::from(bytes)));
                Ok(())
            }
        }
        let collected = Collector(RefCell::new(Vec::new()));
        self.pending
            .copy_to(&collected)
            .map_err(SnapshotError::Storage)?;
        let mut blocks = collected.0.into_inner();
        blocks.sort_by_key(|(cid, _)| *cid);
        let metrics = MaterializationMetrics {
            blocks: blocks.len() as u64,
            encoded_bytes: blocks.iter().map(|(_, bytes)| bytes.len() as u64).sum(),
            changed_entries: self.changed_entries,
        };
        Ok(StagedBlocks { blocks, metrics })
    }
}

impl<BS: Blockstore> Blockstore for Materializer<'_, BS> {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        match self.pending.get(cid)? {
            Some(bytes) => Ok(Some(bytes)),
            None => CheckedStore(self.base).get(cid),
        }
    }

    fn put_keyed(&self, cid: &Cid, bytes: &[u8]) -> anyhow::Result<()> {
        verify_block(cid, bytes)?;
        // Existing immutable bytes need no duplicate staged insert. Fresh base
        // authentication is the blockstore owner's responsibility.
        if self.base.has(cid)? {
            return Ok(());
        }
        self.pending.put_keyed(cid, bytes)
    }
}

pub fn hydrate_map<K, V, BS>(
    store: &BS,
    root: &SnapshotMapRoot,
) -> Result<OrdMap<K, V>, SnapshotError>
where
    K: SnapshotKey,
    V: Clone + Eq + Serialize + DeserializeOwned,
    BS: Blockstore,
{
    let checked = CheckedStore(store);
    let hamt: Hamt<_, V> =
        Hamt::load_with_config(root.cid(), &checked, HAMT_CONFIG).map_err(hamt_error)?;
    let mut facts = OrdMap::new();
    hamt.for_each_cacheless(|key, value| {
        let key = K::decode_key(&key.0)?;
        if facts.insert(key, value.clone()).is_some() {
            return Err(anyhow::anyhow!("duplicate typed snapshot key"));
        }
        Ok(())
    })
    .map_err(hamt_error)?;
    Ok(facts)
}

/// Scope facts have a separately authenticated binding identity. Equality is
/// compact; the immutable issuing owner never substitutes rows under that ID.
#[derive(Clone, Debug)]
pub struct ImmutableScopeProofRef(Arc<ScopeProofV3>);

impl ImmutableScopeProofRef {
    pub(crate) fn from_issued(proof: Arc<ScopeProofV3>) -> Self {
        Self(proof)
    }
    pub fn proof(&self) -> &Arc<ScopeProofV3> {
        &self.0
    }
}

impl PartialEq for ImmutableScopeProofRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.selection == other.0.selection
    }
}
impl Eq for ImmutableScopeProofRef {}

/// Proof availability is distinct from roots, namespace and native byte custody.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExactSelectionSnapshot {
    pub(crate) scopes: OrdMap<ArtifactSelectionId, ImmutableScopeProofRef>,
    pub(crate) roots: OrdMap<ArtifactBindingNode, ()>,
    pub(crate) namespace: OrdMap<ArtifactId, ArtifactBinding>,
    pub(crate) native_custody: OrdMap<ArtifactBinding, ()>,
}

#[cfg(test)]
mod tests;
