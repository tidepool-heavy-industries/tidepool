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
    CompilerInputRole, ScopeProofV3,
};
use crate::declaration_join::{ExactLexicalNode, ExactModuleIdentity};

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

pub fn read_value<T: DeserializeOwned, BS: Blockstore>(
    store: &BS,
    cid: &Cid,
) -> Result<T, SnapshotError> {
    let bytes = store
        .get(cid)
        .map_err(storage_error)?
        .ok_or(SnapshotError::MissingBlock(*cid))?;
    verify_block(cid, &bytes)?;
    fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)
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

/// FVM HAMTs and their unsynchronized child caches never leave this owner.
pub struct Materializer<'a, BS: Blockstore> {
    base: &'a BS,
    pending: MemoryBlockstore,
    changed_entries: u64,
    offered_bytes: Cell<u64>,
    serialized_bytes: Cell<u64>,
    cid_validation_bytes: Cell<u64>,
    selection_base: Option<MaterializedSelection>,
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
    store: &'a BS,
    selections: RefCell<std::collections::BTreeMap<SelectionRoot, MaterializedSelection>>,
    scopes: RefCell<std::collections::BTreeMap<Cid, ImmutableScopeProofRef>>,
}
impl<'a, BS: Blockstore> SnapshotHydrator<'a, BS> {
    fn new(store: &'a BS) -> Self {
        Self {
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
    encoded: OnceLock<(Cid, Arc<[u8]>)>,
}

impl ImmutableScopeProofRef {
    pub(crate) fn from_issued(proof: Arc<ScopeProofV3>) -> Self {
        Self(Arc::new(ScopeFact {
            proof,
            encoded: OnceLock::new(),
        }))
    }
    pub fn proof(&self) -> &Arc<ScopeProofV3> {
        &self.0.proof
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
            selections: self
                .scopes
                .values()
                .map(|proof| proof.proof().clone())
                .collect(),
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
        let (cid, bytes) = value.encoded()?;
        store.put_keyed(cid, bytes).map_err(storage_error)?;
        Ok(*cid)
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
        let proof = ImmutableScopeProofRef(Arc::new(ScopeFact {
            proof: Arc::new(proof),
            encoded: OnceLock::from((cid, bytes.into())),
        }));
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
    pub fn materialize_selection(
        &mut self,
        base: Option<&MaterializedSelection>,
        next: &ExactSelectionSnapshot,
    ) -> Result<MaterializedSelection, SnapshotError> {
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
        Ok(MaterializedSelection {
            root,
            snapshot: Arc::new(next.clone()),
            scopes,
            roots,
            namespace,
            custody_roots,
        })
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeOfferSnapshotBody {
    Empty(crate::artifact_inventory::ExactArtifactSelection),
    Partial(crate::artifact_inventory::ExactArtifactSelection),
    Full(crate::artifact_inventory::ExactArtifactSelection),
}
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// Exact compiler facts. Source-instance authority remains with the live
/// declaration/transaction owner and is deliberately absent from this snapshot.
#[derive(Clone, Debug, Default)]
pub struct ExactContextSnapshot {
    pub(crate) producer: [u8; 32],
    pub(crate) selection: Arc<ExactSelectionSnapshot>,
    pub(crate) roles: OrdMap<ExactModuleIdentity, CompilerInputRole>,
    pub(crate) lexical: ExactLexicalSnapshot,
    pub(crate) offers: OrdMap<[u8; 32], NativeOfferSnapshotFacts>,
    pub(crate) input_decisions:
        OrdMap<ExactModuleIdentity, crate::artifact_inventory::RecordedInputNativeDecision>,
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
        self.roles.values().cloned().collect()
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
            } => {
                let base = store.selection_base.clone();
                let selection = store.materialize_selection(base.as_ref(), selection.snapshot())?;
                CompilerRoleFact::PublishedSourceOriginal {
                    interface: *interface,
                    original: *original,
                    source_revision: source_revision.clone(),
                    original_input_identity: original_input_identity.clone(),
                    selection: selection.root,
                    selection_sha256: *selection_sha256,
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
            } => {
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
                }
            }
        })
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
        let role = CompilerRoleCodec::decode(store, wire.role)?;
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextDescriptor {
    version: u32,
    producer: [u8; 32],
    selection: SelectionRoot,
    roles: SnapshotMapRoot,
    lexical: SnapshotMapRoot,
    offers: SnapshotMapRoot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_decisions: Option<SnapshotMapRoot>,
}
#[derive(Clone, Debug)]
pub struct MaterializedContext {
    root: ContextRoot,
    snapshot: ExactContextSnapshot,
    selection: MaterializedSelection,
    roles: MaterializedMap<ExactModuleIdentity, CompilerInputRole, CompilerRoleCodec>,
    lexical: MaterializedMap<ExactModuleIdentity, ImmutableLexicalFactRef, LexicalFactCodec>,
    offers: MaterializedMap<[u8; 32], NativeOfferSnapshotFacts, NativeOfferCodec>,
    input_decisions: MaterializedMap<
        ExactModuleIdentity,
        crate::artifact_inventory::RecordedInputNativeDecision,
        InputNativeDecisionCodec,
    >,
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
        self.selection_base = base.map(|base| base.selection.clone());
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
        let input_decisions = self.materialize_map_with::<_, _, InputNativeDecisionCodec>(
            base.map(|base| &base.input_decisions),
            &next.input_decisions,
        )?;
        let descriptor = ContextDescriptor {
            version: 4,
            producer: next.producer,
            selection: selection.root.clone(),
            roles: roles.root.clone(),
            lexical: lexical.root.clone(),
            offers: offers.root.clone(),
            input_decisions: Some(input_decisions.root.clone()),
        };
        let root = if base.is_some_and(|base| {
            base.snapshot.producer == next.producer
                && base.selection.root == selection.root
                && base.roles.root == roles.root
                && base.lexical.root == lexical.root
                && base.offers.root == offers.root
                && base.input_decisions.root == input_decisions.root
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
            input_decisions,
        })
    }
}
pub fn hydrate_context<BS: Blockstore>(
    store: &BS,
    root: &ContextRoot,
) -> Result<MaterializedContext, SnapshotError> {
    let hydrator = SnapshotHydrator::new(store);
    let store = &hydrator;
    let descriptor: ContextDescriptor = read_value(store, root.cid())?;
    if descriptor.version != 4 {
        return Err(malformed(
            "context encoding requires version 4 with issued input decisions",
        ));
    }
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
    let input_decisions = hydrate_map_owned::<_, _, InputNativeDecisionCodec, _>(
        store,
        descriptor
            .input_decisions
            .as_ref()
            .ok_or_else(|| malformed("context version 4 omits input decisions"))?,
    )?;
    let snapshot = ExactContextSnapshot {
        producer: descriptor.producer,
        selection: selection.snapshot.clone(),
        roles: roles.facts.clone(),
        offers: offers.facts.clone(),
        input_decisions: input_decisions.facts.clone(),
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
        input_decisions,
    })
}

#[cfg(test)]
mod tests;
