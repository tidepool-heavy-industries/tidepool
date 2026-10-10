//! Immutable exact snapshots. Resident maps retain structural sharing; the
//! operation-local materializer applies their library diff to FVM HAMTs.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::OnceLock;

use cid::Cid;
use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
use fvm_ipld_encoding::{CborStore, DAG_CBOR};
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
            Err(source) => SnapshotError::Storage(source),
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
            .map_err(storage_error)
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
        self.materialize_map_with(base, next, &InlineValues)
    }

    /// The codec is chosen by the fact owner, rather than a per-call closure.
    pub fn materialize_map_with<K, V, C>(
        &mut self,
        base: Option<&MaterializedMap<K, V, C>>,
        next: &OrdMap<K, V>,
        codec: &C,
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
                    edits.push((
                        BytesKey(key.encode_key()?),
                        Some(codec.encode(self, value)?),
                    ));
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
        if let Some(existing) = self.base.get(cid).map_err(storage_error)? {
            verify_block(cid, &existing)?;
            return Ok(());
        }
        self.pending.put_keyed(cid, bytes).map_err(storage_error)?;
        Ok(())
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
        &self,
        store: &mut Materializer<'_, BS>,
        value: &V,
    ) -> Result<Self::Wire, SnapshotError>;
    fn decode<BS: Blockstore>(&self, store: &BS, value: Self::Wire) -> Result<V, SnapshotError>;
}
#[derive(Clone, Copy, Debug)]
pub struct InlineValues;
impl<V: Clone + Eq + Serialize + DeserializeOwned> SnapshotValueCodec<V> for InlineValues {
    type Wire = V;
    fn encode<BS: Blockstore>(
        &self,
        _: &mut Materializer<'_, BS>,
        value: &V,
    ) -> Result<V, SnapshotError> {
        Ok(value.clone())
    }
    fn decode<BS: Blockstore>(&self, _: &BS, value: V) -> Result<V, SnapshotError> {
        Ok(value)
    }
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
    hydrate_map_with(store, root, &InlineValues)
}

pub fn hydrate_map_with<K, V, C, BS>(
    store: &BS,
    root: &SnapshotMapRoot,
    codec: &C,
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
        let key = K::decode_key(&key.0)?;
        if facts
            .insert(key, codec.decode(store, value.clone())?)
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

/// Scope facts have a separately authenticated binding identity. Equality is
/// compact; the immutable issuing owner never substitutes rows under that ID.
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
        self.proof().selection == other.proof().selection
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
            native_custody: graph
                .native_custody
                .iter()
                .map(|binding| (*binding, ()))
                .collect(),
        }
    }

    fn selected_nodes(
        &self,
    ) -> Result<std::collections::BTreeSet<ArtifactBindingNode>, SnapshotError> {
        let mut selected = std::collections::BTreeSet::new();
        let mut pending: Vec<_> = self
            .roots
            .keys()
            .copied()
            .chain(
                self.native_custody
                    .keys()
                    .copied()
                    .map(ArtifactBindingNode::Artifact),
            )
            .collect();
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

    /// Derive selected payload/group rows from exact typed roots and custody.
    /// Proof-only ancestors remain available without becoming payload authority.
    pub fn graph_selection(&self) -> Result<ArtifactGraphSelection, SnapshotError> {
        let selected = self.selected_nodes()?;
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
            native_custody: self.native_custody.keys().copied().collect(),
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
        let selected = self.selected_nodes()?;
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
    native_custody: SnapshotMapRoot,
}

#[derive(Clone, Copy, Debug)]
pub struct ScopeProofCodec;
impl SnapshotValueCodec<ImmutableScopeProofRef> for ScopeProofCodec {
    type Wire = Cid;
    fn encode<BS: Blockstore>(
        &self,
        store: &mut Materializer<'_, BS>,
        value: &ImmutableScopeProofRef,
    ) -> Result<Cid, SnapshotError> {
        let (cid, bytes) = value.encoded()?;
        store.put_keyed(cid, bytes).map_err(storage_error)?;
        Ok(*cid)
    }
    fn decode<BS: Blockstore>(
        &self,
        store: &BS,
        cid: Cid,
    ) -> Result<ImmutableScopeProofRef, SnapshotError> {
        let proof: ScopeProofV3 = read_value(store, &cid)?;
        // The inventory authenticates complete scope identities and ancestry
        // before these decoded facts become live compiler custody.
        Ok(ImmutableScopeProofRef::from_issued(Arc::new(proof)))
    }
}

#[derive(Clone, Debug)]
pub struct MaterializedSelection {
    root: SelectionRoot,
    snapshot: Arc<ExactSelectionSnapshot>,
    scopes: MaterializedMap<ArtifactSelectionId, ImmutableScopeProofRef, ScopeProofCodec>,
    roots: MaterializedMap<ArtifactBindingNode, ()>,
    namespace: MaterializedMap<ArtifactId, ArtifactBinding>,
    native_custody: MaterializedMap<ArtifactBinding, ()>,
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
        let scopes =
            self.materialize_map_with(base.map(|b| &b.scopes), &next.scopes, &ScopeProofCodec)?;
        let roots = self.materialize_map(base.map(|b| &b.roots), &next.roots)?;
        let namespace = self.materialize_map(base.map(|b| &b.namespace), &next.namespace)?;
        let native_custody =
            self.materialize_map(base.map(|b| &b.native_custody), &next.native_custody)?;
        let descriptor = SelectionDescriptor {
            version: 1,
            scopes: scopes.root.clone(),
            roots: roots.root.clone(),
            namespace: namespace.root.clone(),
            native_custody: native_custody.root.clone(),
        };
        let root = if base.is_some_and(|base| {
            base.scopes.root == scopes.root
                && base.roots.root == roots.root
                && base.namespace.root == namespace.root
                && base.native_custody.root == native_custody.root
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
            native_custody,
        })
    }
}
pub fn hydrate_selection<BS: Blockstore>(
    store: &BS,
    root: &SelectionRoot,
) -> Result<MaterializedSelection, SnapshotError> {
    let descriptor: SelectionDescriptor = read_value(store, root.cid())?;
    if descriptor.version != 1 {
        return Err(malformed("selection encoding version"));
    }
    let scopes: MaterializedMap<ArtifactSelectionId, ImmutableScopeProofRef, ScopeProofCodec> =
        hydrate_map_with(store, &descriptor.scopes, &ScopeProofCodec)?;
    if scopes
        .facts
        .iter()
        .any(|(id, proof)| *id != proof.proof().selection)
    {
        return Err(malformed("scope fact has another binding identity"));
    }
    let roots = hydrate_map(store, &descriptor.roots)?;
    let namespace = hydrate_map(store, &descriptor.namespace)?;
    let native_custody = hydrate_map(store, &descriptor.native_custody)?;
    let snapshot = Arc::new(ExactSelectionSnapshot {
        scopes: scopes.facts.clone(),
        roots: roots.facts.clone(),
        namespace: namespace.facts.clone(),
        native_custody: native_custody.facts.clone(),
    });
    snapshot.graph_selection()?;
    Ok(MaterializedSelection {
        root: root.clone(),
        snapshot,
        scopes,
        roots,
        namespace,
        native_custody,
    })
}

#[cfg(test)]
mod tests;
