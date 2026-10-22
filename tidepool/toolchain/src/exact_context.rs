//! Immutable exact snapshots. Resident maps retain structural sharing; the
//! operation-local materializer applies their library diff to FVM HAMTs.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::OnceLock;

use cid::Cid;
use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::{BytesKey, Config, Hamt};
use imbl::{ordmap::DiffItem, OrdMap};
use multihash_codetable::{Code, MultihashDigest};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::artifact_inventory::{
    ArtifactBinding, ArtifactBindingNode, ArtifactGraphSelection, ArtifactId, ArtifactSelectionId,
    CompilerInputRole, NativeCompletionSnapshotFacts, ScopeProofV3,
};
use crate::declaration_context::{
    NativeAvailabilitySnapshotFacts, NativeContributionSnapshotFacts, NativeTransportMode,
};
use crate::declaration_join::{ExactLexicalNode, ExactModuleIdentity};

// Transport facts retain canonical tuple order; decoding never acquires a view.
pub(crate) mod serde_ord_map {
    use super::*;
    pub fn serialize<K: Ord + Clone + Serialize, V: Clone + Serialize, S: serde::Serializer>(
        map: &OrdMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map.iter())
    }
    pub fn deserialize<
        'de,
        K: Ord + Clone + Deserialize<'de>,
        V: Clone + Deserialize<'de>,
        D: serde::Deserializer<'de>,
    >(
        deserializer: D,
    ) -> Result<OrdMap<K, V>, D::Error> {
        let rows = Vec::<(K, V)>::deserialize(deserializer)?;
        if rows.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(serde::de::Error::custom("noncanonical immutable map"));
        }
        Ok(rows.into_iter().collect())
    }
}
pub(crate) mod serde_ord_set {
    use super::*;
    pub fn serialize<K: Ord + Clone + Serialize, S: serde::Serializer>(
        set: &imbl::OrdSet<K>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(set.iter())
    }
    pub fn deserialize<'de, K: Ord + Clone + Deserialize<'de>, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<imbl::OrdSet<K>, D::Error> {
        let rows = Vec::<K>::deserialize(deserializer)?;
        if rows.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(serde::de::Error::custom("noncanonical immutable set"));
        }
        Ok(rows.into_iter().collect())
    }
}

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
    #[error("compiler context encoding version {found} is unsupported; expected {expected}")]
    UnsupportedContextVersion { found: u32, expected: u32 },
    #[error("malformed immutable snapshot: {0}")]
    Malformed(String),
}

/// Projection keeps legacy materialization errors separate from operation refusal.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SnapshotProjectionError {
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Operation(#[from] tidepool_repr::execution_schema::ParseError),
    #[error("snapshot projection checkpoint: {0}")]
    Checkpoint(#[from] std::io::Error),
    #[error("selected snapshot key is outside its cold fact owner")]
    MissingFact,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MapProjectionWork {
    pub(crate) key_lookups: u64,
    pub(crate) reused_values: u64,
    pub(crate) route_blocks: u64,
    pub(crate) route_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FrontierProjectionWork {
    pub(crate) root_references: u64,
    pub(crate) edges: u64,
    pub(crate) reused_blocks: u64,
    pub(crate) reused_bytes: u64,
    pub(crate) new_blocks: u64,
    pub(crate) new_bytes: u64,
    pub(crate) staged_hash_bytes: u64,
}

/// Count borrowed scalar/key wire bytes before allocating their encoding or copy.
/// The four publication codecs use scalar CID wires; this never serializes facts.
fn projection_wire_bound<T: Serialize>(
    value: &T,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<(), SnapshotProjectionError> {
    struct Counter<'a> {
        operation: &'a tidepool_repr::execution_schema::InventoryOperation,
        refused: Option<tidepool_repr::execution_schema::ParseError>,
    }
    impl std::io::Write for Counter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let amount = bytes
                .len()
                .checked_mul(4)
                .ok_or_else(|| std::io::Error::other("snapshot wire bound overflow"))?;
            if let Err(error) = self.operation.charge(amount) {
                self.refused = Some(error);
                return Err(std::io::Error::other("snapshot wire budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter {
        operation,
        refused: None,
    };
    if let Err(error) = ciborium::ser::into_writer(value, &mut counter) {
        if let Some(error) = counter.refused {
            return Err(error.into());
        }
        return Err(malformed(error).into());
    }
    Ok(())
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
impl SnapshotKey for [u8; 32] {}
impl SnapshotKey for String {}
impl SnapshotKey for ArtifactId {}
impl SnapshotKey for ArtifactSelectionId {}
impl SnapshotKey for ArtifactBinding {}
impl SnapshotKey for ArtifactBindingNode {}
impl SnapshotKey for crate::declaration_join::ExactModuleIdentity {}

fn malformed(error: impl std::fmt::Display) -> SnapshotError {
    SnapshotError::Malformed(error.to_string())
}

fn storage_error(source: anyhow::Error) -> SnapshotError {
    match source.downcast::<SnapshotError>() {
        Ok(error) => error,
        Err(source) => SnapshotError::Storage(source),
    }
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
            .map_err(storage_error)?
            .ok_or(SnapshotError::MissingBlock(*cid))?;
        verify_block(cid, &bytes)?;
        Ok(Some(bytes))
    }
    fn put_keyed(&self, cid: &Cid, bytes: &[u8]) -> anyhow::Result<()> {
        verify_block(cid, bytes)?;
        self.0.put_keyed(cid, bytes).map_err(storage_error)?;
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

fn read_checked_bytes<BS: Blockstore>(store: &BS, cid: &Cid) -> Result<Vec<u8>, SnapshotError> {
    let bytes = store
        .get(cid)
        .map_err(storage_error)?
        .ok_or(SnapshotError::MissingBlock(*cid))?;
    verify_block(cid, &bytes)?;
    Ok(bytes)
}

pub fn read_value<T: DeserializeOwned, BS: Blockstore>(
    store: &BS,
    cid: &Cid,
) -> Result<T, SnapshotError> {
    fvm_ipld_encoding::from_slice(&read_checked_bytes(store, cid)?).map_err(malformed)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaterializationMetrics {
    pub blocks: u64,
    pub staged_bytes: u64,
    /// All bytes offered to this materializer's block writer, including
    /// previously encoded leaves and blocks whose CID already exists.
    pub offered_bytes: u64,
    /// Output bytes encoded by `materialize_value`. Library HAMT/key encoding
    /// and immutable leaf encoders execute outside this counter.
    pub serialized_bytes: u64,
    /// Bytes authenticated by this materializer, including existing block
    /// reads and reuse checks. CID construction is a separate hash operation.
    pub cid_validation_bytes: u64,
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

/// Current-root block custody for an immutable snapshot. Reference counts are
/// local to this frontier, so forks share bytes without owning predecessor
/// frontiers. Only staged/replaced paths change during publication.
#[derive(Clone, Default)]
pub(crate) struct ReachableBlocks {
    roots: Vec<Cid>,
    blocks: OrdMap<Cid, ReachableBlock>,
    work: FrontierWork,
}

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub(crate) struct FrontierWork {
    pub(crate) link_visits: u64,
    pub(crate) unlink_visits: u64,
    pub(crate) verified_bytes: u64,
    pub(crate) decoded_bytes: u64,
}

/// The named roots that define a compiler world's semantic frontier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SemanticRoots {
    pub(crate) primary: Cid,
    pub(crate) associations: Cid,
    pub(crate) bindings: Cid,
    pub(crate) lexical: Cid,
}

#[derive(Clone, Eq, PartialEq)]
struct ReachableBlock {
    bytes: Arc<[u8]>,
    children: Arc<[Cid]>,
    incoming: u64,
}

impl ReachableBlocks {
    pub(crate) fn work(&self) -> FrontierWork {
        self.work
    }

    pub(crate) fn roots(&self) -> &[Cid] {
        &self.roots
    }

    pub(crate) fn blocks(&self) -> impl Iterator<Item = (&Cid, &Arc<[u8]>)> {
        self.blocks.iter().map(|(cid, block)| (cid, &block.bytes))
    }

    /// Link all new roots before removing old roots: shared descendants never
    /// transiently disappear. Staged blocks outside the new closure are dropped.
    pub(crate) fn replace_roots(
        &self,
        staged: &StagedBlocks,
        roots: Vec<Cid>,
    ) -> Result<Self, SnapshotError> {
        if roots == self.roots {
            let mut unchanged = self.clone();
            unchanged.work = FrontierWork::default();
            return Ok(unchanged);
        }
        let pending = staged
            .blocks
            .iter()
            .map(|(cid, bytes)| (*cid, bytes))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut next = self.clone();
        next.work = FrontierWork::default();
        for root in &roots {
            next.retain(*root, &pending)?;
        }
        for root in &self.roots {
            next.release(*root)?;
        }
        next.roots = roots;
        Ok(next)
    }

    /// Replace the semantic frontier from its named roots.
    pub(crate) fn replace_semantic_roots(
        &self,
        staged: &StagedBlocks,
        roots: SemanticRoots,
    ) -> Result<(SemanticRoots, Self), SnapshotError> {
        let frontier = self.replace_roots(
            staged,
            vec![
                roots.primary,
                roots.associations,
                roots.bindings,
                roots.lexical,
            ],
        )?;
        Ok((roots, frontier))
    }

    /// Build independent custody for selected roots, borrowing only cached block
    /// bytes/links. Local incoming counts belong to this root set, never the base.
    pub(crate) fn project_roots(
        &self,
        staged: &StagedBlocks,
        roots: Vec<Cid>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<(Self, FrontierProjectionWork), SnapshotProjectionError> {
        fn tag_capacity(value: &ciborium::value::Value) -> usize {
            use ciborium::value::Value;
            match value {
                Value::Array(values) => values.iter().map(tag_capacity).sum(),
                Value::Map(values) => values
                    .iter()
                    .map(|(k, v)| tag_capacity(k) + tag_capacity(v))
                    .sum(),
                Value::Tag(_, _) => 1,
                _ => 0,
            }
        }
        let mut next = Self::default();
        let mut work = FrontierProjectionWork::default();
        let mut pending = Vec::new();
        for root in &roots {
            operation.reserve::<Cid>(1)?;
            pending.push(*root);
            work.root_references += 1;
        }
        while let Some(cid) = pending.pop() {
            crate::host_work::checkpoint()?;
            operation.charge(1)?;
            next.work.link_visits += 1;
            if let Some(block) = next.blocks.get_mut(&cid) {
                block.incoming = block
                    .incoming
                    .checked_add(1)
                    .ok_or_else(|| malformed("snapshot block reference overflow"))?;
                continue;
            }
            operation.reserve::<(Cid, ReachableBlock)>(4)?;
            let (bytes, children) = if let Some(block) = self.blocks.get(&cid) {
                work.reused_blocks += 1;
                work.reused_bytes += block.bytes.len() as u64;
                (Arc::clone(&block.bytes), Arc::clone(&block.children))
            } else {
                // Staged packets are sorted by Materializer::finish. A keyed
                // search avoids constructing a census of unrelated staged blocks.
                let index = staged
                    .blocks
                    .binary_search_by_key(&cid, |(key, _)| *key)
                    .map_err(|_| SnapshotError::MissingBlock(cid))?;
                let bytes = &staged.blocks[index].1;
                if bytes.len() > operation.limits().max_bytes {
                    return Err(tidepool_repr::execution_schema::ParseError::ByteLimit {
                        limit: operation.limits().max_bytes,
                        actual: bytes.len(),
                    }
                    .into());
                }
                operation.charge(bytes.len())?;
                work.staged_hash_bytes += bytes.len() as u64;
                verify_block(&cid, bytes)?;
                let value = operation.decode_value(bytes, operation.limits().max_bytes)?;
                let capacity = tag_capacity(&value);
                operation.reserve::<Cid>(capacity)?;
                let mut children = Vec::with_capacity(capacity);
                snapshot_links(&value, &mut children)?;
                work.new_blocks += 1;
                work.new_bytes += bytes.len() as u64;
                next.work.verified_bytes += bytes.len() as u64;
                next.work.decoded_bytes += bytes.len() as u64;
                (Arc::clone(bytes), Arc::from(children))
            };
            for child in children.iter() {
                operation.reserve::<Cid>(1)?;
                pending.push(*child);
                work.edges += 1;
            }
            next.blocks.insert(
                cid,
                ReachableBlock {
                    bytes,
                    children,
                    incoming: 1,
                },
            );
        }
        next.roots = roots;
        Ok((next, work))
    }

    fn retain(
        &mut self,
        cid: Cid,
        staged: &std::collections::BTreeMap<Cid, &Arc<[u8]>>,
    ) -> Result<(), SnapshotError> {
        self.work.link_visits = self.work.link_visits.saturating_add(1);
        if let Some(block) = self.blocks.get_mut(&cid) {
            block.incoming = block
                .incoming
                .checked_add(1)
                .ok_or_else(|| malformed("snapshot block reference overflow"))?;
            return Ok(());
        }
        let bytes = staged.get(&cid).ok_or(SnapshotError::MissingBlock(cid))?;
        self.work.verified_bytes = self.work.verified_bytes.saturating_add(bytes.len() as u64);
        verify_block(&cid, bytes)?;
        self.work.decoded_bytes = self.work.decoded_bytes.saturating_add(bytes.len() as u64);
        let value: ciborium::value::Value =
            ciborium::de::from_reader(&bytes[..]).map_err(malformed)?;
        let mut children = Vec::new();
        snapshot_links(&value, &mut children)?;
        // Preserve edge multiplicity, including identical child CIDs.
        for child in &children {
            self.retain(*child, staged)?;
        }
        self.blocks.insert(
            cid,
            ReachableBlock {
                bytes: Arc::clone(bytes),
                children: children.into(),
                incoming: 1,
            },
        );
        Ok(())
    }

    fn release(&mut self, cid: Cid) -> Result<(), SnapshotError> {
        self.work.unlink_visits = self.work.unlink_visits.saturating_add(1);
        let block = self
            .blocks
            .get_mut(&cid)
            .ok_or_else(|| malformed("released snapshot block is not retained"))?;
        if block.incoming > 1 {
            block.incoming -= 1;
            return Ok(());
        }
        let children = Arc::clone(&block.children);
        self.blocks.remove(&cid);
        for child in children.iter() {
            self.release(*child)?;
        }
        Ok(())
    }
}

impl Blockstore for ReachableBlocks {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.blocks.get(cid).map(|block| block.bytes.to_vec()))
    }
    fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
        Err(malformed("immutable snapshot frontier cannot accept writes").into())
    }
}

fn snapshot_links(
    value: &ciborium::value::Value,
    links: &mut Vec<Cid>,
) -> Result<(), SnapshotError> {
    use ciborium::value::Value;
    match value {
        Value::Array(values) => {
            for value in values {
                snapshot_links(value, links)?;
            }
        }
        Value::Map(values) => {
            for (key, value) in values {
                snapshot_links(key, links)?;
                snapshot_links(value, links)?;
            }
        }
        Value::Tag(42, body) => {
            let Value::Bytes(bytes) = body.as_ref() else {
                return Err(malformed("snapshot CID link is not bytes"));
            };
            let Some((&0, cid)) = bytes.split_first() else {
                return Err(malformed("snapshot CID link lacks identity prefix"));
            };
            links.push(Cid::try_from(cid).map_err(malformed)?);
        }
        Value::Tag(_, _) => return Err(malformed("unsupported snapshot CBOR tag")),
        _ => (),
    }
    Ok(())
}

/// FVM HAMTs and their unsynchronized child caches never leave this owner.
/// The caller retains immutable CID storage for this operation. Another store
/// or publication attempt requires a fresh materializer and authentication.
pub struct Materializer<'a, BS: Blockstore> {
    base: &'a BS,
    pending: MemoryBlockstore,
    changed_entries: u64,
    offered_bytes: Cell<u64>,
    serialized_bytes: Cell<u64>,
    cid_validation_bytes: Cell<u64>,
    selection_base: Option<MaterializedSelection>,
    native_availability_base: Option<
        MaterializedMap<ArtifactId, NativeAvailabilitySnapshotFacts, NativeAvailabilityCodec>,
    >,
    scopes: std::collections::BTreeMap<ArtifactSelectionId, MaterializedScope>,
    selections: std::collections::BTreeMap<usize, MaterializedSelection>,
    publication_native: std::collections::BTreeMap<usize, (Arc<PublicationNativeFacts>, Cid)>,
    publication_native_base: Option<Arc<PublicationNativeFacts>>,
}

struct MaterializedScope {
    cid: Cid,
    owners: std::collections::BTreeMap<usize, ImmutableScopeProofRef>,
}

impl<'a, BS: Blockstore> Materializer<'a, BS> {
    pub fn new(base: &'a BS) -> Self {
        Self {
            base,
            pending: MemoryBlockstore::new(),
            changed_entries: 0,
            offered_bytes: Cell::new(0),
            serialized_bytes: Cell::new(0),
            cid_validation_bytes: Cell::new(0),
            selection_base: None,
            native_availability_base: None,
            scopes: Default::default(),
            selections: Default::default(),
            publication_native: Default::default(),
            publication_native_base: None,
        }
    }

    pub fn materialize_value<T: Serialize>(&mut self, value: &T) -> Result<Cid, SnapshotError> {
        let bytes = fvm_ipld_encoding::to_vec(value).map_err(malformed)?;
        add_bytes(&self.serialized_bytes, bytes.len());
        let cid = Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&bytes));
        self.put_keyed(&cid, &bytes).map_err(storage_error)?;
        Ok(cid)
    }

    /// The base receipt couples its root, actual facts and fixed codec family.
    pub fn materialize_map<K, V>(
        &mut self,
        base: Option<&MaterializedMap<K, V>>,
        next: &OrdMap<K, V>,
    ) -> Result<MaterializedMap<K, V>, SnapshotError>
    where
        K: SnapshotKey,
        V: Clone + Eq + Serialize + DeserializeOwned,
    {
        self.materialize_map_with::<K, V, InlineValues>(base, next)
    }

    /// The codec is chosen by the fact owner, rather than a per-call closure.
    pub fn materialize_map_with<K, V, C>(
        &mut self,
        base: Option<&MaterializedMap<K, V, C>>,
        next: &OrdMap<K, V>,
    ) -> Result<MaterializedMap<K, V, C>, SnapshotError>
    where
        K: SnapshotKey,
        V: Clone + Eq,
        C: SnapshotValueCodec<V>,
    {
        let empty = OrdMap::new();
        let previous = base.map_or(&empty, |base| &base.facts);
        let mut changes = previous.diff(next).peekable();
        if changes.peek().is_none() {
            if let Some(base) = base {
                return Ok(base.clone());
            }
        }
        let mut edits = Vec::new();
        for change in changes {
            self.changed_entries += 1;
            match change {
                DiffItem::Add(key, value)
                | DiffItem::Update {
                    new: (key, value), ..
                } => {
                    edits.push((BytesKey(key.encode_key()?), Some(C::encode(self, value)?)));
                }
                DiffItem::Remove(key, _) => edits.push((BytesKey(key.encode_key()?), None)),
            }
        }
        let mut hamt: Hamt<_, C::Wire> = match base {
            Some(base) => {
                Hamt::load_with_config(base.root.cid(), &*self, HAMT_CONFIG).map_err(hamt_error)?
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
        let root = hamt.flush().map(SnapshotMapRoot).map_err(hamt_error)?;
        Ok(MaterializedMap {
            root,
            facts: next.clone(),
            codec: PhantomData,
        })
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
        self.pending.copy_to(&collected).map_err(storage_error)?;
        let mut blocks = collected.0.into_inner();
        blocks.sort_by_key(|(cid, _)| *cid);
        let metrics = MaterializationMetrics {
            blocks: blocks.len() as u64,
            staged_bytes: blocks.iter().map(|(_, bytes)| bytes.len() as u64).sum(),
            offered_bytes: self.offered_bytes.get(),
            serialized_bytes: self.serialized_bytes.get(),
            cid_validation_bytes: self.cid_validation_bytes.get(),
            changed_entries: self.changed_entries,
        };
        Ok(StagedBlocks { blocks, metrics })
    }
}

impl<BS: Blockstore> Blockstore for Materializer<'_, BS> {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        match self.pending.get(cid)? {
            Some(bytes) => Ok(Some(bytes)),
            None => {
                let bytes = self
                    .base
                    .get(cid)
                    .map_err(storage_error)?
                    .ok_or(SnapshotError::MissingBlock(*cid))?;
                add_bytes(&self.cid_validation_bytes, bytes.len());
                verify_block(cid, &bytes)?;
                Ok(Some(bytes))
            }
        }
    }

    fn put_keyed(&self, cid: &Cid, bytes: &[u8]) -> anyhow::Result<()> {
        add_bytes(&self.offered_bytes, bytes.len());
        add_bytes(&self.cid_validation_bytes, bytes.len());
        verify_block(cid, bytes)?;
        if let Some(existing) = self.base.get(cid).map_err(storage_error)? {
            add_bytes(&self.cid_validation_bytes, existing.len());
            verify_block(cid, &existing)?;
            return Ok(());
        }
        self.pending.put_keyed(cid, bytes).map_err(storage_error)?;
        Ok(())
    }
}

fn add_bytes(counter: &Cell<u64>, bytes: usize) {
    counter.set(counter.get().saturating_add(bytes as u64));
}

/// Keyed reads borrow the actual immutable receipt so copy size is known
/// before Blockstore's Vec allocation. Cached block integrity is already held.
struct ProjectionReadStore<'a> {
    base: &'a ReachableBlocks,
    operation: &'a tidepool_repr::execution_schema::InventoryOperation,
    blocks: Cell<u64>,
    bytes: Cell<u64>,
}
impl Blockstore for ProjectionReadStore<'_> {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        let block = self
            .base
            .blocks
            .get(cid)
            .ok_or(SnapshotError::MissingBlock(*cid))?;
        // Reserve the copy plus bounded HAMT key/pointer decode storage before
        // exposing the encoded route. The byte counter below stays actual.
        self.operation
            .charge(block.bytes.len().checked_mul(8).ok_or_else(|| {
                SnapshotProjectionError::Operation(
                    tidepool_repr::execution_schema::ParseError::LimitExceeded("work"),
                )
            })?)
            .map_err(SnapshotProjectionError::Operation)?;
        self.blocks.set(self.blocks.get() + 1);
        self.bytes.set(self.bytes.get() + block.bytes.len() as u64);
        Ok(Some(block.bytes.to_vec()))
    }
    fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
        Err(malformed("immutable projection cannot write its cold route").into())
    }
}
fn projection_hamt_error(error: fvm_ipld_hamt::Error) -> SnapshotProjectionError {
    match error {
        fvm_ipld_hamt::Error::Dynamic(source) => match source.downcast::<SnapshotProjectionError>()
        {
            Ok(error) => error,
            Err(source) => hamt_error(fvm_ipld_hamt::Error::Dynamic(source)).into(),
        },
        error => hamt_error(error).into(),
    }
}
impl Materializer<'_, ReachableBlocks> {
    /// Select actual cold CID wires by key. No caller-supplied fact, equality
    /// comparison, value clone or value codec participates in this operation.
    pub(crate) fn project_map_with<K, V, C>(
        &mut self,
        base: &MaterializedMap<K, V, C>,
        keys: &std::collections::BTreeSet<K>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<(SnapshotMapRoot, MapProjectionWork), SnapshotProjectionError>
    where
        K: SnapshotKey,
        C: SnapshotValueCodec<V, Wire = Cid>,
    {
        let mut work = MapProjectionWork::default();
        let mut edits = Vec::new();
        let route = ProjectionReadStore {
            base: self.base,
            operation,
            blocks: Cell::new(0),
            bytes: Cell::new(0),
        };
        operation.reserve::<usize>(64)?;
        // Do not load any cold HAMT for an empty projection.
        let cold = if keys.is_empty() {
            None
        } else {
            Some(
                Hamt::<_, C::Wire>::load_with_config(base.root.cid(), &route, HAMT_CONFIG)
                    .map_err(projection_hamt_error)?,
            )
        };
        for key in keys {
            crate::host_work::checkpoint()?;
            operation.charge(1)?;
            work.key_lookups += 1;
            projection_wire_bound(&(key,), operation)?;
            if !base.facts.contains_key(key) {
                return Err(SnapshotProjectionError::MissingFact);
            }
            operation.reserve::<(BytesKey, C::Wire)>(4)?;
            operation.reserve::<usize>(64)?;
            let key = BytesKey(key.encode_key()?);
            let wire = cold
                .as_ref()
                .expect("nonempty projection")
                .get(&key)
                .map_err(projection_hamt_error)?
                .ok_or_else(|| malformed("cold snapshot facts lack their encoded value"))?;
            projection_wire_bound(wire, operation)?;
            edits.push((key, *wire));
            work.reused_values += 1;
        }
        work.route_blocks = route.blocks.get();
        work.route_bytes = route.bytes.get();
        let mut projected: Hamt<_, C::Wire> = Hamt::new_with_config(&*self, HAMT_CONFIG);
        for (key, wire) in edits {
            projected.set(key, wire).map_err(hamt_error)?;
        }
        let root = projected.flush().map(SnapshotMapRoot).map_err(hamt_error)?;
        Ok((root, work))
    }
}

/// A structurally authenticated root and its actual immutable resident facts.
/// Decoding establishes no compiler or source-instance authority.
#[derive(Debug)]
pub struct MaterializedMap<K: Ord, V, C = InlineValues> {
    root: SnapshotMapRoot,
    facts: OrdMap<K, V>,
    codec: PhantomData<C>,
}
impl<K: Ord, V, C> Clone for MaterializedMap<K, V, C> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            facts: self.facts.clone(),
            codec: PhantomData,
        }
    }
}
impl<K: Ord, V, C> MaterializedMap<K, V, C> {
    pub fn root(&self) -> &SnapshotMapRoot {
        &self.root
    }
    pub fn facts(&self) -> &OrdMap<K, V> {
        &self.facts
    }
    pub fn into_facts(self) -> OrdMap<K, V> {
        self.facts
    }
}

pub trait SnapshotValueCodec<V> {
    type Wire: Clone + Eq + Serialize + DeserializeOwned;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        value: &V,
    ) -> Result<Self::Wire, SnapshotError>;
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        value: Self::Wire,
    ) -> Result<V, SnapshotError>;
}
#[derive(Clone, Copy, Debug)]
pub struct InlineValues;
impl<V: Clone + Eq + Serialize + DeserializeOwned> SnapshotValueCodec<V> for InlineValues {
    type Wire = V;
    fn encode<BS: Blockstore>(_: &mut Materializer<'_, BS>, value: &V) -> Result<V, SnapshotError> {
        Ok(value.clone())
    }
    fn decode<BS: Blockstore>(_: &SnapshotHydrator<'_, BS>, value: V) -> Result<V, SnapshotError> {
        Ok(value)
    }
}

/// Authenticate only the HAMT route selected by this typed key. Unselected
/// leaves are neither inspected nor granted compiler authority.
pub fn lookup_map<K, V, BS>(
    store: &BS,
    root: &SnapshotMapRoot,
    key: &K,
) -> Result<Option<V>, SnapshotError>
where
    K: SnapshotKey,
    V: Clone + Eq + Serialize + DeserializeOwned,
    BS: Blockstore,
{
    lookup_map_with::<K, V, InlineValues, BS>(store, root, key)
}

pub fn lookup_map_with<K, V, C, BS>(
    store: &BS,
    root: &SnapshotMapRoot,
    key: &K,
) -> Result<Option<V>, SnapshotError>
where
    K: SnapshotKey,
    V: Clone + Eq,
    C: SnapshotValueCodec<V>,
    BS: Blockstore,
{
    let encoded = key.encode_key()?;
    if K::decode_key(&encoded)?.encode_key()? != encoded {
        return Err(malformed("noncanonical typed snapshot key"));
    }
    let checked = CheckedStore(store);
    let hamt: Hamt<_, C::Wire> =
        Hamt::load_with_config(root.cid(), &checked, HAMT_CONFIG).map_err(hamt_error)?;
    hamt.get(&BytesKey(encoded))
        .map_err(hamt_error)?
        .cloned()
        .map(|value| C::decode(&SnapshotHydrator::new(store), value))
        .transpose()
}

pub fn hydrate_map<K, V, BS>(
    store: &BS,
    root: &SnapshotMapRoot,
) -> Result<MaterializedMap<K, V>, SnapshotError>
where
    K: SnapshotKey,
    V: Clone + Eq + Serialize + DeserializeOwned,
    BS: Blockstore,
{
    hydrate_map_with::<K, V, InlineValues, BS>(store, root)
}

pub fn hydrate_map_with<K, V, C, BS>(
    store: &BS,
    root: &SnapshotMapRoot,
) -> Result<MaterializedMap<K, V, C>, SnapshotError>
where
    K: SnapshotKey,
    V: Clone + Eq,
    C: SnapshotValueCodec<V>,
    BS: Blockstore,
{
    hydrate_map_owned::<K, V, C, BS>(&SnapshotHydrator::new(store), root)
}

fn hydrate_map_owned<K, V, C, BS>(
    store: &SnapshotHydrator<'_, BS>,
    root: &SnapshotMapRoot,
) -> Result<MaterializedMap<K, V, C>, SnapshotError>
where
    K: SnapshotKey,
    V: Clone + Eq,
    C: SnapshotValueCodec<V>,
    BS: Blockstore,
{
    let checked = CheckedStore(store);
    let hamt: Hamt<_, C::Wire> =
        Hamt::load_with_config(root.cid(), &checked, HAMT_CONFIG).map_err(hamt_error)?;
    let mut facts = OrdMap::new();
    hamt.for_each_cacheless(|key, value| {
        let decoded = K::decode_key(&key.0)?;
        if decoded.encode_key()? != key.0 {
            return Err(malformed("noncanonical typed snapshot key").into());
        }
        let key = decoded;
        if facts
            .insert(key, C::decode(store, value.clone())?)
            .is_some()
        {
            return Err(malformed("duplicate typed snapshot key").into());
        }
        Ok(())
    })
    .map_err(hamt_error)?;
    Ok(MaterializedMap {
        root: root.clone(),
        facts,
        codec: PhantomData,
    })
}

/// One read operation shares immutable decoded graph owners by exact CID.
/// These decoded facts still require inventory authentication before use.
pub struct SnapshotHydrator<'a, BS> {
    publication_native: RefCell<std::collections::BTreeMap<Cid, Arc<PublicationNativeFacts>>>,
    store: &'a BS,
    selections: RefCell<std::collections::BTreeMap<SelectionRoot, MaterializedSelection>>,
    scopes: RefCell<std::collections::BTreeMap<Cid, ImmutableScopeProofRef>>,
}
impl<'a, BS: Blockstore> SnapshotHydrator<'a, BS> {
    fn new(store: &'a BS) -> Self {
        Self {
            publication_native: Default::default(),
            store,
            selections: RefCell::new(Default::default()),
            scopes: RefCell::new(Default::default()),
        }
    }
    fn selection(&self, root: &SelectionRoot) -> Result<MaterializedSelection, SnapshotError> {
        if let Some(selection) = self.selections.borrow().get(root) {
            return Ok(selection.clone());
        }
        let selection = hydrate_selection_owned(self, root)?;
        self.selections
            .borrow_mut()
            .insert(root.clone(), selection.clone());
        Ok(selection)
    }
}
impl<BS: Blockstore> Blockstore for SnapshotHydrator<'_, BS> {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        self.store.get(cid)
    }
    fn put_keyed(&self, cid: &Cid, bytes: &[u8]) -> anyhow::Result<()> {
        self.store.put_keyed(cid, bytes)
    }
}

/// Scope facts keep their complete immutable proof. Independently decoded
/// facts with the same claimed scope identity need not have the same body.
#[derive(Clone, Debug)]
pub struct ImmutableScopeProofRef(Arc<ScopeFact>);

#[derive(Debug)]
struct ScopeFact {
    proof: Arc<ScopeProofV3>,
    direct_ancestors: std::collections::BTreeSet<ArtifactSelectionId>,
    encoded: OnceLock<(Cid, Arc<[u8]>)>,
}

impl ImmutableScopeProofRef {
    pub(crate) fn from_issued(proof: Arc<ScopeProofV3>) -> Self {
        let direct_ancestors = proof.scope_dependencies();
        Self(Arc::new(ScopeFact {
            proof,
            direct_ancestors,
            encoded: OnceLock::new(),
        }))
    }
    pub fn proof(&self) -> &Arc<ScopeProofV3> {
        &self.0.proof
    }
    /// Direct external scope references belong to this immutable proof owner.
    /// They are not the transitive proof closure and are derived only at birth.
    pub(crate) fn direct_ancestors(&self) -> &std::collections::BTreeSet<ArtifactSelectionId> {
        &self.0.direct_ancestors
    }
    fn encoded(&self) -> Result<&(Cid, Arc<[u8]>), SnapshotError> {
        if let Some(encoded) = self.0.encoded.get() {
            return Ok(encoded);
        }
        let bytes: Arc<[u8]> = fvm_ipld_encoding::to_vec(self.proof().as_ref())
            .map_err(malformed)?
            .into();
        let cid = Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&bytes));
        let _ = self.0.encoded.set((cid, bytes));
        Ok(self.0.encoded.get().expect("scope fact initialized"))
    }
}

impl PartialEq for ImmutableScopeProofRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.proof() == other.proof()
    }
}
impl Eq for ImmutableScopeProofRef {}

/// Proof availability is distinct from selected roots, namespace and exact custody.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExactSelectionSnapshot {
    pub(crate) scopes: OrdMap<ArtifactSelectionId, ImmutableScopeProofRef>,
    pub(crate) roots: OrdMap<ArtifactBindingNode, ()>,
    pub(crate) namespace: OrdMap<ArtifactId, ArtifactBinding>,
    pub(crate) custody_roots: OrdMap<ArtifactBindingNode, ()>,
}

impl ExactSelectionSnapshot {
    pub(crate) fn from_graph_selection(graph: &ArtifactGraphSelection) -> Self {
        Self {
            scopes: graph
                .selections
                .iter()
                .map(|proof| {
                    (
                        proof.selection,
                        ImmutableScopeProofRef::from_issued(proof.clone()),
                    )
                })
                .collect(),
            roots: graph.roots.iter().map(|root| (*root, ())).collect(),
            namespace: graph
                .namespace
                .iter()
                .map(|binding| (binding.artifact, *binding))
                .collect(),
            custody_roots: graph
                .custody_roots
                .iter()
                .map(|binding| (*binding, ()))
                .collect(),
        }
    }

    pub(crate) fn retained_nodes(
        &self,
    ) -> Result<std::collections::BTreeSet<ArtifactBindingNode>, SnapshotError> {
        self.nodes_from_seeds(
            self.roots
                .keys()
                .copied()
                .chain(self.custody_roots.keys().copied()),
        )
    }

    pub(crate) fn nodes_from_seeds(
        &self,
        seeds: impl IntoIterator<Item = ArtifactBindingNode>,
    ) -> Result<std::collections::BTreeSet<ArtifactBindingNode>, SnapshotError> {
        let mut selected = std::collections::BTreeSet::new();
        let mut pending: Vec<_> = seeds.into_iter().collect();
        while let Some(node) = pending.pop() {
            if !selected.insert(node) {
                continue;
            }
            let proof = self
                .scopes
                .get(&node.selection())
                .ok_or_else(|| malformed("selected node lacks scope proof"))?
                .proof();
            if proof.nodes.binary_search(&node).is_err() {
                return Err(malformed("selected node outside scope proof"));
            }
            let start = proof
                .dependencies
                .partition_point(|(source, _, _)| *source < node);
            pending.extend(
                proof.dependencies[start..]
                    .iter()
                    .take_while(|(source, _, _)| *source == node)
                    .map(|(_, target, _)| *target),
            );
        }
        Ok(selected)
    }

    /// Candidate raw artifacts reached by newly selected roots or byte custody.
    /// The run owner removes IDs whose durable references it already retains.
    /// An unchanged snapshot does not traverse its inherited closure.
    pub fn artifact_ids_introduced(
        &self,
        previous: Option<&Self>,
    ) -> Result<std::collections::BTreeSet<ArtifactId>, SnapshotError> {
        let Some(previous) = previous else {
            return Ok(self
                .retained_nodes()?
                .into_iter()
                .map(ArtifactBindingNode::artifact)
                .collect());
        };
        let roots = previous
            .roots
            .diff(&self.roots)
            .filter_map(|change| match change {
                DiffItem::Add(root, _) => Some(*root),
                DiffItem::Update { new: (root, _), .. } => Some(*root),
                DiffItem::Remove(_, _) => None,
            });
        let custody = previous
            .custody_roots
            .diff(&self.custody_roots)
            .filter_map(|change| match change {
                DiffItem::Add(binding, _) => Some(*binding),
                DiffItem::Update {
                    new: (binding, _), ..
                } => Some(*binding),
                DiffItem::Remove(_, _) => None,
            });
        Ok(self
            .nodes_from_seeds(roots.chain(custody))?
            .into_iter()
            .map(ArtifactBindingNode::artifact)
            .collect())
    }

    /// Derive retained payload/group rows from exact typed roots and custody.
    /// Proof-only ancestors remain available without becoming payload authority.
    /// A selected graph packet carries complete atomic scope proofs and their
    /// ancestry, independently of wider availability retained by this snapshot.
    pub(crate) fn scope_proofs_for_nodes(
        &self,
        nodes: impl IntoIterator<Item = ArtifactBindingNode>,
    ) -> Result<Vec<Arc<ScopeProofV3>>, SnapshotError> {
        let mut required = std::collections::BTreeMap::new();
        let mut pending = nodes
            .into_iter()
            .map(|node| node.selection())
            .collect::<Vec<_>>();
        while let Some(scope) = pending.pop() {
            if required.contains_key(&scope) {
                continue;
            }
            let proof = self
                .scopes
                .get(&scope)
                .ok_or_else(|| malformed("selected binding scope proof missing"))?
                .proof();
            pending.extend(proof.scope_dependencies());
            required.insert(scope, Arc::clone(proof));
        }
        Ok(required.into_values().collect())
    }

    pub fn graph_selection(&self) -> Result<ArtifactGraphSelection, SnapshotError> {
        let selected = self.retained_nodes()?;
        if self
            .namespace
            .values()
            .any(|binding| !selected.contains(&ArtifactBindingNode::Artifact(*binding)))
        {
            return Err(malformed("namespace outside exact selected closure"));
        }
        Ok(ArtifactGraphSelection {
            selections: self.scope_proofs_for_nodes(selected.iter().copied())?,
            bindings: selected
                .iter()
                .filter_map(|node| match node {
                    ArtifactBindingNode::Artifact(binding) => Some(*binding),
                    _ => None,
                })
                .collect(),
            namespace: self.namespace.values().copied().collect(),
            roots: self.roots.keys().copied().collect(),
            native_groups: selected
                .iter()
                .filter_map(|node| match node {
                    ArtifactBindingNode::Group(group) => Some(*group),
                    _ => None,
                })
                .collect(),
            custody_roots: self.custody_roots.keys().copied().collect(),
        })
    }

    pub fn binding_dependencies(
        &self,
    ) -> Result<
        Vec<(
            ArtifactBindingNode,
            ArtifactBindingNode,
            crate::artifact_inventory::ArtifactDependency,
        )>,
        SnapshotError,
    > {
        let selected = self.retained_nodes()?;
        let mut edges = Vec::new();
        for source in &selected {
            let proof = self.scopes[&source.selection()].proof();
            let start = proof
                .dependencies
                .partition_point(|(node, _, _)| node < source);
            edges.extend(
                proof.dependencies[start..]
                    .iter()
                    .take_while(|(node, _, _)| node == source)
                    .cloned(),
            );
        }
        Ok(edges)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionDescriptor {
    version: u32,
    scopes: SnapshotMapRoot,
    roots: SnapshotMapRoot,
    namespace: SnapshotMapRoot,
    custody_roots: SnapshotMapRoot,
}

#[derive(Clone, Copy, Debug)]
pub struct ScopeProofCodec;
impl SnapshotValueCodec<ImmutableScopeProofRef> for ScopeProofCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        value: &ImmutableScopeProofRef,
    ) -> Result<Cid, SnapshotError> {
        store.materialize_scope(value)
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<ImmutableScopeProofRef, SnapshotError> {
        if let Some(proof) = store.scopes.borrow().get(&cid) {
            return Ok(proof.clone());
        }
        let bytes = store
            .get(&cid)
            .map_err(storage_error)?
            .ok_or(SnapshotError::MissingBlock(cid))?;
        verify_block(&cid, &bytes)?;
        let proof: ScopeProofV3 = fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)?;
        // The inventory authenticates complete scope identities and ancestry
        // before these decoded facts become live compiler custody.
        let proof = ImmutableScopeProofRef::from_issued(Arc::new(proof));
        let _ = proof.0.encoded.set((cid, bytes.into()));
        store.scopes.borrow_mut().insert(cid, proof.clone());
        Ok(proof)
    }
}

#[derive(Clone, Debug)]
pub struct MaterializedSelection {
    root: SelectionRoot,
    snapshot: Arc<ExactSelectionSnapshot>,
    scopes: MaterializedMap<ArtifactSelectionId, ImmutableScopeProofRef, ScopeProofCodec>,
    roots: MaterializedMap<ArtifactBindingNode, ()>,
    namespace: MaterializedMap<ArtifactId, ArtifactBinding>,
    custody_roots: MaterializedMap<ArtifactBindingNode, ()>,
}
impl MaterializedSelection {
    pub fn root(&self) -> &SelectionRoot {
        &self.root
    }
    pub fn snapshot(&self) -> &Arc<ExactSelectionSnapshot> {
        &self.snapshot
    }
}
impl<BS: Blockstore> Materializer<'_, BS> {
    fn materialize_scope(&mut self, value: &ImmutableScopeProofRef) -> Result<Cid, SnapshotError> {
        let selection = value.proof().selection;
        let identity = Arc::as_ptr(&value.0) as usize;
        if let Some(receipt) = self.scopes.get_mut(&selection) {
            if receipt.owners.contains_key(&identity) {
                return Ok(receipt.cid);
            }
            // A different incoming owner must establish its own exact bytes.
            // A claimed scope identity alone cannot borrow another proof.
            let (cid, bytes) = value.encoded()?;
            add_bytes(&self.cid_validation_bytes, bytes.len());
            verify_block(cid, bytes)?;
            let original = receipt.owners.values().next().expect("issued scope owner");
            let (_, original_bytes) = original.encoded()?;
            if *cid != receipt.cid
                || value.proof() != original.proof()
                || bytes.as_ref() != original_bytes.as_ref()
            {
                return Err(malformed("conflicting immutable scope proof"));
            }
            receipt.owners.insert(identity, value.clone());
            return Ok(receipt.cid);
        }
        let (cid, bytes) = value.encoded()?;
        self.put_keyed(cid, bytes).map_err(storage_error)?;
        self.scopes.insert(
            selection,
            MaterializedScope {
                cid: *cid,
                owners: [(identity, value.clone())].into(),
            },
        );
        Ok(*cid)
    }

    pub(crate) fn materialize_selection(
        &mut self,
        base: Option<&MaterializedSelection>,
        next: &Arc<ExactSelectionSnapshot>,
    ) -> Result<MaterializedSelection, SnapshotError> {
        let identity = Arc::as_ptr(next) as usize;
        if let Some(receipt) = self.selections.get(&identity) {
            return Ok(receipt.clone());
        }
        let scopes = self
            .materialize_map_with::<_, _, ScopeProofCodec>(base.map(|b| &b.scopes), &next.scopes)?;
        let roots = self.materialize_map(base.map(|b| &b.roots), &next.roots)?;
        let namespace = self.materialize_map(base.map(|b| &b.namespace), &next.namespace)?;
        let custody_roots =
            self.materialize_map(base.map(|b| &b.custody_roots), &next.custody_roots)?;
        let descriptor = SelectionDescriptor {
            version: 2,
            scopes: scopes.root.clone(),
            roots: roots.root.clone(),
            namespace: namespace.root.clone(),
            custody_roots: custody_roots.root.clone(),
        };
        let root = if base.is_some_and(|base| {
            base.scopes.root == scopes.root
                && base.roots.root == roots.root
                && base.namespace.root == namespace.root
                && base.custody_roots.root == custody_roots.root
        }) {
            base.expect("equal descriptor implies base").root.clone()
        } else {
            SelectionRoot(self.materialize_value(&descriptor)?)
        };
        let receipt = MaterializedSelection {
            root,
            snapshot: Arc::clone(next),
            scopes,
            roots,
            namespace,
            custody_roots,
        };
        // Retaining the original immutable snapshot prevents allocation-address
        // reuse. This lookup table lasts only for this materialization operation.
        self.selections.insert(identity, receipt.clone());
        Ok(receipt)
    }
}
pub fn hydrate_selection<BS: Blockstore>(
    store: &BS,
    root: &SelectionRoot,
) -> Result<MaterializedSelection, SnapshotError> {
    SnapshotHydrator::new(store).selection(root)
}
fn hydrate_selection_owned<BS: Blockstore>(
    store: &SnapshotHydrator<'_, BS>,
    root: &SelectionRoot,
) -> Result<MaterializedSelection, SnapshotError> {
    let descriptor: SelectionDescriptor = read_value(store, root.cid())?;
    if descriptor.version != 2 {
        return Err(malformed("selection encoding version"));
    }
    let scopes: MaterializedMap<ArtifactSelectionId, ImmutableScopeProofRef, ScopeProofCodec> =
        hydrate_map_owned::<_, _, ScopeProofCodec, _>(store, &descriptor.scopes)?;
    if scopes
        .facts
        .iter()
        .any(|(id, proof)| *id != proof.proof().selection)
    {
        return Err(malformed("scope fact has another binding identity"));
    }
    let roots = hydrate_map_owned::<_, _, InlineValues, _>(store, &descriptor.roots)?;
    let namespace = hydrate_map_owned::<_, _, InlineValues, _>(store, &descriptor.namespace)?;
    let custody_roots =
        hydrate_map_owned::<_, _, InlineValues, _>(store, &descriptor.custody_roots)?;
    let snapshot = Arc::new(ExactSelectionSnapshot {
        scopes: scopes.facts.clone(),
        roots: roots.facts.clone(),
        namespace: namespace.facts.clone(),
        custody_roots: custody_roots.facts.clone(),
    });
    snapshot.graph_selection()?;
    Ok(MaterializedSelection {
        root: root.clone(),
        snapshot,
        scopes,
        roots,
        namespace,
        custody_roots,
    })
}

/// Canonical immutable lexical facts share resident nodes and their encoded bytes.
#[derive(Clone, Debug)]
pub struct ImmutableLexicalFactRef {
    cid: Cid,
    node: Arc<ExactLexicalNode>,
    bytes: Arc<[u8]>,
}
impl PartialEq for ImmutableLexicalFactRef {
    fn eq(&self, other: &Self) -> bool {
        self.cid == other.cid
    }
}
impl Eq for ImmutableLexicalFactRef {}
impl ImmutableLexicalFactRef {
    fn new(mut node: ExactLexicalNode) -> Result<Self, SnapshotError> {
        node.imports.sort();
        if node.imports.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(malformed("duplicate selected lexical import"));
        }
        let bytes: Arc<[u8]> = fvm_ipld_encoding::to_vec(&node).map_err(malformed)?.into();
        let cid = Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&bytes));
        Ok(Self {
            cid,
            node: Arc::new(node),
            bytes,
        })
    }
}

#[cfg(test)]
thread_local! { static LEXICAL_APPEND_WORK: Cell<(usize, usize)> = const { Cell::new((0, 0)) }; }

#[derive(Clone, Debug, Default)]
pub(crate) struct ExactLexicalSnapshot {
    facts: OrdMap<ExactModuleIdentity, ImmutableLexicalFactRef>,
    rows: Arc<OnceLock<Vec<ExactLexicalNode>>>,
}
impl PartialEq for ExactLexicalSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.facts == other.facts
    }
}
impl Eq for ExactLexicalSnapshot {}
impl ExactLexicalSnapshot {
    pub(crate) fn append_issued(
        &self,
        nodes: impl IntoIterator<Item = ExactLexicalNode>,
    ) -> Result<Self, crate::CompileError> {
        let mut facts = self.facts.clone();
        let mut changed = false;
        for mut node in nodes {
            #[cfg(test)]
            LEXICAL_APPEND_WORK.with(|work| {
                let (rows, issued) = work.get();
                work.set((rows + 1, issued));
            });
            node.imports.sort();
            node.imports.dedup();
            if let Some(previous) = facts.get(&node.owner) {
                if previous.node.as_ref() != &node {
                    return Err(crate::CompileError::ExtractFailed(
                        "program support changed its selected source graph".into(),
                    ));
                }
                continue;
            }
            let owner = node.owner.clone();
            let fact = ImmutableLexicalFactRef::new(node)
                .map_err(|error| crate::CompileError::ExtractFailed(error.to_string()))?;
            #[cfg(test)]
            LEXICAL_APPEND_WORK.with(|work| {
                let (rows, issued) = work.get();
                work.set((rows, issued + 1));
            });
            facts.insert(owner, fact);
            changed = true;
        }
        Ok(if changed {
            Self {
                facts,
                rows: Arc::new(OnceLock::new()),
            }
        } else {
            self.clone()
        })
    }

    pub(crate) fn get(&self, owner: &ExactModuleIdentity) -> Option<&ExactLexicalNode> {
        self.facts.get(owner).map(|fact| fact.node.as_ref())
    }

    pub(crate) fn replace(
        &self,
        nodes: Vec<ExactLexicalNode>,
    ) -> Result<Self, crate::CompileError> {
        let mut facts = self.facts.clone();
        let mut selected = std::collections::BTreeSet::new();
        for mut node in nodes {
            if !selected.insert(node.owner.clone()) {
                return Err(crate::CompileError::ExtractFailed(format!(
                    "duplicate selected lexical owner {}:{}",
                    node.owner.unit, node.owner.module
                )));
            }
            node.imports.sort();
            if facts
                .get(&node.owner)
                .is_some_and(|previous| previous.node.as_ref() == &node)
            {
                continue;
            }
            let owner = node.owner.clone();
            let fact = ImmutableLexicalFactRef::new(node)
                .map_err(|error| crate::CompileError::ExtractFailed(error.to_string()))?;
            facts.insert(owner, fact);
        }
        for owner in self.facts.keys().filter(|owner| !selected.contains(*owner)) {
            facts.remove(owner);
        }
        Ok(Self {
            facts,
            rows: Arc::new(OnceLock::new()),
        })
    }
}
impl std::ops::Deref for ExactLexicalSnapshot {
    type Target = [ExactLexicalNode];
    fn deref(&self) -> &Self::Target {
        self.rows.get_or_init(|| {
            self.facts
                .values()
                .map(|fact| fact.node.as_ref().clone())
                .collect()
        })
    }
}
impl<'a> IntoIterator for &'a ExactLexicalSnapshot {
    type Item = &'a ExactLexicalNode;
    type IntoIter = std::slice::Iter<'a, ExactLexicalNode>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LexicalFactCodec;
impl SnapshotValueCodec<ImmutableLexicalFactRef> for LexicalFactCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        fact: &ImmutableLexicalFactRef,
    ) -> Result<Cid, SnapshotError> {
        store
            .put_keyed(&fact.cid, &fact.bytes)
            .map_err(storage_error)?;
        Ok(fact.cid)
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<ImmutableLexicalFactRef, SnapshotError> {
        let bytes = store
            .get(&cid)
            .map_err(storage_error)?
            .ok_or(SnapshotError::MissingBlock(cid))?;
        verify_block(&cid, &bytes)?;
        let node: ExactLexicalNode = fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)?;
        if node.imports.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(malformed("noncanonical lexical adjacency"));
        }
        Ok(ImmutableLexicalFactRef {
            cid,
            node: Arc::new(node),
            bytes: bytes.into(),
        })
    }
}

/// Availability is an issuance fact independent of compiler roles and execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum NativeOfferSnapshotBody {
    Empty(crate::artifact_inventory::ExactArtifactSelection),
    Partial(crate::artifact_inventory::ExactArtifactSelection),
    Full(crate::artifact_inventory::ExactArtifactSelection),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeOfferSnapshotFacts {
    pub(crate) original: ArtifactId,
    pub(crate) interface: ArtifactId,
    pub(crate) owner: ExactModuleIdentity,
    pub(crate) body: NativeOfferSnapshotBody,
}
impl NativeOfferSnapshotFacts {
    pub(crate) fn identity(&self) -> Result<[u8; 32], crate::CompileError> {
        use sha2::{Digest, Sha256};
        let (mode, selection) = match &self.body {
            NativeOfferSnapshotBody::Empty(custody) => (0u8, Some(custody.observation_digest()?)),
            NativeOfferSnapshotBody::Partial(selection) => {
                (1u8, Some(selection.observation_digest()?))
            }
            NativeOfferSnapshotBody::Full(selection) => {
                (2u8, Some(selection.observation_digest()?))
            }
        };
        let bytes = fvm_ipld_encoding::to_vec(&(
            self.original,
            self.interface,
            &self.owner,
            mode,
            selection,
        ))
        .map_err(|error| {
            crate::CompileError::ExtractFailed(format!("native offer identity: {error}"))
        })?;
        let mut digest = Sha256::new();
        digest.update(b"Tidepool native offer fact v1\0");
        digest.update(bytes);
        Ok(digest.finalize().into())
    }
    pub(crate) fn restore(
        &self,
        source: &crate::artifact_inventory::ArtifactView,
    ) -> Result<crate::artifact_inventory::IssuedNativeOffer, crate::CompileError> {
        crate::artifact_inventory::IssuedNativeOffer::restore_snapshot(self, source)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NativeOfferBodyWire {
    Empty { custody: SelectionRoot },
    Partial { selection: SelectionRoot },
    Full { selection: SelectionRoot },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOfferWire {
    original: ArtifactId,
    interface: ArtifactId,
    owner: ExactModuleIdentity,
    body: NativeOfferBodyWire,
}
#[derive(Clone, Copy, Debug)]
struct NativeOfferCodec;
impl SnapshotValueCodec<NativeOfferSnapshotFacts> for NativeOfferCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        facts: &NativeOfferSnapshotFacts,
    ) -> Result<Cid, SnapshotError> {
        let body = match &facts.body {
            NativeOfferSnapshotBody::Empty(selection)
            | NativeOfferSnapshotBody::Partial(selection)
            | NativeOfferSnapshotBody::Full(selection) => {
                let base = store.selection_base.clone();
                let selection = store
                    .materialize_selection(base.as_ref(), selection.snapshot())?
                    .root;
                match &facts.body {
                    NativeOfferSnapshotBody::Empty(_) => {
                        NativeOfferBodyWire::Empty { custody: selection }
                    }
                    NativeOfferSnapshotBody::Partial(_) => {
                        NativeOfferBodyWire::Partial { selection }
                    }
                    _ => NativeOfferBodyWire::Full { selection },
                }
            }
        };
        store.materialize_value(&NativeOfferWire {
            original: facts.original,
            interface: facts.interface,
            owner: facts.owner.clone(),
            body,
        })
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<NativeOfferSnapshotFacts, SnapshotError> {
        let wire: NativeOfferWire = read_value(store, &cid)?;
        let body = match wire.body {
            NativeOfferBodyWire::Empty { custody } => NativeOfferSnapshotBody::Empty(
                crate::artifact_inventory::ExactArtifactSelection::from_snapshot(
                    store.selection(&custody)?.snapshot,
                ),
            ),
            NativeOfferBodyWire::Partial { selection } => NativeOfferSnapshotBody::Partial(
                crate::artifact_inventory::ExactArtifactSelection::from_snapshot(
                    store.selection(&selection)?.snapshot,
                ),
            ),
            NativeOfferBodyWire::Full { selection } => NativeOfferSnapshotBody::Full(
                crate::artifact_inventory::ExactArtifactSelection::from_snapshot(
                    store.selection(&selection)?.snapshot,
                ),
            ),
        };
        Ok(NativeOfferSnapshotFacts {
            original: wire.original,
            interface: wire.interface,
            owner: wire.owner,
            body,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeContributionWire {
    offer: [u8; 32],
    reachable: SnapshotMapRoot,
}

#[derive(Clone, Copy, Debug)]
struct NativeContributionCodec;
impl SnapshotValueCodec<NativeContributionSnapshotFacts> for NativeContributionCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        facts: &NativeContributionSnapshotFacts,
    ) -> Result<Cid, SnapshotError> {
        let reachable = facts
            .reachable
            .iter()
            .copied()
            .map(|node| (node, ()))
            .collect();
        let reachable = store.materialize_map::<ArtifactBindingNode, ()>(None, &reachable)?;
        store.materialize_value(&NativeContributionWire {
            offer: facts.offer,
            reachable: reachable.root,
        })
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<NativeContributionSnapshotFacts, SnapshotError> {
        let wire: NativeContributionWire = read_value(store, &cid)?;
        let reachable =
            hydrate_map_owned::<ArtifactBindingNode, (), InlineValues, _>(store, &wire.reachable)?;
        Ok(NativeContributionSnapshotFacts {
            offer: wire.offer,
            reachable: reachable.facts.keys().copied().collect(),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NativeAvailabilityModeWire {
    Full,
    IssuedGroups,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeAvailabilityOwnerWire {
    unit: String,
    module: String,
    module_version: [u8; 32],
    skinny_iface_sha256: [u8; 32],
    product_sha256: [u8; 32],
}

pub(crate) mod serde_cached_home_owner {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        NativeAvailabilityOwnerWire {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: owner.module_version.0,
            skinny_iface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
        }
        .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<tidepool_repr::execution_schema::CachedHomeOwner, D::Error> {
        let owner = NativeAvailabilityOwnerWire::deserialize(deserializer)?;
        Ok(tidepool_repr::execution_schema::CachedHomeOwner {
            unit: owner.unit,
            module: owner.module,
            module_version: tidepool_repr::execution_schema::ModuleVersion(owner.module_version),
            skinny_iface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeAvailabilityWire {
    original: ArtifactId,
    owner: NativeAvailabilityOwnerWire,
    mode: NativeAvailabilityModeWire,
    contributions: SnapshotMapRoot,
}

#[derive(Clone, Copy, Debug)]
struct NativeAvailabilityCodec;
impl SnapshotValueCodec<NativeAvailabilitySnapshotFacts> for NativeAvailabilityCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        facts: &NativeAvailabilitySnapshotFacts,
    ) -> Result<Cid, SnapshotError> {
        let prior = store.native_availability_base.as_ref().and_then(|base| {
            base.facts
                .get(&facts.original)
                .map(|fact| (base.root.clone(), fact.contributions.clone()))
        });
        let prior = if let Some((root, contributions)) = prior {
            let cid = lookup_map::<ArtifactId, Cid, _>(&*store, &root, &facts.original)?
                .ok_or_else(|| malformed("native availability base omits its row"))?;
            let wire: NativeAvailabilityWire = read_value(&SnapshotHydrator::new(&*store), &cid)?;
            Some(MaterializedMap::<_, _, NativeContributionCodec> {
                root: wire.contributions,
                facts: contributions,
                codec: PhantomData,
            })
        } else {
            None
        };
        let contributions = store.materialize_map_with::<_, _, NativeContributionCodec>(
            prior.as_ref(),
            &facts.contributions,
        )?;
        store.materialize_value(&NativeAvailabilityWire {
            original: facts.original,
            owner: NativeAvailabilityOwnerWire {
                unit: facts.owner.unit.clone(),
                module: facts.owner.module.clone(),
                module_version: facts.owner.module_version.0,
                skinny_iface_sha256: facts.owner.skinny_iface_sha256,
                product_sha256: facts.owner.product_sha256,
            },
            mode: match &facts.mode {
                NativeTransportMode::Full => NativeAvailabilityModeWire::Full,
                NativeTransportMode::IssuedGroups => NativeAvailabilityModeWire::IssuedGroups,
            },
            contributions: contributions.root,
        })
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<NativeAvailabilitySnapshotFacts, SnapshotError> {
        let wire: NativeAvailabilityWire = read_value(store, &cid)?;
        let contributions =
            hydrate_map_owned::<_, _, NativeContributionCodec, _>(store, &wire.contributions)?;
        for (identity, fact) in &contributions.facts {
            if *identity != fact.identity().map_err(malformed)? {
                return Err(malformed(
                    "native contribution key differs from fact identity",
                ));
            }
        }
        Ok(NativeAvailabilitySnapshotFacts {
            original: wire.original,
            owner: tidepool_repr::execution_schema::CachedHomeOwner {
                unit: wire.owner.unit,
                module: wire.owner.module,
                module_version: tidepool_repr::execution_schema::ModuleVersion(
                    wire.owner.module_version,
                ),
                skinny_iface_sha256: wire.owner.skinny_iface_sha256,
                product_sha256: wire.owner.product_sha256,
            },
            mode: match wire.mode {
                NativeAvailabilityModeWire::Full => NativeTransportMode::Full,
                NativeAvailabilityModeWire::IssuedGroups => NativeTransportMode::IssuedGroups,
            },
            contributions: contributions.facts,
        })
    }
}

/// Already-maintained typed maps and their compact semantic root. Neither
/// decoding nor owning immutable bytes acquires native execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationNativeMaps {
    #[serde(with = "serde_ord_map")]
    pub(crate) roles: OrdMap<ExactModuleIdentity, crate::artifact_inventory::RecordedCompilerRole>,
    #[serde(with = "serde_ord_map")]
    pub(crate) offers: OrdMap<[u8; 32], NativeOfferSnapshotFacts>,
    #[serde(with = "serde_ord_map")]
    pub(crate) native_availability: OrdMap<ArtifactId, NativeAvailabilitySnapshotFacts>,
    #[serde(with = "serde_ord_map")]
    pub(crate) input_decisions:
        OrdMap<ExactModuleIdentity, crate::artifact_inventory::RecordedInputNativeDecision>,
}
pub(crate) struct PublicationNativeFacts {
    maps: PublicationNativeMaps,
    receipt: NativeFactReceipt,
}
impl std::ops::Deref for PublicationNativeFacts {
    type Target = PublicationNativeMaps;
    fn deref(&self) -> &Self::Target {
        &self.maps
    }
}
impl std::fmt::Debug for PublicationNativeFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicationNativeFacts")
            .field("root", &self.receipt.root)
            .finish()
    }
}
impl PartialEq for PublicationNativeFacts {
    fn eq(&self, other: &Self) -> bool {
        self.receipt.root == other.receipt.root
    }
}
impl Eq for PublicationNativeFacts {}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationNativeTransport {
    root: Cid,
    maps: PublicationNativeMaps,
}
impl Serialize for PublicationNativeFacts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut wire = serializer.serialize_struct("PublicationNativeFacts", 2)?;
        wire.serialize_field("root", &self.receipt.root)?;
        wire.serialize_field("maps", &self.maps)?;
        wire.end()
    }
}
impl<'de> Deserialize<'de> for PublicationNativeFacts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = PublicationNativeTransport::deserialize(deserializer)?;
        let facts = Self::maintain(None, wire.maps).map_err(serde::de::Error::custom)?;
        if facts.receipt.root != wire.root {
            return Err(serde::de::Error::custom(
                "publication native commitment differs",
            ));
        }
        Arc::try_unwrap(facts)
            .map_err(|_| serde::de::Error::custom("decoded publication facts unexpectedly shared"))
    }
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeFactWork {
    pub(crate) changed_entries: u64,
    pub(crate) serialized_bytes: u64,
    pub(crate) offered_bytes: u64,
    pub(crate) cid_validation_bytes: u64,
    pub(crate) link_visits: u64,
    pub(crate) unlink_visits: u64,
}
#[cfg(test)]
thread_local! { static NATIVE_FACT_WORK:Cell<NativeFactWork>=Cell::new(NativeFactWork::default()); }
#[cfg(test)]
pub(crate) fn take_native_fact_work() -> NativeFactWork {
    NATIVE_FACT_WORK.with(|work| work.replace(NativeFactWork::default()))
}

/// Key selection is passive data, not a replacement permission/role value.
/// The actual consumer must refuse changed permission rows before selecting keys.
#[derive(Default)]
pub(crate) struct PublicationNativeMapKeys {
    pub(crate) roles: std::collections::BTreeSet<ExactModuleIdentity>,
    pub(crate) offers: std::collections::BTreeSet<[u8; 32]>,
    pub(crate) native_availability: std::collections::BTreeSet<ArtifactId>,
    pub(crate) input_decisions: std::collections::BTreeSet<ExactModuleIdentity>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeFactProjectionWork {
    pub(crate) maps: MapProjectionWork,
    pub(crate) frontier: FrontierProjectionWork,
    pub(crate) materialization: MaterializationMetrics,
}

impl PublicationNativeFacts {
    /// Select exact cold facts and their own encoded wires from the same owner.
    /// Changed permission/role facts require their issuing consumer's separate
    /// refusal/encoding path and cannot be supplied to this projection.
    pub(crate) fn project(
        self: &Arc<Self>,
        keys: PublicationNativeMapKeys,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<(Arc<Self>, NativeFactProjectionWork), SnapshotProjectionError> {
        fn role_strings(
            role: &crate::artifact_inventory::RecordedCompilerRole,
            operation: &tidepool_repr::execution_schema::InventoryOperation,
        ) -> Result<(), SnapshotProjectionError> {
            if let crate::artifact_inventory::RecordedCompilerRole::PublishedSourceOriginal {
                source_revision,
                original_input_identity,
                ..
            } = role
            {
                operation.charge(source_revision.len())?;
                operation.charge(original_input_identity.len())?;
            }
            Ok(())
        }
        fn owner_strings(
            owner: &ExactModuleIdentity,
            operation: &tidepool_repr::execution_schema::InventoryOperation,
        ) -> Result<(), SnapshotProjectionError> {
            operation.charge(owner.unit.len())?;
            operation.charge(owner.module.len())?;
            Ok(())
        }
        let mut maps = PublicationNativeMaps {
            roles: OrdMap::new(),
            offers: OrdMap::new(),
            native_availability: OrdMap::new(),
            input_decisions: OrdMap::new(),
        };
        // Only these concrete immutable fact types are copied: their variable
        // owned strings are charged first; proof and persistent maps share roots.
        for key in &keys.roles {
            crate::host_work::checkpoint()?;
            owner_strings(key, operation)?;
            let value = self
                .receipt
                .roles
                .facts
                .get(key)
                .ok_or(SnapshotProjectionError::MissingFact)?;
            operation.reserve::<(
                ExactModuleIdentity,
                crate::artifact_inventory::RecordedCompilerRole,
            )>(4)?;
            role_strings(value, operation)?;
            maps.roles.insert(key.clone(), value.clone());
        }
        for key in &keys.offers {
            crate::host_work::checkpoint()?;
            let value = self
                .receipt
                .offers
                .facts
                .get(key)
                .ok_or(SnapshotProjectionError::MissingFact)?;
            operation.reserve::<([u8; 32], NativeOfferSnapshotFacts)>(4)?;
            owner_strings(&value.owner, operation)?;
            maps.offers.insert(*key, value.clone());
        }
        for key in &keys.native_availability {
            crate::host_work::checkpoint()?;
            let value = self
                .receipt
                .native_availability
                .facts
                .get(key)
                .ok_or(SnapshotProjectionError::MissingFact)?;
            operation.reserve::<(ArtifactId, NativeAvailabilitySnapshotFacts)>(4)?;
            operation.charge(value.owner.unit.len())?;
            operation.charge(value.owner.module.len())?;
            maps.native_availability.insert(*key, value.clone());
        }
        for key in &keys.input_decisions {
            crate::host_work::checkpoint()?;
            owner_strings(key, operation)?;
            let value = self
                .receipt
                .input_decisions
                .facts
                .get(key)
                .ok_or(SnapshotProjectionError::MissingFact)?;
            operation.reserve::<(
                ExactModuleIdentity,
                crate::artifact_inventory::RecordedInputNativeDecision,
            )>(4)?;
            role_strings(&value.role, operation)?;
            maps.input_decisions.insert(key.clone(), value.clone());
        }
        let mut writer = Materializer::new(&self.receipt.frontier);
        let (root, rw) = writer.project_map_with(&self.receipt.roles, &keys.roles, operation)?;
        let roles = MaterializedMap {
            root,
            facts: maps.roles.clone(),
            codec: PhantomData,
        };
        let (root, ow) = writer.project_map_with(&self.receipt.offers, &keys.offers, operation)?;
        let offers = MaterializedMap {
            root,
            facts: maps.offers.clone(),
            codec: PhantomData,
        };
        let (root, nw) = writer.project_map_with(
            &self.receipt.native_availability,
            &keys.native_availability,
            operation,
        )?;
        let native_availability = MaterializedMap {
            root,
            facts: maps.native_availability.clone(),
            codec: PhantomData,
        };
        let (root, iw) = writer.project_map_with(
            &self.receipt.input_decisions,
            &keys.input_decisions,
            operation,
        )?;
        let input_decisions = MaterializedMap {
            root,
            facts: maps.input_decisions.clone(),
            codec: PhantomData,
        };
        let descriptor = PublicationNativeDescriptor {
            version: 2,
            roles: roles.root.clone(),
            offers: offers.root.clone(),
            native_availability: native_availability.root.clone(),
            input_decisions: input_decisions.root.clone(),
        };
        projection_wire_bound(&descriptor, operation)?;
        let root = writer.materialize_value(&descriptor)?;
        let staged = writer.finish()?;
        let materialization = staged.metrics();
        operation.reserve::<Cid>(1)?;
        let (frontier, fw) = self
            .receipt
            .frontier
            .project_roots(&staged, vec![root], operation)?;
        let work = NativeFactProjectionWork {
            maps: MapProjectionWork {
                key_lookups: rw.key_lookups + ow.key_lookups + nw.key_lookups + iw.key_lookups,
                reused_values: rw.reused_values
                    + ow.reused_values
                    + nw.reused_values
                    + iw.reused_values,
                route_blocks: rw.route_blocks + ow.route_blocks + nw.route_blocks + iw.route_blocks,
                route_bytes: rw.route_bytes + ow.route_bytes + nw.route_bytes + iw.route_bytes,
            },
            frontier: fw,
            materialization,
        };
        Ok((
            Arc::new(Self {
                maps,
                receipt: NativeFactReceipt {
                    root,
                    roles,
                    offers,
                    native_availability,
                    input_decisions,
                    frontier,
                },
            }),
            work,
        ))
    }

    pub(crate) fn commitment(&self) -> Cid {
        self.receipt.root
    }
    pub(crate) fn maintain(
        prior: Option<&Arc<Self>>,
        maps: PublicationNativeMaps,
    ) -> Result<Arc<Self>, SnapshotError> {
        if let Some(prior) = prior {
            if prior.roles.diff(&maps.roles).next().is_none()
                && prior.offers.diff(&maps.offers).next().is_none()
                && prior
                    .native_availability
                    .diff(&maps.native_availability)
                    .next()
                    .is_none()
                && prior
                    .input_decisions
                    .diff(&maps.input_decisions)
                    .next()
                    .is_none()
            {
                return Ok(Arc::clone(prior));
            }
        }
        let empty = ReachableBlocks::default();
        let frontier = prior.map_or(&empty, |p| &p.receipt.frontier);
        let mut writer = Materializer::new(frontier);
        writer.publication_native_base = prior.cloned();
        writer.native_availability_base = prior.map(|p| p.receipt.native_availability.clone());
        let roles = writer.materialize_map_with::<_, _, CompilerRoleCodec>(
            prior.map(|p| &p.receipt.roles),
            &maps.roles,
        )?;
        let offers = writer.materialize_map_with::<_, _, NativeOfferCodec>(
            prior.map(|p| &p.receipt.offers),
            &maps.offers,
        )?;
        let native_availability = writer.materialize_map_with::<_, _, NativeAvailabilityCodec>(
            prior.map(|p| &p.receipt.native_availability),
            &maps.native_availability,
        )?;
        let input_decisions = writer.materialize_map_with::<_, _, InputNativeDecisionCodec>(
            prior.map(|p| &p.receipt.input_decisions),
            &maps.input_decisions,
        )?;
        let root = writer.materialize_value(&PublicationNativeDescriptor {
            version: 2,
            roles: roles.root.clone(),
            offers: offers.root.clone(),
            native_availability: native_availability.root.clone(),
            input_decisions: input_decisions.root.clone(),
        })?;
        let staged = writer.finish()?;
        let frontier = frontier.replace_roots(&staged, vec![root])?;
        let metrics = staged.metrics();
        let work = frontier.work();
        tracing::debug!(target:"exomonad_harness::timing", changed_entries=metrics.changed_entries,serialized_bytes=metrics.serialized_bytes,offered_bytes=metrics.offered_bytes,cid_validation_bytes=metrics.cid_validation_bytes,link_visits=work.link_visits,unlink_visits=work.unlink_visits,"native fact root maintained");
        #[cfg(test)]
        NATIVE_FACT_WORK.with(|counter| {
            let mut total = counter.get();
            total.changed_entries += metrics.changed_entries;
            total.serialized_bytes += metrics.serialized_bytes;
            total.offered_bytes += metrics.offered_bytes;
            total.cid_validation_bytes += metrics.cid_validation_bytes;
            total.link_visits += work.link_visits;
            total.unlink_visits += work.unlink_visits;
            counter.set(total);
        });
        Ok(Arc::new(Self {
            maps,
            receipt: NativeFactReceipt {
                root,
                roles,
                offers,
                native_availability,
                input_decisions,
                frontier,
            },
        }))
    }
}

/// Exact compiler facts. Source-instance authority remains with the live
/// declaration/transaction owner and is deliberately absent from this snapshot.
#[derive(Clone, Debug, Default)]
pub struct ExactContextSnapshot {
    pub(crate) native_fact_owner: Option<Arc<PublicationNativeFacts>>,
    pub(crate) producer: [u8; 32],
    pub(crate) selection: Arc<ExactSelectionSnapshot>,
    pub(crate) roles: OrdMap<ExactModuleIdentity, crate::artifact_inventory::RecordedCompilerRole>,
    pub(crate) lexical: ExactLexicalSnapshot,
    pub(crate) offers: OrdMap<[u8; 32], NativeOfferSnapshotFacts>,
    pub(crate) native_availability: OrdMap<ArtifactId, NativeAvailabilitySnapshotFacts>,
    pub(crate) input_decisions:
        OrdMap<ExactModuleIdentity, crate::artifact_inventory::RecordedInputNativeDecision>,
    pub(crate) native_completions: OrdMap<[u8; 32], Arc<NativeCompletionSnapshotFacts>>,
}
impl ExactContextSnapshot {
    pub(crate) fn native_offers(&self) -> impl Iterator<Item = &NativeOfferSnapshotFacts> {
        self.offers.values()
    }
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn producer_sha256(&self) -> [u8; 32] {
        self.producer
    }
    pub fn selection(&self) -> &Arc<ExactSelectionSnapshot> {
        &self.selection
    }
    pub fn graph_selection(&self) -> Result<ArtifactGraphSelection, SnapshotError> {
        self.selection.graph_selection()
    }
    pub fn binding_dependencies(
        &self,
    ) -> Result<
        Vec<(
            ArtifactBindingNode,
            ArtifactBindingNode,
            crate::artifact_inventory::ArtifactDependency,
        )>,
        SnapshotError,
    > {
        self.selection.binding_dependencies()
    }
    pub fn compiler_roles(&self) -> Vec<CompilerInputRole> {
        self.roles
            .values()
            .map(|role| role.to_recorded_role())
            .collect()
    }
    pub fn lexical(&self) -> Vec<ExactLexicalNode> {
        self.lexical.to_vec()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompilerRoleFact {
    InterfaceOnly {
        interface: ArtifactId,
    },
    ReusableOriginal {
        interface: ArtifactId,
        original: ArtifactId,
    },
    PublishedSourceOriginal {
        interface: ArtifactId,
        original: ArtifactId,
        source_revision: String,
        original_input_identity: String,
        selection: SelectionRoot,
        selection_sha256: [u8; 32],
        native_authority: Cid,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct CompilerRoleCodec;
impl SnapshotValueCodec<CompilerInputRole> for CompilerRoleCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        role: &CompilerInputRole,
    ) -> Result<Cid, SnapshotError> {
        let fact = match role {
            CompilerInputRole::InterfaceOnly { interface } => CompilerRoleFact::InterfaceOnly {
                interface: *interface,
            },
            CompilerInputRole::ReusableOriginal {
                interface,
                original,
            } => CompilerRoleFact::ReusableOriginal {
                interface: *interface,
                original: *original,
            },
            CompilerInputRole::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection,
                selection_sha256,
                native_authority,
            } => {
                let authority = store.materialize_publication_native(native_authority.facts())?;
                let base = store.selection_base.clone();
                let selection = store.materialize_selection(base.as_ref(), selection.snapshot())?;
                CompilerRoleFact::PublishedSourceOriginal {
                    interface: *interface,
                    original: *original,
                    source_revision: source_revision.clone(),
                    original_input_identity: original_input_identity.clone(),
                    selection: selection.root,
                    selection_sha256: *selection_sha256,
                    native_authority: authority,
                }
            }
        };
        store.materialize_value(&fact)
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<CompilerInputRole, SnapshotError> {
        Ok(match read_value::<CompilerRoleFact, _>(store, &cid)? {
            CompilerRoleFact::InterfaceOnly { interface } => {
                CompilerInputRole::InterfaceOnly { interface }
            }
            CompilerRoleFact::ReusableOriginal {
                interface,
                original,
            } => CompilerInputRole::ReusableOriginal {
                interface,
                original,
            },
            CompilerRoleFact::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection,
                selection_sha256,
                native_authority,
            } => {
                let native_authority = hydrate_publication_native(store, &native_authority)?;
                let selection = store.selection(&selection)?;
                CompilerInputRole::PublishedSourceOriginal {
                    interface,
                    original,
                    source_revision,
                    original_input_identity,
                    selection: crate::artifact_inventory::ExactArtifactSelection::from_snapshot(
                        selection.snapshot,
                    ),
                    selection_sha256,
                    native_authority:
                        crate::artifact_inventory::PublicationNativeAuthority::recorded(
                            native_authority,
                        ),
                }
            }
        })
    }
}

impl SnapshotValueCodec<crate::artifact_inventory::RecordedCompilerRole> for CompilerRoleCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        value: &crate::artifact_inventory::RecordedCompilerRole,
    ) -> Result<Cid, SnapshotError> {
        use crate::artifact_inventory::RecordedCompilerRole;
        let fact = match value {
            RecordedCompilerRole::InterfaceOnly { interface } => CompilerRoleFact::InterfaceOnly {
                interface: *interface,
            },
            RecordedCompilerRole::ReusableOriginal {
                interface,
                original,
            } => CompilerRoleFact::ReusableOriginal {
                interface: *interface,
                original: *original,
            },
            RecordedCompilerRole::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection,
                selection_sha256,
                native_authority,
            } => {
                let authority = store.materialize_publication_native(native_authority)?;
                let base = store.selection_base.clone();
                let selection = store.materialize_selection(base.as_ref(), selection.snapshot())?;
                CompilerRoleFact::PublishedSourceOriginal {
                    interface: *interface,
                    original: *original,
                    source_revision: source_revision.clone(),
                    original_input_identity: original_input_identity.clone(),
                    selection: selection.root,
                    selection_sha256: *selection_sha256,
                    native_authority: authority,
                }
            }
        };
        store.materialize_value(&fact)
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        wire: Cid,
    ) -> Result<crate::artifact_inventory::RecordedCompilerRole, SnapshotError> {
        <Self as SnapshotValueCodec<CompilerInputRole>>::decode(store, wire)
            .map(|role| role.recorded_fact())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputNativeDecisionWire {
    role: Cid,
    groups: Vec<crate::artifact_inventory::NativeGroupBinding>,
}

#[derive(Clone, Copy, Debug)]
struct InputNativeDecisionCodec;
impl SnapshotValueCodec<crate::artifact_inventory::RecordedInputNativeDecision>
    for InputNativeDecisionCodec
{
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        value: &crate::artifact_inventory::RecordedInputNativeDecision,
    ) -> Result<Cid, SnapshotError> {
        let role = CompilerRoleCodec::encode(store, &value.role)?;
        store.materialize_value(&InputNativeDecisionWire {
            role,
            groups: value.groups.values().copied().collect(),
        })
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<crate::artifact_inventory::RecordedInputNativeDecision, SnapshotError> {
        let wire: InputNativeDecisionWire = read_value(store, &cid)?;
        let mut groups = OrdMap::new();
        for group in wire.groups {
            if groups.insert(group.original_ordinal, group).is_some() {
                return Err(malformed("input decision repeats an ordinal"));
            }
        }
        let role: crate::artifact_inventory::RecordedCompilerRole =
            CompilerRoleCodec::decode(store, wire.role)?;
        let original = role
            .original()
            .ok_or_else(|| malformed("input decision has no original role"))?;
        if groups
            .values()
            .any(|group| group.binding.artifact() != original)
        {
            return Err(malformed("input decision group differs from its original"));
        }
        Ok(crate::artifact_inventory::RecordedInputNativeDecision { role, groups })
    }
}

#[derive(Clone, Copy, Debug)]
struct NativeCompletionCodec;
impl SnapshotValueCodec<Arc<NativeCompletionSnapshotFacts>> for NativeCompletionCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        facts: &Arc<NativeCompletionSnapshotFacts>,
    ) -> Result<Cid, SnapshotError> {
        if facts.version != 1 {
            return Err(malformed("native completion encoding requires version 1"));
        }
        store.materialize_value(facts.as_ref())
    }
    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<Arc<NativeCompletionSnapshotFacts>, SnapshotError> {
        let facts: NativeCompletionSnapshotFacts = read_value(store, &cid)?;
        if facts.version != 1 {
            return Err(malformed("native completion encoding requires version 1"));
        }
        Ok(Arc::new(facts))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationNativeDescriptor {
    version: u32,
    roles: SnapshotMapRoot,
    offers: SnapshotMapRoot,
    native_availability: SnapshotMapRoot,
    input_decisions: SnapshotMapRoot,
}
struct NativeFactReceipt {
    root: Cid,
    roles: MaterializedMap<
        ExactModuleIdentity,
        crate::artifact_inventory::RecordedCompilerRole,
        CompilerRoleCodec,
    >,
    offers: MaterializedMap<[u8; 32], NativeOfferSnapshotFacts, NativeOfferCodec>,
    native_availability:
        MaterializedMap<ArtifactId, NativeAvailabilitySnapshotFacts, NativeAvailabilityCodec>,
    input_decisions: MaterializedMap<
        ExactModuleIdentity,
        crate::artifact_inventory::RecordedInputNativeDecision,
        InputNativeDecisionCodec,
    >,
    frontier: ReachableBlocks,
}
impl<BS: Blockstore> Materializer<'_, BS> {
    fn materialize_publication_native(
        &mut self,
        facts: &Arc<PublicationNativeFacts>,
    ) -> Result<Cid, SnapshotError> {
        let key = Arc::as_ptr(facts) as usize;
        if let Some((_, cid)) = self.publication_native.get(&key) {
            return Ok(*cid);
        }
        let empty = OrdMap::new();
        let previous = self
            .publication_native_base
            .as_ref()
            .map_or(&empty, |base| &base.receipt.frontier.blocks);
        for change in previous.diff(&facts.receipt.frontier.blocks) {
            if let DiffItem::Add(cid, block)
            | DiffItem::Update {
                new: (cid, block), ..
            } = change
            {
                self.put_keyed(cid, &block.bytes).map_err(storage_error)?;
            }
        }
        // A context references the four map roots directly, so its native-domain
        // descriptor itself need not have been present in the base store.
        let descriptor = facts
            .receipt
            .frontier
            .blocks
            .get(&facts.receipt.root)
            .ok_or(SnapshotError::MissingBlock(facts.receipt.root))?;
        self.put_keyed(&facts.receipt.root, &descriptor.bytes)
            .map_err(storage_error)?;
        self.publication_native
            .insert(key, (Arc::clone(facts), facts.receipt.root));
        Ok(facts.receipt.root)
    }
}
fn hydrate_publication_native<BS: Blockstore>(
    store: &SnapshotHydrator<'_, BS>,
    cid: &Cid,
) -> Result<Arc<PublicationNativeFacts>, SnapshotError> {
    if let Some(facts) = store.publication_native.borrow().get(cid) {
        return Ok(Arc::clone(facts));
    }
    let descriptor: PublicationNativeDescriptor = read_value(store, cid)?;
    if descriptor.version != 2 {
        return Err(malformed(
            "publication native authority requires version 2 with born roles",
        ));
    }
    let roles = hydrate_map_owned::<_, _, CompilerRoleCodec, _>(store, &descriptor.roles)?;
    let offers = hydrate_map_owned::<_, _, NativeOfferCodec, _>(store, &descriptor.offers)?;
    let native_availability = hydrate_map_owned::<_, _, NativeAvailabilityCodec, _>(
        store,
        &descriptor.native_availability,
    )?;
    let input_decisions =
        hydrate_map_owned::<_, _, InputNativeDecisionCodec, _>(store, &descriptor.input_decisions)?;
    for (key, offer) in &offers.facts {
        if *key != offer.identity().map_err(malformed)? {
            return Err(malformed("publication native offer key differs"));
        }
    }
    for (key, row) in &native_availability.facts {
        if *key != row.original {
            return Err(malformed("publication native row key differs"));
        }
        if row
            .contributions
            .values()
            .any(|c| !offers.facts.contains_key(&c.offer))
        {
            return Err(malformed("publication native row references missing offer"));
        }
    }
    let facts = PublicationNativeFacts::maintain(
        None,
        PublicationNativeMaps {
            roles: roles.facts,
            offers: offers.facts,
            native_availability: native_availability.facts,
            input_decisions: input_decisions.facts,
        },
    )?;
    if facts.commitment() != *cid {
        return Err(malformed("publication native descriptor is not canonical"));
    }
    store
        .publication_native
        .borrow_mut()
        .insert(*cid, Arc::clone(&facts));
    Ok(facts)
}

const CONTEXT_ENCODING_VERSION: u32 = 7;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextDescriptor {
    version: u32,
    producer: [u8; 32],
    selection: SelectionRoot,
    roles: SnapshotMapRoot,
    lexical: SnapshotMapRoot,
    offers: SnapshotMapRoot,
    native_availability: SnapshotMapRoot,
    input_decisions: SnapshotMapRoot,
    native_completions: SnapshotMapRoot,
}
#[derive(Clone, Debug)]
pub struct MaterializedContext {
    root: ContextRoot,
    snapshot: ExactContextSnapshot,
    selection: MaterializedSelection,
    roles: MaterializedMap<
        ExactModuleIdentity,
        crate::artifact_inventory::RecordedCompilerRole,
        CompilerRoleCodec,
    >,
    lexical: MaterializedMap<ExactModuleIdentity, ImmutableLexicalFactRef, LexicalFactCodec>,
    offers: MaterializedMap<[u8; 32], NativeOfferSnapshotFacts, NativeOfferCodec>,
    native_availability:
        MaterializedMap<ArtifactId, NativeAvailabilitySnapshotFacts, NativeAvailabilityCodec>,
    input_decisions: MaterializedMap<
        ExactModuleIdentity,
        crate::artifact_inventory::RecordedInputNativeDecision,
        InputNativeDecisionCodec,
    >,
    native_completions:
        MaterializedMap<[u8; 32], Arc<NativeCompletionSnapshotFacts>, NativeCompletionCodec>,
}
impl MaterializedContext {
    pub fn root(&self) -> &ContextRoot {
        &self.root
    }
    pub fn snapshot(&self) -> &ExactContextSnapshot {
        &self.snapshot
    }
}
impl<BS: Blockstore> Materializer<'_, BS> {
    pub fn materialize_context(
        &mut self,
        base: Option<&MaterializedContext>,
        next: &ExactContextSnapshot,
    ) -> Result<MaterializedContext, SnapshotError> {
        self.publication_native_base =
            base.and_then(|base| base.snapshot.native_fact_owner.clone());
        self.selection_base = base.map(|base| base.selection.clone());
        self.native_availability_base = base.map(|base| base.native_availability.clone());
        let selection =
            self.materialize_selection(base.map(|base| &base.selection), &next.selection)?;
        let roles = self.materialize_map_with::<_, _, CompilerRoleCodec>(
            base.map(|base| &base.roles),
            &next.roles,
        )?;
        let lexical = self.materialize_map_with::<_, _, LexicalFactCodec>(
            base.map(|base| &base.lexical),
            &next.lexical.facts,
        )?;
        let offers = self.materialize_map_with::<_, _, NativeOfferCodec>(
            base.map(|base| &base.offers),
            &next.offers,
        )?;
        let native_availability = self.materialize_map_with::<_, _, NativeAvailabilityCodec>(
            base.map(|base| &base.native_availability),
            &next.native_availability,
        )?;
        let input_decisions = self.materialize_map_with::<_, _, InputNativeDecisionCodec>(
            base.map(|base| &base.input_decisions),
            &next.input_decisions,
        )?;
        let native_completions = self.materialize_map_with::<_, _, NativeCompletionCodec>(
            base.map(|base| &base.native_completions),
            &next.native_completions,
        )?;
        let descriptor = ContextDescriptor {
            version: CONTEXT_ENCODING_VERSION,
            producer: next.producer,
            selection: selection.root.clone(),
            roles: roles.root.clone(),
            lexical: lexical.root.clone(),
            offers: offers.root.clone(),
            native_availability: native_availability.root.clone(),
            input_decisions: input_decisions.root.clone(),
            native_completions: native_completions.root.clone(),
        };
        let root = if base.is_some_and(|base| {
            base.snapshot.producer == next.producer
                && base.selection.root == selection.root
                && base.roles.root == roles.root
                && base.lexical.root == lexical.root
                && base.offers.root == offers.root
                && base.native_availability.root == native_availability.root
                && base.input_decisions.root == input_decisions.root
                && base.native_completions.root == native_completions.root
        }) {
            base.expect("equal context implies base").root.clone()
        } else {
            ContextRoot(self.materialize_value(&descriptor)?)
        };
        Ok(MaterializedContext {
            root,
            snapshot: next.clone(),
            selection,
            roles,
            lexical,
            offers,
            native_availability,
            input_decisions,
            native_completions,
        })
    }
}
pub fn hydrate_context<BS: Blockstore>(
    store: &BS,
    root: &ContextRoot,
) -> Result<MaterializedContext, SnapshotError> {
    let hydrator = SnapshotHydrator::new(store);
    let store = &hydrator;
    let bytes = read_checked_bytes(store, root.cid())?;
    // Refuse an old schema before decoding its layout or following any links.
    #[derive(Deserialize)]
    struct ContextVersion {
        version: u32,
    }
    let version: ContextVersion = fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)?;
    if version.version != CONTEXT_ENCODING_VERSION {
        return Err(SnapshotError::UnsupportedContextVersion {
            found: version.version,
            expected: CONTEXT_ENCODING_VERSION,
        });
    }
    let descriptor: ContextDescriptor = fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)?;
    let selection = store.selection(&descriptor.selection)?;
    let roles = hydrate_map_owned::<_, _, CompilerRoleCodec, _>(store, &descriptor.roles)?;
    let lexical: MaterializedMap<ExactModuleIdentity, ImmutableLexicalFactRef, LexicalFactCodec> =
        hydrate_map_owned::<_, _, LexicalFactCodec, _>(store, &descriptor.lexical)?;
    if lexical
        .facts
        .iter()
        .any(|(owner, fact)| owner != &fact.node.owner)
    {
        return Err(malformed("lexical key differs from fact owner"));
    }
    let offers = hydrate_map_owned::<_, _, NativeOfferCodec, _>(store, &descriptor.offers)?;
    for (identity, fact) in offers.facts.iter() {
        if *identity != fact.identity().map_err(malformed)? {
            return Err(malformed("native offer key differs from fact identity"));
        }
    }
    let native_availability = hydrate_map_owned::<_, _, NativeAvailabilityCodec, _>(
        store,
        &descriptor.native_availability,
    )?;
    for (original, row) in &native_availability.facts {
        if *original != row.original {
            return Err(malformed("native availability key differs from original"));
        }
        for contribution in row.contributions.values() {
            if !offers.facts.contains_key(&contribution.offer) {
                return Err(malformed("native availability references a missing offer"));
            }
        }
    }
    let input_decisions =
        hydrate_map_owned::<_, _, InputNativeDecisionCodec, _>(store, &descriptor.input_decisions)?;
    let native_completions =
        hydrate_map_owned::<_, _, NativeCompletionCodec, _>(store, &descriptor.native_completions)?;
    for (identity, facts) in &native_completions.facts {
        if *identity != facts.identity().map_err(malformed)? {
            return Err(malformed(
                "native completion key differs from its issued facts",
            ));
        }
    }
    let native_fact_owner = Some(PublicationNativeFacts::maintain(
        None,
        PublicationNativeMaps {
            roles: roles.facts.clone(),
            offers: offers.facts.clone(),
            native_availability: native_availability.facts.clone(),
            input_decisions: input_decisions.facts.clone(),
        },
    )?);
    let snapshot = ExactContextSnapshot {
        native_fact_owner,
        producer: descriptor.producer,
        selection: selection.snapshot.clone(),
        roles: roles.facts.clone(),
        offers: offers.facts.clone(),
        native_availability: native_availability.facts.clone(),
        input_decisions: input_decisions.facts.clone(),
        native_completions: native_completions.facts.clone(),
        lexical: ExactLexicalSnapshot {
            facts: lexical.facts.clone(),
            rows: Arc::new(OnceLock::new()),
        },
    };
    Ok(MaterializedContext {
        root: root.clone(),
        snapshot,
        selection,
        roles,
        lexical,
        offers,
        native_availability,
        input_decisions,
        native_completions,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod projection_tests {
    use super::*;
    use std::collections::BTreeMap;
    use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};

    thread_local! { static CODEC_WORK: Cell<(u64,u64)> = const { Cell::new((0,0)) }; }
    struct ObservedScopeCodec;
    impl SnapshotValueCodec<ImmutableScopeProofRef> for ObservedScopeCodec {
        type Wire = Cid;
        fn encode<BS: Blockstore>(
            store: &mut Materializer<'_, BS>,
            value: &ImmutableScopeProofRef,
        ) -> Result<Cid, SnapshotError> {
            CODEC_WORK.with(|work| {
                let (e, d) = work.get();
                work.set((e + 1, d));
            });
            ScopeProofCodec::encode(store, value)
        }
        fn decode<BS: Blockstore>(
            store: &SnapshotHydrator<'_, BS>,
            value: Cid,
        ) -> Result<ImmutableScopeProofRef, SnapshotError> {
            CODEC_WORK.with(|work| {
                let (e, d) = work.get();
                work.set((e, d + 1));
            });
            ScopeProofCodec::decode(store, value)
        }
    }

    #[test]
    fn projection_reuses_only_demanded_scope_values_without_codec_work() {
        // Serializer facts only: these packets issue no inventory/native authority.
        for unrelated in [0u64, 32, 128] {
            let facts = (0..=unrelated)
                .map(|key| {
                    let mut id = [0; 32];
                    id[..8].copy_from_slice(&key.to_le_bytes());
                    let selection = ArtifactSelectionId(id);
                    let proof = ImmutableScopeProofRef::from_issued(Arc::new(ScopeProofV3 {
                        selection,
                        nodes: (0..128)
                            .map(|n| {
                                ArtifactBindingNode::Artifact(ArtifactBinding {
                                    artifact: ArtifactId([n; 32]),
                                    selection,
                                })
                            })
                            .collect(),
                        dependencies: vec![],
                    }));
                    (key, proof)
                })
                .collect::<OrdMap<_, _>>();
            let empty = ReachableBlocks::default();
            let mut cold = Materializer::new(&empty);
            let base = cold
                .materialize_map_with::<_, _, ObservedScopeCodec>(None, &facts)
                .unwrap();
            assert!(CODEC_WORK.with(|work| work.replace((0, 0))).0 > 0);
            let staged = cold.finish().unwrap();
            let frontier = empty
                .replace_roots(&staged, vec![*base.root.cid()])
                .unwrap();
            let next = [(0u64, facts[&0].clone())]
                .into_iter()
                .collect::<OrdMap<u64, _>>();
            let keys = std::collections::BTreeSet::from([0u64]);
            let operation = InventoryOperation::new(Default::default());
            let mut writer = Materializer::new(&frontier);
            let (projected, work) = writer.project_map_with(&base, &keys, &operation).unwrap();
            assert_eq!((work.key_lookups, work.reused_values), (1, 1));
            assert_eq!(CODEC_WORK.with(Cell::get), (0, 0));
            let staged = writer.finish().unwrap();
            assert_eq!(staged.metrics().changed_entries, 0);
            let (selected, fw) = frontier
                .project_roots(&staged, vec![*projected.cid()], &operation)
                .unwrap();
            assert!(fw.reused_blocks > 0 && fw.reused_bytes > 4096);
            let proof_cid = facts[&0].encoded().unwrap().0;
            assert!(Arc::ptr_eq(
                &selected.blocks[&proof_cid].bytes,
                &frontier.blocks[&proof_cid].bytes
            ));
            let mut oracle = Materializer::new(&empty);
            let canonical = oracle
                .materialize_map_with::<_, _, ObservedScopeCodec>(None, &next)
                .unwrap();
            assert_eq!(&projected, canonical.root());
            assert!(CODEC_WORK.with(|work| work.replace((0, 0))).0 > 0);
            let exhausted = InventoryOperation::new(InventoryDecodeLimits {
                max_work: 0,
                ..Default::default()
            });
            let mut refused = Materializer::new(&frontier);
            assert!(matches!(
                refused.project_map_with(&base, &keys, &exhausted),
                Err(SnapshotProjectionError::Operation(_))
            ));
            let missing = std::collections::BTreeSet::from([unrelated + 1]);
            assert!(matches!(
                refused.project_map_with(&base, &missing, &operation),
                Err(SnapshotProjectionError::MissingFact)
            ));
            assert_eq!(CODEC_WORK.with(Cell::get), (0, 0));
            drop(frontier);
            drop(staged);
            drop(base);
            drop(facts);
            assert!(selected.get(&proof_cid).unwrap().is_some());
        }
    }

    fn block_fixture() -> (ReachableBlocks, Vec<Cid>, BTreeMap<Cid, Vec<Cid>>) {
        let empty = ReachableBlocks::default();
        let mut writer = Materializer::new(&empty);
        let leaf = writer.materialize_value(&("leaf", 0u64)).unwrap();
        let a = writer.materialize_value(&("a", vec![leaf, leaf])).unwrap();
        let b = writer.materialize_value(&("b", vec![leaf])).unwrap();
        let top = writer.materialize_value(&("top", vec![a, b])).unwrap();
        let other = writer.materialize_value(&("other", 1u64)).unwrap();
        let staged = writer.finish().unwrap();
        let frontier = empty.replace_roots(&staged, vec![top, other]).unwrap();
        (
            frontier,
            vec![leaf, a, b, top, other],
            BTreeMap::from([
                (leaf, vec![]),
                (a, vec![leaf, leaf]),
                (b, vec![leaf]),
                (top, vec![a, b]),
                (other, vec![]),
            ]),
        )
    }
    fn expected_counts(roots: &[Cid], edges: &BTreeMap<Cid, Vec<Cid>>) -> BTreeMap<Cid, u64> {
        let mut selected = std::collections::BTreeSet::new();
        let mut pending = roots.to_vec();
        while let Some(cid) = pending.pop() {
            if selected.insert(cid) {
                pending.extend(&edges[&cid]);
            }
        }
        selected
            .iter()
            .map(|cid| {
                let count = roots.iter().filter(|root| *root == cid).count()
                    + selected
                        .iter()
                        .map(|parent| edges[parent].iter().filter(|child| *child == cid).count())
                        .sum::<usize>();
                (*cid, count as u64)
            })
            .collect()
    }
    #[test]
    fn projection_frontier_recounts_edges_and_survives_owner_drop() {
        let (base, nodes, edges) = block_fixture();
        let operation = InventoryOperation::new(Default::default());
        let roots = vec![nodes[1], nodes[1], nodes[2]];
        let (next, work) = base
            .project_roots(&StagedBlocks::default(), roots.clone(), &operation)
            .unwrap();
        assert_eq!(
            next.blocks
                .iter()
                .map(|(k, v)| (*k, v.incoming))
                .collect::<BTreeMap<_, _>>(),
            expected_counts(&roots, &edges)
        );
        assert_eq!(
            (
                work.root_references,
                work.edges,
                work.reused_blocks,
                work.new_blocks
            ),
            (3, 3, 3, 0)
        );
        assert!(!next.blocks.contains_key(&nodes[3]) && !next.blocks.contains_key(&nodes[4]));
        let weak = Arc::downgrade(&base.blocks[&nodes[0]].bytes);
        let (fork, _) = base
            .project_roots(&StagedBlocks::default(), vec![nodes[0]], &operation)
            .unwrap();
        assert!(Arc::ptr_eq(
            &base.blocks[&nodes[0]].bytes,
            &next.blocks[&nodes[0]].bytes
        ));
        drop(base);
        drop(fork);
        assert!(weak.upgrade().is_some());
        assert!(next.get(&nodes[0]).unwrap().is_some());
        drop(next);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn projection_frontier_refuses_missing_children_without_mutating_base() {
        let (base, nodes, _) = block_fixture();
        let mut broken = base.clone();
        broken.blocks.remove(&nodes[0]);
        let before = broken.blocks.clone();
        let operation = InventoryOperation::new(Default::default());
        assert!(
            matches!(broken.project_roots(&StagedBlocks::default(),vec![nodes[3]],&operation),Err(SnapshotProjectionError::Snapshot(SnapshotError::MissingBlock(cid))) if cid==nodes[0])
        );
        assert!(broken.blocks == before);
        assert!(base.get(&nodes[0]).unwrap().is_some());
        let mut writer = Materializer::new(&base);
        let new = writer.materialize_value(&("new", vec![nodes[0]])).unwrap();
        let staged = writer.finish().unwrap();
        assert!(
            matches!(ReachableBlocks::default().project_roots(&staged,vec![new],&operation),Err(SnapshotProjectionError::Snapshot(SnapshotError::MissingBlock(cid))) if cid==nodes[0])
        );
    }
    #[test]
    fn native_fact_projection_matches_cold_subset_and_refuses_missing_keys() {
        for unrelated in [0u64, 32, 128] {
            let roles = (0..=unrelated)
                .map(|index| {
                    (
                        ExactModuleIdentity {
                            unit: "unit".into(),
                            module: format!("M{index}"),
                        },
                        CompilerInputRole::InterfaceOnly {
                            interface: ArtifactId([index as u8; 32]),
                        }
                        .recorded_fact(),
                    )
                })
                .collect::<OrdMap<_, _>>();
            let key = ExactModuleIdentity {
                unit: "unit".into(),
                module: "M0".into(),
            };
            let cold = PublicationNativeFacts::maintain(
                None,
                PublicationNativeMaps {
                    roles: roles.clone(),
                    offers: Default::default(),
                    native_availability: Default::default(),
                    input_decisions: Default::default(),
                },
            )
            .unwrap();
            let maps = PublicationNativeMaps {
                roles: [(key.clone(), roles[&key].clone())].into_iter().collect(),
                offers: Default::default(),
                native_availability: Default::default(),
                input_decisions: Default::default(),
            };
            let operation = InventoryOperation::new(Default::default());
            let keys = PublicationNativeMapKeys {
                roles: std::collections::BTreeSet::from([key.clone()]),
                ..Default::default()
            };
            let (projected, work) = cold.project(keys, &operation).unwrap();
            assert_eq!((work.maps.key_lookups, work.maps.reused_values), (1, 1));
            assert_eq!(
                projected.commitment(),
                PublicationNativeFacts::maintain(None, maps.clone())
                    .unwrap()
                    .commitment()
            );
            let missing = PublicationNativeMapKeys {
                roles: std::collections::BTreeSet::from([ExactModuleIdentity {
                    unit: "unit".into(),
                    module: "missing".into(),
                }]),
                ..Default::default()
            };
            assert!(matches!(
                cold.project(missing, &operation),
                Err(SnapshotProjectionError::MissingFact)
            ));
            let root = projected.commitment();
            drop(cold);
            assert!(projected.receipt.frontier.get(&root).unwrap().is_some());
        }
    }
    #[test]
    fn projection_staged_hash_is_bounded_before_integrity_work() {
        let (base, nodes, _) = block_fixture();
        // Intentionally wrong CID distinguishes pre-hash refusal from Integrity.
        let staged = StagedBlocks {
            blocks: vec![(nodes[0], Arc::from(vec![0u8; 65_536]))],
            metrics: Default::default(),
        };
        let work_limited = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 16_384,
            ..Default::default()
        });
        assert!(matches!(
            ReachableBlocks::default().project_roots(&staged, vec![nodes[0]], &work_limited),
            Err(SnapshotProjectionError::Operation(
                tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
            ))
        ));
        let bytes_limited = InventoryOperation::new(InventoryDecodeLimits {
            max_bytes: 256,
            ..Default::default()
        });
        assert!(matches!(
            ReachableBlocks::default().project_roots(&staged, vec![nodes[0]], &bytes_limited),
            Err(SnapshotProjectionError::Operation(
                tidepool_repr::execution_schema::ParseError::ByteLimit { .. }
            ))
        ));
        assert!(base.get(&nodes[0]).unwrap().is_some());
    }

    #[test]
    fn key_projection_keeps_full_proof_bytes_when_observations_are_equal() {
        fn data(extra_unselected_node: bool) -> NativeOfferSnapshotFacts {
            let scope = ArtifactSelectionId([1; 32]);
            let binding = ArtifactBinding {
                artifact: ArtifactId([2; 32]),
                selection: scope,
            };
            let node = ArtifactBindingNode::Artifact(binding);
            let mut nodes = vec![node];
            if extra_unselected_node {
                nodes.push(ArtifactBindingNode::Artifact(ArtifactBinding {
                    artifact: ArtifactId([3; 32]),
                    selection: scope,
                }));
            }
            let proof = ImmutableScopeProofRef::from_issued(Arc::new(ScopeProofV3 {
                selection: scope,
                nodes,
                dependencies: vec![],
            }));
            let snapshot = Arc::new(ExactSelectionSnapshot {
                scopes: [(scope, proof)].into_iter().collect(),
                roots: [(node, ())].into_iter().collect(),
                namespace: [(binding.artifact, binding)].into_iter().collect(),
                custody_roots: Default::default(),
            });
            NativeOfferSnapshotFacts {
                original: binding.artifact,
                interface: ArtifactId([4; 32]),
                owner: ExactModuleIdentity {
                    unit: "unit".into(),
                    module: "Data".into(),
                },
                body: NativeOfferSnapshotBody::Full(
                    crate::artifact_inventory::ExactArtifactSelection::from_snapshot(snapshot),
                ),
            }
        }
        // Passive decoded DATA may claim the same scope identity with different
        // atomic proof bytes. This is not a pair of authenticated live issuers.
        let a = data(false);
        let b: NativeOfferSnapshotFacts =
            serde_json::from_slice(&serde_json::to_vec(&data(true)).unwrap()).unwrap();
        assert_eq!(a, b, "selected observations omit the unselected proof node");
        let key = a.identity().unwrap();
        assert_eq!(key, b.identity().unwrap());
        let maps = |fact| PublicationNativeMaps {
            roles: Default::default(),
            offers: [(key, fact)].into_iter().collect(),
            native_availability: Default::default(),
            input_decisions: Default::default(),
        };
        let cold_a = PublicationNativeFacts::maintain(None, maps(a)).unwrap();
        let cold_b = PublicationNativeFacts::maintain(None, maps(b)).unwrap();
        assert_ne!(
            cold_a.commitment(),
            cold_b.commitment(),
            "complete proof encoding preserves the extra atomic node"
        );
        let wire = |facts: &PublicationNativeFacts| {
            lookup_map::<_, Cid, _>(&facts.receipt.frontier, facts.receipt.offers.root(), &key)
                .unwrap()
                .unwrap()
        };
        assert_ne!(wire(&cold_a), wire(&cold_b));
        let mismatched = PublicationNativeTransport {
            root: cold_a.commitment(),
            maps: cold_b.maps.clone(),
        };
        assert!(
            serde_json::from_slice::<PublicationNativeFacts>(
                &serde_json::to_vec(&mismatched).unwrap()
            )
            .is_err(),
            "semantic Eq cannot associate foreign full facts with another owner's encoded root"
        );
        let keys = PublicationNativeMapKeys {
            offers: std::collections::BTreeSet::from([key]),
            ..Default::default()
        };
        let operation = InventoryOperation::new(Default::default());
        let (projected, work) = cold_a.project(keys, &operation).unwrap();
        assert_eq!((work.maps.key_lookups, work.maps.reused_values), (1, 1));
        assert_eq!(wire(&projected), wire(&cold_a));
        let decoded: PublicationNativeFacts =
            serde_json::from_slice(&serde_json::to_vec(projected.as_ref()).unwrap()).unwrap();
        assert_eq!(decoded.commitment(), projected.commitment());
        assert_eq!(wire(&decoded), wire(&cold_a));
    }
}
